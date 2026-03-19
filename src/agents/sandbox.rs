use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use crate::agents::capabilities::{AgentCapabilities, FsCapability, HttpCapability};
use crate::agents::error::AgentError;

// ---------------------------------------------------------------------------
// SandboxConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SandboxConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_bwrap_path")]
    pub bwrap_path: String,
    #[serde(default = "default_fallback")]
    pub fallback_unsandboxed: bool,
}

fn default_enabled() -> bool {
    true
}
fn default_bwrap_path() -> String {
    "/usr/bin/bwrap".into()
}
fn default_fallback() -> bool {
    true
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            bwrap_path: default_bwrap_path(),
            fallback_unsandboxed: default_fallback(),
        }
    }
}

// ---------------------------------------------------------------------------
// Startup helper
// ---------------------------------------------------------------------------

/// Check if bubblewrap is available. Call once at startup.
pub fn check_bwrap_available(config: &SandboxConfig) -> bool {
    if !config.enabled {
        return false;
    }
    std::path::Path::new(&config.bwrap_path).exists()
}

// ---------------------------------------------------------------------------
// Sandboxed execution
// ---------------------------------------------------------------------------

/// Execute a CLI tool inside a bubblewrap sandbox, falling back to the
/// unsandboxed executor according to `sandbox_config`.
pub async fn execute_sandboxed_tool(
    command: &str,
    args: &[String],
    capabilities: &AgentCapabilities,
    timeout_ms: u64,
    sandbox_config: &SandboxConfig,
) -> Result<String, AgentError> {
    // 1. Sandboxing disabled — delegate directly.
    if !sandbox_config.enabled {
        return crate::agents::tool::execute_cli_tool(command, args, timeout_ms).await;
    }

    // 2. Check whether bwrap exists.
    let bwrap_exists = std::path::Path::new(&sandbox_config.bwrap_path).exists();

    if !bwrap_exists {
        if sandbox_config.fallback_unsandboxed {
            // 3. Fallback: warn and run unsandboxed.
            tracing::warn!(
                bwrap_path = %sandbox_config.bwrap_path,
                "bubblewrap not found — falling back to unsandboxed execution"
            );
            return crate::agents::tool::execute_cli_tool(command, args, timeout_ms).await;
        } else {
            // 4. Hard failure.
            return Err(AgentError::ToolExecutionError {
                tool: command.to_string(),
                message: format!(
                    "bubblewrap not found at '{}' and fallback_unsandboxed is disabled",
                    sandbox_config.bwrap_path
                ),
            });
        }
    }

    // 5. Build the bwrap command with namespace isolation.
    let mut cmd = Command::new(&sandbox_config.bwrap_path);
    cmd.arg("--unshare-all")
        .arg("--die-with-parent")
        .arg("--ro-bind")
        .arg("/usr")
        .arg("/usr")
        .arg("--ro-bind")
        .arg("/bin")
        .arg("/bin")
        .arg("--tmpfs")
        .arg("/tmp")
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev");

    // Bind /lib and /lib64 only when they exist on the host.
    if std::path::Path::new("/lib").exists() {
        cmd.arg("--ro-bind").arg("/lib").arg("/lib");
    }
    if std::path::Path::new("/lib64").exists() {
        cmd.arg("--ro-bind").arg("/lib64").arg("/lib64");
    }

    // Filesystem access from capabilities.
    match &capabilities.filesystem {
        FsCapability::ReadOnly { read_only } => {
            for p in read_only {
                cmd.arg("--ro-bind").arg(p).arg(p);
            }
        }
        FsCapability::ReadWrite { read_write } => {
            for p in read_write {
                cmd.arg("--bind").arg(p).arg(p);
            }
        }
        FsCapability::None => {}
    }

    // Network: share only when HTTP capability is not None.
    if !matches!(capabilities.http, HttpCapability::None) {
        cmd.arg("--share-net");
    }

    cmd.arg("--");

    // 6. Append the actual command and its arguments.
    let mut parts = command.split_whitespace();
    let program = parts.next().unwrap_or(command);
    let base_args: Vec<&str> = parts.collect();

    cmd.arg(program);
    cmd.args(&base_args);
    cmd.args(args);

    // 7. Apply timeout.
    let future = cmd.output();
    let output = tokio::time::timeout(Duration::from_millis(timeout_ms), future)
        .await
        .map_err(|_| AgentError::ToolTimeout {
            tool: command.to_string(),
            timeout_ms,
        })?
        .map_err(|e| AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: format!("failed to spawn sandboxed process: {e}"),
        })?;

    // 8. Return stdout on success, stderr on failure.
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        Err(AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: stderr,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandbox_config_defaults() {
        let config = SandboxConfig::default();
        assert!(config.enabled);
        assert_eq!(config.bwrap_path, "/usr/bin/bwrap");
        assert!(config.fallback_unsandboxed);
    }

    #[test]
    fn test_check_bwrap_available() {
        let config = SandboxConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!check_bwrap_available(&config));
    }
}
