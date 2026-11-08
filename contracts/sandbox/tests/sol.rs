//! Solana through Chain Signatures: the vault learns its Solana address from
//! the signer, checks every byte of a Jupiter swap message against the owner's
//! policy, counts the amount against its limit, and gets it signed. Here the
//! signer is a test stand-in that really signs with Ed25519, so we verify the
//! signature exactly as a Solana validator would. Run on a relay vault: the
//! brain checks the message, the vault asks for the signature with its own path.

mod common;
mod relayw;

use base64::Engine;
use common::wasm;
use relayw::*;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde_json::{json, Value};
use solguard::testkit::*;
use solguard::*;

const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[tokio::test]
async fn the_vault_signs_only_swaps_its_owner_allows() -> anyhow::Result<()> {
    let RelayWorld { worker, brain, alice, keeper, signer, vault, .. } = relay_world().await?;
    let mallory = worker.dev_create_account().await?;

    // 1. The vault asks the signer for its own Solana address (nobody can hand it a fake one).
    x(&alice, &vault, "sol_setup", json!({"signer": signer.id(), "max_cu_price": 100_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let cfg = view(&brain, &vault, "get_sol", Value::Null).await?;
    let address = cfg["address"].as_str().unwrap().to_string();
    let expected: String = signer.view("derived_public_key").args_json(json!({"path": "solana-1", "predecessor": vault, "domain_id": 1})).await?.json()?;
    assert_eq!(format!("ed25519:{address}"), expected);
    println!("vault's Solana address: {address}");
    let me = pubkey(&address);

    // 2. The owner allows USDC -> SOL, at most 50 USDC per swap, at least 0.004 SOL per USDC.
    //    (USDC 6 decimals, SOL 9: 0.004 SOL/USDC = 4 lamports per micro-USDC -> 4e12 scaled.)
    x(&alice, &vault, "sol_allow", json!({"pair": {"in_mint": USDC_MINT, "out_mint": WSOL_MINT,
        "max_in": "50000000", "min_out_per_in_e12": "4000000000000"}})).await?.into_result()?;
    assert!(x(&keeper, &vault, "sol_allow", json!({"pair": {"in_mint": USDC_MINT, "out_mint": WSOL_MINT,
        "max_in": "1", "min_out_per_in_e12": "1"}})).await?.is_failure(), "only the owner sets policy");
    x(&alice, &vault, "set_limit", json!({"token": format!("sol:{USDC_MINT}"), "max": "60000000", "window_ns": 3_600_000_000_000u64})).await?.into_result()?;

    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let sign = |who: &near_workspaces::Account, msg: Vec<u8>| {
        let (who, vault_id, m) = (who.clone(), vault.clone(), b64(&msg));
        async move { x(&who, &vault_id, "sol_sign_swap", json!({"message": m})).await }
    };
    let (usdc, sol) = (pubkey(USDC_MINT), pubkey(WSOL_MINT));

    // 3. An honest swap: 10 USDC for ~0.05 SOL. Signed, and the signature verifies.
    let msg = honest_for(me, usdc, sol, 10_000_000, 50_000_000);
    let r = sign(&keeper, msg.clone()).await?;
    println!("GAS sign a Solana swap: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    let signed = r.logs().iter().find(|l| l.contains("sol_signed")).cloned().unwrap_or_else(|| panic!("{:?} {:?}", r.logs(), r.failures()));
    let sig_hex = signed.split(r#""signature":""#).nth(1).unwrap().split('"').next().unwrap();
    let sig = Signature::from_slice(&hex::decode(sig_hex)?)?;
    VerifyingKey::from_bytes(&me)?.verify(&msg, &sig).expect("a valid Solana signature by the vault's address");
    assert!(r.logs().iter().any(|l| l.contains(r#""in_amount":"10000000""#)), "the amount read from the message is what's reported");

    // 4. Attacks: each refused, nothing signed.
    let thief = key(66);
    let mut transfer = vec![2u8, 0, 0, 0];
    transfer.extend(1_000_000_000u64.to_le_bytes());
    let drain = Tx::new(me).ix(pubkey(SYSTEM_PROGRAM), vec![me, thief], transfer).bytes();
    let to_thief = Tx::new(me).ix(pubkey(JUPITER_V6), shared_accounts(me, ata_of(&me, &usdc), ata_of(&thief, &sol), usdc, sol, pubkey(JUPITER_V6)),
        jup_data("shared_accounts_route", 10_000_000, 50_000_000, 50, 0)).bytes();
    let bad_price = honest_for(me, usdc, sol, 10_000_000, 30_000_000); // 0.003 SOL/USDC < floor
    let too_big = honest_for(me, usdc, sol, 51_000_000, 300_000_000);
    for (name, m) in [("drain", drain), ("proceeds to a thief", to_thief), ("price under the floor", bad_price), ("over 50 USDC", too_big)] {
        let r = sign(&keeper, m).await?;
        assert!(format!("{:?}", r.failures()).contains("refused"), "{name}: {:?}", r.failures());
        assert!(!r.logs().iter().any(|l| l.contains("sol_signed")), "{name} was signed!");
    }
    assert!(sign(&mallory, honest_for(me, usdc, sol, 1, 5)).await?.is_failure(), "strangers can't ask");

    // 5. The spending limit counts what the messages really spend: 10 + 40 = 50 of 60, then 20 more is refused.
    assert!(sign(&keeper, honest_for(me, usdc, sol, 40_000_000, 200_000_000)).await?.logs().iter().any(|l| l.contains("sol_signed")));
    let r = sign(&keeper, honest_for(me, usdc, sol, 20_000_000, 100_000_000)).await?;
    assert!(format!("{:?}", r.failures()).contains("spending limit"), "{:?}", r.failures());

    // 6. A market floor on top of the hard one: USDC $1, SOL from the oracle, 1% slippage.
    let pyth = worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let (sol_id, usdc_id) = ("ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d", "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a");
    let set = |id: &str, p: i64| json!({"price_id": id, "price": p.to_string(), "conf": "0", "expo": -8, "ema": p.to_string(), "age_s": 0});
    alice.call(pyth.id(), "set_price").args_json(set(usdc_id, 100_000_000)).transact().await?.into_result()?;
    x(&alice, &vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "sol", "contract": pyth.id(), "price_id": sol_id, "backup": {"kind": "ema"}},
        {"name": "usdc", "contract": pyth.id(), "price_id": usdc_id, "backup": {"kind": "ema"}}]}})).await?.into_result()?;
    x(&alice, &vault, "sol_allow", json!({"pair": {"in_mint": USDC_MINT, "out_mint": WSOL_MINT, "max_in": "50000000", "min_out_per_in_e12": "4000000000000",
        "market": {"sell": "usdc", "buy": "sol", "dec_in": 6, "dec_out": 9, "slippage_bps": 100}}})).await?.into_result()?;
    // 1 USDC for 0.005 SOL, accepting 0.5% slippage: min 0.004975 SOL.
    let msg = honest_for(me, usdc, sol, 1_000_000, 5_000_000);
    // SOL at $200: the market floor is 1 / 200 x 99% = 0.00495 SOL: the message's 0.004975 passes.
    alice.call(pyth.id(), "set_price").args_json(set(sol_id, 20_000_000_000)).transact().await?.into_result()?;
    let r = sign(&keeper, msg.clone()).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?} {:?}", r.logs(), r.failures());
    // SOL fell to $150: the market says at least 0.0066 SOL per USDC. The same message (above the
    // owner's old 0.004 hard floor) would now sell SOL cheap: refused.
    alice.call(pyth.id(), "set_price").args_json(set(sol_id, 15_000_000_000)).transact().await?.into_result()?;
    let r = sign(&keeper, msg).await?;
    assert!(format!("{:?}", r.failures()).contains("refused") && !r.logs().iter().any(|l| l.contains("sol_signed")), "{:?}", r.failures());
    Ok(())
}
