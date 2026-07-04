//! Conversion options for the HTML-to-markdown engine.
//!
//! Provides configurable options including heading style, bullet markers,
//! code block style, emphasis delimiters, and more.

/// Style for rendering headings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingStyle {
    /// ATX-style headings using `#` characters (e.g., `## Heading`).
    Atx,
    /// Setext-style headings using underlines (`===` for h1, `---` for h2).
    /// Falls back to ATX for h3-h6.
    Setext,
}

/// Style for rendering code blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeBlockStyle {
    /// Fenced code blocks using backticks or tildes.
    Fenced,
    /// Indented code blocks using 4 spaces.
    Indented,
}

/// Style for rendering links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStyle {
    /// Inline links: `[text](url "title")`.
    Inline,
    /// Reference-style links: `[text][ref]` with definitions at the end.
    Reference,
}

/// Configuration options for the HTML-to-markdown conversion engine.
#[derive(Debug, Clone)]
pub struct Options {
    /// Style for rendering headings (ATX or Setext).
    pub heading_style: HeadingStyle,
    /// Character used as the bullet marker for unordered lists.
    pub bullet_list_marker: char,
    /// Style for rendering code blocks (fenced or indented).
    pub code_block_style: CodeBlockStyle,
    /// Character used for fenced code block delimiters (backtick or tilde).
    pub fence_char: char,
    /// Delimiter for emphasis (e.g., `_` or `*`).
    pub em_delimiter: &'static str,
    /// Delimiter for strong emphasis (e.g., `**` or `__`).
    pub strong_delimiter: &'static str,
    /// Style for rendering links (inline or reference).
    pub link_style: LinkStyle,
    /// String used for horizontal rules.
    pub hr: &'static str,
    /// String used for line breaks (typically two trailing spaces).
    pub br: &'static str,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            heading_style: HeadingStyle::Atx,
            bullet_list_marker: '-',
            code_block_style: CodeBlockStyle::Fenced,
            fence_char: '`',
            em_delimiter: "_",
            strong_delimiter: "**",
            link_style: LinkStyle::Inline,
            hr: "---",
            br: "  ",
        }
    }
}
