//! Config resolution module for LLM provider profiles.
//!
//! `ConfigResolver` handles layered merging of profile data from user-level
//! and project-level settings files, then applies CLI and environment variable
//! overrides to produce a fully-resolved profile for provider construction.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::profile::ProfilesSection;
use crate::settings::SettingsLoader;

/// Inputs for config resolution. Collected from CLI parsing and the environment.
#[derive(Debug, Clone, Default)]
pub struct ConfigInputs {
    /// --profile flag value (if provided)
    pub profile_name: Option<String>,
    /// --model flag value (if provided)
    pub model_override: Option<String>,
    /// Working directory (for project-level settings lookup)
    pub working_dir: PathBuf,
}

/// Errors from config resolution.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// --profile referenced a name that doesn't exist
    UnknownProfile { name: String },
    /// Profile requires an API key but none is available
    MissingCredentials { provider: String, profile: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProfile { name } => {
                write!(
                    f,
                    "Unknown profile '{}': not found in any settings file",
                    name
                )
            }
            Self::MissingCredentials { provider, profile } => {
                write!(
                    f,
                    "Profile '{}' requires an API key for provider '{}' but none was found \
                     (check config or set environment variable)",
                    profile, provider
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// A fully-resolved profile after merging all config sources.
/// All mandatory fields for provider construction are present.
#[derive(Debug, Clone)]
pub struct ResolvedProfile {
    pub provider: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: String,
    pub context_window: Option<usize>,
    pub max_output_tokens: Option<usize>,
    pub extra: HashMap<String, serde_json::Value>,
}

/// Resolves LLM provider configuration from layered sources.
///
/// Resolution priority (highest to lowest):
/// 1. Environment variables
/// 2. CLI flags (--model, --profile)
/// 3. Project-level settings file profiles
/// 4. User-level settings file profiles
/// 5. Built-in defaults
pub struct ConfigResolver;

impl ConfigResolver {
    /// Resolve a profile from all configuration sources.
    ///
    /// Returns `None` if no profiles are configured and no --profile flag was given
    /// (caller should fall back to existing env-var-based detection).
    ///
    /// Returns `Err` if --profile references an unknown name or credentials are missing.
    pub fn resolve(inputs: &ConfigInputs) -> Result<Option<ResolvedProfile>, ConfigError> {
        Self::resolve_with_user_path(inputs, SettingsLoader::user_path())
    }

    /// Same as `resolve`, but with the user-level settings path injected
    /// explicitly rather than resolved from the real home directory.
    ///
    /// Tests use this directly (passing `None`, or a path under a `TempDir`)
    /// so they stay hermetic — a developer's real `~/.arlo/settings.json`
    /// must never leak into a test's expectations.
    fn resolve_with_user_path(
        inputs: &ConfigInputs,
        user_path: Option<PathBuf>,
    ) -> Result<Option<ResolvedProfile>, ConfigError> {
        let user_profiles = match user_path {
            Some(p) => Self::load_profiles_from_path(&p),
            None => ProfilesSection::default(),
        };
        let project_profiles = Self::load_profiles_from_project(&inputs.working_dir);
        let merged = Self::merge_profiles(user_profiles, project_profiles);

        if merged.profiles.is_empty() && inputs.profile_name.is_none() {
            return Ok(None);
        }

        let selected = match Self::select_profile(&merged, &inputs.profile_name)? {
            Some(profile) => profile,
            None => return Ok(None), // Fall back to env-based detection
        };

        let with_model = Self::apply_model_override(selected, &inputs.model_override);
        let resolved = Self::apply_env_overrides(with_model);
        Self::validate_credentials(&resolved)?;
        Ok(Some(resolved))
    }

    /// Select a profile by CLI flag, default key, or fall back to None.
    ///
    /// - If `--profile <name>` is given and exists: returns that profile.
    /// - If `--profile <name>` is given but doesn't exist: returns `Err(UnknownProfile)`.
    /// - If no `--profile` and `"default"` key exists and references a valid profile: returns it.
    /// - If no `--profile` and `"default"` references a nonexistent profile: warns and returns `Ok(None)`.
    /// - If no `--profile` and no `"default"` key: returns `Ok(None)`.
    fn select_profile(
        merged: &ProfilesSection,
        cli_profile: &Option<String>,
    ) -> Result<Option<ResolvedProfile>, ConfigError> {
        let profile_name = match cli_profile {
            Some(name) => name.clone(),
            None => match &merged.default {
                Some(default_name) => {
                    if !merged.profiles.contains_key(default_name) {
                        tracing::warn!(
                            default = %default_name,
                            "Default profile not found, falling back to env-based detection"
                        );
                        return Ok(None);
                    }
                    default_name.clone()
                }
                None => return Ok(None),
            },
        };

        let config =
            merged
                .profiles
                .get(&profile_name)
                .ok_or_else(|| ConfigError::UnknownProfile {
                    name: profile_name.clone(),
                })?;

        Ok(Some(ResolvedProfile {
            provider: config.provider.clone().unwrap_or_default(),
            api_key: config.api_key.clone(),
            base_url: config.base_url.clone(),
            model: config.model.clone().unwrap_or_default(),
            context_window: config.context_window,
            max_output_tokens: config.max_output_tokens,
            extra: config.extra.clone(),
        }))
    }

    /// Apply `--model` override to the resolved profile.
    ///
    /// If a model override is provided, it replaces the profile's model field.
    /// All other fields remain unchanged.
    fn apply_model_override(
        mut profile: ResolvedProfile,
        model_override: &Option<String>,
    ) -> ResolvedProfile {
        if let Some(model) = model_override {
            profile.model = model.clone();
        }
        profile
    }

    /// Apply environment variable overrides (highest priority).
    ///
    /// Env vars override the resolved profile's api_key and base_url
    /// based on the provider type:
    /// - `"openai"`: `OPENAI_API_KEY`, `OPENAI_BASE_URL`
    /// - `"anthropic"`: `ANTHROPIC_API_KEY`
    /// - `"ollama"`: `OLLAMA_HOST`
    fn apply_env_overrides(mut profile: ResolvedProfile) -> ResolvedProfile {
        match profile.provider.as_str() {
            "openai" => {
                if let Ok(key) = std::env::var("OPENAI_API_KEY") {
                    profile.api_key = Some(key);
                }
                if let Ok(url) = std::env::var("OPENAI_BASE_URL") {
                    profile.base_url = Some(url);
                }
            }
            "anthropic" => {
                if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                    profile.api_key = Some(key);
                }
            }
            "ollama" => {
                if let Ok(host) = std::env::var("OLLAMA_HOST") {
                    profile.base_url = Some(host);
                }
            }
            _ => {}
        }
        profile
    }

    /// Validate that required credentials are present.
    ///
    /// Providers "openai" and "anthropic" require an API key.
    /// Returns an error if the key is missing after all overrides have been applied.
    fn validate_credentials(profile: &ResolvedProfile) -> Result<(), ConfigError> {
        let needs_key = matches!(profile.provider.as_str(), "openai" | "anthropic");
        if needs_key && profile.api_key.is_none() {
            return Err(ConfigError::MissingCredentials {
                provider: profile.provider.clone(),
                profile: "(resolved)".to_string(),
            });
        }
        Ok(())
    }

    /// Load profiles from the project-level settings file (.arlo/settings.json).
    ///
    /// Returns `ProfilesSection::default()` if:
    /// - File does not exist
    /// - File contains invalid JSON
    /// - File has no "profiles" key
    fn load_profiles_from_project(working_dir: &Path) -> ProfilesSection {
        let path = SettingsLoader::project_path(working_dir);
        Self::load_profiles_from_path(&path)
    }

    /// Load profiles from a settings file at the given path.
    ///
    /// Reads the file, parses JSON, extracts the "profiles" key, and
    /// converts it to a `ProfilesSection`. Returns default on any failure.
    fn load_profiles_from_path(path: &Path) -> ProfilesSection {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read settings file for profiles"
                    );
                }
                return ProfilesSection::default();
            }
        };

        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Settings file contains invalid JSON (profiles)"
                );
                return ProfilesSection::default();
            }
        };

        match json.get("profiles") {
            Some(profiles_value) => ProfilesSection::from_value(profiles_value),
            None => ProfilesSection::default(),
        }
    }

    /// Merge two ProfilesSections. Project-level fully overrides user-level
    /// for profiles with the same name. Non-conflicting profiles are unioned.
    ///
    /// Default key: project takes priority, then user.
    pub fn merge_profiles(user: ProfilesSection, project: ProfilesSection) -> ProfilesSection {
        let mut merged_profiles = user.profiles;

        // Project profiles fully replace user profiles of the same name
        for (name, config) in project.profiles {
            merged_profiles.insert(name, config);
        }

        // Default key: project takes priority, then user
        let default = project.default.or(user.default);

        ProfilesSection {
            default,
            profiles: merged_profiles,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileConfig;
    use tempfile::TempDir;

    #[test]
    fn test_config_inputs_default() {
        let inputs = ConfigInputs::default();
        assert_eq!(inputs.profile_name, None);
        assert_eq!(inputs.model_override, None);
        assert_eq!(inputs.working_dir, PathBuf::new());
    }

    #[test]
    fn test_config_error_unknown_profile_display() {
        let err = ConfigError::UnknownProfile {
            name: "myprofile".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("myprofile"));
        assert!(msg.contains("not found in any settings file"));
    }

    #[test]
    fn test_config_error_missing_credentials_display() {
        let err = ConfigError::MissingCredentials {
            provider: "openai".to_string(),
            profile: "work".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("work"));
        assert!(msg.contains("openai"));
        assert!(msg.contains("API key"));
    }

    #[test]
    fn test_config_error_equality() {
        let a = ConfigError::UnknownProfile {
            name: "x".to_string(),
        };
        let b = ConfigError::UnknownProfile {
            name: "x".to_string(),
        };
        assert_eq!(a, b);

        let c = ConfigError::MissingCredentials {
            provider: "openai".to_string(),
            profile: "p".to_string(),
        };
        let d = ConfigError::MissingCredentials {
            provider: "openai".to_string(),
            profile: "p".to_string(),
        };
        assert_eq!(c, d);
        assert_ne!(a, c);
    }

    #[test]
    fn test_resolved_profile_construction() {
        let profile = ResolvedProfile {
            provider: "anthropic".to_string(),
            api_key: Some("sk-test".to_string()),
            base_url: None,
            model: "claude-sonnet-4-20250514".to_string(),
            context_window: Some(200000),
            max_output_tokens: Some(16384),
            extra: HashMap::new(),
        };
        assert_eq!(profile.provider, "anthropic");
        assert_eq!(profile.api_key, Some("sk-test".to_string()));
        assert_eq!(profile.model, "claude-sonnet-4-20250514");
        assert_eq!(profile.context_window, Some(200000));
        assert_eq!(profile.max_output_tokens, Some(16384));
    }

    // ===================================================================
    // Merge logic tests
    // ===================================================================

    #[test]
    fn merge_empty_inputs_returns_empty() {
        let user = ProfilesSection::default();
        let project = ProfilesSection::default();
        let merged = ConfigResolver::merge_profiles(user, project);
        assert!(merged.profiles.is_empty());
        assert_eq!(merged.default, None);
    }

    #[test]
    fn merge_user_only_profiles_preserved() {
        let mut user_profiles = HashMap::new();
        user_profiles.insert(
            "work".to_string(),
            ProfileConfig {
                provider: Some("anthropic".to_string()),
                api_key: Some("sk-user".to_string()),
                ..Default::default()
            },
        );

        let user = ProfilesSection {
            default: Some("work".to_string()),
            profiles: user_profiles,
        };
        let project = ProfilesSection::default();

        let merged = ConfigResolver::merge_profiles(user, project);
        assert_eq!(merged.profiles.len(), 1);
        assert_eq!(merged.profiles["work"].api_key, Some("sk-user".to_string()));
        assert_eq!(merged.default, Some("work".to_string()));
    }

    #[test]
    fn merge_project_only_profiles_preserved() {
        let mut project_profiles = HashMap::new();
        project_profiles.insert(
            "local".to_string(),
            ProfileConfig {
                provider: Some("ollama".to_string()),
                base_url: Some("http://localhost:11434".to_string()),
                ..Default::default()
            },
        );

        let user = ProfilesSection::default();
        let project = ProfilesSection {
            default: Some("local".to_string()),
            profiles: project_profiles,
        };

        let merged = ConfigResolver::merge_profiles(user, project);
        assert_eq!(merged.profiles.len(), 1);
        assert_eq!(
            merged.profiles["local"].provider,
            Some("ollama".to_string())
        );
        assert_eq!(merged.default, Some("local".to_string()));
    }

    #[test]
    fn merge_project_fully_replaces_user_for_same_name() {
        let mut user_profiles = HashMap::new();
        user_profiles.insert(
            "work".to_string(),
            ProfileConfig {
                provider: Some("openai".to_string()),
                api_key: Some("sk-user-key".to_string()),
                model: Some("gpt-4".to_string()),
                ..Default::default()
            },
        );

        let mut project_profiles = HashMap::new();
        project_profiles.insert(
            "work".to_string(),
            ProfileConfig {
                provider: Some("anthropic".to_string()),
                api_key: Some("sk-project-key".to_string()),
                // model is None in project - should NOT inherit from user
                ..Default::default()
            },
        );

        let user = ProfilesSection {
            default: None,
            profiles: user_profiles,
        };
        let project = ProfilesSection {
            default: None,
            profiles: project_profiles,
        };

        let merged = ConfigResolver::merge_profiles(user, project);
        assert_eq!(merged.profiles.len(), 1);
        // Project fully replaces - no field blending
        let work = &merged.profiles["work"];
        assert_eq!(work.provider, Some("anthropic".to_string()));
        assert_eq!(work.api_key, Some("sk-project-key".to_string()));
        assert_eq!(work.model, None); // NOT inherited from user
    }

    #[test]
    fn merge_non_conflicting_profiles_unioned() {
        let mut user_profiles = HashMap::new();
        user_profiles.insert(
            "personal".to_string(),
            ProfileConfig {
                provider: Some("openai".to_string()),
                ..Default::default()
            },
        );

        let mut project_profiles = HashMap::new();
        project_profiles.insert(
            "ci".to_string(),
            ProfileConfig {
                provider: Some("anthropic".to_string()),
                ..Default::default()
            },
        );

        let user = ProfilesSection {
            default: None,
            profiles: user_profiles,
        };
        let project = ProfilesSection {
            default: None,
            profiles: project_profiles,
        };

        let merged = ConfigResolver::merge_profiles(user, project);
        assert_eq!(merged.profiles.len(), 2);
        assert!(merged.profiles.contains_key("personal"));
        assert!(merged.profiles.contains_key("ci"));
    }

    #[test]
    fn merge_default_project_takes_priority() {
        let user = ProfilesSection {
            default: Some("user-default".to_string()),
            profiles: HashMap::new(),
        };
        let project = ProfilesSection {
            default: Some("project-default".to_string()),
            profiles: HashMap::new(),
        };

        let merged = ConfigResolver::merge_profiles(user, project);
        assert_eq!(merged.default, Some("project-default".to_string()));
    }

    #[test]
    fn merge_default_falls_back_to_user_when_project_has_none() {
        let user = ProfilesSection {
            default: Some("user-default".to_string()),
            profiles: HashMap::new(),
        };
        let project = ProfilesSection {
            default: None,
            profiles: HashMap::new(),
        };

        let merged = ConfigResolver::merge_profiles(user, project);
        assert_eq!(merged.default, Some("user-default".to_string()));
    }

    // ===================================================================
    // Load profiles from file tests
    // ===================================================================

    #[test]
    fn load_profiles_from_path_missing_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let result = ConfigResolver::load_profiles_from_path(&path);
        assert_eq!(result, ProfilesSection::default());
    }

    #[test]
    fn load_profiles_from_path_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, "not valid json {{{").unwrap();
        let result = ConfigResolver::load_profiles_from_path(&path);
        assert_eq!(result, ProfilesSection::default());
    }

    #[test]
    fn load_profiles_from_path_no_profiles_key() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, r#"{"permissions": {"allow": []}}"#).unwrap();
        let result = ConfigResolver::load_profiles_from_path(&path);
        assert_eq!(result, ProfilesSection::default());
    }

    #[test]
    fn load_profiles_from_path_valid_profiles() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let content = r#"{
            "profiles": {
                "default": "work",
                "work": {
                    "provider": "anthropic",
                    "api_key": "sk-ant-test",
                    "model": "claude-sonnet-4-20250514"
                },
                "local": {
                    "provider": "ollama",
                    "base_url": "http://localhost:11434"
                }
            }
        }"#;
        fs::write(&path, content).unwrap();

        let result = ConfigResolver::load_profiles_from_path(&path);
        assert_eq!(result.default, Some("work".to_string()));
        assert_eq!(result.profiles.len(), 2);
        assert_eq!(
            result.profiles["work"].provider,
            Some("anthropic".to_string())
        );
        assert_eq!(
            result.profiles["local"].provider,
            Some("ollama".to_string())
        );
    }

    #[test]
    fn load_profiles_from_project_uses_correct_path() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        let settings_path = arlo_dir.join("settings.json");
        let content = r#"{
            "profiles": {
                "default": "test",
                "test": {"provider": "openai", "api_key": "sk-test"}
            }
        }"#;
        fs::write(&settings_path, content).unwrap();

        let result = ConfigResolver::load_profiles_from_project(tmp.path());
        assert_eq!(result.default, Some("test".to_string()));
        assert_eq!(result.profiles.len(), 1);
        assert_eq!(result.profiles["test"].api_key, Some("sk-test".to_string()));
    }

    #[test]
    fn resolve_returns_none_when_no_profiles_and_no_flag() {
        let tmp = TempDir::new().unwrap();
        let inputs = ConfigInputs {
            profile_name: None,
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
        };

        let result = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap();
        assert!(result.is_none());
    }

    // ===================================================================
    // select_profile tests
    // ===================================================================

    #[test]
    fn select_profile_with_cli_flag_existing_profile() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "work".to_string(),
            ProfileConfig {
                provider: Some("anthropic".to_string()),
                api_key: Some("sk-ant".to_string()),
                model: Some("claude-sonnet-4-20250514".to_string()),
                ..Default::default()
            },
        );
        let merged = ProfilesSection {
            default: None,
            profiles,
        };

        let result = ConfigResolver::select_profile(&merged, &Some("work".to_string()));
        let profile = result.unwrap().unwrap();
        assert_eq!(profile.provider, "anthropic");
        assert_eq!(profile.api_key, Some("sk-ant".to_string()));
        assert_eq!(profile.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn select_profile_with_cli_flag_unknown_profile_errors() {
        let merged = ProfilesSection {
            default: None,
            profiles: HashMap::new(),
        };

        let result = ConfigResolver::select_profile(&merged, &Some("nonexistent".to_string()));
        assert_eq!(
            result.unwrap_err(),
            ConfigError::UnknownProfile {
                name: "nonexistent".to_string()
            }
        );
    }

    #[test]
    fn select_profile_uses_default_key() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "main".to_string(),
            ProfileConfig {
                provider: Some("openai".to_string()),
                api_key: Some("sk-openai".to_string()),
                model: Some("gpt-4o".to_string()),
                ..Default::default()
            },
        );
        let merged = ProfilesSection {
            default: Some("main".to_string()),
            profiles,
        };

        let result = ConfigResolver::select_profile(&merged, &None);
        let profile = result.unwrap().unwrap();
        assert_eq!(profile.provider, "openai");
        assert_eq!(profile.model, "gpt-4o");
    }

    #[test]
    fn select_profile_default_references_nonexistent_returns_none() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "existing".to_string(),
            ProfileConfig {
                provider: Some("ollama".to_string()),
                ..Default::default()
            },
        );
        let merged = ProfilesSection {
            default: Some("deleted-profile".to_string()),
            profiles,
        };

        let result = ConfigResolver::select_profile(&merged, &None);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn select_profile_no_cli_no_default_returns_none() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "something".to_string(),
            ProfileConfig {
                provider: Some("openai".to_string()),
                ..Default::default()
            },
        );
        let merged = ProfilesSection {
            default: None,
            profiles,
        };

        let result = ConfigResolver::select_profile(&merged, &None);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn select_profile_maps_all_fields_from_config() {
        let mut extra = HashMap::new();
        extra.insert("org".to_string(), serde_json::json!("org-123"));

        let mut profiles = HashMap::new();
        profiles.insert(
            "full".to_string(),
            ProfileConfig {
                provider: Some("openai".to_string()),
                api_key: Some("sk-key".to_string()),
                base_url: Some("https://custom.api.com".to_string()),
                model: Some("gpt-4".to_string()),
                context_window: Some(128000),
                max_output_tokens: Some(4096),
                extra,
            },
        );
        let merged = ProfilesSection {
            default: None,
            profiles,
        };

        let result = ConfigResolver::select_profile(&merged, &Some("full".to_string()));
        let profile = result.unwrap().unwrap();
        assert_eq!(profile.provider, "openai");
        assert_eq!(profile.api_key, Some("sk-key".to_string()));
        assert_eq!(profile.base_url, Some("https://custom.api.com".to_string()));
        assert_eq!(profile.model, "gpt-4");
        assert_eq!(profile.context_window, Some(128000));
        assert_eq!(profile.max_output_tokens, Some(4096));
        assert_eq!(
            profile.extra.get("org"),
            Some(&serde_json::json!("org-123"))
        );
    }

    #[test]
    fn select_profile_defaults_missing_provider_and_model() {
        let mut profiles = HashMap::new();
        profiles.insert(
            "minimal".to_string(),
            ProfileConfig {
                api_key: Some("sk-key".to_string()),
                ..Default::default()
            },
        );
        let merged = ProfilesSection {
            default: None,
            profiles,
        };

        let result = ConfigResolver::select_profile(&merged, &Some("minimal".to_string()));
        let profile = result.unwrap().unwrap();
        assert_eq!(profile.provider, ""); // unwrap_or_default
        assert_eq!(profile.model, ""); // unwrap_or_default
    }

    // ===================================================================
    // apply_model_override tests
    // ===================================================================

    #[test]
    fn apply_model_override_replaces_model() {
        let profile = ResolvedProfile {
            provider: "openai".to_string(),
            api_key: Some("sk-key".to_string()),
            base_url: None,
            model: "gpt-4".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        let result = ConfigResolver::apply_model_override(profile, &Some("gpt-4o".to_string()));
        assert_eq!(result.model, "gpt-4o");
        // Other fields unchanged
        assert_eq!(result.provider, "openai");
        assert_eq!(result.api_key, Some("sk-key".to_string()));
    }

    #[test]
    fn apply_model_override_none_leaves_model_unchanged() {
        let profile = ResolvedProfile {
            provider: "anthropic".to_string(),
            api_key: Some("sk-ant".to_string()),
            base_url: None,
            model: "claude-sonnet-4-20250514".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        let result = ConfigResolver::apply_model_override(profile, &None);
        assert_eq!(result.model, "claude-sonnet-4-20250514");
    }

    // ===================================================================
    // apply_env_overrides tests
    //
    // NOTE: These tests manipulate environment variables which are
    // process-global state. They are consolidated into a single test
    // to avoid race conditions with parallel test execution.
    // ===================================================================

    #[test]
    fn apply_env_overrides_all_providers() {
        // --- OpenAI: API key override ---
        std::env::set_var("OPENAI_API_KEY", "sk-env-openai");
        std::env::remove_var("OPENAI_BASE_URL");

        let profile = ResolvedProfile {
            provider: "openai".to_string(),
            api_key: Some("sk-config-key".to_string()),
            base_url: None,
            model: "gpt-4".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };
        let result = ConfigResolver::apply_env_overrides(profile);
        assert_eq!(result.api_key, Some("sk-env-openai".to_string()));
        assert_eq!(result.base_url, None); // OPENAI_BASE_URL not set

        // --- OpenAI: base_url override ---
        std::env::remove_var("OPENAI_API_KEY");
        std::env::set_var("OPENAI_BASE_URL", "https://custom.openai.com/v1");

        let profile = ResolvedProfile {
            provider: "openai".to_string(),
            api_key: Some("sk-key".to_string()),
            base_url: None,
            model: "gpt-4".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };
        let result = ConfigResolver::apply_env_overrides(profile);
        assert_eq!(
            result.base_url,
            Some("https://custom.openai.com/v1".to_string())
        );
        assert_eq!(result.api_key, Some("sk-key".to_string())); // unchanged, env var removed

        std::env::remove_var("OPENAI_BASE_URL");

        // --- Anthropic: API key override ---
        std::env::set_var("ANTHROPIC_API_KEY", "sk-ant-env");

        let profile = ResolvedProfile {
            provider: "anthropic".to_string(),
            api_key: Some("sk-ant-config".to_string()),
            base_url: None,
            model: "claude-sonnet-4-20250514".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };
        let result = ConfigResolver::apply_env_overrides(profile);
        assert_eq!(result.api_key, Some("sk-ant-env".to_string()));

        std::env::remove_var("ANTHROPIC_API_KEY");

        // --- Ollama: host override ---
        std::env::set_var("OLLAMA_HOST", "http://remote:11434");

        let profile = ResolvedProfile {
            provider: "ollama".to_string(),
            api_key: None,
            base_url: Some("http://localhost:11434".to_string()),
            model: "llama3".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };
        let result = ConfigResolver::apply_env_overrides(profile);
        assert_eq!(result.base_url, Some("http://remote:11434".to_string()));

        std::env::remove_var("OLLAMA_HOST");

        // --- Unknown provider: no changes ---
        let profile = ResolvedProfile {
            provider: "custom".to_string(),
            api_key: Some("key".to_string()),
            base_url: Some("http://custom.api".to_string()),
            model: "model".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };
        let result = ConfigResolver::apply_env_overrides(profile);
        assert_eq!(result.api_key, Some("key".to_string()));
        assert_eq!(result.base_url, Some("http://custom.api".to_string()));
    }

    // ===================================================================
    // validate_credentials tests
    // ===================================================================

    #[test]
    fn validate_credentials_openai_with_key_passes() {
        let profile = ResolvedProfile {
            provider: "openai".to_string(),
            api_key: Some("sk-key".to_string()),
            base_url: None,
            model: "gpt-4".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        assert!(ConfigResolver::validate_credentials(&profile).is_ok());
    }

    #[test]
    fn validate_credentials_openai_no_key_errors() {
        let profile = ResolvedProfile {
            provider: "openai".to_string(),
            api_key: None,
            base_url: None,
            model: "gpt-4".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        let err = ConfigResolver::validate_credentials(&profile).unwrap_err();
        assert_eq!(
            err,
            ConfigError::MissingCredentials {
                provider: "openai".to_string(),
                profile: "(resolved)".to_string(),
            }
        );
    }

    #[test]
    fn validate_credentials_anthropic_no_key_errors() {
        let profile = ResolvedProfile {
            provider: "anthropic".to_string(),
            api_key: None,
            base_url: None,
            model: "claude-sonnet-4-20250514".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        let err = ConfigResolver::validate_credentials(&profile).unwrap_err();
        assert_eq!(
            err,
            ConfigError::MissingCredentials {
                provider: "anthropic".to_string(),
                profile: "(resolved)".to_string(),
            }
        );
    }

    #[test]
    fn validate_credentials_ollama_no_key_passes() {
        let profile = ResolvedProfile {
            provider: "ollama".to_string(),
            api_key: None,
            base_url: Some("http://localhost:11434".to_string()),
            model: "llama3".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        assert!(ConfigResolver::validate_credentials(&profile).is_ok());
    }

    #[test]
    fn validate_credentials_unknown_provider_no_key_passes() {
        let profile = ResolvedProfile {
            provider: "custom".to_string(),
            api_key: None,
            base_url: None,
            model: "model".to_string(),
            context_window: None,
            max_output_tokens: None,
            extra: HashMap::new(),
        };

        assert!(ConfigResolver::validate_credentials(&profile).is_ok());
    }

    // ===================================================================
    // Integration tests for resolve() with full pipeline
    // ===================================================================

    #[test]
    fn resolve_selects_default_profile_from_project() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        let content = r#"{
            "profiles": {
                "default": "work",
                "work": {
                    "provider": "ollama",
                    "base_url": "http://localhost:11434",
                    "model": "llama3"
                }
            }
        }"#;
        fs::write(arlo_dir.join("settings.json"), content).unwrap();

        let inputs = ConfigInputs {
            profile_name: None,
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
        };

        let result = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap().unwrap();
        assert_eq!(result.provider, "ollama");
        assert_eq!(result.model, "llama3");
        assert_eq!(result.base_url, Some("http://localhost:11434".to_string()));
    }

    #[test]
    fn resolve_cli_profile_selects_named_profile() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        let content = r#"{
            "profiles": {
                "default": "other",
                "other": {"provider": "ollama", "model": "llama3"},
                "work": {
                    "provider": "ollama",
                    "base_url": "http://localhost:11434",
                    "model": "codellama"
                }
            }
        }"#;
        fs::write(arlo_dir.join("settings.json"), content).unwrap();

        let inputs = ConfigInputs {
            profile_name: Some("work".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
        };

        let result = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap().unwrap();
        assert_eq!(result.provider, "ollama");
        assert_eq!(result.model, "codellama");
    }

    #[test]
    fn resolve_cli_unknown_profile_errors() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        let content = r#"{
            "profiles": {
                "work": {"provider": "ollama", "model": "llama3"}
            }
        }"#;
        fs::write(arlo_dir.join("settings.json"), content).unwrap();

        let inputs = ConfigInputs {
            profile_name: Some("nonexistent".to_string()),
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
        };

        let err = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap_err();
        assert_eq!(
            err,
            ConfigError::UnknownProfile {
                name: "nonexistent".to_string()
            }
        );
    }

    #[test]
    fn resolve_model_override_applied() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        let content = r#"{
            "profiles": {
                "default": "work",
                "work": {
                    "provider": "ollama",
                    "base_url": "http://localhost:11434",
                    "model": "llama3"
                }
            }
        }"#;
        fs::write(arlo_dir.join("settings.json"), content).unwrap();

        let inputs = ConfigInputs {
            profile_name: None,
            model_override: Some("codellama".to_string()),
            working_dir: tmp.path().to_path_buf(),
        };

        let result = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap().unwrap();
        assert_eq!(result.model, "codellama"); // Overridden
        assert_eq!(result.provider, "ollama"); // Unchanged
    }

    #[test]
    fn resolve_returns_none_when_no_default_and_no_flag() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        // Profiles exist but no default key and no --profile flag
        let content = r#"{
            "profiles": {
                "work": {"provider": "ollama", "model": "llama3"}
            }
        }"#;
        fs::write(arlo_dir.join("settings.json"), content).unwrap();

        let inputs = ConfigInputs {
            profile_name: None,
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
        };

        let result = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn resolve_returns_none_when_default_references_nonexistent() {
        let tmp = TempDir::new().unwrap();
        let arlo_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&arlo_dir).unwrap();

        let content = r#"{
            "profiles": {
                "default": "deleted",
                "work": {"provider": "ollama", "model": "llama3"}
            }
        }"#;
        fs::write(arlo_dir.join("settings.json"), content).unwrap();

        let inputs = ConfigInputs {
            profile_name: None,
            model_override: None,
            working_dir: tmp.path().to_path_buf(),
        };

        let result = ConfigResolver::resolve_with_user_path(&inputs, None).unwrap();
        assert!(result.is_none());
    }

    // ===================================================================
    // Property-based tests for ConfigResolver
    // ===================================================================

    mod prop_tests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy for generating valid profile names (non-empty alphanumeric strings).
        fn arb_profile_name() -> impl Strategy<Value = String> {
            "[a-z][a-z0-9_-]{0,12}".prop_filter("must not be 'default'", |s| s != "default")
        }

        /// Strategy for generating a ProfileConfig with a provider.
        fn arb_profile_config_with_provider() -> impl Strategy<Value = ProfileConfig> {
            (
                prop_oneof![
                    Just("openai".to_string()),
                    Just("anthropic".to_string()),
                    Just("ollama".to_string()),
                ],
                proptest::option::of("[a-zA-Z0-9_-]{5,20}"),
                proptest::option::of("https?://[a-z0-9.-]{3,15}(:[0-9]{2,5})?"),
                proptest::option::of("[a-zA-Z][a-zA-Z0-9._-]{1,20}"),
                proptest::option::of(1usize..500_000),
                proptest::option::of(1usize..50_000),
            )
                .prop_map(
                    |(provider, api_key, base_url, model, context_window, max_output_tokens)| {
                        ProfileConfig {
                            provider: Some(provider),
                            api_key,
                            base_url,
                            model,
                            context_window,
                            max_output_tokens,
                            extra: HashMap::new(),
                        }
                    },
                )
        }

        proptest! {
            /// **Validates: Requirements 4.2, 4.3, 4.4**
            ///
            /// Property 3: Project-level profile overrides user-level
            ///
            /// For any profile name that exists in both user-level and project-level,
            /// the merged result SHALL contain exactly the project-level profile's values.
            #[test]
            fn prop_project_overrides_user_for_same_name(
                name in arb_profile_name(),
                user_config in arb_profile_config_with_provider(),
                project_config in arb_profile_config_with_provider(),
            ) {
                let mut user_profiles = HashMap::new();
                user_profiles.insert(name.clone(), user_config);
                let user = ProfilesSection {
                    default: None,
                    profiles: user_profiles,
                };

                let mut project_profiles = HashMap::new();
                project_profiles.insert(name.clone(), project_config.clone());
                let project = ProfilesSection {
                    default: None,
                    profiles: project_profiles,
                };

                let merged = ConfigResolver::merge_profiles(user, project);

                // Merged result must contain exactly the project-level config
                prop_assert_eq!(merged.profiles.len(), 1);
                prop_assert_eq!(&merged.profiles[&name], &project_config);
            }

            /// **Validates: Requirements 4.1, 4.3, 4.4**
            ///
            /// Property 4: Profile merge preserves non-conflicting entries
            ///
            /// For any set of disjoint user/project profile names, merged result
            /// SHALL contain all profiles from both sources.
            #[test]
            fn prop_merge_preserves_disjoint_profiles(
                user_names in prop::collection::hash_set(arb_profile_name(), 1..5),
                project_names in prop::collection::hash_set(arb_profile_name(), 1..5),
                user_config in arb_profile_config_with_provider(),
                project_config in arb_profile_config_with_provider(),
            ) {
                // Make sets disjoint by prefixing
                let user_names: Vec<String> = user_names.into_iter().map(|n| format!("u_{}", n)).collect();
                let project_names: Vec<String> = project_names.into_iter().map(|n| format!("p_{}", n)).collect();

                let mut user_profiles = HashMap::new();
                for name in &user_names {
                    user_profiles.insert(name.clone(), user_config.clone());
                }
                let user = ProfilesSection {
                    default: None,
                    profiles: user_profiles,
                };

                let mut project_profiles = HashMap::new();
                for name in &project_names {
                    project_profiles.insert(name.clone(), project_config.clone());
                }
                let project = ProfilesSection {
                    default: None,
                    profiles: project_profiles,
                };

                let merged = ConfigResolver::merge_profiles(user, project);

                // All user profiles must be present
                for name in &user_names {
                    prop_assert!(merged.profiles.contains_key(name),
                        "User profile '{}' missing from merged result", name);
                }
                // All project profiles must be present
                for name in &project_names {
                    prop_assert!(merged.profiles.contains_key(name),
                        "Project profile '{}' missing from merged result", name);
                }
                // Total count must be the union
                prop_assert_eq!(
                    merged.profiles.len(),
                    user_names.len() + project_names.len()
                );
            }

            /// **Validates: Requirements 3.1, 3.2**
            ///
            /// Property 5: CLI --profile selects the correct profile
            ///
            /// For any profile name that exists in the merged set, resolving with
            /// that name SHALL return the profile associated with that name.
            #[test]
            fn prop_cli_profile_selects_correct_profile(
                name in arb_profile_name(),
                config in arb_profile_config_with_provider(),
                other_names in prop::collection::hash_set(arb_profile_name(), 0..3),
                other_config in arb_profile_config_with_provider(),
            ) {
                let mut profiles = HashMap::new();
                profiles.insert(name.clone(), config.clone());
                for other_name in other_names {
                    if other_name != name {
                        profiles.insert(other_name, other_config.clone());
                    }
                }

                let merged = ProfilesSection {
                    default: None,
                    profiles,
                };

                let result = ConfigResolver::select_profile(&merged, &Some(name.clone()));
                let profile = result.unwrap().unwrap();

                prop_assert_eq!(profile.provider, config.provider.unwrap_or_default());
                prop_assert_eq!(profile.api_key, config.api_key);
                prop_assert_eq!(profile.base_url, config.base_url);
                prop_assert_eq!(profile.model, config.model.unwrap_or_default());
                prop_assert_eq!(profile.context_window, config.context_window);
                prop_assert_eq!(profile.max_output_tokens, config.max_output_tokens);
            }

            /// **Validates: Requirements 3.3**
            ///
            /// Property 6: Unknown --profile produces error containing the name
            ///
            /// For any name that doesn't exist in the merged profile set, resolving
            /// SHALL return UnknownProfile error with the requested name.
            #[test]
            fn prop_unknown_profile_produces_error_with_name(
                requested_name in arb_profile_name(),
                existing_names in prop::collection::hash_set(arb_profile_name(), 0..4),
                config in arb_profile_config_with_provider(),
            ) {
                // Ensure the requested name is NOT in the existing set
                let requested = format!("missing_{}", requested_name);

                let mut profiles = HashMap::new();
                for name in existing_names {
                    profiles.insert(name, config.clone());
                }

                let merged = ProfilesSection {
                    default: None,
                    profiles,
                };

                let result = ConfigResolver::select_profile(&merged, &Some(requested.clone()));
                match result {
                    Err(ConfigError::UnknownProfile { name }) => {
                        prop_assert_eq!(name, requested);
                    }
                    other => {
                        prop_assert!(false, "Expected UnknownProfile error, got {:?}", other);
                    }
                }
            }

            /// **Validates: Requirements 3.4, 6.3**
            ///
            /// Property 7: --model override replaces profile model
            ///
            /// The final model field equals the --model value while other fields
            /// remain unchanged.
            #[test]
            fn prop_model_override_replaces_model_only(
                original_model in "[a-zA-Z][a-zA-Z0-9._-]{1,20}",
                override_model in "[a-zA-Z][a-zA-Z0-9._-]{1,20}",
                provider in prop_oneof![Just("openai".to_string()), Just("anthropic".to_string()), Just("ollama".to_string())],
                api_key in proptest::option::of("[a-zA-Z0-9_-]{5,20}"),
                base_url in proptest::option::of("https?://[a-z0-9.-]{3,15}"),
                context_window in proptest::option::of(1usize..500_000),
                max_output_tokens in proptest::option::of(1usize..50_000),
            ) {
                let profile = ResolvedProfile {
                    provider: provider.clone(),
                    api_key: api_key.clone(),
                    base_url: base_url.clone(),
                    model: original_model,
                    context_window,
                    max_output_tokens,
                    extra: HashMap::new(),
                };

                let result = ConfigResolver::apply_model_override(
                    profile,
                    &Some(override_model.clone()),
                );

                // Model must be the override value
                prop_assert_eq!(&result.model, &override_model);
                // All other fields must be unchanged
                prop_assert_eq!(&result.provider, &provider);
                prop_assert_eq!(&result.api_key, &api_key);
                prop_assert_eq!(&result.base_url, &base_url);
                prop_assert_eq!(result.context_window, context_window);
                prop_assert_eq!(result.max_output_tokens, max_output_tokens);
            }

            /// **Validates: Requirements 5.1, 5.2, 5.3, 5.4, 5.5, 6.1**
            ///
            /// Property 8: Environment variables override profile credentials
            ///
            /// Env vars take highest priority for api_key/base_url. Since env vars
            /// are global state, this test uses unique prefixed env var names to
            /// minimize conflicts but still tests the actual logic by temporarily
            /// setting and unsetting real env vars.
            ///
            /// NOTE: This test exercises `apply_env_overrides` with carefully controlled
            /// env var manipulation. We test each provider in sequence within a single
            /// test case to avoid parallel test races.
            #[test]
            fn prop_env_vars_override_profile_credentials(
                profile_key in "[a-zA-Z0-9_-]{5,20}",
                profile_url in "https?://[a-z0-9.-]{3,15}",
                env_key in "[a-zA-Z0-9_-]{5,20}",
                env_url in "https?://[a-z0-9.-]{3,15}",
            ) {
                // Test OpenAI provider: OPENAI_API_KEY overrides api_key
                std::env::set_var("OPENAI_API_KEY", &env_key);
                std::env::set_var("OPENAI_BASE_URL", &env_url);

                let profile = ResolvedProfile {
                    provider: "openai".to_string(),
                    api_key: Some(profile_key.clone()),
                    base_url: Some(profile_url.clone()),
                    model: "gpt-4".to_string(),
                    context_window: None,
                    max_output_tokens: None,
                    extra: HashMap::new(),
                };

                let result = ConfigResolver::apply_env_overrides(profile);
                prop_assert_eq!(result.api_key.as_deref(), Some(env_key.as_str()));
                prop_assert_eq!(result.base_url.as_deref(), Some(env_url.as_str()));

                std::env::remove_var("OPENAI_API_KEY");
                std::env::remove_var("OPENAI_BASE_URL");

                // Test Anthropic provider: ANTHROPIC_API_KEY overrides api_key
                std::env::set_var("ANTHROPIC_API_KEY", &env_key);

                let profile = ResolvedProfile {
                    provider: "anthropic".to_string(),
                    api_key: Some(profile_key.clone()),
                    base_url: None,
                    model: "claude-sonnet-4-20250514".to_string(),
                    context_window: None,
                    max_output_tokens: None,
                    extra: HashMap::new(),
                };

                let result = ConfigResolver::apply_env_overrides(profile);
                prop_assert_eq!(result.api_key.as_deref(), Some(env_key.as_str()));

                std::env::remove_var("ANTHROPIC_API_KEY");

                // Test Ollama provider: OLLAMA_HOST overrides base_url
                std::env::set_var("OLLAMA_HOST", &env_url);

                let profile = ResolvedProfile {
                    provider: "ollama".to_string(),
                    api_key: None,
                    base_url: Some(profile_url.clone()),
                    model: "llama3".to_string(),
                    context_window: None,
                    max_output_tokens: None,
                    extra: HashMap::new(),
                };

                let result = ConfigResolver::apply_env_overrides(profile);
                prop_assert_eq!(result.base_url.as_deref(), Some(env_url.as_str()));

                std::env::remove_var("OLLAMA_HOST");
            }

            /// **Validates: Requirements 4.5, 2.1, 2.2**
            ///
            /// Property 9: Default key resolution respects project-over-user priority
            ///
            /// Project default takes precedence over user default. If only user-level
            /// has a default key, it SHALL be used.
            #[test]
            fn prop_default_key_project_over_user(
                user_default in arb_profile_name(),
                project_default in arb_profile_name(),
            ) {
                // Case 1: Both have defaults - project wins
                let user = ProfilesSection {
                    default: Some(user_default.clone()),
                    profiles: HashMap::new(),
                };
                let project = ProfilesSection {
                    default: Some(project_default.clone()),
                    profiles: HashMap::new(),
                };

                let merged = ConfigResolver::merge_profiles(user, project);
                prop_assert_eq!(merged.default.as_ref(), Some(&project_default));

                // Case 2: Only user has default - user is used
                let user = ProfilesSection {
                    default: Some(user_default.clone()),
                    profiles: HashMap::new(),
                };
                let project = ProfilesSection {
                    default: None,
                    profiles: HashMap::new(),
                };

                let merged = ConfigResolver::merge_profiles(user, project);
                prop_assert_eq!(merged.default.as_ref(), Some(&user_default));

                // Case 3: Only project has default - project is used
                let user = ProfilesSection {
                    default: None,
                    profiles: HashMap::new(),
                };
                let project = ProfilesSection {
                    default: Some(project_default.clone()),
                    profiles: HashMap::new(),
                };

                let merged = ConfigResolver::merge_profiles(user, project);
                prop_assert_eq!(merged.default.as_ref(), Some(&project_default));
            }

            /// **Validates: Requirements 7.5**
            ///
            /// Property 13: Missing credentials produce error
            ///
            /// openai/anthropic without api_key and no env var → MissingCredentials error.
            #[test]
            fn prop_missing_credentials_produce_error(
                provider in prop_oneof![Just("openai".to_string()), Just("anthropic".to_string())],
                model in "[a-zA-Z][a-zA-Z0-9._-]{1,20}",
                base_url in proptest::option::of("https?://[a-z0-9.-]{3,15}"),
                context_window in proptest::option::of(1usize..500_000),
                max_output_tokens in proptest::option::of(1usize..50_000),
            ) {
                let profile = ResolvedProfile {
                    provider: provider.clone(),
                    api_key: None, // No API key
                    base_url,
                    model,
                    context_window,
                    max_output_tokens,
                    extra: HashMap::new(),
                };

                let result = ConfigResolver::validate_credentials(&profile);
                match result {
                    Err(ConfigError::MissingCredentials { provider: p, .. }) => {
                        prop_assert_eq!(p, provider);
                    }
                    other => {
                        prop_assert!(false,
                            "Expected MissingCredentials error for provider '{}', got {:?}",
                            provider, other
                        );
                    }
                }
            }
        }
    }
}
