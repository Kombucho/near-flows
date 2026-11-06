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
//!
//! Two kinds of vault:
//! - named (`create_vault`): owned by the caller's account, and the factory
//!   remembers owner -> vault so the app can find it.
//! - stealth (`create_stealth_vault`): owned by a public key (vault stealth.rs).
//!   The factory records nothing about who asked: no owner -> vault entry, no
//!   caller in the event. Anyone may create one for a key and pay from the
//!   attached deposit, typically a relayer, so the owner's wallet never appears.
//!   (The relayer itself sees both sides, and how the owner pays the relayer
//!   can link them: that payment should not come from the owner's wallet.)

use near_sdk::store::{LookupMap, Vector};
use near_sdk::{env, near, require, AccountId, CryptoHash, CurveType, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PublicKey};

/// What a new vault must be funded with: its state (~0.003 NEAR), its 0.05 NEAR reserve, headroom.
pub const MIN_VAULT_DEPOSIT: NearToken = NearToken::from_millinear(100);
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
    /// Every vault ever created, in order (so keepers and the app can find them).
    pub list: Vector<AccountId>,
}

#[near]
impl Factory {
    #[init]
    pub fn new(owner: AccountId) -> Self {
        Self { owner, code_hash: None, vaults: LookupMap::new(b"v"), list: Vector::new(b"l") }
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
        require!(deposit >= MIN_VAULT_DEPOSIT, "attach at least 0.1 NEAR");
        require!(!self.vaults.contains_key(&owner), "you already have a vault");
        let vault = self.vault_id(&name);
        self.vaults.insert(owner.clone(), vault.clone());
        let init = near_sdk::serde_json::json!({"owner": owner, "keeper": keeper}).to_string().into_bytes();
        self.deploy(&vault, deposit, "new", init)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_created(owner, vault, deposit))
    }

    /// Create a stealth vault `<name>.<factory>` owned by `owner_key` (`ed25519:<base58>`),
    /// funded with the attached NEAR. The caller (anyone, e.g. a relayer) is not recorded;
    /// if the name is taken, the deposit goes back to the caller.
    /// Pick a name that says nothing about the owner (e.g. random).
    #[payable]
    pub fn create_stealth_vault(&mut self, name: String, owner_key: PublicKey, keeper: Option<AccountId>) -> Promise {
        let deposit = env::attached_deposit();
        require!(deposit >= MIN_VAULT_DEPOSIT, "attach at least 0.1 NEAR");
        require!(owner_key.curve_type() == CurveType::ED25519, "the owner key must be ed25519");
        let vault = self.vault_id(&name);
        let init = near_sdk::serde_json::json!({"owner_key": owner_key, "keeper": keeper}).to_string().into_bytes();
        self.deploy(&vault, deposit, "new_stealth", init)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_created_stealth(env::predecessor_account_id(), vault, deposit))
    }

    /// Like `on_created`, but the payer is neither kept nor logged: it's only here to refund.
    #[private]
    pub fn on_created_stealth(&mut self, payer: AccountId, vault: AccountId, deposit: NearToken, #[callback_result] r: Result<(), PromiseError>) -> bool {
        if r.is_ok() {
            self.list.push(vault.clone());
            env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"vault_created","data":[{{"stealth":true,"vault":"{vault}"}}]}}"#));
            return true;
        }
        Promise::new(payer).transfer(deposit).detach();
        false
    }

    /// If the vault couldn't be made (e.g. the name is taken), forget it and
    /// refund. The failed creation already returned the deposit to us.
    #[private]
    pub fn on_created(&mut self, owner: AccountId, vault: AccountId, deposit: NearToken, #[callback_result] r: Result<(), PromiseError>) -> bool {
        if r.is_ok() {
            self.list.push(vault.clone());
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

    /// Vaults `from_index`.. (at most 100 per page), oldest first.
    pub fn get_vaults(&self, from_index: u32, limit: u32) -> Vec<AccountId> {
        (from_index..from_index.saturating_add(limit.min(100)).min(self.list.len())).filter_map(|i| self.list.get(i).cloned()).collect()
    }

    pub fn get_vault_count(&self) -> u32 {
        self.list.len()
    }

    pub fn get_code_hash(&self) -> Option<String> {
        self.code_hash.map(|h| near_sdk::bs58::encode(h).into_string())
    }
}

impl Factory {
    fn vault_id(&self, name: &str) -> AccountId {
        require!(
            (2..=32).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_'),
            "name: 2-32 of a-z 0-9 - _"
        );
        format!("{name}.{}", env::current_account_id()).parse().unwrap_or_else(|_| env::panic_str("bad name"))
    }

    /// A keyless account running the published vault code, initialised by `init_method`.
    fn deploy(&self, vault: &AccountId, deposit: NearToken, init_method: &str, init: Vec<u8>) -> Promise {
        let hash = self.code_hash.unwrap_or_else(|| env::panic_str("no vault code published yet"));
        Promise::new(vault.clone())
            .create_account()
            .transfer(deposit)
            .use_global_contract(hash)
            .function_call(init_method.to_string(), init, NearToken::from_yoctonear(0), GAS_INIT)
    }
}
