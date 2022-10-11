# ☁ Puff ☁
The original deep stack framework.

- [☁ Puff ☁](#--puff--)
  * [What is Puff?](#what-is-puff-)
  * [A Different Path to Python Performance](#a-different-path-to-python-performance)
  * [What is a Deep Stack Framework?](#what-is-a-deep-stack-framework-)
  * [Puff Principles](#puff-principles)
  * [Why no Async](#why-no-async)


## What is Puff?
Puff is a "build your own runtime" for Python using Rust. It uses the standard CPython bindings with pyo3 and offers safe interaction of python code from a lower level framework. IO operations like HTTP requests and SQL queries get off-loaded to the lower level Puff runtime which in turn runs on Tokio. This makes 100% of Python packages and 100% of tokio Rust packages compatible with Puff.

Puff gives Python...

* Greenlets on Tokio.
* A high performance HTTP Server - WSGI compatible
* Rust calls Python natively in the same process, no sockets or serialization.
* An easy-to-use GraphQL service
* Multi-node pub-sub
* Rust level Redis Pool
* Rust level Postgres Pool
* Websockets
* Optimistically compatible with Psycopg2 (hopefully good enough for most of Django)
* A safe convenient way to drop into rust for maximum performance

## A Different Path to Python Performance
A lot of buzz has been gathering around Async support in Python. With promises of increased scalability for the web, it has been a developing feature for the past decade. However, Async python doesn't solve the primary bottleneck of python code: the GIL. No matter how many threads you have, you can only execute python bytecode on one thread. The rest of the threads can only be busy with IO.

The only way to actually make standard python faster is to reduce the python used. The fundamental thesis of Puff is that Python is a great high level language and should be used to generate structured data that gets handed off to a lower level runtime to be executed. The goal of Puff is to offload as much work from python as possible into the custom runtime you build. Every instruction that python hands off, is an opportunity for the multi-threaded runtime to better schedule its execution.

For example, in standard python world, you might consume a SQL query, iterate over it producing a python dictionary and then serialize that python dictionary to JSON which then gets handed off to a socket. In Puff and the Deep stack, that SQL query gets handed off to Rust and the reading and transformation of the data takes places in Rust and the tokio runtime. This completely avoids the GIL.

## What is a Deep Stack Framework?
Currently, you have the frontend and backend that makes up your "full stack". Deep stack is about safely controlling the runtime that your full stack app executes on. Think of an ASGI or WSGI server that is probably written in C or another low level language that executes your higher level Python backend code. Deep stack is about giving you full (and safe) control over that lower level server to run your higher level operations. Its aggressively embracing that different levels of languages have different pros and cons.

Deep stack is about safely scaling a custom low level runtime for your team.

The most flexible interface with a robust amount of tools that work well with what Puff is working to achieve is GraphQL. Therefore Puff offers GraphQL as a primary API. Build subscriptions, queries and mutations with Python. Using GraphQL as an abstraction allows Puff to only momentarily enter into the higher level runtime and quickly exit with the minimal information necessarily to execute the rest of the request entirely in Rust.


### Deep Stack Teams

The thesis of the deepstack project is to have two backend engineering roles: Scale and Deepstack. Deepstack engineers' primary goal should be to optimize and abstract the hot paths of the application into Rust to enable the Scale engineers who implement business logic in Python to achieve an efficient ratio between productivity, safety, and performance. Nothing is absolute and the decision to move code between Rust and Python is a gradient rather than binary.


## Benefits of Deep Stack

* Control High performance async Rust Computations with Flexible Python Abstractions. 
* Maximum Performance: Only enter the python GIL to get the query and parameters and exit to execute the query and compute the results in Rust.
* Django compatible: Full Admin, Migrations, Views, Tests, ...everything.
* Axum Compatible: All extractors are ready to be used.
* Rapid iteration on the data control layer (Pyton / Django / Flask) without total recompilation of the deep stack layer.
* Quickly scale the Flexibility of Python with the Performance and Safety of Rust.
* Composable Rust level packages and a Python Runtime.

## Puff Principles

* If there is a question over performance vs understanding, err on the side of making things understandable. 
* A python engineer should be able to jump into a Puff context and understand what is going on and be productive.
* 100% Rust compatible
* 100% Python compatible
* Postgres is the database protocol
* Redis is the KV/Cache/PubSub protocol
* GraphQL is the primary interface between services.
* GraphQL's subscriptions are the abstraction for subscriptions.

Making these assumptions, Puff becomes incredible productive for a team.

## Why no AsyncIO?

The thesis of puff is that the GIL is bad and all Computation should be shifted down to the rust layer. While it is 100% possible to
make Puff AsyncIO compatible, Puff wants to encourage users to return pure data structures that represent queries that do zero
IO and allow the Rust runtime to compute the results.

The WSGI interface is primarily a fallback for auth, admin and one off activities and shouldn't be relied on to be the primary interface for serving data. That should be Graphql. This allows maximum flexibility to use Django and Flask packages for Auth, admin, files, etc, while serving the bulk of your data from Rust.

## Should I use this?

Right now this is just a proof of concept of exploring using higher level languages with a low level runtime. It is 
also meant to showcase how Graphql can be an efficient representation to achieve maximum data throughput using
a high level abstraction like Django.

Want to help make it work? email `kyle <at> getdynasty <dot> com` to join the first deep stack team and push the limits of Python.