//! Provider presets, per-model overrides, and endpoint resolution.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::agent::session::ReplayReasoning;
use crate::llm::{ApiKind, AuthStyle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub api: Option<ApiKind>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    pub replay_reasoning: Option<ReplayReasoning>,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub thinking_off: Option<String>,
    pub forced_thinking: Option<bool>,
    pub compact_threshold_tokens: Option<u32>,
}

impl ModelSpec {
    fn id_only(id: &str) -> Self {
        Self {
            id: id.to_string(),
            api: None,
            base_url: None,
            max_tokens: None,
            context_window: None,
            replay_reasoning: None,
            reasoning_effort: None,
            reasoning_budget_tokens: None,
            thinking_off: None,
            forced_thinking: None,
            compact_threshold_tokens: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    pub name: String,
    pub base_url: String,
    pub api: ApiKind,
    pub auth: AuthStyle,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub default_model: String,
    pub max_tokens: u32,
    pub context_window: u32,
    pub context_window_explicit: bool,
    pub replay_reasoning: ReplayReasoning,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub thinking_off: Option<String>,
    /// A model whose thinking cannot be switched off (config
    /// `forced_thinking`): a thought-only cut gets no retry, because the
    /// same request would think its way to the same cut.
    pub forced_thinking: bool,
    pub compact_threshold_tokens: Option<u32>,
    pub compact_threshold_explicit: bool,
    pub models: Vec<ModelSpec>,
}

impl ProviderConfig {
    pub fn resolve(&self, model: Option<&str>) -> Endpoint {
        let chosen = model
            .filter(|m| !m.trim().is_empty())
            .unwrap_or(&self.default_model);
        let spec = self.models.iter().find(|m| m.id == chosen);
        Endpoint {
            provider: self.name.clone(),
            model: chosen.to_string(),
            model_explicit: false,
            base_url: spec
                .and_then(|s| s.base_url.clone())
                .unwrap_or_else(|| self.base_url.clone()),
            api: spec.and_then(|s| s.api).unwrap_or(self.api),
            auth: self.auth,
            api_key: self.api_key.clone(),
            api_key_env: self.api_key_env.clone(),
            max_tokens: spec.and_then(|s| s.max_tokens).unwrap_or(self.max_tokens),
            context_window: spec
                .and_then(|s| s.context_window)
                .unwrap_or(self.context_window),
            context_window_explicit: self.context_window_explicit
                || spec.is_some_and(|s| s.context_window.is_some()),
            replay_reasoning: spec
                .and_then(|s| s.replay_reasoning)
                .unwrap_or(self.replay_reasoning),
            reasoning_effort: spec
                .and_then(|s| s.reasoning_effort.as_deref())
                .map(|s| {
                    let t = s.trim();
                    (!t.is_empty()).then(|| t.to_string())
                })
                .unwrap_or_else(|| self.reasoning_effort.clone()),
            reasoning_budget_tokens: spec
                .and_then(|s| s.reasoning_budget_tokens)
                .map(|v| (v > 0).then_some(v))
                .unwrap_or(self.reasoning_budget_tokens),
            thinking_off: spec
                .and_then(|s| s.thinking_off.as_deref())
                .map(|s| {
                    let t = s.trim();
                    (!t.is_empty()).then(|| t.to_string())
                })
                .unwrap_or_else(|| self.thinking_off.clone()),
            forced_thinking: spec
                .and_then(|s| s.forced_thinking)
                .unwrap_or(self.forced_thinking),
            compact_threshold_tokens: spec
                .and_then(|s| s.compact_threshold_tokens)
                .map(|v| (v > 0).then_some(v))
                .unwrap_or(self.compact_threshold_tokens),
            compact_threshold_explicit: self.compact_threshold_explicit
                || spec.is_some_and(|s| s.compact_threshold_tokens.is_some()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub provider: String,
    pub model: String,
    pub model_explicit: bool,
    pub base_url: String,
    pub api: ApiKind,
    pub auth: AuthStyle,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub max_tokens: u32,
    pub context_window: u32,
    pub context_window_explicit: bool,
    pub replay_reasoning: ReplayReasoning,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub thinking_off: Option<String>,
    pub forced_thinking: bool,
    pub compact_threshold_tokens: Option<u32>,
    pub compact_threshold_explicit: bool,
}

impl Endpoint {
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider, self.model)
    }

    pub fn require_auth(&self) -> anyhow::Result<()> {
        if self.api_key.is_none()
            && let Some(env_var) = &self.api_key_env
        {
            anyhow::bail!(
                "provider '{}' requires an API key (set [providers.{}].api_key in ~/.gg/config.toml, or export {} / GG_API_KEY)",
                self.provider,
                self.provider,
                env_var
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogRow {
    pub provider: String,
    pub model: String,
    pub api: ApiKind,
    pub context_window: u32,
    pub needs_key: bool,
    pub has_key: bool,
    pub status: Option<String>,
}

#[derive(Default, Deserialize)]
pub(super) struct FileProviderConfig {
    pub base_url: Option<String>,
    pub api: Option<ApiKind>,
    pub auth: Option<AuthStyle>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    pub replay_reasoning: Option<ReplayReasoning>,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub thinking_off: Option<String>,
    pub forced_thinking: Option<bool>,
    pub compact_threshold_tokens: Option<u32>,
    pub models: Option<Vec<FileModelEntry>>,
    /// `disabled = true` removes the provider instead of patching it.
    pub disabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum FileModelEntry {
    Id(String),
    Spec(FileModelSpec),
}

#[derive(Deserialize)]
pub(super) struct FileModelSpec {
    pub id: String,
    pub api: Option<ApiKind>,
    pub base_url: Option<String>,
    pub max_tokens: Option<u32>,
    pub context_window: Option<u32>,
    pub replay_reasoning: Option<ReplayReasoning>,
    pub reasoning_effort: Option<String>,
    pub reasoning_budget_tokens: Option<u32>,
    pub thinking_off: Option<String>,
    pub forced_thinking: Option<bool>,
    pub compact_threshold_tokens: Option<u32>,
}

impl From<FileModelEntry> for ModelSpec {
    fn from(entry: FileModelEntry) -> Self {
        match entry {
            FileModelEntry::Id(id) => ModelSpec::id_only(&id),
            FileModelEntry::Spec(s) => ModelSpec {
                id: s.id,
                api: s.api,
                base_url: s.base_url,
                max_tokens: s.max_tokens,
                context_window: s.context_window,
                replay_reasoning: s.replay_reasoning,
                reasoning_effort: s.reasoning_effort,
                reasoning_budget_tokens: s.reasoning_budget_tokens,
                thinking_off: s.thinking_off,
                forced_thinking: s.forced_thinking,
                compact_threshold_tokens: s.compact_threshold_tokens,
            },
        }
    }
}

pub fn canonical_provider_name(name: &str) -> &str {
    match name {
        "zai-coding-plan" => "glm-coding-plan",
        other => other,
    }
}

/// The shipped presets, embedded as data (not Rust literals) so the table
/// is inspectable, diffable against the generated reference page, and
/// overridable or removable from config like any other provider.
const PRESETS_TOML: &str = include_str!("presets.toml");

/// `default_provider` in the presets: one provider, or the providers to try
/// in order (see [`default_provider_order`]).
#[derive(Deserialize)]
#[serde(untagged)]
enum DefaultProvider {
    One(String),
    Ranked(Vec<String>),
}

#[derive(Deserialize)]
struct PresetFile {
    default_provider: Option<DefaultProvider>,
    #[serde(default)]
    providers: BTreeMap<String, FileProviderConfig>,
}

/// The embedded presets, parsed.
struct Presets {
    /// Providers to try in order when no flag, environment variable or
    /// config file names one. Each is a key of `providers`.
    default_provider: Vec<String>,
    providers: BTreeMap<String, ProviderConfig>,
}

impl Presets {
    /// Parse the embedded presets through the same path a user's
    /// `[providers.<name>]` table takes, so presets and config cannot drift
    /// apart. Their values are *defaults*: only config marks a limit explicit,
    /// which is what suppresses `/health` probing.
    fn load() -> Self {
        let parsed: PresetFile =
            toml::from_str(PRESETS_TOML).expect("embedded provider presets parse as TOML");
        let mut map = BTreeMap::new();
        apply_provider_layer(&mut map, parsed.providers, &mut BTreeSet::new())
            .expect("embedded provider presets are complete provider tables");
        for provider in map.values_mut() {
            provider.context_window_explicit = false;
            provider.compact_threshold_explicit = false;
        }
        let default_provider = match parsed.default_provider {
            Some(DefaultProvider::One(name)) => vec![name],
            Some(DefaultProvider::Ranked(names)) => names,
            None => panic!("embedded provider presets set default_provider"),
        }
        .into_iter()
        .map(|name| canonical_provider_name(&name).to_string())
        .collect::<Vec<_>>();
        assert!(
            !default_provider.is_empty(),
            "embedded default_provider names at least one provider"
        );
        let mut seen = BTreeSet::new();
        for name in &default_provider {
            assert!(
                seen.insert(name),
                "embedded default_provider names {name:?} twice"
            );
            assert!(
                map.contains_key(name),
                "embedded default_provider {name:?} is not a preset"
            );
        }
        Self {
            default_provider,
            providers: map,
        }
    }
}

/// The embedded presets, parsed on first use.
fn presets() -> &'static Presets {
    static PRESETS: std::sync::OnceLock<Presets> = std::sync::OnceLock::new();
    PRESETS.get_or_init(Presets::load)
}

/// The shipped provider table (see [`PRESETS_TOML`]).
pub fn builtin_providers() -> BTreeMap<String, ProviderConfig> {
    presets().providers.clone()
}

/// The first provider tried when nothing names one: the head of
/// [`default_provider_order`].
pub fn default_provider() -> String {
    presets().default_provider[0].clone()
}

/// The providers tried in order when no flag, environment variable or config
/// file names one: the first you hold a key for wins (`config::merge`). It is
/// data (`default_provider` in `presets.toml`), so re-pointing the shipped
/// default is a one-line edit there: no Rust changes, and the generated
/// reference page follows.
pub fn default_provider_order() -> Vec<String> {
    presets().default_provider.clone()
}

pub(super) fn apply_provider_layer(
    providers: &mut BTreeMap<String, ProviderConfig>,
    layer: BTreeMap<String, FileProviderConfig>,
    removed: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    for (raw_name, patch) in layer {
        let name = canonical_provider_name(&raw_name).to_string();
        // `disabled = true` subtracts a preset (or a provider an earlier
        // layer added). One-way by design: removal is final for the merge,
        // so a project file can neither resurrect nor re-patch what an
        // earlier layer switched off.
        if patch.disabled == Some(true) {
            providers.remove(&name);
            removed.insert(name);
            continue;
        }
        if removed.contains(&name) {
            continue;
        }
        if let Some(existing) = providers.get_mut(&name) {
            if let Some(v) = patch.base_url {
                existing.base_url = v;
            }
            if let Some(v) = patch.api {
                existing.api = v;
            }
            if let Some(v) = patch.auth {
                existing.auth = v;
            }
            if let Some(v) = patch.api_key {
                existing.api_key = (!v.is_empty()).then_some(v);
            }
            if let Some(v) = patch.api_key_env {
                existing.api_key_env = (!v.is_empty()).then_some(v);
            }
            if let Some(v) = patch.model {
                existing.default_model = v;
            }
            if let Some(v) = patch.max_tokens {
                existing.max_tokens = v;
            }
            if let Some(v) = patch.context_window {
                existing.context_window = v;
                existing.context_window_explicit = true;
            }
            if let Some(v) = patch.replay_reasoning {
                existing.replay_reasoning = v;
            }
            if let Some(v) = patch.reasoning_effort {
                existing.reasoning_effort = (!v.is_empty()).then_some(v);
            }
            if let Some(v) = patch.reasoning_budget_tokens {
                existing.reasoning_budget_tokens = (v > 0).then_some(v);
            }
            if let Some(v) = patch.thinking_off {
                existing.thinking_off = (!v.trim().is_empty()).then_some(v);
            }
            if let Some(v) = patch.forced_thinking {
                existing.forced_thinking = v;
            }
            if let Some(v) = patch.compact_threshold_tokens {
                existing.compact_threshold_tokens = (v > 0).then_some(v);
                existing.compact_threshold_explicit = true;
            }
            if let Some(models) = patch.models {
                existing.models = models.into_iter().map(ModelSpec::from).collect();
            }
        } else {
            let base_url = patch
                .base_url
                .ok_or_else(|| anyhow::anyhow!("custom provider '{name}' must set base_url"))?;
            let default_model = patch
                .model
                .ok_or_else(|| anyhow::anyhow!("custom provider '{name}' must set model"))?;
            let models = patch
                .models
                .map(|m| m.into_iter().map(ModelSpec::from).collect())
                .unwrap_or_else(|| vec![ModelSpec::id_only(&default_model)]);
            providers.insert(
                name.clone(),
                ProviderConfig {
                    name,
                    base_url,
                    api: patch.api.unwrap_or_default(),
                    auth: patch.auth.unwrap_or_default(),
                    api_key: patch.api_key.filter(|k| !k.is_empty()),
                    api_key_env: patch.api_key_env.filter(|k| !k.is_empty()),
                    default_model,
                    max_tokens: patch.max_tokens.unwrap_or(4096),
                    context_window: patch.context_window.unwrap_or(131_072),
                    context_window_explicit: patch.context_window.is_some(),
                    replay_reasoning: patch.replay_reasoning.unwrap_or(ReplayReasoning::Never),
                    reasoning_effort: patch.reasoning_effort.filter(|s| !s.is_empty()),
                    reasoning_budget_tokens: patch.reasoning_budget_tokens.filter(|&v| v > 0),
                    thinking_off: patch.thinking_off.filter(|s| !s.trim().is_empty()),
                    forced_thinking: patch.forced_thinking.unwrap_or(false),
                    compact_threshold_tokens: patch.compact_threshold_tokens.filter(|&v| v > 0),
                    compact_threshold_explicit: patch.compact_threshold_tokens.is_some(),
                    models,
                },
            );
        }
    }
    Ok(())
}

pub(super) fn resolve_provider_keys(
    providers: &mut BTreeMap<String, ProviderConfig>,
    env: &[(String, String)],
) {
    for provider in providers.values_mut() {
        if provider.api_key.is_none()
            && let Some(var) = &provider.api_key_env
            && let Some((_, val)) = env.iter().find(|(n, v)| n == var && !v.is_empty())
        {
            provider.api_key = Some(val.clone());
        }
    }
}

/// `Some(prefix)` when `spec` is `prefix/model` and `prefix` names a
/// provider that config removed — the caller reports the removal instead of
/// silently treating the whole spec as a model id.
pub(super) fn disabled_provider_prefix<'a>(
    spec: &'a str,
    disabled: &BTreeSet<String>,
) -> Option<&'a str> {
    let (lhs, rhs) = spec.split_once('/')?;
    (!rhs.is_empty() && disabled.contains(canonical_provider_name(lhs))).then_some(lhs)
}

pub(super) fn split_provider_model<'a>(
    spec: &'a str,
    providers: &BTreeMap<String, ProviderConfig>,
) -> Option<(&'a str, &'a str)> {
    let (lhs, rhs) = spec.split_once('/')?;
    if rhs.is_empty() {
        return None;
    }
    let canon = canonical_provider_name(lhs);
    if providers.contains_key(canon) {
        Some((canon, rhs))
    } else {
        None
    }
}

pub fn catalog_with_discovered(
    providers: &BTreeMap<String, ProviderConfig>,
    discovered: &BTreeMap<String, Vec<crate::llm::discovery::DiscoveredModel>>,
) -> Vec<CatalogRow> {
    let mut rows = Vec::new();
    for provider in providers.values() {
        let needs_key = provider.api_key_env.is_some();
        let has_key = provider.api_key.is_some();
        if let Some(live) = discovered.get(&provider.name)
            && !live.is_empty()
        {
            for d in live {
                let ep = provider.resolve(Some(&d.id));
                let context_window = if ep.context_window_explicit {
                    ep.context_window
                } else {
                    d.context_window
                        .filter(|&c| c > 0)
                        .unwrap_or(ep.context_window)
                };
                rows.push(CatalogRow {
                    provider: provider.name.clone(),
                    model: ep.model,
                    api: ep.api,
                    context_window,
                    needs_key,
                    has_key,
                    status: d.status.clone(),
                });
            }
            continue;
        }
        if provider.models.is_empty() {
            let ep = provider.resolve(None);
            rows.push(CatalogRow {
                provider: provider.name.clone(),
                model: ep.model,
                api: ep.api,
                context_window: ep.context_window,
                needs_key,
                has_key,
                status: None,
            });
            continue;
        }
        let mut saw_default = false;
        for spec in &provider.models {
            if spec.id == provider.default_model {
                saw_default = true;
            }
            let ep = provider.resolve(Some(&spec.id));
            rows.push(CatalogRow {
                provider: provider.name.clone(),
                model: ep.model,
                api: ep.api,
                context_window: ep.context_window,
                needs_key,
                has_key,
                status: None,
            });
        }
        if !saw_default {
            let ep = provider.resolve(None);
            rows.push(CatalogRow {
                provider: provider.name.clone(),
                model: ep.model,
                api: ep.api,
                context_window: ep.context_window,
                needs_key,
                has_key,
                status: None,
            });
        }
    }
    rows
}
