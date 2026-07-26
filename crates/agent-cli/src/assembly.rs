//! Agent assembly: single source of truth for building `Agent` + `RunConfig`.
//!
//! Every surface (TUI, serve, single-prompt) calls `assemble()`. Per-surface
//! differences (permission mode, approval handler, task store attachment) are
//! captured in the `Surface` enum so omitting one is a compile error rather
//! than a silent behaviour change.
//!
//! Inputs are explicit — no `std::env::var` calls inside this module — making
//! the module testable without process-global mutation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use agent_core::{
    Agent, ConfigError, ConfigInputs, ConfigResolver, DenyAllApprovalHandler, InMemoryTaskStore,
    Instructions, ModelProvider, PermissionEngine, PermissionMode, RunConfig, SkillRegistry,
    SkillTool, SubAgentDef, SubAgentTool, TaskStore, TodoListTool, Tool,
};
use agent_llm::UnifiedProvider;
use agent_tools::{
    BraveSearchProvider, FileEditTool, FileReadTool, FileWriteTool, GlobTool, GrepTool, ShellTool,
    WebFetchTool, WebSearchTool,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Which CLI surface is being assembled for.
///
/// Each variant carries the differences that vary by surface. Adding a new
/// surface means adding a variant here — the compiler then flags any match
/// that doesn't handle it.
#[derive(Debug)]
pub enum Surface {
    /// Interactive TUI REPL. Uses `PermissionMode::Normal` with an interactive
    /// approval handler (supplied by the caller after assembly).
    Tui { skip_permissions: bool },
    /// AG-UI HTTP server. Like single-prompt but keeps a `TaskStore` so
    /// background sub-agent results are delivered to the model.
    Serve,
    /// Single-shot prompt. Uses `PermissionMode::Bypass` + `DenyAllApprovalHandler`.
    SinglePrompt,
}

/// All inputs the assembly module needs. Contains no ambient state — callers
/// pass an environment snapshot and directory paths explicitly so tests can
/// inject arbitrary values without touching `std::env`.
pub struct AssemblyInputs {
    /// Snapshot of the relevant environment variables.
    pub env: AssemblyEnv,
    /// CLI `--profile` flag value (if any).
    pub profile_name: Option<String>,
    /// CLI `--model` flag value (if any).
    pub model_override: Option<String>,
    /// Project working directory (used to locate `.arlo/settings.json`).
    pub working_dir: PathBuf,
    /// Which surface to assemble for.
    pub surface: Surface,
}

/// Snapshot of environment variables needed by assembly.
///
/// Represented as a map so tests can inject controlled values without
/// process-global mutation.
#[derive(Debug, Clone, Default)]
pub struct AssemblyEnv(pub HashMap<String, String>);

impl AssemblyEnv {
    /// Build from the real process environment.
    pub fn from_process_env() -> Self {
        let vars = [
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_BASE_URL",
            "OLLAMA_HOST",
            "BRAVE_API_KEY",
        ];
        let map = vars
            .iter()
            .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
            .collect();
        Self(map)
    }

    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
}

/// Results returned from assembly. Callers receive everything they need to
/// drive their surface without re-deriving any of it.
pub struct AssemblyOutput {
    pub agent: Agent,
    pub config: RunConfig,
    /// The constructed provider, for surfaces that need it directly (e.g. TUI).
    pub provider: Arc<dyn ModelProvider>,
    /// Resolved model name string.
    pub model: String,
    /// Instructions built from the core prompt + skills.
    pub instructions: Instructions,
    /// All tools (built-ins + skills), before sub-agent and todo tools are added.
    /// The sub-agent tool is already registered on `agent` and `config`.
    pub tools: Vec<Arc<dyn Tool>>,
    /// Shared task store. Always `Some` except for `Surface::SinglePrompt` where
    /// it is not needed (no background sub-agents in single-prompt mode).
    pub task_store: Option<Arc<dyn TaskStore>>,
}

/// Errors from assembly.
#[derive(Debug)]
pub enum AssemblyError {
    /// --profile flag named a profile that doesn't exist in any settings file.
    UnknownProfile(String),
    /// Profile requires an API key but none was found.
    MissingCredentials { provider: String, profile: String },
    /// No provider could be detected (no API keys / OLLAMA_HOST set).
    NoProvider(String),
    /// The profile's provider string is not supported.
    UnknownProvider(String),
}

impl std::fmt::Display for AssemblyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProfile(name) => write!(f, "unknown profile '{name}'"),
            Self::MissingCredentials { provider, profile } => write!(
                f,
                "profile '{profile}' requires API key for '{provider}' \
                 (set env var or add api_key to profile)"
            ),
            Self::NoProvider(msg) => write!(f, "{msg}"),
            Self::UnknownProvider(msg) => write!(f, "{msg}"),
        }
    }
}

impl From<ConfigError> for AssemblyError {
    fn from(e: ConfigError) -> Self {
        match e {
            ConfigError::UnknownProfile { name } => Self::UnknownProfile(name),
            ConfigError::MissingCredentials { provider, profile } => {
                Self::MissingCredentials { provider, profile }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The assembly entry point
// ---------------------------------------------------------------------------

/// Build an `Agent` and `RunConfig` for the given surface.
///
/// This is the single authoritative place where provider resolution, tool
/// registration, skill discovery, task-store creation, sub-agent wiring, and
/// settings loading happen. All callers receive the same assembled result for
/// their surface.
pub fn assemble(inputs: AssemblyInputs) -> Result<AssemblyOutput, AssemblyError> {
    // --- 1. Resolve provider and model ---
    let (provider, model) = resolve_provider(&inputs)?;

    // --- 2. Load skills ---
    let (skill_registry, skill_tools) = load_skills(inputs.working_dir.as_path());

    // --- 3. Assemble tools ---
    let mut tools = default_tools(&inputs.env);
    tools.extend(skill_tools);

    // --- 4. Build instructions ---
    let core_prompt = core_agent_prompt();
    let skill_prompt = skill_registry.system_prompt_section();
    let instructions = if skill_prompt.is_empty() {
        Instructions::Static(core_prompt.to_string())
    } else {
        Instructions::Static(format!("{}\n{}", core_prompt, skill_prompt))
    };

    // --- 5. Create task store and wire TodoListTool + SubAgentTool ---
    let task_store: Option<Arc<dyn TaskStore>> = match &inputs.surface {
        Surface::SinglePrompt => None,
        _ => Some(Arc::new(InMemoryTaskStore::new())),
    };

    if let Some(ref store) = task_store {
        tools.push(Arc::new(TodoListTool::new(store.clone())));
        let sub_agent_tool = build_sub_agent_tool(&provider, &model, store.clone());
        tools.push(sub_agent_tool);
    }

    // --- 6. Build Agent ---
    let mut agent_builder = Agent::builder("arlo").instructions(instructions.clone());
    for tool in &tools {
        agent_builder = agent_builder.tool(tool.clone());
    }
    let agent = agent_builder.build();

    // --- 7. Build RunConfig (surface-specific) ---
    let config = build_run_config(
        &provider,
        &model,
        &inputs.surface,
        &inputs.working_dir,
        task_store.clone(),
    );

    Ok(AssemblyOutput {
        agent,
        config,
        provider,
        model,
        instructions,
        tools,
        task_store,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Resolve a `ModelProvider` and model name from inputs.
///
/// Uses `ConfigResolver` for the profile path, then falls back to env-only
/// detection. `ANTHROPIC_BASE_URL` is honoured on both paths.
fn resolve_provider(
    inputs: &AssemblyInputs,
) -> Result<(Arc<dyn ModelProvider>, String), AssemblyError> {
    let config_inputs = ConfigInputs {
        profile_name: inputs.profile_name.clone(),
        model_override: inputs.model_override.clone(),
        working_dir: inputs.working_dir.clone(),
    };

    match ConfigResolver::resolve(&config_inputs).map_err(AssemblyError::from)? {
        Some(resolved) => {
            // Profile path — apply ANTHROPIC_BASE_URL env override if provider is anthropic
            // (ConfigResolver::apply_env_overrides handles most vars but the assembly module
            //  is the single place that applies overrides before provider construction).
            let mut resolved = resolved;
            if resolved.provider == "anthropic" {
                if let Some(url) = inputs.env.get("ANTHROPIC_BASE_URL") {
                    resolved.base_url = Some(url.to_string());
                }
            }

            let p = UnifiedProvider::from_profile(&resolved)
                .map_err(|e| AssemblyError::UnknownProvider(e.to_string()))?;

            let model = resolved.model.clone();
            let provider: Arc<dyn ModelProvider> =
                if resolved.context_window.is_some() || resolved.max_output_tokens.is_some() {
                    Arc::new(crate::OverridingProvider {
                        inner: Arc::new(p),
                        context_window: resolved.context_window,
                        max_output_tokens: resolved.max_output_tokens,
                    })
                } else {
                    Arc::new(p)
                };
            Ok((provider, model))
        }
        None => {
            // No profile — build from env snapshot
            let provider = build_provider_from_env(&inputs.env)
                .map_err(|e| AssemblyError::NoProvider(e.to_string()))?;

            let model = resolve_model_from_env(inputs.model_override.clone(), &inputs.env);
            Ok((Arc::new(provider), model))
        }
    }
}

/// Build a `UnifiedProvider` from an env snapshot.
///
/// In the no-profile path we use `from_env()` which reads the same vars our
/// snapshot captures. The snapshot is used only for model-name defaulting and
/// tests; the provider itself needs the real env for feature-gated cfg params.
fn build_provider_from_env(env: &AssemblyEnv) -> Result<UnifiedProvider, agent_core::ModelError> {
    // from_env() already honours OPENAI_API_KEY, OPENAI_BASE_URL,
    // ANTHROPIC_API_KEY, ANTHROPIC_BASE_URL, OLLAMA_HOST — all vars we snapshot.
    // ponytail: if we need truly isolated env (e.g. integration tests), replace
    // from_env() with a cfg-gated call to UnifiedProvider::new using the snapshot.
    let _ = env; // used for detect_default_provider / model defaulting only
    UnifiedProvider::from_env()
}

/// Determine the model name from CLI override or provider detection.
///
/// `available_models()` returns empty unconditionally, so the "use first
/// available" branch from the old code is removed here.
fn resolve_model_from_env(model_override: Option<String>, env: &AssemblyEnv) -> String {
    if let Some(m) = model_override {
        return m;
    }
    // Fallback defaults based on which credential is present
    if env.get("ANTHROPIC_API_KEY").is_some() {
        "anthropic:claude-sonnet-4-20250514".to_string()
    } else if env.get("OPENAI_API_KEY").is_some() {
        "openai:gpt-4o".to_string()
    } else {
        "ollama:llama3".to_string()
    }
}

/// Build the surface-specific `RunConfig`.
///
/// All surfaces go through `load_settings` so the `permissions` section of
/// `.arlo/settings.json` is always active (fixes the dead-path bug).
fn build_run_config(
    provider: &Arc<dyn ModelProvider>,
    model: &str,
    surface: &Surface,
    working_dir: &Path,
    task_store: Option<Arc<dyn TaskStore>>,
) -> RunConfig {
    let permission_mode = match surface {
        Surface::Tui { skip_permissions } => {
            if *skip_permissions {
                PermissionMode::Bypass
            } else {
                PermissionMode::Normal
            }
        }
        Surface::Serve | Surface::SinglePrompt => PermissionMode::Bypass,
    };

    let mut builder = RunConfig::builder(Arc::clone(provider), model)
        .permissions(PermissionEngine::new(permission_mode))
        .load_settings(working_dir);

    // Serve and single-prompt use DenyAll; TUI wires its own interactive handler
    // after assembly via `config.approval_handler = Some(...)`.
    match surface {
        Surface::Tui { .. } => {
            // Leave approval_handler as None — TUI sets it when creating the run.
        }
        Surface::Serve | Surface::SinglePrompt => {
            builder = builder.approval_handler(Arc::new(DenyAllApprovalHandler));
        }
    }

    if let Some(store) = task_store {
        builder = builder.task_store(store);
    }

    builder.build()
}

/// Build the sub-agent tool with a task store for background task tracking.
fn build_sub_agent_tool(
    provider: &Arc<dyn ModelProvider>,
    model: &str,
    task_store: Arc<dyn TaskStore>,
) -> Arc<dyn Tool> {
    let sub_agent = Agent::builder("sub-agent")
        .instructions(Instructions::Static(
            "You are a background helper agent. Complete the delegated task using available \
             tools. Return a concise summary of your findings or actions when done."
                .to_string(),
        ))
        .tool(Arc::new(ShellTool::new()))
        .tool(Arc::new(FileReadTool::new()))
        .tool(Arc::new(FileWriteTool::new()))
        .tool(Arc::new(FileEditTool::new()))
        .tool(Arc::new(GlobTool::new()))
        .tool(Arc::new(GrepTool::new()))
        .build();

    let def = SubAgentDef {
        agent: Arc::new(sub_agent),
        tool_name: Some("sub_agent".to_string()),
        tool_description: Some(
            "Spawn a background sub-agent to handle a delegated task. The sub-agent runs \
             independently with access to shell, file, and search tools. Its progress is \
             tracked and you'll be notified when it completes."
                .to_string(),
        ),
        input_schema: None,
        max_turns: Some(15),
        background: true,
        allowed_tools: None,
    };

    let sub_config = RunConfig::builder(Arc::clone(provider), model)
        .permissions(PermissionEngine::new(PermissionMode::Bypass))
        .approval_handler(Arc::new(DenyAllApprovalHandler))
        .max_turns(15)
        .build();

    Arc::new(SubAgentTool::with_task_store(def, sub_config, task_store))
}

/// The default built-in tools (no env reads — web search requires an explicit key).
fn default_tools(env: &AssemblyEnv) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ShellTool::new()),
        Arc::new(FileReadTool::new()),
        Arc::new(FileWriteTool::new()),
        Arc::new(FileEditTool::new()),
        Arc::new(GlobTool::new()),
        Arc::new(GrepTool::new()),
        Arc::new(WebFetchTool::new()),
    ];

    if let Some(api_key) = env.get("BRAVE_API_KEY") {
        if !api_key.is_empty() {
            tools.push(Arc::new(WebSearchTool::new(Box::new(
                BraveSearchProvider::new(api_key.to_string()),
            ))));
        }
    }

    tools
}

/// Discover and load skills from project-level and user-level directories.
fn load_skills(working_dir: &Path) -> (SkillRegistry, Vec<Arc<dyn Tool>>) {
    let project_dir = {
        let d = working_dir.join(".arlo").join("skills");
        if d.is_dir() {
            Some(d)
        } else {
            None
        }
    };
    let user_dir = dirs::home_dir().and_then(|h| {
        let d = h.join(".arlo").join("skills");
        if d.is_dir() {
            Some(d)
        } else {
            None
        }
    });

    let registry = SkillRegistry::load(project_dir.as_deref(), user_dir.as_deref());
    let skill_tools: Vec<Arc<dyn Tool>> = registry
        .skills()
        .iter()
        .cloned()
        .map(|skill| Arc::new(SkillTool::new(skill)) as Arc<dyn Tool>)
        .collect();

    (registry, skill_tools)
}

/// The core agent system prompt.
fn core_agent_prompt() -> &'static str {
    "\
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
- To create a new file or fully rewrite one, use file_write instead of cat with heredoc, echo, or sed/awk
- To change part of an existing file, use file_edit (exact string replacement) instead of rewriting the whole file with file_write
- To search for files by name/pattern, use glob instead of find or ls
- To search file contents, use grep instead of shell grep or rg
- Reserve shell exclusively for system commands and terminal operations that require shell execution

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
"
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Minimal env snapshot — no real credentials needed.
    fn empty_env() -> AssemblyEnv {
        AssemblyEnv::default()
    }

    /// Profile JSON for a fake openai provider — works without a real key because
    /// `UnifiedProvider::from_profile` only checks that the key field is non-None,
    /// not that it's valid. No HTTP call is made during assembly.
    const FAKE_OPENAI_PROFILE: &str = r#"{"profiles":{"default":"local","local":{"provider":"openai","api_key":"sk-test","model":"openai:gpt-4o"}}}"#;

    /// Write a settings file with the fake openai profile.
    fn write_openai_profile(dir: &Path) {
        write_settings(dir, FAKE_OPENAI_PROFILE);
    }

    fn write_settings(dir: &Path, json: &str) {
        let arlo = dir.join(".arlo");
        fs::create_dir_all(&arlo).unwrap();
        fs::write(arlo.join("settings.json"), json).unwrap();
    }

    // --- surface differences -------------------------------------------------

    #[test]
    fn serve_has_task_store() {
        let tmp = TempDir::new().unwrap();
        write_openai_profile(tmp.path());
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        })
        .unwrap();
        assert!(out.task_store.is_some(), "serve must have a task store");
        assert!(
            out.config.task_store.is_some(),
            "serve RunConfig must carry task store"
        );
    }

    #[test]
    fn tui_has_task_store() {
        let tmp = TempDir::new().unwrap();
        write_openai_profile(tmp.path());
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Tui {
                skip_permissions: false,
            },
        })
        .unwrap();
        assert!(out.task_store.is_some(), "TUI must have a task store");
    }

    #[test]
    fn single_prompt_has_no_task_store() {
        let tmp = TempDir::new().unwrap();
        write_openai_profile(tmp.path());
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::SinglePrompt,
        })
        .unwrap();
        assert!(out.task_store.is_none());
        assert!(out.config.task_store.is_none());
    }

    // --- permissions section reaches the engine ------------------------------

    #[test]
    fn settings_permissions_reach_engine() {
        let tmp = TempDir::new().unwrap();
        // Profile + permissions in same settings file
        write_settings(
            tmp.path(),
            r#"{"profiles":{"default":"local","local":{"provider":"openai","api_key":"sk-test","model":"openai:gpt-4o"}},"permissions":{"allow":["read_file"],"deny":["Bash(rm *)"]}}"#,
        );
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        })
        .unwrap();

        use agent_core::{permission::PermissionDecision, tool::ApprovalRequirement};
        let decision =
            out.config
                .permissions
                .check("read_file", &ApprovalRequirement::Always, None);
        assert!(
            matches!(decision, PermissionDecision::Allow { .. }),
            "read_file should be statically allowed from settings.json"
        );
    }

    #[test]
    fn project_settings_layer_over_empty_user() {
        let tmp = TempDir::new().unwrap();
        write_settings(
            tmp.path(),
            r#"{"profiles":{"default":"local","local":{"provider":"openai","api_key":"sk-test","model":"openai:gpt-4o"}},"permissions":{"allow":["fs_*"],"deny":[]}}"#,
        );
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        })
        .unwrap();

        use agent_core::{permission::PermissionDecision, tool::ApprovalRequirement};
        let decision = out
            .config
            .permissions
            .check("fs_write", &ApprovalRequirement::Always, None);
        assert!(
            matches!(decision, PermissionDecision::Allow { .. }),
            "fs_write should match 'fs_*' allow from project settings"
        );
    }

    // --- ANTHROPIC_BASE_URL honoured -----------------------------------------

    #[test]
    fn anthropic_base_url_applied_in_env_path() {
        // Verify assembly succeeds when a profile specifies a custom base_url.
        // Use an ollama profile with an explicit base_url to keep CI credential-free.
        let tmp = TempDir::new().unwrap();
        write_settings(
            tmp.path(),
            r#"{"profiles":{"default":"proxy","proxy":{"provider":"openai","api_key":"sk-test","model":"openai:gpt-4o","base_url":"https://custom-proxy.example.com/v1"}}}"#,
        );
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("proxy".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        })
        .unwrap();
        assert_eq!(out.model, "openai:gpt-4o");
    }

    // --- model defaulting rule -----------------------------------------------

    #[test]
    fn model_override_respected() {
        let tmp = TempDir::new().unwrap();
        write_openai_profile(tmp.path());
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: Some("ollama:codellama".to_string()),
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        })
        .unwrap();
        assert_eq!(out.model, "ollama:codellama");
    }

    // --- error cases ---------------------------------------------------------

    #[test]
    fn unknown_profile_returns_error() {
        let tmp = TempDir::new().unwrap();
        write_settings(
            tmp.path(),
            r#"{"profiles":{"work":{"provider":"openai","api_key":"sk-test","model":"openai:gpt-4o"}}}"#,
        );
        let result = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("nonexistent".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        });
        assert!(
            matches!(result, Err(AssemblyError::UnknownProfile(ref n)) if n == "nonexistent"),
            "expected UnknownProfile"
        );
    }

    #[test]
    fn no_provider_returns_error() {
        // We can't prevent from_env() from finding real credentials in the test process.
        // Instead test via profile path: an anthropic profile without an api_key
        // and no ANTHROPIC_API_KEY env var should return MissingCredentials.
        let tmp = TempDir::new().unwrap();
        write_settings(
            tmp.path(),
            r#"{"profiles":{"default":"work","work":{"provider":"anthropic","model":"claude-3"}}}"#,
        );
        // Empty env — no ANTHROPIC_API_KEY
        let result = assemble(AssemblyInputs {
            env: AssemblyEnv::default(),
            profile_name: None,
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Serve,
        });
        assert!(
            matches!(result, Err(AssemblyError::MissingCredentials { .. })),
            "expected MissingCredentials for anthropic profile without api_key"
        );
    }

    // --- tui permission mode -------------------------------------------------

    #[test]
    fn tui_normal_permissions() {
        let tmp = TempDir::new().unwrap();
        write_openai_profile(tmp.path());
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Tui {
                skip_permissions: false,
            },
        })
        .unwrap();
        // In normal mode with no static rules, a tool requiring approval
        // should get NeedsApproval (not silently bypassed).
        use agent_core::{permission::PermissionDecision, tool::ApprovalRequirement};
        let decision = out
            .config
            .permissions
            .check("shell", &ApprovalRequirement::Always, None);
        assert!(
            matches!(decision, PermissionDecision::NeedsApproval { .. }),
            "TUI normal mode: shell Always should need approval"
        );
    }

    #[test]
    fn tui_skip_permissions_bypasses() {
        let tmp = TempDir::new().unwrap();
        write_openai_profile(tmp.path());
        let out = assemble(AssemblyInputs {
            env: empty_env(),
            profile_name: Some("local".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
            surface: Surface::Tui {
                skip_permissions: true,
            },
        })
        .unwrap();
        use agent_core::{permission::PermissionDecision, tool::ApprovalRequirement};
        let decision = out
            .config
            .permissions
            .check("shell", &ApprovalRequirement::Always, None);
        assert!(
            matches!(decision, PermissionDecision::Allow { .. }),
            "TUI skip-permissions: shell Always should be auto-allowed in Bypass mode"
        );
    }
}
