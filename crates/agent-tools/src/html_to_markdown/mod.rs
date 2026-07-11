//! Native HTML-to-markdown conversion engine.
//!
//! A rule-based conversion engine modeled after the turndown.js architecture.
//! Parses HTML into a DOM tree using `scraper` (html5ever), applies whitespace
//! collapsing, traverses depth-first applying conversion rules, and returns
//! CommonMark-compliant markdown.

pub mod commonmark;
pub mod escape;
pub mod node_info;
pub mod options;
pub mod rules;
pub mod whitespace;

use std::collections::HashMap;

use ego_tree::NodeRef;
use scraper::node::Node;
use scraper::Html;

use escape::escape_markdown;
use node_info::{
    is_block_element, is_meaningful_when_blank, is_void_element, FlankingWhitespace, NodeContext,
    NodeInfo,
};
use options::Options;
use whitespace::{collapse, CollapsedDocument};

// Re-export rule types for public API
pub use rules::{ConversionRule, RuleFilter, RuleSet};

/// Elements that are always removed (produce empty string, descendants suppressed).
const REMOVED_ELEMENTS: &[&str] = &[
    "script", "style", "noscript", "iframe", "svg", "canvas", "template", "head",
];

/// HTML-to-markdown converter.
///
/// Converts HTML content into CommonMark-compliant markdown using a
/// rule-based conversion engine.
pub struct HtmlToMarkdown {
    /// Conversion options (heading style, delimiters, etc.).
    options: Options,
    /// The rule set used for conversion.
    rules: RuleSet,
}

impl HtmlToMarkdown {
    /// Create a new `HtmlToMarkdown` converter with default options.
    pub fn new() -> Self {
        let options = Options::default();
        let rules = RuleSet::with_commonmark_rules(&options);
        Self { options, rules }
    }

    /// Create a new `HtmlToMarkdown` converter with the given options.
    pub fn with_options(options: Options) -> Self {
        let rules = RuleSet::with_commonmark_rules(&options);
        Self { options, rules }
    }

    /// Add a custom conversion rule at highest priority.
    ///
    /// The rule will be checked before all existing rules (including built-in
    /// CommonMark rules). If multiple rules match the same node, the most
    /// recently added rule wins.
    pub fn add_rule(&mut self, name: &str, rule: Box<dyn ConversionRule>) {
        self.rules.add_rule(name, rule);
    }

    /// Add a removal rule: matching nodes produce empty output.
    ///
    /// Elements matching the filter (and all their descendants) will be
    /// excluded from the markdown output.
    pub fn remove(&mut self, filter: RuleFilter) {
        self.rules.add_remove(filter);
    }

    /// Add a keep rule: matching nodes preserve their content as-is.
    ///
    /// Elements matching the filter will have their converted content
    /// passed through without any additional transformation.
    pub fn keep(&mut self, filter: RuleFilter) {
        self.rules.add_keep(filter);
    }

    /// Apply a plugin function that can configure this converter.
    ///
    /// Plugins receive a mutable reference to the converter and can call
    /// `add_rule`, `remove`, `keep`, or any other configuration method.
    pub fn use_plugin(&mut self, plugin: impl FnOnce(&mut Self)) {
        plugin(self);
    }

    /// Convert an HTML string to CommonMark markdown.
    ///
    /// Steps:
    /// 1. Parse HTML into a DOM tree
    /// 2. Collapse whitespace in text nodes
    /// 3. Recursively convert nodes depth-first
    /// 4. Post-process the output
    pub fn convert(&self, html: &str) -> String {
        if html.trim().is_empty() {
            return String::new();
        }

        // Parse the HTML document
        let doc = Html::parse_document(html);

        // Collapse whitespace
        let collapsed = collapse(&doc);

        // Convert the root's children depth-first
        let root = doc.tree.root();
        let output = self.convert_children(&root, &doc, &collapsed);

        // Post-process
        self.post_process(output)
    }

    /// Convert all children of a node, joining them with smart_join.
    fn convert_children(
        &self,
        node: &NodeRef<Node>,
        doc: &Html,
        collapsed: &CollapsedDocument,
    ) -> String {
        let mut result = String::new();

        for child in node.children() {
            let child_output = self.convert_node(&child, doc, collapsed);
            result = smart_join(result, child_output);
        }

        result
    }

    /// Convert a single DOM node to markdown.
    ///
    /// - Text nodes: look up collapsed text, escape if not in code context
    /// - Element nodes: recurse into children, build NodeInfo, apply rule
    /// - Other nodes (comments, doctypes): ignored
    fn convert_node(
        &self,
        node: &NodeRef<Node>,
        doc: &Html,
        collapsed: &CollapsedDocument,
    ) -> String {
        match node.value() {
            Node::Text(_) => {
                // Get collapsed text for this node
                let text = match collapsed.get_text(node.id()) {
                    Some(t) => t.to_string(),
                    None => return String::new(),
                };

                if text.is_empty() {
                    return String::new();
                }

                // Check if inside a pre/code context — don't escape
                if is_ancestor_pre(node) {
                    text
                } else {
                    escape_markdown(&text)
                }
            }
            Node::Element(el) => {
                let tag = el.name().to_string();

                // Check if this element is in the disallowed list — skip entirely
                if REMOVED_ELEMENTS.contains(&tag.as_str()) {
                    return String::new();
                }

                // Recurse into children first
                let content = self.convert_children(node, doc, collapsed);

                // Build NodeInfo for rule application
                let node_info = self.build_node_info(node, &tag, &content, doc, collapsed);

                // Apply conversion rule
                let replacement = self.rules.apply_rule(&content, &node_info, &self.options);

                // For inline elements with flanking whitespace, re-attach it
                if !node_info.is_block && !replacement.is_empty() {
                    let leading = &node_info.flanking_whitespace.leading;
                    let trailing = &node_info.flanking_whitespace.trailing;
                    if !leading.is_empty() || !trailing.is_empty() {
                        return format!("{}{}{}", leading, replacement, trailing);
                    }
                }

                replacement
            }
            _ => {
                // Comments, doctypes, processing instructions — ignore
                String::new()
            }
        }
    }

    /// Build a `NodeInfo` from a DOM element node for rule dispatch.
    fn build_node_info(
        &self,
        node: &NodeRef<Node>,
        tag: &str,
        _content: &str,
        _doc: &Html,
        collapsed: &CollapsedDocument,
    ) -> NodeInfo {
        let element = match node.value() {
            Node::Element(el) => el,
            _ => unreachable!("build_node_info called on non-element"),
        };

        let is_block = is_block_element(tag);
        let inside_pre = is_ancestor_pre(node);
        let is_code = inside_pre || tag == "code" || tag == "pre";

        // Collect attributes
        let attrs: HashMap<String, String> = element
            .attrs()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();

        // Compute text content (all descendant text concatenated)
        let text_content = compute_text_content(node, collapsed);

        // Compute is_blank
        let is_blank = compute_is_blank(node, &text_content);

        // Compute parent context
        let parent_tag = node.parent().and_then(|p| match p.value() {
            Node::Element(el) => Some(el.name().to_string()),
            _ => None,
        });

        let parent_attrs = node
            .parent()
            .and_then(|p| match p.value() {
                Node::Element(el) => Some(
                    el.attrs()
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default();

        // Sibling index (count preceding element siblings)
        let sibling_index = compute_sibling_index(node);

        // Has next sibling (element sibling)
        let has_next_sibling = has_next_element_sibling(node);

        // First child info
        let (first_child_tag, first_child_text, first_child_attrs) = compute_first_child_info(node);

        // Flanking whitespace (for inline elements)
        let flanking_whitespace = if !is_block {
            compute_flanking_whitespace(&text_content)
        } else {
            FlankingWhitespace::default()
        };

        let context = NodeContext {
            parent_tag,
            parent_attrs,
            sibling_index,
            has_next_sibling,
            first_child_tag,
            first_child_text,
            first_child_attrs,
            text_content,
            inside_pre,
        };

        NodeInfo::new(
            tag.to_string(),
            is_block,
            is_code,
            is_blank,
            flanking_whitespace,
            attrs,
            context,
        )
    }

    /// Post-process the converted output.
    ///
    /// - Collect rule appends (e.g., reference link definitions)
    /// - Trim leading tabs/carriage-returns/newlines
    /// - Trim trailing whitespace
    fn post_process(&self, mut output: String) -> String {
        // Collect appends from rules
        let appends = self.rules.collect_appends(&self.options);
        for append in appends {
            output.push_str(&append);
        }

        // Trim leading whitespace characters (tabs, \r, \n)
        let trimmed_start = output.trim_start_matches(['\t', '\r', '\n']);
        let start_offset = output.len() - trimmed_start.len();
        output = output[start_offset..].to_string();

        // Trim trailing whitespace
        let trimmed = output.trim_end();
        output.truncate(trimmed.len());

        output
    }
}

impl Default for HtmlToMarkdown {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// smart_join — joining two markdown fragments with appropriate newlines
// ---------------------------------------------------------------------------

/// Join two markdown fragments with appropriate newline separation.
///
/// - If either is empty, return the other.
/// - Count trailing newlines in left and leading newlines in right.
/// - Use the maximum of the two counts, capped at 2.
/// - Join with that many newlines as separator.
pub fn smart_join(left: String, right: String) -> String {
    if left.is_empty() {
        return right;
    }
    if right.is_empty() {
        return left;
    }

    let trailing_newlines = left.chars().rev().take_while(|&c| c == '\n').count();
    let leading_newlines = right.chars().take_while(|&c| c == '\n').count();

    let separator_count = trailing_newlines.max(leading_newlines).min(2);

    // Strip trailing newlines from left and leading newlines from right
    let left_trimmed = left.trim_end_matches('\n');
    let right_trimmed = right.trim_start_matches('\n');

    let separator: String = "\n".repeat(separator_count);
    format!("{}{}{}", left_trimmed, separator, right_trimmed)
}

// ---------------------------------------------------------------------------
// Helper functions for building NodeInfo
// ---------------------------------------------------------------------------

/// Check if any ancestor of this node is a `<pre>` element.
fn is_ancestor_pre(node: &NodeRef<Node>) -> bool {
    let mut current = node.parent();
    while let Some(parent) = current {
        if let Node::Element(el) = parent.value() {
            if el.name() == "pre" {
                return true;
            }
        }
        current = parent.parent();
    }
    false
}

/// Compute the text content of a node by concatenating all descendant text nodes
/// from the collapsed document.
fn compute_text_content(node: &NodeRef<Node>, collapsed: &CollapsedDocument) -> String {
    let mut text = String::new();
    collect_text_recursive(node, collapsed, &mut text);
    text
}

/// Recursively collect text from descendant text nodes.
fn collect_text_recursive(node: &NodeRef<Node>, collapsed: &CollapsedDocument, out: &mut String) {
    for child in node.children() {
        match child.value() {
            Node::Text(_) => {
                if let Some(t) = collapsed.get_text(child.id()) {
                    out.push_str(t);
                }
            }
            Node::Element(_) => {
                collect_text_recursive(&child, collapsed, out);
            }
            _ => {}
        }
    }
}

/// Compute whether a node is "blank" — only whitespace text and no void/meaningful descendants.
///
/// A void element (img, br, hr, etc.) is never blank — it has inherent meaning.
/// A meaningful-when-blank element (a, table, etc.) is never blank.
/// Otherwise, blank means: text content is all whitespace AND no void or
/// meaningful-when-blank descendant exists.
fn compute_is_blank(node: &NodeRef<Node>, text_content: &str) -> bool {
    // The node itself might be a void or meaningful-when-blank element
    if let Node::Element(el) = node.value() {
        let tag = el.name();
        if is_void_element(tag) || is_meaningful_when_blank(tag) {
            return false;
        }
    }

    if !text_content.trim().is_empty() {
        return false;
    }

    // Check if any descendant is a void element or meaningful-when-blank
    !has_meaningful_descendant(node)
}

/// Check if any descendant is a void element or meaningful-when-blank element.
fn has_meaningful_descendant(node: &NodeRef<Node>) -> bool {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            let name = el.name();
            if is_void_element(name) || is_meaningful_when_blank(name) {
                return true;
            }
            if has_meaningful_descendant(&child) {
                return true;
            }
        }
    }
    false
}

/// Compute the zero-based sibling index of a node among element siblings.
fn compute_sibling_index(node: &NodeRef<Node>) -> usize {
    let mut index = 0;
    let mut prev = node.prev_sibling();
    while let Some(sibling) = prev {
        if matches!(sibling.value(), Node::Element(_)) {
            index += 1;
        }
        prev = sibling.prev_sibling();
    }
    index
}

/// Check if there is a next element sibling after this node.
fn has_next_element_sibling(node: &NodeRef<Node>) -> bool {
    let mut next = node.next_sibling();
    while let Some(sibling) = next {
        if matches!(sibling.value(), Node::Element(_)) {
            return true;
        }
        next = sibling.next_sibling();
    }
    false
}

/// Compute first child element info (tag, text content, attributes).
fn compute_first_child_info(
    node: &NodeRef<Node>,
) -> (Option<String>, Option<String>, HashMap<String, String>) {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            let tag = el.name().to_string();
            let attrs: HashMap<String, String> = el
                .attrs()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();

            // First child text is the immediate text content of the first child
            let text = child.children().find_map(|c| match c.value() {
                Node::Text(t) => Some(t.text.to_string()),
                _ => None,
            });

            return (Some(tag), text, attrs);
        }
    }
    (None, None, HashMap::new())
}

/// Compute flanking whitespace for inline elements.
///
/// Leading whitespace: whitespace characters at the start of text_content.
/// Trailing whitespace: whitespace characters at the end of text_content.
fn compute_flanking_whitespace(text_content: &str) -> FlankingWhitespace {
    if text_content.is_empty() {
        return FlankingWhitespace::default();
    }

    let leading: String = text_content
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();

    let trailing: String = text_content
        .chars()
        .rev()
        .take_while(|c| c.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    // Don't count the same whitespace as both leading and trailing
    if leading.len() == text_content.len() {
        // The entire content is whitespace — treat as leading only
        return FlankingWhitespace {
            leading,
            trailing: String::new(),
        };
    }

    FlankingWhitespace { leading, trailing }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- smart_join tests ---

    #[test]
    fn test_smart_join_empty_left() {
        assert_eq!(smart_join(String::new(), "hello".to_string()), "hello");
    }

    #[test]
    fn test_smart_join_empty_right() {
        assert_eq!(smart_join("hello".to_string(), String::new()), "hello");
    }

    #[test]
    fn test_smart_join_both_empty() {
        assert_eq!(smart_join(String::new(), String::new()), "");
    }

    #[test]
    fn test_smart_join_no_newlines() {
        assert_eq!(
            smart_join("hello".to_string(), "world".to_string()),
            "helloworld"
        );
    }

    #[test]
    fn test_smart_join_trailing_newline_in_left() {
        assert_eq!(
            smart_join("hello\n".to_string(), "world".to_string()),
            "hello\nworld"
        );
    }

    #[test]
    fn test_smart_join_leading_newline_in_right() {
        assert_eq!(
            smart_join("hello".to_string(), "\nworld".to_string()),
            "hello\nworld"
        );
    }

    #[test]
    fn test_smart_join_two_newlines_max() {
        assert_eq!(
            smart_join("hello\n\n".to_string(), "world".to_string()),
            "hello\n\nworld"
        );
    }

    #[test]
    fn test_smart_join_caps_at_two() {
        assert_eq!(
            smart_join("hello\n\n\n".to_string(), "world".to_string()),
            "hello\n\nworld"
        );
    }

    #[test]
    fn test_smart_join_uses_max_of_both() {
        // Left has 1 trailing newline, right has 2 leading — use 2
        assert_eq!(
            smart_join("hello\n".to_string(), "\n\nworld".to_string()),
            "hello\n\nworld"
        );
    }

    #[test]
    fn test_smart_join_many_newlines_capped() {
        assert_eq!(
            smart_join("a\n\n\n\n".to_string(), "\n\n\n\nb".to_string()),
            "a\n\nb"
        );
    }

    // --- End-to-end conversion tests ---

    #[test]
    fn test_convert_empty_input() {
        let converter = HtmlToMarkdown::new();
        assert_eq!(converter.convert(""), "");
        assert_eq!(converter.convert("   "), "");
    }

    #[test]
    fn test_convert_simple_paragraph() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p>Hello world</p>");
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_convert_heading() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<h1>Title</h1>");
        assert_eq!(result, "# Title");
    }

    #[test]
    fn test_convert_h2() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<h2>Subtitle</h2>");
        assert_eq!(result, "## Subtitle");
    }

    #[test]
    fn test_convert_nested_structure() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<div><h1>Title</h1><p>Content here</p></div>");
        assert_eq!(result, "# Title\n\nContent here");
    }

    #[test]
    fn test_convert_code_block() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<pre><code>fn main() {}</code></pre>");
        assert_eq!(result, "```\nfn main() {}\n```");
    }

    #[test]
    fn test_convert_code_block_with_language() {
        let converter = HtmlToMarkdown::new();
        let result =
            converter.convert("<pre><code class=\"language-rust\">let x = 1;</code></pre>");
        assert_eq!(result, "```rust\nlet x = 1;\n```");
    }

    #[test]
    fn test_convert_inline_link() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p><a href=\"https://example.com\">Example</a></p>");
        assert_eq!(result, "[Example](https://example.com)");
    }

    #[test]
    fn test_convert_image() {
        let converter = HtmlToMarkdown::new();
        let result = converter
            .convert("<p><img src=\"https://example.com/img.png\" alt=\"A picture\" /></p>");
        assert_eq!(result, "![A picture](https://example.com/img.png)");
    }

    #[test]
    fn test_convert_emphasis() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p><em>italic</em> and <strong>bold</strong></p>");
        assert_eq!(result, "_italic_ and **bold**");
    }

    #[test]
    fn test_convert_unordered_list() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<ul><li>One</li><li>Two</li><li>Three</li></ul>");
        assert_eq!(result, "- One\n- Two\n- Three");
    }

    #[test]
    fn test_convert_ordered_list() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<ol><li>First</li><li>Second</li></ol>");
        assert_eq!(result, "1. First\n2. Second");
    }

    #[test]
    fn test_convert_blockquote() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<blockquote><p>A quote</p></blockquote>");
        assert_eq!(result, "> A quote");
    }

    #[test]
    fn test_convert_horizontal_rule() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p>Before</p><hr><p>After</p>");
        assert_eq!(result, "Before\n\n---\n\nAfter");
    }

    #[test]
    fn test_convert_inline_code() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p>Use <code>let x = 1</code> here</p>");
        assert_eq!(result, "Use `let x = 1` here");
    }

    #[test]
    fn test_convert_line_break() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p>Line one<br>Line two</p>");
        assert_eq!(result, "Line one  \nLine two");
    }

    #[test]
    fn test_convert_whitespace_collapsing() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p>Hello   world\n\n  test</p>");
        assert_eq!(result, "Hello world test");
    }

    #[test]
    fn test_post_process_trims_leading_newlines() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("\n\n<p>Hello</p>");
        assert_eq!(result, "Hello");
    }

    #[test]
    fn test_post_process_trims_trailing_whitespace() {
        let converter = HtmlToMarkdown::new();
        // Multiple paragraphs end with \n\n but post-process trims trailing
        let result = converter.convert("<p>Hello</p>  ");
        assert_eq!(result, "Hello");
    }

    // --- Flanking whitespace tests ---

    #[test]
    fn test_compute_flanking_whitespace_empty() {
        let fw = compute_flanking_whitespace("");
        assert_eq!(fw.leading, "");
        assert_eq!(fw.trailing, "");
    }

    #[test]
    fn test_compute_flanking_whitespace_no_whitespace() {
        let fw = compute_flanking_whitespace("hello");
        assert_eq!(fw.leading, "");
        assert_eq!(fw.trailing, "");
    }

    #[test]
    fn test_compute_flanking_whitespace_leading() {
        let fw = compute_flanking_whitespace(" hello");
        assert_eq!(fw.leading, " ");
        assert_eq!(fw.trailing, "");
    }

    #[test]
    fn test_compute_flanking_whitespace_trailing() {
        let fw = compute_flanking_whitespace("hello ");
        assert_eq!(fw.leading, "");
        assert_eq!(fw.trailing, " ");
    }

    #[test]
    fn test_compute_flanking_whitespace_both() {
        let fw = compute_flanking_whitespace(" hello ");
        assert_eq!(fw.leading, " ");
        assert_eq!(fw.trailing, " ");
    }

    #[test]
    fn test_compute_flanking_whitespace_all_whitespace() {
        let fw = compute_flanking_whitespace("   ");
        assert_eq!(fw.leading, "   ");
        assert_eq!(fw.trailing, "");
    }

    // --- is_ancestor_pre tests ---

    #[test]
    fn test_is_ancestor_pre_in_pre() {
        let doc = Html::parse_document("<pre><code>hello</code></pre>");
        for node_ref in doc.tree.nodes() {
            if let Node::Text(t) = node_ref.value() {
                if t.text.as_ref() == "hello" {
                    assert!(is_ancestor_pre(&node_ref));
                }
            }
        }
    }

    #[test]
    fn test_is_ancestor_pre_not_in_pre() {
        let doc = Html::parse_document("<p>hello</p>");
        for node_ref in doc.tree.nodes() {
            if let Node::Text(t) = node_ref.value() {
                if t.text.as_ref() == "hello" {
                    assert!(!is_ancestor_pre(&node_ref));
                }
            }
        }
    }

    // --- add_rule / remove / keep / use_plugin tests ---

    use rules::ConversionRule;

    /// A custom rule that converts <p> to uppercase-wrapped content.
    struct UppercaseParagraphRule;

    impl ConversionRule for UppercaseParagraphRule {
        fn filter(&self, node: &node_info::NodeInfo, _options: &options::Options) -> bool {
            node.tag_name() == "p"
        }

        fn replacement(
            &self,
            content: &str,
            _node: &node_info::NodeInfo,
            _options: &options::Options,
        ) -> String {
            format!("\n\n[UPPER:{}]\n\n", content.to_uppercase())
        }
    }

    /// A custom rule for <div> elements.
    struct CustomDivRule;

    impl ConversionRule for CustomDivRule {
        fn filter(&self, node: &node_info::NodeInfo, _options: &options::Options) -> bool {
            node.tag_name() == "div"
        }

        fn replacement(
            &self,
            content: &str,
            _node: &node_info::NodeInfo,
            _options: &options::Options,
        ) -> String {
            format!("\n\n[DIV:{}]\n\n", content)
        }
    }

    /// A custom rule for <span> elements.
    struct CustomSpanRule;

    impl ConversionRule for CustomSpanRule {
        fn filter(&self, node: &node_info::NodeInfo, _options: &options::Options) -> bool {
            node.tag_name() == "span"
        }

        fn replacement(
            &self,
            content: &str,
            _node: &node_info::NodeInfo,
            _options: &options::Options,
        ) -> String {
            format!("[SPAN:{}]", content)
        }
    }

    #[test]
    fn test_add_rule_overrides_builtin() {
        let mut converter = HtmlToMarkdown::new();
        converter.add_rule("custom_paragraph", Box::new(UppercaseParagraphRule));

        let result = converter.convert("<p>Hello world</p>");
        // The custom rule should override the built-in paragraph rule
        assert_eq!(result, "[UPPER:HELLO WORLD]");
    }

    #[test]
    fn test_remove_produces_empty_output() {
        let mut converter = HtmlToMarkdown::new();
        // Use a tag that has NO primary built-in rule so the remove rule applies
        converter.remove(RuleFilter::Tag("aside".to_string()));

        let result =
            converter.convert("<div><aside>This should be removed</aside><h1>Title</h1></div>");
        // The <aside> has no primary rule, so remove rule kicks in
        assert_eq!(result, "# Title");
    }

    #[test]
    fn test_remove_with_tags_filter() {
        let mut converter = HtmlToMarkdown::new();
        converter.remove(RuleFilter::Tags(vec![
            "nav".to_string(),
            "footer".to_string(),
        ]));

        // <nav> and <footer> have no primary rules, so remove applies
        let result =
            converter.convert("<div><nav>menu</nav><p>Content</p><footer>foot</footer></div>");
        assert_eq!(result, "Content");
    }

    #[test]
    fn test_keep_preserves_content() {
        let mut converter = HtmlToMarkdown::new();
        converter.keep(RuleFilter::Tag("custom-widget".to_string()));

        let result = converter.convert("<div><custom-widget>widget content</custom-widget></div>");
        // The custom-widget doesn't match any primary rule, so keep rule preserves content
        assert_eq!(result, "widget content");
    }

    #[test]
    fn test_use_plugin_adds_multiple_rules() {
        let mut converter = HtmlToMarkdown::new();

        converter.use_plugin(|c| {
            c.add_rule("custom_div", Box::new(CustomDivRule));
            c.add_rule("custom_span", Box::new(CustomSpanRule));
        });

        let result = converter.convert("<div><span>inner</span></div>");
        assert_eq!(result, "[DIV:[SPAN:inner]]");
    }

    #[test]
    fn test_use_plugin_can_add_remove_and_keep() {
        let mut converter = HtmlToMarkdown::new();

        converter.use_plugin(|c| {
            c.remove(RuleFilter::Tag("nav".to_string()));
            c.keep(RuleFilter::Tag("aside".to_string()));
        });

        // <nav> should produce empty output (no primary rule matches it)
        let result = converter.convert("<div><nav>navigation</nav><aside>sidebar</aside></div>");
        assert_eq!(result, "sidebar");
    }

    // --- Built-in removal rules for disallowed elements ---

    #[test]
    fn test_script_element_removed() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<script>alert('hi')</script>");
        assert_eq!(result, "");
    }

    #[test]
    fn test_style_element_removed() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<style>.foo { color: red; }</style>");
        assert_eq!(result, "");
    }

    #[test]
    fn test_script_inline_with_surrounding_text() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert("<p>Hello<script>bad</script> world</p>");
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_all_disallowed_elements_removed() {
        let converter = HtmlToMarkdown::new();

        // script
        assert_eq!(converter.convert("<script>var x = 1;</script>"), "");
        // style
        assert_eq!(converter.convert("<style>body{}</style>"), "");
        // noscript
        assert_eq!(converter.convert("<noscript>Enable JS</noscript>"), "");
        // iframe
        assert_eq!(
            converter.convert("<iframe src=\"https://example.com\"></iframe>"),
            ""
        );
        // svg
        assert_eq!(converter.convert("<svg><circle r=\"5\"/></svg>"), "");
        // canvas
        assert_eq!(converter.convert("<canvas>fallback</canvas>"), "");
        // template
        assert_eq!(converter.convert("<template><p>hidden</p></template>"), "");
        // head
        assert_eq!(converter.convert("<head><title>Page</title></head>"), "");
    }

    #[test]
    fn test_disallowed_elements_nested_content_suppressed() {
        let converter = HtmlToMarkdown::new();
        // Nested content within script should also be suppressed
        let result =
            converter.convert("<div><script><p>Should not appear</p></script><p>Visible</p></div>");
        assert_eq!(result, "Visible");
    }

    #[test]
    fn test_disallowed_elements_dont_affect_other_content() {
        let converter = HtmlToMarkdown::new();
        let result = converter.convert(
            "<div><style>.x{}</style><h1>Title</h1><script>bad();</script><p>Content</p></div>",
        );
        assert_eq!(result, "# Title\n\nContent");
    }

    // --- Property-based tests ---

    use proptest::prelude::*;

    proptest! {
        /// **Validates: Requirements 5.5**
        ///
        /// Property 6: Block elements produce output bounded by at most two newlines.
        /// smart_join never produces more than two consecutive newlines regardless of
        /// how many trailing/leading newlines the input fragments have.
        #[test]
        fn prop_smart_join_never_exceeds_two_newlines(
            left_content in "[a-z]{1,10}",
            right_content in "[a-z]{1,10}",
            left_newlines in 0usize..=5,
            right_newlines in 0usize..=5,
        ) {
            let left = format!("{}{}", left_content, "\n".repeat(left_newlines));
            let right = format!("{}{}", "\n".repeat(right_newlines), right_content);

            let result = smart_join(left, right);

            // Assert no more than 2 consecutive newlines anywhere in result
            prop_assert!(
                !result.contains("\n\n\n"),
                "smart_join produced more than 2 consecutive newlines: {:?}",
                result
            );
        }

        /// **Validates: Requirements 5.19, 13.8**
        ///
        /// Property 15: Removed elements produce empty output.
        /// HTML elements in the REMOVED_ELEMENTS list (script, style, noscript,
        /// iframe, svg, canvas, template, head) must never leak their content
        /// into the converted markdown output.
        ///
        /// Note: `<head>` requires wrapping content in `<title>` because the HTML5
        /// spec forbids raw text in `<head>` — the parser moves it to `<body>`.
        #[test]
        fn prop_removed_elements_produce_empty_output(
            tag_idx in 0usize..8,
            content in "[a-z ]{1,30}"
        ) {
            let tags = ["script", "style", "noscript", "iframe", "svg", "canvas", "template", "head"];
            let tag = tags[tag_idx];

            // For <head>, raw text is not valid — the HTML5 parser moves it to <body>.
            // Wrap content in <title> which is a valid child of <head>.
            let html = if tag == "head" {
                format!("<head><title>{}</title></head>", content)
            } else {
                format!("<{0}>{1}</{0}>", tag, content)
            };

            let converter = HtmlToMarkdown::new();
            let result = converter.convert(&html);

            // Result should not contain any of the original content
            prop_assert!(
                !result.contains(&content),
                "Removed element <{}> leaked content: {:?} (from html: {:?})",
                tag, result, html
            );
        }
    }
}
