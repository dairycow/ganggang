//! Skills: instructions, not code (spec §11).
//!
//! A skill is a directory with a `SKILL.md` whose frontmatter carries
//! `name` and `description`. The whole trick is that only names and
//! descriptions are advertised in the system prompt — the model reads the
//! full instructions with the ordinary `read` tool when a task matches,
//! so skills add no new trust surface and no new execution path.
//!
//! Frontmatter is a hand-parsed `key: value` subset of YAML (§6 decision:
//! no yaml crate). Unknown keys are ignored per the Agent Skills spec.

use std::path::{Path, PathBuf};

/// The parsed `key: value` block between the `---` fences.
///
/// `disable_model_invocation` skills are still discovered and loadable on
/// demand (`/skill <name>`); the flag only hides them from system-prompt
/// advertising — the M3.5 consumer filters on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    pub disable_model_invocation: bool,
}

/// A discovered skill: frontmatter plus everything the advertiser and the
/// `/skill` command need — the absolute `SKILL.md` path and the
/// instruction body that follows the fences.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub disable_model_invocation: bool,
    /// Absolute path of the `SKILL.md` — advertised so the model can read it.
    pub path: PathBuf,
    /// The file's content after the frontmatter fences.
    #[allow(dead_code)] // consumed by the TUI's /skill forcing (M4.5)
    pub instructions: String,
}

/// Split a `SKILL.md` into `(frontmatter block, instruction body)`.
/// Returns None unless the file opens with a `---` fence and closes it.
///
/// CRLF files are first-class: offsets accumulate from raw lines with
/// their terminators included, so the byte math never drifts across the
/// `\r`s (str::lines strips them, and counting stripped lengths sliced
/// into multibyte boundaries — a startup panic). A BOM ahead of the
/// first fence makes the file fence-less → the skill is skipped.
fn split_fences(text: &str) -> Option<(&str, &str)> {
    let mut lines = text.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let mut consumed = first.len();
    for line in lines {
        let fence_at = consumed;
        consumed += line.len();
        if line.trim() == "---" {
            // The block spans from after the opening fence line to the
            // start of the closing one; the body starts after the closer.
            return Some((
                &text[first.len()..fence_at],
                &text[consumed.min(text.len())..],
            ));
        }
    }
    None // unclosed fence: not valid frontmatter, skill is skipped
}

/// Parse the `key: value` subset. The first colon separates key from
/// value; whitespace around both is trimmed. Absent or garbage
/// `disable-model-invocation` reads as false (lenient on purpose — a
/// typo'd flag must not hide a skill from forcing). Missing or empty
/// `name`/`description` is a skip, not a guess.
pub fn parse_frontmatter(text: &str) -> Option<SkillFrontmatter> {
    let (block, _body) = split_fences(text)?;
    let mut name = None;
    let mut description = None;
    let mut disable = false;
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue; // not a key: value line — ignore, like unknown keys
        };
        let (key, value) = (key.trim(), strip_quotes(value.trim()));
        match key {
            "name" => name = Some(value.to_string()).filter(|v| !v.is_empty()),
            "description" => description = Some(value.to_string()).filter(|v| !v.is_empty()),
            "disable-model-invocation" => disable = value.eq_ignore_ascii_case("true"),
            _ => {} // unknown keys are ignored (spec: warn, never fail)
        }
    }
    Some(SkillFrontmatter {
        name: name?,
        description: description?,
        disable_model_invocation: disable,
    })
}

/// Drop one matching pair of surrounding quotes. Real-world skills quote
/// YAML values routinely; advertising them with the quotes intact is an
/// interop wart. ASCII quotes make byte slicing safe.
fn strip_quotes(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Read a `SKILL.md` from disk into a [`Skill`]. `Err` carries the reason
/// a caller turns into a warning — a skipped skill is never an error.
pub fn load_skill(path: &Path) -> Result<Skill, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (_block, body) = split_fences(&text)
        .ok_or_else(|| format!("{} has no frontmatter fences", path.display()))?;
    let front = parse_frontmatter(&text)
        .ok_or_else(|| format!("{} is missing name or description", path.display()))?;
    Ok(Skill {
        name: front.name,
        description: front.description,
        disable_model_invocation: front.disable_model_invocation,
        path: path.to_path_buf(),
        instructions: body.trim().to_string(),
    })
}

/// Discover skills (spec §11): `~/.agents/skills/` first, then
/// `.agents/skills/` walking cwd → ancestors, stopping at the repo root.
/// Only immediate child directories containing a `SKILL.md` count
/// (the portable `skills/<name>/SKILL.md` form; deeper nesting and
/// standalone `.md` files are not skills). First discovered wins on name
/// collisions; every skip and collision is reported as a warning string,
/// never an error.
pub fn discover(cwd: &Path) -> (Vec<Skill>, Vec<String>) {
    let home = crate::home::dir().map(|h| h.join(".agents").join("skills"));
    discover_with_roots(home.as_deref(), cwd)
}

/// The testable core of [`discover`]: explicit home skills dir, no
/// environment reads.
pub fn discover_with_roots(home_skills: Option<&Path>, cwd: &Path) -> (Vec<Skill>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut dirs = Vec::new();
    if let Some(home) = home_skills {
        dirs.push(home.to_path_buf());
    }
    // With no `.git` anywhere the project walk reaches the fs root and
    // re-enters `$HOME/.agents/skills` — the same dir already scanned as
    // home. Scanning it twice reads every SKILL.md twice and reports each
    // home skill as colliding with itself.
    dirs.extend(
        project_skill_dirs(cwd)
            .into_iter()
            .filter(|d| Some(d.as_path()) != home_skills),
    );

    let mut skills: Vec<Skill> = Vec::new();
    for dir in dirs {
        for skill_md in scan_dir(&dir) {
            match load_skill(&skill_md) {
                Ok(skill) => {
                    if let Some(first) = skills.iter().find(|s| s.name == skill.name) {
                        warnings.push(format!(
                            "skill '{}' from {} skipped: already provided by {}",
                            skill.name,
                            skill.path.display(),
                            first.path.display()
                        ));
                    } else {
                        skills.push(skill);
                    }
                }
                Err(reason) => warnings.push(reason),
            }
        }
    }
    (skills, warnings)
}

/// `.agents/skills` directories from cwd upward through the repo root
/// (inclusive). With no `.git` anywhere, the walk runs to the fs root —
/// the same rule AGENTS.md discovery follows.
fn project_skill_dirs(cwd: &Path) -> Vec<PathBuf> {
    let repo_root = super::context::find_repo_root(cwd);
    let mut dirs = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let candidate = d.join(".agents").join("skills");
        if candidate.is_dir() {
            dirs.push(candidate);
        }
        if Some(d) == repo_root.as_deref() {
            break; // the repo root is the last stop
        }
        dir = d.parent();
    }
    dirs
}

/// Immediate child directories of `skills/` that contain a `SKILL.md`,
/// sorted by name — filesystem order is not deterministic, and
/// first-wins collisions must be stable.
fn scan_dir(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("SKILL.md").is_file())
        .collect();
    found.sort();
    found.into_iter().map(|p| p.join("SKILL.md")).collect()
}

#[cfg(test)]
mod tests;
