//! Configuration for the memory compaction layer system.

use std::path::PathBuf;

/// Configuration for the compaction layer system.
#[derive(Debug, Clone)]
pub struct CompactionLayerConfig {
    /// Tool names eligible for tools_compact clearing.
    /// Default: ["file_read", "shell", "grep", "glob", "web_fetch"]
    pub compactable_tools: Vec<String>,

    /// Number of most recent turns exempt from tools_compact clearing.
    /// Default: 5
    pub tools_compact_exempt_turns: u32,

    /// Number of recent non-system messages preserved by Full Summarize.
    /// Default: 10
    pub preserve_recent_messages: usize,

    /// Token buffer subtracted from effective context window to get trigger threshold.
    /// Default: 13_000
    pub trigger_buffer: usize,

    /// Maximum tokens of recent messages preserved by Session Memory layer.
    /// Default: 40_000
    pub session_memory_max_preserved_tokens: usize,

    /// Minimum token count before Session Memory layer activates.
    /// Default: 10_000
    pub session_memory_min_tokens: usize,

    /// Minimum text-block messages before Session Memory layer activates.
    /// Default: 5
    pub session_memory_min_messages: usize,

    /// Optional model name for Full Summarize. If None, uses the run's primary model.
    pub summary_model: Option<String>,

    /// Path to the session memory file, if one exists.
    pub session_memory_path: Option<PathBuf>,
}

impl Default for CompactionLayerConfig {
    fn default() -> Self {
        Self {
            compactable_tools: vec![
                "file_read".into(),
                "shell".into(),
                "grep".into(),
                "glob".into(),
                "web_fetch".into(),
            ],
            tools_compact_exempt_turns: 5,
            preserve_recent_messages: 10,
            trigger_buffer: 13_000,
            session_memory_max_preserved_tokens: 40_000,
            session_memory_min_tokens: 10_000,
            session_memory_min_messages: 5,
            summary_model: None,
            session_memory_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_compactable_tools() {
        let config = CompactionLayerConfig::default();
        assert_eq!(
            config.compactable_tools,
            vec!["file_read", "shell", "grep", "glob", "web_fetch"]
        );
    }

    #[test]
    fn default_numeric_values() {
        let config = CompactionLayerConfig::default();
        assert_eq!(config.tools_compact_exempt_turns, 5);
        assert_eq!(config.preserve_recent_messages, 10);
        assert_eq!(config.trigger_buffer, 13_000);
        assert_eq!(config.session_memory_max_preserved_tokens, 40_000);
        assert_eq!(config.session_memory_min_tokens, 10_000);
        assert_eq!(config.session_memory_min_messages, 5);
    }

    #[test]
    fn default_optional_fields_are_none() {
        let config = CompactionLayerConfig::default();
        assert!(config.summary_model.is_none());
        assert!(config.session_memory_path.is_none());
    }
}
