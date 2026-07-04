//! Node information and classification utilities.
//!
//! Provides the `NodeInfo` struct and helpers for classifying HTML elements
//! as block-level or inline, detecting void elements, computing flanking
//! whitespace, and determining blank status.

use std::collections::HashMap;

/// HTML elements that generate block-level formatting (paragraph breaks).
pub const BLOCK_ELEMENTS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "center",
    "dd",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "menu",
    "nav",
    "ol",
    "output",
    "p",
    "pre",
    "section",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
];

/// HTML void elements (self-closing, cannot have children).
pub const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "command", "embed", "hr", "img", "input", "keygen", "link",
    "meta", "param", "source", "track", "wbr",
];

/// Elements that are considered meaningful even when they contain only whitespace.
pub const MEANINGFUL_WHEN_BLANK: &[&str] = &[
    "a", "table", "thead", "tbody", "tfoot", "th", "td", "iframe", "script", "audio", "video",
];

/// Returns true if the given tag name is a block-level element.
pub fn is_block_element(tag: &str) -> bool {
    BLOCK_ELEMENTS.contains(&tag)
}

/// Returns true if the given tag name is a void element.
pub fn is_void_element(tag: &str) -> bool {
    VOID_ELEMENTS.contains(&tag)
}

/// Returns true if the given tag name is meaningful when blank.
pub fn is_meaningful_when_blank(tag: &str) -> bool {
    MEANINGFUL_WHEN_BLANK.contains(&tag)
}

/// Flanking whitespace extracted from an inline node's text content.
///
/// Leading whitespace is extracted before conversion and trailing whitespace
/// after, so they can be re-attached outside the conversion rule output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlankingWhitespace {
    /// Whitespace before the node's content.
    pub leading: String,
    /// Whitespace after the node's content.
    pub trailing: String,
}

/// Contextual information about a node's position in the DOM tree.
///
/// Contains information about the node's parent, siblings, and first child
/// that conversion rules may need for decision-making.
#[derive(Debug, Clone, Default)]
pub struct NodeContext {
    /// Tag name of the parent element, if any.
    pub parent_tag: Option<String>,
    /// Attributes of the parent element.
    pub parent_attrs: HashMap<String, String>,
    /// Zero-based index of this node among its siblings.
    pub sibling_index: usize,
    /// Whether a next sibling exists after this node.
    pub has_next_sibling: bool,
    /// Tag name of this node's first child element, if any.
    pub first_child_tag: Option<String>,
    /// Text content of this node's first child, if it is a text node.
    pub first_child_text: Option<String>,
    /// Attributes of this node's first child element.
    pub first_child_attrs: HashMap<String, String>,
    /// The full text content of this node (all descendant text concatenated).
    pub text_content: String,
    /// Whether this node is inside a `<pre>` element.
    pub inside_pre: bool,
}

/// Computed information about a DOM node for use by conversion rules.
///
/// Contains the node's tag name, classification flags, attributes, flanking
/// whitespace, and contextual information about its position in the tree.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// The tag name of this element (lowercase).
    pub tag: String,
    /// Whether this element is block-level.
    pub is_block: bool,
    /// Whether this element is inside a code/pre context.
    pub is_code: bool,
    /// Whether this element is blank (only whitespace, no meaningful children).
    pub is_blank: bool,
    /// Flanking whitespace extracted from this node's content.
    pub flanking_whitespace: FlankingWhitespace,
    /// This element's attributes.
    pub attrs: HashMap<String, String>,
    /// Contextual information about parent, siblings, and children.
    pub context: NodeContext,
}

impl NodeInfo {
    /// Create a new `NodeInfo` with all fields specified.
    pub fn new(
        tag: String,
        is_block: bool,
        is_code: bool,
        is_blank: bool,
        flanking_whitespace: FlankingWhitespace,
        attrs: HashMap<String, String>,
        context: NodeContext,
    ) -> Self {
        Self {
            tag,
            is_block,
            is_code,
            is_blank,
            flanking_whitespace,
            attrs,
            context,
        }
    }

    /// Returns the tag name of this element.
    pub fn tag_name(&self) -> &str {
        &self.tag
    }

    /// Returns the value of the given attribute, if present.
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(|s| s.as_str())
    }

    /// Returns the parent element's tag name, if any.
    pub fn parent_tag(&self) -> Option<&str> {
        self.context.parent_tag.as_deref()
    }

    /// Returns the value of the given attribute on the parent element, if present.
    pub fn parent_attr(&self, name: &str) -> Option<&str> {
        self.context.parent_attrs.get(name).map(|s| s.as_str())
    }

    /// Returns the zero-based sibling index of this node.
    pub fn sibling_index(&self) -> usize {
        self.context.sibling_index
    }

    /// Returns whether this node has a next sibling.
    pub fn has_next_sibling(&self) -> bool {
        self.context.has_next_sibling
    }

    /// Returns whether this node is the last child of its parent.
    pub fn is_last_child(&self) -> bool {
        !self.context.has_next_sibling
    }

    /// Returns the tag name of this node's first child element, if any.
    pub fn first_child_tag(&self) -> Option<&str> {
        self.context.first_child_tag.as_deref()
    }

    /// Returns the text content of this node's first child, if it is a text node.
    pub fn first_child_text_content(&self) -> Option<&str> {
        self.context.first_child_text.as_deref()
    }

    /// Returns the value of the given attribute on the first child element, if present.
    pub fn first_child_attr(&self, name: &str) -> Option<&str> {
        self.context.first_child_attrs.get(name).map(|s| s.as_str())
    }

    /// Returns the full text content of this node (all descendant text concatenated).
    pub fn text_content(&self) -> &str {
        &self.context.text_content
    }

    /// Returns whether this node is inside a `<pre>` element.
    pub fn is_inside_pre(&self) -> bool {
        self.context.inside_pre
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_elements_classification() {
        assert!(is_block_element("div"));
        assert!(is_block_element("p"));
        assert!(is_block_element("h1"));
        assert!(is_block_element("blockquote"));
        assert!(is_block_element("table"));
        assert!(is_block_element("ul"));
        assert!(is_block_element("ol"));
        assert!(is_block_element("li"));
        assert!(is_block_element("pre"));
        assert!(is_block_element("hr"));

        // Inline elements should not be block
        assert!(!is_block_element("a"));
        assert!(!is_block_element("span"));
        assert!(!is_block_element("em"));
        assert!(!is_block_element("strong"));
        assert!(!is_block_element("code"));
        assert!(!is_block_element("img"));
    }

    #[test]
    fn test_void_elements() {
        assert!(is_void_element("br"));
        assert!(is_void_element("hr"));
        assert!(is_void_element("img"));
        assert!(is_void_element("input"));
        assert!(is_void_element("meta"));

        assert!(!is_void_element("div"));
        assert!(!is_void_element("span"));
        assert!(!is_void_element("p"));
    }

    #[test]
    fn test_meaningful_when_blank() {
        assert!(is_meaningful_when_blank("a"));
        assert!(is_meaningful_when_blank("table"));
        assert!(is_meaningful_when_blank("td"));
        assert!(is_meaningful_when_blank("iframe"));

        assert!(!is_meaningful_when_blank("div"));
        assert!(!is_meaningful_when_blank("span"));
        assert!(!is_meaningful_when_blank("p"));
    }

    #[test]
    fn test_node_info_construction_and_accessors() {
        let mut attrs = HashMap::new();
        attrs.insert("class".to_string(), "heading".to_string());
        attrs.insert("id".to_string(), "title".to_string());

        let mut parent_attrs = HashMap::new();
        parent_attrs.insert("class".to_string(), "container".to_string());

        let mut first_child_attrs = HashMap::new();
        first_child_attrs.insert("href".to_string(), "https://example.com".to_string());

        let context = NodeContext {
            parent_tag: Some("div".to_string()),
            parent_attrs,
            sibling_index: 2,
            has_next_sibling: true,
            first_child_tag: Some("a".to_string()),
            first_child_text: Some("link text".to_string()),
            first_child_attrs,
            text_content: "Hello world".to_string(),
            inside_pre: false,
        };

        let info = NodeInfo::new(
            "h1".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            attrs,
            context,
        );

        assert_eq!(info.tag_name(), "h1");
        assert!(info.is_block);
        assert!(!info.is_code);
        assert!(!info.is_blank);
        assert_eq!(info.attr("class"), Some("heading"));
        assert_eq!(info.attr("id"), Some("title"));
        assert_eq!(info.attr("nonexistent"), None);
        assert_eq!(info.parent_tag(), Some("div"));
        assert_eq!(info.parent_attr("class"), Some("container"));
        assert_eq!(info.parent_attr("nonexistent"), None);
        assert_eq!(info.sibling_index(), 2);
        assert!(info.has_next_sibling());
        assert!(!info.is_last_child());
        assert_eq!(info.first_child_tag(), Some("a"));
        assert_eq!(info.first_child_text_content(), Some("link text"));
        assert_eq!(
            info.first_child_attr("href"),
            Some("https://example.com")
        );
        assert_eq!(info.text_content(), "Hello world");
        assert!(!info.is_inside_pre());
    }

    #[test]
    fn test_node_info_last_child() {
        let context = NodeContext {
            has_next_sibling: false,
            ..Default::default()
        };

        let info = NodeInfo::new(
            "p".to_string(),
            true,
            false,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );

        assert!(!info.has_next_sibling());
        assert!(info.is_last_child());
    }

    #[test]
    fn test_node_info_inside_pre() {
        let context = NodeContext {
            inside_pre: true,
            ..Default::default()
        };

        let info = NodeInfo::new(
            "code".to_string(),
            false,
            true,
            false,
            FlankingWhitespace::default(),
            HashMap::new(),
            context,
        );

        assert!(info.is_inside_pre());
        assert!(info.is_code);
    }

    #[test]
    fn test_flanking_whitespace() {
        let flanking = FlankingWhitespace {
            leading: " ".to_string(),
            trailing: "  ".to_string(),
        };

        let info = NodeInfo::new(
            "em".to_string(),
            false,
            false,
            false,
            flanking.clone(),
            HashMap::new(),
            NodeContext::default(),
        );

        assert_eq!(info.flanking_whitespace.leading, " ");
        assert_eq!(info.flanking_whitespace.trailing, "  ");
    }
}
