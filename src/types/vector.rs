//! Fast Read-Only sequential structures.
use crate::types::vector_builder::IntoVectorBuilder;
use crate::types::{Puff, VectorBuilder};
use owning_ref::ArcRef;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt::{Debug, Formatter};
use std::ops::Range;
use std::sync::Arc;

/// Fast Read-Only sequential structures.
///
/// Puff's `Vector` type uses [owning_ref::ArcRef] under the hood. This allows you to slice the
/// Vector cheaply while retaining only one copy of the underlying data without having to deal with
/// lifetimes.
///
/// Vectors can only be constructed from types implementing `IntoVectorBuilder`.
///
/// ```
/// use puff_rs::types::Vector;
///
/// let vector = Vector::from_vec(vec![123]);
/// assert_eq!(vector.get(0), Some(123));
/// assert_eq!(vector.get(1), None);
///
/// for v in vector {
///     assert_eq!(v, 123);
/// }
/// ```
///
#[derive(Clone)]
pub struct Vector<T: Puff>(ArcRef<Vec<T>, [T]>);

impl<T: Serialize + Puff> Serialize for Vector<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de> + Puff> Deserialize<'de> for Vector<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::deserialize(deserializer).map(|v| Vector::from_vec(v))
    }
}

impl<T: Debug + Puff> Debug for Vector<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: PartialEq + Eq + Puff> PartialEq for Vector<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq(&other.0)
    }
}

impl<T: Puff> Vector<T>
where
    T: Clone,
{
    /// Create an empty Vector
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let map: Vector<i32> = Vector::new();
    /// ```
    pub fn new() -> Vector<T> {
        Vector(ArcRef::new(Arc::new(Vec::new())).map(|v| v.as_slice()))
    }

    /// Create a new Vector from a raw Vec.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let map: Vector<i32> = Vector::from_vec(Vec::new());
    /// ```
    pub fn from_vec(hm: Vec<T>) -> Vector<T> {
        Vector(ArcRef::new(Arc::new(hm)).map(|v| v.as_slice()))
    }

    /// Create a new Vector from a `IntoVectorBuilder`
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let map: Vector<i32> = Vector::from_vector_builder(Vec::new());
    /// ```
    pub fn from_vector_builder<V: IntoVectorBuilder<T>>(hm: V) -> Vector<T> {
        hm.into_vector_builder().into_vector()
    }

    /// Returns if a Vector is empty
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let v: Vector<i32> = Vector::new();
    /// assert!(v.is_empty());
    /// ```
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns an element.
    ///
    /// - If given a position, returns a reference to the element at that
    ///   position or `None` if out of bounds.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let v = Vector::from_vec(vec![10, 40, 30]);
    /// assert_eq!(Some(40), v.get(1));
    /// assert_eq!(None, v.get(3));
    /// ```
    pub fn get(&self, index: usize) -> Option<T> {
        self.0.get(index).map(|v| v.puff())
    }

    /// Returns an Vector depending on the slice.
    ///
    /// - If given a range, returns the subslice corresponding to that range,
    ///   or `None` if out of bounds.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let v = Vector::from_vec(vec![10, 40, 30]);
    /// assert_eq!(Some(Vector::from_vec(vec![10, 40])), v.slice(0..2));
    /// assert_eq!(None, v.slice(0..4));
    /// ```
    pub fn slice(&self, index: Range<usize>) -> Option<Vector<T>> {
        let index_len = index.len();
        let inner = self.0.clone().map(|r| r.get(index).unwrap_or_default());
        if inner.len() < index_len {
            None
        } else {
            Some(Vector(inner))
        }
    }

    /// Returns the length of a vector
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::Vector;
    /// let v: Vector<i32> = Vector::new();
    /// assert_eq!(v.len(), 0);
    /// ```
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl<T: Puff> IntoIterator for Vector<T> {
    type Item = T;
    type IntoIter = VectorIterator<T>;

    fn into_iter(self) -> Self::IntoIter {
        VectorIterator(self, 0)
    }
}

/// An Iterator over a `Vector`.
pub struct VectorIterator<T: Puff>(Vector<T>, usize);

impl<T: Puff> Iterator for VectorIterator<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        let old_ix = self.1;
        self.1 += 1;
        self.0 .0.get(old_ix).map(|v| v.puff())
    }
}

impl<T: Puff> From<VectorBuilder<T>> for Vector<T> {
    fn from(v: VectorBuilder<T>) -> Self {
        v.into_vector()
    }
}
