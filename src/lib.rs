//! # ☁ Puff ☁
//!
//! The deep stack framework.
//!
//! Puff is a "build your own runtime" for Python using Rust. It uses the standard CPython bindings with pyo3 and offers safe interaction of python code from a lower level framework. IO operations like HTTP requests and SQL queries get off-loaded to the lower level Puff runtime which in turn runs on Tokio. This makes 100% of Python packages and 100% of tokio Rust packages compatible with Puff.
//!
//! # Puff Benefits
//!
//! * Rust Compatible
//!     * Axum Compatible
//!     * Tokio Compatible
//!     * Std Compatible
//! * Python Compatible
//!     * WSGI Compatible
//!     * Django Compatible
//!     * Flask Compatible
//! * Easy-To-Use Interface
//!     * Rust Types optimized for easy sharing with Python
//!     * Sync-Code, Write highly concurrent programs without using `async`... in Python or Rust.
//!     * Clear idioms for sharable read-only structures and mutable builders.
//!         * Text
//!         * Vector
//!         * Map
//!         * Bytes
//!         * Datetime
//!         * Date
//!     * Tightly Integrated Standard Library
//!         * Postgres Pools
//!         * Redis Connections
//!         * Pub-Sub Channels across microservices
//!         * Async Job Queue
//!         * MPSC and Oneshot Channels
//!         * Fast memory efficient Bytes, Map, Vector, Datetime, and Text types.
//! * Quick Escape Hatches
//!     * Switch to Python to handle jobs that don't fit well in the Rust paradigm.
//!     * Drop down to pure Rust or Assembly to handle the most performance critical paths of your app.
//! * Cargo
//!     * Use Crates package management for Puff packages
//!     * Use Cargo build system for reliable deployments
//!
//! ## Basic Example
//!
//! This example shows how to make a simple Puff web service.
//!
//! ```no_run
//! use std::process::ExitCode;
//! use puff::program::commands::http::ServerCommand;
//! use puff::program::Program;
//! use puff::types::text::{Text, ToText};
//! use puff::web::server::Router;
//! use puff::errors::Result;
//! use std::time::Duration;
//!
//! fn main() -> ExitCode {
//!     // build our application with a route//!
//!     let app = Router::new().get("/", root);
//!
//!     Program::new("my_first_app")
//!         .about("This is my first app")
//!         .command(ServerCommand::new(app)) // Expose the `server` command to the CLI
//!         .run()
//! }
//!
//! // Basic handler that responds with a static string
//! fn root() -> Result<Text> {
//!     // Puff functions don't block even though they are sync!
//!     puff::tasks::sleep(Duration::from_secs(1));
//!     Ok("ok".to_text())
//! }
//! ```
//!
//! Run with `cargo run server` or use `cargo run help` to show all CLI options of your Puff program.
//!
//!
extern crate core;

pub mod channels;
pub mod context;
pub mod databases;
pub mod errors;
pub mod graphql;
pub mod program;
pub mod python;
pub mod rand;
pub mod runtime;
pub mod tasks;
pub mod types;
pub mod web;

pub use tracing;
