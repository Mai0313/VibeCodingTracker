//! Per-request context-tier classification support.
//!
//! LiteLLM publishes `*_above_Nk_tokens` price tiers whose real billing
//! semantics are **per request**: a request is promoted to the tier rate only
//! when its own prompt context exceeds the threshold. The usage aggregator,
//! however, merges tokens across records, files, and sessions before pricing,
//! so tier selection at pricing time would compare the threshold against a
//! cumulative figure and promote a whole month of small requests to the
//! elevated rate.
//!
//! [`TierThresholds`] is a `Send + Sync` snapshot of "model → lowest tier
//! threshold" derived from a [`ModelPricingMap`](super::ModelPricingMap). The
//! usage scan hands it to the session parsers, which classify each request as
//! it is folded and accumulate the above-threshold slice into a separate
//! `above_tier` bucket that `calculate_cost` bills at the tier rate. Parsers
//! without the snapshot (the `analysis` paths, offline runs) classify nothing,
//! which degrades to billing everything at base rates.

use crate::constants::FastHashMap;
use crate::pricing::normalize_model_name;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Immutable "model → lowest context-tier threshold (tokens)" snapshot.
///
/// Keys are stored both as the LiteLLM key lowercased and as its normalized
/// form, so the session-log model names (`gpt-5.4`, `azure/gpt-5.5`,
/// `claude-sonnet-5`) resolve without re-implementing the full pricing match
/// chain. A model that resolves to no entry simply has no tier.
#[derive(Debug, Default)]
pub struct TierThresholds {
    thresholds: FastHashMap<Box<str>, i64>,
    fingerprint: u64,
}

impl TierThresholds {
    /// Builds the snapshot from `(model key, lowest threshold)` pairs.
    ///
    /// On key collisions (e.g. `openai/gpt-5.4` and `azure/gpt-5.4`
    /// normalizing to the same name) the smallest threshold wins — the
    /// conservative choice given the tier rate is the higher one.
    pub(crate) fn from_entries<'a>(entries: impl Iterator<Item = (&'a str, i64)>) -> Self {
        let mut thresholds: FastHashMap<Box<str>, i64> = FastHashMap::default();
        let mut insert_min = |key: String, threshold: i64| {
            thresholds
                .entry(key.into_boxed_str())
                .and_modify(|existing| *existing = (*existing).min(threshold))
                .or_insert(threshold);
        };
        for (key, threshold) in entries {
            insert_min(key.to_lowercase(), threshold);
            insert_min(normalize_model_name(key), threshold);
        }

        // Order-independent fingerprint so scan caches can detect a changed
        // snapshot (daily pricing reload) without hashing map iteration order.
        let mut fingerprint = thresholds.len() as u64;
        for (key, threshold) in &thresholds {
            let mut hasher = DefaultHasher::new();
            (key, threshold).hash(&mut hasher);
            fingerprint ^= hasher.finish();
        }

        Self {
            thresholds,
            fingerprint,
        }
    }

    /// Lowest tier threshold for `model`, or `None` when the model has no
    /// context tier (or cannot be resolved).
    pub fn threshold_for(&self, model: &str) -> Option<i64> {
        if self.thresholds.is_empty() {
            return None;
        }
        let lowered = model.to_lowercase();
        if let Some(threshold) = self.thresholds.get(lowered.as_str()) {
            return Some(*threshold);
        }
        self.thresholds
            .get(normalize_model_name(model).as_str())
            .copied()
    }

    /// Whether no model carries a tier (nothing will ever classify).
    pub fn is_empty(&self) -> bool {
        self.thresholds.is_empty()
    }

    /// Stable identity of this snapshot's contents, `0` only when empty.
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

/// Per-parse memoized classifier over a [`TierThresholds`] snapshot.
///
/// Memoizes the per-model resolution (which lowercases/normalizes the name)
/// so the per-record hot path is one map hit plus an integer comparison.
#[derive(Debug)]
pub struct TierClassifier<'a> {
    thresholds: &'a TierThresholds,
    memo: FastHashMap<String, Option<i64>>,
}

impl<'a> TierClassifier<'a> {
    pub fn new(thresholds: &'a TierThresholds) -> Self {
        Self {
            thresholds,
            memo: FastHashMap::default(),
        }
    }

    /// Whether a request for `model` with `request_context` prompt tokens
    /// (input + cache read + cache creation) is billed at the tier rate.
    pub fn is_above(&mut self, model: &str, request_context: i64) -> bool {
        let threshold = match self.memo.get(model) {
            Some(threshold) => *threshold,
            None => {
                let resolved = self.thresholds.threshold_for(model);
                self.memo.insert(model.to_string(), resolved);
                resolved
            }
        };
        threshold.is_some_and(|threshold| request_context > threshold)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> TierThresholds {
        TierThresholds::from_entries(
            [("gpt-5.4", 272_000), ("gemini-3.1-pro-preview", 200_000)].into_iter(),
        )
    }

    #[test]
    fn resolves_exact_and_normalized_names() {
        let tiers = snapshot();
        assert_eq!(tiers.threshold_for("gpt-5.4"), Some(272_000));
        assert_eq!(tiers.threshold_for("GPT-5.4"), Some(272_000));
        assert_eq!(tiers.threshold_for("gpt-4o"), None);
    }

    #[test]
    fn collision_keeps_the_smallest_threshold() {
        let tiers = TierThresholds::from_entries(
            [("openai/gpt-x", 272_000), ("gpt-x", 200_000)].into_iter(),
        );
        assert_eq!(tiers.threshold_for("gpt-x"), Some(200_000));
    }

    #[test]
    fn classifier_compares_strictly_above() {
        let tiers = snapshot();
        let mut classifier = TierClassifier::new(&tiers);
        assert!(!classifier.is_above("gpt-5.4", 272_000));
        assert!(classifier.is_above("gpt-5.4", 272_001));
        assert!(!classifier.is_above("no-tier-model", i64::MAX));
        // Memoized second lookup takes the fast path.
        assert!(classifier.is_above("gpt-5.4", 300_000));
    }

    #[test]
    fn fingerprint_is_order_independent_and_content_sensitive() {
        let a = TierThresholds::from_entries([("m1", 100), ("m2", 200)].into_iter());
        let b = TierThresholds::from_entries([("m2", 200), ("m1", 100)].into_iter());
        let c = TierThresholds::from_entries([("m1", 100), ("m2", 300)].into_iter());
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
        assert_eq!(TierThresholds::default().fingerprint(), 0);
    }
}
