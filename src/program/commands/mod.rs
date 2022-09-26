use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::types::Text;
use anyhow::anyhow;
use clap::{ArgMatches, Command};
use std::sync::Mutex;

pub mod asgi;
pub mod http;
pub mod wsgi;

pub struct BasicCommand<F: FnOnce() -> Result<()> + Send + 'static> {
    name: Text,
    inner_func: Mutex<Option<F>>,
}

impl<F: FnOnce() -> Result<()> + Send + 'static> BasicCommand<F> {
    pub fn new<T: Into<Text>>(name: T, f: F) -> Self {
        Self {
            name: name.into(),
            inner_func: Mutex::new(Some(f)),
        }
    }
}

impl<F: FnOnce() -> Result<()> + Send + Sync + 'static> RunnableCommand for BasicCommand<F> {
    fn cli_parser(&self) -> Command {
        Command::new(self.name.to_string())
    }

    fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> Result<Runnable> {
        let this_self_func = self
            .inner_func
            .lock()
            .unwrap()
            .take()
            .ok_or(anyhow!("Already ran command."))?;
        let fut = context.dispatcher().dispatch(this_self_func);
        Ok(Runnable::new(fut))
    }
}
