//! Documentation drift guards (test-only).
//!
//! The reference pages under `docs/src/reference/` are generated from the
//! live code ([`generate`]); the narrative guide is held to the code by
//! [`contracts`]. `UPDATE_DOCS=1 cargo test doc_sync` rewrites the
//! generated pages; a plain `cargo test` fails when they are stale.

mod contracts;
mod generate;

use std::path::{Path, PathBuf};

/// Where the published guide lives.
pub(crate) const DOCS_URL: &str = "https://ganggang.io";

pub(crate) fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn read(rel: &str) -> String {
    let path = root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every `*.md` file under `dir`, recursively, sorted.
pub(crate) fn markdown_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(markdown_files(&path));
        } else if path.extension().is_some_and(|e| e == "md") {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Write `expected` when `UPDATE_DOCS` is set; otherwise require the file
/// on disk to match it exactly.
fn sync_or_assert(rel: &str, expected: &str) {
    let path = root().join(rel);
    if std::env::var_os("UPDATE_DOCS").is_some() {
        std::fs::create_dir_all(path.parent().expect("reference page has a parent"))
            .expect("creating the reference directory");
        std::fs::write(&path, expected).expect("writing the reference page");
        return;
    }
    let actual = std::fs::read_to_string(&path).unwrap_or_default();
    if actual != expected {
        let line = actual
            .lines()
            .zip(expected.lines())
            .position(|(a, e)| a != e)
            .map_or_else(
                || actual.lines().count().min(expected.lines().count()) + 1,
                |i| i + 1,
            );
        panic!(
            "reference doc {rel} is out of date (first difference at line {line}).\n\
             Run: UPDATE_DOCS=1 cargo test doc_sync   (or ./scripts/sync-docs.sh)"
        );
    }
}

#[test]
fn reference_docs_are_up_to_date() {
    sync_or_assert("docs/src/reference/cli.md", &generate::cli_reference());
    sync_or_assert(
        "docs/src/reference/providers-and-models.md",
        &generate::providers_reference(),
    );
    sync_or_assert("docs/src/reference/tools.md", &generate::tools_reference());
    sync_or_assert(
        "docs/src/reference/commands.md",
        &generate::commands_reference(),
    );
}
