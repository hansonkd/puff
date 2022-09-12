//! Fast UTF8 data references.

use crate::types::Puff;
use axum::body::Bytes as AxumBytes;
use axum::response::{IntoResponse, Response};
use compact_str::{CompactString, ToCompactString};
use serde::Serialize;
use std::ops::{Add, Deref};

/// Fast UTF8 data references.
///
/// Puff's Text type uses [compact_str::CompactString] under the hood. This inlines small strings
/// for maximum performance and storage as well as prevents copying 'static strings. The trade off
/// for better string memory management comes at the cost of mutability. `Text` types are immutable.
///
/// ```
/// use puff::types::Text;
///
/// let text = Text::from("Hello");
/// ```
///
/// Standard rust `Strings` are meant to be mutable. They are available as a `TextBuilder`:
///
/// ```
/// use puff::types::{TextBuilder, Text};
///
/// let mut buff = TextBuilder::new();
/// buff.push_str("hello");
/// buff.push_str(" ");
/// buff.push_str("world");
/// let t: Text = buff.into_text();
/// ```
#[derive(Hash, PartialEq, Eq, Debug, Clone, Serialize)]
pub struct Text(CompactString);

impl Text {
    pub fn new() -> Text {
        Text(CompactString::from(""))
    }

    pub fn into_string(self) -> String {
        return self.0.into();
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
//
// impl<'source> FromPyObject<'source> for Text {
//     fn extract(ob: &'source PyAny) -> PyResult<Self> {
//         let s: &str = ob.extract()?;
//         Ok(Text(CompactString::from(s)))
//     }
// }

// impl ToPyObject for Text {
//     fn to_object(&self, py: Python<'_>) -> PyObject {
//         self.0.to_object(py)
//     }
// }

impl Add for Text {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Text(self.0 + &other.0)
    }
}

impl Add<&str> for Text {
    type Output = Self;

    fn add(self, other: &str) -> Self {
        let mut s = self.0.to_string();
        s.push_str(other);
        Text(CompactString::from(s))
    }
}

// impl AddAssign<&str> for Text {
//     fn add_assign(&mut self, rhs: &str) {
//         self.0 = self.0.clone().add(rhs)
//     }
// }
//
// impl AddAssign<&Text> for Text {
//     fn add_assign(&mut self, rhs: &Text) {
//         self.0 = self.0.clone().add(rhs.0.as_str())
//     }
// }
//
// impl AddAssign for Text {
//     fn add_assign(&mut self, rhs: Self) {
//         self.0 = self.0.clone().add(&rhs.0)
//     }
// }
//
// impl From<u32> for Text {
//     fn from(x: u32) -> Self {
//         Text(x.to_compact_string())
//     }
// }

pub trait ToText {
    fn to_text(self) -> Text;
}

impl<T: ToCompactString> ToText for T {
    fn to_text(self) -> Text {
        Text(self.to_compact_string())
    }
}

impl<T: ToCompactString> From<T> for Text {
    fn from(x: T) -> Self {
        Text(x.to_compact_string())
    }
}

impl From<Text> for String {
    fn from(x: Text) -> Self {
        x.to_string()
    }
}

impl IntoResponse for Text {
    fn into_response(self) -> Response {
        AxumBytes::copy_from_slice(self.0.as_bytes()).into_response()
    }
}
//
// impl From<String> for Text {
//     fn from(x: String) -> Self {
//         Text(x.to_compact_string())
//     }
// }
//
// impl From<&'static str> for Text {
//     fn from(x: &'static str) -> Self {
//         Text(x.to_compact_string())
//     }
// }

impl Puff for Text {}

impl Deref for Text {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        self.0.as_str()
    }
}
