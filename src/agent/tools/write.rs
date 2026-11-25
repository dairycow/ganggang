//! `write`: create or overwrite a file, inside the working directory.
//!
//! The cwd restriction is a guardrail, not a sandbox (spec §10: the bash
//! tool is the documented trust boundary) — `force` overrides it for the
//! occasional legitimate write elsewhere.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Cancel, Tool, ToolResult, display_relative, parse_args};

pub struct WriteTool {
    cwd: PathBuf,
}

impl WriteTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
    force: Option<bool>,
}

/// Lexically resolve `.`/`..` components against `cwd`. `std::path::absolute`
/// keeps `..` verbatim, which would let `sub/../escape` slip past a
/// component-wise `starts_with` containment check.
fn resolve_lexically(cwd: &Path, path: &str) -> PathBuf {
    let joined = super::resolve_path(cwd, path);
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

fn write(cwd: &PathBuf, path: &str, content: &str, force: bool) -> ToolResult {
    // Lexical containment; symlinks can still escape it, which is accepted
    // (no sandbox — see the module doc).
    let target = resolve_lexically(cwd, path);
    if !force && !target.starts_with(cwd) {
        return ToolResult::err(format!(
            "refusing to write outside the working directory: {} \
(pass force: true to override)",
            display_relative(cwd, &target)
        ));
    }

    if let Some(parent) = target.parent()
        && !parent.exists()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return ToolResult::err(format!(
            "cannot create {}: {e}",
            display_relative(cwd, parent)
        ));
    }

    match std::fs::write(&target, content) {
        Ok(()) => ToolResult::ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            display_relative(cwd, &target)
        )),
        Err(e) => ToolResult::err(format!(
            "cannot write {}: {e}",
            display_relative(cwd, &target)
        )),
    }
}

impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn description(&self) -> &'static str {
        "Create or overwrite a file. Refuses paths outside the working directory unless \
force is true; reports bytes written."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File to create or overwrite, relative to the cwd, absolute, or starting with ~"},
                "content": {"type": "string", "description": "Full file content"},
                "force": {"type": "boolean", "description": "Allow writing outside the working directory"}
            },
            "required": ["path", "content"]
        })
    }

    fn execute<'a>(&'a self, args: &'a Value, cancel: &'a Cancel) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            // Bounded work: one file written in one call. Refusing to
            // start is the whole cancellation story; a part-written file
            // would be worse than a refused write.
            if cancel.is_cancelled() {
                return ToolResult::err("aborted by user".into());
            }
            let args: WriteArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            write(
                &self.cwd,
                &args.path,
                &args.content,
                args.force.unwrap_or(false),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write_file(dir: &std::path::Path, args: Value) -> ToolResult {
        WriteTool::new(dir.to_path_buf())
            .execute(&args, &Cancel::none())
            .await
    }

    #[tokio::test]
    async fn creates_and_overwrites_with_byte_count() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("new.txt");

        let r = write_file(
            dir.path(),
            serde_json::json!({"path": "new.txt", "content": "hello"}),
        )
        .await;
        assert!(!r.is_error);
        assert!(r.output.contains("wrote 5 bytes"));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "hello");

        let r = write_file(
            dir.path(),
            serde_json::json!({"path": "new.txt", "content": "goodbye!"}),
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "goodbye!");
    }

    #[tokio::test]
    async fn refuses_paths_outside_cwd_unless_forced() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("escape.txt");
        let rel = target.to_string_lossy().to_string();

        let r = write_file(dir.path(), serde_json::json!({"path": rel, "content": "x"})).await;
        assert!(r.is_error);
        assert!(r.output.contains("outside the working directory"));
        assert!(!target.exists());

        let r = write_file(
            dir.path(),
            serde_json::json!({"path": rel, "content": "x", "force": true}),
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "x");
    }

    #[tokio::test]
    async fn refuses_dotdot_traversal_that_lexically_escapes_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("../escaped.txt");

        let r = write_file(
            dir.path(),
            serde_json::json!({"path": "../escaped.txt", "content": "x"}),
        )
        .await;
        assert!(r.is_error, "must refuse, got: {}", r.output);
        assert!(r.output.contains("outside the working directory"));
        assert!(!target.exists());

        // Inside-cwd `..` that resolves back inside must NOT be blocked.
        let r = write_file(
            dir.path(),
            serde_json::json!({"path": "sub/../inside.txt", "content": "x"}),
        )
        .await;
        assert!(!r.is_error);
        assert!(dir.path().join("inside.txt").exists());
    }

    #[tokio::test]
    async fn creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let r = write_file(
            dir.path(),
            serde_json::json!({"path": "a/b/c.txt", "content": "deep"}),
        )
        .await;
        assert!(!r.is_error);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "deep"
        );
    }
}
