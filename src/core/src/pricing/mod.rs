//! LiteLLM model pricing: fetch, daily cache, name matching, and USD cost calculation.
//!
//! Prices are cached on disk by UTC calendar date as raw LiteLLM cost-field projections and parsed on load into [`ModelPricing`]. Lookups resolve exact, normalized, substring, then Jaro-Winkler fuzzy via [`ModelPricingMap::get`], and [`calculate_cost`] prices flat, threshold-tiered, and range-tiered models.

mod cache;
mod calculation;
mod cost;
mod matching;
mod tiers;

use crate::utils::{find_pricing_cache_for_date_in, get_cache_dir};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const LITELLM_PRICING_URL: &str =
    "https://github.com/BerriAI/litellm/raw/refs/heads/main/model_prices_and_context_window.json";
const PRICING_FETCH_FAILURE_BACKOFF: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PricingFetchKey {
    url: String,
    cache_dir: PathBuf,
}

static FAILED_FETCHES: LazyLock<Mutex<HashMap<PricingFetchKey, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub use cache::{ModelPricing, ThresholdTier, TierRange};
pub use calculation::calculate_cost;
pub use cost::{CostSource, resolve_model_cost};
pub use matching::{
    ModelPricingMap, ModelPricingResult, clear_pricing_cache, normalize_model_name,
};
pub use tiers::{TierClassifier, TierThresholds};

/// Returns model pricing from today's UTC cache, fetching from LiteLLM on a miss.
///
/// A missing, corrupt, or legacy on-disk cache falls through to a remote fetch, and a cache write failure is logged without failing the call. When `VCT_OFFLINE` is set, no network request is made and a cache miss returns an empty [`ModelPricingMap`]. Failed remote fetches are backed off for five minutes per URL and cache directory.
///
/// # Examples
///
/// ```no_run
/// use vct_core::pricing::fetch_model_pricing;
///
/// let pricing = fetch_model_pricing()?;
/// let opus = pricing.get("claude-opus-4");
/// assert!(opus.pricing.input_cost_per_token >= 0.0);
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
///
/// Returns an error if the cache directory cannot be resolved, a recent fetch failure is still in backoff, the HTTP request fails or returns a non-success status, or the payload is not valid JSON containing at least one valid priced model.
pub fn fetch_model_pricing() -> Result<ModelPricingMap> {
    let cache_dir = get_cache_dir()?;

    if crate::utils::network_disabled() {
        let today = cache::pricing_cache_date();
        if find_pricing_cache_for_date_in(&cache_dir, &today).is_some()
            && let Ok(pricing) = cache::load_from_cache_in(&cache_dir)
        {
            log::debug!("Loaded model pricing from today's cache (offline)");
            return Ok(ModelPricingMap::new(pricing));
        }
        return Ok(ModelPricingMap::new(std::collections::HashMap::new()));
    }

    fetch_model_pricing_with(LITELLM_PRICING_URL, &cache_dir)
}

/// Returns model pricing from today's cache under `cache_dir`, fetching `url` on a miss.
///
/// Unlike [`fetch_model_pricing`], this does not consult `VCT_OFFLINE`. A corrupt or legacy cache is logged and triggers a refetch; cache write failures are logged without failing the call. Failed remote fetches are backed off for five minutes per `(url, cache_dir)`.
///
/// # Errors
///
/// Returns an error if a recent fetch failure is still in backoff, the HTTP client or request fails, the status is not successful, or the body is not valid JSON containing at least one valid priced model.
pub fn fetch_model_pricing_with(url: &str, cache_dir: &Path) -> Result<ModelPricingMap> {
    let today = cache::pricing_cache_date();
    let fetch_key = PricingFetchKey {
        url: url.to_string(),
        cache_dir: cache_dir.to_path_buf(),
    };

    if find_pricing_cache_for_date_in(cache_dir, &today).is_some() {
        match cache::load_from_cache_in(cache_dir) {
            Ok(pricing) => {
                log::debug!("Loaded model pricing from today's cache");
                clear_fetch_failure(&fetch_key);
                return Ok(ModelPricingMap::new(pricing));
            }
            Err(e) => {
                log::warn!("Failed to load from cache: {}, fetching from remote", e);
            }
        }
    }

    if let Some(remaining) = fetch_backoff_remaining(&fetch_key) {
        anyhow::bail!(
            "Model pricing fetch is in failure backoff; retry in {} seconds",
            remaining.as_secs().max(1)
        );
    }

    let result = fetch_model_pricing_remote(url, cache_dir);
    match result {
        Ok(pricing) => {
            clear_fetch_failure(&fetch_key);
            Ok(ModelPricingMap::new(pricing))
        }
        Err(error) => {
            record_fetch_failure(fetch_key);
            Err(error)
        }
    }
}

fn fetch_model_pricing_remote(
    url: &str,
    cache_dir: &Path,
) -> Result<HashMap<String, ModelPricing>> {
    log::info!("Fetching model pricing from remote...");
    // Bound the blocking fetch so an unreachable network cannot stall startup.
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .get(url)
        .send()
        .context("Failed to fetch model pricing from LiteLLM")?;

    anyhow::ensure!(
        response.status().is_success(),
        "Failed to fetch model pricing from LiteLLM: HTTP {}",
        response.status()
    );

    let raw: serde_json::Value = response
        .json()
        .context("Failed to parse model pricing JSON")?;

    anyhow::ensure!(
        raw.is_object(),
        "Invalid model pricing JSON: top-level value must be an object"
    );

    // Filter before parsing so in-memory pricing and the on-disk cache derive from the exact same subset of keys.
    let filtered_raw = cache::build_filtered_cost_json(&raw);

    let parsed = cache::parse_litellm_pricing_map(filtered_raw.clone());
    anyhow::ensure!(
        cache::pricing_rates_are_valid(&parsed),
        "Invalid model pricing JSON: negative or non-finite price"
    );
    let pricing = cache::normalize_pricing(parsed);
    anyhow::ensure!(
        !pricing.is_empty(),
        "Invalid model pricing JSON: no priced models"
    );

    // Persist the raw cost-key projection rather than `ModelPricing` so unparsed LiteLLM cost keys remain available to future versions.
    if let Err(e) = cache::save_to_cache_in(cache_dir, &filtered_raw) {
        log::warn!("Failed to save pricing to cache: {}", e);
    } else {
        log::debug!("Saved model pricing to cache with today's date");
    }

    Ok(pricing)
}

fn fetch_backoff_remaining(key: &PricingFetchKey) -> Option<Duration> {
    let mut failures = FAILED_FETCHES.lock().ok()?;
    failures.retain(|_, failed_at| failed_at.elapsed() < PRICING_FETCH_FAILURE_BACKOFF);
    let failed_at = failures.get(key)?;
    PRICING_FETCH_FAILURE_BACKOFF.checked_sub(failed_at.elapsed())
}

fn record_fetch_failure(key: PricingFetchKey) {
    if let Ok(mut failures) = FAILED_FETCHES.lock() {
        failures.insert(key, Instant::now());
    }
}

fn clear_fetch_failure(key: &PricingFetchKey) {
    if let Ok(mut failures) = FAILED_FETCHES.lock() {
        failures.remove(key);
    }
}

#[cfg(test)]
pub use cache::normalize_pricing;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_normalize_pricing_preserves_valid_model() {
        let mut pricing_map = HashMap::new();
        pricing_map.insert(
            "test-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                cache_read_input_token_cost: 0.0000001,
                cache_creation_input_token_cost: 0.0000005,
                ..Default::default()
            },
        );

        let normalized = cache::normalize_pricing(pricing_map);
        let p = normalized.get("test-model").unwrap();

        assert_eq!(p.input_cost_per_token, 0.000001);
        assert_eq!(p.output_cost_per_token, 0.000002);
        assert_eq!(p.cache_read_input_token_cost, 0.0000001);
        assert_eq!(p.cache_creation_input_token_cost, 0.0000005);
        assert!(p.tiers.is_empty());
        assert!(p.ranges.is_none());
    }

    #[test]
    fn test_normalize_pricing_filters_zero_cost_models() {
        let mut pricing_map = HashMap::new();

        pricing_map.insert(
            "valid-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                ..Default::default()
            },
        );

        pricing_map.insert("zero-cost-model".to_string(), ModelPricing::default());

        pricing_map.insert(
            "another-zero-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.0,
                output_cost_per_token: 0.0,
                cache_read_input_token_cost: 0.0,
                cache_creation_input_token_cost: 0.0,
                ..Default::default()
            },
        );

        let normalized = cache::normalize_pricing(pricing_map);

        assert_eq!(normalized.len(), 1);
        assert!(normalized.contains_key("valid-model"));
        assert!(!normalized.contains_key("zero-cost-model"));
        assert!(!normalized.contains_key("another-zero-model"));
    }
}
