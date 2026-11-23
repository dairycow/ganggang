//! One keypress, classified. The mapping is pure so it is testable
//! without a terminal; the event loop decides what each action means
//! in state.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Insert(char),
    Backspace,
    Newline,
    Send,
    Abort,
    Quit,
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
    /// ↓: suggestion next while the popup is open; cursor down otherwise.
    SuggestNext,
    /// ↑: suggestion previous while the popup is open; cursor up otherwise.
    SuggestPrev,
    /// Tab while the suggestion popup is open.
    SuggestComplete,
    /// Ctrl+G: edit the draft in the external editor, which takes the
    /// terminal until it exits.
    ExternalEditor,
    /// Ctrl+R: rebuild and restart in `--watch` mode (a no-op hint
    /// otherwise).
    Rebuild,
    Ignore,
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
        KeyCode::Char('c') if ctrl => KeyAction::Quit,
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
        KeyCode::Esc => KeyAction::Abort,
        KeyCode::Backspace => KeyAction::Backspace,
        KeyCode::Home => KeyAction::LineStart,
        KeyCode::End => KeyAction::LineEnd,
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
