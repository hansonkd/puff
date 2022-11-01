//! Run a Python function on Puff Greenlet.
use crate::context::PuffContext;
use crate::errors::PuffResult;
use crate::program::{Runnable, RunnableCommand};
use crate::types::Text;
use clap::{ArgMatches, Command};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::process::ExitCode;

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

        let fut = async move {
            let res = if is_coroutine {
                Python::with_gil(|py| {
                    context.python_dispatcher().dispatch_asyncio(
                        py,
                        python_function,
                        (),
                        PyDict::new(py),
                    )
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
