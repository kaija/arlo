//! Skill system: registry, loading, and tool implementation.
//!
//! Skills are reusable prompt templates loaded from `SKILL.md` files with YAML
//! frontmatter. The `SkillRegistry` discovers skills from project-level and user-level
//! directories, and the `SkillTool` renders them with template variable substitution.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;

use crate::error::ToolError;
use crate::tool::{Concurrency, Tool, ToolContext, ToolOutput};

// ─── Types ───────────────────────────────────────────────────────────────────

/// How a skill is executed when invoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillContext {
    /// Inject the rendered body directly into the current conversation.
    Inline,
    /// Spawn an isolated sub-agent with the skill's allowed_tools.
    Fork { max_turns: Option<u32> },
}

/// Tracks where a skill was loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSource {
    /// Loaded from a project-level `.agent/skills/` directory.
    Project(PathBuf),
    /// Loaded from the user-level `~/.agent/skills/` directory.
    User(PathBuf),
}

/// A single argument accepted by a skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillArgument {
    /// The argument name.
    pub name: String,
    /// Human-readable description of the argument.
    pub description: String,
    /// Whether this argument is required.
    pub required: bool,
}

/// A loaded skill definition parsed from a SKILL.md file.
#[derive(Debug, Clone)]
pub struct Skill {
    /// The unique name of this skill.
    pub name: String,
    /// Human-readable description of what this skill does.
    pub description: String,
    /// Optional guidance on when the model should use this skill.
    pub when_to_use: Option<String>,
    /// Tools allowed when this skill runs in Fork context.
    pub allowed_tools: Vec<String>,
    /// Arguments this skill accepts.
    pub arguments: Vec<SkillArgument>,
    /// How this skill is executed (Inline or Fork).
    pub context: SkillContext,
    /// Optional hook configuration (placeholder for future use).
    pub hooks: Option<serde_json::Value>,
    /// The template body (content after frontmatter).
    pub body: String,
    /// The directory containing the SKILL.md file.
    pub base_dir: PathBuf,
    /// Where this skill was loaded from.
    pub source: SkillSource,
}

// ─── YAML Frontmatter Schema ─────────────────────────────────────────────────

/// Raw YAML frontmatter schema for deserialization.
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    when_to_use: Option<String>,
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    arguments: Option<Vec<RawSkillArgument>>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    max_turns: Option<u32>,
    #[serde(default)]
    hooks: Option<serde_json::Value>,
}

/// Raw argument from YAML frontmatter.
#[derive(Debug, Deserialize)]
struct RawSkillArgument {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_true")]
    required: bool,
}

fn default_true() -> bool {
    true
}

// ─── Parsing ─────────────────────────────────────────────────────────────────

/// Parse a SKILL.md file's content into frontmatter + body.
///
/// Frontmatter is delimited by `---` at the start of the file.
/// Returns `None` if frontmatter is missing or invalid.
fn parse_skill_file(content: &str, file_path: &Path, source: SkillSource) -> Option<Skill> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        tracing::warn!("Skill file {:?} missing frontmatter delimiter", file_path);
        return None;
    }

    // Find the closing ---
    let after_first = &trimmed[3..];
    let end_idx = after_first.find("\n---")?;
    let yaml_str = &after_first[..end_idx];
    let body_start = end_idx + 4; // skip "\n---"
    let body = after_first[body_start..].trim_start_matches('\n').to_string();

    let frontmatter: SkillFrontmatter = match serde_yaml::from_str(yaml_str) {
        Ok(fm) => fm,
        Err(e) => {
            tracing::warn!(
                "Skill file {:?} has invalid frontmatter: {}",
                file_path,
                e
            );
            return None;
        }
    };

    let context = match frontmatter.context.as_deref() {
        Some("fork") | Some("Fork") => SkillContext::Fork {
            max_turns: frontmatter.max_turns,
        },
        _ => SkillContext::Inline,
    };

    let arguments = frontmatter
        .arguments
        .unwrap_or_default()
        .into_iter()
        .map(|a| SkillArgument {
            name: a.name,
            description: a.description.unwrap_or_default(),
            required: a.required,
        })
        .collect();

    let base_dir = file_path.parent().unwrap_or(Path::new(".")).to_path_buf();

    Some(Skill {
        name: frontmatter.name,
        description: frontmatter.description.unwrap_or_default(),
        when_to_use: frontmatter.when_to_use,
        allowed_tools: frontmatter.allowed_tools.unwrap_or_default(),
        arguments,
        context,
        hooks: frontmatter.hooks,
        body,
        base_dir,
        source,
    })
}

// ─── SkillRegistry ───────────────────────────────────────────────────────────

/// Registry of loaded skills, providing lookup and system prompt generation.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Load skills from project-level and user-level directories.
    ///
    /// Project-level skills (from `.agent/skills/`) take precedence over
    /// user-level skills (`~/.agent/skills/`) when names conflict.
    pub fn load(project_dir: Option<&Path>, user_dir: Option<&Path>) -> Self {
        let mut registry = SkillRegistry { skills: Vec::new() };

        // Load user-level first (lower precedence)
        if let Some(dir) = user_dir {
            registry.load_from_directory(dir, |path| SkillSource::User(path.to_path_buf()));
        }

        // Load project-level (higher precedence — overwrites user-level on conflict)
        if let Some(dir) = project_dir {
            registry.load_from_directory(dir, |path| SkillSource::Project(path.to_path_buf()));
        }

        registry
    }

    /// Load all SKILL.md files from a directory.
    fn load_from_directory(&mut self, dir: &Path, make_source: impl Fn(&Path) -> SkillSource) {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return, // Directory doesn't exist or isn't readable
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            // Accept files ending in .md within the skills directory
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if !file_name.to_lowercase().ends_with(".md") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to read skill file {:?}: {}", path, e);
                    continue;
                }
            };

            let source = make_source(&path);
            if let Some(skill) = parse_skill_file(&content, &path, source) {
                // Project-level takes precedence: remove any existing skill with same name
                self.skills.retain(|s| s.name != skill.name);
                self.skills.push(skill);
            }
        }
    }

    /// Find a skill by name.
    pub fn find(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Return all skills whose base_dir is an ancestor of the given path,
    /// or all skills if no path filtering is needed.
    pub fn activate_for_path(&self, path: &Path) -> Vec<&Skill> {
        self.skills
            .iter()
            .filter(|s| path.starts_with(&s.base_dir))
            .collect()
    }

    /// Generate a system prompt section listing available skills.
    pub fn system_prompt_section(&self) -> String {
        if self.skills.is_empty() {
            return String::new();
        }

        let mut section = String::from("## Available Skills\n\n");
        for skill in &self.skills {
            section.push_str(&format!("- **{}**", skill.name));
            if !skill.description.is_empty() {
                section.push_str(&format!(": {}", skill.description));
            }
            section.push('\n');
            if let Some(ref when) = skill.when_to_use {
                section.push_str(&format!("  When to use: {}\n", when));
            }
            if !skill.arguments.is_empty() {
                section.push_str("  Arguments: ");
                let args: Vec<String> = skill
                    .arguments
                    .iter()
                    .map(|a| {
                        if a.required {
                            format!("{} (required)", a.name)
                        } else {
                            format!("{} (optional)", a.name)
                        }
                    })
                    .collect();
                section.push_str(&args.join(", "));
                section.push('\n');
            }
        }
        section
    }

    /// Get all loaded skills.
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }
}

// ─── Template Variable Substitution ──────────────────────────────────────────

/// Render a skill body by substituting template variables.
///
/// Recognized variables:
/// - `$ARGUMENTS` — the full arguments string
/// - `$1`, `$2`, ... — positional arguments (space-split from arguments)
/// - `${SKILL_DIR}` — the skill's base directory path
///
/// Unresolved positional variables (e.g., `$3` when only 2 args provided)
/// are replaced with empty strings.
pub fn render_skill_body(body: &str, arguments: &str, skill_dir: &Path) -> String {
    let positional: Vec<&str> = if arguments.is_empty() {
        Vec::new()
    } else {
        arguments.split_whitespace().collect()
    };

    let skill_dir_str = skill_dir.to_string_lossy();

    let mut result = body.to_string();

    // Substitute ${SKILL_DIR} first (before $S could match something)
    result = result.replace("${SKILL_DIR}", &skill_dir_str);

    // Substitute $ARGUMENTS
    result = result.replace("$ARGUMENTS", arguments);

    // Substitute positional variables $1, $2, ... up to some reasonable limit
    // We need to handle higher numbers first to avoid $1 matching in $10
    let max_positional = 20.max(positional.len());
    for i in (1..=max_positional).rev() {
        let var = format!("${}", i);
        let value = positional.get(i - 1).copied().unwrap_or("");
        result = result.replace(&var, value);
    }

    result
}

// ─── SkillTool ───────────────────────────────────────────────────────────────

/// A tool that invokes a skill by rendering its template body.
///
/// For `Inline` context: returns the rendered body as `ToolOutput::Text`.
/// For `Fork` context: currently returns the rendered body (sub-agent spawning
/// will be wired in a future task).
pub struct SkillTool {
    /// The skill this tool wraps.
    skill: Skill,
}

impl SkillTool {
    /// Create a new SkillTool wrapping the given skill.
    pub fn new(skill: Skill) -> Self {
        Self { skill }
    }

    /// Get a reference to the underlying skill.
    pub fn skill(&self) -> &Skill {
        &self.skill
    }
}

impl std::fmt::Debug for SkillTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillTool")
            .field("name", &self.skill.name)
            .field("context", &self.skill.context)
            .finish()
    }
}

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        &self.skill.name
    }

    fn description(&self) -> &str {
        &self.skill.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "arguments": {
                    "type": "string",
                    "description": "Arguments to pass to the skill"
                }
            }
        })
    }

    fn concurrency(&self, _input: &serde_json::Value) -> Concurrency {
        Concurrency::Safe
    }

    async fn execute(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let arguments = input
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let rendered = render_skill_body(&self.skill.body, arguments, &self.skill.base_dir);

        match &self.skill.context {
            SkillContext::Inline => Ok(ToolOutput::Text(rendered)),
            SkillContext::Fork { .. } => {
                // For Fork context, in the full implementation this would spawn
                // a sub-agent with the skill's allowed_tools. For now, return
                // the rendered body (the sub-agent wiring comes in a later task).
                Ok(ToolOutput::Text(rendered))
            }
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::fs;
    use tempfile::tempdir;

    fn sample_skill_md() -> &'static str {
        concat!(
            "---\n",
            "name: code-review\n",
            "description: Review code for quality issues\n",
            "when_to_use: When the user asks for a code review\n",
            "allowed_tools:\n",
            "- read_file\n",
            "- grep\n",
            "arguments:\n",
            "- name: path\n",
            "  description: Path to the file to review\n",
            "  required: true\n",
            "- name: focus\n",
            "  description: What to focus on\n",
            "  required: false\n",
            "context: inline\n",
            "---\n",
            "Review the file at $1 focusing on $2.\n",
            "Full args: $ARGUMENTS\n",
            "Skill dir: ${SKILL_DIR}\n",
        )
    }

    fn fork_skill_md() -> &'static str {
        concat!(
            "---\n",
            "name: refactor\n",
            "description: Refactor code\n",
            "context: fork\n",
            "max_turns: 5\n",
            "allowed_tools:\n",
            "- read_file\n",
            "- write_file\n",
            "---\n",
            "Refactor the code with arguments: $ARGUMENTS\n",
        )
    }

    #[test]
    fn parse_valid_inline_skill() {
        let content = sample_skill_md();
        let path = Path::new("/project/.agent/skills/code-review.md");
        let source = SkillSource::Project(path.to_path_buf());
        let skill = parse_skill_file(content, path, source).unwrap();

        assert_eq!(skill.name, "code-review");
        assert_eq!(skill.description, "Review code for quality issues");
        assert_eq!(
            skill.when_to_use,
            Some("When the user asks for a code review".to_string())
        );
        assert_eq!(skill.allowed_tools, vec!["read_file", "grep"]);
        assert_eq!(skill.arguments.len(), 2);
        assert_eq!(skill.arguments[0].name, "path");
        assert!(skill.arguments[0].required);
        assert_eq!(skill.arguments[1].name, "focus");
        assert!(!skill.arguments[1].required);
        assert_eq!(skill.context, SkillContext::Inline);
        assert!(skill.body.contains("$1"));
    }

    #[test]
    fn parse_valid_fork_skill() {
        let content = fork_skill_md();
        let path = Path::new("/project/.agent/skills/refactor.md");
        let source = SkillSource::Project(path.to_path_buf());
        let skill = parse_skill_file(content, path, source).unwrap();

        assert_eq!(skill.name, "refactor");
        assert_eq!(
            skill.context,
            SkillContext::Fork {
                max_turns: Some(5)
            }
        );
        assert_eq!(skill.allowed_tools, vec!["read_file", "write_file"]);
    }

    #[test]
    fn parse_missing_frontmatter() {
        let content = "Just some text without frontmatter";
        let path = Path::new("/skills/bad.md");
        let source = SkillSource::User(path.to_path_buf());
        assert!(parse_skill_file(content, path, source).is_none());
    }

    #[test]
    fn parse_invalid_yaml() {
        let content = "---\ninvalid: [yaml: broken\n---\nbody";
        let path = Path::new("/skills/broken.md");
        let source = SkillSource::User(path.to_path_buf());
        assert!(parse_skill_file(content, path, source).is_none());
    }

    #[test]
    fn parse_missing_name_field() {
        let content = "---\ndescription: no name field\n---\nbody";
        let path = Path::new("/skills/noname.md");
        let source = SkillSource::User(path.to_path_buf());
        assert!(parse_skill_file(content, path, source).is_none());
    }

    #[test]
    fn render_body_all_variables() {
        let body = "File: $1\nFocus: $2\nAll: $ARGUMENTS\nDir: ${SKILL_DIR}";
        let result = render_skill_body(body, "src/main.rs performance", Path::new("/my/project"));
        assert_eq!(
            result,
            "File: src/main.rs\nFocus: performance\nAll: src/main.rs performance\nDir: /my/project"
        );
    }

    #[test]
    fn render_body_unresolved_positional() {
        let body = "Arg1: $1, Arg2: $2, Arg3: $3";
        let result = render_skill_body(body, "only-one", Path::new("/dir"));
        assert_eq!(result, "Arg1: only-one, Arg2: , Arg3: ");
    }

    #[test]
    fn render_body_empty_arguments() {
        let body = "Args: $ARGUMENTS, Pos: $1";
        let result = render_skill_body(body, "", Path::new("/dir"));
        assert_eq!(result, "Args: , Pos: ");
    }

    #[test]
    fn render_body_no_variables() {
        let body = "Static content with no variables.";
        let result = render_skill_body(body, "ignored", Path::new("/dir"));
        assert_eq!(result, "Static content with no variables.");
    }

    #[test]
    fn registry_load_from_directory() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path().join(".agent").join("skills");
        fs::create_dir_all(&skills_dir).unwrap();
        fs::write(skills_dir.join("review.md"), sample_skill_md()).unwrap();
        fs::write(skills_dir.join("refactor.md"), fork_skill_md()).unwrap();

        let registry = SkillRegistry::load(Some(&skills_dir), None);
        assert_eq!(registry.skills().len(), 2);
        assert!(registry.find("code-review").is_some());
        assert!(registry.find("refactor").is_some());
    }

    #[test]
    fn registry_project_takes_precedence() {
        let project_dir = tempdir().unwrap();
        let user_dir = tempdir().unwrap();

        let project_skills = project_dir.path();
        let user_skills = user_dir.path();

        // Both have a skill named "code-review" but with different descriptions
        let project_content = "---\nname: code-review\ndescription: Project version\n---\nProject body\n";
        let user_content = "---\nname: code-review\ndescription: User version\n---\nUser body\n";

        fs::write(project_skills.join("review.md"), project_content).unwrap();
        fs::write(user_skills.join("review.md"), user_content).unwrap();

        let registry = SkillRegistry::load(Some(project_skills), Some(user_skills));
        let skill = registry.find("code-review").unwrap();
        assert_eq!(skill.description, "Project version");
    }

    #[test]
    fn registry_skips_invalid_files() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path();

        // Valid skill
        fs::write(skills_dir.join("good.md"), sample_skill_md()).unwrap();
        // Invalid skill (no name)
        fs::write(
            skills_dir.join("bad.md"),
            "---\ndescription: missing name\n---\nbody",
        )
        .unwrap();
        // Not a .md file
        fs::write(skills_dir.join("readme.txt"), "not a skill").unwrap();

        let registry = SkillRegistry::load(Some(skills_dir), None);
        assert_eq!(registry.skills().len(), 1);
        assert_eq!(registry.skills()[0].name, "code-review");
    }

    #[test]
    fn registry_nonexistent_directory() {
        let registry = SkillRegistry::load(
            Some(Path::new("/nonexistent/path")),
            Some(Path::new("/also/nonexistent")),
        );
        assert!(registry.skills().is_empty());
    }

    #[test]
    fn registry_find_nonexistent() {
        let registry = SkillRegistry::default();
        assert!(registry.find("nonexistent").is_none());
    }

    #[test]
    fn registry_activate_for_path() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path();
        fs::write(skills_dir.join("review.md"), sample_skill_md()).unwrap();

        let registry = SkillRegistry::load(Some(skills_dir), None);
        // Path within the skills_dir should match
        let activated = registry.activate_for_path(skills_dir);
        assert_eq!(activated.len(), 1);

        // Path outside should not match
        let outside = registry.activate_for_path(Path::new("/completely/different"));
        assert!(outside.is_empty());
    }

    #[test]
    fn registry_system_prompt_section_empty() {
        let registry = SkillRegistry::default();
        assert_eq!(registry.system_prompt_section(), "");
    }

    #[test]
    fn registry_system_prompt_section_with_skills() {
        let dir = tempdir().unwrap();
        let skills_dir = dir.path();
        fs::write(skills_dir.join("review.md"), sample_skill_md()).unwrap();

        let registry = SkillRegistry::load(Some(skills_dir), None);
        let section = registry.system_prompt_section();
        assert!(section.contains("## Available Skills"));
        assert!(section.contains("code-review"));
        assert!(section.contains("Review code for quality issues"));
        assert!(section.contains("When to use:"));
        assert!(section.contains("path (required)"));
        assert!(section.contains("focus (optional)"));
    }

    #[tokio::test]
    async fn skill_tool_inline_execution() {
        let skill = Skill {
            name: "test-skill".to_string(),
            description: "A test skill".to_string(),
            when_to_use: None,
            allowed_tools: vec![],
            arguments: vec![],
            context: SkillContext::Inline,
            hooks: None,
            body: "Hello $1, you said: $ARGUMENTS".to_string(),
            base_dir: PathBuf::from("/test"),
            source: SkillSource::Project(PathBuf::from("/test/skill.md")),
        };

        let tool = SkillTool::new(skill);
        assert_eq!(tool.name(), "test-skill");
        assert_eq!(tool.description(), "A test skill");

        let ctx = ToolContext {
            session_id: "sess-1".to_string(),
            working_dir: PathBuf::from("/tmp"),
        };

        let result = tool
            .execute(serde_json::json!({"arguments": "world extra"}), &ctx)
            .await
            .unwrap();

        assert_eq!(
            result,
            ToolOutput::Text("Hello world, you said: world extra".to_string())
        );
    }

    #[tokio::test]
    async fn skill_tool_fork_execution() {
        let skill = Skill {
            name: "fork-skill".to_string(),
            description: "A fork skill".to_string(),
            when_to_use: None,
            allowed_tools: vec!["shell".to_string()],
            arguments: vec![],
            context: SkillContext::Fork { max_turns: Some(3) },
            hooks: None,
            body: "Do the thing with $ARGUMENTS".to_string(),
            base_dir: PathBuf::from("/project"),
            source: SkillSource::Project(PathBuf::from("/project/skill.md")),
        };

        let tool = SkillTool::new(skill);
        let ctx = ToolContext {
            session_id: "sess-2".to_string(),
            working_dir: PathBuf::from("/tmp"),
        };

        let result = tool
            .execute(serde_json::json!({"arguments": "refactor main.rs"}), &ctx)
            .await
            .unwrap();

        // Fork currently returns rendered body (sub-agent wiring is future work)
        assert_eq!(
            result,
            ToolOutput::Text("Do the thing with refactor main.rs".to_string())
        );
    }

    #[tokio::test]
    async fn skill_tool_no_arguments() {
        let skill = Skill {
            name: "no-args".to_string(),
            description: "Skill without args".to_string(),
            when_to_use: None,
            allowed_tools: vec![],
            arguments: vec![],
            context: SkillContext::Inline,
            hooks: None,
            body: "Static: $1 and $2".to_string(),
            base_dir: PathBuf::from("/dir"),
            source: SkillSource::User(PathBuf::from("/dir/skill.md")),
        };

        let tool = SkillTool::new(skill);
        let ctx = ToolContext {
            session_id: "sess-3".to_string(),
            working_dir: PathBuf::from("/tmp"),
        };

        // No arguments field in input
        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert_eq!(result, ToolOutput::Text("Static:  and ".to_string()));
    }

    #[test]
    fn skill_context_enum_equality() {
        assert_eq!(SkillContext::Inline, SkillContext::Inline);
        assert_eq!(
            SkillContext::Fork { max_turns: Some(5) },
            SkillContext::Fork { max_turns: Some(5) }
        );
        assert_ne!(SkillContext::Inline, SkillContext::Fork { max_turns: None });
    }

    #[test]
    fn skill_source_enum_equality() {
        let p1 = SkillSource::Project(PathBuf::from("/a"));
        let p2 = SkillSource::Project(PathBuf::from("/a"));
        let u1 = SkillSource::User(PathBuf::from("/b"));
        assert_eq!(p1, p2);
        assert_ne!(p1, u1);
    }

    /// **Validates: Requirements 20.4**
    mod prop_tests {
        use super::*;

        /// Generate a valid skill name: 1-30 lowercase alphanumeric chars with hyphens,
        /// starting and ending with alphanumeric.
        fn skill_name_strategy() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9\\-]{0,28}[a-z0-9]"
        }

        /// Generate a non-empty description string that survives YAML serialization
        /// unchanged. Uses only alphanumeric chars with no leading/trailing whitespace.
        fn description_strategy() -> impl Strategy<Value = String> {
            "[A-Za-z][A-Za-z0-9]{0,29}"
        }

        proptest! {
            /// Property 24: Skill registry precedence
            ///
            /// When skills with the same name exist in both project-level and user-level
            /// directories, find() returns the project-level skill (higher precedence).
            #[test]
            fn skill_registry_project_takes_precedence_over_user(
                skill_name in skill_name_strategy(),
                project_desc in description_strategy(),
                user_desc in description_strategy(),
            ) {
                // Ensure the two descriptions are different so we can distinguish them
                prop_assume!(project_desc != user_desc);

                let project_dir = tempdir().unwrap();
                let user_dir = tempdir().unwrap();

                let project_skills_path = project_dir.path();
                let user_skills_path = user_dir.path();

                // Write SKILL.md with the same name but different descriptions
                let project_content = format!(
                    "---\nname: {}\ndescription: {}\n---\nProject body\n",
                    skill_name, project_desc
                );
                let user_content = format!(
                    "---\nname: {}\ndescription: {}\n---\nUser body\n",
                    skill_name, user_desc
                );

                fs::write(project_skills_path.join("skill.md"), &project_content).unwrap();
                fs::write(user_skills_path.join("skill.md"), &user_content).unwrap();

                // Load registry with both directories
                let registry = SkillRegistry::load(
                    Some(project_skills_path),
                    Some(user_skills_path),
                );

                // find() should return the project-level skill
                let found = registry.find(&skill_name);
                prop_assert!(found.is_some(), "Skill '{}' should be findable", skill_name);

                let skill = found.unwrap();
                prop_assert_eq!(
                    &skill.description, &project_desc,
                    "find() should return project-level skill, got description: '{}', expected: '{}'",
                    skill.description, project_desc
                );

                // Verify the source is Project
                match &skill.source {
                    SkillSource::Project(_) => {} // expected
                    SkillSource::User(_) => {
                        prop_assert!(false, "Skill source should be Project, not User");
                    }
                }

                // Verify only one skill with that name exists (user-level is NOT accessible)
                let all_with_name: Vec<&Skill> = registry
                    .skills()
                    .iter()
                    .filter(|s| s.name == skill_name)
                    .collect();
                prop_assert_eq!(
                    all_with_name.len(), 1,
                    "Only one skill with name '{}' should exist in registry",
                    skill_name
                );
            }
        }
    }

    // ─── Property 18: Skill template variable substitution ───────────────────
    // **Validates: Requirements 20.7**
    //
    // Generate skill bodies with various template variables and argument strings,
    // assert all recognized variables are substituted and unresolved positional vars
    // become empty strings.

    /// Strategy to generate a list of argument words (space-separated).
    fn arb_arguments() -> impl Strategy<Value = Vec<String>> {
        prop::collection::vec("[a-zA-Z0-9_./\\-]{1,20}", 0..8)
    }

    /// Strategy to generate a base_dir path.
    fn arb_base_dir() -> impl Strategy<Value = String> {
        prop::collection::vec("[a-zA-Z0-9_\\-]{1,10}", 1..5).prop_map(|parts| {
            format!("/{}", parts.join("/"))
        })
    }

    /// Strategy to generate a template body containing template variables.
    /// Inserts $ARGUMENTS, ${SKILL_DIR}, and some positional vars ($1..$max_pos)
    /// interspersed with static text.
    fn arb_template_body(max_pos: usize) -> impl Strategy<Value = (String, Vec<usize>)> {
        let static_text = "[a-zA-Z ,.]{0,20}";
        (
            prop::collection::vec(static_text, 3..6),
            prop::bool::ANY,     // include $ARGUMENTS?
            prop::bool::ANY,     // include ${SKILL_DIR}?
            prop::collection::vec(1..=max_pos, 0..5), // positional vars to include
        )
            .prop_map(|(texts, include_args, include_dir, positions)| {
                let mut body = String::new();
                body.push_str(&texts[0]);

                if include_args {
                    body.push_str("$ARGUMENTS");
                    body.push_str(texts.get(1).map(|s| s.as_str()).unwrap_or(""));
                }

                for &pos in &positions {
                    body.push_str(&format!("${}", pos));
                    body.push(' ');
                }

                if include_dir {
                    body.push_str("${SKILL_DIR}");
                    body.push_str(texts.get(2).map(|s| s.as_str()).unwrap_or(""));
                }

                body.push_str(texts.last().map(|s| s.as_str()).unwrap_or(""));
                (body, positions)
            })
    }

    proptest! {
        /// Property 18: After rendering, $ARGUMENTS is replaced with the full arguments string.
        #[test]
        fn prop_skill_template_arguments_substituted(
            words in arb_arguments(),
            base_dir in arb_base_dir(),
        ) {
            let arguments = words.join(" ");
            let body = "prefix $ARGUMENTS suffix";
            let result = render_skill_body(body, &arguments, Path::new(&base_dir));

            // $ARGUMENTS must be replaced
            prop_assert!(!result.contains("$ARGUMENTS"),
                "Result still contains $ARGUMENTS: {}", result);
            prop_assert!(result.contains(&arguments),
                "Result does not contain the arguments string '{}': {}", arguments, result);
        }

        /// Property 18: After rendering, ${SKILL_DIR} is replaced with the base_dir path.
        #[test]
        fn prop_skill_template_skill_dir_substituted(
            words in arb_arguments(),
            base_dir in arb_base_dir(),
        ) {
            let arguments = words.join(" ");
            let body = "dir: ${SKILL_DIR} end";
            let result = render_skill_body(body, &arguments, Path::new(&base_dir));

            // ${SKILL_DIR} must be replaced
            prop_assert!(!result.contains("${SKILL_DIR}"),
                "Result still contains ${{SKILL_DIR}}: {}", result);
            prop_assert!(result.contains(&base_dir),
                "Result does not contain base_dir '{}': {}", base_dir, result);
        }

        /// Property 18: Positional $N is replaced with the Nth word, or empty if N > word count.
        #[test]
        fn prop_skill_template_positional_substituted(
            words in arb_arguments(),
            pos in 1usize..=10,
            base_dir in arb_base_dir(),
        ) {
            let arguments = words.join(" ");
            let body = format!("before ${} after", pos);
            let result = render_skill_body(&body, &arguments, Path::new(&base_dir));

            let expected_value = words.get(pos - 1).map(|s| s.as_str()).unwrap_or("");
            let var_pattern = format!("${}", pos);

            // The positional var should not remain in output
            prop_assert!(!result.contains(&var_pattern),
                "Result still contains {}: {}", var_pattern, result);

            // The expected value should be present in the result
            let expected_rendered = format!("before {} after", expected_value);
            prop_assert_eq!(result, expected_rendered);
        }

        /// Property 18: Full template with mixed variables — all recognized vars are substituted.
        #[test]
        fn prop_skill_template_all_vars_substituted(
            words in arb_arguments(),
            base_dir in arb_base_dir(),
            (body, positions) in arb_template_body(10),
        ) {
            let arguments = words.join(" ");
            let result = render_skill_body(&body, &arguments, Path::new(&base_dir));

            // $ARGUMENTS must not remain
            prop_assert!(!result.contains("$ARGUMENTS"),
                "Result still contains $ARGUMENTS after rendering.\nBody: {}\nResult: {}", body, result);

            // ${SKILL_DIR} must not remain
            prop_assert!(!result.contains("${SKILL_DIR}"),
                "Result still contains ${{SKILL_DIR}} after rendering.\nBody: {}\nResult: {}", body, result);

            // All positional variables that were in the template must be resolved
            for &pos in &positions {
                let var_pattern = format!("${}", pos);
                prop_assert!(!result.contains(&var_pattern),
                    "Result still contains {} after rendering.\nBody: {}\nResult: {}", var_pattern, body, result);
            }
        }
    }
}
