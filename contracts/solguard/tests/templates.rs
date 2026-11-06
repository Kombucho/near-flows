//! Solana templates: a pump.fun buy and a Meteora position the owner co-signs,
//! with the honest transaction and each way a helper could try to cheat.

use solguard::template::{check_tx, Acc, Field, FieldRule, Template, TxPolicy, Ty};
use solguard::testkit::*;
use solguard::{discriminator, pubkey, Refusal, TOKEN_PROGRAM};

const PUMP: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const DLMM: &str = "LBUZKhRxPF3XUpBCjp4YzTKgLccjZhTSDM9YuVaPwxo";

fn vault() -> [u8; 32] {
    key(VAULT)
}
fn owner() -> [u8; 32] {
    key(77)
}
fn policy() -> TxPolicy {
    TxPolicy { authority: vault(), owner: Some(owner()), max_cu_price: 1_000_000, max_cu_limit: 400_000 }
}

/// pump.fun buy_exact_sol_in: spend at most 0.5 SOL, get at least 1,000 tokens per 0.000001 SOL... per lamport here: 1 token unit per lamport.
fn pump_buy() -> Template {
    let mut a = vec![Acc::Any; 16];
    a[5] = Acc::VaultAta { mint_at: 2, program_at: 8 }; // associated_user: the vault's own token account for the coin
    a[6] = Acc::Vault; // user
    a[7] = Acc::Fixed(pubkey("11111111111111111111111111111111"));
    a[11] = Acc::Fixed(pubkey(PUMP));
    Template {
        name: "pump_buy".into(),
        program: pubkey(PUMP),
        prefix: discriminator("buy_exact_sol_in").to_vec(),
        fields: vec![
            Field { ty: Ty::U64, rule: FieldRule::AtMost(500_000_000) },
            Field { ty: Ty::U64, rule: FieldRule::AtLeast { of: 0, num: 1, den: 1 } },
            Field { ty: Ty::Bool, rule: FieldRule::Any },
            Field { ty: Ty::Bool, rule: FieldRule::Fixed(vec![0]) },
        ],
        accounts: a,
        extra_accounts: 2, // bonding_curve_v2, buyback_fee_recipient (2026-09 upgrade)
        needs_owner: false,
    }
}

fn pump_data(sol_in: u64, min_out: u64) -> Vec<u8> {
    let mut d = discriminator("buy_exact_sol_in").to_vec();
    d.extend(sol_in.to_le_bytes());
    d.extend(min_out.to_le_bytes());
    d.extend([1, 0]);
    d
}

fn pump_accounts(user_ata: [u8; 32], user: [u8; 32]) -> Vec<[u8; 32]> {
    let mint = key(40);
    let mut a: Vec<[u8; 32]> = (0..16u8).map(|i| key(100 + i)).collect();
    a[2] = mint;
    a[5] = user_ata;
    a[6] = user;
    a[7] = pubkey("11111111111111111111111111111111");
    a[8] = pubkey(TOKEN_PROGRAM);
    a[11] = pubkey(PUMP);
    a.extend([key(140), key(141)]); // the two trailing accounts
    a
}

#[test]
fn a_pump_buy_passes_only_as_the_owner_allows() {
    let (v, mint) = (vault(), key(40));
    let mine = ata_of(&v, &mint);
    let ok = Tx::new(v).ix(pubkey(PUMP), pump_accounts(mine, v), pump_data(100_000_000, 150_000_000)).bytes();
    let m = check_tx(&ok, &[pump_buy()], &policy()).expect("honest buy");
    assert_eq!(m[0].values[..2], [100_000_000, 150_000_000]);

    let thief = key(66);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("coins to the thief's token account", Tx::new(v).ix(pubkey(PUMP), pump_accounts(ata_of(&thief, &mint), v), pump_data(100_000_000, 150_000_000)).bytes()),
        ("someone else as the buyer", Tx::new(v).ix(pubkey(PUMP), pump_accounts(mine, thief), pump_data(100_000_000, 150_000_000)).bytes()),
        ("over 0.5 SOL", Tx::new(v).ix(pubkey(PUMP), pump_accounts(mine, v), pump_data(600_000_000, 900_000_000)).bytes()),
        ("under the floor", Tx::new(v).ix(pubkey(PUMP), pump_accounts(mine, v), pump_data(100_000_000, 99_999_999)).bytes()),
        ("a partial fill", { let mut d = pump_data(100_000_000, 150_000_000); *d.last_mut().unwrap() = 1; Tx::new(v).ix(pubkey(PUMP), pump_accounts(mine, v), d).bytes() }),
        ("a SOL transfer riding along", Tx::new(v).ix(pubkey(PUMP), pump_accounts(mine, v), pump_data(1, 1))
            .ix(pubkey("11111111111111111111111111111111"), vec![v, thief], vec![2, 0, 0, 0, 0, 202, 154, 59, 0, 0, 0, 0]).bytes()),
        ("a stranger as extra signer", Tx::new(v).signer(thief).ix(pubkey(PUMP), pump_accounts(mine, v), pump_data(1, 1)).bytes()),
        ("a third trailing account", { let mut a = pump_accounts(mine, v); a.push(key(142)); Tx::new(v).ix(pubkey(PUMP), a, pump_data(1, 1)).bytes() }),
        ("someone else pays the fees", Tx::new(thief).ix(pubkey(PUMP), pump_accounts(mine, v), pump_data(1, 1)).bytes()),
    ];
    for (name, msg) in cases {
        assert!(check_tx(&msg, &[pump_buy()], &policy()).is_err(), "{name} passed");
    }
}

/// Meteora DLMM initialize_position(lower_bin_id, width): payer = vault, a fresh position key, owner = vault...
/// and the OWNER's wallet must co-sign the transaction (Pedro's rule for LP opens).
fn dlmm_open() -> Template {
    Template {
        name: "dlmm_open".into(),
        program: pubkey(DLMM),
        prefix: discriminator("initialize_position").to_vec(),
        fields: vec![Field { ty: Ty::I32, rule: FieldRule::Any }, Field { ty: Ty::I32, rule: FieldRule::Any }],
        accounts: vec![Acc::Vault, Acc::NewSigner, Acc::Any, Acc::Vault, Acc::Any, Acc::Any, Acc::Any, Acc::Fixed(pubkey(DLMM))],
        extra_accounts: 0,
        needs_owner: true,
    }
}


#[test]
fn a_meteora_position_opens_only_with_the_owner_signing() {
    let (v, pos, thief) = (vault(), key(50), key(66));
    let accounts = vec![v, pos, key(51), v, key(52), key(53), key(54), pubkey(DLMM)];
    let data = { let mut d = discriminator("initialize_position").to_vec(); d.extend((-300i32).to_le_bytes()); d.extend(69i32.to_le_bytes()); d };
    let ts = [dlmm_open()];
    let ok = Tx::new(v).signer(owner()).signer(pos).ix(pubkey(DLMM), accounts.clone(), data.clone()).bytes();
    check_tx(&ok, &ts, &policy()).expect("vault + owner + fresh position key");
    // Without the owner's co-signature, a helper can't open positions alone.
    let alone = Tx::new(v).signer(pos).ix(pubkey(DLMM), accounts.clone(), data.clone()).bytes();
    assert!(check_tx(&alone, &ts, &policy()).is_err(), "no owner signature");
    let fake_owner = Tx::new(v).signer(thief).signer(pos).ix(pubkey(DLMM), accounts.clone(), data.clone()).bytes();
    assert!(check_tx(&fake_owner, &ts, &policy()).is_err(), "someone else posing as the owner");
    // The position must be owned by the vault, not by whoever signed.
    let mut stolen = accounts.clone();
    stolen[3] = thief;
    let r = check_tx(&Tx::new(v).signer(owner()).signer(pos).ix(pubkey(DLMM), stolen, data.clone()).bytes(), &ts, &policy());
    assert!(matches!(r, Err(Refusal::Program(_))), "{r:?}");
    // The "fresh key" can't be the vault itself (it would be signing for its own account as a position).
    let mut selfpos = accounts;
    selfpos[1] = v;
    assert!(check_tx(&Tx::new(v).signer(owner()).ix(pubkey(DLMM), selfpos, data).bytes(), &ts, &policy()).is_err());
}

#[test]
fn templates_are_checked_when_saved() {
    let mut t = pump_buy();
    t.fields[1].rule = FieldRule::AtLeast { of: 2, num: 1, den: 1 };
    assert!(t.validate().is_err(), "a floor relative to a bool");
    let mut t = pump_buy();
    t.fields[3].rule = FieldRule::Fixed(vec![0, 0]);
    assert!(t.validate().is_err(), "fixed value of the wrong size");
    let mut t = pump_buy();
    t.accounts[5] = Acc::VaultAta { mint_at: 40, program_at: 8 };
    assert!(t.validate().is_err(), "mint position out of range");
    assert!(pump_buy().validate().is_ok() && dlmm_open().validate().is_ok());
}
