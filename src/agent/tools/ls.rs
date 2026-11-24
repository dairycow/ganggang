//! `ls`: one level of a directory, one line per entry — dirs first,
//! then files, alphabetical. Unreadable entries are skipped with a
//! noted line, never a failure (§10: tool problems are values).

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{BoxFuture, Tool, ToolResult, display, parse_args};

pub struct LsTool {
    cwd: PathBuf,
}

impl LsTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    fn resolve(&self, path: Option<&str>) -> PathBuf {
        self.cwd.join(path.unwrap_or("."))
    }
}

#[derive(Deserialize)]
struct LsArgs {
    path: Option<String>,
}

/// One rendered entry: dirs first, then files, alphabetical.
struct Row {
    is_dir: bool,
    name: String,
    /// File size in bytes (dirs render bare).
    size: u64,
}

fn ls(path: &Path) -> ToolResult {
    // Follow the root symlink (metadata, not symlink_metadata): listing
    // through a symlinked directory is the normal expectation. Entries
    // inside keep their own identity via metadata-per-entry below.
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return ToolResult::err(format!("cannot list {}: {e}", display(path))),
    };
    if !meta.is_dir() {
        return ToolResult::err(format!("cannot list {}: not a directory", display(path)));
    }
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(e) => return ToolResult::err(format!("cannot read {}: {e}", display(path))),
    };

    let mut rows = Vec::new();
    let mut notes = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Symlinks classify by what they point at — a link to a file
        // sizes as that file, a broken link lands in the notes.
        match std::fs::metadata(entry.path()) {
            Ok(m) => rows.push(Row {
                is_dir: m.is_dir(),
                size: m.len(),
                name,
            }),
            Err(e) => notes.push(format!("[unreadable: {name}: {e}]")),
        }
    }
    rows.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));

    let mut out = String::new();
    for row in &rows {
        out.push_str(&row.name);
        if row.is_dir {
            out.push('/');
        } else {
            out.push_str(&format!(" {} B", row.size));
        }
        out.push('\n');
    }
    if rows.is_empty() && notes.is_empty() {
        out.push_str("(empty directory)\n");
    }
    for note in &notes {
        out.push_str(note);
        out.push('\n');
    }
    ToolResult::ok(out)
}

impl Tool for LsTool {
    fn name(&self) -> &'static str {
        "ls"
    }

    fn description(&self) -> &'static str {
        "List one directory level: directories first (name/), then files with their byte \
size, alphabetical. Not recursive — use find to locate files deeper down. Hidden entries \
included. Entries that cannot be read are skipped with a noted line."
    }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory to list (default: the working directory)"}
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn execute<'a>(&'a self, args: &'a Value) -> BoxFuture<'a, ToolResult> {
        Box::pin(async move {
            let args: LsArgs = match parse_args(args) {
                Ok(a) => a,
                Err(e) => return ToolResult::err(e),
            };
            let path = self.resolve(args.path.as_deref());
            match tokio::task::spawn_blocking(move || ls(&path)).await {
                Ok(res) => res,
                Err(e) => ToolResult::err(format!("ls task failed: {e}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
        LsTool::new(dir.to_path_buf()).execute(&args).await
    }

    fn tree(dir: &Path) {
        std::fs::create_dir_all(dir.join("beta_dir")).unwrap();
        std::fs::create_dir_all(dir.join("alpha_dir")).unwrap();
        std::fs::write(dir.join("zeta.txt"), "hello").unwrap();
        std::fs::write(dir.join("beta_dir/inner.txt"), "").unwrap();
        std::fs::write(dir.join(".env"), "x=1").unwrap();
    }

    #[tokio::test]
    async fn lists_dirs_first_then_files_alphabetically() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(dir.path(), serde_json::json!({})).await;
        assert!(!r.is_error);
        let lines: Vec<&str> = r.output.lines().collect();
        assert_eq!(
            lines,
            vec!["alpha_dir/", "beta_dir/", ".env 3 B", "zeta.txt 5 B"]
        );
    }

    #[tokio::test]
    async fn empty_directory_says_so() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();

        let r = run(dir.path(), serde_json::json!({"path": "empty"})).await;
        assert!(!r.is_error);
        assert_eq!(r.output, "(empty directory)\n");
    }

    #[tokio::test]
    async fn missing_or_file_path_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());

        let r = run(dir.path(), serde_json::json!({"path": "nope"})).await;
        assert!(r.is_error);
        assert!(r.output.contains("cannot list"));

        let r = run(dir.path(), serde_json::json!({"path": "zeta.txt"})).await;
        assert!(r.is_error);
        assert!(r.output.contains("not a directory"));
    }

    /// A symlinked directory as the listing ROOT is followed — it used to
    /// error "not a directory" on a path that lists fine.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_root_lists() {
        let dir = tempfile::tempdir().unwrap();
        tree(dir.path());
        let link = dir.path().join("link-beta");
        std::os::unix::fs::symlink(dir.path().join("beta_dir"), &link).unwrap();

        let r = run(dir.path(), serde_json::json!({"path": "link-beta"})).await;
        assert!(!r.is_error, "{}", r.output);
        assert!(r.output.contains("inner.txt"), "{}", r.output);
    }

    #[tokio::test]
    async fn an_unreadable_entry_is_noted_not_fatal() {
        // A broken symlink is the portable way to make metadata() fail
        // while read_dir succeeds.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.txt"), "x").unwrap();
        std::os::unix::fs::symlink("does-not-exist", dir.path().join("dangling")).unwrap();

        let r = run(dir.path(), serde_json::json!({})).await;
        assert!(!r.is_error);
        assert!(r.output.contains("real.txt"));
        assert!(r.output.contains("[unreadable: dangling:"));
    }
}
