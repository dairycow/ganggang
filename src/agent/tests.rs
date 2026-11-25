//! Tests for [`super`]: live context and skill refresh through
//! `Agent::reload_context`.

use super::Agent;
use crate::agent::tools::Registry;
use crate::llm::client::LlmClient;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

fn new_agent(cwd: &Path) -> Agent {
    Agent::new(
        LlmClient::new("http://127.0.0.1:1").unwrap(),
        Registry::new(),
        "m".into(),
        64,
        Arc::new(AtomicBool::new(false)),
        cwd.to_path_buf(),
        None,
        false,
        131_072,
        Default::default(),
    )
}

fn write_skill(root: &Path, dir: &str, name: &str, description: &str) {
    let skill_dir = root.join(".agents/skills").join(dir);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\nbody\n"),
    )
    .unwrap();
}

/// A `/reload` re-discovers context files and skills created after the
/// agent was built: both reach the next system prompt, and the session's
/// identity (entries, session id, frozen prompt date) survives.
#[test]
fn reload_context_discovers_files_created_after_construction() {
    let dir = tempfile::tempdir().unwrap();
    let mut agent = new_agent(dir.path());
    let prompt_date = agent.prompt_date.clone();
    let session_id = agent.session_id.clone();
    assert!(
        !agent
            .system_prompt()
            .contains("reload marker: context rules"),
        "nothing to reload yet"
    );
    assert!(
        !agent.system_prompt().contains("a late skill"),
        "nothing to reload yet"
    );

    std::fs::write(
        dir.path().join("AGENTS.md"),
        "reload marker: context rules\n",
    )
    .unwrap();
    write_skill(dir.path(), "reload-test", "reload-test", "a late skill");

    let (agents_count, skills_count, _warnings) = agent.reload_context();
    assert!(agents_count >= 1, "the new AGENTS.md is discovered");
    assert!(skills_count >= 1, "the new skill is discovered");
    assert!(
        agent
            .agents_files
            .iter()
            .any(|f| f.content.contains("reload marker: context rules")),
        "the new AGENTS.md is in the set"
    );
    assert!(
        agent.skills().iter().any(|s| s.name == "reload-test"),
        "the new skill is in the set"
    );
    let prompt = agent.system_prompt();
    assert!(prompt.contains("reload marker: context rules"), "{prompt}");
    assert!(
        prompt.contains("a late skill"),
        "the skill is advertised: {prompt}"
    );
    assert_eq!(agent.prompt_date, prompt_date, "the frozen date survives");
    assert_eq!(agent.session_id, session_id, "the session id survives");
}

/// A second reload removes deleted files and skills from the prompt and
/// skill list, and replaces the previous discovery warnings instead of
/// re-surfacing them.
#[test]
fn reload_context_removes_files_and_clears_stale_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let mut agent = new_agent(dir.path());
    std::fs::write(
        dir.path().join("AGENTS.md"),
        "reload marker: removable rules\n",
    )
    .unwrap();
    // Two skills with one name: the first wins and the second warns.
    write_skill(
        dir.path(),
        "duplicate-a",
        "duplicate-reload",
        "duplicate description",
    );
    write_skill(
        dir.path(),
        "duplicate-b",
        "duplicate-reload",
        "duplicate description",
    );

    let (_, _, warnings) = agent.reload_context();
    assert!(
        warnings.iter().any(|w| w.contains("duplicate-reload")),
        "a duplicate skill name warns: {warnings:?}"
    );
    let prompt = agent.system_prompt();
    assert!(
        prompt.contains("reload marker: removable rules"),
        "{prompt}"
    );
    assert!(prompt.contains("duplicate description"), "{prompt}");

    std::fs::remove_file(dir.path().join("AGENTS.md")).unwrap();
    std::fs::remove_dir_all(dir.path().join(".agents")).unwrap();

    let (_, _, warnings) = agent.reload_context();
    assert!(
        !warnings.iter().any(|w| w.contains("duplicate-reload")),
        "the stale warning is cleared: {warnings:?}"
    );
    assert!(
        !agent.skills().iter().any(|s| s.name == "duplicate-reload"),
        "the deleted skill is gone"
    );
    let prompt = agent.system_prompt();
    assert!(
        !prompt.contains("reload marker: removable rules"),
        "{prompt}"
    );
    assert!(!prompt.contains("duplicate description"), "{prompt}");
}
