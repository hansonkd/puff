use axum::http;
use axum::http::HeaderValue;
use dotenvy::{dotenv, from_path};
use hyper::Method;
use puff_rs::graphql::handlers::{handle_graphql, handle_subscriptions, playground};
use puff_rs::prelude::*;
use puff_rs::program::commands::{
    ASGIServerCommand, BasicCommand, DjangoManagementCommand, PytestCommand, PythonCommand,
    ServerCommand, WSGIServerCommand, WaitForever,
};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use std::{fmt, fs};
use tower::ServiceBuilder;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    django: Option<bool>,
    postgres: Option<bool>,
    redis: Option<bool>,
    pubsub: Option<bool>,
    task_queue: Option<bool>,
    greenlets: Option<bool>,
    asyncio: Option<bool>,
    dotenv: Option<bool>,
    add_cwd_to_path: Option<bool>,
    compression_middleware: Option<bool>,
    dotenv_path: Option<String>,
    pytest_path: Option<String>,
    wsgi: Option<String>,
    asgi: Option<String>,
    graphql_schema: Option<String>,
    graphql_url: Option<String>,
    graphql_subscription_url: Option<String>,
    graphql_playground_url: Option<String>,
    commands: Option<Vec<PyCommand>>,
    cors: Option<Cors>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
enum CorsBaseConfig {
    Default,
    Permissive,
    VeryPermissive,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cors {
    /// A Base Config to use, Default, Permissive, or VeryPermissive
    base_config: Option<CorsBaseConfig>,
    /// If none provided, CORS responses won't be cached
    max_age_secs: Option<u64>,
    /// Allow Credentials
    allow_credentials: Option<bool>,
    /// If list is empty provided, echos request origin
    allowed_origins: Option<AnyOrCorsOrigins>,
    /// If the list is empty, and all preflight with a request header will be rejected.
    allowed_headers: Option<AnyOrHeaderNames>,
    /// If list is empty and all preflight requests will be rejected
    allowed_methods: Option<AnyOrHttpMethods>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnyOrCorsOrigins {
    Any(MatchAny),
    Vec(Vec<CorsOrigin>),
}

impl Into<AllowOrigin> for AnyOrCorsOrigins {
    fn into(self) -> AllowOrigin {
        match self {
            AnyOrCorsOrigins::Any(_) => AllowOrigin::any(),
            AnyOrCorsOrigins::Vec(v) => AllowOrigin::list(v.into_iter().map(|c| c.0)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnyOrHeaderNames {
    Any(MatchAny),
    Vec(Vec<HeaderName>),
}

impl Into<AllowHeaders> for AnyOrHeaderNames {
    fn into(self) -> AllowHeaders {
        match self {
            AnyOrHeaderNames::Any(_) => AllowHeaders::any(),
            AnyOrHeaderNames::Vec(v) => AllowHeaders::list(v.into_iter().map(|c| c.0)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum AnyOrHttpMethods {
    Any(MatchAny),
    Vec(Vec<HttpMethod>),
}

impl Into<AllowMethods> for AnyOrHttpMethods {
    fn into(self) -> AllowMethods {
        match self {
            AnyOrHttpMethods::Any(_) => AllowMethods::any(),
            AnyOrHttpMethods::Vec(v) => AllowMethods::list(v.into_iter().map(|c| c.0)),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchAny;

impl Serialize for MatchAny {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str("*")
    }
}

impl<'de> Deserialize<'de> for MatchAny {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MatchAnyVisitor;
        impl<'de> Visitor<'de> for MatchAnyVisitor {
            type Value = MatchAny;

            fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
                write!(fmt, "*",)
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                use serde::de::Unexpected;

                if v == "*" {
                    return Ok(MatchAny);
                }

                return Err(E::invalid_value(Unexpected::Str(v), &self));
            }
        }
        deserializer.deserialize_str(MatchAnyVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorsOrigin(HeaderValue);

impl Serialize for CorsOrigin {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.to_str().expect("Invalid HeaderValue"))
    }
}

impl<'de> Deserialize<'de> for CorsOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CorsOriginVisitor;
        impl<'de> Visitor<'de> for CorsOriginVisitor {
            type Value = CorsOrigin;

            fn expecting(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
                write!(fmt, "an origin in format http[s]://example.com[:3000]",)
            }

            fn visit_str<E>(self, v: &str) -> std::result::Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                use serde::de::Unexpected;

                let uri = http::uri::Uri::from_str(v).map_err(E::custom)?;
                if let Some(s) = uri.scheme_str() {
                    if s != "http" && s != "https" {
                        return Err(E::invalid_value(Unexpected::Str(v), &self));
                    }
                } else {
                    return Err(E::invalid_value(Unexpected::Str(v), &self));
                }
                if let Some(p) = uri.path_and_query() {
                    if p.as_str() != "/" {
                        return Err(E::invalid_value(Unexpected::Str(v), &self));
                    }
                }
                Ok(CorsOrigin(
                    HeaderValue::from_str(v.trim_end_matches('/'))
                        .map_err(|_| E::invalid_value(Unexpected::Str(v), &self))?,
                ))
            }
        }
        deserializer.deserialize_str(CorsOriginVisitor)
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct HeaderName(pub http::header::HeaderName);

impl<'de> Deserialize<'de> for HeaderName {
    fn deserialize<D>(deserializer: D) -> Result<HeaderName, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct HeaderNameVisitor;

        impl<'de> serde::de::Visitor<'de> for HeaderNameVisitor {
            type Value = HeaderName;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a sequence of valid http header names")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                http::header::HeaderName::from_str(value)
                    .map(HeaderName)
                    .map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_str(HeaderNameVisitor)
    }
}

impl Serialize for HeaderName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.as_ref())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpMethod(pub http::method::Method);

impl<'de> Deserialize<'de> for HttpMethod {
    fn deserialize<D>(deserializer: D) -> Result<HttpMethod, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = HttpMethod;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a http method(verb)")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                http::method::Method::from_str(value)
                    .map(HttpMethod)
                    .map_err(serde::de::Error::custom)
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

impl Serialize for HttpMethod {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.0.as_ref())
    }
}

#[derive(Serialize, Deserialize)]
struct PyCommand {
    function: String,
    command_name: String,
}

fn help_text() -> String {
    let example_config = Config {
        django: Some(false),
        redis: Some(false),
        postgres: Some(false),
        pubsub: Some(false),
        task_queue: Some(false),
        greenlets: Some(true),
        asyncio: Some(false),
        dotenv: Some(false),
        add_cwd_to_path: Some(true),
        compression_middleware: Some(true),
        dotenv_path: Some(".env".to_owned()),
        pytest_path: Some("./".to_owned()),
        wsgi: Some("my_wsgi.app".to_owned()),
        asgi: Some("my_asgi.app".to_owned()),
        graphql_schema: Some("my_graphql.Schema".to_owned()),
        graphql_url: Some("/graphql/".to_owned()),
        graphql_subscription_url: Some("/subscriptions/".to_owned()),
        graphql_playground_url: Some("/playground/".to_owned()),
        commands: Some(vec![PyCommand {
            function: "my_python_mod.some_func".to_owned(),
            command_name: "execute_func".to_owned(),
        }]),
        cors: Some(Cors {
            base_config: Some(CorsBaseConfig::Default),
            allowed_headers: Some(AnyOrHeaderNames::Any(MatchAny)),
            allowed_methods: Some(AnyOrHttpMethods::Vec(vec![HttpMethod(Method::GET)])),
            allowed_origins: Some(AnyOrCorsOrigins::Vec(vec![CorsOrigin(
                HeaderValue::from_str("http://localhost:7777").unwrap(),
            )])),
            allow_credentials: Some(true),
            max_age_secs: Some(60 * 60 * 24),
        }),
    };

    toml::to_string_pretty(&example_config).unwrap()
}

fn make_cors_layer(cm: Cors) -> CorsLayer {
    let mut cl = match cm.base_config.unwrap_or(CorsBaseConfig::Default) {
        CorsBaseConfig::Default => CorsLayer::new(),
        CorsBaseConfig::Permissive => CorsLayer::permissive(),
        CorsBaseConfig::VeryPermissive => CorsLayer::very_permissive(),
    };

    if let Some(ac) = cm.allow_credentials {
        cl = cl.allow_credentials(ac);
    }

    if let Some(am) = cm.allowed_methods {
        cl = cl.allow_methods(am);
    }

    if let Some(am) = cm.allowed_origins {
        cl = cl.allow_origin(am);
    }

    if let Some(ah) = cm.allowed_headers {
        cl = cl.allow_headers(ah);
    }

    if let Some(max_age_secs) = cm.max_age_secs {
        cl = cl.max_age(Duration::from_secs(max_age_secs));
    }

    cl
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    tracing_subscriber::fmt::init();

    let puff_config_path = std::env::var("PUFF_CONFIG").unwrap_or("puff.toml".to_owned());

    let config: Config = if let Ok(contents) = fs::read_to_string(&puff_config_path) {
        let c = toml::from_str(&contents).expect(&format!(
            "Could not parse Puff TOML config file {}",
            &puff_config_path
        ));
        info!("Loaded {}.", &puff_config_path);
        c
    } else {
        info!(
            "Could not read Puff TOML config file {}, using default config.",
            &puff_config_path
        );
        toml::from_str("").expect("Couldn't parse default.")
    };

    if config.dotenv.unwrap_or(true) {
        if let Some(p) = config.dotenv_path.as_ref() {
            from_path(p).unwrap();
            info!("Loaded dotenv {}", &p);
        } else {
            if let Ok(p) = dotenv() {
                info!("Loaded dotenv {}", p.to_string_lossy());
            }
        }
    }

    let mut rc = RuntimeConfig::default()
        .set_greenlets(config.greenlets.unwrap_or(true))
        .set_postgres(config.postgres.unwrap_or(false))
        .set_redis(config.redis.unwrap_or(false))
        .set_task_queue(config.task_queue.unwrap_or(false))
        .set_asyncio(config.asyncio.unwrap_or(false))
        .set_pubsub(config.pubsub.unwrap_or(false));

    if let Some(schema) = config.graphql_schema.as_ref() {
        rc = rc.set_gql_schema(schema);
    }

    if config.add_cwd_to_path.unwrap_or(true) {
        rc = rc.add_cwd_to_python_path();
    }

    let mut program = Program::new("puff")
        .about("Puff CLI. Reads puff.toml or configuration file specified with PUFF_CONFG")
        .version(VERSION)
        .after_help("☁ Thanks for using Puff ☁")
        .runtime_config(rc)
        .command(BasicCommand::new_with_options(
            clap::Command::new("example_config").about("Display an example puff.toml"),
            |_opts| async {
                println!("{}", help_text());
                Ok(ExitCode::SUCCESS)
            },
        ))
        .command(WaitForever::new());

    if let Some(wsgi_app) = config.wsgi.as_ref() {
        let router = build_service_layer(&config);
        program = program.command(WSGIServerCommand::new_with_router(wsgi_app, router))
    } else if let Some(asgi_app) = config.asgi.as_ref() {
        let router = build_service_layer(&config);
        program = program.command(ASGIServerCommand::new_with_router(asgi_app, router))
    } else if config.graphql_url.as_ref().is_some()
        || config.graphql_subscription_url.as_ref().is_some()
    {
        let router = build_service_layer(&config);
        program = program.command(ServerCommand::new(router))
    }

    if let Some(pytest_path) = config.pytest_path {
        program = program.command(PytestCommand::new(pytest_path))
    }

    if config.django.unwrap_or(false) {
        program = program.command(DjangoManagementCommand::new())
    }

    if let Some(commands) = config.commands {
        for command in commands {
            program = program.command(PythonCommand::new(command.command_name, command.function))
        }
    }

    program.run()
}

fn build_service_layer(config: &Config) -> Router {
    let mut router = Router::new();
    if let Some(url) = &config.graphql_url {
        router = router.post(url, handle_graphql())
    }

    if let Some(url) = &config.graphql_subscription_url {
        router = router.get(url, handle_subscriptions())
    }

    if let Some(url) = &config.graphql_playground_url {
        let gql_url = config
            .graphql_url
            .as_ref()
            .expect("can only use playground with graphql_url");
        router = router.get(
            url,
            playground(gql_url.to_owned(), config.graphql_subscription_url.clone()),
        )
    }

    if config.compression_middleware.clone().unwrap_or(false) {
        if let Some(cm) = config.cors.clone() {
            let cl = make_cors_layer(cm);
            router.layer(
                ServiceBuilder::new()
                    .layer(CompressionLayer::new())
                    .layer(cl),
            )
        } else {
            router.layer(CompressionLayer::new())
        }
    } else if let Some(cm) = config.cors.clone() {
        let cl = make_cors_layer(cm);
        router.layer(cl)
    } else {
        router
    }
}
