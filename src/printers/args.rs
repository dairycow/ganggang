//! One tool call's arguments, made legible for the `▸ name args` progress
//! line: the useful field per tool rather than a wall of JSON, collapsed
//! to one line and capped. The fallback is the raw JSON.

use super::one_line;

pub(super) fn format_tool_args(name: &str, args: &serde_json::Value) -> String {
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
                let off = args["offset"]
                    .as_u64()
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
        _ => args.to_string(),
    };
    one_line(&raw, 180)
}
