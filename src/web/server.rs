//! Puff HTTP Server
//!
//! The Puff web server is powered by Axum. Build up a service using the `get`, `post`, etc methods.
//! PuffHandlers are functions or closures that take Axum Extractors as arguments. Puff handlers are almost
//! the same as Axum handlers, but they can be sync and do not have to return a future.
//!
//! Sync Puff Handlers are run in a Puff context on a coroutine thread. Async Puff Handlers are not run
//! inside of puff and behave as normal Axum Handlers. In an Async Puff Handler you must
//! manually use the puff_context if you want to renter the puff context.
//!
//! All Puff handlers must return a type compatible with [axum::response::IntoResponse].
//!
//! If using an extractor, Puff handlers must also include as the final argument to their function,
//! a FromRequestExtractor (use `_: Request` to fill in a null one).
//!
//! ## Basic Example
//!
//! This example shows how to make a simple Puff web service with a variety of Request extractors and Responses.
//!
//! ```no_run
//! use std::process::ExitCode;
//! use puff_rs::program::commands::http::ServerCommand;
//! use puff_rs::program::Program;
//! use puff_rs::errors::Result;
//! use puff_rs::types::text::{Text, ToText};
//! use puff_rs::web::server::{Request, Response, ResponseBuilder, Router, Json, body_text};
//! use axum::extract::Path;
//! use std::time::Duration;
//! use serde_json::{Value, json};
//!
//!
//! fn main() -> ExitCode {
//!     // build our application with a route
//!     let app = Router::new()
//!                 .get("/", root)
//!                 .get("/user/:id", get_user);
//!
//!     Program::new("my_first_app")
//!         .about("This is my first app")
//!         .command(ServerCommand::new(app)) // Expose the `server` command to the CLI
//!         .run()
//! }
//!
//! // basic handler that responds with a static string
//! async fn root() -> Text {
//!     "ok".to_text()
//! }
//!
//! // basic handler that uses an Axum extractor. We must use a FromRequest Extractor as the final argument.
//! async fn get_user(Path(user_id): Path<String>, _: Request) -> Json<Value> {
//!     Json(json!({ "data": 42 }))
//! }
//!
//! ```
//!
use std::convert::Infallible;
use std::future::Future;

use axum::http::Request as AxumRequest;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::{self, Extension};
use std::net::SocketAddr;

use axum::body::{Body, BoxBody, Bytes};
use axum::handler::Handler;
use axum::routing::{any_service, on, IntoMakeService, MethodFilter, MethodRouter};

use hyper::server::conn::AddrIncoming;
use tower_service::Service;

pub use axum::http::StatusCode;

use crate::context::PuffContext;

use crate::types::text::Text;

pub use axum::response::Json;
pub type Request = AxumRequest<Body>;
pub type Response = AxumResponse<BoxBody>;
pub type ResponseBuilder = AxumResponse<()>;

/// Router for building a web application. Uses Axum router underneath and supports using handlers
/// with axum's Extractors
/// Run with [crate::program::commands::http::ServerCommand]
#[derive(Clone)]
pub struct Router<S = ()>(axum::Router<S>);

pub trait PuffHandler<Inp, S, Res> {
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S>;
}

pub struct AxumHandlerArgs<T>(T);

impl<F, S, T1>
    PuffHandler<AxumHandlerArgs<T1>, S, <<F as Handler<T1, S>>::Future as Future>::Output> for F
where
    S: Send + Sync + 'static,
    T1: Send + Sync + 'static,
    F: Handler<T1, S>,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, self)
    }
}

impl<S> Router<S>
where
    S: Send + Sync + Default + 'static,
{
    pub fn new() -> Self {
        Self(axum::Router::default())
    }

    pub fn on<TextLike, H, T1, Res>(self, filter: MethodFilter, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        Self(self.0.route(
            &path.into(),
            handler.into_handler(filter), // on(filter, move |disp, parts, req| internal_handler(disp, || f(parts, req).into_response())),
        ))
    }

    pub fn get<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::GET, path, handler)
    }

    pub fn post<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::POST, path, handler)
    }

    pub fn head<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::HEAD, path, handler)
    }

    pub fn options<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::OPTIONS, path, handler)
    }

    pub fn put<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::PUT, path, handler)
    }

    pub fn patch<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::PATCH, path, handler)
    }

    pub fn trace<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::TRACE, path, handler)
    }

    pub fn delete<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::DELETE, path, handler)
    }

    pub fn any<TextLike, H, T1, Res>(self, path: TextLike, handler: H) -> Self
    where
        TextLike: Into<Text>,
        Res: IntoResponse,
        H: PuffHandler<T1, S, Res>,
    {
        self.on(MethodFilter::all(), path, handler)
    }

    pub fn service<TextLike, T>(self, path: TextLike, f: T) -> Self
    where
        TextLike: Into<Text>,
        T: Service<AxumRequest<Body>, Error = Infallible> + Clone + Send + 'static,
        T::Response: IntoResponse + 'static,
        T::Future: Send + 'static,
    {
        Self(self.0.route(&path.into(), any_service(f)))
    }

    pub(crate) fn into_axum_router(self, puff_context: PuffContext) -> axum::Router<S> {
        self.0.layer(Extension(puff_context)).clone()
    }

    pub fn into_hyper_server(
        self,
        addr: &SocketAddr,
        puff_context: PuffContext,
    ) -> axum::Server<AddrIncoming, IntoMakeService<axum::Router<S>>> {
        let new_router = self.into_axum_router(puff_context);
        axum::Server::bind(addr).serve(new_router.into_make_service())
    }
}

pub fn body_iter_bytes<
    B: Into<Bytes> + 'static,
    I: IntoIterator<Item = std::result::Result<B, std::io::Error>>,
>(
    chunks: I,
) -> Body
where
    <I as IntoIterator>::IntoIter: Send + 'static,
{
    Body::wrap_stream(futures_util::stream::iter(chunks))
}

pub fn body_bytes<B>(chunks: B) -> Body
where
    B: Into<Bytes> + 'static,
{
    Body::from(chunks.into())
}

pub fn body_text<B>(chunks: B) -> Body
where
    B: Into<Text> + 'static,
{
    let t = chunks.into();
    Body::from(Bytes::copy_from_slice(t.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::PuffContext;
    use crate::types::text::ToText;
    use crate::types::Puff;
    use tokio::runtime::Runtime;

    #[test]
    fn check_router() {
        let router: Router<()> = Router::new().get("/", || async { "ok".to_text() });

        let rt = Runtime::new().unwrap();
        let puff_context = PuffContext::default();

        let fut = router.into_axum_router(puff_context.puff()).call(
            AxumRequest::get("http://localhost/")
                .body(Body::empty())
                .unwrap(),
        );

        let result = rt.block_on(fut);
        assert!(result.is_ok());
        let response = result.unwrap();
        println!("{:?}", response.body());
        assert_eq!(response.status(), StatusCode::OK);
    }
}
