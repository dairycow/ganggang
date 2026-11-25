//! System-prompt assembly (spec §12): built from named sections —
//! preamble, context files, skills (M3.5), environment — with the
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

/// The identity and discipline half of the prompt, tailored to the
/// active tool allowlist so disabled tools are never advertised.
fn preamble(tools: &[&str]) -> String {
    let has = |name: &str| tools.contains(&name);
    let mut working = Vec::new();
    if has("edit") || has("write") {
        working.push(
            "- Ground every claim in tool output you have seen: read before editing, run before claiming results, never fabricate file contents, output or tests.".to_string(),
        );
    } else {
        working.push(
            "- Ground every claim in tool output you have seen: inspect before claiming results, never fabricate file contents, output or tests.".to_string(),
        );
    }

    let ro_tools: Vec<&str> = ["outline", "grep", "read", "find", "ls"]
        .into_iter()
        .filter(|name| has(name))
        .collect();
    if !ro_tools.is_empty() {
        let mut line = format!(
            "- Every tool result stays in context. Batch all independent read-only calls ({}) into one turn.",
            ro_tools.join(", ")
        );
        if has("read") {
            match (has("outline"), has("grep")) {
                (true, true) => line.push_str(
                    " For files over ~200 lines, start with outline or grep (`files_only: true` for broad searches), then targeted read(offset, limit).",
                ),
                (true, false) => line.push_str(
                    " For files over ~200 lines, start with outline, then targeted read(offset, limit).",
                ),
                (false, true) => line.push_str(
                    " For files over ~200 lines, start with grep (`files_only: true` for broad searches), then targeted read(offset, limit).",
                ),
                (false, false) => line.push_str(
                    " Prefer targeted read(offset, limit) over full-file reads.",
                ),
            }
        } else if has("grep") {
            line.push_str(" Use grep with `files_only: true` for broad searches.");
        }
        working.push(line);
    }

    if has("bash") {
        let prefix = if tools.iter().any(|t| *t != "bash") {
            "Use native tools instead of bash (cat/sed/grep/find/python3); reserve bash for builds, tests, git and shell-only work. "
        } else {
            ""
        };
        working.push(format!(
            "- {prefix}bash runs in cwd (never `cd <cwd> &&`) and already keeps head and tail past 10 KiB: chain dependent steps with `&&`; combine independent checks in one call; never pipe checks through `head`/`tail` without `pipefail` (it masks exit codes)."
        ));
    }

    if has("edit") {
        working.push(
            "- For multi-file tasks, edit each file right after reading it instead of reading every file first (compaction drops earlier reads). Make the smallest exact edit; after compaction or formatting, re-read target lines before calling edit. Verify after changes, then stop without redundant re-reads or re-runs.".to_string(),
        );
    } else if has("write") {
        working.push(
            "- Make minimal exact changes; verify after changes, then stop without redundant re-reads or re-runs.".to_string(),
        );
    } else {
        working.push(
            "- Stop once you have the evidence needed; avoid redundant re-reads or re-runs."
                .to_string(),
        );
    }
    working.push("- A failed call is data: read the error, fix the approach, retry.".to_string());

    format!(
        "You are gg, a terminal-based coding agent. Be precise and do not pad.\n\n\
         Working style:\n{}\n\n\
         Answering:\n\
         - Lead with the answer and the minimum context needed to trust it. No filler or restating the question.\n\n\
         Boundaries:\n\
         - Ask before destructive or hard-to-undo actions (force pushes, deletions outside the task, system or new dependency installs).\n\
         - Do not read or print secret or credential files (`~/.gg/config.toml`, `.env`, private keys, tokens) unless the user explicitly asks.\n\
         - Ask one sharp question only if ambiguity changes what gets built; otherwise make the reasonable call and note it in one line.",
        working.join("\n")
    )
}

/// Assemble the system prompt: preamble → context files → skills →
/// environment (spec §12). Empty inputs omit their section
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
    build_system_prompt_for(cwd, agents_files, skills, date, None, super::tools::NAMES)
}

pub fn build_system_prompt_for(
    cwd: &Path,
    agents_files: &[AgentsFile],
    skills: &[Skill],
    date: &str,
    model: Option<&str>,
    tools: &[&str],
) -> String {
    let mut sections = vec![preamble(tools)];

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
             following it (bundled skill files live beside SKILL.md; repository commands run from cwd):\n{}",
            lines.join("\n")
        ));
    }

    sections.push(environment_section(cwd, date, model));

    sections.join("\n\n")
}

fn environment_section(cwd: &Path, date: &str, model: Option<&str>) -> String {
    let mut out = format!(
        "# Environment\ncwd: {}\nos: {}\ndate: {date}",
        cwd.display(),
        std::env::consts::OS,
    );
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        out.push_str(&format!("\nmodel: {model}"));
    }
    out
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
