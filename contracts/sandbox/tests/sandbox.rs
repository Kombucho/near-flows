//! Integration tests: the real wasm files in a local NEAR node.
//!
//! Unlike the unit tests, here every cross-contract call really happens:
//! tokens move between contracts, payouts can fail, callbacks run.
//! Build the wasm first:  make build   (from the repo root)

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 token (18 decimals in these mocks)

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

struct World {
    worker: Worker<Sandbox>,
    owner: Account,
    alice: Account,
    bob: Account,
    coin: Contract,
    wnear: Contract,
    market: Contract,
}

async fn deploy_token(worker: &Worker<Sandbox>, owner: &Account, supply: u128, symbol: &str) -> anyhow::Result<Contract> {
    deploy_token_with_fee(worker, owner, supply, symbol, 0).await
}

/// A token that keeps `call_fee_bps` on every ft_transfer_call (like justhoot v2).
async fn deploy_token_with_fee(worker: &Worker<Sandbox>, owner: &Account, supply: u128, symbol: &str, call_fee_bps: u16) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new")
        .args_json(json!({"owner_id": owner.id(), "total_supply": supply.to_string(),
                          "name": format!("Mock {symbol}"), "symbol": symbol, "decimals": 18, "call_fee_bps": call_fee_bps}))
        .transact().await?.into_result()?;
    Ok(t)
}

/// `payer` registers `who` on `token` (pays the token's storage for them).
async fn register(payer: &Account, token: &Contract, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token.id(), "storage_deposit")
        .args_json(json!({"account_id": who}))
        .deposit(NearToken::from_millinear(10))
        .transact().await?.into_result()?;
    Ok(())
}

/// `from` sends `amount` of `token` to the market with `msg`.
async fn send(w: &World, from: &Account, token: &Contract, amount: u128, msg: &str) -> anyhow::Result<ExecutionFinalResult> {
    Ok(from.call(token.id(), "ft_transfer_call")
        .args_json(json!({"receiver_id": w.market.id(), "amount": amount.to_string(), "msg": msg}))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact().await?)
}

async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

async fn transfer(from: &Account, token: &Contract, to: &AccountId, amount: u128) -> anyhow::Result<()> {
    from.call(token.id(), "ft_transfer")
        .args_json(json!({"receiver_id": to, "amount": amount.to_string()}))
        .deposit(NearToken::from_yoctonear(1))
        .transact().await?.into_result()?;
    Ok(())
}

/// A launched native market: 4_000 mock wNEAR raised, 500M coins in the pool.
/// Alice and Bob each hold 100 wNEAR; Bob is NOT registered on the coin.
async fn setup() -> anyhow::Result<World> {
    setup_with_coin_fee(0).await
}

async fn setup_with_coin_fee(coin_call_fee_bps: u16) -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;
    let alice = worker.dev_create_account().await?;
    let bob = worker.dev_create_account().await?;
    let coin = deploy_token_with_fee(&worker, &owner, 1_000_000_000 * ONE, "MCOIN", coin_call_fee_bps).await?;
    let wnear = deploy_token(&worker, &owner, 1_000_000 * ONE, "mWNEAR").await?;
    let market = worker.dev_deploy(&wasm("market")).await?;
    market.call("new")
        .args_json(json!({"owner": owner.id(), "token": coin.id(), "quote": wnear.id(), "native": true, "fee_bps": 30,
                          "protocol_fee_after_daily_volume": (5_000 * ONE).to_string()}))
        .transact().await?.into_result()?;
    for who in [market.id(), alice.id()] {
        register(&owner, &coin, who).await?;
        register(&owner, &wnear, who).await?;
    }
    register(&owner, &wnear, bob.id()).await?;
    let w = World { worker, owner, alice, bob, coin, wnear, market };
    for a in [&w.owner, &w.alice, &w.bob] {
        a.call(w.market.id(), "storage_deposit").args_json(json!({})).deposit(NearToken::from_near(1)).transact().await?.into_result()?;
    }
    send(&w, &w.owner, &w.coin, 500_000_000 * ONE, r#"{"action":"fund"}"#).await?.into_result()?;
    send(&w, &w.owner, &w.wnear, 4_000 * ONE, r#"{"action":"fund"}"#).await?.into_result()?;
    w.owner.call(w.market.id(), "launch_native").args_json(json!({"bins_per_side": 230})).max_gas().transact().await?.into_result()?;
    transfer(&w.owner, &w.wnear, w.alice.id(), 100 * ONE).await?;
    transfer(&w.owner, &w.wnear, w.bob.id(), 100 * ONE).await?;
    Ok(w)
}

/// The market's real token balances must cover everything it owes.
async fn assert_solvent(w: &World) -> anyhow::Result<()> {
    let owes: Vec<String> = w.market.view("get_obligations").args_json(json!({})).await?.json()?;
    let (owe_q, owe_t): (u128, u128) = (owes[0].parse()?, owes[1].parse()?);
    let (has_q, has_t) = (balance(&w.wnear, w.market.id()).await?, balance(&w.coin, w.market.id()).await?);
    assert!(has_q >= owe_q, "wNEAR: holds {has_q}, owes {owe_q}");
    assert!(has_t >= owe_t, "coin: holds {has_t}, owes {owe_t}");
    Ok(())
}

async fn positions(w: &World, who: &AccountId) -> anyhow::Result<Vec<Value>> {
    Ok(w.market.view("get_positions").args_json(json!({"account_id": who})).await?.json()?)
}

#[tokio::test]
async fn buy_and_sell_move_real_tokens() -> anyhow::Result<()> {
    let w = setup().await?;
    let r = send(&w, &w.alice, &w.wnear, 10 * ONE, r#"{"action":"buy","min_out":"0","ladder":true}"#).await?;
    println!("GAS buy with ladder (whole tx incl. token + payout): {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "buy failed: {:?}", r.failures());
    let coins = balance(&w.coin, w.alice.id()).await?;
    assert!(coins > 0, "Alice received coins");
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 90 * ONE, "Alice paid 10 wNEAR");
    assert_eq!(positions(&w, w.alice.id()).await?.len(), 4, "4 take-profit rungs (launch window)");
    assert_solvent(&w).await?;

    let r = send(&w, &w.alice, &w.coin, coins / 2, r#"{"action":"sell","min_out":"0"}"#).await?;
    assert!(r.is_success(), "sell failed: {:?}", r.failures());
    assert!(balance(&w.wnear, w.alice.id()).await? > 90 * ONE, "Alice got wNEAR back");
    assert_solvent(&w).await
}

#[tokio::test]
async fn a_failed_payout_is_kept_and_can_be_withdrawn() -> anyhow::Result<()> {
    let w = setup().await?;
    // Bob is not registered on the coin, so sending him coins fails.
    send(&w, &w.bob, &w.wnear, 10 * ONE, r#"{"action":"buy","min_out":"0"}"#).await?.into_result()?;
    assert_eq!(balance(&w.coin, w.bob.id()).await?, 0);
    let owed: String = w.market.view("owed_of").args_json(json!({"token": w.coin.id(), "account_id": w.bob.id()})).await?.json()?;
    let owed: u128 = owed.parse()?;
    assert!(owed > 0, "the failed payout was recorded");
    assert_solvent(&w).await?;
    // Bob registers and pulls it.
    register(&w.bob, &w.coin, w.bob.id()).await?;
    w.bob.call(w.market.id(), "withdraw_owed").args_json(json!({"token": w.coin.id()}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    assert_eq!(balance(&w.coin, w.bob.id()).await?, owed, "Bob got exactly what he was owed");
    Ok(())
}

#[tokio::test]
async fn a_fake_token_is_refunded() -> anyhow::Result<()> {
    let w = setup().await?;
    let evil = deploy_token(&w.worker, &w.alice, 1_000 * ONE, "EVIL").await?;
    register(&w.alice, &evil, w.market.id()).await?;
    let _ = send(&w, &w.alice, &evil, 100 * ONE, r#"{"action":"buy","min_out":"0"}"#).await?; // rejected inside, refunded
    assert_eq!(balance(&evil, w.alice.id()).await?, 1_000 * ONE, "fully refunded");
    assert_eq!(balance(&evil, w.market.id()).await?, 0);
    Ok(())
}

#[tokio::test]
async fn ladder_coins_you_hold_then_cancel_a_rung() -> anyhow::Result<()> {
    let w = setup().await?;
    transfer(&w.owner, &w.coin, w.alice.id(), 1_000_000 * ONE).await?;
    send(&w, &w.alice, &w.coin, 1_000_000 * ONE, r#"{"action":"ladder"}"#).await?.into_result()?;
    let mine = positions(&w, w.alice.id()).await?;
    assert_eq!(mine.len(), 4);
    assert_eq!(balance(&w.coin, w.alice.id()).await?, 0, "all on the ladder");
    let id = mine[0]["id"].as_u64().unwrap();
    w.alice.call(w.market.id(), "withdraw").args_json(json!({"position_id": id}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    assert_eq!(balance(&w.coin, w.alice.id()).await?, 250_000 * ONE, "one rung (25%) came back");
    assert_solvent(&w).await
}

#[tokio::test]
async fn stakers_receive_real_dividends() -> anyhow::Result<()> {
    let w = setup().await?;
    transfer(&w.owner, &w.coin, w.alice.id(), 10_000_000 * ONE).await?;
    send(&w, &w.alice, &w.coin, 10_000_000 * ONE, r#"{"action":"stake"}"#).await?.into_result()?;
    let vote = w.alice.call(w.market.id(), "set_vote").args_json(json!({"vote": [10_000, 0, 0]})).max_gas().transact().await?;
    println!("GAS set_vote: {:.1} Tgas", vote.total_gas_burnt.as_gas() as f64 / 1e12);
    vote.into_result()?;
    // Some trading to create fees.
    send(&w, &w.bob, &w.wnear, 50 * ONE, r#"{"action":"buy","min_out":"0"}"#).await?.into_result()?;
    w.bob.call(w.market.id(), "crank").max_gas().transact().await?.into_result()?; // anyone can crank
    let before = balance(&w.wnear, w.alice.id()).await?;
    w.alice.call(w.market.id(), "claim_dividends").deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    assert!(balance(&w.wnear, w.alice.id()).await? > before, "Alice was paid wNEAR dividends");
    // The crank is rate-limited: a second one in the same hour fails.
    let again = w.bob.call(w.market.id(), "crank").max_gas().transact().await?;
    assert!(again.is_failure());
    assert_solvent(&w).await
}

#[tokio::test]
async fn pause_blocks_buys_but_never_exits() -> anyhow::Result<()> {
    let w = setup().await?;
    transfer(&w.owner, &w.coin, w.alice.id(), 1_000 * ONE).await?;
    send(&w, &w.alice, &w.coin, 1_000 * ONE, r#"{"action":"ladder"}"#).await?.into_result()?;
    w.owner.call(w.market.id(), "pause").max_gas().transact().await?.into_result()?;
    let _ = send(&w, &w.alice, &w.wnear, 10 * ONE, r#"{"action":"buy","min_out":"0"}"#).await?; // fails inside, refunded
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 100 * ONE, "paused buy was refunded");
    let id = positions(&w, w.alice.id()).await?[0]["id"].as_u64().unwrap();
    let r = w.alice.call(w.market.id(), "withdraw").args_json(json!({"position_id": id}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?;
    assert!(r.is_success(), "withdraw works while paused: {:?}", r.failures());
    let stranger = w.alice.call(w.market.id(), "unpause").max_gas().transact().await?;
    assert!(stranger.is_failure(), "only the owner can unpause");
    Ok(())
}

#[tokio::test]
async fn a_coin_with_a_3pct_transfer_fee_keeps_the_market_solvent() -> anyhow::Result<()> {
    // Like justhoot v2 tokens: 3% kept on every ft_transfer_call, receiver gets net.
    let w = setup_with_coin_fee(300).await?;
    send(&w, &w.alice, &w.wnear, 20 * ONE, r#"{"action":"buy","min_out":"0"}"#).await?.into_result()?;
    let coins = balance(&w.coin, w.alice.id()).await?;
    assert!(coins > 0);
    send(&w, &w.alice, &w.coin, coins / 2, r#"{"action":"sell","min_out":"0"}"#).await?.into_result()?;
    send(&w, &w.alice, &w.coin, coins / 4, r#"{"action":"ladder"}"#).await?.into_result()?;
    assert_solvent(&w).await
}

#[tokio::test]
async fn a_relayer_buys_for_alice_and_failures_are_owed_to_her() -> anyhow::Result<()> {
    let w = setup().await?;
    // Bob relays a buy for Alice (Alice has storage on the market; Bob pays).
    let msg = format!(r#"{{"action":"buy","min_out":"0","ladder":true,"for":"{}"}}"#, w.alice.id());
    send(&w, &w.bob, &w.wnear, 10 * ONE, &msg).await?.into_result()?;
    assert_eq!(positions(&w, w.alice.id()).await?.len(), 4, "the rungs are Alice's");
    assert!(balance(&w.coin, w.alice.id()).await? > 0, "the coins went to Alice");
    // A relayed buy that fails (absurd min_out) is owed to Alice, not refunded to Bob.
    let bad = format!(r#"{{"action":"buy","min_out":"999999999999999999999999999999","for":"{}"}}"#, w.alice.id());
    let bob_before = balance(&w.wnear, w.bob.id()).await?;
    send(&w, &w.bob, &w.wnear, 5 * ONE, &bad).await?.into_result()?;
    assert_eq!(balance(&w.wnear, w.bob.id()).await?, bob_before - 5 * ONE, "no refund to the relayer");
    let owed: String = w.market.view("owed_of").args_json(json!({"token": w.wnear.id(), "account_id": w.alice.id()})).await?.json()?;
    assert_eq!(owed, (5 * ONE).to_string(), "owed to Alice");
    w.alice.call(w.market.id(), "withdraw_owed").args_json(json!({"token": w.wnear.id()}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    assert_solvent(&w).await
}

#[tokio::test]
async fn anyone_can_settle_but_only_the_owner_gets_paid() -> anyhow::Result<()> {
    let w = setup().await?;
    // Alice LPs her bag: 10M coins on 4 one-way rungs (1.5x, 2x, 3x, 5x).
    transfer(&w.owner, &w.coin, w.alice.id(), 10_000_000 * ONE).await?;
    send(&w, &w.alice, &w.coin, 10_000_000 * ONE, r#"{"action":"ladder"}"#).await?.into_result()?;
    // A pump: the owner buys enough to push the price past 1.5x.
    send(&w, &w.owner, &w.wnear, 1_500 * ONE, r#"{"action":"buy","min_out":"0"}"#).await?.into_result()?;
    let ids: Vec<u64> = w.market.view("get_settleable").args_json(json!({"limit": 100})).await?.json()?;
    let alice_rungs: Vec<u64> = positions(&w, w.alice.id()).await?.iter().map(|p| p["id"].as_u64().unwrap()).collect();
    let filled: Vec<u64> = ids.into_iter().filter(|i| alice_rungs.contains(i)).collect();
    assert!(!filled.is_empty(), "at least Alice's 1.5x rung earned something");
    // Bob (a stranger, e.g. the keeper) settles it: the wNEAR goes to ALICE.
    let (alice_before, bob_before) = (balance(&w.wnear, w.alice.id()).await?, balance(&w.wnear, w.bob.id()).await?);
    for id in &filled {
        w.bob.call(w.market.id(), "settle").args_json(json!({"position_id": id})).max_gas().transact().await?.into_result()?;
    }
    assert!(balance(&w.wnear, w.alice.id()).await? > alice_before, "Alice was paid");
    assert_eq!(balance(&w.wnear, w.bob.id()).await?, bob_before, "Bob got nothing");
    assert_solvent(&w).await
}
