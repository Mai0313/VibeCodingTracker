//! Pricing-first one-shot `usage` scan pipeline.
//!
//! [`scan_usage_priced`] fetches pricing to derive context-tier thresholds, degrades to base-rate classification on fetch failure, and runs the scan on the caller's Rayon pool.

use crate::config::ProvidersConfig;
use crate::models::TimeRange;
use crate::pricing::{ModelPricingMap, fetch_model_pricing};
use crate::usage::aggregator::{
    aggregate_daily_usage_from_home_with_diagnostics_opts_and_remote_hook,
    aggregate_usage_from_home_with_diagnostics_opts_and_remote_hook,
};
use crate::usage::{DailyUsageCollection, UsageCollection, UsageScanOptions};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

/// A completed usage scan together with the pricing map it was classified with.
pub struct PricedUsageScan {
    /// Collected usage plus scan diagnostics.
    pub collection: UsageCollection,
    /// The pricing map used for tier classification; empty when the fetch failed.
    pub pricing: ModelPricingMap,
    /// Pricing-fetch error message when degraded to an empty map, or `None` on success.
    pub pricing_error: Option<String>,
}

/// A completed per-day usage scan together with the pricing map it was classified with.
pub struct PricedDailyUsageScan {
    /// Collected per-day usage plus scan diagnostics.
    pub collection: DailyUsageCollection,
    /// The pricing map used for tier classification; empty when the fetch failed.
    pub pricing: ModelPricingMap,
    /// Pricing-fetch error message when degraded to an empty map, or `None` on success.
    pub pricing_error: Option<String>,
}

fn fetch_pricing_or_degrade() -> (ModelPricingMap, Option<String>, UsageScanOptions) {
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
    (pricing, pricing_error, options)
}

/// Fetches model pricing, derives context-tier thresholds, and runs a usage scan on `pool`.
///
/// Pricing fetch failures are logged and degraded to an empty [`ModelPricingMap`] (classifying all requests at base rates) with the error recorded in [`PricedUsageScan::pricing_error`]. Execution stays isolated to `pool` without initializing Rayon's global pool.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved. Source read failures are recorded in [`UsageCollection::diagnostics`] instead.
pub fn scan_usage_priced(
    time_range: TimeRange,
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
) -> Result<PricedUsageScan> {
    scan_usage_priced_with_remote_hook(time_range, providers, pool, || {})
}

/// Like [`scan_usage_priced`], invoking `before_remotes` after local scanning finishes and before remote snapshots are loaded and folded.
#[doc(hidden)]
pub fn scan_usage_priced_with_remote_hook(
    time_range: TimeRange,
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
    before_remotes: impl FnOnce() + Send,
) -> Result<PricedUsageScan> {
    let (pricing, pricing_error, options) = fetch_pricing_or_degrade();
    let collection = pool.install(|| {
        aggregate_usage_from_home_with_diagnostics_opts_and_remote_hook(
            time_range,
            providers,
            &options,
            before_remotes,
        )
    })?;
    Ok(PricedUsageScan {
        collection,
        pricing,
        pricing_error,
    })
}

/// Fetches model pricing, derives context-tier thresholds, and runs a per-day usage scan on `pool`.
///
/// Uses the same pricing fetch, tier-threshold classification, provider filtering, and remote deduplication as [`scan_usage_priced`], preserving local `YYYY-MM-DD` day boundaries.
///
/// # Errors
///
/// Returns an error if provider paths cannot be resolved. Source read failures are recorded in [`DailyUsageCollection::diagnostics`] instead.
pub fn scan_daily_usage_priced(
    time_range: TimeRange,
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
) -> Result<PricedDailyUsageScan> {
    scan_daily_usage_priced_with_remote_hook(time_range, providers, pool, || {})
}

/// Like [`scan_daily_usage_priced`], invoking `before_remotes` after local scanning finishes and before remote snapshots are loaded and folded.
#[doc(hidden)]
pub fn scan_daily_usage_priced_with_remote_hook(
    time_range: TimeRange,
    providers: ProvidersConfig,
    pool: &rayon::ThreadPool,
    before_remotes: impl FnOnce() + Send,
) -> Result<PricedDailyUsageScan> {
    let (pricing, pricing_error, options) = fetch_pricing_or_degrade();
    let collection = pool.install(|| {
        aggregate_daily_usage_from_home_with_diagnostics_opts_and_remote_hook(
            time_range,
            providers,
            &options,
            before_remotes,
        )
    })?;
    Ok(PricedDailyUsageScan {
        collection,
        pricing,
        pricing_error,
    })
}
