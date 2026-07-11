//! Profile configuration data structures for LLM provider profiles.
//!
//! Profiles are defined in `.arlo/settings.json` under the `"profiles"` key.
//! Each profile specifies a provider, credentials, and model defaults.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A single named provider profile as it appears in settings.json.
/// All fields are optional to support partial overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ProfileConfig {
    /// Provider identifier: "openai", "anthropic", or "ollama"
    pub provider: Option<String>,
    /// API key / token for the provider
    pub api_key: Option<String>,
    /// Base URL override (e.g., custom OpenAI-compatible endpoint)
    pub base_url: Option<String>,
    /// Default model name for this profile
    pub model: Option<String>,
    /// Override the model's default context window (in tokens)
    pub context_window: Option<usize>,
    /// Override the model's default max output tokens
    pub max_output_tokens: Option<usize>,
    /// Provider-specific extra configuration
    #[serde(default)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// The "profiles" section of a settings file.
///
/// The JSON structure uses profile names as keys alongside a special `"default"` key:
///
/// ```json
/// {
///   "default": "work",
///   "work": { "provider": "anthropic", "api_key": "sk-..." },
///   "local": { "provider": "ollama", "base_url": "http://localhost:11434" }
/// }
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfilesSection {
    /// The name of the default profile to use when no --profile flag is given.
    pub default: Option<String>,
    /// Named profiles. Keys are profile names, values are configurations.
    pub profiles: HashMap<String, ProfileConfig>,
}

impl ProfilesSection {
    /// Custom deserialization: extract "default" as a special key,
    /// everything else becomes a profile entry.
    ///
    /// Returns `ProfilesSection::default()` if the value is not a JSON object.
    /// Skips entries that fail to deserialize as a `ProfileConfig` (logs a warning).
    pub fn from_value(value: &serde_json::Value) -> Self {
        let obj = match value.as_object() {
            Some(o) => o,
            None => return Self::default(),
        };

        let default = obj.get("default").and_then(|v| v.as_str()).map(String::from);

        let mut profiles = HashMap::new();
        for (key, val) in obj {
            if key == "default" {
                continue;
            }
            if let Ok(config) = serde_json::from_value::<ProfileConfig>(val.clone()) {
                profiles.insert(key.clone(), config);
            } else {
                tracing::warn!(profile = %key, "Skipping unparseable profile entry");
            }
        }

        ProfilesSection { default, profiles }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    #[test]
    fn test_profile_config_all_fields() {
        let json = json!({
            "provider": "openai",
            "api_key": "sk-test",
            "base_url": "https://api.example.com",
            "model": "gpt-4o",
            "context_window": 128000,
            "max_output_tokens": 4096,
            "extra": { "organization": "org-123" }
        });

        let config: ProfileConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.provider, Some("openai".to_string()));
        assert_eq!(config.api_key, Some("sk-test".to_string()));
        assert_eq!(config.base_url, Some("https://api.example.com".to_string()));
        assert_eq!(config.model, Some("gpt-4o".to_string()));
        assert_eq!(config.context_window, Some(128000));
        assert_eq!(config.max_output_tokens, Some(4096));
        assert_eq!(
            config.extra.get("organization"),
            Some(&serde_json::Value::String("org-123".to_string()))
        );
    }

    #[test]
    fn test_profile_config_empty_object() {
        let json = json!({});
        let config: ProfileConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config, ProfileConfig::default());
    }

    #[test]
    fn test_profile_config_ignores_unknown_fields() {
        let json = json!({
            "provider": "anthropic",
            "unknown_field": "should be ignored",
            "another_unknown": 42
        });

        let config: ProfileConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.provider, Some("anthropic".to_string()));
        // Unknown fields are silently ignored
    }

    #[test]
    fn test_profile_config_partial_fields() {
        let json = json!({
            "provider": "ollama",
            "base_url": "http://localhost:11434"
        });

        let config: ProfileConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.provider, Some("ollama".to_string()));
        assert_eq!(config.base_url, Some("http://localhost:11434".to_string()));
        assert_eq!(config.api_key, None);
        assert_eq!(config.model, None);
        assert_eq!(config.context_window, None);
        assert_eq!(config.max_output_tokens, None);
    }

    #[test]
    fn test_profiles_section_from_value_full() {
        let json = json!({
            "default": "work",
            "work": {
                "provider": "anthropic",
                "api_key": "sk-ant-xxx",
                "model": "claude-sonnet-4-20250514"
            },
            "local": {
                "provider": "ollama",
                "base_url": "http://localhost:11434",
                "model": "llama3"
            }
        });

        let section = ProfilesSection::from_value(&json);
        assert_eq!(section.default, Some("work".to_string()));
        assert_eq!(section.profiles.len(), 2);
        assert_eq!(
            section.profiles["work"].provider,
            Some("anthropic".to_string())
        );
        assert_eq!(
            section.profiles["local"].provider,
            Some("ollama".to_string())
        );
    }

    #[test]
    fn test_profiles_section_from_value_no_default() {
        let json = json!({
            "work": {
                "provider": "openai",
                "api_key": "sk-test"
            }
        });

        let section = ProfilesSection::from_value(&json);
        assert_eq!(section.default, None);
        assert_eq!(section.profiles.len(), 1);
        assert!(section.profiles.contains_key("work"));
    }

    #[test]
    fn test_profiles_section_from_value_not_object() {
        let json = json!("not an object");
        let section = ProfilesSection::from_value(&json);
        assert_eq!(section, ProfilesSection::default());
    }

    #[test]
    fn test_profiles_section_from_value_null() {
        let json = serde_json::Value::Null;
        let section = ProfilesSection::from_value(&json);
        assert_eq!(section, ProfilesSection::default());
    }

    #[test]
    fn test_profiles_section_skips_unparseable_entries() {
        // An array value is not a valid ProfileConfig object
        let json = json!({
            "default": "good",
            "good": { "provider": "openai" },
            "bad": [1, 2, 3]
        });

        let section = ProfilesSection::from_value(&json);
        assert_eq!(section.default, Some("good".to_string()));
        assert_eq!(section.profiles.len(), 1);
        assert!(section.profiles.contains_key("good"));
        assert!(!section.profiles.contains_key("bad"));
    }

    // --- Property-based tests for ProfileConfig deserialization ---

    /// Strategy for generating arbitrary JSON values (limited depth).
    fn arb_json_value() -> impl Strategy<Value = serde_json::Value> {
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            any::<i64>().prop_map(|n| serde_json::Value::Number(serde_json::Number::from(n))),
            "[a-zA-Z0-9 _-]{0,30}".prop_map(|s| serde_json::Value::String(s)),
        ];

        leaf.prop_recursive(
            2,  // depth
            32, // max nodes
            5,  // items per collection
            |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..3)
                        .prop_map(serde_json::Value::Array),
                    prop::collection::hash_map("[a-zA-Z_][a-zA-Z0-9_]{0,8}", inner, 0..3)
                        .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
                ]
            },
        )
    }

    /// Strategy for generating arbitrary ProfileConfig values.
    fn arb_profile_config() -> impl Strategy<Value = ProfileConfig> {
        (
            proptest::option::of("[a-zA-Z][a-zA-Z0-9_-]{0,15}"),        // provider
            proptest::option::of("[a-zA-Z0-9_-]{1,30}"),                 // api_key
            proptest::option::of("https?://[a-z0-9.-]{1,20}(:[0-9]{2,5})?(/[a-z0-9_-]{0,10})*"), // base_url
            proptest::option::of("[a-zA-Z][a-zA-Z0-9._-]{0,30}"),       // model
            proptest::option::of(1usize..1_000_000),                     // context_window
            proptest::option::of(1usize..100_000),                       // max_output_tokens
            prop::collection::hash_map(
                "[a-zA-Z_][a-zA-Z0-9_]{0,10}",
                arb_json_value(),
                0..3,
            ), // extra
        )
            .prop_map(
                |(provider, api_key, base_url, model, context_window, max_output_tokens, extra)| {
                    ProfileConfig {
                        provider,
                        api_key,
                        base_url,
                        model,
                        context_window,
                        max_output_tokens,
                        extra,
                    }
                },
            )
    }

    proptest! {
        /// **Validates: Requirements 1.1, 1.2, 1.3, 1.4**
        ///
        /// Property 1: Profile serialization round-trip
        ///
        /// For any valid ProfileConfig struct, serializing to JSON and deserializing
        /// back SHALL produce an equivalent ProfileConfig.
        #[test]
        fn profile_config_serialization_roundtrip(config in arb_profile_config()) {
            let json_str = serde_json::to_string(&config)
                .expect("ProfileConfig serialization should not fail");
            let deserialized: ProfileConfig = serde_json::from_str(&json_str)
                .expect("ProfileConfig deserialization should not fail");
            prop_assert_eq!(&config, &deserialized);
        }

        /// **Validates: Requirements 1.1, 1.2, 1.3, 1.4**
        ///
        /// Property 2: All field subsets parse successfully
        ///
        /// For any subset of recognized profile fields, a JSON object containing
        /// only those fields SHALL deserialize into a valid ProfileConfig without error.
        #[test]
        fn profile_config_any_field_subset_parses(
            include_provider in any::<bool>(),
            include_api_key in any::<bool>(),
            include_base_url in any::<bool>(),
            include_model in any::<bool>(),
            include_context_window in any::<bool>(),
            include_max_output_tokens in any::<bool>(),
            include_extra in any::<bool>(),
            provider_val in "[a-zA-Z][a-zA-Z0-9_-]{0,10}",
            api_key_val in "[a-zA-Z0-9_-]{1,20}",
            base_url_val in "https?://[a-z0-9.-]{1,15}",
            model_val in "[a-zA-Z][a-zA-Z0-9._-]{0,15}",
            context_window_val in 1usize..500_000,
            max_output_tokens_val in 1usize..50_000,
        ) {
            let mut obj = serde_json::Map::new();

            if include_provider {
                obj.insert("provider".to_string(), json!(provider_val));
            }
            if include_api_key {
                obj.insert("api_key".to_string(), json!(api_key_val));
            }
            if include_base_url {
                obj.insert("base_url".to_string(), json!(base_url_val));
            }
            if include_model {
                obj.insert("model".to_string(), json!(model_val));
            }
            if include_context_window {
                obj.insert("context_window".to_string(), json!(context_window_val));
            }
            if include_max_output_tokens {
                obj.insert("max_output_tokens".to_string(), json!(max_output_tokens_val));
            }
            if include_extra {
                let mut extra_map = serde_json::Map::new();
                extra_map.insert("key".to_string(), json!("value"));
                obj.insert("extra".to_string(), serde_json::Value::Object(extra_map));
            }

            let json_value = serde_json::Value::Object(obj.clone());
            let result = serde_json::from_value::<ProfileConfig>(json_value);
            prop_assert!(result.is_ok(), "Failed to parse subset {:?}: {:?}", obj.keys().collect::<Vec<_>>(), result.err());

            // Verify included fields match
            let config = result.unwrap();
            if include_provider {
                prop_assert_eq!(config.provider, Some(provider_val));
            } else {
                prop_assert_eq!(config.provider, None);
            }
            if include_api_key {
                prop_assert_eq!(config.api_key, Some(api_key_val));
            } else {
                prop_assert_eq!(config.api_key, None);
            }
            if include_base_url {
                prop_assert_eq!(config.base_url, Some(base_url_val));
            } else {
                prop_assert_eq!(config.base_url, None);
            }
            if include_model {
                prop_assert_eq!(config.model, Some(model_val));
            } else {
                prop_assert_eq!(config.model, None);
            }
            if include_context_window {
                prop_assert_eq!(config.context_window, Some(context_window_val));
            } else {
                prop_assert_eq!(config.context_window, None);
            }
            if include_max_output_tokens {
                prop_assert_eq!(config.max_output_tokens, Some(max_output_tokens_val));
            } else {
                prop_assert_eq!(config.max_output_tokens, None);
            }
        }
    }
}
