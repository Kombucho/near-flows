//! Stealth vaults: owned by a key, not an account. The owner signs commands off
//! chain; a relayer (here the keeper) sends them and pays the gas; the vault checks
//! the signature itself. The owner's account never touches the vault or the factory.
//! Run on relay vaults: the relay factory makes them, the relay's `exec` door
//! forwards the signed command, the brain checks it. (relay_ports.rs has a
//! one-test tour of the same; these keep each property on its own.)
//! Build the wasm first: make build

use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const HOUR_NS: u64 = 3_600 * 1_000_000_000;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64
}

/// The owner's key, held off chain (in the app). Signs commands for one vault.
struct Owner {
    key: SigningKey,
    nonce: u64,
}

impl Owner {
    fn new(seed: u8) -> Self {
        Owner { key: SigningKey::from_bytes(&[seed; 32]), nonce: 0 }
    }
    fn public(&self) -> String {
        format!("ed25519:{}", bs58::encode(self.key.verifying_key().as_bytes()).into_string())
    }
    /// The arguments of `exec`: the command plus the owner's signature over the vault's text.
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

async fn exec(relayer: &Account, vault: &AccountId, signed: &Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(relayer.call(vault, "exec").args_json(signed).max_gas().transact().await?)
}

fn refused(r: &ExecutionFinalResult, why: &str) {
    assert!(r.is_failure(), "expected a refusal ({why}), it went through");
    let all = format!("{:?}", r.failures());
    assert!(all.contains(why), "expected '{why}', got {all}");
}

async fn near_of(worker: &Worker<Sandbox>, who: &AccountId) -> anyhow::Result<u128> {
    Ok(worker.view_account(who).await?.balance.as_yoctonear())
}

async fn token(worker: &Worker<Sandbox>, owner: &Account) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000 * ONE).to_string(), "name": "USDC", "symbol": "USDC", "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn ft(t: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    Ok(t.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json::<String>()?.parse()?)
}

struct World {
    worker: Worker<Sandbox>,
    brain: Contract,
    factory: Contract,
    relayer: Account,
}

/// Brain + relay factory + the relay published. No vault yet: each test makes its own.
async fn world() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let us = root.create_subaccount("flows").initial_balance(NearToken::from_near(30)).transact().await?.into_result()?;
    let brain_acc = root.create_subaccount("brain").initial_balance(NearToken::from_near(15)).transact().await?.into_result()?;
    let brain = brain_acc.deploy(&wasm("brain")).await?.into_result()?;
    brain.call("new").args_json(json!({"factory": us.id()})).transact().await?.into_result()?;
    let factory = us.deploy(&wasm("factory_relay")).await?.into_result()?;
    factory.call("new").args_json(json!({"owner": us.id(), "brain": brain.id()})).transact().await?.into_result()?;
    us.call(factory.id(), "publish").args(wasm("relay")).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let relayer = worker.dev_create_account().await?;
    Ok(World { worker, brain, factory, relayer })
}

/// A brain view about one vault (the relay vault itself holds no state).
async fn bview(w: &World, vault: &AccountId, method: &str) -> anyhow::Result<Value> {
    Ok(w.brain.view(method).args_json(json!({"vault": vault})).await?.json()?)
}

/// A request through the relay's generic door, as `who`.
async fn x(who: &Account, vault: &AccountId, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(vault, "x").args(json!({ method: args }).to_string().into_bytes()).max_gas().transact().await?)
}

async fn stealth_vault(w: &World, name: &str, owner: &Owner) -> anyhow::Result<AccountId> {
    let r = w.relayer.call(w.factory.id(), "create_stealth_vault").args_json(json!({"name": name, "owner_key": owner.public(), "keeper": w.relayer.id()}))
        .deposit(NearToken::from_near(2)).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    for log in r.logs() {
        assert!(!log.contains(w.relayer.id().as_str()), "the factory logs nothing about who paid: {log}");
    }
    Ok(format!("{name}.{}", w.factory.id()).parse()?)
}

#[tokio::test]
async fn a_relayer_creates_a_vault_owned_by_a_key_and_the_factory_records_no_creator() -> anyhow::Result<()> {
    let w = world().await?;
    let owner = Owner::new(7);
    let vault = stealth_vault(&w, "q8z1x", &owner).await?;

    let key: Option<String> = serde_json::from_value(bview(&w, &vault, "get_owner_key").await?)?;
    assert_eq!(key, Some(owner.public()));
    assert_eq!(bview(&w, &vault, "get_vault").await?["owner"], vault.as_str(), "a stealth vault is its own owner");
    assert!(w.worker.view_access_keys(&vault).await?.is_empty(), "no keys: nobody can redeploy it");
    let by_relayer: Option<String> = w.factory.view("get_vault").args_json(json!({"owner": w.relayer.id()})).await?.json()?;
    assert!(by_relayer.is_none(), "no creator -> vault mapping");
    let listed: Vec<String> = w.factory.view("get_vaults").args_json(json!({"from_index": 0, "limit": 10})).await?.json()?;
    assert_eq!(listed, vec![vault.to_string()], "keepers can still find it");

    // A taken name refunds the relayer; named vaults still work next to stealth ones.
    let before = near_of(&w.worker, w.relayer.id()).await?;
    let r = w.relayer.call(w.factory.id(), "create_stealth_vault").args_json(json!({"name": "q8z1x", "owner_key": Owner::new(8).public(), "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    assert!(!r.json::<bool>()?, "the name is taken");
    // (saturating: a gas refund from an earlier call may land in between)
    assert!(before.saturating_sub(near_of(&w.worker, w.relayer.id()).await?) <NearToken::from_millinear(10).as_yoctonear(), "deposit refunded");
    let alice = w.worker.dev_create_account().await?;
    alice.call(w.factory.id(), "create_vault").args_json(json!({"name": "alice", "keeper": null})).deposit(NearToken::from_near(1)).max_gas().transact().await?.into_result()?;
    let named: Option<String> = w.factory.view("get_vault").args_json(json!({"owner": alice.id()})).await?.json()?;
    assert_eq!(named, Some(format!("alice.{}", w.factory.id())));
    let none: Option<String> = serde_json::from_value(bview(&w, &format!("alice.{}", w.factory.id()).parse()?, "get_owner_key").await?)?;
    assert!(none.is_none(), "a named vault has no owner key");
    Ok(())
}

#[tokio::test]
async fn signed_commands_run_once_before_expiry_and_only_for_their_own_vault() -> anyhow::Result<()> {
    let w = world().await?;
    let mut owner = Owner::new(7);
    let vault = stealth_vault(&w, "q8z1x", &owner).await?;
    let twin = stealth_vault(&w, "m3k9v", &owner).await?;
    let wrap: AccountId = "wrap.test.near".parse()?;

    // A signed command runs as the owner, sent by the relayer.
    let trust = owner.sign(&vault, "trust", json!({"add": [{"contract": wrap, "methods": ["near_deposit"]}], "remove": []}));
    let r = exec(&w.relayer, &vault, &trust).await?;
    println!("GAS exec(trust): {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    let nodes = json!([{"input": "near", "action": {"label": "wrap", "calls": [{"call": "fn", "contract": wrap, "method": "near_deposit",
        "args": "{}", "deposit": {"kind": "input"}, "tgas": 10}], "out": wrap, "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    let save = owner.sign(&vault, "save_flow", json!({"name": "first", "nodes": nodes}));
    let id: u32 = exec(&w.relayer, &vault, &save).await?.into_result()?.json()?;
    assert_eq!(id, 0, "the command's return value comes back");
    assert_eq!(bview(&w, &vault, "get_vault").await?["flows"][0]["name"], "first");
    assert_eq!(bview(&w, &vault, "get_nonce").await?, "2");

    // Replayed: refused. An older nonce after a newer one: refused.
    refused(&exec(&w.relayer, &vault, &save).await?, "nonce already used");
    let mut stale = owner.sign(&vault, "drain", json!({"flow_id": id}));
    let newer = owner.sign(&vault, "drain", json!({"flow_id": id}));
    exec(&w.relayer, &vault, &newer).await?.into_result()?;
    refused(&exec(&w.relayer, &vault, &stale).await?, "nonce already used");

    // Expired: refused.
    let old = owner.sign_with(&vault, "drain", json!({"flow_id": id}), 100, now_ns() - HOUR_NS);
    refused(&exec(&w.relayer, &vault, &old).await?, "expired");

    // Signed by another key: refused.
    let mut mallory = Owner::new(66);
    mallory.nonce = 1_000;
    refused(&exec(&w.relayer, &vault, &mallory.sign(&vault, "set_keeper", json!({"keeper": null}))).await?, "bad signature");

    // Signed for another vault (same owner key): useless here, and vice versa.
    let for_twin = owner.sign(&twin, "set_keeper", json!({"keeper": null}));
    refused(&exec(&w.relayer, &vault, &for_twin).await?, "bad signature");
    exec(&w.relayer, &twin, &for_twin).await?.into_result()?;

    // Tampered by the relayer: any changed byte breaks the signature.
    let mut t = owner.sign(&vault, "set_keeper", json!({"keeper": w.relayer.id()}));
    t["args"] = json!(json!({"keeper": "mallory.test.near"}).to_string());
    refused(&exec(&w.relayer, &vault, &t).await?, "bad signature");
    stale["nonce"] = json!("999");
    refused(&exec(&w.relayer, &vault, &stale).await?, "bad signature");
    let mut starve = owner.sign_with(&vault, "set_keeper", json!({"keeper": null}), 100, now_ns() + HOUR_NS);
    // The relay needs 150 for 100 signed: 100 + 5 (a relay round) + 40 (first round) + 5 (exec).
    // (The full vault asked for 110 with 50 attached; 140 is just under the relay's need.)
    let r = w.relayer.call(&vault, "exec").args_json(&starve).gas(near_workspaces::types::Gas::from_tgas(140)).transact().await?;
    refused(&r, "attach at least 150 Tgas");
    starve["tgas"] = json!(5);
    refused(&exec(&w.relayer, &vault, &starve).await?, "bad signature");

    // Requests that are not owner commands can't run through exec, even signed: here a
    // re-init that would hand the vault to another key. (The relay has no vault callbacks
    // to name, the full vault's on_withdraw_to_balance case; its callbacks are brain-internal.)
    let reinit = owner.sign(&vault, "init_stealth", json!({"owner_key": Owner::new(66).public(), "keeper": null}));
    refused(&exec(&w.relayer, &vault, &reinit).await?, "not a command");
    Ok(())
}

#[tokio::test]
async fn the_relayer_can_do_nothing_but_relay() -> anyhow::Result<()> {
    let w = world().await?;
    let mut owner = Owner::new(7);
    let vault = stealth_vault(&w, "q8z1x", &owner).await?;
    let r = &w.relayer;
    for (m, a) in [
        ("set_keeper", json!({"keeper": null})),
        ("trust", json!({"add": [{"contract": "wrap.test.near", "methods": []}], "remove": []})),
        ("withdraw", json!({"token": "near", "amount": "1"})),
        ("withdraw_to", json!({"token": "near", "amount": "1", "to": r.id(), "via_intents": false})),
        ("set_limit", json!({"token": "near", "max": null, "window_ns": 0})),
        ("set_owner_key", json!({"owner_key": Owner::new(9).public()})),
    ] {
        refused(&x(r, &vault, m, a).await?, "owner only");
    }
    // Nor can it impersonate the vault's own callback (the relay's `cb`), nor a callback name.
    refused(&r.call(&vault, "cb").args(vec![1, 0, 0, 0, 0, 0]).max_gas().transact().await?, "only the vault's own callback");
    refused(&x(r, &vault, "on_withdraw_to_balance", json!({"token": "near", "amount": "1", "to": r.id(), "via_intents": false})).await?, "bad request");

    // The owner can rotate the key; the old one stops working at once.
    let rotate = owner.sign(&vault, "set_owner_key", json!({"owner_key": Owner::new(9).public()}));
    exec(r, &vault, &rotate).await?.into_result()?;
    refused(&exec(r, &vault, &owner.sign(&vault, "set_keeper", json!({"keeper": null}))).await?, "bad signature");
    let mut new_owner = Owner::new(9);
    new_owner.nonce = owner.nonce;
    exec(r, &vault, &new_owner.sign(&vault, "set_keeper", json!({"keeper": null}))).await?.into_result()?;
    Ok(())
}

#[tokio::test]
async fn the_owner_withdraws_to_any_account_it_signs_for_or_into_intents() -> anyhow::Result<()> {
    let w = world().await?;
    let mut owner = Owner::new(7);
    let vault = stealth_vault(&w, "q8z1x", &owner).await?;
    let root = w.worker.dev_create_account().await?;
    let fresh = w.worker.dev_create_account().await?;

    // NEAR to a fresh account the owner signs for.
    let before = near_of(&w.worker, fresh.id()).await?;
    let out = owner.sign(&vault, "withdraw_to", json!({"token": "near", "amount": (NearToken::from_near(1).as_yoctonear()).to_string(), "to": fresh.id(), "via_intents": false}));
    exec(&w.relayer, &vault, &out).await?.into_result()?;
    assert_eq!(near_of(&w.worker, fresh.id()).await? - before, NearToken::from_near(1).as_yoctonear());

    // A command that fails still uses up its nonce: it can't be replayed later, when it would succeed.
    let too_much = owner.sign(&vault, "withdraw_to", json!({"token": "near", "amount": NearToken::from_near(50).as_yoctonear().to_string(), "to": fresh.id(), "via_intents": false}));
    refused(&exec(&w.relayer, &vault, &too_much).await?, "not enough free NEAR");
    refused(&exec(&w.relayer, &vault, &too_much).await?, "nonce already used");

    // Tokens to a fresh account.
    let usdc = token(&w.worker, &root).await?;
    for who in [vault.clone(), fresh.id().clone()] {
        root.call(usdc.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    }
    root.call(usdc.id(), "ft_transfer").args_json(json!({"receiver_id": vault, "amount": (500 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    let out = owner.sign(&vault, "withdraw_to", json!({"token": usdc.id(), "amount": (100 * ONE).to_string(), "to": fresh.id(), "via_intents": false}));
    exec(&w.relayer, &vault, &out).await?.into_result()?;
    assert_eq!(ft(&usdc, fresh.id()).await?, 100 * ONE);

    // Into NEAR Intents (the real verifier code), credited to an account that never touched the vault.
    let verifier = w.worker.dev_deploy(&std::fs::read(format!("{}/protocols/verifier.tidepool-sandbox.testnet.wasm", env!("CARGO_MANIFEST_DIR"))).expect("scripts/fetch-protocols.sh")).await?;
    verifier.call("new").args_json(json!({"config": {"wnear_id": usdc.id(), "fees": {"fee": 1, "fee_collector": root.id()}, "roles": {}}})).max_gas().transact().await?.into_result()?;
    root.call(usdc.id(), "storage_deposit").args_json(json!({"account_id": verifier.id()})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    let signer = w.worker.dev_deploy(&wasm("mock_signer")).await?;
    let setup = owner.sign(&vault, "intents_setup", json!({"verifier": verifier.id(), "signer": signer.id()}));
    exec(&w.relayer, &vault, &setup).await?.into_result()?;
    let elsewhere: AccountId = "a-fresh-intents-user.test.near".parse()?;
    let out = owner.sign(&vault, "withdraw_to", json!({"token": usdc.id(), "amount": (150 * ONE).to_string(), "to": elsewhere, "via_intents": true}));
    exec(&w.relayer, &vault, &out).await?.into_result()?;
    let mt: String = verifier.view("mt_balance_of").args_json(json!({"account_id": elsewhere, "token_id": format!("nep141:{}", usdc.id())})).await?.json()?;
    assert_eq!(mt, (150 * ONE).to_string(), "credited on Intents to the account the owner signed for");
    assert_eq!(ft(&usdc, &vault).await?, 250 * ONE);
    Ok(())
}
