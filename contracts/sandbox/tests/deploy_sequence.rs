//! The testnet deploy script, replayed in a sandbox: scripts/deploy-relay-testnet.sh
//! runs with DRY=1 PLAN=<file> (sends nothing, writes each command as a JSON
//! line), and this test executes those exact lines, same accounts, same args,
//! same deposits and gas. If the script drifts from the contracts (a renamed
//! init arg, a request the brain refuses, too little deposit), this fails.
//!
//! WHY the same account names as testnet: a sandbox-only patch creates the
//! top-level `testnet` account, so every `*.testnet` name in the script exists
//! here too. Ours start with the faucet's 10 NEAR, which checks the funding
//! table. Outside contracts get stand-ins at their real names: mock_ft for
//! the tokens, mock_pyth for Pyth, and Rhea's REAL code read from testnet
//! (views only; skipped offline with a stand-in that only accepts the calls).
//! Build the wasm first: make build

mod common;

use common::wasm;
use near_workspaces::network::Sandbox;
use near_workspaces::operations::Function;
use near_workspaces::types::{AccessKey, AccountDetailsPatch, Gas, KeyType, NearToken, SecretKey};
use near_workspaces::{Account, AccountId, Worker};
use serde_json::{json, Value};
use std::collections::HashMap;

const SUFFIX: &str = "sbx";
const STEPS: &str = "accounts,brain,factory,publish,priceboard,vault,flow";

fn script() -> String {
    format!("{}/../../scripts/deploy-relay-testnet.sh", env!("CARGO_MANIFEST_DIR"))
}

/// "1.099 NEAR", "0 NEAR", "1 yoctoNEAR" -> yocto.
fn yocto(s: &str) -> u128 {
    let (n, unit) = s.trim().split_once(' ').unwrap_or_else(|| panic!("deposit {s:?}"));
    if unit == "yoctoNEAR" {
        return n.parse().unwrap();
    }
    assert_eq!(unit, "NEAR", "deposit {s:?}");
    let (whole, frac) = n.split_once('.').unwrap_or((n, ""));
    assert!(frac.len() <= 24);
    whole.parse::<u128>().unwrap() * 10u128.pow(24) + format!("{frac:0<24}").parse::<u128>().unwrap()
}

fn tgas(s: &str) -> Gas {
    Gas::from_tgas(s.trim_end_matches(" Tgas").parse::<f64>().unwrap() as u64)
}

fn near(y: u128) -> f64 {
    y as f64 / 1e24
}

/// Run the script in DRY mode and return its plan, one command per line.
fn plan(attester: &str) -> Vec<Value> {
    let out = std::env::temp_dir().join(format!("deploy_sequence_{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let r = std::process::Command::new("bash")
        .arg(script())
        .env("SUFFIX", SUFFIX)
        .env("DRY", "1")
        .env("PLAN", &out)
        .env("STEP", STEPS)
        .env("ATTESTER_KEY", attester)
        .output()
        .expect("bash");
    assert!(r.status.success(), "script failed: {}", String::from_utf8_lossy(&r.stderr));
    let text = std::fs::read_to_string(&out).expect("the script wrote no plan");
    let _ = std::fs::remove_file(&out);
    text.lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// A sub-account chain under `testnet`, e.g. mev-33.factory.v9.meme-cooking.testnet.
async fn make(top: &Account, id: &str, near: u128) -> anyhow::Result<Account> {
    let mut parts: Vec<&str> = id.strip_suffix(".testnet").unwrap().split('.').collect();
    parts.reverse();
    let mut acc = top.clone();
    let n = parts.len() as u128;
    for (i, p) in parts.into_iter().enumerate() {
        // Each parent keeps enough to make the next level down.
        let bal = near + 5 * (n - 1 - i as u128);
        acc = acc.create_subaccount(p).initial_balance(NearToken::from_near(bal)).transact().await?.into_result()?;
    }
    Ok(acc)
}

/// Rhea's real code from testnet (a read), initialised here. Ok(false): testnet
/// unreachable (the caller falls back to a stand-in and says so). Err: Rhea's init changed.
async fn real_rhea(w: &Worker<Sandbox>, owner: &Account) -> anyhow::Result<bool> {
    let id: AccountId = "ref-finance-101.testnet".parse()?;
    let Ok(testnet) = near_workspaces::testnet().await else { return Ok(false) };
    // Its code is ~900 KB: the account must hold ~90 NEAR of storage.
    if let Err(e) = w.import_contract(&id, &testnet).initial_balance(NearToken::from_near(200)).transact().await {
        println!("testnet unreachable: {e}");
        return Ok(false);
    }
    // Ref's init has changed over versions: the newer one names its farm and lending contracts.
    let newer = json!({"owner_id": owner.id(), "boost_farm_id": owner.id(), "burrowland_id": owner.id(), "exchange_fee": 0, "referral_fee": 0});
    let older = json!({"owner_id": owner.id(), "exchange_fee": 0, "referral_fee": 0});
    let mut why = String::new();
    for args in [newer, older] {
        let r = owner.call(&id, "new").args_json(args).transact().await?;
        if r.is_success() {
            return Ok(true);
        }
        let all = format!("{:?}", r.failures());
        why += all.split("panicked at ").nth(1).unwrap_or(&all).split("',").next().unwrap_or_default();
        why += " | ";
    }
    anyhow::bail!("Rhea's init: {why}")
}

#[tokio::test]
async fn the_testnet_deploy_script_runs_as_written() -> anyhow::Result<()> {
    let w = near_workspaces::sandbox().await?;
    let root = w.root_account()?;
    // The `testnet` top-level account (sandbox patch), signed with root's key.
    let sk = root.secret_key().clone();
    w.patch(&"testnet".parse()?)
        .account(AccountDetailsPatch::default().balance(NearToken::from_near(100_000)).storage_usage(200))
        .access_key(sk.public_key(), AccessKey::full_access())
        .transact()
        .await?;
    let top = Account::from_secret_key("testnet".parse()?, sk, &w);

    // Our accounts' keys first: the price board's attester is the keeper's key.
    let keys: HashMap<String, SecretKey> = ["brain", "relay", "board", "owner", "keeper"]
        .iter()
        .map(|n| (format!("nf-{n}-{SUFFIX}.testnet"), SecretKey::from_random(KeyType::ED25519)))
        .collect();
    let keeper_id = format!("nf-keeper-{SUFFIX}.testnet");
    let steps = plan(&keys[&keeper_id].public_key().to_string());
    println!("plan: {} commands", steps.len());

    // Stand-ins at the real names.
    for (id, dec) in [("wrap.testnet", 24), ("usdt.itachicara.testnet", 24), ("mev-33.factory.v9.meme-cooking.testnet", 18)] {
        let t = make(&top, id, 10).await?;
        t.deploy(&wasm("mock_ft")).await?.into_result()?;
        t.call(t.id(), "new").args_json(json!({"owner_id": top.id(), "total_supply": "1000000000000000000000000000000", "name": id, "symbol": "T", "decimals": dec}))
            .transact().await?.into_result()?;
    }
    let pyth = make(&top, "pyth-oracle.testnet", 10).await?;
    pyth.deploy(&wasm("mock_pyth")).await?.into_result()?;
    pyth.call(pyth.id(), "new").transact().await?.into_result()?;
    make(&top, "linear-protocol.testnet", 10).await?;
    let real = real_rhea(&w, &root).await?;
    if !real {
        // A token contract answers storage_deposit; register_tokens can't be checked.
        println!("SKIPPED Rhea's real code (offline): register_tokens is not checked");
        let r = make(&top, "ref-finance-101.testnet", 10).await?;
        r.deploy(&wasm("mock_ft")).await?.into_result()?;
        r.call(r.id(), "new").args_json(json!({"owner_id": top.id(), "total_supply": "1", "name": "rhea", "symbol": "R", "decimals": 24})).transact().await?.into_result()?;
    }

    // Replay.
    let mut accs: HashMap<String, Account> = HashMap::new();
    let mut faucet = 0u128;
    for (i, s) in steps.iter().enumerate() {
        let op = s["op"].as_str().unwrap();
        let str_of = |k: &str| s[k].as_str().unwrap_or_default().to_string();
        if op == "create" {
            let id = str_of("account");
            faucet = yocto(&format!("{} NEAR", str_of("near")));
            let prefix = id.strip_suffix(".testnet").unwrap();
            let a = top.create_subaccount(prefix).initial_balance(NearToken::from_yoctonear(faucet)).keys(keys[&id].clone()).transact().await?.into_result()?;
            accs.insert(id, a);
            continue;
        }
        let who = if op == "deploy" { str_of("account") } else { str_of("signer") };
        if !accs.contains_key(&who) {
            anyhow::bail!("step {i}: no key for {who} (the script signs with an account it didn't create)");
        }
        let a = accs[&who].clone();
        let (label, r) = match op {
            "deploy" => {
                let code = wasm(&str_of("wasm"));
                let r = a.batch(a.id()).deploy(&code)
                    .call(Function::new(&str_of("method")).args(str_of("args").into_bytes()).gas(tgas(&str_of("gas"))))
                    .transact().await?;
                (format!("deploy {} -> {}", str_of("wasm"), who), r)
            }
            "call" => {
                let args = if s.get("file").is_some() { wasm(&str_of("file")) } else { str_of("args").into_bytes() };
                let contract: AccountId = str_of("contract").parse()?;
                let r = a.call(&contract, &str_of("method")).args(args).deposit(NearToken::from_yoctonear(yocto(&str_of("deposit"))))
                    .gas(tgas(&str_of("gas"))).transact().await?;
                let what = if str_of("method") == "x" { str_of("args").split('"').nth(1).unwrap_or("?").to_string() } else { str_of("method") };
                (format!("{who} -> {contract}.{what}"), r)
            }
            "send" => {
                let to: AccountId = str_of("to").parse()?;
                let r = a.transfer_near(&to, NearToken::from_yoctonear(yocto(&str_of("deposit")))).await?;
                assert!(r.is_success(), "step {i}: send failed: {r:?}");
                println!("  ok {i:>2} send {} -> {to}", str_of("deposit"));
                continue;
            }
            _ => anyhow::bail!("unknown op {op}"),
        };
        assert!(r.is_success(), "step {i} ({label}) failed: {:?}", r.failures());
        println!("  ok {i:>2} {label}: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    }

    // What the script built is what the contracts say they are.
    let id = |n: &str| -> AccountId { format!("nf-{n}-{SUFFIX}.testnet").parse().unwrap() };
    let vault: AccountId = format!("demo.{}", id("relay")).parse()?;
    let view = |c: AccountId, m: &'static str, a: Value| {
        let w = w.clone();
        async move { anyhow::Ok(w.view(&c, m).args_json(a).await?.json::<Value>()?) }
    };
    assert_eq!(view(id("brain"), "get_factory", json!({})).await?, json!(id("relay")));
    assert_eq!(view(id("relay"), "get_brain", json!({})).await?, json!(id("brain")));
    assert!(view(id("relay"), "get_code_hash", json!({})).await?.is_string(), "relay published");
    assert_eq!(view(id("relay"), "get_vault", json!({"owner": id("owner")})).await?, json!(vault));
    let rec = view(id("brain"), "get_vault", json!({"vault": vault})).await?;
    assert_eq!(rec["owner"], json!(id("owner")));
    assert_eq!(rec["keeper"], json!(id("keeper")));
    assert_eq!(rec["trusted"].as_array().unwrap().len(), 5);
    let oracle = view(id("brain"), "get_oracle", json!({"vault": vault})).await?;
    let feeds: Vec<&str> = oracle["feeds"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(feeds, ["near", "usdc", "eth", "sol", "nvda"]);
    let flow = view(id("brain"), "get_flow", json!({"vault": vault, "flow_id": 0})).await?;
    assert_eq!(flow["nodes"].as_array().unwrap().len(), 5);
    let allocated = view(id("brain"), "get_allocated", json!({"vault": vault, "token": "near"})).await?;
    assert_eq!(allocated, json!("3500000000000000000000000"), "1 + 2 + 0.5 NEAR funded");
    let source = view(id("board"), "get_source", json!({})).await?;
    assert_eq!(source["attesters"], json!([keys[&keeper_id].public_key().to_string()]));
    if real {
        let mine = view("ref-finance-101.testnet".parse()?, "get_user_whitelisted_tokens", json!({"account_id": vault})).await?;
        assert!(mine.to_string().contains("mev-33.factory.v9.meme-cooking.testnet"), "the vault registered the memecoin on Rhea: {mine}");
    }

    // The funding table, measured: each account started with the faucet's 10 NEAR;
    // used = spent (deposits, burns, gas) + what its storage now locks. (The brain also
    // RECEIVES: vaults pay it for their records, so its "used" comes out a little lower.)
    let storage = view(id("brain"), "get_storage", json!({"vault": vault})).await?;
    println!("\nbrain record for the vault: costs {:.4} NEAR, paid {:.4}", near(storage[0].as_str().unwrap().parse()?), near(storage[1].as_str().unwrap().parse()?));
    println!("account                          used    locked   free left");
    let mut names: Vec<&String> = accs.keys().collect();
    names.sort();
    for n in names {
        let d = accs[n].view_account().await?;
        let (bal, locked) = (d.balance.as_yoctonear(), d.storage_usage as u128 * 10u128.pow(19));
        println!("{n:<30} {:>7.4}  {:>7.4}  {:>9.4}", near(faucet) - near(bal) + near(locked), near(locked), near(bal - locked));
        assert!(bal > locked + 10u128.pow(23), "{n} is down to its last 0.1 NEAR: the faucet's 10 NEAR isn't enough");
    }
    let v = w.view_account(&vault).await?;
    println!("{:<30} balance {:.4}, storage {} B", vault.as_str(), near(v.balance.as_yoctonear()), v.storage_usage);
    Ok(())
}
