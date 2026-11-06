//! Hops out of NEAR Intents (brain hop.rs), against the REAL intents.near code
//! (the Tidepool sandbox verifier, as tests/intents.rs) and the REAL code of the
//! bridged tokens, fetched from mainnet: base.omft.near (the POA token: the EVM
//! chains' omft tokens; a WITHDRAW_TO transfer burns) and sol.omft.near (Omni
//! Bridge's omni-token 0.4.3, which the Solana omft tokens were migrated to: a
//! WITHDRAW_TO transfer goes to the bridge's withdraw relayer, memo and all).
//! A relay vault holds bridged SOL and Base ETH in its Intents account; a hop
//! sends them out with `WITHDRAW_TO:<the vault's own address>`, which the bridge
//! pays out on the other chain. The bridge's off-chain side is the one thing not
//! run: the burn / relayer transfer and its memo are what it acts on.
//! Run scripts/fetch-protocols.sh and make build first.

mod common;
mod relayw;

use common::{protocol, wasm};
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract};
use relayw::*;
use serde_json::{json, Value};

const SOL: u128 = 1_000_000_000;
const ETH: u128 = 1_000_000_000_000_000_000;

/// `<label>.omft.<root>` running mainnet's `code` (base.omft.near: POA, sol.omft.near: omni-token),
/// owned / controlled by `omft`, the bridge's stand-in.
async fn bridged(omft: &Account, label: &str, code: &str, decimals: u8) -> anyhow::Result<Contract> {
    let acc = omft.create_subaccount(label).initial_balance(NearToken::from_near(5)).transact().await?.into_result()?;
    let t = acc.deploy(&protocol(code)).await?.into_result()?;
    let args = if code == "sol.omft.near" {
        json!({"controller": omft.id(), "metadata": {"name": label, "symbol": label, "decimals": decimals}})
    } else {
        json!({"owner_id": omft.id(), "metadata": {"spec": "ft-1.0.0", "name": label, "symbol": label, "decimals": decimals}})
    };
    // omni-token: only the parent account may init it.
    omft.call(t.id(), "new").args_json(args).transact().await?.into_result()?;
    Ok(t)
}

/// The bridge credits `amount` to `who`'s Intents account, as it does on a deposit.
async fn bridge_in(omft: &Account, t: &Contract, verifier: &AccountId, who: &AccountId, amount: u128) -> anyhow::Result<()> {
    for acc in [omft.id(), verifier] {
        omft.call(t.id(), "storage_deposit").args_json(json!({"account_id": acc})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    }
    if t.id().as_str().starts_with("sol.") {
        // omni-token: the controller mints straight into the verifier for `who`.
        omft.call(t.id(), "mint").args_json(json!({"account_id": verifier, "amount": amount.to_string(), "msg": who}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    } else {
        omft.call(t.id(), "ft_deposit").args_json(json!({"owner_id": omft.id(), "amount": amount.to_string()})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        omft.call(t.id(), "ft_transfer_call").args_json(json!({"receiver_id": verifier, "amount": amount.to_string(), "msg": who}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    }
    Ok(())
}

async fn mt(verifier: &Contract, who: &AccountId, token: &AccountId) -> anyhow::Result<u128> {
    let v: String = verifier.view("mt_balance_of").args_json(json!({"account_id": who, "token_id": format!("nep141:{token}")})).await?.json()?;
    Ok(v.parse()?)
}

async fn supply(t: &Contract) -> anyhow::Result<u128> {
    Ok(t.view("ft_total_supply").await?.json::<String>()?.parse()?)
}

/// The token's withdrawal event (the POA token's ft_burn, omni-token's ft_transfer to its
/// relayer), whichever carries a WITHDRAW_TO memo: (event, amount, memo).
fn withdrawn(r: &near_workspaces::result::ExecutionFinalResult) -> Option<(String, u128, String)> {
    let l = r.logs().into_iter().find(|l| l.contains("WITHDRAW_TO:") && (l.contains(r#""event":"ft_burn""#) || l.contains(r#""event":"ft_transfer""#)))?;
    let v: Value = serde_json::from_str(l.trim_start_matches("EVENT_JSON:")).ok()?;
    Some((v["event"].as_str()?.to_string(), v["data"][0]["amount"].as_str()?.parse().ok()?, v["data"][0]["memo"].as_str()?.to_string()))
}

async fn balance(t: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    Ok(t.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json::<String>()?.parse()?)
}

#[tokio::test]
async fn a_relay_vault_hops_bridged_tokens_out_of_intents_only_to_its_own_addresses() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let root = w.worker.root_account()?;
    let omft = root.create_subaccount("omft").initial_balance(NearToken::from_near(20)).transact().await?.into_result()?;
    let (sol, base) = (bridged(&omft, "sol", "sol.omft.near", 9).await?, bridged(&omft, "base", "base.omft.near", 18).await?);
    // As on mainnet: the Solana omft tokens hand WITHDRAW_TO transfers to the bridge's relayer.
    let relayer = w.worker.dev_create_account().await?;
    omft.call(sol.id(), "set_withdraw_relayer_address").args_json(json!({"relayer": relayer.id()})).transact().await?.into_result()?;
    omft.call(sol.id(), "storage_deposit").args_json(json!({"account_id": relayer.id()})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    let wnear = w.worker.dev_deploy(&wasm("mock_ft")).await?;
    wnear.call("new").args_json(json!({"owner_id": root.id(), "total_supply": "1000", "name": "w", "symbol": "w", "decimals": 24, "call_fee_bps": 0})).transact().await?.into_result()?;
    let verifier = w.worker.dev_deploy(&protocol("verifier.tidepool-sandbox.testnet")).await?;
    verifier.call("new").args_json(json!({"config": {"wnear_id": wnear.id(), "fees": {"fee": 1, "fee_collector": root.id()}, "roles": {}}})).max_gas().transact().await?.into_result()?;
    let stranger = w.worker.dev_create_account().await?;

    // The vault's own addresses on Solana and Base (MPC-derived), and its Intents account.
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 8453u64, "max_fee": "2000000000", "max_tip": "100000000", "gas_cap": 500_000u64})).await?.into_result()?;
    x(&w.alice, &w.vault, "intents_setup", json!({"verifier": verifier.id(), "signer": w.signer.id()})).await?.into_result()?;
    let me_sol = w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    let me_evm = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("the vault's own addresses: Solana {me_sol}, EVM {me_evm}");
    bridge_in(&omft, &sol, verifier.id(), &w.vault, 3 * SOL).await?;
    bridge_in(&omft, &base, verifier.id(), &w.vault, ETH).await?;
    assert_eq!(mt(&verifier, &w.vault, sol.id()).await?, 3 * SOL);

    // 1. The owner allows each token out, to its own kind of chain, capped; nobody else can.
    let hop = |t: &Contract, chain: &str, max: u128, fee: u128| json!({"hop": {"token": t.id(), "chain": chain, "max": max.to_string(), "max_fee": fee.to_string()}});
    assert!(fails_with(&x(&w.keeper, &w.vault, "intents_hop", hop(&sol, "sol", 2 * SOL, SOL / 1000)).await?, "owner only"));
    assert!(fails_with(&x(&w.alice, &w.vault, "intents_hop", hop(&sol, "evm", 2 * SOL, SOL / 1000)).await?, "another kind of chain"));
    assert!(fails_with(&x(&w.alice, &w.vault, "intents_hop", hop(&wnear, "evm", 1, 0)).await?, "not a POA bridge token"));
    x(&w.alice, &w.vault, "intents_hop", hop(&sol, "sol", 2 * SOL, SOL / 1000)).await?.into_result()?;
    x(&w.alice, &w.vault, "intents_hop", hop(&base, "evm", ETH / 2, ETH / 1000)).await?.into_result()?;
    x(&w.alice, &w.vault, "set_limit", json!({"token": format!("intents:{}", sol.id()), "max": (2 * SOL).to_string(), "window_ns": 86_400_000_000_000u64})).await?.into_result()?;

    // 2. A flow node: 1.5 SOL out of Intents to the vault's Solana address; what lands is reported.
    let input = format!("intents:{}", sol.id());
    let node = |input: &str, out: &str| json!({"input": input, "when": {"when": "now"},
        "action": {"label": "Hop to Solana", "calls": [], "out": null, "min_out_per_in": null, "price": null, "remote": {"call": {"chain": "hop"}, "out": out}}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "hop", "nodes": [node(&input, "sol:SOL")]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (3 * SOL / 2).to_string()})).await?.into_result()?;
    assert!(fails_with(&x(&stranger, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": {"chain": "hop"}})).await?, "owner or keeper only"));
    // The keeper names its own address: ignored, the brain builds the destination itself.
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": {"chain": "hop", "to": "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin"}})).await?;
    println!("GAS relay step_remote (hop): {:.1} Tgas", tgas(&r));
    let (event, amount, memo) = withdrawn(&r).unwrap_or_else(|| panic!("no withdrawal: {:?} {:?}", r.logs(), r.failures()));
    println!("{event} {amount} with memo {memo}");
    assert_eq!((amount, memo), (3 * SOL / 2, format!("WITHDRAW_TO:{me_sol}")), "out to the vault's own Solana address");
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"hop_sent""#)));
    assert_eq!(mt(&verifier, &w.vault, sol.id()).await?, 3 * SOL / 2);
    assert_eq!(balance(&sol, relayer.id()).await?, 3 * SOL / 2, "omni-token handed it to the bridge's relayer");
    let p: Value = w.brain.view("get_remote_pending").args_json(json!({"vault": w.vault, "flow_id": f, "node": 0})).await?.json()?;
    assert_eq!((p["signed"].as_bool(), p["min_out"].as_str(), p["max_out"].as_str()), (Some(true), Some("1499000000"), Some("1500000000")), "{p}");

    // 3. The report: not more than went out, not less than the owner's fee bound; a failure is the owner's call.
    let report = |who: &Account, out: Option<u128>| {
        let (who, v) = (who.clone(), w.vault.clone());
        async move { x(&who, &v, "remote_report", json!({"flow_id": f, "node": 0, "out": out.map(|o| o.to_string())})).await }
    };
    assert!(fails_with(&report(&w.keeper, Some(3 * SOL / 2 + 1)).await?, "over the node's cap"));
    assert!(fails_with(&report(&w.keeper, Some(SOL)).await?, "below the signed minimum"));
    assert!(fails_with(&report(&w.keeper, None).await?, "may still land"));
    assert!(fails_with(&report(&stranger, Some(3 * SOL / 2)).await?, "owner or keeper only"));
    let r = report(&w.keeper, Some(3 * SOL / 2 - 14_000)).await?; // the bridge's SOL fee today: 14,000 lamports
    assert!(r.is_success() && r.logs().iter().any(|l| l.contains(r#""event":"step""#) && l.contains(r#""remote":true"#)), "{:?}", r.failures());

    // 4. One-off hops (no flow): capped per hop, counted against intents:<token>, owner or keeper only.
    let out = |who: &Account, t: &Contract, amount: u128| {
        let (who, v, args) = (who.clone(), w.vault.clone(), json!({"token": t.id(), "amount": amount.to_string()}));
        async move { x(&who, &v, "intents_hop_out", args).await }
    };
    assert!(fails_with(&out(&w.keeper, &sol, 2 * SOL + 1).await?, "a hop moves 1 to"));
    assert!(fails_with(&out(&w.keeper, &sol, SOL).await?, "spending limit"), "1.5 of the day's 2 SOL already hopped");
    assert!(fails_with(&out(&stranger, &sol, 1).await?, "owner or keeper only"));
    assert!(fails_with(&out(&w.keeper, &wnear, 1).await?, "no hop for that token"));
    let r = out(&w.keeper, &base, ETH / 4).await?;
    println!("GAS relay intents_hop_out: {:.1} Tgas", tgas(&r));
    assert_eq!(withdrawn(&r), Some(("ft_burn".into(), ETH / 4, format!("WITHDRAW_TO:{me_evm}"))), "Base ETH to the vault's own EVM address: {:?}", r.failures());
    assert_eq!(mt(&verifier, &w.vault, base.id()).await?, 3 * ETH / 4);
    assert_eq!(supply(&base).await?, 3 * ETH / 4, "the POA token burned what left");

    // 5. A hop node must report on its token's own chain: Base ETH lands as evm:8453:..., not sol:.
    let binput = format!("intents:{}", base.id());
    let g: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "hop base", "nodes": [node(&binput, "sol:SOL")]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": g, "node": 0, "amount": (ETH / 10).to_string()})).await?.into_result()?;
    assert!(fails_with(&x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": g, "node": 0, "args": {"chain": "hop"}})).await?, "lands on evm:8453:"));
    // 6. Funded (on the owner's word) beyond what Intents holds: the verifier refuses, nothing burns,
    //    the node gets its input back.
    let h: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "hop base 2", "nodes": [node(&binput, "evm:8453:eth")]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": h, "node": 0, "amount": (ETH / 2).to_string()})).await?.into_result()?;
    x(&w.alice, &w.vault, "intents_hop_out", json!({"token": base.id(), "amount": (ETH / 2).to_string()})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": h, "node": 0, "args": {"chain": "hop"}})).await?;
    assert!(withdrawn(&r).is_none() && r.logs().iter().any(|l| l.contains(r#""event":"hop_failed""#)) && r.logs().iter().any(|l| l.contains(r#""event":"step_failed""#)), "{:?}", r.logs());
    assert!(w.brain.view("get_remote_pending").args_json(json!({"vault": w.vault, "flow_id": h, "node": 0})).await?.json::<Option<Value>>()?.is_none());
    assert_eq!(mt(&verifier, &w.vault, base.id()).await?, ETH / 4, "the verifier kept it");

    // 7. Recipes still can't withdraw from Intents themselves (ft_withdraw stays fund-moving).
    let r = x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": verifier.id(), "methods": ["ft_withdraw"]}], "remove": []})).await?;
    assert!(r.is_failure(), "ft_withdraw can't be handed to recipes");
    Ok(())
}
