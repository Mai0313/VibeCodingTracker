//! Compact sorted key-value map backed by a `Vec<(K, V)>` for small per-session maps.
//!
//! Preserves the JSON object wire format and deterministic key order of `BTreeMap` while avoiding its per-map leaf allocation for maps that typically hold one or two entries.

use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde::ser::{Serialize, Serializer};
use std::borrow::Borrow;
use std::fmt;
use std::marker::PhantomData;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TinyMap<K, V>(Vec<(K, V)>);

impl<K, V> Default for TinyMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> TinyMap<K, V> {
    pub(crate) const fn new() -> Self {
        Self(Vec::new())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn as_slice(&self) -> &[(K, V)] {
        &self.0
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.0.iter_mut().map(|(_, value)| value)
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        self.0.retain_mut(|(key, value)| keep(key, value));
    }

    #[cfg(test)]
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0
            .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
            .ok()
            .map(|index| &self.0[index].1)
    }

    pub(crate) fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0
            .binary_search_by(|(probe, _)| probe.borrow().cmp(key))
            .ok()
            .map(|index| &mut self.0[index].1)
    }

    #[cfg(test)]
    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.get(key).is_some()
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        if self.0.is_empty() {
            self.0.reserve_exact(1);
            self.0.push((key, value));
            return None;
        }
        if self.0.last().is_some_and(|(last, _)| last < &key) {
            self.0.push((key, value));
            return None;
        }
        match self.0.binary_search_by(|(probe, _)| probe.cmp(&key)) {
            Ok(index) => Some(std::mem::replace(&mut self.0[index].1, value)),
            Err(index) => {
                self.0.insert(index, (key, value));
                None
            }
        }
    }

    pub(crate) fn entry(&mut self, key: K) -> Entry<'_, K, V>
    where
        K: Ord,
    {
        match self.0.binary_search_by(|(probe, _)| probe.cmp(&key)) {
            Ok(index) => Entry::Occupied(&mut self.0[index].1),
            Err(index) => Entry::Vacant {
                entries: &mut self.0,
                index,
                key,
            },
        }
    }
}

pub(crate) enum Entry<'a, K, V> {
    Occupied(&'a mut V),
    Vacant {
        entries: &'a mut Vec<(K, V)>,
        index: usize,
        key: K,
    },
}

impl<'a, K, V> Entry<'a, K, V> {
    pub(crate) fn or_insert_with(self, default: impl FnOnce() -> V) -> &'a mut V {
        match self {
            Self::Occupied(slot) => slot,
            Self::Vacant {
                entries,
                index,
                key,
            } => {
                let value = default();
                if entries.is_empty() {
                    entries.reserve_exact(1);
                    entries.push((key, value));
                    &mut entries[0].1
                } else {
                    entries.insert(index, (key, value));
                    &mut entries[index].1
                }
            }
        }
    }

    pub(crate) fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }
}

impl<'a, K, V> IntoIterator for &'a TinyMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::iter::Map<std::slice::Iter<'a, (K, V)>, fn(&'a (K, V)) -> (&'a K, &'a V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().map(|(key, value)| (key, value))
    }
}

impl<K, V> IntoIterator for TinyMap<K, V> {
    type Item = (K, V);
    type IntoIter = std::vec::IntoIter<(K, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<K: Ord, V, const N: usize> From<[(K, V); N]> for TinyMap<K, V> {
    fn from(items: [(K, V); N]) -> Self {
        let mut map = Self::new();
        for (key, value) in items {
            map.insert(key, value);
        }
        map.0.shrink_to_fit();
        map
    }
}

#[cfg(test)]
impl<K, Q, V> std::ops::Index<&Q> for TinyMap<K, V>
where
    K: Borrow<Q>,
    Q: Ord + ?Sized,
{
    type Output = V;

    fn index(&self, key: &Q) -> &Self::Output {
        self.get(key).expect("no entry found for key")
    }
}

impl<K: Serialize, V: Serialize> Serialize for TinyMap<K, V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
    }
}

impl<'de, K, V> Deserialize<'de> for TinyMap<K, V>
where
    K: Deserialize<'de> + Ord,
    V: Deserialize<'de>,
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct TinyMapVisitor<K, V>(PhantomData<(K, V)>);

        impl<'de, K, V> Visitor<'de> for TinyMapVisitor<K, V>
        where
            K: Deserialize<'de> + Ord,
            V: Deserialize<'de>,
        {
            type Value = TinyMap<K, V>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let Some((first_key, first_val)) = access.next_entry()? else {
                    return Ok(TinyMap::new());
                };
                let Some((second_key, second_val)) = access.next_entry()? else {
                    return Ok(TinyMap(vec![(first_key, first_val)]));
                };
                let mut map = TinyMap(vec![(first_key, first_val)]);
                map.insert(second_key, second_val);
                while let Some((key, value)) = access.next_entry()? {
                    map.insert(key, value);
                }
                map.0.shrink_to_fit();
                Ok(map)
            }
        }

        deserializer.deserialize_map(TinyMapVisitor(PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_unsorted_and_duplicate_keys_in_sorted_last_wins_order() {
        let map: TinyMap<String, i32> =
            serde_json::from_str(r#"{"b":1,"a":2,"c":3,"b":4}"#).unwrap();
        assert_eq!(
            serde_json::to_string(&map).unwrap(),
            r#"{"a":2,"b":4,"c":3}"#
        );
    }

    #[test]
    fn single_entry_deserialization_keeps_exact_capacity() {
        let map: TinyMap<String, i32> = serde_json::from_str(r#"{"only":7}"#).unwrap();
        assert_eq!(map.0.capacity(), 1);
        assert_eq!(map["only"], 7);
    }
}
