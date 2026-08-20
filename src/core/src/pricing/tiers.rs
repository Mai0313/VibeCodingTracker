//! Per-request context-tier classification support.
//!
//! LiteLLM publishes `*_above_Nk_tokens` price tiers and `tiered_pricing`
//! ranges whose real billing semantics are **per request**: a request bills at
//! the price level its own prompt context falls in. The usage aggregator,
//! however, merges tokens across records, files, and sessions before pricing,
//! so selection at pricing time would compare the boundaries against a
//! cumulative figure and promote a whole month of small requests to the
//! elevated rate.
//!
//! [`TierThresholds`] is a `Send + Sync` snapshot of "model → the ascending
//! boundaries between its price levels" derived from a
//! [`ModelPricingMap`](super::ModelPricingMap). The usage scan hands it to the
//! session parsers, which classify each request as it is folded and accumulate
//! its buckets into that level's slice of the `above_tier` object, which
//! `calculate_cost` bills at the matching level's prices. Parsers without the
//! snapshot (the `analysis` paths, offline runs) classify nothing, which
//! degrades to billing everything at the model's lowest level.

use crate::constants::FastHashMap;
use crate::pricing::normalize_model_name;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Immutable "model → ascending price-level boundaries (tokens)" snapshot.
///
/// Keys are stored both as the LiteLLM key lowercased and as its normalized
/// form, so the session-log model names (`gpt-5.4`, `azure/gpt-5.5`,
/// `claude-sonnet-5`) resolve without re-implementing the full pricing match
/// chain. A model that resolves to no entry simply has one price level.
///
/// A boundary means the same thing to a classifier whichever pricing strategy
/// produced it — "above this one, the next price level applies" — but the two
/// strategies read a level back against different price rows, so a normalized
/// collision between a range-based and a threshold-based model would classify
/// one of them against the other's boundaries. No such collision exists in the
/// LiteLLM data today.
#[derive(Debug, Default)]
pub struct TierThresholds {
    boundaries: FastHashMap<Box<str>, Box<[i64]>>,
    fingerprint: u64,
}

impl TierThresholds {
    /// Builds the snapshot from `(model key, ascending boundaries)` pairs.
    ///
    /// On key collisions (e.g. `openai/gpt-5.4` and `azure/gpt-5.4`
    /// normalizing to the same name) the lexicographically smallest list wins,
    /// which for a single boundary is the smallest threshold — the
    /// conservative choice, since every level above the first is dearer.
    pub(crate) fn from_entries<'a>(entries: impl Iterator<Item = (&'a str, Vec<i64>)>) -> Self {
        let mut boundaries: FastHashMap<Box<str>, Box<[i64]>> = FastHashMap::default();
        let mut insert_min = |key: String, levels: &[i64]| {
            boundaries
                .entry(key.into_boxed_str())
                .and_modify(|existing| {
                    if levels < &**existing {
                        *existing = levels.into();
                    }
                })
                .or_insert_with(|| levels.into());
        };
        for (key, levels) in entries {
            insert_min(key.to_lowercase(), &levels);
            insert_min(normalize_model_name(key), &levels);
        }

        // Order-independent fingerprint so scan caches can detect a changed
        // snapshot (daily pricing reload) without hashing map iteration order.
        let mut fingerprint = boundaries.len() as u64;
        for entry in &boundaries {
            let mut hasher = DefaultHasher::new();
            entry.hash(&mut hasher);
            fingerprint ^= hasher.finish();
        }

        Self {
            boundaries,
            fingerprint,
        }
    }

    /// Ascending price-level boundaries for `model`, or `None` when the model
    /// has a single price level (or cannot be resolved).
    pub fn boundaries_for(&self, model: &str) -> Option<&[i64]> {
        if self.boundaries.is_empty() {
            return None;
        }
        let lowered = model.to_lowercase();
        if let Some(levels) = self.boundaries.get(lowered.as_str()) {
            return Some(levels);
        }
        self.boundaries
            .get(normalize_model_name(model).as_str())
            .map(|levels| &**levels)
    }

    /// Whether no model carries a second price level (nothing will ever
    /// classify).
    pub fn is_empty(&self) -> bool {
        self.boundaries.is_empty()
    }

    /// Order-independent identity of this snapshot's contents; an empty
    /// snapshot is `0`.
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
    memo: FastHashMap<String, Option<&'a [i64]>>,
}

impl<'a> TierClassifier<'a> {
    pub fn new(thresholds: &'a TierThresholds) -> Self {
        Self {
            thresholds,
            memo: FastHashMap::default(),
        }
    }

    /// Which price level a request for `model` is billed at: `0` for the
    /// model's base level, `n` for the level above its `n`th boundary.
    ///
    /// `request_context` is that one request's own full prompt size, cached
    /// tokens included — never a sum across requests.
    pub fn level(&mut self, model: &str, request_context: i64) -> usize {
        let thresholds: &'a TierThresholds = self.thresholds;
        let boundaries = match self.memo.get(model) {
            Some(boundaries) => *boundaries,
            None => {
                let resolved = thresholds.boundaries_for(model);
                self.memo.insert(model.to_string(), resolved);
                resolved
            }
        };
        // Boundaries ascend, so "the request cleared this one" holds for a
        // prefix and its length is the level.
        boundaries.map_or(0, |boundaries| {
            boundaries.partition_point(|boundary| request_context > *boundary)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> TierThresholds {
        TierThresholds::from_entries(
            [
                ("gpt-5.4", vec![272_000]),
                ("gemini-3.1-pro-preview", vec![200_000]),
            ]
            .into_iter(),
        )
    }

    #[test]
    fn resolves_exact_and_normalized_names() {
        let tiers = snapshot();
        assert_eq!(tiers.boundaries_for("gpt-5.4"), Some(&[272_000][..]));
        assert_eq!(tiers.boundaries_for("GPT-5.4"), Some(&[272_000][..]));
        assert_eq!(tiers.boundaries_for("gpt-4o"), None);
    }

    #[test]
    fn collision_keeps_the_smallest_boundaries() {
        let tiers = TierThresholds::from_entries(
            [
                ("openai/gpt-x", vec![272_000]),
                ("gpt-x", vec![200_000, 400_000]),
            ]
            .into_iter(),
        );
        assert_eq!(tiers.boundaries_for("gpt-x"), Some(&[200_000, 400_000][..]));
    }

    #[test]
    fn classifier_compares_strictly_above() {
        let tiers = snapshot();
        let mut classifier = TierClassifier::new(&tiers);
        assert_eq!(classifier.level("gpt-5.4", 272_000), 0);
        assert_eq!(classifier.level("gpt-5.4", 272_001), 1);
        assert_eq!(classifier.level("no-tier-model", i64::MAX), 0);
        // Memoized second lookup takes the fast path.
        assert_eq!(classifier.level("gpt-5.4", 300_000), 1);
    }

    #[test]
    fn classifier_counts_every_boundary_the_request_cleared() {
        // A four-row range model (dashscope/qwen3-coder-plus): the boundaries
        // are the first three rows' exclusive upper bounds, so each level is
        // the row that prices it.
        let tiers = TierThresholds::from_entries(
            [("qwen3-coder-plus", vec![31_999, 127_999, 255_999])].into_iter(),
        );
        let mut classifier = TierClassifier::new(&tiers);
        assert_eq!(classifier.level("qwen3-coder-plus", 10_000), 0);
        assert_eq!(classifier.level("qwen3-coder-plus", 32_000), 1);
        assert_eq!(classifier.level("qwen3-coder-plus", 128_000), 2);
        assert_eq!(classifier.level("qwen3-coder-plus", 500_000), 3);
        // Past the top row's own bound there is no further level to reach.
        assert_eq!(classifier.level("qwen3-coder-plus", 5_000_000), 3);
    }

    #[test]
    fn fingerprint_is_order_independent_and_content_sensitive() {
        let a = TierThresholds::from_entries([("m1", vec![100]), ("m2", vec![200])].into_iter());
        let b = TierThresholds::from_entries([("m2", vec![200]), ("m1", vec![100])].into_iter());
        let c = TierThresholds::from_entries([("m1", vec![100]), ("m2", vec![300])].into_iter());
        let d =
            TierThresholds::from_entries([("m1", vec![100]), ("m2", vec![200, 900])].into_iter());
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
        assert_ne!(a.fingerprint(), d.fingerprint());
        assert_eq!(TierThresholds::default().fingerprint(), 0);
    }
}
