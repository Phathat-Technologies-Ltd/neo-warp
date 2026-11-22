//! Model picker ratings from Artificial Analysis, for organization builds whose admin configured
//! a key. Fetched at most once a day and cached on disk, so the free tier's daily quota holds.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use ::ai::model_specs::{BenchmarkedModel, Catalog};
use serde::{Deserialize, Serialize};
use warpui::r#async::Timer;
use warpui::{Entity, ModelContext, SingletonEntity};

use crate::ai::llms::{LLMInfo, LLMSpec};
use crate::auth;

const CACHE_FILE: &str = "artificial-analysis-models.json";
const REFRESH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Prefixes the app gives client-side models' IDs, which Artificial Analysis doesn't know.
const ID_PREFIXES: &[&str] = &["cerebras:", "chatgpt:"];

#[derive(Serialize, Deserialize)]
struct CachedCatalog {
    fetched_at: SystemTime,
    models: Vec<BenchmarkedModel>,
}

pub struct ModelSpecs {
    catalog: Option<Catalog>,
    aliases: HashMap<String, String>,
}

impl ModelSpecs {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let Some(config) = auth::organization::model_specs_config() else {
            return Self {
                catalog: None,
                aliases: HashMap::new(),
            };
        };
        let cached = read_cache();
        let next_refresh = cached
            .as_ref()
            .and_then(|cache| cache.fetched_at.elapsed().ok())
            .map_or(Duration::ZERO, |age| REFRESH_INTERVAL.saturating_sub(age));
        schedule_refresh(
            config.artificial_analysis_api_key.clone(),
            next_refresh,
            ctx,
        );
        Self {
            catalog: cached.map(|cache| Catalog::new(cache.models)),
            aliases: config.aliases.clone(),
        }
    }

    /// Whether ratings come from Artificial Analysis, which then must be credited.
    pub fn is_active(&self) -> bool {
        self.catalog.is_some()
    }

    /// Ratings for `llm`, when it matches a benchmarked model.
    pub fn spec_for(&self, llm: &LLMInfo) -> Option<LLMSpec> {
        let catalog = self.catalog.as_ref()?;
        let id = llm.id.as_str();
        let id = ID_PREFIXES
            .iter()
            .find_map(|prefix| id.strip_prefix(prefix))
            .unwrap_or(id);
        catalog
            .scores_for(id, &llm.display_name, &self.aliases)
            .map(|scores| LLMSpec {
                cost: scores.cost,
                quality: scores.quality,
                speed: scores.speed,
            })
    }
}

#[cfg(target_family = "wasm")]
fn schedule_refresh(_: String, _: Duration, _: &mut ModelContext<ModelSpecs>) {}

#[cfg(not(target_family = "wasm"))]
fn schedule_refresh(api_key: String, delay: Duration, ctx: &mut ModelContext<ModelSpecs>) {
    ctx.spawn(
        async move {
            Timer::after(delay).await;
            ::ai::model_specs::fetch_catalog(api_key.clone())
                .await
                .map(|models| (api_key, models))
        },
        |me, result, ctx| {
            let api_key = match result {
                Ok((api_key, models)) => {
                    log::info!("Artificial Analysis lists {} models", models.len());
                    write_cache(&models);
                    me.catalog = Some(Catalog::new(models));
                    api_key
                }
                Err(error) => {
                    log::warn!("Couldn't refresh model ratings: {error:#}");
                    match auth::organization::model_specs_config() {
                        Some(config) => config.artificial_analysis_api_key.clone(),
                        None => return,
                    }
                }
            };
            schedule_refresh(api_key, REFRESH_INTERVAL, ctx);
        },
    );
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

fn write_cache(models: &[BenchmarkedModel]) {
    let cache = CachedCatalog {
        fetched_at: SystemTime::now(),
        models: models.to_vec(),
    };
    let path = cache_path();
    let result = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, serde_json::to_vec(&cache).unwrap_or_default()));
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
