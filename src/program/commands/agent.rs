use crate::agents::agent::{Agent, AgentConfig};
use crate::agents::llm::{LlmClient, LlmConfig};
use crate::agents::server::{agent_router, AgentServerState};
use crate::agents::skill::Skill;
use crate::agents::tool::ToolRegistry;
use crate::context::PuffContext;
use crate::errors::Result;
use crate::program::{Runnable, RunnableCommand};
use clap::{ArgMatches, Command};
use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Serve agents via HTTP API.
pub struct AgentServeCommand {
    pub agent_configs: Vec<AgentConfig>,
    pub llm_config: LlmConfig,
    pub port: u16,
}

impl RunnableCommand for AgentServeCommand {
    fn cli_parser(&self) -> Command {
        Command::new("agent")
            .about("Serve agents via HTTP API")
            .arg(clap::arg!(--port <PORT> "Port to listen on"))
    }

    fn make_runnable(&mut self, args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let port: u16 = args
            .get_one::<String>("port")
            .and_then(|p| p.parse().ok())
            .unwrap_or(self.port);

        let llm_client = LlmClient::new(self.llm_config.clone())?;

        let mut agents = HashMap::new();
        for config in &self.agent_configs {
            let mut agent = Agent::new(config.clone());
            for skill_path in &config.skills {
                let path = std::path::Path::new(skill_path);
                if path.exists() {
                    match Skill::load_from_dir(path) {
                        Ok(skill) => {
                            if let Some(ref ctx) = skill.context {
                                agent = agent.with_context(ctx.clone());
                            }
                            let mut registry = ToolRegistry::new();
                            for tool in skill.into_registered_tools() {
                                registry.register(tool);
                            }
                            agent = agent.with_tools(registry);
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load skill '{}': {}", skill_path, e);
                        }
                    }
                }
            }
            agents.insert(config.name.clone(), agent);
        }

        let state = Arc::new(AgentServerState {
            agents,
            conversations: RwLock::new(HashMap::new()),
            llm_client,
        });
        let router = agent_router(state);

        Ok(Runnable::new(async move {
            let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port))
                .await
                .map_err(|e| anyhow::anyhow!("Failed to bind to port {}: {}", port, e))?;
            tracing::info!("Agent server listening on port {}", port);
            axum::serve(listener, router)
                .await
                .map_err(|e| anyhow::anyhow!("Server error: {}", e))?;
            Ok(ExitCode::SUCCESS)
        }))
    }
}
