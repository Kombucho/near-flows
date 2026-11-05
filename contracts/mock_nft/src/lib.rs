//! TESTNET MOCK NFT: a plain NEP-171 collection where anyone can mint.
//! Used by the sandbox tests of the bazaar. Not a real asset.

use near_contract_standards::non_fungible_token::metadata::TokenMetadata;
use near_contract_standards::non_fungible_token::{NonFungibleToken, Token, TokenId};
use near_sdk::{env, near, AccountId, BorshStorageKey, PanicOnDefault, Promise, PromiseOrValue};

#[near]
#[derive(BorshStorageKey)]
enum Key {
    Owners,
    Metadata,
    Enumeration,
    Approvals,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockNft {
    tokens: NonFungibleToken,
}

#[near]
impl MockNft {
    #[init]
    pub fn new() -> Self {
        let owner = env::current_account_id();
        MockNft {
            tokens: NonFungibleToken::new(Key::Owners, owner, Some(Key::Metadata), Some(Key::Enumeration), Some(Key::Approvals)),
        }
    }

    /// Anyone can mint (it's a mock). Attach ~0.01 NEAR for storage.
    #[payable]
    pub fn mint(&mut self, token_id: TokenId, title: String) -> Token {
        let to = env::predecessor_account_id();
        let meta = TokenMetadata { title: Some(title), ..Default::default() };
        self.tokens.internal_mint(token_id, to, Some(meta))
    }
}

near_contract_standards::impl_non_fungible_token_core!(MockNft, tokens);
near_contract_standards::impl_non_fungible_token_enumeration!(MockNft, tokens);
near_contract_standards::impl_non_fungible_token_approval!(MockNft, tokens);

#[allow(dead_code)]
fn _unused(_: PromiseOrValue<bool>, _: AccountId, _: Option<Promise>) {}
