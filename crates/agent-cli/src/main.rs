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
    run, Agent, DenyAllApprovalHandler, Input, Instructions, ModelProvider, PermissionEngine,
    PermissionMode, RunConfig, SkillRegistry, SkillTool, Tool,
};
use agent_llm::UnifiedProvider;
use agent_tools::{FileReadTool, FileWriteTool, GlobTool, GrepTool, ShellTool, WebFetchTool, WebSearchTool, BraveSearchProvider};

/// Parsed CLI options.
struct CliArgs {
    model: Option<String>,
    prompt: Option<String>,
    dump_prompt: bool,
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
    let system_text = match instructions {
        Instructions::Static(s) => s.clone(),
        Instructions::Dynamic(_) => "(dynamic — cannot be rendered statically)".to_string(),
    };

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
async fn run_single_prompt(
    provider: Arc<UnifiedProvider>,
    model: &str,
    prompt: &str,
    tools: Vec<Arc<dyn Tool>>,
    instructions: Instructions,
) -> Result<String, String> {
    let mut builder = Agent::builder("arlo").instructions(instructions);
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build();

    let permissions = PermissionEngine::new(PermissionMode::Bypass);

    let config = RunConfig::builder(provider.clone(), model)
        .permissions(permissions)
        .approval_handler(Arc::new(DenyAllApprovalHandler))
        .build();

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
                    Instructions::Static(String::new())
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

    // Build instructions including available skills
    let skill_prompt = skill_registry.system_prompt_section();
    let instructions = if skill_prompt.is_empty() {
        Instructions::Static(String::new())
    } else {
        Instructions::Static(skill_prompt)
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
            match run_single_prompt(provider, &model, &prompt_text, tools, instructions).await {
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
            if let Err(e) = tui::run_tui_repl(provider, &model, tools, instructions).await {
                eprintln!("{}", e);
                process::exit(1);
            }
        }
    }
}
