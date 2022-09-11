use std::convert::Infallible;

use axum::body::{BoxBody, HttpBody};
use axum::http::Request as AxumRequest;
use axum::response::{IntoResponse, Response as AxumResponse};
use axum::{self, http, Extension};
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::errors::Error;
use axum::body::{Body, Bytes};
use axum::handler::{Handler as AxumHandler, Handler};
use axum::routing::{any_service, on, IntoMakeService, MethodFilter, MethodRouter};
use futures::future::BoxFuture;
use futures::FutureExt;
use hyper::server::conn::AddrIncoming;
use tower_service::Service;
use tracing::error;

pub use axum::http::StatusCode;
use axum::routing::future::IntoMakeServiceFuture;

use axum::async_trait;
use axum::extract::{FromRequest, FromRequestParts};

use crate::tasks::dispatcher::Dispatcher;
use crate::tasks::DISPATCHER;
use crate::types::text::Text;

pub type Request = AxumRequest<Body>;
pub type Response = AxumResponse<Body>;
pub type ResponseBuilder = AxumResponse<()>;

#[derive(Clone)]
pub struct Router<S = ()>(axum::Router<S>);

async fn internal_handler<F>(
    Extension(dispatcher): Extension<Arc<Dispatcher>>,
    f: F,
) -> AxumResponse<BoxBody>
where
    F: FnOnce() -> AxumResponse<BoxBody> + Send + Sync + 'static,
{
    let res = dispatcher.dispatch(|| Ok(f())).await;
    match res {
        Ok(r) => r,
        Err(r) => {
            error!("Error processing request: {:?}", r);
            AxumResponse::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::empty())
                .unwrap()
                .into_response()
        }
    }
}

pub trait PuffHandler<Inp, S, Res> {
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S>;
}

struct AxumHandlerArgs<T>(T);

impl<F, S, T1, Res> PuffHandler<AxumHandlerArgs<T1>, S, Res> for F
where
    Res: IntoResponse,
    S: Send + Sync + 'static,
    T1: Send + Sync + 'static,
    F: Handler<T1, S>,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, self)
    }
}

impl<F, S, Res> PuffHandler<(), S, Res> for F
where
    Res: IntoResponse,
    S: Send + Sync + 'static,
    F: FnOnce() -> Res + Send + Sync + Clone + 'static,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, move |disp| {
            internal_handler(disp, || self().into_response())
        })
    }
}

impl<F, S, Req, Res> PuffHandler<(Req,), S, Res> for F
where
    Res: IntoResponse,
    S: Send + Sync + 'static,
    Req: FromRequest<S, Body> + Send + Sync + 'static,
    F: FnOnce(Req) -> Res + Send + Sync + Clone + 'static,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, move |disp, req| {
            internal_handler(disp, || self(req).into_response())
        })
    }
}

impl<F, S, Req, T1, Res> PuffHandler<(Req, T1), S, Res> for F
where
    Res: IntoResponse,
    S: Send + Sync + 'static,
    Req: FromRequest<S, Body> + Send + Sync + 'static,
    T1: FromRequestParts<S> + Send + Sync + 'static,
    F: FnOnce(T1, Req) -> Res + Send + Sync + Clone + 'static,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, move |disp, parts, req| {
            internal_handler(disp, || self(parts, req).into_response())
        })
    }
}

impl<F, S, Req, T1, T2, Res> PuffHandler<(Req, T1, T2), S, Res> for F
where
    Res: IntoResponse,
    S: Send + Sync + 'static,
    Req: FromRequest<S, Body> + Send + Sync + 'static,
    T1: FromRequestParts<S> + Send + Sync + 'static,
    T2: FromRequestParts<S> + Send + Sync + 'static,
    F: FnOnce(T1, T2, Req) -> Res + Send + Sync + Clone + 'static,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, move |disp, parts, parts2, req| {
            internal_handler(disp, || self(parts, parts2, req).into_response())
        })
    }
}

impl<F, S, Req, T1, T2, T3, Res> PuffHandler<(Req, T1, T2, T3), S, Res> for F
where
    Res: IntoResponse,
    S: Send + Sync + 'static,
    Req: FromRequest<S, Body> + Send + Sync + 'static,
    T1: FromRequestParts<S> + Send + Sync + 'static,
    T2: FromRequestParts<S> + Send + Sync + 'static,
    T3: FromRequestParts<S> + Send + Sync + 'static,
    F: FnOnce(T1, T2, T3, Req) -> Res + Send + Sync + Clone + 'static,
{
    fn into_handler(self, filter: MethodFilter) -> MethodRouter<S> {
        on(filter, move |disp, parts, parts2, parts3, req| {
            internal_handler(disp, || self(parts, parts2, parts3, req).into_response())
        })
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
        self.on(MethodFilter::TRACE, path, handler)
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

    pub fn into_hyper_server(
        self,
        addr: &SocketAddr,
        dispatcher: Arc<Dispatcher>,
    ) -> axum::Server<AddrIncoming, IntoMakeService<axum::Router<S>>> {
        let new_router = self.0.layer(Extension(dispatcher)).clone();
        axum::Server::bind(addr).serve(new_router.into_make_service())
    }
}

pub fn body_iter_bytes<B: Into<Bytes> + 'static, I: IntoIterator<Item = Result<B, Error>>>(
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
    use crate::tasks::dispatcher::Dispatcher;
    use tokio::runtime::Runtime;

    #[test]
    fn check_router() {
        let mut router = Router::new().get(
            "/",
            FnHandler(Arc::new(|_| {
                AxumResponse::builder()
                    .status(StatusCode::OK)
                    .body(body_bytes(vec![Ok("ok")]))
                    .unwrap()
            })),
        );

        let fut = router.0.call(
            AxumRequest::get("http://localhost/")
                .body(Body::empty())
                .unwrap(),
        );

        let rt = Runtime::new().unwrap();
        let dispatcher = Arc::new(Dispatcher::default());
        let result = rt.block_on(DISPATCHER.scope(dispatcher, fut));
        assert!(result.is_ok());
        let response = result.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
