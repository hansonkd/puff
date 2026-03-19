//! Compiled query cache — skip parsing/validation for repeated queries.
//!
//! For agents that repeat the same queries (which they do — agents are
//! formulaic), this avoids redundant parse/validate/execute work by caching
//! successful responses keyed on (query, operation_name, variables).
//!
//! Mutations are never cached. Cached results have a short TTL (5 s by
//! default) so data freshness is preserved.  Call [`QueryCache::invalidate`]
//! after mutations to eagerly flush stale results.

use async_graphql::dynamic::Schema;
use async_graphql::Response;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// Cache key: hash of (query_string, operation_name).
fn query_hash(query: &str, operation: Option<&str>) -> u64 {
    let mut hasher = DefaultHasher::new();
    query.hash(&mut hasher);
    operation.hash(&mut hasher);
    hasher.finish()
}

/// Hash the variables portion of a request.
///
/// `async_graphql::Variables` is a wrapper around `Value`.  We hash the
/// debug representation — not ideal for production hot-paths, but perfectly
/// adequate for a cache key where collisions just cause a re-execute.
fn variables_hash(vars: &async_graphql::Variables) -> u64 {
    let mut hasher = DefaultHasher::new();
    format!("{:?}", vars).hash(&mut hasher);
    hasher.finish()
}

/// A single cached response — stored as pre-serialized JSON bytes so we
/// can serve them without re-serializing and without requiring `Response:
/// Clone`.
struct CachedResponse {
    /// Pre-serialized JSON bytes of the response.
    bytes: Vec<u8>,
    cached_at: Instant,
    ttl: Duration,
}

/// Per-query cache entry: one entry per unique (query, operation_name) pair,
/// with sub-entries keyed on the variables hash.
struct CachedEntry {
    results: HashMap<u64, CachedResponse>,
}

/// Compiled query cache — skip parsing/validation for repeated queries.
pub struct QueryCache {
    cache: RwLock<HashMap<u64, CachedEntry>>,
    max_entries: usize,
    enabled: bool,
    ttl: Duration,
}

impl QueryCache {
    /// Create a new cache that holds up to `max_entries` unique queries.
    pub fn new(max_entries: usize) -> Self {
        Self {
            cache: RwLock::new(HashMap::with_capacity(max_entries.min(256))),
            max_entries,
            enabled: true,
            ttl: Duration::from_secs(5),
        }
    }

    /// Create a disabled (no-op) cache.
    pub fn disabled() -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            max_entries: 0,
            enabled: false,
            ttl: Duration::ZERO,
        }
    }

    /// Execute a query, using the cache when possible.
    ///
    /// * Mutations are always executed fresh (never cached).
    /// * Queries check the result cache first; on miss, execute and cache the
    ///   result (only when the response has no errors).
    ///
    /// Returns `(response_bytes, response)`.  The caller can use
    /// `response_bytes` directly for fast serialization and `response` for
    /// anything else (e.g. extracting http_headers).
    pub async fn execute(
        &self,
        schema: &Schema,
        request: async_graphql::Request,
    ) -> Response {
        if !self.enabled {
            return schema.execute(request).await;
        }

        let query_str = &request.query;

        // Never cache mutations.
        if query_str.trim_start().starts_with("mutation") {
            let response = schema.execute(request).await;
            // Invalidate on mutation so that subsequent reads see fresh data.
            self.invalidate();
            return response;
        }

        let qhash = query_hash(query_str, request.operation_name.as_deref());
        let vhash = variables_hash(&request.variables);

        // Fast path: check the result cache (read lock).
        {
            if let Ok(cache) = self.cache.read() {
                if let Some(entry) = cache.get(&qhash) {
                    if let Some(cached) = entry.results.get(&vhash) {
                        if cached.cached_at.elapsed() < cached.ttl {
                            // Deserialize the cached bytes back into a Response.
                            if let Ok(response) =
                                serde_json::from_slice::<Response>(&cached.bytes)
                            {
                                return response;
                            }
                        }
                    }
                }
            }
        }

        // Cache miss — execute the query.
        let response = schema.execute(request).await;

        // Cache the result if no errors.
        if response.errors.is_empty() {
            if let Ok(bytes) = serde_json::to_vec(&response) {
                if let Ok(mut cache) = self.cache.write() {
                    // Simple eviction: if we've exceeded the budget, clear all.
                    if cache.len() >= self.max_entries {
                        cache.clear();
                    }
                    let entry = cache.entry(qhash).or_insert_with(|| CachedEntry {
                        results: HashMap::new(),
                    });
                    entry.results.insert(
                        vhash,
                        CachedResponse {
                            bytes,
                            cached_at: Instant::now(),
                            ttl: self.ttl,
                        },
                    );
                }
            }
        }

        response
    }

    /// Execute and return pre-serialized bytes directly (for the fast
    /// serialization path).  Returns `(bytes, http_headers)`.
    pub async fn execute_to_bytes(
        &self,
        schema: &Schema,
        request: async_graphql::Request,
    ) -> (Vec<u8>, http::HeaderMap) {
        if !self.enabled {
            let response = schema.execute(request).await;
            let headers = response.http_headers.clone();
            let bytes =
                serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
            return (bytes, headers);
        }

        let query_str = &request.query;

        // Never cache mutations.
        if query_str.trim_start().starts_with("mutation") {
            let response = schema.execute(request).await;
            self.invalidate();
            let headers = response.http_headers.clone();
            let bytes =
                serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
            return (bytes, headers);
        }

        let qhash = query_hash(query_str, request.operation_name.as_deref());
        let vhash = variables_hash(&request.variables);

        // Fast path: check the result cache (read lock).
        {
            if let Ok(cache) = self.cache.read() {
                if let Some(entry) = cache.get(&qhash) {
                    if let Some(cached) = entry.results.get(&vhash) {
                        if cached.cached_at.elapsed() < cached.ttl {
                            return (cached.bytes.clone(), http::HeaderMap::new());
                        }
                    }
                }
            }
        }

        // Cache miss — execute.
        let response = schema.execute(request).await;
        let headers = response.http_headers.clone();
        let bytes =
            serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());

        // Cache on success.
        if response.errors.is_empty() {
            if let Ok(mut cache) = self.cache.write() {
                if cache.len() >= self.max_entries {
                    cache.clear();
                }
                let entry = cache.entry(qhash).or_insert_with(|| CachedEntry {
                    results: HashMap::new(),
                });
                entry.results.insert(
                    vhash,
                    CachedResponse {
                        bytes: bytes.clone(),
                        cached_at: Instant::now(),
                        ttl: self.ttl,
                    },
                );
            }
        }

        (bytes, headers)
    }

    /// Invalidate all cached results (call after mutations).
    pub fn invalidate(&self) {
        if self.enabled {
            if let Ok(mut cache) = self.cache.write() {
                for entry in cache.values_mut() {
                    entry.results.clear();
                }
            }
        }
    }

    /// Clear the entire cache.
    pub fn clear(&self) {
        if self.enabled {
            if let Ok(mut cache) = self.cache.write() {
                cache.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_hash_deterministic() {
        let h1 = query_hash("{ users { id } }", None);
        let h2 = query_hash("{ users { id } }", None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_query_hash_differs_for_different_queries() {
        let h1 = query_hash("{ users { id } }", None);
        let h2 = query_hash("{ posts { id } }", None);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_query_hash_differs_with_operation_name() {
        let h1 = query_hash("query A { users { id } }", Some("A"));
        let h2 = query_hash("query A { users { id } }", Some("B"));
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_disabled_cache() {
        let cache = QueryCache::disabled();
        assert!(!cache.enabled);
        assert_eq!(cache.max_entries, 0);
    }

    #[test]
    fn test_new_cache() {
        let cache = QueryCache::new(100);
        assert!(cache.enabled);
        assert_eq!(cache.max_entries, 100);
    }

    #[test]
    fn test_invalidate_and_clear_do_not_panic_on_disabled() {
        let cache = QueryCache::disabled();
        cache.invalidate();
        cache.clear();
    }

    #[test]
    fn test_invalidate_and_clear_do_not_panic_on_enabled() {
        let cache = QueryCache::new(10);
        cache.invalidate();
        cache.clear();
    }
}
