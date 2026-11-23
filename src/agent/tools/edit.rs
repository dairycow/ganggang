//! `edit`: exact-match, all-or-nothing text replacement.
//!
//! Every edit must match exactly once in the file as of its turn; any miss
//! or ambiguity leaves the file untouched. The model reads the error and
//! retries with better anchors — that loop is the whole tool.

use std::io::Read;
use std::path::PathBuf;

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Tool, ToolResult, parse_args};

/// Files above this size are refused outright: editing a truncated view and
/// writing it back would silently destroy the tail.
const MAX_BYTES: usize = 50 * 1024;

pub struct EditTool {
    cwd: PathBuf,
}

impl EditTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    edits: Vec<EditItem>,
}

#[derive(Deserialize)]
struct EditItem {
    old_text: String,
    new_text: String,
}

fn excerpt(s: &str) -> String {
    s.chars().take(80).collect()
}

fn edit(path: &PathBuf, edits: &[EditItem]) -> ToolResult {
    if edits.is_empty() {
        return ToolResult::err("edits must not be empty".to_string());
    }
    // Bounded read (take caps the read itself — /dev/zero never EOFs).
    // Refuse above the cap rather than editing a truncated view: writing it
    // back would destroy the file's tail.
    let mut bytes = Vec::new();
    let loaded = std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes));
    if let Err(e) = loaded {
        return ToolResult::err(format!("cannot read {}: {e}", path.display()));
    }
    if bytes.len() > MAX_BYTES {
        return ToolResult::err(format!(
            "refusing to edit {}: file exceeds the {} byte cap",
            path.display(),
            MAX_BYTES
        ));
    }
    let mut content = match String::from_utf8(bytes) {
        Ok(c) => c,
        Err(_) => {
            return ToolResult::err(format!("cannot read {}: not valid UTF-8", path.display()));
        }
    };

    // All edits apply in memory first; the disk write happens only after
    // every match succeeded, so a late failure cannot leave a half-edited file.
    for (i, item) in edits.iter().enumerate() {
        let hits = content.matches(&item.old_text).count();
        match hits {
            1 => content = content.replacen(&item.old_text, &item.new_text, 1),
            0 => {
                return ToolResult::err(format!(
                    "edit {} failed: old_text not found: {:?}",
                    i + 1,
                    excerpt(&item.old_text)
                ));
            }
            n => {
                return ToolResult::err(format!(
                    "edit {} failed: old_text found {n} times (must be unique): {:?}",
                    i + 1,
                    excerpt(&item.old_text)
                ));
            }
        }
    }

    match std::fs::write(path, &content) {
        Ok(()) => ToolResult::ok(format!(
            "applied {} edit(s) to {}",
            edits.len(),
            path.display()
        )),
        Err(e) => ToolResult::err(format!("cannot write {}: {e}", path.display())),
    }
}

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn description(&self) -> &'static str {
        "Apply exact-match text edits to a file. Every old_text must appear exactly once; \
if any edit misses or is ambiguous, nothing is written."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to edit"},
                "edits": {
                    "type": "array",
                    "description": "Edits applied in order, atomically",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {"type": "string"},
                            "new_text": {"type": "string"}
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: EditArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            edit(&self.cwd.join(&args.path), &args.edits)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn edit_file(dir: &std::path::Path, args: Value) -> ToolResult {
        EditTool::new(dir.to_path_buf()).execute(&args).await
    }

    fn written(path: &std::path::Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[tokio::test]
    async fn applies_multiple_edits_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "fn main() {\n    todo!()\n}\n").unwrap();

        let r = edit_file(
            dir.path(),
            serde_json::json!({"path": "f.txt", "edits": [
                {"old_text": "todo!()", "new_text": "println!(\"hi\")"},
                {"old_text": "fn main", "new_text": "pub fn main"}
            ]}),
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(written(&f), "pub fn main() {\n    println!(\"hi\")\n}\n");
    }

    #[tokio::test]
    async fn a_miss_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "alpha beta\n").unwrap();

        let r = edit_file(
            dir.path(),
            serde_json::json!({"path": "f.txt", "edits": [
                {"old_text": "alpha", "new_text": "A"},
                {"old_text": "gamma", "new_text": "G"}
            ]}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("edit 2 failed"));
        assert!(r.output.contains("not found"));
        assert_eq!(written(&f), "alpha beta\n", "first edit must not persist");
    }

    #[tokio::test]
    async fn an_ambiguous_match_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "x x x\n").unwrap();

        let r = edit_file(
            dir.path(),
            serde_json::json!({"path": "f.txt", "edits": [
                {"old_text": "x", "new_text": "y"}
            ]}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("found 3 times"));
        assert_eq!(written(&f), "x x x\n");
    }

    #[tokio::test]
    async fn missing_file_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let r = edit_file(
            dir.path(),
            serde_json::json!({"path": "nope.txt", "edits": [
                {"old_text": "a", "new_text": "b"}
            ]}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("cannot read"));
    }

    #[tokio::test]
    async fn a_file_over_the_cap_is_refused_not_half_edited() {
        let dir = tempfile::tempdir().unwrap();
        let big = "x".repeat(MAX_BYTES + 1);
        std::fs::write(dir.path().join("big.txt"), &big).unwrap();

        let r = edit_file(
            dir.path(),
            serde_json::json!({"path": "big.txt", "edits": [
                {"old_text": "x", "new_text": "y"}
            ]}),
        )
        .await;
        assert!(r.is_error);
        assert!(r.output.contains("exceeds"));
        // The file is untouched — not rewritten from a truncated view.
        assert_eq!(
            std::fs::read(dir.path().join("big.txt")).unwrap().len(),
            big.len()
        );
    }
}
