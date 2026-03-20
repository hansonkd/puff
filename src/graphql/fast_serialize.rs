//! Fast GraphQL response serialization.
//!
//! Instead of async-graphql's default serde serialization path
//! (Response -> GraphQLResponse -> axum Json -> serde_json::to_string -> bytes),
//! write directly to a byte buffer using `serde_json::to_vec` / `to_writer`,
//! which skip the intermediate `String` allocation that axum's `Json`
//! extractor uses.

use async_graphql::Response;

/// Serialize a GraphQL response directly to bytes, skipping intermediate
/// `String`.
///
/// `serde_json::to_vec` writes directly to `Vec<u8>` without going through
/// `String`.  The key optimization is avoiding the
/// `Response -> String -> bytes` path that axum's `Json` extractor uses.
pub fn serialize_response_to_bytes(response: &Response) -> Vec<u8> {
    serde_json::to_vec(response).unwrap_or_else(|_| b"{}".to_vec())
}

/// Serialize a response with a pre-allocated buffer based on estimated size.
///
/// Returns raw bytes suitable for writing directly into an HTTP response body.
pub fn serialize_response_fast(response: &Response) -> Vec<u8> {
    let estimated_size = estimate_response_size(response);
    let mut buf = Vec::with_capacity(estimated_size);
    serde_json::to_writer(&mut buf, response).unwrap_or_else(|_| {
        buf.clear();
        buf.extend_from_slice(b"{}");
    });
    buf
}

/// Build an `axum::response::Response` with pre-serialized bytes.
///
/// This bypasses axum's `Json` wrapper and its internal serialization.
pub fn bytes_to_response(
    bytes: Vec<u8>,
    extra_headers: http::HeaderMap,
) -> axum::response::Response {
    let mut builder = axum::http::Response::builder().header("content-type", "application/json");

    // Forward any headers the GraphQL response set.
    if let Some(headers) = builder.headers_mut() {
        headers.extend(extra_headers);
    }

    builder
        .body(axum::body::Body::from(bytes))
        .unwrap_or_else(|_| {
            axum::http::Response::builder()
                .status(500)
                .body(axum::body::Body::from("Internal error"))
                .unwrap()
        })
}

fn estimate_response_size(response: &Response) -> usize {
    // Rough estimate: most GraphQL responses are 256B-4KB.
    // For lists, estimate ~100 bytes per item.
    match &response.data {
        async_graphql::Value::Object(obj) => {
            let mut size = 64; // base overhead
            for (_, val) in obj.iter() {
                size += estimate_value_size(val);
            }
            size
        }
        _ => 256,
    }
}

fn estimate_value_size(val: &async_graphql::Value) -> usize {
    match val {
        async_graphql::Value::Null => 4,
        async_graphql::Value::Boolean(_) => 5,
        async_graphql::Value::Number(_) => 10,
        async_graphql::Value::String(s) => s.len() + 2,
        async_graphql::Value::List(items) => {
            items.iter().map(estimate_value_size).sum::<usize>() + items.len() * 2
        }
        async_graphql::Value::Object(obj) => {
            obj.iter()
                .map(|(k, v)| k.len() + estimate_value_size(v) + 4)
                .sum::<usize>()
                + 2
        }
        _ => 16,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::Value;

    #[test]
    fn test_serialize_empty_response() {
        let response = Response::default();
        let bytes = serialize_response_to_bytes(&response);
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.contains("data"));
    }

    #[test]
    fn test_serialize_response_fast_matches_regular() {
        let response = Response::new(Value::Boolean(true));
        let regular = serialize_response_to_bytes(&response);
        let fast = serialize_response_fast(&response);
        assert_eq!(regular, fast);
    }

    #[test]
    fn test_estimate_response_size_object() {
        let response = Response::new(Value::Object(Default::default()));
        let size = estimate_response_size(&response);
        assert!(size >= 64);
    }

    #[test]
    fn test_estimate_response_size_null() {
        let response = Response::new(Value::Null);
        let size = estimate_response_size(&response);
        assert_eq!(size, 256);
    }

    #[test]
    fn test_bytes_to_response_has_json_content_type() {
        let bytes = b"{\"data\":null}".to_vec();
        let resp = bytes_to_response(bytes, http::HeaderMap::new());
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }
}
