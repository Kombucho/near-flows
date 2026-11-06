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

const PIN: &str = "dc1b9b766ada17527508e3c4b9ba99fde2d75a9bf5c0d56a28180d4648416eeb";
