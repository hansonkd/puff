//! Dynamically build a Bytes object.

/// Fast Byte Buffer
///
/// Puff's `BytesBuilder` type uses [std::vec::Vec] under the hood. `Vec`s are not
/// sharable structures and so need to be wrapped in an Arc to be shared between threads and cheaply copied.
/// You should use a `BytesBuilder` and convert it into `Bytes` when you are done writing to it.
///
/// # Examples
///
/// ```
/// use puff::types::BytesBuilder;
///
/// let mut builder = BytesBuilder::new();
/// builder.put("hello_world");
/// let bytes = builder.into_bytes();
/// ```
///
pub struct BytesBuilder(Vec<u8>);
use crate::types::Bytes;

impl BytesBuilder {
    /// Creates an empty `BytesBuilder`.
    ///
    /// The hash map is initially created with a capacity of 0, so it will not allocate until it
    /// is first inserted into.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::{Text, BytesBuilder};
    /// let mut builder = BytesBuilder::new();
    /// ```
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Creates an empty `BytesBuilder` with the specified capacity.
    ///
    /// The buffer will be able to hold at least `capacity` elements without
    /// reallocating. If `capacity` is 0, the hash map will not allocate.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::BytesBuilder;
    /// let mut builder = BytesBuilder::with_capacity(10);
    /// ```
    pub fn with_capacity(capacity: usize) -> Self {
        Self(Vec::with_capacity(capacity))
    }

    /// Puts some bytes into the builder.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::BytesBuilder;
    /// let mut builder = BytesBuilder::new();
    /// builder.put("Hi");
    /// ```
    pub fn put<T: Into<Bytes>>(&mut self, slice: T) {
        self.0.extend_from_slice(slice.into().as_ref())
    }

    /// Puts a slice into the builder.
    ///
    /// # Examples
    ///
    /// ```
    /// use puff::types::BytesBuilder;
    /// let mut builder = BytesBuilder::new();
    /// builder.put_slice("Hi".as_bytes());
    /// ```
    pub fn put_slice(&mut self, slice: &[u8]) {
        self.0.extend_from_slice(slice)
    }

    /// Converts `BytesBuilder` into `Bytes`.
    pub fn into_bytes(self) -> Bytes {
        Bytes::from(self.0)
    }
}
