# 0.2.0

Puff v2: Deep Stack Agent Runtime.

* **BREAKING:** Remove greenlet executor. Free-threaded Python (3.13+) replaces greenlets for parallelism.
* Add AI Agent runtime with LLM-powered agent execution loop
* Add multi-provider LLM Gateway (Anthropic, OpenAI, Ollama) with SSE streaming in Rust
* Add retry with exponential backoff and provider fallback chains
* Add embedding generation (embed, embed_batch)
* Add Tool system with ToolRegistry and CLI tool execution
* Add Skills — curated capability packages with TOML config, whitelisted CLI commands, and permission enforcement
* Add three-tier memory: Redis conversation history, Postgres+pgvector long-term memory, working memory
* Add multi-agent orchestration: Router, Supervisor, Chain, Parallel (real tokio::spawn)
* Add mid-conversation agent Handoff
* Add Agent Server with REST API (5 endpoints) and WebSocket streaming with human-in-the-loop approval
* Add observability: structured traces, cost estimation, Postgres persistence for usage and traces
* Add evaluation framework with contains, regex, tool_call, and semantic assertions
* Add CLI commands: puff agent, agent-ask, agent-list, skill-list
* Add Python bindings: puff.Agent(), puff.ToolDef() via PyO3
* Add pgvector support for semantic memory search

# 0.1.9

* Add layer cache for Graphql
* Expose required columns for children, Graphql
* Bump Axum version

# 0.1.8

* Improve Postgres DBAPI2.0 Compliance.
* Add multiple simultaneous Postgres, Redis, PubSub, Graphql Configurations
* Improve Graphql Resolution - Error if returning null in a non-null field.

# 0.1.7

* Improve GraphQL return type compatibility and subscription URLs.
* Update to pyo3 0.17
* Rename runserver to serve
* Don't require Postgres for GraphQL
* Add CORS, Compression support.
* Add puff-watch

# 0.1.6

* Add Puff CLI

# 0.1.5

* Improve Startup coordination of services.

# 0.1.4

* Improve Redis compatibility
* Improve task queue responsiveness
* Add bytes version of json serialization

# 0.1.3

* Performance improvements

# 0.1.2

* HTTP Client
* Distributed TaskQueue
* Rust based JSON serialization, using hyperjson

# 0.1.1

* Async python dispatcher
* Async python graphql handlers
* ASGI handlers
* uvloop

# 0.1.0

Initial Release