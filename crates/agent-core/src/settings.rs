//! Settings file loading and policy merging for tool permissions.
//!
//! This module provides:
//! - `SettingsFile`: A parsed representation of `.arlo/settings.json`
//! - `SettingsLoader`: Loads and parses settings files from known paths
//! - `MergedPolicy`: The result of merging multiple settings levels
//! - `PolicyMerger`: Combines user, project, and runtime permission rules

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::pattern::ToolPattern;
use crate::profile::ProfilesSection;

/// A parsed settings file containing allow and deny rules.
///
/// Loaded from `.arlo/settings.json` at either the project or user level.
/// An empty/default instance represents "no rules" (permissive pass-through).
#[derive(Debug, Clone, Default)]
pub struct SettingsFile {
    /// Tool patterns that are explicitly allowed.
    pub allow: Vec<ToolPattern>,
    /// Tool patterns that are explicitly denied.
    pub deny: Vec<ToolPattern>,
}

/// A merged policy combining rules from multiple levels (user, project, runtime).
///
/// After merging, `deny` contains all effective deny rules and `allow` contains
/// all effective allow rules, with conflicts resolved by precedence.
#[derive(Debug, Clone, Default)]
pub struct MergedPolicy {
    /// Ordered deny rules (first match wins within this list).
    pub deny: Vec<ToolPattern>,
    /// Ordered allow rules (first match wins within this list).
    pub allow: Vec<ToolPattern>,
}

/// Extended settings file structure (backward compatible).
///
/// Holds both the existing permissions and the optional profiles section.
/// Used by `SettingsLoader::load_extended()` to parse the full settings file.
#[derive(Debug, Clone, Default)]
pub struct ExtendedSettingsFile {
    /// Existing permissions (unchanged behavior).
    pub permissions: SettingsFile,
    /// New: provider profiles (`None` if `"profiles"` key absent).
    pub profiles: Option<ProfilesSection>,
}

/// Loads and parses settings files from well-known paths.
pub struct SettingsLoader;

impl SettingsLoader {
    /// Load a settings file from the given path.
    ///
    /// - Returns `SettingsFile::default()` if the file doesn't exist (no log).
    /// - Returns `SettingsFile::default()` if the file contains invalid JSON (logs warning).
    /// - Skips individual unparseable pattern strings with a warning log.
    pub fn load(path: &Path) -> SettingsFile {
        let content = match fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read settings file"
                    );
                }
                return SettingsFile::default();
            }
        };

        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Settings file contains invalid JSON"
                );
                return SettingsFile::default();
            }
        };

        let permissions = match json.get("permissions") {
            Some(p) => p,
            None => return SettingsFile::default(),
        };

        let allow = Self::parse_pattern_array(permissions.get("allow"), path);
        let deny = Self::parse_pattern_array(permissions.get("deny"), path);

        SettingsFile { allow, deny }
    }

    /// Resolve the project-level settings path.
    pub fn project_path(working_dir: &Path) -> PathBuf {
        working_dir.join(".arlo").join("settings.json")
    }

    /// Resolve the user-level settings path.
    ///
    /// Returns `None` if the home directory cannot be determined.
    pub fn user_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".arlo").join("settings.json"))
    }

    /// Parse an optional JSON array value into a vector of `ToolPattern`s.
    /// Skips entries that are not strings or fail to parse, logging warnings.
    fn parse_pattern_array(value: Option<&serde_json::Value>, path: &Path) -> Vec<ToolPattern> {
        let arr = match value {
            Some(serde_json::Value::Array(arr)) => arr,
            _ => return Vec::new(),
        };

        arr.iter()
            .filter_map(|entry| {
                let s = match entry.as_str() {
                    Some(s) => s,
                    None => {
                        tracing::warn!(
                            path = %path.display(),
                            entry = %entry,
                            "Skipping non-string entry in permissions array"
                        );
                        return None;
                    }
                };

                match ToolPattern::parse(s) {
                    Some(pattern) => Some(pattern),
                    None => {
                        tracing::warn!(
                            path = %path.display(),
                            rule = %s,
                            "Skipping unparseable permission rule"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// Load the extended settings file, parsing both `"permissions"` and `"profiles"`.
    ///
    /// This method parses both keys independently from the same JSON file.
    /// - Returns defaults if the file doesn't exist or contains invalid JSON.
    /// - The existing `load()` method remains unchanged for backward compatibility.
    pub fn load_extended(path: &Path) -> ExtendedSettingsFile {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "Failed to read settings file"
                    );
                }
                return ExtendedSettingsFile {
                    permissions: SettingsFile::default(),
                    profiles: None,
                };
            }
        };

        let json: serde_json::Value = match serde_json::from_str(&content) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "Settings file contains invalid JSON"
                );
                return ExtendedSettingsFile {
                    permissions: SettingsFile::default(),
                    profiles: None,
                };
            }
        };

        // Parse permissions using existing logic (independent of profiles)
        let permissions = Self::parse_permissions(&json, path);

        // Parse profiles independently (None if "profiles" key absent)
        let profiles = json.get("profiles").map(ProfilesSection::from_value);

        ExtendedSettingsFile {
            permissions,
            profiles,
        }
    }

    /// Parse the `"permissions"` key from a JSON value into a `SettingsFile`.
    ///
    /// Returns `SettingsFile::default()` if the key is missing.
    fn parse_permissions(json: &serde_json::Value, path: &Path) -> SettingsFile {
        let permissions = match json.get("permissions") {
            Some(p) => p,
            None => return SettingsFile::default(),
        };

        let allow = Self::parse_pattern_array(permissions.get("allow"), path);
        let deny = Self::parse_pattern_array(permissions.get("deny"), path);

        SettingsFile { allow, deny }
    }
}

/// Merges policies from multiple levels with defined precedence.
///
/// Merge order: user-level → project-level → runtime.
/// Later sources override earlier sources for the same pattern string.
/// Within the same level, deny wins over allow for overlapping patterns.
/// Non-conflicting rules are preserved as a union.
pub struct PolicyMerger;

impl PolicyMerger {
    /// Merge policies with precedence: user < project < runtime.
    ///
    /// For conflicting patterns (same Display string in multiple levels),
    /// the later level's decision wins. Within the same level, if a pattern
    /// appears in both allow and deny, deny wins.
    /// Non-conflicting rules are unioned across all levels.
    pub fn merge(
        user: &SettingsFile,
        project: &SettingsFile,
        runtime_allow: &[String],
        runtime_deny: &[String],
    ) -> MergedPolicy {
        // We track decisions by pattern string → (is_deny, pattern).
        // Later levels override earlier levels for the same pattern string.
        // Within a level, deny wins if the pattern appears in both allow and deny.
        let mut decisions: HashMap<String, (bool, ToolPattern)> = HashMap::new();

        // Process user-level (lowest precedence)
        Self::apply_level(&mut decisions, &user.allow, &user.deny);

        // Process project-level (overrides user)
        Self::apply_level(&mut decisions, &project.allow, &project.deny);

        // Process runtime level (highest precedence)
        let runtime_allow_patterns: Vec<ToolPattern> = runtime_allow
            .iter()
            .map(|s| {
                ToolPattern::parse(s)
                    .unwrap_or_else(|| ToolPattern::Bare(s.clone()))
            })
            .collect();
        let runtime_deny_patterns: Vec<ToolPattern> = runtime_deny
            .iter()
            .map(|s| {
                ToolPattern::parse(s)
                    .unwrap_or_else(|| ToolPattern::Bare(s.clone()))
            })
            .collect();
        Self::apply_level(&mut decisions, &runtime_allow_patterns, &runtime_deny_patterns);

        // Separate into allow and deny lists
        let mut allow = Vec::new();
        let mut deny = Vec::new();

        for (_key, (is_deny, pattern)) in decisions {
            if is_deny {
                deny.push(pattern);
            } else {
                allow.push(pattern);
            }
        }

        MergedPolicy { deny, allow }
    }

    /// Apply a single level's rules to the decisions map.
    ///
    /// Within the same level, deny wins over allow for the same pattern string.
    /// Patterns from this level override any previous level's decision for the same key.
    fn apply_level(
        decisions: &mut HashMap<String, (bool, ToolPattern)>,
        allow: &[ToolPattern],
        deny: &[ToolPattern],
    ) {
        // Collect this level's patterns. If a pattern appears in both allow and deny
        // within this level, deny wins.
        let mut level_decisions: HashMap<String, (bool, ToolPattern)> = HashMap::new();

        // First, insert all allow rules
        for pattern in allow {
            let key = pattern.to_string();
            level_decisions.insert(key, (false, pattern.clone()));
        }

        // Then, insert all deny rules (overrides allow for same pattern within this level)
        for pattern in deny {
            let key = pattern.to_string();
            level_decisions.insert(key, (true, pattern.clone()));
        }

        // Apply this level's decisions to the overall map (overriding previous levels)
        for (key, decision) in level_decisions {
            decisions.insert(key, decision);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ===================================================================
    // SettingsLoader tests
    // ===================================================================

    #[test]
    fn load_missing_file_returns_default() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("nonexistent.json");
        let result = SettingsLoader::load(&path);
        assert!(result.allow.is_empty());
        assert!(result.deny.is_empty());
    }

    #[test]
    fn load_invalid_json_returns_default() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, "not valid json {{{").unwrap();
        let result = SettingsLoader::load(&path);
        assert!(result.allow.is_empty());
        assert!(result.deny.is_empty());
    }

    #[test]
    fn load_valid_json_no_permissions_key() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, r#"{"other": "stuff"}"#).unwrap();
        let result = SettingsLoader::load(&path);
        assert!(result.allow.is_empty());
        assert!(result.deny.is_empty());
    }

    #[test]
    fn load_valid_settings_with_allow_and_deny() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let content = r#"{
            "permissions": {
                "allow": ["fs_*", "read_file", "Bash(npm*)"],
                "deny": ["Bash(rm *)", "fs_write(/etc/*)"]
            }
        }"#;
        fs::write(&path, content).unwrap();

        let result = SettingsLoader::load(&path);

        assert_eq!(result.allow.len(), 3);
        assert_eq!(result.allow[0], ToolPattern::Bare("fs_*".to_string()));
        assert_eq!(result.allow[1], ToolPattern::Bare("read_file".to_string()));
        assert_eq!(
            result.allow[2],
            ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "npm*".to_string(),
            }
        );

        assert_eq!(result.deny.len(), 2);
        assert_eq!(
            result.deny[0],
            ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "rm *".to_string(),
            }
        );
        assert_eq!(
            result.deny[1],
            ToolPattern::Compound {
                tool_name: "fs_write".to_string(),
                arg_glob: "/etc/*".to_string(),
            }
        );
    }

    #[test]
    fn load_skips_unparseable_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let content = r#"{
            "permissions": {
                "allow": ["fs_*", "", "Bash(unclosed", "valid_tool"],
                "deny": []
            }
        }"#;
        fs::write(&path, content).unwrap();

        let result = SettingsLoader::load(&path);

        // Empty string and unclosed paren should be skipped
        assert_eq!(result.allow.len(), 2);
        assert_eq!(result.allow[0], ToolPattern::Bare("fs_*".to_string()));
        assert_eq!(result.allow[1], ToolPattern::Bare("valid_tool".to_string()));
    }

    #[test]
    fn load_skips_non_string_entries() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let content = r#"{
            "permissions": {
                "allow": ["fs_*", 42, null, "read_file"],
                "deny": [true, "Bash(rm *)"]
            }
        }"#;
        fs::write(&path, content).unwrap();

        let result = SettingsLoader::load(&path);

        assert_eq!(result.allow.len(), 2);
        assert_eq!(result.allow[0], ToolPattern::Bare("fs_*".to_string()));
        assert_eq!(result.allow[1], ToolPattern::Bare("read_file".to_string()));

        assert_eq!(result.deny.len(), 1);
        assert_eq!(
            result.deny[0],
            ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "rm *".to_string(),
            }
        );
    }

    #[test]
    fn load_handles_missing_allow_or_deny_arrays() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");

        // Only allow, no deny
        let content = r#"{"permissions": {"allow": ["fs_*"]}}"#;
        fs::write(&path, content).unwrap();
        let result = SettingsLoader::load(&path);
        assert_eq!(result.allow.len(), 1);
        assert!(result.deny.is_empty());

        // Only deny, no allow
        let content = r#"{"permissions": {"deny": ["Bash(rm *)"]}}"#;
        fs::write(&path, content).unwrap();
        let result = SettingsLoader::load(&path);
        assert!(result.allow.is_empty());
        assert_eq!(result.deny.len(), 1);
    }

    #[test]
    fn load_ignores_unrecognized_top_level_keys() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("settings.json");
        let content = r#"{
            "version": 2,
            "permissions": {"allow": ["fs_*"]},
            "future_feature": {"some": "config"}
        }"#;
        fs::write(&path, content).unwrap();

        let result = SettingsLoader::load(&path);
        assert_eq!(result.allow.len(), 1);
    }

    #[test]
    fn project_path_returns_correct_path() {
        let working_dir = Path::new("/home/user/project");
        let path = SettingsLoader::project_path(working_dir);
        assert_eq!(path, PathBuf::from("/home/user/project/.arlo/settings.json"));
    }

    #[test]
    fn user_path_returns_some_with_home_dir() {
        // This test will pass if dirs::home_dir() returns Some on this system
        let path = SettingsLoader::user_path();
        if let Some(p) = path {
            assert!(p.to_string_lossy().ends_with(".arlo/settings.json"));
        }
        // If home_dir is None (unlikely in CI), the test still passes
    }

    // ===================================================================
    // PolicyMerger tests
    // ===================================================================

    #[test]
    fn merge_empty_inputs_returns_empty_policy() {
        let user = SettingsFile::default();
        let project = SettingsFile::default();
        let result = PolicyMerger::merge(&user, &project, &[], &[]);
        assert!(result.allow.is_empty());
        assert!(result.deny.is_empty());
    }

    #[test]
    fn merge_user_rules_preserved_when_no_conflict() {
        let user = SettingsFile {
            allow: vec![ToolPattern::Bare("fs_*".to_string())],
            deny: vec![ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "rm *".to_string(),
            }],
        };
        let project = SettingsFile::default();

        let result = PolicyMerger::merge(&user, &project, &[], &[]);
        assert_eq!(result.allow.len(), 1);
        assert_eq!(result.deny.len(), 1);
        assert_eq!(result.allow[0], ToolPattern::Bare("fs_*".to_string()));
        assert_eq!(
            result.deny[0],
            ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "rm *".to_string(),
            }
        );
    }

    #[test]
    fn merge_project_overrides_user_for_same_pattern() {
        // User allows fs_*, project denies fs_*
        let user = SettingsFile {
            allow: vec![ToolPattern::Bare("fs_*".to_string())],
            deny: vec![],
        };
        let project = SettingsFile {
            allow: vec![],
            deny: vec![ToolPattern::Bare("fs_*".to_string())],
        };

        let result = PolicyMerger::merge(&user, &project, &[], &[]);

        // Project's deny should override user's allow
        assert!(result.allow.is_empty());
        assert_eq!(result.deny.len(), 1);
        assert_eq!(result.deny[0], ToolPattern::Bare("fs_*".to_string()));
    }

    #[test]
    fn merge_runtime_overrides_project_and_user() {
        // User denies fs_*, project denies fs_*, runtime allows fs_*
        let user = SettingsFile {
            allow: vec![],
            deny: vec![ToolPattern::Bare("fs_*".to_string())],
        };
        let project = SettingsFile {
            allow: vec![],
            deny: vec![ToolPattern::Bare("fs_*".to_string())],
        };

        let result = PolicyMerger::merge(&user, &project, &["fs_*".to_string()], &[]);

        // Runtime allow should override both
        assert_eq!(result.allow.len(), 1);
        assert_eq!(result.allow[0], ToolPattern::Bare("fs_*".to_string()));
        assert!(result.deny.is_empty());
    }

    #[test]
    fn merge_deny_wins_within_same_level() {
        // Same pattern in both allow and deny at project level: deny wins
        let user = SettingsFile::default();
        let project = SettingsFile {
            allow: vec![ToolPattern::Bare("fs_*".to_string())],
            deny: vec![ToolPattern::Bare("fs_*".to_string())],
        };

        let result = PolicyMerger::merge(&user, &project, &[], &[]);

        // Deny should win within same level
        assert!(result.allow.is_empty());
        assert_eq!(result.deny.len(), 1);
        assert_eq!(result.deny[0], ToolPattern::Bare("fs_*".to_string()));
    }

    #[test]
    fn merge_non_conflicting_rules_are_unioned() {
        let user = SettingsFile {
            allow: vec![ToolPattern::Bare("read_file".to_string())],
            deny: vec![ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "rm *".to_string(),
            }],
        };
        let project = SettingsFile {
            allow: vec![ToolPattern::Bare("fs_*".to_string())],
            deny: vec![ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "sudo *".to_string(),
            }],
        };

        let result = PolicyMerger::merge(&user, &project, &[], &[]);

        // All non-conflicting rules should be present
        assert_eq!(result.allow.len(), 2);
        assert_eq!(result.deny.len(), 2);

        // Check allows contain both patterns (order may vary due to HashMap)
        let allow_strings: Vec<String> = result.allow.iter().map(|p| p.to_string()).collect();
        assert!(allow_strings.contains(&"read_file".to_string()));
        assert!(allow_strings.contains(&"fs_*".to_string()));

        // Check denies contain both patterns
        let deny_strings: Vec<String> = result.deny.iter().map(|p| p.to_string()).collect();
        assert!(deny_strings.contains(&"Bash(rm *)".to_string()));
        assert!(deny_strings.contains(&"Bash(sudo *)".to_string()));
    }

    #[test]
    fn merge_runtime_deny_overrides_runtime_allow_for_same_pattern() {
        // Runtime has same pattern in both allow and deny: deny wins
        let user = SettingsFile::default();
        let project = SettingsFile::default();

        let result = PolicyMerger::merge(
            &user,
            &project,
            &["Bash".to_string()],
            &["Bash".to_string()],
        );

        // Within runtime level, deny wins
        assert!(result.allow.is_empty());
        assert_eq!(result.deny.len(), 1);
        assert_eq!(result.deny[0], ToolPattern::Bare("Bash".to_string()));
    }

    #[test]
    fn merge_runtime_strings_parsed_with_bare_fallback() {
        let user = SettingsFile::default();
        let project = SettingsFile::default();

        // Valid compound pattern
        let result = PolicyMerger::merge(
            &user,
            &project,
            &["Bash(npm*)".to_string()],
            &["fs_write(/etc/*)".to_string()],
        );

        assert_eq!(result.allow.len(), 1);
        assert_eq!(
            result.allow[0],
            ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "npm*".to_string(),
            }
        );
        assert_eq!(result.deny.len(), 1);
        assert_eq!(
            result.deny[0],
            ToolPattern::Compound {
                tool_name: "fs_write".to_string(),
                arg_glob: "/etc/*".to_string(),
            }
        );
    }

    #[test]
    fn merge_runtime_unparseable_falls_back_to_bare() {
        let user = SettingsFile::default();
        let project = SettingsFile::default();

        // "Bash(unclosed" won't parse, but the fallback makes it Bare
        let result = PolicyMerger::merge(
            &user,
            &project,
            &["Bash(unclosed".to_string()],
            &[],
        );

        assert_eq!(result.allow.len(), 1);
        assert_eq!(
            result.allow[0],
            ToolPattern::Bare("Bash(unclosed".to_string())
        );
    }

    #[test]
    fn merge_compound_patterns_override_correctly() {
        // User allows Bash(npm*), project denies Bash(npm*)
        let user = SettingsFile {
            allow: vec![ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "npm*".to_string(),
            }],
            deny: vec![],
        };
        let project = SettingsFile {
            allow: vec![],
            deny: vec![ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "npm*".to_string(),
            }],
        };

        let result = PolicyMerger::merge(&user, &project, &[], &[]);

        // Project deny overrides user allow
        assert!(result.allow.is_empty());
        assert_eq!(result.deny.len(), 1);
        assert_eq!(
            result.deny[0],
            ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "npm*".to_string(),
            }
        );
    }

    #[test]
    fn merge_multiple_levels_with_complex_scenario() {
        // User: allow read_file, deny Bash(rm *)
        // Project: allow fs_*, deny read_file (overrides user allow)
        // Runtime: allow read_file (overrides project deny)
        let user = SettingsFile {
            allow: vec![ToolPattern::Bare("read_file".to_string())],
            deny: vec![ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "rm *".to_string(),
            }],
        };
        let project = SettingsFile {
            allow: vec![ToolPattern::Bare("fs_*".to_string())],
            deny: vec![ToolPattern::Bare("read_file".to_string())],
        };

        let result = PolicyMerger::merge(
            &user,
            &project,
            &["read_file".to_string()],
            &[],
        );

        let allow_strings: Vec<String> = result.allow.iter().map(|p| p.to_string()).collect();
        let deny_strings: Vec<String> = result.deny.iter().map(|p| p.to_string()).collect();

        // read_file: user allows, project denies, runtime allows → final: allow
        assert!(allow_strings.contains(&"read_file".to_string()));
        // fs_*: project allows, no conflict → final: allow
        assert!(allow_strings.contains(&"fs_*".to_string()));
        // Bash(rm *): user denies, no override → final: deny
        assert!(deny_strings.contains(&"Bash(rm *)".to_string()));
    }

    // ===================================================================
    // Integration-style tests
    // ===================================================================

    #[test]
    fn end_to_end_load_and_merge() {
        let tmp = TempDir::new().unwrap();

        // Create a project settings file
        let project_dir = tmp.path().join(".arlo");
        fs::create_dir_all(&project_dir).unwrap();
        let project_settings_path = project_dir.join("settings.json");
        let project_content = r#"{
            "permissions": {
                "allow": ["fs_*", "Bash(npm*)"],
                "deny": ["Bash(rm *)", "Bash(sudo *)"]
            }
        }"#;
        fs::write(&project_settings_path, project_content).unwrap();

        // Load project settings
        let project_settings = SettingsLoader::load(&project_settings_path);
        assert_eq!(project_settings.allow.len(), 2);
        assert_eq!(project_settings.deny.len(), 2);

        // Merge with empty user settings and runtime override
        let user_settings = SettingsFile::default();
        let result = PolicyMerger::merge(
            &user_settings,
            &project_settings,
            &[],
            &["Bash(npm*)".to_string()], // runtime denies npm (overrides project allow)
        );

        let allow_strings: Vec<String> = result.allow.iter().map(|p| p.to_string()).collect();
        let deny_strings: Vec<String> = result.deny.iter().map(|p| p.to_string()).collect();

        // fs_* should still be allowed (no conflict)
        assert!(allow_strings.contains(&"fs_*".to_string()));
        // Bash(npm*) should now be denied (runtime override)
        assert!(deny_strings.contains(&"Bash(npm*)".to_string()));
        // Other denies preserved
        assert!(deny_strings.contains(&"Bash(rm *)".to_string()));
        assert!(deny_strings.contains(&"Bash(sudo *)".to_string()));
    }

    #[test]
    fn project_path_concatenates_correctly() {
        let path = SettingsLoader::project_path(Path::new("/workspace/myproject"));
        assert_eq!(
            path,
            PathBuf::from("/workspace/myproject/.arlo/settings.json")
        );
    }

    // ===================================================================
    // Property-Based Tests
    // ===================================================================

    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy for valid bare tool pattern strings (lowercase letters + underscore).
        fn bare_pattern_strategy() -> impl Strategy<Value = String> {
            "[a-z][a-z_]{0,9}"
        }

        /// Strategy for compound patterns like `"Name(arg*)"`.
        fn compound_pattern_strategy() -> impl Strategy<Value = String> {
            ("[A-Z][a-z]{1,8}", "[a-z0-9/*]{1,10}").prop_map(|(name, arg)| {
                format!("{}({})", name, arg)
            })
        }

        /// Strategy for valid tool pattern strings (either bare or compound).
        fn valid_pattern_strategy() -> impl Strategy<Value = String> {
            prop_oneof![
                bare_pattern_strategy(),
                compound_pattern_strategy(),
            ]
        }

        // ===================================================================
        // Property 1: Settings File Parsing Preserves Content
        // **Validates: Requirements 1.3, 1.4, 2.1, 2.4**
        // ===================================================================

        proptest! {
            /// Property 1: For valid JSON with permissions.allow and permissions.deny
            /// string arrays, parsing produces a SettingsFile whose allow and deny
            /// vectors contain exactly the valid ToolPatterns corresponding to each
            /// parseable string in the input arrays (in order), with unparseable
            /// strings skipped.
            ///
            /// **Validates: Requirements 1.3, 2.1, 2.4**
            #[test]
            fn parsing_preserves_valid_patterns_in_order(
                allow_patterns in proptest::collection::vec(valid_pattern_strategy(), 0..6),
                deny_patterns in proptest::collection::vec(valid_pattern_strategy(), 0..6),
                extra_key in "[a-z]{3,8}",
                extra_value in "[a-z]{3,8}",
            ) {
                let tmp = TempDir::new().unwrap();
                let path = tmp.path().join("settings.json");

                // Build valid JSON with allow and deny arrays (plus an extra unrecognized key)
                let allow_json: Vec<String> = allow_patterns.iter().map(|s| format!("\"{}\"", s)).collect();
                let deny_json: Vec<String> = deny_patterns.iter().map(|s| format!("\"{}\"", s)).collect();

                let content = format!(
                    r#"{{"permissions": {{"allow": [{}], "deny": [{}]}}, "{}": "{}"}}"#,
                    allow_json.join(", "),
                    deny_json.join(", "),
                    extra_key,
                    extra_value
                );

                fs::write(&path, &content).unwrap();
                let result = SettingsLoader::load(&path);

                // Compute expected patterns (only those that parse successfully)
                let expected_allow: Vec<ToolPattern> = allow_patterns
                    .iter()
                    .filter_map(|s| ToolPattern::parse(s))
                    .collect();
                let expected_deny: Vec<ToolPattern> = deny_patterns
                    .iter()
                    .filter_map(|s| ToolPattern::parse(s))
                    .collect();

                prop_assert_eq!(
                    result.allow.len(), expected_allow.len(),
                    "Allow list length mismatch"
                );
                prop_assert_eq!(
                    result.deny.len(), expected_deny.len(),
                    "Deny list length mismatch"
                );

                // Verify order is preserved
                for (i, (got, expected)) in result.allow.iter().zip(expected_allow.iter()).enumerate() {
                    prop_assert_eq!(
                        got, expected,
                        "Allow pattern at index {} differs", i
                    );
                }
                for (i, (got, expected)) in result.deny.iter().zip(expected_deny.iter()).enumerate() {
                    prop_assert_eq!(
                        got, expected,
                        "Deny pattern at index {} differs", i
                    );
                }
            }
        }

        // ===================================================================
        // Property 2: Invalid Input Resilience
        // **Validates: Requirements 1.4, 2.3**
        // ===================================================================

        proptest! {
            /// Property 2a: Invalid JSON returns default SettingsFile without panic.
            ///
            /// For any byte string that is not valid JSON, SettingsLoader::load SHALL
            /// return a default (empty) SettingsFile without panicking.
            ///
            /// **Validates: Requirements 1.4, 2.3**
            #[test]
            fn invalid_json_returns_default_without_panic(
                garbage in proptest::collection::vec(any::<u8>(), 0..100),
            ) {
                let tmp = TempDir::new().unwrap();
                let path = tmp.path().join("settings.json");
                fs::write(&path, &garbage).unwrap();

                // Should not panic
                let result = SettingsLoader::load(&path);
                // If it happened to be valid JSON with correct structure, we
                // just verify it returns *something* without panic.
                // But for truly invalid JSON, it returns default.
                if serde_json::from_slice::<serde_json::Value>(&garbage).is_err() {
                    prop_assert!(result.allow.is_empty());
                    prop_assert!(result.deny.is_empty());
                }
            }

            /// Property 2b: Invalid pattern strings are skipped, remaining valid rules preserved.
            ///
            /// When valid JSON contains a mix of parseable and unparseable pattern
            /// strings, unparseable strings are skipped and the remaining valid
            /// rules are preserved in order.
            ///
            /// **Validates: Requirements 1.4, 2.3**
            #[test]
            fn invalid_patterns_skipped_valid_preserved(
                valid_patterns in proptest::collection::vec(bare_pattern_strategy(), 1..4),
                invalid_patterns in proptest::collection::vec(
                    prop_oneof![
                        Just("".to_string()),
                        Just("Bash(unclosed".to_string()),
                        Just("foo)bar".to_string()),
                    ],
                    1..3
                ),
            ) {
                let tmp = TempDir::new().unwrap();
                let path = tmp.path().join("settings.json");

                // Interleave valid and invalid patterns
                let mut all_patterns: Vec<String> = Vec::new();
                for (i, vp) in valid_patterns.iter().enumerate() {
                    all_patterns.push(vp.clone());
                    if i < invalid_patterns.len() {
                        all_patterns.push(invalid_patterns[i].clone());
                    }
                }

                let patterns_json: Vec<String> = all_patterns.iter().map(|s| format!("\"{}\"", s)).collect();
                let content = format!(
                    r#"{{"permissions": {{"allow": [{}]}}}}"#,
                    patterns_json.join(", ")
                );

                fs::write(&path, &content).unwrap();
                let result = SettingsLoader::load(&path);

                // Only the valid patterns should be present
                let expected: Vec<ToolPattern> = valid_patterns
                    .iter()
                    .filter_map(|s| ToolPattern::parse(s))
                    .collect();

                prop_assert_eq!(
                    result.allow.len(), expected.len(),
                    "Expected {} valid patterns, got {}",
                    expected.len(), result.allow.len()
                );

                for (i, (got, exp)) in result.allow.iter().zip(expected.iter()).enumerate() {
                    prop_assert_eq!(got, exp, "Pattern at index {} differs", i);
                }
            }
        }

        // ===================================================================
        // Property 3: Policy Merge Precedence
        // **Validates: Requirements 3.1, 3.3, 3.5**
        // ===================================================================

        proptest! {
            /// Property 3: For any pattern appearing in multiple levels, the
            /// highest-precedence source (runtime > project > user) determines
            /// the effective rule.
            ///
            /// **Validates: Requirements 3.1, 3.3, 3.5**
            #[test]
            fn merge_precedence_highest_level_wins(
                pattern in bare_pattern_strategy(),
                user_is_allow in any::<bool>(),
                project_is_allow in any::<bool>(),
                runtime_is_allow in any::<bool>(),
            ) {
                // Place the same pattern at all three levels with varying allow/deny
                let user = if user_is_allow {
                    SettingsFile {
                        allow: vec![ToolPattern::parse(&pattern).unwrap()],
                        deny: vec![],
                    }
                } else {
                    SettingsFile {
                        allow: vec![],
                        deny: vec![ToolPattern::parse(&pattern).unwrap()],
                    }
                };

                let project = if project_is_allow {
                    SettingsFile {
                        allow: vec![ToolPattern::parse(&pattern).unwrap()],
                        deny: vec![],
                    }
                } else {
                    SettingsFile {
                        allow: vec![],
                        deny: vec![ToolPattern::parse(&pattern).unwrap()],
                    }
                };

                let (runtime_allow, runtime_deny) = if runtime_is_allow {
                    (vec![pattern.clone()], vec![])
                } else {
                    (vec![], vec![pattern.clone()])
                };

                let merged = PolicyMerger::merge(&user, &project, &runtime_allow, &runtime_deny);

                // Runtime is highest precedence, its decision wins
                let pattern_str = pattern.clone();
                let in_allow = merged.allow.iter().any(|p| p.to_string() == pattern_str);
                let in_deny = merged.deny.iter().any(|p| p.to_string() == pattern_str);

                if runtime_is_allow {
                    prop_assert!(in_allow,
                        "Pattern '{}' should be in allow (runtime allows)", pattern_str);
                    prop_assert!(!in_deny,
                        "Pattern '{}' should NOT be in deny (runtime allows)", pattern_str);
                } else {
                    prop_assert!(in_deny,
                        "Pattern '{}' should be in deny (runtime denies)", pattern_str);
                    prop_assert!(!in_allow,
                        "Pattern '{}' should NOT be in allow (runtime denies)", pattern_str);
                }
            }

            /// Property 3b: When runtime has no opinion, project overrides user.
            ///
            /// **Validates: Requirements 3.1, 3.3, 3.5**
            #[test]
            fn merge_project_overrides_user_when_no_runtime(
                pattern in bare_pattern_strategy(),
                user_is_allow in any::<bool>(),
                project_is_allow in any::<bool>(),
            ) {
                let user = if user_is_allow {
                    SettingsFile {
                        allow: vec![ToolPattern::parse(&pattern).unwrap()],
                        deny: vec![],
                    }
                } else {
                    SettingsFile {
                        allow: vec![],
                        deny: vec![ToolPattern::parse(&pattern).unwrap()],
                    }
                };

                let project = if project_is_allow {
                    SettingsFile {
                        allow: vec![ToolPattern::parse(&pattern).unwrap()],
                        deny: vec![],
                    }
                } else {
                    SettingsFile {
                        allow: vec![],
                        deny: vec![ToolPattern::parse(&pattern).unwrap()],
                    }
                };

                // No runtime rules for this pattern
                let merged = PolicyMerger::merge(&user, &project, &[], &[]);

                let pattern_str = pattern.clone();
                let in_allow = merged.allow.iter().any(|p| p.to_string() == pattern_str);
                let in_deny = merged.deny.iter().any(|p| p.to_string() == pattern_str);

                // Project level wins over user level
                if project_is_allow {
                    prop_assert!(in_allow,
                        "Pattern '{}' should be in allow (project allows)", pattern_str);
                    prop_assert!(!in_deny,
                        "Pattern '{}' should NOT be in deny (project allows)", pattern_str);
                } else {
                    prop_assert!(in_deny,
                        "Pattern '{}' should be in deny (project denies)", pattern_str);
                    prop_assert!(!in_allow,
                        "Pattern '{}' should NOT be in allow (project denies)", pattern_str);
                }
            }
        }

        // ===================================================================
        // Property 4: Deny Wins Over Allow at Same Level
        // **Validates: Requirements 3.2, 5.2**
        // ===================================================================

        proptest! {
            /// Property 4: For any pattern in both allow and deny within the same
            /// level, deny wins.
            ///
            /// **Validates: Requirements 3.2, 5.2**
            #[test]
            fn deny_wins_over_allow_at_same_level(
                pattern in bare_pattern_strategy(),
                level in 0..3u8,  // 0=user, 1=project, 2=runtime
            ) {
                let tp = ToolPattern::parse(&pattern).unwrap();

                // Place pattern in both allow and deny at the chosen level
                let (user, project, runtime_allow, runtime_deny) = match level {
                    0 => {
                        // Conflict at user level
                        let user = SettingsFile {
                            allow: vec![tp.clone()],
                            deny: vec![tp.clone()],
                        };
                        (user, SettingsFile::default(), vec![], vec![])
                    }
                    1 => {
                        // Conflict at project level
                        let project = SettingsFile {
                            allow: vec![tp.clone()],
                            deny: vec![tp.clone()],
                        };
                        (SettingsFile::default(), project, vec![], vec![])
                    }
                    _ => {
                        // Conflict at runtime level
                        (
                            SettingsFile::default(),
                            SettingsFile::default(),
                            vec![pattern.clone()],
                            vec![pattern.clone()],
                        )
                    }
                };

                let merged = PolicyMerger::merge(&user, &project, &runtime_allow, &runtime_deny);

                let pattern_str = pattern.clone();
                let in_allow = merged.allow.iter().any(|p| p.to_string() == pattern_str);
                let in_deny = merged.deny.iter().any(|p| p.to_string() == pattern_str);

                // Deny should win when pattern is in both allow and deny at same level
                prop_assert!(in_deny,
                    "Pattern '{}' should be in deny (deny wins at level {})", pattern_str, level);
                prop_assert!(!in_allow,
                    "Pattern '{}' should NOT be in allow (deny wins at level {})", pattern_str, level);
            }
        }

        // ===================================================================
        // Property 5: Non-Conflicting Rule Union
        // **Validates: Requirements 3.4**
        // ===================================================================

        proptest! {
            /// Property 5: When no two rules target the same pattern across levels,
            /// the merged policy contains all rules from all levels.
            ///
            /// **Validates: Requirements 3.4**
            #[test]
            fn non_conflicting_rules_are_unioned(
                user_allow in proptest::collection::vec("[a-z]{3,6}", 0..3),
                user_deny in proptest::collection::vec("[a-z]{3,6}", 0..3),
                project_allow in proptest::collection::vec("[a-z]{3,6}", 0..3),
                project_deny in proptest::collection::vec("[a-z]{3,6}", 0..3),
            ) {
                // Use a numbering prefix to ensure no collisions across levels
                let user_allow_pats: Vec<String> = user_allow.iter().enumerate()
                    .map(|(i, s)| format!("ua{}_{}", i, s))
                    .collect();
                let user_deny_pats: Vec<String> = user_deny.iter().enumerate()
                    .map(|(i, s)| format!("ud{}_{}", i, s))
                    .collect();
                let project_allow_pats: Vec<String> = project_allow.iter().enumerate()
                    .map(|(i, s)| format!("pa{}_{}", i, s))
                    .collect();
                let project_deny_pats: Vec<String> = project_deny.iter().enumerate()
                    .map(|(i, s)| format!("pd{}_{}", i, s))
                    .collect();

                let user = SettingsFile {
                    allow: user_allow_pats.iter().filter_map(|s| ToolPattern::parse(s)).collect(),
                    deny: user_deny_pats.iter().filter_map(|s| ToolPattern::parse(s)).collect(),
                };
                let project = SettingsFile {
                    allow: project_allow_pats.iter().filter_map(|s| ToolPattern::parse(s)).collect(),
                    deny: project_deny_pats.iter().filter_map(|s| ToolPattern::parse(s)).collect(),
                };

                let merged = PolicyMerger::merge(&user, &project, &[], &[]);

                let allow_strings: Vec<String> = merged.allow.iter().map(|p| p.to_string()).collect();
                let deny_strings: Vec<String> = merged.deny.iter().map(|p| p.to_string()).collect();

                // All user allow patterns should be in merged allow
                for pat in &user_allow_pats {
                    if ToolPattern::parse(pat).is_some() {
                        prop_assert!(
                            allow_strings.contains(pat),
                            "User allow pattern '{}' missing from merged allow. Got: {:?}",
                            pat, allow_strings
                        );
                    }
                }

                // All user deny patterns should be in merged deny
                for pat in &user_deny_pats {
                    if ToolPattern::parse(pat).is_some() {
                        prop_assert!(
                            deny_strings.contains(pat),
                            "User deny pattern '{}' missing from merged deny. Got: {:?}",
                            pat, deny_strings
                        );
                    }
                }

                // All project allow patterns should be in merged allow
                for pat in &project_allow_pats {
                    if ToolPattern::parse(pat).is_some() {
                        prop_assert!(
                            allow_strings.contains(pat),
                            "Project allow pattern '{}' missing from merged allow. Got: {:?}",
                            pat, allow_strings
                        );
                    }
                }

                // All project deny patterns should be in merged deny
                for pat in &project_deny_pats {
                    if ToolPattern::parse(pat).is_some() {
                        prop_assert!(
                            deny_strings.contains(pat),
                            "Project deny pattern '{}' missing from merged deny. Got: {:?}",
                            pat, deny_strings
                        );
                    }
                }

                // Total count should be the union
                let total_expected_allow = user_allow_pats.iter()
                    .chain(project_allow_pats.iter())
                    .filter(|s| ToolPattern::parse(s).is_some())
                    .count();
                let total_expected_deny = user_deny_pats.iter()
                    .chain(project_deny_pats.iter())
                    .filter(|s| ToolPattern::parse(s).is_some())
                    .count();

                prop_assert_eq!(
                    merged.allow.len(), total_expected_allow,
                    "Allow count mismatch"
                );
                prop_assert_eq!(
                    merged.deny.len(), total_expected_deny,
                    "Deny count mismatch"
                );
            }
        }

        // ===================================================================
        // Property 14: Profiles and permissions are independent
        // **Validates: Requirements 8.2, 8.3**
        // ===================================================================

        /// Strategy for generating a valid permissions JSON object (with allow/deny arrays).
        fn permissions_json_strategy() -> impl Strategy<Value = serde_json::Value> {
            (
                proptest::collection::vec(valid_pattern_strategy(), 0..4),
                proptest::collection::vec(valid_pattern_strategy(), 0..4),
            )
                .prop_map(|(allow, deny)| {
                    serde_json::json!({
                        "allow": allow,
                        "deny": deny
                    })
                })
        }

        /// Strategy for generating a valid profiles JSON object.
        fn profiles_json_strategy() -> impl Strategy<Value = serde_json::Value> {
            (
                proptest::option::of("[a-z]{3,8}"),   // default profile name
                proptest::collection::vec(
                    (
                        "[a-z]{3,8}",                // profile name
                        proptest::option::of(prop_oneof![
                            Just("openai".to_string()),
                            Just("anthropic".to_string()),
                            Just("ollama".to_string()),
                        ]),
                        proptest::option::of("[a-zA-Z0-9_-]{5,20}"), // api_key
                        proptest::option::of("https?://[a-z0-9]{3,10}\\.[a-z]{2,4}"), // base_url
                        proptest::option::of("[a-z]{3,10}-[0-9]{1,2}"), // model
                    ),
                    1..4,
                ),
            )
                .prop_map(|(default_name, profile_entries)| {
                    let mut obj = serde_json::Map::new();
                    if let Some(d) = &default_name {
                        obj.insert("default".to_string(), serde_json::json!(d));
                    }
                    for (name, provider, api_key, base_url, model) in &profile_entries {
                        let mut profile_obj = serde_json::Map::new();
                        if let Some(p) = provider {
                            profile_obj.insert("provider".to_string(), serde_json::json!(p));
                        }
                        if let Some(k) = api_key {
                            profile_obj.insert("api_key".to_string(), serde_json::json!(k));
                        }
                        if let Some(u) = base_url {
                            profile_obj.insert("base_url".to_string(), serde_json::json!(u));
                        }
                        if let Some(m) = model {
                            profile_obj.insert("model".to_string(), serde_json::json!(m));
                        }
                        obj.insert(
                            name.clone(),
                            serde_json::Value::Object(profile_obj),
                        );
                    }
                    serde_json::Value::Object(obj)
                })
        }

        proptest! {
            /// Property 14: Profiles and permissions are independent.
            ///
            /// For any settings file containing both "profiles" and "permissions" keys,
            /// loading and parsing one key SHALL not affect the parsed result of the other key.
            ///
            /// We verify this by:
            /// 1. Changing the "profiles" content doesn't affect the parsed "permissions" result
            /// 2. Changing the "permissions" content doesn't affect the parsed "profiles" result
            /// 3. Both keys are parsed independently from the same file
            ///
            /// **Validates: Requirements 8.2, 8.3**
            #[test]
            fn profiles_and_permissions_are_independent(
                permissions_a in permissions_json_strategy(),
                permissions_b in permissions_json_strategy(),
                profiles_a in profiles_json_strategy(),
                profiles_b in profiles_json_strategy(),
            ) {
                use crate::profile::ProfilesSection;

                let tmp = tempfile::TempDir::new().unwrap();

                // --- Test 1: Changing profiles does NOT affect permissions ---
                // Write file with permissions_a + profiles_a
                let file_path = tmp.path().join("test1.json");
                let content_1 = serde_json::json!({
                    "permissions": permissions_a,
                    "profiles": profiles_a
                });
                fs::write(&file_path, serde_json::to_string(&content_1).unwrap()).unwrap();
                let result_1 = SettingsLoader::load_extended(&file_path);

                // Write file with permissions_a + profiles_b (different profiles, same permissions)
                let file_path_2 = tmp.path().join("test2.json");
                let content_2 = serde_json::json!({
                    "permissions": permissions_a,
                    "profiles": profiles_b
                });
                fs::write(&file_path_2, serde_json::to_string(&content_2).unwrap()).unwrap();
                let result_2 = SettingsLoader::load_extended(&file_path_2);

                // Permissions should be identical regardless of profiles content
                prop_assert_eq!(
                    result_1.permissions.allow.len(),
                    result_2.permissions.allow.len(),
                    "Permissions allow count differs when only profiles changed"
                );
                prop_assert_eq!(
                    result_1.permissions.deny.len(),
                    result_2.permissions.deny.len(),
                    "Permissions deny count differs when only profiles changed"
                );
                for (i, (a, b)) in result_1.permissions.allow.iter()
                    .zip(result_2.permissions.allow.iter()).enumerate()
                {
                    prop_assert_eq!(a, b,
                        "Permissions allow[{}] differs when only profiles changed", i);
                }
                for (i, (a, b)) in result_1.permissions.deny.iter()
                    .zip(result_2.permissions.deny.iter()).enumerate()
                {
                    prop_assert_eq!(a, b,
                        "Permissions deny[{}] differs when only profiles changed", i);
                }

                // --- Test 2: Changing permissions does NOT affect profiles ---
                // Write file with permissions_b + profiles_a (different permissions, same profiles)
                let file_path_3 = tmp.path().join("test3.json");
                let content_3 = serde_json::json!({
                    "permissions": permissions_b,
                    "profiles": profiles_a
                });
                fs::write(&file_path_3, serde_json::to_string(&content_3).unwrap()).unwrap();
                let result_3 = SettingsLoader::load_extended(&file_path_3);

                // Profiles should be identical regardless of permissions content
                let profiles_from_1 = result_1.profiles.clone()
                    .unwrap_or_else(ProfilesSection::default);
                let profiles_from_3 = result_3.profiles.clone()
                    .unwrap_or_else(ProfilesSection::default);

                prop_assert_eq!(
                    profiles_from_1.default,
                    profiles_from_3.default,
                    "Profiles default differs when only permissions changed"
                );
                prop_assert_eq!(
                    profiles_from_1.profiles.len(),
                    profiles_from_3.profiles.len(),
                    "Profiles count differs when only permissions changed"
                );
                for (name, config_1) in &profiles_from_1.profiles {
                    let config_3 = profiles_from_3.profiles.get(name);
                    prop_assert!(
                        config_3.is_some(),
                        "Profile '{}' missing when only permissions changed", name
                    );
                    prop_assert_eq!(
                        config_1, config_3.unwrap(),
                        "Profile '{}' content differs when only permissions changed", name
                    );
                }
            }
        }
    }
}
