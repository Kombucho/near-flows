//! Local mainnet forks (Foundry's anvil) for the EVM template tests.
#![allow(dead_code)]

use std::process::{Child, Command, Stdio};

use serde_json::Value;

/// anvil's first test account (a well-known key, funded on every anvil chain).
pub const ANVIL_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

/// A feed's backup on mock_pyth: the same asset from its second publisher, which
/// mock_pyth serves under the id reversed (its `set_price` sets both).
pub fn backup(contract: impl std::fmt::Display, price_id: &str) -> Value {
    serde_json::json!({"kind": "other", "contract": contract.to_string(), "price_id": price_id.chars().rev().collect::<String>()})
}

/// A feed with no independent second publisher (NVDA), said plainly.
pub fn single_source() -> Value {
    serde_json::json!({"kind": "single_source"})
}

pub fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

/// The node library's templates for one chain, exactly as published.
pub fn templates(chain: &str) -> Vec<Value> {
    let lib: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../../nodes/evm-templates.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    lib["chains"][chain]["templates"].as_array().unwrap().clone()
}

/// A Solana template from the node library (nodes/sol-templates.json), exactly as published,
/// with the owner's per-coin values filled into its {placeholders}.
pub fn sol_template(name: &str, fill: &[(&str, &str)]) -> Value {
    let lib: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../../nodes/sol-templates.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    let t = lib["templates"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no Solana template {name}"));
    filled(name, t, fill)
}

/// An EVM Intents deposit template (nodes/evm-templates.json, a chain's `intents_deposits`),
/// with the owner's deposit address and caps filled in.
pub fn intents_deposit_template(chain: &str, name: &str, fill: &[(&str, &str)]) -> Value {
    let lib: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../../nodes/evm-templates.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    let t = lib["chains"][chain]["intents_deposits"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no {chain} deposit template {name}"));
    filled(name, t, fill)
}

fn filled(name: &str, t: &Value, fill: &[(&str, &str)]) -> Value {
    let mut text = t.to_string();
    for (k, v) in fill {
        text = text.replace(&format!("{{{k}}}"), v);
    }
    let left = text.split('{').skip(1).find(|s| s.split_once('}').is_some_and(|(w, _)| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c == '_')));
    assert!(left.is_none(), "{name}: placeholder {{{}}} left unfilled", left.unwrap_or_default());
    serde_json::from_str(&text).unwrap()
}

fn foundry(bin: &str) -> String {
    let p = format!("{}/.foundry/bin/{bin}", std::env::var("HOME").unwrap_or_default());
    if std::path::Path::new(&p).exists() { p } else { bin.to_string() }
}

/// A running fork; killed when dropped.
pub struct Fork {
    child: Child,
    pub rpc: String,
}

impl Drop for Fork {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl Fork {
    /// Fork `url` on `port` (ANVIL_PORT overrides it). None (test skipped) when Foundry isn't installed.
    pub async fn start(url: &str, port: u16) -> Option<Fork> {
        let port = std::env::var("ANVIL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(port);
        if Command::new(foundry("anvil")).arg("--version").output().is_err() {
            println!("SKIPPED: install Foundry (anvil) to run fork tests");
            return None;
        }
        let child = Command::new(foundry("anvil")).args(["--fork-url", url, "--port", &port.to_string(), "--silent"]).stdout(Stdio::null()).spawn().ok()?;
        let f = Fork { child, rpc: format!("http://127.0.0.1:{port}") };
        for _ in 0..120 {
            if Command::new(foundry("cast")).args(["chain-id", "--rpc-url", &f.rpc]).output().map_or(false, |o| o.status.success()) {
                return Some(f);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        panic!("the fork of {url} didn't start");
    }

    /// Run `cast` against the fork, return trimmed stdout (panics with stderr on failure).
    pub fn cast(&self, args: &[&str]) -> String {
        let out = Command::new(foundry("cast")).args(args).args(["--rpc-url", &self.rpc]).output().expect("cast");
        assert!(out.status.success(), "cast {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    pub fn balance(&self, token: &str, who: &str) -> u128 {
        num(&self.cast(&["call", token, "balanceOf(address)(uint256)", who]))
    }

    /// Broadcast a signed transaction; returns the receipt's status (1 = success).
    pub fn send_raw(&self, raw: &str) -> u64 {
        let hash: String = serde_json::from_str(&self.cast(&["rpc", "eth_sendRawTransaction", raw])).unwrap();
        // `cast receipt` waits until the transaction is mined.
        let receipt: Value = serde_json::from_str(&self.cast(&["receipt", &hash, "--json"])).unwrap();
        u64::from_str_radix(receipt["status"].as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
    }

    /// Give `to` some of `token` by impersonating a holder (a pool, on a fork only).
    pub fn give(&self, token: &str, holder: &str, to: &str, amount: u128) {
        self.cast(&["rpc", "anvil_setBalance", holder, "0xDE0B6B3A7640000"]);
        self.cast(&["rpc", "anvil_impersonateAccount", holder]);
        self.cast(&["send", token, "transfer(address,uint256)", to, &amount.to_string(), "--from", holder, "--unlocked"]);
    }
}

/// The first number cast prints ("222419608 [2.224e8]" -> 222419608).
pub fn num(s: &str) -> u128 {
    s.split_whitespace().next().unwrap().parse().unwrap()
}

/// The raw signed transaction from an evm_call's logs.
pub fn signed(r: &near_workspaces::result::ExecutionFinalResult) -> String {
    let log = r.logs().iter().find(|l| l.contains(r#""event":"evm_signed""#)).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures())).to_string();
    let v: Value = serde_json::from_str(log.trim_start_matches("EVENT_JSON:")).unwrap();
    v["data"][0]["raw"].as_str().unwrap().to_string()
}

/// The Ed25519 signature (hex) a vault logged for a Solana message, if it signed one.
pub fn sol_sig(r: &near_workspaces::result::ExecutionFinalResult) -> Option<String> {
    r.logs().iter().find(|l| l.contains("sol_signed")).map(|l| l.split(r#""signature":""#).nth(1).unwrap().split('"').next().unwrap().to_string())
}

/// A real protocol's wasm as deployed on testnet (fetched by scripts/fetch-protocols.sh).
pub fn protocol(id: &str) -> Vec<u8> {
    let path = format!("{}/protocols/{id}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run scripts/fetch-protocols.sh first"))
}

/// A Solana mainnet fork (surfpool); killed when dropped.
pub struct SolFork {
    child: Child,
    pub rpc: String,
}

impl Drop for SolFork {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

impl SolFork {
    /// On `port` and `port` + 1 (SURFPOOL_PORT overrides it). None (test skipped) when surfpool isn't installed.
    pub async fn start(port: u16) -> Option<SolFork> {
        let port = std::env::var("SURFPOOL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(port);
        let bin = format!("{}/.local/bin/surfpool", std::env::var("HOME").unwrap_or_default());
        let bin = if std::path::Path::new(&bin).exists() { bin } else { "surfpool".to_string() };
        if Command::new(&bin).arg("--version").output().is_err() {
            println!("SKIPPED: install surfpool to run Solana fork tests");
            return None;
        }
        // A private RPC (SURFPOOL_DATASOURCE_RPC_URL) avoids the public one's rate limits. surfpool
        // won't take it together with --network, and it carries an API key, so it's never printed.
        let source = match std::env::var("SURFPOOL_DATASOURCE_RPC_URL") {
            Ok(url) if !url.is_empty() => vec!["--rpc-url".to_string(), url],
            _ => vec!["--network".to_string(), "mainnet".to_string()],
        };
        let child = Command::new(&bin)
            .arg("start")
            .args(&source)
            .args(["--port", &port.to_string(), "--ws-port", &(port + 1).to_string(), "--no-tui", "--no-studio", "--no-deploy", "--skip-blockhash-check", "-y"])
            .stdout(Stdio::null()).stderr(Stdio::null()).spawn().ok()?;
        let f = SolFork { child, rpc: format!("http://127.0.0.1:{port}") };
        for _ in 0..120 {
            if f.rpc_call("getSlot", serde_json::json!([])).get("result").is_some() {
                return Some(f);
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        panic!("surfpool didn't start");
    }

    pub fn rpc_call(&self, method: &str, params: Value) -> Value {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
        let out = Command::new("curl").args(["-s", "-X", "POST", &self.rpc, "-H", "content-type: application/json", "-d", &body]).output().expect("curl");
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }

    /// The keeper's Solana helper (scripts/keeper/src/fork-sol.ts) against this fork; its last line, as JSON.
    pub fn helper(&self, args: &[&str]) -> Value {
        let dir = format!("{}/../../scripts/keeper", env!("CARGO_MANIFEST_DIR"));
        let out = Command::new("npx").args(["tsx", "src/fork-sol.ts"]).args(args).current_dir(dir).env("SOLANA_RPC", &self.rpc).output().expect("npx");
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(text.trim().lines().last().unwrap_or("null")).unwrap_or_else(|_| panic!("helper {args:?}: {text} {}", String::from_utf8_lossy(&out.stderr)))
    }
}
