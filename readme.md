# ☁ Puff ☁
The deep stack framework


- [☁ Puff ☁](#--puff--)
  * [What is Puff?](#what-is-puff-)
  * [A puff example](#a-puff-example)
    + [First puff app](#first-puff-app)
  * [A Different Path to Python Performance](#a-different-path-to-python-performance)
  * [What is a Deep Stack Framework?](#what-is-a-deep-stack-framework-)
  * [Puff Principles](#puff-principles)
  * [Architecture](#architecture)


## What is Puff?
Puff is a "build your own runtime" for Python using Rust. It uses the standard CPython bindings with pyo3 and offers safe interaction of python code from a lower level framework. IO operations like HTTP requests and SQL queries get off-loaded to the lower level Puff runtime which in turn runs on Tokio. This makes 100% of Python packages and 100% of tokio Rust packages compatible with Puff.

Puff gives Python...

* A high performance HTTP Server - ASGI and WSGI compatible
* An easy-to-use GraphQL service
* Database pool
* Cache
* Websockets
* Scalable coroutines without AsyncIO
* A safe convenient way to drop into rust for maximum performance

## A puff example

Here is a puff project that exposes a python GQL query endpoint. It returns a variety of python objects and a queries.

```python
from flask import Flask

@dataclass
class Query:

    @classmethod
    def hello_world(cls, parents, context, /, my_input: Optional[int] = None) -> str:
        return f"hello: {my_input}"
      
    def hello_world_object(cls, parents, context, /, my_input: Optional[int] = None) -> Tuple[List[SomeObject], List[Any]]:
        return ..., [[SomeObject(field1=0, field2="fa three"), SomeObject(field1=0, field2="fa so la de do"), SomeObject(field1=5, field2="None"), SomeObject(field1=1, field2="reda")]]

    @classmethod
    def hello_world_query(cls, parents, context, /, my_input: Optional[int] = None) -> Tuple[SomeObject, str, List[Any], str]:
        return ..., "SELECT 'HELLO WORLD'::TEXT FROM WHERE one IN $1", [parents], "id"
        

@dataclass
class Schema:
   query: Query


app = Flask(__name__)

@app.route("/hello-world")
def hello_world():
    from puff.context import db
    # Use the Puff Database pool to make a query. This is non-blocking if using puff coroutines
    rows = db.query("SELECT * FROM MY_TABLE")
    return f"<p>Hello, World! {rows}</p>"
```

Notice how `hello_world_query` returns a raw SQL query. This query gets handed off to the puff runtime to be executed asyncronously on Tokio.

We also expose a flask app that our puff server will run.

### First puff app

Now we will expose our python module using puff. First we will expose a gql endpoint

```rust
use std::net::SocketAddr;
use std::time::Duration;
use puff::program::commands::http::ServerCommand;
use puff::program::Program;
use puff::web::http::{
    body_text, FnHandler, Request, Response, ResponseBuilder, Router, StatusCode,
};
use puff::python::web::{make_wsgi_service, make_graphql_python_service};

fn main() {
    // build our application with a route
    let app = Router::new()
        .get("/", root);
        .post("/graphql/", make_graphql_python_service("my_module.Schema"))
        .fallback(make_wsgi_service("my_module.app"));

    Program::new("my_first_app")
        .about("This is my first app")
        .command(ServerCommand::new(app))
        .run()
}

// basic handler that responds with a static string
fn root(req: Request) -> Response {
    // Buff coroutines will not block despite being sync code!
    puff::tasks::task::sleep(Duration::from_secs(1));

    ResponseBuilder::builder()
        .status(StatusCode::OK)
        .body(body_text("ok"))
        .unwrap()
}

```

Vising "/" will run Rust, Visting "/graphql" will run our python query functions and then offload the queries into the rust runtime, "/hello-world" will run in flask.


## A Different Path to Python Performance
A lot of buzz has been gathering around Async support in Python. With promises of increased scalability for the web, it has been a developing feature for the past decade. However, Async python doesn't solve the primary bottleneck of python code: the GIL. No matter how many cores you have, you can only execute python bytecode on one core. The rest of the cores can only be busy with IO.

The only way to actually make standard python faster is to reduce the python used. The fundamental thesis of Puff is that Python is a great high level language and should be used to generate structured data that gets handed off to a lower level runtime to be executed. The goal of Puff is to offload as much work from python as possible into the custom runtime you build. Every instruction that python hands off, is an opportunity for the multi-threaded runtime to better schedule its execution.

For example, in standard python world, you might consume a SQL query, iterate over it and transform it into HTML. This locks the GIL for a long time. With Puff, Python would hand off the SQL query and a template to the runtime where it would execute and be transformed. This strategy frees developers to continue to work with their favorite Python ORM or Framework (like Django) to generate queries and return responses, while significantly boosting performance.

## What is a Deep Stack Framework?
Currently, you have the frontend and backend that makes up your "full stack". Deep stack is about safely controlling the runtime that your full stack app executes on. Think of an ASGI or WSGI server that is probably written in C or another low level language that executes your higher level Python backend code. Deep stack is about giving you full (and safe) control over that lower level server to run your higher level operations. Its aggressively embracing that different levels of languages have different pros and cons.

In short, deep stack is about safely scaling a custom low level runtime for your team.

## Puff Principles

* If there is a question over performance vs ease of understanding, err on the side of making things understandable. 
* Lifetime free: Puff interfaces and functions should only deal with lifetimes that are compatible with `'static`.
* Async free: Puff is written with sync functions that execute on coroutines that switch their context back to lower level async only as necassary. This allows running sync code on an async runtime.
* A python engineer should be able to jump into a Puff context and understand what is going on and be productive.
* 100% Rust compatible
* 100% Python compatible
* Postgres is the database protocol
* Redis is the KV/Cache/PubSub protocol
* GraphQL is the primary interface between services.

Making these assumptions, Puff becomes incredible productive for a team.

## Architecture

Puff is divided into 3 main environments in which code executes: a multithreaded tokio runtime handles most of the heavy lifting, taking care of database queries and running the http server. Puff also spawns a number of single threaded tokio runtimes that run the coroutines. If you use non-blocking puff functions, your tasks can run on these workers. If you use other libraries with blocking IO, Puff can spawn one off threads in which your tasks execute.

![Puff Architecture](https://user-images.githubusercontent.com/496914/188336920-62d8c5d0-ebcb-494c-9b95-7538d26a621c.svg)