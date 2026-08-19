//! The one-shot `usage` scan policy, shared by every non-interactive frontend.
//!
//! [`scan_usage_priced`] fetches pricing first (so per-request context-tier
//! classification has its thresholds), degrades to base-rate classification
//! when that fetch fails, and runs the scan on the caller's pool. It lives in
//! core so a non-CLI backend runs the same pipeline instead of re-deriving it.

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
    /// unavailable); `None` on success.
    pub pricing_error: Option<String>,
}

/// Fetches pricing, derives the context-tier thresholds, and scans usage.
///
/// A failed pricing fetch is logged and downgraded to an empty map (the scan
/// still runs, classifying every request at the base rate) rather than aborting;
/// the returned [`PricedUsageScan::pricing_error`] carries the concrete cause.
/// The scan runs on `pool` so it never touches Rayon's global pool.
///
/// # Errors
///
/// Returns an error only when the provider paths cannot be resolved (e.g. the
/// home directory is unavailable). Pricing failures degrade instead of erroring,
/// and an unreadable source is recorded in the collection's diagnostics.
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
