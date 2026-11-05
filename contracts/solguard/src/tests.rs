//! Synthetic messages built here, one honest swap and one attack per rule.
//! (Real Jupiter transactions are checked in `real_jupiter`.)

use crate::testkit::*;
use crate::*;

#[test]
fn an_honest_swap_passes_and_reports_what_it_will_do() {
    let v = check_swap(&honest().bytes(), &policy()).unwrap();
    assert_eq!(v.in_amount, 10_000_000);
    assert_eq!(v.min_out, 50_000_000 * 9_950 / 10_000, "quoted minus 0.5% slippage");
    assert_eq!(v.max_priority_fee_lamports, 30_000, "50,000 micro-lamports x 600,000 units");
}

#[test]
fn a_plain_transfer_is_refused() {
    // The classic drain: System transfer of all SOL to an attacker.
    let v = key(VAULT);
    let mut data = vec![2, 0, 0, 0];
    data.extend(1_000_000_000u64.to_le_bytes());
    let tx = Tx::new(v).ix(pubkey(SYSTEM_PROGRAM), vec![v, key(66)], data).bytes();
    assert_eq!(check_swap(&tx, &policy()), Err(Refusal::Program(SYSTEM_PROGRAM.into())));
    // A token transfer, or an approval for someone to pull tokens later.
    let tx = Tx::new(v).ix(pubkey(TOKEN_PROGRAM), vec![ata_of(&v, &key(USDC)), key(66), v], vec![3, 1, 0, 0, 0, 0, 0, 0, 0]).bytes();
    assert!(matches!(check_swap(&tx, &policy()), Err(Refusal::Program(_))));
}

#[test]
fn a_swap_paying_someone_else_is_refused() {
    let (v, usdc, sol, thief) = (key(VAULT), key(USDC), key(SOL), key(66));
    let mk = |dst: Pubkey, src: Pubkey, fee: Pubkey, auth: Pubkey| {
        Tx::new(v).ix(pubkey(JUPITER_V6), shared_accounts(auth, src, dst, usdc, sol, fee), jup_data("shared_accounts_route", 1_000_000, 5_000_000, 50, 0)).bytes()
    };
    let ok_src = ata_of(&v, &usdc);
    let r = check_swap(&mk(ata_of(&thief, &sol), ok_src, pubkey(JUPITER_V6), v), &policy());
    assert!(matches!(r, Err(Refusal::Swap(ref s)) if s.contains("destination")), "{r:?}");
    let r = check_swap(&mk(key(55), ok_src, pubkey(JUPITER_V6), v), &policy());
    assert!(matches!(r, Err(Refusal::Swap(ref s)) if s.contains("destination")), "a random account: {r:?}");
    let r = check_swap(&mk(ata_of(&v, &sol), ok_src, ata_of(&thief, &sol), v), &policy());
    assert!(matches!(r, Err(Refusal::Swap(ref s)) if s.contains("platform fee")), "a fee account: {r:?}");
    let r = check_swap(&mk(ata_of(&v, &sol), ata_of(&thief, &usdc), pubkey(JUPITER_V6), v), &policy());
    assert!(matches!(r, Err(Refusal::Swap(ref s)) if s.contains("source")), "someone else's source: {r:?}");
}

#[test]
fn a_platform_fee_a_bad_price_or_too_much_is_refused() {
    let (v, usdc, sol) = (key(VAULT), key(USDC), key(SOL));
    let mk = |in_amount: u64, quoted: u64, slip: u16, fee: u8| {
        Tx::new(v).ix(pubkey(JUPITER_V6), shared_accounts(v, ata_of(&v, &usdc), ata_of(&v, &sol), usdc, sol, pubkey(JUPITER_V6)),
            jup_data("shared_accounts_route", in_amount, quoted, slip, fee)).bytes()
    };
    assert!(matches!(check_swap(&mk(1_000_000, 5_000_000, 50, 20), &policy()), Err(Refusal::Swap(ref s)) if s.contains("no platform fee")));
    // Floor: 4 per unit -> 1,000,000 in needs >= 4,000,000 out after slippage.
    assert!(check_swap(&mk(1_000_000, 4_100_000, 100, 0), &policy()).is_ok(), "4,059,000 after 1%");
    assert!(matches!(check_swap(&mk(1_000_000, 4_100_000, 300, 0), &policy()), Err(Refusal::Swap(ref s)) if s.contains("floor")), "3% slippage dips under");
    assert!(matches!(check_swap(&mk(1_000_000, 4_000_000, 10_000, 0), &policy()), Err(Refusal::Swap(_))), "100% slippage = no minimum");
    assert!(matches!(check_swap(&mk(100_000_001, u64::MAX / 20_000, 50, 0), &policy()), Err(Refusal::Swap(ref s)) if s.contains("limit")));
}

#[test]
fn the_wrong_pair_signer_or_shape_is_refused() {
    let (v, usdc, sol) = (key(VAULT), key(USDC), key(SOL));
    let jup = |auth: Pubkey, in_mint: Pubkey, out_mint: Pubkey| Tx::new(v)
        .ix(pubkey(JUPITER_V6), shared_accounts(auth, ata_of(&v, &in_mint), ata_of(&v, &out_mint), in_mint, out_mint, pubkey(JUPITER_V6)), jup_data("shared_accounts_route", 1_000_000, 5_000_000, 50, 0));
    assert!(matches!(check_swap(&jup(v, usdc, key(9)).bytes(), &policy()), Err(Refusal::Swap(_))), "another output token");
    assert!(matches!(check_swap(&jup(key(8), usdc, sol).bytes(), &policy()), Err(Refusal::Swap(ref s)) if s.contains("authority")));
    let mut two = jup(v, usdc, sol);
    two.signers = 2;
    assert_eq!(check_swap(&two.bytes(), &policy()), Err(Refusal::Signers), "a second signer could be anyone");
    let other_payer = Tx::new(key(66)).ix(pubkey(JUPITER_V6), shared_accounts(v, ata_of(&v, &usdc), ata_of(&v, &sol), usdc, sol, pubkey(JUPITER_V6)), jup_data("shared_accounts_route", 1, 5, 0, 0));
    assert_eq!(check_swap(&other_payer.bytes(), &policy()), Err(Refusal::FeePayer));
    let mut lut = jup(v, usdc, sol);
    lut.v0_lookups = 1;
    assert_eq!(check_swap(&lut.bytes(), &policy()), Err(Refusal::LookupTables), "accounts hidden in a lookup table can't be checked");
    let twice = jup(v, usdc, sol).ix(pubkey(JUPITER_V6), shared_accounts(v, ata_of(&v, &usdc), ata_of(&v, &sol), usdc, sol, pubkey(JUPITER_V6)), jup_data("shared_accounts_route", 1_000_000, 5_000_000, 50, 0));
    assert_eq!(check_swap(&twice.bytes(), &policy()), Err(Refusal::TwoSwaps), "two swaps would double the spend");
    let exact_out = Tx::new(v).ix(pubkey(JUPITER_V6), shared_accounts(v, ata_of(&v, &usdc), ata_of(&v, &sol), usdc, sol, pubkey(JUPITER_V6)), jup_data("exact_out_route", 1, 5, 0, 0));
    assert!(matches!(check_swap(&exact_out.bytes(), &policy()), Err(Refusal::Swap(ref s)) if s.contains("only route")));
    assert_eq!(check_swap(&Tx::new(v).bytes(), &policy()), Err(Refusal::NoSwap));
    let mut junk = honest().bytes();
    junk.push(0);
    assert_eq!(check_swap(&junk, &policy()), Err(Refusal::Parse(ParseError::Trailing)));
    assert!(matches!(check_swap(&junk[..40], &policy()), Err(Refusal::Parse(_))));
}

#[test]
fn fees_are_capped_and_token_accounts_are_only_the_vaults_own() {
    let (v, sol) = (key(VAULT), key(SOL));
    let pricey = honest().ix(pubkey(COMPUTE_BUDGET), vec![], [vec![3], 10_000_000u64.to_le_bytes().to_vec()].concat());
    assert!(matches!(check_swap(&pricey.bytes(), &policy()), Err(Refusal::ComputeBudget(_))), "a huge priority fee drains SOL");
    let theirs = Tx::new(v).ix(pubkey(ATA_PROGRAM), vec![v, ata_of(&key(66), &sol), key(66), sol, pubkey(SYSTEM_PROGRAM), pubkey(TOKEN_PROGRAM)], vec![1]);
    assert!(matches!(check_swap(&theirs.bytes(), &policy()), Err(Refusal::AtaCreate(_))), "paying rent for someone else's account");
    let recover = Tx::new(v).ix(pubkey(ATA_PROGRAM), vec![v, ata_of(&v, &sol), v, sol, pubkey(SYSTEM_PROGRAM), pubkey(TOKEN_PROGRAM)], vec![2]);
    assert!(matches!(check_swap(&recover.bytes(), &policy()), Err(Refusal::AtaCreate(_))), "RecoverNested moves tokens");
}

#[test]
fn discriminators_match_anchor() {
    // Anchor: sha256("global:route")[..8]. Cross-checked against Jupiter's IDL in `real_jupiter`.
    assert_eq!(discriminator("route").len(), 8);
    assert_ne!(discriminator("route"), discriminator("shared_accounts_route"));
}

#[test]
fn jupiter_v2_and_exact_out_instructions_are_refused() {
    // Discriminators from Jupiter's on-chain IDL (2026-11-05). v2 puts the amounts first, so the
    // 19-byte tail would read the wrong numbers: refuse rather than misread.
    let (v, usdc, sol) = (key(VAULT), key(USDC), key(SOL));
    for (name, disc) in [("route_v2", "bb64facc31c4af14"), ("shared_accounts_route_v2", "d19853937cfed8e9"),
                         ("exact_out_route", "d033ef977b2bed5c"), ("shared_accounts_exact_out_route", "b0d169a89a7d453e")] {
        assert_eq!(hex_of(&discriminator(name)), disc, "{name}: our discriminator matches the IDL");
        let tx = Tx::new(v).ix(pubkey(JUPITER_V6), shared_accounts(v, ata_of(&v, &usdc), ata_of(&v, &sol), usdc, sol, pubkey(JUPITER_V6)),
            jup_data(name, 1_000_000, 5_000_000, 50, 0)).bytes();
        assert!(matches!(check_swap(&tx, &policy()), Err(Refusal::Swap(ref s)) if s.contains("only route")), "{name}");
    }
    assert_eq!(hex_of(&discriminator("route")), "e517cb977ae3ad2a");
    assert_eq!(hex_of(&discriminator("shared_accounts_route")), "c1209b3341d69c81");
}

fn hex_of(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
