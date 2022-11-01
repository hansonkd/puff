# ☁ Puff ☁
The deep stack framework.

[![Crates.io][crates-badge]][crates-url]
[![MIT licensed][mit-badge]][mit-url]
[![Documentation](https://docs.rs/puff-rs/badge.svg)](https://docs.rs/puff_rs)

[crates-badge]: https://img.shields.io/crates/v/puff_rs.svg
[crates-url]: https://crates.io/crates/puff_rs
[mit-badge]: https://img.shields.io/badge/license-MIT-blue.svg
[mit-url]: https://github.com/hansonkd/puff/blob/master/LICENSE

- [What is Puff?](#what-is-puff)
  * [Quick Start](#quick-start)
  * [Puff ♥ Python](#puff--python)
  * [Puff ♥ Rust](#puff--rust)
  * [Puff ♥ Django](#puff--django)
  * [Puff ♥ Graphql](#puff--graphql)
  * [Puff ♥ Pytest](#puff--pytest)
  * [Puff ♥ AsyncIO](#puff--asyncio)
  * [Puff ♥ Django + GraphQL](#puff--django--graphql)
  * [Puff ♥ Tasks](#puff--tasks)
  * [Puff ♥ HTTP](#puff--http)
  * [FAQ](#faq)
      - [Puff Dependencies](#why-a-monolithic-project)
      - [Architecture](#architecture)
      - [Why is Greenlet environment single threaded?](#why-is-greenlet-environment-single-threaded)
      - [What is a Deep Stack Framework?](#what-is-a-deep-stack-framework)
      - [Deep Stack Teams](#deep-stack-teams)
      - [Benefits of Deep Stack](#benefits-of-deep-stack)
      
# What is Puff?

Puff is a batteries included "deep stack" for Python. It's an experiment to minimize the barrier between Python and Rust to unlock the full potential of high level languages. Build your own Runtime using standard CPython and extend it with Rust. Imagine if GraphQL, Postgres, Redis and PubSub were part of the standard library. That's Puff.

The old approach for integrating Rust in Python would be to make a Python package that uses rust and import it from Python. This approach has some flaws as the rust packages can't cooperate. Puff gives Rust its own layer, so you can build a cohesive set of tools in Rust that all work flawlessly together without having to re-enter Python.

High level overview is that Puff gives Python

* Greenlets on Rust's Tokio.
* High performance HTTP Server - combine Axum with Python WSGI apps (Flask, Django, etc.)
* Rust / Python natively in the same process, no sockets or serialization.
* AsyncIO / uvloop / ASGI integration with Rust
* An easy-to-use GraphQL service
* Multi-node pub-sub
* Rust level Redis Pool
* Rust level Postgres Pool
* Websockets
* HTTP Client
* Distributed, at-least-once, priority and scheduled task queue
* semi-compatible with Psycopg2 (hopefully good enough for most of Django)
* A safe convenient way to drop into rust for maximum performance

The idea is Rust and Python are near perfect complements to each other and building a framework to let them talk
leads to greater efficiency in terms of productivity, scalability and performance.

| Python                                             | Rust                                            |
|----------------------------------------------------|-------------------------------------------------|
| ✅ High-Level                                       | ✅ Low-Level                                     |
| ✅ Lots of tools and packages                       | ✅ Lots of tools and packages                    |
| ✅ Easy to get started                              | ✅ Easy to get started                           |
| 🟡 Interpreted (productivity at the cost of speed) | 🟡 Compiled (speed at the cost of productivity) |
| ✅ Easy to get master                               | ❌ The learning curve gets steep quickly.        |
| ✅ Fast iteration to prototype                      | ❌ Requires planning for correctness             |
| ✅ Google a problem, copy paste, it works.          | ❌ Less examples floating in the wild            |
| ❌ Weak type system                                 | ✅ Great Type System                             |
| ❌ GIL prevents threading                           | ✅ High Performance                              |
| ❌ Not-so safe                                      | ✅ Safe                                          |


The Zen of deepstack is recognizing that no language is the ultimate answer. Seek progress instead of perfection by using Python for rapid development and Rust to optimize the most critical paths once you find them later. Find the balance. 

## Quick Start
Puff requires Python >= 3.10, Rust / Cargo. Python's [Poetry](https://python-poetry.org) is optional.

Your Rust Puff Project needs to find your Python project. Even if they are in the same folder, they need to be added to the PYTHONPATH.

One way to set up a Puff project is like this:

```bash
cargo new my_puff_proj --bin
cd my_puff_proj
cargo add puff-rs
poetry new my_puff_proj_py
cd my_puff_proj_py
poetry install puff-py
```

And add the puff plugin for poetry.

```toml title="/app/my_puff_proj/my_puff_proj_py/pyproject.toml"
[tool.poetry.scripts]
cargo = "puff.poetry_plugins:cargo"
```

Now from `my_puff_proj_py` you can run your project with `poetry run cargo` to access cargo from poetry and expose the virtual environment to Puff.

The Python project doesn't need to be inside off your Rust package. It only needs to be on the PYTHONPATH. If you don't want to use poetry, you will have to set up a virtual environment if needed and set `PYTHONPATH` when running Puff.


## Puff ♥ Python

Python programs in Puff are run by building a `Program` in Rust and registering the Python function there.

The Python method is bootstrapped and run as a greenlet in the Puff runtime.


```rust title="/app/src/main.rs" no_run
use puff_rs::prelude::*;
use puff_rs::program::commands::PythonCommand;

fn main() -> ExitCode {
    Program::new("my_first_app")
        .about("This is my first app")
        .command(PythonCommand::new("run_hello_world", "my_python_app.hello_world"))
        .run()
}
```

Python: 

```python title="/app/py_code/my_python_app.py"
import puff

# Standard python functions run on Puff greenlets. You can only use special Puff async functions without pausing other greenlets.
def hello_world():
    fn = "my_file.zip"
    result_bytes = puff.read_file_bytes(fn) # Puff async function that runs in Tokio.
    result_py_bytes = do_some_blocking_work(fn) # Python blocking that spawns a thread to prevent pausing the greenlet thread.
    print(f"Hello from python!! Zip is {len(result_bytes)} bytes long from rust and {len(result_py_bytes)} bytes from Python.")


# 100% of python packages are compatible by wrapping them in blocking decorator.
@puff.blocking    
def do_some_blocking_work(fn):
    with open(fn, "rb") as f:
        return f.read()
```


## Puff ♥ Rust

The primary feature of Puff is the seamless ability to go from python into Rust with little configuration.

This makes the full Rust ecosystem available to your Python program with very little integration overhead or performance degradation.

```rust title="/app/src/main.rs" no_run
use puff_rs::prelude::*;
use puff_rs::program::commands::PythonCommand;


// Use pyo3 to generate Python compatible Rust classes.
#[pyclass]
struct MyPythonState;

#[pymethods]
impl MyPythonState {
    fn hello_from_rust(&self, py_says: Text) -> Text {
        format!("Hello from Rust! Python Says: {}", py_says).into()
    }
    
    // Async Puff functions take a function to return the result with and offload the future onto Tokio.
    fn hello_from_rust_async(&self, return_func: PyObject, py_says: Text) {
        run_python_async(return_func, async {
            tokio::time::sleep(Duration::from_secs(1)).await;
            debug!("Python says: {}", &py_says);
            Ok(42)
        })
    }
}


fn main() -> ExitCode {
    let rc = RuntimeConfig::default().set_global_state_fn(|py| Ok(MyPythonState.into_py(py)));
    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(PythonCommand::new("run_hello_world", "my_python_app.hello_world"))
        .run()
}
```

Python: 

```python title="/app/py_code/my_python_app.py"
from puff import global_state

rust_obj = global_state()

def hello_world():
    print(rust_obj.hello_from_rust("Hello from Python!"))
    # Rust functions that execute async code need to be passed a special return function.
    print(puff.wrap_async(lambda ret_func: rust_obj.hello_from_rust_async(ret_func, "hello async")))
```

## Puff ♥ Django

While it can run any WSGI app, Puff has a special affection for Django. Puff believes that business logic should be implemented on a higher level layer and Rust should be used as an optimization. Django is a perfect high level framework to use with Puff as it handles migrations, admin, etc. Puff mimics the psycopg2 drivers and cache so that Django uses the Puff Database and Redis pool.

Transform your sync Django project into a highly concurrent Puff program with a few lines of code. Puff wraps the management commands so migrate, etc. all work as expected. Simply run `cargo run django [command]` instead of using `./manage.py [command]`. For example `cargo run django migrate`. Don't use django's dev server, instead use Puff's with `cargo run runserver`.


```rust title="/app/src/main.rs" no_run
use puff_rs::program::commands::django_management::DjangoManagementCommand;
use puff_rs::program::commands::pytest::PytestCommand;
use puff_rs::program::commands::wsgi::WSGIServerCommand;
use puff_rs::prelude::*;


fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_env("DJANGO_SETTINGS_MODULE", "puff_django_example.settings")
        .set_postgres(true)
        .set_redis(true)
        .set_pubsub(true);

    Program::new("puff_django_app_example")
        .about("This is my first django app")
        .runtime_config(rc)
        .command(WSGIServerCommand::new("puff_django_example.wsgi.application"))
        .command(DjangoManagementCommand::new())
        .command(PytestCommand::new("examples/puff_django_example"))
        .run()
}
```

Use Puff everywhere in your Django app. Even create Django management commands that use Rust!

See [puff-py repo](https://github.com/hansonkd/puff-py) for a more complete Django example.


## Puff ♥ Graphql

Puff exposes Graphql Mutations, Queries and Subscriptions based on Python Class definitions. A core "killer feature" of the Puff Graphql engine is that it works on a "layer base" instead of a Node base. This allows each step of Graphql to gather the complete data necessary to query all data it needs at once. This avoids the dreaded n+1 and dataloader overhead traditionally associated with GraphQL.

GrapqhQL python functions can pass off Pure SQL queries to Puff and puff will render and transform the query without needing to return to python. This allows the Python Graphql interface to be largely IO free, but still flexible to have access to Puff resources when needed.

```python title="/app/py_code/my_python_gql_app.py"
from puff import graphql
from dataclasses import dataclass
from typing import Optional, Tuple, List, Any

pubsub = puff.global_pubsub()
CHANNEL = "my_puff_chat_channel"

@dataclass
class SomeInputObject:
    some_count: int
    some_string: str


@dataclass
class SomeObject:
    field1: int
    field2: str
    
@dataclass
class DbObject:
    was_input: int
    title: str
    
    @classmethod
    def child_sub_query(cls, context, /) -> Tuple[DbObject, str, List[Any], List[str], List[str]]:
        # Extract column values from the previous layer to use in this one.
        parent_values = [r[0] for r in context.parent_values(["field1"])]
        sql_q = "SELECT a::int as was_input, $2 as title FROM unnest($1::int[]) a"
        # returning a sql query along with 2 lists allow you to correlate and join the parent with the child.
        return ..., sql_q, [parent_values, "from child"], ["field1"], ["was_input"]


@dataclass
class Query:

    @classmethod
    def hello_world(cls, parents, context, /, my_input: int) -> Tuple[List[DbObject], str, List[Any]]:
        # Return a Raw query for Puff to execute in Postgres.
        # The elipsis is a placeholder allowing the Python type system to know which Field type it should tranform into.
        return ..., "SELECT $1::int as was_input, \'hi from pg\'::TEXT as title", [my_input]

    @classmethod
    def hello_world_object(cls, parents, context, /, my_input: List[SomeInputObject]) -> Tuple[List[SomeObject], List[SomeObject]]:
        objs = [SomeObject(field1=0, field2="Python object")]
        if my_input:
            for inp in my_input:
                objs.append(SomeObject(field1=inp.some_count, field2=inp.some_string))
        # Return some normal Python objects.
        return ..., objs
    
    @classmethod
    def new_connection_id(cls, context, /) -> str:
        # Get a new connection identifier for pubsub.
        return pubsub.new_connection_id()


@dataclass
class Mutation:
    @classmethod
    def send_message_to_channel(cls, context, /, connection_id: str, message: str) -> bool:
        print(context.auth_token) #  Authoritzation bearer token passed in the context
        return pubsub.publish_as(connection_id, CHANNEL, message)


@dataclass
class MessageObject:
    message_text: str
    from_connection_id: str
    num_processed: int


@dataclass
class Subscription:
    @classmethod
    def read_messages_from_channel(cls, context, /, connection_id: Optional[str] = None) -> Iterable[MessageObject]:
        if connection_id is not None:
            conn = pubsub.connection_with_id(connection_id)
        else:
            conn = pubsub.connection()
        conn.subscribe(CHANNEL)
        num_processed = 0
        while msg := conn.receive():
            from_connection_id = msg.from_connection_id
            # Filter out messages from yourself.
            if connection_id != from_connection_id:
                yield MessageObject(message_text=msg.text, from_connection_id=from_connection_id, num_processed=num_processed)
                num_processed += 1


@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription

```

Rust:

```rust title="/app/src/main.rs" no_run
use puff_rs::program::commands::ServerCommand;
use puff_rs::graphql::handlers::{handle_graphql, handle_subscriptions, playground};
use puff_rs::prelude::*;


fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .set_postgres(true)
        .set_redis(true)
        .set_pubsub(true)
        .set_gql_schema_class("my_python_gql_app.schema");
    
    let router = Router::new()
            .get("/", playground("/graphql", "/subscriptions"))
            .post("/graphql", handle_graphql())
            .get("/subscriptions", handle_subscriptions());

    Program::new("puff_gql_app_example")
        .about("This is my first graphql app")
        .runtime_config(rc)
        .command(ServerCommand::new(router))
        .run()
}
```
Produces a Graphql Schema like so:

![Schema](https://user-images.githubusercontent.com/496914/195461156-1613c3e6-7b82-4143-8796-1b95ff10f7c3.png)

In addition to making it easier to write the fastest queries, a layer based design allows Puff to fully exploit the multithreaded async Rust runtime and solve branches independently. This gives you a  performance advantages out of the box.

## Puff ♥ Pytest

Integrate with pytest to easily test your Graphql and Puff apps. Simply add the `PytestCommand` to your Program and write tests as normal only run them with `cargo run pytest`

```python title="/app/src/py_code/test_gql.py"
from hello_world_py_app import __version__
from puff import global_graphql

gql = global_graphql()


def test_version():
    assert __version__ == '0.1.0'


def test_gql():
    QUERY = """
    query {
        hello_world(my_input: 3) {
            title
            was_input
        }
    }
    """
    result = gql.query(QUERY, {})
    assert 'data' in result
    assert 'errors' not in result
    assert result['data']["hello_world"][0]["title"] == "hi from pg"
    assert result['data']["hello_world"][0]["was_input"] == 3
```

## Puff ♥ AsyncIO

Puff has built in integrations for ASGI and asyncio. You first need to configure the RuntimeConfig to use it. Puff will automatically use uvloop if installed when starting the event loop.

`asgiref.sync.async_to_sync` and `asgiref.sync.sync_to_async` have both been patched so that you can call puff greenlets from async or async from puff greenlets easily.

```python title="/app/src/py_code/fast_api_example.py"
from fastapi import FastAPI
from puff import global_state, wrap_async


state = global_state()

app = FastAPI()


@app.get("/fast-api")
async def read_root():
    result = await wrap_async(lambda r: state.hello_from_rust_async(r, "hello from asyncio"))
    return {"Hello": "World", "from": "Fast API", "rust_value": result}
```

Rust

```rust title="/app/src/main.rs" no_run
use puff_rs::prelude::*;
use puff_rs::program::commands::ASGIServerCommand;


// Use pyo3 to generate Python compatible Rust classes.
#[pyclass]
struct MyPythonState;

#[pymethods]
impl MyPythonState {
    // Async Puff functions take a function to return the result with and offload the future onto Tokio.
    fn hello_from_rust_async(&self, return_func: PyObject, py_says: Text) {
        run_python_async(return_func, async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            debug!("Python says: {}", &py_says);
            Ok(42)
        })
    }
}

fn main() -> ExitCode {
    let app = Router::new().get("/", root);
    let rc = RuntimeConfig::default()
        .set_asyncio(true)
        .set_global_state_fn(|py| Ok(MyPythonState.into_py(py)));
        
    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(ASGIServerCommand::new_with_router(
            "fast_api_example.app",
            app,
        ))
        .run()
}

// Basic handler that responds with a static string
async fn root() -> Text {
    "Ok".to_text()
}
```

## Puff ♥ Django + Graphql

Puff GraphQL integrates seamlessly with Django. Convert Django querysets to SQL to offload all computation to Rust. Or decorate with `borrow_db_context` and let Django have access to the GraphQL connection, allowing you fallback to the robustness of django for complicated lookups.

```python
from dataclasses import dataclass
from puff import graphql
from polls.models import Question, Choice
from django.utils import timezone


@dataclass
class ChoiceObject:
    id: int
    question_id: int
    choice_text: str
    votes: int


@dataclass
class QuestionObject:
    id: int
    pub_date: str
    question_text: str

    @classmethod
    def choices(cls, context, /) -> Tuple[List[ChoiceObject], str, List[Any], List[str], List[str]]:
        # Extract column values from the previous layer to use in this one.
        parent_values = [r[0] for r in context.parent_values(["id"])]
        # Convert a Django queryset to sql and params to pass off to Puff. This function does 0 IO in Python.
        qs = Choice.objects.filter(question_id__in=parent_values)
        sql_q, params = puff.contrib.django.query_and_params(qs)
        return ..., sql_q, params, ["id"], ["question_id"]


@dataclass
class Query:

    @classmethod
    def questions(cls, context, /) -> Tuple[List[QuestionObject], str, List[Any]]:
        # Convert a Django queryset to sql and params to pass off to Puff. This function does 0 IO in Python.
        qs = Question.objects.all()
        sql_q, params = query_and_params(qs)
        return ..., sql_q, params

    @classmethod
    @graphql.borrow_db_context  # Decorate with borrow_db_context to use same DB connection in Django as the rest of GQL
    def question_objs(cls, context, /) -> Tuple[List[QuestionObject], List[Any]]:
        # You can also compute the python values with Django and hand them off to Puff.
        # This version of the same `questions` field, is slower since Django is constructing the objects.
        objs = list(Question.objects.all())
        return ..., objs


@dataclass
class Mutation:
    @classmethod
    @graphql.borrow_db_context  # Decorate with borrow_db_context to use same DB connection in Django as the rest of GQL
    def create_question(cls, context, /, question_text: str) -> QuestionObject:
        question = Question.objects.create(question_text=question_text, pub_date=timezone.now())
        return question

@dataclass
class Subscription:
    pass

@dataclass
class Schema:
    query: Query
    mutation: Mutation
    subscription: Subscription

```


## Puff ♥ Tasks

Sometimes you need to execute a function in the future, or you need to execute it, but you don't care about the result right now. For example, you might have a webhook or an email to send.

Puff provides a TaskQueue abstraction as part of the standard library. It is powered by Redis and has the ability to distribute tasks across nodes with priorities, delays and retries. TaskQueues can be persisted (if Redis is configured to persist to disk), so you can shutdown and restart your server without worrying about losing your queued functions.

TaskQueues run in the background of every Puff instance. In order to have a "worker" instance, simply use the `WaitForever` command. This means by default, your HTTP server will also handle processing and running background tasks. This is handy for small projects and scales out well by using `wait_forever` to add more processing power if needed.

A "Task" is simply a python function that takes a JSONable Payload that you care executes, but you don't care when or where. What's a "JSONable" payload? Think simple Python structures (dicts, lists, strings, etc) that can be serialized to JSON. Queues will monitor tasks and retry them if they don't get a result in `timeout_ms`. This means that you might have the same task running multiple times if you don't configure timeouts correctly, so be aware if you are sending HTTP requests or something that might take a while to respond. Tasks should return a "JSONable" result which will be kept for `keep_results_for_ms` seconds.

Only pass in top-level functions into `schedule_function` that can be imported (no lambda's or closures). This function should be accessible on all Puff instances.

Implement "priorities" by utilizing `scheduled_time_unix_ms`. The TaskQueue sorts all tasks by this value and executes the first one up until the current time. So if you schedule `scheduled_time_unix_ms=1`, you can be sure that your Task will be the first to execute on the first availability. Use `scheduled_time_unix_ms=1`, `scheduled_time_unix_ms=2`. `scheduled_time_unix_ms=3`, etc for different task types that are high priority and you want to execute as soon as possible. Just be careful that you don't "starve" the other tasks if you aren't processing these high priority Tasks fast enough. By default Puff schedules new tasks with the current unix time to be "fair" and provide a sense of "FIFO" order. You can also set this value to a unix timestamp in the future to delay execution of a task.

You can have as many tasks running as you want (use `set_task_queue_concurrent_tasks`), but there is a small overhead in terms of monitoring and finding new tasks by increasing this value. The default is `num_cpu x 4`

```python title="/app/src/py_code/task_queue_example.py"
from puff.task_queue import global_task_queue

task_queue = global_task_queue()


def run_main():
    all_tasks = []
    for x in range(100):
        task1 = task_queue.schedule_function(my_awesome_task, {"type": "coroutine", "x": [x]}, timeout_ms=100, keep_results_for_ms=5 * 1000)
        #  override `scheduled_time_unix_ms` so that async tasks execute with priority over the coroutine tasks.
        #  Notice that since all async tasks have the same priority, they may be executed out of the order they were scheduled.
        task2 = task_queue.schedule_function(my_awesome_task_async, {"type": "async", "x": [x]}, scheduled_time_unix_ms=1, timeout_ms=100, keep_results_for_ms=5 * 1000)
        #  These tasks will keep their order since their priorities as defined by `scheduled_time_unix_ms` match their order.
        task3 = task_queue.schedule_function(my_awesome_task_async, {"type": "async-ordered", "x": [x]}, scheduled_time_unix_ms=x, timeout_ms=100, keep_results_for_ms=5 * 1000)
        print(f"Put tasks {task1}, {task2}, {task3} in queue")
        all_tasks.append(task1)
        all_tasks.append(task2)
        all_tasks.append(task3)

    for task in all_tasks:
        result = task_queue.wait_for_task_result(task, 100, 1000)
        print(f"{task} returned {result}")


def my_awesome_task(payload):
    print(f"In task {payload}")
    return payload["x"][0]


async def my_awesome_task_async(payload):
    print(f"In async task {payload}")
    return payload["x"][0]
```

Rust

```rust title="/app/src/main.rs" no_run
use puff_rs::prelude::*;
use puff_rs::program::commands::{WaitForever, PythonCommand};

fn main() -> ExitCode {
    let rc = RuntimeConfig::default()
        .add_python_path("./examples")
        .set_asyncio(true)
        .set_task_queue(true);

    Program::new("my_first_app")
        .about("This is my first app")
        .runtime_config(rc)
        .command(PythonCommand::new(
            "run_main",
            "task_queue_example.run_main",
        ))
        .command(WaitForever)
        .run()
}
```

## Puff ♥ HTTP

Puff has a built in asyncrounous HTTP client that can handle HTTP1 and HTTP2 and reuse connections. It uses rust to encode and decode JSON ultra-fast.

```python
from puff.http import global_http_client

http_client = global_http_client()

async def do_http_request():
    this_response = await http_client.post("http://localhost:7777/", json={"my_data": ["some", "json_data"]})
    return await this_response.json()


def do_http_request_greenlet():
    """greenlets can use same async functions. Puff will automatically handle awaiting and context switching."""
    this_response = http_client.post("http://localhost:7777/", json={"my_data": ["some", "json_data"]})
    return this_response.json()
```

## FAQ

#### Why a monolithic project?

Puff follows the Django model of having everything you need built-in. A modern SaaS App expects HTTP server, Postgres, Redis, PubSub and an API. Instead of
saying that the default is an environment with none of these, Puff takes a practical approach and says that it is actually more
of an edge case not to need those resources. Eventually these dependencies should be configurable via feature flags.

While it has a heavy upfront compilation cost, with the following config the binary for the puff runtime ends up being around 4mb. 

```toml
[profile.release]
opt-level = 3
strip = true
debug = false
codegen-units = 1
lto = true
```


#### Architecture

Puff consists of multithreaded Tokio Runtime and a single thread which runs all Python computations on Greenlets. Python offloads the IO to Tokio which schedules it and returns it if necessary.

![Untitled Diagram-2](https://user-images.githubusercontent.com/496914/195153405-7a1c7bcf-a864-4502-806c-c7d5e7aac3b9.png)


#### Why is Greenlet environment single threaded?

Only one thread can have the GIL at any particular time. All IO is done outside the GIL in the Rust layer and so the greenlets will only be utilizing the GIL efficiently most of the time. Adding more Python threads will not increase performance, however you can dispatch blocking greenlets which will run on their own thread if you need to do IO blocking work with the Python standard library or 3rd party packages.


#### What is a Deep Stack Framework?
Currently, you have the frontend and backend that makes up your "full stack". Deep stack is about safely controlling the runtime that your full stack app executes on. Think of an ASGI or WSGI server that is probably written in C or another low level language that executes your higher level Python backend code. Deep stack is about giving you full (and safe) control over that lower level server to run your higher level operations. Its aggressively embracing that different levels of languages have different pros and cons.

![Untitled Diagram](https://user-images.githubusercontent.com/496914/195152641-2dc8567e-8d06-41d4-a651-413312f18aef.png)


#### Deep Stack Teams

The thesis of the deepstack project is to have two backend engineering roles: Scale and Deepstack. Deepstack engineers' primary goal should be to optimize and abstract the hot paths of the application into Rust to enable the Scale engineers who implement business logic in Python to achieve an efficient ratio between productivity, safety, and performance. Nothing is absolute and the decision to move code between Rust and Python is a gradient rather than binary.


#### Benefits of Deep Stack

* Control High performance async Rust Computations with Flexible Python Abstractions. 
* Maximum Performance: Only enter the python GIL to get the query and parameters and exit to execute the query and compute the results in Rust.
* Django compatible: Full Admin, Migrations, Views, Tests, etc...
* Axum Compatible: All extractors are ready to be used.
* Rapid iteration on the data control layer (Pyton / Django / Flask) without total recompilation of the deep stack layer.
* Quickly scale the Flexibility of Python with the Performance and Safety of Rust.

#### Performance

Right now there hasn't been too much focus on raw performance in GraphQL, because ultimately performance comes from SQL query optimizations (indexes, no n+1, etc). Puff's structure encourages you write your queries in a layer basis without having to rely on dataloaders or complicated optimizers allowing you to directly express the proper SQL. Ultimately the performance of the GQL server is based on how optimized your queries are to the indexes and structure of your DB.

Puff won't magically make WSGI faster, but where Puff really excels is pushing down tight loops or iterations into the Rust Layer. Think about if you wanted to run multiple queries in parallel and perform some computation on them, resulting in Bytes. It's better to push this function into Puff. See the `/examples/wsgi.rs::get_many` function for an example.

AsyncIO / uvloop performance is about the same if not 10-25% faster as using hypercorn or similar when using 1 worker.

When thinking about performance on Deep Stack, remember about the extra dimension it makes possible. For example, if you are making a micro-benchmark and wanted to compare a static json response from FastAPI, it would be more idiomatic for Puff to write the static JSON response as an Axum handler in Rust and instead of a FastAPI endpoint. This can deliver a 2-80x performance boost. Specializing a route in Rust is not cheating; it's how its designed.

#### Status

This is extremely early in development. The scope of the project is ambitious. Expect things to break. 

Probably the end game of puff is to have something like gevent's monkeypatch to automatically make existing projects compatible.