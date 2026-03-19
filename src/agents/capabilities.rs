use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;

use crate::agents::error::AgentError;

// ---------------------------------------------------------------------------
// AgentCapabilities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct AgentCapabilities {
    #[serde(default = "default_sql")]
    pub sql: SqlCapability,
    #[serde(default = "default_http")]
    pub http: HttpCapability,
    #[serde(default = "default_fs")]
    pub filesystem: FsCapability,
    #[serde(default = "default_tools")]
    pub tools: ToolCapability,
    #[serde(default = "default_agents")]
    pub agents: AgentCapability,
    #[serde(default)]
    pub budget: BudgetCapability,
}

impl Default for AgentCapabilities {
    fn default() -> Self {
        AgentCapabilities {
            sql: default_sql(),
            http: default_http(),
            filesystem: default_fs(),
            tools: default_tools(),
            agents: default_agents(),
            budget: BudgetCapability::default(),
        }
    }
}

fn default_sql() -> SqlCapability {
    SqlCapability::ReadWrite
}

fn default_http() -> HttpCapability {
    HttpCapability::Any
}

fn default_fs() -> FsCapability {
    FsCapability::None
}

fn default_tools() -> ToolCapability {
    ToolCapability::All
}

fn default_agents() -> AgentCapability {
    AgentCapability::All
}

// ---------------------------------------------------------------------------
// SqlCapability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum SqlCapability {
    None,
    ReadOnly,
    ReadWrite,
}

impl Default for SqlCapability {
    fn default() -> Self {
        SqlCapability::ReadWrite
    }
}

impl<'de> Deserialize<'de> for SqlCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) if s == "none" => Ok(SqlCapability::None),
            serde_json::Value::String(s) if s == "read_only" => Ok(SqlCapability::ReadOnly),
            serde_json::Value::String(s) if s == "read_write" => Ok(SqlCapability::ReadWrite),
            _ => Err(serde::de::Error::custom(
                "expected 'none', 'read_only', or 'read_write'",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// HttpCapability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum HttpCapability {
    None,
    Allowlist(Vec<String>),
    Any,
}

impl Default for HttpCapability {
    fn default() -> Self {
        HttpCapability::Any
    }
}

impl<'de> Deserialize<'de> for HttpCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) if s == "none" => Ok(HttpCapability::None),
            serde_json::Value::String(s) if s == "any" => Ok(HttpCapability::Any),
            serde_json::Value::Array(arr) => {
                let v: Vec<String> = arr
                    .into_iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect();
                Ok(HttpCapability::Allowlist(v))
            }
            _ => Err(serde::de::Error::custom(
                "expected 'none', 'any', or array of strings",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// FsCapability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum FsCapability {
    None,
    ReadOnly { read_only: Vec<PathBuf> },
    ReadWrite { read_write: Vec<PathBuf> },
}

impl Default for FsCapability {
    fn default() -> Self {
        FsCapability::None
    }
}

impl<'de> Deserialize<'de> for FsCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) if s == "none" => Ok(FsCapability::None),
            serde_json::Value::Object(ref map) if map.contains_key("read_only") => {
                let arr = map["read_only"]
                    .as_array()
                    .ok_or_else(|| serde::de::Error::custom("read_only must be an array"))?;
                let paths: Vec<PathBuf> = arr
                    .iter()
                    .map(|v| PathBuf::from(v.as_str().unwrap_or("")))
                    .collect();
                Ok(FsCapability::ReadOnly { read_only: paths })
            }
            serde_json::Value::Object(ref map) if map.contains_key("read_write") => {
                let arr = map["read_write"]
                    .as_array()
                    .ok_or_else(|| serde::de::Error::custom("read_write must be an array"))?;
                let paths: Vec<PathBuf> = arr
                    .iter()
                    .map(|v| PathBuf::from(v.as_str().unwrap_or("")))
                    .collect();
                Ok(FsCapability::ReadWrite { read_write: paths })
            }
            _ => Err(serde::de::Error::custom(
                "expected 'none', {read_only: [...]}, or {read_write: [...]}",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// ToolCapability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ToolCapability {
    None,
    Specific(Vec<String>),
    All,
}

impl Default for ToolCapability {
    fn default() -> Self {
        ToolCapability::All
    }
}

impl<'de> Deserialize<'de> for ToolCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) if s == "none" => Ok(ToolCapability::None),
            serde_json::Value::String(s) if s == "all" => Ok(ToolCapability::All),
            serde_json::Value::Array(arr) => {
                let v: Vec<String> = arr
                    .into_iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect();
                Ok(ToolCapability::Specific(v))
            }
            _ => Err(serde::de::Error::custom(
                "expected 'none', 'all', or array of strings",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// AgentCapability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum AgentCapability {
    None,
    Specific(Vec<String>),
    All,
}

impl Default for AgentCapability {
    fn default() -> Self {
        AgentCapability::All
    }
}

impl<'de> Deserialize<'de> for AgentCapability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        match value {
            serde_json::Value::String(s) if s == "none" => Ok(AgentCapability::None),
            serde_json::Value::String(s) if s == "all" => Ok(AgentCapability::All),
            serde_json::Value::Array(arr) => {
                let v: Vec<String> = arr
                    .into_iter()
                    .map(|v| v.as_str().unwrap_or("").to_string())
                    .collect();
                Ok(AgentCapability::Specific(v))
            }
            _ => Err(serde::de::Error::custom(
                "expected 'none', 'all', or array of strings",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// BudgetCapability
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Default)]
pub struct BudgetCapability {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub max_tool_calls: Option<u32>,
    pub max_tool_cpu_seconds: Option<f64>,
}

// ---------------------------------------------------------------------------
// Check methods
// ---------------------------------------------------------------------------

impl AgentCapabilities {
    pub fn check_sql(&self, is_write: bool) -> Result<(), AgentError> {
        match &self.sql {
            SqlCapability::ReadWrite => Ok(()),
            SqlCapability::ReadOnly if !is_write => Ok(()),
            SqlCapability::ReadOnly => Err(AgentError::ToolPermissionDenied {
                tool: "sql".into(),
                reason: "Write operations not allowed (read_only)".into(),
            }),
            SqlCapability::None => Err(AgentError::ToolPermissionDenied {
                tool: "sql".into(),
                reason: "SQL access not allowed".into(),
            }),
        }
    }

    pub fn check_http(&self, url: &str) -> Result<(), AgentError> {
        match &self.http {
            HttpCapability::Any => Ok(()),
            HttpCapability::Allowlist(patterns) => {
                if patterns.iter().any(|p| url.contains(p.as_str())) {
                    Ok(())
                } else {
                    Err(AgentError::ToolPermissionDenied {
                        tool: "http".into(),
                        reason: format!("URL {} not in HTTP allowlist", url),
                    })
                }
            }
            HttpCapability::None => Err(AgentError::ToolPermissionDenied {
                tool: "http".into(),
                reason: "HTTP access not allowed".into(),
            }),
        }
    }

    pub fn check_tool(&self, tool_name: &str) -> Result<(), AgentError> {
        match &self.tools {
            ToolCapability::All => Ok(()),
            ToolCapability::Specific(names) => {
                if names.iter().any(|n| n == tool_name) {
                    Ok(())
                } else {
                    Err(AgentError::ToolPermissionDenied {
                        tool: tool_name.into(),
                        reason: format!("Tool '{}' not in allowed tool list", tool_name),
                    })
                }
            }
            ToolCapability::None => Err(AgentError::ToolPermissionDenied {
                tool: tool_name.into(),
                reason: "Tool execution not allowed".into(),
            }),
        }
    }

    pub fn check_agent(&self, agent_name: &str) -> Result<(), AgentError> {
        match &self.agents {
            AgentCapability::All => Ok(()),
            AgentCapability::Specific(names) => {
                if names.iter().any(|n| n == agent_name) {
                    Ok(())
                } else {
                    Err(AgentError::ToolPermissionDenied {
                        tool: "agent".into(),
                        reason: format!("Agent '{}' not in allowed agent list", agent_name),
                    })
                }
            }
            AgentCapability::None => Err(AgentError::ToolPermissionDenied {
                tool: "agent".into(),
                reason: "Sub-agent invocation not allowed".into(),
            }),
        }
    }

    pub fn check_filesystem(&self, path: &Path, write: bool) -> Result<(), AgentError> {
        match &self.filesystem {
            FsCapability::None => Err(AgentError::ToolPermissionDenied {
                tool: "filesystem".into(),
                reason: "Filesystem access not allowed".into(),
            }),
            FsCapability::ReadOnly { read_only } => {
                if write {
                    return Err(AgentError::ToolPermissionDenied {
                        tool: "filesystem".into(),
                        reason: "Write operations not allowed (read_only)".into(),
                    });
                }
                if read_only.iter().any(|allowed| path.starts_with(allowed)) {
                    Ok(())
                } else {
                    Err(AgentError::ToolPermissionDenied {
                        tool: "filesystem".into(),
                        reason: format!(
                            "Path '{}' not under any allowed read_only directory",
                            path.display()
                        ),
                    })
                }
            }
            FsCapability::ReadWrite { read_write } => {
                if read_write.iter().any(|allowed| path.starts_with(allowed)) {
                    Ok(())
                } else {
                    Err(AgentError::ToolPermissionDenied {
                        tool: "filesystem".into(),
                        reason: format!(
                            "Path '{}' not under any allowed read_write directory",
                            path.display()
                        ),
                    })
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Defaults are permissive
    // -----------------------------------------------------------------------

    #[test]
    fn test_default_capabilities_are_permissive() {
        let caps = AgentCapabilities::default();

        // SQL: read and write both allowed
        assert!(caps.check_sql(false).is_ok());
        assert!(caps.check_sql(true).is_ok());

        // HTTP: any URL allowed
        assert!(caps.check_http("https://example.com/api").is_ok());
        assert!(caps.check_http("https://unknown-host.io/data").is_ok());

        // Tools: any tool allowed
        assert!(caps.check_tool("my_tool").is_ok());
        assert!(caps.check_tool("another_tool").is_ok());

        // Agents: any sub-agent allowed
        assert!(caps.check_agent("sub_agent").is_ok());

        // Filesystem: no access by default (safe default)
        assert!(caps
            .check_filesystem(Path::new("/tmp/file.txt"), false)
            .is_err());
    }

    // -----------------------------------------------------------------------
    // check_sql
    // -----------------------------------------------------------------------

    #[test]
    fn test_check_sql_read_only_allows_reads() {
        let mut caps = AgentCapabilities::default();
        caps.sql = SqlCapability::ReadOnly;
        assert!(caps.check_sql(false).is_ok());
    }

    #[test]
    fn test_check_sql_read_only_blocks_writes() {
        let mut caps = AgentCapabilities::default();
        caps.sql = SqlCapability::ReadOnly;
        let err = caps.check_sql(true).unwrap_err();
        assert!(matches!(err, AgentError::ToolPermissionDenied { .. }));
        assert!(err.to_string().contains("read_only"));
    }

    #[test]
    fn test_check_sql_none_blocks_everything() {
        let mut caps = AgentCapabilities::default();
        caps.sql = SqlCapability::None;
        assert!(caps.check_sql(false).is_err());
        assert!(caps.check_sql(true).is_err());
    }

    #[test]
    fn test_check_sql_read_write_allows_everything() {
        let caps = AgentCapabilities::default(); // default is ReadWrite
        assert!(caps.check_sql(false).is_ok());
        assert!(caps.check_sql(true).is_ok());
    }

    // -----------------------------------------------------------------------
    // check_http
    // -----------------------------------------------------------------------

    #[test]
    fn test_check_http_any_allows_all() {
        let caps = AgentCapabilities::default(); // default is Any
        assert!(caps.check_http("https://anything.com/path").is_ok());
    }

    #[test]
    fn test_check_http_none_blocks_all() {
        let mut caps = AgentCapabilities::default();
        caps.http = HttpCapability::None;
        assert!(caps.check_http("https://example.com").is_err());
    }

    #[test]
    fn test_check_http_allowlist_allows_listed_host() {
        let mut caps = AgentCapabilities::default();
        caps.http = HttpCapability::Allowlist(vec![
            "api.stripe.com".into(),
            "api.github.com".into(),
        ]);
        assert!(caps
            .check_http("https://api.stripe.com/v1/charges")
            .is_ok());
        assert!(caps
            .check_http("https://api.github.com/repos")
            .is_ok());
    }

    #[test]
    fn test_check_http_allowlist_blocks_unlisted_host() {
        let mut caps = AgentCapabilities::default();
        caps.http = HttpCapability::Allowlist(vec!["api.stripe.com".into()]);
        let err = caps
            .check_http("https://evil.example.com/steal")
            .unwrap_err();
        assert!(matches!(err, AgentError::ToolPermissionDenied { .. }));
        assert!(err.to_string().contains("not in HTTP allowlist"));
    }

    // -----------------------------------------------------------------------
    // check_tool
    // -----------------------------------------------------------------------

    #[test]
    fn test_check_tool_all_allows_any() {
        let caps = AgentCapabilities::default();
        assert!(caps.check_tool("read_file").is_ok());
        assert!(caps.check_tool("write_file").is_ok());
    }

    #[test]
    fn test_check_tool_none_blocks_all() {
        let mut caps = AgentCapabilities::default();
        caps.tools = ToolCapability::None;
        assert!(caps.check_tool("read_file").is_err());
    }

    #[test]
    fn test_check_tool_specific_allows_named() {
        let mut caps = AgentCapabilities::default();
        caps.tools = ToolCapability::Specific(vec!["read_file".into(), "list_dir".into()]);
        assert!(caps.check_tool("read_file").is_ok());
        assert!(caps.check_tool("list_dir").is_ok());
    }

    #[test]
    fn test_check_tool_specific_blocks_others() {
        let mut caps = AgentCapabilities::default();
        caps.tools = ToolCapability::Specific(vec!["read_file".into()]);
        let err = caps.check_tool("write_file").unwrap_err();
        assert!(matches!(err, AgentError::ToolPermissionDenied { .. }));
        assert!(err.to_string().contains("not in allowed tool list"));
    }

    // -----------------------------------------------------------------------
    // check_agent
    // -----------------------------------------------------------------------

    #[test]
    fn test_check_agent_all_allows_any() {
        let caps = AgentCapabilities::default();
        assert!(caps.check_agent("worker_agent").is_ok());
    }

    #[test]
    fn test_check_agent_none_blocks_all() {
        let mut caps = AgentCapabilities::default();
        caps.agents = AgentCapability::None;
        assert!(caps.check_agent("any_agent").is_err());
    }

    #[test]
    fn test_check_agent_specific_allows_and_blocks() {
        let mut caps = AgentCapabilities::default();
        caps.agents = AgentCapability::Specific(vec!["worker".into()]);
        assert!(caps.check_agent("worker").is_ok());
        let err = caps.check_agent("rogue_agent").unwrap_err();
        assert!(matches!(err, AgentError::ToolPermissionDenied { .. }));
    }

    // -----------------------------------------------------------------------
    // check_filesystem
    // -----------------------------------------------------------------------

    #[test]
    fn test_check_filesystem_none_blocks_all() {
        let caps = AgentCapabilities::default(); // default is None
        assert!(caps
            .check_filesystem(Path::new("/tmp/file.txt"), false)
            .is_err());
        assert!(caps
            .check_filesystem(Path::new("/tmp/file.txt"), true)
            .is_err());
    }

    #[test]
    fn test_check_filesystem_read_only_allows_reads_under_allowed_path() {
        let mut caps = AgentCapabilities::default();
        caps.filesystem = FsCapability::ReadOnly {
            read_only: vec![PathBuf::from("/data/reports")],
        };
        assert!(caps
            .check_filesystem(Path::new("/data/reports/q1.csv"), false)
            .is_ok());
    }

    #[test]
    fn test_check_filesystem_read_only_blocks_writes() {
        let mut caps = AgentCapabilities::default();
        caps.filesystem = FsCapability::ReadOnly {
            read_only: vec![PathBuf::from("/data/reports")],
        };
        assert!(caps
            .check_filesystem(Path::new("/data/reports/q1.csv"), true)
            .is_err());
    }

    #[test]
    fn test_check_filesystem_read_only_blocks_path_outside_allowed() {
        let mut caps = AgentCapabilities::default();
        caps.filesystem = FsCapability::ReadOnly {
            read_only: vec![PathBuf::from("/data/reports")],
        };
        assert!(caps
            .check_filesystem(Path::new("/etc/passwd"), false)
            .is_err());
    }

    #[test]
    fn test_check_filesystem_read_write_allows_read_and_write_under_path() {
        let mut caps = AgentCapabilities::default();
        caps.filesystem = FsCapability::ReadWrite {
            read_write: vec![PathBuf::from("/workspace")],
        };
        assert!(caps
            .check_filesystem(Path::new("/workspace/output.txt"), false)
            .is_ok());
        assert!(caps
            .check_filesystem(Path::new("/workspace/output.txt"), true)
            .is_ok());
    }

    #[test]
    fn test_check_filesystem_read_write_blocks_path_outside_allowed() {
        let mut caps = AgentCapabilities::default();
        caps.filesystem = FsCapability::ReadWrite {
            read_write: vec![PathBuf::from("/workspace")],
        };
        assert!(caps
            .check_filesystem(Path::new("/etc/hosts"), true)
            .is_err());
    }

    // -----------------------------------------------------------------------
    // Deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn test_deserialize_sql_capability() {
        let none: SqlCapability = serde_json::from_str("\"none\"").unwrap();
        let ro: SqlCapability = serde_json::from_str("\"read_only\"").unwrap();
        let rw: SqlCapability = serde_json::from_str("\"read_write\"").unwrap();
        assert_eq!(none, SqlCapability::None);
        assert_eq!(ro, SqlCapability::ReadOnly);
        assert_eq!(rw, SqlCapability::ReadWrite);
    }

    #[test]
    fn test_deserialize_http_capability() {
        let none: HttpCapability = serde_json::from_str("\"none\"").unwrap();
        let any: HttpCapability = serde_json::from_str("\"any\"").unwrap();
        let list: HttpCapability =
            serde_json::from_str("[\"api.stripe.com\", \"api.github.com\"]").unwrap();
        assert_eq!(none, HttpCapability::None);
        assert_eq!(any, HttpCapability::Any);
        assert_eq!(
            list,
            HttpCapability::Allowlist(vec![
                "api.stripe.com".into(),
                "api.github.com".into()
            ])
        );
    }

    #[test]
    fn test_deserialize_tool_capability() {
        let none: ToolCapability = serde_json::from_str("\"none\"").unwrap();
        let all: ToolCapability = serde_json::from_str("\"all\"").unwrap();
        let specific: ToolCapability =
            serde_json::from_str("[\"read_file\", \"list_dir\"]").unwrap();
        assert_eq!(none, ToolCapability::None);
        assert_eq!(all, ToolCapability::All);
        assert_eq!(
            specific,
            ToolCapability::Specific(vec!["read_file".into(), "list_dir".into()])
        );
    }

    #[test]
    fn test_deserialize_agent_capability() {
        let none: AgentCapability = serde_json::from_str("\"none\"").unwrap();
        let all: AgentCapability = serde_json::from_str("\"all\"").unwrap();
        let specific: AgentCapability =
            serde_json::from_str("[\"worker\", \"summarizer\"]").unwrap();
        assert_eq!(none, AgentCapability::None);
        assert_eq!(all, AgentCapability::All);
        assert_eq!(
            specific,
            AgentCapability::Specific(vec!["worker".into(), "summarizer".into()])
        );
    }

    #[test]
    fn test_deserialize_fs_capability() {
        let none: FsCapability = serde_json::from_str("\"none\"").unwrap();
        let ro: FsCapability =
            serde_json::from_str("{\"read_only\": [\"/data\", \"/tmp\"]}").unwrap();
        let rw: FsCapability =
            serde_json::from_str("{\"read_write\": [\"/workspace\"]}").unwrap();
        assert_eq!(none, FsCapability::None);
        assert_eq!(
            ro,
            FsCapability::ReadOnly {
                read_only: vec![PathBuf::from("/data"), PathBuf::from("/tmp")]
            }
        );
        assert_eq!(
            rw,
            FsCapability::ReadWrite {
                read_write: vec![PathBuf::from("/workspace")]
            }
        );
    }

    #[test]
    fn test_deserialize_full_agent_capabilities() {
        let json = serde_json::json!({
            "sql": "read_only",
            "http": ["api.stripe.com"],
            "filesystem": {"read_write": ["/workspace"]},
            "tools": ["read_file", "write_file"],
            "agents": "none",
            "budget": {
                "max_tool_calls": 50,
                "max_cost_usd": 1.0
            }
        });

        let caps: AgentCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps.sql, SqlCapability::ReadOnly);
        assert_eq!(
            caps.http,
            HttpCapability::Allowlist(vec!["api.stripe.com".into()])
        );
        assert_eq!(
            caps.filesystem,
            FsCapability::ReadWrite {
                read_write: vec![PathBuf::from("/workspace")]
            }
        );
        assert_eq!(
            caps.tools,
            ToolCapability::Specific(vec!["read_file".into(), "write_file".into()])
        );
        assert_eq!(caps.agents, AgentCapability::None);
        assert_eq!(caps.budget.max_tool_calls, Some(50));
        assert_eq!(caps.budget.max_cost_usd, Some(1.0));
    }

    #[test]
    fn test_deserialize_defaults_when_fields_absent() {
        let caps: AgentCapabilities = serde_json::from_str("{}").unwrap();
        assert_eq!(caps.sql, SqlCapability::ReadWrite);
        assert_eq!(caps.http, HttpCapability::Any);
        assert_eq!(caps.filesystem, FsCapability::None);
        assert_eq!(caps.tools, ToolCapability::All);
        assert_eq!(caps.agents, AgentCapability::All);
    }
}
