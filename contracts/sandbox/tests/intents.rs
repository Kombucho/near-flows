//! NEAR Intents from a keyless vault, against the REAL intents.near code
//! (v0.4.5; fetched from the Tidepool testnet sandbox, same code hash as mainnet).
//! The vault signs only intents it built itself, via Chain Signatures (here a
//! stand-in signer that really signs Ed25519); a solver matches; the real
//! verifier checks both signatures and settles.

use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;

fn ours(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"))).expect("make build")
}
fn intents_wasm() -> Vec<u8> {
    std::fs::read(format!("{}/protocols/verifier.tidepool-sandbox.testnet.wasm", env!("CARGO_MANIFEST_DIR"))).expect("scripts/fetch-protocols.sh")
}

async fn token(worker: &Worker<Sandbox>, owner: &Account, symbol: &str) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&ours("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000 * ONE).to_string(), "name": symbol, "symbol": symbol, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn mt(verifier: &Contract, who: &AccountId, token: &Contract) -> anyhow::Result<u128> {
    let v: String = verifier.view("mt_balance_of").args_json(json!({"account_id": who, "token_id": format!("nep141:{}", token.id())})).await?.json()?;
    Ok(v.parse()?)
}

async fn ft(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

fn signed_by_solver(key: &SigningKey, solver: &AccountId, verifier: &AccountId, salt: [u8; 4], give: (&AccountId, u128), get: (&AccountId, u128), seq: u8) -> Value {
    let deadline = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64) + 600_000_000_000;
    let msg = intentsig::swap_message(solver.as_str(), deadline, give.0.as_str(), give.1, get.0.as_str(), get.1);
    let nonce = intentsig::versioned_nonce(salt, deadline as i64, [seq; 15]);
    let sig = key.sign(&intentsig::nep413_hash(&msg, &nonce, verifier.as_str()));
    let s = intentsig::signed_json(&msg, &nonce, verifier.as_str(), &bs58::encode(key.verifying_key().as_bytes()).into_string(), &bs58::encode(sig.to_bytes()).into_string());
    serde_json::from_str(&s).unwrap()
}

async fn execute(who: &Account, verifier: &Contract, signed: Vec<Value>) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(verifier.id(), "execute_intents").args_json(json!({"signed": signed})).max_gas().transact().await?)
}

#[tokio::test]
async fn a_keyless_vault_swaps_on_real_intents_with_intents_it_signed_itself() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.dev_create_account().await?;
    let (alice, keeper, solver) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    let (a, b) = (token(&worker, &root, "AAA").await?, token(&worker, &root, "BBB").await?);
    let verifier = worker.dev_deploy(&intents_wasm()).await?;
    verifier.call("new").args_json(json!({"config": {"wnear_id": a.id(), "fees": {"fee": 1, "fee_collector": root.id()}, "roles": {}}})).max_gas().transact().await?.into_result()?;
    let signer = worker.dev_deploy(&ours("mock_signer")).await?;
    let vault = worker.dev_deploy(&ours("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    for t in [&a, &b] {
        for who in [verifier.id(), vault.id(), solver.id()] {
            root.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    root.call(a.id(), "ft_transfer").args_json(json!({"receiver_id": vault.id(), "amount": (1_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    root.call(b.id(), "ft_transfer").args_json(json!({"receiver_id": solver.id(), "amount": (1_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;

    // 1. The vault registers its MPC key on its own Intents account (no private key anywhere).
    alice.call(vault.id(), "intents_setup").args_json(json!({"verifier": verifier.id(), "signer": signer.id()})).max_gas().transact().await?.into_result()?;
    let key = vault.view("get_intents").await?.json::<Value>()?["key"].as_str().unwrap().to_string();
    let keys: Vec<String> = verifier.view("public_keys_of").args_json(json!({"account_id": vault.id()})).await?.json()?;
    assert_eq!(keys, vec![format!("ed25519:{key}")], "the verifier knows the vault's key");

    // 2. Deposit 300 A into Intents with a flow node (a plain recipe: ft_transfer_call, msg "").
    alice.call(vault.id(), "trust").args_json(json!({"add": [{"contract": a.id()}, {"contract": b.id()}, {"contract": verifier.id()}], "remove": []})).transact().await?.into_result()?;
    let f: u32 = alice.call(vault.id(), "save_flow").args_json(json!({"name": "to intents", "nodes": [{"input": a.id(), "when": {"when": "now"},
        "action": {"label": "Deposit to Intents", "calls": [{"call": "ft", "receiver": verifier.id(), "msg": "", "tgas": 50}], "out": null, "min_out_per_in": null, "price": null}}]}))
        .max_gas().transact().await?.into_result()?.json()?;
    alice.call(vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": (300 * ONE).to_string()})).max_gas().transact().await?.into_result()?;
    keeper.call(vault.id(), "step").args_json(json!({"flow_id": f, "node": 0})).max_gas().transact().await?.into_result()?;
    assert_eq!(mt(&verifier, vault.id(), &a).await?, 300 * ONE);

    // The solver: its own key on Intents, 1,000 B deposited.
    let skey = SigningKey::from_bytes(&[42; 32]);
    solver.call(verifier.id(), "add_public_key").args_json(json!({"public_key": format!("ed25519:{}", bs58::encode(skey.verifying_key().as_bytes()).into_string())}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    solver.call(b.id(), "ft_transfer_call").args_json(json!({"receiver_id": verifier.id(), "amount": (1_000 * ONE).to_string(), "msg": ""}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;

    // 3. The owner allows A -> B with a floor of 0.95 B per A; at most 200 A per intent.
    alice.call(vault.id(), "intents_allow").args_json(json!({"pair": {"token_in": a.id(), "token_out": b.id(), "max_in": (200 * ONE).to_string(), "floor_out": "95", "floor_per_in": "100"}}))
        .transact().await?.into_result()?;
    // The keeper asks for 100 A -> "at least 1 wei" of B: the vault signs for at least 95 B (the floor).
    let r = keeper.call(vault.id(), "intents_sign_swap").args_json(json!({"token_in": a.id(), "token_out": b.id(), "amount_in": (100 * ONE).to_string(), "min_out": "1", "valid_secs": 600}))
        .max_gas().transact().await?;
    println!("GAS sign an intent: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    let ev = r.logs().iter().find(|l| l.contains("intents_signed")).cloned().unwrap_or_else(|| panic!("{:?} {:?}", r.logs(), r.failures()));
    let vault_signed = serde_json::from_str::<Value>(ev.trim_start_matches("EVENT_JSON:"))?["data"][0]["signed"].clone();
    println!("vault's intent: {}", vault_signed["payload"]["message"]);
    assert!(vault_signed["payload"]["message"].as_str().unwrap().contains(&format!(r#""nep141:{}":"{}""#, b.id(), 95 * ONE)));
    let salt: String = verifier.view("current_salt").await?.json()?;
    let salt: [u8; 4] = hex::decode(salt)?.try_into().unwrap();

    // 4a. A greedy solver offering 90 B can't settle: the vault's own intent demands 95.
    let greedy = signed_by_solver(&skey, solver.id(), verifier.id(), salt, (b.id(), 90 * ONE), (a.id(), 100 * ONE - 100), 1);
    let r = execute(&keeper, &verifier, vec![vault_signed.clone(), greedy]).await?;
    assert!(r.is_failure(), "shortchanging the vault must not settle");
    // 4b. A fair solver: gives G of B so that G minus its 0.0001% fee (charged on G, rounded up) is
    //     exactly 95; takes 100 A minus the fee on the vault's side. The real verifier settles.
    let gross = |net: u128| {
        let g0 = net * 1_000_000 / 999_999; // close: then step to the exact value
        (g0.saturating_sub(10)..g0 + 10).find(|g| g - g.div_ceil(1_000_000) == net).unwrap()
    };
    let fee_b = gross(95 * ONE) - 95 * ONE;
    let fair = signed_by_solver(&skey, solver.id(), verifier.id(), salt, (b.id(), 95 * ONE + fee_b), (a.id(), 100 * ONE - (100 * ONE).div_ceil(1_000_000)), 2);
    let r = execute(&keeper, &verifier, vec![vault_signed.clone(), fair.clone()]).await?;
    println!("GAS execute_intents (vault + solver): {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(mt(&verifier, vault.id(), &a).await?, 200 * ONE, "gave exactly 100 A");
    assert_eq!(mt(&verifier, vault.id(), &b).await?, 95 * ONE, "got 95 B");
    // 4c. The same signed intent can't be used twice (nonce spent).
    let again = signed_by_solver(&skey, solver.id(), verifier.id(), salt, (b.id(), 95 * ONE + fee_b), (a.id(), 100 * ONE - (100 * ONE).div_ceil(1_000_000)), 3);
    assert!(execute(&keeper, &verifier, vec![vault_signed, again]).await?.is_failure(), "replay refused");

    // 5. Withdraw the 95 B: it can only go back to the vault.
    keeper.call(vault.id(), "intents_withdraw").args_json(json!({"token": b.id(), "amount": (95 * ONE).to_string()})).max_gas().transact().await?.into_result()?;
    assert_eq!(ft(&b, vault.id()).await?, 95 * ONE);
    assert_eq!(mt(&verifier, vault.id(), &b).await?, 0);

    // 6. Refusals: over the per-intent limit, a pair nobody allowed, a stranger.
    for (who, args) in [
        (&keeper, json!({"token_in": a.id(), "token_out": b.id(), "amount_in": (201 * ONE).to_string(), "min_out": "1", "valid_secs": 600})),
        (&keeper, json!({"token_in": b.id(), "token_out": a.id(), "amount_in": "1", "min_out": "1", "valid_secs": 600})),
        (&solver, json!({"token_in": a.id(), "token_out": b.id(), "amount_in": "1", "min_out": "1", "valid_secs": 600})),
    ] {
        let r = who.call(vault.id(), "intents_sign_swap").args_json(args).max_gas().transact().await?;
        assert!(r.is_failure() && !r.logs().iter().any(|l| l.contains("intents_signed")));
    }
    // 7. A market floor: A and B both at $1 per the oracle, 1% slippage. The keeper asks for 1 wei;
    //    the hard floor says 95; the market says 99: the vault signs for 99.
    let pyth = worker.dev_deploy(&ours("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let ids = ["1111111111111111111111111111111111111111111111111111111111111111", "2222222222222222222222222222222222222222222222222222222222222222"];
    for id in ids {
        alice.call(pyth.id(), "set_price").args_json(json!({"price_id": id, "price": "100000000", "conf": "0", "expo": -8, "ema": "100000000", "age_s": 0}))
            .transact().await?.into_result()?;
    }
    alice.call(vault.id(), "oracle_setup").args_json(json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "a", "contract": pyth.id(), "price_id": ids[0], "backup": {"kind": "ema"}},
        {"name": "b", "contract": pyth.id(), "price_id": ids[1], "backup": {"kind": "ema"}}]}})).transact().await?.into_result()?;
    alice.call(vault.id(), "intents_allow").args_json(json!({"pair": {"token_in": a.id(), "token_out": b.id(), "max_in": (200 * ONE).to_string(), "floor_out": "95", "floor_per_in": "100",
        "market": {"sell": "a", "buy": "b", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}}})).transact().await?.into_result()?;
    let r = keeper.call(vault.id(), "intents_sign_swap").args_json(json!({"token_in": a.id(), "token_out": b.id(), "amount_in": (100 * ONE).to_string(), "min_out": "1", "valid_secs": 600}))
        .max_gas().transact().await?;
    let ev = r.logs().iter().find(|l| l.contains("intents_signed")).cloned().unwrap_or_else(|| panic!("{:?} {:?}", r.logs(), r.failures()));
    assert!(ev.contains(&format!(r#"\"nep141:{}\":\"{}\""#, b.id(), 99 * ONE)), "signed for the market floor: {ev}");

    // Recipes can never move Intents balances directly (mt_transfer / ft_withdraw are fund-moving).
    let r = alice.call(vault.id(), "trust").args_json(json!({"add": [{"contract": verifier.id(), "methods": ["ft_withdraw"]}], "remove": []})).transact().await?;
    assert!(r.is_failure(), "ft_withdraw can't be handed to recipes");
    // ...nor can the MPC signer's `sign` (it would skip every byte check), nor Intents keys.
    for (contract, method) in [(signer.id(), "sign"), (verifier.id(), "add_public_key"), (verifier.id(), "mt_withdraw")] {
        let r = alice.call(vault.id(), "trust").args_json(json!({"add": [{"contract": contract, "methods": [method]}], "remove": []})).transact().await?;
        assert!(r.is_failure(), "{method} must never be callable by a recipe");
        let r = alice.call(vault.id(), "act").args_json(json!({"target": contract, "method": method, "json_args": "{}", "one_yocto": true})).max_gas().transact().await?;
        assert!(r.is_failure(), "{method} must never be callable by a one-off act either");
    }
    Ok(())
}
