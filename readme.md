# ☁ Puff ☁
The deep stack framework

## What is Puff?
Puff is a "build your own runtime" for Python using Rust. It uses the standard CPython bindings with pyo3 and offers safe interaction of python code from a lower level framework. IO operations like HTTP requests and SQL queries get off-loaded to the lower level Puff runtime which in turn runs on Tokio. This makes 100% of Python packages and 100% of tokio Rust packages compatible with Puff.

Puff gives Python...

* A high performance HTTP Server - ASGI and WSGI compatible
* An easy-to-use GraphQL service
* Database pool
* Cache
* Websockets
* Scalable coroutines
* A safe convenient way to drop into rust for maximum performance

## A Path to Python Nirvana
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
