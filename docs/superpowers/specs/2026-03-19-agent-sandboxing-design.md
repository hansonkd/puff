# Puff Agent Sandboxing — Five-Layer Security

**Date:** 2026-03-19
**Author:** Kyle Hanson
**Status:** Draft

## Vision

Every time Python calls into Puff — to execute a tool, query a database, make an HTTP request, talk to another agent — it crosses through PyO3 into Rust. That crossing point is the sandbox. Python can't bypass it. It's not middleware you can forget to apply. It's the architecture.

Five layers, each building on the previous, enforced at the Rust boundary with zero overhead for the Python developer.

## Layer 1: Capability Tokens

Every Agent gets an immutable `AgentCapabilities` struct at creation. Replaces the current `AgentPermissions` config-only struct with a Rust-enforced permission system.

```rust
pub struct AgentCapabilities {
    pub sql: SqlCapability,
    pub http: HttpCapability,
    pub filesystem: FsCapability,
    pub tools: ToolCapability,
    pub agents: AgentCapability,
    pub budget: BudgetCapability,
}

pub enum SqlCapability { None, ReadOnly, ReadWrite }
pub enum HttpCapability { None, Allowlist(Vec<String>), Any }
pub enum FsCapability { None, ReadOnly(Vec<PathBuf>), ReadWrite(Vec<PathBuf>) }
pub enum ToolCapability { None, Specific(Vec<String>), All }
pub enum AgentCapability { None, Specific(Vec<String>), All }

pub struct BudgetCapability {
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub max_cost_usd: Option<f64>,
    pub max_tool_calls: Option<u32>,
    pub max_tool_cpu_seconds: Option<f64>,
}
```

Capability checks are struct field matches — nanosecond cost. Checked at every Rust boundary:
- `LlmClient::stream()` checks budget capability
- `execute_cli_tool()` / `execute_sandboxed_tool()` checks tools + filesystem
- Postgres connector checks sql capability before issuing queries
- HTTP client checks allowlist before making requests
- Agent orchestration checks agents capability before invoking other agents

Default capabilities: `All` for tools, `ReadWrite` for SQL, `Any` for HTTP, unlimited budget. Restrictive capabilities are opt-in per agent.

### Python API

```python
agent = Agent(
    name="reader-bot",
    capabilities={
        "sql": "read_only",
        "http": ["api.stripe.com", "api.sendgrid.com"],
        "filesystem": {"read_only": ["/data/reports"]},
        "tools": ["search", "list-prs"],
        "budget": {"max_cost_usd": 1.0, "max_tool_calls": 50},
    },
)
```

### puff.toml

```toml
[[agents]]
name = "reader-bot"

[agents.capabilities]
sql = "read_only"
http = ["api.stripe.com", "api.sendgrid.com"]
tools = ["search", "list-prs"]

[agents.capabilities.filesystem]
read_only = ["/data/reports"]

[agents.capabilities.budget]
max_cost_usd = 1.0
max_tool_calls = 50
```

## Layer 2: Budget Enforcement

Atomic per-agent usage tracking with hard limits.

```rust
pub struct AgentBudgetTracker {
    limits: BudgetCapability,
    input_tokens_used: AtomicU64,
    output_tokens_used: AtomicU64,
    cost_usd_micros: AtomicU64,   // microdollars for atomic precision
    tool_calls_made: AtomicU32,
    tool_cpu_ms: AtomicU64,        // milliseconds
}
```

### Check methods

```rust
impl AgentBudgetTracker {
    pub fn check_llm_budget(&self) -> Result<(), AgentError> { ... }
    pub fn record_llm_usage(&self, input_tokens: u64, output_tokens: u64, cost_usd: f64) { ... }
    pub fn check_tool_budget(&self) -> Result<(), AgentError> { ... }
    pub fn record_tool_usage(&self, cpu_ms: u64) { ... }
}
```

All operations use `AtomicU64::fetch_add` with `Ordering::Relaxed` — no locks, no contention, no overhead.

When a limit is hit: `AgentError::BudgetExceeded { resource: "output_tokens", limit: 100000, used: 100001 }`. The error propagates to the Python agent, which can handle it or let it bubble to the conversation.

### Integration points

- `LlmClient::stream()` — calls `check_llm_budget()` before HTTP request. After response, calls `record_llm_usage()`.
- `execute_cli_tool()` / `execute_sandboxed_tool()` — calls `check_tool_budget()` before execution. After completion, measures elapsed CPU time and calls `record_tool_usage()`.

## Layer 3: Connection Quotas

Per-agent semaphore on database connection pools. Prevents one agent from exhausting all connections.

```rust
pub struct QuotedPool<M: bb8::ManageConnection> {
    inner: Pool<M>,
    agent_semaphores: DashMap<String, Arc<Semaphore>>,
    max_per_agent: usize,
}

impl<M: bb8::ManageConnection> QuotedPool<M> {
    pub async fn get(&self, agent_name: &str) -> Result<QuotedConnection<M>, AgentError> {
        let semaphore = self.agent_semaphores
            .entry(agent_name.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.max_per_agent)))
            .clone();

        let permit = semaphore.acquire_owned().await
            .map_err(|_| AgentError::ConfigError("Connection quota exhausted".into()))?;

        let conn = self.inner.get().await
            .map_err(|e| AgentError::MemoryError(format!("Pool exhausted: {}", e)))?;

        Ok(QuotedConnection { conn, _permit: permit })
    }
}
```

Default: `max_per_agent = pool_size / 4`. Configurable in `puff.toml`:

```toml
[postgres]
pool_size = 20
max_per_agent = 5
```

Applied to both Postgres and Redis pools. The existing `conn_task` in the Postgres connector acquires its connection through the quoted pool. Non-agent code (GraphQL, WSGI) uses the pool directly — no quota.

## Layer 4: Bubblewrap CLI Sandboxing

Wrap CLI tool execution with Linux namespace isolation via bubblewrap (`bwrap`).

```rust
pub async fn execute_sandboxed_tool(
    command: &str,
    args: &[String],
    capabilities: &AgentCapabilities,
    timeout_ms: u64,
) -> Result<String, AgentError> {
    let mut cmd = Command::new("bwrap");

    // Minimal root filesystem — read-only system binaries
    cmd.arg("--unshare-all")
       .arg("--die-with-parent")
       .arg("--ro-bind").arg("/usr").arg("/usr")
       .arg("--ro-bind").arg("/lib").arg("/lib")
       .arg("--ro-bind").arg("/lib64").arg("/lib64")
       .arg("--ro-bind").arg("/bin").arg("/bin")
       .arg("--symlink").arg("/usr/lib64/ld-linux-x86-64.so.2").arg("/lib64/ld-linux-x86-64.so.2")
       .arg("--tmpfs").arg("/tmp")
       .arg("--proc").arg("/proc")
       .arg("--dev").arg("/dev");

    // Filesystem access from capabilities
    match &capabilities.filesystem {
        FsCapability::ReadOnly(paths) => {
            for p in paths { cmd.arg("--ro-bind").arg(p).arg(p); }
        }
        FsCapability::ReadWrite(paths) => {
            for p in paths { cmd.arg("--bind").arg(p).arg(p); }
        }
        FsCapability::None => {}
    }

    // Network: only if HTTP capability allows it
    if !matches!(capabilities.http, HttpCapability::None) {
        cmd.arg("--share-net");
    }

    cmd.arg("--").arg(command);
    for arg in args { cmd.arg(arg); }

    let output = tokio::time::timeout(
        Duration::from_millis(timeout_ms),
        cmd.output(),
    ).await
    .map_err(|_| AgentError::ToolTimeout { tool: command.to_string(), timeout_ms })??;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        Err(AgentError::ToolExecutionError {
            tool: command.to_string(),
            message: String::from_utf8_lossy(&output.stderr).to_string(),
        })
    }
}
```

### Fallback

If `bwrap` is not installed, falls back to unsandboxed `execute_cli_tool()` with a `tracing::warn!("bubblewrap not found — CLI tools running unsandboxed")`. This is logged once at startup.

### Configuration

```toml
[agent_server.sandbox]
enabled = true
bwrap_path = "/usr/bin/bwrap"
fallback_unsandboxed = true   # if false, error when bwrap not found
```

### Overhead

Bubblewrap adds ~1-2ms per tool invocation (namespace setup). For tools that take 100ms+, this is negligible. For very fast tools, the overhead is measurable but acceptable for the security gain.

## Layer 5: WASM Tool Runtime

New `ToolExecutor::Wasm` variant for running tools as WebAssembly modules via `wasmtime`.

### Skill definition

```toml
# skill.toml
[[tools]]
name = "compute-hash"
description = "Compute SHA256 hash of input"
wasm = "compute_hash.wasm"
output = "json"
```

### Execution model

```rust
pub async fn execute_wasm_tool(
    module_path: &Path,
    input: &serde_json::Value,
    timeout_ms: u64,
) -> Result<String, AgentError> {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::from_file(&engine, module_path)
        .map_err(|e| AgentError::ToolExecutionError {
            tool: module_path.display().to_string(),
            message: format!("Failed to load WASM module: {}", e),
        })?;

    let mut store = wasmtime::Store::new(&engine, ());
    let mut linker = wasmtime::Linker::new(&engine);

    // WASI with minimal capabilities — no filesystem, no network
    wasmtime_wasi::add_to_linker_sync(&mut linker)?;

    let instance = linker.instantiate(&mut store, &module)?;

    // Write input to stdin, read output from stdout
    let input_json = serde_json::to_string(input)?;
    // ... pipe input to WASI stdin, capture WASI stdout ...

    // Apply timeout
    store.set_epoch_deadline(1);
    // ... run with epoch-based interruption for timeout ...
}
```

### Isolation guarantees

- **Memory**: WASM module runs in its own linear memory — can't access Puff's process memory
- **Filesystem**: No access by default (WASI can grant specific directory access)
- **Network**: No access by default
- **CPU**: Epoch-based interruption for timeouts
- **Deterministic**: Same input always produces same output (no side effects)

### Caching

WASM modules are compiled once and cached as `wasmtime::Module`. Compilation is expensive (~10-50ms); execution is fast (~microseconds for simple tools). The module cache lives on the `ToolRegistry`.

## New Files

| File | Purpose |
|---|---|
| `src/agents/capabilities.rs` | `AgentCapabilities`, all capability enums, `check_*` methods |
| `src/agents/budget.rs` | `AgentBudgetTracker`, atomic counters, limit enforcement |
| `src/agents/sandbox.rs` | `execute_sandboxed_tool()`, bubblewrap wrapper, config |
| `src/agents/wasm.rs` | `WasmToolExecutor`, wasmtime integration, module cache |

## Modified Files

| File | Change |
|---|---|
| `src/agents/agent.rs` | Add `capabilities: AgentCapabilities`, `budget: Arc<AgentBudgetTracker>` to Agent |
| `src/agents/llm.rs` | Check budget before `stream()`, record usage after |
| `src/agents/tool.rs` | Add `Wasm` variant to `ToolExecutor`, route execution through sandbox/wasm |
| `src/agents/skill.rs` | Parse `wasm = "file.wasm"` from skill.toml |
| `src/agents/error.rs` | Add `BudgetExceeded`, `SandboxError` variants |
| `Cargo.toml` | Add `wasmtime`, `wasmtime-wasi`, `dashmap` dependencies |

## Configuration

Full `puff.toml` example with all sandboxing options:

```toml
[agent_server]
port = 8080

[agent_server.sandbox]
enabled = true
bwrap_path = "/usr/bin/bwrap"
fallback_unsandboxed = true

[postgres]
pool_size = 20
max_per_agent = 5

[redis]
pool_size = 10
max_per_agent = 3

[[agents]]
name = "untrusted-bot"
model = "claude-haiku-4-5"

[agents.capabilities]
sql = "read_only"
http = ["api.example.com"]
tools = ["search", "compute-hash"]

[agents.capabilities.filesystem]
read_only = ["/data/public"]

[agents.capabilities.budget]
max_cost_usd = 0.50
max_tool_calls = 20
max_tool_cpu_seconds = 30.0
```

## Design Principles

1. **Rust boundary IS the sandbox.** No middleware, no proxies, no containers for the common case.
2. **Zero overhead for unconstrained agents.** Default capabilities are permissive. Checks are struct field matches.
3. **Defense in depth.** Capability checks + budget limits + namespace isolation + WASM containment.
4. **Graceful degradation.** Missing bubblewrap → fallback to unsandboxed. No WASM → skip WASM tools.
5. **Observable.** Budget tracker exposes metrics. Sandbox failures are logged and traced.
