//! The one-shot `usage` scan policy, shared by every non-interactive frontend.
//!
//! Fetching pricing before the scan (so per-request context-tier classification
//! has its thresholds), degrading to base-rate classification when the fetch
//! fails, and running the scan on the caller's pool is policy the CLI used to
//! inline. Keeping it here lets a non-CLI backend (e.g. a future GUI) run the
//! exact same pipeline instead of re-deriving it.

use crate::config::ProvidersConfig;
use crate::models::TimeRange;
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::usage::{
    UsageCollection, UsageScanOptions, aggregate_usage_from_home_with_diagnostics_opts,
};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// A completed usage scan together with the pricing map it was classified with.
pub struct PricedUsageScan {
    /// Collected usage plus scan diagnostics.
    pub collection: UsageCollection,
    /// The pricing map used for tier classification; empty when the fetch failed.
    pub pricing: ModelPricingMap,
    /// The pricing-fetch error when it degraded to an empty map (costs
    /// unavailable), so the caller can surface the concrete cause; `None` on
    /// success.
    pub pricing_error: Option<String>,
}

/// Fetches pricing, derives the context-tier thresholds, and scans usage.
///
/// A failed pricing fetch is logged and downgraded to an empty map (the scan
/// still runs, classifying every request at the base rate) rather than aborting;
/// the returned [`PricedUsageScan::pricing_error`] carries the concrete cause so
/// the caller can surface it however it wants. The scan runs on `pool` so it
/// never touches Rayon's global pool.
///
/// # Errors
///
/// Propagates only a hard scan failure (an all-failed collection); pricing
/// failures degrade instead of erroring.
pub fn scan_usage_priced(
    time_range: TimeRange,
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
) -> Result<PricedUsageScan> {
    let (pricing, pricing_error) = match fetch_model_pricing() {
        Ok(map) => (map, None),
        Err(e) => {
            log::warn!("failed to fetch pricing data: {e}; costs unavailable");
            (ModelPricingMap::new(HashMap::new()), Some(e.to_string()))
        }
    };
    let options = UsageScanOptions {
        tiers: Some(Arc::new(pricing.tier_thresholds())),
    };
    let collection = pool.install(|| {
        aggregate_usage_from_home_with_diagnostics_opts(time_range, providers, &options)
    })?;
    Ok(PricedUsageScan {
        collection,
        pricing,
        pricing_error,
    })
}
