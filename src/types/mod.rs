//! Convenient types for working in the cloud.
//!
//! Puff favors 'static lifetimes. Puff tends to use more smart pointers and other tricks
//! for managing data to make things easier. This incurs a slight overhead, but makes up for it in terms
//! of usability.
//!
//! Puff is targeted towards Python developers and as such tries not to expose references
//! or lifetimes too often in the public API.
//!
//! ## Cheap Clones with `puff()`
//!
//! A new Trait, [crate::types::Puff] is also introduced to explicitly make the distinction between Types that are cheap to
//! clone and those that are not.
//!
//! Calling `puff()` on data will act the same as clone, but in general is expected to be a copy of a reference
//! or other cheap operation.
//!
//! ## Builder Types
//! In general there are two types: the Puff type and the Builder. For example you have:
//! Text and TextBuilder, Map and MapBuilder, Vector and VectorBuilder, etc...
//!
//! The builder Type is not meant to be passed across the Task closure. Instead convert Builders to their
//! underlying Puff type. If you want to share a Text among several Puff Tasks, first build it with
//! `TextBuilder` then convert it into `Text`.
//!
//! # Example
//!
//! ```
//! use puff::types::{Puff, Text};
//! let my_str = String::from("wow!");
//! let other_str = my_str.clone(); // Potentially expensive because it will copy memory
//!
//! // Compared to a Puff trait:
//! let text = Text::from("Hello");
//! let other_text = text.puff(); // Creates a cheap reference to immutable data.
//! ```
//!
pub mod bytes_builder;
pub mod map;
pub mod map_builder;
pub mod text;
pub mod text_builder;
pub mod vector;
pub mod vector_builder;

use axum::body::Bytes as AxumBytes;
pub use bytes_builder::BytesBuilder;
use chrono::{Date, DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ops::Deref;
use axum::response::{IntoResponse, Response};
use bb8_redis::redis::{ErrorKind, FromRedisValue, RedisError, RedisResult, Value};
use pyo3::{Py, PyObject};
use pyo3::{IntoPy, Python, ToPyObject};
use pyo3::types::PyBytes;

pub use map::Map;
pub use map_builder::MapBuilder;
pub use text::Text;
pub use text_builder::TextBuilder;
pub use vector::Vector;
pub use vector_builder::VectorBuilder;

/// A Trait representing a cheap clone. The only types that should implement `Puff` are generally
/// references wrapped in Arc or other type that is cheaply cloned. For example a struct holding a `Vec` is not a
/// good candidate for a Puff trait, but a struct holding an `Arc<_>` is.
///
/// Puff Types must also be lifetime free and be able to be sent across a Thread.
pub trait Puff: Clone + Send + Sync + 'static {
    fn puff(&self) -> Self {
        self.clone()
    }
}

/// A Puff Date for UTC.
#[derive(Clone)]
pub struct Bytes(AxumBytes);

impl Serialize for Bytes {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serde_bytes::Bytes::new(&self.0).serialize(serializer)
    }
}

impl FromRedisValue for Bytes {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        match v {
            Value::Data(v) => Ok(Bytes::copy_from_slice(v.as_slice())),
            Value::Okay => Ok("OK".into()),
            Value::Status(ref val) => Ok(val.to_string().into()),
            val => Err(RedisError::from((
                ErrorKind::TypeError,
                "Response was of incompatible type",
                format!("Response type not bytes compatible. (response was {:?})", val),
            ))),
        }
    }
}

impl IntoResponse for Bytes {
    fn into_response(self) -> Response {
        self.0.into_response()
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        serde_bytes::ByteBuf::deserialize(deserializer)
            .map(|v| Bytes(AxumBytes::from(v.into_vec())))
    }
}

impl Bytes {
    pub fn copy_from_slice(slice: &[u8]) -> Self {
        Self(AxumBytes::copy_from_slice(slice))
    }

    pub fn into_bytes(self) -> AxumBytes {
        self.0
    }
}

impl<T: Into<AxumBytes>> From<T> for Bytes {
    fn from(v: T) -> Self {
        Bytes(v.into())
    }
}

impl IntoPy<Py<PyBytes>> for Bytes {
    fn into_py(self, py: Python<'_>) -> Py<PyBytes> {
        PyBytes::new(py, &self).into_py(py)
    }
}

impl ToPyObject for Bytes {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.clone().into_py(py).to_object(py)
    }
}


impl Deref for Bytes {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// A Puff Datetime for UTC.
#[derive(Clone, Serialize, Deserialize)]
pub struct UtcDateTime(DateTime<Utc>);

impl Deref for UtcDateTime {
    type Target = DateTime<Utc>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl UtcDateTime {
    pub fn new(dt: DateTime<Utc>) -> Self {
        Self(dt)
    }
}

/// A Puff Date for UTC.
#[derive(Clone)]
pub struct UtcDate(Date<Utc>);

impl Deref for UtcDate {
    type Target = Date<Utc>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl UtcDate {
    pub fn new(dt: Date<Utc>) -> Self {
        Self(dt)
    }
}

impl<T: Puff, T2: Puff> Puff for (T, T2) {
    fn puff(&self) -> Self {
        (self.0.puff(), self.1.puff())
    }
}

impl<T: Puff, T2: Puff, T3: Puff> Puff for (T, T2, T3) {
    fn puff(&self) -> Self {
        (self.0.puff(), self.1.puff(), self.2.puff())
    }
}

impl<T: Puff, T2: Puff, T3: Puff, T4: Puff> Puff for (T, T2, T3, T4) {
    fn puff(&self) -> Self {
        (self.0.puff(), self.1.puff(), self.2.puff(), self.3.puff())
    }
}

impl Puff for UtcDate {}
impl Puff for UtcDateTime {}
impl<T: Puff> Puff for Option<T> {}
impl Puff for usize {}
impl Puff for f64 {}
impl Puff for f32 {}
impl Puff for u8 {}
impl Puff for u16 {}
impl Puff for u32 {}
impl Puff for u64 {}
impl Puff for i8 {}
impl Puff for i32 {}
impl Puff for i64 {}
impl Puff for bool {}
impl Puff for () {}
impl Puff for Bytes {}
