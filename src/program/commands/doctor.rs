use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::runtime::RuntimeConfig;
use crate::types::Text;
use anyhow::Context;
use clap::{Arg, ArgAction, ArgMatches, Command};
use pyo3::prelude::*;
use serde::Serialize;
use std::path::Path;
use std::process::ExitCode;

pub struct DoctorCommand {
    runtime_config: RuntimeConfig,
}

impl DoctorCommand {
    pub fn new(runtime_config: RuntimeConfig) -> Self {
        Self { runtime_config }
    }
}

impl RunnableCommand for DoctorCommand {
    fn cli_parser(&self) -> Command {
        Command::new("doctor")
            .about("Report Python package provenance and backend wiring for the current Puff app.")
            .arg(
                Arg::new("json")
                    .long("json")
                    .action(ArgAction::SetTrue)
                    .help("Emit machine-readable JSON instead of text."),
            )
    }

    fn make_runnable(&mut self, args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let as_json = args.get_flag("json");
        let summary = DoctorSummary::collect(self.runtime_config.clone(), args)?;

        Ok(Runnable::new(async move {
            if as_json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                print!("{}", summary.render_text());
            }
            Ok(ExitCode::SUCCESS)
        }))
    }
}

#[derive(Debug, Serialize)]
struct DoctorSummary {
    runtime: RuntimeSummary,
    python: Option<PythonSummary>,
    backends: Vec<BackendSummary>,
    graphql: Vec<GraphqlSummary>,
    http_clients: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct RuntimeSummary {
    tokio_worker_threads: usize,
    max_blocking_threads: usize,
    asyncio: bool,
    python_enabled: bool,
    python_paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PythonSummary {
    sys_path_head: Vec<String>,
    modules: Vec<PythonModuleSummary>,
}

#[derive(Debug, Serialize)]
struct PythonModuleSummary {
    name: String,
    file: Option<String>,
    from_repo_bundle: bool,
}

#[derive(Debug, Serialize)]
struct BackendSummary {
    kind: &'static str,
    name: String,
    url: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct GraphqlSummary {
    name: String,
    import: String,
    database: Option<String>,
}

impl DoctorSummary {
    fn collect(runtime_config: RuntimeConfig, args: &ArgMatches) -> Result<Self> {
        let mut warnings = Vec::new();
        let runtime = RuntimeSummary {
            tokio_worker_threads: runtime_config.tokio_worker_threads(),
            max_blocking_threads: runtime_config.max_blocking_threads(),
            asyncio: runtime_config.asyncio(),
            python_enabled: runtime_config.python(),
            python_paths: runtime_config
                .python_paths()
                .into_iter()
                .map(|p| p.to_string())
                .collect(),
        };

        let python = if runtime_config.python() {
            let python = collect_python_summary()?;
            if let Some(main_module) = python.modules.iter().find(|m| m.name == "puff") {
                if !main_module.from_repo_bundle && bundled_puff_dir().is_dir() {
                    warnings.push(format!(
                        "Python imported puff from '{}' instead of the bundled repo package.",
                        main_module.file.as_deref().unwrap_or("<unknown>")
                    ));
                }
            }
            Some(python)
        } else {
            None
        };

        let mut backends = Vec::new();
        for name in runtime_config.postgres().keys() {
            backends.push(BackendSummary {
                kind: "postgres",
                name: name.to_string(),
                url: arg_value(args, &format!("{}_postgres_url", name.to_lowercase())),
                status: "ok",
            });
        }
        for name in runtime_config.redis().keys() {
            backends.push(BackendSummary {
                kind: "redis",
                name: name.to_string(),
                url: arg_value(args, &format!("{}_redis_url", name.to_lowercase())),
                status: "ok",
            });
        }
        for name in runtime_config.pubsub().keys() {
            backends.push(BackendSummary {
                kind: "pubsub",
                name: name.to_string(),
                url: arg_value(args, &format!("{}_pubsub_url", name.to_lowercase())),
                status: "ok",
            });
        }
        for name in runtime_config.task_queue().keys() {
            backends.push(BackendSummary {
                kind: "task_queue",
                name: name.to_string(),
                url: arg_value(args, &format!("{}_task_queue_url", name.to_lowercase())),
                status: "ok",
            });
        }

        let graphql = runtime_config
            .gql_modules()
            .iter()
            .map(|(name, gql)| GraphqlSummary {
                name: name.to_string(),
                import: gql.schema_import.to_string(),
                database: gql.db.as_ref().map(Text::to_string),
            })
            .collect();

        let http_clients = runtime_config
            .http_client_builder()
            .keys()
            .map(Text::to_string)
            .collect();

        Ok(Self {
            runtime,
            python,
            backends,
            graphql,
            http_clients,
            warnings,
        })
    }

    fn render_text(&self) -> String {
        let mut out = String::new();
        out.push_str("Runtime\n");
        out.push_str(&format!(
            "  Tokio worker threads: {}\n  Max blocking threads: {}\n  AsyncIO enabled: {}\n  Python enabled: {}\n",
            self.runtime.tokio_worker_threads,
            self.runtime.max_blocking_threads,
            self.runtime.asyncio,
            self.runtime.python_enabled
        ));

        if !self.runtime.python_paths.is_empty() {
            out.push_str("  Configured Python paths:\n");
            for path in &self.runtime.python_paths {
                out.push_str(&format!("    - {}\n", path));
            }
        }

        if let Some(python) = &self.python {
            out.push_str("\nPython\n");
            for module in &python.modules {
                let source = if module.from_repo_bundle {
                    "repo-bundled"
                } else {
                    "external"
                };
                out.push_str(&format!(
                    "  {}: {} ({})\n",
                    module.name,
                    module.file.as_deref().unwrap_or("<unknown>"),
                    source
                ));
            }
            if !python.sys_path_head.is_empty() {
                out.push_str("  sys.path head:\n");
                for path in &python.sys_path_head {
                    out.push_str(&format!("    - {}\n", path));
                }
            }
        }

        out.push_str("\nBackends\n");
        if self.backends.is_empty() {
            out.push_str("  none configured\n");
        } else {
            for backend in &self.backends {
                out.push_str(&format!(
                    "  {}[{}]: {} ({})\n",
                    backend.kind, backend.name, backend.url, backend.status
                ));
            }
        }

        out.push_str("\nGraphQL\n");
        if self.graphql.is_empty() {
            out.push_str("  none configured\n");
        } else {
            for gql in &self.graphql {
                out.push_str(&format!("  {}: {}", gql.name, gql.import));
                if let Some(db) = &gql.database {
                    out.push_str(&format!(" (db: {})", db));
                }
                out.push('\n');
            }
        }

        out.push_str("\nHTTP clients\n");
        if self.http_clients.is_empty() {
            out.push_str("  none configured\n");
        } else {
            for client in &self.http_clients {
                out.push_str(&format!("  {}\n", client));
            }
        }

        if !self.warnings.is_empty() {
            out.push_str("\nWarnings\n");
            for warning in &self.warnings {
                out.push_str(&format!("  - {}\n", warning));
            }
        }

        out
    }
}

fn collect_python_summary() -> Result<PythonSummary> {
    Python::with_gil(|py| -> Result<PythonSummary> {
        let sys_path = py
            .import("sys")?
            .getattr("path")?
            .extract::<Vec<String>>()
            .context("Could not read sys.path")?;

        let modules = ["puff", "puff.graphql", "puff.postgres"]
            .into_iter()
            .map(|name| -> Result<PythonModuleSummary> {
                let module = py.import(name)?;
                let file = module
                    .getattr("__file__")
                    .ok()
                    .and_then(|value| value.extract::<String>().ok());
                let from_repo_bundle = file
                    .as_deref()
                    .map(|f| Path::new(f).starts_with(bundled_puff_dir()))
                    .unwrap_or(false);

                Ok(PythonModuleSummary {
                    name: name.to_owned(),
                    file,
                    from_repo_bundle,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(PythonSummary {
            sys_path_head: sys_path.into_iter().take(8).collect(),
            modules,
        })
    })
}

fn arg_value(args: &ArgMatches, key: &str) -> String {
    args.get_one::<String>(key)
        .cloned()
        .unwrap_or_else(|| "<unset>".to_owned())
}

fn bundled_puff_dir() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/puff_py"))
}
