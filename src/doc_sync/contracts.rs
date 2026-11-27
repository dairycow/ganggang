//! Contracts between the hand-written guide and the code: links resolve,
//! every page is reachable, documented TOML really parses, and every
//! environment variable, event, hook, and key the code knows is named.

use std::collections::BTreeSet;
use std::path::Path;

use super::{DOCS_URL, markdown_files, read, root};

/// `(line, in_fence)` pairs: the lines of `text` with fenced-code state.
fn prose_lines(text: &str) -> Vec<&str> {
    let mut in_fence = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if !in_fence {
            out.push(line);
        }
    }
    out
}

/// Targets of every `](target)` link in `line`.
fn link_targets(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        if let Some(end) = rest.find(')') {
            out.push(&rest[..end]);
            rest = &rest[end..];
        }
    }
    out
}

#[test]
fn summary_covers_all_pages_and_links_resolve() {
    let src = root().join("docs/src");
    let summary = read("docs/src/SUMMARY.md");
    let mut listed = BTreeSet::new();
    for line in summary.lines() {
        for target in link_targets(line) {
            assert!(
                src.join(target).is_file(),
                "SUMMARY.md links a missing page: {target}"
            );
            listed.insert(src.join(target));
        }
    }
    for page in markdown_files(&src) {
        if page.file_name().is_some_and(|n| n == "SUMMARY.md") {
            continue;
        }
        assert!(
            listed.contains(&page),
            "{} is not listed in SUMMARY.md",
            page.display()
        );
        let text = std::fs::read_to_string(&page).expect("readable page");
        for line in prose_lines(&text) {
            for target in link_targets(line) {
                let target = target.split('#').next().unwrap_or_default();
                if target.is_empty() || target.contains("://") || target.starts_with("mailto:") {
                    continue;
                }
                let resolved = page.parent().expect("page has a parent").join(target);
                assert!(
                    resolved.exists(),
                    "{} links a missing file: {target}",
                    page.display()
                );
            }
        }
    }
}

/// Bodies of ```toml fences in `text`.
fn toml_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        match (&mut current, trimmed) {
            (None, t) if t.starts_with("```toml") => current = Some(String::new()),
            (Some(_), t) if t.starts_with("```") => blocks.extend(current.take()),
            (Some(buf), _) => {
                buf.push_str(line);
                buf.push('\n');
            }
            _ => {}
        }
    }
    blocks
}

#[test]
fn documented_toml_examples_parse_and_validate() {
    let mut files = markdown_files(&root().join("docs/src"));
    files.push(root().join("README.md"));
    let (mut hooks, mut configs) = (0, 0);
    for file in files {
        let text = std::fs::read_to_string(&file).expect("readable page");
        for block in toml_blocks(&text) {
            let first = block.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            if first.trim() == "# ~/.gg/hooks.toml" || block.contains("[[hooks]]") {
                let dir = tempfile::tempdir().expect("tempdir");
                let path = dir.path().join("hooks.toml");
                std::fs::write(&path, &block).expect("write hooks file");
                if let Err(e) = crate::agent::hooks::load_from(Some(&path), dir.path(), false) {
                    panic!(
                        "hooks example in {} is invalid: {e:#}\n{block}",
                        file.display()
                    );
                }
                hooks += 1;
            } else if matches!(
                first.trim(),
                "# ~/.gg/config.toml" | "# .gg/config.toml" | "# config.toml"
            ) {
                let merged =
                    crate::config::merge(crate::config::Config::default(), Some(&block), None, &[]);
                if let Err(e) = merged {
                    panic!(
                        "config example in {} is invalid: {e:#}\n{block}",
                        file.display()
                    );
                }
                configs += 1;
            }
        }
    }
    assert!(hooks >= 1, "no hooks.toml example was validated");
    assert!(configs >= 1, "no config.toml example was validated");
}

fn rust_sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "doc_sync" && name != "tests" {
                rust_sources(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" {
            out.push(path);
        }
    }
}

#[test]
fn env_vars_are_documented() {
    let mut files = Vec::new();
    rust_sources(&root().join("src"), &mut files);
    let mut vars = BTreeSet::new();
    for file in files {
        let text = std::fs::read_to_string(&file).expect("readable source");
        let mut rest = text.as_str();
        while let Some(at) = rest.find("GG_") {
            let word: String = rest[at..]
                .chars()
                .take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            rest = &rest[at + word.len()..];
            vars.insert(word.trim_end_matches('_').to_string());
        }
    }
    for p in crate::config::providers::builtin_providers().values() {
        vars.extend(p.api_key_env.clone());
    }
    assert!(
        vars.contains("GG_PROVIDER"),
        "the scan found nothing: {vars:?}"
    );
    let guide = read("docs/src/guide/configuration.md");
    for var in vars {
        assert!(
            guide.contains(&var),
            "{var} is not documented in guide/configuration.md"
        );
    }
}

#[test]
fn events_hooks_and_keys_are_exhaustively_documented() {
    use crate::agent::hooks::HookEvent;
    use crate::events::AgentEvent;
    use crate::tui::keys::KeyAction;

    // Exhaustive matches: a new variant fails to compile until it is named
    // here, and then the assertions below force it into the guide.
    let hook_name = |e: HookEvent| match e {
        HookEvent::PreTool | HookEvent::PostTool | HookEvent::RunEnd => e.as_str(),
    };
    let hooks = read("docs/src/guide/hooks.md");
    for e in [HookEvent::PreTool, HookEvent::PostTool, HookEvent::RunEnd] {
        assert!(
            hooks.contains(hook_name(e)),
            "hook event {} is undocumented",
            e.as_str()
        );
    }

    let event_type = |e: &AgentEvent| match e {
        AgentEvent::RunStarted
        | AgentEvent::TurnStarted
        | AgentEvent::AssistantText { .. }
        | AgentEvent::Thinking { .. }
        | AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolCallFinished { .. }
        | AgentEvent::TurnEnded { .. }
        | AgentEvent::RunEnded { .. }
        | AgentEvent::RunAborted
        | AgentEvent::Warned { .. } => serde_json::to_value(e).expect("serializable")["type"]
            .as_str()
            .expect("tagged")
            .to_string(),
    };
    let s = String::new;
    let samples = [
        AgentEvent::RunStarted,
        AgentEvent::TurnStarted,
        AgentEvent::AssistantText { delta: s() },
        AgentEvent::Thinking { delta: s() },
        AgentEvent::ToolCallStarted {
            name: s(),
            id: s(),
            args: serde_json::Value::Null,
        },
        AgentEvent::ToolCallFinished {
            name: s(),
            id: s(),
            output: s(),
            is_error: false,
        },
        AgentEvent::TurnEnded {
            stop_reason: s(),
            usage: None,
            timings: None,
            thinking_pct: None,
        },
        AgentEvent::RunEnded { turns: 0 },
        AgentEvent::RunAborted,
        AgentEvent::Warned { message: s() },
    ];
    let modes = read("docs/src/guide/modes.md");
    for sample in &samples {
        let name = event_type(sample);
        assert!(
            modes.contains(&name),
            "event {name} is undocumented in guide/modes.md"
        );
    }

    let keys_of = |a: KeyAction| -> &'static [&'static str] {
        match a {
            KeyAction::Insert(_) | KeyAction::Ignore => &[],
            KeyAction::Backspace => &["Backspace"],
            // Backticked: the Backspace row's prose says "Delete" too.
            KeyAction::Delete => &["`Delete`"],
            KeyAction::Newline => &["Alt+Enter", "Ctrl+J"],
            KeyAction::Send => &["Enter"],
            KeyAction::Abort => &["Esc"],
            KeyAction::Interrupt => &["Ctrl+C"],
            KeyAction::HistoryPrev | KeyAction::HistoryNext => &["Ctrl+P", "Ctrl+N"],
            KeyAction::ToggleExpand => &["Ctrl+O"],
            KeyAction::ToggleThinking => &["Ctrl+T"],
            KeyAction::CopyAnswer => &["Ctrl+X"],
            KeyAction::ScrollUp => &["PageUp"],
            KeyAction::ScrollDown => &["PageDown"],
            KeyAction::LineStart => &["Ctrl+A", "Home"],
            KeyAction::LineEnd => &["Ctrl+E", "End"],
            KeyAction::KillToEnd => &["Ctrl+K"],
            KeyAction::KillToStart => &["Ctrl+U"],
            KeyAction::CursorLeft | KeyAction::CursorRight => &["←", "→"],
            KeyAction::WordLeft | KeyAction::WordRight => &["Ctrl+←", "Ctrl+→"],
            KeyAction::SuggestNext | KeyAction::SuggestPrev => &["↑", "↓"],
            KeyAction::SuggestComplete => &["Tab"],
            KeyAction::ExternalEditor => &["Ctrl+G"],
            KeyAction::Rebuild => &["Ctrl+R"],
        }
    };
    let tui = read("docs/src/guide/tui.md");
    for a in [
        KeyAction::Insert('a'),
        KeyAction::Backspace,
        KeyAction::Delete,
        KeyAction::Newline,
        KeyAction::Send,
        KeyAction::Abort,
        KeyAction::Interrupt,
        KeyAction::HistoryPrev,
        KeyAction::HistoryNext,
        KeyAction::ToggleExpand,
        KeyAction::ToggleThinking,
        KeyAction::CopyAnswer,
        KeyAction::ScrollUp,
        KeyAction::ScrollDown,
        KeyAction::LineStart,
        KeyAction::LineEnd,
        KeyAction::KillToEnd,
        KeyAction::KillToStart,
        KeyAction::CursorLeft,
        KeyAction::CursorRight,
        KeyAction::WordLeft,
        KeyAction::WordRight,
        KeyAction::SuggestNext,
        KeyAction::SuggestPrev,
        KeyAction::SuggestComplete,
        KeyAction::ExternalEditor,
        KeyAction::Rebuild,
        KeyAction::Ignore,
    ] {
        for key in keys_of(a) {
            assert!(
                tui.contains(key),
                "key {key} ({a:?}) is undocumented in guide/tui.md"
            );
        }
    }
}

#[test]
fn crate_metadata_and_readme_link_published_guide() {
    assert!(read("Cargo.toml").contains(&format!("documentation = \"{DOCS_URL}\"")));
    assert!(read("README.md").contains(DOCS_URL));
    assert!(read("docs/book.toml").contains("site-url = \"/\""));
}
