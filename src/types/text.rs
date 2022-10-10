//! Fast UTF8 data references.

use crate::types::Puff;
use axum::body::Bytes as AxumBytes;
use axum::response::{IntoResponse, Response};
use bb8_redis::redis::{
    ErrorKind, FromRedisValue, RedisError, RedisResult, RedisWrite, ToRedisArgs, Value,
};
use compact_str::{CompactString, ToCompactString};
use pyo3::types::PyString;
use pyo3::{FromPyObject, PyAny, PyObject, PyResult, Python, ToPyObject};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter, Pointer};
use std::ops::{Add, Deref};
use std::str::{from_utf8, FromStr};

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
#[derive(Hash, PartialEq, Eq, Debug, Clone, Serialize, Deserialize)]
pub struct Text(CompactString);

impl Display for Text {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialOrd for Text {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.0.as_str().partial_cmp(other.0.as_str())
    }
}


impl Ord for Text {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.as_str().cmp(other.0.as_str())
    }
}


impl Text {
    pub fn new() -> Text {
        Text(CompactString::from(""))
    }

    pub fn from_utf8(s: &[u8]) -> Option<Text> {
        from_utf8(s).ok().map(|f| Text::from(f))
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

impl ToPyObject for Text {
    fn to_object(&self, py: Python<'_>) -> PyObject {
        self.0.as_str().to_object(py)
    }
}

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

impl<'a> From<&'a str> for Text {
    fn from(s: &'a str) -> Self {
        Text(CompactString::from(s))
    }
}

impl From<String> for Text {
    fn from(s: String) -> Self {
        Text(CompactString::from(s))
    }
}

impl<'a> From<&'a String> for Text {
    fn from(s: &'a String) -> Self {
        Text(CompactString::from(s))
    }
}

impl<'a> From<Cow<'a, str>> for Text {
    fn from(cow: Cow<'a, str>) -> Self {
        Text(CompactString::from(cow))
    }
}

impl From<Box<str>> for Text {
    fn from(b: Box<str>) -> Self {
        Text(CompactString::from(b))
    }
}

impl From<Text> for String {
    fn from(s: Text) -> Self {
        s.0.into()
    }
}

impl FromStr for Text {
    type Err = core::convert::Infallible;
    fn from_str(s: &str) -> Result<Text, Self::Err> {
        Ok(Text(CompactString::from_str(s)?))
    }
}

impl<'source> FromPyObject<'source> for Text {
    fn extract(ob: &'source PyAny) -> PyResult<Self> {
        let s = ob.downcast::<PyString>()?;
        Ok(Text::from(s.to_str()?))
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

// impl Serialize for Text {
//     fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
//         self.0.serialize(serializer)
//     }
// }

//
// impl<'de> Deserialize<'de> for Text {
//     fn deserialize<D>(deserializer: D) -> Result<Text, D::Error>
//     where
//         D: Deserializer<'de>,
//     {
//         let cs = CompactString::deserialize(deserializer)?;
//         Ok(Text(cs))
//     }
// }

impl ToRedisArgs for Text {
    fn write_redis_args<W>(&self, out: &mut W)
    where
        W: ?Sized + RedisWrite,
    {
        out.write_arg(self.as_bytes())
    }
}

impl FromRedisValue for Text {
    fn from_redis_value(v: &Value) -> RedisResult<Self> {
        match v {
            Value::Data(ref bytes) => Ok(from_utf8(bytes)?.into()),
            Value::Okay => Ok("OK".into()),
            Value::Status(ref val) => Ok(val.to_string().into()),
            val => Err(RedisError::from((
                ErrorKind::TypeError,
                "Response was of incompatible type",
                format!(
                    "Response type not string compatible. (response was {:?})",
                    val
                ),
            ))),
        }
    }
}
