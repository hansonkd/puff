# Building an RPC with Puff

Puff can distribute tasks among nodes. These tasks can be triggered from any puff instance, including your server or the command line.

This tutorial will show how to trigger tasks from the command line and strategies for returning the results.

# Setup Puff Environment

Install and run Redis in the system or as part of docker. Next setup your Puff project.

```bash
cargo new my_puff_proj --bin
cd my_puff_proj
cargo add puff-rs
poetry new my_puff_proj_py
cd my_puff_proj_py
poetry add puff-py
```

and add cargo plugin to `my_puff_proj/my_puff_proj_py/pyproject.toml`

```toml
[tool.poetry.scripts]
run_cargo = "puff.poetry_plugins:run_cargo"
```

# Option 1

The first option is to use `task_queue.wait_for_task_result` to get the result from a task. This is the simplest built-in method.

Update `my_puff_proj/src/main.rs`

```rust
use puff_rs::prelude::*;
use puff_rs::program::commands::{WaitForever, PythonCommand};

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .set_asyncio(true)
        .add_default_task_queue();

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(PythonCommand::new(
            "queue_task",
            "task_queue_example.put_task_in_queue",
        ))
        .command(WaitForever)
        .run()
}
```

And update `my_puff_proj/my_puff_proj_py/__init__.py`

```python
from puff.task_queue import global_task_queue

task_queue = global_task_queue


def put_task_in_queue():
    task = task_queue.schedule_function(my_awesome_task_async, {"hello": "world"})

    result = task_queue.wait_for_task_result(task)
    print(f"{task} returned {result}")


async def my_awesome_task_async(payload):
    print(f"In async task {payload}")
    return payload["hello"]
```

Now run `poetry run run_cargo wait_forever` in one or more terminals and `poetry run run_cargo queue_task` in another terminal repeatedly. Your tasks should be distributed across all listening nodes.


# Option 2: Improving responsiveness

`wait_for_task_result` polls for a result on an interval. This can cause a small delay to retrieve the result. This is because by default, Puff can't know whether you will clean up your tasks, so it stores all task results as a Redis value with a timeout. This value can only be polled and cannot be read in a "blocking" way.

Instead, you can use the `blpop` pattern to build a more responsive RPC where you know you will consume the result.

First enable redis in Puff:

```rust
    let rc = RuntimeConfig::default()
        .set_asyncio(true)
        .add_default_task_queue()
        .add_default_redis;
```

Update your code so that it instead stores `None` as the Task result and the real result is sent with `lpush`.

```python
import secrets
from puff.task_queue import global_task_queue as task_queue
from puff.redis import global_redis as redis
from puff.json import dumpb


def put_task_in_queue():
    # Generate a random channel name to send the task.
    this_channel = secrets.token_hex(6)
    task = task_queue.schedule_function(example_task_lpush_async, {"channel": this_channel, "hello": "world"}, scheduled_time_unix_ms=1)

    # Block forever waiting for the task result
    result = redis.blpop(this_channel, 0)
    print(f"{task} returned {result}")


async def example_task_lpush_async(payload):
    channel = payload["channel"]
    # Do some work here...
    await redis.lpush(channel.encode("utf8"), dumpb({"my_result": f"lpop-{payload['hello']}"}))
    return None
```

# Option 3: Utilize Pubsub Channels

Only one listener can retrieve the result with `blpop`, but sometimes multiple tasks need the result at the same time. You can use pubsub to accomplish this scenario.

Puff efficiently handles Pubsub for you, utilzing 1 reader connection per node, so that multiple tasks listening to channels on the same node, Puff smartly utilize Redis connections between them.

First enable redis in Puff:

```rust
    let rc = RuntimeConfig::default()
        .set_asyncio(true)
        .add_default_task_queue()
        .add_default_pubsub()
;
```

Now update your code to subscribe to channels:

```python
import secrets
from puff.task_queue import global_task_queue as task_queue
from puff.pubsub import global_pubsub as pubsub


def put_task_in_queue():
    # Generate a random channel name to send the task.
    this_channel = secrets.token_hex(6)
    conn = pubsub.connection()
    conn.subscribe(this_channel)
    listener = task_queue.schedule_function(listen_for_result, {"channel": this_channel}, scheduled_time_unix_ms=1)
    task = task_queue.schedule_function(example_task_lpush_async, {"channel": this_channel, "hello": "world"})

    # Block forever waiting for the task result
    result = conn.receive()
    data = result.json()
    print(f"{task} returned {data}")
    task_queue.wait_for_task_result(listener)


async def listen_for_result(payload):
    channel = payload["channel"]
    conn = pubsub.connection()
    await conn.subscribe(channel)
    result = await conn.receive()
    data = result.json()
    print(f"listener got {data}")
    return None


async def example_task_lpush_async(payload):
    channel = payload["channel"]
    # Do some work here...
    await pubsub.publish_json_as(pubsub.new_connection_id(), channel, {"my_result": f"pubsub-{payload['hello']}"})
    return None
```


# Bonus: Graphql Tasks

You can access all Puff resources from inside your tasks, including a configured GraphQL schema. This is useful if you want to make queries or reuse your logic from GraphQL in your tasks.

First add postgres and the gql_schema_class to your config in Rust.

```rust
    let rc = RuntimeConfig::default()
        .set_asyncio(true)
        .add_default_redis()
        .add_default_pubsub()

        .add_default_postgres()
        .set_gql_schema("my_puff_proj_py.Schema")
        .add_default_task_queue();
```

Now setup the GraphQL schema and modify your task function to call GrapQL.

```python
import secrets
from dataclasses import dataclass
from puff.task_queue import global_task_queue as task_queue
from puff.pubsub import global_pubsub as pubsub
from puff.graphql import global_graphql as gql
from puff import sleep_ms


@dataclass
class HelloWorldResult:
    message: str
    original_value: int


@dataclass
class Query:
    @classmethod
    def hello_world(cls, context, /, my_input: int) -> HelloWorldResult:
        return HelloWorldResult(
            message=f"You said {my_input}",
            original_value=my_input
        )


@dataclass
class Mutation:
    pass


@dataclass
class Subscription:
    pass


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription


GQL_QUERY = """
query HelloWorld($my_input: Int!) {
    hello_world(my_input: $my_input) {
       message
       original_value
   }
}
"""

def put_task_in_queue():
    # Generate a random channel name to send the task.
    this_channel = secrets.token_hex(6)
    conn = pubsub.connection()
    conn.subscribe(this_channel)
    task = task_queue.schedule_function(example_task_lpush_async, {"channel": this_channel, "query": GQL_QUERY, "variables": {"my_input": 56}})

    # Block forever waiting for the task result
    result = conn.receive()
    data = result.json()
    print(f"{task} returned {data}")
    task_queue.wait_for_task_result(listener)


async def example_task_lpush_async(payload):
    channel = payload["channel"]
    query = payload["query"]
    variables = payload["variables"]

    # Query the configured GQL schema.
    result = await gql.query(query, variables)

    await pubsub.publish_json_as(pubsub.new_connection_id(), channel, result)
    return None
 ```