//! Real Rhea DEX (v1 pools) and wNEAR, their testnet wasm, in the local node:
//! shared by protocols.rs (the full vault) and demo.rs (a relay vault).
#![allow(dead_code)]

use crate::common::protocol;
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

pub const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

pub struct Rhea {
    pub wrap: Contract,
    pub dex: Contract,
}

pub async fn register(payer: &Account, token: &AccountId, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token, "storage_deposit").args_json(json!({"account_id": who, "registration_only": true}))
        .deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

pub async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

/// wNEAR and Rhea, initialised as on testnet (exchange fee 4 bps, referral 1 bp of each pool's fee).
pub async fn deploy(worker: &Worker<Sandbox>) -> anyhow::Result<Rhea> {
    let wrap = worker.dev_deploy(&protocol("wrap.testnet")).await?;
    wrap.call("new").transact().await?.into_result()?;
    let dex = worker.dev_deploy(&protocol("ref-finance-101.testnet")).await?;
    dex.call("new").args_json(json!({"owner_id": dex.id(), "exchange_fee": 4, "referral_fee": 1, "boost_farm_id": "boostfarm.test.near",
        "burrowland_id": "burrow.test.near", "wnear_id": wrap.id()})).transact().await?.into_result()?;
    Ok(Rhea { wrap, dex })
}

impl Rhea {
    /// `lp` wraps `wnear` NEAR and opens a simple pool (0.25% fee) of wNEAR : `other_amount` of `other`.
    pub async fn pool(&self, lp: &Account, other: &AccountId, wnear: u128, other_amount: u128) -> anyhow::Result<u64> {
        let (wrap, dex) = (self.wrap.id(), self.dex.id());
        lp.call(wrap, "storage_deposit").args_json(json!({})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        lp.call(wrap, "near_deposit").deposit(NearToken::from_yoctonear(wnear)).transact().await?.into_result()?;
        for t in [wrap, other] {
            register(lp, t, dex).await?;
        }
        self.dex.call("extend_whitelisted_tokens").args_json(json!({"tokens": [wrap, other]})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
        let pool: u64 = lp.call(dex, "add_simple_pool").args_json(json!({"tokens": [wrap, other], "fee": 25}))
            .deposit(NearToken::from_millinear(100)).transact().await?.json()?;
        lp.call(dex, "storage_deposit").args_json(json!({})).deposit(NearToken::from_near(1)).transact().await?.into_result()?;
        lp.call(dex, "register_tokens").args_json(json!({"token_ids": [wrap, other]})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
        for (t, a) in [(wrap, wnear), (other, other_amount)] {
            lp.call(t, "ft_transfer_call").args_json(json!({"receiver_id": dex, "amount": a.to_string(), "msg": ""}))
                .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
        }
        lp.call(dex, "add_liquidity").args_json(json!({"pool_id": pool, "amounts": [wnear.to_string(), other_amount.to_string()]}))
            .deposit(NearToken::from_near(1)).max_gas().transact().await?.into_result()?;
        Ok(pool)
    }

    /// Recipe: wrap the node's NEAR into wNEAR.
    pub fn wrap_recipe(&self) -> Value {
        json!({"label": "Wrap NEAR", "calls": [{"call": "fn", "contract": self.wrap.id(), "method": "near_deposit", "args": "{}",
            "deposit": {"kind": "input"}, "tgas": 10}], "out": self.wrap.id(), "min_out_per_in": null, "price": null})
    }

    /// The msg of a Rhea v1 instant swap, exactly as the Rhea SDK builds it ({amount} and {min_out} filled by the vault).
    pub fn swap_msg(pool: u64, from: &AccountId, to: &AccountId) -> String {
        format!(r#"{{"force":0,"actions":[{{"pool_id":{pool},"token_in":"{from}","token_out":"{to}","amount_in":"{{amount}}","min_amount_out":"{{min_out}}"}}],"skip_unwrap_near":true}}"#)
    }

    /// Recipe: swap on Rhea, with a hard floor (`min_out_per_in`, scaled 1e24) and Rhea's own quote as the price.
    pub fn swap_recipe(&self, pool: u64, from: &AccountId, to: &AccountId, min_out_per_in: Option<u128>) -> Value {
        json!({"label": "Swap on Rhea", "calls": [{"call": "ft", "receiver": self.dex.id(), "msg": Self::swap_msg(pool, from, to), "tgas": 120}],
               "out": to, "min_out_per_in": min_out_per_in.map(|m| m.to_string()),
               "price": {"contract": self.dex.id(), "method": "get_return",
                         "args": json!({"pool_id": pool, "token_in": from, "amount_in": NEAR.to_string(), "token_out": to}).to_string(),
                         "per": NEAR.to_string()}})
    }

    /// What the pool pays now for `amount` of `from`.
    pub async fn quote(&self, pool: u64, from: &AccountId, to: &AccountId, amount: u128) -> anyhow::Result<u128> {
        let v: String = self.dex.view("get_return").args_json(json!({"pool_id": pool, "token_in": from, "amount_in": amount.to_string(), "token_out": to})).await?.json()?;
        Ok(v.parse()?)
    }

    /// A trader's own swap (no vault): `amount` of `from` into the pool, no minimum.
    pub async fn trade(&self, who: &Account, pool: u64, from: &AccountId, to: &AccountId, amount: u128) -> anyhow::Result<()> {
        let msg = Self::swap_msg(pool, from, to).replace("{amount}", &amount.to_string()).replace("{min_out}", "0");
        who.call(from, "ft_transfer_call").args_json(json!({"receiver_id": self.dex.id(), "amount": amount.to_string(), "msg": msg}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
        Ok(())
    }
}
