use anyhow::{anyhow};
use juniper::{graphql_scalar, FromInputValue, InputValue, ScalarValue, Value as JuniperValue};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use std::error::Error;
use std::fmt;
use std::fmt::{Display, Formatter};
use std::hash::{Hash, Hasher};

use tokio_postgres::types::private::BytesMut;
use tokio_postgres::types::{to_sql_checked, IsNull, ToSql, Type};

pub type AggroValue = JuniperValue<AggroScalarValue>;


fn convert_from_input(value: &InputValue<AggroScalarValue>) -> AggroValue {
    match value {
        InputValue::Null => AggroValue::Null,
        InputValue::Scalar(s) => AggroValue::Scalar(s.clone()),
        InputValue::List(l) => {
            AggroValue::List(l.iter().map(|c| convert_from_input(&c.item)).collect())
        }
        InputValue::Object(l) => AggroValue::Object(
            l.iter()
                .map(|(k, c)| (k.item.clone(), convert_from_input(&c.item)))
                .collect(),
        ),
        _ => panic!("Cannot run convert_from_input"),
    }
}

#[graphql_scalar(
    // You can rename the type for GraphQL by specifying the name here.
    name = "GenericScalar",
    scalar = AggroScalarValue,
    // You can also specify a description here.
    // If present, doc comments will be ignored.
    description = "An opaque representation of raw input")]
#[derive(Debug, PartialEq, Clone)]
pub struct GenericScalar(AggroValue);

impl GenericScalar {
    pub fn to_output(v: &GenericScalar) -> AggroValue {
        v.0.clone()
    }

    pub fn from_input(v: &InputValue<AggroScalarValue>) -> Result<GenericScalar, String> {
        Ok(GenericScalar(convert_from_input(v)))
    }

    fn parse_token<'a>(
        _value: juniper::ScalarToken<'a>,
    ) -> juniper::ParseScalarResult<AggroScalarValue> {
        panic!("Shouldn't from_str");
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AggroScalarValue {
    Int(i32),
    // Long(i64),
    Float(f64),
    String(String),
    Boolean(bool),
    Generic(Box<AggroValue>),
}

impl<'de> Deserialize<'de> for AggroScalarValue {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = AggroScalarValue;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a valid input value")
            }

            fn visit_bool<E: de::Error>(self, b: bool) -> Result<Self::Value, E> {
                Ok(AggroScalarValue::Boolean(b))
            }

            fn visit_i32<E: de::Error>(self, n: i32) -> Result<Self::Value, E> {
                Ok(AggroScalarValue::Int(n))
            }

            fn visit_i64<E: de::Error>(self, n: i64) -> Result<Self::Value, E> {
                if i64::from(i32::MIN) <= n && n <= i64::from(i32::MAX) {
                    self.visit_i32(n.try_into().unwrap())
                } else {
                    Err(de::Error::custom("Invalid integer bounds"))
                }
            }

            fn visit_u32<E: de::Error>(self, n: u32) -> Result<Self::Value, E> {
                if n <= i32::MAX as u32 {
                    self.visit_i32(n.try_into().unwrap())
                } else {
                    self.visit_u64(n.into())
                }
            }

            fn visit_u64<E: de::Error>(self, n: u64) -> Result<Self::Value, E> {
                if n <= i64::MAX as u64 {
                    self.visit_i64(n.try_into().unwrap())
                } else {
                    // Browser's `JSON.stringify()` serialize all numbers
                    // having no fractional part as integers (no decimal
                    // point), so we must parse large integers as floating
                    // point, otherwise we would error on transferring large
                    // floating point numbers.
                    Ok(AggroScalarValue::Float(n as f64))
                }
            }

            fn visit_f64<E: de::Error>(self, f: f64) -> Result<Self::Value, E> {
                Ok(AggroScalarValue::Float(f))
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
                self.visit_string(s.into())
            }

            fn visit_string<E: de::Error>(self, s: String) -> Result<Self::Value, E> {
                Ok(AggroScalarValue::String(s))
            }
        }

        de.deserialize_any(Visitor)
    }
}

// #[derive(Debug, PartialEq, Clone)]
// #[allow(missing_docs)]
// pub enum AggroScalarValue {
//     Int(i32),
//     Float(f64),
//     String(String),
//     Boolean(bool),
//     Generic(Box<AggroValue>),
// }
//
// impl<'de> Deserialize for AggroScalarValue {
//     fn deserialize<D>(deserializer: D) -> Result<Self, serde::de::Error> where D: Deserializer<'de> {
//         let Some(s) = i32::deserialize(deserializer) {
//             return AggroScalarValue::Int(s)
//         }
//     }
// }

#[derive(Debug, PartialEq, Clone)]
pub struct AggroSqlValue(JuniperValue<AggroScalarValue>);

impl AggroSqlValue {
    pub fn new(value: JuniperValue<AggroScalarValue>) -> AggroSqlValue {
        AggroSqlValue(value)
    }
}

impl ToSql for AggroSqlValue {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>
    where
        Self: Sized,
    {
        match &self.0 {
            JuniperValue::List(i) => {
                let mut scalar_vec = Vec::new();
                for v in i {
                    match v {
                        JuniperValue::Scalar(s) => scalar_vec.push(s),
                        _ => {
                            return Err(
                                anyhow!("Only one dimensional arrays supported to sql").into()
                            )
                        }
                    }
                }
                scalar_vec.to_sql(ty, out)
            }
            JuniperValue::Object(_i) => return Err(anyhow!("Can't convert objects to sql").into()),
            JuniperValue::Scalar(i) => i.to_sql(ty, out),
            JuniperValue::Null => (None as Option<i32>).to_sql(ty, out),
        }
    }
    fn accepts(ty: &Type) -> bool
    where
        Self: Sized,
    {
        match *ty {
            Type::FLOAT8
            | Type::FLOAT4
            | Type::INT4
            | Type::INT2
            | Type::INT8
            | Type::TEXT
            | Type::VARCHAR
            | Type::BOOL => true,
            Type::FLOAT8_ARRAY
            | Type::FLOAT4_ARRAY
            | Type::INT4_ARRAY
            | Type::INT2_ARRAY
            | Type::INT8_ARRAY
            | Type::TEXT_ARRAY
            | Type::VARCHAR_ARRAY
            | Type::BOOL_ARRAY => true,
            _ => false,
        }
    }

    to_sql_checked!();
}

impl ToSql for AggroScalarValue {
    fn to_sql(&self, ty: &Type, out: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>>
    where
        Self: Sized,
    {
        match self {
            AggroScalarValue::Int(i) => i.to_sql(ty, out),
            AggroScalarValue::Float(i) => i.to_sql(ty, out),
            AggroScalarValue::String(i) => i.to_sql(ty, out),
            AggroScalarValue::Boolean(i) => i.to_sql(ty, out),
            AggroScalarValue::Generic(_i) => Err(anyhow!("Cannot convert generic to sql").into()),
        }
    }
    fn accepts(ty: &Type) -> bool
    where
        Self: Sized,
    {
        match *ty {
            Type::FLOAT8
            | Type::FLOAT4
            | Type::INT4
            | Type::INT2
            | Type::INT8
            | Type::TEXT
            | Type::VARCHAR
            | Type::BOOL => true,
            _ => false,
        }
    }

    to_sql_checked!();
}

impl Hash for AggroScalarValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            AggroScalarValue::Int(i) => i.hash(state),
            AggroScalarValue::String(i) => i.hash(state),
            AggroScalarValue::Boolean(i) => i.hash(state),
            v => {
                panic!("Tried to hash {:?}", v)
            }
        }
    }
}

impl Eq for AggroScalarValue {}

impl Display for AggroScalarValue {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            AggroScalarValue::Int(i) => i.fmt(f),
            AggroScalarValue::Float(i) => i.fmt(f),
            AggroScalarValue::String(i) => i.fmt(f),
            AggroScalarValue::Boolean(i) => i.fmt(f),
            AggroScalarValue::Generic(i) => i.fmt(f),
        }
    }
}

impl FromInputValue<AggroScalarValue> for AggroScalarValue {
    type Error = ();

    fn from_input_value(v: &InputValue<AggroScalarValue>) -> Result<Self, ()> {
        match v {
            InputValue::Scalar(s) => Ok(s.clone()),
            InputValue::Object(_s) => {
                Ok(AggroScalarValue::Generic(Box::new(convert_from_input(v))))
            }
            _ => Err(()),
        }
    }
}

impl Serialize for AggroScalarValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            AggroScalarValue::Float(s) => s.serialize(serializer),
            AggroScalarValue::Int(s) => s.serialize(serializer),
            AggroScalarValue::String(s) => s.serialize(serializer),
            AggroScalarValue::Boolean(s) => s.serialize(serializer),
            AggroScalarValue::Generic(s) => s.serialize(serializer),
        }
    }
}

impl From<String> for AggroScalarValue {
    fn from(s: String) -> Self {
        AggroScalarValue::String(s)
    }
}

impl From<bool> for AggroScalarValue {
    fn from(b: bool) -> Self {
        AggroScalarValue::Boolean(b)
    }
}

impl From<i32> for AggroScalarValue {
    fn from(v: i32) -> Self {
        AggroScalarValue::Int(v)
    }
}

impl From<f64> for AggroScalarValue {
    fn from(v: f64) -> Self {
        AggroScalarValue::Float(v)
    }
}

impl ScalarValue for AggroScalarValue {
    // type Visitor = AggroScalarValueVisitor;

    fn as_int(&self) -> Option<i32> {
        match *self {
            Self::Int(ref i) => Some(*i),
            _ => None,
        }
    }

    fn as_string(&self) -> Option<String> {
        match *self {
            Self::String(ref s) => Some(s.clone()),
            _ => None,
        }
    }

    fn into_string(self) -> Option<String> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match *self {
            Self::String(ref s) => Some(s.as_str()),
            _ => None,
        }
    }

    fn as_float(&self) -> Option<f64> {
        match *self {
            Self::Int(ref i) => Some(*i as f64),
            Self::Float(ref f) => Some(*f),
            _ => None,
        }
    }

    fn as_bool(&self) -> Option<bool> {
        match *self {
            Self::Boolean(ref b) => Some(*b),
            _ => None,
        }
    }

    fn into_another<S: ScalarValue>(self) -> S {
        match self {
            Self::Int(i) => S::from(i),
            Self::Float(f) => S::from(f),
            Self::String(s) => S::from(s),
            Self::Boolean(b) => S::from(b),
            Self::Generic(_b) => panic!("Cannot convert generic into another"),
        }
    }
}

impl<'a> From<&'a str> for AggroScalarValue {
    fn from(s: &'a str) -> Self {
        Self::String(s.into())
    }
}

impl<'a> From<&'a AggroValue> for AggroScalarValue {
    fn from(s: &'a AggroValue) -> Self {
        Self::Generic(Box::new(s.clone()))
    }
}

#[derive(Default, Clone, Copy, Debug)]
pub struct AggroScalarValueVisitor;

impl<'de> de::Visitor<'de> for AggroScalarValueVisitor {
    type Value = AggroScalarValue;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a valid input value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<AggroScalarValue, E> {
        Ok(AggroScalarValue::Boolean(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<AggroScalarValue, E>
    where
        E: de::Error,
    {
        if value >= i64::from(i32::MIN) && value <= i64::from(i32::MAX) {
            Ok(AggroScalarValue::Int(value as i32))
        } else {
            // Browser's JSON.stringify serialize all numbers having no
            // fractional part as integers (no decimal point), so we
            // must parse large integers as floating point otherwise
            // we would error on transferring large floating point
            // numbers.
            Ok(AggroScalarValue::Float(value as f64))
        }
    }

    fn visit_u64<E>(self, value: u64) -> Result<AggroScalarValue, E>
    where
        E: de::Error,
    {
        if value <= i32::MAX as u64 {
            self.visit_i64(value as i64)
        } else {
            // Browser's JSON.stringify serialize all numbers having no
            // fractional part as integers (no decimal point), so we
            // must parse large integers as floating point otherwise
            // we would error on transferring large floating point
            // numbers.
            Ok(AggroScalarValue::Float(value as f64))
        }
    }

    fn visit_f64<E>(self, value: f64) -> Result<AggroScalarValue, E> {
        Ok(AggroScalarValue::Float(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<AggroScalarValue, E>
    where
        E: de::Error,
    {
        self.visit_string(value.into())
    }

    fn visit_string<E>(self, value: String) -> Result<AggroScalarValue, E> {
        Ok(AggroScalarValue::String(value))
    }
}
