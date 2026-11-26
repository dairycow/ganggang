//! The run's wall-clock budget (`--max-seconds`): what the loop does at
//! each of the budget's two lines, decided from the clock alone.
//!
//! The loop reads the clock before each request and hands the figures to
//! [`step`]. At 80% of the budget the model is nudged once to wrap up
//! ([`Step::Soft`]); at the budget itself the request carries the final
//! notice instead ([`Step::Final`]), and the run stops when that turn is
//! over, whether the model answered or asked for tools.

/// The first line of the budget: from here the model is nudged to wrap up.
const SOFT_NUM: u64 = 4;
const SOFT_DEN: u64 = 5;

/// What the loop does about the time budget before one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    /// Nothing to say yet.
    None,
    /// The 80% line: attach [`SOFT_NOTICE`] once.
    Soft,
    /// The budget is spent: attach [`FINAL_NOTICE`], then stop the run.
    Final,
}

/// The notice for the 80% line.
const SOFT_NOTICE: &str = "[system notice: you have used {elapsed}s of a {limit}s time budget. \
Wrap up your work and return your final response soon without starting new \
exploration.]";

/// The notice for a request made after the budget is spent.
const FINAL_NOTICE: &str = "[system notice: time budget ({limit}s) reached. Do not call any \
tools; provide your final response now.]";

/// `Some` when the run has a budget: the whole seconds gone and the
/// limit. `elapsed` is `None` until the run has started.
pub(super) fn clock(elapsed: Option<u64>, limit: Option<u64>) -> Option<(u64, u64)> {
    Some((elapsed?, limit.filter(|limit| *limit >= 1)?))
}

/// The decision for a request `elapsed` seconds into a budget of `limit`.
/// The soft line needs a budget of at least 5 s for 80% of it to be a
/// whole second before the end; each line speaks once per run.
pub(super) fn step(elapsed: u64, limit: u64, soft_nudged: bool, final_turn: bool) -> Step {
    if final_turn {
        return Step::None;
    }
    if elapsed >= limit {
        Step::Final
    } else if !soft_nudged && limit >= SOFT_DEN && elapsed * SOFT_DEN >= limit * SOFT_NUM {
        Step::Soft
    } else {
        Step::None
    }
}

/// The notice that rides the request for `step`, if it has one.
pub(super) fn notice(step: Step, elapsed: u64, limit: u64) -> Option<String> {
    match step {
        Step::None => None,
        Step::Soft => Some(
            SOFT_NOTICE
                .replace("{elapsed}", &elapsed.to_string())
                .replace("{limit}", &limit.to_string()),
        ),
        Step::Final => Some(FINAL_NOTICE.replace("{limit}", &limit.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_soft_line_is_80_pct_and_speaks_once() {
        assert_eq!(step(0, 100, false, false), Step::None);
        assert_eq!(step(79, 100, false, false), Step::None);
        assert_eq!(step(80, 100, false, false), Step::Soft);
        assert_eq!(step(90, 100, true, false), Step::None);
        // A short budget has no soft line: 1 of 5 s is the final turn.
        for limit in 1..5 {
            assert_eq!(step(0, limit, false, false), Step::None, "{limit}");
        }
    }

    #[test]
    fn the_budget_itself_is_the_final_turn() {
        assert_eq!(step(100, 100, false, false), Step::Final);
        assert_eq!(step(100, 100, false, true), Step::None);
        assert_eq!(step(5, 5, false, false), Step::Final);
        assert_eq!(step(3, 5, false, false), Step::None);
    }

    #[test]
    fn the_clock_needs_a_budget_and_a_start() {
        assert_eq!(clock(Some(3), None), None);
        assert_eq!(clock(Some(3), Some(0)), None, "0 disables");
        assert_eq!(clock(None, Some(30)), None);
        assert_eq!(clock(Some(3), Some(30)), Some((3, 30)));
    }

    #[test]
    fn notices_name_the_budget() {
        assert_eq!(notice(Step::None, 1, 2), None);
        assert_eq!(
            notice(Step::Soft, 80, 100).as_deref(),
            Some(
                "[system notice: you have used 80s of a 100s time budget. Wrap up your work \
and return your final response soon without starting new exploration.]"
            )
        );
        assert_eq!(
            notice(Step::Final, 120, 100).as_deref(),
            Some(
                "[system notice: time budget (100s) reached. Do not call any tools; provide \
your final response now.]"
            )
        );
    }
}
