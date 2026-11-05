//! `Map`: the engine's key-value collection.
//!
//! Natively (tests, simulator) it is a plain BTreeMap. Inside the NEAR
//! contract (feature "near") it is a near-sdk LookupMap: every entry is its
//! own storage slot, read only when a call touches it. That is what keeps a
//! call's gas flat as the market grows: a buy reads the few bins it crosses,
//! not every position ever opened.
//!
//! The price of laziness: on chain a Map cannot be iterated. Anything that
//! needs "all of them" keeps its own small index (bin ids, ids by owner) or
//! walks a range of sequential ids.

use std::ops::Index;

#[cfg(not(feature = "near"))]
use std::collections::BTreeMap;

#[cfg_attr(not(feature = "near"), derive(Clone, Debug, PartialEq, Eq))]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct Map<K: Key, V: Val> {
    #[cfg(feature = "near")]
    inner: near_sdk::store::LookupMap<K, V>,
    #[cfg(not(feature = "near"))]
    inner: BTreeMap<K, V>,
    len: u64,
}

#[cfg(feature = "near")]
mod backend {
    pub trait Key: borsh::BorshSerialize + borsh::BorshDeserialize + Ord + Clone {}
    impl<T: borsh::BorshSerialize + borsh::BorshDeserialize + Ord + Clone> Key for T {}
    pub trait Val: borsh::BorshSerialize + borsh::BorshDeserialize {}
    impl<T: borsh::BorshSerialize + borsh::BorshDeserialize> Val for T {}
}

#[cfg(not(feature = "near"))]
mod backend {
    pub trait Key: Ord + Clone {}
    impl<T: Ord + Clone> Key for T {}
    pub trait Val {}
    impl<T> Val for T {}
}

pub use backend::{Key, Val};

impl<K: Key, V: Val> Map<K, V> {
    /// `prefix` must be unique per map in the contract (it is ignored natively).
    pub fn new(prefix: &[u8]) -> Self {
        #[cfg(feature = "near")]
        let inner = near_sdk::store::LookupMap::new(prefix.to_vec());
        #[cfg(not(feature = "near"))]
        let inner = { let _ = prefix; BTreeMap::new() };
        Map { inner, len: 0 }
    }

    pub fn get(&self, k: &K) -> Option<&V> {
        self.inner.get(k)
    }

    pub fn get_mut(&mut self, k: &K) -> Option<&mut V> {
        self.inner.get_mut(k)
    }

    pub fn contains_key(&self, k: &K) -> bool {
        self.inner.contains_key(k)
    }

    pub fn insert(&mut self, k: K, v: V) -> Option<V> {
        let old = self.inner.insert(k, v);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    pub fn remove(&mut self, k: &K) -> Option<V> {
        let old = self.inner.remove(k);
        if old.is_some() {
            self.len -= 1;
        }
        old
    }

    /// The entry at `k`, created with `make()` if missing.
    pub fn get_or_insert_with(&mut self, k: K, make: impl FnOnce() -> V) -> &mut V {
        if !self.inner.contains_key(&k) {
            self.inner.insert(k.clone(), make());
            self.len += 1;
        }
        self.inner.get_mut(&k).expect("just inserted")
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Write changed entries to storage now (on chain; a no-op natively).
    /// The contract calls it before measuring storage use.
    pub fn flush(&mut self) {
        #[cfg(feature = "near")]
        self.inner.flush();
    }
}

impl<K: Key, V: Val + Default> Map<K, V> {
    pub fn get_or_default(&mut self, k: K) -> &mut V {
        self.get_or_insert_with(k, V::default)
    }
}

/// Native-only: whole-map iteration, for tests, the simulator and audits.
#[cfg(not(feature = "near"))]
impl<K: Key, V> Map<K, V> {
    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, K, V> {
        self.inner.iter()
    }
    pub fn values(&self) -> std::collections::btree_map::Values<'_, K, V> {
        self.inner.values()
    }
    pub fn keys(&self) -> std::collections::btree_map::Keys<'_, K, V> {
        self.inner.keys()
    }
}

impl<K: Key, V: Val> Index<&K> for Map<K, V> {
    type Output = V;
    fn index(&self, k: &K) -> &V {
        self.get(k).expect("key exists")
    }
}
