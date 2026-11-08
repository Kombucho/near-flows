//! The relay vault factory: one call, one vault of your own, the split way.
//!
//! Same as factory/src/lib.rs (kept separate so both designs can be compared),
//! except the published code is the relay (the vault BODY, ~9 KB, ~1 NEAR to
//! publish) and every vault is born obeying one fixed brain: the relay's `new`
//! gets the brain's account plus the init request, and the brain accepts that
//! request only when this factory sent it.
//!
//! A vault gets NO access keys, and the relay has no code path that deploys:
//! nobody can ever change its code or its brain.
//!
//! Two kinds, as in factory/src/lib.rs: named (`create_vault`, owner -> vault
//! remembered) and stealth (`create_stealth_vault`, owned by a key; the caller,
//! typically a relayer, is neither kept nor logged).
//!
//! Closed names are tombstoned forever. A vault's addresses on other chains are
//! derived from its account name (Chain Signatures path + predecessor), so a new
//! vault under a closed name would control whatever was stranded at the old
//! one's addresses. The brain tells this factory when it closes a vault
//! (`on_vault_closed`, accepted only from the brain); after that the name can
//! never be created here again, and only this factory can create it at all.
//! The brain deletes a vault only once this tombstone succeeded, and every vault
//! leaves VAULT_SHARE here to pay for it.
//!
//! A vault the brain refuses at birth (no record: it could never run) deletes
//! itself, its NEAR back to the payer, and the create call returns false.

use near_sdk::store::{LookupMap, LookupSet, Vector};
use near_sdk::{env, near, require, AccountId, CryptoHash, CurveType, Gas, NearToken, PanicOnDefault, Promise, PublicKey};

/// A new vault's funding: its own few bytes, the 0.05 NEAR reserve, its record in the brain, headroom.
pub const MIN_VAULT_DEPOSIT: NearToken = NearToken::from_millinear(100);
/// Kept here out of every deposit, for good: what this vault costs the factory's
/// storage (its list entry, its owner's map entry, one day its tombstone: ~400
/// bytes at most). So no stream of vaults made and closed can use up the
/// factory's own NEAR and leave a closed name without its tombstone.
pub const VAULT_SHARE: NearToken = NearToken::from_millinear(5);
/// The relay's `new` asks the brain, which answers; that whole round runs on this.
const GAS_INIT: Gas = Gas::from_tgas(150);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Factory {
    pub owner: AccountId,
    /// The brain every vault made here obeys. Fixed.
    pub brain: AccountId,
    /// sha256 of the relay code new vaults use.
    pub code_hash: Option<CryptoHash>,
    pub vaults: LookupMap<AccountId, AccountId>,
    pub list: Vector<AccountId>,
    /// Names of closed vaults: never made again.
    pub closed: LookupSet<AccountId>,
}

#[near]
impl Factory {
    #[init]
    pub fn new(owner: AccountId, brain: AccountId) -> Self {
        Self { owner, brain, code_hash: None, vaults: LookupMap::new(b"v"), list: Vector::new(b"l"), closed: LookupSet::new(b"c") }
    }

    /// The brain closed `vault` (it deleted itself): tombstone the name; its owner
    /// may make a new vault under another name. Only the brain can say so.
    pub fn on_vault_closed(&mut self, vault: AccountId, owner: Option<AccountId>) {
        require!(env::predecessor_account_id() == self.brain, "only the brain closes vaults");
        self.closed.insert(vault.clone());
        if let Some(o) = owner {
            if self.vaults.get(&o) == Some(&vault) {
                self.vaults.remove(&o);
            }
        }
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"vault_closed","data":[{{"vault":"{vault}"}}]}}"#));
    }

    pub fn is_closed(&self, vault: AccountId) -> bool {
        self.closed.contains(&vault)
    }

    /// Publish relay code (the raw wasm is the call's input, not JSON).
    #[payable]
    pub fn publish(&mut self) -> Promise {
        require!(env::predecessor_account_id() == self.owner, "owner only");
        let code = env::input().unwrap_or_else(|| env::panic_str("no code"));
        require!(code.len() > 1_000, "that isn't a contract");
        let hash: CryptoHash = env::sha256_array(&code);
        self.code_hash = Some(hash);
        env::log_str(&format!("relay code published: {}", near_sdk::bs58::encode(hash).into_string()));
        Promise::new(env::current_account_id()).deploy_global_contract(code)
    }

    /// Create your vault `<name>.<factory>`, funded with the attached NEAR (less
    /// VAULT_SHARE). `expected_code_hash` (base58, optional): refuse unless the
    /// published relay is exactly that code (pin the relay you audited).
    #[payable]
    pub fn create_vault(&mut self, name: String, keeper: Option<AccountId>, expected_code_hash: Option<String>) -> Promise {
        let owner = env::predecessor_account_id();
        let deposit = env::attached_deposit();
        require!(deposit >= MIN_VAULT_DEPOSIT, "attach at least 0.1 NEAR");
        require!(!self.vaults.contains_key(&owner), "you already have a vault");
        let vault = self.vault_id(&name);
        self.vaults.insert(owner.clone(), vault.clone());
        self.deploy(&vault, deposit, &owner, expected_code_hash, near_sdk::serde_json::json!({"init": {"owner": owner, "keeper": keeper}}))
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_created(owner, vault, deposit))
    }

    /// Create a stealth vault `<name>.<factory>` owned by `owner_key` (`ed25519:<base58>`).
    /// The caller (anyone, e.g. a relayer) is not recorded; a taken name refunds it.
    #[payable]
    pub fn create_stealth_vault(&mut self, name: String, owner_key: PublicKey, keeper: Option<AccountId>, expected_code_hash: Option<String>) -> Promise {
        let deposit = env::attached_deposit();
        require!(deposit >= MIN_VAULT_DEPOSIT, "attach at least 0.1 NEAR");
        require!(owner_key.curve_type() == CurveType::ED25519, "the owner key must be ed25519");
        let vault = self.vault_id(&name);
        let payer = env::predecessor_account_id();
        self.deploy(&vault, deposit, &payer, expected_code_hash, near_sdk::serde_json::json!({"init_stealth": {"owner_key": owner_key, "keeper": keeper}}))
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_created_stealth(payer, vault, deposit))
    }

    /// Like `on_created`, but the payer is neither kept nor logged: it's only here to refund.
    #[private]
    pub fn on_created_stealth(&mut self, payer: AccountId, vault: AccountId, deposit: NearToken) -> bool {
        match created() {
            Some(true) => {
                self.list.push(vault.clone());
                env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"vault_created","data":[{{"stealth":true,"vault":"{vault}"}}]}}"#));
                true
            }
            refused => {
                refund(payer, deposit, refused);
                false
            }
        }
    }

    /// If the vault couldn't be made (e.g. the name is taken) or the brain refused
    /// it (the vault deleted itself, its NEAR back to the owner), forget it and
    /// refund what's still here.
    #[private]
    pub fn on_created(&mut self, owner: AccountId, vault: AccountId, deposit: NearToken) -> bool {
        match created() {
            Some(true) => {
                self.list.push(vault.clone());
                env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"vault_created","data":[{{"owner":"{owner}","vault":"{vault}"}}]}}"#));
                true
            }
            refused => {
                self.vaults.remove(&owner);
                refund(owner, deposit, refused);
                false
            }
        }
    }

    pub fn get_brain(&self) -> AccountId {
        self.brain.clone()
    }

    pub fn get_vault(&self, owner: AccountId) -> Option<AccountId> {
        self.vaults.get(&owner).cloned()
    }

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
        let id: AccountId = format!("{name}.{}", env::current_account_id()).parse().unwrap_or_else(|_| env::panic_str("bad name"));
        require!(!self.closed.contains(&id), "that name belonged to a closed vault: it is never reused");
        id
    }

    /// Make the keyless account on the published relay, funded with the deposit
    /// less VAULT_SHARE; its `new` gets the brain's account and the payer (each
    /// u8 length + bytes) followed by the init request the brain sees.
    fn deploy(&self, vault: &AccountId, deposit: NearToken, payer: &AccountId, expected: Option<String>, request: near_sdk::serde_json::Value) -> Promise {
        let hash = self.code_hash.unwrap_or_else(|| env::panic_str("no vault code published yet"));
        if let Some(e) = expected {
            require!(e == near_sdk::bs58::encode(hash).into_string(), "the published relay isn't the relay code you expected");
        }
        let mut init = vec![self.brain.len() as u8];
        init.extend(self.brain.as_bytes());
        init.push(payer.len() as u8);
        init.extend(payer.as_bytes());
        init.extend(request.to_string().into_bytes());
        Promise::new(vault.clone())
            .create_account()
            .transfer(deposit.saturating_sub(VAULT_SHARE))
            .use_global_contract(hash)
            .function_call("new".to_string(), init, NearToken::from_yoctonear(0), GAS_INIT)
    }
}

/// What the vault's creation came to, as its `new` returned it: Some(true) made;
/// Some(false) the brain refused it (the vault deleted itself, its NEAR back to
/// the payer); None never made (e.g. the name is taken: the NEAR came back here).
fn created() -> Option<bool> {
    env::promise_result_checked(0, 16).ok().map(|v| v != b"false")
}

/// Give the payer back what's here: the whole deposit if no vault was made, the
/// share kept from it if the vault already sent the rest back.
fn refund(payer: AccountId, deposit: NearToken, refused: Option<bool>) {
    let back = if refused.is_some() { VAULT_SHARE } else { deposit };
    Promise::new(payer).transfer(back).detach();
}
