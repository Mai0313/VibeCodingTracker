//! Model pricing: fetch, cache, match, and cost calculation.
//!
//! This module pulls per-model token prices from LiteLLM, caches them on disk
//! (one file per calendar day), matches a session's model name against that
//! table, and computes the USD cost of a request. The public surface is the
//! re-exports below; the `cache` / `calculation` / `matching` submodules are
//! internal wiring.
//!
//! Lookup proceeds exact -> normalized -> substring -> Jaro-Winkler fuzzy
//! (see [`ModelPricingMap::get`]), and cost is computed by [`calculate_cost`]
//! across flat, threshold-tiered, and range-tiered pricing shapes.

mod cache;
mod calculation;
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

// Re-export public types and functions
pub use cache::{ModelPricing, ThresholdTier, TierRange};
pub use calculation::calculate_cost;
pub use matching::{
    ModelPricingMap, ModelPricingResult, clear_pricing_cache, normalize_model_name,
};
pub use tiers::{TierClassifier, TierThresholds};

/// Fetches AI model pricing data from the LiteLLM repository with automatic caching.
///
/// Returns an optimized pricing map with precomputed indices for fast lookups.
/// Pricing is cached locally by UTC calendar date to minimize network calls.
/// If today's cache exists and is in the current schema it is loaded directly;
/// otherwise the upstream JSON is fetched, filtered to its cost fields,
/// persisted, and parsed. A failure to write the cache is logged but does not
/// abort the fetch.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built, the LiteLLM request
/// fails, the response is not successful, or the response body does not contain
/// at least one priced model. A corrupt or legacy on-disk cache does not surface
/// here — it is logged and falls through to a refetch.
///
/// # Examples
///
/// ```no_run
/// use vibe_coding_tracker::pricing::fetch_model_pricing;
///
/// let pricing = fetch_model_pricing()?;
/// let opus = pricing.get("claude-opus-4");
/// assert!(opus.pricing.input_cost_per_token >= 0.0);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn fetch_model_pricing() -> Result<ModelPricingMap> {
    let cache_dir = get_cache_dir()?;

    // Offline mode: never hit the network, but still honour a cache hit.
    // Callers already treat a missing price as $0, so an empty map keeps
    // `usage` working (cost unavailable) without a fetch.
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

/// Fetches model pricing from an explicit URL, caching under an explicit dir.
///
/// The env-free, injectable counterpart of [`fetch_model_pricing`]: today's
/// cache under `cache_dir` short-circuits before any request, otherwise `url`
/// is fetched, filtered to its cost fields, persisted, and parsed. Tests point
/// `url` at a local mock server and `cache_dir` at a temp directory so no real
/// API is reached and the real `~/.vct` is never touched. This function does
/// **not** consult `VCT_OFFLINE` — the offline gate lives in the production
/// wrapper.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be built, the request fails, the
/// response is not successful, or the response body does not contain at least
/// one priced model. A corrupt or legacy on-disk cache is logged and falls
/// through to a refetch. Failed requests are backed off for five minutes per
/// URL and cache directory.
pub fn fetch_model_pricing_with(url: &str, cache_dir: &Path) -> Result<ModelPricingMap> {
    let today = cache::pricing_cache_date();
    let fetch_key = PricingFetchKey {
        url: url.to_string(),
        cache_dir: cache_dir.to_path_buf(),
    };

    // Check if today's cache exists
    if find_pricing_cache_for_date_in(cache_dir, &today).is_some() {
        // Load from cache
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
    // Fetch from remote
    log::info!("Fetching model pricing from remote...");
    // Bound the fetch so a slow/blocked network cannot hang the usage TUI's
    // first frame (which fetches pricing synchronously on the first launch of
    // the day, before any cache exists).
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

    // Project the upstream JSON down to just the cost-related keys before
    // anything else. Doing the filter first guarantees the on-disk cache
    // and the in-memory `ModelPricing` are derived from the *same* view of
    // the data — so nothing we price against can differ from what the
    // cache preserves for future calculation strategies.
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

    // Save the filtered raw JSON to cache. We deliberately persist the raw
    // cost keys (rather than our derived `ModelPricing` shape) so
    // priority / flex / batch / audio / image tiers that `calculate_cost`
    // doesn't consume yet are still available to future versions without
    // a re-fetch.
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

// Re-export test helper functions
#[cfg(test)]
pub use cache::normalize_pricing;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_normalize_pricing_preserves_valid_model() {
        // normalize_pricing no longer mutates prices — it only drops zero-cost
        // entries. Verify a model with valid base prices survives unchanged.
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

        // Add a valid model with non-zero costs
        pricing_map.insert(
            "valid-model".to_string(),
            ModelPricing {
                input_cost_per_token: 0.000001,
                output_cost_per_token: 0.000002,
                ..Default::default()
            },
        );

        // Add a model with all zero costs - should be filtered out
        pricing_map.insert("zero-cost-model".to_string(), ModelPricing::default());

        // Add another model with all zero costs
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

        // Only the valid model should remain
        assert_eq!(normalized.len(), 1);
        assert!(normalized.contains_key("valid-model"));
        assert!(!normalized.contains_key("zero-cost-model"));
        assert!(!normalized.contains_key("another-zero-model"));
    }
}
