//! agent-cli: CLI binary for the arlo-rust agent framework.
//!
//! Provides two modes of operation:
//! - **Single-prompt mode**: `arlo [--model MODEL] "your prompt here"`
//! - **Interactive REPL mode**: `arlo [--model MODEL]` (default when no prompt)
//!
//! API keys are read from environment variables:
//! - `OPENAI_API_KEY` for OpenAI models
//! - `ANTHROPIC_API_KEY` for Anthropic models
//! - `OLLAMA_HOST` for local Ollama models

mod tui;

use std::path::PathBuf;
use std::process;
use std::sync::Arc;

use agent_core::{
    run, Agent, DenyAllApprovalHandler, InMemoryTaskStore, Input, Instructions, ModelProvider,
    PermissionEngine, PermissionMode, RunConfig, SkillRegistry, SkillTool, SubAgentDef,
    SubAgentTool, TaskStore, TodoListTool, Tool,
};
use agent_llm::UnifiedProvider;
use agent_tools::{FileReadTool, FileWriteTool, GlobTool, GrepTool, ShellTool, WebFetchTool, WebSearchTool, BraveSearchProvider};

/// Parsed CLI options.
struct CliArgs {
    model: Option<String>,
    prompt: Option<String>,
    dump_prompt: bool,
    /// When true, skip all permission checks (bypass mode).
    skip_permissions: bool,
}

/// Parse CLI arguments manually (no clap dependency needed).
///
/// Usage: arlo [--model MODEL] [--dump-prompt] [PROMPT...]
///
/// Returns parsed CLI arguments.
fn parse_args() -> Result<CliArgs, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut model: Option<String> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut dump_prompt = false;
    let mut skip_permissions = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--model" => {
                i += 1;
                if i >= args.len() {
                    return Err("--model requires a value".to_string());
                }
                model = Some(args[i].clone());
            }
            "--dump-prompt" => {
                dump_prompt = true;
            }
            "--skip-permissions" | "--yolo" | "--no-permissions" => {
                skip_permissions = true;
            }
            "--help" | "-h" => {
                print_usage();
                process::exit(0);
            }
            arg if arg.starts_with("--") => {
                return Err(format!("unrecognized option: {}", arg));
            }
            _ => {
                prompt_parts.push(args[i].clone());
            }
        }
        i += 1;
    }

    let prompt = if prompt_parts.is_empty() {
        None
    } else {
        Some(prompt_parts.join(" "))
    };

    Ok(CliArgs {
        model,
        prompt,
        dump_prompt,
        skip_permissions,
    })
}

/// Print usage information.
fn print_usage() {
    eprintln!("Usage: arlo [OPTIONS] [PROMPT...]");
    eprintln!();
    eprintln!("An autonomous coding agent powered by LLMs.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --model <MODEL>   Model name (e.g., openai:gpt-4, anthropic:claude-sonnet-4-20250514)");
    eprintln!("  --dump-prompt     Print the full system prompt (instructions + tool definitions) and exit");
    eprintln!("  --skip-permissions");
    eprintln!("                    Skip all permission checks (auto-approve every tool call)");
    eprintln!("  --yolo            Alias for --skip-permissions");
    eprintln!("  --help, -h        Show this help message");
    eprintln!();
    eprintln!("If PROMPT is provided, run in single-prompt mode (print response and exit).");
    eprintln!("If no PROMPT is provided, enter interactive REPL mode.");
    eprintln!();
    eprintln!("Environment variables:");
    eprintln!("  OPENAI_API_KEY      API key for OpenAI models");
    eprintln!("  ANTHROPIC_API_KEY   API key for Anthropic models");
    eprintln!("  OLLAMA_HOST         Host URL for local Ollama server");
    eprintln!("  BRAVE_API_KEY       API key for Brave Search (enables web_search tool)");
}

/// Create the default set of built-in tools.
fn default_tools() -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ShellTool::new()),
        Arc::new(FileReadTool::new()),
        Arc::new(FileWriteTool::new()),
        Arc::new(GlobTool::new()),
        Arc::new(GrepTool::new()),
        Arc::new(WebFetchTool::new()),
    ];

    // Register WebSearchTool only if Brave API key is available
    if let Ok(api_key) = std::env::var("BRAVE_API_KEY") {
        if !api_key.is_empty() {
            tools.push(Arc::new(WebSearchTool::new(
                Box::new(BraveSearchProvider::new(api_key)),
            )));
        }
    }

    tools
}

/// Discover the project-level skills directory.
///
/// Looks for `.arlo/skills/` in the current working directory.
fn project_skills_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let dir = cwd.join(".arlo").join("skills");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Discover the user-level skills directory.
///
/// Looks for `~/.arlo/skills/`.
fn user_skills_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    let dir = home.join(".arlo").join("skills");
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// Load skills from project-level and user-level directories, returning
/// the registry and the skill tools ready for registration.
fn load_skills() -> (SkillRegistry, Vec<Arc<dyn Tool>>) {
    let project_dir = project_skills_dir();
    let user_dir = user_skills_dir();

    let registry = SkillRegistry::load(
        project_dir.as_deref(),
        user_dir.as_deref(),
    );

    let skill_tools: Vec<Arc<dyn Tool>> = registry
        .skills()
        .iter()
        .cloned()
        .map(|skill| Arc::new(SkillTool::new(skill)) as Arc<dyn Tool>)
        .collect();

    (registry, skill_tools)
}

/// Determine the model name to use.
///
/// Priority: --model flag > default from provider (first available).
fn resolve_model_name(model_override: Option<String>, provider: &UnifiedProvider) -> String {
    if let Some(model) = model_override {
        return model;
    }

    // Use a sensible default based on available providers
    let models = provider.available_models();
    if !models.is_empty() {
        return models[0].clone();
    }

    // Fallback defaults based on environment
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        "anthropic:claude-sonnet-4-20250514".to_string()
    } else if std::env::var("OPENAI_API_KEY").is_ok() {
        "openai:gpt-4o".to_string()
    } else {
        "ollama:llama3".to_string()
    }
}

/// Dump the full system prompt (instructions + tool definitions) for debugging.
///
/// This helps troubleshoot where tokens are being spent by showing exactly what
/// gets sent to the model as the system message and tool schema.
fn dump_prompt(instructions: &Instructions, tools: &[Arc<dyn Tool>]) {
    let mut system_text = match instructions {
        Instructions::Static(s) => s.clone(),
        Instructions::Dynamic(_) => "(dynamic — cannot be rendered statically)".to_string(),
    };

    // Append the current date and time to match runtime resolution
    let now = chrono::Local::now().to_rfc3339();
    if !system_text.is_empty() {
        system_text.push_str("\n\n");
    }
    system_text.push_str(&format!("Current date and time: {}", now));

    println!("╔══════════════════════════════════════════════════════════════════╗");
    println!("║                     SYSTEM PROMPT DUMP                          ║");
    println!("╚══════════════════════════════════════════════════════════════════╝");
    println!();

    // --- System Instructions ---
    println!("┌─── System Instructions ───────────────────────────────────────────");
    if system_text.is_empty() {
        println!("│ (empty — no system prompt configured)");
    } else {
        for line in system_text.lines() {
            println!("│ {}", line);
        }
    }
    println!("└────────────────────────────────────────────────────────────────────");
    println!();

    // --- Tool Definitions ---
    let enabled_tools: Vec<&Arc<dyn Tool>> = tools.iter().filter(|t| t.is_enabled()).collect();
    println!("┌─── Tool Definitions ({} tools) ─────────────────────────────────", enabled_tools.len());

    let mut total_schema_bytes: usize = 0;
    for tool in &enabled_tools {
        let schema = tool.parameters_schema();
        let schema_str = serde_json::to_string_pretty(&schema).unwrap_or_default();
        total_schema_bytes += schema_str.len();

        println!("│");
        println!("│ ▸ {} ", tool.name());
        println!("│   description: {}", tool.description());
        println!("│   schema:");
        for line in schema_str.lines() {
            println!("│     {}", line);
        }
    }
    println!("│");
    println!("└────────────────────────────────────────────────────────────────────");
    println!();

    // --- Token estimate ---
    let instructions_chars = system_text.len();
    // Rough estimate: ~4 chars per token for English text, ~3 for JSON
    let est_instruction_tokens = instructions_chars / 4;
    let est_schema_tokens = total_schema_bytes / 3;
    let est_total = est_instruction_tokens + est_schema_tokens;

    println!("┌─── Estimated Token Usage ─────────────────────────────────────────");
    println!("│ Instructions:  ~{:>6} chars  (~{} tokens)", instructions_chars, est_instruction_tokens);
    println!("│ Tool schemas:  ~{:>6} chars  (~{} tokens)", total_schema_bytes, est_schema_tokens);
    println!("│ ─────────────────────────────────────");
    println!("│ Total estimate: ~{} tokens (before model-specific tokenization)", est_total);
    println!("└────────────────────────────────────────────────────────────────────");
}

/// Run a single prompt through the agent and return the output.
///
/// In single-prompt (non-interactive) mode, a `DenyAllApprovalHandler` is wired
/// so that any tool requiring approval is automatically denied rather than
/// hanging on user input that will never come. The default `PermissionMode::Bypass`
/// means most tools skip permission checks entirely, but if the mode is changed
/// to `Normal` (e.g., via settings file loading), the handler ensures safe behavior.
///
/// If a `TaskStore` is provided, `SubAgentTool` instances will be constructed with
/// task tracking enabled via `with_task_store()`.
async fn run_single_prompt(
    provider: Arc<UnifiedProvider>,
    model: &str,
    prompt: &str,
    tools: Vec<Arc<dyn Tool>>,
    instructions: Instructions,
    _task_store: Option<Arc<dyn TaskStore>>,
) -> Result<String, String> {
    let mut builder = Agent::builder("arlo").instructions(instructions);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build();

    let permissions = PermissionEngine::new(PermissionMode::Bypass);

    let mut config_builder = RunConfig::builder(provider.clone(), model)
        .permissions(permissions)
        .approval_handler(Arc::new(DenyAllApprovalHandler));

    if let Some(store) = _task_store {
        config_builder = config_builder.task_store(store);
    }

    let config = config_builder.build();

    let input = Input::Fresh {
        prompt: prompt.to_string(),
    };

    match run(&agent, input, &config).await {
        Ok(result) => Ok(result.output),
        Err(e) => Err(format!("Error: {}", e)),
    }
}

#[tokio::main]
async fn main() {
    // Parse arguments
    let cli = match parse_args() {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!();
            print_usage();
            process::exit(1);
        }
    };

    // Initialize the unified provider from environment (not needed for --dump-prompt)
    let provider = match UnifiedProvider::from_env() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            if cli.dump_prompt {
                // For dump-prompt, provider isn't strictly necessary but we
                // still want to show the model resolution if possible.
                eprintln!("warning: {}", e);
                eprintln!();

                // Load skills and tools anyway for the dump
                let (skill_registry, skill_tools) = load_skills();
                let mut tools = default_tools();
                tools.extend(skill_tools);

                let skill_prompt = skill_registry.system_prompt_section();
                let instructions = if skill_prompt.is_empty() {
                    Instructions::Static("(core prompt omitted in no-provider mode)".to_string())
                } else {
                    Instructions::Static(skill_prompt)
                };

                dump_prompt(&instructions, &tools);
                process::exit(0);
            }
            eprintln!("error: {}", e);
            eprintln!();
            eprintln!("Set at least one of: OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_HOST");
            process::exit(1);
        }
    };

    // Resolve the model name
    let model = resolve_model_name(cli.model, &provider);

    // Load skills from .arlo/skills/ directories
    let (skill_registry, skill_tools) = load_skills();

    // Build the combined tools list (built-in + skills)
    let mut tools = default_tools();
    tools.extend(skill_tools);

    // Create the shared TaskStore for background task tracking and todo planning
    let task_store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());

    // Register TodoListTool with the shared store
    tools.push(Arc::new(TodoListTool::new(task_store.clone())));

    // Register SubAgentTool for background task delegation
    {
        let sub_agent = Agent::builder("sub-agent")
            .instructions(Instructions::Static(
                "You are a background helper agent. Complete the delegated task using available tools. \
                 Return a concise summary of your findings or actions when done.".to_string()
            ))
            .tool(Arc::new(ShellTool::new()))
            .tool(Arc::new(FileReadTool::new()))
            .tool(Arc::new(FileWriteTool::new()))
            .tool(Arc::new(GlobTool::new()))
            .tool(Arc::new(GrepTool::new()))
            .build();

        let sub_agent_def = SubAgentDef {
            agent: Arc::new(sub_agent),
            tool_name: Some("sub_agent".to_string()),
            tool_description: Some(
                "Spawn a background sub-agent to handle a delegated task. The sub-agent runs \
                 independently with access to shell, file, and search tools. Its progress is \
                 tracked and you'll be notified when it completes.".to_string()
            ),
            input_schema: None,
            max_turns: Some(15),
            background: true,
            allowed_tools: None,
        };

        let sub_agent_config = RunConfig::builder(provider.clone(), &model)
            .permissions(PermissionEngine::new(PermissionMode::Bypass))
            .approval_handler(Arc::new(DenyAllApprovalHandler))
            .max_turns(15)
            .build();

        tools.push(Arc::new(SubAgentTool::with_task_store(
            sub_agent_def,
            sub_agent_config,
            task_store.clone(),
        )));
    }

    // Core agent system prompt — defines autonomous behavior
    let core_prompt = "\
You are arlo, an autonomous coding agent running in the user's terminal. You have access to tools for file operations, shell commands, web search, and planning.

## Task Approach

- When given a task, break it into steps and execute each step using available tools. Do not stop after planning — work through the plan.
- Use the todolist tool to track multi-step work: add items, mark them in_progress as you work, and mark completed when done.
- After creating a plan, immediately begin executing the first item. Continue until all items are complete or you need user input.
- Mark each sub-task as completed immediately upon finishing — do not batch completions.
- When given an unclear instruction, interpret it in the context of the current environment and prior conversation.
- Do not propose changes on material you haven't reviewed. Examine existing state before suggesting modifications.
- If an approach fails, diagnose why before switching tactics — review the error, check assumptions, try a focused fix. Don't retry identically, but don't abandon a viable approach after a single failure either.

## Tool Usage

Using dedicated tools allows the user to better understand and review your work. This is CRITICAL:
- To read files, use file_read instead of cat, head, tail, or sed
- To create or edit files, use file_write instead of cat with heredoc, echo, or sed/awk
- To search for files by name/pattern, use glob instead of find or ls
- To search file contents, use grep instead of shell grep or rg
- Reserve shell exclusively for system commands and terminal operations that require shell execution (installing packages, running builds/tests, git operations, process management)

Additional tool guidance:
- When multiple tool calls are independent, make them in parallel for efficiency.
- If a tool call fails, diagnose why before retrying. Don't retry the identical action blindly.

## Scope & Communication

- Do exactly what was asked. Don't add extras, reorganize surrounding material, or make improvements beyond the request.
- Don't create unnecessary structure or abstractions for one-time operations.
- Prefer modifying what already exists over creating new artifacts.
- Go straight to the point. Lead with the action, not the reasoning. Skip filler.
- If you need clarification or are blocked, ask the user directly.
- For destructive or irreversible actions (deleting files, modifying shared configs, publishing), confirm with the user first.

## Sub-Agent Delegation

- Use the sub_agent tool to delegate independent research or background tasks that don't need your immediate attention.
- The sub-agent runs in the background — you'll be notified when it completes.
- Continue working on other items while background tasks run.

## Safety

- Freely take local, reversible actions (editing files, running queries, reading data).
- For actions that are hard to reverse, affect shared systems, or could be destructive, check with the user before proceeding.
";

    // Build instructions: core prompt + available skills (if any)
    let skill_prompt = skill_registry.system_prompt_section();
    let instructions = if skill_prompt.is_empty() {
        Instructions::Static(core_prompt.to_string())
    } else {
        Instructions::Static(format!("{}\n{}", core_prompt, skill_prompt))
    };

    // Handle --dump-prompt: print everything and exit
    if cli.dump_prompt {
        println!("Model: {}", model);
        println!();
        dump_prompt(&instructions, &tools);
        process::exit(0);
    }

    // Dispatch to single-prompt or REPL mode
    match cli.prompt {
        Some(prompt_text) => {
            // Single-prompt mode: run, print, exit
            match run_single_prompt(
                provider,
                &model,
                &prompt_text,
                tools,
                instructions,
                Some(task_store),
            )
            .await
            {
                Ok(output) => {
                    println!("{}", output);
                }
                Err(e) => {
                    eprintln!("{}", e);
                    process::exit(1);
                }
            }
        }
        None => {
            // Interactive TUI REPL mode
            let permission_mode = if cli.skip_permissions {
                PermissionMode::Bypass
            } else {
                PermissionMode::Normal
            };
            if let Err(e) = tui::run_tui_repl(
                provider,
                &model,
                tools,
                instructions,
                permission_mode,
                task_store,
            )
            .await
            {
                eprintln!("{}", e);
                process::exit(1);
            }
        }
    }
}
