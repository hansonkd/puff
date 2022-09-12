//! Fast Read-Only Key Value structures.
//!
use crate::types::map_builder::IntoMapBuilder;
use crate::types::Puff;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::hash_map::{IntoIter, IntoKeys, IntoValues};
use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Arc;

/// Fast Read-Only Key Value structures.
///
/// Puff's `Map` type uses [std::collections::hash_map::HashMap] under the hood. However, in order
/// for it to handle cheap clones, it is wrapped in an Arc, making it read-only.
///
/// Maps can only be constructed from types implementing `IntoMapBuilder`.
///
/// ```
/// use puff::types::Map;
///
/// let map = Map::from_builder(vec![(123, 456)]);
/// assert_eq!(map.get(123), Some(456))
/// ```
///
#[derive(Clone)]
pub struct Map<K: Clone, V: Clone>(Arc<HashMap<K, V>>);

impl<K: Serialize + Clone + Eq + Hash, V: Serialize + Clone> Serialize for Map<K, V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, K: Deserialize<'de> + Clone + Eq + Hash + Puff, V: Deserialize<'de> + Clone + Puff>
    Deserialize<'de> for Map<K, V>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        HashMap::deserialize(deserializer).map(|v| Map::from_hash_map(v))
    }
}

impl<K, T> Map<K, T>
where
    K: Puff + Hash + Eq,
    T: Puff + Send + 'static,
{
    /// Create a new empty Map
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::Map;
    /// let map: Map<u32, u32> = Map::new();
    /// ```
    pub fn new() -> Map<K, T> {
        Map(Arc::new(HashMap::new()))
    }

    /// Take a datatype that implements `IntoMapBuilder` and turn it into a read-only `Map`
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::Map;
    /// let map = Map::from_builder(vec![(123, 456)]);
    /// ```
    pub fn from_builder<B: IntoMapBuilder<K, T>>(builder: B) -> Map<K, T> {
        builder.into_map_builder().into_map()
    }

    /// Create a new Map from a raw HashMap.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::collections::HashMap;
    /// use puff::types::Map;
    /// let map: Map<i32, i32> = Map::from_hash_map(HashMap::new());
    /// ```
    pub fn from_hash_map(hm: HashMap<K, T>) -> Map<K, T> {
        Map(Arc::new(hm))
    }

    /// Returns true if the map contains no elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::Map;
    /// let map: Map<i32, i32> = Map::new();
    /// assert!(map.is_empty())
    /// ```
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the data associated with the key.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::Map;
    /// let map = Map::from_builder(vec![(123, 456)]);
    /// assert_eq!(map.get(123), Some(456))
    /// ```
    pub fn get(&self, index: K) -> Option<T> {
        self.0.get(&index).map(|v| v.puff())
    }

    /// Returns the key, value pair corresponding to the key
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::Map;
    /// let map = Map::from_builder(vec![(123, 456)]);
    /// assert_eq!(map.get_key_value(123), Some((123, 456)))
    /// ```
    pub fn get_key_value(&self, index: K) -> Option<(K, T)> {
        self.0
            .get_key_value(&index)
            .map(|(k, v)| (k.puff(), v.puff()))
    }

    /// The length of the Map
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::Map;
    /// let map: Map<i32, i32> = Map::new();
    /// assert_eq!(map.len(), 0)
    /// ```
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Iterate over the Map.
    ///
    /// Note: normally calling `.iter()` on a Map iterates over `(&K, &V)`,
    /// however, puff's `Map` cheaply clones the data so that concrete data is returned instead.
    /// If this is not desirable, manually iterate over the underlying type using `arc_hashmap`.
    ///
    /// This applies to all Iterators in `Map` (`keys`, `values`, `items`).
    pub fn iter(&self) -> IntoIter<K, T> {
        // Yeah, cloning a HashMap isn't ideal, but we will deal with making proper iterators later.
        let hm = HashMap::clone(&self.0);
        hm.into_iter()
    }

    /// Iterate over all items, cloning their references.
    pub fn items(self) -> IntoIter<K, T> {
        self.iter()
    }

    /// Iterate over all keys, cloning their references.
    pub fn keys(self) -> IntoKeys<K, T> {
        // Yeah, cloning a HashMap isn't ideal, but we will deal with making proper iterators later.
        let hm = HashMap::clone(&self.0);
        hm.into_keys()
    }

    /// Iterate over all values, cloning their references.
    pub fn values(self) -> IntoValues<K, T> {
        // Yeah, cloning a HashMap isn't ideal, but we will deal with making proper iterators later.
        let hm = HashMap::clone(&self.0);
        hm.into_values()
    }

    /// Returns a reference to the underlying raw `HashMap`
    pub fn arc_hashmap(self) -> Arc<HashMap<K, T>> {
        self.0.clone()
    }
}

impl<K: Puff, V: Puff> Puff for Map<K, V> {}
