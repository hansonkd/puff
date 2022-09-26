use puff::errors::Result;
use puff::program::commands::http::ServerCommand;
use puff::program::Program;
use puff::types::text::{Text};
use puff::web::client::{Client, PuffClientResponse, PuffRequestBuilder};
use puff::web::server::Router;


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

// Basic handler that responds with a static string
fn root() -> Result<Text> {
    let client = Client::new();
    client
        .get("http://google.com/")
        .puff_response()?
        .puff_text()
}
