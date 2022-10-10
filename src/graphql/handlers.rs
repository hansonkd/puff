use axum::extract::{FromRequest, Query, WebSocketUpgrade};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::Json;
use juniper::{
    BoxFuture, GraphQLSubscriptionType, GraphQLType, GraphQLTypeAsync, GraphQLValue, InputValue,
    LocalBoxFuture, RootNode, ScalarValue,
};
use std::future;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::graphql::scalar::AggroScalarValue;
use juniper::http::{GraphQLBatchRequest, GraphQLBatchResponse, GraphQLRequest};

use crate::context::with_puff_context;
use crate::graphql::AggroContext;
use async_trait::async_trait;
use axum::extract::ws::{Message, WebSocket};
use futures_util::FutureExt;
use hyper::Body;
use juniper::futures::{SinkExt, StreamExt, TryStreamExt};
use juniper_graphql_ws::{ClientMessage, Connection, ConnectionConfig, Schema, WebsocketError};
use serde;
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
    /// Returns true if the request body is an empty array
    fn is_empty_batch(&self) -> bool {
        match self {
            JsonRequestBody::Batch(r) => r.is_empty(),
            JsonRequestBody::Single(_) => false,
        }
    }
}

/// An extractor for Axum to Extract a JuniperRequest
///
/// # Example
///
/// ```rust
/// use std::sync::Arc;
///
/// use axum::{
///     body::Body,
///     Json,
///     routing::post,
///     Router,
///     Extension,
/// };
/// use juniper::{
///     http::GraphQLBatchResponse,
///     RootNode, EmptySubscription, EmptyMutation, graphql_object,
/// };
/// use juniper_axum::{extract::JuniperRequest, response::JuniperResponse};
///
/// #[derive(Clone, Copy, Debug)]
/// pub struct Context;
///
/// impl juniper::Context for Context {}
///
/// #[derive(Clone, Copy, Debug)]
/// pub struct Query;
///
/// #[graphql_object(context = Context)]
/// impl Query {
///     fn add(a: i32, b: i32) -> i32 {
///         a + b
///     }
/// }
///
/// type Schema = RootNode<'static, Query, EmptyMutation<Context>, EmptySubscription<Context>>;
///
/// let schema = Schema::new(
///    Query,
///    EmptyMutation::<Context>::new(),
///    EmptySubscription::<Context>::new()
/// );
///
/// let context = Context;
///
/// let app: Router = Router::new()
///     .route("/graphql", post(graphql))
///     .layer(Extension(schema))
///     .layer(Extension(context));
///
/// async fn graphql(
///     JuniperRequest(request): JuniperRequest,
///     Extension(schema): Extension<Schema>,
///     Extension(context): Extension<Context>
/// ) -> JuniperResponse {
///     JuniperResponse(request.execute(&schema, &context).await)
/// }

#[derive(Debug, PartialEq)]
pub struct JuniperPuffRequest(pub GraphQLBatchRequest<AggroScalarValue>);

impl TryFrom<SingleRequestBody> for JuniperPuffRequest {
    type Error = serde_json::Error;

    fn try_from(value: SingleRequestBody) -> Result<JuniperPuffRequest, Self::Error> {
        Ok(JuniperPuffRequest(GraphQLBatchRequest::Single(
            GraphQLRequest::try_from(value)?,
        )))
    }
}

impl TryFrom<SingleRequestBody> for GraphQLRequest<AggroScalarValue> {
    type Error = serde_json::Error;

    fn try_from(value: SingleRequestBody) -> Result<GraphQLRequest<AggroScalarValue>, Self::Error> {
        // Convert Map<String, Value> to InputValue with the help of serde_json
        let variables: Option<InputValue<AggroScalarValue>> = value
            .variables
            .map(|vars| serde_json::to_string(&vars))
            .transpose()?
            .map(|s| serde_json::from_str(&s))
            .transpose()?;

        Ok(GraphQLRequest::new(
            value.query,
            value.operation_name,
            variables,
        ))
    }
}

impl TryFrom<JsonRequestBody> for JuniperPuffRequest {
    type Error = serde_json::Error;

    fn try_from(value: JsonRequestBody) -> Result<JuniperPuffRequest, Self::Error> {
        match value {
            JsonRequestBody::Single(r) => JuniperPuffRequest::try_from(r),
            JsonRequestBody::Batch(requests) => {
                let mut graphql_requests: Vec<GraphQLRequest<AggroScalarValue>> = Vec::new();

                for request in requests {
                    let gq = GraphQLRequest::<AggroScalarValue>::try_from(request)?;
                    graphql_requests.push(gq);
                }

                Ok(JuniperPuffRequest(GraphQLBatchRequest::Batch(
                    graphql_requests,
                )))
            }
        }
    }
}

impl From<String> for JuniperPuffRequest {
    fn from(query: String) -> Self {
        JuniperPuffRequest(GraphQLBatchRequest::Single(GraphQLRequest::new(
            query, None, None,
        )))
    }
}

impl TryFrom<GetQueryVariables> for JuniperPuffRequest {
    type Error = serde_json::Error;

    fn try_from(value: GetQueryVariables) -> Result<JuniperPuffRequest, Self::Error> {
        let variables: Option<InputValue<AggroScalarValue>> = value
            .variables
            .map(|var| serde_json::from_str(&var))
            .transpose()?;

        Ok(JuniperPuffRequest(GraphQLBatchRequest::Single(
            GraphQLRequest::new(value.query, value.operation_name, variables),
        )))
    }
}

/// Helper trait to get some nice clean code
#[async_trait]
trait TryFromRequest {
    type Rejection;

    /// Get `content-type` header from request
    fn try_get_content_type_header(&self) -> Result<Option<&str>, Self::Rejection>;

    /// Try to convert GET request to RequestBody
    async fn try_from_get_request(self) -> Result<JuniperPuffRequest, Self::Rejection>;

    /// Try to convert POST json request to RequestBody
    async fn try_from_json_post_request(self) -> Result<JuniperPuffRequest, Self::Rejection>;

    /// Try to convert POST graphql request to RequestBody
    async fn try_from_graphql_post_request(self) -> Result<JuniperPuffRequest, Self::Rejection>;
}

#[async_trait]
impl TryFromRequest for Request<Body> {
    type Rejection = (StatusCode, &'static str);

    fn try_get_content_type_header(&self) -> Result<Option<&str>, Self::Rejection> {
        self.headers()
            .get("content-Type")
            .map(|header| header.to_str())
            .transpose()
            .map_err(|_e| {
                (
                    StatusCode::BAD_REQUEST,
                    "content-type header not a valid string",
                )
            })
    }

    async fn try_from_get_request(self) -> Result<JuniperPuffRequest, Self::Rejection> {
        let query_vars = Query::<GetQueryVariables>::from_request(self, &())
            .await
            .map(|result| result.0)
            .map_err(|_err| (StatusCode::BAD_REQUEST, "Request not valid"))?;

        JuniperPuffRequest::try_from(query_vars)
            .map_err(|_err| (StatusCode::BAD_REQUEST, "Could not convert variables"))
    }

    async fn try_from_json_post_request(self) -> Result<JuniperPuffRequest, Self::Rejection> {
        let json_body = Json::<JsonRequestBody>::from_request(self, &())
            .await
            .map_err(|_err| (StatusCode::BAD_REQUEST, "JSON invalid"))
            .map(|result| result.0)?;

        if json_body.is_empty_batch() {
            return Err((StatusCode::BAD_REQUEST, "Batch request can not be empty"));
        }

        JuniperPuffRequest::try_from(json_body)
            .map_err(|_err| (StatusCode::BAD_REQUEST, "Could not convert variables"))
    }

    async fn try_from_graphql_post_request(self) -> Result<JuniperPuffRequest, Self::Rejection> {
        String::from_request(self, &())
            .await
            .map(|s| s.into())
            .map_err(|_err| (StatusCode::BAD_REQUEST, "Not valid utf-8"))
    }
}

#[async_trait]
impl<S: Send + Sync> FromRequest<S, Body> for JuniperPuffRequest {
    type Rejection = (StatusCode, &'static str);

    async fn from_request(req: Request<Body>, state: &S) -> Result<Self, Self::Rejection> {
        let content_type = req.try_get_content_type_header()?;

        // Convert `req` to JuniperRequest based on request method and content-type header
        match (req.method(), content_type) {
            (&Method::GET, _) => req.try_from_get_request().await,
            (&Method::POST, Some("application/json")) => req.try_from_json_post_request().await,
            (&Method::POST, Some("application/graphql")) => {
                req.try_from_graphql_post_request().await
            }
            (&Method::POST, _) => Err((
                StatusCode::BAD_REQUEST,
                "Header content-type is not application/json or application/graphql",
            )),
            _ => Err((StatusCode::METHOD_NOT_ALLOWED, "Method not supported")),
        }
    }
}

/// A wrapper around [`GraphQLBatchResponse`] that implements [`IntoResponse`]
/// so it can be returned from axum handlers.
pub struct JuniperPuffResponse(pub GraphQLBatchResponse<AggroScalarValue>);

impl IntoResponse for JuniperPuffResponse {
    fn into_response(self) -> Response {
        if !self.0.is_ok() {
            return (StatusCode::BAD_REQUEST, Json(self.0)).into_response();
        }

        Json(self.0).into_response()
    }
}

#[derive(Debug)]
struct AxumMessage(Message);

#[derive(Debug)]
enum SubscriptionError {
    Juniper(WebsocketError),
    Axum(axum::Error),
    Serde(serde_json::Error),
}

impl<S: ScalarValue> TryFrom<AxumMessage> for ClientMessage<S> {
    type Error = serde_json::Error;

    fn try_from(msg: AxumMessage) -> serde_json::Result<Self> {
        serde_json::from_slice(&msg.0.into_data())
    }
}

/// Redirect the axum [`Websocket`] to a juniper [`Connection`] and vice versa.
///
/// # Example
///
/// ```rust
/// use std::{pin::Pin, time::Duration};
///
/// use axum::{
///     extract::WebSocketUpgrade,
///     body::Body,
///     response::Response,
///     routing::get,
///     Extension, Router
/// };
/// use futures::{Stream, StreamExt as _};
/// use juniper::{
///     graphql_object, graphql_subscription, EmptyMutation, FieldError,
///     RootNode,
/// };
/// use juniper_axum::{playground, subscriptions::handle_graphql_socket};
/// use tokio::time::interval;
/// use tokio_stream::wrappers::IntervalStream;
///
/// type Schema = RootNode<'static, Query, EmptyMutation, Subscription>;
///
/// #[derive(Clone, Copy, Debug)]
/// pub struct Query;
///
/// #[graphql_object]
/// impl Query {
///     /// Add two numbers a and b
///     fn add(a: i32, b: i32) -> i32 {
///         a + b
///     }
/// }
///
/// #[derive(Clone, Copy, Debug)]
/// pub struct Subscription;
///
/// type NumberStream = Pin<Box<dyn Stream<Item = Result<i32, FieldError>> + Send>>;
///
/// #[graphql_subscription]
/// impl Subscription {
///     /// Count seconds
///     async fn count() -> NumberStream {
///         let mut value = 0;
///         let stream = IntervalStream::new(interval(Duration::from_secs(1))).map(move |_| {
///             value += 1;
///             Ok(value)
///         });
///         Box::pin(stream)
///     }
/// }
///
/// async fn juniper_subscriptions(
///     Extension(schema): Extension<Schema>,
///     ws: WebSocketUpgrade,
/// ) -> Response {
///     ws.protocols(["graphql-ws"])
///         .max_frame_size(1024)
///         .max_message_size(1024)
///         .max_send_queue(100)
///         .on_upgrade(move |socket| handle_graphql_socket(socket, schema, ()))
/// }
///
/// let schema = Schema::new(Query, EmptyMutation::new(), Subscription);
///
/// let app: Router = Router::new()
///     .route("/subscriptions", get(juniper_subscriptions))
///     .layer(Extension(schema));
/// ```
pub async fn handle_graphql_socket<S: Schema>(socket: WebSocket, schema: S, context: S::Context) {
    let config = ConnectionConfig::new(context);
    let (ws_tx, ws_rx) = socket.split();
    let (juniper_tx, juniper_rx) = Connection::new(schema, config).split();

    // In the following section we make the streams and sinks from
    // Axum and Juniper compatible with each other. This makes it
    // possible to forward an incoming message from Axum to Juniper
    // and vice versa.
    let juniper_tx = juniper_tx.sink_map_err(SubscriptionError::Juniper);

    let send_websocket_message_to_juniper = ws_rx
        .map_err(SubscriptionError::Axum)
        .map(|result| result.map(AxumMessage))
        .forward(juniper_tx);

    let ws_tx = ws_tx.sink_map_err(SubscriptionError::Axum);

    let send_juniper_message_to_axum = juniper_rx
        .map(|msg| serde_json::to_string(&msg).map(Message::Text))
        .map_err(SubscriptionError::Serde)
        .forward(ws_tx);

    // Start listening for messages from axum, and redirect them to juniper
    let _result = futures::future::select(
        send_websocket_message_to_juniper,
        send_juniper_message_to_axum,
    )
    .await;
}

pub fn graphql_subscriptions<S: Schema>(
    schema: S,
    context: S::Context,
) -> impl FnOnce(WebSocketUpgrade, ()) -> future::Ready<Response> + Clone + Send
where
    <S as Schema>::Context: Clone,
{
    move |ws: WebSocketUpgrade, _| {
        let s = ws
            .protocols(["graphql-ws"])
            .max_frame_size(1024)
            .max_message_size(1024)
            .max_send_queue(100)
            .on_upgrade(move |socket| handle_graphql_socket(socket, schema, context));
        future::ready(s)
    }
}

pub fn graphql_execute<QueryT, MutationT, SubscriptionT, Ctx>(
    root_node: Arc<RootNode<'static, QueryT, MutationT, SubscriptionT, AggroScalarValue>>,
    context: Ctx,
) -> impl FnOnce(JuniperPuffRequest) -> BoxFuture<'static, JuniperPuffResponse> + Clone + Send + 'static
where
    Ctx: Send + Sync + Clone + 'static,
    QueryT: GraphQLTypeAsync<AggroScalarValue, Context = Ctx> + Send + 'static,
    QueryT::TypeInfo: Send + Sync + 'static,
    MutationT: GraphQLTypeAsync<AggroScalarValue, Context = Ctx> + Send + 'static,
    MutationT::TypeInfo: Send + Sync + 'static,
    SubscriptionT: GraphQLSubscriptionType<AggroScalarValue, Context = Ctx> + Send + 'static,
    SubscriptionT::TypeInfo: Send + Sync + 'static,
{
    let root_node = root_node.clone();
    let new_ctx = context.clone();
    move |JuniperPuffRequest(request): JuniperPuffRequest| {
        Box::pin(async move {
            let root_node = root_node.clone();
            let new_ctx = new_ctx.clone();
            JuniperPuffResponse(request.execute(&root_node, &new_ctx).await)
        })
    }
}

pub fn handle_graphql(
) -> impl FnOnce(JuniperPuffRequest) -> BoxFuture<'static, JuniperPuffResponse> + Clone + Send + 'static
{
    move |JuniperPuffRequest(request): JuniperPuffRequest| {
        Box::pin(async move {
            let root_node = with_puff_context(|ctx| ctx.gql());
            let new_ctx = AggroContext::new();
            JuniperPuffResponse(request.execute(&root_node, &new_ctx).await)
        })
    }
}

pub fn handle_subscriptions(
) -> impl FnOnce(WebSocketUpgrade, ()) -> future::Ready<Response> + Clone + Send {
    move |ws: WebSocketUpgrade, _| {
        let root_node = with_puff_context(|ctx| ctx.gql());
        let new_ctx = AggroContext::new();

        let s = ws
            .protocols(["graphql-ws"])
            .max_frame_size(1024)
            .max_message_size(1024)
            .max_send_queue(100)
            .on_upgrade(move |socket| handle_graphql_socket(socket, root_node, new_ctx));
        future::ready(s)
    }
}

pub fn playground<'a>(
    graphql_endpoint_url: &str,
    subscriptions_endpoint_url: impl Into<Option<&'a str>>,
) -> impl FnOnce() -> future::Ready<Response> + Clone + Send {
    let html = Html(juniper::http::playground::playground_source(
        graphql_endpoint_url,
        subscriptions_endpoint_url.into(),
    ));

    || future::ready(html.into_response())
}
