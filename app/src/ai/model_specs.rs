//! Model picker ratings from Artificial Analysis, for organization builds whose admin configured
//! a key. Every install shares that key's small daily quota, so the list is fetched once and
//! again only when a model appears that it hasn't seen, at most once a day.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use ::ai::model_specs::{BenchmarkedModel, Catalog};
use serde::{Deserialize, Serialize};
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::ai::llms::{LLMInfo, LLMPreferences, LLMPreferencesEvent, LLMSpec};
use crate::auth;

const CACHE_FILE: &str = "artificial-analysis-models.json";
const MIN_FETCH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Prefixes the app gives client-side models' IDs, which Artificial Analysis doesn't know.
const ID_PREFIXES: &[&str] = &["cerebras:", "chatgpt:"];

#[derive(Default, Serialize, Deserialize)]
struct CachedCatalog {
    fetched_at: Option<SystemTime>,
    models: Vec<BenchmarkedModel>,
    /// Models the app offered that the last fetch didn't list. They don't trigger another fetch.
    #[serde(default)]
    unlisted: BTreeSet<String>,
}

pub struct ModelSpecs {
    api_key: Option<String>,
    aliases: HashMap<String, String>,
    catalog: Option<Catalog>,
    unlisted: BTreeSet<String>,
    /// When the list was last fetched or a fetch last failed.
    last_attempt: Option<SystemTime>,
    fetching: bool,
}

impl ModelSpecs {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let Some(config) = auth::organization::model_specs_config() else {
            return Self {
                api_key: None,
                aliases: HashMap::new(),
                catalog: None,
                unlisted: BTreeSet::new(),
                last_attempt: None,
                fetching: false,
            };
        };
        ctx.subscribe_to_model(&LLMPreferences::handle(ctx), |me, _, event, ctx| {
            if matches!(event, LLMPreferencesEvent::UpdatedAvailableLLMs) {
                me.fetch_if_needed(ctx);
            }
        });
        let cache = read_cache().unwrap_or_default();
        Self {
            api_key: Some(config.artificial_analysis_api_key.clone()),
            aliases: config.aliases.clone(),
            catalog: (!cache.models.is_empty()).then(|| Catalog::new(cache.models)),
            unlisted: cache.unlisted,
            last_attempt: cache.fetched_at,
            fetching: false,
        }
    }

    /// Whether ratings come from Artificial Analysis, which then must be credited.
    pub fn is_active(&self) -> bool {
        self.catalog.is_some()
    }

    /// Ratings for `llm`, when it matches a benchmarked model.
    pub fn spec_for(&self, llm: &LLMInfo) -> Option<LLMSpec> {
        let catalog = self.catalog.as_ref()?;
        catalog
            .scores_for(benchmark_id(llm), &llm.display_name, &self.aliases)
            .map(|scores| LLMSpec {
                cost: scores.cost,
                quality: scores.quality,
                speed: scores.speed,
            })
    }

    /// Fetches the list when the app offers a model it hasn't seen yet, listed or not.
    fn fetch_if_needed(&mut self, ctx: &mut ModelContext<Self>) {
        let Some(api_key) = self.api_key.clone() else {
            return;
        };
        if self.fetching
            || self
                .last_attempt
                .and_then(|at| at.elapsed().ok())
                .is_some_and(|age| age < MIN_FETCH_INTERVAL)
        {
            return;
        }
        let unseen = offered_models(ctx).into_iter().any(|llm| {
            self.spec_for(&llm).is_none() && !self.unlisted.contains(benchmark_id(&llm))
        });
        if self.catalog.is_some() && !unseen {
            return;
        }
        self.fetch(api_key, ctx);
    }

    #[cfg(not(target_family = "wasm"))]
    fn fetch(&mut self, api_key: String, ctx: &mut ModelContext<Self>) {
        self.fetching = true;
        ctx.spawn(
            ::ai::model_specs::fetch_catalog(api_key),
            |me, result, ctx| {
                me.fetching = false;
                me.last_attempt = Some(SystemTime::now());
                let models = match result {
                    Ok(models) => models,
                    Err(error) => {
                        log::warn!("Couldn't fetch model ratings: {error:#}");
                        return;
                    }
                };
                log::info!("Artificial Analysis lists {} models", models.len());
                me.catalog = Some(Catalog::new(models.clone()));
                me.unlisted = offered_models(ctx)
                    .iter()
                    .filter(|llm| me.spec_for(llm).is_none())
                    .map(|llm| benchmark_id(llm).to_owned())
                    .collect();
                write_cache(&CachedCatalog {
                    fetched_at: me.last_attempt,
                    models,
                    unlisted: me.unlisted.clone(),
                });
            },
        );
    }

    #[cfg(target_family = "wasm")]
    fn fetch(&mut self, _: String, _: &mut ModelContext<Self>) {}
}

/// Every model the picker can offer.
fn offered_models(app: &AppContext) -> Vec<LLMInfo> {
    let preferences = LLMPreferences::as_ref(app);
    preferences
        .get_base_llm_choices_for_agent_mode_for_team_uid(None, app)
        .chain(preferences.custom_llm_choices(app))
        .cloned()
        .collect()
}

/// The model's ID without the app's own prefixes.
fn benchmark_id(llm: &LLMInfo) -> &str {
    let id = llm.id.as_str();
    ID_PREFIXES
        .iter()
        .find_map(|prefix| id.strip_prefix(prefix))
        .unwrap_or(id)
}

fn cache_path() -> PathBuf {
    warp_core::paths::state_dir().join(CACHE_FILE)
}

fn read_cache() -> Option<CachedCatalog> {
    let contents = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&contents)
        .inspect_err(|error| log::warn!("Ignoring the cached model ratings: {error}"))
        .ok()
}

fn write_cache(cache: &CachedCatalog) {
    let path = cache_path();
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, serde_json::to_vec(cache).unwrap_or_default()));
    if let Err(error) = result {
        log::warn!(
            "Couldn't cache model ratings at {}: {error}",
            path.display()
        );
    }
}

impl Entity for ModelSpecs {
    type Event = ();
}

impl SingletonEntity for ModelSpecs {}
