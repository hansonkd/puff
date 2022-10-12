//! Fast sequential structure for local construction.
use crate::types::{Puff, Vector};

/// Fast Key Value structure for local construction.
///
/// Puff's `Vector` type uses [std::vec::Vec] under the hood. `Vec`s are not
/// sharable structures and so need to be wrapped in an Arc to be shared between threads and cheaply copied.
/// You should use a `VectorBuilder` and convert it into a `Vector` when you are done writing to it.
///
/// Maps can only be constructed from types implementing `IntoVectorBuilder`.
///
/// # Examples
///
/// ```
/// use puff_rs::types::VectorBuilder;
///
/// let mut builder = VectorBuilder::new();
/// builder.push(0);
/// let vector = builder.into_vector();
/// ```
///
#[derive(Clone)]
pub struct VectorBuilder<V>(Vec<V>);

impl<T> VectorBuilder<T>
where
    T: Puff,
{
    /// Creates an empty `VectorBuilder`.
    ///
    /// The hash map is initially created with a capacity of 0, so it will not allocate until it
    /// is first inserted into.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::VectorBuilder;
    /// let mut vector: VectorBuilder<i32> = VectorBuilder::new();
    /// ```
    pub fn new() -> VectorBuilder<T> {
        Self(Vec::new())
    }

    /// Creates an empty `VectorBuilder` with the specified capacity.
    ///
    /// The vector will be able to hold at least `capacity` elements without
    /// reallocating. If `capacity` is 0, the hash map will not allocate.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::{Text, VectorBuilder};
    /// let mut vector: VectorBuilder<i32> = VectorBuilder::with_capacity(10);
    /// ```
    pub fn with_capacity(capacity: usize) -> VectorBuilder<T> {
        Self(Vec::with_capacity(capacity))
    }

    /// Inserts a value into the `VectorBuilder`.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::VectorBuilder;
    ///
    /// let mut vector = VectorBuilder::new();
    /// assert_eq!(vector.is_empty(), true);
    /// vector.push(42);
    /// assert_eq!(vector.is_empty(), false);
    /// ```
    #[inline]
    pub fn push(&mut self, item: T) -> () {
        self.0.push(item)
    }

    /// Returns true if the vector contains no elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::{VectorBuilder};
    /// let vector: VectorBuilder<usize> = VectorBuilder::new();
    /// assert!(vector.is_empty())
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
    /// use puff_rs::types::VectorBuilder;
    /// let v = VectorBuilder::from(vec![10, 40, 30]);
    /// assert_eq!(Some(40), v.get(1));
    /// assert_eq!(None, v.get(3));
    /// ```
    pub fn get(&self, index: usize) -> Option<T> {
        self.0.get(index).map(|v| v.puff())
    }

    /// Returns the number of elements in the `VectorBuilder`.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff_rs::types::VectorBuilder;
    ///
    /// let mut vector = VectorBuilder::new();
    /// assert_eq!(vector.len(), 0);
    /// vector.push(2);
    /// assert_eq!(vector.len(), 1);
    /// ```
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Convert the `VectorBuilder` into a Read-Only `Vector` that can be shared between Tasks.
    pub fn into_vector(self) -> Vector<T> {
        Vector::from_vec(self.0)
    }
}

impl<T: Puff> From<Vec<T>> for VectorBuilder<T> {
    fn from(v: Vec<T>) -> Self {
        VectorBuilder(v)
    }
}

pub trait IntoVectorBuilder<V> {
    fn into_vector_builder(self) -> VectorBuilder<V>;
}

impl<T: Into<VectorBuilder<V>>, V: Puff> IntoVectorBuilder<V> for T {
    fn into_vector_builder(self) -> VectorBuilder<V> {
        self.into()
    }
}
