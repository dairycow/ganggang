//! The terminal-event reader. crossterm's `read` blocks, and blocking has
//! no place inside a tokio `select!` — so a dedicated thread reads and
//! forwards key and paste events. Bracketed paste is what makes multi-line
//! pastes land as text instead of submitting at the first newline.

use std::time::Duration;

use crossterm::event::Event;
use tokio::sync::mpsc;

use super::keys::{KeyAction, classify};

pub(super) struct KeyReader {
    events: mpsc::Receiver<Event>,
    /// Wakes the thread after a key that lent the terminal away.
    resume: std::sync::mpsc::Sender<()>,
}

impl KeyReader {
    pub(super) fn spawn() -> Self {
        let (tx, events) = mpsc::channel(64);
        let (resume, wake) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(event @ (Event::Key(_) | Event::Paste(_))) => {
                        // Ctrl+G lends the terminal to the editor. The read
                        // cannot be interrupted from outside, and while
                        // another program reads the same tty every byte is
                        // a race — so the thread stops HERE and waits to be
                        // woken.
                        let lent = matches!(
                            &event,
                            Event::Key(key) if classify(*key) == KeyAction::ExternalEditor
                        );
                        // Before Ctrl+G goes on to the loop: the editor
                        // starts only once the loop has the key, and
                        // nothing meant for the editor must be caught here.
                        if lent {
                            drop_queued();
                        }
                        if tx.blocking_send(event).is_err() {
                            break;
                        }
                        if lent && wake.recv().is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });
        Self { events, resume }
    }

    /// The next event; `None` ends the TUI (the tty is gone).
    pub(super) async fn recv(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    /// Take the terminal back once the editor is gone and raw mode is on
    /// again. Nothing is flushed on the way in: the parting query an editor
    /// makes for the terminal's attributes (nvim sends one) is answered in
    /// a form crossterm consumes itself, and discarding input here would
    /// only cost the keys a fast hand types as the editor closes. Replies
    /// crossterm does not recognize would arrive as keys; no editor
    /// measured sends one on exit — a corner recorded in §15.
    pub(super) fn resume(&self) {
        let _ = self.resume.send(());
    }
}

/// Everything crossterm has queued and, at that instant, still in the tty.
/// It parses a whole read at once: keys that shared Ctrl+G's chunk are
/// queued here, and they can never reach the editor — gg read them first.
/// Dropped, they do not land in the composer afterwards; an Enter among
/// them would send the saved draft sight unseen.
fn drop_queued() {
    while crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}
