//! System-prompt assembly (spec §12): built from named sections —
//! preamble, context files, skills (M3.5), working directory — with the
//! UTC date frozen per [`Agent`](super::Agent) so the prompt prefix stays
//! KV-cacheable across turns.

use std::path::{Path, PathBuf};

use super::skills::Skill;

/// One `AGENTS.md` file pulled into the prompt: where it came from (shown
/// as a header so the model can tell instructions apart) and what it said.
pub struct AgentsFile {
    pub origin: String,
    pub content: String,
}

/// The identity and discipline half of the prompt. Everything here is a
/// standing instruction; situational facts live in the later sections.
const PREAMBLE: &str = "\
You are gg, a terminal-based coding agent. You work in the user's files: \
reading, editing, writing, running commands, answering questions. Be \
precise and do not pad.

Working style:
- Ground every claim in tool output you have seen: read before editing, run \
before claiming results, never fabricate file contents, output, or tests.
- Prefer read (with offset/limit), grep, find, ls, edit and write over bash \
(cat/sed/grep/find/python3); reserve bash for builds, tests, git and \
anything no tool covers. bash runs in cwd: never `cd <cwd> &&`.
- Every tool result stays in context on every future turn. Prefer grep and \
targeted read over full-file reads, and batch independent read-only calls in \
one turn.
- Make the smallest exact edit; verify after code changes, then stop. No \
redundant re-reads or re-runs.
- A failed call is data: read the error, fix the approach, retry.

Answering:
- Lead with the answer and the minimum context needed to trust it. No filler \
or restating the question.

Boundaries:
- Ask before destructive or hard-to-undo actions (force pushes, deletions \
outside the task, system installs). Routine dependency installs are fine.
- Ask one sharp question only if ambiguity changes what gets built; \
otherwise make the reasonable call and note it in one line.";

/// Assemble the system prompt: preamble → context files → skills →
/// working directory (spec §12). Empty inputs omit their section
/// entirely — no placeholder headers for absent content.
#[cfg_attr(not(test), allow(dead_code))]
pub fn build_system_prompt(cwd: &Path, agents_files: &[AgentsFile], skills: &[Skill]) -> String {
    build_system_prompt_with_date(cwd, agents_files, skills, &today_utc_iso())
}

pub fn today_utc_iso() -> String {
    let today = time::OffsetDateTime::now_utc();
    format!(
        "{:04}-{:02}-{:02}",
        today.year(),
        u8::from(today.month()),
        today.day()
    )
}

pub fn build_system_prompt_with_date(
    cwd: &Path,
    agents_files: &[AgentsFile],
    skills: &[Skill],
    date: &str,
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
            "# Skills\nRead a matching skill's SKILL.md with the read tool before \
             following it (scripts run relative to its directory):\n{}",
            lines.join("\n")
        ));
    }

    sections.push(working_dir_section(cwd, date));

    sections.join("\n\n")
}

fn working_dir_section(cwd: &Path, date: &str) -> String {
    format!(
        "# Working directory\ncwd: {}\nos: {}\ndate: {date}",
        cwd.display(),
        std::env::consts::OS,
    )
}

/// Discover every `AGENTS.md` that governs this cwd (spec §12): the user's
/// `~/.gg/AGENTS.md` first, then repo-root → cwd concatenated root-first so
/// deeper files read as refinements. Missing files simply do not appear;
/// files that exist but cannot be read produce a warning string (they are
/// advisory — never fatal, but §17 says never silently dropped either).
pub fn discover_agents_md(cwd: &Path) -> (Vec<AgentsFile>, Vec<String>) {
    let home = crate::home::dir();
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
