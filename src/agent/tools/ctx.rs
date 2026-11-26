//! The tool context: what a tool sees of the run that invoked it.
//!
//! Tools are plain values in and out; a tool that merely runs an action
//! needs nothing beyond its arguments and the stop request. A tool that
//! *observes* the run (the `task` tool, which streams a nested agent's
//! events into the parent's event channel) needs the call's id and the
//! sender. `ToolCtx` carries all three; the [`Tool::execute_with`] default
//! throws the context away, so ordinary tools keep the bare [`execute`]
//! surface.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Boxed future for `dyn` tool execution. `async fn` in traits is stable but
/// not dyn-compatible, and this alias costs less than an `async-trait`
/// dependency.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The user's stop request, handed to every tool call.
///
/// An `Arc<AtomicBool>` wrapper over the loop's abort flag: a tool whose
/// action can run a long time (a shell command, a directory walk) polls it
/// and stops, so Esc is not dead until the next call boundary. The check is
/// one acquire load — it pairs with the loop's release store, so a tool on
/// another thread sees a stop request as soon as it is made — and it is
/// cheap enough to repeat inside a walk.
#[derive(Clone)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new(flag: Arc<AtomicBool>) -> Self {
        Self(flag)
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// A flag nothing ever sets, for a caller that must supply a [`Cancel`]
    /// but has no cancellation path of its own (the TUI's `!` shell).
    pub fn none() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }
}

/// What a tool needs beyond its arguments: the run's stop request, the
/// parent's event stream, and the id of the call that is running.
#[derive(Clone)]
pub struct ToolCtx {
    pub cancel: Cancel,
    pub events: Option<tokio::sync::mpsc::Sender<crate::events::AgentEvent>>,
    pub call_id: String,
    pub session_id: Option<String>,
}

impl ToolCtx {
    /// A context for a caller that has no event stream and no call to
    /// identify (tests, the TUI's `!` shell): the stop request alone.
    pub fn none(cancel: &Cancel) -> Self {
        Self {
            cancel: cancel.clone(),
            events: None,
            call_id: String::new(),
            session_id: None,
        }
    }
}
