//! Prompt templates (spec §02, M7.4): `~/.gg/prompts/*.md` files whose
//! frontmatter carries `name` and `description`; the body is the text a
//! `/<name>` command drops into the editor. Like skills, frontmatter is a
//! hand-parsed `key: value` subset — but a template file without fences
//! is still a template (the filename stem names it), because a prompt
//! needs no discovery contract with the model, only a name for its
//! command.

use std::path::{Path, PathBuf};

/// A loaded prompt template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptTemplate {
    pub name: String,
    pub description: String,
    /// Absolute path of the source file — `/prompts` lists it.
    pub path: PathBuf,
    /// The text inserted by `/<name>` (everything after the fences, or
    /// the whole file when it has none).
    pub body: String,
}

/// Command names the TUI owns outright; a template colliding with one is
/// skipped at load with a warning — built-ins always win.
const BUILTINS: &[&str] = &[
    "new", "resume", "compact", "skills", "session", "context", "quit", "exit", "prompts", "model",
];

/// Discover templates from `~/.gg/prompts/`. A missing directory is the
/// common case, not a warning. Every other problem is reported as a
/// warning string, never an error — templates are conveniences.
pub fn discover() -> (Vec<PromptTemplate>, Vec<String>) {
    let Some(home) = crate::home::dir() else {
        return (Vec::new(), Vec::new());
    };
    load_from(&home.join(".gg").join("prompts"))
}

/// The testable core of [`discover`]: read every `*.md` under `dir`
/// (sorted, so collisions resolve deterministically), first-name-wins.
pub fn load_from(dir: &Path) -> (Vec<PromptTemplate>, Vec<String>) {
    let mut warnings = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), warnings);
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();

    let mut templates: Vec<PromptTemplate> = Vec::new();
    for path in files {
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        match load_one(&path, &stem) {
            Ok(template) => {
                if template.name.chars().any(char::is_whitespace) {
                    // `/name` parsing splits at the first whitespace, so
                    // such a name can never be invoked — refuse it loudly
                    // instead of listing an unusable template.
                    warnings.push(format!(
                        "prompt '{}' from {} skipped: name contains whitespace",
                        template.name,
                        path.display()
                    ));
                } else if BUILTINS.contains(&template.name.as_str()) {
                    warnings.push(format!(
                        "prompt '{}' from {} skipped: /{} is a built-in command",
                        template.name,
                        path.display(),
                        template.name
                    ));
                } else if template.name.starts_with("skill:") {
                    // `/skill:<name>` is the TUI's forcing form: such a
                    // template could never be invoked — every submission
                    // of it routes to the skill command instead.
                    warnings.push(format!(
                        "prompt '{}' from {} skipped: /skill:<name> is a built-in command form",
                        template.name,
                        path.display()
                    ));
                } else if let Some(first) = templates.iter().find(|t| t.name == template.name) {
                    warnings.push(format!(
                        "prompt '{}' from {} skipped: already provided by {}",
                        template.name,
                        path.display(),
                        first.path.display()
                    ));
                } else {
                    templates.push(template);
                }
            }
            Err(reason) => warnings.push(reason),
        }
    }
    (templates, warnings)
}

/// Read one file into a template. `stem` names it when the frontmatter
/// does not. Unreadable files are a skip-with-reason.
fn load_one(path: &Path, stem: &str) -> Result<PromptTemplate, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (block, body) = split_fences(&text).unwrap_or(("", &text[..]));
    let mut name = stem.to_string();
    let mut description = String::new();
    for line in block.lines() {
        let line = line.trim();
        let Some((key, value)) = line.split_once(':') else {
            continue; // not a key: value line — ignore, like unknown keys
        };
        let value = strip_quotes(value.trim());
        match key.trim() {
            "name" if !value.is_empty() => name = value.to_string(),
            "description" if !value.is_empty() => description = value.to_string(),
            _ => {} // unknown keys are ignored
        }
    }
    Ok(PromptTemplate {
        name,
        description,
        path: path.to_path_buf(),
        body: body.trim().to_string(),
    })
}

/// Split leading `---` fences the way skills do (inclusive byte walk, so
/// CRLF cannot drift the offsets). `None` when the file opens with no
/// fence — then the whole file is the body.
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
            return Some((
                &text[first.len()..fence_at],
                &text[consumed.min(text.len())..],
            ));
        }
    }
    None // unclosed fence: treat the whole file as body
}

/// Drop one matching pair of surrounding quotes (skills' rule: real-world
/// YAML values arrive quoted).
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gg-prompts-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &Path, file: &str, content: &str) {
        fs::write(dir.join(file), content).unwrap();
    }

    #[test]
    fn frontmatter_names_and_describes_a_template() {
        let d = dir("front");
        write(
            &d,
            "review.md",
            "---\nname: review\ndescription: Review a diff carefully\n---\nReview this diff line by line.",
        );
        let (templates, warnings) = load_from(&d);
        assert!(warnings.is_empty());
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "review");
        assert_eq!(templates[0].description, "Review a diff carefully");
        assert_eq!(templates[0].body, "Review this diff line by line.");
        assert_eq!(templates[0].path, d.join("review.md"));
    }

    #[test]
    fn no_frontmatter_falls_back_to_the_stem() {
        let d = dir("stem");
        write(&d, "ship-it.md", "just ship it");
        let (templates, _) = load_from(&d);
        assert_eq!(templates[0].name, "ship-it");
        assert_eq!(templates[0].description, "");
        assert_eq!(templates[0].body, "just ship it");
    }

    #[test]
    fn quoted_values_and_unknown_keys() {
        let d = dir("quotes");
        write(
            &d,
            "a.md",
            "---\nname: \"quoted\"\ndescription: 'single'\nextra: ignored\n---\nbody",
        );
        let (templates, warnings) = load_from(&d);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(templates[0].name, "quoted");
        assert_eq!(templates[0].description, "single");
        assert_eq!(templates[0].body, "body");
    }

    /// `/name` parsing splits at the first whitespace, so a name WITH
    /// whitespace could never be invoked — it is refused at load with a
    /// warning instead of listed and unreachable.
    #[test]
    fn whitespace_names_are_refused_with_a_warning() {
        let d = dir("spacey");
        write(
            &d,
            "a.md",
            "---\nname: \"quoted name\"\ndescription: unusable\n---\nbody",
        );
        let (templates, warnings) = load_from(&d);
        assert!(templates.is_empty());
        assert!(
            warnings.iter().any(|w| w.contains("whitespace")),
            "{warnings:?}"
        );
    }

    #[test]
    fn builtin_and_duplicate_names_are_skipped_with_warnings() {
        let d = dir("collide");
        // Sorted order: a-quit.md loads first, so the builtin warning
        // fires for it and the plain duplicate fires for b-quit.md.
        write(&d, "a-quit.md", "---\nname: quit\n---\nnope");
        write(&d, "b-quit.md", "---\nname: dupe\n---\none");
        write(&d, "c-quit.md", "---\nname: dupe\n---\ntwo");
        write(&d, "d-prompts.md", "---\nname: prompts\n---\nnope");
        let (templates, warnings) = load_from(&d);
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].body, "one");
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("built-in"));
        assert!(warnings[1].contains("already provided by"));
        assert!(warnings[2].contains("built-in"));
    }

    /// `/skill:<name>` submits as the skill-forcing command, so a
    /// template named `skill:…` would be unreachable — refused at load
    /// like a built-in collision (and never advertised by the popup).
    #[test]
    fn skill_forcing_prefix_names_are_refused_with_a_warning() {
        let d = dir("skillprefix");
        write(&d, "a.md", "---\nname: skill:web\n---\nbody");
        let (templates, warnings) = load_from(&d);
        assert!(templates.is_empty());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("built-in command form"));
    }

    #[test]
    fn empty_name_value_falls_back_to_the_stem() {
        let d = dir("empty");
        write(
            &d,
            "fallback.md",
            "---\nname: \"\"\ndescription: keeps going\n---\nbody",
        );
        let (templates, _) = load_from(&d);
        assert_eq!(templates[0].name, "fallback");
        assert_eq!(templates[0].description, "keeps going");
    }

    #[test]
    fn missing_directory_is_silently_empty() {
        let (templates, warnings) = load_from(Path::new("/nonexistent-gg-prompts"));
        assert!(templates.is_empty());
        assert!(warnings.is_empty());
    }
}
