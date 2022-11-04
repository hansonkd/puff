use dotenvy::{dotenv, from_path};
use puff_rs::prelude::*;
use puff_rs::program::commands::{
    ASGIServerCommand, BasicCommand, DjangoManagementCommand, PytestCommand, PythonCommand,
    WSGIServerCommand, WaitForever,
};
use puff_rs::graphql::handlers::{handle_graphql, handle_subscriptions};
use serde::{Deserialize, Serialize};
use std::fs;

#[derive(Serialize, Deserialize)]
struct Config {
    django: Option<bool>,
    postgres: Option<bool>,
    redis: Option<bool>,
    pubsub: Option<bool>,
    task_queue: Option<bool>,
    greenlets: Option<bool>,
    asyncio: Option<bool>,
    dotenv: Option<bool>,
    dotenv_path: Option<String>,
    pytest_path: Option<String>,
    wsgi: Option<String>,
    asgi: Option<String>,
    graphql: Option<String>,
    graphql_url: Option<String>,
    graphql_subscription_url: Option<String>,
    commands: Option<Vec<PyCommand>>,
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
        dotenv_path: Some(".env".to_owned()),
        pytest_path: Some("./".to_owned()),
        wsgi: Some("my_wsgi.app".to_owned()),
        asgi: Some("my_asgi.app".to_owned()),
        graphql: Some("my_graphql.Schema".to_owned()),
        graphql_url: Some("/graphql/".to_owned()),
        graphql_subscription_url: Some("/subscriptions/".to_owned()),
        commands: Some(vec![PyCommand {
            function: "my_python_mod.some_func".to_owned(),
            command_name: "execute_func".to_owned(),
        }]),
    };

    toml::to_string_pretty(&example_config).unwrap()
}

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let puff_config_path = std::env::var("PUFF_CONFG").unwrap_or("puff.toml".to_owned());

    let contents = fs::read_to_string(&puff_config_path).expect(&format!(
        "Could not read Puff TOML config file {}",
        &puff_config_path
    ));

    let config: Config = toml::from_str(&contents).expect(&format!(
        "Could not parse Puff TOML config file {}",
        &puff_config_path
    ));

    if config.dotenv.unwrap_or(true) {
        if let Some(p) = config.dotenv_path {
            from_path(&p).unwrap();
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

    if let Some(schema) = config.graphql {
        rc = rc.set_gql_schema_class(schema);
    }

    let mut program = Program::new("puff")
        .about("Puff CLI. Reads puff.toml or configuration file specified with PUFF_CONFG")
        .version(VERSION)
        .after_help("☁ Thanks for using Puff ☁")
        .runtime_config(rc)
        .command(BasicCommand::new_with_options(
            clap::Command::new("example_config").about("Display an example puff.toml"),
            async {
                println!("{}", help_text());
                Ok(ExitCode::SUCCESS)
            },
        ))
        .command(WaitForever::new());

    if let Some(wsgi_app) = config.wsgi {
        if config.graphql_url.as_ref().is_some() || config.graphql_subscription_url.as_ref().is_some() {
            let mut router = Router::new();
            if let Some(asgi_app) = &config.graphql_url {
                router = router.post(asgi_app, handle_graphql())
            }

            if let Some(asgi_app) = &config.graphql_subscription_url {
                router = router.get(asgi_app, handle_subscriptions())
            }

            program = program.command(WSGIServerCommand::new_with_router(wsgi_app, router))
        } else {
            program = program.command(WSGIServerCommand::new(wsgi_app))
        }
    }

    if let Some(asgi_app) = config.asgi {
        if config.graphql_url.as_ref().is_some() || config.graphql_subscription_url.as_ref().is_some() {
            let mut router = Router::new();
            if let Some(ref url) = &config.graphql_url {
                router = router.post(url, handle_graphql())
            }

            if let Some(ref url) = &config.graphql_subscription_url {
                router = router.get(url, handle_subscriptions())
            }

            program = program.command(ASGIServerCommand::new_with_router(asgi_app, router))
        } else {
            program = program.command(ASGIServerCommand::new(asgi_app))
        }

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
