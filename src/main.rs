mod agent;
mod cli;
mod config;
mod embedding;
mod llm;
mod mcp;
mod memory;
mod prompt;
mod skill;
mod subagent;
mod tools;
mod workspace;

use agent::AgentMode;
use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let (mut agent, args, _log_guard) = bootstrap().await?;

    // Dispatch to the appropriate mode
    if let Some(ref prompt_arg) = args.print {
        // ── Non-interactive (print) mode ──
        let prompt = if prompt_arg == "-" {
            cli::read_stdin_prompt()?
        } else {
            prompt_arg.clone()
        };

        tracing::info!(
            prompt_len = prompt.len(),
            output_format = ?args.output_format,
            "Running in print mode"
        );

        let exit_code = cli::run_print(&mut agent, &prompt, &args.output_format).await?;

        // Persist memory and perform cleanup before exiting
        if let Err(e) = agent.shutdown().await {
            tracing::error!(error = %e, "Failed to shutdown agent cleanly");
        }

        std::process::exit(match exit_code {
            std::process::ExitCode::SUCCESS => 0,
            _ => 1,
        });
    } else {
        // ── Interactive (REPL) mode ──
        cli::run_interactive(&mut agent).await?;
    }

    Ok(())
}

/// Bootstrap the application: parse args, load config, initialize logging,
/// create the agent with all extensions (MCP, skills, subagents).
///
/// This separates initialization from mode dispatch, keeping `main()` focused
/// on the high-level control flow.
async fn bootstrap() -> Result<(agent::ChatAgent, cli::CliArgs, config::LogGuard)> {
    // Parse CLI arguments first (before any other initialization)
    let args = cli::CliArgs::parse();

    // Resolve workspace (zero-config: auto-creates ~/.daedalus/)
    let workspace = workspace::Workspace::resolve()?;

    // Phase 1: Load raw config from workspace YAML (single file read, no tracing)
    let (mut raw_config, log_config) = config::load_from_workspace(&workspace)?;

    // Apply CLI overrides to the raw config before building AgentConfig
    apply_cli_overrides(&args, &mut raw_config);

    // Initialize logging with the loaded config
    // In verbose mode, override log level to debug on stderr
    let _log_guard = if args.verbose {
        config::init_logging_verbose(&log_config)?
    } else {
        config::init_logging(&log_config)?
    };

    // Phase 2: Build AgentConfig (now tracing is available for soul file loading)
    let mut agent_config = raw_config.into_agent_config(&workspace);

    // Apply system prompt overrides from CLI
    apply_prompt_overrides(&args, &mut agent_config);

    tracing::info!("Daedalus Agent starting...");
    tracing::info!(
        workspace = %workspace.root().display(),
        kind = %workspace.kind(),
        config_file = workspace.has_config_file(),
        print_mode = args.is_print_mode(),
        "Workspace resolved"
    );
    tracing::info!("Using model: {}", agent_config.model());
    if let Some(base_url) = agent_config.api_base() {
        tracing::info!("Using API base URL: {}", base_url);
    }

    // Build the agent with all extensions
    let agent = build_agent(&args, &workspace, &agent_config).await?;

    Ok((agent, args, _log_guard))
}

/// Create the ChatAgent and attach all extensions (MCP, skills, subagents, filters).
async fn build_agent(
    args: &cli::CliArgs,
    workspace: &workspace::Workspace,
    agent_config: &config::AgentConfig,
) -> Result<agent::ChatAgent> {
    let skip_extensions = args.bare;

    // Load MCP configuration (workspace-aware search chain)
    let mcp_manager = if !skip_extensions {
        load_mcp(workspace).await?
    } else {
        tracing::info!("Bare mode: skipping MCP server discovery");
        None
    };

    // Create the LLM provider
    let provider = llm::create_provider(agent_config.llm.clone())?;

    // Build the chat agent with workspace-aware memory persistence
    let mut agent = agent::ChatAgent::new_with_workspace(provider, agent_config, workspace.clone());
    if let Some(manager) = mcp_manager {
        agent.attach_mcp(manager);
    }

    if !skip_extensions {
        // Load skills from workspace skills directory
        let ws_skills_dir = workspace.skills_dir();
        load_skills(&mut agent, &ws_skills_dir);

        // Also load skills from cwd/skills/ if different from workspace
        let cwd_skills_dir = std::env::current_dir()
            .unwrap_or_default()
            .join("skills");
        if cwd_skills_dir != ws_skills_dir {
            load_skills(&mut agent, &cwd_skills_dir);
        }

        // Load subagent definitions from workspace and global directories
        load_subagents(&mut agent, workspace, agent_config);
    } else {
        tracing::info!("Bare mode: skipping skills and subagents discovery");
    }

    // Apply tool filtering from CLI args
    apply_tool_filtering(args, &mut agent);

    // Apply max-turns override from CLI args
    if let Some(max_turns) = args.max_turns {
        agent.set_max_tool_rounds(max_turns);
    }

    Ok(agent)
}

/// Load MCP servers from workspace configuration.
async fn load_mcp(workspace: &workspace::Workspace) -> Result<Option<mcp::McpManager>> {
    let mcp_config = mcp::McpConfig::load_with_workspace(workspace)?;
    if mcp_config.servers.is_empty() {
        return Ok(None);
    }

    tracing::info!(
        servers = mcp_config.servers.len(),
        "Connecting to MCP servers..."
    );
    let manager = mcp::McpManager::from_config(&mcp_config).await;
    tracing::info!(
        tools = manager.tool_count(),
        "MCP initialization complete"
    );
    Ok(Some(manager))
}

/// Apply CLI argument overrides to the raw config (before AgentConfig is built).
fn apply_cli_overrides(args: &cli::CliArgs, raw_config: &mut config::RawConfig) {
    if let Some(ref model) = args.model {
        raw_config.set_model(model.clone());
    }
}

/// Apply system prompt overrides from CLI arguments.
fn apply_prompt_overrides(args: &cli::CliArgs, config: &mut config::AgentConfig) {
    if let Some(ref prompt) = args.system_prompt {
        config.system_prompt = prompt.clone();
        config.is_custom_prompt = true;
    }
    if let Some(ref append) = args.append_system_prompt {
        config.system_prompt = format!("{}\n\n{}", config.system_prompt, append);
        config.is_custom_prompt = true;
    }
}

/// Apply tool filtering based on CLI arguments.
///
/// Creates a `ToolFilter` from the allowed/disallowed tool lists and
/// applies it to the agent's tool router.
fn apply_tool_filtering(args: &cli::CliArgs, agent: &mut agent::ChatAgent) {
    let allowed = args.allowed_tools_list();
    let disallowed = args.disallowed_tools_list();

    if let Some(filter) = agent::ToolFilter::new(allowed.clone(), disallowed.clone()) {
        tracing::info!(
            allowed = ?allowed,
            disallowed = ?disallowed,
            "Applying tool filter"
        );
        agent.set_tool_filter(Some(filter));
    }
}

/// Load skills from a directory, logging the result.
fn load_skills(agent: &mut agent::ChatAgent, dir: &std::path::Path) {
    match agent.load_skills(dir) {
        Ok(count) if count > 0 => {
            tracing::info!(
                skills = count,
                path = %dir.display(),
                "Skills loaded successfully"
            );
        }
        Ok(_) => {
            tracing::debug!(
                path = %dir.display(),
                "No skills found in directory"
            );
        }
        Err(e) => {
            tracing::warn!(
                path = %dir.display(),
                error = %e,
                "Failed to load skills, continuing without skills"
            );
        }
    }
}

/// Load subagent definitions from workspace and global directories.
///
/// Subagents are loaded from two locations with priority:
/// 1. Project-level: `.daedalus/agents/` (higher priority)
/// 2. Global: `~/.daedalus/agents/` (lower priority, if different)
fn load_subagents(
    agent: &mut agent::ChatAgent,
    workspace: &workspace::Workspace,
    config: &config::AgentConfig,
) {
    let ws_agents_dir = workspace.agents_dir();

    // Collect directories and sources
    let mut dirs: Vec<&std::path::Path> = vec![&ws_agents_dir];
    let mut sources = vec![subagent::SubagentSource::Project];

    // Also load from global ~/.daedalus/agents/ if different from workspace
    let global_agents_dir = global_agents_dir();
    if let Some(ref global_dir) = global_agents_dir {
        if *global_dir != ws_agents_dir {
            dirs.push(global_dir.as_path());
            sources.push(subagent::SubagentSource::Global);
        }
    }

    match agent.load_subagents(&dirs, &sources, config.llm.clone()) {
        Ok(count) if count > 0 => {
            tracing::info!(
                subagents = count,
                "Subagents loaded successfully"
            );
        }
        Ok(_) => {
            tracing::debug!("No subagent definitions found");
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Failed to load subagents, continuing without subagents"
            );
        }
    }
}

/// Get the global `~/.daedalus/agents/` directory path.
fn global_agents_dir() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(|home| std::path::PathBuf::from(home).join(".daedalus/agents"))
}
