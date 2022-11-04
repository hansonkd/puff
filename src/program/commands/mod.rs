//! Commands to build a program.
use crate::context::PuffContext;
use crate::errors::{PuffResult, Result};
use crate::program::{Runnable, RunnableCommand};
use crate::types::Text;
use anyhow::anyhow;
use clap::{Arg, ArgMatches, Command};
use hyper::server::conn::AddrIncoming;
use hyper::server::Builder;
use std::future::Future;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::Duration;
use tracing::info;

pub use asgi::ASGIServerCommand;
pub use django_management::DjangoManagementCommand;
pub use http::ServerCommand;
pub use pytest::PytestCommand;
pub use python::PythonCommand;
pub use wsgi::WSGIServerCommand;

pub mod asgi;
pub mod django_management;
pub mod http;
pub mod pytest;
pub mod python;
pub mod wsgi;

/// Expose a future to the command line.
pub struct BasicCommand<F: Future<Output = PuffResult<ExitCode>> + 'static> {
    command: Command,
    inner_func: Mutex<Option<F>>,
}

impl<Fut: Future<Output = PuffResult<ExitCode>> + 'static> BasicCommand<Fut> {
    pub fn new<T: Into<Text>>(name: T, f: Fut) -> Self {
        Self {
            inner_func: Mutex::new(Some(f)),
            command: Command::new(name.into().to_string()),
        }
    }

    pub fn new_with_options(command: Command, f: Fut) -> Self {
        Self {
            inner_func: Mutex::new(Some(f)),
            command: command,
        }
    }
}

impl<F: Future<Output = PuffResult<ExitCode>> + 'static> RunnableCommand for BasicCommand<F> {
    fn cli_parser(&self) -> Command {
        self.command.clone()
    }

    fn make_runnable(&mut self, _args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let this_self_func = self
            .inner_func
            .lock()
            .unwrap()
            .take()
            .ok_or(anyhow!("Already ran command."))?;
        Ok(Runnable::new(this_self_func))
    }
}

/// Run Puff runtime until terminated
pub struct WaitForever;

impl WaitForever {
    pub fn new() -> Self {
        Self
    }
}

impl RunnableCommand for WaitForever {
    fn cli_parser(&self) -> Command {
        Command::new("wait_forever").about("Keep the program alive until terminated by a signal.")
    }

    fn make_runnable(&mut self, _args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let fut = async {
            info!("Program will now wait forever.");
            loop {
                tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
            }
        };
        Ok(Runnable::new(fut))
    }
}

/// Configuration for how to run an Axum Server.
pub struct HttpServerConfig {
    socket_addr: SocketAddr,
    reuse_port: bool,
}

impl HttpServerConfig {
    pub fn server_builder(&self) -> Builder<AddrIncoming> {
        info!("Serving on http://{}", &self.socket_addr);
        if self.reuse_port {
            let sock = socket2::Socket::new(
                match self.socket_addr {
                    SocketAddr::V4(_) => socket2::Domain::IPV4,
                    SocketAddr::V6(_) => socket2::Domain::IPV6,
                },
                socket2::Type::STREAM,
                None,
            )
            .unwrap();

            sock.set_reuse_address(true).unwrap();
            sock.set_reuse_port(true).unwrap();
            sock.set_nonblocking(true).unwrap();
            sock.bind(&self.socket_addr.into()).unwrap();
            sock.listen(8192).unwrap();
            sock.set_keepalive(true).unwrap();
            sock.set_nodelay(true).unwrap();

            axum::Server::from_tcp(sock.into()).unwrap()
        } else {
            axum::Server::bind(&self.socket_addr)
        }
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
