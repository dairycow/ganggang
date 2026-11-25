//! Configuration: defaults < user file < project file < environment
//! (spec §16). CLI flags sit above everything and are applied via
//! [`Config::apply_cli`] after [`load`].
//!
//! An invalid value is an error wherever it comes from — a typo'd
//! `GG_MAX_TURNS` or a string-typed `max_tokens` must fail loudly,
//! never silently fall back to a default that hides the mistake.

pub mod providers;

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Context;

use crate::agent::session::ReplayReasoning;
use crate::llm::{ApiKind, AuthStyle};
pub use providers::{Endpoint, ProviderConfig};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct EnvOverrides {
    base_url: Option<String>,
    api_key: Option<Option<String>>,
}

impl EnvOverrides {
    fn apply(&self, ep: &mut Endpoint) {
        if let Some(v) = &self.base_url {
            ep.base_url = v.clone();
        }
        if let Some(v) = &self.api_key {
            ep.api_key = v.clone();
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub provider: String,
    pub base_url: String,
    pub api: ApiKind,
    pub auth: AuthStyle,
    pub model: String,
    pub model_explicit: bool,
    pub max_tokens: u32,
    pub api_key: Option<String>,
    pub context_window: u32,
    pub context_window_explicit: bool,
    pub replay_reasoning: ReplayReasoning,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub compact_threshold_tokens: Option<u32>,
    pub compact_threshold_explicit: bool,
    pub max_turns: Option<u32>,
    pub bash_timeout_secs: u64,
    /// Extra attempts a transient LLM transport failure earns (0 turns
    /// retrying off). The policy itself lives in `llm::retry`.
    pub max_retries: u32,
    /// The whole-response timeout for one LLM request — response head AND
    /// body, so it bounds a stream too. Long generations are the reason it
    /// is a setting (GAP 2); connecting stays a fixed 10 s.
    pub request_timeout_secs: u64,
    pub tools: Vec<String>,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Presets (or earlier layers' providers) removed with
    /// `disabled = true`. Kept so a later `-P`/`-m` naming one says why it
    /// is gone instead of blaming a typo.
    pub disabled_providers: BTreeSet<String>,
    env_overrides: EnvOverrides,
}

impl Default for Config {
    fn default() -> Self {
        let providers = providers::builtin_providers();
        let ep = providers
            .get("strata")
            .expect("built-in strata provider")
            .resolve(None);
        Self {
            provider: ep.provider,
            base_url: ep.base_url,
            api: ep.api,
            auth: ep.auth,
            model: ep.model,
            model_explicit: false,
            max_tokens: ep.max_tokens,
            api_key: ep.api_key,
            context_window: ep.context_window,
            context_window_explicit: ep.context_window_explicit,
            replay_reasoning: ep.replay_reasoning,
            reasoning_effort: ep.reasoning_effort,
            reasoning_budget_tokens: ep.reasoning_budget_tokens,
            compact_threshold_tokens: ep.compact_threshold_tokens,
            compact_threshold_explicit: ep.compact_threshold_explicit,
            max_turns: None,
            bash_timeout_secs: 30,
            max_retries: 3,
            request_timeout_secs: 600,
            tools: crate::agent::tools::NAMES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            providers,
            disabled_providers: BTreeSet::new(),
            env_overrides: EnvOverrides::default(),
        }
    }
}

impl Config {
    pub fn active_endpoint(&self) -> Endpoint {
        let api_key_env = self
            .providers
            .get(&self.provider)
            .and_then(|p| p.api_key_env.clone());
        Endpoint {
            provider: self.provider.clone(),
            model: self.model.clone(),
            model_explicit: self.model_explicit,
            base_url: self.base_url.clone(),
            api: self.api,
            auth: self.auth,
            api_key: self.api_key.clone(),
            api_key_env,
            max_tokens: self.max_tokens,
            context_window: self.context_window,
            context_window_explicit: self.context_window_explicit,
            replay_reasoning: self.replay_reasoning,
            reasoning_effort: self.reasoning_effort.clone(),
            reasoning_budget_tokens: self.reasoning_budget_tokens,
            compact_threshold_tokens: self.compact_threshold_tokens,
            compact_threshold_explicit: self.compact_threshold_explicit,
        }
    }

    pub fn apply_cli(&mut self, provider: Option<&str>, model: Option<&str>) -> anyhow::Result<()> {
        if provider.is_none() && model.is_none() {
            return Ok(());
        }
        let (model_provider, model_id) = match model {
            Some(m) => match providers::split_provider_model(m, &self.providers) {
                Some((p, id)) => (Some(p.to_string()), Some(id)),
                None => {
                    if let Some(p) =
                        providers::disabled_provider_prefix(m, &self.disabled_providers)
                    {
                        anyhow::bail!(
                            "provider {p:?} is disabled by `[providers.{p}] disabled = true`"
                        );
                    }
                    (None, Some(m))
                }
            },
            None => (None, None),
        };
        let cli_provider = provider.map(|p| providers::canonical_provider_name(p).to_string());
        if let (Some(cp), Some(mp)) = (&cli_provider, &model_provider)
            && cp != mp
        {
            anyhow::bail!("--provider {cp:?} conflicts with --model provider prefix {mp:?}");
        }
        let target_provider = cli_provider
            .or(model_provider)
            .unwrap_or_else(|| self.provider.clone());
        anyhow::ensure!(
            !self.disabled_providers.contains(&target_provider),
            "provider {target_provider:?} is disabled by `[providers.{target_provider}] disabled = true`"
        );
        let pcfg = lookup_provider(&self.providers, &target_provider)?;
        let mut ep = pcfg.resolve(model_id);
        ep.model_explicit = model_id.is_some();
        self.env_overrides.apply(&mut ep);
        self.apply_endpoint(&ep);
        validate(self)
    }

    pub fn apply_reasoning_cli(&mut self, effort: Option<&str>, budget: Option<u32>) {
        if let Some(v) = effort {
            let trimmed = v.trim();
            self.reasoning_effort = (!trimmed.is_empty()).then(|| trimmed.to_string());
        }
        if let Some(b) = budget {
            self.reasoning_budget_tokens = (b > 0).then_some(b);
        }
    }

    pub fn apply_max_turns_cli(&mut self, max_turns: Option<u32>) {
        if let Some(n) = max_turns {
            self.max_turns = (n > 0).then_some(n);
        }
    }

    #[cfg(test)]
    pub fn resolve_switch(&self, spec: &str) -> anyhow::Result<Endpoint> {
        self.resolve_switch_with_discovered(spec, &[])
    }

    pub fn resolve_switch_with_discovered(
        &self,
        spec: &str,
        discovered: &[(String, String)],
    ) -> anyhow::Result<Endpoint> {
        let spec = spec.trim();
        anyhow::ensure!(!spec.is_empty(), "model specification must not be empty");
        anyhow::ensure!(
            !self
                .disabled_providers
                .contains(providers::canonical_provider_name(spec)),
            "provider {spec:?} is disabled by `[providers.{spec}] disabled = true`"
        );
        if let Some(p) = providers::disabled_provider_prefix(spec, &self.disabled_providers) {
            anyhow::bail!("provider {p:?} is disabled by `[providers.{p}] disabled = true`");
        }
        let canon = providers::canonical_provider_name(spec);
        let mut ep = if let Some(pcfg) = self.providers.get(canon) {
            let mut ep = pcfg.resolve(None);
            ep.model_explicit = false;
            ep
        } else if let Some((p, m)) = providers::split_provider_model(spec, &self.providers) {
            let pcfg = lookup_provider(&self.providers, p)?;
            let mut ep = pcfg.resolve(Some(m));
            ep.model_explicit = true;
            ep
        } else if let Some(cur) = self.providers.get(&self.provider)
            && cur.models.iter().any(|m| m.id == spec)
        {
            let mut ep = cur.resolve(Some(spec));
            ep.model_explicit = true;
            ep
        } else {
            let mut matches: std::collections::BTreeSet<&str> = self
                .providers
                .values()
                .filter(|p| p.models.iter().any(|m| m.id == spec))
                .map(|p| p.name.as_str())
                .collect();
            for (p, m) in discovered {
                if m == spec
                    && let Some(pcfg) = self.providers.get(p)
                {
                    matches.insert(pcfg.name.as_str());
                }
            }
            let mut ep = match matches.iter().copied().collect::<Vec<_>>().as_slice() {
                [one] => lookup_provider(&self.providers, one)?.resolve(Some(spec)),
                [] => lookup_provider(&self.providers, &self.provider)?.resolve(Some(spec)),
                many => {
                    let choices: Vec<String> = many.iter().map(|p| format!("{p}/{spec}")).collect();
                    anyhow::bail!(
                        "model {spec:?} exists on multiple providers; specify one of: {}",
                        choices.join(", ")
                    );
                }
            };
            ep.model_explicit = true;
            ep
        };
        if ep.api_key.is_none()
            && ep.api_key_env.is_some()
            && let Some(Some(key)) = &self.env_overrides.api_key
        {
            ep.api_key = Some(key.clone());
        }
        Ok(ep)
    }

    fn apply_endpoint(&mut self, ep: &Endpoint) {
        self.provider = ep.provider.clone();
        self.model = ep.model.clone();
        self.model_explicit = ep.model_explicit;
        self.base_url = ep.base_url.clone();
        self.api = ep.api;
        self.auth = ep.auth;
        self.api_key = ep.api_key.clone();
        self.max_tokens = ep.max_tokens;
        self.context_window = ep.context_window;
        self.context_window_explicit = ep.context_window_explicit;
        self.replay_reasoning = ep.replay_reasoning;
        self.reasoning_effort = ep.reasoning_effort.clone();
        self.reasoning_budget_tokens = ep.reasoning_budget_tokens;
        self.compact_threshold_tokens = ep.compact_threshold_tokens;
        self.compact_threshold_explicit = ep.compact_threshold_explicit;
    }
}

/// What a config file may set. Unknown keys are ignored, the same
/// forward-compatibility rule the wire types follow.
#[derive(Default, serde::Deserialize)]
struct FileConfig {
    provider: Option<String>,
    base_url: Option<String>,
    api: Option<ApiKind>,
    auth: Option<AuthStyle>,
    model: Option<String>,
    max_tokens: Option<u32>,
    api_key: Option<String>,
    context_window: Option<u32>,
    replay_reasoning: Option<ReplayReasoning>,
    reasoning_effort: Option<String>,
    reasoning_budget_tokens: Option<u32>,
    compact_threshold_tokens: Option<u32>,
    max_turns: Option<u32>,
    bash_timeout_secs: Option<u64>,
    max_retries: Option<u32>,
    request_timeout_secs: Option<u64>,
    tools: Option<Vec<String>>,
    #[serde(default)]
    providers: BTreeMap<String, providers::FileProviderConfig>,
}

/// Read every layer and merge. The pure merging logic lives in [`merge`];
/// this function only gathers real files and environment.
pub fn load() -> anyhow::Result<Config> {
    let user = read_if_exists(&user_path())?;
    let project = read_if_exists(&Some(std::path::PathBuf::from(".gg/config.toml")))?;
    let env: Vec<(String, String)> = std::env::vars().collect();
    merge(Config::default(), user.as_deref(), project.as_deref(), &env)
}

/// `~/.gg/config.toml` — absent when HOME is unset (no home, no user file).
fn user_path() -> Option<std::path::PathBuf> {
    crate::home::dir().map(|home| home.join(".gg").join("config.toml"))
}

/// None = no file. A file that exists but cannot be read (permissions,
/// encoding) is an error: silently dropping a layer would hide it.
fn read_if_exists(path: &Option<std::path::PathBuf>) -> anyhow::Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Merge precedence, lowest to highest: `defaults` < `user_toml` <
/// `project_toml` < `env`. Shallow: a layer that sets a key wins whole,
/// no deep merging. Pure — tests feed strings, never the real HOME.
pub fn merge(
    defaults: Config,
    user_toml: Option<&str>,
    project_toml: Option<&str>,
    env: &[(String, String)],
) -> anyhow::Result<Config> {
    let user = parse_layer(user_toml, "user config (~/.gg/config.toml)")?;
    let project = parse_layer(project_toml, "project config (.gg/config.toml)")?;

    let mut providers = defaults.providers.clone();
    let mut removed: BTreeSet<String> = BTreeSet::new();
    providers::apply_provider_layer(&mut providers, user.providers, &mut removed)?;
    providers::apply_provider_layer(&mut providers, project.providers, &mut removed)?;
    providers::resolve_provider_keys(&mut providers, env);

    let pick = |u: &Option<String>, p: &Option<String>| p.clone().or_else(|| u.clone());
    let pick_list =
        |u: &Option<Vec<String>>, p: &Option<Vec<String>>| p.clone().or_else(|| u.clone());

    let file_provider = pick(&user.provider, &project.provider)
        .map(|p| providers::canonical_provider_name(&p).to_string())
        .unwrap_or_else(|| defaults.provider.clone());

    let env_provider = env_value(env, "GG_PROVIDER")
        .filter(|s| !s.trim().is_empty())
        .map(|p| providers::canonical_provider_name(&p).to_string());
    let env_model_raw = env_value(env, "GG_MODEL");
    let (env_model_provider, env_model_id) = match env_model_raw.as_deref() {
        Some(m) => match providers::split_provider_model(m, &providers) {
            Some((p, id)) => (Some(p.to_string()), Some(id.to_string())),
            None => (None, Some(m.to_string())),
        },
        None => (None, None),
    };
    if let (Some(ep), Some(mp)) = (&env_provider, &env_model_provider)
        && ep != mp
    {
        anyhow::bail!("GG_PROVIDER={ep:?} conflicts with GG_MODEL provider prefix {mp:?}");
    }

    let active_provider = env_provider
        .or(env_model_provider)
        .unwrap_or_else(|| file_provider.clone());
    anyhow::ensure!(
        !removed.contains(&active_provider),
        "provider {active_provider:?} is disabled by `[providers.{active_provider}] disabled = true`"
    );
    let pcfg = lookup_provider(&providers, &active_provider)?;
    let apply_file_flat = active_provider == file_provider;

    let file_model = if apply_file_flat {
        pick(&user.model, &project.model)
    } else {
        None
    };
    let chosen_model = env_model_id.as_deref().or(file_model.as_deref());
    let mut ep = pcfg.resolve(chosen_model);
    ep.model_explicit = env_model_id.is_some();

    if apply_file_flat {
        if let Some(v) = pick(&user.base_url, &project.base_url) {
            ep.base_url = v;
        }
        if let Some(v) = project.api.or(user.api) {
            ep.api = v;
        }
        if let Some(v) = project.auth.or(user.auth) {
            ep.auth = v;
        }
        if let Some(v) = project.max_tokens.or(user.max_tokens) {
            ep.max_tokens = v;
        }
        if let Some(v) = pick(&user.api_key, &project.api_key) {
            ep.api_key = (!v.is_empty()).then_some(v);
        }
        if let Some(v) = project.context_window.or(user.context_window) {
            ep.context_window = v;
            ep.context_window_explicit = true;
        }
        if let Some(v) = project.replay_reasoning.or(user.replay_reasoning) {
            ep.replay_reasoning = v;
        }
        if let Some(v) = pick(&user.reasoning_effort, &project.reasoning_effort) {
            ep.reasoning_effort = (!v.is_empty()).then_some(v);
        }
        if let Some(v) = project
            .reasoning_budget_tokens
            .or(user.reasoning_budget_tokens)
        {
            ep.reasoning_budget_tokens = (v > 0).then_some(v);
        }
        if let Some(v) = project
            .compact_threshold_tokens
            .or(user.compact_threshold_tokens)
        {
            ep.compact_threshold_tokens = (v > 0).then_some(v);
            ep.compact_threshold_explicit = true;
        }
    }

    let mut env_overrides = EnvOverrides::default();
    if let Some(v) = env_value(env, "GG_BASE_URL") {
        env_overrides.base_url = Some(v);
    }
    if let Some(v) = env_value(env, "GG_API_KEY") {
        env_overrides.api_key = Some((!v.is_empty()).then_some(v));
    }
    env_overrides.apply(&mut ep);
    let file_max_turns = project
        .max_turns
        .or(user.max_turns)
        .and_then(|n| (n > 0).then_some(n));
    let max_turns = match env_value(env, "GG_MAX_TURNS") {
        Some(raw) => {
            let parsed: u32 = parse_env(Some(raw), "GG_MAX_TURNS", 0)?;
            (parsed > 0).then_some(parsed)
        }
        None => file_max_turns,
    };

    let cfg = Config {
        provider: ep.provider,
        base_url: ep.base_url,
        api: ep.api,
        auth: ep.auth,
        model: ep.model,
        model_explicit: ep.model_explicit,
        max_tokens: ep.max_tokens,
        api_key: ep.api_key,
        context_window: ep.context_window,
        context_window_explicit: ep.context_window_explicit,
        replay_reasoning: ep.replay_reasoning,
        reasoning_effort: ep.reasoning_effort,
        reasoning_budget_tokens: ep.reasoning_budget_tokens,
        compact_threshold_tokens: ep.compact_threshold_tokens,
        compact_threshold_explicit: ep.compact_threshold_explicit,
        max_turns,
        bash_timeout_secs: parse_env(
            env_value(env, "GG_BASH_TIMEOUT"),
            "GG_BASH_TIMEOUT",
            pick_num(
                &user.bash_timeout_secs,
                &project.bash_timeout_secs,
                defaults.bash_timeout_secs,
            ),
        )?,
        max_retries: parse_env(
            env_value(env, "GG_MAX_RETRIES"),
            "GG_MAX_RETRIES",
            pick_num(
                &user.max_retries,
                &project.max_retries,
                defaults.max_retries,
            ),
        )?,
        request_timeout_secs: parse_env(
            env_value(env, "GG_REQUEST_TIMEOUT"),
            "GG_REQUEST_TIMEOUT",
            pick_num(
                &user.request_timeout_secs,
                &project.request_timeout_secs,
                defaults.request_timeout_secs,
            ),
        )?,
        tools: pick_list(&user.tools, &project.tools).unwrap_or(defaults.tools),
        providers,
        disabled_providers: removed,
        env_overrides,
    };
    validate(&cfg)?;
    Ok(cfg)
}

fn lookup_provider<'a>(
    providers: &'a BTreeMap<String, ProviderConfig>,
    name: &str,
) -> anyhow::Result<&'a ProviderConfig> {
    providers.get(name).ok_or_else(|| {
        let known: Vec<&str> = providers.keys().map(String::as_str).collect();
        anyhow::anyhow!("unknown provider {name:?} (known: {})", known.join(", "))
    })
}

fn parse_layer(raw: Option<&str>, what: &str) -> anyhow::Result<FileConfig> {
    let Some(raw) = raw else {
        return Ok(FileConfig::default());
    };
    toml::from_str(raw).with_context(|| format!("parsing {what}"))
}

fn env_value(env: &[(String, String)], name: &str) -> Option<String> {
    env.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone())
}

fn pick_num<T: Clone>(user: &Option<T>, project: &Option<T>, default: T) -> T {
    project.clone().or_else(|| user.clone()).unwrap_or(default)
}

/// Validate cross-field rules after merging. Each message names the
/// setting so the user knows exactly what to fix.
fn validate(cfg: &Config) -> anyhow::Result<()> {
    if cfg.base_url.trim().is_empty() {
        anyhow::bail!("base_url must not be empty");
    }
    if cfg.model.trim().is_empty() {
        anyhow::bail!("model must not be empty");
    }
    if cfg.max_tokens == 0 {
        anyhow::bail!("max_tokens must be positive");
    }
    if cfg.context_window == 0 {
        anyhow::bail!("context_window must be positive");
    }
    if cfg.bash_timeout_secs == 0 {
        anyhow::bail!("bash_timeout_secs must be positive");
    }
    if cfg.request_timeout_secs == 0 {
        anyhow::bail!("request_timeout_secs must be positive");
    }
    for tool in &cfg.tools {
        if tool.trim().is_empty() {
            anyhow::bail!("tools list contains an empty name");
        }
        if !crate::agent::tools::NAMES.contains(&tool.as_str()) {
            anyhow::bail!("unknown tool in tools allowlist: {tool}");
        }
    }
    for p in cfg.providers.values() {
        if p.base_url.trim().is_empty() {
            anyhow::bail!("provider '{}' base_url must not be empty", p.name);
        }
        if p.default_model.trim().is_empty() {
            anyhow::bail!("provider '{}' model must not be empty", p.name);
        }
        if p.max_tokens == 0 {
            anyhow::bail!("provider '{}' max_tokens must be positive", p.name);
        }
        if p.context_window == 0 {
            anyhow::bail!("provider '{}' context_window must be positive", p.name);
        }
    }
    Ok(())
}

/// Parse one env var's string into a typed value, erroring with the var
/// name on garbage. Kept as a free function so tests never mutate
/// process-global environment state.
fn parse_env<T>(raw: Option<String>, name: &str, default: T) -> anyhow::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let Some(raw) = raw else {
        return Ok(default);
    };
    raw.parse()
        .with_context(|| format!("{name}={raw:?} is not a valid number"))
}

/// Resolve the effective tool allowlist: CLI flag > config setting,
/// validated against the names the registry actually offers (the caller
/// knows the registry; config does not — layering).
pub fn resolve_tools(
    cli: Option<Vec<String>>,
    config: Vec<String>,
    registry_names: &[&str],
) -> anyhow::Result<Vec<String>> {
    let chosen = cli.unwrap_or(config);
    for name in &chosen {
        if !registry_names.contains(&name.as_str()) {
            anyhow::bail!("unknown tool in allowlist: {name}");
        }
    }
    Ok(chosen)
}

#[cfg(test)]
mod tests;
