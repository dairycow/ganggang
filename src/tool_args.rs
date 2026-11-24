//! One tool call's arguments, made legible for a `▸ name args` line: the
//! useful field per tool rather than a wall of JSON, collapsed to one line
//! and capped. The fallback is the raw JSON.
//!
//! Both surfaces that show a call — the TUI transcript and print mode —
//! read this, so a `read` call reads the same wherever it is drawn.

/// The useful part of `args` for `name`, flattened to one line and capped
/// at `cap` chars with an ellipsis.
pub(crate) fn format_tool_args(name: &str, args: &serde_json::Value, cap: usize) -> String {
    let raw = match name {
        "bash" => match args["command"].as_str() {
            Some(cmd) => match args["timeout_secs"].as_u64() {
                Some(t) => format!("{cmd} (timeout {t}s)"),
                None => cmd.to_string(),
            },
            None => args.to_string(),
        },
        "read" => match args["path"].as_str() {
            Some(path) => {
                // A signed offset, so a tail read reads as `path:-30`.
                let off = args["offset"]
                    .as_i64()
                    .map(|o| format!(":{o}"))
                    .unwrap_or_default();
                let lim = args["limit"]
                    .as_u64()
                    .map(|l| format!("+{l}"))
                    .unwrap_or_default();
                format!("{path}{off}{lim}")
            }
            None => args.to_string(),
        },
        "edit" => match args["path"].as_str() {
            Some(path) => {
                let n = args["edits"].as_array().map_or(0, |a| a.len());
                format!("{path} ({n} edit(s))")
            }
            None => args.to_string(),
        },
        "write" => match args["path"].as_str() {
            Some(path) => {
                let chars = args["content"].as_str().map_or(0, |c| c.chars().count());
                format!("{path} ({chars} chars)")
            }
            None => args.to_string(),
        },
        "grep" => match args["pattern"].as_str() {
            Some(pat) => {
                let mut s = format!("{pat:?}");
                if let Some(p) = args["path"].as_str() {
                    s.push_str(&format!(" in {p}"));
                }
                if let Some(g) = args["glob"].as_str() {
                    s.push_str(&format!(" glob={g}"));
                }
                s
            }
            None => args.to_string(),
        },
        "find" => match args["pattern"].as_str() {
            Some(pat) => match args["path"].as_str() {
                Some(p) => format!("{pat} in {p}"),
                None => pat.to_string(),
            },
            None => args.to_string(),
        },
        "ls" => {
            if args.get("path").is_none() || args["path"].is_null() {
                ".".to_string()
            } else if let Some(p) = args["path"].as_str() {
                p.to_string()
            } else {
                args.to_string()
            }
        }
        "outline" => match args["path"].as_str() {
            Some(path) => match args["max_entries"].as_u64() {
                Some(n) => format!("{path} (max {n})"),
                None => path.to_string(),
            },
            None => args.to_string(),
        },
        _ => args.to_string(),
    };
    one_line(&raw, cap)
}

/// Collapse to one line and cap the length, marking a cut with `…`.
fn one_line(text: &str, cap: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= cap {
        flat
    } else {
        let cut: String = flat.chars().take(cap).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn each_tool_reads_as_its_useful_field() {
        assert_eq!(
            format_tool_args("bash", &json!({"command": "cargo test"}), 100),
            "cargo test"
        );
        assert_eq!(
            format_tool_args("read", &json!({"path": "src/main.rs", "offset": 10}), 100),
            "src/main.rs:10"
        );
        assert_eq!(
            format_tool_args("read", &json!({"path": "src/main.rs", "offset": -30}), 100),
            "src/main.rs:-30"
        );
        assert_eq!(
            format_tool_args("outline", &json!({"path": "src/main.rs"}), 100),
            "src/main.rs"
        );
        assert_eq!(
            format_tool_args(
                "outline",
                &json!({ "path": "src/main.rs", "max_entries": 40 }),
                100
            ),
            "src/main.rs (max 40)"
        );
        assert_eq!(
            format_tool_args("edit", &json!({"path": "a.rs", "edits": [{}, {}]}), 100),
            "a.rs (2 edit(s))"
        );
        assert_eq!(
            format_tool_args("grep", &json!({"pattern": "fn main", "path": "src"}), 100),
            "\"fn main\" in src"
        );
        assert_eq!(format_tool_args("ls", &json!({}), 100), ".");
    }

    #[test]
    fn long_arguments_cut_with_an_ellipsis() {
        let long = "x".repeat(200);
        let rendered = format_tool_args("bash", &json!({"command": long}), 60);
        assert!(rendered.starts_with("xxxx"));
        assert!(rendered.ends_with('…'));
        assert_eq!(rendered.chars().count(), 61);
    }

    #[test]
    fn unknown_tools_fall_back_to_raw_json_on_one_line() {
        assert_eq!(
            format_tool_args("echo", &json!({"text": "first"}), 100),
            "{\"text\":\"first\"}"
        );
    }
}
