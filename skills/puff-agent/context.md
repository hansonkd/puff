# Puff Agent Skill

You are working with a Puff application — a deep-stack Python/Rust runtime with built-in GraphQL, Postgres, Redis, and AI agent orchestration.

## GraphQL

Puff's GraphQL engine runs on async-graphql with layer-based resolution. Schemas are defined in Python using dataclasses.

### Querying Data

Use the `graphql_query` tool to read and write data:

```
graphql_query(query: "{ users { id name email } }")
graphql_query(query: "mutation { createUser(name: \"Alice\") { id } }")
graphql_query(query: "{ user(id: $id) { name } }", variables: "{\"id\": 1}")
```

### Understanding the Schema

Use `graphql_schema` to get the full schema in SDL format before writing queries. This tells you exactly what types, fields, and arguments are available.

### Query Tips

- GraphQL variables use `$name` syntax in the query and are passed as a JSON object
- Mutations that modify data use `mutation { ... }` syntax
- Fields with `!` are non-nullable
- `[Type!]!` means a non-null list of non-null items
- Use `graphql_validate` to check query syntax before executing

## Python Schema Definition

Puff schemas are defined as Python dataclasses:

```python
from dataclasses import dataclass
from typing import List, Tuple, Any
from puff.graphql import sql

@dataclass
class UserObject:
    id: int
    name: str
    email: str

@dataclass
class Query:
    @classmethod
    @sql("SELECT id, name, email FROM users")
    def users(cls, context, /) -> Tuple[List[UserObject], str, List[Any]]:
        ...

    @classmethod
    @sql("SELECT id, name, email FROM users WHERE id = $1", args=["id"])
    def user_by_id(cls, context, /, id: int) -> Tuple[UserObject, str, List[Any]]:
        ...
```

### The @sql Decorator

Fields with `@sql` execute directly in Rust — zero Python overhead at request time. Use it for all static SQL queries:

- `@sql("SELECT ...")` — static query, no parameters
- `@sql("SELECT ... WHERE id = $1", args=["id"])` — maps GraphQL arg `id` to `$1`
- `@sql("SELECT ... WHERE parent_id = ANY($1)", parent_key=["id"], child_key=["parent_id"])` — correlated child field for N+1 prevention

Fields WITHOUT `@sql` call the Python method at request time — use for dynamic queries.

### Return Type Conventions

Producer methods return a tuple where the first element is `...` (ellipsis):

- `(ellipsis, sql_query, params)` — SQL query with params list
- `(ellipsis, sql_query, params, parent_keys, child_keys)` — correlated child query
- `(ellipsis, python_list)` — Python list of objects

## Database Access

### Postgres

```python
import puff

conn = puff.postgres()           # default connection
conn = puff.postgres("readonly") # named connection

cursor = conn.cursor()
cursor.execute("SELECT * FROM users WHERE id = %s", [42])
rows = cursor.fetchall()         # list of tuples
cursor.description               # column metadata

cursor.execute("INSERT INTO users (name) VALUES (%s)", ["Alice"])
conn.commit()
```

- Parameter placeholders: `%s` (converted to `$1`, `$2` automatically)
- Prepared statements are cached per connection
- Connections come from a bb8 pool — don't hold them longer than needed

### Redis

```python
from puff.redis import global_redis

redis = global_redis
redis.get(b"key")
redis.set(b"key", b"value", ex=3600)  # with TTL
redis.delete(b"key")
redis.incr(b"counter", 1)
```

### PubSub

```python
from puff.pubsub import global_pubsub

pubsub = global_pubsub
conn = pubsub.connection()
conn.subscribe("channel_name")
msg = conn.receive()  # blocks until message
print(msg.text, msg.from_connection_id)
conn.publish("channel_name", "hello")
```

## Agent Orchestration

Puff has built-in multi-agent patterns:

- **Router** — dispatches to specialist agents based on message classification
- **Supervisor** — delegates tasks to workers, reviews output
- **Chain** — sequential pipeline, each agent's output feeds the next
- **Parallel** — runs agents simultaneously, optionally merges results

## Configuration (puff.toml)

```toml
[llm]
default_model = "claude-sonnet-4-6"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[[postgres]]
name = "default"

[[redis]]
name = "default"

[[graphql]]
schema = "my_app.Schema"
url = "/graphql/"

[[agents]]
name = "assistant"
model = "claude-sonnet-4-6"
skills = ["./skills/puff-agent"]
```

## Performance Notes

- `@sql` fields bypass Python entirely — Postgres → Rust → GraphQL response
- GraphQL statement cache is shared across requests (prepared once, reused)
- Free-threaded Python (3.13t) enables true parallelism — each request gets its own thread
- Connection pools are bounded — don't hold connections in long-running operations
- Use `fetchall()` for small results, iterate with `fetchone()` for large ones
