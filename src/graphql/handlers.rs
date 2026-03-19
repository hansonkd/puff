//! HTTP handlers for GraphQL using async-graphql-axum.

use axum::body::Body;
use axum::extract::FromRequest;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;

use anyhow::Error;
use std::future;
use std::sync::Arc;

use crate::types::Text;

use crate::context::with_puff_context;
use crate::errors::{log_puff_error, PuffResult};
use crate::graphql::PuffGraphqlConfig;
use crate::python::postgres::close_conn;
use crate::python::{py_obj_to_bytes, PythonDispatcher};
use futures_util::FutureExt;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict};
use pyo3::{PyObject, Python};
use serde::Deserialize;
use serde_json::{Map, Value};

/// The query variables for a GET request
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct GetQueryVariables {
    query: String,
    operation_name: Option<String>,
    variables: Option<String>,
}

/// The request body for JSON POST
#[derive(Deserialize, Debug)]
#[serde(untagged)]
enum JsonRequestBody {
    Single(SingleRequestBody),
    Batch(Vec<SingleRequestBody>),
}

/// The request body for a single JSON POST request
#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct SingleRequestBody {
    query: String,
    operation_name: Option<String>,
    variables: Option<Map<String, Value>>,
}

impl JsonRequestBody {
    fn is_empty_batch(&self) -> bool {
        match self {
            JsonRequestBody::Batch(r) => r.is_empty(),
            JsonRequestBody::Single(_) => false,
        }
    }
}

/// Our request wrapper (replaces JuniperPuffRequest)
#[derive(Debug)]
pub struct PuffGraphqlRequest(pub async_graphql::Request);

impl From<SingleRequestBody> for PuffGraphqlRequest {
    fn from(value: SingleRequestBody) -> Self {
        let mut request = async_graphql::Request::new(&value.query);
        if let Some(op) = value.operation_name {
            request = request.operation_name(op);
        }
        if let Some(vars) = value.variables {
            let variables = async_graphql::Variables::from_json(serde_json::Value::Object(vars));
            request = request.variables(variables);
        }
        PuffGraphqlRequest(request)
    }
}

impl From<String> for PuffGraphqlRequest {
    fn from(query: String) -> Self {
        PuffGraphqlRequest(async_graphql::Request::new(query))
    }
}

impl From<GetQueryVariables> for PuffGraphqlRequest {
    fn from(value: GetQueryVariables) -> Self {
        let mut request = async_graphql::Request::new(value.query);
        if let Some(op) = value.operation_name {
            request = request.operation_name(op);
        }
        if let Some(var_str) = value.variables {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&var_str) {
                let variables = async_graphql::Variables::from_json(val);
                request = request.variables(variables);
            }
        }
        PuffGraphqlRequest(request)
    }
}

impl<S: Send + Sync> FromRequest<S> for PuffGraphqlRequest {
    type Rejection = (StatusCode, &'static str);

    async fn from_request(req: Request<Body>, _state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());

        match (req.method().clone(), content_type.as_deref()) {
            (Method::GET, _) => {
                let query_vars = axum::extract::Query::<GetQueryVariables>::from_request(req, &())
                    .await
                    .map(|result| result.0)
                    .map_err(|_err| (StatusCode::BAD_REQUEST, "Request not valid"))?;
                Ok(PuffGraphqlRequest::from(query_vars))
            }
            (Method::POST, Some("application/json")) => {
                let json_body = Json::<JsonRequestBody>::from_request(req, &())
                    .await
                    .map_err(|_err| (StatusCode::BAD_REQUEST, "JSON invalid"))
                    .map(|result| result.0)?;

                if json_body.is_empty_batch() {
                    return Err((StatusCode::BAD_REQUEST, "Batch request can not be empty"));
                }

                match json_body {
                    JsonRequestBody::Single(r) => Ok(PuffGraphqlRequest::from(r)),
                    JsonRequestBody::Batch(requests) => {
                        if let Some(first) = requests.into_iter().next() {
                            Ok(PuffGraphqlRequest::from(first))
                        } else {
                            Err((StatusCode::BAD_REQUEST, "Empty batch"))
                        }
                    }
                }
            }
            (Method::POST, Some("application/graphql")) => {
                let body = String::from_request(req, &())
                    .await
                    .map_err(|_err| (StatusCode::BAD_REQUEST, "Not valid utf-8"))?;
                Ok(PuffGraphqlRequest::from(body))
            }
            (Method::POST, _) => Err((
                StatusCode::BAD_REQUEST,
                "Header content-type is not application/json or application/graphql",
            )),
            _ => Err((StatusCode::METHOD_NOT_ALLOWED, "Method not supported")),
        }
    }
}

/// Execute a GraphQL request using the async-graphql schema.
///
/// Uses the query cache to skip repeated parse/validate/execute work and the
/// fast serialization path to write JSON bytes directly into the response body
/// (avoiding axum's `Json` wrapper and its double-serialization).
async fn execute_gql_request(
    config: &PuffGraphqlConfig,
    auth_obj: PyObject,
    request: async_graphql::Request,
) -> Response {
    let context = config.new_context(Some(auth_obj));
    let ctx_arc = Arc::new(context);

    let request = request.data(ctx_arc.clone());

    // Use query cache + fast serialization path.
    let schema = config.schema();
    let (bytes, extra_headers) = config.query_cache.execute_to_bytes(&schema, request).await;

    // Close non-shared connections.
    if let Some(conn_mutex) = ctx_arc.connection() {
        let conn = conn_mutex.lock().await;
        if !config.is_shared_connection(&conn) {
            close_conn(&conn).await;
        }
    }

    crate::graphql::fast_serialize::bytes_to_response(bytes, extra_headers)
}

pub fn handle_graphql(
) -> impl FnOnce(HeaderMap, PuffGraphqlRequest) -> futures_util::future::BoxFuture<'static, Response>
       + Clone
       + Send
       + 'static {
    handle_graphql_named("default")
}

pub fn handle_graphql_named<T: Into<Text>>(
    name: T,
) -> impl FnOnce(HeaderMap, PuffGraphqlRequest) -> futures_util::future::BoxFuture<'static, Response>
       + Clone
       + Send
       + 'static {
    let name = name.into();
    move |headers: HeaderMap, PuffGraphqlRequest(request): PuffGraphqlRequest| {
        Box::pin(async move {
            let config = with_puff_context(|ctx| ctx.gql_named(name.as_str()));
            let dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());

            let s: PuffResult<PyObject> = auth_result(headers, &config, dispatcher).await;

            match log_puff_error("GQL handler auth", s) {
                Ok(v) => {
                    let opt_res = auth_result_to_response(&v);

                    if let Some(res) =
                        log_puff_error("Construct GQL Rejection", opt_res).unwrap_or_default()
                    {
                        return res.into_response();
                    }

                    execute_gql_request(&config, v, request).await
                }
                Err(_e) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal Error").into_response(),
            }
        })
    }
}

fn auth_result_to_response(v: &PyObject) -> Result<Option<Response<String>>, Error> {
    Python::with_gil(|py| {
        if v.bind(py).hasattr("is_rejection")? {
            let status = v.getattr(py, "status")?.extract::<u16>(py)?;
            let message = v.getattr(py, "message")?.extract::<String>(py)?;
            let header_v = v.getattr(py, "headers")?;
            let headers = header_v
                .bind(py)
                .downcast::<PyDict>()
                .map_err(|e| anyhow::anyhow!("Expected headers to be a dict: {}", e))?;
            let mut r = axum::http::Response::builder().status(status);
            for (hn, hv) in headers.iter() {
                let header_name: String = hn.extract()?;
                r = r.header(header_name, py_obj_to_bytes(&hv)?)
            }
            Ok(Some(r.body(message)?))
        } else {
            Ok(None)
        }
    })
}

async fn auth_result(
    headers: HeaderMap,
    root_node: &PuffGraphqlConfig,
    dispatcher: PythonDispatcher,
) -> Result<PyObject, Error> {
    if let Some(auth) = root_node.auth.as_ref() {
        let (auth, py_headers) = Python::with_gil(|py| -> PyResult<(PyObject, PyObject)> {
            let auth_clone = auth.clone_ref(py);
            let headers_dict = PyDict::new(py);
            for (hn, hv) in headers.iter() {
                headers_dict.set_item(hn.to_string(), PyBytes::new(py, hv.as_bytes()))?;
            }
            Ok((auth_clone, headers_dict.into_py(py)))
        })?;

        if root_node.auth_async {
            async {
                Ok(dispatcher
                    .dispatch_asyncio(auth, (py_headers,), None)?
                    .await??)
            }
            .await
        } else {
            async { Ok(dispatcher.dispatch1(auth, (py_headers,))?.await??) }.await
        }
    } else {
        Ok(Python::with_gil(|py| py.None()))
    }
}

#[cfg(not(Py_GIL_DISABLED))]
pub fn handle_subscriptions() -> impl FnOnce(
    HeaderMap,
    axum::extract::ws::WebSocketUpgrade,
    (),
) -> futures_util::future::BoxFuture<'static, Response>
       + Clone
       + Send {
    handle_subscriptions_named("default")
}

#[cfg(Py_GIL_DISABLED)]
pub fn handle_subscriptions() -> impl FnOnce(
    HeaderMap,
    axum::extract::ws::WebSocketUpgrade,
    (),
) -> futures_util::future::BoxFuture<'static, Response>
       + Clone
       + Send {
    use axum::extract::ws::Message;
    move |_headers: HeaderMap, ws: axum::extract::ws::WebSocketUpgrade, _| {
        let fut = async {
            ws.on_upgrade(|mut socket| async move {
                let _ = socket.send(Message::Text(
                    r#"{"type":"error","payload":{"message":"GraphQL subscriptions not supported under free-threaded Python"}}"#.into()
                )).await;
                socket.close().await.ok();
            }).into_response()
        };
        fut.boxed()
    }
}

#[cfg(not(Py_GIL_DISABLED))]
pub fn handle_subscriptions_named<N: Into<Text>>(
    name: N,
) -> impl FnOnce(
    HeaderMap,
    axum::extract::ws::WebSocketUpgrade,
    (),
) -> futures_util::future::BoxFuture<'static, Response>
       + Clone
       + Send {
    let name = name.into();
    move |headers: HeaderMap, ws: axum::extract::ws::WebSocketUpgrade, _| {
        let config = with_puff_context(|ctx| ctx.gql_named(name.as_str()));
        let dispatcher = with_puff_context(|ctx| ctx.python_dispatcher());
        let fut = async move {
            let s: PuffResult<PyObject> = auth_result(headers, &config, dispatcher).await;

            match log_puff_error("GQL subscription auth", s) {
                Ok(v) => {
                    let opt_res = auth_result_to_response(&v);

                    if let Some(res) =
                        log_puff_error("Construct GQL Rejection", opt_res).unwrap_or_default()
                    {
                        return res.into_response();
                    }

                    let context = config.new_context(Some(v));
                    let ctx_arc = Arc::new(context);
                    let schema = config.schema();

                    ws.protocols(["graphql-transport-ws", "graphql-ws"])
                        .on_upgrade(move |socket| {
                            use axum::extract::ws::Message;
                            use futures_util::{future, SinkExt, StreamExt};

                            async move {
                                let (mut ws_sink, ws_stream) = socket.split();

                                let input = ws_stream
                                    .take_while(|res| future::ready(res.is_ok()))
                                    .map(Result::unwrap)
                                    .filter_map(|msg| {
                                        if let Message::Text(_) | Message::Binary(_) = msg {
                                            future::ready(Some(msg))
                                        } else {
                                            future::ready(None)
                                        }
                                    })
                                    .map(Message::into_data);

                                let protocol = async_graphql::http::WebSocketProtocols::GraphQLWS;
                                let mut data = async_graphql::Data::default();
                                data.insert(ctx_arc);

                                let mut gql_stream =
                                    std::pin::pin!(async_graphql::http::WebSocket::new(
                                        schema, input, protocol
                                    )
                                    .connection_data(data)
                                    .map(|msg| match msg {
                                        async_graphql::http::WsMessage::Text(text) =>
                                            Message::Text(text.into()),
                                        async_graphql::http::WsMessage::Close(code, status) => {
                                            Message::Close(Some(axum::extract::ws::CloseFrame {
                                                code,
                                                reason: status.into(),
                                            }))
                                        }
                                    }));

                                while let Some(msg) = gql_stream.next().await {
                                    if ws_sink.send(msg).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        })
                }
                Err(_e) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal Error").into_response(),
            }
        };
        fut.boxed()
    }
}

#[cfg(Py_GIL_DISABLED)]
pub fn handle_subscriptions_named<N: Into<Text>>(
    _name: N,
) -> impl FnOnce(
    HeaderMap,
    axum::extract::ws::WebSocketUpgrade,
    (),
) -> futures_util::future::BoxFuture<'static, Response>
       + Clone
       + Send {
    use axum::extract::ws::Message;
    move |_headers: HeaderMap, ws: axum::extract::ws::WebSocketUpgrade, _| {
        let fut = async {
            ws.on_upgrade(|mut socket| async move {
                let _ = socket.send(Message::Text(
                    r#"{"type":"error","payload":{"message":"GraphQL subscriptions not supported under free-threaded Python"}}"#.into()
                )).await;
                socket.close().await.ok();
            }).into_response()
        };
        fut.boxed()
    }
}

pub fn playground<U: Into<Text>, S: Into<Text>>(
    graphql_endpoint_url: U,
    subscriptions_endpoint_url: Option<S>,
) -> impl FnOnce() -> future::Ready<Response> + Clone + Send {
    let gurl = graphql_endpoint_url.into();
    let surl = subscriptions_endpoint_url.map(|f| f.into());

    let sub_url = surl.as_deref().unwrap_or("");
    let html_content = format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset=utf-8/>
  <meta name="viewport" content="user-scalable=no, initial-scale=1.0, minimum-scale=1.0, maximum-scale=1.0, minimal-ui">
  <title>GraphQL Playground</title>
  <link rel="stylesheet" href="//cdn.jsdelivr.net/npm/graphql-playground-react/build/static/css/index.css"/>
  <link rel="shortcut icon" href="//cdn.jsdelivr.net/npm/graphql-playground-react/build/favicon.png"/>
  <script src="//cdn.jsdelivr.net/npm/graphql-playground-react/build/static/js/middleware.js"></script>
</head>
<body>
<div id="root"></div>
<script>window.addEventListener('load', function (event) {{
    GraphQLPlayground.init(document.getElementById('root'), {{
      endpoint: '{gql_url}',
      subscriptionEndpoint: '{sub_url}',
    }})
  }})</script>
</body>
</html>"#,
        gql_url = gurl.as_str(),
        sub_url = sub_url
    );

    let html = Html(html_content);
    || future::ready(html.into_response())
}
