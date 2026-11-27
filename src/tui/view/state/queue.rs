//! The queue of prompts: what Enter holds back while a run works, and
//! what happens to it when the run ends. A plain prompt submitted while
//! the run is busy joins the back of the line; when a run completes
//! normally the first item starts as a run of its own; a stopped or
//! failed run holds everything until the user asks. The queue lives on
//! [`State`](super::State) beside the rest of the view, and the event
//! loop and [`apply`](crate::tui::agent_task::apply) only call it.

use std::collections::VecDeque;

use crate::tui::commands::{Submission, classify_submission};

/// The queue itself: plain prompts waiting for the run in flight to end,
/// and whether sending is held after a stop or a failure. First in,
/// first out; ↑ takes from the back.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Queue {
    items: VecDeque<String>,
    held: bool,
}

impl Queue {
    /// Join the back of the line.
    fn push(&mut self, text: String) {
        self.items.push_back(text);
    }

    /// The first item, off to its run.
    fn pop_first(&mut self) -> Option<String> {
        let first = self.items.pop_front();
        if self.items.is_empty() {
            self.held = false;
        }
        first
    }

    /// The newest item, back to the composer.
    fn pop_last(&mut self) -> Option<String> {
        let last = self.items.pop_back();
        if self.items.is_empty() {
            self.held = false;
        }
        last
    }

    /// How many items wait.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing waits.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether a stop or a failure is holding the line.
    pub fn held(&self) -> bool {
        self.held
    }

    fn set_held(&mut self, held: bool) {
        self.held = held;
    }

    /// Empty the queue and lift any hold: what a new session starts with.
    /// The new session itself (in [`log`](super::log)) is the caller.
    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.held = false;
    }
}

impl crate::tui::view::State {
    /// What Enter does while a run is busy: a plain prompt queues and
    /// leaves the composer (history keeps it, the transcript names its
    /// place in line), while a slash command, a shell line, an empty
    /// composer, or a press while the run is stopping keeps today's note
    /// and the text where it was typed. `true` means the prompt queued.
    pub fn busy_submit(&mut self, text: &str, stopping: bool) -> bool {
        let Submission::Plain(prompt) = classify_submission(text) else {
            return false;
        };
        if stopping || prompt.trim().is_empty() {
            return false;
        }
        self.editor.take_submission();
        self.history.record(text);
        self.queue_prompt(&prompt);
        true
    }

    /// Queue one plain prompt: the transcript names its place in line,
    /// the queue takes the text. (History is the submitter's business:
    /// [`busy_submit`](Self::busy_submit) records before calling here.)
    pub fn queue_prompt(&mut self, prompt: &str) {
        self.queue.push(prompt.to_string());
        let place = self.queue.len();
        let first = prompt.lines().next().unwrap_or_default();
        self.push_info(&format!("queued {place}: {first}"));
    }

    /// How many prompts wait in the queue.
    pub fn queued_count(&self) -> usize {
        self.queue.len()
    }

    /// Whether a stop or a failure is holding the queue.
    pub fn queue_held(&self) -> bool {
        self.queue.held()
    }

    /// ↑ on an empty composer: the newest queued prompt comes back to
    /// the composer and out of the queue. An empty queue leaves the
    /// composer alone, so history recall keeps its turn.
    pub fn queue_take_back(&mut self) -> Option<String> {
        let item = self.queue.pop_last()?;
        self.editor.set_text(&item);
        Some(item)
    }

    /// Enter on an empty composer while the queue is held: the first
    /// item becomes the run and the hold lifts. A typed draft comes
    /// first, though: with text in the composer this does nothing, so
    /// the submit path sends the draft as an ordinary run and the hold
    /// stays in place.
    pub fn queue_send_first(&mut self) -> Option<String> {
        if !self.queue.held() || !self.editor.text().trim().is_empty() {
            return None;
        }
        let item = self.queue.pop_first()?;
        self.queue.set_held(false);
        Some(item)
    }

    /// A run has just ended normally: the prompt to start next, when the
    /// queue may send. One item per run end, so the decision to stop or
    /// quit applies between items.
    pub fn queue_next_run(&mut self) -> Option<String> {
        if self.queue.held() {
            return None;
        }
        self.queue.pop_first()
    }

    /// The run was stopped, or failed: the queue is held, nothing sends
    /// until the user asks. One muted line says how many wait.
    pub fn queue_hold_after_stop(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.queue.set_held(true);
        let count = self.queue.len();
        self.push_info(&format!("{count} queued: Enter sends the next"));
    }

    /// A run failed: the first queued prompt returns to the composer so
    /// it can be edited before it is sent again, and the rest stay
    /// queued and held. One muted line says so.
    pub fn queue_run_failed(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        if let Some(first) = self.queue.pop_first() {
            self.editor.set_text(&first);
        }
        let rest = self.queue.len();
        if rest == 0 {
            self.push_info("the queued message is back in the composer");
        } else {
            self.queue.set_held(true);
            self.push_info(&format!(
                "the first queued message is back in the composer; {rest} held"
            ));
        }
    }

    /// Whether the run that has just reported its end was stopped by the
    /// user: the run's own `RunAborted` event sets the flag, and the
    /// stream forwards it before the run's outcome arrives.
    pub fn run_was_stopped(&self) -> bool {
        self.run_stopped
    }
}

#[cfg(test)]
mod tests {
    use super::Queue;

    #[test]
    fn the_queue_is_first_in_first_out() {
        let mut queue = Queue::default();
        assert!(queue.is_empty());
        queue.push("first".into());
        queue.push("second".into());
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop_first().as_deref(), Some("first"));
        assert_eq!(queue.pop_first().as_deref(), Some("second"));
        assert_eq!(queue.pop_first(), None);
        assert!(queue.is_empty());
    }

    #[test]
    fn take_back_removes_the_newest_first() {
        let mut queue = Queue::default();
        queue.push("first".into());
        queue.push("second".into());
        assert_eq!(queue.pop_last().as_deref(), Some("second"));
        assert_eq!(queue.pop_last().as_deref(), Some("first"));
        assert_eq!(queue.pop_last(), None);
    }

    #[test]
    fn a_hold_stops_the_line_until_it_lifts() {
        let mut queue = Queue::default();
        queue.push("first".into());
        assert!(!queue.held());
        queue.set_held(true);
        assert!(queue.held());
        // The hold is about the waiting items: the last one leaving
        // takes the hold with it.
        assert_eq!(queue.pop_first().as_deref(), Some("first"));
        assert!(!queue.held(), "an empty queue is not held");

        queue.push("second".into());
        queue.set_held(true);
        assert_eq!(queue.pop_last().as_deref(), Some("second"));
        assert!(!queue.held());

        // A new session clears both the items and the hold.
        queue.push("third".into());
        queue.set_held(true);
        queue.clear();
        assert!(queue.is_empty() && !queue.held());
        assert_eq!(queue, Queue::default());
    }
}
