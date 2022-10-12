//! Dynamically build a Text object.
use crate::types::text::ToText;
use crate::types::Text;
use std::ops::Deref;

/// Dynamically build a Text object.
///
/// `TextBuilder` uses a standard Rust `String` under the hood. It can efficiently build UTF-8 strings.
///
/// ```
/// use puff_rs::types::{TextBuilder, Text};
///
/// let mut buff = TextBuilder::new();
/// buff.push_str("hello");
/// buff.push_str(" ");
/// buff.push_str("world");
/// let t: Text = buff.into_text();
/// ```
#[derive(Clone, Debug)]
pub struct TextBuilder(String);

impl TextBuilder {
    /// Creates a new empty `TextBuilder`.
    ///
    /// Given that the `TextBuilder` is empty, this will not allocate any initial
    /// buffer. While that means that this initial operation is very
    /// inexpensive, it may cause excessive allocation later when you add
    /// data. If you have an idea of how much data the `TextBuilder` will hold,
    /// consider the [`with_capacity`] method to prevent excessive
    /// re-allocation.
    ///
    /// [`with_capacity`]: TextBuilder::with_capacity
    ///
    pub fn new() -> TextBuilder {
        Self(String::new())
    }

    /// Creates a new empty `TextBuilder` with a particular capacity.
    ///
    /// `TextBuilder`s have an internal buffer to hold their data. The capacity is
    /// the length of that buffer. This method creates an empty `TextBuilder`, but one with an initial
    /// buffer that can hold `capacity` bytes. This is useful when you may be
    /// appending a bunch of data to the `TextBuilder`, reducing the number of
    /// reallocations it needs to do.
    ///
    /// If the given capacity is `0`, no allocation will occur, and this method
    /// is identical to the [`new`] method.
    ///
    /// [`new`]: TextBuilder::new
    ///
    pub fn with_capacity(capacity: usize) -> TextBuilder {
        Self(String::with_capacity(capacity))
    }

    /// Creates a new `TextBuilder` starting with the given `Text`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use puff_rs::types::TextBuilder;
    /// let mut s = TextBuilder::from_text("foo");
    /// ```
    pub fn from_text<T: Into<Text>>(s: T) -> TextBuilder {
        Self(s.into().to_string())
    }

    /// Appends a given string slice onto the end of this `TextBuilder`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use puff_rs::types::TextBuilder;
    /// let mut s = TextBuilder::from_text("foo");
    ///
    /// s.push_str("bar");
    ///
    /// assert_eq!("foobar", s.as_str());
    /// ```
    #[inline]
    pub fn push_str(&mut self, s: &str) -> () {
        self.0.push_str(s)
    }

    /// Appends a given string slice onto the end of this `TextBuilder`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use puff_rs::types::TextBuilder;
    /// let mut s = TextBuilder::from_text("foo");
    ///
    /// s.push_text("bar");
    ///
    /// assert_eq!("foobar", s.as_str());
    /// ```
    #[inline]
    pub fn push_text<T: Into<Text>>(&mut self, s: T) -> () {
        self.0.push_str(s.into().as_str())
    }

    /// Converts a `TextBuilder` into `Text`.
    ///
    /// # Examples
    ///
    /// Basic usage:
    ///
    /// ```
    /// use puff_rs::types::{Text, TextBuilder};
    /// let mut s = TextBuilder::from_text("foo");
    ///
    /// s.push_text("bar");
    ///
    /// assert_eq!(Text::from("foobar"), s.into_text());
    /// ```
    #[inline]
    pub fn into_text(self) -> Text {
        self.0.to_text()
    }

    /// Get a reference to the underlying str slice.
    #[inline]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl Deref for TextBuilder {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0.as_str()
    }
}
