//! Model intelligence, speed, and price from Artificial Analysis's free Data API, matched to the
//! models the app offers. Artificial Analysis requires visible attribution wherever this data is
//! shown.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

pub const ATTRIBUTION: &str = "Intelligence and speed from Artificial Analysis \
(artificialanalysis.ai), measured on each model's own provider.";

/// Words that name a variant of a model (reasoning effort, release channel) rather than a
/// different model, so they may differ between our ID and Artificial Analysis's.
const VARIANT_WORDS: &[&str] = &[
    "adaptive",
    "chat",
    "exp",
    "experimental",
    "high",
    "instruct",
    "latest",
    "low",
    "max",
    "medium",
    "minimal",
    "non",
    "nonreasoning",
    "preview",
    "reasoning",
    "thinking",
    "xhigh",
];

/// One model as Artificial Analysis lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkedModel {
    pub slug: String,
    pub name: String,
    /// The Artificial Analysis Intelligence Index.
    pub intelligence: Option<f32>,
    /// Median output tokens per second on the model creator's own API.
    pub output_tokens_per_second: Option<f32>,
    pub price_1m_input_tokens: Option<f32>,
    pub price_1m_output_tokens: Option<f32>,
}

/// Scores from 0 to 1, as the model picker draws them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scores {
    pub quality: f32,
    pub speed: f32,
    pub cost: f32,
}

#[derive(Deserialize)]
pub(crate) struct FreeModelsPage {
    pub data: Vec<FreeModel>,
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

#[derive(Deserialize)]
pub(crate) struct Pagination {
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Deserialize)]
pub(crate) struct FreeModel {
    slug: String,
    name: String,
    #[serde(default)]
    evaluations: Option<Evaluations>,
    #[serde(default)]
    pricing: Option<Pricing>,
    #[serde(default)]
    performance: Option<Performance>,
}

#[derive(Deserialize)]
struct Evaluations {
    #[serde(default)]
    artificial_analysis_intelligence_index: Option<f32>,
}

#[derive(Deserialize)]
struct Pricing {
    #[serde(default)]
    price_1m_input_tokens: Option<f32>,
    #[serde(default)]
    price_1m_output_tokens: Option<f32>,
}

#[derive(Deserialize)]
struct Performance {
    #[serde(default)]
    median_output_tokens_per_second: Option<f32>,
}

impl From<FreeModel> for BenchmarkedModel {
    fn from(model: FreeModel) -> Self {
        Self {
            slug: model.slug,
            name: model.name,
            intelligence: model
                .evaluations
                .and_then(|e| e.artificial_analysis_intelligence_index),
            output_tokens_per_second: model
                .performance
                .and_then(|p| p.median_output_tokens_per_second),
            price_1m_input_tokens: model.pricing.as_ref().and_then(|p| p.price_1m_input_tokens),
            price_1m_output_tokens: model.pricing.and_then(|p| p.price_1m_output_tokens),
        }
    }
}

/// The benchmarked models, with the maxima scores are scaled against.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    models: Vec<BenchmarkedModel>,
    max_intelligence: f32,
    max_speed: f32,
    max_price: f32,
}

impl Catalog {
    pub fn new(models: Vec<BenchmarkedModel>) -> Self {
        let max = |value: fn(&BenchmarkedModel) -> Option<f32>| {
            models
                .iter()
                .filter_map(value)
                .filter(|v| v.is_finite())
                .fold(0., f32::max)
        };
        Self {
            max_intelligence: max(|m| m.intelligence),
            max_speed: max(|m| m.output_tokens_per_second),
            max_price: max(blended_price),
            models,
        }
    }

    pub fn models(&self) -> &[BenchmarkedModel] {
        &self.models
    }

    /// Scores for the model the app calls `id` (or `display_name`), when it matches a benchmarked
    /// model with an intelligence score. `aliases` maps our IDs to Artificial Analysis slugs where
    /// matching by name falls short.
    pub fn scores_for(
        &self,
        id: &str,
        display_name: &str,
        aliases: &HashMap<String, String>,
    ) -> Option<Scores> {
        let model = match aliases.get(id) {
            Some(slug) => self.models.iter().find(|m| &m.slug == slug),
            None => self
                .best_match(id)
                .or_else(|| self.best_match(display_name)),
        }?;
        let intelligence = model.intelligence?;
        Some(Scores {
            quality: ratio(intelligence, self.max_intelligence),
            speed: model
                .output_tokens_per_second
                .map_or(0., |tps| log_ratio(tps, self.max_speed)),
            cost: blended_price(model).map_or(0., |price| log_ratio(price, self.max_price)),
        })
    }

    fn best_match(&self, name: &str) -> Option<&BenchmarkedModel> {
        let ours = tokens(name);
        if ours.is_empty() {
            return None;
        }
        self.models
            .iter()
            .filter(|m| m.intelligence.is_some())
            .filter_map(|m| {
                let theirs = tokens(&m.slug);
                match_distance(&ours, &theirs)
                    .or_else(|| match_distance(&ours, &tokens(&m.name)))
                    .map(|distance| (distance, m))
            })
            // Among equally close variants, the highest-scoring one is the default the
            // provider serves.
            .min_by(|(a, x), (b, y)| {
                a.cmp(b).then(
                    y.intelligence
                        .partial_cmp(&x.intelligence)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            })
            .map(|(_, model)| model)
    }
}

/// How many variant or size words separate two names that otherwise name the same model, or
/// `None` when they differ in anything else, including any version number.
fn match_distance(a: &[String], b: &[String]) -> Option<usize> {
    // Counted, so `5.5` and `5` stay different versions.
    let mut counts: HashMap<&str, i32> = HashMap::new();
    for token in a {
        *counts.entry(token).or_default() += 1;
    }
    for token in b {
        *counts.entry(token).or_default() -= 1;
    }
    let mut distance = 0;
    for (token, count) in counts {
        if count == 0 {
            continue;
        }
        if !is_ignorable(token) {
            return None;
        }
        distance += count.unsigned_abs() as usize;
    }
    Some(distance)
}

fn is_ignorable(token: &str) -> bool {
    VARIANT_WORDS.contains(&token) || is_size_tag(token) || is_date(token)
}

/// Parameter counts and similar tags, such as `480b`, `a35b`, or `16e`.
fn is_size_tag(token: &str) -> bool {
    let body = token.strip_prefix('a').unwrap_or(token);
    body.len() >= 2
        && body.ends_with(['b', 'e', 'm'])
        && body[..body.len() - 1].chars().all(|c| c.is_ascii_digit())
}

fn is_date(token: &str) -> bool {
    token.len() >= 6 && token.chars().all(|c| c.is_ascii_digit())
}

/// Lowercase words of a model name, splitting a family name from a glued version (`qwen3` becomes
/// `qwen`, `3`) but keeping size tags whole.
fn tokens(name: &str) -> Vec<String> {
    name.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .flat_map(|word| {
            let split = word.find(|c: char| c.is_ascii_digit()).filter(|&i| {
                i > 0
                    && word[..i].chars().all(|c| c.is_ascii_alphabetic())
                    && word[i..].chars().all(|c| c.is_ascii_digit())
            });
            match split {
                Some(i) if !is_size_tag(word) => vec![word[..i].to_owned(), word[i..].to_owned()],
                _ => vec![word.to_owned()],
            }
        })
        .collect()
}

/// Price per million tokens, weighted three input tokens to one output token.
fn blended_price(model: &BenchmarkedModel) -> Option<f32> {
    let input = model.price_1m_input_tokens?;
    let output = model.price_1m_output_tokens?;
    Some((3. * input + output) / 4.)
}

fn ratio(value: f32, max: f32) -> f32 {
    if max <= 0. {
        return 0.;
    }
    (value / max).clamp(0., 1.)
}

/// `value` against `max` on a log scale, so a 10x difference reads as a clear gap rather than an
/// empty bar.
fn log_ratio(value: f32, max: f32) -> f32 {
    if max <= 0. {
        return 0.;
    }
    ((1. + value).ln() / (1. + max).ln()).clamp(0., 1.)
}

#[cfg(not(target_family = "wasm"))]
pub async fn fetch_catalog(api_key: String) -> anyhow::Result<Vec<BenchmarkedModel>> {
    use anyhow::{Context, bail};

    const URL: &str = "https://artificialanalysis.ai/api/v2/language/models/free";
    const MAX_PAGES: u32 = 10;
    let mut models = Vec::new();
    for page in 1..=MAX_PAGES {
        let response = http_client::Client::new()
            .get(format!("{URL}?page={page}"))
            .header("x-api-key", &api_key)
            .send()
            .await
            .context("failed to reach Artificial Analysis")?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("Artificial Analysis refused the model list ({status}): {body}");
        }
        let page: FreeModelsPage =
            serde_json::from_str(&body).context("unexpected Artificial Analysis response")?;
        let has_more = page.pagination.is_some_and(|p| p.has_more);
        models.extend(page.data.into_iter().map(BenchmarkedModel::from));
        if !has_more {
            break;
        }
    }
    Ok(models)
}

#[cfg(test)]
#[path = "model_specs_tests.rs"]
mod tests;
