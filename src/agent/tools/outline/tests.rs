//! Outline tests: one fixture per language family, then the refusals, the
//! two truncation notices, and the stop request.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::{MAX_OUTPUT_BYTES, OutlineTool};
use crate::agent::tools::Cancel;
use crate::agent::tools::Tool;

/// A fresh directory holding one outlined file, so paths never collide.
fn tmp_dir(name: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut dir = std::env::temp_dir();
    dir.push(format!("gg-outline-{stamp}-{name}"));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the tool over `content` as a file named `name`, answering
/// `(is_error, content)` and leaving nothing behind.
async fn outlined(name: &str, content: &[u8], extra: Value) -> (bool, String) {
    let dir = tmp_dir(name);
    fs::write(dir.join(name), content).unwrap();
    let mut args = json!({ "path": name });
    if let (Some(base), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    let tool = OutlineTool::new(dir.clone());
    let result = tool.execute(&args, &Cancel::none()).await;
    fs::remove_dir_all(&dir).ok();
    (result.is_error, result.output)
}

/// One rendered entry, for assertions about nesting and spans.
struct Row {
    depth: usize,
    start: usize,
    end: usize,
    sig: String,
}

/// Parse the rendered entries back out of the outline.
fn rows(out: &str) -> Vec<Row> {
    out.lines()
        .skip(1)
        .filter_map(|line| {
            let indent = line.len() - line.trim_start().len();
            let body = line.trim_start();
            let (range, sig) = body.split_once(": ")?;
            let (start, end) = range
                .split_once('-')
                .map_or((range, range), |(start, end)| (start, end));
            Some(Row {
                depth: indent / 2,
                start: start.parse().ok()?,
                end: end.parse().ok()?,
                sig: sig.to_string(),
            })
        })
        .collect()
}

/// The 1-based line of `needle` in a fixture.
fn line_of(src: &str, needle: &str) -> usize {
    src.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("fixture has no {needle:?}"))
        + 1
}

/// The row whose signature mentions `needle`.
fn row<'a>(rows: &'a [Row], needle: &str) -> &'a Row {
    rows.iter()
        .find(|row| row.sig.contains(needle))
        .unwrap_or_else(|| panic!("no entry for {needle:?}"))
}

#[tokio::test]
async fn rust_items_and_nested_methods() {
    let src = r#"
use std::fs::File;

/// A point.
pub struct Point {
    x: i32,
    y: i32,
}

pub(crate) enum Color { Red, Green }

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    // A closing brace in a comment } must not end the impl.
    fn sum(&self) -> i32 {
        let s = "}{";
        s.len() as i32 + self.x
    }
}

mod inner {
    pub fn hidden() {}
}

fn free() {}
"#;
    let (err, out) = outlined("src.rs", src.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(out.starts_with("src.rs ("));
    let found = rows(&out);
    assert_eq!(
        row(&found, "struct Point").start,
        line_of(src, "pub struct Point")
    );
    assert_eq!(row(&found, "struct Point").end, line_of(src, "y: i32,") + 1);
    assert_eq!(
        row(&found, "enum Color").start,
        line_of(src, "pub(crate) enum Color")
    );
    assert_eq!(
        row(&found, "enum Color").end,
        line_of(src, "pub(crate) enum Color")
    );
    let imp = row(&found, "impl Point");
    assert_eq!((imp.depth, imp.start), (0, line_of(src, "impl Point {")));
    assert_eq!(imp.end, line_of(src, "mod inner {") - 2);
    assert_eq!(row(&found, "pub fn new").depth, 1);
    assert_eq!(row(&found, "fn sum").depth, 1);
    assert_eq!(row(&found, "fn hidden").depth, 1);
    assert_eq!(row(&found, "fn free").depth, 0);
    assert_eq!(row(&found, "fn free").start, line_of(src, "fn free() {}"));
    assert!(
        !out.contains("let s ="),
        "a body line became an entry:\n{out}"
    );
}

#[tokio::test]
async fn python_classes_and_defs() {
    let src = "class Outer:\n    def one(self):\n        return 1\n\n    async def two(self):\n        \"\"\"Doc.\"\"\"\n        pass\n\ndef top():\n    pass\n";
    let (err, out) = outlined("m.py", src.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    let found = rows(&out);
    let class = row(&found, "class Outer");
    assert_eq!((class.depth, class.start), (0, 1));
    assert_eq!(class.end, line_of(src, "pass"));
    assert_eq!(row(&found, "def one").depth, 1);
    assert_eq!(row(&found, "async def two").depth, 1);
    assert_eq!(row(&found, "def top").depth, 0);
    assert_eq!(row(&found, "def top").end, 10);
}

#[tokio::test]
async fn typescript_declarations_and_methods() {
    let src = "export interface Shape { area(): number }\nexport class Circle implements Shape {\n  constructor(private r: number) {}\n  area() { return Math.PI * this.r ** 2 }\n}\nconst square = (s: number) => s * s;\ntype Alias = Circle | Square;\nfunction helper(a: number) { return a }\n";
    let (err, out) = outlined("m.ts", src.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    let found = rows(&out);
    assert_eq!(row(&found, "interface Shape").end, 1);
    assert_eq!(row(&found, "class Circle").start, 2);
    assert_eq!(row(&found, "constructor").depth, 1);
    assert_eq!(row(&found, "return Math.PI").depth, 1);
    assert_eq!(row(&found, "const square").start, 6);
    assert_eq!(row(&found, "type Alias").start, 7);
    assert_eq!(row(&found, "function helper").start, 8);
}

#[tokio::test]
async fn markdown_headings_skip_fenced_code() {
    let src = "# Title\n\nIntro.\n\n```sh\n# not a heading\n```\n\n## Section\n\n- item\n\n### Deep\n\ntext\n\n## Next\n\nend\n";
    let (err, out) = outlined("README.md", src.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    let found = rows(&out);
    assert_eq!(found.len(), 4);
    let title = row(&found, "# Title");
    assert_eq!((title.depth, title.start, title.end), (0, 1, 19));
    let section = row(&found, "## Section");
    assert_eq!((section.depth, section.end), (1, 16));
    assert_eq!(row(&found, "### Deep").depth, 2);
    assert_eq!(row(&found, "## Next").end, 19);
    assert!(
        !out.contains("not a heading"),
        "fence body became a heading:\n{out}"
    );
}

#[tokio::test]
async fn toml_tables() {
    let src = "# comment\n[package]\nname = \"x\"\n\n[dependencies]\nserde = \"1\"\nkey = [1, 2]\n\n[[bin]]\nname = \"y\"\n";
    let (err, out) = outlined("Cargo.toml", src.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    let found = rows(&out);
    assert_eq!(found.len(), 3);
    assert_eq!(row(&found, "[package]").end, 4);
    assert_eq!(row(&found, "[dependencies]").end, 8);
    assert_eq!(row(&found, "[[bin]]").end, 10);
}

#[tokio::test]
async fn refuses_missing_binary_and_oversized_files() {
    let (err, out) = outlined("here.rs", b"fn a() {}\n", json!({ "path": "gone.rs" })).await;
    assert!(err && out.contains("no such file"), "{out}");
    let (err, out) = outlined("bin.rs", b"fn a() {}\0fn b() {}\n", json!({})).await;
    assert!(err && out.contains("binary"), "{out}");
    let big = vec![b'a'; MAX_OUTPUT_BYTES * 128 + 1];
    let (err, out) = outlined("big.rs", &big, json!({})).await;
    assert!(err && out.contains("larger than"), "{out}");
}

#[tokio::test]
async fn entry_cap_and_byte_cap_are_visible() {
    let src: String = (0..300).map(|i| format!("fn f{i}() {{}}\n")).collect();
    let (err, out) = outlined("many.rs", src.as_bytes(), json!({ "max_entries": 5 })).await;
    assert!(!err, "{out}");
    assert_eq!(rows(&out).len(), 5);
    assert!(
        out.contains("[truncated: showing 5 of 300 entries]"),
        "{out}"
    );

    let long: String = (0..200)
        .map(|i| {
            format!(
                "fn long_name_{i}({}a: i32) -> i32 {{ a }}\n",
                "x".repeat(120)
            )
        })
        .collect();
    let (err, out) = outlined("long.rs", long.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    let shown = rows(&out).len();
    assert!(shown < 200, "all {shown} entries fit, the cap never bit");
    assert!(
        out.contains(&format!(
            "[truncated: output capped at {MAX_OUTPUT_BYTES} bytes]"
        )),
        "{out}"
    );
    assert!(out.len() <= MAX_OUTPUT_BYTES + 96, "{} bytes", out.len());
}

#[tokio::test]
async fn plain_text_says_use_read() {
    let (err, out) = outlined(
        "notes.txt",
        "just prose, no code here\n".repeat(3).as_bytes(),
        json!({}),
    )
    .await;
    assert!(!err, "{out}");
    assert!(
        out.contains("(no symbols found in 3 lines; use read)"),
        "{out}"
    );
}

#[tokio::test]
async fn hardens_rust_java_js_python_markdown_and_toml_edge_cases() {
    // Rust: raw strings, pub(crate) fn, inline mod, multiline where, [u8; 32], extern "C"
    let rs = r##"const RAW: &str = r#"}"#;
pub mod inner {
    pub struct S;
    impl S {
        pub(crate) fn method(&self, buf: &[u8; 32]) {}
    }
}
pub extern "C" fn c_entry() {}
"##;
    let (err, out) = outlined("edge.rs", rs.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(out.contains("const RAW: &str = r#\"}\"#;"), "{out}");
    assert!(out.contains("pub mod inner"), "{out}");
    assert!(out.contains("pub struct S"), "{out}");
    assert!(
        out.contains("pub(crate) fn method(&self, buf: &[u8; 32])"),
        "{out}"
    );
    assert!(out.contains("pub extern \"C\" fn c_entry()"), "{out}");

    // Java: public class + method
    let java = "public class Foo {\n    public void bar() {\n    }\n}\n";
    let (err, out) = outlined("Foo.java", java.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(out.contains("1-4: public class Foo"), "{out}");
    assert!(out.contains("2-3: public void bar()"), "{out}");

    // C: struct field does not leak
    let c = "struct Node {\n    struct Node *next;\n};\n";
    let (err, out) = outlined("node.c", c.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(!out.contains("next"), "{out}");

    // JS: export const arrow, local .map(x =>) ignored
    let js = "export const handler = (x) => {\n  const ys = xs.map(y => y + 1);\n};\n";
    let (err, out) = outlined("app.js", js.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(out.contains("1-3: export const handler"), "{out}");
    assert!(!out.contains("ys"), "{out}");

    // Python: multiline def + asyncdef non-match
    let py = "asyncdef = 1\ndef foo(\n    a: int,\n) -> int:\n    return a\n";
    let (err, out) = outlined("m.py", py.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(!out.contains("asyncdef"), "{out}");
    assert!(out.contains("2-5: def foo("), "{out}");

    // Markdown: ### contains ####, 4-backtick fence
    let md = "### Parent\n#### Child\n````md\n```\n# NotHeading\n```\n````\n";
    let (err, out) = outlined("doc.md", md.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(out.contains("1-7: ### Parent"), "{out}");
    assert!(out.contains("2-7: #### Child"), "{out}");
    assert!(!out.contains("NotHeading"), "{out}");

    // TOML: inline comment + multiline array non-match
    let toml = "[deps] # runtime\narr = [\n  [1, 2]\n]\n";
    let (err, out) = outlined("Cargo.toml", toml.as_bytes(), json!({})).await;
    assert!(!err, "{out}");
    assert!(out.contains("1-4: [deps] # runtime"), "{out}");
    assert!(!out.contains("[1, 2]"), "{out}");
}

#[tokio::test]
async fn a_stopped_run_reports_the_stop() {
    let dir = tmp_dir("cancel.rs");
    fs::write(dir.join("cancel.rs"), "fn a() {}\n").unwrap();
    let cancel = Cancel::new(Arc::new(AtomicBool::new(true)));
    let result = OutlineTool::new(dir.clone())
        .execute(&json!({ "path": "cancel.rs" }), &cancel)
        .await;
    fs::remove_dir_all(&dir).ok();
    assert!(result.is_error);
    assert_eq!(result.output, "aborted by user");
}
