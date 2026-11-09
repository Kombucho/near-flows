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
fn a_refusal_names_the_template_the_instruction_was_meant_for() {
    // Two templates for the same program: the reason must come from the one whose discriminator
    // matched (dlmm_open), not from whichever was tried last.
    let mut other = dlmm_open();
    other.name = "dlmm_close".into();
    other.prefix = discriminator("close_position_if_empty").to_vec();
    let (v, pos, thief) = (vault(), key(50), key(66));
    let accounts = vec![v, pos, key(51), thief, key(52), key(53), key(54), pubkey(DLMM)];
    let data = { let mut d = discriminator("initialize_position").to_vec(); d.extend((-300i32).to_le_bytes()); d.extend(69i32.to_le_bytes()); d };
    let msg = Tx::new(v).signer(owner()).signer(pos).ix(pubkey(DLMM), accounts, data).bytes();
    match check_tx(&msg, &[dlmm_open(), other], &policy()) {
        Err(Refusal::Program(why)) => assert!(why.starts_with("dlmm_open: account 3"), "{why}"),
        r => panic!("{r:?}"),
    }
}

const WSOL: &str = "So11111111111111111111111111111111111111112";
const SYSTEM: &str = "11111111111111111111111111111111";

/// Wrapping SOL: a System transfer of at most 1 SOL from the vault into the vault's OWN wSOL
/// account (a mint the instruction doesn't name), then sync_native and close_account on it.
fn wsol_templates() -> Vec<Template> {
    let mine = Acc::VaultAtaOf { mint: pubkey(WSOL), program: pubkey(TOKEN_PROGRAM) };
    vec![
        Template { name: "wsol_wrap".into(), program: pubkey(SYSTEM), prefix: vec![2, 0, 0, 0],
            fields: vec![Field { ty: Ty::U64, rule: FieldRule::AtMost(1_000_000_000) }],
            accounts: vec![Acc::Vault, mine.clone()], extra_accounts: 0, needs_owner: false },
        Template { name: "wsol_sync".into(), program: pubkey(TOKEN_PROGRAM), prefix: vec![17], fields: vec![],
            accounts: vec![mine.clone()], extra_accounts: 0, needs_owner: false },
        Template { name: "wsol_unwrap".into(), program: pubkey(TOKEN_PROGRAM), prefix: vec![9], fields: vec![],
            accounts: vec![mine, Acc::Vault, Acc::Vault], extra_accounts: 0, needs_owner: false },
    ]
}

#[test]
fn sol_is_wrapped_only_into_the_vaults_own_wsol_account() {
    let (v, thief) = (vault(), key(66));
    let (wsol, usdc) = (pubkey(WSOL), key(USDC));
    let mine = ata_of(&v, &wsol);
    let transfer = |to: [u8; 32], lamports: u64| (vec![v, to], { let mut d = vec![2, 0, 0, 0]; d.extend(lamports.to_le_bytes()); d });
    let ts = wsol_templates();
    let (accs, data) = transfer(mine, 500_000_000);
    // (3 instructions: without a unit limit the runtime's 600k default is over the policy's 400k.)
    let limit = [vec![2], 300_000u32.to_le_bytes().to_vec()].concat();
    let ok = Tx::new(v).ix(pubkey(solguard::COMPUTE_BUDGET), vec![], limit).ix(pubkey(SYSTEM), accs, data).ix(pubkey(TOKEN_PROGRAM), vec![mine], vec![17]).ix(pubkey(TOKEN_PROGRAM), vec![mine, v, v], vec![9]).bytes();
    let m = check_tx(&ok, &ts, &policy()).expect("wrap, sync, unwrap the vault's own wSOL");
    assert_eq!(m.iter().map(|m| m.template.as_str()).collect::<Vec<_>>(), ["wsol_wrap", "wsol_sync", "wsol_unwrap"]);

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("SOL into the thief's wSOL account", { let (a, d) = transfer(ata_of(&thief, &wsol), 1); Tx::new(v).ix(pubkey(SYSTEM), a, d).bytes() }),
        ("SOL into the vault's account for another mint", { let (a, d) = transfer(ata_of(&v, &usdc), 1); Tx::new(v).ix(pubkey(SYSTEM), a, d).bytes() }),
        ("SOL straight to the thief", { let (a, d) = transfer(thief, 1); Tx::new(v).ix(pubkey(SYSTEM), a, d).bytes() }),
        ("over 1 SOL", { let (a, d) = transfer(mine, 1_000_000_001); Tx::new(v).ix(pubkey(SYSTEM), a, d).bytes() }),
        ("unwrapped to the thief", Tx::new(v).ix(pubkey(TOKEN_PROGRAM), vec![mine, thief, v], vec![9]).bytes()),
        ("closing the vault's USDC account", Tx::new(v).ix(pubkey(TOKEN_PROGRAM), vec![ata_of(&v, &usdc), v, v], vec![9]).bytes()),
    ];
    for (name, msg) in cases {
        assert!(check_tx(&msg, &ts, &policy()).is_err(), "{name} passed");
    }
}

#[test]
fn without_a_unit_limit_each_instruction_counts_200k_units() {
    // Wrap, sync, unwrap with no SetComputeUnitLimit: the runtime allows 3 x 200k = 600k units,
    // over the policy's 400k (the priority fee would be price x 600k).
    let v = vault();
    let mine = ata_of(&v, &pubkey(WSOL));
    let wrap = { let mut d = vec![2, 0, 0, 0]; d.extend(1_000u64.to_le_bytes()); d };
    let price = [vec![3], 1_000_000u64.to_le_bytes().to_vec()].concat();
    let txs = |limit: Option<u32>| {
        let mut t = Tx::new(v).ix(pubkey(solguard::COMPUTE_BUDGET), vec![], price.clone());
        if let Some(l) = limit {
            t = t.ix(pubkey(solguard::COMPUTE_BUDGET), vec![], [vec![2], l.to_le_bytes().to_vec()].concat());
        }
        t.ix(pubkey(SYSTEM), vec![v, mine], wrap.clone()).ix(pubkey(TOKEN_PROGRAM), vec![mine], vec![17]).ix(pubkey(TOKEN_PROGRAM), vec![mine, v, v], vec![9]).bytes()
    };
    let r = check_tx(&txs(None), &wsol_templates(), &policy());
    assert!(matches!(r, Err(Refusal::ComputeBudget(_))), "{r:?}");
    check_tx(&txs(Some(300_000)), &wsol_templates(), &policy()).expect("an explicit limit under the cap");
}

#[test]
fn only_token_accounts_the_templates_use_can_be_created() {
    // Each create locks ~0.002 SOL of the vault's in rent: a helper could add a dozen for random
    // mints to any template transaction. Only the vault's accounts the matched templates name pass.
    let v = vault();
    let (wsol, junk) = (pubkey(WSOL), key(99));
    let create = |mint: [u8; 32]| (pubkey(solguard::ATA_PROGRAM), vec![v, ata_of(&v, &mint), v, mint, pubkey(SYSTEM), pubkey(TOKEN_PROGRAM)], vec![1u8]);
    let wrap = |t: Tx| { let mut d = vec![2, 0, 0, 0]; d.extend(1_000u64.to_le_bytes()); t.ix(pubkey(SYSTEM), vec![v, ata_of(&v, &wsol)], d).bytes() };
    let (p, a, d) = create(wsol);
    check_tx(&wrap(Tx::new(v).ix(p, a, d)), &wsol_templates(), &policy()).expect("the wSOL account the wrap pays into");
    let (p, a, d) = create(junk);
    let r = check_tx(&wrap(Tx::new(v).ix(p, a, d)), &wsol_templates(), &policy());
    assert!(matches!(r, Err(Refusal::AtaCreate(_))), "an account for a mint no template uses: {r:?}");
    let mut five = Tx::new(v);
    for _ in 0..5 {
        let (p, a, d) = create(wsol);
        five = five.ix(p, a, d);
    }
    assert!(matches!(check_tx(&wrap(five), &wsol_templates(), &policy()), Err(Refusal::AtaCreate(_))), "five creates");
    // A pump buy may create the vault's account for the coin it buys (a VaultAta position).
    let mint = key(40);
    let (p, a, d) = create(mint);
    let buy = Tx::new(v).ix(p, a, d).ix(pubkey(PUMP), pump_accounts(ata_of(&v, &mint), v), pump_data(1, 1)).bytes();
    check_tx(&buy, &[pump_buy()], &policy()).expect("the coin's account");
}

#[test]
fn no_durable_nonce_even_if_a_template_allows_it() {
    // advance_nonce_account first makes a signed transaction valid until someone sends it: a keeper
    // could hold a swap for hours and land it once its floor is stale. Refused even when the owner
    // wrote a template for it; with a recent blockhash the transaction dies in ~1 minute.
    let v = vault();
    let mine = ata_of(&v, &pubkey(WSOL));
    let nonce_acc = key(55);
    let advance = Template { name: "advance".into(), program: pubkey(SYSTEM), prefix: vec![4, 0, 0, 0], fields: vec![],
        accounts: vec![Acc::Any, Acc::Any, Acc::Vault], extra_accounts: 0, needs_owner: false };
    let mut ts = wsol_templates();
    ts.push(advance);
    let wrap = { let mut d = vec![2, 0, 0, 0]; d.extend(1_000u64.to_le_bytes()); d };
    let held = Tx::new(v).ix(pubkey(SYSTEM), vec![nonce_acc, pubkey("SysvarRecentB1ockHashes11111111111111111111"), v], vec![4, 0, 0, 0])
        .ix(pubkey(SYSTEM), vec![v, mine], wrap.clone()).bytes();
    match check_tx(&held, &ts, &policy()) {
        Err(Refusal::Program(why)) => assert!(why.contains("durable nonce"), "{why}"),
        r => panic!("a durable nonce passed: {r:?}"),
    }
    check_tx(&Tx::new(v).ix(pubkey(SYSTEM), vec![v, mine], wrap).bytes(), &ts, &policy()).expect("the same wrap on a recent blockhash");
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

const ROUTER: &str = "hyRouTRDAgn65xyyJ3L5c4k5SFmSdr3NxDV8Euzjy3f";

/// Hylo's router route_v2 for one pair (nodes/sol-templates.json, hylo_*), shrunk to the accounts
/// that matter: [router config, user, user's input ATA, user's output ATA, input mint, output
/// mint, token program]. Data: discriminator, token_a, token_b, amount, Some(slippage config):
/// expected out {bits, exp -6}, tolerance {bits, exp -4}.
fn hylo(name: &str, a: [u8; 32], b: [u8; 32]) -> Template {
    Template {
        name: name.into(),
        program: pubkey(ROUTER),
        prefix: discriminator("route_v2").to_vec(),
        fields: vec![
            Field { ty: Ty::Bytes(32), rule: FieldRule::Fixed(a.to_vec()) },
            Field { ty: Ty::Bytes(32), rule: FieldRule::Fixed(b.to_vec()) },
            Field { ty: Ty::U64, rule: FieldRule::AtMost(1_000_000_000) },
            Field { ty: Ty::U8, rule: FieldRule::Fixed(vec![1]) },
            Field { ty: Ty::U64, rule: FieldRule::AtLeast { of: 2, num: 2_000, den: 1_000 } },
            Field { ty: Ty::U8, rule: FieldRule::Fixed(vec![0xfa]) },
            Field { ty: Ty::U64, rule: FieldRule::AtMost(100) },
            Field { ty: Ty::U8, rule: FieldRule::Fixed(vec![0xfc]) },
        ],
        accounts: vec![
            Acc::Fixed(key(90)),
            Acc::Vault,
            Acc::VaultAta { mint_at: 4, program_at: 6 },
            Acc::VaultAta { mint_at: 5, program_at: 6 },
            Acc::Fixed(a),
            Acc::Fixed(b),
            Acc::Fixed(pubkey(TOKEN_PROGRAM)),
        ],
        extra_accounts: 0,
        needs_owner: false,
    }
}

fn route_data(a: [u8; 32], b: [u8; 32], amount: u64, expected: Option<u64>, tolerance: u64) -> Vec<u8> {
    let mut d = discriminator("route_v2").to_vec();
    d.extend(a);
    d.extend(b);
    d.extend(amount.to_le_bytes());
    match expected {
        Some(e) => {
            d.push(1);
            d.extend(e.to_le_bytes());
            d.push(0xfa);
            d.extend(tolerance.to_le_bytes());
            d.push(0xfc);
        }
        None => d.push(0),
    }
    d
}

#[test]
fn a_hylo_mint_passes_only_with_a_slippage_floor_and_proceeds_to_the_vault() {
    let (v, jito, xsol, hyusd, thief) = (vault(), key(41), key(42), key(43), key(66));
    let ts = [hylo("hylo_mint_xsol", jito, xsol), hylo("hylo_mint_hyusd", jito, hyusd)];
    for t in &ts {
        assert!(t.validate().is_ok(), "{}", t.name);
    }
    let accounts = |out_owner: [u8; 32]| vec![key(90), v, ata_of(&v, &jito), ata_of(&out_owner, &xsol), jito, xsol, pubkey(TOKEN_PROGRAM)];
    let tx = |acc: Vec<[u8; 32]>, data: Vec<u8>| Tx::new(v).ix(pubkey(ROUTER), acc, data).bytes();
    // 0.5 jitoSOL for at least 2 xSOL units per jitoSOL unit (2,000 xSOL per jitoSOL), 0.5% slippage.
    let m = check_tx(&tx(accounts(v), route_data(jito, xsol, 500_000_000, Some(1_400_000_000), 50)), &ts, &policy()).expect("honest mint");
    assert_eq!((m[0].template.as_str(), m[0].values[2], m[0].values[4]), ("hylo_mint_xsol", 500_000_000, 1_400_000_000));

    // Every refusal names the template the instruction was meant for (hylo_mint_xsol: its pair
    // matched), not hylo_mint_hyusd, which shares the router's discriminator.
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("xSOL to the thief's token account", tx(accounts(thief), route_data(jito, xsol, 500_000_000, Some(1_400_000_000), 50)), "hylo_mint_xsol: account 3"),
        ("over the 1 jitoSOL cap", tx(accounts(v), route_data(jito, xsol, 1_500_000_000, Some(4_000_000_000), 50)), "hylo_mint_xsol: field 2"),
        ("an expected out under the floor", tx(accounts(v), route_data(jito, xsol, 500_000_000, Some(1), 50)), "hylo_mint_xsol: field 4"),
        ("a 5% tolerance", tx(accounts(v), route_data(jito, xsol, 500_000_000, Some(1_400_000_000), 500)), "hylo_mint_xsol: field 6"),
        ("no slippage config at all", tx(accounts(v), route_data(jito, xsol, 500_000_000, None, 0)), "data is"),
        ("a pair the owner never approved", tx(accounts(v), route_data(xsol, jito, 500_000_000, Some(1_400_000_000), 50)), "field 0: not the fixed value"),
    ];
    for (name, msg, why) in cases {
        match check_tx(&msg, &ts, &policy()) {
            Err(Refusal::Program(r)) => assert!(r.contains(why), "{name}: {r}"),
            r => panic!("{name}: {r:?}"),
        }
    }
}
