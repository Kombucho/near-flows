//! NEAR Intents with bridged tokens, shared by the hop tests (hop.rs, hop_fork.rs):
//! the REAL intents.near code (the Tidepool sandbox verifier) and the REAL code of
//! two bridged tokens fetched from mainnet: base.omft.near (the POA token: the EVM
//! chains' omft tokens; a WITHDRAW_TO transfer burns) and sol.omft.near (Omni
//! Bridge's omni-token 0.4.3: a WITHDRAW_TO transfer goes to the bridge's withdraw
//! relayer, memo and all). Run scripts/fetch-protocols.sh and make build first.
#![allow(dead_code)]

use crate::common::{protocol, wasm};
use crate::relayw::{x, RelayWorld};
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract};
use serde_json::{json, Value};

pub const SOL: u128 = 1_000_000_000;
pub const ETH: u128 = 1_000_000_000_000_000_000;

pub struct Intents {
    /// The bridge's stand-in: owns / controls the omft tokens.
    pub omft: Account,
    pub sol: Contract,
    pub base: Contract,
    /// Where omni-token hands WITHDRAW_TO transfers (the bridge's withdraw relayer).
    pub relayer: Account,
    pub wnear: Contract,
    pub verifier: Contract,
}

/// `<label>.omft.<root>` running mainnet's `code` (base.omft.near: POA, sol.omft.near: omni-token).
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

/// The verifier, the two bridged tokens, and the vault's Intents account set up
/// (intents_setup) in `w`.
pub async fn intents(w: &RelayWorld) -> anyhow::Result<Intents> {
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
    x(&w.alice, &w.vault, "intents_setup", json!({"verifier": verifier.id(), "signer": w.signer.id()})).await?.into_result()?;
    Ok(Intents { omft, sol, base, relayer, wnear, verifier })
}

impl Intents {
    /// The bridge credits `amount` of `t` to `who`'s Intents account, as it does on a deposit.
    pub async fn bridge_in(&self, t: &Contract, who: &AccountId, amount: u128) -> anyhow::Result<()> {
        let (omft, verifier) = (&self.omft, self.verifier.id());
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

    /// `who`'s balance of `token` in its Intents account.
    pub async fn mt(&self, who: &AccountId, token: &AccountId) -> anyhow::Result<u128> {
        let v: String = self.verifier.view("mt_balance_of").args_json(json!({"account_id": who, "token_id": format!("nep141:{token}")})).await?.json()?;
        Ok(v.parse()?)
    }
}

/// A hop node: `input` (intents:<token>) out to the vault's own address, landing as `out`.
pub fn hop_node(input: &str, out: &str, next: Value) -> Value {
    json!({"input": input, "when": {"when": "now"}, "next": next,
        "action": {"label": "Hop out of Intents", "calls": [], "out": null, "min_out_per_in": null, "price": null, "remote": {"call": {"chain": "hop"}, "out": out}}})
}

/// The owner lets `token` hop to `chain`, at most `max` a hop, the bridge keeping at most `max_fee`.
pub fn hop_rule(token: &AccountId, chain: &str, max: u128, max_fee: u128) -> Value {
    json!({"hop": {"token": token, "chain": chain, "max": max.to_string(), "max_fee": max_fee.to_string()}})
}

pub async fn supply(t: &Contract) -> anyhow::Result<u128> {
    Ok(t.view("ft_total_supply").await?.json::<String>()?.parse()?)
}

pub async fn ft_balance(t: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    Ok(t.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json::<String>()?.parse()?)
}

/// The token's withdrawal event (the POA token's ft_burn, omni-token's ft_transfer to its
/// relayer), whichever carries a WITHDRAW_TO memo: (event, amount, memo).
pub fn withdrawn(r: &near_workspaces::result::ExecutionFinalResult) -> Option<(String, u128, String)> {
    let l = r.logs().into_iter().find(|l| l.contains("WITHDRAW_TO:") && (l.contains(r#""event":"ft_burn""#) || l.contains(r#""event":"ft_transfer""#)))?;
    let v: Value = serde_json::from_str(l.trim_start_matches("EVENT_JSON:")).ok()?;
    Some((v["event"].as_str()?.to_string(), v["data"][0]["amount"].as_str()?.parse().ok()?, v["data"][0]["memo"].as_str()?.to_string()))
}
