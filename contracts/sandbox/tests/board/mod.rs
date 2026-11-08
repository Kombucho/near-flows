//! An attested price board (contracts/priceboard) for a memecoin: k of n
//! attesters sign each price (Ed25519), the board keeps the median. Shared by
//! priceboard.rs and demo.rs. The attester keys are test keys.
#![allow(dead_code)]

use crate::common::wasm;
use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};

/// GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump, a pump.fun coin.
pub const MEME: &str = "e202a830669968911d12fb8957d8651fcfa7a56a28cb6b84b7fc21239d8859df";

pub fn attester(i: u8) -> SigningKey {
    SigningKey::from_bytes(&[i + 1; 32])
}

pub fn near_key(k: &SigningKey) -> String {
    format!("ed25519:{}", bs58::encode(k.verifying_key().as_bytes()).into_string())
}

/// The bytes attesters sign, built here independently of the contract.
pub fn message(feed: &str, price: i64, expo: i32, t: i64) -> Vec<u8> {
    let mut m = b"NEARFLOWS_PRICE_V1".to_vec();
    m.extend(hex::decode(feed).unwrap());
    m.extend(price.to_be_bytes());
    m.extend(expo.to_be_bytes());
    m.extend(t.to_be_bytes());
    m
}

/// One attester's signed price, x 1e-12 USD.
pub fn report(k: &SigningKey, feed: &str, price: i64, t: i64) -> Value {
    json!({"attester": near_key(k), "price": price.to_string(), "expo": -12, "publish_time": t,
        "signature": hex::encode(k.sign(&message(feed, price, -12, t)).to_bytes())})
}

/// The chain's clock, in seconds.
pub async fn now(w: &Worker<Sandbox>) -> anyhow::Result<i64> {
    Ok((w.view_block().await?.timestamp() / 1_000_000_000) as i64)
}

/// A board of three attesters, `k` of them needed; its EMA follows over 10 minutes.
pub async fn attested_board(w: &Worker<Sandbox>, owner: &Account, k: u8) -> anyhow::Result<Contract> {
    let b = w.dev_deploy(&wasm("priceboard")).await?;
    b.call("new").args_json(json!({"owner": owner.id(), "ema_window_s": 600,
        "source": {"kind": "attested", "attesters": (0..3).map(|i| near_key(&attester(i))).collect::<Vec<_>>(), "k": k, "max_skew_s": 60}}))
        .transact().await?.into_result()?;
    Ok(b)
}

pub async fn push(who: &Account, board: &Contract, reports: Vec<Value>) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(board.id(), "push").args_json(json!({"feed": MEME, "reports": reports})).max_gas().transact().await?)
}

/// A board serves its average (a vault's `ema` backup) only on half a window of
/// unbroken history, prices at most a third of a window apart (600 s window: 300 s,
/// 200 s). So: `post(t)` at the chain's clock, then about every 150 s, until 300 s
/// of history stand behind the average. Returns the last post's time.
pub async fn history<F, Fut>(w: &Worker<Sandbox>, mut post: F) -> anyhow::Result<i64>
where
    F: FnMut(i64) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let start = now(w).await?;
    post(start).await?;
    let mut last = start;
    while last - start < 300 {
        while now(w).await? < last + 150 {
            w.fast_forward(20).await?;
        }
        last = now(w).await?;
        post(last).await?;
    }
    Ok(last)
}

/// MEME at `price` (x 1e-12), signed by attesters 0 and 1, with the history a vault's backup needs.
pub async fn meme_history(w: &Worker<Sandbox>, who: &Account, board: &Contract, price: i64) -> anyhow::Result<i64> {
    history(w, |t| async move {
        let r = push(who, board, vec![report(&attester(0), MEME, price, t), report(&attester(1), MEME, price, t)]).await?;
        println!("GAS attested push (2 reports): {:.2} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
        r.into_result()?;
        Ok(())
    })
    .await
}
