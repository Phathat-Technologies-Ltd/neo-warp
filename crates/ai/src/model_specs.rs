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
#[derive(Clone, Debug, PartialEq)]
pub struct Scores {
    pub quality: f32,
    pub speed: f32,
    pub cost: f32,
    /// The display name of the benchmarked model these scores are borrowed from, when it isn't
    /// the same model: our ID carries a vendor prefix or names a version that isn't benchmarked.
    pub estimated_from: Option<String>,
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

/// The benchmarked models, with what scores are scaled against.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    models: Vec<BenchmarkedModel>,
    max_intelligence: f32,
    /// Sorted speeds and prices of the models with an intelligence score.
    speeds: Vec<f32>,
    prices: Vec<f32>,
}

impl Catalog {
    pub fn new(models: Vec<BenchmarkedModel>) -> Self {
        let rated = || models.iter().filter(|m| m.intelligence.is_some());
        let sorted = |values: Vec<f32>| {
            let mut values: Vec<f32> = values.into_iter().filter(|v| v.is_finite()).collect();
            values.sort_by(f32::total_cmp);
            values
        };
        Self {
            max_intelligence: rated()
                .filter_map(|m| m.intelligence)
                .filter(|v| v.is_finite())
                .fold(0., f32::max),
            speeds: sorted(rated().filter_map(|m| m.output_tokens_per_second).collect()),
            prices: sorted(rated().filter_map(blended_price).collect()),
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
        let (model, estimated) = match aliases.get(id) {
            Some(slug) => (self.models.iter().find(|m| &m.slug == slug)?, false),
            None => self.find(&[id, display_name])?,
        };
        let intelligence = model.intelligence?;
        Some(Scores {
            estimated_from: estimated.then(|| model.name.clone()),
            quality: ratio(intelligence, self.max_intelligence),
            speed: model
                .output_tokens_per_second
                .map_or(0., |tps| percentile(tps, &self.speeds)),
            cost: blended_price(model).map_or(0., |price| percentile(price, &self.prices)),
        })
    }

    /// The benchmarked model for the first of `names` that matches, and whether it is only an
    /// estimate. Exact matches win over a vendor-prefixed match, which wins over the nearest
    /// version of the same family.
    fn find(&self, names: &[&str]) -> Option<(&BenchmarkedModel, bool)> {
        let names: Vec<Vec<String>> = names
            .iter()
            .map(|name| tokens(name))
            .filter(|t| !t.is_empty())
            .collect();
        if let Some(model) = names.iter().find_map(|ours| self.best_match(ours)) {
            return Some((model, false));
        }
        let unprefixed: Vec<&[String]> =
            names.iter().filter_map(|ours| strip_vendor(ours)).collect();
        unprefixed
            .iter()
            .find_map(|ours| self.best_match(ours))
            .or_else(|| {
                names
                    .iter()
                    .map(Vec::as_slice)
                    .chain(unprefixed.iter().copied())
                    .find_map(|ours| self.nearest_version(ours))
            })
            .map(|model| (model, true))
    }

    /// The benchmarked model of exactly the same family (the same words, ignoring variant and size
    /// words) whose version is numerically closest to ours. Ties go to the higher intelligence.
    fn nearest_version(&self, ours: &[String]) -> Option<&BenchmarkedModel> {
        let (family, version) = family_and_version(ours)?;
        self.models
            .iter()
            .filter(|m| m.intelligence.is_some())
            .filter_map(|m| {
                [tokens(&m.slug), tokens(&m.name)]
                    .iter()
                    .filter_map(|theirs| family_and_version(theirs))
                    .filter(|(their_family, _)| *their_family == family)
                    .map(|(_, their_version)| (their_version - version).abs())
                    .min_by(f64::total_cmp)
                    .map(|distance| (distance, m))
            })
            .min_by(|(a, x), (b, y)| {
                a.total_cmp(b).then(
                    y.intelligence
                        .partial_cmp(&x.intelligence)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
            })
            .map(|(_, model)| model)
    }

    fn best_match(&self, ours: &[String]) -> Option<&BenchmarkedModel> {
        self.models
            .iter()
            .filter(|m| m.intelligence.is_some())
            .filter_map(|m| {
                let theirs = tokens(&m.slug);
                match_distance(ours, &theirs)
                    .or_else(|| match_distance(ours, &tokens(&m.name)))
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

/// `ours` without a leading vendor word, such as `zai` in `zai-glm-5.3`.
fn strip_vendor(ours: &[String]) -> Option<&[String]> {
    let (first, rest) = ours.split_first()?;
    (first.chars().all(|c| c.is_ascii_alphabetic())
        && !is_ignorable(first)
        && rest
            .iter()
            .any(|t| t.chars().all(|c| c.is_ascii_alphabetic())))
    .then_some(rest)
}

/// The sorted family words of a name and its version number (`5`, `3` reads as 5.3), or `None`
/// when the name has no family word or no version.
fn family_and_version(name: &[String]) -> Option<(Vec<&str>, f64)> {
    let mut family = Vec::new();
    let mut numbers = Vec::new();
    for token in name {
        if is_ignorable(token) {
            continue;
        }
        if token.chars().all(|c| c.is_ascii_digit()) {
            numbers.push(token.as_str());
        } else {
            family.push(token.as_str());
        }
    }
    if family.is_empty() || numbers.is_empty() {
        return None;
    }
    family.sort_unstable();
    family.dedup();
    let version = match numbers.split_first()? {
        (major, []) => major.to_string(),
        (major, rest) => format!("{major}.{}", rest.concat()),
    };
    Some((family, version.parse().ok()?))
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

/// Where `value` ranks among `sorted`, from 0 to 1. A rank rather than a ratio, because a few
/// outliers (very fast or very expensive models) would otherwise squeeze everyone else together.
/// Never quite empty, so a measured model doesn't look unmeasured.
fn percentile(value: f32, sorted: &[f32]) -> f32 {
    const MIN: f32 = 0.05;
    if sorted.is_empty() {
        return MIN;
    }
    let below = sorted.partition_point(|v| *v < value);
    let equal = sorted[below..].partition_point(|v| *v <= value);
    let rank = (below as f32 + equal as f32 / 2.) / sorted.len() as f32;
    rank.clamp(MIN, 1.)
}

#[cfg(not(target_family = "wasm"))]
pub async fn fetch_catalog(api_key: String) -> anyhow::Result<Vec<BenchmarkedModel>> {
    use anyhow::{Context, bail};

    const URL: &str = "https://artificialanalysis.ai/api/v2/language/models/free";
    const MAX_PAGES: u32 = 20;
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
