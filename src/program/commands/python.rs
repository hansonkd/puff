use crate::context::PuffContext;
use crate::errors::PuffResult;
use crate::program::{Runnable, RunnableCommand};
use crate::types::{Puff, Text};
use clap::{ArgMatches, Command};
use pyo3::prelude::*;

/// The WSGIServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
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
        Command::new(self.command_name.puff())
    }

    fn runnable_from_args(&self, _args: &ArgMatches, context: PuffContext) -> PuffResult<Runnable> {
        let python_function = Python::with_gil(|py| {
            let puff_mod = py.import("puff")?;
            let f = puff_mod
                .call_method1("import_string", (self.function_path.clone().into_py(py),))?
                .into_py(py);
            PyResult::Ok(f)
        })?;

        let fut = async move {
            let res = context.python_dispatcher().dispatch1(python_function, ())?;
            res.await??;
            Ok(())
        };
        Ok(Runnable::new(fut))
    }
}
