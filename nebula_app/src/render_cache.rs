//! Byte and entry bounded LRU used by application-owned render resources.

use std::collections::HashMap;
use std::hash::Hash;

struct Entry<V> {
    value: V,
    bytes: usize,
    touched: u64,
}

pub(crate) struct RenderCache<K, V> {
    entries: HashMap<K, Entry<V>>,
    max_bytes: usize,
    max_entries: usize,
    used: usize,
    clock: u64,
}

impl<K: Eq + Hash + Clone, V> RenderCache<K, V> {
    pub(crate) fn new(max_bytes: usize, max_entries: usize) -> Self {
        Self { entries: HashMap::new(), max_bytes, max_entries, used: 0, clock: 0 }
    }

    pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
        let entry = self.entries.get_mut(key)?;
        self.clock = self.clock.saturating_add(1);
        entry.touched = self.clock;
        Some(&entry.value)
    }

    /// Reject an individually oversized value before disturbing useful entries.
    pub(crate) fn insert(&mut self, key: K, value: V, bytes: usize) -> bool {
        let bytes = bytes.max(1);
        if bytes > self.max_bytes || self.max_entries == 0 {
            return false;
        }
        self.remove(&key);
        while self.entries.len() >= self.max_entries
            || self.used.saturating_add(bytes) > self.max_bytes
        {
            let oldest = self.entries.iter().min_by_key(|(_, entry)| entry.touched);
            let Some(key) = oldest.map(|(key, _)| key.clone()) else { break };
            self.remove(&key);
        }
        self.clock = self.clock.saturating_add(1);
        self.used += bytes;
        self.entries.insert(key, Entry { value, bytes, touched: self.clock });
        true
    }

    pub(crate) fn remove(&mut self, key: &K) {
        if let Some(entry) = self.entries.remove(key) {
            self.used -= entry.bytes;
        }
    }

    /// Reserve part of the owner's budget before background allocation begins.
    /// Growing the allowance does not eagerly allocate or resurrect evicted data.
    pub(crate) fn set_byte_budget(&mut self, bytes: usize) {
        self.max_bytes = bytes;
        while self.used > bytes {
            let oldest = self.entries.iter().min_by_key(|(_, entry)| entry.touched);
            let Some(key) = oldest.map(|(key, _)| key.clone()) else { break };
            self.remove(&key);
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn used_bytes(&self) -> usize {
        self.used
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reservations_release_cold_resources_before_allocation() {
        let mut cache = RenderCache::new(100, 8);
        cache.insert("cold", 1, 40);
        cache.insert("hot", 2, 40);
        cache.set_byte_budget(50);
        assert_eq!(cache.get(&"cold"), None);
        assert_eq!(cache.get(&"hot"), Some(&2));
        assert!(!cache.insert("large", 3, 51));
        cache.set_byte_budget(100);
        assert_eq!(cache.used_bytes(), 40);
        cache.set_byte_budget(0);
        assert_eq!(cache.len(), 0);
        assert!(!cache.insert("failure", 4, 1));
    }

    #[test]
    fn scrolling_evicts_one_cold_resource_and_keeps_hot_resources() {
        let mut cache = RenderCache::new(64, 4);
        for key in 0..4 {
            cache.insert(key, key, 16);
        }
        assert_eq!(cache.get(&0), Some(&0));
        cache.insert(4, 4, 16);
        assert_eq!(cache.len(), 4);
        assert_eq!(cache.get(&0), Some(&0));
        assert_eq!(cache.get(&1), None);
        assert_eq!(cache.get(&2), Some(&2));
        assert_eq!(cache.used_bytes(), 64);
    }

    #[test]
    fn resize_replacements_and_oversized_images_preserve_accounting() {
        let mut cache = RenderCache::new(100, 8);
        cache.insert("formula", 1, 40);
        cache.insert("molecule", 2, 40);
        assert!(!cache.insert("too large", 3, 101));
        assert_eq!(cache.len(), 2);
        cache.insert("formula", 4, 10);
        assert_eq!(cache.used_bytes(), 50);
        cache.remove(&"molecule");
        assert_eq!(cache.used_bytes(), 10);
    }

    #[test]
    fn eighty_logical_sessions_share_one_budget_during_repeated_resize() {
        let mut cache = RenderCache::new(32 * 1024, 128);
        for resize in 0..40 {
            for session in 0..80 {
                cache.insert((session, resize), session, 1024);
                assert!(cache.used_bytes() <= 32 * 1024);
                assert!(cache.len() <= 32);
            }
        }
        assert_eq!(cache.get(&(79, 39)), Some(&79));
        assert_eq!(cache.get(&(0, 0)), None);
    }
}
