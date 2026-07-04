/// A line-editing input buffer with cursor tracking.
///
/// Tracks cursor position at char boundaries for correct Unicode handling.
/// Used by the TUI adapter's input area.
#[derive(Debug, Clone)]
pub struct InputBuffer {
    /// The accumulated text content.
    content: String,
    /// Cursor position in bytes (always aligned to a char boundary).
    cursor: usize,
}

impl InputBuffer {
    /// Create a new empty input buffer with cursor at position 0.
    pub fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
        }
    }

    /// Insert a character at the current cursor position and advance the cursor.
    pub fn insert(&mut self, ch: char) {
        self.content.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    /// Delete the character before the cursor and move the cursor back.
    ///
    /// No-op if the cursor is at position 0.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // Find the previous char boundary
        let prev = self.prev_char_boundary();
        self.content.drain(prev..self.cursor);
        self.cursor = prev;
    }

    /// Delete the character at the cursor position without moving the cursor.
    ///
    /// No-op if the cursor is at the end of the content.
    pub fn delete(&mut self) {
        if self.cursor >= self.content.len() {
            return;
        }
        // Find the next char boundary after cursor
        let next = self.next_char_boundary();
        self.content.drain(self.cursor..next);
    }

    /// Move the cursor one character boundary to the left.
    ///
    /// No-op if the cursor is at position 0.
    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        self.cursor = self.prev_char_boundary();
    }

    /// Move the cursor one character boundary to the right.
    ///
    /// No-op if the cursor is at the end of the content.
    pub fn move_right(&mut self) {
        if self.cursor >= self.content.len() {
            return;
        }
        self.cursor = self.next_char_boundary();
    }

    /// Move the cursor to position 0 (start of content).
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Move the cursor to the end of the content.
    pub fn move_end(&mut self) {
        self.cursor = self.content.len();
    }

    /// Clear all content and reset cursor to 0.
    pub fn clear(&mut self) {
        self.content.clear();
        self.cursor = 0;
    }

    /// Return the content and reset the buffer to an empty state.
    ///
    /// Similar to `Option::take` — extracts the value and leaves empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.content)
    }

    /// Returns true if the content is empty.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Get a reference to the content string for display.
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Get the cursor byte position.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Find the byte position of the previous char boundary before `self.cursor`.
    fn prev_char_boundary(&self) -> usize {
        let mut pos = self.cursor - 1;
        while !self.content.is_char_boundary(pos) {
            pos -= 1;
        }
        pos
    }

    /// Find the byte position of the next char boundary after `self.cursor`.
    fn next_char_boundary(&self) -> usize {
        let mut pos = self.cursor + 1;
        while pos < self.content.len() && !self.content.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    }
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_buffer_is_empty() {
        let buf = InputBuffer::new();
        assert!(buf.is_empty());
        assert_eq!(buf.cursor(), 0);
        assert_eq!(buf.content(), "");
    }

    #[test]
    fn test_insert_ascii() {
        let mut buf = InputBuffer::new();
        buf.insert('h');
        buf.insert('e');
        buf.insert('l');
        buf.insert('l');
        buf.insert('o');
        assert_eq!(buf.content(), "hello");
        assert_eq!(buf.cursor(), 5);
    }

    #[test]
    fn test_insert_unicode() {
        let mut buf = InputBuffer::new();
        buf.insert('こ');
        buf.insert('ん');
        buf.insert('に');
        buf.insert('ち');
        buf.insert('は');
        assert_eq!(buf.content(), "こんにちは");
        // Each Japanese char is 3 bytes in UTF-8
        assert_eq!(buf.cursor(), 15);
    }

    #[test]
    fn test_insert_at_middle() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('c');
        buf.move_left();
        buf.insert('b');
        assert_eq!(buf.content(), "abc");
        assert_eq!(buf.cursor(), 2); // After 'b'
    }

    #[test]
    fn test_backspace_at_start_is_noop() {
        let mut buf = InputBuffer::new();
        buf.backspace();
        assert!(buf.is_empty());
        assert_eq!(buf.cursor(), 0);
    }

    #[test]
    fn test_backspace_removes_previous_char() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('b');
        buf.insert('c');
        buf.backspace();
        assert_eq!(buf.content(), "ab");
        assert_eq!(buf.cursor(), 2);
    }

    #[test]
    fn test_backspace_unicode() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('é'); // 2-byte char
        buf.insert('b');
        buf.backspace();
        buf.backspace();
        assert_eq!(buf.content(), "a");
        assert_eq!(buf.cursor(), 1);
    }

    #[test]
    fn test_delete_at_end_is_noop() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.delete();
        assert_eq!(buf.content(), "a");
        assert_eq!(buf.cursor(), 1);
    }

    #[test]
    fn test_delete_removes_char_at_cursor() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('b');
        buf.insert('c');
        buf.move_home();
        buf.delete();
        assert_eq!(buf.content(), "bc");
        assert_eq!(buf.cursor(), 0);
    }

    #[test]
    fn test_move_left_at_start_is_noop() {
        let mut buf = InputBuffer::new();
        buf.move_left();
        assert_eq!(buf.cursor(), 0);
    }

    #[test]
    fn test_move_right_at_end_is_noop() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.move_right();
        assert_eq!(buf.cursor(), 1);
    }

    #[test]
    fn test_move_left_and_right() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('b');
        buf.insert('c');
        assert_eq!(buf.cursor(), 3);
        buf.move_left();
        assert_eq!(buf.cursor(), 2);
        buf.move_left();
        assert_eq!(buf.cursor(), 1);
        buf.move_right();
        assert_eq!(buf.cursor(), 2);
    }

    #[test]
    fn test_move_home_and_end() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('b');
        buf.insert('c');
        buf.move_home();
        assert_eq!(buf.cursor(), 0);
        buf.move_end();
        assert_eq!(buf.cursor(), 3);
    }

    #[test]
    fn test_clear() {
        let mut buf = InputBuffer::new();
        buf.insert('a');
        buf.insert('b');
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.cursor(), 0);
        assert_eq!(buf.content(), "");
    }

    #[test]
    fn test_take() {
        let mut buf = InputBuffer::new();
        buf.insert('h');
        buf.insert('i');
        let taken = buf.take();
        assert_eq!(taken, "hi");
        assert!(buf.is_empty());
        assert_eq!(buf.cursor(), 0);
        assert_eq!(buf.content(), "");
    }

    #[test]
    fn test_take_empty() {
        let mut buf = InputBuffer::new();
        let taken = buf.take();
        assert_eq!(taken, "");
        assert!(buf.is_empty());
    }

    #[test]
    fn test_unicode_cursor_navigation() {
        let mut buf = InputBuffer::new();
        // Mix of 1-byte, 2-byte, 3-byte, and 4-byte chars
        buf.insert('a');   // 1 byte
        buf.insert('é');   // 2 bytes
        buf.insert('中');  // 3 bytes
        buf.insert('🦀'); // 4 bytes
        assert_eq!(buf.cursor(), 10); // 1 + 2 + 3 + 4

        buf.move_left(); // back over 🦀
        assert_eq!(buf.cursor(), 6);

        buf.move_left(); // back over 中
        assert_eq!(buf.cursor(), 3);

        buf.move_left(); // back over é
        assert_eq!(buf.cursor(), 1);

        buf.move_left(); // back over a
        assert_eq!(buf.cursor(), 0);

        buf.move_right(); // forward over a
        assert_eq!(buf.cursor(), 1);

        buf.move_right(); // forward over é
        assert_eq!(buf.cursor(), 3);
    }

    #[test]
    fn test_insert_between_multibyte_chars() {
        let mut buf = InputBuffer::new();
        buf.insert('あ'); // 3 bytes
        buf.insert('う'); // 3 bytes
        buf.move_left();  // cursor before 'う'
        buf.insert('い'); // insert between
        assert_eq!(buf.content(), "あいう");
        assert_eq!(buf.cursor(), 6); // after 'い'
    }

    #[test]
    fn test_backspace_multibyte_in_middle() {
        let mut buf = InputBuffer::new();
        buf.insert('あ');
        buf.insert('い');
        buf.insert('う');
        buf.move_left(); // before 'う'
        buf.backspace();  // remove 'い'
        assert_eq!(buf.content(), "あう");
        assert_eq!(buf.cursor(), 3); // after 'あ'
    }

    #[test]
    fn test_delete_multibyte() {
        let mut buf = InputBuffer::new();
        buf.insert('あ');
        buf.insert('い');
        buf.insert('う');
        buf.move_home();
        buf.delete(); // remove 'あ'
        assert_eq!(buf.content(), "いう");
        assert_eq!(buf.cursor(), 0);
    }

    /// **Validates: Requirements 8.2**
    #[cfg(test)]
    mod property_tests {
        use super::*;
        use proptest::prelude::*;

        /// Property 13: Non-empty input submission — verify `take()` returns
        /// the accumulated content and resets state.
        ///
        /// Insert each char one by one into a fresh InputBuffer, call take(),
        /// and verify it equals the original string and the buffer is empty after.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]
            #[test]
            fn take_returns_accumulated_content(s in "\\PC{1,100}") {
                let mut buf = InputBuffer::new();
                for ch in s.chars() {
                    buf.insert(ch);
                }
                let taken = buf.take();
                prop_assert_eq!(&taken, &s);
                prop_assert!(buf.is_empty());
                prop_assert_eq!(buf.cursor(), 0);
                prop_assert_eq!(buf.content(), "");
            }
        }

        /// Operation enum for generating random sequences of InputBuffer operations.
        #[derive(Debug, Clone)]
        enum BufOp {
            Insert(char),
            Backspace,
            Delete,
            MoveLeft,
            MoveRight,
            MoveHome,
            MoveEnd,
        }

        /// Strategy to generate a random InputBuffer operation.
        fn buf_op_strategy() -> impl Strategy<Value = BufOp> {
            prop_oneof![
                4 => any::<char>().prop_map(BufOp::Insert),
                1 => Just(BufOp::Backspace),
                1 => Just(BufOp::Delete),
                1 => Just(BufOp::MoveLeft),
                1 => Just(BufOp::MoveRight),
                1 => Just(BufOp::MoveHome),
                1 => Just(BufOp::MoveEnd),
            ]
        }

        /// Cursor never exceeds content length — for any sequence of operations,
        /// after each operation: cursor() <= content().len() and
        /// content().is_char_boundary(cursor()) is true.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]
            #[test]
            fn cursor_never_exceeds_content_length(
                ops in proptest::collection::vec(buf_op_strategy(), 1..50)
            ) {
                let mut buf = InputBuffer::new();
                for op in ops {
                    match op {
                        BufOp::Insert(ch) => buf.insert(ch),
                        BufOp::Backspace => buf.backspace(),
                        BufOp::Delete => buf.delete(),
                        BufOp::MoveLeft => buf.move_left(),
                        BufOp::MoveRight => buf.move_right(),
                        BufOp::MoveHome => buf.move_home(),
                        BufOp::MoveEnd => buf.move_end(),
                    }
                    prop_assert!(
                        buf.cursor() <= buf.content().len(),
                        "cursor {} exceeds content length {}",
                        buf.cursor(), buf.content().len()
                    );
                    prop_assert!(
                        buf.content().is_char_boundary(buf.cursor()),
                        "cursor {} is not a char boundary in {:?}",
                        buf.cursor(), buf.content()
                    );
                }
            }
        }

        /// Insert at any position produces correct string — generate a base string,
        /// insert all chars, move cursor to a random valid position, insert a new char,
        /// and verify resulting content has correct length and contains the inserted char.
        proptest! {
            #![proptest_config(ProptestConfig::with_cases(100))]
            #[test]
            fn insert_at_any_position_produces_correct_string(
                base in "[a-z]{0,30}",
                new_char in any::<char>(),
                pos_fraction in 0.0f64..=1.0f64,
            ) {
                let mut buf = InputBuffer::new();
                for ch in base.chars() {
                    buf.insert(ch);
                }

                // Move cursor to a random valid position
                let char_count = base.chars().count();
                let target_pos = if char_count == 0 {
                    0
                } else {
                    ((pos_fraction * char_count as f64) as usize).min(char_count)
                };

                // Move to start, then advance to target position
                buf.move_home();
                for _ in 0..target_pos {
                    buf.move_right();
                }

                // Insert the new character
                buf.insert(new_char);

                // Verify resulting content
                let result = buf.content().to_string();
                let expected_char_count = char_count + 1;
                prop_assert_eq!(
                    result.chars().count(),
                    expected_char_count,
                    "Expected {} chars, got {} in {:?}",
                    expected_char_count, result.chars().count(), result
                );

                // The inserted char must exist in the result
                prop_assert!(
                    result.contains(new_char),
                    "Result {:?} does not contain inserted char {:?}",
                    result, new_char
                );

                // Verify the char is specifically at the target position
                let result_chars: Vec<char> = result.chars().collect();
                prop_assert_eq!(
                    result_chars[target_pos], new_char,
                    "Expected char {:?} at position {}, got {:?}",
                    new_char, target_pos, result_chars[target_pos]
                );
            }
        }
    }
}
