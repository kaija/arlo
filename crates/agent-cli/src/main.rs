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

use std::io::{self, BufRead, Write};
use std::process;
use std::sync::Arc;

use agent_core::{
    run, Agent, Input, ModelProvider, PermissionEngine, PermissionMode, RunConfig,
    Tool,
};
use agent_llm::UnifiedProvider;
use agent_tools::{FileReadTool, FileWriteTool, GlobTool, GrepTool, ShellTool};

/// Parse CLI arguments manually (no clap dependency needed).
///
/// Usage: arlo [--model MODEL] [PROMPT...]
///
/// Returns (model_override, prompt) where prompt is None for REPL mode.
fn parse_args() -> Result<(Option<String>, Option<String>), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    let mut model: Option<String> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
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

    Ok((model, prompt))
}

/// Print usage information.
fn print_usage() {
    eprintln!("Usage: arlo [OPTIONS] [PROMPT...]");
    eprintln!();
    eprintln!("An autonomous coding agent powered by LLMs.");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --model <MODEL>   Model name (e.g., openai:gpt-4, anthropic:claude-sonnet-4-20250514)");
    eprintln!("  --help, -h        Show this help message");
    eprintln!();
    eprintln!("If PROMPT is provided, run in single-prompt mode (print response and exit).");
    eprintln!("If no PROMPT is provided, enter interactive REPL mode.");
    eprintln!();
    eprintln!("Environment variables:");
    eprintln!("  OPENAI_API_KEY      API key for OpenAI models");
    eprintln!("  ANTHROPIC_API_KEY   API key for Anthropic models");
    eprintln!("  OLLAMA_HOST         Host URL for local Ollama server");
}

/// Create the default set of built-in tools.
fn default_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(ShellTool::new()),
        Arc::new(FileReadTool::new()),
        Arc::new(FileWriteTool::new()),
        Arc::new(GlobTool::new()),
        Arc::new(GrepTool::new()),
    ]
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

/// Run a single prompt through the agent and return the output.
async fn run_single_prompt(
    provider: Arc<UnifiedProvider>,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let tools = default_tools();

    let mut builder = Agent::builder("arlo");
    for tool in tools {
        builder = builder.tool(tool);
    }
    let agent = builder.build();

    let permissions = PermissionEngine::new(PermissionMode::Bypass);

    let config = RunConfig::builder(provider.clone(), model)
        .permissions(permissions)
        .build();

    let input = Input::Fresh {
        prompt: prompt.to_string(),
    };

    match run(&agent, input, &config).await {
        Ok(result) => Ok(result.output),
        Err(e) => Err(format!("Error: {}", e)),
    }
}

/// Run the interactive REPL loop.
async fn run_repl(provider: Arc<UnifiedProvider>, model: &str) -> Result<(), String> {
    eprintln!("arlo interactive mode (model: {})", model);
    eprintln!("Type your prompt and press Enter. Type \"exit\" or \"quit\" to leave.");
    eprintln!();

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    loop {
        // Print prompt indicator
        eprint!("> ");
        io::stderr().flush().ok();

        // Read a line from stdin
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => {
                // EOF (Ctrl-D)
                eprintln!();
                break;
            }
            Ok(_) => {}
            Err(e) => {
                return Err(format!("Failed to read input: {}", e));
            }
        }

        let trimmed = line.trim();

        // Check for exit commands
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }

        // Run the prompt
        match run_single_prompt(provider.clone(), model, trimmed).await {
            Ok(output) => {
                writeln!(stdout, "{}", output).ok();
                stdout.flush().ok();
            }
            Err(e) => {
                eprintln!("{}", e);
            }
        }

        eprintln!();
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    // Parse arguments
    let (model_override, prompt) = match parse_args() {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!();
            print_usage();
            process::exit(1);
        }
    };

    // Initialize the unified provider from environment
    let provider = match UnifiedProvider::from_env() {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("error: {}", e);
            eprintln!();
            eprintln!("Set at least one of: OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_HOST");
            process::exit(1);
        }
    };

    // Resolve the model name
    let model = resolve_model_name(model_override, &provider);

    // Dispatch to single-prompt or REPL mode
    match prompt {
        Some(prompt_text) => {
            // Single-prompt mode: run, print, exit
            match run_single_prompt(provider, &model, &prompt_text).await {
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
            // Interactive REPL mode
            if let Err(e) = run_repl(provider, &model).await {
                eprintln!("{}", e);
                process::exit(1);
            }
        }
    }
}
