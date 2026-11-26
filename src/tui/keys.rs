//! One keypress, classified. The mapping is pure so it is testable
//! without a terminal; the event loop decides what each action means
//! in state.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Insert(char),
    Backspace,
    Newline,
    Send,
    Abort,
    /// Ctrl+C: cancel what is happening — the run when one is in flight,
    /// the draft when one is typed — and quit only when there is nothing
    /// left to cancel. Never an unconditional quit (gap §5). The way out
    /// on one's own terms stays `/quit`, a command rather than a key.
    Interrupt,
    ToggleExpand,
    ToggleThinking,
    CopyAnswer,
    ScrollUp,
    ScrollDown,
    /// Ctrl+A / Home: the start of the current line.
    LineStart,
    /// Ctrl+E / End: the end of the current line.
    LineEnd,
    /// Ctrl+K: delete from the cursor to the end of the line (readline:
    /// at the end, the newline itself goes and the next line joins on).
    KillToEnd,
    /// Ctrl+U: delete from the start of the line to the cursor.
    KillToStart,
    CursorLeft,
    CursorRight,
    /// Ctrl+←: to the start of the word before the cursor (readline's
    /// backward-word).
    WordLeft,
    /// Ctrl+→: past the word after the cursor (readline's forward-word).
    WordRight,
    /// ↑: suggestion previous while the popup is open; history recall or
    /// cursor up otherwise (the loop decides — see
    /// [`history_for`](super::history::history_for)).
    SuggestPrev,
    /// ↓: suggestion next while the popup is open; history recall, cursor
    /// down, or nothing (same rule).
    SuggestNext,
    /// Ctrl+P / Ctrl+N: history recall whatever the composer holds, arrows
    /// aside (readline's muscle memory, gap §4).
    HistoryPrev,
    HistoryNext,
    /// Tab while the suggestion popup is open, or the `/session` list.
    SuggestComplete,
    /// Ctrl+G: edit the draft in the external editor, which takes the
    /// terminal until it exits.
    ExternalEditor,
    /// Ctrl+R: rebuild and restart in `--watch` mode (a no-op hint
    /// otherwise).
    Rebuild,
    Ignore,
}

/// What one mouse-wheel notch does: `Up` shows older rows, `Down` newer
/// ones — the same direction the transcript's scroll offset counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wheel {
    Up,
    Down,
}

/// Rows one mouse-wheel notch scrolls. Three: a notch should move a chunk
/// of the transcript without losing the row a reader was looking at, and a
/// PageUp/PageDown (ten) stays the bigger step.
pub const WHEEL_ROWS: u16 = 3;

/// What a mouse event means. Only the wheel means anything: a press, a
/// release, a drag or a hold does nothing at all, because gg uses the mouse
/// to scroll and for no other purpose — which is also why a click cannot
/// move a cursor or select the wrong thing. Pure, so the mapping is testable
/// without a terminal.
pub fn classify_wheel(event: MouseEvent) -> Option<Wheel> {
    match event.kind {
        MouseEventKind::ScrollUp => Some(Wheel::Up),
        MouseEventKind::ScrollDown => Some(Wheel::Down),
        _ => None,
    }
}

/// Shift+Enter arrives as SHIFT|Enter only from terminals that report
/// key modifiers (the kitty protocol, or tmux's `extended-keys on`);
/// most send a plain Enter. Ctrl+J and Alt+Enter are the portable
/// newline keys — documented behavior, not a fallback the code hides.
pub fn classify(key: KeyEvent) -> KeyAction {
    if key.kind != KeyEventKind::Press {
        return KeyAction::Ignore;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => KeyAction::Newline,
        KeyCode::Enter => KeyAction::Send,
        // Kitty's CSI-u form of Ctrl+J can arrive as U+000A with CONTROL
        // rather than 'j'. Both mean the same key.
        KeyCode::Char('\n') if ctrl => KeyAction::Newline,
        KeyCode::Char('j') if ctrl => KeyAction::Newline,
        KeyCode::Char('c') if ctrl => KeyAction::Interrupt,
        KeyCode::Char('o') if ctrl => KeyAction::ToggleExpand,
        KeyCode::Char('t') if ctrl => KeyAction::ToggleThinking,
        KeyCode::Char('x') if ctrl => KeyAction::CopyAnswer,
        KeyCode::Char('g') if ctrl => KeyAction::ExternalEditor,
        KeyCode::Char('r') if ctrl => KeyAction::Rebuild,
        // Readline's line editing, on the composer.
        KeyCode::Char('a') if ctrl => KeyAction::LineStart,
        KeyCode::Char('e') if ctrl => KeyAction::LineEnd,
        KeyCode::Char('k') if ctrl => KeyAction::KillToEnd,
        KeyCode::Char('u') if ctrl => KeyAction::KillToStart,
        // Readline's history, kept beside readline's line editing. Ctrl+P
        // was unassigned; Ctrl+N was ignored.
        KeyCode::Char('p') if ctrl => KeyAction::HistoryPrev,
        KeyCode::Char('n') if ctrl => KeyAction::HistoryNext,
        KeyCode::Esc => KeyAction::Abort,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Home => KeyAction::LineStart,
        KeyCode::End => KeyAction::LineEnd,
        KeyCode::Left if ctrl => KeyAction::WordLeft,
        KeyCode::Right if ctrl => KeyAction::WordRight,
        KeyCode::Left => KeyAction::CursorLeft,
        KeyCode::Right => KeyAction::CursorRight,
        KeyCode::PageUp => KeyAction::ScrollUp,
        KeyCode::PageDown => KeyAction::ScrollDown,
        // Arrow keys belong to the suggestion popup when it is open; with
        // it closed they move the composer's cursor (PageUp/Down own
        // paging).
        KeyCode::Up => KeyAction::SuggestPrev,
        KeyCode::Down => KeyAction::SuggestNext,
        KeyCode::Tab => KeyAction::SuggestComplete,
        KeyCode::BackTab => KeyAction::Ignore,
        KeyCode::Char(c) if !ctrl && !alt => KeyAction::Insert(c),
        _ => KeyAction::Ignore,
    }
}
