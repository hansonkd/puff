use std::collections::HashMap;
use std::hash::Hash;

pub struct Map<K, V>(HashMap<K, V>);

impl<K, T> Map<K, T>
where
    K: Clone + Sync + Send + Hash + Eq + 'static,
    T: Clone + Sync + Send + 'static,
{
    pub fn new() -> Map<K, T> {
        Map(HashMap::new())
    }

    pub fn insert(&self, key: K, item: T) -> Map<K, T> {
        let mut s = self.0.clone();
        s.insert(key, item);
        Map(s)
    }

    pub fn set(&mut self, key: K, item: T) -> () {
        self.0.insert(key, item);
    }

    pub fn get(&self, index: K) -> Option<T> {
        self.0.get(&index).map(|v| v.clone())
    }

    // pub fn into_iter(self) -> ConsumingIter<(K, T)> {
    //     self.0.into_iter()
    // }

    fn len(&self) -> usize {
        self.0.len()
    }
}
