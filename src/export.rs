//! Session export to a single self-contained HTML file (spec §13, M7.7):
//! `gg --export <session> [out.html]`. Pure file-to-file — no LLM, no
//! server, no session mutation. Every piece of conversation text goes
//! through [`esc`] before touching the document: the output is meant to
//! be opened in a browser, so all content is hostile by default. No
//! `<script>` anywhere — collapsing is native `<details>`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

use crate::agent::session::io::{self, SessionHeader};
use crate::agent::session::{Entry, Message, Role};

/// Resolve what `--export` was pointed at: an existing file path (as
/// given) wins; otherwise the spec is a session id or unique id prefix
/// within this directory's session store, exactly like `--resume`.
pub fn resolve_target(spec: &str, cwd: &Path) -> anyhow::Result<PathBuf> {
    let direct = if Path::new(spec).is_absolute() {
        PathBuf::from(spec)
    } else {
        cwd.join(spec)
    };
    if direct.is_file() {
        return Ok(direct);
    }
    let Some(dir) = io::sessions_dir(cwd) else {
        bail!("no session store for this directory; pass a session file path instead");
    };
    let (sessions, _) = io::list_sessions(&dir);
    let chosen = io::resolve_prefix(&sessions, spec)?;
    Ok(chosen.path)
}

/// The default output path: the session file's stem + `.html`, in cwd.
pub fn default_out_path(session: &Path, cwd: &Path) -> PathBuf {
    let stem = session
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    cwd.join(format!("{stem}.html"))
}

/// Load a session file and write it out as HTML.
pub fn export_file(session: &Path, out: &Path) -> anyhow::Result<()> {
    let (header, entries, warnings) =
        io::load(session).with_context(|| format!("loading session {}", session.display()))?;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }
    let html = render_html(&header, &entries);
    std::fs::write(out, html).with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

/// Escape text for HTML content and attribute contexts.
fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// The document. Inline CSS, dark, zero external references.
const CSS: &str = r#"
:root { color-scheme: dark; }
body { margin: 0 auto; max-width: 52rem; padding: 1.5rem 1rem 4rem;
       background: #16191C; color: #E9E3D7;
       font: 15px/1.55 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; }
header { border-bottom: 1px solid #3A4047; padding-bottom: .75rem;
         margin-bottom: 1.25rem; }
h1 { font-size: 1rem; margin: 0 0 .25rem; color: #E9E3D7; }
.meta { color: #98A0A6; font-size: .8rem; }
.user, .assistant, .tool, .compaction { margin: 1rem 0; }
.user { border-left: 3px solid #D94B4B; padding: .25rem 0 .25rem .75rem; }
.assistant { border-left: 3px solid #3A4047; padding: .25rem 0 .25rem .75rem; }
.tool { border-left: 3px solid #3A4047; padding: .25rem 0 .25rem .75rem; }
.compaction { border-left: 3px dashed #D9A441; color: #98A0A6;
              padding: .25rem 0 .25rem .75rem; font-size: .85rem; }
pre { background: #2A2F35; border: 1px solid #3A4047; border-radius: 4px;
      padding: .6rem .75rem; overflow-x: auto; font-size: .82rem;
      font-family: ui-monospace, "SF Mono", "Cascadia Code", Menlo, Consolas, monospace;
      color: #C7D0C0;
      white-space: pre-wrap; word-break: break-word; margin: .4rem 0; }
details { margin: .3rem 0; }
summary { cursor: pointer; color: #98A0A6; font-size: .85rem; }
details.thinking summary { color: #6B7379; font-style: italic; }
.tool summary { color: #E07A4F; }
.usage { color: #6B7379; font-size: .75rem; margin-top: .2rem; }
"#;

/// Render one session as a complete HTML document. Tool results are
/// spliced into the `<details>` of the assistant call that requested
/// them (two passes: results first, then the walk); a result without a
/// matching call renders standalone.
pub fn render_html(header: &SessionHeader, entries: &[Entry]) -> String {
    let results: HashMap<&str, &Message> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message {
                message:
                    message @ Message {
                        role: Role::ToolResult,
                        tool_call_id: Some(id),
                        ..
                    },
                ..
            } => Some((id.as_str(), message)),
            _ => None,
        })
        .collect();
    // Which calls an assistant entry actually made: a result outside
    // this set has no call to ride inside and renders standalone.
    let called_ids: Vec<&str> = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message {
                message:
                    Message {
                        role: Role::Assistant,
                        tool_calls,
                        ..
                    },
                ..
            } => Some(tool_calls.iter().map(|c| c.id.as_str())),
            _ => None,
        })
        .flatten()
        .collect();

    let mut body = String::new();
    for entry in entries {
        match entry {
            Entry::Message {
                timestamp, message, ..
            } => match message.role {
                Role::User => {
                    body.push_str(&format!(
                        "<div class=\"user\" title=\"{ts}\"><pre>{text}</pre></div>\n",
                        ts = esc(timestamp),
                        text = esc(message.content.as_deref().unwrap_or("")),
                    ));
                }
                Role::Assistant => {
                    body.push_str("<div class=\"assistant\">");
                    if let Some(thinking) = &message.reasoning {
                        body.push_str(&format!(
                            "<details class=\"thinking\"><summary>thinking</summary>\
                             <pre>{}</pre></details>",
                            esc(thinking)
                        ));
                    }
                    if let Some(text) = &message.content {
                        body.push_str(&format!("<pre>{}</pre>", esc(text)));
                    }
                    for call in &message.tool_calls {
                        let error_note = match &call.args_error {
                            Some(err) => format!(" ⚠ {}", esc(err)),
                            None => String::new(),
                        };
                        body.push_str(&format!(
                            "<details class=\"tool\"><summary>▸ {name} {args}\
                             {error_note}</summary>",
                            name = esc(&call.name),
                            args = esc(&call.args.to_string()),
                        ));
                        if let Some(result) = results.get(call.id.as_str()) {
                            let status = if result.is_error { " ✗" } else { "" };
                            body.push_str(&format!(
                                "<pre>→{status} {}</pre>",
                                esc(result.content.as_deref().unwrap_or(""))
                            ));
                        }
                        body.push_str("</details>");
                    }
                    if let Some(usage) = &message.usage {
                        body.push_str(&format!(
                            "<div class=\"usage\">{} in / {} out</div>",
                            usage.prompt_tokens, usage.completion_tokens
                        ));
                    }
                    body.push_str("</div>\n");
                }
                Role::ToolResult => {
                    if message
                        .tool_call_id
                        .as_deref()
                        .is_some_and(|id| called_ids.contains(&id))
                    {
                        continue; // rendered inside its call above
                    }
                    // Dangling (its call never appears — e.g. an aborted
                    // batch synthesized around): render standalone so
                    // nothing is lost.
                    let status = if message.is_error { " ✗" } else { "" };
                    body.push_str(&format!(
                        "<div class=\"tool\"><pre>→{status} {}</pre></div>\n",
                        esc(message.content.as_deref().unwrap_or(""))
                    ));
                }
            },
            Entry::Compaction {
                summary, timestamp, ..
            } => {
                body.push_str(&format!(
                    "<div class=\"compaction\" title=\"{ts}\">⇄ context compacted: \
                     <details><summary>summary</summary><pre>{}</pre></details></div>\n",
                    esc(summary),
                    ts = esc(timestamp)
                ));
            }
        }
    }

    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>gg session {id}</title>\n<style>{CSS}</style>\n</head>\n<body>\n\
         <header><h1>gg session</h1><div class=\"meta\">{model} · {cwd} · started {ts}\
         </div></header>\n{body}</body>\n</html>\n",
        id = esc(&header.id),
        model = esc(&header.model),
        cwd = esc(&header.cwd),
        ts = esc(&header.timestamp),
    )
}

#[cfg(test)]
mod tests;
