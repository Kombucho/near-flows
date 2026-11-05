//! Solana through Chain Signatures: the vault learns its Solana address from
//! the signer, checks every byte of a Jupiter swap message against the owner's
//! policy, counts the amount against its limit, and gets it signed. Here the
//! signer is a test stand-in that really signs with Ed25519, so we verify the
//! signature exactly as a Solana validator would.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use near_workspaces::types::NearToken;
use serde_json::{json, Value};
use solguard::testkit::*;
use solguard::*;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";

#[tokio::test]
async fn the_vault_signs_only_swaps_its_owner_allows() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper, mallory) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;

    // 1. The vault asks the signer for its own Solana address (nobody can hand it a fake one).
    alice.call(vault.id(), "sol_setup").args_json(json!({"signer": signer.id(), "max_cu_price": 100_000u64, "max_cu_limit": 1_400_000u32}))
        .max_gas().transact().await?.into_result()?;
    let cfg: Value = vault.view("get_sol").await?.json()?;
    let address = cfg["address"].as_str().unwrap().to_string();
    let expected: String = signer.view("derived_public_key").args_json(json!({"path": "solana-1", "predecessor": vault.id(), "domain_id": 1})).await?.json()?;
    assert_eq!(format!("ed25519:{address}"), expected);
    println!("vault's Solana address: {address}");
    let me = pubkey(&address);

    // 2. The owner allows USDC -> SOL, at most 50 USDC per swap, at least 0.004 SOL per USDC.
    //    (USDC 6 decimals, SOL 9: 0.004 SOL/USDC = 4 lamports per micro-USDC -> 4e12 scaled.)
    alice.call(vault.id(), "sol_allow").args_json(json!({"pair": {"in_mint": USDC_MINT, "out_mint": WSOL_MINT,
        "max_in": "50000000", "min_out_per_in_e12": "4000000000000"}})).transact().await?.into_result()?;
    assert!(keeper.call(vault.id(), "sol_allow").args_json(json!({"pair": {"in_mint": USDC_MINT, "out_mint": WSOL_MINT,
        "max_in": "1", "min_out_per_in_e12": "1"}})).transact().await?.is_failure(), "only the owner sets policy");
    alice.call(vault.id(), "set_limit").args_json(json!({"token": format!("sol:{USDC_MINT}"), "max": "60000000", "window_ns": 3_600_000_000_000u64}))
        .transact().await?.into_result()?;

    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let sign = |who: &near_workspaces::Account, msg: Vec<u8>| {
        let (who, vault_id, m) = (who.clone(), vault.id().clone(), b64(&msg));
        async move { who.call(&vault_id, "sol_sign_swap").args_json(json!({"message": m})).max_gas().transact().await }
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
    Ok(())
}
