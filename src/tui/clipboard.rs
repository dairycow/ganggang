//! Clipboard copy without dependencies (§15's Ctrl+X): the OSC 52
//! escape sequence, terminal-native and SSH-safe. Terminals without
//! clipboard support ignore it silently.

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
