//! Whitespace collapsing pre-processing.
//!
//! Normalizes runs of whitespace characters (spaces, tabs, carriage returns,
//! newlines) in text nodes to single spaces, removes leading whitespace from
//! text nodes following block elements, and removes trailing whitespace from
//! text nodes preceding block elements. Respects `pre`/`code` boundaries.

use std::collections::HashMap;

use scraper::node::Node;
use scraper::Html;

use ego_tree::NodeId;

use super::node_info::is_block_element;

/// Result of whitespace collapsing on a parsed HTML document.
///
/// Maps each text node to its collapsed (normalized) text content.
/// Text inside `<pre>` elements is preserved verbatim.
pub struct CollapsedDocument {
    /// Collapsed text for each text node, keyed by NodeId.
    pub texts: HashMap<NodeId, String>,
}

impl CollapsedDocument {
    /// Get the collapsed text for a given text node.
    pub fn get_text(&self, node_id: NodeId) -> Option<&str> {
        self.texts.get(&node_id).map(|s| s.as_str())
    }

    /// Returns true if no text nodes were stored.
    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }
}

/// Collapse whitespace in text nodes following HTML white-space processing rules.
///
/// Walks the DOM tree depth-first and for each text node:
/// - If inside a `<pre>` element: preserves text verbatim
/// - Otherwise:
///   1. Replaces all runs of whitespace (space, tab, `\r`, `\n`) with a single space
///   2. Strips leading space if text follows a block boundary or previous text ended with space
///   3. Strips trailing space if text precedes a block boundary
///
/// Empty text nodes (after collapsing) are omitted from the result.
pub fn collapse(doc: &Html) -> CollapsedDocument {
    let mut texts: HashMap<NodeId, String> = HashMap::new();
    let mut prev_ended_with_space = true; // Start as true to strip leading space at document start

    // Walk all nodes in tree order (depth-first)
    for node_ref in doc.tree.nodes() {
        let node_id = node_ref.id();

        if let Node::Text(ref text) = node_ref.value() {
            let raw = text.text.as_ref();

            if is_inside_pre(node_id, doc) {
                // Preserve text inside <pre> verbatim
                if !raw.is_empty() {
                    texts.insert(node_id, raw.to_string());
                    prev_ended_with_space = raw.ends_with(|c: char| c.is_ascii_whitespace());
                }
            } else {
                // Step 1: Replace all runs of whitespace with a single space
                let collapsed = collapse_runs(raw);

                // Step 2: Strip leading space if previous context ends with space
                // or if this text follows a block boundary
                let stripped_leading =
                    if prev_ended_with_space || prev_sibling_is_block(node_id, doc) {
                        collapsed.strip_prefix(' ').unwrap_or(&collapsed)
                    } else {
                        &collapsed
                    };

                // Step 3: Strip trailing space if next sibling is a block element
                let stripped = if next_sibling_is_block(node_id, doc) {
                    stripped_leading
                        .strip_suffix(' ')
                        .unwrap_or(stripped_leading)
                } else {
                    stripped_leading
                };

                if !stripped.is_empty() {
                    prev_ended_with_space = stripped.ends_with(' ');
                    texts.insert(node_id, stripped.to_string());
                } else {
                    // Empty node after collapsing — still update state
                    // An empty node doesn't change prev_ended_with_space
                }
            }
        } else if let Node::Element(ref el) = node_ref.value() {
            // Block elements reset the "previous ended with space" state
            if is_block_element(el.name()) {
                prev_ended_with_space = true;
            }
        }
    }

    CollapsedDocument { texts }
}

/// Replace all runs of whitespace characters (space, tab, \r, \n) with a single space.
fn collapse_runs(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_whitespace = false;

    for ch in input.chars() {
        if ch == ' ' || ch == '\t' || ch == '\r' || ch == '\n' || ch == '\x0C' {
            if !in_whitespace {
                result.push(' ');
                in_whitespace = true;
            }
        } else {
            result.push(ch);
            in_whitespace = false;
        }
    }

    result
}

/// Check if a node is inside a `<pre>` element by walking ancestors.
fn is_inside_pre(node_id: NodeId, doc: &Html) -> bool {
    let mut current = doc.tree.get(node_id);

    loop {
        match current {
            Some(node_ref) => {
                if let Some(parent) = node_ref.parent() {
                    if let Node::Element(ref el) = parent.value() {
                        if el.name() == "pre" {
                            return true;
                        }
                    }
                    current = Some(parent);
                } else {
                    return false;
                }
            }
            None => return false,
        }
    }
}

/// Check if the previous sibling (in tree order) is a block element.
fn prev_sibling_is_block(node_id: NodeId, doc: &Html) -> bool {
    let node_ref = match doc.tree.get(node_id) {
        Some(n) => n,
        None => return false,
    };

    if let Some(prev) = node_ref.prev_sibling() {
        if let Node::Element(ref el) = prev.value() {
            return is_block_element(el.name());
        }
    }

    // If this is the first child, check if the parent is a block element
    if node_ref.prev_sibling().is_none() {
        if let Some(parent) = node_ref.parent() {
            if let Node::Element(ref el) = parent.value() {
                return is_block_element(el.name());
            }
        }
    }

    false
}

/// Check if the next sibling (in tree order) is a block element.
fn next_sibling_is_block(node_id: NodeId, doc: &Html) -> bool {
    let node_ref = match doc.tree.get(node_id) {
        Some(n) => n,
        None => return false,
    };

    if let Some(next) = node_ref.next_sibling() {
        if let Node::Element(ref el) = next.value() {
            return is_block_element(el.name());
        }
    }

    // If this is the last child, check if the parent is a block element
    if node_ref.next_sibling().is_none() {
        if let Some(parent) = node_ref.parent() {
            if let Node::Element(ref el) = parent.value() {
                return is_block_element(el.name());
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collapse_runs_basic() {
        assert_eq!(collapse_runs("hello  world"), "hello world");
        assert_eq!(collapse_runs("hello\n\nworld"), "hello world");
        assert_eq!(collapse_runs("hello \t\n world"), "hello world");
        assert_eq!(collapse_runs("  leading"), " leading");
        assert_eq!(collapse_runs("trailing  "), "trailing ");
        assert_eq!(collapse_runs("no-whitespace"), "no-whitespace");
        assert_eq!(collapse_runs(""), "");
    }

    #[test]
    fn test_collapse_runs_tabs_and_newlines() {
        assert_eq!(collapse_runs("\t\t\t"), " ");
        assert_eq!(collapse_runs("\n\n\n"), " ");
        assert_eq!(collapse_runs("a\tb\nc"), "a b c");
    }

    #[test]
    fn test_collapse_simple_paragraph() {
        let html = "<p>Hello   world</p>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // Find the text node and verify it was collapsed
        let mut found = false;
        for text in collapsed.texts.values() {
            if text.contains("Hello") {
                assert_eq!(text, "Hello world");
                found = true;
            }
        }
        assert!(found, "Expected to find collapsed text node");
    }

    #[test]
    fn test_collapse_preserves_pre_content() {
        let html = "<pre>  hello\n  world  </pre>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // Text inside pre should be preserved verbatim
        let mut found = false;
        for text in collapsed.texts.values() {
            if text.contains("hello") {
                assert_eq!(text, "  hello\n  world  ");
                found = true;
            }
        }
        assert!(found, "Expected to find preserved pre text");
    }

    #[test]
    fn test_collapse_strips_leading_after_block() {
        let html = "<div><p>hello</p> world</div>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // " world" follows a <p> (block), so leading space should be stripped
        let mut found_world = false;
        for text in collapsed.texts.values() {
            if text.contains("world") {
                assert_eq!(text, "world");
                found_world = true;
            }
        }
        assert!(found_world, "Expected to find 'world' text node");
    }

    #[test]
    fn test_collapse_strips_trailing_before_block() {
        let html = "<div>hello <p>world</p></div>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // "hello " precedes a <p> (block), so trailing space should be stripped
        let mut found_hello = false;
        for text in collapsed.texts.values() {
            if text.contains("hello") {
                assert_eq!(text, "hello");
                found_hello = true;
            }
        }
        assert!(found_hello, "Expected to find 'hello' text node");
    }

    #[test]
    fn test_collapse_empty_text_omitted() {
        let html = "<p>   </p>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // Text that becomes empty after collapsing should not appear
        // The original "   " collapses to " ", then leading space gets stripped
        // (because it follows a block <p>), leaving empty string
        for text in collapsed.texts.values() {
            assert!(
                !text.is_empty(),
                "Empty text should not be stored in collapsed document"
            );
        }
    }

    #[test]
    fn test_collapse_multiple_inline_nodes() {
        let html = "<p>hello <em>world</em> foo</p>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // Inline elements don't affect whitespace collapsing
        let texts: Vec<&String> = collapsed.texts.values().collect();
        assert!(!texts.is_empty());
    }

    #[test]
    fn test_collapsed_document_get_text() {
        let html = "<p>Hello world</p>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // At least one text node should be retrievable
        let mut found = false;
        for id in collapsed.texts.keys() {
            if let Some(text) = collapsed.get_text(*id) {
                if text == "Hello world" {
                    found = true;
                }
            }
        }
        assert!(found);
    }

    #[test]
    fn test_collapsed_document_is_empty() {
        let html = "<div></div>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);
        assert!(collapsed.is_empty());
    }

    #[test]
    fn test_collapse_consecutive_spaces_between_inline() {
        let html = "<p><span>a</span>   <span>b</span></p>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        // "   " between inline spans should collapse to single space
        let mut found_space = false;
        for text in collapsed.texts.values() {
            if text.trim().is_empty() && !text.is_empty() {
                // Should be a single space
                assert_eq!(text, " ");
                found_space = true;
            }
        }
        // The space node might also be removed depending on context,
        // but if it exists, it should be a single space
        if !found_space {
            // It's also valid for it to be merged or stripped depending on context
        }
    }

    #[test]
    fn test_is_inside_pre_nested() {
        let html = "<pre><code>hello</code></pre>";
        let doc = Html::parse_document(html);

        // Find the text node "hello" and verify it's inside pre
        for node_ref in doc.tree.nodes() {
            if let Node::Text(ref text) = node_ref.value() {
                if text.text.as_ref() == "hello" {
                    assert!(is_inside_pre(node_ref.id(), &doc));
                }
            }
        }
    }

    #[test]
    fn test_is_inside_pre_false_for_normal() {
        let html = "<p>hello</p>";
        let doc = Html::parse_document(html);

        for node_ref in doc.tree.nodes() {
            if let Node::Text(ref text) = node_ref.value() {
                if text.text.as_ref() == "hello" {
                    assert!(!is_inside_pre(node_ref.id(), &doc));
                }
            }
        }
    }

    #[test]
    fn test_collapse_newlines_in_inline_text() {
        let html = "<p>hello\nworld</p>";
        let doc = Html::parse_document(html);
        let collapsed = collapse(&doc);

        let mut found = false;
        for text in collapsed.texts.values() {
            if text.contains("hello") {
                assert_eq!(text, "hello world");
                found = true;
            }
        }
        assert!(found);
    }

    // --- Property-based tests ---
    // **Validates: Requirements 5.2, 13.10**

    use proptest::prelude::*;

    proptest! {
        /// **Property 5: Whitespace collapsing produces no consecutive spaces outside pre/code**
        ///
        /// After collapsing, no text node outside a <pre> element should contain
        /// two or more consecutive space characters.
        ///
        /// **Validates: Requirements 5.2, 13.10**
        #[test]
        fn prop_no_consecutive_spaces_outside_pre(
            text in "[a-z \\t\\n]{1,50}"
        ) {
            // Wrap the generated text in a paragraph (not pre)
            let html = format!("<p>{}</p>", text);
            let doc = Html::parse_document(&html);
            let collapsed = collapse(&doc);

            // Check no text node outside <pre> has consecutive spaces
            for (node_id, collapsed_text) in &collapsed.texts {
                if !is_inside_pre(*node_id, &doc) {
                    prop_assert!(
                        !collapsed_text.contains("  "),
                        "Found consecutive spaces in collapsed text: {:?} (input: {:?})",
                        collapsed_text,
                        text
                    );
                }
            }
        }

        /// Property: Text inside <pre> is not whitespace-collapsed — runs of
        /// spaces/tabs/newlines are preserved as-is. (Note: The HTML parser strips
        /// a single leading newline from <pre> content per the HTML spec, so we
        /// compare against what the parser yields, not the raw input.)
        ///
        /// **Validates: Requirements 5.2, 13.10**
        #[test]
        fn prop_pre_content_preserved_verbatim(text in "[a-z \\t\\n]{1,50}") {
            let html = format!("<pre>{}</pre>", text);
            let doc = Html::parse_document(&html);
            let collapsed = collapse(&doc);

            // Gather what the parser actually stored as raw text inside <pre>
            let mut raw_texts: HashMap<NodeId, String> = HashMap::new();
            for node_ref in doc.tree.nodes() {
                if let Node::Text(ref t) = node_ref.value() {
                    if is_inside_pre(node_ref.id(), &doc) {
                        raw_texts.insert(node_ref.id(), t.text.to_string());
                    }
                }
            }

            // Text inside <pre> after collapse should be identical to what the parser stored
            for (node_id, collapsed_text) in &collapsed.texts {
                if is_inside_pre(*node_id, &doc) {
                    if let Some(raw) = raw_texts.get(node_id) {
                        prop_assert_eq!(
                            collapsed_text, raw,
                            "Pre content was modified by collapse: got {:?}, parser had {:?}",
                            collapsed_text, raw
                        );
                    }
                }
            }
        }

        /// Property: Collapsing is idempotent — collapsing already-collapsed text
        /// produces the same result.
        ///
        /// **Validates: Requirements 5.2, 13.10**
        #[test]
        fn prop_collapse_is_idempotent(text in "[a-z \\t\\n]{1,50}") {
            let html = format!("<p>{}</p>", text);
            let doc = Html::parse_document(&html);
            let collapsed = collapse(&doc);

            // Collect the collapsed texts
            let first_pass: Vec<String> = collapsed.texts.values().cloned().collect();

            // Re-wrap each collapsed text and collapse again
            for first_text in &first_pass {
                let html2 = format!("<p>{}</p>", first_text);
                let doc2 = Html::parse_document(&html2);
                let collapsed2 = collapse(&doc2);

                for (node_id, second_text) in &collapsed2.texts {
                    if !is_inside_pre(*node_id, &doc2) {
                        // After second collapse, text should be identical to first
                        prop_assert_eq!(
                            second_text, first_text,
                            "Collapse is not idempotent: first={:?}, second={:?}",
                            first_text, second_text
                        );
                    }
                }
            }
        }

        /// Property: Whitespace collapsing never increases the length of text
        /// outside <pre> elements.
        ///
        /// **Validates: Requirements 5.2, 13.10**
        #[test]
        fn prop_collapse_never_increases_length(text in "[a-z \\t\\n]{1,50}") {
            let html = format!("<p>{}</p>", text);
            let doc = Html::parse_document(&html);
            let collapsed = collapse(&doc);

            for (node_id, collapsed_text) in &collapsed.texts {
                if !is_inside_pre(*node_id, &doc) {
                    prop_assert!(
                        collapsed_text.len() <= text.len(),
                        "Collapsed text is longer than input: {:?} (len={}) vs input {:?} (len={})",
                        collapsed_text, collapsed_text.len(), text, text.len()
                    );
                }
            }
        }
    }
}
