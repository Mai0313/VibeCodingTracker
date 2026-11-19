//! Per-request context-tier classification against a [`TierThresholds`] snapshot.
//!
//! Context-tier boundaries are evaluated per request rather than on aggregated token totals. [`TierThresholds`] captures each model's ascending token boundaries from a [`ModelPricingMap`](super::ModelPricingMap), and [`TierClassifier`] maps each request's prompt size to a 0-based price level (`0` for the base level, `n` for the level above the `n`th boundary) for [`calculate_cost`](super::calculate_cost).

use crate::constants::FastHashMap;
use crate::pricing::normalize_model_name;
use std::hash::{DefaultHasher, Hash, Hasher};

/// Immutable map from model name to ascending context-size boundaries (in tokens).
///
/// Entries are indexed by both the lowercased LiteLLM key and its [`normalize_model_name`] form. A model with no entry has a single price level.
///
/// Range-based and threshold-based models share this normalized namespace; a normalized-name collision between the two strategies would misalign the resulting level index.
#[derive(Debug, Default)]
pub struct TierThresholds {
    boundaries: FastHashMap<Box<str>, Box<[i64]>>,
    fingerprint: u64,
}

impl TierThresholds {
    /// Builds a snapshot from `(model_key, ascending_boundaries)` pairs, keeping the lexicographically smallest boundary list on key collision.
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

        // XOR per-entry hashes for an order-independent fingerprint across map iteration orders.
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

    /// Returns the ascending price-level boundaries for `model`, or `None` if it has a single price level or is absent.
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

    /// Returns whether no model in the snapshot has multiple price levels.
    pub fn is_empty(&self) -> bool {
        self.boundaries.is_empty()
    }

    /// Returns an order-independent fingerprint of the snapshot's entries (`0` when empty).
    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }
}

/// Memoized per-request price-level classifier over a [`TierThresholds`] snapshot.
#[derive(Debug)]
pub struct TierClassifier<'a> {
    thresholds: &'a TierThresholds,
    memo: FastHashMap<String, Option<&'a [i64]>>,
}

impl<'a> TierClassifier<'a> {
    /// Creates a classifier backed by `thresholds`.
    pub fn new(thresholds: &'a TierThresholds) -> Self {
        Self {
            thresholds,
            memo: FastHashMap::default(),
        }
    }

    /// Returns the 0-based price level for one `model` request (`0` for the base level, `n` above the `n`th boundary).
    ///
    /// `request_context` must be a single request's total prompt tokens (including cached tokens), not an aggregate across requests.
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
        // Boundaries are strictly ascending, so `partition_point` counts how many boundaries `request_context` exceeds.
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
        // Boundaries are each higher row's `min_tokens - 1`, so each returned level indexes the pricing row.
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
