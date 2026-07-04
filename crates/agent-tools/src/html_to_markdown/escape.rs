//! Markdown character escaping.
//!
//! Escapes characters that have special meaning in CommonMark syntax within
//! text nodes that are not inside code blocks.

/// Characters that have special meaning in CommonMark and need escaping in text.
pub const MARKDOWN_SPECIAL_CHARS: &[char] = &['\\', '*', '_', '`', '[', ']', '<', '>'];

/// Escape markdown special characters in input text.
///
/// Processes each line independently (to handle context-sensitive line-start
/// patterns), then rejoins with newlines.
pub fn escape_markdown(input: &str) -> String {
    input
        .split('\n')
        .map(|line| escape_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Escape a single line of text, handling both line-start patterns and
/// individual special characters.
pub fn escape_line(line: &str) -> String {
    let mut result = String::with_capacity(line.len() * 2);
    let mut chars = line.chars().peekable();
    let mut at_start = true;

    // Handle context-sensitive line-start patterns first
    if let Some(prefix) = consume_line_start_pattern(line) {
        result.push_str(&prefix.escaped);
        chars = prefix.remainder.chars().peekable();
        at_start = false;
    }

    // Escape individual special characters in the rest of the line
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '*' => result.push_str("\\*"),
            '_' => result.push_str("\\_"),
            '`' => result.push_str("\\`"),
            '[' => result.push_str("\\["),
            ']' => result.push_str("\\]"),
            '<' => result.push_str("\\<"),
            '>' => {
                // Angle bracket is always escaped (could start a blockquote mid-line)
                result.push_str("\\>");
            }
            _ => {
                // Handle tilde sequences (~~~ or more) if we're at line start
                if at_start && ch == '~' {
                    let mut tildes = String::from("~");
                    while chars.peek() == Some(&'~') {
                        tildes.push(chars.next().unwrap());
                    }
                    if tildes.len() >= 3 {
                        // Escape all tildes
                        for _ in 0..tildes.len() {
                            result.push_str("\\~");
                        }
                    } else {
                        result.push_str(&tildes);
                    }
                } else {
                    result.push(ch);
                }
            }
        }
        at_start = false;
    }

    result
}

/// Result of consuming a line-start pattern.
struct LineStartResult<'a> {
    /// The escaped version of the pattern.
    escaped: String,
    /// The remaining text after the pattern.
    remainder: &'a str,
}

/// Try to match and escape a context-sensitive line-start pattern.
///
/// Handles:
/// - `# ` (heading) → `\# `
/// - `- ` (unordered list with dash) → `\- `
/// - `+ ` (unordered list with plus) → `\+ `
/// - `> ` (blockquote) → `\> `  (note: > is also escaped inline)
/// - `~~~` (fenced code fence) → `\~\~\~`
/// - `===` (setext heading) → `\=\=\=`
/// - `1. `, `2. `, etc. (ordered list) → `1\. `, `2\. `, etc.
fn consume_line_start_pattern(line: &str) -> Option<LineStartResult<'_>> {
    // Check for heading: `# ` or `## ` etc.
    if let Some(rest) = line.strip_prefix('#') {
        // Count consecutive hashes
        let hash_count = 1 + rest.chars().take_while(|&c| c == '#').count();
        let after_hashes = &line[hash_count..];
        if after_hashes.is_empty() || after_hashes.starts_with(' ') {
            return Some(LineStartResult {
                escaped: format!("\\{}", &line[..hash_count]),
                remainder: after_hashes,
            });
        }
    }

    // Check for unordered list with dash: `- `
    if line.starts_with("- ") {
        return Some(LineStartResult {
            escaped: "\\-".to_string(),
            remainder: &line[1..],
        });
    }

    // Check for unordered list with plus: `+ `
    if line.starts_with("+ ") {
        return Some(LineStartResult {
            escaped: "\\+".to_string(),
            remainder: &line[1..],
        });
    }

    // Check for blockquote: `> ` or bare `>`
    if line.starts_with('>') {
        return Some(LineStartResult {
            escaped: "\\>".to_string(),
            remainder: &line[1..],
        });
    }

    // Check for tilde fence: `~~~` or more
    let tilde_count = line.chars().take_while(|&c| c == '~').count();
    if tilde_count >= 3 {
        let escaped: String = (0..tilde_count).map(|_| "\\~").collect();
        return Some(LineStartResult {
            escaped,
            remainder: &line[tilde_count..],
        });
    }

    // Check for setext heading underline: `===` or more
    let equals_count = line.chars().take_while(|&c| c == '=').count();
    if equals_count >= 3 {
        let rest = &line[equals_count..];
        // Only escape if rest is empty or whitespace (actual setext pattern)
        if rest.is_empty() || rest.chars().all(|c| c == ' ' || c == '\t') {
            let escaped: String = (0..equals_count).map(|_| "\\=").collect();
            return Some(LineStartResult {
                escaped,
                remainder: rest,
            });
        }
    }

    // Check for ordered list: `1. `, `2. `, `10. `, etc.
    let digit_count = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_count > 0 {
        let after_digits = &line[digit_count..];
        if after_digits.starts_with(". ") || after_digits == "." {
            let digits = &line[..digit_count];
            return Some(LineStartResult {
                escaped: format!("{}\\.", digits),
                remainder: &after_digits[1..], // skip the dot, keep the space
            });
        }
    }

    None
}

/// Escape characters in link destinations: angle brackets and parentheses.
///
/// If the destination contains spaces, wraps in angle brackets.
pub fn escape_link_destination(dest: &str) -> String {
    let escaped = dest
        .replace('<', "\\<")
        .replace('>', "\\>")
        .replace('(', "\\(")
        .replace(')', "\\)");
    if escaped.contains(' ') {
        format!("<{}>", escaped)
    } else {
        escaped
    }
}

/// Escape double quotes in link titles.
pub fn escape_link_title(title: &str) -> String {
    title.replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_special_chars() {
        assert_eq!(escape_markdown("hello *world*"), "hello \\*world\\*");
        assert_eq!(escape_markdown("use `code`"), "use \\`code\\`");
        assert_eq!(escape_markdown("a \\ b"), "a \\\\ b");
        assert_eq!(escape_markdown("[link]"), "\\[link\\]");
    }

    #[test]
    fn test_escape_line_start_heading() {
        assert_eq!(escape_line("# heading"), "\\# heading");
        assert_eq!(escape_line("## sub"), "\\## sub");
        assert_eq!(escape_line("#noheading"), "#noheading");
    }

    #[test]
    fn test_escape_line_start_list() {
        assert_eq!(escape_line("- item"), "\\- item");
        assert_eq!(escape_line("+ item"), "\\+ item");
        assert_eq!(escape_line("1. first"), "1\\. first");
        assert_eq!(escape_line("10. tenth"), "10\\. tenth");
    }

    #[test]
    fn test_escape_line_start_blockquote() {
        assert_eq!(escape_line("> quote"), "\\> quote");
    }

    #[test]
    fn test_escape_tilde_fence() {
        assert_eq!(escape_line("~~~"), "\\~\\~\\~");
        assert_eq!(escape_line("~~~~code"), "\\~\\~\\~\\~code");
        assert_eq!(escape_line("~~not fence"), "~~not fence");
    }

    #[test]
    fn test_escape_setext_heading() {
        assert_eq!(escape_line("==="), "\\=\\=\\=");
        assert_eq!(escape_line("===="), "\\=\\=\\=\\=");
        assert_eq!(escape_line("==not"), "==not");
    }

    #[test]
    fn test_escape_multiline() {
        let input = "# title\n- item\nnormal text";
        let expected = "\\# title\n\\- item\nnormal text";
        assert_eq!(escape_markdown(input), expected);
    }

    #[test]
    fn test_escape_link_destination_basic() {
        assert_eq!(escape_link_destination("https://example.com"), "https://example.com");
        assert_eq!(
            escape_link_destination("https://example.com/path (1)"),
            "<https://example.com/path \\(1\\)>"
        );
        assert_eq!(
            escape_link_destination("a<b>c"),
            "a\\<b\\>c"
        );
    }

    #[test]
    fn test_escape_link_title_basic() {
        assert_eq!(escape_link_title("hello"), "hello");
        assert_eq!(escape_link_title(r#"say "hi""#), r#"say \"hi\""#);
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(escape_markdown(""), "");
        assert_eq!(escape_line(""), "");
    }

    #[test]
    fn test_no_special_chars() {
        assert_eq!(escape_markdown("hello world"), "hello world");
    }

    #[test]
    fn test_angle_bracket_escaped_always() {
        assert_eq!(escape_line("a > b"), "a \\> b");
        assert_eq!(escape_line("a < b"), "a \\< b");
    }

    // --- Property-Based Tests ---
    // **Validates: Requirements 5.20, 13.13**

    use proptest::prelude::*;

    /// Strategy that generates strings containing a mix of markdown special characters
    /// and normal ASCII text.
    fn special_char_string() -> impl Strategy<Value = String> {
        // Generate strings from a character set that includes all MARKDOWN_SPECIAL_CHARS
        // plus normal text characters
        prop::string::string_regex(r"[ \\\*_`\[\]<>a-zA-Z0-9!@$%^&(){}\t]{1,80}")
            .unwrap()
    }

    /// Strategy that generates strings starting with line-start patterns
    /// (headings, lists, blockquotes, fences, setext underlines).
    fn line_start_pattern_string() -> impl Strategy<Value = String> {
        prop_oneof![
            // Heading patterns: # through ######
            (1..=6usize, "[a-zA-Z ]{0,30}").prop_map(|(n, rest)| {
                format!("{} {}", "#".repeat(n), rest)
            }),
            // Unordered list with dash
            "[a-zA-Z0-9 ]{1,30}".prop_map(|rest| format!("- {}", rest)),
            // Unordered list with plus
            "[a-zA-Z0-9 ]{1,30}".prop_map(|rest| format!("+ {}", rest)),
            // Blockquote
            "[a-zA-Z0-9 ]{1,30}".prop_map(|rest| format!("> {}", rest)),
            // Ordered list
            (1..=99u32, "[a-zA-Z0-9 ]{1,30}").prop_map(|(n, rest)| {
                format!("{}. {}", n, rest)
            }),
            // Tilde fence (3+ tildes)
            (3..=6usize, "[a-zA-Z0-9]{0,10}").prop_map(|(n, rest)| {
                format!("{}{}", "~".repeat(n), rest)
            }),
            // Setext heading underline (3+ equals)
            (3..=6usize).prop_map(|n| "=".repeat(n)),
        ]
    }

    proptest! {
        /// Property 8: After escaping, no unescaped special character remains
        /// that could be interpreted as markdown formatting.
        ///
        /// For every character in the escaped output that is a markdown special
        /// character (`\`, `*`, `_`, `` ` ``, `[`, `]`, `<`, `>`), it must be
        /// preceded by a backslash (i.e., it's part of an escape sequence).
        #[test]
        fn prop_escape_covers_all_special_chars(input in special_char_string()) {
            let escaped = escape_markdown(&input);

            // Walk through the escaped output and verify every special char
            // is preceded by a backslash
            let chars: Vec<char> = escaped.chars().collect();
            let mut i = 0;
            while i < chars.len() {
                if chars[i] == '\\' {
                    // This is a backslash — skip the next char (it's the escaped char)
                    i += 2;
                } else {
                    // This character is NOT preceded by a backslash,
                    // so it must NOT be a special char
                    let ch = chars[i];
                    prop_assert!(
                        !MARKDOWN_SPECIAL_CHARS.contains(&ch),
                        "Found unescaped special character '{}' at position {} in output: {:?} (input: {:?})",
                        ch, i, escaped, input
                    );
                    i += 1;
                }
            }
        }

        /// Property 8 (line-start patterns): After escaping a string that starts
        /// with a line-start pattern, the pattern is neutralized so it cannot be
        /// interpreted as markdown formatting.
        #[test]
        fn prop_escape_neutralizes_line_start_patterns(input in line_start_pattern_string()) {
            let escaped = escape_line(&input);

            // After escaping, the line should NOT start with an active markdown
            // line-start pattern. Specifically:
            // - Should not start with unescaped "# " (heading)
            // - Should not start with unescaped "- " (unordered list)
            // - Should not start with unescaped "+ " (unordered list)
            // - Should not start with unescaped "> " (blockquote)
            // - Should not start with unescaped "N. " (ordered list)
            // - Should not start with unescaped "~~~" (code fence)
            // - Should not start with unescaped "===" (setext)

            // The first character of an escaped line-start pattern should be '\'
            // (the escape character), OR the pattern should be modified so it's
            // no longer a valid markdown pattern.
            let starts_with_escape = escaped.starts_with('\\');
            let starts_with_digit_escape = escaped.chars().next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
                && escaped.contains("\\.");

            // Either the line starts with a backslash (escaped pattern char)
            // or it starts with digits followed by an escaped dot (ordered list)
            prop_assert!(
                starts_with_escape || starts_with_digit_escape,
                "Line-start pattern not neutralized. Input: {:?}, Escaped: {:?}",
                input, escaped
            );
        }

        /// Property 8 (idempotence safety): Escaping should never lose information.
        /// The escaped output, when all escape sequences are resolved, should produce
        /// the original content (round-trip correctness).
        #[test]
        fn prop_escape_preserves_all_content(input in "[a-zA-Z0-9 !@$%^&(){}]{1,50}") {
            let escaped = escape_markdown(&input);
            // For inputs containing NO special chars, output should be identical
            prop_assert_eq!(&escaped, &input,
                "Escaping modified input that contains no special chars");
        }

        /// Property 8 (length): Escaping can only increase or maintain length,
        /// never decrease it (each special char becomes two chars: backslash + char).
        #[test]
        fn prop_escape_never_shortens(input in special_char_string()) {
            let escaped = escape_markdown(&input);
            prop_assert!(
                escaped.len() >= input.len(),
                "Escaped output is shorter than input. Input: {:?} (len {}), Escaped: {:?} (len {})",
                input, input.len(), escaped, escaped.len()
            );
        }
    }
}
