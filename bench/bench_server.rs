/// Standalone Puff WSGI server for benchmarking.
/// Avoids GraphQL subscription code that has Unpin issues under free-threaded Python.
use puff_rs::program::commands::WSGIServerCommand;
use puff_rs::program::Program;
use puff_rs::runtime::RuntimeConfig;

fn main() {
    let mut rc = RuntimeConfig::new();
    rc.add_cwd_to_python_path();

    let wsgi_module = std::env::var("BENCH_WSGI").unwrap_or("wsgi_app.application".to_string());

    Program::new("puff-bench")
        .about("Puff WSGI benchmark server")
        .runtime_config(rc)
        .command(WSGIServerCommand::new(wsgi_module))
        .try_run();
}
