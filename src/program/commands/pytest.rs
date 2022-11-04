//! Run Pytest in a Puff Context.
use crate::context::PuffContext;
use crate::errors::PuffResult;
use crate::program::{Runnable, RunnableCommand};
use crate::types::Text;
use clap::{Arg, ArgMatches, Command};
use pyo3::prelude::*;
use pyo3::types::PyList;
use std::path::PathBuf;
use std::process::ExitCode;

/// The PytestCommand.
///
/// Run pytest for the project.
#[derive(Clone)]
pub struct PytestCommand {
    path: PathBuf,
}

impl PytestCommand {
    pub fn new<N: Into<Text>>(path: N) -> Self {
        let input_path: PathBuf = path.into().parse().expect("Could not convert text to path");
        let path = if input_path.is_relative() {
            let cwd = std::env::current_dir()
                .expect("Could not read Current Working Directory and path is relative.");
            cwd.join(input_path)
        } else {
            input_path
        };
        Self { path }
    }
}

impl RunnableCommand for PytestCommand {
    fn cli_parser(&self) -> Command {
        Command::new("pytest")
            .about("Run pytest in a Puff Context")
            .arg(
                Arg::new("arg")
                    .num_args(1..)
                    .value_name("ARG")
                    .help("Arguments to pass to Pytest."),
            )
            .disable_help_flag(true)
    }

    fn make_runnable(&mut self, args: &ArgMatches, context: PuffContext) -> PuffResult<Runnable> {
        let (pytest_args, python_function) = Python::with_gil(|py| {
            let pytest_args = PyList::empty(py);
            pytest_args.append(format!(
                "--rootdir={}",
                self.path
                    .as_path()
                    .as_os_str()
                    .to_str()
                    .expect("Invalid path")
            ))?;
            pytest_args.append("--import-mode=importlib")?;
            for arg in args.get_raw("arg").unwrap_or_default() {
                pytest_args.append(arg.into_py(py))?;
            }
            let run_pytest = py.import("pytest")?.getattr("main")?;
            PyResult::Ok((pytest_args.to_object(py), run_pytest.into_py(py)))
        })?;
        let run_path = self.path.clone();
        let fut = async move {
            std::env::set_current_dir(run_path.as_path())?;
            let res = context
                .python_dispatcher()
                .dispatch1(python_function, (pytest_args,))?;
            let r = res.await??;
            let exit_status = Python::with_gil(|py| r.extract::<u8>(py))?;
            Ok(ExitCode::from(exit_status))
        };
        Ok(Runnable::new(fut))
    }
}
