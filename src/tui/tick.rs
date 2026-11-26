//! The spinner's clock: when its next frame is due.
//!
//! One deadline at a time, fixed the moment it is armed. The event loop
//! passes through its `select!` for every key, text delta and watch event;
//! a wait that started over on each pass would be pushed back by any
//! stream faster than the period, and the spinner would stand still
//! exactly while an answer was pouring in. A deadline does not move: the
//! next pass waits for the same instant, and the frame turns on time.
//!
//! Nothing here sleeps. The loop asks for the deadline and waits on it;
//! the clock only says when.

use std::time::Duration;

use tokio::time::Instant;

/// One frame every 600 ms: three beats per turn, long enough that a
/// reader can follow each one. The old 100 ms turn ran ten frames a
/// second, so the mark flipped faster than the eye could track.
pub const PERIOD: Duration = Duration::from_millis(600);

/// The spinner's next frame: armed while something is spinning, never
/// otherwise — an idle view does not wake on a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tick(Option<Instant>);

impl Tick {
    /// Disarmed: nothing is spinning.
    pub const fn new() -> Self {
        Self(None)
    }

    /// Bring the clock in line with the footer: armed a period from `now`
    /// while something is spinning, disarmed while nothing is. An armed
    /// deadline is left exactly where it was, whatever `now` is.
    pub fn sync(&mut self, now: Instant, spinning: bool) {
        match (spinning, self.0) {
            (false, _) => self.0 = None,
            (true, None) => self.0 = Some(now + PERIOD),
            (true, Some(_)) => {}
        }
    }

    /// When the next frame is due; `None` while nothing is spinning.
    pub fn deadline(&self) -> Option<Instant> {
        self.0
    }

    /// The deadline arrived: the frame after this one is a period from
    /// `now`.
    pub fn fired(&mut self, now: Instant) {
        self.0 = Some(now + PERIOD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn a_deadline_holds_still_while_other_events_arrive() {
        let mut tick = Tick::new();
        let start = Instant::now();
        tick.sync(start, true);
        let first = tick.deadline();
        assert_eq!(first, Some(start + PERIOD));

        // Every pass through the loop asks again; none may push it back.
        for ms in [10, 25, 40, 60, 90, 99] {
            tick.sync(start + ms * MS, true);
            assert_eq!(tick.deadline(), first, "{ms} ms in");
        }

        // Late by however long the loop was busy: the next frame is still
        // a full period from the last frame's due instant, so the spin
        // never runs fast to catch up.
        tick.fired(start + 140 * MS);
        assert_eq!(tick.deadline(), Some(start + PERIOD + 140 * MS));
    }

    #[test]
    fn an_idle_clock_is_never_armed() {
        let mut tick = Tick::new();
        let start = Instant::now();
        tick.sync(start, false);
        assert_eq!(tick.deadline(), None);

        // A run ends mid-period: the frame it had coming is cancelled.
        tick.sync(start, true);
        tick.sync(start + 50 * MS, false);
        assert_eq!(tick.deadline(), None);

        // The next run arms from its own start, not from the old deadline.
        tick.sync(start + 5 * PERIOD, true);
        assert_eq!(tick.deadline(), Some(start + 6 * PERIOD));
    }
}
