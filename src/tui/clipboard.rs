//! Clipboard copy: the desktop's own clipboard tool when one is
//! installed, and the OSC 52 escape sequence through the terminal for
//! everything else. Many terminals ignore OSC 52 (GNOME Terminal among
//! them), so a copy the terminal alone has to carry is a copy the user
//! may never get.

use std::io::Write;
use std::process::{Command, Stdio};

/// The desktop clipboard tool a copy goes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Helper {
    /// Wayland: `wl-copy` takes the text on stdin.
    WlCopy,
    /// X11: `xclip -selection clipboard`.
    Xclip,
    /// X11: `xsel --clipboard --input`.
    Xsel,
    /// macOS: `pbcopy`.
    Pbcopy,
}

impl Helper {
    /// The program to run.
    pub fn program(self) -> &'static str {
        match self {
            Helper::WlCopy => "wl-copy",
            Helper::Xclip => "xclip",
            Helper::Xsel => "xsel",
            Helper::Pbcopy => "pbcopy",
        }
    }

    /// The arguments that make the program take the clipboard's text on
    /// stdin.
    pub fn args(self) -> &'static [&'static str] {
        match self {
            Helper::WlCopy | Helper::Pbcopy => &[],
            Helper::Xclip => &["-selection", "clipboard"],
            Helper::Xsel => &["--clipboard", "--input"],
        }
    }
}

/// The machine as the helper choice reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Desktop {
    pub platform: Platform,
    /// `WAYLAND_DISPLAY` is set.
    pub wayland: bool,
    /// `DISPLAY` is set.
    pub x11: bool,
}

/// What the process runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Other,
}

/// The helper for `desktop`, when one is installed. A Wayland session
/// may also carry an X11 `DISPLAY`, but its clipboard is the Wayland
/// one, so `wl-copy` wins when both are there; `xclip` wins over `xsel`
/// when a machine has both. `found` answers whether a program is on
/// PATH.
pub fn helper(desktop: Desktop, found: impl Fn(&str) -> bool) -> Option<Helper> {
    match desktop.platform {
        Platform::MacOs => found("pbcopy").then_some(Helper::Pbcopy),
        Platform::Linux if desktop.wayland && found("wl-copy") => Some(Helper::WlCopy),
        Platform::Linux if desktop.x11 && found("xclip") => Some(Helper::Xclip),
        Platform::Linux if desktop.x11 && found("xsel") => Some(Helper::Xsel),
        _ => None,
    }
}

/// This machine's desktop, read from the environment.
pub fn desktop() -> Desktop {
    Desktop {
        platform: if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        },
        wayland: std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()),
        x11: std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()),
    }
}

/// Copy `text` to the clipboard: through the desktop's clipboard tool
/// when one is installed, else through the terminal's OSC 52 sequence.
/// The helper takes the text whole; an OSC 52 payload is capped below.
/// A helper that is installed but fails (a display it cannot reach)
/// falls back to the terminal rather than failing the copy: the
/// terminal may still have a clipboard.
pub fn copy(text: &str) -> std::io::Result<()> {
    if let Some(helper) = helper(desktop(), on_path)
        && write_to(helper.program(), helper.args(), text).is_ok()
    {
        return Ok(());
    }
    write_osc52(text)
}

/// Send `text` to `program`'s stdin and wait for it to exit: every helper
/// here reads the text, sets the clipboard and exits, leaving a child
/// behind to serve the selection (`wl-copy` and `xclip` do; `pbcopy`
/// sets it and goes).
fn write_to(program: &str, args: &[&str], text: &str) -> std::io::Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    // Dropping the pipe closes it: the helper reads that as the end of
    // the text.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "{program} exited with {status}"
        )));
    }
    Ok(())
}

/// Whether `program` is a file on PATH. A directory there is not looked
/// into, and a file that is not executable only costs the fallback to
/// the terminal.
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// Copy through the terminal: the OSC 52 sequence, written to
/// `/dev/tty`, the terminal's own file, so it works even when the
/// process's file descriptors point elsewhere. A terminal without
/// clipboard support ignores the sequence; one without a tty is an
/// error the caller reports.
fn write_osc52(text: &str) -> std::io::Result<()> {
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    tty.write_all(osc52_sequence(text).as_bytes())
}

/// The OSC 52 clipboard sequence for `text` (spec §15: Ctrl+X copies the
/// last answer). Terminal-native, zero dependencies, works over SSH;
/// terminals without clipboard support ignore it.
pub fn osc52_sequence(text: &str) -> String {
    // Cap well under common terminal limits; a clipboard payload larger
    // than this is not an "answer" anymore. The cap floors to a char
    // boundary — splitting a multibyte char would paste mojibake.
    let mut end = text.len().min(70_000);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("\x1b]52;c;{}\x07", base64(&text.as_bytes()[..end]))
}

/// Standard base64 (RFC 4648, padded) — OSC 52 payloads require it and
/// the no-new-dependencies rule (§06) says hand-roll this much.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let triple = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux(wayland: bool, x11: bool) -> Desktop {
        Desktop {
            platform: Platform::Linux,
            wayland,
            x11,
        }
    }

    #[test]
    fn base64_matches_the_reference_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_base64_in_the_escape_sequence() {
        let seq = osc52_sequence("hi");
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with("\x07"));
        assert!(seq.contains(&base64(b"hi")));
    }

    #[test]
    fn osc52_caps_huge_payloads_on_a_char_boundary() {
        let huge = "x".repeat(200_000);
        let seq = osc52_sequence(&huge);
        // 70_000 raw bytes encode to ≤ 93_334 chars; the whole sequence
        // stays well under terminal limits.
        assert!(seq.len() < 100_000);
        // The cap floors to a char boundary: multibyte tails survive.
        let multibyte = "é".repeat(50_000); // 100_000 bytes, cap lands mid-char
        let seq = osc52_sequence(&multibyte);
        let payload = seq
            .trim_start_matches("\x1b]52;c;")
            .trim_end_matches("\x07");
        let decoded_bytes = base64_decode(payload);
        let text = String::from_utf8_lossy(&decoded_bytes).to_string();
        assert!(
            text.ends_with('é'),
            "no mojibake at the cut: …{}",
            &text[text.len() - 4..]
        );
        assert!(text.chars().all(|c| c == 'é'));
    }

    /// The helper's program and arguments: each reads the clipboard's
    /// text on stdin, which is what `write_to` feeds it.
    #[test]
    fn every_helper_names_its_program_and_stdin_arguments() {
        assert_eq!(Helper::WlCopy.program(), "wl-copy");
        assert_eq!(Helper::WlCopy.args(), [] as [&str; 0]);
        assert_eq!(Helper::Pbcopy.program(), "pbcopy");
        assert_eq!(Helper::Pbcopy.args(), [] as [&str; 0]);
        assert_eq!(Helper::Xclip.program(), "xclip");
        assert_eq!(Helper::Xclip.args(), ["-selection", "clipboard"]);
        assert_eq!(Helper::Xsel.program(), "xsel");
        assert_eq!(Helper::Xsel.args(), ["--clipboard", "--input"]);
    }

    /// Wayland's tool first, then X11's, then macOS's; nothing when the
    /// display or the program is missing.
    #[test]
    fn the_helper_is_the_desktop_s_first_installed_tool() {
        let everything = |_: &str| true;
        let nothing = |_: &str| false;
        assert_eq!(helper(linux(true, false), everything), Some(Helper::WlCopy));
        assert_eq!(helper(linux(true, true), everything), Some(Helper::WlCopy));
        assert_eq!(helper(linux(false, true), everything), Some(Helper::Xclip));
        assert_eq!(
            helper(linux(false, true), |program| program != "xclip"),
            Some(Helper::Xsel),
            "xsel is the second X11 tool"
        );
        assert_eq!(helper(linux(false, true), nothing), None);
        assert_eq!(helper(linux(true, false), nothing), None);
        let mac = Desktop {
            platform: Platform::MacOs,
            wayland: false,
            x11: false,
        };
        assert_eq!(helper(mac, everything), Some(Helper::Pbcopy));
        let other = Desktop {
            platform: Platform::Other,
            ..mac
        };
        assert_eq!(helper(other, everything), None, "OSC 52 is the way there");
    }

    /// The helper gets the text on stdin, whole and unaltered.
    #[test]
    fn write_to_hands_the_text_to_the_program_on_stdin() {
        let path = std::env::temp_dir().join(format!("gg-clip-{}", std::process::id()));
        let script = format!("cat > '{}'", path.display());
        write_to("sh", &["-c", &script], "two\nlines\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two\nlines\n");
        let _ = std::fs::remove_file(&path);
    }

    /// A program that is not there, and one that exits non-zero, are both
    /// errors the copy falls back from.
    #[test]
    fn write_to_reports_a_missing_program_and_a_failed_one() {
        let missing = write_to("gg-no-such-clipboard-helper", &[], "x").unwrap_err();
        assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);
        let failed = write_to("sh", &["-c", "exit 3"], "x").unwrap_err();
        assert!(
            failed.to_string().contains("exited with"),
            "the exit status names the failure: {failed}"
        );
    }

    fn base64_decode(text: &str) -> Vec<u8> {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let body: Vec<u8> = text.bytes().filter(|b| *b != b'=').collect();
        for chunk in body.chunks(4) {
            let idx: Vec<u8> = chunk
                .iter()
                .map(|b| ALPHABET.iter().position(|a| a == b).unwrap_or(0) as u8)
                .collect();
            let triple = ((idx[0] as u32) << 18)
                | (idx.get(1).copied().unwrap_or(0) as u32) << 12
                | (idx.get(2).copied().unwrap_or(0) as u32) << 6
                | idx.get(3).copied().unwrap_or(0) as u32;
            out.push((triple >> 16) as u8);
            if chunk.len() > 2 {
                out.push((triple >> 8) as u8);
            }
            if chunk.len() > 3 {
                out.push(triple as u8);
            }
        }
        out
    }
}
