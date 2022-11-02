# Building an RPC with Puff

Puff can distribute tasks among nodes.

# Step 1: Setup Puff Environment

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

Update `my_puff_proj/src/main.rs`

```rust
use puff_rs::prelude::*;
use puff_rs::program::commands::{WaitForever, PythonCommand};

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .set_asyncio(true)
        .set_redis(true)
        .set_task_queue(true);

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

Update `my_puff_proj/my_puff_proj_py/__init__.py`


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


# Step 2: Improving responsiveness

`wait_for_task_result` polls for a result on an interval. This can cause a small delay to retrieve the result. Instead, you can use the `blpop` pattern to build a more responsive RPC.

Update your code, to instead store None as the Task result and the real result is sent with `lpush`

```python
import secrets
from puff.task_queue import global_task_queue as task_queue
from puff.redis import global_redis as redis
from puff.json import dumpb


def put_task_in_queue():
    this_channel = secrets.token_hex(6)
    task_queue.schedule_function(example_task_lpush_async, {"channel": this_channel, "hello": "world"}, scheduled_time_unix_ms=1)

    result = redis.blpop(this_channel, 0)
    print(f"{task} returned {result}")


async def example_task_lpush_async(payload):
    channel = payload["channel"]
    # Do some work here...
    await redis.lpush(channel.encode("utf8"), dumpb({"my_result": f"lpop-{payload['hello']}"}))
    return None
```