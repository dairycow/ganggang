//! Tests for [`super`]: layer merging, validation, allowlist resolution.

use super::*;

fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(n, v)| (n.to_string(), v.to_string()))
        .collect()
}

/// The first ranked choice's endpoint, read from the presets rather than
/// named, so these tests follow a `default_provider` change in `presets.toml`.
fn shipped_default() -> Endpoint {
    let providers = providers::builtin_providers();
    providers[&providers::default_provider()].resolve(None)
}

/// Every provider's key variable, set: enough for the first ranked choice
/// to be usable, so a test can say "nothing else set" and mean it.
fn all_keys() -> Vec<(String, String)> {
    providers::builtin_providers()
        .values()
        .filter_map(|p| p.api_key_env.clone())
        .map(|var| (var, "test-key".to_string()))
        .collect()
}

/// A provider shaped for the ranking tests: `key_env` names the variable it
/// needs, or `None` when it needs no key.
fn ranked_provider(name: &str, key_env: Option<&str>) -> ProviderConfig {
    let mut p = providers::builtin_providers()[&providers::default_provider()].clone();
    p.name = name.into();
    p.api_key = None;
    p.api_key_env = key_env.map(str::to_string);
    p.default_model = format!("{name}-model");
    p
}

/// A shipped provider other than the default, for tests that must not
/// touch the active one.
fn other_provider() -> String {
    let default = providers::default_provider();
    providers::builtin_providers()
        .keys()
        .find(|name| **name != default)
        .expect("at least two presets")
        .clone()
}

#[test]
fn nothing_set_leaves_defaults() {
    // Keys alone name no provider: holding every one keeps the first choice,
    // and apart from the keys themselves nothing differs from the bare
    // default.
    let head_key = providers::builtin_providers()[&providers::default_provider()]
        .api_key_env
        .is_some()
        .then(|| "test-key".to_string());
    let mut expected = Config {
        api_key: head_key,
        ..Config::default()
    };
    for provider in expected.providers.values_mut() {
        if provider.api_key_env.is_some() {
            provider.api_key = Some("test-key".into());
        }
    }
    let cfg = merge(Config::default(), None, None, &all_keys()).unwrap();
    assert_eq!(cfg, expected);
}

#[test]
fn pick_ranked_takes_the_first_usable_and_names_what_it_skipped() {
    let cloud = ranked_provider("cloud", Some("CLOUD_KEY"));
    let local = ranked_provider("local", None);
    let other = ranked_provider("other", Some("OTHER_KEY"));
    // What `merge` makes of a ranked list: the chosen provider and what was
    // passed over, or `None` when nothing is usable.
    let rank = |present: &[&ProviderConfig], generic_key, own_server| {
        pick_ranked(present, |p| usable(p, generic_key, own_server)).map(|(chosen, skipped)| {
            (
                chosen.name.clone(),
                skipped
                    .iter()
                    .map(|p| passed_over(p, own_server))
                    .collect::<Vec<_>>(),
            )
        })
    };

    // The first choice needs no key: it stands, with nothing passed over.
    assert_eq!(
        rank(&[&local, &cloud], false, false),
        Some(("local".into(), vec![]))
    );

    // A key held for the first choice: it stands.
    let mut keyed = cloud.clone();
    keyed.api_key = Some("k".into());
    assert_eq!(
        rank(&[&keyed, &local], false, false),
        Some(("cloud".into(), vec![]))
    );

    // `GG_API_KEY` serves whichever provider is active.
    assert_eq!(
        rank(&[&cloud, &local], true, false),
        Some(("cloud".into(), vec![]))
    );

    // No key for the first: the next usable one, with every provider passed
    // over and the variable to set.
    assert_eq!(
        rank(&[&cloud, &other, &local], false, false),
        Some((
            "local".into(),
            vec!["cloud (no CLOUD_KEY)".into(), "other (no OTHER_KEY)".into()]
        ))
    );

    // A `base_url` override: a provider that holds a key of its own does not
    // follow it, whether held, attached by the config, or covered by a key
    // given for the endpoint. One that holds none does.
    assert_eq!(
        rank(&[&keyed, &local], false, true),
        Some((
            "local".into(),
            vec!["cloud (base_url is set and no provider is named)".into()]
        ))
    );
    assert_eq!(
        rank(&[&cloud, &local], true, true),
        Some((
            "local".into(),
            vec!["cloud (base_url is set and no provider is named)".into()]
        ))
    );
    let mut attached = local.clone();
    attached.api_key = Some("k".into());
    assert_eq!(rank(&[&keyed, &attached], false, true), None);

    // Nothing usable: nothing is picked, and the caller decides.
    assert_eq!(rank(&[&cloud, &other], false, false), None);
    assert_eq!(rank(&[&keyed], false, true), None);
}

#[test]
fn ranked_default_follows_the_keys_when_nothing_names_a_provider() {
    let ranked = providers::default_provider_order();
    let all = providers::builtin_providers();

    // No key anywhere: the first choice that needs none.
    let cfg = merge(Config::default(), None, None, &[]).unwrap();
    let expected = ranked
        .iter()
        .find(|name| all[*name].api_key_env.is_none())
        .unwrap_or(&ranked[0]);
    assert_eq!(cfg.provider, *expected);
    let passed_over: Vec<String> = ranked
        .iter()
        .take_while(|name| *name != expected)
        .map(|name| {
            let var = all[name].api_key_env.as_deref().unwrap();
            format!("{name} (no {var})")
        })
        .collect();
    let expected_note = (!passed_over.is_empty()).then(|| {
        format!(
            "passed over {}: using {}/{}",
            passed_over.join(", "),
            expected,
            all[expected].default_model
        )
    });
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), expected_note);

    // Every key held: the first choice, with nothing to report.
    let cfg = merge(Config::default(), None, None, &all_keys()).unwrap();
    assert_eq!(cfg.provider, ranked[0]);
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);

    // `GG_API_KEY`, and the flat `api_key` key, serve whichever provider is
    // active.
    let cfg = merge(Config::default(), None, None, &env(&[("GG_API_KEY", "k")])).unwrap();
    assert_eq!(cfg.provider, ranked[0]);
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);
    let cfg = merge(Config::default(), Some("api_key = \"k\"\n"), None, &[]).unwrap();
    assert_eq!(cfg.provider, ranked[0], "a flat api_key is a held key");
    assert_eq!(cfg.api_key.as_deref(), Some("k"));
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);

    // A key in the provider's own table is a held key too.
    let table = format!("[providers.{0}]\napi_key = \"from-table\"\n", ranked[0]);
    let cfg = merge(Config::default(), Some(&table), None, &[]).unwrap();
    assert_eq!(cfg.provider, ranked[0]);
    assert_eq!(cfg.api_key.as_deref(), Some("from-table"));
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);

    // A blank `GG_API_KEY` is no key at all.
    let blank = merge(Config::default(), None, None, &env(&[("GG_API_KEY", "  ")])).unwrap();
    let none = merge(Config::default(), None, None, &[]).unwrap();
    assert_eq!(blank.provider, none.provider);
    assert_eq!(
        blank.fallback_note(&blank.active_endpoint()),
        none.fallback_note(&none.active_endpoint())
    );
}

#[test]
fn naming_a_provider_outranks_the_ranked_default() {
    let head = providers::default_provider();
    // An explicit `provider` stands without its key: the auth check, not
    // the ranking, answers for it.
    let cfg = merge(
        Config::default(),
        Some(&format!("provider = \"{head}\"\n")),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.provider, head);
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);
    if providers::builtin_providers()[&head].api_key_env.is_some() {
        assert!(
            cfg.active_endpoint().require_auth().is_err(),
            "a named provider without its key fails the auth check"
        );
    }

    // The environment outranks the ranking too, notice included.
    let other = other_provider();
    let cfg = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_PROVIDER", other.as_str())]),
    )
    .unwrap();
    assert_eq!(cfg.provider, other);
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);

    // A bare `model` names no provider: it lands on the ranked pick, and
    // whatever was passed over is still passed over.
    let cfg = merge(Config::default(), Some("model = \"my-model\"\n"), None, &[]).unwrap();
    let picked = merge(Config::default(), None, None, &[]).unwrap();
    assert_eq!(cfg.provider, picked.provider);
    assert_eq!(cfg.model, "my-model");
    assert_eq!(cfg.fallback_skipped, picked.fallback_skipped);
}

#[test]
fn ranking_passes_over_a_first_choice_without_a_key() {
    let ranked = providers::default_provider_order();
    assert!(
        ranked.len() >= 2,
        "these presets rank at least two providers"
    );
    let (head, next) = (ranked[0].clone(), ranked[1].clone());
    // The first choice needs a key nobody holds and every other key is
    // held, so the ranking must pass over it whatever the presets say.
    let patch = format!("[providers.{head}]\napi_key_env = \"RANK_TEST_KEY\"\n");
    let picked = providers::builtin_providers()[&next].default_model.clone();
    let cfg = merge(Config::default(), Some(&patch), None, &all_keys()).unwrap();
    assert_eq!(cfg.provider, next);
    let passed_over = format!("{head} (no RANK_TEST_KEY)");
    let expected = format!("passed over {passed_over}: using {next}/{picked}");
    assert_eq!(
        cfg.fallback_note(&cfg.active_endpoint()).as_deref(),
        Some(expected.as_str())
    );

    // The file's flat keys follow the pick. The value is one no preset ships.
    let with_flat = format!("max_tokens = 4321\n{patch}");
    let cfg = merge(Config::default(), Some(&with_flat), None, &all_keys()).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.max_tokens, 4321);

    // A bare `model` lands on the pick, and the notice names the model now
    // in use rather than the provider's own default.
    let with_model = format!("model = \"mine\"\n{patch}");
    let cfg = merge(Config::default(), Some(&with_model), None, &all_keys()).unwrap();
    assert_eq!(cfg.model, "mine");
    let expected_mine = format!("passed over {passed_over}: using {next}/mine");
    assert_eq!(
        cfg.fallback_note(&cfg.active_endpoint()).as_deref(),
        Some(expected_mine.as_str())
    );

    // The notice follows the endpoint it is given, so a model a local
    // server's probe adopted is the one it names.
    let mut probed = cfg.active_endpoint();
    probed.model = "probe-model".into();
    let expected_probed = format!("passed over {passed_over}: using {next}/probe-model");
    assert_eq!(
        cfg.fallback_note(&probed).as_deref(),
        Some(expected_probed.as_str())
    );

    // A flag that names the provider ends the notice: `-P`, or a model
    // carrying its provider.
    let mut flagged = merge(Config::default(), Some(&patch), None, &all_keys()).unwrap();
    assert_eq!(
        flagged.fallback_note(&flagged.active_endpoint()).as_deref(),
        Some(expected.as_str())
    );
    flagged.apply_cli(Some(&head), None).unwrap();
    assert_eq!(flagged.provider, head);
    assert_eq!(flagged.fallback_note(&flagged.active_endpoint()), None);
    let mut prefixed = merge(Config::default(), Some(&patch), None, &all_keys()).unwrap();
    prefixed
        .apply_cli(None, Some(&format!("{next}/other-model")))
        .unwrap();
    assert_eq!(prefixed.model, "other-model");
    assert_eq!(prefixed.fallback_note(&prefixed.active_endpoint()), None);

    // A bare model from a flag lands on the pick too, and the notice stays
    // to say so.
    let mut bare = merge(Config::default(), Some(&patch), None, &all_keys()).unwrap();
    bare.apply_cli(None, Some("mine")).unwrap();
    assert_eq!(bare.provider, next);
    assert_eq!(bare.model, "mine");
    assert_eq!(
        bare.fallback_note(&bare.active_endpoint()).as_deref(),
        Some(expected_mine.as_str())
    );

    // The environment naming a provider ends the notice too, even when it
    // names the very provider the ranking picked. A bare `GG_MODEL` lands on
    // the pick like a bare flag does.
    for (var, value) in [
        ("GG_PROVIDER", next.clone()),
        ("GG_MODEL", format!("{next}/other-model")),
    ] {
        let mut vars = all_keys();
        vars.push((var.to_string(), value));
        let cfg = merge(Config::default(), Some(&patch), None, &vars).unwrap();
        assert_eq!(cfg.provider, next, "{var}");
        assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None, "{var}");
    }
    let mut vars = all_keys();
    vars.push(("GG_MODEL".to_string(), "mine".to_string()));
    let cfg = merge(Config::default(), Some(&patch), None, &vars).unwrap();
    assert_eq!(cfg.model, "mine");
    assert_eq!(
        cfg.fallback_note(&cfg.active_endpoint()).as_deref(),
        Some(expected_mine.as_str())
    );

    // A provider config removed was not passed over for want of a key: it
    // goes without a notice.
    let removed = format!("[providers.{head}]\ndisabled = true\n");
    let cfg = merge(Config::default(), Some(&removed), None, &all_keys()).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.fallback_note(&cfg.active_endpoint()), None);
}

#[test]
fn a_base_url_override_never_carries_a_stored_key() {
    let ranked = providers::default_provider_order();
    assert!(
        ranked.len() >= 2,
        "these presets rank at least two providers"
    );
    let (head, next) = (ranked[0].clone(), ranked[1].clone());
    // The first choice needs a key that is held, so without an override it
    // wins. The second is made to need no key, whatever the presets say.
    let patch = format!("[providers.{head}]\napi_key_env = \"RANK_TEST_KEY\"\n");
    let keyless = format!("[providers.{next}]\napi_key_env = \"\"\n");
    let held = env(&[("RANK_TEST_KEY", "held")]);
    let cfg = merge(Config::default(), Some(&patch), None, &held).unwrap();
    assert_eq!(cfg.provider, head, "without an override the held key wins");

    // A `base_url` override, from a file or from `GG_BASE_URL`, never
    // carries a key of a provider's own: the first default that holds none
    // is taken instead, and the notice says why.
    let url = "http://127.0.0.1:9/v1";
    let sculpted = format!("{patch}{keyless}");
    let mut disable_rest = String::new();
    for name in &ranked[2..] {
        disable_rest.push_str(&format!("[providers.{name}]\ndisabled = true\n"));
    }
    let model = providers::builtin_providers()[&next].default_model.clone();
    let reason = "base_url is set and no provider is named";
    let expected = format!("passed over {head} ({reason}): using {next}/{model}");
    let note = |cfg: &Config| cfg.fallback_note(&cfg.active_endpoint());
    let file = format!("base_url = \"{url}\"\n{sculpted}");
    let cfg = merge(Config::default(), Some(&file), None, &held).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.base_url, url);
    assert_eq!(cfg.api_key, None, "no key of a provider's own follows");
    assert_eq!(note(&cfg).as_deref(), Some(expected.as_str()));
    let mut vars = held.clone();
    vars.push(("GG_BASE_URL".to_string(), url.to_string()));
    let cfg = merge(Config::default(), Some(&sculpted), None, &vars).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.base_url, url);
    assert_eq!(cfg.api_key, None);
    assert_eq!(note(&cfg).as_deref(), Some(expected.as_str()));

    // Naming the provider in the file, beside the file's `base_url`, is how
    // its own key is sent to that address.
    let named = format!("provider = \"{head}\"\nbase_url = \"{url}\"\n{patch}");
    let cfg = merge(Config::default(), Some(&named), None, &held).unwrap();
    assert_eq!(cfg.provider, head);
    assert_eq!(cfg.base_url, url);
    assert_eq!(cfg.api_key.as_deref(), Some("held"));
    assert_eq!(note(&cfg), None);

    // A key given outright for the endpoint, `GG_API_KEY` or a flat `api_key`
    // key, follows the override, and does not make a provider that holds a
    // key of its own eligible.
    let mut vars = held.clone();
    vars.push(("GG_BASE_URL".to_string(), url.to_string()));
    vars.push(("GG_API_KEY".to_string(), "user-key".to_string()));
    let cfg = merge(Config::default(), Some(&sculpted), None, &vars).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.base_url, url);
    assert_eq!(cfg.api_key.as_deref(), Some("user-key"));
    assert_eq!(note(&cfg).as_deref(), Some(expected.as_str()));
    let flat = format!("base_url = \"{url}\"\napi_key = \"user-key\"\n{sculpted}");
    let cfg = merge(Config::default(), Some(&flat), None, &held).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.api_key.as_deref(), Some("user-key"));
    assert_eq!(note(&cfg).as_deref(), Some(expected.as_str()));

    // The file's flat keys survive a bare model switch, as they do a
    // `GG_MODEL`, and a bare `/model` in the TUI. Rebuilt from the provider
    // table alone, the endpoint would carry the provider's own key to that
    // address, or pair the user's address with the preset's limits.
    let flat_all =
        format!("base_url = \"{url}\"\napi_key = \"user-key\"\nmax_tokens = 4321\n{sculpted}");
    let mut cfg = merge(Config::default(), Some(&flat_all), None, &held).unwrap();
    assert_eq!(cfg.provider, next);
    cfg.apply_cli(None, Some("mine")).unwrap();
    assert_eq!(cfg.provider, next);
    assert_eq!(cfg.model, "mine");
    assert_eq!(cfg.base_url, url);
    assert_eq!(cfg.api_key.as_deref(), Some("user-key"));
    assert_eq!(cfg.max_tokens, 4321);
    let switched = cfg.resolve_switch("another").unwrap();
    assert_eq!(switched.base_url, url);
    assert_eq!(switched.api_key.as_deref(), Some("user-key"));
    assert_eq!(switched.max_tokens, 4321);
    // A provider named by a flag keeps its own address and key. The file's
    // keys belong to the provider the file made active, whoever is active
    // later, so a bare `/model` never carries them to another provider.
    let mut named = merge(Config::default(), Some(&flat_all), None, &held).unwrap();
    named.apply_cli(Some(&head), None).unwrap();
    assert_eq!(named.provider, head);
    let own = providers::builtin_providers()[&head].base_url.clone();
    let listed = providers::builtin_providers()[&head].default_model.clone();
    for spec in ["another", listed.as_str()] {
        let switched = named.resolve_switch(spec).unwrap();
        assert_eq!(switched.provider, head, "{spec}");
        assert_eq!(switched.base_url, own, "{spec}: its own address");
        assert_eq!(
            switched.api_key.as_deref(),
            Some("held"),
            "{spec}: its own key"
        );
    }
    // An address from the environment and a key from the file.
    let flat_key = format!("api_key = \"user-key\"\n{sculpted}");
    let mut vars = held.clone();
    vars.push(("GG_BASE_URL".to_string(), url.to_string()));
    let mut cfg = merge(Config::default(), Some(&flat_key), None, &vars).unwrap();
    assert_eq!(cfg.provider, next);
    cfg.apply_cli(None, Some("mine")).unwrap();
    assert_eq!(cfg.model, "mine");
    assert_eq!(cfg.base_url, url);
    assert_eq!(cfg.api_key.as_deref(), Some("user-key"));
    // Without an override the key the file gave wins the first choice, and
    // a bare model still keeps it.
    let mut cfg = merge(Config::default(), Some(&flat_key), None, &held).unwrap();
    assert_eq!(cfg.provider, head);
    cfg.apply_cli(None, Some("mine")).unwrap();
    assert_eq!(cfg.provider, head);
    assert_eq!(cfg.api_key.as_deref(), Some("user-key"));

    // A first choice that needs no key stands under an override, silently.
    let plain = format!("base_url = \"{url}\"\n[providers.{head}]\napi_key_env = \"\"\n");
    let cfg = merge(Config::default(), Some(&plain), None, &held).unwrap();
    assert_eq!(cfg.provider, head);
    assert_eq!(note(&cfg), None);

    // With nothing left that holds no key, the choice is the user's. gg
    // holds the error back until the flags have named a provider, or not.
    // Every other choice is made to need a key, and the rest removed.
    let refusing = format!(
        "base_url = \"{url}\"\n{patch}[providers.{next}]\napi_key_env = \"RANK_REST_KEY\"\n{disable_rest}"
    );
    let mut cfg = merge(Config::default(), Some(&refusing), None, &held).unwrap();
    assert!(cfg.base_url_without_provider);
    let err = cfg.require_provider_for_base_url().unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains(reason), "{msg}");
    // A bare model names no provider; `-P` does.
    cfg.apply_cli(None, Some("mine")).unwrap();
    assert!(cfg.require_provider_for_base_url().is_err());
    cfg.apply_cli(Some(&head), None).unwrap();
    assert_eq!(cfg.provider, head);
    cfg.require_provider_for_base_url().unwrap();
    // A key the config attached to a provider that needs none counts too.
    let attached = format!(
        "base_url = \"{url}\"\n{patch}[providers.{next}]\napi_key = \"attached\"\n{disable_rest}"
    );
    let cfg = merge(Config::default(), Some(&attached), None, &[]).unwrap();
    assert!(cfg.base_url_without_provider);
    // With no key held at all nothing would be sent: the first choice
    // stands, and the auth check reports the missing key.
    let cfg = merge(Config::default(), Some(&refusing), None, &[]).unwrap();
    assert_eq!(cfg.provider, head);
    assert!(!cfg.base_url_without_provider);
    cfg.require_provider_for_base_url().unwrap();
    assert!(
        cfg.active_endpoint().require_auth().is_err(),
        "the auth check reports the missing key"
    );
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
        &all_keys(),
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
        &env(&[("GG_MODEL", "env-model"), ("GG_BASH_TIMEOUT", "2048")]),
    )
    .unwrap();
    assert_eq!(cfg.model, "env-model");
    assert_eq!(cfg.bash_timeout_secs, 2048);
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
        &env(&[("GG_BASH_TIMEOUT", "lots")]),
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("GG_BASH_TIMEOUT"),
        "message should name the var: {msg}"
    );
}

/// The retry budget is a setting like any other: default 5, file then
/// env over it, and zero is a real value (one attempt, no retrying).
#[test]
fn max_retries_merges_like_other_numbers() {
    let default = merge(Config::default(), None, None, &[]).unwrap();
    assert_eq!(default.max_retries, 5);
    let cfg = merge(
        Config::default(),
        Some("max_retries = 5\n"),
        None,
        &env(&[("GG_MAX_RETRIES", "0")]),
    )
    .unwrap();
    assert_eq!(cfg.max_retries, 0, "env wins, and zero turns retries off");
    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_MAX_RETRIES", "often")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("GG_MAX_RETRIES"), "{err:#}");
}

/// The whole-response timeout merges like any other number: default 600
/// (the old fixed 300 guillotined long generations), file under env, and
/// zero refused — a request with no deadline could hang a run forever.
#[test]
fn request_timeout_secs_merges_like_other_numbers() {
    let default = merge(Config::default(), None, None, &[]).unwrap();
    assert_eq!(default.request_timeout_secs, 600);
    let cfg = merge(
        Config::default(),
        Some("request_timeout_secs = 900\n"),
        Some("request_timeout_secs = 1200\n"),
        &env(&[("GG_REQUEST_TIMEOUT", "60")]),
    )
    .unwrap();
    assert_eq!(cfg.request_timeout_secs, 60, "env wins over both files");
    let cfg = merge(
        Config::default(),
        Some("request_timeout_secs = 900\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.request_timeout_secs, 900);
    let err = merge(
        Config::default(),
        Some("request_timeout_secs = 0\n"),
        None,
        &[],
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("request_timeout_secs"),
        "{err:#}"
    );
    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_REQUEST_TIMEOUT", "soon")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("GG_REQUEST_TIMEOUT"), "{err:#}");
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
        &all_keys(),
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
    // The shipped default is data (`default_provider` in `presets.toml`), so
    // this reads it rather than naming a provider.
    assert_eq!(cfg.provider, providers::default_provider());
    assert_eq!(cfg.model, shipped_default().model);
    for name in [
        "strata",
        "glm-coding-plan",
        "opencode-go",
        "deepseek",
        "openrouter",
    ] {
        assert!(cfg.providers.contains_key(name), "missing preset {name}");
    }
    let mut cfg_alias = cfg.clone();
    cfg_alias.apply_cli(Some("zai-coding-plan"), None).unwrap();
    assert_eq!(cfg_alias.provider, "glm-coding-plan");
    assert_eq!(cfg_alias.model, "glm-5.3-flash");
}

/// The presets are data (`presets.toml`), so nothing else pins their
/// contents: this test does, including the fields the generated reference
/// page cannot show. A preset's limits are defaults, never explicit (that
/// is what keeps `strata` probing `/health`).
#[test]
fn presets_keep_their_shipped_values() {
    let providers = providers::builtin_providers();
    let strata = &providers["strata"];
    assert_eq!(strata.base_url, "http://127.0.0.1:8080/v1");
    assert_eq!(strata.default_model, "qwen3.8-flash-next-iq3_s");
    assert_eq!(strata.max_tokens, 16_384);
    assert_eq!(strata.context_window, 131_072);
    assert!(!strata.context_window_explicit);
    assert_eq!(strata.replay_reasoning, ReplayReasoning::Turn);
    assert_eq!(strata.reasoning_effort.as_deref(), Some("medium"));
    assert_eq!(strata.reasoning_budget_tokens, Some(4096));
    assert_eq!(strata.compact_threshold_tokens, Some(65_536));
    assert!(!strata.compact_threshold_explicit);
    assert!(strata.api_key_env.is_none());

    let go = &providers["opencode-go"];
    assert_eq!(go.api_key_env.as_deref(), Some("OPENCODE_API_KEY"));
    assert_eq!(go.replay_reasoning, ReplayReasoning::All);
    // One preset, two wire protocols: the Anthropic models ride their own
    // base URL and never replay reasoning.
    let anthropic = go
        .models
        .iter()
        .find(|m| m.id == "claude-haiku-5-5")
        .expect("claude-haiku-5-5 is an opencode-go model");
    assert_eq!(anthropic.api, Some(ApiKind::AnthropicMessages));
    assert_eq!(
        anthropic.base_url.as_deref(),
        Some("https://opencode.ai/inference/go/anthropic")
    );
    assert_eq!(anthropic.replay_reasoning, Some(ReplayReasoning::Never));
    let hy4 = go
        .models
        .iter()
        .find(|m| m.id == "hy4")
        .expect("hy4 is an opencode-go model");
    assert_eq!(hy4.context_window, Some(1_048_576));
    assert_eq!(hy4.replay_reasoning, Some(ReplayReasoning::Never));
    assert_eq!(go.models.len(), 19);

    let deepseek = &providers["deepseek"];
    let ids: Vec<&str> = deepseek.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["deepseek-flash"]);

    let glm = &providers["glm-coding-plan"];
    let ids: Vec<&str> = glm.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["glm-5.3-flash", "glm-5.4"]);

    // An OpenRouter id carries a vendor prefix of its own. The listed model
    // has its real window; the provider's is the fallback for any other
    // OpenRouter id, which the preset cannot know a window for.
    let openrouter = &providers["openrouter"];
    assert_eq!(openrouter.base_url, "https://openrouter.ai/api/v1");
    assert_eq!(openrouter.api, ApiKind::OpenAiCompletions);
    assert_eq!(openrouter.auth, AuthStyle::Auto);
    assert_eq!(
        openrouter.api_key_env.as_deref(),
        Some("OPENROUTER_API_KEY")
    );
    assert_eq!(openrouter.default_model, "upstage/solar-mini-4-5");
    assert_eq!(openrouter.max_tokens, 16_384);
    assert_eq!(openrouter.context_window, 131_072);
    assert_eq!(openrouter.replay_reasoning, ReplayReasoning::All);
    assert_eq!(openrouter.reasoning_effort, None);
    let ids: Vec<&str> = openrouter.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["upstage/solar-mini-4-5"]);
    assert_eq!(
        openrouter
            .models
            .iter()
            .find(|m| m.id == "upstage/solar-mini-4-5")
            .and_then(|m| m.context_window),
        Some(524_288)
    );

    for provider in providers.values() {
        assert!(
            !provider.context_window_explicit && !provider.compact_threshold_explicit,
            "preset {} must not mark a limit explicit",
            provider.name
        );
    }
}

#[test]
fn disabled_removes_a_preset_for_every_later_layer() {
    // A preset is subtractable, not merely overridable: `disabled = true`
    // wins over a later layer that tries to patch the provider back in.
    // Take one that is not the shipped default: removing that one is an error.
    let gone = other_provider();
    let cfg = merge(
        Config::default(),
        Some(&format!("[providers.{gone}]\ndisabled = true\n")),
        Some(&format!("[providers.{gone}]\nmodel = \"glm-5.3\"\n")),
        &[],
    )
    .unwrap();
    assert!(!cfg.providers.contains_key(&gone));
    let all = providers::builtin_providers();
    let kept: Vec<&str> = all
        .keys()
        .map(String::as_str)
        .filter(|name| *name != gone)
        .collect();
    assert_eq!(
        cfg.providers.keys().map(String::as_str).collect::<Vec<_>>(),
        kept
    );
    // The name no longer resolves, and a spec naming it says why instead
    // of silently becoming a model id on the active provider.
    let err = cfg.resolve_switch(&format!("{gone}/glm-5.3")).unwrap_err();
    assert!(format!("{err:#}").contains("disabled"), "{err:#}");
    let err = cfg.resolve_switch(&gone).unwrap_err();
    assert!(format!("{err:#}").contains("disabled"), "{err:#}");
}

#[test]
fn disabled_custom_provider_and_active_provider_error() {
    // Disabling something that never existed is a no-op, not an error.
    let cfg = merge(
        Config::default(),
        Some("[providers.ghost]\ndisabled = true\n"),
        None,
        &all_keys(),
    )
    .unwrap();
    assert_eq!(cfg.provider, providers::default_provider());

    // Switching off the provider you are pointed at fails loudly at load.
    let err = merge(
        Config::default(),
        Some("provider = \"deepseek\"\n[providers.deepseek]\ndisabled = true\n"),
        None,
        &[],
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("disabled"), "{msg}");
    assert!(msg.contains("deepseek"), "{msg}");

    // `-P` on a removed provider fails too, naming it. Remove one that is not
    // the shipped default: removing that one is the error above.
    let other = other_provider();
    let cfg = merge(
        Config::default(),
        Some(&format!("[providers.{other}]\ndisabled = true\n")),
        None,
        &[],
    )
    .unwrap();
    let mut cfg = cfg;
    let err = cfg.apply_cli(Some(&other), None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("disabled"), "{msg}");
    let err = cfg
        .apply_cli(None, Some(&format!("{other}/any-model")))
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("disabled"), "{msg}");
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

/// An OpenRouter id carries a vendor prefix of its own
/// (`upstage/solar-mini-4-5`), so its `provider/model` spec has two
/// slashes. The first segment is the provider and the rest is the model,
/// however the spec arrives: `-m`, `GG_MODEL`, the `/model` list's own
/// label, or the bare id of a listed model. `-P` with a bare model works
/// too, unless the vendor is also a provider name: the first segment of
/// `-m` is then read as a provider, so such a model takes the full spec.
#[test]
fn openrouter_model_ids_keep_their_slash() {
    const MODEL: &str = "upstage/solar-mini-4-5";
    const SPEC: &str = "openrouter/upstage/solar-mini-4-5";

    let is_solar = |ep: &Endpoint| {
        assert_eq!(ep.provider, "openrouter");
        assert_eq!(ep.model, MODEL);
        assert_eq!(ep.api, ApiKind::OpenAiCompletions);
        assert_eq!(ep.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(ep.context_window, 524_288, "the listed model's window");
    };

    // `-m provider/model`.
    let mut cfg = Config::default();
    cfg.apply_cli(None, Some(SPEC)).unwrap();
    is_solar(&cfg.active_endpoint());
    assert!(cfg.model_explicit);

    // `-P openrouter -m <id>`: the provider and the model passed apart.
    let mut cfg = Config::default();
    cfg.apply_cli(Some("openrouter"), Some(MODEL)).unwrap();
    is_solar(&cfg.active_endpoint());
    assert!(cfg.model_explicit);

    // Apart only works while no vendor is a provider name: the first
    // segment of `-m` names one, so the pair reads as two providers. The
    // full spec is unambiguous, and is what a `--watch` restart emits.
    let mut apart = Config::default();
    let err = apart
        .apply_cli(Some("openrouter"), Some("deepseek/other"))
        .unwrap_err();
    assert!(err.to_string().contains("conflicts"), "{err}");
    let mut cfg = Config::default();
    cfg.apply_cli(None, Some("openrouter/deepseek/other"))
        .unwrap();
    assert_eq!(cfg.provider, "openrouter");
    assert_eq!(cfg.model, "deepseek/other");

    // `GG_MODEL` takes the same spec.
    let cfg = merge(Config::default(), None, None, &env(&[("GG_MODEL", SPEC)])).unwrap();
    is_solar(&cfg.active_endpoint());
    assert!(cfg.model_explicit);

    // The `/model` list's command, and the label the command is built
    // from: one resolves to what the other shows.
    let cfg = Config::default();
    let ep = cfg.resolve_switch(SPEC).unwrap();
    is_solar(&ep);
    assert!(ep.model_explicit);
    assert_eq!(ep.label(), SPEC);
    assert_eq!(cfg.resolve_switch(&ep.label()).unwrap(), ep);

    // A listed model is found by its id alone.
    is_solar(&cfg.resolve_switch(MODEL).unwrap());

    // Any other OpenRouter id resolves the same way, whole, and takes the
    // provider's fallback window.
    let other = cfg
        .resolve_switch("openrouter/other-vendor/other-model")
        .unwrap();
    assert_eq!(other.provider, "openrouter");
    assert_eq!(other.model, "other-vendor/other-model");
    assert_eq!(other.context_window, 131_072);

    // What `/model` and `--list-models` show: the model, its own window.
    let rows = providers::catalog_with_discovered(&cfg.providers, &BTreeMap::new());
    let row = rows
        .iter()
        .find(|r| r.provider == "openrouter")
        .expect("an openrouter row");
    assert_eq!(row.model, MODEL);
    assert_eq!(row.context_window, 524_288);
    assert!(row.needs_key);
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

    // From a provider that lists it, an id two providers share resolves there.
    let mut listing = cfg.clone();
    listing.apply_cli(Some("opencode-go"), None).unwrap();
    let own = listing.resolve_switch("glm-5.3-flash").unwrap();
    assert_eq!(own.label(), "opencode-go/glm-5.3-flash");

    // From any other provider the same id is ambiguous.
    let mut other = cfg.clone();
    other.apply_cli(Some("strata"), None).unwrap();
    let err = other.resolve_switch("glm-5.3-flash").unwrap_err();
    assert!(format!("{err:#}").contains("multiple providers"));

    // An id no provider lists falls to the active one.
    let adhoc = cfg.resolve_switch("my-custom-lora").unwrap();
    assert_eq!(adhoc.label(), format!("{}/my-custom-lora", cfg.provider));
}

#[test]
fn preset_defaults_and_reasoning_controls_override_cleanly() {
    let def = Config::default();
    let shipped = shipped_default();
    assert_eq!(def.max_tokens, shipped.max_tokens);
    assert_eq!(def.reasoning_effort, shipped.reasoning_effort);
    assert_eq!(def.reasoning_budget_tokens, shipped.reasoning_budget_tokens);

    // File layer 0 clears a preset budget (`strata` ships one).
    let cleared = merge(
        Config::default(),
        Some("provider = \"strata\"\nreasoning_budget_tokens = 0\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cleared.reasoning_budget_tokens, None);

    // Model-scoped env vars are ignored by merge: preset values stay,
    // per-model config and apply_reasoning_cli still work. Keys alone name
    // no provider, so holding them all keeps the first choice in play.
    let mut model_scoped = all_keys();
    model_scoped.extend(env(&[
        ("GG_REASONING_EFFORT", "low"),
        ("GG_THINKING_BUDGET", "2048"),
        ("GG_MAX_TOKENS", "2048"),
        ("GG_CONTEXT_WINDOW", "262144"),
        ("GG_COMPACT_THRESHOLD", "98304"),
    ]));
    let env_ignored = merge(Config::default(), None, None, &model_scoped).unwrap();
    assert_eq!(env_ignored.reasoning_effort, shipped.reasoning_effort);
    assert_eq!(
        env_ignored.reasoning_budget_tokens,
        shipped.reasoning_budget_tokens
    );
    assert_eq!(env_ignored.max_tokens, shipped.max_tokens);
    assert_eq!(env_ignored.context_window, shipped.context_window);
    assert_eq!(
        env_ignored.compact_threshold_tokens,
        shipped.compact_threshold_tokens
    );

    // CLI overrides even after provider switch.
    let mut switched = Config::default();
    switched.apply_cli(Some("deepseek"), None).unwrap();
    switched.apply_reasoning_cli(Some("xhigh"), Some(8192));
    assert_eq!(switched.reasoning_effort.as_deref(), Some("xhigh"));
    assert_eq!(switched.reasoning_budget_tokens, Some(8192));
    switched.apply_reasoning_cli(Some(""), Some(0));
    assert_eq!(switched.reasoning_effort, None);
    assert_eq!(switched.reasoning_budget_tokens, None);

    // Per-model config still reaches the shipped default's own model.
    let per_model = format!(
        "[providers.{p}]\nmodels = [{{ id = \"{m}\", reasoning_effort = \"high\", reasoning_budget_tokens = 8192 }}]\n",
        p = def.provider,
        m = def.model
    );
    let per_model_set = merge(Config::default(), Some(&per_model), None, &all_keys()).unwrap();
    assert_eq!(per_model_set.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(per_model_set.reasoning_budget_tokens, Some(8192));
}

#[test]
fn replay_reasoning_compact_threshold_max_turns_and_explicit_ctx() {
    let def = Config::default();
    let shipped = shipped_default();
    assert_eq!(def.replay_reasoning, shipped.replay_reasoning);
    assert_eq!(
        def.compact_threshold_tokens,
        shipped.compact_threshold_tokens
    );
    assert!(!def.context_window_explicit);
    assert_eq!(def.max_turns, None);
    assert_eq!(def.max_seconds, None);

    let cfg = merge(
        Config::default(),
        Some("replay_reasoning = \"all\"\ncompact_threshold_tokens = 0\nmax_turns = 25\nmax_seconds = 900\ncontext_window = 262144\n"),
        None,
        &[],
    )
    .unwrap();
    assert_eq!(cfg.replay_reasoning, ReplayReasoning::All);
    assert_eq!(cfg.compact_threshold_tokens, None);
    assert_eq!(cfg.max_turns, Some(25));
    assert_eq!(cfg.max_seconds, Some(900));
    let mut cli_cfg = cfg.clone();
    cli_cfg.apply_max_seconds_cli(Some(0));
    assert_eq!(cli_cfg.max_seconds, None, "0 disables");
    cli_cfg.apply_max_seconds_cli(None);
    assert_eq!(cli_cfg.max_seconds, None);
    cli_cfg.apply_max_seconds_cli(Some(45));
    assert_eq!(cli_cfg.max_seconds, Some(45));
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
        &env(&[("GG_MAX_TURNS", "40")]),
    )
    .unwrap();
    assert_eq!(env_cfg.max_turns, Some(40));

    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_MAX_TURNS", "nope")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("GG_MAX_TURNS"));

    let seconds_cfg = merge(
        Config::default(),
        Some("max_seconds = 900\n"),
        None,
        &env(&[("GG_MAX_SECONDS", "0")]),
    )
    .unwrap();
    assert_eq!(seconds_cfg.max_seconds, None, "the environment wins");
    let seconds_cfg = merge(
        Config::default(),
        Some("max_seconds = 0\n"),
        None,
        &env(&[("GG_MAX_SECONDS", "120")]),
    )
    .unwrap();
    assert_eq!(seconds_cfg.max_seconds, Some(120));
    let err = merge(
        Config::default(),
        None,
        None,
        &env(&[("GG_MAX_SECONDS", "soon")]),
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("GG_MAX_SECONDS"));
}

#[test]
fn env_overrides_survive_apply_cli_and_resolve_switch() {
    let mut cfg = merge(
        Config::default(),
        None,
        None,
        &env(&[
            ("GG_BASE_URL", "http://127.0.0.1:9999/v1"),
            ("GG_API_KEY", "env-key"),
        ]),
    )
    .unwrap();
    cfg.apply_cli(Some("deepseek"), Some("deepseek-flash"))
        .unwrap();
    assert_eq!(cfg.provider, "deepseek");
    assert_eq!(cfg.model, "deepseek-flash");
    assert_eq!(cfg.base_url, "http://127.0.0.1:9999/v1");
    assert_eq!(cfg.api_key.as_deref(), Some("env-key"));
    // Model-scoped fields come from the target provider, not env vars.
    assert_eq!(cfg.max_tokens, 16_384);
    assert_eq!(cfg.context_window, 1_000_000);
    assert_eq!(cfg.reasoning_effort, None);
    assert_eq!(cfg.reasoning_budget_tokens, None);
    assert_eq!(cfg.compact_threshold_tokens, None);

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
