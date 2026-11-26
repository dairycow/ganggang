//! One tool call's arguments, made legible for a `▸ name args` line: the
//! useful field per tool rather than a wall of JSON, collapsed to one line
//! and capped. The fallback is the raw JSON.
//!
//! Both surfaces that show a call — the TUI transcript and print mode —
//! read this, so a `read` call reads the same wherever it is drawn.

/// The useful part of `args` for `name`: what the tool acted on, verbatim,
/// and the facts beside it (a count, a limit, a timeout). One line for
/// both, as `▸ name subject · fact · fact`: the subject is capped at `cap`
/// chars with an ellipsis and the facts come after the cut, so a long path
/// or command never costs the line its count.
pub(crate) fn format_tool_args(name: &str, args: &serde_json::Value, cap: usize) -> String {
    let (subject, facts) = describe(name, args);
    let mut line = one_line(&subject, cap);
    if !facts.is_empty() {
        line.push_str(" · ");
        line.push_str(&facts.join(" · "));
    }
    line
}

/// The subject and the facts of one call, before either is capped. An
/// unknown tool, or a call without the field it is read by, falls back to
/// the raw JSON and no facts.
fn describe(name: &str, args: &serde_json::Value) -> (String, Vec<String>) {
    let facts = |facts: &[String]| facts.iter().filter(|f| !f.is_empty()).cloned().collect();
    let subject = match name {
        "bash" => match args["command"].as_str() {
            Some(cmd) => {
                let timeout = args["timeout_secs"]
                    .as_u64()
                    .map(|t| format!("timeout {t}s"))
                    .unwrap_or_default();
                return (cmd.to_string(), facts(&[timeout]));
            }
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
                return (path.to_string(), facts(&[count(n, "edit")]));
            }
            None => args.to_string(),
        },
        "write" => match args["path"].as_str() {
            Some(path) => {
                let chars = args["content"].as_str().map_or(0, |c| c.chars().count());
                return (path.to_string(), facts(&[format!("{chars} chars")]));
            }
            None => args.to_string(),
        },
        "grep" => match args["pattern"].as_str() {
            Some(pat) => {
                let mut s = format!("{pat:?}");
                if let Some(p) = args["path"].as_str() {
                    s.push_str(&format!(" in {p}"));
                }
                let glob = args["glob"]
                    .as_str()
                    .map(|g| format!("glob {g}"))
                    .unwrap_or_default();
                return (s, facts(&[glob]));
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
            Some(path) => {
                let max = args["max_entries"]
                    .as_u64()
                    .map(|n| format!("max {n}"))
                    .unwrap_or_default();
                return (path.to_string(), facts(&[max]));
            }
            None => args.to_string(),
        },
        _ => args.to_string(),
    };
    (subject, Vec::new())
}

/// `1 edit`, `2 edits`: the count with a real plural.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("{n} {noun}")
    } else {
        format!("{n} {noun}s")
    }
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
            "src/main.rs · max 40"
        );
        assert_eq!(
            format_tool_args("edit", &json!({"path": "a.rs", "edits": [{}, {}]}), 100),
            "a.rs · 2 edits"
        );
        assert_eq!(
            format_tool_args("edit", &json!({"path": "a.rs", "edits": [{}]}), 100),
            "a.rs · 1 edit"
        );
        assert_eq!(
            format_tool_args(
                "bash",
                &json!({"command": "sleep 5", "timeout_secs": 90}),
                100
            ),
            "sleep 5 · timeout 90s"
        );
        assert_eq!(
            format_tool_args(
                "grep",
                &json!({"pattern": "fn main", "path": "src", "glob": "*.rs"}),
                100
            ),
            "\"fn main\" in src · glob *.rs"
        );
        assert_eq!(format_tool_args("ls", &json!({}), 100), ".");
    }

    /// The cap is the subject's: a fact survives the cut it would once
    /// have been lost to.
    #[test]
    fn facts_survive_a_capped_subject() {
        let long = format!("src/{}rs", "x".repeat(80));
        assert_eq!(
            format_tool_args("edit", &json!({"path": long, "edits": [{}]}), 20),
            "src/xxxxxxxxxxxxxxxx… · 1 edit"
        );
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
