# ☁ Puff ☁
The deep stack framework.

- [What is Puff?](#what-is-puff)
  * [Quick Start](#quick-start)
  * [Puff ♥ Python](#puff--python)
  * [Puff ♥ Rust](#puff--rust)
  * [Puff ♥ Django](#puff--django)
  * [Puff ♥ Graphql](#puff--graphql)
  * [Puff ♥ Pytest](#puff--pytest)
  * [FAQ](#faq)
      - [Puff Dependencies](#why-a-monolithic-project)
      - [Architecture](#architecture)
      - [Why is Greenlet environment single threaded?](#why-is-greenlet-environment-single-threaded)
      - [Why no AsyncIO?](#why-no-asyncio)
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
* An easy-to-use GraphQL service
* Multi-node pub-sub
* Rust level Redis Pool
* Rust level Postgres Pool
* Websockets
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

Your Rust Puff Project needs to find your Python project. Even if they are in the same folder, they need to be added to the path.

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

If you don't want to use poetry, you will have to set up a virtual environment and set `PYTHONPATH` when running Puff.


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
        greenlet_async(return_func, async {
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

##

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

#### Why no AsyncIO?

A core belief of puff is that the GIL is bad and all Computation should be shifted down to the rust layer. While it is 100% possible to make Puff AsyncIO compatible, Puff wants to encourage users to return pure data structures that represent queries that do zero IO and allow the Rust runtime to compute the results.

The WSGI interface is primarily a fallback for auth, admin and one off activities and shouldn't be relied on to be the primary interface for serving data. Graphql should serve the majority of the traffic that comes from a Puff site. This allows maximum flexibility to use Django and Flask packages for Auth, admin, files, etc, while serving the bulk of your data from Rust.

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

Right now there hasn't been too much focus on raw performance, because ultimately performance comes from SQL query optimizations (indexes, no n+1, etc). Puff's structure encourages you write your queries in a layer basis without having to rely on dataloaders or complicated optimizers allowing you to directly express the proper SQL. Ultimately the performance of the GQL server is based on how optimized your queries are to the indexes and structure of your DB.

#### Status

This is extremely early in development. While many months of weekends have gotten it this far, the scope of the project is ambitious. Expect things to break. 

Probably the end game of puff is to have something like gevent's monkeypatch to automatically make projects compatible.