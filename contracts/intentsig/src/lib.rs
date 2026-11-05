//! INTENTSIG: the vault writes its own NEAR Intents and signs them through
//! Chain Signatures. The vault registers its MPC-derived Ed25519 key on its own
//! Intents account; since only the vault can ask the MPC network to sign with
//! that key, the only intents that ever get signed are the ones built here:
//! "give exactly X of A, receive at least Y of B, before this deadline".
//!
//! Formats follow intents.near v0.4.5 (github.com/near/intents, tag defuse/v0.4.5):
//! - message: JSON {"signer_id", "deadline" (RFC 3339), "intents":[{"intent":"token_diff","diff":{..}}]}
//! - NEP-413: sha256(borsh(2^31+413 as u32, {message, nonce[32], recipient, callback_url: None}))
//! - nonce (versioned V1): 5628f6c6 || 00 || salt[4] || deadline ns i64 LE || 15 random bytes

use sha2::{Digest, Sha256};

pub const NEP413_TAG: u32 = 2_147_484_061; // 2^31 + 413
pub const NONCE_MAGIC: [u8; 4] = [0x56, 0x28, 0xf6, 0xc6];

/// Versioned V1 nonce: magic, version 0, the verifier's current salt, the deadline, randomness.
pub fn versioned_nonce(salt: [u8; 4], deadline_ns: i64, random: [u8; 15]) -> [u8; 32] {
    let mut n = [0u8; 32];
    n[..4].copy_from_slice(&NONCE_MAGIC);
    n[4] = 0;
    n[5..9].copy_from_slice(&salt);
    n[9..17].copy_from_slice(&deadline_ns.to_le_bytes());
    n[17..].copy_from_slice(&random);
    n
}

/// RFC 3339 (UTC, second precision) from nanoseconds since 1970.
pub fn rfc3339(ns: u64) -> String {
    let secs = ns / 1_000_000_000;
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The intent message: `signer` gives `amount_in` of `token_in` and receives at least
/// `min_out` of `token_out` (NEP-141 contracts), valid until `deadline_ns`.
pub fn swap_message(signer: &str, deadline_ns: u64, token_in: &str, amount_in: u128, token_out: &str, min_out: u128) -> String {
    format!(
        r#"{{"signer_id":{},"deadline":{},"intents":[{{"intent":"token_diff","diff":{{{}:"-{amount_in}",{}:"{min_out}"}}}}]}}"#,
        json_str(signer),
        json_str(&rfc3339(deadline_ns)),
        json_str(&format!("nep141:{token_in}")),
        json_str(&format!("nep141:{token_out}")),
    )
}

/// What the key signs: the NEP-413 prehash.
pub fn nep413_hash(message: &str, nonce: &[u8; 32], recipient: &str) -> [u8; 32] {
    let mut b = Vec::with_capacity(64 + message.len() + recipient.len());
    b.extend(NEP413_TAG.to_le_bytes());
    b.extend((message.len() as u32).to_le_bytes());
    b.extend(message.as_bytes());
    b.extend(nonce);
    b.extend((recipient.len() as u32).to_le_bytes());
    b.extend(recipient.as_bytes());
    b.push(0); // callback_url: None
    Sha256::digest(&b).into()
}

/// The signed payload for `execute_intents`.
pub fn signed_json(message: &str, nonce: &[u8; 32], recipient: &str, public_key_b58: &str, signature_b58: &str) -> String {
    use base64::Engine;
    format!(
        r#"{{"standard":"nep413","payload":{{"message":{},"nonce":"{}","recipient":{}}},"public_key":"ed25519:{public_key_b58}","signature":"ed25519:{signature_b58}"}}"#,
        json_str(message),
        base64::engine::general_purpose::STANDARD.encode(nonce),
        json_str(recipient),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_match_the_intents_crate() {
        // Cases from defuse_time's own tests.
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(1_782_395_622_123_456_789), "2026-06-25T13:53:42Z");
        assert_eq!(rfc3339(951_782_400 * 1_000_000_000), "2000-02-29T00:00:00Z", "a leap day");
    }

    #[test]
    fn nonce_layout() {
        let n = versioned_nonce([1, 2, 3, 4], 0x0102030405060708, [9; 15]);
        assert_eq!(&n[..5], &[0x56, 0x28, 0xf6, 0xc6, 0]);
        assert_eq!(&n[5..9], &[1, 2, 3, 4]);
        assert_eq!(&n[9..17], &0x0102030405060708i64.to_le_bytes());
        assert_eq!(&n[17..], &[9; 15]);
    }

    #[test]
    fn the_message_says_exactly_give_x_get_at_least_y() {
        let m = swap_message("v.near", 0, "wrap.near", 100, "usdc.near", 95);
        assert_eq!(m, r#"{"signer_id":"v.near","deadline":"1970-01-01T00:00:00Z","intents":[{"intent":"token_diff","diff":{"nep141:wrap.near":"-100","nep141:usdc.near":"95"}}]}"#);
        assert!(swap_message("a\"b", 0, "x", 1, "y", 1).contains(r#""a\"b""#), "quotes can't break out of the JSON");
    }

    #[test]
    fn a_dalek_signature_over_the_hash_verifies() {
        use ed25519_dalek::{Signer, SigningKey, Verifier};
        let key = SigningKey::from_bytes(&[7; 32]);
        let h = nep413_hash("hello", &[1; 32], "intents.near");
        let sig = key.sign(&h);
        assert!(key.verifying_key().verify(&h, &sig).is_ok());
    }
}
