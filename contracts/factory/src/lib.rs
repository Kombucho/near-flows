//! The vault factory: one call, one vault of your own.
//!
//! The vault code is published ONCE as a global contract, identified by its
//! hash. Each new vault is a sub-account (`<name>.<factory>`) that points at
//! that hash, so a vault costs a few hundred bytes of state instead of a
//! 260 KB copy of the code.
//!
//! A vault gets NO access keys. Nobody can redeploy it, not its owner and not
//! us: its code is fixed to the hash it was born with. Publishing a new
//! version only changes what NEW vaults use.

use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, CryptoHash, Gas, NearToken, PanicOnDefault, Promise, PromiseError};

/// What a new vault must be funded with: its state, its 0.2 NEAR reserve, headroom.
pub const MIN_VAULT_DEPOSIT: NearToken = NearToken::from_millinear(500);
const GAS_INIT: Gas = Gas::from_tgas(20);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Factory {
    pub owner: AccountId,
    /// sha256 of the vault code new vaults use.
    pub code_hash: Option<CryptoHash>,
    /// owner -> their vault
    pub vaults: LookupMap<AccountId, AccountId>,
}

#[near]
impl Factory {
    #[init]
    pub fn new(owner: AccountId) -> Self {
        Self { owner, code_hash: None, vaults: LookupMap::new(b"v") }
    }

    /// Publish vault code (the raw wasm is the call's input, not JSON).
    /// Pays the global contract's storage from the attached deposit.
    #[payable]
    pub fn publish(&mut self) -> Promise {
        require!(env::predecessor_account_id() == self.owner, "owner only");
        let code = env::input().unwrap_or_else(|| env::panic_str("no code"));
        require!(code.len() > 1_000, "that isn't a contract");
        let hash: CryptoHash = env::sha256_array(&code);
        self.code_hash = Some(hash);
        env::log_str(&format!("vault code published: {}", near_sdk::bs58::encode(hash).into_string()));
        Promise::new(env::current_account_id()).deploy_global_contract(code)
    }

    /// Create your vault `<name>.<factory>`, funded with the attached NEAR.
    /// `keeper` may run your flows' fired triggers; it can't take anything.
    #[payable]
    pub fn create_vault(&mut self, name: String, keeper: Option<AccountId>) -> Promise {
        let owner = env::predecessor_account_id();
        let deposit = env::attached_deposit();
        require!(deposit >= MIN_VAULT_DEPOSIT, "attach at least 0.5 NEAR");
        require!(!self.vaults.contains_key(&owner), "you already have a vault");
        require!(
            (2..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
            "name: 2-32 of a-z 0-9 - _"
        );
        let hash = self.code_hash.unwrap_or_else(|| env::panic_str("no vault code published yet"));
        let vault: AccountId = format!("{name}.{}", env::current_account_id()).parse().unwrap_or_else(|_| env::panic_str("bad name"));
        self.vaults.insert(owner.clone(), vault.clone());
        let init = near_sdk::serde_json::json!({"owner": owner, "keeper": keeper}).to_string().into_bytes();
        Promise::new(vault.clone())
            .create_account()
            .transfer(deposit)
            .use_global_contract(hash)
            .function_call("new".to_string(), init, NearToken::from_yoctonear(0), GAS_INIT)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_created(owner, vault, deposit))
    }

    /// If the vault couldn't be made (e.g. the name is taken), forget it and
    /// refund. The failed creation already returned the deposit to us.
    #[private]
    pub fn on_created(&mut self, owner: AccountId, vault: AccountId, deposit: NearToken, #[callback_result] r: Result<(), PromiseError>) -> bool {
        if r.is_ok() {
            env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"vault_created","data":[{{"owner":"{owner}","vault":"{vault}"}}]}}"#));
            return true;
        }
        self.vaults.remove(&owner);
        Promise::new(owner).transfer(deposit).detach();
        false
    }

    pub fn get_vault(&self, owner: AccountId) -> Option<AccountId> {
        self.vaults.get(&owner).cloned()
    }

    pub fn get_code_hash(&self) -> Option<String> {
        self.code_hash.map(|h| near_sdk::bs58::encode(h).into_string())
    }
}
