//! The vault modules ported into the brain, run through relay vaults: spending
//! limits, hidden flows, receiver hooks, act, stealth vaults (signed commands
//! through the `exec` door, same text as the keeper's stealth.ts) and NEAR
//! Intents against the real intents.near code. Build the wasm first: make build
//! (Intents also needs scripts/fetch-protocols.sh).

use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const SCALE: u128 = 1_000_000_000_000_000_000_000_000;
const HOUR_NS: u64 = 3_600 * 1_000_000_000;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

struct World {
    worker: Worker<Sandbox>,
    factory: Account,
    brain: Contract,
    alice: Account,
    keeper: Account,
    mallory: Account,
    minter: Account,
    wnear: Contract,
    usdc: Contract,
    dex: Contract,
    vault: AccountId,
}

async fn token(worker: &Worker<Sandbox>, owner: &Account, symbol: &str) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new")
        .args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
                          "name": symbol, "symbol": symbol, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn register(payer: &Account, token: &Contract, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token.id(), "storage_deposit").args_json(json!({"account_id": who}))
        .deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

async fn give(from: &Account, token: &Contract, to: &AccountId, amount: u128) -> anyhow::Result<()> {
    from.call(token.id(), "ft_transfer").args_json(json!({"receiver_id": to, "amount": amount.to_string()}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    Ok(())
}

async fn x(who: &Account, vault: &AccountId, req: Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(vault, "x").args(req.to_string().into_bytes()).max_gas().transact().await?)
}

fn fails_with(r: &ExecutionFinalResult, why: &str) -> bool {
    r.is_failure() && format!("{:?}", r.failures()).contains(why)
}

fn swap(dex: &Contract, to: &Contract) -> Value {
    json!({"label": "Swap", "calls": [{"call": "ft", "receiver": dex.id(), "tgas": 50,
            "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, to.id())}],
           "out": to.id(), "min_out_per_in": null, "price": null})
}

/// Brain + relay factory; Alice's relay vault holds 1,000 wNEAR and trusts the dex.
async fn setup() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let factory = root.create_subaccount("flows").initial_balance(NearToken::from_near(50)).transact().await?.into_result()?;
    let brain_acc = root.create_subaccount("brain").initial_balance(NearToken::from_near(10)).transact().await?.into_result()?;
    let brain = brain_acc.deploy(&wasm("brain")).await?.into_result()?;
    brain.call("new").args_json(json!({"factory": factory.id()})).transact().await?.into_result()?;
    factory.deploy(&wasm("factory_relay")).await?.into_result()?;
    factory.call(factory.id(), "new").args_json(json!({"owner": factory.id(), "brain": brain.id()})).transact().await?.into_result()?;
    factory.call(factory.id(), "publish").args(wasm("relay")).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let (alice, keeper, mallory, minter) =
        (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    alice.call(factory.id(), "create_vault").args_json(json!({"name": "alice", "keeper": keeper.id()}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.into_result()?;
    let vault: AccountId = format!("alice.{}", factory.id()).parse()?;
    let (wnear, usdc, lst) = (token(&worker, &minter, "wNEAR").await?, token(&worker, &minter, "USDC").await?, token(&worker, &minter, "LST").await?);
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": lst.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [alice.id(), dex.id(), &vault, mallory.id()] {
            register(&minter, t, who).await?;
        }
        give(&minter, t, dex.id(), 100_000_000 * ONE).await?;
    }
    give(&minter, &wnear, &vault, 1_000 * ONE).await?;
    for (a, b, r) in [(&wnear, &usdc, 2 * SCALE), (&usdc, &wnear, SCALE / 2)] {
        alice.call(dex.id(), "set_rate").args_json(json!({"token_in": a.id(), "token_out": b.id(), "rate": r.to_string()})).transact().await?.into_result()?;
    }
    x(&alice, &vault, json!({"trust": {"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id(), "methods": ["get_return", "set_broken"]}], "remove": []}}))
        .await?.into_result()?;
    Ok(World { worker, factory, brain, alice, keeper, mallory, minter, wnear, usdc, dex, vault })
}

#[tokio::test]
async fn spending_limits_hold_through_the_brain_and_loosening_waits_a_day() -> anyhow::Result<()> {
    let w = setup().await?;
    let node = json!({"input": w.wnear.id(), "action": swap(&w.dex, &w.usdc), "when": {"when": "now"}});
    let f: u32 = x(&w.alice, &w.vault, json!({"save_flow": {"name": "dca", "nodes": [node]}})).await?.json()?;
    x(&w.alice, &w.vault, json!({"set_limit": {"token": w.wnear.id(), "max": (150 * ONE).to_string(), "window_ns": HOUR_NS}})).await?.into_result()?;
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"set_limit": {"token": w.wnear.id(), "max": null, "window_ns": 0}})).await?, "owner only"));
    for _ in 0..2 {
        x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (100 * ONE).to_string()}})).await?.into_result()?;
    }
    // The whole inbox (200) is over the 150 cap: refused before anything moves.
    let r = x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?;
    assert!(fails_with(&r, "spending limit"), "{:?}", r.failures());
    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE);
    // Tightening is instant; loosening (or removing) waits 24 h, and anyone may apply it then.
    x(&w.alice, &w.vault, json!({"set_limit": {"token": w.wnear.id(), "max": null, "window_ns": 0}})).await?.into_result()?;
    assert!(fails_with(&x(&w.mallory, &w.vault, json!({"apply_limit": {"token": w.wnear.id()}})).await?, "not yet"));
    let (limits, pending): (Vec<Value>, Vec<Value>) = w.brain.view("get_limits").args_json(json!({"vault": w.vault})).await?.json()?;
    assert_eq!((limits.len(), pending.len()), (1, 1));
    assert!(x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?.is_failure(), "still capped until applied");
    Ok(())
}

#[tokio::test]
async fn a_hidden_flow_runs_one_revealed_node_with_the_vaults_fingerprints() -> anyhow::Result<()> {
    let w = setup().await?;
    let node = json!({"input": w.wnear.id(), "action": swap(&w.dex, &w.usdc), "when": {"when": "now"}});
    let salt = "aa".repeat(32);
    let print: String = w.brain.view("fingerprint_of").args_json(json!({"node": node, "salt": salt})).await?.json()?;
    let f: u32 = x(&w.alice, &w.vault, json!({"save_hidden_flow": {"name": "secret", "inputs": [w.wnear.id()], "fingerprints": [print]}})).await?.json()?;
    let stored: Value = w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": f})).await?.json()?;
    assert_eq!(stored["nodes"], json!([]), "nothing but the fingerprint is on chain");
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()}})).await?.into_result()?;
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?, "step_hidden"));
    let wrong = x(&w.keeper, &w.vault, json!({"step_hidden": {"flow_id": f, "node": 0, "reveal": node, "salt": "bb".repeat(32)}})).await?;
    assert!(fails_with(&wrong, "doesn't match"));
    let mut greedy = node.clone();
    greedy["action"]["calls"][0]["receiver"] = json!(w.mallory.id());
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"step_hidden": {"flow_id": f, "node": 0, "reveal": greedy, "salt": salt}})).await?, "doesn't match"));
    let r = x(&w.keeper, &w.vault, json!({"step_hidden": {"flow_id": f, "node": 0, "reveal": node, "salt": salt}})).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(balance(&w.usdc, &w.vault).await?, 20 * ONE);
    x(&w.alice, &w.vault, json!({"reveal_flow": {"flow_id": f, "nodes": [node], "salts": [salt]}})).await?.into_result()?;
    let stored: Value = w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": f})).await?.json()?;
    assert_eq!(stored["nodes"].as_array().unwrap().len(), 1);
    // A market floor with no oracle set up: saved, but it never runs without a price.
    let mut floored = node.clone();
    floored["action"]["market"] = json!({"sell": "near", "buy": "usdc", "dec_in": 24, "dec_out": 18, "slippage_bps": 50});
    let m: u32 = x(&w.alice, &w.vault, json!({"save_flow": {"name": "m", "nodes": [floored]}})).await?.json()?;
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": m, "node": 0, "amount": ONE.to_string()}})).await?.into_result()?;
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"step": {"flow_id": m, "node": 0}})).await?, "no oracle set up"));
    Ok(())
}

#[tokio::test]
async fn hooks_answer_keep_everything_and_act_follows_the_trust_list() -> anyhow::Result<()> {
    let w = setup().await?;
    // Anyone can call the hooks with any claims: the answer is "keep it", nothing changes.
    let r = w.mallory.call(&w.vault, "mt_on_transfer")
        .args_json(json!({"sender_id": w.mallory.id(), "previous_owner_ids": [w.mallory.id(), w.mallory.id()], "token_ids": ["nep141:a", "nep141:b"], "amounts": ["5", "7"], "msg": ""}))
        .gas(near_workspaces::types::Gas::from_tgas(30)).transact().await?;
    println!("GAS mt_on_transfer: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert_eq!(r.into_result()?.json::<Vec<String>>()?, vec!["0", "0"]);
    let r = w.mallory.call(&w.vault, "nft_on_transfer")
        .args_json(json!({"sender_id": w.mallory.id(), "previous_owner_id": w.mallory.id(), "token_id": "1", "msg": ""})).max_gas().transact().await?;
    assert!(!r.into_result()?.json::<bool>()?);
    // act: the owner's one-off call, trusted contract and method only, never one that moves funds.
    assert!(fails_with(&x(&w.alice, &w.vault, json!({"act": {"target": w.dex.id(), "method": "set_rate", "json_args": "{}", "one_yocto": false}})).await?, "untrusted: method"));
    assert!(fails_with(&x(&w.alice, &w.vault, json!({"act": {"target": w.wnear.id(), "method": "ft_transfer", "json_args": "{}", "one_yocto": true}})).await?, "moves funds"));
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"act": {"target": w.dex.id(), "method": "set_broken", "json_args": "{\"broken\":true}", "one_yocto": false}})).await?, "owner only"));
    x(&w.alice, &w.vault, json!({"act": {"target": w.dex.id(), "method": "set_broken", "json_args": "{\"broken\":true}", "one_yocto": false}})).await?.into_result()?;
    let node = json!({"input": w.wnear.id(), "action": swap(&w.dex, &w.usdc), "when": {"when": "now"}});
    let f: u32 = x(&w.alice, &w.vault, json!({"save_flow": {"name": "t", "nodes": [node]}})).await?.json()?;
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": ONE.to_string()}})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?;
    assert!(r.logs().join("").contains("step_failed"), "the act really broke the dex: {:?}", r.logs());
    // set_keeper: owner only, and the old keeper is out.
    x(&w.alice, &w.vault, json!({"set_keeper": {"keeper": w.mallory.id()}})).await?.into_result()?;
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?, "owner or keeper only"));
    Ok(())
}

/// The owner's key, off chain. Signs exactly what the keeper's stealth.ts signs.
struct Owner {
    key: SigningKey,
    nonce: u64,
}

impl Owner {
    fn public(&self) -> String {
        format!("ed25519:{}", bs58::encode(self.key.verifying_key().as_bytes()).into_string())
    }
    fn sign_with(&mut self, vault: &AccountId, method: &str, args: Value, tgas: u64, expires_ns: u64) -> Value {
        self.nonce += 1;
        let args = args.to_string();
        let text = format!("near-flows stealth v1\nvault: {vault}\nnonce: {}\nexpires_ns: {expires_ns}\ntgas: {tgas}\nmethod: {method}\nargs: {args}", self.nonce);
        let sig = hex::encode(self.key.sign(text.as_bytes()).to_bytes());
        json!({"method": method, "args": args, "nonce": self.nonce.to_string(), "expires_ns": expires_ns.to_string(), "tgas": tgas, "signature": sig})
    }
    fn sign(&mut self, vault: &AccountId, method: &str, args: Value) -> Value {
        self.sign_with(vault, method, args, 100, now_ns() + HOUR_NS)
    }
}

/// What the keeper's stealth.ts does: call the vault's `exec` with the signed command.
async fn exec(relayer: &Account, vault: &AccountId, signed: &Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(relayer.call(vault, "exec").args_json(signed).max_gas().transact().await?)
}

#[tokio::test]
async fn a_stealth_relay_vault_obeys_only_its_owners_signed_commands() -> anyhow::Result<()> {
    let w = setup().await?;
    let mut owner = Owner { key: SigningKey::from_bytes(&[7; 32]), nonce: 0 };
    // The keeper creates it (as a relayer): the owner's account never appears.
    let r = w.keeper.call(w.factory.id(), "create_stealth_vault").args_json(json!({"name": "k7xq2m", "owner_key": owner.public(), "keeper": w.keeper.id()}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let sv: AccountId = format!("k7xq2m.{}", w.factory.id()).parse()?;
    let key: Option<String> = w.brain.view("get_owner_key").args_json(json!({"vault": sv})).await?.json()?;
    assert_eq!(key, Some(owner.public()));
    for t in [&w.wnear, &w.usdc] {
        register(&w.minter, t, &sv).await?;
    }
    give(&w.minter, &w.wnear, &sv, 100 * ONE).await?;

    // Owner commands, relayed by the keeper.
    let trust = owner.sign(&sv, "trust", json!({"add": [{"contract": w.wnear.id()}, {"contract": w.usdc.id()}, {"contract": w.dex.id()}], "remove": []}));
    let r = exec(&w.keeper, &sv, &trust).await?;
    println!("GAS exec(trust): {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(fails_with(&exec(&w.keeper, &sv, &trust).await?, "nonce already used"), "a command runs once");
    let node = json!({"input": w.wnear.id(), "action": swap(&w.dex, &w.usdc), "when": {"when": "manual"}});
    exec(&w.keeper, &sv, &owner.sign(&sv, "save_flow", json!({"name": "s", "nodes": [node]}))).await?.into_result()?;
    exec(&w.keeper, &sv, &owner.sign(&sv, "fund", json!({"flow_id": 0, "node": 0, "amount": (10 * ONE).to_string()}))).await?.into_result()?;
    // A manual node: only the owner runs it, so the keeper needs a signed step.
    assert!(fails_with(&x(&w.keeper, &sv, json!({"step": {"flow_id": 0, "node": 0}})).await?, "OwnerOnly"));
    let r = exec(&w.keeper, &sv, &owner.sign_with(&sv, "step", json!({"flow_id": 0, "node": 0}), 200, now_ns() + HOUR_NS)).await?;
    println!("GAS exec(step): {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(balance(&w.usdc, &sv).await?, 20 * ONE);

    // Out to a fresh account, NEAR and a token. Plain `withdraw` has no owner account to go to.
    let fresh = w.worker.dev_create_account().await?;
    register(&w.minter, &w.usdc, fresh.id()).await?;
    exec(&w.keeper, &sv, &owner.sign(&sv, "withdraw_to", json!({"token": w.usdc.id(), "amount": (20 * ONE).to_string(), "to": fresh.id(), "via_intents": false}))).await?.into_result()?;
    assert_eq!(balance(&w.usdc, fresh.id()).await?, 20 * ONE);
    let before = fresh.view_account().await?.balance.as_yoctonear();
    exec(&w.keeper, &sv, &owner.sign(&sv, "withdraw_to", json!({"token": "near", "amount": (SCALE / 10).to_string(), "to": fresh.id(), "via_intents": false}))).await?.into_result()?;
    assert_eq!(fresh.view_account().await?.balance.as_yoctonear() - before, SCALE / 10);
    let r = exec(&w.keeper, &sv, &owner.sign(&sv, "withdraw", json!({"token": "near", "amount": "1"}))).await?;
    assert!(fails_with(&r, "withdraw_to"), "{:?}", r.failures());
    assert!(fails_with(&x(&w.keeper, &sv, json!({"withdraw_to": {"token": "near", "amount": "1", "to": w.keeper.id(), "via_intents": false}})).await?, "owner only"),
        "the relayer itself is nobody");

    // Refused: someone else's key, a changed command, expired, a command that signs again, a starved command.
    let mut stranger = Owner { key: SigningKey::from_bytes(&[9; 32]), nonce: 50 };
    assert!(fails_with(&exec(&w.mallory, &sv, &stranger.sign(&sv, "set_keeper", json!({"keeper": w.mallory.id()}))).await?, "bad signature"));
    let mut tampered = owner.sign(&sv, "withdraw_to", json!({"token": "near", "amount": "1", "to": fresh.id(), "via_intents": false}));
    tampered["args"] = json!(json!({"token": "near", "amount": "1", "to": w.mallory.id(), "via_intents": false}).to_string());
    assert!(fails_with(&exec(&w.mallory, &sv, &tampered).await?, "bad signature"));
    assert!(fails_with(&exec(&w.keeper, &sv, &owner.sign_with(&sv, "set_keeper", json!({"keeper": null}), 50, now_ns() - HOUR_NS)).await?, "expired"));
    let inner = owner.sign(&sv, "trust", json!({"add": [], "remove": []}));
    assert!(fails_with(&exec(&w.keeper, &sv, &owner.sign(&sv, "exec", inner)).await?, "not a command"));
    // The relayer attaches too little for the gas the owner signed: refused before the nonce burns.
    let starved = owner.sign_with(&sv, "set_keeper", json!({"keeper": null}), 100, now_ns() + HOUR_NS);
    // (Relay v2: 100 signed needs 150 = 100 + 5 (a relay round) + 40 (first round) + 5 (exec).)
    let r = w.keeper.call(&sv, "exec").args_json(&starved).gas(near_workspaces::types::Gas::from_tgas(140)).transact().await?;
    assert!(fails_with(&r, "attach at least"), "{:?}", r.failures());
    // A signed command is bound to its vault.
    let other = owner.sign(&w.vault, "set_keeper", json!({"keeper": null}));
    assert!(exec(&w.keeper, &sv, &other).await?.is_failure());
    // The key can be rotated by a signed command; the old key is then dead.
    let next = Owner { key: SigningKey::from_bytes(&[8; 32]), nonce: 0 };
    exec(&w.keeper, &sv, &owner.sign(&sv, "set_owner_key", json!({"owner_key": next.public()}))).await?.into_result()?;
    assert!(fails_with(&exec(&w.keeper, &sv, &owner.sign(&sv, "set_keeper", json!({"keeper": null}))).await?, "bad signature"));
    Ok(())
}

fn intents_wasm() -> Vec<u8> {
    std::fs::read(format!("{}/protocols/verifier.tidepool-sandbox.testnet.wasm", env!("CARGO_MANIFEST_DIR"))).expect("scripts/fetch-protocols.sh")
}

async fn mt(verifier: &Contract, who: &AccountId, token: &Contract) -> anyhow::Result<u128> {
    let v: String = verifier.view("mt_balance_of").args_json(json!({"account_id": who, "token_id": format!("nep141:{}", token.id())})).await?.json()?;
    Ok(v.parse()?)
}

fn signed_by_solver(key: &SigningKey, solver: &AccountId, verifier: &AccountId, salt: [u8; 4], give: (&AccountId, u128), get: (&AccountId, u128), seq: u8) -> Value {
    let deadline = now_ns() + 600_000_000_000;
    let msg = intentsig::swap_message(solver.as_str(), deadline, give.0.as_str(), give.1, get.0.as_str(), get.1);
    let nonce = intentsig::versioned_nonce(salt, deadline as i64, [seq; 15]);
    let sig = key.sign(&intentsig::nep413_hash(&msg, &nonce, verifier.as_str()));
    let s = intentsig::signed_json(&msg, &nonce, verifier.as_str(), &bs58::encode(key.verifying_key().as_bytes()).into_string(), &bs58::encode(sig.to_bytes()).into_string());
    serde_json::from_str(&s).unwrap()
}

#[tokio::test]
async fn a_relay_vault_swaps_on_real_intents_with_intents_the_brain_built() -> anyhow::Result<()> {
    let w = setup().await?;
    let (a, b) = (&w.wnear, &w.usdc);
    let solver = w.worker.dev_create_account().await?;
    let verifier = w.worker.dev_deploy(&intents_wasm()).await?;
    verifier.call("new").args_json(json!({"config": {"wnear_id": a.id(), "fees": {"fee": 1, "fee_collector": w.minter.id()}, "roles": {}}})).max_gas().transact().await?.into_result()?;
    let signer = w.worker.dev_deploy(&wasm("mock_signer")).await?;
    for t in [a, b] {
        for who in [verifier.id(), solver.id()] {
            register(&w.minter, t, who).await?;
        }
    }
    give(&w.minter, b, solver.id(), 1_000 * ONE).await?;

    // 1. Setup: the brain reads the vault's derived key, the vault registers it on its own Intents account.
    let r = x(&w.alice, &w.vault, json!({"intents_setup": {"verifier": verifier.id(), "signer": signer.id()}})).await?;
    println!("GAS intents_setup: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    let cfg: Value = w.brain.view("get_intents").args_json(json!({"vault": w.vault})).await?.json()?;
    let keys: Vec<String> = verifier.view("public_keys_of").args_json(json!({"account_id": w.vault})).await?.json()?;
    assert_eq!(keys, vec![format!("ed25519:{}", cfg["key"].as_str().unwrap())], "the verifier knows the vault's own key");

    // 2. 300 A into Intents with a flow node.
    x(&w.alice, &w.vault, json!({"trust": {"add": [{"contract": verifier.id()}], "remove": []}})).await?.into_result()?;
    let f: u32 = x(&w.alice, &w.vault, json!({"save_flow": {"name": "to intents", "nodes": [{"input": a.id(), "when": {"when": "now"},
        "action": {"label": "Deposit", "calls": [{"call": "ft", "receiver": verifier.id(), "msg": "", "tgas": 50}], "out": null, "min_out_per_in": null, "price": null}}]}})).await?.json()?;
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (300 * ONE).to_string()}})).await?.into_result()?;
    x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?.into_result()?;
    assert_eq!(mt(&verifier, &w.vault, a).await?, 300 * ONE);
    let skey = SigningKey::from_bytes(&[42; 32]);
    solver.call(verifier.id(), "add_public_key").args_json(json!({"public_key": format!("ed25519:{}", bs58::encode(skey.verifying_key().as_bytes()).into_string())}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    solver.call(b.id(), "ft_transfer_call").args_json(json!({"receiver_id": verifier.id(), "amount": (1_000 * ONE).to_string(), "msg": ""}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;

    // 3. Allow A -> B with a floor of 0.95; the keeper asks for 1 wei, the brain signs for 95.
    x(&w.alice, &w.vault, json!({"intents_allow": {"pair": {"token_in": a.id(), "token_out": b.id(), "max_in": (200 * ONE).to_string(), "floor_out": "95", "floor_per_in": "100"}}})).await?.into_result()?;
    let r = x(&w.alice, &w.vault, json!({"intents_sign_swap": {"token_in": a.id(), "token_out": b.id(), "amount_in": (100 * ONE).to_string(), "min_out": "1", "valid_secs": 600}})).await?;
    println!("GAS sign an intent: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    let signed: Value = r.json()?;
    assert!(signed["payload"]["message"].as_str().unwrap().contains(&format!(r#""nep141:{}":"{}""#, b.id(), 95 * ONE)), "{signed}");
    let salt: String = verifier.view("current_salt").await?.json()?;
    let salt: [u8; 4] = hex::decode(salt)?.try_into().unwrap();
    // 4. A greedy solver can't settle; a fair one can, on the real verifier.
    let greedy = signed_by_solver(&skey, solver.id(), verifier.id(), salt, (b.id(), 90 * ONE), (a.id(), 100 * ONE - 100), 1);
    assert!(w.keeper.call(verifier.id(), "execute_intents").args_json(json!({"signed": [signed.clone(), greedy]})).max_gas().transact().await?.is_failure());
    let gross = |net: u128| {
        let g0 = net * 1_000_000 / 999_999;
        (g0.saturating_sub(10)..g0 + 10).find(|g| g - g.div_ceil(1_000_000) == net).unwrap()
    };
    let fee_b = gross(95 * ONE) - 95 * ONE;
    let fair = signed_by_solver(&skey, solver.id(), verifier.id(), salt, (b.id(), 95 * ONE + fee_b), (a.id(), 100 * ONE - (100 * ONE).div_ceil(1_000_000)), 2);
    let r = w.keeper.call(verifier.id(), "execute_intents").args_json(json!({"signed": [signed, fair]})).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(mt(&verifier, &w.vault, b).await?, 95 * ONE, "got 95 B");
    // 5. Withdraw: only ever back to the vault.
    x(&w.keeper, &w.vault, json!({"intents_withdraw": {"token": b.id(), "amount": (95 * ONE).to_string()}})).await?.into_result()?;
    assert_eq!(balance(b, &w.vault).await?, 95 * ONE);
    // 6. Refusals: over the pair's limit, a pair nobody allowed, a stranger, a market floor (not ported).
    for (who, args) in [
        (&w.alice, json!({"token_in": a.id(), "token_out": b.id(), "amount_in": (201 * ONE).to_string(), "min_out": "1", "valid_secs": 600})),
        (&w.alice, json!({"token_in": b.id(), "token_out": a.id(), "amount_in": "1", "min_out": "1", "valid_secs": 600})),
        (&w.mallory, json!({"token_in": a.id(), "token_out": b.id(), "amount_in": "1", "min_out": "1", "valid_secs": 600})),
    ] {
        let r = x(who, &w.vault, json!({"intents_sign_swap": args})).await?;
        assert!(r.is_failure() && !r.logs().iter().any(|l| l.contains("intents_signed")));
    }
    // 7. A market floor: A and B both at $1 per the oracle (read by the brain), 1% slippage. The keeper
    //    asks for 1 wei, the hard floor says 95, the market says 99: the brain signs for 99.
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let ids = ["1111111111111111111111111111111111111111111111111111111111111111", "2222222222222222222222222222222222222222222222222222222222222222"];
    for id in ids {
        w.alice.call(pyth.id(), "set_price").args_json(json!({"price_id": id, "price": "100000000", "conf": "0", "expo": -8, "ema": "100000000", "age_s": 0}))
            .transact().await?.into_result()?;
    }
    x(&w.alice, &w.vault, json!({"oracle_setup": {"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "a", "contract": pyth.id(), "price_id": ids[0], "backup": {"kind": "ema"}},
        {"name": "b", "contract": pyth.id(), "price_id": ids[1], "backup": {"kind": "ema"}}]}}})).await?.into_result()?;
    x(&w.alice, &w.vault, json!({"intents_allow": {"pair": {"token_in": a.id(), "token_out": b.id(), "max_in": (200 * ONE).to_string(), "floor_out": "95", "floor_per_in": "100",
        "market": {"sell": "a", "buy": "b", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}}}})).await?.into_result()?;
    let r = x(&w.alice, &w.vault, json!({"intents_sign_swap": {"token_in": a.id(), "token_out": b.id(), "amount_in": (100 * ONE).to_string(), "min_out": "1", "valid_secs": 600}})).await?;
    println!("GAS sign an intent with a market floor: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    let signed: Value = r.json()?;
    assert!(signed["payload"]["message"].as_str().unwrap().contains(&format!(r#""nep141:{}":"{}""#, b.id(), 99 * ONE)), "{signed}");
    // Recipes and act can never reach sign / add_public_key / ft_withdraw.
    for (contract, method) in [(signer.id(), "sign"), (verifier.id(), "add_public_key"), (verifier.id(), "ft_withdraw")] {
        assert!(x(&w.alice, &w.vault, json!({"trust": {"add": [{"contract": contract, "methods": [method]}], "remove": []}})).await?.is_failure());
        assert!(x(&w.alice, &w.vault, json!({"act": {"target": contract, "method": method, "json_args": "{}", "one_yocto": true}})).await?.is_failure());
    }
    Ok(())
}
