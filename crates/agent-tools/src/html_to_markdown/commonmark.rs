//! Built-in CommonMark conversion rules.
//!
//! Implements the standard set of conversion rules for CommonMark elements:
//! paragraph, heading, blockquote, list, list item, code block, horizontal
//! rule, emphasis, strong, inline code, link, image, and line break.

use super::escape::{escape_link_destination, escape_link_title, escape_markdown};
use super::node_info::NodeInfo;
use super::options::{CodeBlockStyle, HeadingStyle, LinkStyle, Options};
use super::rules::{ConversionRule, NamedRule};

// ---------------------------------------------------------------------------
// ParagraphRule
// ---------------------------------------------------------------------------

/// Converts `<p>` elements to markdown paragraphs.
///
/// Wraps content in double newlines: `\n\n{content}\n\n`.
struct ParagraphRule;

impl ConversionRule for ParagraphRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "p"
    }

    fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
        format!("\n\n{}\n\n", content.trim())
    }
}

// ---------------------------------------------------------------------------
// HeadingRule
// ---------------------------------------------------------------------------

/// Converts `<h1>` through `<h6>` elements to markdown headings.
///
/// Supports ATX-style (`# Heading`) and Setext-style (underline) headings.
/// Setext is only used for h1 and h2; h3-h6 always use ATX regardless of
/// the option setting.
struct HeadingRule;

impl HeadingRule {
    /// Extract the heading level (1-6) from a tag name like "h1", "h2", etc.
    fn heading_level(tag: &str) -> Option<u8> {
        if tag.len() == 2 && tag.starts_with('h') {
            tag[1..].parse::<u8>().ok().filter(|&n| (1..=6).contains(&n))
        } else {
            None
        }
    }
}

impl ConversionRule for HeadingRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        Self::heading_level(node.tag_name()).is_some()
    }

    fn replacement(&self, content: &str, node: &NodeInfo, options: &Options) -> String {
        let level = Self::heading_level(node.tag_name()).unwrap_or(1);
        let trimmed = content.trim();

        if trimmed.is_empty() {
            return "\n\n\n\n".to_string();
        }

        if options.heading_style == HeadingStyle::Setext && level <= 2 {
            let underline_char = if level == 1 { '=' } else { '-' };
            let underline = underline_char.to_string().repeat(trimmed.len());
            format!("\n\n{}\n{}\n\n", trimmed, underline)
        } else {
            let hashes = "#".repeat(level as usize);
            format!("\n\n{} {}\n\n", hashes, trimmed)
        }
    }
}

// ---------------------------------------------------------------------------
// BlockquoteRule
// ---------------------------------------------------------------------------

/// Converts `<blockquote>` elements to markdown blockquotes.
///
/// Trims content and prefixes each line with `"> "`.
struct BlockquoteRule;

impl ConversionRule for BlockquoteRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "blockquote"
    }

    fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
        let trimmed = content.trim();
        if trimmed.is_empty() {
            return "\n\n\n\n".to_string();
        }

        let quoted = trimmed
            .lines()
            .map(|line| {
                if line.is_empty() {
                    ">".to_string()
                } else {
                    format!("> {}", line)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        format!("\n\n{}\n\n", quoted)
    }
}

// ---------------------------------------------------------------------------
// ListRule
// ---------------------------------------------------------------------------

/// Converts `<ul>` and `<ol>` elements to markdown lists.
///
/// Handles nested list newline logic: if this list is nested inside an `<li>`
/// and is the last child, uses a single newline prefix. Otherwise wraps
/// content in double newlines.
struct ListRule;

impl ConversionRule for ListRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        matches!(node.tag_name(), "ul" | "ol")
    }

    fn replacement(&self, content: &str, node: &NodeInfo, _options: &Options) -> String {
        // If this list is nested inside an LI and is the last child, single newline prefix
        if node.parent_tag() == Some("li") && node.is_last_child() {
            format!("\n{}", content)
        } else {
            format!("\n\n{}\n\n", content)
        }
    }
}

// ---------------------------------------------------------------------------
// ListItemRule
// ---------------------------------------------------------------------------

/// Converts `<li>` elements to markdown list items.
///
/// Computes the correct prefix: a bullet marker for unordered lists, or a
/// numbered prefix (respecting the `start` attribute) for ordered lists.
/// Continuation lines are indented to align with the first line's content.
struct ListItemRule;

impl ConversionRule for ListItemRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "li"
    }

    fn replacement(&self, content: &str, node: &NodeInfo, options: &Options) -> String {
        let prefix = if node.parent_tag() == Some("ol") {
            let start = node
                .parent_attr("start")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1);
            let index = node.sibling_index();
            format!("{}. ", start + index)
        } else {
            format!("{} ", options.bullet_list_marker)
        };

        let trimmed = content.trim();
        let indent = " ".repeat(prefix.len());
        let indented = trimmed
            .lines()
            .enumerate()
            .map(|(i, line)| {
                if i == 0 {
                    format!("{}{}", prefix, line)
                } else if line.is_empty() {
                    String::new()
                } else {
                    format!("{}{}", indent, line)
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let suffix = if node.has_next_sibling() { "\n" } else { "" };
        format!("{}{}", indented, suffix)
    }
}

// ---------------------------------------------------------------------------
// FencedCodeBlockRule
// ---------------------------------------------------------------------------

/// Converts `<pre><code>` elements to fenced code blocks.
///
/// Extracts language from the `class` attribute of the `<code>` element
/// (e.g., `class="language-rust"` → `rust`). Computes fence length to be
/// longer than any backtick/tilde sequence in the code content. Preserves
/// code content without escaping.
struct FencedCodeBlockRule;

impl ConversionRule for FencedCodeBlockRule {
    fn filter(&self, node: &NodeInfo, options: &Options) -> bool {
        options.code_block_style == CodeBlockStyle::Fenced
            && node.tag_name() == "pre"
            && node.first_child_tag() == Some("code")
    }

    fn replacement(&self, _content: &str, node: &NodeInfo, options: &Options) -> String {
        // Get the text content of the code element (first child)
        let code_text = node.text_content();

        // Extract language from class attribute like "language-rust"
        let language = node
            .first_child_attr("class")
            .and_then(|c| {
                c.split_whitespace()
                    .find(|s| s.starts_with("language-"))
                    .map(|s| s.trim_start_matches("language-").to_string())
            })
            .unwrap_or_default();

        let fence_char = options.fence_char;
        let mut fence_size: usize = 3;

        // Ensure fence is longer than any backtick/tilde sequence in the code
        let mut consecutive_count = 0usize;
        for ch in code_text.chars() {
            if ch == fence_char {
                consecutive_count += 1;
                if consecutive_count >= fence_size {
                    fence_size = consecutive_count + 1;
                }
            } else {
                consecutive_count = 0;
            }
        }

        let fence: String = std::iter::repeat(fence_char).take(fence_size).collect();
        let code_trimmed = code_text.trim_end_matches('\n');

        format!("\n\n{}{}\n{}\n{}\n\n", fence, language, code_trimmed, fence)
    }
}

// ---------------------------------------------------------------------------
// HorizontalRuleRule
// ---------------------------------------------------------------------------

/// Converts `<hr>` elements to markdown horizontal rules.
///
/// Outputs `options.hr` (default: `"---"`) surrounded by double newlines.
struct HorizontalRuleRule;

impl ConversionRule for HorizontalRuleRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "hr"
    }

    fn replacement(&self, _content: &str, _node: &NodeInfo, options: &Options) -> String {
        format!("\n\n{}\n\n", options.hr)
    }
}

// ---------------------------------------------------------------------------
// EmphasisRule
// ---------------------------------------------------------------------------

/// Converts `<em>` and `<i>` elements to markdown emphasis.
///
/// Wraps non-empty content with the configured `em_delimiter` (default: `_`).
struct EmphasisRule;

impl ConversionRule for EmphasisRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        matches!(node.tag_name(), "em" | "i")
    }

    fn replacement(&self, content: &str, _node: &NodeInfo, options: &Options) -> String {
        if content.trim().is_empty() {
            return String::new();
        }
        format!("{}{}{}", options.em_delimiter, content, options.em_delimiter)
    }
}

// ---------------------------------------------------------------------------
// StrongRule
// ---------------------------------------------------------------------------

/// Converts `<strong>` and `<b>` elements to markdown strong emphasis.
///
/// Wraps non-empty content with the configured `strong_delimiter` (default: `**`).
struct StrongRule;

impl ConversionRule for StrongRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        matches!(node.tag_name(), "strong" | "b")
    }

    fn replacement(&self, content: &str, _node: &NodeInfo, options: &Options) -> String {
        if content.trim().is_empty() {
            return String::new();
        }
        format!(
            "{}{}{}",
            options.strong_delimiter, content, options.strong_delimiter
        )
    }
}

// ---------------------------------------------------------------------------
// InlineCodeRule
// ---------------------------------------------------------------------------

/// Converts `<code>` elements (not inside `<pre>`) to markdown inline code.
///
/// Computes a backtick delimiter that doesn't conflict with the code content,
/// and adds padding spaces when the content starts/ends with backticks or
/// starts and ends with spaces.
struct InlineCodeRule;

impl ConversionRule for InlineCodeRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "code" && !node.is_inside_pre()
    }

    fn replacement(&self, _content: &str, node: &NodeInfo, _options: &Options) -> String {
        let text = node.text_content();
        if text.is_empty() {
            return String::new();
        }

        // Normalize newlines to spaces
        let normalized = text.replace(|c: char| c == '\r' || c == '\n', " ");

        // Choose backtick delimiter that doesn't conflict with content
        let mut delimiter = "`".to_string();
        while normalized.contains(&*delimiter) {
            delimiter.push('`');
        }

        // Add padding if content starts/ends with backtick or starts+ends with space
        let needs_space = normalized.starts_with('`')
            || normalized.ends_with('`')
            || (normalized.starts_with(' ')
                && normalized.ends_with(' ')
                && normalized.len() > 1);
        let space = if needs_space { " " } else { "" };

        format!("{}{}{}{}{}", delimiter, space, normalized, space, delimiter)
    }
}

// ---------------------------------------------------------------------------
// InlineLinkRule
// ---------------------------------------------------------------------------

/// Converts `<a>` elements with `href` to inline markdown links.
///
/// Formats as `[content](href "title")`. Only applies when `link_style` is
/// `Inline` and the element has an `href` attribute. Escapes the link
/// destination and title.
struct InlineLinkRule;

impl ConversionRule for InlineLinkRule {
    fn filter(&self, node: &NodeInfo, options: &Options) -> bool {
        options.link_style == LinkStyle::Inline
            && node.tag_name() == "a"
            && node.attr("href").is_some()
    }

    fn replacement(&self, content: &str, node: &NodeInfo, _options: &Options) -> String {
        let href = node.attr("href").unwrap_or_default();
        let escaped_href = escape_link_destination(href);
        let title = node.attr("title").unwrap_or_default();
        let title_part = if title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", escape_link_title(title))
        };
        format!("[{}]({}{})", content, escaped_href, title_part)
    }
}

// ---------------------------------------------------------------------------
// ImageRule
// ---------------------------------------------------------------------------

/// Converts `<img>` elements to markdown images.
///
/// Formats as `![alt](src "title")`. Returns empty string if `src` is missing.
/// Escapes the alt text, link destination, and title.
struct ImageRule;

impl ConversionRule for ImageRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "img"
    }

    fn replacement(&self, _content: &str, node: &NodeInfo, _options: &Options) -> String {
        let src = node.attr("src").unwrap_or_default();
        if src.is_empty() {
            return String::new();
        }
        let alt = escape_markdown(node.attr("alt").unwrap_or_default());
        let escaped_src = escape_link_destination(src);
        let title = node.attr("title").unwrap_or_default();
        let title_part = if title.is_empty() {
            String::new()
        } else {
            format!(" \"{}\"", escape_link_title(title))
        };
        format!("![{}]({}{})", alt, escaped_src, title_part)
    }
}

// ---------------------------------------------------------------------------
// LineBreakRule
// ---------------------------------------------------------------------------

/// Converts `<br>` elements to markdown line breaks.
///
/// Outputs `options.br` (default: two trailing spaces) followed by a newline.
struct LineBreakRule;

impl ConversionRule for LineBreakRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.tag_name() == "br"
    }

    fn replacement(&self, _content: &str, _node: &NodeInfo, options: &Options) -> String {
        format!("{}\n", options.br)
    }
}

// ---------------------------------------------------------------------------
// builtin_rules — factory function
// ---------------------------------------------------------------------------

/// Returns the built-in CommonMark conversion rules.
///
/// Includes ParagraphRule, HeadingRule, BlockquoteRule, ListRule,
/// ListItemRule, FencedCodeBlockRule, HorizontalRuleRule, EmphasisRule,
/// StrongRule, InlineCodeRule, InlineLinkRule, ImageRule, and LineBreakRule.
pub fn builtin_rules(_options: &Options) -> Vec<NamedRule> {
    vec![
        NamedRule::new("paragraph", Box::new(ParagraphRule)),
        NamedRule::new("heading", Box::new(HeadingRule)),
        NamedRule::new("blockquote", Box::new(BlockquoteRule)),
        NamedRule::new("list", Box::new(ListRule)),
        NamedRule::new("list_item", Box::new(ListItemRule)),
        NamedRule::new("code_block", Box::new(FencedCodeBlockRule)),
        NamedRule::new("horizontal_rule", Box::new(HorizontalRuleRule)),
        NamedRule::new("emphasis", Box::new(EmphasisRule)),
        NamedRule::new("strong", Box::new(StrongRule)),
        NamedRule::new("inline_code", Box::new(InlineCodeRule)),
        NamedRule::new("inline_link", Box::new(InlineLinkRule)),
        NamedRule::new("image", Box::new(ImageRule)),
        NamedRule::new("line_break", Box::new(LineBreakRule)),
    ]
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use super::super::node_info::{FlankingWhitespace, NodeContext};

    fn make_node(tag: &str) -> NodeInfo {
        NodeInfo::new(
            tag.to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            NodeContext::default(),
        )
    }

    // --- ParagraphRule tests ---

    #[test]
    fn test_paragraph_filter_matches_p() {
        let rule = ParagraphRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("p"), &options));
    }

    #[test]
    fn test_paragraph_filter_rejects_non_p() {
        let rule = ParagraphRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("div"), &options));
        assert!(!rule.filter(&make_node("h1"), &options));
        assert!(!rule.filter(&make_node("blockquote"), &options));
    }

    #[test]
    fn test_paragraph_replacement_wraps_content() {
        let rule = ParagraphRule;
        let options = Options::default();
        let node = make_node("p");
        let result = rule.replacement("Hello world", &node, &options);
        assert_eq!(result, "\n\nHello world\n\n");
    }

    #[test]
    fn test_paragraph_replacement_trims_content() {
        let rule = ParagraphRule;
        let options = Options::default();
        let node = make_node("p");
        let result = rule.replacement("  Hello world  ", &node, &options);
        assert_eq!(result, "\n\nHello world\n\n");
    }

    // --- HeadingRule tests ---

    #[test]
    fn test_heading_filter_matches_h1_through_h6() {
        let rule = HeadingRule;
        let options = Options::default();
        for tag in &["h1", "h2", "h3", "h4", "h5", "h6"] {
            assert!(rule.filter(&make_node(tag), &options), "Should match {}", tag);
        }
    }

    #[test]
    fn test_heading_filter_rejects_non_heading() {
        let rule = HeadingRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("p"), &options));
        assert!(!rule.filter(&make_node("h7"), &options));
        assert!(!rule.filter(&make_node("h0"), &options));
        assert!(!rule.filter(&make_node("heading"), &options));
    }

    #[test]
    fn test_heading_atx_style() {
        let rule = HeadingRule;
        let options = Options::default(); // ATX by default

        assert_eq!(
            rule.replacement("Title", &make_node("h1"), &options),
            "\n\n# Title\n\n"
        );
        assert_eq!(
            rule.replacement("Subtitle", &make_node("h2"), &options),
            "\n\n## Subtitle\n\n"
        );
        assert_eq!(
            rule.replacement("Section", &make_node("h3"), &options),
            "\n\n### Section\n\n"
        );
        assert_eq!(
            rule.replacement("Deep", &make_node("h6"), &options),
            "\n\n###### Deep\n\n"
        );
    }

    #[test]
    fn test_heading_setext_style_h1() {
        let rule = HeadingRule;
        let options = Options {
            heading_style: HeadingStyle::Setext,
            ..Options::default()
        };

        let result = rule.replacement("Title", &make_node("h1"), &options);
        assert_eq!(result, "\n\nTitle\n=====\n\n");
    }

    #[test]
    fn test_heading_setext_style_h2() {
        let rule = HeadingRule;
        let options = Options {
            heading_style: HeadingStyle::Setext,
            ..Options::default()
        };

        let result = rule.replacement("Subtitle", &make_node("h2"), &options);
        assert_eq!(result, "\n\nSubtitle\n--------\n\n");
    }

    #[test]
    fn test_heading_setext_falls_back_to_atx_for_h3_plus() {
        let rule = HeadingRule;
        let options = Options {
            heading_style: HeadingStyle::Setext,
            ..Options::default()
        };

        let result = rule.replacement("Section", &make_node("h3"), &options);
        assert_eq!(result, "\n\n### Section\n\n");
    }

    #[test]
    fn test_heading_empty_content() {
        let rule = HeadingRule;
        let options = Options::default();
        let result = rule.replacement("", &make_node("h1"), &options);
        assert_eq!(result, "\n\n\n\n");
    }

    #[test]
    fn test_heading_trims_content() {
        let rule = HeadingRule;
        let options = Options::default();
        let result = rule.replacement("  Hello  ", &make_node("h1"), &options);
        assert_eq!(result, "\n\n# Hello\n\n");
    }

    // --- BlockquoteRule tests ---

    #[test]
    fn test_blockquote_filter_matches() {
        let rule = BlockquoteRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("blockquote"), &options));
    }

    #[test]
    fn test_blockquote_filter_rejects_non_blockquote() {
        let rule = BlockquoteRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("p"), &options));
        assert!(!rule.filter(&make_node("div"), &options));
    }

    #[test]
    fn test_blockquote_single_line() {
        let rule = BlockquoteRule;
        let options = Options::default();
        let node = make_node("blockquote");
        let result = rule.replacement("A quote", &node, &options);
        assert_eq!(result, "\n\n> A quote\n\n");
    }

    #[test]
    fn test_blockquote_multiline() {
        let rule = BlockquoteRule;
        let options = Options::default();
        let node = make_node("blockquote");
        let result = rule.replacement("Line one\nLine two\nLine three", &node, &options);
        assert_eq!(result, "\n\n> Line one\n> Line two\n> Line three\n\n");
    }

    #[test]
    fn test_blockquote_with_empty_line() {
        let rule = BlockquoteRule;
        let options = Options::default();
        let node = make_node("blockquote");
        let result = rule.replacement("Before\n\nAfter", &node, &options);
        assert_eq!(result, "\n\n> Before\n>\n> After\n\n");
    }

    #[test]
    fn test_blockquote_empty_content() {
        let rule = BlockquoteRule;
        let options = Options::default();
        let node = make_node("blockquote");
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n\n\n");
    }

    #[test]
    fn test_blockquote_trims_surrounding_whitespace() {
        let rule = BlockquoteRule;
        let options = Options::default();
        let node = make_node("blockquote");
        let result = rule.replacement("  A quote  ", &node, &options);
        assert_eq!(result, "\n\n> A quote\n\n");
    }

    // --- builtin_rules tests ---

    #[test]
    fn test_builtin_rules_returns_thirteen_rules() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        assert_eq!(rules.len(), 13);
        assert_eq!(rules[0].name, "paragraph");
        assert_eq!(rules[1].name, "heading");
        assert_eq!(rules[2].name, "blockquote");
        assert_eq!(rules[3].name, "list");
        assert_eq!(rules[4].name, "list_item");
        assert_eq!(rules[5].name, "code_block");
        assert_eq!(rules[6].name, "horizontal_rule");
        assert_eq!(rules[7].name, "emphasis");
        assert_eq!(rules[8].name, "strong");
        assert_eq!(rules[9].name, "inline_code");
        assert_eq!(rules[10].name, "inline_link");
        assert_eq!(rules[11].name, "image");
        assert_eq!(rules[12].name, "line_break");
    }

    #[test]
    fn test_builtin_rules_paragraph_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("p");
        assert!(rules[0].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_heading_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("h2");
        assert!(rules[1].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_blockquote_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("blockquote");
        assert!(rules[2].rule.filter(&node, &options));
    }

    // --- FencedCodeBlockRule tests ---

    fn make_pre_with_code(text_content: &str, class: Option<&str>) -> NodeInfo {
        let mut first_child_attrs = HashMap::new();
        if let Some(cls) = class {
            first_child_attrs.insert("class".to_string(), cls.to_string());
        }

        let context = NodeContext {
            first_child_tag: Some("code".to_string()),
            first_child_attrs,
            text_content: text_content.to_string(),
            ..Default::default()
        };

        NodeInfo::new(
            "pre".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        )
    }

    #[test]
    fn test_fenced_code_block_filter_matches_pre_with_code_child() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("hello", None);
        assert!(rule.filter(&node, &options));
    }

    #[test]
    fn test_fenced_code_block_filter_rejects_pre_without_code_child() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_node("pre"); // no first_child_tag set
        assert!(!rule.filter(&node, &options));
    }

    #[test]
    fn test_fenced_code_block_filter_rejects_non_fenced_style() {
        let rule = FencedCodeBlockRule;
        let options = Options {
            code_block_style: super::super::options::CodeBlockStyle::Indented,
            ..Options::default()
        };
        let node = make_pre_with_code("hello", None);
        assert!(!rule.filter(&node, &options));
    }

    #[test]
    fn test_fenced_code_block_simple() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("fn main() {}\n", None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n```\nfn main() {}\n```\n\n");
    }

    #[test]
    fn test_fenced_code_block_with_language() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("fn main() {}\n", Some("language-rust"));
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n```rust\nfn main() {}\n```\n\n");
    }

    #[test]
    fn test_fenced_code_block_with_multiple_classes() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("code", Some("highlight language-python extra"));
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n```python\ncode\n```\n\n");
    }

    #[test]
    fn test_fenced_code_block_longer_fence_for_backticks_in_content() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("use ``` in markdown\n", None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n````\nuse ``` in markdown\n````\n\n");
    }

    #[test]
    fn test_fenced_code_block_even_longer_fence() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("````` five backticks\n", None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n``````\n````` five backticks\n``````\n\n");
    }

    #[test]
    fn test_fenced_code_block_tilde_fence_char() {
        let rule = FencedCodeBlockRule;
        let options = Options {
            fence_char: '~',
            ..Options::default()
        };
        let node = make_pre_with_code("code here\n", None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n~~~\ncode here\n~~~\n\n");
    }

    #[test]
    fn test_fenced_code_block_tilde_fence_longer_for_tildes_in_content() {
        let rule = FencedCodeBlockRule;
        let options = Options {
            fence_char: '~',
            ..Options::default()
        };
        let node = make_pre_with_code("~~~ example\n", None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n~~~~\n~~~ example\n~~~~\n\n");
    }

    #[test]
    fn test_fenced_code_block_strips_trailing_newlines() {
        let rule = FencedCodeBlockRule;
        let options = Options::default();
        let node = make_pre_with_code("code\n\n\n", None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n```\ncode\n```\n\n");
    }

    // --- HorizontalRuleRule tests ---

    #[test]
    fn test_horizontal_rule_filter_matches_hr() {
        let rule = HorizontalRuleRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("hr"), &options));
    }

    #[test]
    fn test_horizontal_rule_filter_rejects_non_hr() {
        let rule = HorizontalRuleRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("p"), &options));
        assert!(!rule.filter(&make_node("div"), &options));
        assert!(!rule.filter(&make_node("br"), &options));
    }

    #[test]
    fn test_horizontal_rule_default_output() {
        let rule = HorizontalRuleRule;
        let options = Options::default();
        let node = make_node("hr");
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n---\n\n");
    }

    #[test]
    fn test_horizontal_rule_custom_hr_string() {
        let rule = HorizontalRuleRule;
        let options = Options {
            hr: "***",
            ..Options::default()
        };
        let node = make_node("hr");
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\n\n***\n\n");
    }

    #[test]
    fn test_builtin_rules_code_block_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_pre_with_code("code", None);
        assert!(rules[5].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_horizontal_rule_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("hr");
        assert!(rules[6].rule.filter(&node, &options));
    }

    // --- ListRule tests ---

    #[test]
    fn test_list_filter_matches_ul() {
        let rule = ListRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("ul"), &options));
    }

    #[test]
    fn test_list_filter_matches_ol() {
        let rule = ListRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("ol"), &options));
    }

    #[test]
    fn test_list_filter_rejects_non_list() {
        let rule = ListRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("li"), &options));
        assert!(!rule.filter(&make_node("div"), &options));
    }

    #[test]
    fn test_list_replacement_top_level() {
        let rule = ListRule;
        let options = Options::default();
        let node = make_node("ul");
        let result = rule.replacement("- item 1\n- item 2", &node, &options);
        assert_eq!(result, "\n\n- item 1\n- item 2\n\n");
    }

    #[test]
    fn test_list_replacement_nested_last_child() {
        let rule = ListRule;
        let options = Options::default();
        // Nested list inside li, as last child
        let context = NodeContext {
            parent_tag: Some("li".to_string()),
            has_next_sibling: false,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "ul".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("- nested", &node, &options);
        assert_eq!(result, "\n- nested");
    }

    #[test]
    fn test_list_replacement_nested_not_last_child() {
        let rule = ListRule;
        let options = Options::default();
        // Nested list inside li, but NOT the last child
        let context = NodeContext {
            parent_tag: Some("li".to_string()),
            has_next_sibling: true,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "ul".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("- nested", &node, &options);
        assert_eq!(result, "\n\n- nested\n\n");
    }

    // --- ListItemRule tests ---

    #[test]
    fn test_list_item_filter_matches_li() {
        let rule = ListItemRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("li"), &options));
    }

    #[test]
    fn test_list_item_filter_rejects_non_li() {
        let rule = ListItemRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("ul"), &options));
        assert!(!rule.filter(&make_node("ol"), &options));
    }

    #[test]
    fn test_list_item_unordered_bullet() {
        let rule = ListItemRule;
        let options = Options::default(); // bullet_list_marker = '-'
        let context = NodeContext {
            parent_tag: Some("ul".to_string()),
            sibling_index: 0,
            has_next_sibling: true,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("Item one", &node, &options);
        assert_eq!(result, "- Item one\n");
    }

    #[test]
    fn test_list_item_unordered_last_item_no_trailing_newline() {
        let rule = ListItemRule;
        let options = Options::default();
        let context = NodeContext {
            parent_tag: Some("ul".to_string()),
            sibling_index: 2,
            has_next_sibling: false,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("Last item", &node, &options);
        assert_eq!(result, "- Last item");
    }

    #[test]
    fn test_list_item_ordered_numbering() {
        let rule = ListItemRule;
        let options = Options::default();
        let mut parent_attrs = HashMap::new();
        parent_attrs.insert("start".to_string(), "1".to_string());
        let context = NodeContext {
            parent_tag: Some("ol".to_string()),
            parent_attrs,
            sibling_index: 0,
            has_next_sibling: true,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("First", &node, &options);
        assert_eq!(result, "1. First\n");
    }

    #[test]
    fn test_list_item_ordered_with_start_attr() {
        let rule = ListItemRule;
        let options = Options::default();
        let mut parent_attrs = HashMap::new();
        parent_attrs.insert("start".to_string(), "5".to_string());
        let context = NodeContext {
            parent_tag: Some("ol".to_string()),
            parent_attrs,
            sibling_index: 2,
            has_next_sibling: true,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("Seventh", &node, &options);
        assert_eq!(result, "7. Seventh\n");
    }

    #[test]
    fn test_list_item_multiline_indentation() {
        let rule = ListItemRule;
        let options = Options::default();
        let context = NodeContext {
            parent_tag: Some("ul".to_string()),
            sibling_index: 0,
            has_next_sibling: false,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("Line one\nLine two\nLine three", &node, &options);
        assert_eq!(result, "- Line one\n  Line two\n  Line three");
    }

    #[test]
    fn test_list_item_multiline_with_empty_line() {
        let rule = ListItemRule;
        let options = Options::default();
        let context = NodeContext {
            parent_tag: Some("ul".to_string()),
            sibling_index: 0,
            has_next_sibling: false,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("Before\n\nAfter", &node, &options);
        assert_eq!(result, "- Before\n\n  After");
    }

    #[test]
    fn test_list_item_custom_bullet_marker() {
        let rule = ListItemRule;
        let options = Options {
            bullet_list_marker: '*',
            ..Options::default()
        };
        let context = NodeContext {
            parent_tag: Some("ul".to_string()),
            sibling_index: 0,
            has_next_sibling: false,
            ..Default::default()
        };
        let node = NodeInfo::new(
            "li".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );
        let result = rule.replacement("Item", &node, &options);
        assert_eq!(result, "* Item");
    }

    #[test]
    fn test_builtin_rules_list_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("ul");
        assert!(rules[3].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_list_item_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("li");
        assert!(rules[4].rule.filter(&node, &options));
    }

    // --- EmphasisRule tests ---

    #[test]
    fn test_emphasis_filter_matches_em() {
        let rule = EmphasisRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("em"), &options));
    }

    #[test]
    fn test_emphasis_filter_matches_i() {
        let rule = EmphasisRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("i"), &options));
    }

    #[test]
    fn test_emphasis_filter_rejects_non_emphasis() {
        let rule = EmphasisRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("strong"), &options));
        assert!(!rule.filter(&make_node("b"), &options));
        assert!(!rule.filter(&make_node("span"), &options));
    }

    #[test]
    fn test_emphasis_wraps_content() {
        let rule = EmphasisRule;
        let options = Options::default();
        let node = make_node("em");
        let result = rule.replacement("hello", &node, &options);
        assert_eq!(result, "_hello_");
    }

    #[test]
    fn test_emphasis_custom_delimiter() {
        let rule = EmphasisRule;
        let options = Options {
            em_delimiter: "*",
            ..Options::default()
        };
        let node = make_node("em");
        let result = rule.replacement("hello", &node, &options);
        assert_eq!(result, "*hello*");
    }

    #[test]
    fn test_emphasis_empty_content_returns_empty() {
        let rule = EmphasisRule;
        let options = Options::default();
        let node = make_node("em");
        assert_eq!(rule.replacement("", &node, &options), "");
        assert_eq!(rule.replacement("   ", &node, &options), "");
        assert_eq!(rule.replacement("\n\t", &node, &options), "");
    }

    // --- StrongRule tests ---

    #[test]
    fn test_strong_filter_matches_strong() {
        let rule = StrongRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("strong"), &options));
    }

    #[test]
    fn test_strong_filter_matches_b() {
        let rule = StrongRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("b"), &options));
    }

    #[test]
    fn test_strong_filter_rejects_non_strong() {
        let rule = StrongRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("em"), &options));
        assert!(!rule.filter(&make_node("i"), &options));
        assert!(!rule.filter(&make_node("span"), &options));
    }

    #[test]
    fn test_strong_wraps_content() {
        let rule = StrongRule;
        let options = Options::default();
        let node = make_node("strong");
        let result = rule.replacement("hello", &node, &options);
        assert_eq!(result, "**hello**");
    }

    #[test]
    fn test_strong_custom_delimiter() {
        let rule = StrongRule;
        let options = Options {
            strong_delimiter: "__",
            ..Options::default()
        };
        let node = make_node("strong");
        let result = rule.replacement("hello", &node, &options);
        assert_eq!(result, "__hello__");
    }

    #[test]
    fn test_strong_empty_content_returns_empty() {
        let rule = StrongRule;
        let options = Options::default();
        let node = make_node("strong");
        assert_eq!(rule.replacement("", &node, &options), "");
        assert_eq!(rule.replacement("   ", &node, &options), "");
        assert_eq!(rule.replacement("\n\t", &node, &options), "");
    }

    // --- InlineCodeRule tests ---

    fn make_code_node(text_content: &str, inside_pre: bool) -> NodeInfo {
        let context = NodeContext {
            text_content: text_content.to_string(),
            inside_pre,
            ..Default::default()
        };
        NodeInfo::new(
            "code".to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        )
    }

    #[test]
    fn test_inline_code_filter_matches_code_not_in_pre() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("hello", false);
        assert!(rule.filter(&node, &options));
    }

    #[test]
    fn test_inline_code_filter_rejects_code_inside_pre() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("hello", true);
        assert!(!rule.filter(&node, &options));
    }

    #[test]
    fn test_inline_code_filter_rejects_non_code() {
        let rule = InlineCodeRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("span"), &options));
        assert!(!rule.filter(&make_node("pre"), &options));
    }

    #[test]
    fn test_inline_code_simple() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("hello", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "`hello`");
    }

    #[test]
    fn test_inline_code_empty_returns_empty() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "");
    }

    #[test]
    fn test_inline_code_normalizes_newlines() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("line1\nline2\rline3", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "`line1 line2 line3`");
    }

    #[test]
    fn test_inline_code_backtick_in_content_uses_double() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("use `code` here", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "``use `code` here``");
    }

    #[test]
    fn test_inline_code_double_backtick_in_content_uses_triple() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("use `` here", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "```use `` here```");
    }

    #[test]
    fn test_inline_code_starts_with_backtick_adds_space() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("`start", false);
        let result = rule.replacement("", &node, &options);
        // Content starts with backtick, so needs double-backtick delimiter + space
        assert_eq!(result, "`` `start ``");
    }

    #[test]
    fn test_inline_code_ends_with_backtick_adds_space() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node("end`", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "`` end` ``");
    }

    #[test]
    fn test_inline_code_starts_and_ends_with_space_adds_padding() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node(" spaced ", false);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "`  spaced  `");
    }

    #[test]
    fn test_inline_code_single_space_no_padding() {
        let rule = InlineCodeRule;
        let options = Options::default();
        let node = make_code_node(" ", false);
        let result = rule.replacement("", &node, &options);
        // Single space: len is 1, so starts_with(' ') && ends_with(' ') && len > 1 is false
        assert_eq!(result, "` `");
    }

    // --- builtin_rules integration for new rules ---

    #[test]
    fn test_builtin_rules_emphasis_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("em");
        assert!(rules[7].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_strong_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("strong");
        assert!(rules[8].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_inline_code_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_code_node("code", false);
        assert!(rules[9].rule.filter(&node, &options));
    }

    // --- InlineLinkRule tests ---

    fn make_link_node(href: &str, title: Option<&str>) -> NodeInfo {
        let mut attrs = HashMap::new();
        attrs.insert("href".to_string(), href.to_string());
        if let Some(t) = title {
            attrs.insert("title".to_string(), t.to_string());
        }
        NodeInfo::new(
            "a".to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            attrs,
            NodeContext::default(),
        )
    }

    #[test]
    fn test_inline_link_filter_matches_a_with_href() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_link_node("https://example.com", None);
        assert!(rule.filter(&node, &options));
    }

    #[test]
    fn test_inline_link_filter_rejects_a_without_href() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_node("a"); // no href attribute
        assert!(!rule.filter(&node, &options));
    }

    #[test]
    fn test_inline_link_filter_rejects_reference_style() {
        let rule = InlineLinkRule;
        let options = Options {
            link_style: LinkStyle::Reference,
            ..Options::default()
        };
        let node = make_link_node("https://example.com", None);
        assert!(!rule.filter(&node, &options));
    }

    #[test]
    fn test_inline_link_simple() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_link_node("https://example.com", None);
        let result = rule.replacement("Click here", &node, &options);
        assert_eq!(result, "[Click here](https://example.com)");
    }

    #[test]
    fn test_inline_link_with_title() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_link_node("https://example.com", Some("Example Site"));
        let result = rule.replacement("Click here", &node, &options);
        assert_eq!(result, "[Click here](https://example.com \"Example Site\")");
    }

    #[test]
    fn test_inline_link_escapes_destination_parens() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_link_node("https://example.com/path(1)", None);
        let result = rule.replacement("link", &node, &options);
        assert_eq!(result, "[link](https://example.com/path\\(1\\))");
    }

    #[test]
    fn test_inline_link_escapes_title_quotes() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_link_node("https://example.com", Some("say \"hello\""));
        let result = rule.replacement("link", &node, &options);
        assert_eq!(result, "[link](https://example.com \"say \\\"hello\\\"\")");
    }

    #[test]
    fn test_inline_link_with_spaces_in_url() {
        let rule = InlineLinkRule;
        let options = Options::default();
        let node = make_link_node("https://example.com/my page", None);
        let result = rule.replacement("link", &node, &options);
        assert_eq!(result, "[link](<https://example.com/my page>)");
    }

    // --- ImageRule tests ---

    fn make_img_node(src: &str, alt: Option<&str>, title: Option<&str>) -> NodeInfo {
        let mut attrs = HashMap::new();
        if !src.is_empty() {
            attrs.insert("src".to_string(), src.to_string());
        }
        if let Some(a) = alt {
            attrs.insert("alt".to_string(), a.to_string());
        }
        if let Some(t) = title {
            attrs.insert("title".to_string(), t.to_string());
        }
        NodeInfo::new(
            "img".to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            attrs,
            NodeContext::default(),
        )
    }

    #[test]
    fn test_image_filter_matches_img() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("photo.jpg", None, None);
        assert!(rule.filter(&node, &options));
    }

    #[test]
    fn test_image_filter_rejects_non_img() {
        let rule = ImageRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("a"), &options));
        assert!(!rule.filter(&make_node("div"), &options));
    }

    #[test]
    fn test_image_simple() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("photo.jpg", Some("A photo"), None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "![A photo](photo.jpg)");
    }

    #[test]
    fn test_image_with_title() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("photo.jpg", Some("A photo"), Some("My Photo"));
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "![A photo](photo.jpg \"My Photo\")");
    }

    #[test]
    fn test_image_empty_src_returns_empty() {
        let rule = ImageRule;
        let options = Options::default();
        // Node with no src attribute
        let node = NodeInfo::new(
            "img".to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            NodeContext::default(),
        );
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "");
    }

    #[test]
    fn test_image_no_alt_uses_empty() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("photo.jpg", None, None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "![](photo.jpg)");
    }

    #[test]
    fn test_image_escapes_alt_special_chars() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("photo.jpg", Some("A [bracketed] *photo*"), None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "![A \\[bracketed\\] \\*photo\\*](photo.jpg)");
    }

    #[test]
    fn test_image_escapes_src_parens() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("path/photo (1).jpg", Some("photo"), None);
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "![photo](<path/photo \\(1\\).jpg>)");
    }

    #[test]
    fn test_image_escapes_title_quotes() {
        let rule = ImageRule;
        let options = Options::default();
        let node = make_img_node("photo.jpg", Some("alt"), Some("a \"title\""));
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "![alt](photo.jpg \"a \\\"title\\\"\")");
    }

    // --- LineBreakRule tests ---

    #[test]
    fn test_line_break_filter_matches_br() {
        let rule = LineBreakRule;
        let options = Options::default();
        assert!(rule.filter(&make_node("br"), &options));
    }

    #[test]
    fn test_line_break_filter_rejects_non_br() {
        let rule = LineBreakRule;
        let options = Options::default();
        assert!(!rule.filter(&make_node("hr"), &options));
        assert!(!rule.filter(&make_node("p"), &options));
    }

    #[test]
    fn test_line_break_default_output() {
        let rule = LineBreakRule;
        let options = Options::default();
        let node = make_node("br");
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "  \n");
    }

    #[test]
    fn test_line_break_custom_br() {
        let rule = LineBreakRule;
        let options = Options {
            br: "\\",
            ..Options::default()
        };
        let node = make_node("br");
        let result = rule.replacement("", &node, &options);
        assert_eq!(result, "\\\n");
    }

    // --- builtin_rules integration for link/image/linebreak ---

    #[test]
    fn test_builtin_rules_inline_link_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_link_node("https://example.com", None);
        assert!(rules[10].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_image_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_img_node("photo.jpg", None, None);
        assert!(rules[11].rule.filter(&node, &options));
    }

    #[test]
    fn test_builtin_rules_line_break_matches() {
        let options = Options::default();
        let rules = builtin_rules(&options);
        let node = make_node("br");
        assert!(rules[12].rule.filter(&node, &options));
    }

    // --- Property-Based Tests ---
    // **Validates: Requirements 5.7, 5.10**

    use proptest::prelude::*;

    proptest! {
        /// **Property 17: Heading level maps correctly to hash count**
        ///
        /// For any heading level 1-6 and non-whitespace content, the HeadingRule (ATX style)
        /// produces output with exactly N hash characters followed by a space.
        /// **Validates: Requirements 5.7**
        #[test]
        fn prop_heading_level_maps_to_hash_count(level in 1u8..=6u8, content in "[a-z][a-z ]{0,19}") {
            let node = make_node(&format!("h{}", level));
            let options = Options::default();
            let rule = HeadingRule;

            let result = rule.replacement(&content, &node, &options);

            // Should contain exactly `level` hashes followed by a space
            let expected_prefix = format!("{} ", "#".repeat(level as usize));
            prop_assert!(
                result.contains(&expected_prefix),
                "h{} should produce '{}' prefix, got: {:?}",
                level, expected_prefix, result
            );

            // The prefix should not be preceded by another hash (i.e., exactly N hashes)
            let hash_section = &result.trim_start_matches('\n');
            let hash_count = hash_section.chars().take_while(|&c| c == '#').count();
            prop_assert_eq!(
                hash_count, level as usize,
                "h{} should have exactly {} hashes, got {}",
                level, level, hash_count
            );
        }

        /// **Property 18: Ordered list numbering respects start attribute**
        ///
        /// For any valid start value and sibling index, the ListItemRule produces
        /// a prefix of (start + index) followed by ". ".
        #[test]
        fn prop_ordered_list_respects_start_attr(
            start in 1usize..=100,
            index in 0usize..=20,
            content in "[a-z]{1,10}"
        ) {
            let mut parent_attrs = HashMap::new();
            parent_attrs.insert("start".to_string(), start.to_string());

            let context = NodeContext {
                parent_tag: Some("ol".to_string()),
                parent_attrs,
                sibling_index: index,
                has_next_sibling: true,
                ..Default::default()
            };

            let node = NodeInfo::new(
                "li".to_string(),
                true,
                false,
                false,
                FlankingWhitespace::default(),
                HashMap::new(),
                context,
            );

            let options = Options::default();
            let rule = ListItemRule;
            let result = rule.replacement(&content, &node, &options);

            let expected_num = start + index;
            let expected_prefix = format!("{}. ", expected_num);
            prop_assert!(
                result.starts_with(&expected_prefix),
                "Expected prefix '{}' but got: {:?}",
                expected_prefix, result
            );
        }

        /// **Property 9: Fenced code block fence is always longer than content backtick sequences**
        ///
        /// For any code content containing backtick sequences, the generated fence
        /// must have strictly more fence characters than the longest backtick sequence
        /// in the content.
        ///
        /// **Validates: Requirements 5.11**
        #[test]
        fn prop_fence_longer_than_content_backticks(
            backtick_count in 3usize..=10,
            prefix in "[a-z]{0,5}",
            suffix in "[a-z]{0,5}"
        ) {
            // Create content with backtick sequences
            let backticks: String = std::iter::repeat('`').take(backtick_count).collect();
            let code_content = format!("{}{}{}", prefix, backticks, suffix);

            // Create a node simulating <pre> with first child <code> containing the text
            let node = make_pre_with_code(&code_content, None);

            let options = Options::default();
            let rule = FencedCodeBlockRule;
            let result = rule.replacement("", &node, &options);

            // Extract the fence from the output (first line after leading \n\n)
            let trimmed = result.trim_start_matches('\n');
            let fence_line = trimmed.lines().next().unwrap_or("");
            let fence_len = fence_line.chars().take_while(|&c| c == '`').count();

            prop_assert!(
                fence_len > backtick_count,
                "Fence length {} should be > content backtick count {}. Result: {:?}",
                fence_len, backtick_count, result
            );
        }

        /// **Property 16: Inline code delimiter avoids conflict with content**
        ///
        /// For any code content containing backtick sequences, the chosen inline
        /// code delimiter must not appear in the content itself.
        ///
        /// **Validates: Requirements 5.15**
        #[test]
        fn prop_inline_code_delimiter_avoids_conflict(
            backtick_count in 1usize..=5,
            prefix in "[a-z]{0,5}",
            suffix in "[a-z]{0,5}"
        ) {
            let backticks: String = std::iter::repeat('`').take(backtick_count).collect();
            let code_content = format!("{}{}{}", prefix, backticks, suffix);

            let node = make_code_node(&code_content, false);

            let options = Options::default();
            let rule = InlineCodeRule;
            let result = rule.replacement("", &node, &options);

            // Extract the delimiter (opening backticks)
            let delimiter_len = result.chars().take_while(|&c| c == '`').count();
            let delimiter: String = std::iter::repeat('`').take(delimiter_len).collect();

            // The delimiter should NOT appear in the original content
            prop_assert!(
                !code_content.contains(&delimiter),
                "Delimiter '{}' conflicts with content '{}'",
                delimiter, code_content
            );
        }
    }
}
