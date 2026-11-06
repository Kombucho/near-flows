use crate::hidden::fingerprint;
use crate::Recipe;
use flow::Node;

/// The keeper computes the same fingerprint in TypeScript (scripts/keeper/src/hidden.test.ts):
/// two independent encoders of borsh(Node<Recipe>) must agree byte for byte.
#[test]
fn fingerprint_is_pinned() {
    let node: Node<Recipe> = near_sdk::serde_json::from_str(include_str!("../tests-data/fingerprint-node.json")).unwrap();
    let salt = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
    let p = fingerprint(&node, salt);
    assert_eq!(p, PIN);
}

const PIN: &str = "996e6f838080503aa9b3851e34d65d50c9aea0a4f0d21f941e68f9fc9f6008f2";

/// A stealth vault's `exec` makes the vault call itself, which passes every
/// #[private] check. `exec` refuses methods named `on_*`, so every #[private]
/// method (a promise callback) MUST be named `on_*`, or a signed command could
/// fake a callback. This keeps that convention from silently breaking.
#[test]
fn every_private_callback_is_named_on_something() {
    let files = [
        include_str!("lib.rs"), include_str!("step.rs"), include_str!("hidden.rs"), include_str!("oracle.rs"),
        include_str!("evm.rs"), include_str!("sol.rs"), include_str!("soltemplates.rs"), include_str!("intents.rs"),
        include_str!("limits.rs"), include_str!("stealth.rs"),
    ];
    for src in files {
        let lines: Vec<&str> = src.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.trim() == "#[private]" {
                let f = lines[i + 1..].iter().find(|l| l.contains("fn ")).unwrap();
                assert!(f.contains("fn on_"), "a #[private] method not named on_*: {f}");
            }
        }
    }
}
