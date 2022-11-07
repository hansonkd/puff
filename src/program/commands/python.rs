//! Run a Python function on Puff Greenlet.
use crate::context::PuffContext;
use crate::errors::PuffResult;
use crate::program::{Runnable, RunnableCommand};
use crate::types::Text;
use clap::{ArgMatches, Command};
use pyo3::prelude::*;
use serde::Serialize;
use std::marker::PhantomData;
use std::process::ExitCode;
use tracing::info;

/// The PythonCommand.
///
/// Assigns a Python function to a command name.
#[derive(Clone)]
pub struct PythonCommand {
    command_name: Text,
    function_path: Text,
}

impl PythonCommand {
    pub fn new<N: Into<Text>, M: Into<Text>>(command_name: N, function_path: M) -> Self {
        Self {
            command_name: command_name.into(),
            function_path: function_path.into(),
        }
    }
}

impl RunnableCommand for PythonCommand {
    fn cli_parser(&self) -> Command {
        Command::new(self.command_name.to_string()).about(format!(
            "Execute python function `{}` in a Puff context.",
            &self.function_path
        ))
    }

    fn make_runnable(&mut self, _args: &ArgMatches, context: PuffContext) -> PuffResult<Runnable> {
        let (python_function, is_coroutine) = Python::with_gil(|py| {
            let puff_mod = py.import("puff")?;
            let inspect_mod = py.import("inspect")?;
            let f = puff_mod
                .call_method1("import_string", (self.function_path.clone().into_py(py),))?
                .into_py(py);
            let is_coroutine = inspect_mod
                .call_method1("iscoroutinefunction", (f.as_ref(py),))?
                .extract::<bool>()?;
            PyResult::Ok((f, is_coroutine))
        })?;
        let fp = self.function_path.clone();
        let fut = async move {
            info!("Running {}", fp);
            let res = if is_coroutine {
                context
                    .python_dispatcher()
                    .dispatch_asyncio(python_function, (), None)?
            } else {
                context.python_dispatcher().dispatch1(python_function, ())?
            };

            res.await??;
            Ok(ExitCode::SUCCESS)
        };
        Ok(Runnable::new(fut))
    }
}

/// The PythonCommandOpts.
///
/// Assigns a Python function to a command name and runs with clap options.
#[derive(Clone)]
pub struct PythonCommandOpts<T: clap::Parser + clap::CommandFactory + Serialize + Sized + 'static> {
    parser: PhantomData<T>,
    function_path: Text,
}

impl<T: clap::Parser + clap::CommandFactory + Serialize + Sized + 'static> PythonCommandOpts<T> {
    pub fn new<M: Into<Text>>(function_path: M) -> Self {
        Self {
            parser: PhantomData,
            function_path: function_path.into(),
        }
    }
}

impl<T: clap::Parser + clap::CommandFactory + Serialize + Sized + 'static> RunnableCommand
    for PythonCommandOpts<T>
{
    fn cli_parser(&self) -> Command {
        T::command()
    }

    fn make_runnable(&mut self, args: &ArgMatches, context: PuffContext) -> PuffResult<Runnable> {
        let c = T::from_arg_matches(args)?;
        let (python_function, is_coroutine) = Python::with_gil(|py| {
            let puff_mod = py.import("puff")?;
            let inspect_mod = py.import("inspect")?;
            let f = puff_mod
                .call_method1("import_string", (self.function_path.clone().into_py(py),))?
                .into_py(py);
            let is_coroutine = inspect_mod
                .call_method1("iscoroutinefunction", (f.as_ref(py),))?
                .extract::<bool>()?;
            PyResult::Ok((f, is_coroutine))
        })?;
        let fp = self.function_path.clone();
        let fut = async move {
            info!("Running {}", fp);
            let res = if is_coroutine {
                Python::with_gil(|py| {
                    let payload = pythonize::pythonize(py, &c)?;
                    context
                        .python_dispatcher()
                        .dispatch_asyncio(python_function, (payload,), None)
                })?
            } else {
                context.python_dispatcher().dispatch1(python_function, ())?
            };

            res.await??;
            Ok(ExitCode::SUCCESS)
        };
        Ok(Runnable::new(fut))
    }
}
