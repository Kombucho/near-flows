//! Bazaar integration tests: the real wasm files in a local NEAR node, with
//! real token and NFT contracts. Build the wasm first: make build

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 token (18 decimals in the mocks)
const NEAR: u128 = 1_000_000_000_000_000_000_000_000; // 1 NEAR in yocto

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

struct World {
    worker: Worker<Sandbox>,
    owner: Account,
    alice: Account,
    bob: Account,
    carol: Account,
    coin: Contract,
    wnear: Contract,
    bazaar: Contract,
}

async fn token(worker: &Worker<Sandbox>, owner: &Account, symbol: &str) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new")
        .args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
                          "name": format!("Mock {symbol}"), "symbol": symbol, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn register(payer: &Account, token: &Contract, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token.id(), "storage_deposit").args_json(json!({"account_id": who}))
        .deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

async fn near_of(a: &Account) -> anyhow::Result<u128> {
    Ok(a.view_account().await?.balance.as_yoctonear())
}

async fn send(w: &World, from: &Account, token: &Contract, amount: u128, msg: &str) -> anyhow::Result<ExecutionFinalResult> {
    Ok(from.call(token.id(), "ft_transfer_call")
        .args_json(json!({"receiver_id": w.bazaar.id(), "amount": amount.to_string(), "msg": msg}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?)
}

async fn lot(w: &World, id: u64) -> anyhow::Result<Value> {
    Ok(w.bazaar.view("get_lot").args_json(json!({"id": id})).await?.json()?)
}

/// `seller` opens a listing of `token` for `ask` wNEAR in total (step 1; step 2 is the fund transfer).
async fn open_listing(w: &World, seller: &Account, token: &Contract, ask: u128, title: &str) -> anyhow::Result<()> {
    let r = seller.call(w.bazaar.id(), "open_sale")
        .args_json(json!({"item": {"kind": "ft", "token": token.id()}, "terms": {"sale": "listing", "ask": ask.to_string()},
                          "pay": w.wnear.id(), "lore": {"title": title, "description": "Selling to fund the next meme"}}))
        .max_gas().transact().await?;
    println!("GAS open a sale: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    Ok(())
}

/// Move the chain's clock until the auction's close has passed.
async fn past_close(w: &World, id: u64) -> anyhow::Result<()> {
    for _ in 0..60 {
        if lot(w, id).await?["open"] == false {
            return Ok(());
        }
        w.worker.fast_forward(2_000).await?;
    }
    anyhow::bail!("auction never closed")
}

/// Alice, Bob and Carol each hold 1,000 COIN and 1,000 wNEAR and have bazaar storage.
async fn setup() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let owner = worker.dev_create_account().await?;
    let (alice, bob, carol) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    let coin = token(&worker, &owner, "COIN").await?;
    let wnear = token(&worker, &owner, "mWNEAR").await?;
    let bazaar = worker.dev_deploy(&wasm("bazaar")).await?;
    bazaar.call("new").transact().await?.into_result()?;
    for t in [&coin, &wnear] {
        for who in [bazaar.id(), alice.id(), bob.id(), carol.id()] {
            register(&owner, t, who).await?;
        }
        for who in [alice.id(), bob.id(), carol.id()] {
            owner.call(t.id(), "ft_transfer").args_json(json!({"receiver_id": who, "amount": (1_000 * ONE).to_string()}))
                .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
        }
    }
    for a in [&alice, &bob, &carol] {
        a.call(bazaar.id(), "storage_deposit").args_json(json!({})).deposit(NearToken::from_near(1)).transact().await?.into_result()?;
    }
    Ok(World { worker, owner, alice, bob, carol, coin, wnear, bazaar })
}

#[tokio::test]
async fn tokens_listed_bought_in_parts_and_the_rest_cancelled() -> anyhow::Result<()> {
    let w = setup().await?;
    open_listing(&w, &w.alice, &w.coin, 500 * ONE, "Half my bag").await?;
    let r = send(&w, &w.alice, &w.coin, 1_000 * ONE, r#"{"action":"fund","lot":0}"#).await?;
    println!("GAS fund a token sale: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    assert_eq!(balance(&w.coin, w.bazaar.id()).await?, 1_000 * ONE, "the bag is held by the bazaar");
    assert_eq!(lot(&w, 0).await?["lore"]["title"], "Half my bag");

    // Bob pays 100 wNEAR: 200 COIN at 0.5 wNEAR each. Alice is paid at once.
    let r = send(&w, &w.bob, &w.wnear, 100 * ONE, r#"{"action":"buy","lot":0}"#).await?;
    println!("GAS buy part of a listing: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    assert_eq!(balance(&w.coin, w.bob.id()).await?, 1_200 * ONE);
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 1_100 * ONE);
    // Carol tries to pay in the wrong token: refunded in full by the token contract.
    let _ = send(&w, &w.carol, &w.coin, 10 * ONE, r#"{"action":"buy","lot":0}"#).await?;
    assert_eq!(balance(&w.coin, w.carol.id()).await?, 1_000 * ONE, "wrong token refunded");
    // Carol overpays: takes 600 of the 800 left? She pays 300 for 600.
    send(&w, &w.carol, &w.wnear, 300 * ONE, r#"{"action":"buy","lot":0}"#).await?.into_result()?;
    assert_eq!(balance(&w.coin, w.carol.id()).await?, 1_600 * ONE);
    // Alice takes the rest back.
    let r = w.alice.call(w.bazaar.id(), "cancel").args_json(json!({"lot_id": 0}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?;
    r.into_result()?;
    assert_eq!(balance(&w.coin, w.alice.id()).await?, 200 * ONE, "the unsold 200 came back");
    assert_eq!(balance(&w.coin, w.bazaar.id()).await?, 0, "nothing left behind");
    assert_eq!(balance(&w.wnear, w.bazaar.id()).await?, 0, "every payment passed straight to the seller");
    Ok(())
}

#[tokio::test]
async fn an_nft_auction_in_near_pays_the_seller_and_refunds_the_change() -> anyhow::Result<()> {
    let w = setup().await?;
    let nft = w.worker.dev_deploy(&wasm("mock_nft")).await?;
    nft.call("new").transact().await?.into_result()?;
    w.alice.call(nft.id(), "mint").args_json(json!({"token_id": "1", "title": "Genesis"}))
        .deposit(NearToken::from_millinear(20)).transact().await?.into_result()?;
    w.alice.call(w.bazaar.id(), "open_sale")
        .args_json(json!({"item": {"kind": "nft", "contract": nft.id(), "token_id": "1"},
                          "terms": {"sale": "auction", "opening": (NEAR / 10).to_string(), "hours": 1}, "lore": {"title": "Genesis #1"}}))
        .max_gas().transact().await?.into_result()?;
    let r = w.alice.call(nft.id(), "nft_transfer_call")
        .args_json(json!({"receiver_id": w.bazaar.id(), "token_id": "1", "msg": r#"{"action":"fund","lot":0}"#}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?;
    println!("GAS auction an NFT: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    let owner: Value = nft.view("nft_token").args_json(json!({"token_id": "1"})).await?.json()?;
    assert_eq!(owner["owner_id"], w.bazaar.id().to_string(), "the NFT is held by the bazaar");

    // Bob locks a 2 NEAR maximum. Carol bids 1 NEAR: outbid at once, refunded.
    let r = w.bob.call(w.bazaar.id(), "bid").args_json(json!({"lot_id": 0})).deposit(NearToken::from_near(2)).max_gas().transact().await?;
    println!("GAS bid: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    let carol_before = near_of(&w.carol).await?;
    let r = w.carol.call(w.bazaar.id(), "bid").args_json(json!({"lot_id": 0})).deposit(NearToken::from_near(1)).max_gas().transact().await?;
    r.clone().into_result()?;
    let carol_spent = carol_before - near_of(&w.carol).await?;
    assert!(carol_spent < NEAR / 100, "Carol's 1 NEAR came back (she only paid gas): spent {carol_spent}");
    let l = lot(&w, 0).await?;
    assert_eq!(l["leader"], w.bob.id().to_string());
    assert_eq!(l["price"], (NEAR + NEAR / 10).to_string(), "Bob's price: one 10% step above Carol");

    // Too early to settle.
    assert!(w.carol.call(w.bazaar.id(), "settle").args_json(json!({"lot_id": 0})).max_gas().transact().await?.is_failure());
    past_close(&w, 0).await?;
    let (alice_before, bob_before) = (near_of(&w.alice).await?, near_of(&w.bob).await?);
    // Carol (anyone) settles. Alice gets 1.1 NEAR, Bob gets the NFT and 0.9 NEAR change.
    w.carol.call(w.bazaar.id(), "settle").args_json(json!({"lot_id": 0})).max_gas().transact().await?.into_result()?;
    assert_eq!(near_of(&w.alice).await? - alice_before, NEAR + NEAR / 10, "seller paid the public price");
    assert_eq!(near_of(&w.bob).await? - bob_before, NEAR - NEAR / 10, "winner got the change back");
    let owner: Value = nft.view("nft_token").args_json(json!({"token_id": "1"})).await?.json()?;
    assert_eq!(owner["owner_id"], w.bob.id().to_string(), "Bob owns the NFT");
    assert!(w.carol.call(w.bazaar.id(), "settle").args_json(json!({"lot_id": 0})).max_gas().transact().await?.is_failure(), "only once");
    Ok(())
}

#[tokio::test]
async fn an_idea_auction_below_its_reserve_refunds_the_bidder() -> anyhow::Result<()> {
    let w = setup().await?;
    let now_ns: u64 = w.worker.view_block().await?.timestamp();
    let r = w.alice.call(w.bazaar.id(), "sell_idea")
        .args_json(json!({"terms": {"sale": "auction", "opening": (10 * ONE).to_string(), "reserve": (500 * ONE).to_string(), "hours": 1},
                          "pledge": {"amount": (1_000_000 * ONE).to_string(), "deliver_by_ns": now_ns + 240 * 3_600_000_000_000u64},
                          "pay": w.wnear.id(), "lore": {"title": "FROG: a coin for frog people", "links": ["https://x.com/frog"]}}))
        .max_gas().transact().await?;
    println!("GAS sell an idea: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    send(&w, &w.bob, &w.wnear, 300 * ONE, r#"{"action":"bid","lot":0}"#).await?.into_result()?;
    assert_eq!(balance(&w.wnear, w.bazaar.id()).await?, 300 * ONE, "Bob's maximum is locked");
    // The seller can't pull the auction now.
    let pull = w.alice.call(w.bazaar.id(), "cancel").args_json(json!({"lot_id": 0}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?;
    assert!(pull.is_failure());
    past_close(&w, 0).await?;
    w.carol.call(w.bazaar.id(), "settle").args_json(json!({"lot_id": 0})).max_gas().transact().await?.into_result()?;
    assert_eq!(balance(&w.wnear, w.bob.id()).await?, 1_000 * ONE, "reserve not met: Bob got all 300 back");
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 1_000 * ONE, "Alice got nothing");
    assert_eq!(balance(&w.wnear, w.bazaar.id()).await?, 0);
    let _ = &w.owner;
    Ok(())
}

/// Retry `finalize` while skipping chain time, until the review has ended.
async fn finalize_after_review(w: &World, id: u64) -> anyhow::Result<()> {
    for _ in 0..80 {
        let r = w.carol.call(w.bazaar.id(), "finalize").args_json(json!({"lot_id": id})).max_gas().transact().await?;
        if r.is_success() {
            return Ok(());
        }
        let err = format!("{:?}", r.failures());
        anyhow::ensure!(err.contains("review still running"), "finalize failed: {err}");
        w.worker.fast_forward(100_000).await?;
    }
    anyhow::bail!("review never ended")
}

#[tokio::test]
async fn an_idea_is_funded_delivered_reviewed_and_paid() -> anyhow::Result<()> {
    let w = setup().await?;
    let frog = token(&w.worker, &w.alice, "FROG").await?; // Alice's new coin; she holds the supply
    for who in [w.bazaar.id(), w.bob.id(), w.carol.id()] {
        register(&w.owner, &frog, who).await?;
    }
    // 100 shares for 1,000 wNEAR; Alice pledges 1M FROG within 10 days.
    let now_ns: u64 = w.worker.view_block().await?.timestamp();
    w.alice.call(w.bazaar.id(), "sell_idea")
        .args_json(json!({"terms": {"sale": "listing", "ask": (1_000 * ONE).to_string()}, "shares": "100",
                          "pledge": {"amount": (1_000_000 * ONE).to_string(), "deliver_by_ns": now_ns + 240 * 3_600_000_000_000u64},
                          "pay": w.wnear.id(), "lore": {"title": "FROG: a coin for frog people"}}))
        .max_gas().transact().await?.into_result()?;
    send(&w, &w.bob, &w.wnear, 600 * ONE, r#"{"action":"buy","lot":0}"#).await?.into_result()?;
    send(&w, &w.carol, &w.wnear, 400 * ONE, r#"{"action":"buy","lot":0}"#).await?.into_result()?;
    assert_eq!(balance(&w.wnear, w.bazaar.id()).await?, 1_000 * ONE, "the money waits in escrow");
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 1_000 * ONE, "Alice is not paid yet");

    // Delivery: Alice sends the pledged FROG. Mallory can't fake a delivery for her.
    let r = send(&w, &w.alice, &frog, 1_000_000 * ONE, r#"{"action":"deliver","lot":0}"#).await?;
    println!("GAS deliver: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    r.into_result()?;
    let e: Value = w.bazaar.view("get_escrow").args_json(json!({"lot_id": 0})).await?.json()?;
    assert_eq!(e["phase"], "review");
    // Carol objects (40%): not enough to fail it.
    w.carol.call(w.bazaar.id(), "object").args_json(json!({"lot_id": 0})).max_gas().transact().await?.into_result()?;
    finalize_after_review(&w, 0).await?;
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 2_000 * ONE, "accepted: Alice is paid the 1,000 raised");
    // Anyone triggers the claims; each backer gets their share of FROG.
    for who in [w.bob.id(), w.carol.id()] {
        w.owner.call(w.bazaar.id(), "claim").args_json(json!({"lot_id": 0, "account": who})).max_gas().transact().await?.into_result()?;
    }
    assert_eq!(balance(&frog, w.bob.id()).await?, 600_000 * ONE);
    assert_eq!(balance(&frog, w.carol.id()).await?, 400_000 * ONE);
    assert_eq!(balance(&frog, w.bazaar.id()).await?, 0);
    assert_eq!(balance(&w.wnear, w.bazaar.id()).await?, 0);
    Ok(())
}

#[tokio::test]
async fn a_rejected_delivery_refunds_backers_and_returns_the_coins() -> anyhow::Result<()> {
    let w = setup().await?;
    let junk = token(&w.worker, &w.alice, "JUNK").await?; // a worthless token Alice made up
    register(&w.owner, &junk, w.bazaar.id()).await?;
    let now_ns: u64 = w.worker.view_block().await?.timestamp();
    w.alice.call(w.bazaar.id(), "sell_idea")
        .args_json(json!({"terms": {"sale": "listing", "ask": (1_000 * ONE).to_string()}, "shares": "100",
                          "pledge": {"amount": (1_000_000 * ONE).to_string(), "deliver_by_ns": now_ns + 240 * 3_600_000_000_000u64},
                          "pay": w.wnear.id(), "lore": {"title": "Totally real coin"}}))
        .max_gas().transact().await?.into_result()?;
    send(&w, &w.bob, &w.wnear, 600 * ONE, r#"{"action":"buy","lot":0}"#).await?.into_result()?;
    send(&w, &w.carol, &w.wnear, 400 * ONE, r#"{"action":"buy","lot":0}"#).await?.into_result()?;
    send(&w, &w.alice, &junk, 1_000_000 * ONE, r#"{"action":"deliver","lot":0}"#).await?.into_result()?;
    w.bob.call(w.bazaar.id(), "object").args_json(json!({"lot_id": 0})).max_gas().transact().await?.into_result()?;
    finalize_after_review(&w, 0).await?;
    for who in [w.bob.id(), w.carol.id()] {
        w.owner.call(w.bazaar.id(), "claim").args_json(json!({"lot_id": 0, "account": who})).max_gas().transact().await?.into_result()?;
    }
    assert_eq!(balance(&w.wnear, w.bob.id()).await?, 1_000 * ONE, "Bob's 600 came back");
    assert_eq!(balance(&w.wnear, w.carol.id()).await?, 1_000 * ONE, "Carol's 400 came back");
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 1_000 * ONE, "Alice got nothing");
    assert_eq!(balance(&junk, w.alice.id()).await?, 1_000_000_000 * ONE, "and her junk back");
    Ok(())
}

#[tokio::test]
async fn a_seller_is_never_paid_for_an_item_the_buyer_did_not_get() -> anyhow::Result<()> {
    // Dave can pay in wNEAR but is NOT registered on COIN, so the coins can't reach him.
    // (The same path covers a seller whose token or NFT contract refuses to release the item.)
    let w = setup().await?;
    let dave = w.worker.dev_create_account().await?;
    register(&w.owner, &w.wnear, dave.id()).await?;
    w.owner.call(w.wnear.id(), "ft_transfer").args_json(json!({"receiver_id": dave.id(), "amount": (100 * ONE).to_string()}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    dave.call(w.bazaar.id(), "storage_deposit").args_json(json!({})).deposit(NearToken::from_near(1)).transact().await?.into_result()?;
    open_listing(&w, &w.alice, &w.coin, 500 * ONE, "My bag").await?;
    send(&w, &w.alice, &w.coin, 1_000 * ONE, r#"{"action":"fund","lot":0}"#).await?.into_result()?;
    let r = send(&w, &dave, &w.wnear, 50 * ONE, r#"{"action":"buy","lot":0}"#).await?;
    assert!(r.logs().iter().any(|l| l.contains("delivery_failed")), "the failed delivery is reported");
    assert_eq!(balance(&w.wnear, dave.id()).await?, 100 * ONE, "Dave got his 50 wNEAR back");
    assert_eq!(balance(&w.wnear, w.alice.id()).await?, 1_000 * ONE, "Alice was NOT paid");
    let owed: String = w.bazaar.view("owed_of").args_json(json!({"token": w.coin.id(), "account_id": w.alice.id()})).await?.json()?;
    assert_eq!(owed, (100 * ONE).to_string(), "the 100 coins that couldn't move are kept for Alice");
    w.alice.call(w.bazaar.id(), "withdraw_owed").args_json(json!({"token": w.coin.id()}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    assert_eq!(balance(&w.coin, w.alice.id()).await?, 100 * ONE, "Alice pulled them back (900 still listed)");
    Ok(())
}

#[tokio::test]
async fn a_fake_token_cannot_list_in_someone_elses_name() -> anyhow::Result<()> {
    // Mallory deploys her own token and has it tell the bazaar "Alice sent this".
    let w = setup().await?;
    open_listing(&w, &w.alice, &w.coin, 500 * ONE, "Alice's real listing").await?;
    let fake = token(&w.worker, &w.carol, "COIN").await?; // same symbol, different contract
    register(&w.carol, &fake, w.bazaar.id()).await?;
    let r = send(&w, &w.carol, &fake, 1_000 * ONE, r#"{"action":"fund","lot":0}"#).await?;
    assert!(format!("{:?}", r.failures()).contains("only the seller funds a sale") || !r.logs().iter().any(|l| l.contains("funded")));
    assert_eq!(lot(&w, 0).await?["sale"], "pending", "still waiting for Alice's real coins");
    assert_eq!(balance(&fake, w.carol.id()).await?, 1_000_000_000 * ONE, "fake tokens bounced back");
    Ok(())
}
