# Puff v2: Deep Stack Agent Runtime

**Date:** 2026-03-17
**Author:** Kyle Hanson
**Status:** Draft

## Vision

Puff reimagined for the agentic world. Same thesis — Python simplicity, Rust speed, everything included — but the organizing principle shifts from web applications to agent applications.

Puff becomes the runtime agents run ON. Not another framework wrapping LLM API calls. The actual execution substrate. One binary, one process, zero overhead. Every piece of infrastructure an agent needs — LLM calls, tool execution, memory, multi-agent coordination, streaming, observability — baked in, not bolted on.

## Why Puff

Every agent framework today (LangChain, CrewAI, AutoGen) is pure Python, stitching together HTTP calls to external services, serializing everything, managing state through hope. They treat infrastructure as an afterthought.

Puff already has the infrastructure:

| Agent Need | Puff Already Has |
|---|---|
| Tool execution at speed | Rust functions callable from Python, zero serialization |
| Agent memory (short-term) | Redis, built-in |
| Agent memory (long-term) | Postgres, built-in (add pgvector) |
| Multi-agent communication | Pub/Sub, built-in |
| Background agent work | Distributed task queues, built-in |
| Streaming responses | WebSockets, built-in |
| Agent API layer | HTTP + GraphQL, built-in |
| Concurrent agent execution | Free-threaded Python on Tokio |

## Breaking Change: Free-Threaded Python Replaces Greenlets

Python 3.13+ removes the GIL (PEP 703). Greenlets were a workaround for the GIL — cooperative multitasking to get concurrency despite the lock. With free-threaded Python, we get real parallelism. The entire greenlet executor layer is removed.

Each agent runs on a real OS thread. When it calls into Puff for I/O (LLM, database, Redis), it crosses into Rust via PyO3, dispatches to Tokio, and the thread blocks on a channel until the result returns. Other agent threads keep running in true parallel.

Benefits:
- **True parallelism** for CPU-bound tool execution (PDF parsing doesn't block LLM calls)
- **Simpler Rust code** (greenlet executor was the most complex part of Puff — it's gone)
- **Standard debugging** (Python's threading tools work, no greenlet stack trace weirdness)
- **No greenlet dependency** (one less C extension)
- **Better memory model** (free-threaded Python uses biased reference counting and per-object locks)

PyO3 0.24, which Puff already uses, has free-threaded support built in.

### Architecture

```
┌──────────────────────────────────────────────────┐
│                  Tokio Runtime                    │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐       │
│  │LLM Stream│  │ Postgres │  │  Redis   │       │
│  │(reqwest) │  │(bb8+pgv) │  │(bb8 pool)│       │
│  └────▲─────┘  └────▲─────┘  └────▲─────┘       │
│       │              │             │              │
│       │    ┌─────────┴─────────────┘              │
│       │    │    PyO3 FFI boundary                 │
│  ┌────┴────┴─────────────────────────────────┐   │
│  │   Free-Threaded Python Thread Pool         │   │
│  │                                            │   │
│  │  Thread 1     Thread 2     Thread 3        │   │
│  │  ┌────────┐  ┌────────┐  ┌────────┐       │   │
│  │  │Agent:  │  │Agent:  │  │Agent:  │       │   │
│  │  │support │  │billing │  │research│       │   │
│  │  └────────┘  └────────┘  └────────┘       │   │
│  └───────────────────────────────────────────┘   │
│       │              │             │              │
│  ┌────▼─────┐  ┌─────▼────┐  ┌────▼─────┐       │
│  │ Pub/Sub  │  │Task Queue│  │WebSocket │       │
│  └──────────┘  └──────────┘  └──────────┘       │
└──────────────────────────────────────────────────┘
```

## 1. Core: The Agent Execution Loop

An agent is a loop: receive input, call LLM, maybe execute tools, maybe call another agent, respond.

```python
def agent_loop(agent, conversation):
    while True:
        context = agent.build_context(conversation)
        response = puff.llm.stream(agent.model, context)

        while response.has_tool_calls():
            results = puff.tools.execute(response.tool_calls)
            response = puff.llm.stream(agent.model, context + results)

        agent.memory.save(conversation, response)
        conversation.respond(response)
        next_message = conversation.receive()
```

Every line that touches I/O transparently crosses into Rust via PyO3, dispatches to Tokio, and blocks the Python thread on a channel. Other agent threads keep running. The developer writes straight-line Python code.

## 2. Tool System: Skills + CLI + @tool

Tools are organized into three layers, all unified in one registry.

### The Three Layers

```
┌─────────────────────────────────────────────┐
│              Agent Tool Registry             │
│                                              │
│  ┌──────────────┐  ┌──────────────────────┐ │
│  │  @tool        │  │  Skills              │ │
│  │  (Python/Rust │  │  ┌────────────────┐  │ │
│  │   functions)  │  │  │ CLI commands   │  │ │
│  │               │  │  │ (whitelisted)  │  │ │
│  │  search_db()  │  │  │ gh pr list     │  │ │
│  │  send_email() │  │  │ rg --json      │  │ │
│  │               │  │  ├────────────────┤  │ │
│  │               │  │  │ @tool functions│  │ │
│  │               │  │  │ (bundled .py)  │  │ │
│  │               │  │  ├────────────────┤  │ │
│  │               │  │  │ context.md     │  │ │
│  │               │  │  │ (system prompt)│  │ │
│  │               │  │  └────────────────┘  │ │
│  └──────────────┘  └──────────────────────┘ │
└─────────────────────────────────────────────┘
```

The agent sees one flat list of tools. It doesn't care if a tool is a Python function, a Rust function, or a whitelisted CLI command wrapped by a skill. Puff normalizes everything into the same JSON schema for LLM function calling.

### @tool — Custom Functions

A decorator. Type hints become the schema. Docstring becomes the description.

```python
from puff import tool

@tool
def search_products(query: str, max_results: int = 10) -> list[dict]:
    """Search the product catalog by name or description."""
    db = puff.postgres()
    return db.query(
        "SELECT * FROM products WHERE name ILIKE $1 LIMIT $2",
        [f"%{query}%", max_results]
    )
```

Puff inspects the function at registration time — name, type hints, docstring, default values — and generates the JSON schema. Every `@tool` is also automatically available as a CLI command:

```bash
puff tool search-products --query "widget" --max-results 5
```

### Rust-Native @tool

For performance-critical operations:

```rust
#[puff_tool(description = "Compute similarity between two text chunks")]
fn cosine_similarity(a: Vec<f32>, b: Vec<f32>) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum::<f32>()
        / (a.iter().map(|x| x * x).sum::<f32>().sqrt()
        * b.iter().map(|x| x * x).sum::<f32>().sqrt())
}
```

Same registry. Same schema. The agent doesn't know or care if a tool is Python or Rust.

### Skills — Curated Capability Packages

A skill is a directory containing tool definitions, whitelisted CLI command patterns, and optional context for the agent's system prompt.

```
skills/
  github/
    skill.toml          # tool definitions, whitelisted commands, permissions
    context.md          # knowledge injected into agent system prompt
  database/
    skill.toml
    context.md
  custom-tool/
    skill.toml
    context.md
    helpers.py          # custom @tool functions bundled with the skill
```

#### skill.toml

```toml
[skill]
name = "github"
description = "Interact with GitHub repositories"
version = "1.0.0"

[[tools]]
name = "list-prs"
description = "List open pull requests"
command = "gh pr list --json number,title,author,url"
output = "json"

[[tools]]
name = "view-pr"
description = "View a specific pull request"
command = "gh pr view {number} --json title,body,comments,reviews"
args = { number = { type = "int", description = "PR number" } }
output = "json"

[[tools]]
name = "create-pr"
description = "Create a new pull request"
command = "gh pr create --title {title} --body {body}"
args = { title = { type = "str" }, body = { type = "str" } }
requires_approval = true    # human-in-the-loop for mutations

# Whitelist patterns — ONLY these command shapes are allowed
[permissions]
allow = [
    "gh pr *",
    "gh issue *",
    "gh repo view *",
]
deny = [
    "gh repo delete *",
]
```

#### context.md

Knowledge injected into the agent's system prompt when the skill is loaded:

```markdown
# GitHub Skill Context

When working with pull requests:
- Always check CI status before suggesting a merge
- Link related issues using "Fixes #N" in PR bodies
- PRs with more than 500 lines changed should be flagged for splitting

When triaging issues:
- Label bugs with priority based on user impact
- Check for duplicates before creating new issues
```

### Agent Configuration with Skills

```python
agent = Agent(
    name="dev-agent",
    model="claude-sonnet-4-6",
    skills=[
        "github",                          # built-in skill
        "database",                        # built-in skill
        "./skills/custom-tool",            # local skill directory
        "puff-skills/slack",               # community skill (git repo)
    ],
    tools=[
        my_custom_function,                # raw @tool still works
    ],
)
```

Or in `puff.toml`:

```toml
[[agents]]
name = "dev-agent"
model = "claude-sonnet-4-6"
skills = ["github", "database", "./skills/custom-tool"]
```

### CLI-First Interface

Puff itself is CLI-first. Agents and tools are all accessible from the command line:

```bash
# Talk to an agent
echo "I was double-charged" | puff agent ask support-desk

# Pipe data through an agent
cat report.csv | puff agent ask analyst "summarize this data"

# Agent tools are CLI tools — composable with Unix pipes
puff tool search-products --query "widget" | puff tool format-table
```

### Security Model

The whitelist patterns are enforced in Rust:

1. Agent requests tool call `"list-prs"`
2. Puff resolves it to skill `github`, command `gh pr list --json ...`
3. Rust checks the command against `permissions.allow` patterns
4. Rust checks against `permissions.deny` patterns
5. If `requires_approval = true`, pause for human approval
6. Spawn process via `tokio::process`, capture output
7. Parse output (json/text/csv) and return to agent

An agent can never execute a command that isn't whitelisted by its loaded skills. No shell injection. No surprise `rm -rf`. The Rust boundary is the security boundary.

### Parallel Tool Execution

When an LLM returns multiple tool calls, Puff executes them in parallel across threads:

```
LLM response: [call search_products, call check_inventory, call get_pricing]
                        │                    │                    │
                   Thread A             Thread B             Thread C
                        │                    │                    │
                   [results collected, sent back to LLM in next turn]
```

### Skill Distribution

Skills are just directories. They can live:
- **Built-in** — shipped with Puff (`skills/github`, `skills/database`, etc.)
- **Local** — in your project (`./skills/my-tool/`)
- **Git** — pulled from any git repo (`puff skill install github.com/user/puff-slack-skill`)
- **MCP bridge** — `mcp-cli` wrapped as a skill for backward compat

```bash
puff skill install github.com/puff-skills/slack
puff skill install github.com/puff-skills/jira
puff skill list
puff skill info github
```

### Built-in Tools

Puff's deep stack provides tools out of the box, available as both `@tool` functions and built-in skills:

| Tool | What it does | Backed by |
|---|---|---|
| `puff.tools.sql` | Query databases | Postgres pool |
| `puff.tools.search` | Semantic vector search | pgvector |
| `puff.tools.http` | Make HTTP requests | reqwest |
| `puff.tools.cache` | Read/write cache | Redis |
| `puff.tools.publish` | Send messages to other agents | Pub/Sub |
| `puff.tools.enqueue` | Schedule background work | Task Queue |
| `puff.tools.embed` | Generate embeddings | LLM gateway |

Direct Rust calls into infrastructure Puff owns. Zero network hop. Zero serialization.

### Permissions (per-agent)

Enforced at the Rust boundary — Python code cannot bypass:

```python
agent = Agent(
    name="reader-bot",
    tools=[search_products, puff.tools.sql, puff.tools.http],
    permissions=Permissions(
        sql="read_only",
        http=["api.stripe.com", "api.sendgrid.com"],
        filesystem=None,
    ),
)
```

## 3. Memory System

### Three Tiers

**Tier 1 — Conversation History (Redis)**

The current thread of messages. Stored in Redis for fast access. Managed automatically by Puff.

When conversations get long, Puff handles context window pressure:
- Recent turns stay verbatim
- Older turns get summarized (cheap LLM call via the gateway)
- Token budget tracked and enforced automatically

Redis keys:
```
puff:conv:{id}:messages    → ordered message list
puff:conv:{id}:summary     → rolling summary of older turns
puff:conv:{id}:token_count → running token budget
```

**Tier 2 — Long-Term Memory (Postgres + pgvector)**

Facts that survive across conversations. Stored as text + embedding vector.

```sql
CREATE TABLE puff_memories (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent       TEXT NOT NULL,
    scope       TEXT NOT NULL,       -- 'user', 'agent', 'global'
    scope_id    TEXT,                -- e.g. user_id
    content     TEXT NOT NULL,
    embedding   VECTOR(1536),
    importance  FLOAT DEFAULT 0.5,
    created_at  TIMESTAMPTZ DEFAULT NOW(),
    accessed_at TIMESTAMPTZ DEFAULT NOW()
);
```

On every turn, Puff embeds the incoming message and does a vector similarity search to pull in relevant memories. They appear in the agent's context automatically.

**Tier 3 — Working Memory (In-Thread)**

Agent's scratchpad during a single task. Lives in Python thread-local state, optionally backed by Redis. Gone when the task is done.

### Developer API

Simple:
```python
agent = Agent(
    name="support",
    memory=True,  # Redis for conversations, Postgres for long-term, sane defaults
)
```

Full control:
```python
agent = Agent(
    name="support",
    memory=Memory(
        conversation="redis",
        long_term="postgres",
        auto_extract=True,
        recall_k=10,
        summarize_after=50,
    ),
)
```

Explicit memory management as tools:
```python
@tool
def remember(fact: str, scope: str = "user"):
    """Save something to long-term memory."""
    puff.memory.save(fact, scope=scope, scope_id=conversation.user_id)

@tool
def recall(query: str, k: int = 5) -> list[str]:
    """Search long-term memory."""
    return puff.memory.search(query, k=k, scope_id=conversation.user_id)
```

### Auto-Extraction

After each conversation, Puff optionally runs a lightweight extraction pass (Haiku-class call):

```
Conversation: "My name is Sarah, I'm on the enterprise plan,
              and I really hate getting SMS notifications."

Extracted:
  → "User's name is Sarah" (scope: user)
  → "User is on enterprise plan" (scope: user)
  → "User dislikes SMS notifications" (scope: user)
```

Agents get smarter over time without the developer writing extraction logic.

## 4. LLM Gateway

Rust-native, multi-provider LLM client. Every LLM call happens in Rust — Python says "talk to Claude" and blocks on a channel. Rust opens an HTTP/2 stream via reqwest, processes SSE tokens, handles tool call parsing, counts tokens, all at native speed.

### Multi-Provider

```python
agent_a = Agent(name="fast",  model="claude-haiku-4-5")
agent_b = Agent(name="smart", model="claude-opus-4-6")
agent_c = Agent(name="local", model="ollama/llama3")
agent_d = Agent(name="gpt",   model="gpt-4o")
```

Puff normalizes all provider differences (API formats, tool call schemas, streaming chunk formats, stop reasons) at the Rust boundary.

Configuration:
```toml
[llm]
default_model = "claude-sonnet-4-6"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.openai]
api_key_env = "OPENAI_API_KEY"

[llm.providers.ollama]
base_url = "http://localhost:11434"
```

### Streaming

Two modes:

**Blocking** — full response when done:
```python
response = puff.llm.chat(model, messages)
```

**Streaming** — tokens as they arrive:
```python
for chunk in puff.llm.stream(model, messages):
    if chunk.text:
        websocket.send(chunk.text)
```

When served through the agent server, streaming to clients happens automatically in Rust. The LLM stream gets forked — one copy to the Python agent loop for tool execution, one directly to the client WebSocket. No Python in the hot path for token delivery.

### Caching

Redis-backed. Checked in Rust before making API calls.

```toml
[llm.cache]
enabled = true
backend = "redis"
ttl = 3600
strategy = "exact"   # or "semantic"
```

### Cost Tracking

Every token counted, every call priced, stored in Postgres:

```sql
CREATE TABLE puff_llm_usage (
    id             UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    agent          TEXT NOT NULL,
    conversation   UUID,
    provider       TEXT NOT NULL,
    model          TEXT NOT NULL,
    input_tokens   INT NOT NULL,
    output_tokens  INT NOT NULL,
    cost_usd       NUMERIC(10, 6),
    latency_ms     INT,
    cached         BOOLEAN DEFAULT FALSE,
    created_at     TIMESTAMPTZ DEFAULT NOW()
);
```

### Rate Limiting & Resilience

Handled in Rust, per-provider:
- Token-per-minute and request-per-minute limits
- Automatic exponential backoff on 429s
- Provider fallback chains
- Request queuing with priority

```toml
[llm.rate_limits]
anthropic = { rpm = 1000, tpm = 100000 }

[llm.fallback]
"claude-sonnet-4-6" = ["gpt-4o"]
```

### Embeddings

Same gateway:
```python
vector = puff.embed("What's the refund policy?")
vectors = puff.embed_batch(["doc1...", "doc2...", "doc3..."])
```

## 5. Orchestration

### Four Multi-Agent Patterns

**Router** — classify and dispatch:

```python
from puff import Agent, Router

support = Router(
    name="support-desk",
    agents=[billing, technical, sales],
    router_prompt="Route billing issues to billing, technical to technical, upgrades to sales.",
    model="claude-haiku-4-5",
)
```

Single cheap LLM call classifies the message. Selected specialist runs on a new thread.

**Supervisor** — delegate, review, synthesize:

```python
from puff import Agent, Supervisor

content_team = Supervisor(
    name="content-team",
    supervisor=Agent(name="editor", system_prompt="Manage research, writing, fact-checking..."),
    workers=[researcher, writer, fact_checker],
)
```

Supervisor agent gets an auto-generated `delegate` tool. Workers execute on separate threads in true parallel.

**Chain** — sequential pipeline:

```python
from puff import Agent, Chain

pipeline = Chain(
    name="report-pipeline",
    agents=[researcher, analyst, writer],
)
report = pipeline.run("Q4 market trends in AI infrastructure")
```

**Parallel** — simultaneous execution, results merged:

```python
from puff import Agent, Parallel

scan = Parallel(
    name="security-scan",
    agents=[code_reviewer, dep_auditor, config_checker],
    merge_prompt="Combine findings into a prioritized report.",
)
```

### Agent-to-Agent Communication

All multi-agent communication runs through Puff's pub/sub (Redis). Agents can also communicate explicitly:

```python
@tool
def ask_expert(question: str, expert: str) -> str:
    """Ask another agent a question."""
    return puff.agents.invoke(expert, question)
```

Goes through pub/sub. The expert could be on a different thread, process, or machine.

### Handoff

Mid-conversation transfer with full context:

```python
@tool
def transfer_to(agent_name: str, reason: str):
    """Transfer this conversation to another agent."""
    puff.conversation.handoff(agent_name, reason=reason)
```

### Human-in-the-Loop

```python
@tool
def request_human_approval(action: str, details: str) -> bool:
    """Pause and wait for human approval."""
    return puff.human.approve(action, details, timeout=3600)
```

Sends a WebSocket event, pauses the agent thread, resumes on human response.

## 6. Agent Server

One command starts everything:

```bash
puff agent my_agents:support_desk --port 8080
```

### Endpoints

| Endpoint | Protocol | Purpose |
|---|---|---|
| `POST /api/conversations` | REST/SSE | Start a conversation |
| `POST /api/conversations/:id` | REST/SSE | Continue a conversation |
| `GET /api/conversations/:id` | REST | Get conversation history |
| `GET /api/agents` | REST | List agents and tools |
| `/ws/conversations/:id` | WebSocket | Real-time bidirectional streaming |
| `POST /graphql` | GraphQL | Introspection and analytics queries |
| `POST /cli/:tool` | REST | CLI-over-HTTP tool execution |
| `GET /health` | REST | Health check |
| `GET /metrics` | REST | Prometheus metrics |

All on Axum. All in one process.

### CLI-First

The agent server itself is CLI-first:

```bash
# Interactive REPL
puff agent ask support-desk

# Pipe input
echo "I was double-charged" | puff agent ask support-desk

# Pipe data through agents
cat report.csv | puff agent ask analyst "summarize this"

# Tools are CLI commands
puff tool search-products --query "widget" | puff tool format-table
```

### Authentication

```toml
[agent_server.auth]
type = "bearer"
secret_env = "PUFF_AUTH_SECRET"
```

### Full Configuration

```toml
[puff]
name = "acme-support"

[postgres]
url = "postgresql://localhost/acme"
enable_pgvector = true
max_connections = 20

[redis]
url = "redis://localhost"

[llm]
default_model = "claude-sonnet-4-6"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.openai]
api_key_env = "OPENAI_API_KEY"

[llm.cache]
enabled = true
ttl = 3600

[memory]
auto_extract = true
recall_k = 10
summarize_after = 50

[agent_server]
port = 8080
websockets = true
graphql = true
cors_origins = ["http://localhost:3000"]

[agent_server.auth]
type = "bearer"
secret_env = "PUFF_AUTH_SECRET"
```

### CLI Commands

```bash
puff agent <module>:<agent>       # serve an agent
puff agent ask <agent>            # interactive REPL
puff agents list                  # list defined agents
puff agents bench <agent>         # run evaluation suite
puff skill install <source>       # install a skill from git
puff skill list                   # list installed skills
puff skill info <name>            # show skill details
puff tool <name> [args]           # run a tool directly
puff memory search <query>        # search long-term memories
puff memory stats                 # memory usage statistics
puff usage report --last 7d       # cost/usage report
```

## 7. Observability

### Traces

Every agent turn produces a structured trace stored in Postgres:

```json
{
    "trace_id": "tr_abc123",
    "agent": "support-desk",
    "conversation": "conv_xyz",
    "turns": [
        {"type": "router", "decision": "billing", "latency_ms": 180},
        {"type": "llm_call", "model": "claude-haiku-4-5", "input_tokens": 1200, "output_tokens": 85, "latency_ms": 420},
        {"type": "tool_call", "tool": "lookup_invoice", "latency_ms": 12},
        {"type": "llm_call", "model": "claude-haiku-4-5", "input_tokens": 1625, "output_tokens": 210, "cost_usd": 0.00043}
    ],
    "total_latency_ms": 1292,
    "total_cost_usd": 0.00061
}
```

### Analytics via GraphQL

```graphql
query {
    agentMetrics(last: "24h") {
        agent
        totalConversations
        avgLatencyMs
        totalCostUsd
        toolUsage { name, callCount, avgLatencyMs, errorRate }
    }
}
```

### Evaluation

Built-in eval framework:

```python
from puff.eval import EvalSuite, Case

suite = EvalSuite(
    agent=support_agent,
    cases=[
        Case(
            input="I was charged twice for order #123",
            expect_tool_calls=["lookup_invoice"],
            expect_contains="refund",
        ),
        Case(
            input="How do I reset my password?",
            expect_handoff="technical",
        ),
    ],
)
results = suite.run(model="claude-haiku-4-5")
```

```bash
$ puff agents bench my_agents:support_desk

Running 24 eval cases...
  ✓ double-charge lookup    420ms  $0.0003
  ✓ password reset handoff  310ms  $0.0002
  ✗ spanish language        680ms  $0.0004

Results: 23/24 passed (95.8%)
Total cost: $0.0089
```

## What Changes in the Codebase

### Removed
- `src/python/async_python.rs` — greenlet executor (replaced by free-threaded Python threads)
- Greenlet dependencies from `Cargo.toml`

### Modified
- `src/python/mod.rs` — new agent Python bindings, free-threaded bootstrap
- `src/databases/postgres.rs` — add pgvector support
- `src/web/server.rs` — add agent API endpoints
- `src/graphql/` — add agent introspection queries
- `src/program/mod.rs` — add agent and skill commands
- `src/main.rs` — parse agent config from `puff.toml`
- `Cargo.toml` — add new dependencies, bump to v2.0

### New Modules
- `src/agents/mod.rs` — Agent definition, execution loop
- `src/agents/tool.rs` — Tool registry, schema generation
- `src/agents/skill.rs` — Skill loading, CLI command whitelisting, permission enforcement
- `src/agents/memory.rs` — Three-tier memory system
- `src/agents/conversation.rs` — Conversation/thread management
- `src/agents/orchestration.rs` — Router, Supervisor, Chain, Parallel
- `src/agents/llm.rs` — Multi-provider LLM gateway
- `src/agents/streaming.rs` — Token streaming, SSE, stream forking
- `src/agents/eval.rs` — Evaluation framework
- `src/agents/trace.rs` — Observability and tracing

### New Dependencies
- `pgvector` — vector similarity search for Postgres
- Provider-specific API types (minimal — mostly reqwest + serde)

## Design Principles

1. **One binary, one process.** No microservices. No Docker-compose. Everything runs together.
2. **Rust does the I/O, Python does the logic.** The boundary is clear and enforced.
3. **Invisible infrastructure.** Developers define agents and tools. Puff handles memory, streaming, routing, tracing.
4. **Free-threaded Python.** Real parallelism. No GIL workarounds. No greenlets.
5. **Skills + CLI native.** Tools are skills with whitelisted CLI commands. Every tool is a CLI command. CLI is the universal interface.
6. **Observable by default.** Every call traced, every token counted, every dollar tracked.

## Startup

```
$ puff agent my_agents:support_desk

  ╔══════════════════════════════════════════╗
  ║          P U F F  v2.0                   ║
  ║    Deep Stack Agent Runtime              ║
  ╠══════════════════════════════════════════╣
  ║                                          ║
  ║  Agents:     support-desk (router)       ║
  ║              ├── billing (haiku)         ║
  ║              ├── technical (sonnet)      ║
  ║              └── sales (sonnet)          ║
  ║                                          ║
  ║  Skills:     github, database, slack     ║
  ║  Tools:      18 registered               ║
  ║  Memory:     redis + postgres/pgvector   ║
  ║  LLM:        anthropic (primary)         ║
  ║              openai (fallback)           ║
  ║                                          ║
  ║  REST API:   http://localhost:8080/api    ║
  ║  WebSocket:  ws://localhost:8080/ws       ║
  ║  GraphQL:    http://localhost:8080/graphql║
  ║  CLI:        puff agent ask support-desk ║
  ║                                          ║
  ║  Postgres:   ✓ connected (pgvector on)   ║
  ║  Redis:      ✓ connected                 ║
  ║  Cache:      ✓ enabled                   ║
  ║                                          ║
  ╚══════════════════════════════════════════╝

  Ready. Waiting for conversations...
```

One process. One binary. Rust speed. Python ease. Everything agents need, nothing they don't.
