//! Tests for [`super`]: section order and content of the assembled prompt,
//! and (M3.2) AGENTS.md discovery.

use std::path::{Path, PathBuf};

use super::*;

#[test]
fn sections_appear_in_spec_order_with_no_files_or_skills() {
    let prompt = build_system_prompt(Path::new("/tmp/proj"), &[], &[]);

    let preamble_at = prompt.find("You are gg").expect("preamble first");
    let workdir_at = prompt
        .find("# Working directory")
        .expect("working dir last");
    assert!(preamble_at < workdir_at);
    assert!(prompt.contains("Ground every claim in tool output"));
    assert!(prompt.contains("never `cd <cwd> &&`"));
    assert!(prompt.contains("Every tool result stays in context"));

    // Absent inputs omit their sections entirely — no placeholder headers,
    // and tools are never duplicated outside the API tools array.
    assert!(!prompt.contains("# Tools"));
    assert!(!prompt.contains("AGENTS.md"));
    assert!(!prompt.contains("Skills"));
}

#[test]
fn working_dir_section_names_the_cwd_and_today() {
    let cwd = Path::new("/home/user/Projects/ganggang");
    let prompt = build_system_prompt(cwd, &[], &[]);
    let section = prompt
        .split("# Working directory")
        .nth(1)
        .expect("section present");
    assert!(section.contains("/home/user/Projects/ganggang"));
    let today = time::OffsetDateTime::now_utc();
    assert!(section.contains(&format!(
        "date: {:04}-{:02}",
        today.year(),
        u8::from(today.month())
    )));
}

#[test]
fn context_files_render_with_their_origin_and_content_root_first() {
    let files = [
        AgentsFile {
            origin: "~/.gg/AGENTS.md".to_string(),
            content: "global rules".to_string(),
        },
        AgentsFile {
            origin: "AGENTS.md".to_string(),
            content: "repo rules".to_string(),
        },
    ];
    let prompt = build_system_prompt(Path::new("/tmp/p"), &files, &[]);
    let section = prompt
        .split("# Context files")
        .nth(1)
        .expect("context section present");
    let global_at = section.find("global rules").unwrap();
    let repo_at = section.find("repo rules").unwrap();
    assert!(global_at < repo_at, "broadest file first");
    assert!(section.contains("### ~/.gg/AGENTS.md"));
    // The context section sits between preamble and working directory.
    let ctx_at = prompt.find("# Context files").unwrap();
    assert!(prompt.find("You are gg").unwrap() < ctx_at);
    assert!(ctx_at < prompt.find("# Working directory").unwrap());
}

fn touch(path: &std::path::Path, content: &str) {
    std::fs::write(path, content).unwrap();
}

#[test]
fn discovery_orders_home_first_then_root_first() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".gg")).unwrap();
    touch(&home.path().join(".gg/AGENTS.md"), "global");

    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    std::fs::create_dir(root.join(".git")).unwrap();
    touch(&root.join("AGENTS.md"), "repo");
    let nested = root.join("src").join("deep");
    std::fs::create_dir_all(&nested).unwrap();
    touch(&nested.join("AGENTS.md"), "deep");

    let (files, warnings) = super::discover_with_home(&nested, Some(home.path()));
    let contents: Vec<&str> = files.iter().map(|f| f.content.as_str()).collect();
    assert_eq!(contents, vec!["global", "repo", "deep"]);
    assert!(warnings.is_empty());
    // Origins are repo-relative so the model can tell them apart.
    assert_eq!(files[1].origin, "AGENTS.md");
    assert_eq!(files[2].origin, "src/deep/AGENTS.md");
}

#[test]
fn discovery_without_any_files_yields_nothing() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let (files, warnings) = super::discover_with_home(repo.path(), Some(home.path()));
    assert!(files.is_empty());
    assert!(warnings.is_empty());
}

#[test]
#[cfg(unix)]
fn an_unreadable_agents_md_is_skipped_with_a_warning_not_silently() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let path = repo.path().join("AGENTS.md");
    std::fs::write(&path, "locked").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

    let (files, warnings) = super::discover_with_home(repo.path(), Some(home.path()));
    assert!(files.is_empty(), "unreadable context is skipped");
    assert_eq!(warnings.len(), 1, "…but never silently: {warnings:?}");
    assert!(warnings[0].contains("AGENTS.md"));
}

#[test]
fn without_a_git_root_the_fs_root_governs() {
    // No .git anywhere: root falls back to "/" and every AGENTS.md from
    // there to cwd is in scope. Building a real /-rooted tree is not
    // possible, so probe the root finder and the between-walk directly.
    let repo = tempfile::tempdir().unwrap();
    assert_eq!(super::find_repo_root(repo.path()), None);
    let between = super::agents_md_between(&PathBuf::from("/"), repo.path());
    // Every ancestor dir of cwd was a candidate; only existing AGENTS.md
    // files survive the filter.
    assert!(
        between
            .iter()
            .all(|p| p.file_name().map(|n| n == "AGENTS.md").unwrap_or(false))
    );
}

#[test]
fn repo_root_is_the_innermost_git_dir() {
    let outer = tempfile::tempdir().unwrap();
    std::fs::create_dir(outer.path().join(".git")).unwrap();
    let inner = outer.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::create_dir(inner.join(".git")).unwrap();

    assert_eq!(super::find_repo_root(&inner), Some(inner));
}

#[test]
fn skills_advertise_name_description_path_and_hide_disabled() {
    use crate::agent::skills::Skill;
    let skill = |name: &str, disabled: bool| Skill {
        name: name.into(),
        description: format!("does {name} things"),
        disable_model_invocation: disabled,
        path: PathBuf::from(format!("/skills/{name}/SKILL.md")),
        instructions: "body".into(),
    };

    let prompt = build_system_prompt(
        Path::new("/tmp/p"),
        &[],
        &[skill("pdf-tools", false), skill("hidden", true)],
    );

    let skills_at = prompt.find("# Skills").expect("skills section present");
    let workdir_at = prompt.find("# Working directory").expect("workdir");
    assert!(
        prompt.find("You are gg").unwrap() < skills_at && skills_at < workdir_at,
        "§12 order"
    );
    assert!(prompt.contains("pdf-tools: does pdf-tools things"));
    assert!(prompt.contains("/skills/pdf-tools/SKILL.md"));
    assert!(
        prompt.contains("Read a matching skill's SKILL.md"),
        "load instructions"
    );
    assert!(
        !prompt.contains("hidden"),
        "disabled skills are not advertised"
    );
}

#[test]
fn build_system_prompt_with_date_pins_the_supplied_date() {
    let prompt = build_system_prompt_with_date(Path::new("/tmp/p"), &[], &[], "2026-01-02");
    assert!(prompt.contains("date: 2026-01-02"));
}

/// Wave 3: the preamble batches reads, sends the model to outline/grep
/// before a big read, and re-reads after anything that moves line numbers.
#[test]
fn preamble_batches_reads_and_re_reads_after_compaction_or_formatting() {
    let prompt = build_system_prompt(Path::new("/tmp/proj"), &[], &[]);
    assert!(
        prompt.contains("batch every independent read-only call"),
        "{prompt}"
    );
    assert!(
        prompt.contains("(outline, grep, read, find, ls)"),
        "{prompt}"
    );
    assert!(prompt.contains("over ~200 lines"), "{prompt}");
    assert!(prompt.contains("`files_only: true`"), "{prompt}");
    assert!(
        prompt.contains("re-read the target lines before calling edit"),
        "{prompt}"
    );
}
