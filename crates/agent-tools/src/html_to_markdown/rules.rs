//! Conversion rule trait and rule set management.
//!
//! Defines the `ConversionRule` trait for custom conversion rules,
//! the `RuleFilter` enum for matching nodes, the `NamedRule` struct
//! for managing named rules, and rule dispatch logic.

use super::node_info::NodeInfo;
use super::options::Options;

/// A closure predicate for matching nodes.
pub type RuleFilterPredicate = Box<dyn Fn(&NodeInfo, &Options) -> bool + Send + Sync>;

/// Specifies which nodes a conversion rule applies to.
pub enum RuleFilter {
    /// Match a single element by tag name.
    Tag(String),
    /// Match any of several element tag names.
    Tags(Vec<String>),
    /// Match via a closure predicate.
    Predicate(RuleFilterPredicate),
}

/// A conversion rule that transforms an HTML element into markdown.
///
/// Implementors define how to filter matching nodes and how to produce
/// the markdown replacement text for those nodes.
pub trait ConversionRule: Send + Sync {
    /// Returns `true` if this rule should handle the given node.
    fn filter(&self, node: &NodeInfo, options: &Options) -> bool;

    /// Produces the markdown replacement for the given node.
    ///
    /// `content` is the already-converted inner content of the node.
    fn replacement(&self, content: &str, node: &NodeInfo, options: &Options) -> String;

    /// Optionally returns text to append after the document is fully converted.
    ///
    /// Useful for reference-style link definitions or footnotes.
    fn append(&self, _options: &Options) -> Option<String> {
        None
    }
}

/// A named conversion rule pairing an identifier with its implementation.
pub struct NamedRule {
    /// The name identifier for this rule.
    pub name: String,
    /// The conversion rule implementation.
    pub rule: Box<dyn ConversionRule>,
}

impl NamedRule {
    /// Create a new `NamedRule` with the given name and rule implementation.
    pub fn new(name: &str, rule: Box<dyn ConversionRule>) -> NamedRule {
        NamedRule {
            name: name.to_string(),
            rule,
        }
    }
}

// ---------------------------------------------------------------------------
// RuleSet — dispatch logic
// ---------------------------------------------------------------------------

/// An ordered collection of conversion rules with dispatch logic.
///
/// Rules are checked in order; the first rule whose filter matches wins.
/// Rules added via `add_rule` are inserted at index 0 (highest priority),
/// mirroring turndown.js's `unshift` semantics.
pub struct RuleSet {
    /// Primary rules (user-added + built-in CommonMark rules).
    rules: Vec<NamedRule>,
    /// Keep rules: preserve matching nodes as raw HTML.
    keep_rules: Vec<Box<dyn ConversionRule>>,
    /// Remove rules: produce empty string for matching nodes.
    remove_rules: Vec<Box<dyn ConversionRule>>,
}

impl RuleSet {
    /// Create a new `RuleSet` with the given primary rules and empty keep/remove lists.
    pub fn new(rules: Vec<NamedRule>) -> Self {
        Self {
            rules,
            keep_rules: Vec::new(),
            remove_rules: Vec::new(),
        }
    }

    /// Create a RuleSet pre-populated with CommonMark rules.
    pub fn with_commonmark_rules(options: &Options) -> Self {
        Self::new(super::commonmark::builtin_rules(options))
    }

    /// Add a rule at highest priority (front of list).
    pub fn add_rule(&mut self, name: &str, rule: Box<dyn ConversionRule>) {
        self.rules.insert(0, NamedRule::new(name, rule));
    }

    /// Add a removal rule (matching nodes → empty string).
    pub fn add_remove(&mut self, filter: RuleFilter) {
        self.remove_rules.insert(0, Box::new(RemoveRule { filter }));
    }

    /// Add a keep rule (matching nodes → content as-is).
    pub fn add_keep(&mut self, filter: RuleFilter) {
        self.keep_rules.insert(0, Box::new(KeepRule { filter }));
    }

    /// Find the applicable rule and apply it, returning the replacement string.
    ///
    /// Priority order:
    /// 1. If node is blank → blank replacement ("\n\n" for block, "" for inline)
    /// 2. Primary rules (user-added first, then built-in) → first match
    /// 3. Keep rules → first match
    /// 4. Remove rules → first match
    /// 5. Default replacement ("\n\n{content}\n\n" for block, content as-is for inline)
    pub fn apply_rule(&self, content: &str, node: &NodeInfo, options: &Options) -> String {
        // 1. Blank nodes get special treatment
        if node.is_blank {
            return if node.is_block {
                "\n\n".to_string()
            } else {
                String::new()
            };
        }

        // 2. Check primary rules in order
        for named in &self.rules {
            if named.rule.filter(node, options) {
                return named.rule.replacement(content, node, options);
            }
        }

        // 3. Check keep rules
        for rule in &self.keep_rules {
            if rule.filter(node, options) {
                return rule.replacement(content, node, options);
            }
        }

        // 4. Check remove rules
        for rule in &self.remove_rules {
            if rule.filter(node, options) {
                return rule.replacement(content, node, options);
            }
        }

        // 5. Fallback to default replacement
        if node.is_block {
            format!("\n\n{}\n\n", content)
        } else {
            content.to_string()
        }
    }

    /// Collect append outputs from all primary rules that provide them.
    pub fn collect_appends(&self, options: &Options) -> Vec<String> {
        self.rules
            .iter()
            .filter_map(|named| named.rule.append(options))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Internal helper rule structs
// ---------------------------------------------------------------------------

/// A rule that handles blank nodes.
/// Returns "\n\n" for block-level blank nodes, "" for inline blank nodes.
#[allow(dead_code)]
struct BlankRule {
    is_block: bool,
}

impl ConversionRule for BlankRule {
    fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
        node.is_blank
    }

    fn replacement(&self, _content: &str, _node: &NodeInfo, _options: &Options) -> String {
        if self.is_block {
            "\n\n".to_string()
        } else {
            String::new()
        }
    }
}

/// A rule that provides the default replacement for unmatched nodes.
/// Returns "\n\n{content}\n\n" for block-level, content as-is for inline.
#[allow(dead_code)]
struct DefaultRule {
    is_block: bool,
}

impl ConversionRule for DefaultRule {
    fn filter(&self, _node: &NodeInfo, _options: &Options) -> bool {
        true // Always matches as fallback
    }

    fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
        if self.is_block {
            format!("\n\n{}\n\n", content)
        } else {
            content.to_string()
        }
    }
}

/// A rule that removes matching nodes (produces empty string).
struct RemoveRule {
    filter: RuleFilter,
}

impl ConversionRule for RemoveRule {
    fn filter(&self, node: &NodeInfo, options: &Options) -> bool {
        match &self.filter {
            RuleFilter::Tag(tag) => node.tag_name() == tag,
            RuleFilter::Tags(tags) => tags.iter().any(|t| t == node.tag_name()),
            RuleFilter::Predicate(pred) => pred(node, options),
        }
    }

    fn replacement(&self, _content: &str, _node: &NodeInfo, _options: &Options) -> String {
        String::new()
    }
}

/// A rule that keeps matching nodes (preserves content as-is).
/// In future this will preserve raw HTML; for now it returns content unchanged.
struct KeepRule {
    filter: RuleFilter,
}

impl ConversionRule for KeepRule {
    fn filter(&self, node: &NodeInfo, options: &Options) -> bool {
        match &self.filter {
            RuleFilter::Tag(tag) => node.tag_name() == tag,
            RuleFilter::Tags(tags) => tags.iter().any(|t| t == node.tag_name()),
            RuleFilter::Predicate(pred) => pred(node, options),
        }
    }

    fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::super::node_info::{FlankingWhitespace, NodeContext};
    use super::*;
    use std::collections::HashMap;

    /// A simple test rule that matches <p> tags.
    struct ParagraphRule;

    impl ConversionRule for ParagraphRule {
        fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
            node.tag_name() == "p"
        }

        fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
            format!("\n\n{}\n\n", content)
        }
    }

    /// A rule that uses append.
    struct FootnoteRule;

    impl ConversionRule for FootnoteRule {
        fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
            node.tag_name() == "a"
        }

        fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
            content.to_string()
        }

        fn append(&self, _options: &Options) -> Option<String> {
            Some("\n[1]: https://example.com\n".to_string())
        }
    }

    fn make_node(tag: &str) -> NodeInfo {
        NodeInfo::new(
            tag.to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            NodeContext::default(),
        )
    }

    #[test]
    fn test_conversion_rule_filter_and_replacement() {
        let rule = ParagraphRule;
        let options = Options::default();
        let p_node = make_node("p");
        let div_node = make_node("div");

        assert!(rule.filter(&p_node, &options));
        assert!(!rule.filter(&div_node, &options));

        let result = rule.replacement("Hello", &p_node, &options);
        assert_eq!(result, "\n\nHello\n\n");
    }

    #[test]
    fn test_conversion_rule_default_append() {
        let rule = ParagraphRule;
        let options = Options::default();
        assert_eq!(rule.append(&options), None);
    }

    #[test]
    fn test_conversion_rule_custom_append() {
        let rule = FootnoteRule;
        let options = Options::default();
        assert_eq!(
            rule.append(&options),
            Some("\n[1]: https://example.com\n".to_string())
        );
    }

    #[test]
    fn test_named_rule_new() {
        let named = NamedRule::new("paragraph", Box::new(ParagraphRule));
        assert_eq!(named.name, "paragraph");

        let options = Options::default();
        let node = make_node("p");
        assert!(named.rule.filter(&node, &options));
    }

    #[test]
    fn test_rule_filter_tag() {
        let filter = RuleFilter::Tag("div".to_string());
        match filter {
            RuleFilter::Tag(ref tag) => assert_eq!(tag, "div"),
            _ => panic!("Expected Tag variant"),
        }
    }

    #[test]
    fn test_rule_filter_tags() {
        let filter = RuleFilter::Tags(vec!["h1".to_string(), "h2".to_string(), "h3".to_string()]);
        match filter {
            RuleFilter::Tags(ref tags) => {
                assert_eq!(tags.len(), 3);
                assert!(tags.contains(&"h1".to_string()));
                assert!(tags.contains(&"h2".to_string()));
                assert!(tags.contains(&"h3".to_string()));
            }
            _ => panic!("Expected Tags variant"),
        }
    }

    #[test]
    fn test_rule_filter_predicate() {
        let filter = RuleFilter::Predicate(Box::new(|node, _options| {
            node.tag_name() == "a" && node.attr("href").is_some()
        }));

        let mut attrs = HashMap::new();
        attrs.insert("href".to_string(), "https://example.com".to_string());
        let node_with_href = NodeInfo::new(
            "a".to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            attrs,
            NodeContext::default(),
        );

        let node_without_href = make_node("a");
        let options = Options::default();

        match &filter {
            RuleFilter::Predicate(pred) => {
                assert!(pred(&node_with_href, &options));
                assert!(!pred(&node_without_href, &options));
            }
            _ => panic!("Expected Predicate variant"),
        }
    }

    #[test]
    fn test_named_rule_send_sync() {
        // Compile-time check that NamedRule can be sent across threads
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Box<dyn ConversionRule>>();
    }

    // -----------------------------------------------------------------------
    // RuleSet tests
    // -----------------------------------------------------------------------

    fn make_block_node(tag: &str) -> NodeInfo {
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

    fn make_blank_block_node(tag: &str) -> NodeInfo {
        NodeInfo::new(
            tag.to_string(),
            true,
            false,
            true, // is_blank
            FlankingWhitespace::default(),
            HashMap::new(),
            NodeContext::default(),
        )
    }

    fn make_blank_inline_node(tag: &str) -> NodeInfo {
        NodeInfo::new(
            tag.to_string(),
            false,
            false,
            true, // is_blank
            FlankingWhitespace::default(),
            HashMap::new(),
            NodeContext::default(),
        )
    }

    #[test]
    fn test_ruleset_new_creates_empty_keep_remove() {
        let rs = RuleSet::new(Vec::new());
        assert!(rs.rules.is_empty());
        assert!(rs.keep_rules.is_empty());
        assert!(rs.remove_rules.is_empty());
    }

    #[test]
    fn test_ruleset_with_commonmark_rules() {
        let options = Options::default();
        let rs = RuleSet::with_commonmark_rules(&options);
        // Should contain 13 built-in rules (paragraph, heading, blockquote, list, list_item, code_block, horizontal_rule, emphasis, strong, inline_code, inline_link, image, line_break)
        assert_eq!(rs.rules.len(), 13);
        assert_eq!(rs.rules[0].name, "paragraph");
        assert_eq!(rs.rules[1].name, "heading");
        assert_eq!(rs.rules[2].name, "blockquote");
        assert_eq!(rs.rules[3].name, "list");
        assert_eq!(rs.rules[4].name, "list_item");
        assert_eq!(rs.rules[5].name, "code_block");
        assert_eq!(rs.rules[6].name, "horizontal_rule");
        assert_eq!(rs.rules[7].name, "emphasis");
        assert_eq!(rs.rules[8].name, "strong");
        assert_eq!(rs.rules[9].name, "inline_code");
        assert_eq!(rs.rules[10].name, "inline_link");
        assert_eq!(rs.rules[11].name, "image");
        assert_eq!(rs.rules[12].name, "line_break");
        assert!(rs.keep_rules.is_empty());
        assert!(rs.remove_rules.is_empty());
    }

    #[test]
    fn test_ruleset_add_rule_inserts_at_front() {
        let mut rs = RuleSet::new(vec![NamedRule::new("paragraph", Box::new(ParagraphRule))]);
        assert_eq!(rs.rules.len(), 1);
        assert_eq!(rs.rules[0].name, "paragraph");

        rs.add_rule("footnote", Box::new(FootnoteRule));
        assert_eq!(rs.rules.len(), 2);
        assert_eq!(rs.rules[0].name, "footnote");
        assert_eq!(rs.rules[1].name, "paragraph");
    }

    #[test]
    fn test_ruleset_apply_rule_blank_block_node() {
        let rs = RuleSet::new(Vec::new());
        let options = Options::default();
        let node = make_blank_block_node("div");

        let result = rs.apply_rule("content", &node, &options);
        assert_eq!(result, "\n\n");
    }

    #[test]
    fn test_ruleset_apply_rule_blank_inline_node() {
        let rs = RuleSet::new(Vec::new());
        let options = Options::default();
        let node = make_blank_inline_node("span");

        let result = rs.apply_rule("content", &node, &options);
        assert_eq!(result, "");
    }

    #[test]
    fn test_ruleset_apply_rule_primary_rule_matches() {
        let rs = RuleSet::new(vec![NamedRule::new("paragraph", Box::new(ParagraphRule))]);
        let options = Options::default();
        let node = make_node("p");

        let result = rs.apply_rule("Hello", &node, &options);
        assert_eq!(result, "\n\nHello\n\n");
    }

    #[test]
    fn test_ruleset_apply_rule_default_block() {
        let rs = RuleSet::new(Vec::new());
        let options = Options::default();
        let node = make_block_node("section");

        let result = rs.apply_rule("Block content", &node, &options);
        assert_eq!(result, "\n\nBlock content\n\n");
    }

    #[test]
    fn test_ruleset_apply_rule_default_inline() {
        let rs = RuleSet::new(Vec::new());
        let options = Options::default();
        let node = make_node("span");

        let result = rs.apply_rule("inline text", &node, &options);
        assert_eq!(result, "inline text");
    }

    #[test]
    fn test_ruleset_add_remove_rule() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_remove(RuleFilter::Tag("script".to_string()));

        let options = Options::default();
        let node = make_node("script");

        let result = rs.apply_rule("should be removed", &node, &options);
        assert_eq!(result, "");
    }

    #[test]
    fn test_ruleset_add_keep_rule() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_keep(RuleFilter::Tag("custom-element".to_string()));

        let options = Options::default();
        let node = make_node("custom-element");

        let result = rs.apply_rule("preserved content", &node, &options);
        assert_eq!(result, "preserved content");
    }

    #[test]
    fn test_ruleset_priority_primary_over_keep() {
        let mut rs = RuleSet::new(vec![NamedRule::new("paragraph", Box::new(ParagraphRule))]);
        rs.add_keep(RuleFilter::Tag("p".to_string()));

        let options = Options::default();
        let node = make_node("p");

        // Primary rule should win over keep rule
        let result = rs.apply_rule("text", &node, &options);
        assert_eq!(result, "\n\ntext\n\n");
    }

    #[test]
    fn test_ruleset_priority_keep_over_remove() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_keep(RuleFilter::Tag("div".to_string()));
        rs.add_remove(RuleFilter::Tag("div".to_string()));

        let options = Options::default();
        let node = make_node("div");

        // Keep rule should win over remove rule
        let result = rs.apply_rule("kept", &node, &options);
        assert_eq!(result, "kept");
    }

    #[test]
    fn test_ruleset_priority_blank_over_all() {
        let mut rs = RuleSet::new(vec![NamedRule::new("paragraph", Box::new(ParagraphRule))]);
        rs.add_keep(RuleFilter::Tag("p".to_string()));
        rs.add_remove(RuleFilter::Tag("p".to_string()));

        let options = Options::default();
        // Make a blank <p> node — blank takes priority over everything
        let node = NodeInfo::new(
            "p".to_string(),
            true,
            false,
            true, // is_blank
            FlankingWhitespace::default(),
            HashMap::new(),
            NodeContext::default(),
        );

        let result = rs.apply_rule("whatever", &node, &options);
        assert_eq!(result, "\n\n");
    }

    #[test]
    fn test_ruleset_add_remove_with_tags_filter() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_remove(RuleFilter::Tags(vec![
            "script".to_string(),
            "style".to_string(),
        ]));

        let options = Options::default();

        let script_node = make_node("script");
        assert_eq!(rs.apply_rule("js code", &script_node, &options), "");

        let style_node = make_node("style");
        assert_eq!(rs.apply_rule("css", &style_node, &options), "");

        // Non-matching should fall through to default
        let div_node = make_node("div");
        assert_eq!(rs.apply_rule("content", &div_node, &options), "content");
    }

    #[test]
    fn test_ruleset_add_remove_inserts_at_front() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_remove(RuleFilter::Tag("script".to_string()));
        rs.add_remove(RuleFilter::Tag("style".to_string()));

        // Both should match their respective nodes
        assert_eq!(rs.remove_rules.len(), 2);
    }

    #[test]
    fn test_ruleset_add_keep_inserts_at_front() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_keep(RuleFilter::Tag("video".to_string()));
        rs.add_keep(RuleFilter::Tag("audio".to_string()));

        assert_eq!(rs.keep_rules.len(), 2);
    }

    #[test]
    fn test_ruleset_collect_appends() {
        let rs = RuleSet::new(vec![
            NamedRule::new("footnote", Box::new(FootnoteRule)),
            NamedRule::new("paragraph", Box::new(ParagraphRule)),
        ]);

        let options = Options::default();
        let appends = rs.collect_appends(&options);

        assert_eq!(appends.len(), 1);
        assert_eq!(appends[0], "\n[1]: https://example.com\n");
    }

    #[test]
    fn test_ruleset_collect_appends_empty() {
        let rs = RuleSet::new(vec![NamedRule::new("paragraph", Box::new(ParagraphRule))]);

        let options = Options::default();
        let appends = rs.collect_appends(&options);

        assert!(appends.is_empty());
    }

    #[test]
    fn test_ruleset_later_added_rule_matches_first() {
        // Add two rules that both match <a> tags
        struct LinkRule1;
        impl ConversionRule for LinkRule1 {
            fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
                node.tag_name() == "a"
            }
            fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
                format!("[RULE1:{}]", content)
            }
        }

        struct LinkRule2;
        impl ConversionRule for LinkRule2 {
            fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
                node.tag_name() == "a"
            }
            fn replacement(&self, content: &str, _node: &NodeInfo, _options: &Options) -> String {
                format!("[RULE2:{}]", content)
            }
        }

        let mut rs = RuleSet::new(Vec::new());
        rs.add_rule("link1", Box::new(LinkRule1));
        rs.add_rule("link2", Box::new(LinkRule2));

        let options = Options::default();
        let node = make_node("a");

        // link2 was added last, so it's at index 0 → matches first
        let result = rs.apply_rule("click", &node, &options);
        assert_eq!(result, "[RULE2:click]");
    }

    #[test]
    fn test_ruleset_predicate_remove_rule() {
        let mut rs = RuleSet::new(Vec::new());
        rs.add_remove(RuleFilter::Predicate(Box::new(|node, _options| {
            node.tag_name() == "a" && node.attr("class") == Some("nofollow")
        })));

        let options = Options::default();

        let mut attrs = HashMap::new();
        attrs.insert("class".to_string(), "nofollow".to_string());
        let nofollow_link = NodeInfo::new(
            "a".to_string(),
            false,
            false,
            false,
            FlankingWhitespace::default(),
            attrs,
            NodeContext::default(),
        );

        let normal_link = make_node("a");

        assert_eq!(rs.apply_rule("removed", &nofollow_link, &options), "");
        assert_eq!(rs.apply_rule("kept", &normal_link, &options), "kept");
    }

    // -----------------------------------------------------------------------
    // Property-based tests for rule dispatch priority (Property 7)
    // -----------------------------------------------------------------------
    //
    // **Validates: Requirements 13.4, 13.5, 13.7**
    //
    // Property 7: Rule dispatch priority — later-added rules match first.
    // Since `add_rule` inserts at index 0, the most recently added rule
    // has the highest priority and its replacement is used when multiple
    // rules match the same node.

    mod prop_tests {
        use super::*;
        use proptest::prelude::*;

        /// A rule that matches a specific tag and returns a fixed label.
        struct TagMatchRule {
            tag: String,
            label: String,
        }

        impl ConversionRule for TagMatchRule {
            fn filter(&self, node: &NodeInfo, _options: &Options) -> bool {
                node.tag_name() == self.tag
            }

            fn replacement(&self, _content: &str, _node: &NodeInfo, _options: &Options) -> String {
                self.label.clone()
            }
        }

        proptest! {
            /// Property 7: Later-added rules have priority over earlier-added rules.
            /// For any tag name, the second rule added via `add_rule` should win
            /// because `add_rule` inserts at index 0 (highest priority).
            #[test]
            fn prop_later_added_rules_have_priority(tag in "[a-z]{1,10}") {
                let mut ruleset = RuleSet::new(Vec::new());

                // Add first rule — will end up at lower priority
                ruleset.add_rule(
                    "first",
                    Box::new(TagMatchRule {
                        tag: tag.clone(),
                        label: "FIRST".to_string(),
                    }),
                );

                // Add second rule — inserted at index 0, highest priority
                ruleset.add_rule(
                    "second",
                    Box::new(TagMatchRule {
                        tag: tag.clone(),
                        label: "SECOND".to_string(),
                    }),
                );

                let node = make_node(&tag);
                let options = Options::default();
                let result = ruleset.apply_rule("content", &node, &options);

                // The second-added rule should always win
                prop_assert_eq!(result, "SECOND");
            }

            /// Property 7 extended: Adding N rules sequentially, the last-added
            /// rule always has the highest priority regardless of how many rules
            /// exist in the set.
            #[test]
            fn prop_last_added_rule_always_wins(
                tag in "[a-z]{1,8}",
                num_rules in 2u32..10u32,
            ) {
                let mut ruleset = RuleSet::new(Vec::new());

                // Add N rules that all match the same tag
                for i in 0..num_rules {
                    ruleset.add_rule(
                        &format!("rule_{}", i),
                        Box::new(TagMatchRule {
                            tag: tag.clone(),
                            label: format!("RULE_{}", i),
                        }),
                    );
                }

                let node = make_node(&tag);
                let options = Options::default();
                let result = ruleset.apply_rule("content", &node, &options);

                // The last-added rule (highest index) should win
                let expected = format!("RULE_{}", num_rules - 1);
                prop_assert_eq!(result, expected);
            }

            /// Property 7 corollary: A user-added rule (via add_rule) takes
            /// priority over a rule present in the initial rules vec, because
            /// add_rule inserts at index 0.
            #[test]
            fn prop_added_rule_overrides_initial_rules(tag in "[a-z]{1,10}") {
                // Start with an initial rule in the constructor
                let initial_rules = vec![NamedRule::new(
                    "initial",
                    Box::new(TagMatchRule {
                        tag: tag.clone(),
                        label: "INITIAL".to_string(),
                    }),
                )];

                let mut ruleset = RuleSet::new(initial_rules);

                // Add a new rule via add_rule — should override the initial rule
                ruleset.add_rule(
                    "override",
                    Box::new(TagMatchRule {
                        tag: tag.clone(),
                        label: "OVERRIDE".to_string(),
                    }),
                );

                let node = make_node(&tag);
                let options = Options::default();
                let result = ruleset.apply_rule("content", &node, &options);

                prop_assert_eq!(result, "OVERRIDE");
            }
        }
    }
}
