//! The editor: the TUI's input box. A plain `String` plus a char-wise
//! cursor — no editing framework, just enough to type, wrap-render, and
//! submit. Kept free of any terminal dependency so it is unit-testable.

/// Multi-line input being composed. `cursor` is a CHAR index into `text`
/// (byte-slicing a char boundary would panic; char indexing cannot).
#[derive(Debug, Default)]
pub struct Editor {
    text: String,
    cursor: usize,
}

impl Editor {
    pub fn insert_char(&mut self, c: char) {
        let byte = self.char_to_byte(self.cursor);
        self.text.insert(byte, c);
        self.cursor += 1;
    }

    pub fn newline(&mut self) {
        self.insert_char('\n');
    }

    /// Insert a bracketed-paste payload at the cursor. Line endings
    /// normalize to `\n`, and nothing here can submit — a paste is text,
    /// not Enter (that is the entire point of bracketed paste mode).
    pub fn insert_paste(&mut self, pasted: &str) {
        let pasted = pasted.replace("\r\n", "\n");
        let byte = self.char_to_byte(self.cursor);
        self.text.insert_str(byte, &pasted);
        self.cursor += pasted.chars().count();
    }

    /// Delete the char before the cursor. No-op at the very start.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.char_to_byte(self.cursor - 1);
        let end = self.char_to_byte(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// One char left, clamped at the start.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// One char right, clamped at the end.
    pub fn move_right(&mut self) {
        if self.cursor < self.char_count() {
            self.cursor += 1;
        }
    }

    /// Ctrl+A / Home: the start of the logical line the cursor is on.
    pub fn move_home(&mut self) {
        self.cursor = self.line_start();
    }

    /// Ctrl+E / End: the end of the logical line the cursor is on.
    pub fn move_end(&mut self) {
        self.cursor = self.line_end();
    }

    /// One logical line up, keeping the column where the line is long
    /// enough and clamping at its end where it is not.
    pub fn move_up(&mut self) {
        let start = self.line_start();
        if start == 0 {
            return;
        }
        let column = self.cursor - start;
        let previous_end = start - 1; // the newline before this line
        let previous_start = self.line_start_before(previous_end);
        self.cursor = (previous_start + column).min(previous_end);
    }

    /// One logical line down, with the same column rule as [`move_up`].
    pub fn move_down(&mut self) {
        let end = self.line_end();
        if end >= self.char_count() {
            return;
        }
        let column = self.cursor - self.line_start();
        let next_start = end + 1;
        let next_end = self.line_end_after(next_start);
        self.cursor = (next_start + column).min(next_end);
    }

    /// Ctrl+K: delete to the end of the line; at the end of a line the
    /// newline goes too, joining the next line on (readline's kill-line).
    pub fn kill_to_end(&mut self) {
        let end = self.line_end();
        let to = if self.cursor < end {
            end
        } else if end < self.char_count() {
            end + 1
        } else {
            return;
        };
        let start = self.char_to_byte(self.cursor);
        let end = self.char_to_byte(to);
        self.text.replace_range(start..end, "");
    }

    /// Ctrl+U: delete from the start of the line to the cursor. No-op
    /// with the cursor already at the line's start.
    pub fn kill_to_start(&mut self) {
        let start = self.line_start();
        if self.cursor <= start {
            return;
        }
        let from = self.char_to_byte(start);
        let to = self.char_to_byte(self.cursor);
        self.text.replace_range(from..to, "");
        self.cursor = start;
    }

    fn char_count(&self) -> usize {
        self.text.chars().count()
    }

    /// Char index of the first char on the cursor's line.
    fn line_start(&self) -> usize {
        self.line_start_before(self.cursor)
    }

    /// Char index of the first char on the line ending at `newline`.
    fn line_start_before(&self, newline: usize) -> usize {
        let mut start = 0;
        for (i, c) in self.text.chars().enumerate().take(newline) {
            if c == '\n' {
                start = i + 1;
            }
        }
        start
    }

    /// Char index of the newline (or text end) ending the cursor's line.
    fn line_end(&self) -> usize {
        self.line_end_after(self.cursor)
    }

    /// Char index of the newline (or text end) ending the line that
    /// contains `from`.
    fn line_end_after(&self, from: usize) -> usize {
        for (i, c) in self.text.chars().enumerate().skip(from) {
            if c == '\n' {
                return i;
            }
        }
        self.char_count()
    }

    /// Submit: hand over the text and reset. Empty submissions are
    /// refused by returning None (the caller never sends blank lines).
    pub fn take_submission(&mut self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }
        self.cursor = 0;
        Some(std::mem::take(&mut self.text))
    }

    /// The editor's lines for rendering. The terminal does the wrapping.
    pub fn lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }

    /// The whole text — suggestion filtering reads the composer.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replace everything (Tab completion of a suggestion); the cursor
    /// parks at the end, ready for arguments.
    pub fn set_text(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.chars().count();
    }

    /// Cursor position as (row, col) in CHARACTERS within [`lines`](Self::lines).
    /// The renderer uses it to place the real terminal cursor; under
    /// soft-wrapping the placement is approximate, which is fine for a
    /// compose box.
    pub fn cursor_row_col(&self) -> (usize, usize) {
        let before = self.text[..self.char_to_byte(self.cursor)].to_string();
        let mut row = 0usize;
        let mut col = 0usize;
        for c in before.chars() {
            if c == '\n' {
                row += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (row, col)
    }

    /// Byte offset of a char index. `char_indices` walks valid boundaries
    /// only, so this cannot split a multibyte char.
    fn char_to_byte(&self, char_index: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_index)
            .map(|(byte, _)| byte)
            .unwrap_or(self.text.len())
    }

    /// Test seam: load text directly, cursor at end.
    #[cfg(test)]
    fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            self.insert_char(c);
        }
    }

    /// Test seam: park the cursor at a char index.
    #[cfg(test)]
    fn set_cursor(&mut self, char_index: usize) {
        self.cursor = char_index.min(self.text.chars().count());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn types_edits_and_deletes_char_wise() {
        let mut ed = Editor::default();
        for c in "héllo".chars() {
            ed.insert_char(c);
        }
        assert_eq!(ed.lines(), vec!["héllo"]);
        ed.backspace();
        assert_eq!(ed.lines(), vec!["héll"], "é is one char");

        // Cursor mid-word: backspace removes the char before it.
        let mut ed = Editor::default();
        for c in "ab".chars() {
            ed.insert_char(c);
        }
        ed.insert_char('X'); // cursor now after X
        ed.backspace();
        assert_eq!(ed.lines(), vec!["ab"]);
    }

    #[test]
    fn newlines_split_lines_and_move_the_cursor() {
        let mut ed = Editor::default();
        ed.insert_str("one\ntwo");
        assert_eq!(ed.lines(), vec!["one", "two"]);
        assert_eq!(ed.cursor_row_col(), (1, 3));

        let mut ed = Editor::default();
        ed.insert_str("a\nb");
        ed.set_cursor(1); // between 'a' and the newline
        ed.insert_char('Z');
        assert_eq!(ed.lines(), vec!["aZ", "b"]);
    }

    #[test]
    fn submission_takes_the_text_and_resets() {
        let mut ed = Editor::default();
        ed.insert_str("  hello  ");
        assert_eq!(ed.take_submission().as_deref(), Some("  hello  "));
        assert!(ed.lines() == vec![""], "submission empties the editor");

        let mut ed = Editor::default();
        ed.insert_str("   ");
        assert!(ed.take_submission().is_none(), "blank never submits");
        ed.insert_str("x");
        assert!(ed.take_submission().is_some());
    }

    #[test]
    fn deleting_at_the_start_is_a_noop() {
        let mut ed = Editor::default();
        ed.backspace();
        ed.insert_str("word");
        ed.set_cursor(0);
        ed.backspace();
        assert_eq!(ed.lines(), vec!["word"]);
    }

    #[test]
    fn cursor_moves_by_char_and_by_line() {
        let mut ed = Editor::default();
        ed.insert_str("ab\ncdef\ngh");
        // Cursor at the end of "gh".
        ed.move_left();
        assert_eq!(ed.cursor_row_col(), (2, 1));
        ed.move_left();
        assert_eq!(ed.cursor_row_col(), (2, 0));
        for _ in 0..8 {
            ed.move_left(); // clamped at the start of the text
        }
        assert_eq!(ed.cursor_row_col(), (0, 0));
        ed.move_right();
        assert_eq!(ed.cursor_row_col(), (0, 1));

        ed.set_cursor(0);
        ed.move_down();
        assert_eq!(ed.cursor_row_col(), (1, 0));
        ed.move_end();
        assert_eq!(ed.cursor_row_col(), (1, 4));
        ed.move_down();
        assert_eq!(ed.cursor_row_col(), (2, 2), "column clamps to a short line");
        ed.move_down();
        assert_eq!(ed.cursor_row_col(), (2, 2), "no line below the last");
        ed.move_up();
        assert_eq!(
            ed.cursor_row_col(),
            (1, 2),
            "the column is kept where the line can take it"
        );
    }

    #[test]
    fn ctrl_a_and_ctrl_e_jump_within_the_line() {
        let mut ed = Editor::default();
        ed.insert_str("one\ntwo");
        ed.move_home();
        assert_eq!(ed.cursor_row_col(), (1, 0));
        ed.move_end();
        assert_eq!(ed.cursor_row_col(), (1, 3));
        ed.move_up();
        ed.move_end();
        assert_eq!(ed.cursor_row_col(), (0, 3));
        // ..and typing lands where the cursor is.
        ed.insert_char('!');
        assert_eq!(ed.lines(), vec!["one!", "two"]);
    }

    #[test]
    fn ctrl_k_kills_to_the_end_and_joins_at_the_end_of_a_line() {
        let mut ed = Editor::default();
        ed.insert_str("one\ntwo");
        ed.set_cursor(1); // after 'o' on the first line
        ed.kill_to_end();
        assert_eq!(ed.lines(), vec!["o", "two"]);
        assert_eq!(ed.cursor_row_col(), (0, 1));
        // At the end of the last line there is nothing to kill.
        ed.set_cursor(5);
        ed.kill_to_end();
        assert_eq!(ed.lines(), vec!["o", "two"]);
        // At the end of the first line, Ctrl+K takes the newline: the
        // lines join and the cursor stays put.
        ed.set_cursor(1);
        ed.kill_to_end();
        assert_eq!(ed.lines(), vec!["otwo"]);
        assert_eq!(ed.cursor_row_col(), (0, 1));
    }

    #[test]
    fn ctrl_u_kills_from_the_line_start_to_the_cursor() {
        let mut ed = Editor::default();
        ed.insert_str("one\ntwo");
        ed.set_cursor(5); // between 't' and 'w'
        ed.kill_to_start();
        assert_eq!(ed.lines(), vec!["one", "wo"]);
        assert_eq!(ed.cursor_row_col(), (1, 0));
        // Already at the line's start: nothing to do, and the line above
        // is untouched.
        ed.kill_to_start();
        assert_eq!(ed.lines(), vec!["one", "wo"]);
    }

    #[test]
    fn set_text_replaces_the_composer_and_parks_the_cursor_at_the_end() {
        let mut ed = Editor::default();
        ed.insert_str("/ne");
        ed.set_text("/new ");
        assert_eq!(ed.text(), "/new ");
        assert_eq!(ed.cursor_row_col(), (0, 5), "cursor after the space");
        // Replacing is total: prior content and cursor position are gone.
        ed.set_cursor(0);
        ed.set_text("héllo");
        assert_eq!(ed.cursor_row_col(), (0, 5), "é counts one char");
        ed.insert_char('!');
        assert_eq!(ed.lines(), vec!["héllo!"]);
    }

    #[test]
    fn pasted_text_inserts_verbatim_without_submitting() {
        let mut ed = Editor::default();
        ed.insert_str("a");
        ed.insert_paste("multi\r\nline\r\npaste");
        assert_eq!(ed.lines(), vec!["amulti", "line", "paste"]);
        assert_eq!(ed.cursor_row_col(), (2, 5));
        // The text sits in the editor; nothing about a paste submits it.
        assert!(ed.take_submission().is_some());
    }
}
