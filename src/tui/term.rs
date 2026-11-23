//! Terminal setup/teardown for the TUI: raw mode, the alternate screen, and
//! bracketed paste in — everything restored on the way out, including
//! panics, which would otherwise leave the user's terminal unusable — and
//! put back as it was found once a program the terminal was lent to is done
//! ([`lend`], [`reclaim`], [`settle`]).

use std::io::stdout;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use anyhow::Context;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    KeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// The tty as the shell left it, in the form `stty` prints with `-g` and
/// takes back verbatim. Recorded by [`Guard::new`] before gg first goes raw.
static SHELL_MODE: OnceLock<String> = OnceLock::new();

/// Enter raw mode, the alternate screen, bracketed-paste mode, and the
/// terminal's enhanced-key reporting (see [`request_enhanced_keys`]). Call
/// [`restore`] on the way out, no matter how the way out happens.
pub fn enter() -> anyhow::Result<()> {
    enable_raw_mode().context("entering raw mode")?;
    if let Err(e) = execute!(stdout(), EnterAlternateScreen, EnableBracketedPaste) {
        // Raw mode is already on — leave the terminal as we found it
        // instead of exiting with the keyboard still raw.
        let _ = disable_raw_mode();
        return Err(anyhow::Error::from(e).context("entering alternate screen"));
    }
    request_enhanced_keys();
    Ok(())
}

/// Ask for modified keys, so Shift+Enter is distinguishable. tmux ignores
/// the kitty request (crossterm's own) but understands xterm's
/// modifyOtherKeys; with `extended-keys on` it then answers in CSI-u form,
/// which crossterm parses. Direct terminals get the kitty flags. Both are
/// switched off again by [`restore`] and [`reset_modes`]; a terminal that
/// knows neither ignores the sequence.
fn request_enhanced_keys() {
    if std::env::var_os("TMUX").is_some() {
        let _ = execute!(stdout(), Print("\x1b[>4;1m"));
    } else {
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
}

pub fn restore() {
    // Best effort both ways: if raw mode is somehow already off, the
    // alternate screen still needs leaving. The keyboard modes gg asked
    // for are popped here too — an editor is handed a plain terminal, and
    // the shell gets one back (a tmux pane left in an extended key mode
    // reports modified keys the shell never asked for).
    let _ = execute!(
        stdout(),
        DisableBracketedPaste,
        LeaveAlternateScreen,
        Print("\x1b[<u\x1b[>4;0m")
    );
    let _ = disable_raw_mode();
}

/// Lend the terminal to another program: [`restore`]. It fails only when
/// the tty mode was never recorded — then an editor that crashes cannot be
/// undone, which is worth a warning before it happens. crossterm keeps a
/// single "mode before raw" and forgets it in `restore`; the recorded copy
/// is what [`reclaim`] and [`settle`] put back.
pub fn lend() -> anyhow::Result<()> {
    restore();
    if SHELL_MODE.get().is_none() {
        anyhow::bail!(
            "the terminal's mode could not be recorded, so gg cannot undo an editor \
             that crashes — is `stty` installed?"
        );
    }
    Ok(())
}

/// Take the terminal back from a program that may not have cleaned up
/// after itself — a killed editor leaves raw mode, mouse reporting,
/// keypad and key-modification modes on. The tty goes back to the shell's
/// mode first, so that [`enter`] remembers that mode rather than the
/// borrower's. Returns what could not be put right, for the user to see.
pub fn reclaim() -> anyhow::Result<Option<String>> {
    let not_put_right = put_tty_back();
    reset_modes()?;
    enter()?;
    Ok(not_put_right)
}

/// gg is leaving with the terminal still lent (the borrower has just been
/// ended): the tty and the terminal's modes back, for gg's own exit to
/// build on. Returns what could not be put right.
pub fn settle() -> Option<String> {
    let not_put_right = put_tty_back();
    match reset_modes() {
        Ok(()) => not_put_right,
        Err(e) => Some(format!("could not reset the terminal's modes ({e})")),
    }
}

/// The tty back to the shell's mode. Nothing to do when it was never
/// recorded — [`lend`] said so at the time.
fn put_tty_back() -> Option<String> {
    let mode = SHELL_MODE.get()?;
    match stty(&[mode]) {
        Ok(_) => None,
        Err(e) => Some(format!(
            "could not restore the terminal mode ({e:#}); if keys misbehave, run `stty sane`"
        )),
    }
}

/// Switch off what an editor switches on. These modes live in the
/// terminal, not the tty: nothing undoes them for a program that dies, and
/// gg's own exit only undoes its own. No-ops after a clean exit.
pub fn reset_modes() -> anyhow::Result<()> {
    execute!(
        stdout(),
        DisableMouseCapture,
        DisableFocusChange,
        // Keyboard-enhancement flags popped at every level (a borrower may
        // have pushed more than one). Then synchronized output off (left
        // on, the terminal holds every frame), cursor keys and keypad back
        // to their normal sequences (left on, the numpad types nothing),
        // key modification off (left on, Ctrl+letter keys arrive altered or
        // not at all), autowrap on, the scrolling region and origin mode
        // reset, colors and attributes reset.
        Print(concat!(
            "\x1b[<u\x1b[?2026l\x1b[?1l\x1b>\x1b[>4;0m",
            "\x1b[?7h\x1b[39m\x1b[49m\x1b[0m\x1b[r\x1b[?6l"
        ))
    )
    .context("resetting terminal modes")
}

/// Run `stty` against the tty on stdin. A subprocess rather than a libc
/// call: §01 bans `unsafe` and the crate has no libc dependency — the same
/// trade the bash tool makes with `/bin/kill`.
fn stty(args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .output()
        .context("running stty")?;
    if !output.status.success() {
        // Not the arguments: the one that restores a mode is the whole
        // mode, a line of digits the user has no use for.
        anyhow::bail!(
            "stty {} failed: {}",
            if args.first() == Some(&"-g") {
                "-g"
            } else {
                "with the saved mode"
            },
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The screen gg draws on.
pub type Screen = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// A full repaint without asking the terminal anything. `Terminal::clear`
/// queries the cursor position and blocks until the terminal answers —
/// which not every terminal does, and a timeout there would end the
/// session. A resize to the current size clears the screen and resets the
/// diff buffer: after a program gg lent the terminal to, the renderer must
/// not trust what it last drew.
pub fn repaint(screen: &mut Screen) -> anyhow::Result<()> {
    let size = screen.size().context("sizing the terminal")?;
    screen
        .resize(ratatui::layout::Rect::new(0, 0, size.width, size.height))
        .context("clearing the screen")
}

/// RAII guard so early returns and unwinding both restore the terminal.
pub struct Guard;

impl Guard {
    /// Enter and install the panic hook. The hook restores BEFORE the
    /// default hook prints, so the panic message lands on a clean screen.
    pub fn new() -> anyhow::Result<Self> {
        // Before the first `enter`: the one moment the tty is known to be
        // the shell's rather than a borrower's. Not recordable (no `stty`)
        // is not fatal here — `lend` says so when it matters.
        if let Ok(mode) = stty(&["-g"]) {
            let _ = SHELL_MODE.set(mode);
        }
        enter()?;
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // With the terminal lent, this is the last chance to put the
            // tty and the terminal's modes back: the loop will not.
            let _ = settle();
            restore();
            previous(info);
        }));
        Ok(Self)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        restore();
    }
}
