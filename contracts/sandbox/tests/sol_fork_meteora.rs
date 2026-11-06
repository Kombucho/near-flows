//! A Meteora DLMM liquidity position run by the vault, executed for real on a local fork of
//! Solana mainnet (surfpool): the vault opens a position in the SOL/USDC pool with USDC only
//! (the owner's own wallet co-signs the open), a trader's swap earns it fees, the vault sweeps
//! the fees into its own token accounts, then withdraws everything and closes the position.
//! The keeper builds every transaction with Meteora's own SDK; the vault checks every byte
//! against the templates published in nodes/sol-templates.json and signs (test signer: a real
//! Ed25519 key standing in for the MPC network).
//!
//! Needs surfpool and the keeper's node modules (scripts/keeper: npm install --ignore-scripts).

mod common;

use common::{sol_sig, sol_template, wasm, SolFork};
use serde_json::{json, Value};

const POOL: &str = "5rCf1DM8LjKTw4YqhnoLcngyZYeNnQqztScTogYHAS6";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const WSOL: &str = "So11111111111111111111111111111111111111112";
const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

fn units(v: &Value, k: &str) -> u128 {
    v[k].as_str().unwrap_or_else(|| panic!("{k} in {v}")).parse().unwrap()
}

#[tokio::test]
async fn the_vault_runs_a_meteora_position_on_a_solana_fork() -> anyhow::Result<()> {
    let Some(fork) = SolFork::start(9100).await else { return Ok(()) };

    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "sol_setup").args_json(json!({"signer": signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32}))
        .max_gas().transact().await?.into_result()?;
    let me = vault.view("get_sol").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("vault's Solana address: {me}");

    // The owner's own Solana wallet: it co-signs LP opens (the app holds it; here, the test).
    let owner = fork.helper(&["keypair"]);
    let (owner_key, owner_secret) = (owner["pubkey"].as_str().unwrap().to_string(), owner["secret"].as_str().unwrap().to_string());
    alice.call(vault.id(), "sol_owner_wallet").args_json(json!({"wallet": owner_key})).transact().await?.into_result()?;
    for name in ["dlmm_open", "dlmm_add_usdc", "dlmm_claim", "dlmm_remove", "dlmm_close"] {
        alice.call(vault.id(), "sol_template").args_json(json!({"template": sol_template(name, &[])})).transact().await?.into_result()?;
    }
    alice.call(vault.id(), "set_limit").args_json(json!({"token": format!("sol:{USDC}"), "max": "300000000", "window_ns": 86_400_000_000_000u64}))
        .transact().await?.into_result()?;

    // On the fork: 1 SOL for fees and rent, 500 USDC in the vault's own token account.
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": 1_000_000_000u64}]));
    fork.rpc_call("surfnet_setTokenAccount", json!([me, USDC, {"amount": 500_000_000u64}, TOKEN]));

    let sign = |msg: String| {
        let (k, v) = (keeper.clone(), vault.id().clone());
        async move { k.call(&v, "sol_sign_tx").args_json(json!({"message": msg})).max_gas().transact().await }
    };
    let refused = |r: &near_workspaces::result::ExecutionFinalResult, why: &str| {
        assert!(sol_sig(r).is_none(), "signed, expected a refusal ({why})");
        assert!(format!("{:?}", r.failures()).contains(why), "expected '{why}': {:?}", r.failures());
    };
    let usdc = |who: &str| units(&fork.helper(&["balance", who, USDC]), "amount");

    // 1. Open: 100 USDC over the 10 bins at and below the price (a bid ladder), built by Meteora's SDK.
    let open = fork.helper(&["dlmm-open", &me, &owner_key, POOL, "100000000", "10"]);
    let (msg, position, pos_secret) = (open["message"].as_str().unwrap(), open["position"].as_str().unwrap().to_string(), open["positionSecret"].as_str().unwrap());
    println!("open: position {position}, bins {}..{}", open["minBinId"], open["maxBinId"]);

    // Without the owner's wallet in it, the vault won't sign an open, whoever asks.
    let alone = fork.helper(&["dlmm-open", &me, "-", POOL, "100000000", "10"]);
    refused(&sign(alone["message"].as_str().unwrap().to_string()).await?, "Signers");
    // Over the 200 USDC cap of the template.
    let big = fork.helper(&["dlmm-open", &me, &owner_key, POOL, "250000000", "10"]);
    refused(&sign(big["message"].as_str().unwrap().to_string()).await?, "over the cap");
    // Another owner: a stranger's key in the owner's place.
    let stranger = fork.helper(&["keypair"]);
    let fake = fork.helper(&["dlmm-open", &me, stranger["pubkey"].as_str().unwrap(), POOL, "100000000", "10"]);
    refused(&sign(fake["message"].as_str().unwrap().to_string()).await?, "Signers");

    let r = sign(msg.to_string()).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("open not signed: {:?} {:?}", r.logs(), r.failures()));
    // The vault's signature alone doesn't land it: the chain wants the owner's too.
    let half = fork.helper(&["send", msg, &sig, pos_secret]);
    assert!(half["signature"].is_null(), "landed without the owner's signature: {half}");
    let sent = fork.helper(&["send", msg, &sig, pos_secret, &owner_secret]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "open not executed: {sent}");
    let p = fork.helper(&["dlmm-position", POOL, &position]);
    println!("opened on the fork: position holds {} USDC units, {} SOL units; vault has {} USDC units left (tx {})", p["y"], p["x"], usdc(&me), sent["signature"]);
    assert_eq!(usdc(&me), 400_000_000, "100 USDC went into the position");
    // The active bin holds both tokens, so a deposit into it is shared out in both: value it at the pool's price.
    let price: f64 = fork.helper(&["dlmm-info", POOL])[0]["price"].as_str().unwrap().parse()?;
    let value = units(&p, "y") as f64 + units(&p, "x") as f64 * price / 1000.0;
    println!("position value: {:.2} USDC at {price:.2} USDC/SOL", value / 1e6);
    assert!(value >= 99_000_000.0, "the position holds the 100 USDC: {p}");

    // 2. A trader sells 100 SOL into the pool: the vault's bins earn fees (in SOL).
    let trader = fork.helper(&["keypair"]);
    fork.rpc_call("surfnet_setAccount", json!([trader["pubkey"], {"lamports": 200_000_000_000u64}]));
    let swap = fork.helper(&["dlmm-swap", trader["secret"].as_str().unwrap(), POOL, "100000000000"]);
    assert!(swap["err"].is_null() && swap["signature"].is_string(), "trader's swap: {swap}");
    let p = fork.helper(&["dlmm-position", POOL, &position]);
    println!("after a 100 SOL sell: active bin {}, position {} SOL units + {} USDC units, fees {} SOL units + {} USDC units",
        swap["activeAfter"], p["x"], p["y"], p["feeX"], p["feeY"]);
    assert!(units(&p, "feeX") > 0, "the vault's bins earned fees: {p}");

    // 3. Claim: the fees, swept into the vault's own token accounts. Only the vault signs.
    let claim = fork.helper(&["dlmm-claim", &me, POOL, &position]);
    let thief = fork.helper(&["keypair"]);
    let thief_ata = fork.helper(&["ata", thief["pubkey"].as_str().unwrap(), WSOL]);
    let mine_ata = fork.helper(&["ata", &me, WSOL]);
    let stolen = fork.helper(&["swap-key", claim["message"].as_str().unwrap(), mine_ata["ata"].as_str().unwrap(), thief_ata["ata"].as_str().unwrap()]);
    refused(&sign(stolen["message"].as_str().unwrap().to_string()).await?, "dlmm_claim: account 5");
    let r = sign(claim["message"].as_str().unwrap().to_string()).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("claim not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", claim["message"].as_str().unwrap(), &sig]);
    assert!(sent["err"].is_null() && sent["signature"].is_string(), "claim not executed: {sent}");
    let fees = units(&fork.helper(&["balance", &me, WSOL]), "amount");
    println!("claimed on the fork: the vault's own SOL token account holds {fees} lamports of fees (tx {})", sent["signature"]);
    assert!(fees > 0);

    // 4. Withdraw everything and close: tokens back to the vault, the position's rent back to the vault.
    let before = fork.helper(&["balance", &me, USDC]);
    let close = fork.helper(&["dlmm-close", &me, POOL, &position]);
    let mine_usdc = fork.helper(&["ata", &me, USDC]);
    let stolen = fork.helper(&["swap-key", close["message"].as_str().unwrap(), mine_usdc["ata"].as_str().unwrap(), thief_ata["ata"].as_str().unwrap()]);
    refused(&sign(stolen["message"].as_str().unwrap().to_string()).await?, "dlmm_remove: account 4");
    let r = sign(close["message"].as_str().unwrap().to_string()).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("close not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", close["message"].as_str().unwrap(), &sig]);
    assert!(sent["err"].is_null() && sent["signature"].is_string(), "close not executed: {sent}");
    let after = fork.helper(&["balance", &me, USDC]);
    let sol_after = units(&fork.helper(&["balance", &me, WSOL]), "amount");
    let gone = fork.rpc_call("getAccountInfo", json!([position, {"encoding": "base64"}]));
    println!("closed on the fork: vault {} -> {} USDC units, {} -> {} lamports, {} SOL units in its token account; position account: {}",
        units(&before, "amount"), units(&after, "amount"), before["lamports"], after["lamports"], sol_after, gone["result"]["value"]);
    assert!(gone["result"]["value"].is_null(), "the position is closed");
    assert!(after["lamports"].as_u64() > before["lamports"].as_u64(), "the position's rent came back to the vault");
    assert!(units(&after, "amount") > units(&before, "amount") || sol_after > fees, "the liquidity came back to the vault");
    Ok(())
}
