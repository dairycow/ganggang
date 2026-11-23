//! Tests for [`super`]: frontmatter parsing rules from spec §11.

use super::*;

const HAPPY: &str = "---\nname: pdf-tools\ndescription: Extract text and tables from PDFs.\n---\nRun scripts relative to this skill directory…\n";

#[test]
fn happy_path_parses_all_keys_and_the_body_is_kept_separate() {
    let front = parse_frontmatter(HAPPY).unwrap();
    assert_eq!(front.name, "pdf-tools");
    assert_eq!(front.description, "Extract text and tables from PDFs.");
    assert!(!front.disable_model_invocation);

    // load_skill stitches frontmatter and body back together.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("SKILL.md");
    std::fs::write(&path, HAPPY).unwrap();
    let skill = load_skill(&path).unwrap();
    assert_eq!(skill.name, "pdf-tools");
    assert_eq!(skill.path, path);
    assert_eq!(
        skill.instructions,
        "Run scripts relative to this skill directory…"
    );
}

#[test]
fn unknown_keys_are_ignored() {
    let text = "---\nname: a\ndescription: d\nversion: 3\nallowed-tools: [read]\n---\nbody";
    let front = parse_frontmatter(text).unwrap();
    assert_eq!(front.name, "a");
    assert_eq!(front.description, "d");
}

#[test]
fn missing_description_is_skipped() {
    let text = "---\nname: a\n---\nbody";
    assert!(parse_frontmatter(text).is_none());
}

#[test]
fn missing_name_is_skipped() {
    let text = "---\ndescription: d\n---\nbody";
    assert!(parse_frontmatter(text).is_none());
}

#[test]
fn empty_name_or_description_counts_as_missing() {
    let text = "---\nname: \ndescription: d\n---\nbody";
    assert!(parse_frontmatter(text).is_none());
}

#[test]
fn no_fences_is_skipped() {
    // Markdown that simply starts with prose, not a frontmatter block.
    assert!(parse_frontmatter("Just instructions, no fences.").is_none());
}

#[test]
fn unclosed_fence_is_skipped() {
    let text = "---\nname: a\ndescription: d\nbody keeps going";
    assert!(parse_frontmatter(text).is_none());
}

#[test]
fn fence_must_be_the_first_line() {
    let text = "# Title\n---\nname: a\ndescription: d\n---\n";
    assert!(parse_frontmatter(text).is_none());
}

#[test]
fn disable_model_invocation_parses_leniently() {
    for (raw, expected) in [
        ("true", true),
        ("True", true),
        ("TRUE", true),
        ("false", false),
        ("absent", false),
        ("garbage", false),
    ] {
        let text = match raw {
            "absent" => "---\nname: a\ndescription: d\n---\nb".to_string(),
            other => {
                format!("---\nname: a\ndescription: d\ndisable-model-invocation: {other}\n---\nb")
            }
        };
        assert_eq!(
            parse_frontmatter(&text).unwrap().disable_model_invocation,
            expected,
            "case {raw:?}"
        );
    }
}

#[test]
fn multiline_description_stays_on_one_value() {
    // Only the first `: ` splits; colons inside the value are literal.
    let text = "---\nname: a\ndescription: Use when: reading PDFs\n---\nb";
    assert_eq!(
        parse_frontmatter(text).unwrap().description,
        "Use when: reading PDFs"
    );
}

// ------------------------------------------------------------ discovery (M3.4)

use std::path::Path;

/// Make `<root>/repo/.agents/skills/<name>/SKILL.md` with frontmatter.
fn make_skill(root: &Path, segments: &[&str], name: &str) {
    let dir = segments
        .iter()
        .fold(root.to_path_buf(), |acc, s| acc.join(s));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: the {name} skill\n---\nDo {name} things.\n"),
    )
    .unwrap();
}

#[test]
fn scan_dir_finds_immediate_children_only_in_name_order() {
    let root = tempfile::tempdir().unwrap();
    let skills = root.path().join("skills");
    make_skill(root.path(), &["skills", "zeta"], "zeta");
    make_skill(root.path(), &["skills", "alpha"], "alpha");
    // A nested skill directory is NOT a skill at this level…
    make_skill(root.path(), &["skills", "zeta", "nested"], "nested");
    // …and neither is a standalone file.
    std::fs::write(
        skills.join("loose.md"),
        "---\nname: loose\ndescription: d\n---\nb",
    )
    .unwrap();

    let found = super::scan_dir(&skills);
    let names: Vec<String> = found
        .iter()
        .map(|p| {
            p.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        names,
        vec!["alpha", "zeta"],
        "immediate children, sorted, no nesting"
    );
}

#[test]
fn home_skills_come_first_and_win_collisions() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home").join(".agents").join("skills");
    std::fs::create_dir_all(&home).unwrap();
    make_skill(
        root.path().join("home").as_path(),
        &[".agents", "skills", "shared"],
        "shared",
    );
    make_skill(
        root.path().join("repo").as_path(),
        &[".agents", "skills", "shared"],
        "shared",
    );
    make_skill(
        root.path().join("repo").as_path(),
        &[".agents", "skills", "only-proj"],
        "only-proj",
    );

    let (skills, warnings) = super::discover_with_roots(Some(&home), &root.path().join("repo"));
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["shared", "only-proj"]);
    // The home copy won; the project copy was skipped with a warning.
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("already provided by"));
    assert!(skills[0].path.starts_with(root.path().join("home")));
}

#[test]
fn ancestor_walk_stops_at_the_repo_root() {
    let root = tempfile::tempdir().unwrap();
    // repo root (has .git) with a skills dir, a plain ancestor above it
    // (also with one — must be ignored), and cwd nested inside.
    std::fs::create_dir_all(root.path().join("repo/.git")).unwrap();
    make_skill(
        root.path().join("repo").as_path(),
        &[".agents", "skills", "in-repo"],
        "in-repo",
    );
    make_skill(root.path(), &[".agents", "skills", "outside"], "outside");
    make_skill(
        root.path().join("repo/sub/deep").as_path(),
        &[".agents", "skills", "deep"],
        "deep",
    );

    let (skills, _) = super::discover_with_roots(None, &root.path().join("repo/sub/deep"));
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["deep", "in-repo"],
        "cwd first, then up to the repo root inclusive"
    );
}

#[test]
fn without_a_repo_root_the_walk_reaches_the_fs_root_of_the_tree() {
    let root = tempfile::tempdir().unwrap();
    // No .git anywhere: every ancestor's .agents/skills counts.
    make_skill(
        root.path().join("a").as_path(),
        &[".agents", "skills", "top"],
        "top",
    );
    make_skill(
        root.path().join("a/b").as_path(),
        &[".agents", "skills", "mid"],
        "mid",
    );

    let (skills, _) = super::discover_with_roots(None, &root.path().join("a/b"));
    let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["mid", "top"]);
}

#[test]
fn invalid_skills_are_skipped_with_warnings_not_errors() {
    let root = tempfile::tempdir().unwrap();
    let skills = root.path().join(".agents").join("skills");
    std::fs::create_dir_all(skills.join("no-desc")).unwrap();
    std::fs::write(skills.join("no-desc/SKILL.md"), "---\nname: nod\n---\nbody").unwrap();
    make_skill(root.path(), &[".agents", "skills", "fine"], "fine");

    let (found, warnings) = super::discover_with_roots(None, root.path());
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "fine");
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("missing name or description"),
        "{warnings:?}"
    );
}

#[test]
fn crlf_skill_files_parse_without_panicking_or_drifting() {
    // The byte-offset bug this pins: str::lines strips \r but the old
    // length accounting did not, drifting one byte per line until a slice
    // landed inside a multibyte char (startup panic).
    let text = "---\r\nname: crlf\r\ndescription: qüick\r\n---\r\nbody\r\n";
    let front = parse_frontmatter(text).expect("CRLF frontmatter parses");
    assert_eq!(front.name, "crlf");
    assert_eq!(front.description, "qüick");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("SKILL.md");
    std::fs::write(&path, text).unwrap();
    let skill = load_skill(&path).expect("loads");
    assert_eq!(skill.instructions, "body");
}

#[test]
fn a_bom_ahead_of_the_fences_makes_the_file_fence_less() {
    let text = "\u{feff}---\nname: bom\ndescription: bombed\n---\nbody\n";
    assert!(parse_frontmatter(text).is_none(), "skipped, not mis-parsed");
}

#[test]
fn quoted_frontmatter_values_lose_their_quotes() {
    let text = "---\nname: \"quoted\"\ndescription: 'single quoted'\n---\nbody\n";
    let front = parse_frontmatter(text).unwrap();
    assert_eq!(front.name, "quoted");
    assert_eq!(front.description, "single quoted");
}

#[test]
fn outside_a_repo_the_home_skills_dir_is_not_scanned_twice() {
    let home = tempfile::tempdir().unwrap();
    let home_skills = home.path().join(".agents").join("skills");
    let dir = home_skills.join("alpha");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: alpha\ndescription: a\n---\nbody\n",
    )
    .unwrap();
    // cwd under HOME, deliberately WITHOUT .git anywhere up the tree.
    let work = home.path().join("work");
    std::fs::create_dir_all(&work).unwrap();

    let (skills, warnings) = super::discover_with_roots(Some(&home_skills), &work);
    assert_eq!(skills.len(), 1);
    assert!(
        warnings.is_empty(),
        "no self-collision warnings: {warnings:?}"
    );
}

#[test]
fn frontmatter_name_is_authoritative_over_the_directory_name() {
    let root = tempfile::tempdir().unwrap();
    // Directory says x, frontmatter says y: the skill is y (recorded in
    // the v2.1 changelog as a deliberate divergence from agentskills.io).
    let dir = root.path().join(".agents").join("skills").join("x");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: y\ndescription: d\n---\nbody\n",
    )
    .unwrap();

    let (skills, warnings) = super::discover_with_roots(None, root.path());
    assert!(warnings.is_empty());
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].name, "y");
    assert_eq!(skills[0].path, dir.join("SKILL.md"));
}

#[test]
fn disabled_skills_remain_in_the_discovered_set() {
    // §11: disable-model-invocation hides a skill from ADVERTISING only —
    // discovery keeps it so /skill:<name> forcing still works (M4).
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".agents").join("skills").join("priv");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        "---\nname: priv\ndescription: d\ndisable-model-invocation: true\n---\nbody\n",
    )
    .unwrap();

    let (skills, _) = super::discover_with_roots(None, root.path());
    assert_eq!(skills.len(), 1);
    assert!(skills[0].disable_model_invocation);
}
