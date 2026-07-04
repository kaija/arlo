//! Pattern matching for tool permission rules.
//!
//! This module provides glob-based pattern matching for tool names and
//! compound patterns that match tool name + primary argument.
//!
//! # Pattern Forms
//!
//! - **Bare**: A glob pattern applied to tool names (e.g., `fs_*`, `Bash`, `read_?ile`)
//! - **Compound**: `ToolName(arg_glob)` — exact match on tool name + glob on primary argument
//!   (e.g., `Bash(npm*)`, `fs_write(/tmp/*)`)

use std::fmt;

/// A parsed tool permission pattern.
///
/// Supports two forms:
/// - `Bare`: a glob pattern matching tool names (e.g., `fs_*` matches `fs_write`, `fs_read`)
/// - `Compound`: `ToolName(arg_glob)` matching tool name exactly + argument by glob
///
/// # Examples
///
/// ```
/// use agent_core::pattern::ToolPattern;
///
/// let bare = ToolPattern::parse("fs_*").unwrap();
/// assert!(bare.is_glob());
///
/// let compound = ToolPattern::parse("Bash(npm*)").unwrap();
/// assert!(compound.is_glob());
///
/// let exact = ToolPattern::parse("read_file").unwrap();
/// assert!(!exact.is_glob());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolPattern {
    /// A glob pattern applied to tool names only.
    /// E.g., `fs_*` matches `fs_read`, `fs_write`, `fs_append`.
    Bare(String),
    /// A compound pattern: exact tool name + glob on primary argument.
    /// E.g., `Bash(npm*)` matches Bash tool with command starting with "npm".
    Compound {
        tool_name: String,
        arg_glob: String,
    },
}

impl ToolPattern {
    /// Parse a pattern string into a `ToolPattern`.
    ///
    /// Returns `None` if the string is empty or malformed (e.g., unclosed parens,
    /// empty tool name before parens).
    ///
    /// # Syntax
    ///
    /// - Bare: any non-empty string without unbalanced parentheses
    /// - Compound: `Name(arg_glob)` where Name is non-empty and the closing `)` is the last char
    ///
    /// # Examples
    ///
    /// ```
    /// use agent_core::pattern::ToolPattern;
    ///
    /// assert_eq!(
    ///     ToolPattern::parse("fs_*"),
    ///     Some(ToolPattern::Bare("fs_*".to_string()))
    /// );
    /// assert_eq!(
    ///     ToolPattern::parse("Bash(npm*)"),
    ///     Some(ToolPattern::Compound {
    ///         tool_name: "Bash".to_string(),
    ///         arg_glob: "npm*".to_string(),
    ///     })
    /// );
    /// assert_eq!(ToolPattern::parse(""), None);
    /// ```
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }

        // Look for compound pattern: Name(arg_glob)
        if let Some(open_paren) = s.find('(') {
            // Must end with ')'
            if !s.ends_with(')') {
                return None;
            }

            let tool_name = &s[..open_paren];
            if tool_name.is_empty() {
                return None;
            }

            // Extract arg_glob between ( and final )
            let arg_glob = &s[open_paren + 1..s.len() - 1];

            Some(ToolPattern::Compound {
                tool_name: tool_name.to_string(),
                arg_glob: arg_glob.to_string(),
            })
        } else {
            // Check for unmatched closing paren
            if s.contains(')') {
                return None;
            }
            Some(ToolPattern::Bare(s.to_string()))
        }
    }

    /// Returns `true` if this pattern contains any glob metacharacters (`*` or `?`).
    ///
    /// A pattern without metacharacters behaves as an exact match, which preserves
    /// backward compatibility with plain tool name strings.
    pub fn is_glob(&self) -> bool {
        match self {
            ToolPattern::Bare(s) => s.contains('*') || s.contains('?'),
            ToolPattern::Compound { tool_name, arg_glob } => {
                tool_name.contains('*')
                    || tool_name.contains('?')
                    || arg_glob.contains('*')
                    || arg_glob.contains('?')
            }
        }
    }
}

impl ToolPattern {
    /// Check if this pattern matches a tool call.
    ///
    /// For `Bare` patterns, the glob is applied to `tool_name` only — `tool_input` is ignored.
    /// For `Compound` patterns, `tool_name` must match the pattern's tool_name exactly,
    /// and the primary argument (extracted from `tool_input`) must match the `arg_glob`.
    /// Returns `false` for compound patterns when `tool_input` is `None` or contains no
    /// recognized primary argument.
    ///
    /// # Examples
    ///
    /// ```
    /// use agent_core::pattern::ToolPattern;
    /// use serde_json::json;
    ///
    /// let bare = ToolPattern::parse("fs_*").unwrap();
    /// assert!(bare.matches("fs_read", None));
    /// assert!(!bare.matches("bash", None));
    ///
    /// let compound = ToolPattern::parse("Bash(npm*)").unwrap();
    /// let input = json!({"command": "npm install"});
    /// assert!(compound.matches("Bash", Some(&input)));
    /// assert!(!compound.matches("Bash", None));
    /// ```
    pub fn matches(&self, tool_name: &str, tool_input: Option<&serde_json::Value>) -> bool {
        match self {
            ToolPattern::Bare(pattern) => glob_matches(pattern, tool_name),
            ToolPattern::Compound {
                tool_name: pat_tool,
                arg_glob,
            } => {
                // Tool name must match exactly
                if tool_name != pat_tool {
                    return false;
                }
                // Must have tool_input with a recognized primary arg
                match tool_input.and_then(extract_primary_arg) {
                    Some(arg) => glob_matches(arg_glob, arg),
                    None => false,
                }
            }
        }
    }
}

impl fmt::Display for ToolPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ToolPattern::Bare(s) => write!(f, "{}", s),
            ToolPattern::Compound { tool_name, arg_glob } => {
                write!(f, "{}({})", tool_name, arg_glob)
            }
        }
    }
}

/// Extracts the "primary argument" from a tool input JSON for pattern matching.
///
/// Checks well-known keys in order: `command`, `path`, `url`.
/// Returns the string value of the first matching key, or `None` if no recognized
/// key is present or the value is not a string.
///
/// # Examples
///
/// ```
/// use agent_core::pattern::extract_primary_arg;
/// use serde_json::json;
///
/// let input = json!({"command": "npm install", "cwd": "/home"});
/// assert_eq!(extract_primary_arg(&input), Some("npm install"));
///
/// let input = json!({"path": "/tmp/foo.txt"});
/// assert_eq!(extract_primary_arg(&input), Some("/tmp/foo.txt"));
///
/// let input = json!({"url": "https://example.com"});
/// assert_eq!(extract_primary_arg(&input), Some("https://example.com"));
///
/// let input = json!({"other_key": "value"});
/// assert_eq!(extract_primary_arg(&input), None);
/// ```
pub fn extract_primary_arg(tool_input: &serde_json::Value) -> Option<&str> {
    const PRIMARY_KEYS: &[&str] = &["command", "path", "url"];

    let obj = tool_input.as_object()?;
    for key in PRIMARY_KEYS {
        if let Some(val) = obj.get(*key) {
            return val.as_str();
        }
    }
    None
}

/// Evaluate a glob pattern against a text string.
///
/// Supports two metacharacters:
/// - `*` matches zero or more of any character
/// - `?` matches exactly one character
///
/// No other glob features (character classes, escaping, etc.) are supported.
///
/// # Examples
///
/// ```
/// use agent_core::pattern::glob_matches;
///
/// assert!(glob_matches("fs_*", "fs_read"));
/// assert!(glob_matches("fs_*", "fs_"));
/// assert!(!glob_matches("fs_*", "read_file"));
///
/// assert!(glob_matches("?at", "cat"));
/// assert!(glob_matches("?at", "bat"));
/// assert!(!glob_matches("?at", "at"));
///
/// assert!(glob_matches("hello", "hello"));
/// assert!(!glob_matches("hello", "world"));
/// ```
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    // DP approach using two-pointer / recursive with memoization.
    // We use an iterative two-row DP for efficiency.
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let m = pat.len();
    let n = txt.len();

    // dp[j] = whether pattern[0..i] matches text[0..j]
    let mut dp = vec![false; n + 1];
    dp[0] = true; // empty pattern matches empty text

    // Initialize: leading `*`s match empty text
    for i in 0..m {
        if pat[i] == '*' {
            // dp[0] remains true (consecutive stars still match empty)
        } else {
            break;
        }
        // After processing pat[0..=i] where all are '*', dp[0] = true
    }

    // Actually, let's use a cleaner two-row DP:
    // prev[j] = does pattern[0..i] match text[0..j]
    // curr[j] = does pattern[0..i+1] match text[0..j]
    let mut prev = vec![false; n + 1];
    prev[0] = true; // empty pattern matches empty text

    for i in 0..m {
        let mut curr = vec![false; n + 1];

        if pat[i] == '*' {
            // '*' matches empty (curr[0] = prev[0]) or extends a match
            curr[0] = prev[0];
            for j in 1..=n {
                // '*' matches zero chars (prev[j]) or one more char (curr[j-1])
                curr[j] = prev[j] || curr[j - 1];
            }
        } else if pat[i] == '?' {
            // '?' matches exactly one character
            curr[0] = false;
            for j in 1..=n {
                curr[j] = prev[j - 1]; // consume one character
            }
        } else {
            // Literal character
            curr[0] = false;
            for j in 1..=n {
                curr[j] = prev[j - 1] && pat[i] == txt[j - 1];
            }
        }

        prev = curr;
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ToolPattern::parse tests ---

    #[test]
    fn parse_empty_returns_none() {
        assert_eq!(ToolPattern::parse(""), None);
        assert_eq!(ToolPattern::parse("  "), None);
    }

    #[test]
    fn parse_bare_simple() {
        assert_eq!(
            ToolPattern::parse("read_file"),
            Some(ToolPattern::Bare("read_file".to_string()))
        );
    }

    #[test]
    fn parse_bare_with_glob() {
        assert_eq!(
            ToolPattern::parse("fs_*"),
            Some(ToolPattern::Bare("fs_*".to_string()))
        );
        assert_eq!(
            ToolPattern::parse("read_?ile"),
            Some(ToolPattern::Bare("read_?ile".to_string()))
        );
    }

    #[test]
    fn parse_compound_simple() {
        assert_eq!(
            ToolPattern::parse("Bash(npm*)"),
            Some(ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "npm*".to_string(),
            })
        );
    }

    #[test]
    fn parse_compound_with_path() {
        assert_eq!(
            ToolPattern::parse("fs_write(/tmp/*)"),
            Some(ToolPattern::Compound {
                tool_name: "fs_write".to_string(),
                arg_glob: "/tmp/*".to_string(),
            })
        );
    }

    #[test]
    fn parse_compound_empty_arg_glob() {
        assert_eq!(
            ToolPattern::parse("Bash()"),
            Some(ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "".to_string(),
            })
        );
    }

    #[test]
    fn parse_compound_unclosed_paren_returns_none() {
        assert_eq!(ToolPattern::parse("Bash(npm*"), None);
    }

    #[test]
    fn parse_compound_empty_tool_name_returns_none() {
        assert_eq!(ToolPattern::parse("(npm*)"), None);
    }

    #[test]
    fn parse_bare_with_unmatched_close_paren_returns_none() {
        assert_eq!(ToolPattern::parse("foo)"), None);
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(
            ToolPattern::parse("  fs_*  "),
            Some(ToolPattern::Bare("fs_*".to_string()))
        );
    }

    #[test]
    fn parse_compound_with_spaces_in_arg() {
        assert_eq!(
            ToolPattern::parse("Bash(cargo build*)"),
            Some(ToolPattern::Compound {
                tool_name: "Bash".to_string(),
                arg_glob: "cargo build*".to_string(),
            })
        );
    }

    // --- ToolPattern::is_glob tests ---

    #[test]
    fn is_glob_bare_no_meta() {
        let p = ToolPattern::Bare("read_file".to_string());
        assert!(!p.is_glob());
    }

    #[test]
    fn is_glob_bare_star() {
        let p = ToolPattern::Bare("fs_*".to_string());
        assert!(p.is_glob());
    }

    #[test]
    fn is_glob_bare_question() {
        let p = ToolPattern::Bare("read_?ile".to_string());
        assert!(p.is_glob());
    }

    #[test]
    fn is_glob_compound_no_meta() {
        let p = ToolPattern::Compound {
            tool_name: "Bash".to_string(),
            arg_glob: "npm".to_string(),
        };
        assert!(!p.is_glob());
    }

    #[test]
    fn is_glob_compound_star_in_arg() {
        let p = ToolPattern::Compound {
            tool_name: "Bash".to_string(),
            arg_glob: "npm*".to_string(),
        };
        assert!(p.is_glob());
    }

    #[test]
    fn is_glob_compound_question_in_tool_name() {
        let p = ToolPattern::Compound {
            tool_name: "fs_?".to_string(),
            arg_glob: "path".to_string(),
        };
        assert!(p.is_glob());
    }

    // --- Display tests ---

    #[test]
    fn display_bare() {
        let p = ToolPattern::Bare("fs_*".to_string());
        assert_eq!(p.to_string(), "fs_*");
    }

    #[test]
    fn display_compound() {
        let p = ToolPattern::Compound {
            tool_name: "Bash".to_string(),
            arg_glob: "npm*".to_string(),
        };
        assert_eq!(p.to_string(), "Bash(npm*)");
    }

    #[test]
    fn display_round_trips_through_parse() {
        let cases = vec!["fs_*", "read_file", "Bash(npm*)", "fs_write(/tmp/*)"];
        for s in cases {
            let parsed = ToolPattern::parse(s).unwrap();
            assert_eq!(parsed.to_string(), s);
        }
    }

    // --- glob_matches tests ---

    #[test]
    fn glob_exact_match() {
        assert!(glob_matches("hello", "hello"));
        assert!(!glob_matches("hello", "world"));
        assert!(!glob_matches("hello", "hell"));
        assert!(!glob_matches("hello", "helloo"));
    }

    #[test]
    fn glob_star_matches_empty() {
        assert!(glob_matches("fs_*", "fs_"));
    }

    #[test]
    fn glob_star_matches_many() {
        assert!(glob_matches("fs_*", "fs_read"));
        assert!(glob_matches("fs_*", "fs_write_all"));
    }

    #[test]
    fn glob_star_at_start() {
        assert!(glob_matches("*file", "file"));
        assert!(glob_matches("*file", "read_file"));
        assert!(!glob_matches("*file", "files"));
    }

    #[test]
    fn glob_star_in_middle() {
        assert!(glob_matches("f*e", "fe"));
        assert!(glob_matches("f*e", "file"));
        assert!(glob_matches("f*e", "foobare"));
        assert!(!glob_matches("f*e", "f"));
    }

    #[test]
    fn glob_multiple_stars() {
        assert!(glob_matches("*a*", "a"));
        assert!(glob_matches("*a*", "abc"));
        assert!(glob_matches("*a*", "xax"));
        assert!(!glob_matches("*a*", "xyz"));
    }

    #[test]
    fn glob_question_matches_one() {
        assert!(glob_matches("?at", "cat"));
        assert!(glob_matches("?at", "bat"));
        assert!(!glob_matches("?at", "at"));
        assert!(!glob_matches("?at", "chat"));
    }

    #[test]
    fn glob_question_and_star() {
        assert!(glob_matches("?s_*", "fs_read"));
        assert!(glob_matches("?s_*", "xs_"));
        assert!(!glob_matches("?s_*", "s_read"));
    }

    #[test]
    fn glob_all_star() {
        assert!(glob_matches("*", ""));
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("**", "anything"));
    }

    #[test]
    fn glob_empty_pattern_matches_empty_text() {
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "nonempty"));
    }

    #[test]
    fn glob_no_meta_exact_only() {
        assert!(glob_matches("read_file", "read_file"));
        assert!(!glob_matches("read_file", "read_files"));
        assert!(!glob_matches("read_file", "read_fil"));
    }

    #[test]
    fn glob_complex_patterns() {
        assert!(glob_matches("npm*install*", "npm install"));
        assert!(glob_matches("npm*install*", "npm run install foo"));
        assert!(!glob_matches("npm*install*", "yarn install"));
    }

    // --- extract_primary_arg tests ---

    #[test]
    fn extract_primary_arg_command_key() {
        let input = serde_json::json!({"command": "npm install", "cwd": "/home"});
        assert_eq!(extract_primary_arg(&input), Some("npm install"));
    }

    #[test]
    fn extract_primary_arg_path_key() {
        let input = serde_json::json!({"path": "/tmp/foo.txt", "content": "hello"});
        assert_eq!(extract_primary_arg(&input), Some("/tmp/foo.txt"));
    }

    #[test]
    fn extract_primary_arg_url_key() {
        let input = serde_json::json!({"url": "https://example.com", "method": "GET"});
        assert_eq!(extract_primary_arg(&input), Some("https://example.com"));
    }

    #[test]
    fn extract_primary_arg_priority_order() {
        // command takes priority over path
        let input = serde_json::json!({"command": "echo hi", "path": "/tmp"});
        assert_eq!(extract_primary_arg(&input), Some("echo hi"));

        // path takes priority over url
        let input = serde_json::json!({"path": "/tmp/file", "url": "https://example.com"});
        assert_eq!(extract_primary_arg(&input), Some("/tmp/file"));
    }

    #[test]
    fn extract_primary_arg_no_recognized_key() {
        let input = serde_json::json!({"foo": "bar", "baz": 42});
        assert_eq!(extract_primary_arg(&input), None);
    }

    #[test]
    fn extract_primary_arg_non_string_value() {
        let input = serde_json::json!({"command": 42});
        assert_eq!(extract_primary_arg(&input), None);
    }

    #[test]
    fn extract_primary_arg_null_value() {
        let input = serde_json::json!({"command": null});
        assert_eq!(extract_primary_arg(&input), None);
    }

    #[test]
    fn extract_primary_arg_non_object() {
        let input = serde_json::json!("just a string");
        assert_eq!(extract_primary_arg(&input), None);

        let input = serde_json::json!([1, 2, 3]);
        assert_eq!(extract_primary_arg(&input), None);

        let input = serde_json::json!(null);
        assert_eq!(extract_primary_arg(&input), None);
    }

    #[test]
    fn extract_primary_arg_empty_object() {
        let input = serde_json::json!({});
        assert_eq!(extract_primary_arg(&input), None);
    }

    // --- ToolPattern::matches tests ---

    #[test]
    fn matches_bare_pattern_tool_name_only() {
        let p = ToolPattern::parse("fs_*").unwrap();
        assert!(p.matches("fs_read", None));
        assert!(p.matches("fs_write", None));
        assert!(!p.matches("bash", None));
    }

    #[test]
    fn matches_bare_exact_no_glob() {
        let p = ToolPattern::parse("read_file").unwrap();
        assert!(p.matches("read_file", None));
        assert!(!p.matches("read_files", None));
        assert!(!p.matches("write_file", None));
    }

    #[test]
    fn matches_bare_ignores_tool_input() {
        let p = ToolPattern::parse("fs_*").unwrap();
        let input = serde_json::json!({"path": "/tmp/secret"});
        // Bare patterns match on tool name regardless of input
        assert!(p.matches("fs_read", Some(&input)));
        assert!(p.matches("fs_read", None));
    }

    #[test]
    fn matches_compound_exact_tool_name_and_arg_glob() {
        let p = ToolPattern::parse("Bash(npm*)").unwrap();
        let input = serde_json::json!({"command": "npm install"});
        assert!(p.matches("Bash", Some(&input)));

        let input = serde_json::json!({"command": "npm run build"});
        assert!(p.matches("Bash", Some(&input)));

        let input = serde_json::json!({"command": "cargo build"});
        assert!(!p.matches("Bash", Some(&input)));
    }

    #[test]
    fn matches_compound_tool_name_case_sensitive() {
        let p = ToolPattern::parse("Bash(npm*)").unwrap();
        let input = serde_json::json!({"command": "npm install"});
        assert!(!p.matches("bash", Some(&input))); // case mismatch
        assert!(!p.matches("BASH", Some(&input))); // case mismatch
    }

    #[test]
    fn matches_compound_returns_false_when_no_input() {
        let p = ToolPattern::parse("Bash(npm*)").unwrap();
        assert!(!p.matches("Bash", None));
    }

    #[test]
    fn matches_compound_returns_false_when_no_primary_arg() {
        let p = ToolPattern::parse("Bash(npm*)").unwrap();
        let input = serde_json::json!({"other_field": "npm install"});
        assert!(!p.matches("Bash", Some(&input)));
    }

    #[test]
    fn matches_compound_with_path_arg() {
        let p = ToolPattern::parse("fs_write(/tmp/*)").unwrap();
        let input = serde_json::json!({"path": "/tmp/foo.txt"});
        assert!(p.matches("fs_write", Some(&input)));

        let input = serde_json::json!({"path": "/home/user/file.txt"});
        assert!(!p.matches("fs_write", Some(&input)));
    }

    #[test]
    fn matches_compound_with_url_arg() {
        let p = ToolPattern::parse("web_fetch(https://example.com/*)").unwrap();
        let input = serde_json::json!({"url": "https://example.com/page"});
        assert!(p.matches("web_fetch", Some(&input)));

        let input = serde_json::json!({"url": "https://other.com/page"});
        assert!(!p.matches("web_fetch", Some(&input)));
    }

    #[test]
    fn matches_compound_wrong_tool_name() {
        let p = ToolPattern::parse("Bash(npm*)").unwrap();
        let input = serde_json::json!({"command": "npm install"});
        assert!(!p.matches("fs_write", Some(&input)));
    }

    #[test]
    fn matches_compound_empty_arg_glob_matches_empty_arg() {
        let p = ToolPattern::parse("Bash()").unwrap();
        let input = serde_json::json!({"command": ""});
        assert!(p.matches("Bash", Some(&input)));

        let input = serde_json::json!({"command": "anything"});
        assert!(!p.matches("Bash", Some(&input)));
    }

    #[test]
    fn matches_bare_question_mark_glob() {
        let p = ToolPattern::parse("read_?ile").unwrap();
        assert!(p.matches("read_file", None));
        assert!(p.matches("read_bile", None));
        assert!(!p.matches("read_ile", None));
        assert!(!p.matches("read_ffile", None));
    }

    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        /// Strategy for valid tool names: starts with a letter, followed by alphanumeric/underscore.
        fn tool_name_strategy() -> impl Strategy<Value = String> {
            "[a-zA-Z][a-zA-Z0-9_]{0,19}"
        }

        /// Strategy for arbitrary non-empty argument strings.
        fn arg_string_strategy() -> impl Strategy<Value = String> {
            "[a-zA-Z0-9/_. -]{1,40}"
        }

        /// Strategy for strings that contain NO glob metacharacters (* or ?).
        fn exact_string_strategy() -> impl Strategy<Value = String> {
            "[a-zA-Z][a-zA-Z0-9_]{0,19}"
        }

        // ===================================================================
        // Property 6: Glob Pattern Matching Correctness
        // **Validates: Requirements 4.1, 4.2, 4.3, 4.4**
        // ===================================================================

        proptest! {
            /// Property 6a: `*` matches zero or more characters.
            ///
            /// For any prefix and suffix, `prefix*` matches `prefix` concatenated
            /// with any string (including empty).
            ///
            /// **Validates: Requirements 4.1, 4.2, 4.3, 4.4**
            #[test]
            fn glob_star_matches_zero_or_more(
                prefix in "[a-zA-Z_]{1,10}",
                suffix in "[a-zA-Z0-9_]{0,20}",
            ) {
                let pattern = format!("{}*", prefix);
                let text = format!("{}{}", prefix, suffix);
                prop_assert!(
                    glob_matches(&pattern, &text),
                    "Pattern '{}' should match text '{}'", pattern, text
                );
            }

            /// Property 6b: `?` matches exactly one character.
            ///
            /// For any prefix and single char, `prefix?suffix` matches
            /// `prefix` + exactly one char + `suffix`.
            ///
            /// **Validates: Requirements 4.1, 4.2, 4.3, 4.4**
            #[test]
            fn glob_question_matches_exactly_one(
                prefix in "[a-zA-Z]{0,5}",
                middle_char in "[a-zA-Z0-9]",
                suffix in "[a-zA-Z]{0,5}",
            ) {
                let pattern = format!("{}?{}", prefix, suffix);
                let text = format!("{}{}{}", prefix, middle_char, suffix);
                prop_assert!(
                    glob_matches(&pattern, &text),
                    "Pattern '{}' should match text '{}'", pattern, text
                );
            }

            /// Property 6c: `?` does NOT match zero characters.
            ///
            /// A pattern with `?` requires exactly one character at that position,
            /// so the text without that character position should NOT match.
            ///
            /// **Validates: Requirements 4.1, 4.2, 4.3, 4.4**
            #[test]
            fn glob_question_does_not_match_zero_chars(
                prefix in "[a-zA-Z]{1,5}",
                suffix in "[a-zA-Z]{1,5}",
            ) {
                let pattern = format!("{}?{}", prefix, suffix);
                // Text is just prefix + suffix (missing the char for '?')
                let text = format!("{}{}", prefix, suffix);
                prop_assert!(
                    !glob_matches(&pattern, &text),
                    "Pattern '{}' should NOT match text '{}' (? requires exactly one char)",
                    pattern, text
                );
            }

            /// Property 6d: Exact strings (no metacharacters) match only themselves.
            ///
            /// For any string without `*` or `?`, glob_matches should behave as
            /// strict equality.
            ///
            /// **Validates: Requirements 4.1, 4.2, 4.3, 4.4**
            #[test]
            fn glob_exact_matches_only_itself(
                text in exact_string_strategy(),
                other in exact_string_strategy(),
            ) {
                // The pattern is the text itself (no metacharacters)
                prop_assert!(
                    glob_matches(&text, &text),
                    "Exact pattern '{}' should match itself", text
                );
                // If other != text, it should not match
                if text != other {
                    prop_assert!(
                        !glob_matches(&text, &other),
                        "Exact pattern '{}' should NOT match different text '{}'",
                        text, other
                    );
                }
            }

            /// Property 6e: `*` at end matches any continuation of the prefix.
            ///
            /// Ensures `prefix*` does NOT match text that doesn't start with prefix.
            ///
            /// **Validates: Requirements 4.1, 4.2, 4.3, 4.4**
            #[test]
            fn glob_star_requires_prefix(
                prefix in "[a-zA-Z]{2,8}",
                non_prefix in "[0-9]{2,8}",
                suffix in "[a-zA-Z0-9]{0,10}",
            ) {
                let pattern = format!("{}*", prefix);
                // Text that does NOT start with prefix should not match
                let text = format!("{}{}", non_prefix, suffix);
                prop_assert!(
                    !glob_matches(&pattern, &text),
                    "Pattern '{}' should NOT match text '{}' (different prefix)",
                    pattern, text
                );
            }
        }

        // ===================================================================
        // Property 7: Missing Primary Argument Rejects Compound Patterns
        // **Validates: Requirements 4.5, 4.6, 9.5**
        // ===================================================================

        proptest! {
            /// Property 7: Compound patterns never match when the primary argument is absent.
            ///
            /// For any compound pattern `ToolName(arg_glob)`, if the tool_input JSON
            /// does not contain a recognized primary argument key (command, path, url),
            /// then matches() must return false.
            ///
            /// **Validates: Requirements 4.5, 4.6, 9.5**
            #[test]
            fn compound_pattern_rejects_missing_primary_arg(
                tool_name in tool_name_strategy(),
                arg_glob in "[a-zA-Z0-9/*?]{1,15}",
                unrecognized_key in "(foo|bar|baz|data|content|output|result)",
                value in arg_string_strategy(),
            ) {
                let pattern = ToolPattern::Compound {
                    tool_name: tool_name.clone(),
                    arg_glob: arg_glob.clone(),
                };

                // Case 1: tool_input is None
                prop_assert!(
                    !pattern.matches(&tool_name, None),
                    "Compound pattern '{}'('{}') should NOT match when tool_input is None",
                    tool_name, arg_glob
                );

                // Case 2: tool_input has no recognized primary key
                let input = serde_json::json!({ unrecognized_key.as_str(): value });
                prop_assert!(
                    !pattern.matches(&tool_name, Some(&input)),
                    "Compound pattern '{}'('{}') should NOT match when input has no recognized primary arg (key='{}')",
                    tool_name, arg_glob, unrecognized_key
                );
            }
        }

        // ===================================================================
        // Property 8: Bare Exact String Backward Compatibility
        // **Validates: Requirements 4.5, 4.6, 9.5**
        // ===================================================================

        proptest! {
            /// Property 8: Bare strings without metacharacters behave as exact match only.
            ///
            /// For any string without `*` or `?`, a Bare pattern matches ONLY the
            /// exact tool name and nothing else. This preserves backward compatibility
            /// with existing static_allow and static_deny lists.
            ///
            /// **Validates: Requirements 4.5, 4.6, 9.5**
            #[test]
            fn bare_exact_string_matches_only_itself(
                tool_name in exact_string_strategy(),
                other_name in exact_string_strategy(),
            ) {
                let pattern = ToolPattern::Bare(tool_name.clone());

                // Should not be a glob
                prop_assert!(
                    !pattern.is_glob(),
                    "Bare pattern '{}' without metacharacters should not be a glob", tool_name
                );

                // Matches itself
                prop_assert!(
                    pattern.matches(&tool_name, None),
                    "Bare exact pattern '{}' should match itself", tool_name
                );

                // Does not match a different name
                if tool_name != other_name {
                    prop_assert!(
                        !pattern.matches(&other_name, None),
                        "Bare exact pattern '{}' should NOT match different name '{}'",
                        tool_name, other_name
                    );
                }
            }
        }

        // ===================================================================
        // Property 13: No-Input Falls Back to Name-Only Matching
        // **Validates: Requirements 4.5, 4.6, 9.5**
        // ===================================================================

        proptest! {
            /// Property 13: Bare patterns still match tool names when tool_input is None.
            ///
            /// A Bare pattern (whether exact or glob) should match based solely on
            /// the tool name, regardless of whether tool_input is provided or not.
            /// This ensures backward compatibility: existing permission checks that
            /// don't pass tool_input still work correctly.
            ///
            /// **Validates: Requirements 4.5, 4.6, 9.5**
            #[test]
            fn bare_pattern_matches_without_input(
                base_name in "[a-zA-Z]{2,8}",
                suffix in "[a-zA-Z0-9_]{0,10}",
            ) {
                let tool_name = format!("{}{}", base_name, suffix);
                let pattern_str = format!("{}*", base_name);
                let pattern = ToolPattern::Bare(pattern_str.clone());

                // Bare pattern matches on tool name when tool_input is None
                prop_assert!(
                    pattern.matches(&tool_name, None),
                    "Bare pattern '{}' should match tool name '{}' when tool_input is None",
                    pattern_str, tool_name
                );

                // Also matches when tool_input is Some (bare ignores input)
                let input = serde_json::json!({"command": "anything"});
                prop_assert!(
                    pattern.matches(&tool_name, Some(&input)),
                    "Bare pattern '{}' should match tool name '{}' even when tool_input is Some",
                    pattern_str, tool_name
                );
            }

            /// Property 13b: Bare exact patterns match tool names when tool_input is None.
            ///
            /// Even without any glob metacharacter, a bare pattern should match the
            /// tool name when no input is available — confirming name-only fallback.
            ///
            /// **Validates: Requirements 4.5, 4.6, 9.5**
            #[test]
            fn bare_exact_pattern_matches_name_only_no_input(
                tool_name in tool_name_strategy(),
            ) {
                let pattern = ToolPattern::Bare(tool_name.clone());

                // Matches with None input
                prop_assert!(
                    pattern.matches(&tool_name, None),
                    "Bare exact pattern '{}' should match tool name when tool_input is None",
                    tool_name
                );
            }
        }
    }
}
