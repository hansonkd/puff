//! Fast Key Value structure for local construction.
use crate::types::{Map, Puff};
use std::collections::HashMap;
use std::hash::Hash;

/// Fast Key Value structure for local construction.
///
/// Puff's `MapBuilder` type uses [std::collections::hash_map::HashMap] under the hood. `HashMap`s are not
/// sharable structures and so need to be wrapped in an Arc to be shared between threads and cheaply copied.
/// You should use a `MapBuilder` and convert it into a `Map` when you are done writing to it.
///
/// Maps can only be constructed from types implementing `IntoMapBuilder`.
///
/// # Examples
///
/// ```
/// use puff::types::MapBuilder;
///
/// let mut builder = MapBuilder::new();
/// builder.insert(42, 0);
/// let map = builder.into_map();
/// ```
///
#[derive(Clone)]
pub struct MapBuilder<K, V>(HashMap<K, V>);

impl<K, T> MapBuilder<K, T>
where
    K: Puff + Hash + Eq,
    T: Puff,
{
    /// Creates an empty `MapBuilder`.
    ///
    /// The hash map is initially created with a capacity of 0, so it will not allocate until it
    /// is first inserted into.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::{Text, MapBuilder};
    /// let mut map: MapBuilder<Text, i32> = MapBuilder::new();
    /// ```
    pub fn new() -> MapBuilder<K, T> {
        Self(HashMap::new())
    }

    /// Creates an empty `MapBuilder` with the specified capacity.
    ///
    /// The hash map will be able to hold at least `capacity` elements without
    /// reallocating. If `capacity` is 0, the hash map will not allocate.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::{Text, MapBuilder};
    /// let mut map: MapBuilder<Text, i32> = MapBuilder::with_capacity(10);
    /// ```
    pub fn with_capacity(capacity: usize) -> MapBuilder<K, T> {
        Self(HashMap::with_capacity(capacity))
    }

    /// Inserts a key-value pair into the `MapBuilder`.
    ///
    /// If the map did not have this key present, [`None`] is returned.
    ///
    /// If the map did have this key present, the value is updated, and the old
    /// value is returned. The key is not updated, though; this matters for
    /// types that can be `==` without being identical. See the [module-level
    /// documentation] for more.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::MapBuilder;
    ///
    /// let mut map = MapBuilder::new();
    /// assert_eq!(map.insert(37, 42), None);
    /// assert_eq!(map.is_empty(), false);
    ///
    /// map.insert(37, 10);
    /// assert_eq!(map.insert(37, 13), Some(10));
    /// ```
    #[inline]
    pub fn insert(&mut self, key: K, item: T) -> Option<T> {
        self.0.insert(key, item)
    }

    /// Returns a reference to the value corresponding to the key.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::MapBuilder;
    ///
    /// let mut map = MapBuilder::new();
    /// map.insert(1, 2);
    /// assert_eq!(map.get(1), Some(2));
    /// ```
    #[inline]
    pub fn get(&self, index: K) -> Option<T> {
        self.0.get(&index).map(|v| v.clone())
    }

    /// Returns true if the map contains no elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::MapBuilder;
    /// let map: MapBuilder<usize, usize> = MapBuilder::new();
    /// assert!(map.is_empty())
    /// ```
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns the number of elements in the `MapBuilder`.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::MapBuilder;
    ///
    /// let mut map = MapBuilder::new();
    /// assert_eq!(map.len(), 0);
    /// map.insert(1, 2);
    /// assert_eq!(map.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Convert the `MapBuilder` into a Read-Only `Map` that can be shared between Tasks.
    pub fn into_map(self) -> Map<K, T> {
        Map::from_hash_map(self.0)
    }
}

pub trait IntoMapBuilder<K, V> {
    fn into_map_builder(self) -> MapBuilder<K, V>;
}

impl<T: Into<MapBuilder<K, V>>, K: Puff + Hash + Eq, V: Puff> IntoMapBuilder<K, V> for T {
    fn into_map_builder(self) -> MapBuilder<K, V> {
        self.into()
    }
}

impl<K: Puff + Hash + Eq, V: Puff> Into<MapBuilder<K, V>> for HashMap<K, V> {
    fn into(self) -> MapBuilder<K, V> {
        MapBuilder(self)
    }
}

impl<K: Puff + Hash + Eq, V: Puff> Into<MapBuilder<K, V>> for Vec<(K, V)> {
    fn into(self) -> MapBuilder<K, V> {
        MapBuilder(self.into_iter().collect())
    }
}
