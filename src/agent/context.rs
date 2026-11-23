//! System-prompt assembly (spec §12): built fresh for every request from
//! named sections — preamble, context files, tools, skills (M3.5), working
//! directory — so there is no prompt cache to invalidate. The preamble is
//! a constant versioned with the code; everything else is discovered.

use std::path::{Path, PathBuf};

use super::skills::Skill;
use super::tools::Registry;

/// One `AGENTS.md` file pulled into the prompt: where it came from (shown
/// as a header so the model can tell instructions apart) and what it said.
pub struct AgentsFile {
    pub origin: String,
    pub content: String,
}

/// The identity and discipline half of the prompt. Everything here is a
/// standing instruction; situational facts live in the later sections.
const PREAMBLE: &str = "\
You are gg, a terminal-based coding agent. You do software work in the \
user's files: reading, editing, writing, running commands, answering \
questions. You are precise and you do not pad.

Working style:
- Ground every claim in what you have actually seen. Read a file before \
editing it; run a command before claiming its output. Never invent file \
contents, command output, or test results — if you do not know, find out \
with a tool or say so.
- Prefer the smallest change that solves the problem. Use edit with exact, \
unique anchors; never rewrite a whole file when an edit fits. Read before \
you edit so your anchors match the file as it is.
- After changing code, verify: build, test, or exercise it. Report results \
honestly, including failures.
- A failed tool call is data, not a disaster: read the error, fix the \
arguments or the approach, retry. Tool errors come back to you as text \
you can act on.

Answering:
- Be concise. Lead with the answer, then the minimum context needed to \
trust it. If the answer is one word, reply with one word.
- No praise, no filler, no restating the question back.
- When the useful artifact is a command or a snippet, give it plainly so \
the user can run it themselves.

Boundaries:
- You run with the user's permissions in their working directory; act \
accordingly. Ask before anything destructive or hard to undo (force \
pushes, deletions outside the task, system-level installs). Routine \
project dependency installs (npm install, cargo fetch) are not in that \
class — just run them.
- If a request is ambiguous in a way that changes what you build, ask one \
sharp question. Otherwise make the reasonable call and note it in one line.";

/// Assemble the system prompt: preamble → context files → tools →
/// skills → working directory (spec §12). Empty inputs omit their section
/// entirely — no placeholder headers for absent content.
pub fn build_system_prompt(
    cwd: &Path,
    tools: &Registry,
    agents_files: &[AgentsFile],
    skills: &[Skill],
) -> String {
    let mut sections = vec![PREAMBLE.to_string()];

    if !agents_files.is_empty() {
        let mut files = String::new();
        for file in agents_files {
            files.push_str(&format!("### {}\n{}\n", file.origin, file.content));
        }
        sections.push(format!(
            "# Context files (AGENTS.md, broadest first)\n{files}"
        ));
    }

    sections.push(format!("# Tools\n{}", tool_lines(tools)));

    // Advertising is names + descriptions + paths, never the instructions:
    // the model pulls a skill's body with the read tool when a task
    // matches. That laziness is the whole trick (spec §11).
    let advertised: Vec<&Skill> = skills
        .iter()
        .filter(|s| !s.disable_model_invocation)
        .collect();
    if !advertised.is_empty() {
        let lines: Vec<String> = advertised
            .iter()
            .map(|s| format!("- {}: {} ({})", s.name, s.description, s.path.display()))
            .collect();
        sections.push(format!(
            "# Skills\nWhen a task matches a skill below, read its SKILL.md \
             with the read tool and follow it (scripts run relative to the \
             skill's directory):\n{}",
            lines.join("\n")
        ));
    }

    sections.push(working_dir_section(cwd));

    sections.join("\n\n")
}

/// One line per tool: name, description, and the argument names from its
/// schema (`*` marks required). Enough for the model to call correctly;
/// the full JSON Schema rides along in the request's tools array.
fn tool_lines(tools: &Registry) -> String {
    tools
        .tools()
        .iter()
        .map(|tool| {
            let schema = tool.schema();
            let required: Vec<String> = schema["required"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let args: Vec<String> = schema["properties"]
                .as_object()
                .map(|props| {
                    props
                        .keys()
                        .map(|k| {
                            if required.iter().any(|r| r == k) {
                                format!("{k}*")
                            } else {
                                k.clone()
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!(
                "- {}: {} ({})",
                tool.name(),
                tool.description(),
                args.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn working_dir_section(cwd: &Path) -> String {
    let today = time::OffsetDateTime::now_utc();
    format!(
        "# Working directory\ncwd: {}\nos: {}\ndate: {:04}-{:02}-{:02}",
        cwd.display(),
        std::env::consts::OS,
        today.year(),
        u8::from(today.month()),
        today.day()
    )
}

/// Discover every `AGENTS.md` that governs this cwd (spec §12): the user's
/// `~/.gg/AGENTS.md` first, then repo-root → cwd concatenated root-first so
/// deeper files read as refinements. Missing files simply do not appear;
/// files that exist but cannot be read produce a warning string (they are
/// advisory — never fatal, but §17 says never silently dropped either).
pub fn discover_agents_md(cwd: &Path) -> (Vec<AgentsFile>, Vec<String>) {
    let home = std::env::var("HOME").ok().map(PathBuf::from);
    discover_with_home(cwd, home.as_deref())
}

/// The testable core of discovery: same walk, injected home so tests never
/// touch the real `~`.
fn discover_with_home(cwd: &Path, home: Option<&Path>) -> (Vec<AgentsFile>, Vec<String>) {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    if let Some(home) = home {
        match load(&home.join(".gg").join("AGENTS.md"), "~/.gg/AGENTS.md") {
            (Some(file), _) => files.push(file),
            (_, Some(warning)) => warnings.push(warning),
            _ => {}
        }
    }
    let root = find_repo_root(cwd).unwrap_or_else(|| PathBuf::from("/"));
    for path in agents_md_between(&root, cwd) {
        let origin = path
            .strip_prefix(&root)
            .map(|rel| rel.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        match load(&path, &origin) {
            (Some(file), _) => files.push(file),
            (_, Some(warning)) => warnings.push(warning),
            _ => {}
        }
    }
    (files, warnings)
}

/// The innermost ancestor (or cwd itself) containing `.git` — a directory
/// (normal clone) or a file (worktree). None when there is no repo: both
/// AGENTS.md and skills discovery then govern from the filesystem root,
/// the spec's stated fallback. Shared by context and skills discovery.
pub(super) fn find_repo_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// AGENTS.md files from `root` down to `cwd`, root first (broadest first).
fn agents_md_between(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<&Path> = cwd.ancestors().collect();
    // Keep the ancestors of cwd that lie inside the repo (root or below);
    // a non-ancestor root yields nothing.
    dirs.retain(|dir| dir.ancestors().any(|a| a == root));
    dirs.reverse();
    dirs.into_iter()
        .map(|dir| dir.join("AGENTS.md"))
        .filter(|path| path.is_file())
        .collect()
}

/// Read one file into the prompt. Absent (NotFound) is silent — missing
/// context is the normal case. Present-but-unreadable is skipped WITH a
/// warning: advisory context must not kill runs, but §17 forbids silent
/// drops at boundaries.
fn load(path: &Path, origin: &str) -> (Option<AgentsFile>, Option<String>) {
    match std::fs::read_to_string(path) {
        Ok(content) => (
            Some(AgentsFile {
                origin: origin.to_string(),
                content,
            }),
            None,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(e) => (
            None,
            Some(format!("cannot read {origin} ({}): {e}", path.display())),
        ),
    }
}

#[cfg(test)]
mod tests;
