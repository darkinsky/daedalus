mod acp;
mod agent;
mod cli;
mod config;
mod embedding;
mod hooks;
mod llm;
mod mcp;
mod memory;
mod middleware;
mod prompt;
mod skill;
mod subagent;
mod tools;
mod workspace;

// Named `agent_tracing` to avoid shadowing the `tracing` crate used for logging.
#[path = "tracing/mod.rs"]
mod agent_tracing;

use agent::AgentMode;
use anyhow::Result;
use clap::Parser;
use std::sync::Arc;

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

    // Extract sub-configs before `into_agent_config()` consumes raw_config.
    let tracing_config = raw_config.tracing.clone();
    let middleware_config = raw_config.middleware.clone();
    let acp_config = raw_config.acp.clone();
    let tools_config = raw_config.tools.clone();
    let permissions_config = raw_config.permissions.clone();
    let hooks_config = raw_config.hooks.clone();

    // Apply CLI overrides to the raw config before building AgentConfig
    apply_cli_overrides(&args, &mut raw_config);

    // Initialize logging with the loaded config
    // In verbose mode, override log level to debug on stderr
    let _log_guard = if args.verbose {
        config::init_logging_verbose(&log_config)?
    } else {
        config::init_logging(&log_config)?
    };

    // Set CLI output verbosity (controls tool event rendering detail level).
    // --verbose flag or DAEDALUS_VERBOSE=1 env var enables verbose output.
    let verbose = args.verbose || std::env::var("DAEDALUS_VERBOSE").map(|v| v == "1").unwrap_or(false);
    cli::set_verbose(verbose);

    // Phase 2: Build AgentConfig (now tracing is available for soul file loading)
    let mut agent_config = raw_config.into_agent_config(&workspace);

    // Validate middleware configuration (emits warnings for common mistakes)
    middleware_config.validate();

    // Pre-initialize blocking resources outside hot async paths.
    // `find_rg_binary()` runs `std::process::Command` which would block
    // a Tokio worker thread if deferred to first tool invocation.
    tools::ensure_rg_init();

    // Apply system prompt overrides from CLI
    apply_prompt_overrides(&args, &mut agent_config);

    // Phase 3: Initialize the TracingManager from config
    let tracing_manager = init_tracing_manager(&tracing_config, &workspace).await;

    tracing::info!("Daedalus Agent starting...");
    tracing::info!(
        workspace = %workspace.root().display(),
        kind = %workspace.kind(),
        config_file = workspace.has_config_file(),
        print_mode = args.is_print_mode(),
        tracing_enabled = tracing_config.enabled,
        "Workspace resolved"
    );
    tracing::info!("Using model: {}", agent_config.model());
    tracing::info!("Using memory strategy: {}", agent_config.memory_strategy);
    if let Some(base_url) = agent_config.api_base() {
        tracing::info!("Using API base URL: {}", base_url);
    }
    if agent_config.project_rules.is_some() {
        tracing::info!("Project rules loaded from DAEDALUS.md");
    }

    // Build the agent with all extensions
    let agent = build_agent(&args, &workspace, &agent_config, tracing_manager, middleware_config, acp_config, tools_config, permissions_config, hooks_config).await?;

    Ok((agent, args, _log_guard))
}

/// Create the ChatAgent and attach all extensions (MCP, skills, subagents, ACP, filters).
async fn build_agent(
    args: &cli::CliArgs,
    workspace: &workspace::Workspace,
    agent_config: &config::AgentConfig,
    tracing_manager: Arc<agent_tracing::TracingManager>,
    middleware_config: middleware::config::MiddlewareConfig,
    acp_config: acp::AcpConfig,
    tools_config: tools::ToolsConfig,
    permissions_config: middleware::builtin::permission_rules::PermissionsConfig,
    hooks_config: hooks::config::HooksConfig,
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
    agent.set_tracing_manager(tracing_manager);
    agent.set_middleware_config(middleware_config);
    // Only replace the bash tool if the user configured non-default settings,
    // avoiding unnecessary tool replacement during bootstrap.
    if tools_config.bash != tools::bash::BashConfig::default() {
        agent.set_bash_config(tools_config.bash.clone());
    }
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

    // Initialize ACP agents (if configured and not in bare mode)
    if !skip_extensions && acp_config.has_agents() {
        load_acp_agents(&mut agent, &acp_config).await;
    }

    // Register web_search tool with the configured provider
    let web_search_tool = tools::web_search::WebSearchTool::new(tools_config.web_search);
    agent.install_acp_tool(Box::new(web_search_tool));

    // Apply tool filtering from CLI args
    apply_tool_filtering(args, &mut agent);

    // Apply permission settings from CLI args
    if args.skip_permissions {
        agent.set_skip_permissions(true);
    }
    agent.set_permissions_config(permissions_config);
    agent.set_hooks_config(hooks_config);

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
    // CLI --prompt-style overrides YAML config
    if let Some(ref style) = args.prompt_style {
        config.prompt_style = match style {
            cli::CliPromptStyle::Default => prompt::PromptStyle::Default,
            cli::CliPromptStyle::Coding => prompt::PromptStyle::Coding,
        };
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

/// Initialize the TracingManager from configuration.
///
/// Creates the appropriate collectors based on the `tracing` config section
/// and returns an `Arc<TracingManager>` ready to be shared with the agent.
async fn init_tracing_manager(
    config: &agent_tracing::TracingConfig,
    workspace: &workspace::Workspace,
) -> Arc<agent_tracing::TracingManager> {
    let flags = agent_tracing::ContentFlags::from_config(config);
    let manager = Arc::new(agent_tracing::TracingManager::new(config.enabled, flags));

    if !config.enabled {
        return manager;
    }

    for collector_config in &config.collectors {
        match collector_config {
            agent_tracing::config::CollectorConfig::File { path, format } => {
                let output_dir = path
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| workspace.traces_dir());
                let collector = agent_tracing::exporters::file::FileCollector::new(
                    output_dir,
                    format.clone(),
                    flags,
                );
                manager.add_collector(Box::new(collector)).await;
            }
            agent_tracing::config::CollectorConfig::Console { verbosity } => {
                let collector =
                    agent_tracing::exporters::console::ConsoleCollector::new(verbosity.clone());
                manager.add_collector(Box::new(collector)).await;
            }
            agent_tracing::config::CollectorConfig::Otel {
                endpoint,
                service_name,
            } => {
                let collector = agent_tracing::exporters::otel::OtelCollector::new(
                    endpoint.clone(),
                    service_name.clone(),
                );
                manager.add_collector(Box::new(collector)).await;
            }
            agent_tracing::config::CollectorConfig::Langfuse {
                public_key,
                secret_key,
                host,
            } => {
                let collector = agent_tracing::exporters::langfuse::LangfuseCollector::new(
                    public_key.clone(),
                    secret_key.clone(),
                    host.clone(),
                );
                manager.add_collector(Box::new(collector)).await;
            }
        }
    }

    manager
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
    let home_agents_dir = workspace::Workspace::home_agents_dir();
    if let Some(ref global_dir) = home_agents_dir {
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

/// Initialize ACP agents from configuration and install the ACP tool.
///
/// Connects to configured remote ACP agents and registers the `call_acp_agent`
/// built-in tool so the LLM can delegate tasks to ACP-compatible agents.
async fn load_acp_agents(
    agent: &mut agent::ChatAgent,
    config: &acp::AcpConfig,
) {
    match acp::init_acp_client(config).await {
        Some((client, cards)) => {
            if let Some(acp_tool) = acp::build_acp_tool(client, cards.clone()) {
                agent.install_acp_tool(acp_tool);
                tracing::info!(
                    agents = cards.len(),
                    names = ?cards.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
                    "ACP agents loaded successfully"
                );
            }
        }
        None => {
            tracing::debug!("No ACP agents available");
        }
    }
}
