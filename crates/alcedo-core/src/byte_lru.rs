//! A small size-bounded LRU map for in-memory caches (message bodies,
//! inline/attachment bytes). Entries are weighed in bytes; when the total
//! exceeds the budget the least recently used entries are dropped.
//!
//! Lookups take `&self` (recency is tracked with a `Cell`), so it drops into
//! `Rc<RefCell<_>>` call sites that only `borrow()` to read. Eviction scans
//! the entries, which is fine for the few hundred items these caches hold.

use std::borrow::Borrow;
use std::cell::Cell;
use std::collections::HashMap;
use std::hash::Hash;

/// Anything stored in a [`ByteLru`] reports its approximate heap size.
pub trait ByteWeight {
    fn byte_weight(&self) -> usize;
}

impl ByteWeight for String {
    fn byte_weight(&self) -> usize {
        self.len()
    }
}

impl ByteWeight for Vec<u8> {
    fn byte_weight(&self) -> usize {
        self.len()
    }
}

impl<A: ByteWeight, B: ByteWeight> ByteWeight for (A, B) {
    fn byte_weight(&self) -> usize {
        self.0.byte_weight() + self.1.byte_weight()
    }
}

struct Entry<V> {
    value: V,
    weight: usize,
    last_used: Cell<u64>,
}

pub struct ByteLru<K, V> {
    entries: HashMap<K, Entry<V>>,
    max_bytes: usize,
    total_bytes: usize,
    clock: Cell<u64>,
}

impl<K: Hash + Eq + Clone, V: ByteWeight> ByteLru<K, V> {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            max_bytes,
            total_bytes: 0,
            clock: Cell::new(0),
        }
    }

    fn tick(&self) -> u64 {
        let next = self.clock.get().wrapping_add(1);
        self.clock.set(next);
        next
    }

    /// Look up `key` and mark it most recently used.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = self.entries.get(key)?;
        entry.last_used.set(self.tick());
        Some(&entry.value)
    }

    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.entries.contains_key(key)
    }

    /// Insert or replace `key`, then evict least recently used entries until
    /// the budget fits. The entry just inserted is never evicted, so a single
    /// value larger than the budget is still kept (alone).
    pub fn insert(&mut self, key: K, value: V) -> Option<V> {
        let weight = value.byte_weight();
        let entry = Entry {
            value,
            weight,
            last_used: Cell::new(self.tick()),
        };
        let old = self.entries.insert(key.clone(), entry).map(|old| {
            self.total_bytes -= old.weight;
            old.value
        });
        self.total_bytes += weight;
        self.evict_except(&key);
        old
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        let entry = self.entries.remove(key)?;
        self.total_bytes -= entry.weight;
        Some(entry.value)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.total_bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    fn evict_except(&mut self, keep: &K) {
        while self.total_bytes > self.max_bytes && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .filter(|(key, _)| *key != keep)
                .min_by_key(|(_, entry)| entry.last_used.get())
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.total_bytes -= entry.weight;
            }
        }
    }
}

impl<K: Hash + Eq + Clone, V: ByteWeight> FromIterator<(K, V)> for ByteLru<K, V> {
    /// Collect without a budget limit (`usize::MAX`); use for fixtures.
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
        let mut cache = Self::new(usize::MAX);
        for (key, value) in iter {
            cache.insert(key, value);
        }
        cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: usize) -> String {
        "x".repeat(n)
    }

    #[test]
    fn evicts_least_recently_used_over_budget() {
        let mut cache: ByteLru<u32, String> = ByteLru::new(10);
        cache.insert(1, s(4));
        cache.insert(2, s(4));
        assert!(cache.get(&1).is_some()); // 1 is now newer than 2
        cache.insert(3, s(4));
        assert!(cache.contains_key(&1));
        assert!(!cache.contains_key(&2));
        assert!(cache.contains_key(&3));
        assert_eq!(cache.total_bytes(), 8);
    }

    #[test]
    fn replace_and_remove_keep_totals_exact() {
        let mut cache: ByteLru<String, (Vec<u8>, String)> = ByteLru::new(100);
        cache.insert("a".into(), (vec![0; 10], "x/y".into()));
        assert_eq!(cache.total_bytes(), 13);
        let old = cache.insert("a".into(), (vec![0; 2], "x".into()));
        assert!(old.is_some());
        assert_eq!(cache.total_bytes(), 3);
        assert!(cache.get("a").is_some());
        cache.remove("a");
        assert_eq!(cache.total_bytes(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn oversized_value_is_kept_alone() {
        let mut cache: ByteLru<u32, String> = ByteLru::new(5);
        cache.insert(1, s(2));
        cache.insert(2, s(50));
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key(&2));
        cache.clear();
        assert_eq!(cache.total_bytes(), 0);
    }
}
