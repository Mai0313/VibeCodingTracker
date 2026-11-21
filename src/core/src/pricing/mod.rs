//! LiteLLM model pricing: fetch, daily cache, name matching, and USD cost calculation.
//!
//! Prices are cached on disk by UTC calendar date as raw LiteLLM cost-field projections and parsed on load into [`ModelPricing`]. Lookups resolve exact, normalized, substring, then Jaro-Winkler fuzzy via [`ModelPricingMap::get`], and [`calculate_cost`] prices flat, threshold-tiered, and range-tiered models.

mod cache;
mod calculation;
mod cost;
mod matching;
mod tiers;

use crate::constants::FastHashMap;
use crate::utils::{find_pricing_cache_for_date_in, get_cache_dir};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
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

static PRICING_HTTP_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();

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
            return Ok(ModelPricingMap::from_fast_map(pricing));
        }
        return Ok(ModelPricingMap::from_fast_map(FastHashMap::default()));
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
                return Ok(ModelPricingMap::from_fast_map(pricing));
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
            Ok(ModelPricingMap::from_fast_map(pricing))
        }
        Err(error) => {
            record_fetch_failure(fetch_key);
            Err(error)
        }
    }
}

fn pricing_http_client() -> Result<&'static reqwest::blocking::Client> {
    if let Some(client) = PRICING_HTTP_CLIENT.get() {
        return Ok(client);
    }
    // Bound the blocking fetch so an unreachable network cannot stall startup.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("Failed to create HTTP client")?;
    Ok(PRICING_HTTP_CLIENT.get_or_init(|| client))
}

fn is_gzip_encoded(headers: &reqwest::header::HeaderMap) -> bool {
    headers
        .get_all(reqwest::header::CONTENT_ENCODING)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|encoding| {
            let encoding = encoding.trim();
            encoding.eq_ignore_ascii_case("gzip") || encoding.eq_ignore_ascii_case("x-gzip")
        })
}

fn fetch_model_pricing_remote(
    url: &str,
    cache_dir: &Path,
) -> Result<FastHashMap<Arc<str>, ModelPricing>> {
    log::info!("Fetching model pricing from remote...");
    let client = pricing_http_client()?;

    // Request gzip manually and decode via `flate2` so we cut transfer size without enabling `reqwest`'s decompression crate tree.
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT_ENCODING, "gzip")
        .send()
        .context("Failed to fetch model pricing from LiteLLM")?;

    anyhow::ensure!(
        response.status().is_success(),
        "Failed to fetch model pricing from LiteLLM: HTTP {}",
        response.status()
    );

    let raw: serde_json::Value = if is_gzip_encoded(response.headers()) {
        let mut decoder = flate2::read::MultiGzDecoder::new(response);
        let mut decompressed = Vec::new();
        decoder
            .read_to_end(&mut decompressed)
            .context("Failed to decompress gzip model pricing response")?;
        serde_json::from_slice(&decompressed).context("Failed to parse model pricing JSON")?
    } else {
        response
            .json()
            .context("Failed to parse model pricing JSON")?
    };

    anyhow::ensure!(
        raw.is_object(),
        "Invalid model pricing JSON: top-level value must be an object"
    );

    // Filter before parsing so in-memory pricing and the on-disk cache derive from the exact same subset of keys.
    let filtered_raw = cache::build_filtered_cost_json(&raw);
    let pricing = cache::parse_and_validate_litellm_map(&filtered_raw)?;

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

    #[test]
    fn pricing_http_client_is_reused() {
        let first = pricing_http_client().expect("build pricing client");
        let second = pricing_http_client().expect("reuse pricing client");
        assert!(std::ptr::eq(first, second));
    }

    #[test]
    fn is_gzip_encoded_matches_header_variants() {
        use reqwest::header::{CONTENT_ENCODING, HeaderMap, HeaderValue};

        let mut headers = HeaderMap::new();
        assert!(!is_gzip_encoded(&headers));

        headers.insert(CONTENT_ENCODING, HeaderValue::from_static("identity"));
        assert!(!is_gzip_encoded(&headers));

        for value in ["gzip", "GZIP", "x-gzip", "identity, gzip"] {
            headers.insert(CONTENT_ENCODING, HeaderValue::from_static(value));
            assert!(is_gzip_encoded(&headers), "expected gzip match for {value}");
        }
    }
}
