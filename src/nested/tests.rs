use std::collections::BTreeMap;

use super::*;

/// A nest read from a literal table of variables.
fn boot(vars: &[(&str, &str)]) -> anyhow::Result<Option<NestedBoot>> {
    NestedBoot::from_env(|name| {
        vars.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.to_string())
    })
}

#[test]
fn without_depth_nothing_is_nested() {
    // A stray variable from some other run does not make a nest.
    let got = boot(&[(PARENT_SESSION, "parent-1")]).unwrap();
    assert_eq!(got, None);
    assert_eq!(boot(&[(DEPTH, "  ")]).unwrap(), None);
}

#[test]
fn depth_must_be_a_positive_integer() {
    for raw in ["0", "two", "-1", "99999999999"] {
        let err = boot(&[(DEPTH, raw)]).unwrap_err().to_string();
        assert!(err.contains("must be a positive integer"), "{raw:?}: {err}");
    }
}

#[test]
fn a_nested_run_cannot_start_a_nested_run() {
    let err = boot(&[(DEPTH, "2")]).unwrap_err().to_string();
    assert!(err.contains("cannot start a nested run"), "{err}");
    assert_eq!(MAX_DEPTH, 1);
}

#[test]
fn exports_round_trip_through_a_nested_boot() {
    let env = ParentEnv {
        depth: 0,
        session: Some("parent-1".into()),
        model: Some("provider/model".into()),
        tool_cap: Some(vec!["read".into(), "bash".into()]),
        max_turns_cap: Some(12),
        max_seconds_cap: Some(300),
        hooks_off: true,
    };
    let vars: Vec<(String, String)> = env
        .exports()
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.expect("every variable set")))
        .collect();
    let got = boot(
        &vars
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>(),
    )
    .unwrap()
    .expect("nested");
    assert_eq!(
        got,
        NestedBoot {
            depth: 1,
            parent_session: Some("parent-1".into()),
            parent_model: Some("provider/model".into()),
            tool_cap: vec!["read".into(), "bash".into()],
            max_turns_cap: Some(12),
            max_seconds_cap: Some(300),
            hooks_off: true,
        }
    );
}

#[test]
fn an_empty_parent_is_only_a_depth_marker() {
    let vars: BTreeMap<&str, Option<String>> = ParentEnv::default().exports().into_iter().collect();
    // The one value that is always set.
    assert_eq!(vars[DEPTH], Some("1".into()));
    for name in [
        PARENT_SESSION,
        PARENT_MODEL,
        TOOL_CAP,
        MAX_TURNS_CAP,
        MAX_SECONDS_CAP,
        NO_HOOKS,
    ] {
        assert_eq!(vars[name], None, "{name} must be removed, not inherited");
    }
    let got = boot(&[(DEPTH, "1")])
        .unwrap()
        .expect("depth alone is a nest");
    assert_eq!(got.depth, 1);
    assert_eq!(got.tool_cap, Vec::<String>::new());
    assert_eq!(got.max_turns_cap, None);
    assert!(!got.hooks_off);
}

#[test]
fn every_variable_is_exported_exactly_once() {
    let exported: Vec<&str> = ParentEnv::default()
        .exports()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    let mut unique = exported.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(exported.len(), unique.len(), "{exported:?}");
    for name in [
        DEPTH,
        PARENT_SESSION,
        PARENT_MODEL,
        TOOL_CAP,
        MAX_TURNS_CAP,
        NO_HOOKS,
    ] {
        assert!(exported.contains(&name), "{name} is never exported");
    }
}

#[test]
fn a_nested_run_passes_its_own_depth_to_the_next_run() {
    let env = ParentEnv {
        depth: 1,
        ..ParentEnv::default()
    };
    assert_eq!(env.exports()[0], (DEPTH, Some("2".into())));
}

#[test]
fn blank_values_read_as_absent_and_zero_turns_as_no_cap() {
    let got = boot(&[
        (DEPTH, "1"),
        (PARENT_SESSION, ""),
        (PARENT_MODEL, " "),
        (TOOL_CAP, " read, bash ,,read "),
        (MAX_TURNS_CAP, "0"),
        (NO_HOOKS, "yes"),
    ])
    .unwrap()
    .expect("nested");
    assert_eq!(got.parent_session, None);
    assert_eq!(got.parent_model, None);
    assert_eq!(got.tool_cap, vec!["read".to_string(), "bash".to_string()]);
    assert_eq!(got.max_turns_cap, None);
    assert!(!got.hooks_off, "only 1 switches hooks off");
}

#[test]
fn a_malformed_turn_cap_fails_the_boot() {
    let err = boot(&[(DEPTH, "1"), (MAX_TURNS_CAP, "many")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("must be a non-negative integer"), "{err}");
}

#[test]
fn a_malformed_time_cap_fails_the_boot() {
    let err = boot(&[(DEPTH, "1"), (MAX_SECONDS_CAP, "soon")])
        .unwrap_err()
        .to_string();
    assert!(err.contains("must be a non-negative integer"), "{err}");
    let nest = boot(&[(DEPTH, "1"), (MAX_SECONDS_CAP, "0")])
        .unwrap()
        .expect("nested");
    assert_eq!(nest.max_seconds_cap, None, "0 is no cap");
}

#[test]
fn a_named_tool_outside_the_parent_allowlist_fails_the_boot() {
    let nest = boot(&[(DEPTH, "1"), (TOOL_CAP, "read,bash")])
        .unwrap()
        .expect("nested");
    let inside = vec!["bash".to_string(), "read".to_string()];
    assert_eq!(
        nest.narrow_tools(Some(&inside), inside.clone()).unwrap(),
        inside
    );
    let outside = vec!["read".to_string(), "edit".to_string()];
    let err = nest
        .narrow_tools(Some(&outside), outside.clone())
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("--tools edit is outside the parent run's allowlist"),
        "{err}"
    );
}

#[test]
fn without_a_flag_the_resolved_list_keeps_only_the_parents_tools() {
    let nest = boot(&[(DEPTH, "1"), (TOOL_CAP, "read,grep")])
        .unwrap()
        .expect("nested");
    let resolved = vec!["read".into(), "bash".into(), "grep".into(), "edit".into()];
    assert_eq!(
        nest.narrow_tools(None, resolved).unwrap(),
        vec!["read".to_string(), "grep".to_string()]
    );
}

#[test]
fn a_parent_with_no_tools_leaves_a_nested_run_with_no_tools() {
    let nest = boot(&[(DEPTH, "1"), (TOOL_CAP, "")])
        .unwrap()
        .expect("nested");
    assert_eq!(nest.tool_cap, Vec::<String>::new());
    let resolved = vec!["read".to_string()];
    assert_eq!(
        nest.narrow_tools(None, resolved.clone()).unwrap(),
        Vec::<String>::new()
    );
    assert!(
        nest.narrow_tools(Some(&resolved), resolved.clone())
            .is_err()
    );
}

#[test]
fn the_turn_budget_can_only_go_down() {
    let capped = NestedBoot {
        depth: 1,
        parent_session: None,
        parent_model: None,
        tool_cap: Vec::new(),
        max_turns_cap: Some(10),
        max_seconds_cap: None,
        hooks_off: false,
    };
    assert_eq!(capped.effective_max_turns(Some(4)), Some(4));
    assert_eq!(capped.effective_max_turns(Some(40)), Some(10));
    assert_eq!(capped.effective_max_turns(None), Some(10));

    let uncapped = NestedBoot {
        max_turns_cap: None,
        ..capped.clone()
    };
    assert_eq!(uncapped.effective_max_turns(Some(40)), Some(40));
    assert_eq!(uncapped.effective_max_turns(None), None);

    let capped = NestedBoot {
        max_seconds_cap: Some(60),
        ..capped.clone()
    };
    assert_eq!(capped.effective_max_seconds(Some(30)), Some(30));
    assert_eq!(capped.effective_max_seconds(Some(600)), Some(60));
    assert_eq!(capped.effective_max_seconds(None), Some(60));
    assert_eq!(uncapped.effective_max_seconds(Some(600)), Some(600));
    assert_eq!(uncapped.effective_max_seconds(None), None);
}

#[test]
fn a_nested_run_boots_with_its_own_bash_ceiling() {
    let nest = boot(&[(DEPTH, "1")]).unwrap().expect("nested");
    // The ceiling comes from the nested run's own config, whatever the
    // person's spinner-sized `bash_timeout_secs` says.
    assert_eq!(nest.effective_bash_timeout_secs(30, 600), 600);
    assert_eq!(nest.effective_bash_timeout_secs(600, 45), 45);
}

#[test]
fn the_shared_nest_follows_a_model_switch() {
    let nest = ParentEnv::default().shared();
    assert_eq!(current(&nest).model, None);
    set_model(&nest, "provider/model".into());
    let exports: BTreeMap<&str, Option<String>> = current(&nest).exports().into_iter().collect();
    assert_eq!(exports[PARENT_MODEL], Some("provider/model".into()));
    assert_eq!(exports[DEPTH], Some("1".into()));
}
