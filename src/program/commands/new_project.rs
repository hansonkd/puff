use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use anyhow::{anyhow, bail, Context};
use clap::{Arg, ArgAction, ArgMatches, Command};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub struct ProjectNewCommand;

impl ProjectNewCommand {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ProjectNewCommand {
    fn default() -> Self {
        Self::new()
    }
}

impl RunnableCommand for ProjectNewCommand {
    fn cli_parser(&self) -> Command {
        Command::new("new")
            .about("Scaffold a new Puff project with an agent-oriented default layout.")
            .arg(
                Arg::new("path")
                    .required(true)
                    .value_name("PATH")
                    .help("Directory to create the project in."),
            )
            .arg(
                Arg::new("force")
                    .long("force")
                    .action(ArgAction::SetTrue)
                    .help("Overwrite generated files in an existing directory."),
            )
            .arg(
                Arg::new("package-name")
                    .long("package-name")
                    .value_name("PYTHON_PACKAGE")
                    .help("Override the generated Python package name."),
            )
    }

    fn make_runnable(&mut self, args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let root = PathBuf::from(
            args.get_one::<String>("path")
                .expect("required clap argument missing"),
        );
        let force = args.get_flag("force");
        let package_name = args.get_one::<String>("package-name").cloned();

        Ok(Runnable::new(async move {
            let spec = ProjectSpec::new(root, package_name)?;
            create_project(&spec, force)?;

            println!("Created Puff project at {}", spec.root.display());
            println!("Next steps:");
            println!("  1. cd {}", spec.root.display());
            println!("  2. cp .env.example .env");
            println!("  3. docker compose up -d");
            println!("  4. puff doctor");
            println!("  5. puff seed_demo");
            println!("  6. puff serve --bind 127.0.0.1:7777");

            Ok(ExitCode::SUCCESS)
        }))
    }
}

#[derive(Debug, Clone)]
struct ProjectSpec {
    root: PathBuf,
    project_name: String,
    package_name: String,
}

impl ProjectSpec {
    fn new(root: PathBuf, package_name: Option<String>) -> Result<Self> {
        let file_name = root
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| anyhow!("Project path must end with a directory name"))?;
        let project_name = file_name.to_owned();
        let package_name = sanitize_package_name(package_name.as_deref().unwrap_or(file_name))
            .ok_or_else(|| {
                anyhow!(
                    "Could not derive a valid Python package name from '{}'",
                    file_name
                )
            })?;

        Ok(Self {
            root,
            project_name,
            package_name,
        })
    }
}

#[derive(Debug)]
struct GeneratedFile {
    relative_path: String,
    contents: String,
    executable: bool,
}

fn create_project(spec: &ProjectSpec, force: bool) -> Result<()> {
    prepare_root(&spec.root, force)?;

    for generated in render_project(spec) {
        let destination = spec.root.join(generated.relative_path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Could not create parent directory for {}",
                    destination.display()
                )
            })?;
        }

        fs::write(&destination, generated.contents)
            .with_context(|| format!("Could not write {}", destination.display()))?;
        if generated.executable {
            set_executable(&destination)?;
        }
    }

    Ok(())
}

fn prepare_root(root: &Path, force: bool) -> Result<()> {
    if root.exists() {
        if !root.is_dir() {
            bail!("Target '{}' exists and is not a directory", root.display());
        }

        let has_entries = fs::read_dir(root)
            .with_context(|| format!("Could not inspect {}", root.display()))?
            .next()
            .transpose()?
            .is_some();

        if has_entries && !force {
            bail!(
                "Target '{}' is not empty. Re-run with --force to overwrite generated files.",
                root.display()
            );
        }
    } else {
        fs::create_dir_all(root).with_context(|| format!("Could not create {}", root.display()))?;
    }

    Ok(())
}

fn render_project(spec: &ProjectSpec) -> Vec<GeneratedFile> {
    vec![
        GeneratedFile {
            relative_path: ".gitignore".to_owned(),
            contents: render_gitignore(),
            executable: false,
        },
        GeneratedFile {
            relative_path: ".env.example".to_owned(),
            contents: render_env_example(),
            executable: false,
        },
        GeneratedFile {
            relative_path: "AGENTS.md".to_owned(),
            contents: render_agents_md(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: "README.md".to_owned(),
            contents: render_readme(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: "Makefile".to_owned(),
            contents: render_makefile(),
            executable: false,
        },
        GeneratedFile {
            relative_path: "docker-compose.yml".to_owned(),
            contents: render_docker_compose(),
            executable: false,
        },
        GeneratedFile {
            relative_path: "pyproject.toml".to_owned(),
            contents: render_pyproject(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: "puff.toml".to_owned(),
            contents: render_puff_toml(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: "bench/graphql_post.lua".to_owned(),
            contents: render_graphql_post_lua(),
            executable: false,
        },
        GeneratedFile {
            relative_path: "bench/profile_graphql.sh".to_owned(),
            contents: render_profile_graphql_sh(),
            executable: true,
        },
        GeneratedFile {
            relative_path: "tests/test_unit.py".to_owned(),
            contents: render_test_unit(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: "tests/test_integration.py".to_owned(),
            contents: render_test_integration(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: "app_placeholder".to_owned(),
            contents: String::new(),
            executable: false,
        },
        GeneratedFile {
            relative_path: format!("{}/__init__.py", spec.package_name),
            contents: render_package_init(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: format!("{}/asgi.py", spec.package_name),
            contents: render_asgi_py(spec),
            executable: false,
        },
        GeneratedFile {
            relative_path: format!("{}/data.py", spec.package_name),
            contents: render_data_py(),
            executable: false,
        },
        GeneratedFile {
            relative_path: format!("{}/graphql_schema.py", spec.package_name),
            contents: render_graphql_schema_py(),
            executable: false,
        },
        GeneratedFile {
            relative_path: format!("{}/commands.py", spec.package_name),
            contents: render_commands_py(),
            executable: false,
        },
        GeneratedFile {
            relative_path: format!("{}/tasks.py", spec.package_name),
            contents: render_tasks_py(),
            executable: false,
        },
    ]
    .into_iter()
    .filter(|file| file.relative_path != "app_placeholder")
    .collect()
}

fn render_gitignore() -> String {
    r#".env
.venv/
__pycache__/
.pytest_cache/
.ruff_cache/
*.pyc
bench/results/
"#
    .to_owned()
}

fn render_env_example() -> String {
    r#"PUFF_DEFAULT_POSTGRES_URL=postgres://postgres:password@127.0.0.1:5432/postgres
PUFF_DEFAULT_REDIS_URL=redis://127.0.0.1:6379
PUFF_DEFAULT_PUBSUB_URL=redis://127.0.0.1:6379
PUFF_DEFAULT_TASK_QUEUE_URL=redis://127.0.0.1:6379
"#
    .to_owned()
}

fn render_agents_md(spec: &ProjectSpec) -> String {
    format!(
        r#"# Agent Notes

This project was scaffolded for Puff's agent-oriented workflow.

## Project map
- Python package: `{package_name}/`
- GraphQL schema: `{package_name}/graphql_schema.py`
- HTTP fallback app: `{package_name}/asgi.py`
- Postgres and Redis helpers: `{package_name}/data.py`
- One-off commands: `{package_name}/commands.py`
- Task queue jobs: `{package_name}/tasks.py`
- Tests: `tests/`
- Bench script: `bench/profile_graphql.sh`

## Normal loop
- Start dependencies: `docker compose up -d`
- Validate runtime wiring: `puff doctor`
- Seed demo data: `puff seed_demo`
- Run the app: `puff serve --bind 127.0.0.1:7777`
- Run tests: `puff pytest`
- Queue a background job: `puff enqueue_demo`
- Profile the default GraphQL query: `bench/profile_graphql.sh`

## Runtime expectations
- GraphQL playground is served at `/`.
- GraphQL POST endpoint is `/graphql`.
- ASGI fallback routes handle `/healthz` and `/info`.
- Postgres is the source of truth for `agent_tasks`.
- Redis is used as the fast cache and task/pubsub backend.

## Safety notes
- Prefer the SQL-decorated GraphQL path for read-heavy list queries.
- Keep task payloads JSON-serializable.
- The integration test is opt-in; set `PUFF_RUN_INTEGRATION=1` before `puff pytest`.
"#,
        package_name = spec.package_name
    )
}

fn render_readme(spec: &ProjectSpec) -> String {
    format!(
        r#"# {project_name}

Opinionated Puff scaffold for coding-agent projects.

## What you get
- ASGI fallback app with `/healthz` and `/info`
- GraphQL schema with one fused SQL query and one cache mutation
- Postgres seed command
- Redis-backed task queue demo
- Unit tests plus opt-in integration test
- `docker-compose.yml`, `.env.example`, `Makefile`, and a small `wrk` bench harness

## Quick start
```bash
cp .env.example .env
docker compose up -d
puff doctor
puff seed_demo
puff serve --bind 127.0.0.1:7777
```

Then open `http://127.0.0.1:7777/` for the GraphQL playground.

## Useful commands
```bash
puff doctor
puff serve --bind 127.0.0.1:7777
puff seed_demo
puff enqueue_demo
puff pytest
bench/profile_graphql.sh
```

## GraphQL examples
```graphql
query {{
  hello(name: "agent")
  tasks(limit: 3) {{
    id
    title
    done
    status
  }}
}}
```

```graphql
mutation {{
  warm_task_cache(task_id: 1)
}}
```

```graphql
query {{
  cached_task_title(task_id: 1)
}}
```

## Project layout
- `{package_name}/graphql_schema.py`: GraphQL types and root resolvers
- `{package_name}/data.py`: Postgres seed and fetch helpers
- `{package_name}/tasks.py`: Redis-backed task queue jobs
- `{package_name}/commands.py`: CLI helpers exposed through `puff.toml`
- `tests/`: unit and opt-in integration tests
- `bench/`: local perf harness
"#,
        project_name = spec.project_name,
        package_name = spec.package_name
    )
}

fn render_makefile() -> String {
    r#".PHONY: up down doctor seed serve test queue bench

up:
	docker compose up -d

down:
	docker compose down

doctor:
	puff doctor

seed:
	puff seed_demo

serve:
	puff serve --bind 127.0.0.1:7777

test:
	puff pytest

queue:
	puff enqueue_demo

bench:
	bench/profile_graphql.sh
"#
    .to_owned()
}

fn render_docker_compose() -> String {
    r#"services:
  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_USER: postgres
      POSTGRES_PASSWORD: password
      POSTGRES_DB: postgres
    ports:
      - "5432:5432"
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres -d postgres"]
      interval: 2s
      timeout: 2s
      retries: 15

  redis:
    image: redis:7-alpine
    ports:
      - "6379:6379"
    healthcheck:
      test: ["CMD", "redis-cli", "ping"]
      interval: 2s
      timeout: 2s
      retries: 15
"#
    .to_owned()
}

fn render_pyproject(spec: &ProjectSpec) -> String {
    format!(
        r#"[build-system]
requires = ["setuptools>=68"]
build-backend = "setuptools.build_meta"

[project]
name = "{project_name}"
version = "0.1.0"
description = "Puff scaffold for coding-agent projects"
readme = "README.md"
requires-python = ">=3.12"
dependencies = []

[project.optional-dependencies]
dev = ["pytest>=8", "ruff>=0.6"]

[tool.pytest.ini_options]
testpaths = ["tests"]
pythonpath = ["."]

[tool.ruff]
line-length = 100

[tool.setuptools.packages.find]
include = ["{package_name}*"]
"#,
        project_name = spec.project_name,
        package_name = spec.package_name
    )
}

fn render_puff_toml(spec: &ProjectSpec) -> String {
    format!(
        r#"asyncio = true
dotenv = true
add_cwd_to_path = true
compression_middleware = true
asgi = "{package_name}.asgi.app"
pytest_path = "tests"

[[postgres]]
name = "default"
pool_size = 10

[[redis]]
name = "default"
pool_size = 20

[[pubsub]]
name = "default"
pool_size = 20

[[task_queue]]
name = "default"
pool_size = 20
max_concurrent_tasks = 32

[[http_client]]
name = "default"
max_idle_connections = 128
user_agent = "{package_name}/0.1.0"

[[graphql]]
name = "default"
schema = "{package_name}.graphql_schema.Schema"
url = "/graphql"
playground_url = "/"
database = "default"

[[commands]]
command_name = "seed_demo"
function = "{package_name}.commands.seed_demo"

[[commands]]
command_name = "enqueue_demo"
function = "{package_name}.tasks.enqueue_demo"
"#,
        package_name = spec.package_name
    )
}

fn render_graphql_post_lua() -> String {
    r#"wrk.method = "POST"
wrk.headers["Content-Type"] = "application/json"
wrk.body = os.getenv("GRAPHQL_BODY")

if wrk.body == nil then
    error("GRAPHQL_BODY env var must be set")
end
"#
    .to_owned()
}

fn render_profile_graphql_sh() -> String {
    r#"#!/usr/bin/env bash
set -euo pipefail

if ! command -v wrk >/dev/null 2>&1; then
    echo "wrk is required to run this bench." >&2
    exit 1
fi

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
GRAPHQL_URL="${GRAPHQL_URL:-http://127.0.0.1:7777/graphql}"
WRK_THREADS="${WRK_THREADS:-2}"
WRK_CONNECTIONS="${WRK_CONNECTIONS:-16}"
WRK_DURATION="${WRK_DURATION:-10s}"
GRAPHQL_BODY='{"query":"query Bench { tasks(limit: 32) { id title done status } }"}'

GRAPHQL_BODY="$GRAPHQL_BODY" \
  wrk -t"$WRK_THREADS" -c"$WRK_CONNECTIONS" -d"$WRK_DURATION" --latency \
  -s "$ROOT_DIR/bench/graphql_post.lua" \
  "$GRAPHQL_URL"
"#
    .to_owned()
}

fn render_package_init(spec: &ProjectSpec) -> String {
    format!(
        r#"__all__ = ["asgi", "commands", "data", "graphql_schema", "tasks"]
__project__ = "{project_name}"
"#,
        project_name = spec.project_name
    )
}

fn render_asgi_py(spec: &ProjectSpec) -> String {
    format!(
        r#"import json


PROJECT_NAME = {project_name:?}


async def app(scope, receive, send):
    if scope["type"] != "http":
        return

    path = scope.get("path", "/")
    if path == "/healthz":
        status = 200
        payload = {{"ok": True}}
    elif path == "/info":
        status = 200
        payload = {{
            "project": PROJECT_NAME,
            "graphql": "/graphql",
            "playground": "/",
            "seed_command": "puff seed_demo",
            "task_command": "puff enqueue_demo",
        }}
    else:
        status = 404
        payload = {{"error": "not_found", "path": path}}

    body = json.dumps(payload).encode("utf-8")
    await send(
        {{
            "type": "http.response.start",
            "status": status,
            "headers": [(b"content-type", b"application/json")],
        }}
    )
    await send({{"type": "http.response.body", "body": body}})
"#,
        project_name = spec.project_name
    )
}

fn render_data_py() -> String {
    r#"from puff.postgres import PostgresConnection


SEED_TASKS = [
    (1, "Draft project brief", False),
    (2, "Implement fast GraphQL path", True),
    (3, "Add background cache warming", False),
    (4, "Profile Postgres under load", False),
]


def cache_key(task_id: int) -> str:
    return f"agent-task:{task_id}"


def ensure_schema() -> None:
    with PostgresConnection(autocommit=True) as conn:
        with conn.cursor() as cursor:
            cursor.execute(
                """
                CREATE TABLE IF NOT EXISTS agent_tasks (
                    id integer PRIMARY KEY,
                    title text NOT NULL,
                    done boolean NOT NULL DEFAULT false
                )
                """
            )


def seed_demo_tasks() -> int:
    ensure_schema()
    with PostgresConnection(autocommit=True) as conn:
        with conn.cursor() as cursor:
            cursor.execute("TRUNCATE agent_tasks")
            cursor.executemany(
                "INSERT INTO agent_tasks (id, title, done) VALUES (%s, %s, %s)",
                SEED_TASKS,
            )
    return len(SEED_TASKS)


def fetch_task_title(task_id: int) -> str | None:
    ensure_schema()
    with PostgresConnection(autocommit=True) as conn:
        with conn.cursor() as cursor:
            cursor.execute("SELECT title FROM agent_tasks WHERE id = %s", [task_id])
            row = cursor.fetchone()
    if row is None:
        return None
    return row[0]
"#
    .to_owned()
}

fn render_graphql_schema_py() -> String {
    r#"from dataclasses import dataclass
from typing import Optional

from puff.graphql import EmptyObject, sql
from puff.redis import global_redis

from .data import cache_key, fetch_task_title


redis = global_redis


def _decode_text(value) -> Optional[str]:
    if value is None:
        return None
    if isinstance(value, bytes):
        return value.decode("utf-8")
    return str(value)


@dataclass
class AgentTask:
    id: int
    title: str
    done: bool

    def status(self, ctx, /) -> str:
        return "done" if self.done else "open"


@dataclass
class Query:
    @classmethod
    def hello(cls, ctx, /, name: Optional[str] = None) -> str:
        target = name or "world"
        return f"hello, {target}"

    @classmethod
    @sql(
        "SELECT id, title, done FROM agent_tasks ORDER BY id LIMIT $1",
        args=["limit"],
    )
    def tasks(cls, ctx, /, limit: int = 20) -> list[AgentTask]:
        ...

    @classmethod
    def cached_task_title(cls, ctx, /, task_id: int = 1) -> Optional[str]:
        return _decode_text(redis.get(cache_key(task_id)))


@dataclass
class Mutation:
    @classmethod
    def warm_task_cache(cls, ctx, /, task_id: int = 1) -> Optional[str]:
        title = fetch_task_title(task_id)
        if title is None:
            return None
        redis.set(cache_key(task_id), title)
        return title


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: EmptyObject
"#
    .to_owned()
}

fn render_commands_py() -> String {
    r#"from .data import seed_demo_tasks


def seed_demo():
    count = seed_demo_tasks()
    print(f"Seeded agent_tasks with {count} rows.")
"#
    .to_owned()
}

fn render_tasks_py() -> String {
    r#"from puff.redis import global_redis
from puff.task_queue import global_task_queue, task

from .data import cache_key, fetch_task_title, seed_demo_tasks


redis = global_redis
task_queue = global_task_queue


@task
def warm_task_cache_job(payload):
    task_id = int(payload.get("task_id", 1))
    title = fetch_task_title(task_id)
    if title is None:
        return {"task_id": task_id, "cached": False}
    redis.set(cache_key(task_id), title)
    return {"task_id": task_id, "cached": True, "title": title}


def enqueue_demo():
    seed_demo_tasks()
    task_id = task_queue.schedule_function(
        warm_task_cache_job,
        {"task_id": 1},
        timeout_ms=1_000,
        keep_results_for_ms=60_000,
    )
    result = task_queue.wait_for_task_result(task_id)
    print(result)
"#
    .to_owned()
}

fn render_test_unit(spec: &ProjectSpec) -> String {
    format!(
        r#"from {package_name}.data import cache_key
from {package_name}.graphql_schema import AgentTask, Query


def test_cache_key():
    assert cache_key(7) == "agent-task:7"


def test_hello_query():
    assert Query.hello(None, name="builder") == "hello, builder"


def test_task_status_field():
    task = AgentTask(id=1, title="Ship it", done=False)
    assert task.status(None) == "open"
"#,
        package_name = spec.package_name
    )
}

fn render_test_integration(spec: &ProjectSpec) -> String {
    format!(
        r#"import os

import pytest

from {package_name}.commands import seed_demo
from {package_name}.graphql_schema import Mutation, Query


@pytest.mark.skipif(
    os.getenv("PUFF_RUN_INTEGRATION") != "1",
    reason="Set PUFF_RUN_INTEGRATION=1 after starting docker compose services.",
)
def test_cache_round_trip():
    seed_demo()
    assert Mutation.warm_task_cache(None, task_id=1) == "Draft project brief"
    assert Query.cached_task_title(None, task_id=1) == "Draft project brief"
"#,
        package_name = spec.package_name
    )
}

fn sanitize_package_name(input: &str) -> Option<String> {
    let mut output = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
        } else {
            output.push('_');
        }
    }

    while output.contains("__") {
        output = output.replace("__", "_");
    }
    output = output.trim_matches('_').to_owned();
    if output.is_empty() {
        return None;
    }

    if output
        .chars()
        .next()
        .map(|ch| ch.is_ascii_digit())
        .unwrap_or(false)
    {
        output.insert_str(0, "app_");
    }

    Some(output)
}

fn set_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(path)
            .with_context(|| format!("Could not stat {}", path.display()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .with_context(|| format!("Could not set executable bit on {}", path.display()))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{render_project, sanitize_package_name, ProjectSpec};
    use std::path::PathBuf;

    #[test]
    fn sanitize_package_name_normalizes_common_inputs() {
        assert_eq!(sanitize_package_name("My App"), Some("my_app".to_owned()));
        assert_eq!(
            sanitize_package_name("123-demo"),
            Some("app_123_demo".to_owned())
        );
        assert_eq!(sanitize_package_name("---"), None);
    }

    #[test]
    fn generated_project_contains_expected_files() {
        let spec = ProjectSpec::new(PathBuf::from("agent-stack"), None).unwrap();
        let files = render_project(&spec);
        let rendered_paths = files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect::<Vec<_>>();

        assert!(rendered_paths.iter().any(|path| *path == "README.md"));
        assert!(rendered_paths.iter().any(|path| *path == "AGENTS.md"));
        assert!(rendered_paths.iter().any(|path| *path == "puff.toml"));
        assert!(rendered_paths
            .iter()
            .any(|path| *path == "bench/profile_graphql.sh"));
        assert!(rendered_paths
            .iter()
            .any(|path| *path == "tests/test_integration.py"));
        assert!(rendered_paths
            .iter()
            .any(|path| *path == "agent_stack/graphql_schema.py"));
    }
}
