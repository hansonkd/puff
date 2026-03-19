use crate::agents::agent::{Agent, AgentConfig};
use crate::agents::conversation::Conversation;
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
            let mut registry = ToolRegistry::new();
            for skill_path in &config.skills {
                let path = std::path::Path::new(skill_path);
                if path.exists() {
                    match Skill::load_from_dir(path) {
                        Ok(skill) => {
                            if let Some(ref ctx) = skill.context {
                                agent = agent.with_context(ctx.clone());
                            }
                            match skill.into_registered_tools() {
                                Ok(tools) => {
                                    for tool in tools {
                                        registry.register(tool);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "Failed to register tools for skill '{}': {}",
                                        skill_path,
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load skill '{}': {}", skill_path, e);
                        }
                    }
                }
            }
            if !registry.is_empty() {
                agent = agent.with_tools(registry);
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

// ---------------------------------------------------------------------------
// AgentAskCommand — interactive REPL
// ---------------------------------------------------------------------------

/// Start an interactive conversation with a configured agent in the terminal.
pub struct AgentAskCommand {
    pub agent_configs: Vec<AgentConfig>,
    pub llm_config: LlmConfig,
}

impl RunnableCommand for AgentAskCommand {
    fn cli_parser(&self) -> Command {
        Command::new("agent-ask")
            .about("Start an interactive REPL with a configured agent")
            .arg(clap::arg!(--agent <NAME> "Name of the agent to talk to").required(true))
    }

    fn make_runnable(&mut self, args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let agent_name = args
            .get_one::<String>("agent")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("--agent is required"))?;

        let config = self
            .agent_configs
            .iter()
            .find(|c| c.name == agent_name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No agent named '{}' found in config", agent_name))?;

        let llm_client = LlmClient::new(self.llm_config.clone())?;

        // Build the agent, loading skills the same way AgentServeCommand does.
        let mut agent = Agent::new(config.clone());
        let mut registry = ToolRegistry::new();
        for skill_path in &config.skills {
            let path = std::path::Path::new(skill_path);
            if path.exists() {
                match Skill::load_from_dir(path) {
                    Ok(skill) => {
                        if let Some(ref ctx) = skill.context {
                            agent = agent.with_context(ctx.clone());
                        }
                        match skill.into_registered_tools() {
                            Ok(tools) => {
                                for tool in tools {
                                    registry.register(tool);
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to register tools for skill '{}': {}",
                                    skill_path,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load skill '{}': {}", skill_path, e);
                    }
                }
            }
        }
        if !registry.is_empty() {
            agent = agent.with_tools(registry);
        }

        Ok(Runnable::new(async move {
            use std::io::{BufRead, Write};

            let mut conv = Conversation::new(&agent.config.name);

            let stdin = std::io::stdin();
            let stdout = std::io::stdout();

            println!(
                "Talking to agent '{}'. Type 'exit' or 'quit' to stop.",
                agent.config.name
            );

            loop {
                print!("> ");
                stdout.lock().flush()?;

                let mut line = String::new();
                if stdin.lock().read_line(&mut line)? == 0 {
                    break; // EOF
                }
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if line == "exit" || line == "quit" {
                    break;
                }

                conv.add_user_message(line);
                match agent.run_turn(&mut conv, &llm_client).await {
                    Ok(response) => println!("\n{}\n", response.text),
                    Err(e) => eprintln!("Error: {}", e),
                }
            }

            Ok(ExitCode::SUCCESS)
        }))
    }
}

// ---------------------------------------------------------------------------
// AgentListCommand — list configured agents
// ---------------------------------------------------------------------------

/// Print a table of all configured agents.
pub struct AgentListCommand {
    pub agent_configs: Vec<AgentConfig>,
}

impl RunnableCommand for AgentListCommand {
    fn cli_parser(&self) -> Command {
        Command::new("agent-list").about("List all configured agents")
    }

    fn make_runnable(&mut self, _args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let configs = self.agent_configs.clone();
        Ok(Runnable::new(async move {
            println!("{:<20} {:<30} {}", "Name", "Model", "Skills");
            println!("{}", "\u{2500}".repeat(70));
            for c in &configs {
                let skills = if c.skills.is_empty() {
                    "(none)".to_string()
                } else {
                    c.skills
                        .iter()
                        .map(|s| {
                            // Use the last path component as the display name.
                            std::path::Path::new(s)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(s.as_str())
                                .to_string()
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                };
                println!("{:<20} {:<30} {}", c.name, c.model, skills);
            }
            Ok(ExitCode::SUCCESS)
        }))
    }
}

// ---------------------------------------------------------------------------
// SkillListCommand — list available skills
// ---------------------------------------------------------------------------

/// For each skill path, try to load the skill and print info.
pub struct SkillListCommand {
    pub skill_paths: Vec<String>,
}

impl RunnableCommand for SkillListCommand {
    fn cli_parser(&self) -> Command {
        Command::new("skill-list").about("List available skills from configured skill paths")
    }

    fn make_runnable(&mut self, _args: &ArgMatches, _context: PuffContext) -> Result<Runnable> {
        let skill_paths = self.skill_paths.clone();
        Ok(Runnable::new(async move {
            println!(
                "{:<20} {:<12} {:<8} {}",
                "Name", "Version", "Tools", "Source"
            );
            println!("{}", "\u{2500}".repeat(60));
            for skill_path in &skill_paths {
                let path = std::path::Path::new(skill_path);
                match Skill::load_from_dir(path) {
                    Ok(skill) => {
                        println!(
                            "{:<20} {:<12} {:<8} {}",
                            skill.name,
                            skill.version,
                            skill.tools.len(),
                            skill.source_dir,
                        );
                    }
                    Err(e) => {
                        eprintln!("Failed to load skill '{}': {}", skill_path, e);
                    }
                }
            }
            Ok(ExitCode::SUCCESS)
        }))
    }
}
