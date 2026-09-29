//! The tree's memos, shared by every thread that asks the tree (DEC-250).
//!
//! Every memo is a function of the tree and its key alone (DEC-200), so two
//! threads that miss one key compute equal values, and whichever is stored
//! first is the answer. No lock is held while a value is computed: the memos
//! recurse — a lookup linearizes, a linearization resolves a path through
//! ancestors — and a lock held across that is the lazy-init deadlock. The API
//! hands out clones, never a guard, so a caller cannot hold one by accident.

use hashbrown::hash_table::{Entry, HashTable};
use std::borrow::Borrow;
use std::hash::{BuildHasher, Hash, RandomState};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

/// Enough that eight threads rarely meet on one lock.
const SHARDS: usize = 32;

type Shard<K, V> = RwLock<HashTable<(K, V)>>;

/// A map filled by whoever misses first: `get`, compute outside any lock,
/// `publish`. A key is hashed once: the shard is picked from bits of the
/// hash the table does not use for its own probe.
pub(super) struct Memo<K, V> {
    hasher: RandomState,
    shards: Box<[Shard<K, V>]>,
}

impl<K: Hash + Eq, V: Clone> Memo<K, V> {
    pub(super) fn new() -> Self {
        Memo {
            hasher: RandomState::new(),
            shards: (0..SHARDS).map(|_| RwLock::default()).collect(),
        }
    }

    fn shard(&self, hash: u64) -> &Shard<K, V> {
        &self.shards[(hash >> 40) as usize % SHARDS]
    }

    pub(super) fn get<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let hash = self.hasher.hash_one(key);
        let shard = self.shard(hash).read();
        shard
            .unwrap_or_else(PoisonError::into_inner)
            .find(hash, |(k, _)| k.borrow() == key)
            .map(|(_, v)| v.clone())
    }

    /// Store `value` unless another thread stored one first, and answer with
    /// whichever is stored — equal, when the memo is a function of its key.
    pub(super) fn publish(&self, key: K, value: V) -> V {
        let hash = self.hasher.hash_one(&key);
        let mut shard = self
            .shard(hash)
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        let hasher = |(k, _): &(K, V)| self.hasher.hash_one(k);
        match shard.entry(hash, |(k, _)| *k == key, hasher) {
            Entry::Occupied(held) => held.get().1.clone(),
            Entry::Vacant(free) => free.insert((key, value)).get().1.clone(),
        }
    }

    /// Every value, for a caller that counts them (the linearization dump).
    #[cfg(test)]
    pub(super) fn values(&self) -> Vec<V> {
        self.shards
            .iter()
            .flat_map(|shard| {
                let shard = shard.read().unwrap_or_else(PoisonError::into_inner);
                shard.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>()
            })
            .collect()
    }

    /// Every key, for a caller that lists what has been loaded.
    pub(super) fn keys(&self) -> Vec<K>
    where
        K: Clone,
    {
        self.shards
            .iter()
            .flat_map(|shard| {
                let shard = shard.read().unwrap_or_else(PoisonError::into_inner);
                shard.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>()
            })
            .collect()
    }
}

/// A value per key computed once, the threads that ask meanwhile waiting for
/// it: for work that is costly to repeat (a name's rows from the store).
/// `init` must not ask for its own key, on any thread it waits on.
pub(super) struct Once<K, V> {
    cells: Memo<K, Arc<OnceLock<V>>>,
}

impl<K: Hash + Eq, V: Clone> Once<K, V> {
    pub(super) fn new() -> Self {
        Once { cells: Memo::new() }
    }

    /// The value, when it has been computed.
    pub(super) fn peek<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.cells.get(key)?.get().cloned()
    }

    pub(super) fn get_or_init<Q>(&self, key: &Q, init: impl FnOnce() -> V) -> V
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ToOwned<Owned = K> + ?Sized,
    {
        let cell = match self.cells.get(key) {
            Some(cell) => cell,
            None => self.cells.publish(key.to_owned(), Arc::default()),
        };
        cell.get_or_init(init).clone()
    }

    /// Set a key's value before anyone asks for it.
    pub(super) fn set(&self, key: K, value: V) {
        self.cells.publish(key, Arc::new(OnceLock::from(value)));
    }

    /// Every key whose value has been computed.
    pub(super) fn done(&self) -> Vec<K>
    where
        K: Clone,
    {
        self.cells
            .keys()
            .into_iter()
            .filter(|key| self.cells.get(key).is_some_and(|cell| cell.get().is_some()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_value_published_is_the_answer() {
        let memo: Memo<String, u32> = Memo::new();
        assert_eq!(memo.get("a"), None);
        assert_eq!(memo.publish("a".into(), 1), 1);
        assert_eq!(memo.publish("a".into(), 2), 1);
        assert_eq!(memo.get("a"), Some(1));
    }

    #[test]
    fn a_once_value_is_computed_once_however_many_threads_ask() {
        let once: Once<String, u32> = Once::new();
        let runs = std::sync::atomic::AtomicU32::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    once.get_or_init("k", || {
                        runs.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        7
                    })
                });
            }
        });
        assert_eq!(runs.into_inner(), 1);
        assert_eq!(once.peek("k"), Some(7));
    }
}
