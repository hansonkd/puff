use puff::program::commands::http::ServerCommand;
use puff::program::Program;
use puff::types::text::{Text, ToText};
use puff::web::http::{body_text, Request, Response, ResponseBuilder, Router, StatusCode};
use std::net::SocketAddr;
use std::time::Duration;

fn main() {
    // build our application with a route
    let app = Router::new().get("/", root);
    // .post("/graphql/", make_graphql_python_service("my_module.Query"))
    // .fallback(make_wsgi_service("my_module.app"));

    Program::new("my_first_app")
        .about("This is my first app")
        .command(ServerCommand::new(app))
        .run()
}

// basic handler that responds with a static string
fn root() -> Text {
    puff::tasks::task::sleep(Duration::from_secs(1));

    "ok".to_text()
}
