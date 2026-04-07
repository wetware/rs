//! Persistent map wrapper over im::HashMap.
//!
//! ValMap is a thin wrapper providing a stable API surface for Glia maps.
//! Today it delegates to im::HashMap (CHAMP trie). Tomorrow the backing
//! can swap to an IPLD HAMT (CID-linked blocks) without changing callers.

use crate::Val;

/// A persistent map backed by im::HashMap (CHAMP trie).
///
/// O(1) clone via structural sharing. O(log N) insert. O(1) amortized lookup.
/// The wrapper exists as the future seam for IPLD-backed persistent maps.
#[derive(Clone)]
pub struct ValMap(im::HashMap<Val, Val>);

impl ValMap {
    /// Create an empty map.
    #[inline]
    pub fn new() -> Self {
        ValMap(im::HashMap::new())
    }

    /// Create a map from key-value pairs.
    #[inline]
    pub fn from_pairs(pairs: Vec<(Val, Val)>) -> Self {
        ValMap(pairs.into_iter().collect())
    }

    /// Number of entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True if the map has no entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Look up a value by key.
    #[inline]
    pub fn get(&self, key: &Val) -> Option<&Val> {
        self.0.get(key)
    }

    /// True if the map contains the given key.
    #[inline]
    pub fn contains_key(&self, key: &Val) -> bool {
        self.0.contains_key(key)
    }

    /// Return a new map with the key-value pair inserted or updated.
    #[inline]
    pub fn assoc(&self, key: Val, val: Val) -> Self {
        ValMap(self.0.update(key, val))
    }

    /// Return a new map without the given key.
    #[inline]
    pub fn dissoc(&self, key: &Val) -> Self {
        ValMap(self.0.without(key))
    }

    /// Iterate over key-value pairs.
    #[inline]
    pub fn iter(&self) -> im::hashmap::Iter<'_, Val, Val> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a ValMap {
    type Item = (&'a Val, &'a Val);
    type IntoIter = im::hashmap::Iter<'a, Val, Val>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Default for ValMap {
    fn default() -> Self {
        ValMap::new()
    }
}

impl PartialEq for ValMap {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Eq for ValMap {}

impl core::fmt::Debug for ValMap {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_map().entries(self.0.iter()).finish()
    }
}

impl std::hash::Hash for ValMap {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Order-independent hash: XOR individual pair hashes.
        let mut xor_hash: u64 = 0;
        for (k, v) in self.0.iter() {
            use std::hash::Hasher;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            k.hash(&mut h);
            v.hash(&mut h);
            xor_hash ^= h.finish();
        }
        state.write_u64(xor_hash);
        state.write_usize(self.0.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_ops() {
        let m = ValMap::new();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);

        let m = m.assoc(Val::Keyword("a".into()), Val::Int(1));
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&Val::Keyword("a".into())), Some(&Val::Int(1)));
        assert!(m.contains_key(&Val::Keyword("a".into())));
        assert!(!m.contains_key(&Val::Keyword("b".into())));

        let m = m.assoc(Val::Keyword("a".into()), Val::Int(2));
        assert_eq!(m.len(), 1);
        assert_eq!(m.get(&Val::Keyword("a".into())), Some(&Val::Int(2)));

        let m = m.dissoc(&Val::Keyword("a".into()));
        assert!(m.is_empty());
    }

    #[test]
    fn structural_sharing() {
        let m1 = ValMap::new()
            .assoc(Val::Keyword("a".into()), Val::Int(1))
            .assoc(Val::Keyword("b".into()), Val::Int(2));

        // Clone is O(1) — both share the same tree.
        let m2 = m1.assoc(Val::Keyword("c".into()), Val::Int(3));

        // m1 is unmodified.
        assert_eq!(m1.len(), 2);
        assert_eq!(m2.len(), 3);
        assert_eq!(m1.get(&Val::Keyword("a".into())), Some(&Val::Int(1)));
        assert!(m1.get(&Val::Keyword("c".into())).is_none());
        assert_eq!(m2.get(&Val::Keyword("c".into())), Some(&Val::Int(3)));
    }

    #[test]
    fn equality() {
        // Same entries in different insertion order should be equal.
        let m1 = ValMap::new()
            .assoc(Val::Int(1), Val::Int(10))
            .assoc(Val::Int(2), Val::Int(20));
        let m2 = ValMap::new()
            .assoc(Val::Int(2), Val::Int(20))
            .assoc(Val::Int(1), Val::Int(10));
        assert_eq!(m1, m2);
    }

    #[test]
    fn from_pairs() {
        let pairs = vec![
            (Val::Keyword("x".into()), Val::Int(1)),
            (Val::Keyword("y".into()), Val::Int(2)),
        ];
        let m = ValMap::from_pairs(pairs);
        assert_eq!(m.len(), 2);
        assert_eq!(m.get(&Val::Keyword("x".into())), Some(&Val::Int(1)));
    }

    #[test]
    fn iter_all_entries() {
        let m = ValMap::new()
            .assoc(Val::Keyword("a".into()), Val::Int(1))
            .assoc(Val::Keyword("b".into()), Val::Int(2));

        let mut entries: Vec<_> = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        entries.sort_by(|a, b| format!("{}", a.0).cmp(&format!("{}", b.0)));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0], (Val::Keyword("a".into()), Val::Int(1)));
        assert_eq!(entries[1], (Val::Keyword("b".into()), Val::Int(2)));
    }
}
