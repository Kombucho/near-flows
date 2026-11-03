//! TESTNET MOCK TOKEN. Not a real asset, no value.
//!
//! A standard NEAR fungible token (NEP-141 transfers, NEP-145 storage,
//! NEP-148 metadata) built from `near-contract-standards`, plus two extras:
//!   - `ft_burn`: holders can destroy their own tokens (supply goes down),
//!     so a native coin's buyback can really burn.
//!   - `mint_for_test`: a capped faucet so demo accounts can get tokens.
//!   - optional `call_fee_bps`: mimics launchpad tokens (e.g. justhoot v2) that
//!     keep a fee on every `ft_transfer_call`, so the receiver gets the net
//!     amount. Plain `ft_transfer` stays free, as on mainnet.
//!
//! Most methods just forward to the library's `FungibleToken`, which does the
//! real work; this file is mostly the wiring NEAR needs to expose them.

use near_contract_standards::fungible_token::events::{FtBurn, FtMint};
use near_contract_standards::fungible_token::metadata::{FungibleTokenMetadata, FungibleTokenMetadataProvider, FT_METADATA_SPEC};
use near_contract_standards::fungible_token::{FungibleToken, FungibleTokenCore, FungibleTokenResolver};
use near_contract_standards::storage_management::{StorageBalance, StorageBalanceBounds, StorageManagement};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault, PromiseOrValue};

/// Most one faucet call may mint (in the token's smallest units).
const FAUCET_CAP: u128 = 1_000_000_000_000_000_000_000_000; // 1e24

#[near]
#[derive(BorshStorageKey)]
enum Key {
    Token,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockFt {
    token: FungibleToken,
    name: String,
    symbol: String,
    decimals: u8,
    call_fee_bps: u16,
}

#[near]
impl MockFt {
    /// Mints the whole `total_supply` to `owner_id`.
    #[init]
    pub fn new(
        owner_id: AccountId, total_supply: U128, name: String, symbol: String, decimals: u8, call_fee_bps: Option<u16>,
    ) -> Self {
        let call_fee_bps = call_fee_bps.unwrap_or(0);
        require!(call_fee_bps < 10_000, "fee too high");
        let mut token = FungibleToken::new(Key::Token);
        token.internal_register_account(&owner_id);
        token.internal_register_account(&env::current_account_id()); // collects call fees
        token.internal_deposit(&owner_id, total_supply.0);
        FtMint { owner_id: &owner_id, amount: total_supply, memo: Some("initial supply (mock)") }.emit();
        MockFt { token, name, symbol, decimals, call_fee_bps }
    }

    /// Destroy `amount` of your own tokens. Total supply goes down.
    pub fn ft_burn(&mut self, amount: U128) {
        let who = env::predecessor_account_id();
        self.token.internal_withdraw(&who, amount.0); // panics if balance is too low
        FtBurn { owner_id: &who, amount, memo: None }.emit();
    }

    /// Testnet faucet: mint up to the cap to yourself (you must be registered).
    pub fn mint_for_test(&mut self, amount: U128) {
        require!(amount.0 <= FAUCET_CAP, "over the faucet cap");
        let who = env::predecessor_account_id();
        self.token.internal_deposit(&who, amount.0);
        FtMint { owner_id: &who, amount, memo: Some("faucet (mock)") }.emit();
    }
}

#[near]
impl FungibleTokenCore for MockFt {
    #[payable]
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>) {
        self.token.ft_transfer(receiver_id, amount, memo)
    }

    #[payable]
    fn ft_transfer_call(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>, msg: String) -> PromiseOrValue<U128> {
        let fee = amount.0 * self.call_fee_bps as u128 / 10_000;
        if fee > 0 {
            // Keep the fee first; the receiver is called with the NET amount.
            let sender = env::predecessor_account_id();
            self.token.internal_transfer(&sender, &env::current_account_id(), fee, Some("call fee (mock)".into()));
        }
        self.token.ft_transfer_call(receiver_id, U128(amount.0 - fee), memo, msg)
    }

    fn ft_total_supply(&self) -> U128 {
        self.token.ft_total_supply()
    }

    fn ft_balance_of(&self, account_id: AccountId) -> U128 {
        self.token.ft_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenResolver for MockFt {
    /// After ft_transfer_call: refunds whatever the receiver returned as unused.
    #[private]
    fn ft_resolve_transfer(&mut self, sender_id: AccountId, receiver_id: AccountId, amount: U128) -> U128 {
        let (used, _burned) = self.token.internal_ft_resolve_transfer(&sender_id, receiver_id, amount);
        used.into()
    }
}

#[near]
impl StorageManagement for MockFt {
    #[payable]
    fn storage_deposit(&mut self, account_id: Option<AccountId>, registration_only: Option<bool>) -> StorageBalance {
        self.token.storage_deposit(account_id, registration_only)
    }

    #[payable]
    fn storage_withdraw(&mut self, amount: Option<NearToken>) -> StorageBalance {
        self.token.storage_withdraw(amount)
    }

    #[payable]
    fn storage_unregister(&mut self, force: Option<bool>) -> bool {
        self.token.internal_storage_unregister(force).is_some()
    }

    fn storage_balance_bounds(&self) -> StorageBalanceBounds {
        self.token.storage_balance_bounds()
    }

    fn storage_balance_of(&self, account_id: AccountId) -> Option<StorageBalance> {
        self.token.storage_balance_of(account_id)
    }
}

#[near]
impl FungibleTokenMetadataProvider for MockFt {
    fn ft_metadata(&self) -> FungibleTokenMetadata {
        FungibleTokenMetadata {
            spec: FT_METADATA_SPEC.to_string(),
            name: self.name.clone(),
            symbol: self.symbol.clone(),
            icon: None,
            reference: None,
            reference_hash: None,
            decimals: self.decimals,
        }
    }
}
