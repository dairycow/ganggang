use super::*;

async fn run(dir: &std::path::Path, args: Value) -> ToolResult {
    ReadTool::new(dir.to_path_buf())
        .execute(&args, &Cancel::none())
        .await
}

#[tokio::test]
async fn reads_a_file_with_line_numbers_and_respects_offset_and_limit() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "f.txt"})).await;
    assert!(!r.is_error);
    assert_eq!(r.output.trim(), "1| a\n2| b\n3| c\n4| d");

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": 2, "limit": 2}),
    )
    .await;
    assert_eq!(r.output.trim(), "2| b\n3| c");
}

#[tokio::test]
async fn offset_pages_cleanly_past_50_kib() {
    let dir = tempfile::tempdir().unwrap();
    // ~120 KiB file: 3000 lines * ~41 bytes each.
    let body: String = (1..=3000)
        .map(|i| format!("line-{i:04}-padding-xxxxxxxxxxxxxxxxxxxxxxxx\n"))
        .collect();
    assert!(body.len() > 100 * 1024);
    std::fs::write(dir.path().join("large.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "large.txt", "offset": 2500, "limit": 3}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output.trim(),
        "2500| line-2500-padding-xxxxxxxxxxxxxxxxxxxxxxxx\n\
         2501| line-2501-padding-xxxxxxxxxxxxxxxxxxxxxxxx\n\
         2502| line-2502-padding-xxxxxxxxxxxxxxxxxxxxxxxx"
    );
}

#[tokio::test]
async fn offset_past_eof_reports_line_count() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("f.txt"), "a\nb\n").unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": 10}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("past end of file (2 lines)"));
}

#[tokio::test]
async fn default_page_truncates_at_400_lines_with_a_notice() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..600).map(|i| format!("line{i}\n")).collect();
    std::fs::write(dir.path().join("big.txt"), body).unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "big.txt"})).await;
    assert!(!r.is_error);
    assert!(r.output.contains("400| line399"));
    assert!(!r.output.contains("401| line400"));
    assert!(r.output.contains("showing lines 1-400 of 600"));
    assert!(r.output.contains("offset=401"));
}

#[tokio::test]
async fn explicit_limit_pages_to_2000_lines() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (0..2500).map(|_| "x\n").collect();
    std::fs::write(dir.path().join("big.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "big.txt", "limit": 2000}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("2000| x"));
    assert!(!r.output.contains("2001| x"));
    assert!(r.output.contains("showing lines 1-2000 of 2500"));
    assert!(r.output.contains("offset=2001"));
}

#[tokio::test]
async fn truncates_at_24_kib_on_a_char_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let line = format!("{}\n", "é".repeat(60));
    std::fs::write(dir.path().join("wide.txt"), line.repeat(500)).unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "wide.txt"})).await;
    assert!(!r.is_error);
    assert!(r.output.contains("truncated"));
    assert!(
        r.output.len() < MAX_BYTES + 200,
        "byte capped: {}",
        r.output.len()
    );
}

#[tokio::test]
async fn long_lines_are_cut_at_500_chars() {
    let dir = tempfile::tempdir().unwrap();
    let long = "a".repeat(1500);
    std::fs::write(dir.path().join("min.txt"), format!("{long}\nshort\n")).unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "min.txt"})).await;
    assert!(!r.is_error);
    let first = r.output.lines().next().unwrap();
    assert!(first.ends_with('…'), "{}", first);
    assert!(
        first.chars().count() < 600,
        "one line cannot consume the page: {}",
        first.chars().count()
    );
    assert!(r.output.contains("2| short"));
}

#[tokio::test]
async fn negative_offset_reads_the_last_lines_with_exact_numbers() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=100).map(|i| format!("line{i}\n")).collect();
    std::fs::write(dir.path().join("f.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": -5}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(
        r.output.trim(),
        "96| line96\n97| line97\n98| line98\n99| line99\n100| line100"
    );
}

#[tokio::test]
async fn negative_offset_larger_than_the_file_reads_it_all() {
    let dir = tempfile::tempdir().unwrap();
    let body: String = (1..=3).map(|i| format!("line{i}\n")).collect();
    std::fs::write(dir.path().join("f.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": -1000}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert_eq!(r.output.trim(), "1| line1\n2| line2\n3| line3");
}

#[tokio::test]
async fn negative_offset_byte_cap_keeps_the_last_lines_and_reports_true_range() {
    let dir = tempfile::tempdir().unwrap();
    let pad = "x".repeat(380);
    let body: String = (1..=200).map(|i| format!("{i:03} {pad}\n")).collect();
    std::fs::write(dir.path().join("f.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "f.txt", "offset": -200}),
    )
    .await;
    assert!(!r.is_error, "{}", r.output);
    assert!(r.output.contains("200| "), "the final line must survive");
    assert!(
        r.output.len() < MAX_BYTES + 200,
        "byte capped: {}",
        r.output.len()
    );

    let lines: Vec<&str> = r.output.lines().collect();
    let note = lines.last().copied().unwrap();
    let content = &lines[..lines.len() - 1];
    let first = content[0].split('|').next().unwrap().trim();
    let last = content.last().unwrap().split('|').next().unwrap().trim();
    assert_eq!(
        note,
        format!(
            "[truncated: byte cap {MAX_BYTES} reached; showing last {} lines ({first}-{last})]",
            content.len()
        )
    );
}

#[tokio::test]
async fn negative_offset_on_endless_file_returns_scan_cap_error() {
    let r = run(
        std::path::Path::new("/dev"),
        serde_json::json!({"path": "zero", "offset": -10}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("scan cap"), "{}", r.output);
}

#[tokio::test]
async fn missing_file_is_an_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(dir.path(), serde_json::json!({"path": "nope.txt"})).await;
    assert!(r.is_error);
    assert!(r.output.contains("cannot read"));
}

#[tokio::test]
async fn an_endless_file_returns_the_cap_quickly_instead_of_hanging() {
    let r = run(
        std::path::Path::new("/dev"),
        serde_json::json!({"path": "zero"}),
    )
    .await;
    assert!(!r.is_error);
    assert!(r.output.contains("truncated"));
}

#[tokio::test]
async fn eof_line_without_trailing_newline_is_not_double_counted() {
    let dir = tempfile::tempdir().unwrap();
    let mut body: String = (1..=2000).map(|i| format!("l{i}\n")).collect();
    body.push_str("l2001");
    std::fs::write(dir.path().join("no_nl.txt"), body).unwrap();

    let r = run(
        dir.path(),
        serde_json::json!({"path": "no_nl.txt", "limit": 2000}),
    )
    .await;
    assert!(!r.is_error);
    assert!(
        r.output.contains("showing lines 1-2000 of 2001"),
        "{}",
        r.output.lines().last().unwrap_or("")
    );
}

#[tokio::test]
async fn corrupt_utf8_at_eof_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("bad.txt"), b"ok\n\xc3").unwrap();

    let r = run(dir.path(), serde_json::json!({"path": "bad.txt"})).await;
    assert!(r.is_error);
    assert!(r.output.contains("not valid UTF-8"));
}

#[tokio::test]
async fn endless_file_with_offset_stops_bounded_without_loop_hint() {
    let r = run(
        std::path::Path::new("/dev"),
        serde_json::json!({"path": "zero", "offset": 10}),
    )
    .await;
    assert!(r.is_error);
    assert!(r.output.contains("scan cap"));
    assert!(!r.output.contains("offset=10"));
}
