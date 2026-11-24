//! Tests for [`super`]: layer merging, validation, allowlist resolution.

use super::*;

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

#[test]
fn nothing_set_leaves_defaults() {
    let cfg = merge(Config::default(), None, None, &[]).unwrap();
    assert_eq!(cfg, Config::default());
}

#[test]
fn the_default_allowlist_names_every_registered_tool() {
    // The allowlist, hook validation, and the registry share one canonical
    // name list (`agent::tools::NAMES`); registration is pinned to it.
    let expected: Vec<String> = crate::agent::tools::NAMES
        .iter()
        .map(|name| name.to_string())
        .collect();
    assert_eq!(Config::default().tools, expected);
}

#[test]
fn user_file_overrides_defaults() {
    let cfg = merge(
        Config::default(),
        Some("model = \"big-model\"\nmax_tokens = 8192\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.model, "big-model");
    assert_eq!(cfg.max_tokens, 8192);
    assert_eq!(cfg.base_url, Config::default().base_url);
}

#[test]
fn project_overrides_user() {
    let cfg = merge(
        Config::default(),
        Some("model = \"user-model\"\nmax_tokens = 1024\n"),
        Some("model = \"project-model\"\n"),
        &[],
    )
    .unwrap();
    assert_eq!(cfg.model, "project-model");
    assert_eq!(cfg.max_tokens, 1024, "unset project key keeps user value");
}

#[test]
fn env_overrides_files() {
    let cfg = merge(
        Config::default(),
        Some("model = \"user-model\"\n"),
        Some("model = \"project-model\"\nmax_tokens = 1024\n"),
        &env(&[("GG_MODEL", "env-model"), ("GG_MAX_TOKENS", "2048")]),
    )
    .unwrap();
    assert_eq!(cfg.model, "env-model");
    assert_eq!(cfg.max_tokens, 2048);
}

#[test]
fn wrong_typed_file_value_names_the_key() {
    let err = merge(
        Config::default(),
        Some("max_tokens = \"lots\"\n"),
        None,
        &[],
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("max_tokens"),
        "message should name the key: {msg}"
    );
}

#[test]
fn invalid_env_value_names_the_var() {
    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_MAX_TOKENS", "lots")]),
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("GG_MAX_TOKENS"),
        "message should name the var: {msg}"
    );
}

#[test]
fn unknown_allowlist_tool_is_an_error() {
    // "grep" graduated to a known tool in M7.1 — the unknown-name
    // example is now a name nothing will ever register.
    let err = merge(
        Config::default(),
        Some("tools = [\"read\", \"chainsaw\"]\n"),
        None,
        &[],
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("chainsaw"),
        "message should name the tool: {msg}"
    );
}

#[test]
fn unknown_file_keys_are_ignored() {
    let cfg = merge(
        Config::default(),
        Some("flavor = \"salty\"\nfuture_thing = { nested = true }\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.model, Config::default().model);
}

#[test]
fn empty_api_key_counts_as_unset() {
    let cfg = merge(Config::default(), Some("api_key = \"\"\n"), None, &[]).unwrap();
    assert_eq!(cfg.api_key, None);
}

#[test]
fn zero_numeric_settings_are_errors() {
    let err = merge(
        Config::default(),
        Some("bash_timeout_secs = 0\n"),
        None,
        &[],
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("bash_timeout_secs"));
}

#[test]
fn parse_env_direct_tests() {
    assert_eq!(parse_env(None, "X", 7).unwrap(), 7);
    assert_eq!(parse_env(Some("9".into()), "X", 7).unwrap(), 9);
    assert!(parse_env(Some("nine".into()), "X", 7).is_err());
}

#[test]
fn cli_allowlist_overrides_config_and_unknown_names_error() {
    let known = ["read", "bash", "edit", "write"];

    let cli = resolve_tools(
        Some(vec!["read".into(), "bash".into()]),
        vec!["edit".into()],
        &known,
    )
    .unwrap();
    assert_eq!(cli, vec!["read".to_string(), "bash".to_string()]);

    let from_cfg = resolve_tools(None, vec!["edit".into()], &known).unwrap();
    assert_eq!(from_cfg, vec!["edit".to_string()]);

    let err = resolve_tools(Some(vec!["grep".into()]), vec![], &known).unwrap_err();
    assert!(format!("{err:#}").contains("grep"));
}

#[test]
fn builtin_presets_and_alias_resolve() {
    let cfg = Config::default();
    assert_eq!(cfg.provider, "strata");
    for name in ["strata", "glm-coding-plan", "opencode-go", "deepseek"] {
        assert!(cfg.providers.contains_key(name), "missing preset {name}");
    }
    let mut cfg_alias = cfg.clone();
    cfg_alias.apply_cli(Some("zai-coding-plan"), None).unwrap();
    assert_eq!(cfg_alias.provider, "glm-coding-plan");
    assert_eq!(cfg_alias.model, "glm-5.3-flash");
}

#[test]
fn per_provider_layer_preserves_builtin_fields_and_resolves_env_keys() {
    let user = r#"
[providers.deepseek]
api_key = "ds-file-key"
"#;
    let cfg = merge(
        Config::default(),
        Some(user),
        None,
        &env(&[
            ("GG_PROVIDER", "deepseek"),
            ("DEEPSEEK_API_KEY", "ds-env-key"),
        ]),
    )
    .unwrap();
    assert_eq!(cfg.provider, "deepseek");
    assert_eq!(cfg.base_url, "https://api.deepseek.com");
    assert_eq!(cfg.model, "deepseek-flash");
    assert_eq!(cfg.replay_reasoning, ReplayReasoning::All);
    assert_eq!(
        cfg.api_key.as_deref(),
        Some("ds-file-key"),
        "file api_key wins over provider api_key_env fallback"
    );

    // Without file api_key, provider api_key_env fills it.
    let cfg2 = merge(
        Config::default(),
        None,
        None,
        &env(&[
            ("GG_PROVIDER", "deepseek"),
            ("DEEPSEEK_API_KEY", "ds-env-key"),
        ]),
    )
    .unwrap();
    assert_eq!(cfg2.api_key.as_deref(), Some("ds-env-key"));

    // GG_API_KEY overrides everything.
    let cfg3 = merge(
        Config::default(),
        Some(user),
        None,
        &env(&[("GG_PROVIDER", "deepseek"), ("GG_API_KEY", "gg-override")]),
    )
    .unwrap();
    assert_eq!(cfg3.api_key.as_deref(), Some("gg-override"));
}

#[test]
fn opencode_go_routes_openai_and_anthropic_models() {
    let mut cfg = Config::default();
    cfg.apply_cli(Some("opencode-go"), Some("deepseek-v4.1-flash"))
        .unwrap();
    assert_eq!(cfg.api, ApiKind::OpenAiCompletions);
    assert_eq!(cfg.base_url, "https://opencode.ai/inference/go/openai/v1");
    assert_eq!(cfg.replay_reasoning, ReplayReasoning::All);

    cfg.apply_cli(None, Some("opencode-go/claude-haiku-5-5"))
        .unwrap();
    assert_eq!(cfg.provider, "opencode-go");
    assert_eq!(cfg.model, "claude-haiku-5-5");
    assert_eq!(cfg.api, ApiKind::AnthropicMessages);
    assert_eq!(cfg.base_url, "https://opencode.ai/inference/go/anthropic");
    assert_eq!(cfg.replay_reasoning, ReplayReasoning::Never);
}

#[test]
fn conflicting_provider_and_prefix_errors_and_flat_file_does_not_leak() {
    let mut cfg = merge(
        Config::default(),
        Some("model = \"local-qwen\"\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.model, "local-qwen");

    // Switching via apply_cli to deepseek must NOT inherit local-qwen.
    cfg.apply_cli(Some("deepseek"), None).unwrap();
    assert_eq!(cfg.model, "deepseek-flash");

    let err = cfg
        .apply_cli(Some("deepseek"), Some("opencode-go/glm-5.3"))
        .unwrap_err();
    assert!(format!("{err:#}").contains("conflicts"));
}

#[test]
fn custom_provider_requires_base_url_and_model() {
    let good = r#"
[providers.local-vllm]
base_url = "http://127.0.0.1:9000/v1"
model = "llama-3"
"#;
    let cfg = merge(
        Config::default(),
        Some(good),
        None,
        &env(&[("GG_PROVIDER", "local-vllm")]),
    )
    .unwrap();
    assert_eq!(cfg.provider, "local-vllm");
    assert_eq!(cfg.model, "llama-3");
    assert!(cfg.active_endpoint().require_auth().is_ok());

    let bad = r#"
[providers.broken]
model = "x"
"#;
    let err = merge(Config::default(), Some(bad), None, &[]).unwrap_err();
    assert!(format!("{err:#}").contains("base_url"));
}

#[test]
fn resolve_switch_and_require_auth() {
    let cfg = Config::default();
    let ep = cfg.resolve_switch("deepseek").unwrap();
    assert_eq!(ep.label(), "deepseek/deepseek-flash");
    assert!(ep.require_auth().is_err());

    let ep2 = cfg.resolve_switch("claude-haiku-5-5").unwrap();
    assert_eq!(ep2.label(), "opencode-go/claude-haiku-5-5");

    let err = cfg.resolve_switch("glm-5.3").unwrap_err();
    assert!(format!("{err:#}").contains("multiple providers"));

    let adhoc = cfg.resolve_switch("my-custom-lora").unwrap();
    assert_eq!(adhoc.label(), "strata/my-custom-lora");
}

#[test]
fn strata_defaults_and_reasoning_controls_override_cleanly() {
    let def = Config::default();
    assert_eq!(def.max_tokens, 16_384);
    assert_eq!(def.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(def.reasoning_budget_tokens, Some(4096));

    // File layer 0 clears preset budget.
    let cleared = merge(
        Config::default(),
        Some("reasoning_budget_tokens = 0\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cleared.reasoning_budget_tokens, None);

    // Env overrides files and presets; empty effort or 0 budget clears them.
    let env_set = merge(
        Config::default(),
        None,
        None,
        &env(&[
            ("GG_REASONING_EFFORT", "low"),
            ("GG_THINKING_BUDGET", "2048"),
        ]),
    )
    .unwrap();
    assert_eq!(env_set.reasoning_effort.as_deref(), Some("low"));
    assert_eq!(env_set.reasoning_budget_tokens, Some(2048));

    let env_clear = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_REASONING_EFFORT", ""), ("GG_THINKING_BUDGET", "0")]),
    )
    .unwrap();
    assert_eq!(env_clear.reasoning_effort, None);
    assert_eq!(env_clear.reasoning_budget_tokens, None);

    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_THINKING_BUDGET", "nope")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("GG_THINKING_BUDGET"));

    // CLI overrides even after provider switch.
    let mut switched = Config::default();
    switched.apply_cli(Some("deepseek"), None).unwrap();
    switched.apply_reasoning_cli(Some("xhigh"), Some(8192));
    assert_eq!(switched.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(switched.reasoning_budget_tokens, Some(8192));
    switched.apply_reasoning_cli(Some(""), Some(0));
    assert_eq!(switched.reasoning_effort, None);
    assert_eq!(switched.reasoning_budget_tokens, None);

    let per_model_clear = merge(
        Config::default(),
        Some(
            r#"
[providers.strata]
models = [{ id = "qwen3.8-flash-next-iq3_s", reasoning_effort = "", reasoning_budget_tokens = 0 }]
"#,
        ),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(per_model_clear.reasoning_effort, None);
    assert_eq!(per_model_clear.reasoning_budget_tokens, None);
}

#[test]
fn replay_reasoning_compact_threshold_max_turns_and_explicit_ctx() {
    let def = Config::default();
    assert_eq!(def.replay_reasoning, ReplayReasoning::Turn);
    assert_eq!(def.compact_threshold_tokens, Some(65_536));
    assert!(!def.context_window_explicit);
    assert_eq!(def.max_turns, None);

    let cfg = merge(
        Config::default(),
        Some("replay_reasoning = \"all\"\ncompact_threshold_tokens = 0\nmax_turns = 25\ncontext_window = 262144\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.replay_reasoning, ReplayReasoning::All);
    assert_eq!(cfg.compact_threshold_tokens, None);
    assert_eq!(cfg.max_turns, Some(25));
    assert!(cfg.context_window_explicit);

    let bool_cfg = merge(
        Config::default(),
        Some("replay_reasoning = false\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(bool_cfg.replay_reasoning, ReplayReasoning::Never);

    assert!(
        merge(
            Config::default(),
            Some("replay_reasoning = \"bogus\"\n"),
            None,
            &[],
        )
        .is_err()
    );

    let env_cfg = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_COMPACT_THRESHOLD", "98304"), ("GG_MAX_TURNS", "40")]),
    )
    .unwrap();
    assert_eq!(env_cfg.compact_threshold_tokens, Some(98_304));
    assert_eq!(env_cfg.max_turns, Some(40));

    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_MAX_TURNS", "nope")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("GG_MAX_TURNS"));
}

#[test]
fn env_overrides_survive_apply_cli_and_resolve_switch() {
    let mut cfg = merge(
        Config::default(),
        None,
        None,
        &env(&[
            ("GG_BASE_URL", "http://127.0.0.1:9999/v1"),
            ("GG_MAX_TOKENS", "4096"),
            ("GG_API_KEY", "env-key"),
            ("GG_CONTEXT_WINDOW", "262144"),
            ("GG_REASONING_EFFORT", "high"),
            ("GG_THINKING_BUDGET", "1024"),
            ("GG_COMPACT_THRESHOLD", "0"),
        ]),
    )
    .unwrap();
    cfg.apply_cli(Some("deepseek"), Some("deepseek-v4-pro"))
        .unwrap();
    assert_eq!(cfg.provider, "deepseek");
    assert_eq!(cfg.model, "deepseek-v4-pro");
    assert_eq!(cfg.base_url, "http://127.0.0.1:9999/v1");
    assert_eq!(cfg.max_tokens, 4096);
    assert_eq!(cfg.api_key.as_deref(), Some("env-key"));
    assert_eq!(cfg.context_window, 262_144);
    assert!(cfg.context_window_explicit);
    assert_eq!(cfg.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(cfg.reasoning_budget_tokens, Some(1024));
    assert_eq!(cfg.compact_threshold_tokens, None);
    assert!(cfg.compact_threshold_explicit);

    let switched = cfg
        .resolve_switch("opencode-go/deepseek-v4.1-flash")
        .unwrap();
    assert_eq!(switched.api_key.as_deref(), Some("env-key"));
}

#[test]
fn model_explicit_and_discovered_catalog_and_switch() {
    let mut cfg = merge(
        Config::default(),
        Some("[providers.strata]\nmodel = \"from-file\"\n"),
        None,
        &[],
    )
    .unwrap();
    assert!(!cfg.model_explicit);

    let env_cfg = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_MODEL", "from-env")]),
    )
    .unwrap();
    assert!(env_cfg.model_explicit);

    cfg.apply_cli(Some("strata"), None).unwrap();
    assert!(!cfg.model_explicit);
    cfg.apply_cli(None, Some("forced-m")).unwrap();
    assert!(cfg.model_explicit);

    let mut discovered = BTreeMap::new();
    discovered.insert(
        "strata".to_string(),
        vec![crate::llm::discovery::DiscoveredModel {
            id: "inkling-2-mini-sq-15".into(),
            context_window: Some(32_768),
            status: Some("loaded".into()),
        }],
    );
    let rows = providers::catalog_with_discovered(&cfg.providers, &discovered);
    let strata_row = rows.iter().find(|r| r.provider == "strata").unwrap();
    assert_eq!(strata_row.model, "inkling-2-mini-sq-15");
    assert_eq!(strata_row.context_window, 32_768);
    assert_eq!(strata_row.status.as_deref(), Some("loaded"));

    let pending = vec![
        ("strata".to_string(), "inkling-2-mini-sq-15".to_string()),
        ("deepseek".to_string(), "shared-id".to_string()),
        ("opencode-go".to_string(), "shared-id".to_string()),
    ];
    let ep = cfg
        .resolve_switch_with_discovered("inkling-2-mini-sq-15", &pending)
        .unwrap();
    assert_eq!(ep.label(), "strata/inkling-2-mini-sq-15");
    assert!(ep.model_explicit);

    let err = cfg
        .resolve_switch_with_discovered("shared-id", &pending)
        .unwrap_err();
    assert!(format!("{err:#}").contains("multiple providers"));
}
