//! Tests for [`super`]: key classification, usage totals, and the view
//! state's folding of agent events — all the pure parts; the terminal
//! loop itself is exercised by the M4.6 manual pass.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use super::agent_task::usage_totals;
use super::footer::Footer;
use super::history::{Arrow, History};
use super::keys::{KeyAction, classify};
use super::transcript::LineKind;
use super::view::{Cancel, State};
use crate::agent::session::{Entry, Message, Role};
use crate::events::AgentEvent;

fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

#[test]
fn enter_sends_and_newline_keys_break_a_line() {
    assert_eq!(
        classify(key(KeyCode::Enter, KeyModifiers::NONE)),
        KeyAction::Send
    );
    assert_eq!(
        classify(key(KeyCode::Enter, KeyModifiers::SHIFT)),
        KeyAction::Newline
    );
    // Alt+Enter works through tmux and terminals that report Meta even
    // when they cannot tell Shift+Enter from Enter.
    assert_eq!(
        classify(key(KeyCode::Enter, KeyModifiers::ALT)),
        KeyAction::Newline
    );
    // Terminals without modifier reporting send a bare Enter for
    // Shift+Enter — that sends, and Ctrl+J is the portable newline.
    assert_eq!(
        classify(key(KeyCode::Char('j'), KeyModifiers::CONTROL)),
        KeyAction::Newline
    );
    // Kitty's CSI-u encoding of the same key can surface as U+000A.
    assert_eq!(
        classify(key(KeyCode::Char('\n'), KeyModifiers::CONTROL)),
        KeyAction::Newline
    );
}

#[test]
fn readline_keys_edit_the_composer_and_arrows_move_the_cursor() {
    let ctrl = KeyModifiers::CONTROL;
    assert_eq!(
        classify(key(KeyCode::Char('a'), ctrl)),
        KeyAction::LineStart
    );
    assert_eq!(classify(key(KeyCode::Char('e'), ctrl)), KeyAction::LineEnd);
    assert_eq!(
        classify(key(KeyCode::Char('k'), ctrl)),
        KeyAction::KillToEnd
    );
    assert_eq!(
        classify(key(KeyCode::Char('u'), ctrl)),
        KeyAction::KillToStart
    );
    assert_eq!(
        classify(key(KeyCode::Home, KeyModifiers::NONE)),
        KeyAction::LineStart
    );
    assert_eq!(
        classify(key(KeyCode::End, KeyModifiers::NONE)),
        KeyAction::LineEnd
    );
    assert_eq!(
        classify(key(KeyCode::Left, KeyModifiers::NONE)),
        KeyAction::CursorLeft
    );
    assert_eq!(
        classify(key(KeyCode::Right, KeyModifiers::NONE)),
        KeyAction::CursorRight
    );
    assert_eq!(classify(key(KeyCode::Left, ctrl)), KeyAction::WordLeft);
    assert_eq!(classify(key(KeyCode::Right, ctrl)), KeyAction::WordRight);
    // Plain letters stay text.
    assert_eq!(
        classify(key(KeyCode::Char('k'), KeyModifiers::NONE)),
        KeyAction::Insert('k')
    );
}

#[test]
fn ctrl_c_interrupts_ctrl_o_expands_ctrl_t_thinks_esc_aborts_pages_scroll() {
    assert_eq!(
        classify(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        KeyAction::Interrupt,
        "Ctrl+C is muscle memory: it cancels, it does not quit"
    );
    assert_eq!(
        classify(key(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        KeyAction::ToggleExpand
    );
    assert_eq!(
        classify(key(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        KeyAction::ToggleThinking
    );
    assert_eq!(
        classify(key(KeyCode::Esc, KeyModifiers::NONE)),
        KeyAction::Abort
    );
    assert_eq!(
        classify(key(KeyCode::PageUp, KeyModifiers::NONE)),
        KeyAction::ScrollUp
    );
    assert_eq!(
        classify(key(KeyCode::PageDown, KeyModifiers::NONE)),
        KeyAction::ScrollDown
    );
}

#[test]
fn ctrl_g_opens_the_external_editor_and_plain_g_is_text() {
    assert_eq!(
        classify(key(KeyCode::Char('g'), KeyModifiers::CONTROL)),
        KeyAction::ExternalEditor
    );
    assert_eq!(
        classify(key(KeyCode::Char('g'), KeyModifiers::NONE)),
        KeyAction::Insert('g')
    );
}

#[test]
fn ctrl_r_requests_a_watch_rebuild_and_plain_r_is_text() {
    assert_eq!(
        classify(key(KeyCode::Char('r'), KeyModifiers::CONTROL)),
        KeyAction::Rebuild
    );
    assert_eq!(
        classify(key(KeyCode::Char('r'), KeyModifiers::NONE)),
        KeyAction::Insert('r')
    );
}

/// Ctrl+P / Ctrl+N: history recall that does not care what the composer
/// holds (arrows do — a multi-line composer keeps its line cursor).
#[test]
fn ctrl_p_and_ctrl_n_recall_history_and_plain_letters_type() {
    let ctrl = KeyModifiers::CONTROL;
    assert_eq!(
        classify(key(KeyCode::Char('p'), ctrl)),
        KeyAction::HistoryPrev
    );
    assert_eq!(
        classify(key(KeyCode::Char('n'), ctrl)),
        KeyAction::HistoryNext
    );
    // tmux and terminals that report Meta dress these up; the control
    // modifier is what counts.
    assert_eq!(
        classify(key(KeyCode::Char('p'), ctrl | KeyModifiers::SHIFT)),
        KeyAction::HistoryPrev
    );
    assert_eq!(
        classify(key(KeyCode::Char('p'), KeyModifiers::NONE)),
        KeyAction::Insert('p'),
        "plain letters stay text"
    );
    assert_eq!(
        classify(key(KeyCode::Char('n'), KeyModifiers::ALT)),
        KeyAction::Ignore,
        "Alt+letter is not a history key"
    );
}

/// The mouse is the wheel and nothing else: a notch scrolls three rows in
/// the direction the transcript counts (up = older), and every other mouse
/// event — a press, a release, a drag, a hold — means nothing at all. Which
/// is also why a stray click cannot move a cursor or a selection.
#[test]
fn the_mouse_wheel_pages_three_rows_and_every_other_mouse_event_is_ignored() {
    use super::keys::{WHEEL_ROWS, Wheel, classify_wheel};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let at = |kind: MouseEventKind| MouseEvent {
        kind,
        column: 40,
        row: 7,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(
        classify_wheel(at(MouseEventKind::ScrollUp)),
        Some(Wheel::Up)
    );
    assert_eq!(
        classify_wheel(at(MouseEventKind::ScrollDown)),
        Some(Wheel::Down),
        "down shows the newer rows"
    );
    assert_eq!(
        WHEEL_ROWS, 3,
        "a notch moves a chunk without losing the row being read"
    );
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Right),
        MouseEventKind::Moved,
        MouseEventKind::ScrollLeft,
        MouseEventKind::ScrollRight,
    ] {
        assert_eq!(classify_wheel(at(kind)), None, "{kind:?} means nothing");
    }
    // tmux and friends report a wheel notch with a modifier sometimes; the
    // kind is what counts.
    let shifted = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 1,
        row: 1,
        modifiers: KeyModifiers::SHIFT,
    };
    assert_eq!(classify_wheel(shifted), Some(Wheel::Down));
}

/// The wheel drives the transcript's scroll, the same clamp PageUp/PageDown
/// obey: three rows a notch, never past the oldest row, never past newest.
#[test]
fn a_wheel_notch_scrolls_the_transcript_three_rows_at_a_time() {
    let mut state = state_with_history(&[]);
    for i in 0..100 {
        state.apply(&AgentEvent::TurnStarted);
        state.apply(&AgentEvent::AssistantText {
            delta: format!("line {i}"),
        });
    }
    assert_eq!(state.offset_for_test(), 0, "pinned to the newest");
    state.scroll_up(super::keys::WHEEL_ROWS);
    assert_eq!(state.offset_for_test(), 3);
    state.scroll_up(super::keys::WHEEL_ROWS);
    assert_eq!(state.offset_for_test(), 6, "two notches, six rows");
    state.scroll_down(super::keys::WHEEL_ROWS);
    state.scroll_down(super::keys::WHEEL_ROWS);
    assert_eq!(state.offset_for_test(), 0, "and back to the newest");
    for _ in 0..100 {
        state.scroll_up(super::keys::WHEEL_ROWS);
    }
    assert_eq!(state.offset_for_test(), 99, "clamped at the oldest row");
}

#[test]
fn plain_chars_insert_and_controlled_or_alt_chars_do_not() {
    assert_eq!(
        classify(key(KeyCode::Char('a'), KeyModifiers::NONE)),
        KeyAction::Insert('a')
    );
    // crossterm reports a shifted letter as the uppercase char plus SHIFT.
    assert_eq!(
        classify(key(KeyCode::Char('A'), KeyModifiers::SHIFT)),
        KeyAction::Insert('A')
    );
    assert_eq!(
        classify(key(KeyCode::Char('f'), KeyModifiers::CONTROL)),
        KeyAction::Ignore,
        "Ctrl+<letter> shortcuts are actions, never text"
    );
    assert_eq!(
        classify(key(KeyCode::Char('x'), KeyModifiers::ALT)),
        KeyAction::Ignore
    );
}

#[test]
fn key_release_events_are_ignored() {
    let release = KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    };
    assert_eq!(classify(release), KeyAction::Ignore);
}

#[test]
fn tab_and_arrows_drive_the_suggestion_popup() {
    assert_eq!(
        classify(key(KeyCode::Tab, KeyModifiers::NONE)),
        KeyAction::SuggestComplete
    );
    assert_eq!(
        classify(key(KeyCode::Up, KeyModifiers::NONE)),
        KeyAction::SuggestPrev
    );
    assert_eq!(
        classify(key(KeyCode::Down, KeyModifiers::NONE)),
        KeyAction::SuggestNext
    );
    // Terminals dress Tab differently (kitty sends it with modifiers);
    // a modified Tab still completes — nothing else wants it.
    assert_eq!(
        classify(key(KeyCode::Tab, KeyModifiers::CONTROL)),
        KeyAction::SuggestComplete
    );
    // Shift+Tab is not a popup key.
    assert_eq!(
        classify(key(KeyCode::BackTab, KeyModifiers::NONE)),
        KeyAction::Ignore
    );
}

/// The popup's full key-path, through the view state the loop drives:
/// `/` opens it, typing filters it, ↑/↓ move the highlight (which an
/// edit resets), Tab completes the highlighted row, and the completed
/// text — trailing space and all — closes it.
#[test]
fn suggestion_popup_tracks_the_composer() {
    use crate::agent::skills::Skill;
    use std::path::PathBuf;
    use std::sync::Arc;

    let skills = vec![Skill {
        name: "pdf-tools".into(),
        description: "pdf extraction".into(),
        disable_model_invocation: false,
        path: PathBuf::from("/tmp/pdf-tools/SKILL.md"),
        instructions: String::new(),
    }];
    let mut state = State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        Arc::new(skills),
        History::default(),
    );

    assert!(!state.suggest_open(), "plain composer: no popup");
    state.editor.insert_char('/');
    assert!(state.suggest_open(), "a bare slash opens the catalog");
    assert_eq!(state.suggestions().len(), 10, "the built-in set");

    // Filtering: two down and one up leaves the highlight on /compact.
    state.suggest_next();
    state.suggest_next();
    state.suggest_next();
    state.suggest_prev();
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/compact ");
    assert!(!state.suggest_open(), "whitespace closes the popup");

    // An edit resets the highlight to the top row: the next-key moved
    // it, the set_text changed the list, and completion still lands on
    // the row the user sees highlighted.
    state.editor.set_text("/ski");
    state.suggest_next();
    state.editor.set_text("/sk");
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/skills ");
    assert!(!state.suggest_open(), "whitespace closes the popup");

    // The skill colon expands one row per matching skill.
    state.editor.set_text("/skill:p");
    assert_eq!(
        state
            .suggestions()
            .iter()
            .map(|s| s.label.clone())
            .collect::<Vec<_>>(),
        vec!["/skill:pdf-tools"]
    );

    // Selection cannot run off either end — and with the popup closed
    // (here: the completed text carries a space), the popup keys are
    // inert, exactly as the loop's `suggest_open` guards make them.
    state.editor.set_text("/");
    state.suggest_prev();
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/new ", "top row");
    for _ in 0..20 {
        state.suggest_next();
    }
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/new ", "closed popup: nothing moves");
    state.editor.set_text("/");
    for _ in 0..20 {
        state.suggest_next();
    }
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/quit ", "last row");
}

/// A short terminal draws fewer rows than the list holds; render
/// publishes the drawn count and the selection clamps to it, so the
/// highlight never walks off-screen and Tab never completes an
/// invisible row. Unrendered (0), the full list is selectable — tests
/// and the pre-first-frame window.
#[test]
fn the_selection_stops_at_the_last_drawn_row_on_short_terminals() {
    let mut state = State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        std::sync::Arc::new(vec![]),
        History::default(),
    );
    state.editor.insert_char('/');

    // Five rows fit: ↓ walks to index 4 and stops there.
    state.set_suggest_view_rows_for_test(5);
    for _ in 0..20 {
        state.suggest_next();
    }
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/session ", "row 4 of the catalog");

    // Never rendered: the whole list is reachable.
    state.editor.set_text("/");
    state.set_suggest_view_rows_for_test(0);
    for _ in 0..20 {
        state.suggest_next();
    }
    state.complete_suggestion();
    assert_eq!(state.editor.text(), "/quit ", "last row of the full list");
}

#[test]
fn usage_totals_sum_assistant_entries_only() {
    let entry = |usage: Option<u32>| Entry::Message {
        id: "e".into(),
        parent_id: None,
        timestamp: "t".into(),
        message: Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
            usage: usage.map(|total| crate::agent::session::Usage {
                prompt_tokens: total,
                completion_tokens: 1,
                total_tokens: total + 1,
                ..Default::default()
            }),
            is_error: false,
            reasoning: None,
        },
    };
    let entries = vec![
        entry(Some(100)),
        Entry::Message {
            id: "u".into(),
            parent_id: None,
            timestamp: "t".into(),
            message: Message {
                role: Role::User,
                content: Some("hi".into()),
                tool_calls: Vec::new(),
                tool_call_id: None,
                usage: Some(crate::agent::session::Usage {
                    prompt_tokens: 999,
                    completion_tokens: 999,
                    total_tokens: 999,
                    ..Default::default()
                }), // a provider that stamps usage on a user message
                is_error: false,
                reasoning: None,
            },
        },
        entry(Some(50)),
    ];
    // Only the two assistant entries count.
    assert_eq!(usage_totals(&entries), (150, 2));
}

#[test]
fn view_folds_the_event_stream_into_lines() {
    let mut state = State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        std::sync::Arc::new(vec![]),
        History::default(),
    );
    state.push_user("question");
    state.apply(&AgentEvent::RunStarted);
    state.apply(&AgentEvent::TurnStarted);
    state.apply(&AgentEvent::AssistantText {
        delta: "part one\npart two".into(),
    });
    state.apply(&AgentEvent::ToolCallStarted {
        name: "read".into(),
        id: "c1".into(),
        args: serde_json::json!({}),
    });
    state.apply(&AgentEvent::Warned {
        message: "careful".into(),
    });
    state.apply(&AgentEvent::RunAborted);

    let kinds: Vec<LineKind> = state_line_kinds(&state);
    assert_eq!(
        kinds,
        vec![
            LineKind::User,
            LineKind::Assistant,
            LineKind::Assistant,
            LineKind::Tool,
            LineKind::Warning,
            LineKind::Aborted,
        ]
    );
    let texts = state_line_texts(&state);
    assert_eq!(texts[0], "you │ question");
    assert_eq!(texts[1], "part one");
    assert_eq!(texts[2], "part two");
    assert_eq!(texts[3], "▸ read {}", "the collapsed call line");
    assert_eq!(texts[4], "⚠ careful");
    assert_eq!(texts[5], "■ run aborted");
}

/// Split deltas of one streamed answer fold into a single assistant
/// block — the same lines the one-shot (resume/replay) shape produces.
#[test]
fn split_deltas_fold_into_the_same_block_as_one_shot_text() {
    let streamed = {
        let mut state = State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            std::sync::Arc::new(vec![]),
            History::default(),
        );
        state.push_user("q");
        state.apply(&AgentEvent::TurnStarted);
        for piece in ["Count", "ing: ", "one", " two", " three"] {
            state.apply(&AgentEvent::AssistantText {
                delta: piece.into(),
            });
        }
        state.apply(&AgentEvent::TurnEnded {
            stop_reason: "stop".into(),
            usage: None,
            timings: None,
            thinking_pct: None,
        });
        state_line_texts(&state)
    };
    let one_shot = {
        let mut state = State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            std::sync::Arc::new(vec![]),
            History::default(),
        );
        state.push_user("q");
        state.apply(&AgentEvent::TurnStarted);
        state.apply(&AgentEvent::AssistantText {
            delta: "Counting: one two three".into(),
        });
        state.apply(&AgentEvent::TurnEnded {
            stop_reason: "stop".into(),
            usage: None,
            timings: None,
            thinking_pct: None,
        });
        state_line_texts(&state)
    };
    assert_eq!(streamed, one_shot);
    assert_eq!(streamed, vec!["you │ q", "Counting: one two three"]);
}

// `lines` is private to view; the test module sits inside the tui module
// tree so the accessors below reach it via a tiny seam instead.
fn state_line_kinds(state: &State) -> Vec<LineKind> {
    state.line_kinds_for_test()
}

fn state_line_texts(state: &State) -> Vec<String> {
    state.line_texts_for_test()
}

#[test]
fn scrolling_stays_within_the_transcript() {
    let mut state = State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        std::sync::Arc::new(vec![]),
        History::default(),
    );
    for i in 0..100 {
        // One text per turn: TurnStarted closes the previous assistant
        // line (consecutive deltas append — that is the streaming shape).
        state.apply(&AgentEvent::TurnStarted);
        state.apply(&AgentEvent::AssistantText {
            delta: format!("line {i}"),
        });
    }
    assert_eq!(state.offset_for_test(), 0, "starts pinned to newest");
    state.scroll_up(10);
    assert_eq!(state.offset_for_test(), 10);
    state.scroll_up(1000); // clamps at the oldest line
    assert_eq!(state.offset_for_test(), 99);
    state.scroll_down(5);
    assert_eq!(state.offset_for_test(), 94);
    state.scroll_down(1000); // reaching the bottom re-pins
    assert_eq!(state.offset_for_test(), 0);
}

#[test]
fn footer_start_values_render_sensibly() {
    let f = Footer::new(std::path::Path::new("/tmp/x"), "m");
    assert!(f.render().contains("↑0 ↓0"));
    assert!(f.render().contains("○ ready"));
}

// ------------------------------------------------------- recall & Ctrl+C --

/// A view with a submitted history behind it — the state the loop drives
/// when an arrow or Ctrl+P/N is pressed.
fn state_with_history(prompts: &[&str]) -> State {
    let mut history = History::default();
    for prompt in prompts {
        history.record(prompt);
    }
    State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        std::sync::Arc::new(vec![]),
        history,
    )
}

/// The arrows of an empty or single-line composer recall; the popup keeps
/// them while it is open; a multi-line composer keeps its line cursor.
#[test]
fn arrows_recall_history_from_an_empty_or_single_line_composer() {
    let mut state = state_with_history(&["first prompt", "second prompt"]);
    assert_eq!(state.arrow(true), Arrow::Prev);
    assert_eq!(
        state.arrow(false),
        Arrow::Nothing,
        "↓ has nothing to walk before a browse starts"
    );

    state.recall_prev();
    assert_eq!(state.editor.text(), "second prompt");
    state.recall_prev();
    assert_eq!(state.editor.text(), "first prompt");
    state.recall_prev();
    assert_eq!(
        state.editor.text(),
        "first prompt",
        "the oldest is where it stops"
    );

    // Down walks forward, and past the newest comes back what the composer
    // held when browsing began — here, nothing.
    state.recall_next();
    assert_eq!(state.editor.text(), "second prompt");
    state.recall_next();
    assert_eq!(state.editor.text(), "", "the draft, and browsing is over");
    assert_eq!(state.arrow(true), Arrow::Prev, "a fresh ↑ starts over");
}

/// A non-empty draft is not lost to a recall: the first ↑ saves it, ↓ past
/// the newest gives it back.
#[test]
fn a_half_typed_draft_survives_a_recall() {
    let mut state = state_with_history(&["older prompt"]);
    state.editor.set_text("half a thought");
    assert_eq!(state.arrow(true), Arrow::Prev);
    state.recall_prev();
    assert_eq!(state.editor.text(), "older prompt");
    // Recalled text is ordinary composer text: editable.
    state.editor.insert_char('?');
    assert_eq!(state.editor.text(), "older prompt?");
    state.recall_next();
    assert_eq!(state.editor.text(), "half a thought", "the draft came back");
}

#[test]
fn a_multiline_composer_keeps_its_line_cursor_until_a_browse_starts() {
    let mut state = state_with_history(&["one", "two"]);
    state.editor.set_text("first\nsecond\nthird");
    assert_eq!(state.arrow(true), Arrow::LineUp);
    assert_eq!(state.arrow(false), Arrow::LineDown);
    state.editor.move_up();
    assert_eq!(
        state.editor.cursor_row_col(),
        (1, 5),
        "not history, just the cursor (the column of the line it left)"
    );

    // Ctrl+P recalls whatever the composer holds, and once a browse is
    // running the arrows follow it — even showing a multi-line prompt.
    state.recall_prev();
    assert_eq!(state.editor.text(), "two");
    assert_eq!(state.arrow(true), Arrow::Prev);
    assert_eq!(state.arrow(false), Arrow::Next);
    state.recall_prev();
    assert_eq!(state.editor.text(), "one");
    state.recall_next();
    assert_eq!(state.editor.text(), "two");
}

/// While the popup is open the arrows are its, history and all.
#[test]
fn the_open_popup_outranks_history_for_the_arrows() {
    let mut state = state_with_history(&["prompt"]);
    state.editor.insert_char('/');
    assert!(state.suggest_open());
    assert_eq!(state.arrow(true), Arrow::SuggestPrev);
    assert_eq!(state.arrow(false), Arrow::SuggestNext);
}

/// Ctrl+C: cancel a live run, clear a draft, and only then quit.
#[test]
fn ctrl_c_cancels_a_run_clears_a_draft_and_only_then_quits() {
    let mut state = state_with_history(&[]);
    // Idle with nothing typed: there is nothing left to cancel.
    assert_eq!(state.interrupt(), Cancel::Quit);

    // Idle with a draft: the first press clears it, the next quits.
    state.editor.set_text("typed but unsent");
    assert_eq!(state.interrupt(), Cancel::DraftCleared);
    assert_eq!(state.editor.text(), "");
    assert_eq!(state.interrupt(), Cancel::Quit);

    // Busy: the first press asks the run to stop and the footer says so;
    // a second, while it is still busy, ends the TUI.
    state.begin_run();
    assert_eq!(state.interrupt(), Cancel::Stopping);
    assert!(state.stopping());
    assert!(state.footer.render().contains("stopping…"));
    assert_eq!(state.interrupt(), Cancel::ForceQuit);
    assert!(state.busy, "the run is still winding down until it reports");

    // Esc stops a run too, but it does not arm an exit: the Ctrl+C that
    // follows still just confirms the stop, and only the next one leaves.
    state.run_finished();
    state.begin_run();
    state.set_stopping(true); // what the loop's Esc arm does
    assert_eq!(state.interrupt(), Cancel::Stopping);
    assert_eq!(state.interrupt(), Cancel::ForceQuit);

    // Any run ending clears the mark — completed, failed, or aborted.
    state.run_finished();
    assert!(!state.stopping());
    assert!(state.footer.render().contains("○ ready"));
    // And a new run does not inherit one.
    state.begin_run();
    state.set_stopping(true);
    assert!(state.stopping());
    state.begin_run();
    assert!(!state.stopping(), "begin_run clears it");
}

/// The stopping mark belongs to a live run: idle, asking for it does
/// nothing (Esc on an idle view leaves no mark, and Ctrl+C's own idle path
/// is the draft/quit pair instead).
#[test]
fn stopping_only_means_something_while_busy() {
    let mut state = state_with_history(&[]);
    state.set_stopping(true);
    assert!(!state.stopping());
    assert!(state.footer.render().contains("○ ready"));
    state.begin_run();
    state.set_stopping(true);
    assert!(state.stopping());
    // `/new` ends it too: the mark is about a run, and a new session has
    // none in flight.
    state.new_session(None, false);
    assert!(!state.stopping());
    assert!(!state.history.browsing(), "/new ends a recall as well");
}

#[tokio::test]
async fn shell_output_over_the_cap_with_multibyte_chars_never_panics() {
    // 14_000 bytes of 2-byte chars: a byte-index truncate at 10_240 would
    // land mid-char. The shared bash runner cuts at a char boundary.
    let (output, code) =
        super::shell::run_shell("printf 'é%.0s' {1..7000}", std::path::Path::new("/tmp")).await;
    assert_eq!(code, Some(0));
    assert!(output.contains("é"), "{output:?}");
    assert!(
        output.len() < super::shell::SHELL_OUTPUT_CAP + 100,
        "capped with a notice: {}",
        output.len()
    );
    assert!(output.contains("bytes omitted"));
}

#[test]
fn long_transcript_lines_wrap_into_exact_rows() {
    use super::line::wrap_line;
    use super::transcript::{LineKind, RenderedLine};

    let line = RenderedLine::new("aéz ".repeat(10), LineKind::Assistant); // 40 chars, multibyte
    let rows = wrap_line(&line, 15);
    assert!(rows.len() >= 3, "40 chars / 15-wide rows: {}", rows.len());
    for row in &rows {
        assert!(row.text.chars().count() <= 15);
        assert_eq!(row.kind, LineKind::Assistant, "style rides every row");
    }
    // No character lost: concatenation reproduces the original.
    let joined: String = rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(joined, line.text);

    let one = wrap_line(&RenderedLine::new("short", LineKind::User), 80);
    assert_eq!(one.len(), 1);
}

/// A bold span split across wrapped rows styles both pieces at the right
/// row-local offsets, and no character is lost or duplicated.
#[test]
fn bold_ranges_split_across_wrapped_rows() {
    use super::line::wrap_line;
    use super::transcript::{LineKind, Mark, RenderedLine};

    // 20 chars, bold covering chars 8..14 — split at width 10.
    let line = RenderedLine::new("0123456789abcdefghij", LineKind::Assistant).with_marks(vec![(
        8,
        14,
        Mark::Bold,
    )]);
    let rows = wrap_line(&line, 10);
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].marks,
        vec![(8, 10, Mark::Bold)],
        "clipped to the first row"
    );
    assert_eq!(
        rows[1].marks,
        vec![(0, 4, Mark::Bold)],
        "remainder, row-local"
    );
    let joined: String = rows.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(joined, line.text);
}

/// Markdown-free text renders exactly as before: same lines, same kinds,
/// no bold — the pass must be invisible when nothing needs it.
#[test]
fn plain_assistant_text_renders_identically_through_markdown() {
    let mut t = super::transcript::Transcript::default();
    t.apply(&AgentEvent::AssistantText {
        delta: "plain one\nplain two".into(),
    });
    let rendered = t.render();
    assert_eq!(rendered.len(), 2);
    assert_eq!(rendered[0].text, "plain one");
    assert_eq!(rendered[0].kind, super::transcript::LineKind::Assistant);
    assert!(rendered[0].marks.is_empty());
}

#[test]
fn seed_from_entries_rebuilds_the_blocks_the_event_stream_would() {
    use crate::agent::session::{Entry, Message, Role, ToolCall};

    let entry = |id: &str, parent: Option<&str>, message: Message| Entry::Message {
        id: id.into(),
        parent_id: parent.map(String::from),
        timestamp: "t".into(),
        message,
    };
    let user = |text: &str| Message {
        role: Role::User,
        content: Some(text.into()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        usage: None,
        is_error: false,
        reasoning: None,
    };
    let assistant = |content: Option<&str>, usage: Option<u32>| Message {
        role: Role::Assistant,
        content: content.map(String::from),
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            args: serde_json::json!({"command": "ls"}),
            args_error: None,
        }],
        tool_call_id: None,
        usage: usage.map(|p| crate::agent::session::Usage {
            prompt_tokens: p,
            completion_tokens: 7,
            total_tokens: p + 7,
            ..Default::default()
        }),
        is_error: false,
        reasoning: None,
    };
    let tool_result = |id: &str, output: &str, error: bool| Message {
        role: Role::ToolResult,
        content: Some(output.into()),
        tool_calls: Vec::new(),
        tool_call_id: Some(id.into()),
        usage: None,
        is_error: error,
        reasoning: None,
    };

    let entries = vec![
        entry("e1", None, user("list the files")),
        entry("e2", Some("e1"), assistant(None, Some(120))),
        entry(
            "e3",
            Some("e2"),
            tool_result("call_1", "a.txt\nb.txt", false),
        ),
        entry("e4", Some("e3"), assistant(Some("two files"), Some(200))),
    ];

    let mut state = State::new(
        std::path::Path::new("/tmp/x"),
        "m",
        131_072,
        std::sync::Arc::new(vec![]),
        History::default(),
    );
    state.seed_from_entries(&entries);

    let texts = state_line_texts(&state);
    assert_eq!(texts[0], "you │ list the files");
    assert!(texts[1].starts_with("▸ bash "), "{texts:?}");
    assert!(texts[1].ends_with("✓"), "result known from disk: {texts:?}");
    assert_eq!(texts[2], "two files");
    // Footer totals come from the seeded entries, not from zero.
    assert_eq!((state.tokens_in, state.tokens_out), (320, 14));
}

/// The `/compact` plumbing end to end, headless: the command goes in as
/// an `AgentInput`, the outcome comes back as a `UiEvent::Compacted`
/// carrying the message and a fresh context estimate — and the task
/// keeps serving afterward. The empty-session path makes no LLM call,
/// so the client can point at a dead port.
#[tokio::test]
async fn compact_input_reports_through_the_ui_channel() {
    use super::agent_task::{AgentInput, UiEvent, agent_task};
    use crate::agent::Agent;
    use crate::agent::tools::Registry;
    use crate::llm::client::LlmClient;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let agent = Agent::new(
        LlmClient::new("http://127.0.0.1:1").unwrap(),
        Registry::new(),
        "m".into(),
        64,
        Arc::new(AtomicBool::new(false)),
        std::env::temp_dir(),
        None,
        false,
        131_072,
        Default::default(),
    );
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let (ui_tx, mut ui_rx) = tokio::sync::mpsc::channel(8);
    tokio::spawn(agent_task(agent, rx, ui_tx));

    tx.send(AgentInput::Compact(None)).await.unwrap();
    match ui_rx.recv().await.unwrap() {
        UiEvent::Compacted {
            message,
            context_tokens,
        } => {
            assert!(message.contains("nothing to compact yet"), "{message}");
            assert_eq!(context_tokens, 0, "an empty session estimates zero");
        }
        other => panic!("expected Compacted, got {other:?}"),
    }

    // The task survives the command and answers the next one too.
    tx.send(AgentInput::Compact(Some("focus".into())))
        .await
        .unwrap();
    assert!(matches!(
        ui_rx.recv().await.unwrap(),
        UiEvent::Compacted { .. }
    ));
}

/// How the loop folds what the agent task reports: each outcome lands
/// where the UI shows it, a `!` line's output becomes the model's next
/// message, and only a dead agent task ends the loop.
#[tokio::test]
async fn task_events_fold_into_the_view() {
    use super::agent_task::{AgentInput, UiEvent, apply};
    use std::sync::Arc;

    let fresh = || {
        State::new(
            std::path::Path::new("/tmp/x"),
            "m",
            131_072,
            Arc::new(vec![]),
            History::default(),
        )
    };
    let mut state = fresh();
    let (mut input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);

    assert!(
        apply(
            UiEvent::RunFailed {
                error: "boom".into()
            },
            &mut state,
            &mut input_tx
        )
        .await
    );
    assert_eq!(state_line_texts(&state), vec!["⚠ run failed: boom"]);
    assert!(!state.busy, "a failed run is not left busy");

    apply(
        UiEvent::SystemPrompt("the prompt".into()),
        &mut state,
        &mut input_tx,
    )
    .await;
    assert_eq!(
        state_line_texts(&state)[1..],
        vec!["— system prompt —", "the prompt"]
    );

    apply(
        UiEvent::Compacted {
            message: "compacted: 3 entries".into(),
            context_tokens: 65_536,
        },
        &mut state,
        &mut input_tx,
    )
    .await;
    assert_eq!(state_line_texts(&state)[3], "compacted: 3 entries");
    assert!(
        state.footer.render().contains("ctx 50%"),
        "{}",
        state.footer.render()
    );

    state.begin_run();
    apply(
        UiEvent::RunComplete {
            tokens_in: 305,
            tokens_out: 81,
            context_tokens: 13_108,
            entries: 3,
        },
        &mut state,
        &mut input_tx,
    )
    .await;
    assert_eq!(
        (state.tokens_in, state.tokens_out, state.entries),
        (305, 81, 3)
    );
    assert!(state.footer.render().contains("↑305 ↓81"));
    assert!(state.footer.render().contains("ctx 10%"));
    assert!(!state.busy, "the run is over");

    // A `!!` line is shown and nothing goes to the model.
    let shell = |to_context: bool| UiEvent::ShellDone {
        command: "ls".into(),
        output: "a.txt".into(),
        exit_code: None,
        to_context,
    };
    assert!(apply(shell(false), &mut state, &mut input_tx).await);
    assert!(!state.busy);
    assert!(input_rx.try_recv().is_err(), "!! keeps the output local");

    // A `!` line becomes the model's next message, between shell markers.
    assert!(apply(shell(true), &mut state, &mut input_tx).await);
    assert!(state.busy, "the model is running on the output");
    assert!(matches!(
        input_rx.recv().await,
        Some(AgentInput::Prompt(text)) if text == "[shell] $ ls\n[exit killed by signal]\na.txt"
    ));
    drop(input_rx);
    assert!(
        !apply(shell(true), &mut state, &mut input_tx).await,
        "an agent task that is gone ends the loop"
    );

    // What the agent itself streamed still folds straight in.
    let mut state = fresh();
    let (mut input_tx, _input_rx) = tokio::sync::mpsc::channel(4);
    apply(
        UiEvent::Agent(AgentEvent::Warned {
            message: "careful".into(),
        }),
        &mut state,
        &mut input_tx,
    )
    .await;
    assert_eq!(state_line_texts(&state), vec!["⚠ careful"]);
}
