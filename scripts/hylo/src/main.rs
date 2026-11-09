//! Hylo transactions for the vault's templates (nodes/sol-templates.json, hylo_*), built with
//! Hylo's own Rust SDK (hylo-quotes / hylo-clients / hylo-idl 2.7.2, github.com/hylo-so/sdk)
//! against a Solana mainnet fork (surfpool), printed as JSON for the sandbox tests
//! (contracts/sandbox/tests/sol_fork_hylo.rs). Signing and sending stay in the keeper's
//! scripts/keeper/src/fork-sol.ts (`send`, `swap-key`, `balance`).
//!
//!   hylo-fork info                                       program ids and mints
//!   hylo-fork fresh                                      re-copy Hylo's Pyth prices from mainnet, stamped at the fork's time
//!   hylo-fork route <vault> <in> <out> <amount> <slippage-bps> [expected-out]
//!       in/out: jitosol xsol hyusd shyusd. Quotes with the SDK's ProtocolStateStrategy (protocol
//!       state read from the fork, Hylo's own math) and builds exactly what the SDK builds: the
//!       vault's ATA for the output (create-idempotent) + the router's route_v2 into the exchange
//!       or earn pool. `expected-out` replaces the SDK's quote inside the slippage config (to show
//!       Hylo itself reverting an over-asked trade, or a template refusing an under-asked one).
//!
//! SOLANA_RPC: the fork (default http://127.0.0.1:8899).

use std::sync::Arc;

use anchor_client::solana_sdk::commitment_config::CommitmentConfig;
use anchor_client::solana_sdk::message::Message;
use anchor_lang::prelude::Pubkey;
use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use hylo_quotes::prelude::*;
use serde_json::json;
use solana_rpc_client::nonblocking::rpc_client::RpcClient;

fn mint(name: &str) -> Result<Pubkey> {
  Ok(match name {
    "jitosol" => JITOSOL::MINT,
    "xsol" => XSOL::MINT,
    "hyusd" => HYUSD::MINT,
    "shyusd" => SHYUSD::MINT,
    _ => bail!("unknown token {name} (jitosol xsol hyusd shyusd)"),
  })
}

// route_v2's data: 8-byte discriminator, token_a, token_b (32 each), amount (u64), then
// Option<SlippageConfig>: a 1-byte tag, expected_token_out {bits u64, exp i8}, tolerance {bits u64, exp i8}.
const EXPECTED_AT: usize = 8 + 32 + 32 + 8 + 1;

/// Pyth's Solana receivers: they own the pushed price accounts (PriceUpdateV2); Hylo's SOL/USD
/// feed is under the Pyth Pro-compatible one (rec2...), older feeds under rec5....
const PYTH_RECEIVERS: [&str; 2] = ["rec2HHDDnjLfj4kE7VyEtFA1HPGQLK33259532cRyHp", "rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ"];

/// Stamp a PriceUpdateV2 as published at `time` and posted at `slot`; returns its real publish
/// time and its price. Layout: 8-byte discriminator, write authority (32), verification level (1
/// byte for Full, 2 for Partial{n}), then the price message: feed id (32), price (i64), conf (8),
/// exponent (i32), publish_time (8), prev_publish_time (8), ema price (8), ema conf (8); then
/// posted_slot (8).
fn restamp(d: &mut [u8], time: i64, slot: u64) -> Option<(i64, f64)> {
  let level = 8 + 32;
  let msg = level + if *d.get(level)? == 0 { 2 } else { 1 };
  let (price, expo) = (msg + 32, msg + 32 + 8 + 8);
  let (publish, posted) = (expo + 4, msg + 84);
  let value = i64::from_le_bytes(d.get(price..price + 8)?.try_into().ok()?) as f64
    * 10f64.powi(i32::from_le_bytes(d.get(expo..expo + 4)?.try_into().ok()?));
  let real = i64::from_le_bytes(d.get(publish..publish + 8)?.try_into().ok()?);
  d.get_mut(publish..publish + 8)?.copy_from_slice(&time.to_le_bytes());
  d.get_mut(posted..posted + 8)?.copy_from_slice(&slot.to_le_bytes());
  Some((real, value))
}

/// The instructions as a legacy message (no lookup tables: the vault's checker reads every key
/// in the message), the vault paying, as base64; plus each instruction for the tests to read.
async fn message(rpc: &RpcClient, payer: &Pubkey, ixs: &[anchor_client::solana_sdk::instruction::Instruction]) -> Result<serde_json::Value> {
  let blockhash = rpc.get_latest_blockhash().await?;
  let bytes = bincode::serialize(&Message::new_with_blockhash(ixs, Some(payer), &blockhash))?;
  Ok(json!({
    "message": base64::engine::general_purpose::STANDARD.encode(&bytes),
    "bytes": bytes.len(),
    "ixs": ixs.iter().map(|i| json!({
      "program": i.program_id.to_string(),
      "data": i.data.iter().map(|x| format!("{x:02x}")).collect::<String>(),
      "accounts": i.accounts.iter().map(|m| json!([m.pubkey.to_string(), m.is_signer, m.is_writable])).collect::<Vec<_>>(),
    })).collect::<Vec<_>>(),
  }))
}

/// One of surfpool's own RPC methods (fork cheat codes).
async fn surfnet(rpc: &RpcClient, method: &'static str, params: serde_json::Value) -> Result<serde_json::Value> {
  use solana_rpc_client_api::request::RpcRequest;
  Ok(rpc.send(RpcRequest::Custom { method }, params).await?)
}

#[tokio::main]
async fn main() -> Result<()> {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let rpc_url = std::env::var("SOLANA_RPC").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
  let rpc = Arc::new(RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed()));
  match args.first().map(String::as_str) {
    Some("info") => {
      println!("{}", json!({
        "exchange": hylo_idl::exchange::ID.to_string(),
        "earn_pool": hylo_idl::earn_pool::ID.to_string(),
        "router": hylo_idl::router::ID.to_string(),
        "jitosol": JITOSOL::MINT.to_string(), "xsol": XSOL::MINT.to_string(),
        "hyusd": HYUSD::MINT.to_string(), "shyusd": SHYUSD::MINT.to_string(),
      }));
    }
    Some("fresh") => {
      // A fork copies each account once, the first time it's read, then keeps it, and its clock
      // drifts from mainnet's. Hylo (on chain and in the SDK) refuses a Pyth price older than its
      // window, a quarter of it for hyUSD (`in_stablecoin_oracle_window`), or posted after the
      // current slot. So: re-copy every Pyth price the protocol state reads from mainnet (today's
      // real prices), then stamp each as published at the fork's own time and slot. The prices
      // themselves are untouched; nothing else is re-copied (the vault's balances stay).
      let registry = rpc.get_account(&hylo_idl::pda::EXO_REGISTRY).await?;
      let entries = hylo_quotes::protocol_state::read_exo_entries(&registry.data)?;
      let keys: Vec<Pubkey> = ProtocolAccounts::PUBKEYS
        .into_iter()
        .chain(hylo_quotes::protocol_state::exo_pubkeys_from_entries(&entries)?)
        .collect();
      let pyth: Vec<Pubkey> = PYTH_RECEIVERS.iter().map(|k| k.parse()).collect::<Result<_, _>>()?;
      let accounts = rpc.get_multiple_accounts(&keys).await?;
      let prices: Vec<Pubkey> = keys.iter().zip(&accounts).filter(|(_, a)| a.as_ref().is_some_and(|a| pyth.contains(&a.owner))).map(|(k, _)| *k).collect();
      for k in &prices {
        surfnet(&rpc, "surfnet_resetAccount", json!([k.to_string()])).await?;
      }
      let fetched = rpc.get_multiple_accounts(&prices).await?;
      let clock: anchor_lang::prelude::Clock = bincode::deserialize(&rpc.get_account(&anchor_lang::solana_program::sysvar::clock::ID).await?.data)?;
      let mut stamped = Vec::new();
      for (k, a) in prices.iter().zip(fetched) {
        let a = a.with_context(|| format!("price {k} gone"))?;
        let mut data = a.data.clone();
        let (real, value) = restamp(&mut data, clock.unix_timestamp, clock.slot.saturating_sub(1)).with_context(|| format!("price {k}: not a PriceUpdateV2"))?;
        surfnet(&rpc, "surfnet_setAccount", json!([k.to_string(), {"data": data.iter().map(|x| format!("{x:02x}")).collect::<String>()}])).await?;
        stamped.push(json!({"account": k.to_string(), "price": value, "mainnet_publish_time": real}));
      }
      let sol_usd = stamped.iter().find(|p| p["account"] == hylo_idl::pda::SOL_USD_PYTH_FEED.to_string()).map(|p| p["price"].clone());
      let state = RpcStateProvider::new(rpc.clone()).fetch_state().await?;
      println!("{}", json!({"prices": stamped, "sol_usd": sol_usd, "fork_time": clock.unix_timestamp, "fork_slot": clock.slot,
        "oracle_interval_secs": state.oracle_interval_secs, "stablecoin_window_ok": state.sol_usd_in_stablecoin_oracle_window()}));
    }
    Some("route") => {
      let [_, vault, a, b, amount, bps, rest @ ..] = args.as_slice() else {
        bail!("route <vault> <in> <out> <amount> <slippage-bps> [expected-out]");
      };
      let user: Pubkey = vault.parse().map_err(|_| anyhow!("vault: not a Solana address"))?;
      let (amount, bps): (u64, u64) = (amount.parse()?, bps.parse()?);
      let strategy = ProtocolStateStrategy::new(RpcStateProvider::new(rpc.clone()));
      let (quote, meta) = strategy
        .runtime_quote_with_metadata(mint(a)?, mint(b)?, amount, user, bps)
        .await
        .context("Hylo quote")?;
      let mut ixs = quote.instructions;
      if let Some(e) = rest.first() {
        // Only the route's slippage expectation changes; every account and the amount stay the SDK's.
        let route = ixs.iter_mut().find(|i| i.program_id == hylo_idl::router::ID).context("no route instruction")?;
        if route.data.get(EXPECTED_AT - 1) != Some(&1) {
          bail!("the SDK built no slippage config");
        }
        route.data[EXPECTED_AT..EXPECTED_AT + 8].copy_from_slice(&e.parse::<u64>()?.to_le_bytes());
      }
      let mut out = message(&rpc, &user, &ixs).await?;
      out["in"] = json!(quote.amount_in.bits.to_string());
      out["out"] = json!(quote.amount_out.bits.to_string());
      out["out_exp"] = json!(quote.amount_out.exp);
      out["fee"] = json!(quote.fee_amount.bits.to_string());
      out["fee_exp"] = json!(quote.fee_amount.exp);
      out["fee_mint"] = json!(quote.fee_mint.to_string());
      out["op"] = json!(meta.description);
      println!("{out}");
    }
    Some("build") => {
      // The SDK's own instruction builder without a quote: for a route Hylo's state refuses to
      // quote right now (e.g. minting hyUSD while the protocol has it switched off), to see
      // what the transaction would be and that Hylo itself refuses it on chain.
      let [_, vault, a, b, amount, expected, bps] = args.as_slice() else {
        bail!("build <vault> <in> <out> <amount> <expected-out> <slippage-bps>");
      };
      let user: Pubkey = vault.parse().map_err(|_| anyhow!("vault: not a Solana address"))?;
      let (amount, expected, bps): (u64, u64, u64) = (amount.parse()?, expected.parse()?, bps.parse()?);
      use hylo_clients::router_client::{InstructionBuilderExt, RouterArgs, RouterClient};
      use hylo_core::slippage_config::SlippageConfig;
      macro_rules! pair {
        ($in:ty, $out:ty, $exp:ty) => {
          RouterClient::build_instructions::<$in, $out>(RouterArgs {
            amount,
            user,
            slippage_config: Some(SlippageConfig::new(UFix64::<$exp>::new(expected), UFix64::<N4>::new(bps))),
          })?
        };
      }
      let ixs = match (a.as_str(), b.as_str()) {
        ("jitosol", "xsol") => pair!(JITOSOL, XSOL, N6),
        ("xsol", "jitosol") => pair!(XSOL, JITOSOL, N9),
        ("jitosol", "hyusd") => pair!(JITOSOL, HYUSD, N6),
        ("hyusd", "jitosol") => pair!(HYUSD, JITOSOL, N9),
        ("hyusd", "shyusd") => pair!(HYUSD, SHYUSD, N6),
        ("shyusd", "hyusd") => pair!(SHYUSD, HYUSD, N6),
        _ => bail!("pair {a} -> {b}: not one of the vault's Hylo templates"),
      };
      println!("{}", message(&rpc, &user, &ixs).await?);
    }
    _ => bail!("commands: info | fresh | route <vault> <in> <out> <amount> <slippage-bps> [expected-out] | build <vault> <in> <out> <amount> <expected-out> <slippage-bps>"),
  }
  Ok(())
}
