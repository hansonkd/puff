use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use crate::types::Text;
use anyhow::anyhow;
use clap::{Arg, ArgMatches, Command};
use hyper::server::conn::AddrIncoming;
use hyper::server::Builder;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Mutex;

pub mod django_management;
pub mod http;
pub mod pytest;
pub mod python;
pub mod wsgi;

pub struct BasicCommand<F: FnOnce() -> Result<ExitCode> + Send + 'static> {
    name: Text,
    inner_func: Mutex<Option<F>>,
}

impl<F: FnOnce() -> Result<ExitCode> + Send + 'static> BasicCommand<F> {
    pub fn new<T: Into<Text>>(name: T, f: F) -> Self {
        Self {
            name: name.into(),
            inner_func: Mutex::new(Some(f)),
        }
    }
}

impl<F: FnOnce() -> Result<ExitCode> + Send + Sync + 'static> RunnableCommand for BasicCommand<F> {
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

pub struct HttpServerConfig {
    socket_addr: SocketAddr,
    reuse_port: bool,
}

impl HttpServerConfig {
    pub fn server_builder(&self) -> Builder<AddrIncoming> {
        return axum::Server::bind(&self.socket_addr);
    }

    pub fn add_command_options(cmd: Command) -> Command {
        cmd.arg(
            Arg::new("bind")
                .long("bind")
                .value_parser(clap::value_parser!(SocketAddr))
                .env("PUFF_BIND")
                .num_args(1)
                .default_value("127.0.0.1:7777")
                .help("The host and port the HTTP server will bind to."),
        )
        .arg(
            Arg::new("reuse-port")
                .long("reuse-port")
                .value_parser(clap::value_parser!(bool))
                .env("PUFF_REUSE_PORT")
                .default_value("false")
                .help("Let multiple servers bind on the same port."),
        )
    }

    pub fn new_from_args(args: &ArgMatches) -> Self {
        let socket_addr = args.get_one::<SocketAddr>("bind").unwrap();
        let reuse_port = args.get_one::<bool>("reuse-port").unwrap();

        Self {
            socket_addr: socket_addr.clone(),
            reuse_port: reuse_port.clone(),
        }
    }
}
