//! # ☁ Puff ☁
//!
//! The deep stack framework.
//!
//! Puff is a "build your own runtime" for Python using Rust. It uses the standard CPython bindings with pyo3 and offers safe interaction of python code from a lower level framework. IO operations like HTTP requests and SQL queries get off-loaded to the lower level Puff runtime which in turn runs on Tokio. This makes 100% of Python packages and 100% of tokio Rust packages compatible with Puff.
//!
//! # Puff Benefits
//!
//! * Integrations:
//!     * PubSub
//!     * Redis
//!     * Postgres
//!     * GraphQL
//! * Rust Compatible
//!     * Axum Compatible
//!     * Tokio Compatible
//!     * Std Compatible
//! * Python Compatible
//!     * WSGI Compatible
//!     * Django Compatible
//!     * Flask Compatible
//! * Quick Escape Hatches
//!     * Switch to Python to handle jobs that don't fit well in the Rust paradigm.
//!     * Drop down to pure Rust or even Assembly to handle the most performance critical paths of your app.
//!
//! ## Basic Example
//!
//! This example shows how to make a simple Puff web service.
//!
//! ```no_run
//! use puff_rs::program::commands::ServerCommand;
//! use puff_rs::prelude::*;
//!
//! fn main() -> ExitCode {
//!     // build our axum application with a route.
//!     let app = Router::new().get("/", root);
//!
//!     Program::new("my_first_app")
//!         .about("This is my first app")
//!         .command(ServerCommand::new(app)) // Expose the `server` command to the CLI
//!         .run()
//! }
//!
//! // Basic handler that responds with a static text
//! async fn root() -> Text {
//!     // Puff handlers are based on Axum.
//!     "ok".to_text()
//! }
//! ```
//!
//! Run with `cargo run runserver` or use `cargo run help` to show all CLI options of your Puff program.
//!
//!
pub mod context;
pub mod databases;
pub mod errors;
pub mod graphql;
pub mod prelude;
pub mod program;
pub mod python;
pub mod rand;
pub mod runtime;
pub mod types;
pub mod web;

pub use {axum, tracing};
