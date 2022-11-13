//! Build a Puff program compatible with the CLI.
//!
//! Use builtin commands or specify your own.
//!
//! Commands use [clap::Command] as their specification and return a `Runnable` that wraps a Future to
//! run on the Parent's multi-threaded runtime. To enter into a Puff context in a `RunnableCommand`, use
//! the dispatcher to create a Future.
//!
//! # Example
//!
//! ```no_run
//! use puff_rs::prelude::*;
//! use puff_rs::program::clap::{ArgMatches, Command};
//! use puff_rs::program::{Program, Runnable, RunnableCommand};
//! use puff_rs::context::PuffContext;
//!
//! struct MyCommand;
//!
//! impl RunnableCommand for MyCommand {
//!     fn cli_parser(&self) -> Command {
//!         Command::new("my_custom_command")
//!     }
//!
//!     fn make_runnable(&mut self, args: &ArgMatches, context: PuffContext) -> puff_rs::errors::Result<Runnable> {
//!         // Do some setup like extract args from ArgMatches.
//!         // ...
//!         // Then return a future to run.
//!         Ok(Runnable::new(async {
//!             println!("hello from rust!");
//!             Ok(ExitCode::SUCCESS)
//!         }))
//!     }
//! }
//!
//! fn main() -> ExitCode {
//!     Program::new("my_first_program")
//!         .author("Kyle Hanson")
//!         .version("0.0.0")
//!         .command(MyCommand)
//!         .run()
//! }
//! ```
//!
//! Run with `cargo run my_custom_command` or use `cargo run help`

use clap::{ArgMatches, Command};

use crate::databases::redis::{add_redis_command_arguments, new_redis_async};
use crate::tasks::{add_task_queue_command_arguments, loop_tasks, new_task_queue_async};
use crate::web::client::new_http_client;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use tokio::runtime::Builder;

use crate::context::{set_puff_context, set_puff_context_waiting, PuffContext, RealPuffContext};
use crate::databases::postgres::{add_postgres_command_arguments, new_postgres_async};
use crate::databases::pubsub::{add_pubsub_command_arguments, new_pubsub_async};
use crate::errors::{handle_puff_error, PuffResult, Result};
use crate::graphql::load_schema;
use crate::python::{bootstrap_puff_globals, setup_python_executors};
use crate::runtime::RuntimeConfig;
use crate::types::text::Text;
use crate::types::Puff;
pub use clap;
use tracing::info;
pub mod commands;

/// A wrapper for a boxed future that is able to be run by a Puff Program.
pub struct Runnable(Pin<Box<dyn Future<Output = Result<ExitCode>> + 'static>>);

impl Runnable {
    pub fn new<F: Future<Output = Result<ExitCode>> + 'static>(inner: F) -> Self {
        Self(Box::pin(inner))
    }
}

/// A Puff command that integrates with the CLI.
///
/// Specify new custom commands with puff by implementing this interface.
///
/// # Example:
///
/// ```no_run
/// use std::process::ExitCode;
/// use puff_rs::program::{clap:: {ArgMatches, Command}, Runnable, RunnableCommand};
/// use puff_rs::context::PuffContext;
///
/// struct MyCommand;
///
/// impl RunnableCommand for MyCommand {
///     fn cli_parser(&self) -> Command {
///         Command::new("my_custom_command")
///     }
///
///     fn make_runnable(&mut self, _args: &ArgMatches, context: PuffContext) -> puff_rs::errors::Result<Runnable> {
///         // Setup code.
///         Ok(Runnable::new(async {
///             // Do something in async context.
///             Ok(ExitCode::SUCCESS)
///         }))
///     }
/// }
/// ```
pub trait RunnableCommand: 'static {
    /// The [clap::Command] that specifies the arguments and meta information.
    fn cli_parser(&self) -> Command;

    /// Converts parsed matches from the command line into a Runnable future.
    fn make_runnable(&mut self, args: &ArgMatches, dispatcher: PuffContext) -> Result<Runnable>;
}

struct PackedCommand(Box<dyn RunnableCommand>);

impl PackedCommand {
    pub fn cli_parser(&self) -> Command {
        self.0.cli_parser()
    }

    pub fn make_runnable(
        &mut self,
        args: &ArgMatches,
        dispatcher: PuffContext,
    ) -> Result<Runnable> {
        self.0.make_runnable(args, dispatcher)
    }
}

fn handle_puff_exit(label: &str, r: PuffResult<ExitCode>) -> ExitCode {
    match r {
        Ok(exit) => exit,
        Err(e) => {
            handle_puff_error(label, e);
            ExitCode::FAILURE
        }
    }
}

/// A Puff Program that is responsible for parsing CLI arguments and starting the Runtime.
pub struct Program {
    name: Text,
    author: Option<Text>,
    version: Option<Text>,
    about: Option<Text>,
    after_help: Option<Text>,
    commands: Vec<PackedCommand>,
    runtime_config: RuntimeConfig,
}

impl Program {
    /// Creates a `Program` with the specified name.
    pub fn new<T: Into<Text>>(name: T) -> Self {
        Self {
            name: name.into(),
            commands: Vec::new(),
            version: None,
            about: None,
            after_help: None,
            author: None,
            runtime_config: RuntimeConfig::default(),
        }
    }

    /// Override the current `RuntimeConfig` for this program.
    pub fn runtime_config(self, runtime_config: RuntimeConfig) -> Self {
        let mut s = self;
        s.runtime_config = runtime_config;
        s
    }

    /// Specify the author.
    pub fn author<T: Into<Text>>(self, author: T) -> Self {
        let mut s = self;
        s.author = Some(author.into());
        s
    }

    /// Specify the version.
    pub fn version<T: Into<Text>>(self, version: T) -> Self {
        let mut s = self;
        s.version = Some(version.into());
        s
    }

    /// Specify what your program does.
    pub fn about<T: Into<Text>>(self, about: T) -> Self {
        let mut s = self;
        s.about = Some(about.into());
        s
    }

    /// Specify text after the help text of the CLI prompt.
    pub fn after_help<T: Into<Text>>(self, after_help: T) -> Self {
        let mut s = self;
        s.after_help = Some(after_help.into());
        s
    }

    /// Adds a new command to be available to the `Program`.
    pub fn command<C: RunnableCommand>(self, command: C) -> Self {
        let box_command = Box::new(command);
        let mut new_self = self;

        new_self.commands.push(PackedCommand(box_command));
        new_self
    }

    fn clap_command(&self) -> Command {
        let mut tl = Command::new(self.name.clone().into_string()).arg_required_else_help(true);
        if let Some(author) = &self.author {
            tl = tl.author(author.to_string());
        }

        if let Some(about) = &self.about {
            tl = tl.about(about.to_string());
        }

        if let Some(version) = &self.version {
            tl = tl.version(version.to_string());
        }

        if let Some(after_help) = &self.after_help {
            tl = tl.after_help(after_help.to_string());
        }

        tl
    }

    fn runtime(&self) -> Result<Builder> {
        let current_thread = self.runtime_config.tokio_worker_threads() == 1;

        let mut rt = if current_thread {
            tokio::runtime::Builder::new_current_thread()
        } else {
            tokio::runtime::Builder::new_multi_thread()
        };
        rt.enable_all()
            .worker_threads(self.runtime_config.tokio_worker_threads())
            .max_blocking_threads(self.runtime_config.max_blocking_threads())
            .thread_keep_alive(self.runtime_config.blocking_task_keep_alive());

        Ok(rt)
    }

    /// Run the program, handle and log the error if it fails.
    ///
    /// See [Self::try_run] for more information.
    pub fn run(self) -> ExitCode {
        handle_puff_exit("Program", self.try_run())
    }

    /// Tries to run the program and returns an Error if it fails.
    ///
    /// This will parse the command line arguments, start a new runtime and blocks until
    /// the command finishes.
    pub fn try_run(self) -> Result<ExitCode> {
        tracing_subscriber::fmt::try_init().unwrap_or_default();

        let mut top_level = self.clap_command();
        let rt_config = self.runtime_config.clone();

        for name in rt_config.postgres().keys() {
            top_level = add_postgres_command_arguments(name.as_str(), top_level)
        }

        for name in rt_config.redis().keys() {
            top_level = add_redis_command_arguments(name.as_str(), top_level)
        }

        for name in rt_config.pubsub().keys() {
            top_level = add_pubsub_command_arguments(name.as_str(), top_level)
        }

        for name in rt_config.task_queue().keys() {
            top_level = add_task_queue_command_arguments(name.as_str(), top_level)
        }

        rt_config.apply_env_vars();
        let mutex_switcher = Arc::new(Mutex::new(None::<PuffContext>));

        let mut builder = self.runtime()?;

        let mut hm: HashMap<Text, PackedCommand> = HashMap::with_capacity(self.commands.len());
        for packed_command in self.commands {
            let parser = packed_command.cli_parser();
            let name = parser.get_name().to_owned();
            top_level = top_level.subcommand(parser);
            hm.insert(name.into(), packed_command);
        }

        let arg_matches = top_level.get_matches();

        if let Some((command, args)) = arg_matches.subcommand() {
            if let Some(mut runner) = hm.remove(&command.to_string().into()) {
                let thread_mutex = mutex_switcher.clone();

                builder.on_thread_start(move || {
                    set_puff_context_waiting(thread_mutex.clone());
                });

                let rt = builder.build()?;

                let python_dispatcher = if rt_config.python() {
                    pyo3::prepare_freethreaded_python();
                    bootstrap_puff_globals(rt_config.clone())?;
                    let dispatcher = setup_python_executors(
                        rt_config.clone(),
                        mutex_switcher.clone(),
                        rt.handle().clone(),
                    )?;
                    Some(dispatcher)
                } else {
                    None
                };

                let mut redis = HashMap::new();
                for (name, config) in rt_config.redis() {
                    let rc = rt.block_on(new_redis_async(
                        arg_matches
                            .get_one::<String>(&format!("{}_redis_url", name.to_lowercase()))
                            .unwrap()
                            .as_str(),
                        true,
                        config.pool_size,
                    ))?;
                    redis.insert(name.to_string(), rc);
                }

                let mut pubsub_client = HashMap::new();
                for (name, config) in rt_config.pubsub() {
                    let ps = rt.block_on(new_pubsub_async(
                        arg_matches
                            .get_one::<String>(&format!("{}_pubsub_url", name.to_lowercase()))
                            .unwrap()
                            .as_str(),
                        true,
                        config.pool_size,
                    ))?;
                    pubsub_client.insert(name.to_string(), ps);
                }

                let mut task_queue_client = HashMap::new();
                for (name, config) in rt_config.task_queue() {
                    let task_queue = rt.block_on(new_task_queue_async(
                        arg_matches
                            .get_one::<String>(&format!("{}_task_queue_url", name.to_lowercase()))
                            .unwrap()
                            .as_str(),
                        true,
                        config.pool_size,
                    ))?;
                    loop_tasks(
                        task_queue.clone(),
                        config.max_concurrent_tasks as usize,
                        rt.handle().clone(),
                        python_dispatcher
                            .clone()
                            .expect("TaskQueue requires Python"),
                    );
                    task_queue_client.insert(name.to_string(), task_queue);
                }

                let mut postgres = HashMap::new();
                for (name, pg) in rt_config.postgres() {
                    let client = rt.block_on(new_postgres_async(
                        arg_matches
                            .get_one::<String>(&format!("{}_postgres_url", name.to_lowercase()))
                            .unwrap()
                            .as_str(),
                        true,
                        pg.pool_size,
                    ))?;
                    postgres.insert(name.to_string(), client);
                }

                let mut gql_roots = HashMap::new();
                for (k, gql_config) in rt_config.gql_modules() {
                    let res = rt.block_on(load_schema(
                        gql_config.schema_import.clone(),
                        gql_config.db.clone(),
                        python_dispatcher.clone().expect("GQL needs Python"),
                    ))?;
                    gql_roots.insert(k.to_string(), res);
                }

                let mut http_clients = HashMap::new();
                for (name, client_builder) in rt_config.http_client_builder() {
                    let http_client = new_http_client(client_builder)?;
                    http_clients.insert(name.to_string(), http_client);
                }

                let context = RealPuffContext::new_with_options(
                    rt.handle().clone(),
                    redis,
                    postgres,
                    python_dispatcher,
                    pubsub_client.clone(),
                    task_queue_client,
                    gql_roots,
                    http_clients,
                );

                set_puff_context(context.puff());

                for psc in pubsub_client.values() {
                    psc.start_supervised_listener();
                }

                let context_to_set = context.puff();
                {
                    let mut borrowed = mutex_switcher.lock().unwrap();
                    *borrowed = Some(context_to_set);
                }

                let runnable = runner.make_runnable(args, context)?;

                let main_fut = async {
                    let shutdown = tokio::signal::ctrl_c();
                    // let result = ctx.start()?.await;
                    tokio::select! {
                        res = runnable.0 => {
                            return res
                        }
                        _ = shutdown => {
                            // The shutdown signal has been received.
                            info!("shutting down");
                            return Ok(ExitCode::SUCCESS)
                        }
                    }
                };
                return rt.block_on(main_fut);
            }
        }

        Ok(ExitCode::SUCCESS)
    }
}
