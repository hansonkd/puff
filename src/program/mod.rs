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
//! use clap::{ArgMatches, Command};
//! use puff::program::{Program, Runnable, RunnableCommand};
//! use puff::context::PuffContext;
//!
//! struct MyCommand;
//!
//! impl RunnableCommand for MyCommand {
//!     fn cli_parser(&self) -> Command {
//!         Command::new("my_custom_command")
//!     }
//!
//!     fn runnable_from_args(&self, args: &ArgMatches, context: PuffContext) -> puff::errors::Result<Runnable> {
//!         Ok(Runnable::new(context.dispatcher().dispatch(|| {
//!             println!("Hello World from a Puff coroutine!");
//!             Ok(())
//!         })))
//!     }
//! }
//!
//! fn main() {
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

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio::runtime::Builder;
use tokio::sync::broadcast;

use crate::context::{set_puff_context, set_puff_context_waiting, PuffContext};
use crate::databases::postgres::{add_postgres_command_arguments, new_postgres_async};
use crate::databases::pubsub::{add_pubsub_command_arguments, new_pubsub_async};
use crate::errors::Result;
use crate::python::{bootstrap_puff_globals, setup_greenlet};
use crate::runtime::dispatcher::Dispatcher;
use crate::runtime::RuntimeConfig;
use crate::types::text::Text;
use crate::types::Puff;
use tracing::{error, info};

pub mod commands;

/// A wrapper for a boxed future that is able to be run by a Puff Program.
pub struct Runnable(Pin<Box<dyn Future<Output = Result<()>> + 'static>>);

impl Runnable {
    pub fn new<F: Future<Output = Result<()>> + 'static>(inner: F) -> Self {
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
/// use clap::{ArgMatches, Command};
/// use puff::program::{Runnable, RunnableCommand};
/// use puff::context::PuffContext;
///
/// struct MyCommand;
///
/// impl RunnableCommand for MyCommand {
///     fn cli_parser(&self) -> Command {
///         Command::new("my_custom_command")
///     }
///
///     fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> puff::errors::Result<Runnable> {
///         Ok(Runnable::new(context.dispatcher().dispatch(|| {
///             // Do something in a Puff coroutine
///             Ok(())
///         })))
///     }
/// }
/// ```
pub trait RunnableCommand: 'static {
    /// The [clap::Command] that specifies the arguments and meta information.
    fn cli_parser(&self) -> Command;

    /// Converts parsed matches from the command line into a Runnable future.
    fn runnable_from_args(&self, args: &ArgMatches, dispatcher: PuffContext) -> Result<Runnable>;
}

#[derive(Clone)]
struct PackedCommand(Arc<dyn RunnableCommand>);

impl PackedCommand {
    pub fn cli_parser(&self) -> Command {
        self.0.cli_parser()
    }

    pub fn runnable_from_args(
        &self,
        args: &ArgMatches,
        dispatcher: PuffContext,
    ) -> Result<Runnable> {
        self.0.runnable_from_args(args, dispatcher)
    }
}

/// A Puff Program that is responsible for parsing CLI arguments and starting the Runtime.
#[derive(Clone)]
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
        let arc_command = Arc::new(command);
        let mut new_self = self;

        new_self.commands.push(PackedCommand(arc_command));
        new_self
    }

    fn clap_command(&self) -> Command {
        let mut tl = Command::new(self.name.clone().into_string()).arg_required_else_help(true);
        if let Some(author) = &self.author {
            tl = tl.author(author.as_str());
        }

        if let Some(about) = &self.about {
            tl = tl.about(about.as_str());
        }

        if let Some(version) = &self.version {
            tl = tl.version(version.as_str());
        }

        if let Some(after_help) = &self.after_help {
            tl = tl.after_help(after_help.as_str());
        }

        tl.allow_invalid_utf8_for_external_subcommands(true)
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
            .thread_keep_alive(self.runtime_config.blocking_task_keep_alive())
            .thread_stack_size(self.runtime_config.stack_size());

        Ok(rt)
    }

    /// Run the program and panics if it fails.
    ///
    /// See [Self::try_run] for more information.
    pub fn run(&self) -> () {
        match self.try_run() {
            Ok(()) => (),
            Err(e) => {
                error!("Error running command: {e}")
            }
        }
    }

    /// Tries to run the program and returns an Error if it fails.
    ///
    /// This will parse the command line arguments, start a new runtime, dispatcher and
    /// coroutine worker threads and blocks until the command finishes.
    pub fn try_run(&self) -> Result<()> {
        tracing_subscriber::fmt::init();
        let (notify_shutdown, _) = broadcast::channel(1);

        let mut top_level = self.clap_command();

        if self.runtime_config.postgres() {
            top_level = add_postgres_command_arguments(top_level)
        }

        if self.runtime_config.redis() {
            top_level = add_redis_command_arguments(top_level)
        }

        if self.runtime_config.pubsub() {
            top_level = add_pubsub_command_arguments(top_level)
        }

        let mut hm: HashMap<Text, PackedCommand> = HashMap::with_capacity(self.commands.len());
        for packed_command in &self.commands {
            let parser = packed_command.cli_parser();
            let name = parser.get_name().to_owned();
            top_level = top_level.subcommand(parser);
            hm.insert(name.into(), packed_command.clone());
        }

        let arg_matches = top_level.get_matches();

        if let Some((command, args)) = arg_matches.subcommand() {
            if let Some(runner) = hm.remove(&command.to_string().into()) {
                let mut builder = self.runtime()?;
                let mutex_switcher = Arc::new(Mutex::new(None::<PuffContext>));

                let python_dispatcher = if self.runtime_config.python() {
                    pyo3::prepare_freethreaded_python();
                    let global_obj = self.runtime_config.global_state()?;
                    bootstrap_puff_globals(global_obj);
                    Some(setup_greenlet(
                        self.runtime_config.clone(),
                        mutex_switcher.clone(),
                    )?)
                } else {
                    None
                };

                let thread_mutex = mutex_switcher.clone();

                builder.on_thread_start(move || {
                    set_puff_context_waiting(thread_mutex.clone());
                });

                let rt = builder.build()?;
                let mut redis = None;
                if self.runtime_config.redis() {
                    redis = Some(rt.block_on(new_redis_async(
                        arg_matches.value_of("redis_url").unwrap(),
                        true,
                    ))?);
                }

                let mut pubsub_client = None;
                if self.runtime_config.pubsub() {
                    pubsub_client = Some(rt.block_on(new_pubsub_async(
                        arg_matches.value_of("pubsub_url").unwrap(),
                        true,
                    ))?);
                }

                let mut postgres = None;
                if self.runtime_config.postgres() {
                    postgres = Some(rt.block_on(new_postgres_async(
                        arg_matches.value_of("postgres_url").unwrap(),
                        true,
                    ))?);
                }
                let (dispatcher, waiting) =
                    Dispatcher::new(notify_shutdown, self.runtime_config.clone());
                let arc_dispatcher = Arc::new(dispatcher);

                arc_dispatcher.start_monitor();

                let context = PuffContext::new_with_options(
                    rt.handle().clone(),
                    arc_dispatcher,
                    redis,
                    postgres,
                    python_dispatcher,
                    pubsub_client.clone(),
                );

                for i in waiting {
                    i.send(context.puff()).unwrap_or(());
                }

                set_puff_context(context.puff());

                pubsub_client.map(|c| c.start_supervised_listener());

                let context_to_set = context.puff();
                {
                    let mut borrowed = mutex_switcher.lock().unwrap();
                    *borrowed = Some(context_to_set);
                }

                let runnable = runner.runnable_from_args(args, context)?;

                let main_fut = async {
                    let shutdown = tokio::signal::ctrl_c();
                    // let result = ctx.start()?.await;
                    tokio::select! {
                        res = runnable.0 => {
                            // If an error is received here, accepting connections from the TCP
                            // listener failed multiple times and the server is giving up and
                            // shutting down.
                            //
                            // Errors encountered when handling individual connections do not
                            // bubble up to this point.
                            if let Err(err) = res {
                                error!(cause = %err, "failed to start command");
                            }
                        }
                        _ = shutdown => {
                            // The shutdown signal has been received.
                            info!("shutting down");
                        }
                    }
                };
                rt.block_on(main_fut);
            }
        }

        Ok(())
    }
}
