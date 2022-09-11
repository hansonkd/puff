use clap::{ArgMatches, Command};
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tracing::error;

use crate::errors::{Error, Result};
use crate::tasks::dispatcher::Dispatcher;
use crate::tasks::DISPATCHER;
use crate::types::text::Text;

pub mod commands;

pub type CommandError = Error;

pub struct Runnable(Pin<Box<dyn Future<Output = Result<()>> + 'static>>);

impl Runnable {
    fn new<F: Future<Output = Result<()>> + 'static>(inner: F) -> Self {
        Self(Box::pin(inner))
    }
}

pub trait RunnableCommand: 'static {
    fn cli_parser(&self) -> Command;
    fn runnable_from_args(
        &self,
        args: &ArgMatches,
        dispatcher: Arc<Dispatcher>,
    ) -> Result<Runnable>;
}

#[derive(Clone)]
pub struct PackedCommand(Arc<dyn RunnableCommand>);

impl PackedCommand {
    pub fn cli_parser(&self) -> Command {
        self.0.cli_parser()
    }

    pub fn runnable_from_args(
        &self,
        args: &ArgMatches,
        dispatcher: Arc<Dispatcher>,
    ) -> Result<Runnable> {
        self.0.runnable_from_args(args, dispatcher)
    }
}

#[derive(Clone)]
pub struct Program {
    name: Text,
    author: Option<Text>,
    version: Option<Text>,
    about: Option<Text>,
    after_help: Option<Text>,
    commands: Vec<PackedCommand>,
}

impl Program {
    pub fn new<T: Into<Text>>(name: T) -> Self {
        Self {
            name: name.into(),
            commands: Vec::new(),
            version: None,
            about: None,
            after_help: None,
            author: None,
        }
    }

    pub fn author<T: Into<Text>>(&self, author: T) -> Self {
        let mut s = self.clone();
        s.author = Some(author.into());
        s
    }

    pub fn version<T: Into<Text>>(&self, version: T) -> Self {
        let mut s = self.clone();
        s.version = Some(version.into());
        s
    }

    pub fn about<T: Into<Text>>(&self, about: T) -> Self {
        let mut s = self.clone();
        s.about = Some(about.into());
        s
    }

    pub fn after_help<T: Into<Text>>(&self, after_help: T) -> Self {
        let mut s = self.clone();
        s.after_help = Some(after_help.into());
        s
    }

    pub fn command<C: RunnableCommand>(&self, command: C) -> Self {
        let arc_command = Arc::new(command);
        let mut new_self = self.clone();

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

        tl
    }

    pub fn run(&self) -> () {
        self.try_run().unwrap()
    }

    pub fn try_run(&self) -> Result<()> {
        let mut top_level = self.clap_command();

        let mut hm: HashMap<Text, PackedCommand> = HashMap::with_capacity(self.commands.len());
        for packed_command in &self.commands {
            let parser = packed_command.cli_parser();
            let name = parser.get_name().to_owned();
            top_level = top_level.subcommand(parser);
            hm.insert(name.into(), packed_command.clone());
        }

        let arg_matches = top_level.get_matches();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        if let Some((command, args)) = arg_matches.subcommand() {
            if let Some(runner) = hm.remove(&command.to_string().into()) {
                let dispatcher = Arc::new(Dispatcher::default());
                let runnable = runner.runnable_from_args(args, dispatcher.clone())?;

                rt.block_on(DISPATCHER.scope(dispatcher, runnable.0))?;
            }
        }

        Ok(())
    }
}
