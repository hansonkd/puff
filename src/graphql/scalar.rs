//! Custom scalar types and value conversion helpers for async-graphql dynamic schema.

use async_graphql::dynamic::*;
use async_graphql::Value as GqlValue;
use base64::Engine;
use chrono::{DateTime, Utc};

/// Type alias used throughout the graphql module.
pub type AggroValue = GqlValue;

/// Register custom scalars (JSON, Binary, Long, DateTime) into the schema builder.
pub fn register_scalars(schema: SchemaBuilder) -> SchemaBuilder {
    schema
        .register(create_json_scalar())
        .register(create_binary_scalar())
        .register(create_long_scalar())
        .register(create_datetime_scalar())
}

fn create_json_scalar() -> Scalar {
    Scalar::new("GenericScalar")
        .description("An opaque representation of raw input (JSON scalar)")
}

fn create_binary_scalar() -> Scalar {
    Scalar::new("Binary")
        .description("Binary data (base64 encoded string)")
}

fn create_long_scalar() -> Scalar {
    Scalar::new("Long")
        .description("A 64-bit integer")
}

fn create_datetime_scalar() -> Scalar {
    Scalar::new("DateTime")
        .description("An ISO 8601 datetime string")
}

// ---------------------------------------------------------------------------
// Helpers to create async_graphql::Value from Rust types
// ---------------------------------------------------------------------------

pub fn gql_string(s: impl Into<String>) -> GqlValue {
    GqlValue::String(s.into())
}

pub fn gql_int(i: i32) -> GqlValue {
    GqlValue::Number(i.into())
}

pub fn gql_long(i: i64) -> GqlValue {
    GqlValue::Number(i.into())
}

pub fn gql_float(f: f64) -> GqlValue {
    match serde_json::Number::from_f64(f) {
        Some(n) => GqlValue::Number(n),
        None => GqlValue::Null,
    }
}

pub fn gql_bool(b: bool) -> GqlValue {
    GqlValue::Boolean(b)
}

pub fn gql_binary(bytes: &[u8]) -> GqlValue {
    GqlValue::String(base64::engine::general_purpose::STANDARD.encode(bytes))
}

pub fn gql_datetime(dt: &DateTime<Utc>) -> GqlValue {
    GqlValue::String(dt.to_rfc3339())
}

pub fn gql_uuid(u: &uuid::Uuid) -> GqlValue {
    GqlValue::String(u.to_string())
}
