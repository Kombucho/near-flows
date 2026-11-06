//! The flow keeper (scripts/keeper, TypeScript) against a local NEAR node: it
//! finds RELAY vaults through the relay factory, reads their flows from the
//! brain, and runs only the steps whose triggers have fired. Build the wasm
//! first (make build) and `npm install` in scripts/keeper.

use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId};
use serde_json::json;
use std::process::Command;

const ONE: u128 = 1_000_000_000_000_000_000;
const SCALE: u128 = 1_000_000_000_000_000_000_000_000;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

/// One round of the real keeper program, as `keeper`.
fn run_keeper(rpc: &str, factory: &AccountId, keeper: &Account) -> String {
    let dir = format!("{}/../../scripts/keeper", env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("npx")
        .args(["tsx", "src/flows-main.ts", "--send", "--once"])
        .current_dir(dir)
        .env("NEAR_RPC", rpc)
        .env("NEAR_NETWORK", "sandbox")
        .env("FACTORY_ID", factory.as_str())
        .env("KEEPER_ACCOUNT_ID", keeper.id().as_str())
        .env("KEEPER_PRIVATE_KEY", keeper.secret_key().to_string())
        .output()
        .expect("run the keeper (npm install in scripts/keeper)");
    let s = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    println!("--- keeper ---\n{s}");
    s
}

#[tokio::test]
async fn the_keeper_runs_fired_steps_and_leaves_the_rest() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    // The relay deployment: the shared brain, the relay factory bound to it, the relay code published.
    let us = root.create_subaccount("flows").initial_balance(NearToken::from_near(30)).transact().await?.into_result()?;
    let brain = root.create_subaccount("brain").initial_balance(NearToken::from_near(15)).transact().await?.into_result()?;
    brain.deploy(&wasm("brain")).await?.into_result()?;
    brain.call(brain.id(), "new").args_json(json!({"factory": us.id(), "fees": null})).transact().await?.into_result()?;
    us.deploy(&wasm("factory_relay")).await?.into_result()?;
    us.call(us.id(), "new").args_json(json!({"owner": us.id(), "brain": brain.id()})).transact().await?.into_result()?;
    us.call(us.id(), "publish").args(wasm("relay")).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let factory = us.id().clone();
    let (alice, keeper, other) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);

    // Tokens and a test exchange (1 wNEAR = 2 USDC).
    let mut tokens = vec![];
    for sym in ["wNEAR", "USDC"] {
        let t = worker.dev_deploy(&wasm("mock_ft")).await?;
        t.call("new").args_json(json!({"owner_id": root.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
            "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0})).transact().await?.into_result()?;
        tokens.push(t);
    }
    let (wnear, usdc) = (&tokens[0], &tokens[1]);
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": usdc.id()})).transact().await?.into_result()?;
    dex.call("set_rate").args_json(json!({"token_in": wnear.id(), "token_out": usdc.id(), "rate": (2 * SCALE).to_string()})).transact().await?.into_result()?;

    // Alice's vault names `keeper`; another vault names someone else (the keeper must ignore it).
    alice.call(&factory, "create_vault").args_json(json!({"name": "alice", "keeper": keeper.id()})).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    other.call(&factory, "create_vault").args_json(json!({"name": "other", "keeper": other.id()})).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let vault: AccountId = format!("alice.{factory}").parse()?;
    // Requests to a relay vault go through `x`, wrapped as {"<method>": args}.
    let x = |method: &str, args: serde_json::Value| {
        let (alice, vault, body) = (alice.clone(), vault.clone(), json!({ method: args }).to_string());
        async move { alice.call(&vault, "x").args(body.into_bytes()).max_gas().transact().await }
    };
    for t in [wnear, usdc] {
        for who in [&vault, dex.id()] {
            root.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    for (t, to, amt) in [(usdc, dex.id(), 1_000_000 * ONE), (wnear, &vault, 100 * ONE)] {
        root.call(t.id(), "ft_transfer").args_json(json!({"receiver_id": to, "amount": amt.to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    }
    x("trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id(), "methods": ["get_return"]}], "remove": []})).await?.into_result()?;
    let swap = |price: bool| json!({"label": "Swap", "out": usdc.id(), "min_out_per_in": null,
        "calls": [{"call": "ft", "receiver": dex.id(), "msg": format!(r#"{{"token_out":"{}","min_out":"0"}}"#, usdc.id()), "tgas": 50}],
        "price": if price { json!({"contract": dex.id(), "method": "get_return",
            "args": json!({"token_in": wnear.id(), "amount_in": SCALE.to_string(), "token_out": usdc.id()}).to_string(), "per": SCALE.to_string()}) } else { json!(null) }});
    x("save_flow", json!({"name": "keeper test", "nodes": [
        {"input": wnear.id(), "action": swap(false), "when": {"when": "now"}},
        {"input": wnear.id(), "action": swap(true), "when": {"when": "price_above", "price": (3 * SCALE).to_string()}},
    ]})).await?.into_result()?;
    let f = 0u32; // the vault's first flow
    for node in [0, 1] {
        x("fund", json!({"flow_id": f, "node": node, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    }
    let usdc_of = || async { let v: String = usdc.view("ft_balance_of").args_json(json!({"account_id": vault})).await?.json()?; anyhow::Ok(v.parse::<u128>()?) };

    // Round 1: the "now" swap runs; the price trigger (wants 3 USDC, pool pays 2) is skipped without a transaction.
    let rpc = worker.rpc_addr();
    let out = run_keeper(&rpc, &factory, &keeper);
    assert!(out.contains("1 of 2 vault(s) name this keeper, 1 step(s) sent"), "only Alice's vault, one step: {out}");
    assert_eq!(usdc_of().await?, 20 * ONE);
    // Round 2: nothing fired, nothing sent.
    let out = run_keeper(&rpc, &factory, &keeper);
    assert!(out.contains("0 step(s) sent"));
    // The price moves to 3: the next round runs the price-triggered swap.
    dex.call("set_rate").args_json(json!({"token_in": wnear.id(), "token_out": usdc.id(), "rate": (3 * SCALE).to_string()})).transact().await?.into_result()?;
    let out = run_keeper(&rpc, &factory, &keeper);
    assert!(out.contains("1 step(s) sent"), "{out}");
    assert_eq!(usdc_of().await?, 50 * ONE, "10 wNEAR at 2, then 10 at 3");
    Ok(())
}
