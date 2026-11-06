//! The vault's EVM swap, executed for real on a local fork of Base mainnet:
//! real Uniswap v3 contracts and pools, no real money. The NEAR side runs in
//! the local NEAR node with the test signer (a real secp256k1 key standing in
//! for the MPC network); the signed transactions go to `anvil --fork-url`.
//!
//! Needs Foundry (anvil, cast). Skipped when anvil isn't installed.
//! BASE_RPC overrides the public Base endpoint.

use std::process::{Child, Command, Stdio};

use near_workspaces::types::NearToken;
use serde_json::{json, Value};

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

// Base mainnet: Uniswap SwapRouter02 and QuoterV2, WETH, USDC.
const ROUTER: &str = "0x2626664c2603336e57b271c5c0b26f421741e481";
const QUOTER: &str = "0x3d4e44eb1374240ce5f1b871ab261cd16335b76a";
const WETH: &str = "0x4200000000000000000000000000000000000006";
const USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
const GWEI: u128 = 1_000_000_000;
const PORT: u16 = 8548;
/// anvil's first test account (a well-known key, funded on every anvil chain).
const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

fn foundry(bin: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = format!("{home}/.foundry/bin/{bin}");
    if std::path::Path::new(&p).exists() { p } else { bin.to_string() }
}

struct Anvil(Child);
impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn rpc() -> String {
    format!("http://127.0.0.1:{PORT}")
}

/// Run `cast`, return trimmed stdout (panics with stderr on failure).
fn cast(args: &[&str]) -> String {
    let out = Command::new(foundry("cast")).args(args).args(["--rpc-url", &rpc()]).output().expect("cast");
    assert!(out.status.success(), "cast {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The first number cast prints ("222419608 [2.224e8]" -> 222419608).
fn num(s: &str) -> u128 {
    s.split_whitespace().next().unwrap().parse().unwrap()
}

fn balance(token: &str, who: &str) -> u128 {
    num(&cast(&["call", token, "balanceOf(address)(uint256)", who]))
}

/// Broadcast a signed transaction; returns the receipt's status (1 = success).
fn send_raw(raw: &str) -> u64 {
    let hash: String = serde_json::from_str(&cast(&["rpc", "eth_sendRawTransaction", raw])).unwrap();
    // `cast receipt` waits until the transaction is mined.
    let receipt: Value = serde_json::from_str(&cast(&["receipt", &hash, "--json"])).unwrap();
    u64::from_str_radix(receipt["status"].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

fn signed(r: &near_workspaces::result::ExecutionFinalResult) -> String {
    let log = r.logs().iter().find(|l| l.contains(r#""event":"evm_signed""#)).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures())).to_string();
    let v: Value = serde_json::from_str(log.trim_start_matches("EVENT_JSON:")).unwrap();
    v["data"][0]["raw"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_vault_swaps_on_real_uniswap_on_a_base_fork() -> anyhow::Result<()> {
    if Command::new(foundry("anvil")).arg("--version").output().is_err() {
        println!("SKIPPED: install Foundry (anvil) to run the Base fork test");
        return Ok(());
    }
    let url = std::env::var("BASE_RPC").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let _anvil = Anvil(Command::new(foundry("anvil")).args(["--fork-url", &url, "--port", &PORT.to_string(), "--silent"]).stdout(Stdio::null()).spawn()?);
    for _ in 0..60 {
        if Command::new(foundry("cast")).args(["chain-id", "--rpc-url", &rpc()]).output().map_or(false, |o| o.status.success()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert_eq!(cast(&["chain-id"]), "8453", "a fork of Base mainnet");
    println!("Base fork at block {}", cast(&["block-number"]));

    // NEAR side: Alice's vault, set up for Base with the test signer.
    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "evm_setup").args_json(json!({"signer": signer.id(), "chain_id": 8453u64, "router": ROUTER,
        "max_fee": (2 * GWEI).to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 400_000u64})).max_gas().transact().await?.into_result()?;
    let me = vault.view("get_evm").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("vault's EVM address on Base: {me}");

    // Fund it on the fork: gas money, and 0.5 WETH wrapped and sent by anvil's test account.
    cast(&["rpc", "anvil_setBalance", &me, "0x16345785D8A0000"]); // 0.1 ETH
    cast(&["send", WETH, "deposit()", "--value", "1ether", "--private-key", ANVIL_KEY]);
    cast(&["send", WETH, "transfer(address,uint256)", &me, "500000000000000000", "--private-key", ANVIL_KEY]);
    assert_eq!(balance(WETH, &me), 500_000_000_000_000_000);

    // The real pool's price right now, from Uniswap's own quoter: 0.1 WETH -> ? USDC.
    let amount: u128 = 100_000_000_000_000_000;
    let quote = num(&cast(&["call", QUOTER, "quoteExactInputSingle((address,address,uint256,uint24,uint160))(uint256,uint160,uint32,uint256)",
        &format!("({WETH},{USDC},{amount},500,0)")]));
    println!("Uniswap quote: 0.1 WETH -> {:.2} USDC", quote as f64 / 1e6);

    // The owner's floor: 90% of today's price (out per in, scaled 1e12). At most 0.2 WETH a swap.
    let floor_e12 = quote * 9 / 10 * 1_000_000_000_000 / amount;
    alice.call(vault.id(), "evm_allow").args_json(json!({"pair": {"token_in": WETH, "token_out": USDC, "fee": 500,
        "max_in": "200000000000000000", "min_out_per_in_e12": floor_e12.to_string()}})).transact().await?.into_result()?;

    let tx = |nonce: u64| json!({"nonce": nonce, "max_fee": GWEI.to_string(), "max_tip": (GWEI / 100).to_string()});
    let swap = |nonce: u64, min_out: u128| {
        let mut a = tx(nonce);
        a["token_in"] = json!(WETH);
        a["token_out"] = json!(USDC);
        a["amount_in"] = json!(amount.to_string());
        a["min_out"] = json!(min_out.to_string());
        a["gas"] = json!(300_000u64);
        a
    };

    // 1. Approve the router, signed by the vault, executed on the fork.
    let mut a = tx(0);
    a["token_in"] = json!(WETH);
    a["amount"] = json!("200000000000000000");
    let r = keeper.call(vault.id(), "evm_approve").args_json(a).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1, "approve executed");

    // 2. A keeper asking more than the pool gives (2x the quote): the vault signs it,
    //    Uniswap refuses it on chain, nothing moves (only gas).
    let r = keeper.call(vault.id(), "evm_swap").args_json(swap(1, quote * 2)).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 0, "Uniswap reverts below the minimum");
    assert_eq!(balance(WETH, &me), 500_000_000_000_000_000);
    assert_eq!(balance(USDC, &me), 0);

    // 3. A fair swap: min_out = 99% of the quote. Real Uniswap pays the vault.
    let r = keeper.call(vault.id(), "evm_swap").args_json(swap(2, quote * 99 / 100)).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1, "swap executed");
    let got = balance(USDC, &me);
    println!("swapped 0.1 WETH -> {:.2} USDC into the vault's own address", got as f64 / 1e6);
    assert!(got >= quote * 99 / 100, "at least the minimum");
    assert_eq!(balance(WETH, &me), 400_000_000_000_000_000);

    // 4. A keeper quote of 1 wei can't push it below the owner's floor: the floor is in the tx.
    let r = keeper.call(vault.id(), "evm_swap").args_json(swap(3, 1)).max_gas().transact().await?;
    let raw = signed(&r);
    let floor = amount * floor_e12 / 1_000_000_000_000;
    assert!(raw.contains(&format!("{floor:064x}")), "minimum in the signed tx = the owner's floor");
    assert_eq!(send_raw(&raw), 1);
    assert!(balance(USDC, &me) >= got + floor);
    let _ = alice.transfer_near(vault.id(), NearToken::from_millinear(1)).await?;
    Ok(())
}
