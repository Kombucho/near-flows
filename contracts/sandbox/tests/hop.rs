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
mod hopw;
mod relayw;

use hopw::*;
use near_workspaces::Account;
use near_workspaces::Contract;
use relayw::*;
use serde_json::{json, Value};

#[tokio::test]
async fn a_relay_vault_hops_bridged_tokens_out_of_intents_only_to_its_own_addresses() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let i = intents(&w).await?;
    let (sol, base, relayer, wnear, verifier) = (&i.sol, &i.base, &i.relayer, &i.wnear, &i.verifier);
    let stranger = w.worker.dev_create_account().await?;

    // The vault's own addresses on Solana and Base (MPC-derived), and its Intents account.
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 8453u64, "max_fee": "2000000000", "max_tip": "100000000", "gas_cap": 500_000u64})).await?.into_result()?;
    let me_sol = w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    let me_evm = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("the vault's own addresses: Solana {me_sol}, EVM {me_evm}");
    i.bridge_in(sol, &w.vault, 3 * SOL).await?;
    i.bridge_in(base, &w.vault, ETH).await?;
    assert_eq!(i.mt(&w.vault, sol.id()).await?, 3 * SOL);

    // 1. The owner allows each token out, to its own kind of chain, capped; nobody else can.
    let hop = |t: &Contract, chain: &str, max: u128, fee: u128| hop_rule(t.id(), chain, max, fee);
    assert!(fails_with(&x(&w.keeper, &w.vault, "intents_hop", hop(sol, "sol", 2 * SOL, SOL / 1000)).await?, "owner only"));
    assert!(fails_with(&x(&w.alice, &w.vault, "intents_hop", hop(sol, "evm", 2 * SOL, SOL / 1000)).await?, "another kind of chain"));
    assert!(fails_with(&x(&w.alice, &w.vault, "intents_hop", hop(wnear, "evm", 1, 0)).await?, "not a POA bridge token"));
    x(&w.alice, &w.vault, "intents_hop", hop(sol, "sol", 2 * SOL, SOL / 1000)).await?.into_result()?;
    x(&w.alice, &w.vault, "intents_hop", hop(base, "evm", ETH / 2, ETH / 1000)).await?.into_result()?;
    x(&w.alice, &w.vault, "set_limit", json!({"token": format!("intents:{}", sol.id()), "max": (2 * SOL).to_string(), "window_ns": 86_400_000_000_000u64})).await?.into_result()?;

    // 2. A flow node: 1.5 SOL out of Intents to the vault's Solana address; what lands is reported.
    let input = format!("intents:{}", sol.id());
    let node = |input: &str, out: &str| hop_node(input, out, json!([]));
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
    assert_eq!(i.mt(&w.vault, sol.id()).await?, 3 * SOL / 2);
    assert_eq!(ft_balance(sol, relayer.id()).await?, 3 * SOL / 2, "omni-token handed it to the bridge's relayer");
    let p: Value = w.brain.view("get_remote_pending").args_json(json!({"vault": w.vault, "flow_id": f, "node": 0})).await?.json()?;
    assert_eq!((p["signed"].as_bool(), p["min_out"].as_str(), p["max_out"].as_str()), (Some(true), Some("1499000000"), Some("1500000000")), "{p}");

    // 3. The report: not more than went out, not less than the owner's fee bound; a failure is the owner's call.
    let report = |who: &Account, out: Option<u128>| {
        let (who, v) = (who.clone(), w.vault.clone());
        async move { x(&who, &v, "remote_report", json!({"flow_id": f, "node": 0, "out": out.map(|o| o.to_string())})).await }
    };
    assert!(fails_with(&report(&w.keeper, Some(3 * SOL / 2 + 1)).await?, "over the node's cap"));
    assert!(fails_with(&report(&w.keeper, Some(SOL)).await?, "below the signed minimum"));
    assert!(fails_with(&report(&w.keeper, None).await?, "only the owner may say"), "a sent hop is failed only on the owner's word");
    assert!(fails_with(&report(&stranger, Some(3 * SOL / 2)).await?, "owner or keeper only"));
    let r = report(&w.keeper, Some(3 * SOL / 2 - 14_000)).await?; // the bridge's SOL fee today: 14,000 lamports
    assert!(r.is_success() && r.logs().iter().any(|l| l.contains(r#""event":"step""#) && l.contains(r#""remote":true"#)), "{:?}", r.failures());

    // 4. One-off hops (no flow): owner only (like the other direct doors: the keeper reaches
    //    other chains through flow nodes), capped per hop, counted against intents:<token>.
    let out = |who: &Account, t: &Contract, amount: u128| {
        let (who, v, args) = (who.clone(), w.vault.clone(), json!({"token": t.id(), "amount": amount.to_string()}));
        async move { x(&who, &v, "intents_hop_out", args).await }
    };
    assert!(fails_with(&out(&w.keeper, base, 1).await?, "owner only"));
    assert!(fails_with(&out(&stranger, sol, 1).await?, "owner only"));
    assert!(fails_with(&out(&w.alice, sol, 2 * SOL + 1).await?, "a hop moves 1 to"));
    assert!(fails_with(&out(&w.alice, sol, SOL).await?, "spending limit"), "1.5 of the day's 2 SOL already hopped");
    assert!(fails_with(&out(&w.alice, wnear, 1).await?, "no hop for that token"));
    let r = out(&w.alice, base, ETH / 4).await?;
    println!("GAS relay intents_hop_out: {:.1} Tgas", tgas(&r));
    assert_eq!(withdrawn(&r), Some(("ft_burn".into(), ETH / 4, format!("WITHDRAW_TO:{me_evm}"))), "Base ETH to the vault's own EVM address: {:?}", r.failures());
    assert_eq!(i.mt(&w.vault, base.id()).await?, 3 * ETH / 4);
    assert_eq!(supply(base).await?, 3 * ETH / 4, "the POA token burned what left");

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
    assert_eq!(i.mt(&w.vault, base.id()).await?, ETH / 4, "the verifier kept it");

    // 7. Recipes still can't withdraw from Intents themselves (ft_withdraw stays fund-moving).
    let r = x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": verifier.id(), "methods": ["ft_withdraw"]}], "remove": []})).await?;
    assert!(r.is_failure(), "ft_withdraw can't be handed to recipes");
    Ok(())
}
