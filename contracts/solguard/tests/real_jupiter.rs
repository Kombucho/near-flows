//! Real Jupiter transactions (fetched from lite-api.jup.ag on 2026-11-05 for the
//! demo vault's derived Solana address), checked by the guard.

use base64::Engine;
use solguard::*;

const VAULT_SOL: &str = "CsqGoKFb1MjRfi1ob3Mj8Y9yEFsWGBpEBtjz92xGrYGB"; // demo.nf-factory-ksleb.testnet, path solana-1
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const WSOL: &str = "So11111111111111111111111111111111111111112";

fn tx(name: &str) -> Vec<u8> {
    let b64 = std::fs::read_to_string(format!("{}/tests/vectors/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
    base64::engine::general_purpose::STANDARD.decode(b64.trim()).unwrap()
}

fn policy(floor_e12: u128) -> SwapPolicy {
    SwapPolicy { authority: pubkey(VAULT_SOL), in_mint: pubkey(USDC), out_mint: pubkey(WSOL), max_in: 100_000,
        min_out_per_in_e12: floor_e12, max_cu_price: 100_000, max_cu_limit: 1_400_000 }
}

#[test]
fn a_real_jupiter_swap_for_the_vault_passes() {
    let t = tx("jupiter-legacy-usdc-sol.tx.base64");
    let m = message_of_transaction(&t).unwrap();
    let parsed = Message::parse(m).unwrap();
    println!("programs: {:?}", parsed.instructions.iter().map(|i| bs58::encode(parsed.key(i.program_index).unwrap()).into_string()).collect::<Vec<_>>());
    // Jupiter quoted 111,074 lamports for 10,000 micro-USDC with 1% slippage (min 109,964).
    let v = check_swap(m, &policy(10_000_000_000_000)).unwrap();
    println!("{v:?}");
    assert_eq!((v.in_amount, v.quoted_out, v.slippage_bps), (10_000, 111_074, 100));
    assert!(v.min_out >= 109_963);
}

#[test]
fn the_same_real_swap_is_refused_under_a_higher_floor_or_for_another_vault() {
    let t = tx("jupiter-legacy-usdc-sol.tx.base64");
    let m = message_of_transaction(&t).unwrap();
    // The owner wants at least 11.5 lamports per micro-USDC: 1% slippage allows 10.99.
    assert!(matches!(check_swap(m, &policy(11_500_000_000_000)), Err(Refusal::Swap(ref s)) if s.contains("floor")));
    // Someone else's address as the authority: refused (fee payer check first).
    let mut p = policy(10_000_000_000_000);
    p.authority = pubkey("DfXeFAtpcZ9KRuC2r5QKtiVsoiQ3jMsmsGVFwNhiBMkJ");
    assert_eq!(check_swap(m, &p), Err(Refusal::FeePayer));
    // A pair the owner didn't allow.
    let mut p = policy(10_000_000_000_000);
    p.out_mint = pubkey(USDC);
    assert!(check_swap(m, &p).is_err());
}

#[test]
fn a_real_v0_jupiter_swap_with_lookup_tables_is_refused() {
    let t = tx("jupiter-v0-usdc-sol.tx.base64");
    let m = message_of_transaction(&t).unwrap();
    let r = check_swap(m, &policy(10_000_000_000_000));
    println!("{r:?}");
    assert_eq!(r, Err(Refusal::LookupTables), "accounts hidden in tables can't be checked");
}
