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

mod assembly;
mod serve;
mod tui;

use std::process;
use std::sync::Arc;

use async_trait::async_trait;

use agent_core::{
    run, FsSessionStore, InMemoryTaskStore, Input, Instructions, Message, Model, ModelError,
    ModelProvider, PermissionMode, SessionStore, Tool,
};
use agent_llm::{ModelOverrideWrapper, UnifiedProvider};

/// A wrapping `ModelProvider` that applies `ModelOverrideWrapper` after resolving a model.
///
/// When a profile specifies `context_window` or `max_output_tokens`, the resolved model
/// is wrapped to override those values. If no overrides are present, the inner model
/// is returned directly (zero-cost passthrough).
pub(crate) struct OverridingProvider {
    pub inner: Arc<UnifiedProvider>,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
}

#[async_trait]
impl ModelProvider for OverridingProvider {
    async fn resolve(&self, model_name: &str) -> Result<Arc<dyn Model>, ModelError> {
        let base_model = self.inner.resolve(model_name).await?;
        Ok(ModelOverrideWrapper::wrap_if_needed(
            base_model,
            self.context_window,
            self.max_output_tokens,
        ))
    }

    fn available_models(&self) -> Vec<String> {
        self.inner.available_models()
    }
}

/// Parsed CLI options.
#[derive(Debug)]
struct CliArgs {
    model: Option<String>,
    profile: Option<String>,
    prompt: Option<String>,
    dump_prompt: bool,
    /// When true, skip all permission checks (bypass mode).
    skip_permissions: bool,
    /// Resume a stored session by id.
    resume: Option<String>,
    /// When true, list stored sessions and exit.
    list_sessions: bool,
    /// Serve mode: `Some(None)` = default addr, `Some(Some("..."))` = explicit addr.
    serve: Option<Option<String>>,
}

/// Parse CLI arguments from a given slice (testable version).
///
/// `args` should NOT include the binary name (argv[0]) — only the user-supplied flags and positional args.
///
/// Usage: arlo [--model MODEL] [--profile NAME] [--dump-prompt] [PROMPT...]
///
/// Returns parsed CLI arguments.
fn parse_args_from(args: &[String]) -> Result<CliArgs, String> {
    let mut model: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut prompt_parts: Vec<String> = Vec::new();
    let mut dump_prompt = false;
    let mut skip_permissions = false;
    let mut resume: Option<String> = None;
    let mut list_sessions = false;
    let mut serve: Option<Option<String>> = None;
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
            "--profile" => {
                i += 1;
                if i >= args.len() {
                    return Err("--profile requires a value".to_string());
                }
                profile = Some(args[i].clone());
            }
            "--dump-prompt" => {
                dump_prompt = true;
            }
            "--skip-permissions" | "--yolo" | "--no-permissions" => {
                skip_permissions = true;
            }
            "--resume" => {
                i += 1;
                if i >= args.len() {
                    return Err("--resume requires a session id".to_string());
                }
                resume = Some(args[i].clone());
            }
            "--sessions" => {
                list_sessions = true;
            }
            "--serve" => {
                // Peek at next arg: if it exists and doesn't start with "--", consume as value
                if i + 1 < args.len() && !args[i + 1].starts_with("--") {
                    i += 1;
                    serve = Some(Some(args[i].clone()));
                } else {
                    serve = Some(None);
                }
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
        profile,
        prompt,
        dump_prompt,
        skip_permissions,
        resume,
        list_sessions,
        serve,
    })
}

/// Parse CLI arguments manually (no clap dependency needed).
///
/// Usage: arlo [--model MODEL] [--dump-prompt] [PROMPT...]
///
/// Returns parsed CLI arguments.
fn parse_args() -> Result<CliArgs, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    parse_args_from(&args)
}

/// Print usage information.
fn print_usage() {
    eprintln!("Usage: arlo [OPTIONS] [PROMPT...]");
    eprintln!();
    eprintln!("An autonomous coding agent powered by LLMs.");
    eprintln!();
    eprintln!("Options:");
    eprintln!(
        "  --model <MODEL>   Model name (e.g., openai:gpt-4, anthropic:claude-sonnet-4-20250514)"
    );
    eprintln!("  --profile <NAME>  Use a named provider profile from settings");
    eprintln!("  --dump-prompt     Print the full system prompt (instructions + tool definitions) and exit");
    eprintln!("  --skip-permissions");
    eprintln!("                    Skip all permission checks (auto-approve every tool call)");
    eprintln!("  --yolo            Alias for --skip-permissions");
    eprintln!("  --resume <ID>     Resume a stored session (see --sessions)");
    eprintln!("  --sessions        List stored sessions (~/.arlo/sessions) and exit");
    eprintln!("  --serve [ADDR]    Start AG-UI HTTP server (default: 127.0.0.1:8080)");
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
    println!(
        "┌─── Tool Definitions ({} tools) ─────────────────────────────────",
        enabled_tools.len()
    );

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
    println!(
        "│ Instructions:  ~{:>6} chars  (~{} tokens)",
        instructions_chars, est_instruction_tokens
    );
    println!(
        "│ Tool schemas:  ~{:>6} chars  (~{} tokens)",
        total_schema_bytes, est_schema_tokens
    );
    println!("│ ─────────────────────────────────────");
    println!(
        "│ Total estimate: ~{} tokens (before model-specific tokenization)",
        est_total
    );
    println!("└────────────────────────────────────────────────────────────────────");
}

/// Run a single prompt through the agent and return the output.
async fn run_single_prompt(
    assembled: &assembly::AssemblyOutput,
    prompt: &str,
    session: &SessionContext,
) -> Result<String, String> {
    let mut messages = session.initial_history.clone();
    messages.push(Message::User {
        content: vec![agent_core::ContentBlock::Text {
            text: prompt.to_string(),
        }],
    });
    let input = Input::Items { messages };

    match run(&assembled.agent, input, &assembled.config).await {
        Ok(result) => {
            if let Err(e) = session
                .store
                .save(&session.id, &result.state.messages)
                .await
            {
                eprintln!("warning: failed to persist session {}: {}", session.id, e);
            }
            Ok(result.output)
        }
        Err(e) => Err(format!("Error: {}", e)),
    }
}

/// Session persistence context threaded through both CLI modes.
struct SessionContext {
    store: Arc<dyn SessionStore>,
    id: String,
    initial_history: Vec<Message>,
}

/// Generate a fresh session id: local timestamp plus pid for uniqueness.
fn new_session_id() -> String {
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        process::id()
    )
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

    // Session history store (~/.arlo/sessions)
    let session_store: Arc<dyn SessionStore> = Arc::new(FsSessionStore::new());

    // Handle --sessions: list stored sessions and exit (no provider needed)
    if cli.list_sessions {
        match session_store.list().await {
            Ok(sessions) if sessions.is_empty() => println!("No stored sessions."),
            Ok(sessions) => {
                for meta in sessions {
                    let updated: chrono::DateTime<chrono::Local> = meta.updated_at.into();
                    println!("{}  {}", updated.format("%Y-%m-%d %H:%M:%S"), meta.id);
                }
            }
            Err(e) => {
                eprintln!("error: failed to list sessions: {}", e);
                process::exit(1);
            }
        }
        process::exit(0);
    }

    // Resolve session id and prior history (--resume loads an existing session)
    let session = match &cli.resume {
        Some(id) => match session_store.load(id).await {
            Ok(history) => SessionContext {
                store: session_store.clone(),
                id: id.clone(),
                initial_history: history,
            },
            Err(e) => {
                eprintln!("error: cannot resume session '{}': {}", id, e);
                process::exit(1);
            }
        },
        None => SessionContext {
            store: session_store.clone(),
            id: new_session_id(),
            initial_history: Vec::new(),
        },
    };

    // Assemble the agent for the requested surface.
    let cwd = std::env::current_dir().unwrap_or_default();
    let env = assembly::AssemblyEnv::from_process_env();

    // Determine the surface.  For --dump-prompt and --serve we need to know the
    // surface before assembly, but assembly may fail so we handle dump-prompt
    // after a successful assemble call.
    let surface = if cli.serve.is_some() {
        assembly::Surface::Serve
    } else if cli.prompt.is_some() {
        assembly::Surface::SinglePrompt
    } else {
        assembly::Surface::Tui {
            skip_permissions: cli.skip_permissions,
        }
    };

    let assembled = match assembly::assemble(assembly::AssemblyInputs {
        env: env.clone(),
        profile_name: cli.profile.clone(),
        model_override: cli.model.clone(),
        working_dir: cwd.clone(),
        surface,
    }) {
        Ok(a) => a,
        Err(assembly::AssemblyError::NoProvider(_)) if cli.dump_prompt => {
            // dump-prompt with no provider: show tool schemas only
            eprintln!("warning: no provider configured — showing tool schemas only");
            eprintln!();
            // Build minimal tool list without skills (no working_dir resolution needed)
            let tools: Vec<Arc<dyn Tool>> = {
                use agent_tools::*;
                let mut t: Vec<Arc<dyn Tool>> = vec![
                    Arc::new(ShellTool::new()),
                    Arc::new(FileReadTool::new()),
                    Arc::new(FileWriteTool::new()),
                    Arc::new(FileEditTool::new()),
                    Arc::new(GlobTool::new()),
                    Arc::new(GrepTool::new()),
                    Arc::new(WebFetchTool::new()),
                ];
                if let Some(k) = env.get("BRAVE_API_KEY") {
                    if !k.is_empty() {
                        t.push(Arc::new(WebSearchTool::new(Box::new(
                            BraveSearchProvider::new(k.to_string()),
                        ))));
                    }
                }
                t
            };
            use agent_core::Instructions;
            let instructions =
                Instructions::Static("(core prompt omitted — no provider configured)".to_string());
            dump_prompt(&instructions, &tools);
            process::exit(0);
        }
        Err(e) => {
            eprintln!("error: {}", e);
            if matches!(e, assembly::AssemblyError::NoProvider(_)) {
                eprintln!();
                eprintln!("Set at least one of: OPENAI_API_KEY, ANTHROPIC_API_KEY, or OLLAMA_HOST");
            }
            process::exit(1);
        }
    };

    // Handle --dump-prompt: print everything and exit
    if cli.dump_prompt {
        println!("Model: {}", assembled.model);
        println!();
        dump_prompt(&assembled.instructions, &assembled.tools);
        process::exit(0);
    }

    // Handle --serve: start AG-UI HTTP server and skip TUI/REPL
    if let Some(ref addr_opt) = cli.serve {
        let bind_addr = match serve::parse_serve_addr(addr_opt.as_deref()) {
            Ok(addr) => addr,
            Err(e) => {
                eprintln!("error: {}", e);
                process::exit(1);
            }
        };
        if let Err(e) = serve::start_server(bind_addr, assembled.agent, assembled.config).await {
            eprintln!("error: {}", e);
            process::exit(1);
        }
        return;
    }

    // Dispatch to single-prompt or REPL mode
    match cli.prompt {
        Some(prompt_text) => {
            // Single-prompt mode: run, print, exit
            match run_single_prompt(&assembled, &prompt_text, &session).await {
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
                assembled.provider,
                &assembled.model,
                assembled.tools,
                assembled.instructions,
                permission_mode,
                assembled
                    .task_store
                    .unwrap_or_else(|| Arc::new(InMemoryTaskStore::new())),
                session.store.clone(),
                session.id.clone(),
                session.initial_history,
            )
            .await
            {
                eprintln!("{}", e);
                process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Strategy for valid profile names: alphanumeric + hyphens + underscores, non-empty.
    fn valid_profile_name() -> impl Strategy<Value = String> {
        "[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}".prop_map(|s| s)
    }

    proptest! {
        /// **Validates: Requirements 3.1**
        ///
        /// Property 15: CLI --profile parsing round-trip
        /// For any valid profile name string, parsing CLI args ["--profile", name]
        /// SHALL produce CliArgs with profile == Some(name).
        #[test]
        fn prop_profile_flag_roundtrip(name in valid_profile_name()) {
            let args = vec!["--profile".to_string(), name.clone()];
            let result = parse_args_from(&args).unwrap();
            prop_assert_eq!(result.profile, Some(name));
            // Other fields should be their defaults
            prop_assert_eq!(result.model, None);
            prop_assert_eq!(result.prompt, None);
            prop_assert!(!result.dump_prompt);
            prop_assert!(!result.skip_permissions);
        }
    }

    /// Verify that `--profile` without a following value produces an error.
    #[test]
    fn test_profile_flag_missing_value() {
        let args = vec!["--profile".to_string()];
        let result = parse_args_from(&args);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "--profile requires a value");
    }

    #[test]
    fn test_serve_flag_no_value() {
        let args = vec!["--serve".to_string()];
        let result = parse_args_from(&args).unwrap();
        assert_eq!(result.serve, Some(None));
        assert_eq!(result.prompt, None);
    }

    #[test]
    fn test_serve_flag_port_only() {
        let args = vec!["--serve".to_string(), "9000".to_string()];
        let result = parse_args_from(&args).unwrap();
        assert_eq!(result.serve, Some(Some("9000".to_string())));
    }

    #[test]
    fn test_serve_flag_host_port() {
        let args = vec!["--serve".to_string(), "0.0.0.0:9000".to_string()];
        let result = parse_args_from(&args).unwrap();
        assert_eq!(result.serve, Some(Some("0.0.0.0:9000".to_string())));
    }

    #[test]
    fn test_serve_flag_followed_by_another_flag() {
        // --serve followed by --model should treat --serve as no-value
        let args = vec![
            "--serve".to_string(),
            "--model".to_string(),
            "openai:gpt-4".to_string(),
        ];
        let result = parse_args_from(&args).unwrap();
        assert_eq!(result.serve, Some(None));
        assert_eq!(result.model, Some("openai:gpt-4".to_string()));
    }

    #[test]
    fn test_serve_flag_at_end() {
        // --serve at the end with no following arg
        let args = vec![
            "--model".to_string(),
            "x".to_string(),
            "--serve".to_string(),
        ];
        let result = parse_args_from(&args).unwrap();
        assert_eq!(result.serve, Some(None));
        assert_eq!(result.model, Some("x".to_string()));
    }
}
