use crate::agents::error::AgentError;
use crate::agents::llm::ToolDefinition;
use crate::agents::tool::{OutputFormat, RegisteredTool, ToolExecutor};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;

// ---------------------------------------------------------------------------
// Private TOML deserialization types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SkillToml {
    skill: SkillMeta,
    #[serde(default)]
    tools: Vec<SkillToolToml>,
    #[serde(default)]
    permissions: SkillPermissionsToml,
}

#[derive(Debug, Deserialize)]
struct SkillMeta {
    name: String,
    description: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct SkillToolToml {
    name: String,
    description: String,
    /// CLI command to run (mutually exclusive with `wasm`).
    command: Option<String>,
    /// Path to a `.wasm` file (mutually exclusive with `command`).
    wasm: Option<String>,
    #[serde(default)]
    args: HashMap<String, ArgDef>,
    #[serde(default = "default_output")]
    output: String,
    #[serde(default)]
    requires_approval: bool,
}

fn default_output() -> String {
    "text".to_string()
}

#[derive(Debug, Deserialize)]
struct ArgDef {
    #[serde(rename = "type")]
    type_: String,
    description: String,
}

#[derive(Debug, Deserialize, Default)]
struct SkillPermissionsToml {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SkillTool {
    pub name: String,
    pub description: String,
    /// CLI command string, if this is a CLI tool.
    pub command: Option<String>,
    /// Path to a `.wasm` module, if this is a WASM tool.
    pub wasm_path: Option<std::path::PathBuf>,
    /// Argument name -> JSON type string
    pub args: HashMap<String, String>,
    pub output_format: OutputFormat,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SkillPermissions {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub version: String,
    pub tools: Vec<SkillTool>,
    pub permissions: SkillPermissions,
    /// Contents of `context.md`, if present.
    pub context: Option<String>,
    pub source_dir: String,
}

// ---------------------------------------------------------------------------
// Skill implementation
// ---------------------------------------------------------------------------

impl Skill {
    /// Parse a `Skill` from raw TOML text.
    pub fn from_toml(toml_str: &str, source_dir: &str) -> Result<Self, AgentError> {
        let parsed: SkillToml =
            toml::from_str(toml_str).map_err(|e| AgentError::SkillLoadError {
                skill: source_dir.to_string(),
                message: format!("TOML parse error: {e}"),
            })?;

        let source_path = std::path::Path::new(source_dir);

        let tools = parsed
            .tools
            .into_iter()
            .map(|t| {
                let output_format = match t.output.to_lowercase().as_str() {
                    "json" => OutputFormat::Json,
                    "csv" => OutputFormat::Csv,
                    _ => OutputFormat::Text,
                };

                let args = t
                    .args
                    .into_iter()
                    .map(|(k, v)| (k, v.type_))
                    .collect::<HashMap<String, String>>();

                // Resolve the WASM path relative to the skill's source directory.
                let wasm_path = t
                    .wasm
                    .map(|w| source_path.join(w));

                SkillTool {
                    name: t.name,
                    description: t.description,
                    command: t.command,
                    wasm_path,
                    args,
                    output_format,
                    requires_approval: t.requires_approval,
                }
            })
            .collect();

        let permissions = SkillPermissions {
            allow: parsed.permissions.allow,
            deny: parsed.permissions.deny,
        };

        Ok(Skill {
            name: parsed.skill.name,
            description: parsed.skill.description,
            version: parsed.skill.version,
            tools,
            permissions,
            context: None,
            source_dir: source_dir.to_string(),
        })
    }

    /// Load a skill from a directory containing `skill.toml` and an optional
    /// `context.md`.
    pub fn load_from_dir(dir: &Path) -> Result<Self, AgentError> {
        let skill_path = dir.join("skill.toml");
        let toml_str =
            std::fs::read_to_string(&skill_path).map_err(|e| AgentError::SkillLoadError {
                skill: dir.display().to_string(),
                message: format!("cannot read skill.toml: {e}"),
            })?;

        let source_dir = dir.display().to_string();
        let mut skill = Self::from_toml(&toml_str, &source_dir)?;

        // Optionally load context.md
        let context_path = dir.join("context.md");
        if context_path.exists() {
            skill.context =
                std::fs::read_to_string(&context_path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
        }

        Ok(skill)
    }

    /// Check whether a command string is allowed by the skill's permissions.
    ///
    /// Deny patterns are checked first. If a deny pattern matches, the command
    /// is blocked regardless of allow patterns. Then, if any allow patterns are
    /// defined, the command must match at least one of them.
    pub fn is_command_allowed(&self, command: &str) -> bool {
        // Check deny patterns first.
        for pattern in &self.permissions.deny {
            if glob_matches(pattern, command) {
                return false;
            }
        }

        // If there are allow patterns, at least one must match.
        if !self.permissions.allow.is_empty() {
            return self
                .permissions
                .allow
                .iter()
                .any(|p| glob_matches(p, command));
        }

        // No allow patterns defined — allow everything not explicitly denied.
        true
    }

    /// Convert this skill into a list of `RegisteredTool`s suitable for
    /// insertion into a `ToolRegistry`.
    pub fn into_registered_tools(self) -> Vec<RegisteredTool> {
        self.tools
            .into_iter()
            .map(|t| {
                // Build a JSON Schema for the tool's arguments.
                let mut properties = serde_json::Map::new();
                let mut required: Vec<serde_json::Value> = Vec::new();

                for (arg_name, arg_type) in &t.args {
                    properties.insert(
                        arg_name.clone(),
                        json!({ "type": arg_type }),
                    );
                    required.push(json!(arg_name));
                }

                let input_schema = json!({
                    "type": "object",
                    "properties": properties,
                    "required": required,
                });

                let requires_approval = t.requires_approval;
                let output_format = t.output_format.clone();

                // Choose executor: WASM takes priority when both are set.
                let executor = if let Some(module_path) = t.wasm_path.clone() {
                    ToolExecutor::Wasm { module_path }
                } else if let Some(command) = t.command.clone() {
                    ToolExecutor::Cli {
                        command,
                        args_template: Vec::new(),
                        output_format,
                    }
                } else {
                    ToolExecutor::Noop
                };

                RegisteredTool {
                    definition: ToolDefinition {
                        name: t.name,
                        description: t.description,
                        input_schema,
                    },
                    executor,
                    requires_approval,
                    timeout_ms: 30_000,
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Glob matching helper
// ---------------------------------------------------------------------------

/// Simple glob matcher.  Only `*` wildcards are supported.
/// `"gh pr *"` matches any string starting with `"gh pr "`.
fn glob_matches(pattern: &str, value: &str) -> bool {
    // Split on `*` and match each segment sequentially.
    let mut parts = pattern.split('*');
    let mut remaining = value;

    // The first segment must be a prefix.
    if let Some(first) = parts.next() {
        if !remaining.starts_with(first) {
            return false;
        }
        remaining = &remaining[first.len()..];
    }

    for part in parts {
        if part.is_empty() {
            // Trailing `*` — matches everything.
            return true;
        }
        match remaining.find(part) {
            Some(pos) => {
                remaining = &remaining[pos + part.len()..];
            }
            None => return false,
        }
    }

    // All segments consumed; `remaining` must be empty (no trailing wildcard).
    remaining.is_empty()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_TOML: &str = r#"
[skill]
name = "github"
description = "GitHub skill"
version = "1.0.0"

[[tools]]
name = "gh_pr_list"
description = "List open PRs"
command = "gh pr list"
output = "json"
requires_approval = false

[tools.args.repo]
type = "string"
description = "Repository name"

[[tools]]
name = "gh_pr_merge"
description = "Merge a PR"
command = "gh pr merge"
output = "text"
requires_approval = true

[permissions]
allow = ["gh pr *", "gh issue *"]
deny = ["gh pr close *"]
"#;

    #[test]
    fn test_parse_skill_toml() {
        let skill = Skill::from_toml(SAMPLE_TOML, "/tmp/github-skill").unwrap();

        assert_eq!(skill.name, "github");
        assert_eq!(skill.version, "1.0.0");
        assert_eq!(skill.tools.len(), 2);

        let pr_list = skill.tools.iter().find(|t| t.name == "gh_pr_list").unwrap();
        assert_eq!(pr_list.command.as_deref(), Some("gh pr list"));
        assert!(pr_list.wasm_path.is_none());
        assert!(!pr_list.requires_approval);
        assert!(pr_list.args.contains_key("repo"));

        let pr_merge = skill.tools.iter().find(|t| t.name == "gh_pr_merge").unwrap();
        assert!(pr_merge.requires_approval);

        assert_eq!(skill.permissions.allow.len(), 2);
        assert_eq!(skill.permissions.deny.len(), 1);
    }

    #[test]
    fn test_permission_glob_matching() {
        let skill = Skill::from_toml(SAMPLE_TOML, "/tmp/github-skill").unwrap();

        // Allowed by "gh pr *"
        assert!(skill.is_command_allowed("gh pr list"));
        assert!(skill.is_command_allowed("gh pr view 42"));

        // Denied by "gh pr close *" (deny checked first)
        assert!(!skill.is_command_allowed("gh pr close 10"));

        // Allowed by "gh issue *"
        assert!(skill.is_command_allowed("gh issue list"));

        // Not in allow list at all
        assert!(!skill.is_command_allowed("rm -rf /"));
        assert!(!skill.is_command_allowed("git push --force"));
    }
}
