use crate::context::PuffContext;
use crate::errors::PuffResult;
use crate::program::{Runnable, RunnableCommand};
use crate::types::{Puff, Text};
use clap::{Arg, ArgMatches, Command};
use pyo3::prelude::*;
use pyo3::types::PyDict;

/// The WSGIServerCommand.
///
/// Exposes options to the command line to set the port and host of the server.
#[derive(Clone)]
pub struct DjangoManagementCommand {}

impl DjangoManagementCommand {
    pub fn new() -> Self {
        Self {}
    }
}

impl RunnableCommand for DjangoManagementCommand {
    fn cli_parser(&self) -> Command {
        Command::new("django").arg(
            Arg::new("arg")
                .num_args(1..)
                .value_name("ARG")
                .help("Arguments to pass to Django."),
        )
    }

    fn runnable_from_args(&self, args: &ArgMatches, context: PuffContext) -> PuffResult<Runnable> {
        let subcommand = args.subcommand_name().unwrap_or("django");

        let (django_args, python_function) = Python::with_gil(|py| {
            let mut django_args = vec![subcommand.into_py(py)];
            for arg in args.get_raw("arg").unwrap_or_default() {
                django_args.push(arg.into_py(py))
            }
            let management = py.import("puff.django.management")?;
            let execute_fn = management.getattr("get_management_utility_execute")?;
            PyResult::Ok((django_args, execute_fn.into_py(py)))
        })?;

        let fut = async move {
            let res = Python::with_gil(|py| {
                context.python_dispatcher().dispatch_blocking(
                    py,
                    python_function,
                    (django_args,),
                    PyDict::new(py),
                )
            })?;
            res.await??;
            Ok(())
        };
        Ok(Runnable::new(fut))
    }
}
