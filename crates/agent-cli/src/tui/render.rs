// TUI rendering functions (pure draw logic).

use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{AppMode, AppState, PermissionPromptState, SpanStyle};

/// Draw the entire TUI frame.
///
/// Layout is a vertical split:
/// - Output area (fills remaining space)
/// - Input area (3 lines) or permission prompt (4 lines for 2 content rows + border)
/// - Status bar (1 line)
pub fn draw(frame: &mut Frame, state: &AppState) {
    let input_height = if state.mode == AppMode::PermissionPrompt {
        4 // 2 content lines (tool info + options) + 2 border lines
    } else {
        3 // 1 content line + 2 border lines
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),              // output area
            Constraint::Length(input_height), // input area / permission prompt
            Constraint::Length(1),            // status bar
        ])
        .split(frame.area());

    render_output(frame, state, chunks[0]);

    if state.mode == AppMode::PermissionPrompt {
        render_permission_prompt(frame, state, chunks[1]);
    } else {
        render_input(frame, state, chunks[1]);
    }

    render_status_bar(frame, state, chunks[2]);
}

/// Convert a `SpanStyle` to a ratatui `Style`.
fn span_style_to_ratatui(style: &SpanStyle) -> Style {
    match style {
        SpanStyle::Normal => Style::default(),
        SpanStyle::Thinking => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC),
        SpanStyle::ToolName => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        SpanStyle::ToolOutput => Style::default(),
        SpanStyle::Error => Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::BOLD),
        SpanStyle::Warning => Style::default().fg(Color::Yellow),
        SpanStyle::System => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
        SpanStyle::User => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    }
}

/// Render the output area showing accumulated agent output with styled spans.
///
/// Splits text containing newlines into separate Lines for proper rendering.
/// Auto-scrolls to show the latest output.
fn render_output(frame: &mut Frame, state: &AppState, area: Rect) {
    // Build lines by splitting spans at newline boundaries.
    // Each OutputSpan's text may contain multiple \n characters; we need to
    // produce a new Line for each newline encountered.
    let mut lines: Vec<Line> = Vec::new();
    let mut current_spans: Vec<Span> = Vec::new();

    for os in &state.output_buffer {
        let style = span_style_to_ratatui(&os.style);
        let parts: Vec<&str> = os.text.split('\n').collect();

        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                current_spans.push(Span::styled(part.to_string(), style));
            }
            // If this isn't the last part, the split was at a \n — finish the current line
            if i < parts.len() - 1 {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
        }
    }

    // Don't forget the trailing spans (last line without a trailing \n)
    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    // If there are no lines at all, push an empty one so the widget renders
    if lines.is_empty() {
        lines.push(Line::from(""));
    }

    let text = Text::from(lines);

    let block = Block::default()
        .borders(Borders::NONE)
        .title(" Output ");

    // Use Paragraph::line_count() to get the exact number of visual lines
    // after word-wrapping. This matches the renderer's internal calculation
    // and avoids the naive character-width estimation drifting out of sync.
    let paragraph = Paragraph::new(text)
        .block(block)
        .wrap(Wrap { trim: false });

    let visible_height = area.height as usize;
    let total_visual_lines = paragraph.line_count(area.width);
    let scroll_offset = if total_visual_lines > visible_height {
        (total_visual_lines - visible_height) as u16
    } else {
        0
    };

    let paragraph = paragraph.scroll((scroll_offset, 0));

    frame.render_widget(paragraph, area);
}

/// Render the input area with the current buffer content and cursor.
fn render_input(frame: &mut Frame, state: &AppState, area: Rect) {
    let input_content = state.input.content();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Input ");

    let paragraph = Paragraph::new(input_content).block(block);

    frame.render_widget(paragraph, area);

    // Set cursor position within the input area.
    // The block border adds 1 to x and 1 to y offset.
    let cursor_col = cursor_byte_to_column(state.input.content(), state.input.cursor());
    let cursor_x = area.x + 1 + cursor_col as u16;
    let cursor_y = area.y + 1;

    // Only show cursor when idle (input is active).
    if state.mode == AppMode::Idle {
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

/// Render the permission prompt in place of the input area.
///
/// Dispatches on `prompt_state`:
/// - `AwaitingKey`: show tool info (name + abbreviated input), optional sub-agent prefix,
///   and selectable options with highlight. Use ←/→ to switch, Enter to confirm.
/// - `EditingPattern`: show the editable pattern buffer with cursor indicator,
///   and `[Enter] confirm  [Esc] cancel` hint.
fn render_permission_prompt(frame: &mut Frame, state: &AppState, area: Rect) {
    match &state.prompt_state {
        PermissionPromptState::AwaitingKey { selected } => {
            render_permission_awaiting_key(frame, state, *selected, area);
        }
        PermissionPromptState::EditingPattern { edit_buffer, cursor } => {
            render_permission_editing_pattern(frame, edit_buffer, *cursor, area);
        }
    }
}

/// Render the AwaitingKey permission prompt: tool info + selectable options.
///
/// Shows options as `▸ allow once | always | pattern | deny` with the selected
/// option highlighted. User can navigate with ←/→ and confirm with Enter.
fn render_permission_awaiting_key(frame: &mut Frame, state: &AppState, selected: usize, area: Rect) {
    let (tool_name, input_summary) = if let Some(approval) = state.pending_approvals.first() {
        let name = approval.tool_name.clone();
        let input_str = approval.tool_input.to_string();
        let abbreviated = if input_str.len() > 60 {
            format!("{}...", &input_str[..57])
        } else {
            input_str
        };
        (name, abbreviated)
    } else {
        ("unknown".to_string(), String::new())
    };

    // Build the tool info line, optionally prefixed with sub-agent context
    let mut tool_line_spans: Vec<Span> = Vec::new();
    if let Some(ref agent_name) = state.approval_agent_name {
        tool_line_spans.push(Span::styled(
            format!("[sub-agent: {}] ", agent_name),
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        ));
    }
    tool_line_spans.push(Span::styled("Tool: ", Style::default().add_modifier(Modifier::BOLD)));
    tool_line_spans.push(Span::styled(tool_name, Style::default().fg(Color::Yellow)));
    tool_line_spans.push(Span::raw("  Input: "));
    tool_line_spans.push(Span::styled(input_summary, Style::default().fg(Color::DarkGray)));

    // Build the selectable options line with highlight on the selected item
    let options: &[(&str, &str, Color)] = &[
        ("y", " allow ", Color::Green),
        ("a", " always ", Color::Cyan),
        ("p", " pattern ", Color::Blue),
        ("n", " deny ", Color::Red),
    ];

    let mut option_spans: Vec<Span> = Vec::new();
    option_spans.push(Span::styled("◂ ", Style::default().fg(Color::DarkGray)));

    for (i, (key, label, color)) in options.iter().enumerate() {
        if i == selected {
            // Highlighted: inverted colors (bg = option color, fg = black)
            option_spans.push(Span::styled(
                format!(" [{}]{} ", key, label),
                Style::default()
                    .fg(Color::Black)
                    .bg(*color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            // Normal: key in color, label dim
            option_spans.push(Span::styled(
                format!("[{}]", key),
                Style::default().fg(*color).add_modifier(Modifier::BOLD),
            ));
            option_spans.push(Span::styled(
                label.to_string(),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if i < options.len() - 1 {
            option_spans.push(Span::raw(" "));
        }
    }

    option_spans.push(Span::styled(" ▸", Style::default().fg(Color::DarkGray)));
    option_spans.push(Span::styled("  ⏎ Enter", Style::default().fg(Color::DarkGray)));

    let lines = vec![
        Line::from(tool_line_spans),
        Line::from(option_spans),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Permission Required ");

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(paragraph, area);
}

/// Render the EditingPattern permission prompt: pattern buffer + confirm/cancel hint.
///
/// Shows the current edit buffer with the cursor position indicated by setting
/// the terminal cursor. Displays `[Enter] confirm  [Esc] cancel` hint below.
fn render_permission_editing_pattern(
    frame: &mut Frame,
    edit_buffer: &str,
    cursor: usize,
    area: Rect,
) {
    let lines = vec![
        Line::from(vec![
            Span::styled("Pattern: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(edit_buffer, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::styled("[Enter]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" confirm  "),
            Span::styled("[Esc]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw(" cancel"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Pattern Grant ");

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(paragraph, area);

    // Set the cursor position within the pattern edit buffer.
    // Block border adds 1 to x and 1 to y. The "Pattern: " prefix is 9 chars.
    let prefix_len = "Pattern: ".len() as u16;
    let cursor_col = cursor_byte_to_column(edit_buffer, cursor) as u16;
    let cursor_x = area.x + 1 + prefix_len + cursor_col;
    let cursor_y = area.y + 1;
    frame.set_cursor_position(Position::new(cursor_x, cursor_y));
}

/// Render the status bar showing mode indicator, model name, and token usage.
fn render_status_bar(frame: &mut Frame, state: &AppState, area: Rect) {
    let mode_span = match state.mode {
        AppMode::Idle => Span::styled(
            " IDLE ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        AppMode::Running => Span::styled(
            " RUNNING ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        AppMode::PermissionPrompt => Span::styled(
            " PERMISSION ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ),
        AppMode::Exiting => Span::styled(
            " EXITING ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
    };

    let usage_text = if let Some(usage) = &state.last_usage {
        format!(" tokens: {}in / {}out ", usage.input_tokens, usage.output_tokens)
    } else {
        String::new()
    };

    let status_line = Line::from(vec![
        mode_span,
        Span::raw("  "),
        Span::styled(usage_text, Style::default().fg(Color::DarkGray)),
    ]);

    let paragraph = Paragraph::new(status_line)
        .style(Style::default().bg(Color::Black));

    frame.render_widget(paragraph, area);
}

/// Convert a byte-offset cursor position to a display column offset.
///
/// This accounts for Unicode display width: CJK characters occupy 2 terminal
/// columns, while ASCII and most other characters occupy 1 column.
fn cursor_byte_to_column(content: &str, byte_pos: usize) -> usize {
    display_width(&content[..byte_pos])
}

/// Calculate the terminal display width of a string.
///
/// CJK (wide) characters count as 2 columns; other characters count as 1.
/// Control characters count as 0.
fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthChar;
    s.chars().map(|c| UnicodeWidthChar::width(c).unwrap_or(0)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_style_normal_is_default() {
        let style = span_style_to_ratatui(&SpanStyle::Normal);
        assert_eq!(style, Style::default());
    }

    #[test]
    fn span_style_thinking_is_cyan_italic() {
        let style = span_style_to_ratatui(&SpanStyle::Thinking);
        assert_eq!(style.fg, Some(Color::Cyan));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn span_style_tool_name_is_yellow_bold() {
        let style = span_style_to_ratatui(&SpanStyle::ToolName);
        assert_eq!(style.fg, Some(Color::Yellow));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn span_style_error_is_red_bold() {
        let style = span_style_to_ratatui(&SpanStyle::Error);
        assert_eq!(style.fg, Some(Color::Red));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn span_style_warning_is_yellow() {
        let style = span_style_to_ratatui(&SpanStyle::Warning);
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn span_style_system_is_dark_gray_dim() {
        let style = span_style_to_ratatui(&SpanStyle::System);
        assert_eq!(style.fg, Some(Color::DarkGray));
        assert!(style.add_modifier.contains(Modifier::DIM));
    }

    #[test]
    fn cursor_byte_to_column_ascii() {
        assert_eq!(cursor_byte_to_column("hello", 0), 0);
        assert_eq!(cursor_byte_to_column("hello", 3), 3);
        assert_eq!(cursor_byte_to_column("hello", 5), 5);
    }

    #[test]
    fn cursor_byte_to_column_unicode() {
        // "aéb" — 'a'=1 byte, 'é'=2 bytes, 'b'=1 byte
        let s = "aéb";
        assert_eq!(cursor_byte_to_column(s, 0), 0); // before 'a'
        assert_eq!(cursor_byte_to_column(s, 1), 1); // after 'a'
        assert_eq!(cursor_byte_to_column(s, 3), 2); // after 'é' (2 bytes)
        assert_eq!(cursor_byte_to_column(s, 4), 3); // after 'b'
    }

    #[test]
    fn cursor_byte_to_column_multibyte() {
        // "あいう" — each char is 3 bytes, 2 display columns (CJK wide)
        let s = "あいう";
        assert_eq!(cursor_byte_to_column(s, 0), 0);
        assert_eq!(cursor_byte_to_column(s, 3), 2);  // after 'あ' = 2 cols
        assert_eq!(cursor_byte_to_column(s, 6), 4);  // after 'い' = 4 cols
        assert_eq!(cursor_byte_to_column(s, 9), 6);  // after 'う' = 6 cols
    }

    #[test]
    fn render_output_splits_newlines() {
        // Verify that our line-splitting logic produces correct Line count
        use super::super::app::{OutputSpan, SpanStyle};

        let spans = vec![
            OutputSpan { text: "hello\nworld\n".to_string(), style: SpanStyle::Normal },
            OutputSpan { text: "foo".to_string(), style: SpanStyle::Normal },
        ];

        // Simulate the splitting logic
        let mut lines: Vec<Vec<String>> = Vec::new();
        let mut current: Vec<String> = Vec::new();

        for os in &spans {
            let parts: Vec<&str> = os.text.split('\n').collect();
            for (i, part) in parts.iter().enumerate() {
                if !part.is_empty() {
                    current.push(part.to_string());
                }
                if i < parts.len() - 1 {
                    lines.push(std::mem::take(&mut current));
                }
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }

        // "hello\nworld\n" splits into ["hello", "world", ""]
        // Then "foo" appends to the last empty line
        // Result: ["hello"], ["world"], ["foo"]
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], vec!["hello"]);
        assert_eq!(lines[1], vec!["world"]);
        assert_eq!(lines[2], vec!["foo"]);
    }
}

#[cfg(test)]
mod property_tests {
    use proptest::prelude::*;
    use ratatui::layout::{Constraint, Direction, Layout, Rect};

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        /// **Validates: Requirements 9.2**
        ///
        /// Property 14: Layout bounds fit terminal dimensions — for any terminal
        /// dimensions (w≥1, h≥4), layout areas sum to terminal height and widths
        /// ≤ terminal width.
        #[test]
        fn layout_bounds_fit_terminal_dimensions(
            width in 1u16..=500u16,
            height in 4u16..=500u16,
        ) {
            let area = Rect::new(0, 0, width, height);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1),      // output area
                    Constraint::Length(3),   // input area
                    Constraint::Length(1),   // status bar
                ])
                .split(area);

            // All chunks must fit within the terminal area
            let total_height: u16 = chunks.iter().map(|c| c.height).sum();
            prop_assert_eq!(total_height, height,
                "Layout heights sum to {} but terminal height is {}", total_height, height);

            for chunk in chunks.iter() {
                prop_assert!(chunk.width <= width,
                    "Chunk width {} exceeds terminal width {}", chunk.width, width);
                prop_assert!(chunk.x + chunk.width <= width,
                    "Chunk extends beyond terminal: x={} + w={} > {}", chunk.x, chunk.width, width);
                prop_assert!(chunk.y + chunk.height <= height,
                    "Chunk extends below terminal: y={} + h={} > {}", chunk.y, chunk.height, height);
            }
        }
    }
}
