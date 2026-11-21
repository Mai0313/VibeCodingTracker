//! On-disk shape of a provider's ledger file and the fold helpers that build per-day entries from parsed sessions.
//!
//! Every value is either normalized provider token counters, a stored cost, or a plain count, so any time-range view reduces to filtering by day key and summing. Priced costs are never written; pricing is computed at display time from the current pricing map.

use super::tiny_map::TinyMap;
use crate::constants::FastHashMap;
use crate::models::{AggregatedAnalysisRow, CodeAnalysis};
use crate::session::diagnostics::UsageTokenContribution;
use crate::utils::token_merge::token_counts_to_flat_value;
use crate::utils::{MAX_TIER_LEVELS, TierSlice, TokenCounts, extract_token_counts};
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt;

/// Schema version of the on-disk ledger format; files with a newer version are left untouched.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// One provider's ledger file.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct LedgerFile {
    pub(crate) schema_version: u32,
    pub(crate) provider: String,
    /// Whole-database read stamps for single-SQLite providers (OpenCode, Hermes); absent for other providers.
    #[serde(default, skip_serializing_if = "HalfStamps::is_empty")]
    pub(crate) source: HalfStamps,
    #[serde(default)]
    pub(crate) sessions: BTreeMap<String, SessionEntry>,
}

/// Minimal header decoded to inspect `schema_version` when full deserialization fails.
#[derive(Deserialize)]
pub(crate) struct LedgerHeader {
    pub(crate) schema_version: u32,
}

/// Independent `usage` and `analysis` read stamps for one source, allowing either half to be current while the other is stale.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HalfStamps {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) usage: Option<ScanStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) analysis: Option<ScanStamp>,
}

impl HalfStamps {
    pub(crate) fn is_empty(&self) -> bool {
        self.usage.is_none() && self.analysis.is_none()
    }

    pub(crate) fn half(&self, feature: ScanFeature) -> Option<&ScanStamp> {
        match feature {
            ScanFeature::Usage => self.usage.as_ref(),
            ScanFeature::Analysis => self.analysis.as_ref(),
        }
    }

    pub(crate) fn set_half(&mut self, feature: ScanFeature, stamp: ScanStamp) {
        match feature {
            ScanFeature::Usage => self.usage = Some(stamp),
            ScanFeature::Analysis => self.analysis = Some(stamp),
        }
    }

    /// Returns whether `feature`'s half matches `current`.
    pub(crate) fn is_current(&self, feature: ScanFeature, current: &ScanStamp) -> bool {
        self.half(feature)
            .is_some_and(|stamp| stamp.is_current(feature, current))
    }

    /// Returns whether `feature`'s half matches `file` and `tiers` without allocating (fast path of [`Self::is_current`] for single-file providers).
    pub(crate) fn is_current_single_file(
        &self,
        feature: ScanFeature,
        file: &crate::utils::directory::DiscoveredFile,
        tiers: Option<&str>,
    ) -> bool {
        self.half(feature)
            .is_some_and(|stamp| stamp.matches_single_file(feature, file, tiers))
    }

    /// Clears retained failure diagnostics on both halves.
    pub(crate) fn clear_failures(&mut self) {
        for stamp in [self.usage.as_mut(), self.analysis.as_mut()]
            .into_iter()
            .flatten()
        {
            stamp.failure = None;
        }
    }
}

/// Feature performing a scan, selecting which half of an entry is read and written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanFeature {
    Usage,
    Analysis,
}

/// Provenance and outcome of one source read: parser version, tier snapshot, dependency file stamps, and read verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ScanStamp {
    pub(crate) parser: String,
    /// Context-tier snapshot fingerprint used by the usage half; always `None` for analysis or unclassified scans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tiers: Option<String>,
    /// Dependency files keyed by name; `None` records an absent optional sidecar.
    pub(crate) files: TinyMap<String, Option<FileStamp>>,
    /// Whether the read produced usable data (a valid blank source included).
    #[serde(default = "default_true", skip_serializing_if = "Clone::clone")]
    pub(crate) parsed: bool,
    /// Retained parse diagnostic for a present source (either a partial-parse note or the reason no data was produced).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) failure: Option<String>,
}

fn default_true() -> bool {
    true
}

impl ScanStamp {
    /// Creates a stamp for a pending read by the current build.
    pub(crate) fn new(tiers: Option<String>, files: TinyMap<String, Option<FileStamp>>) -> Self {
        Self {
            parser: crate::VERSION.to_string(),
            tiers,
            files,
            parsed: true,
            failure: None,
        }
    }

    /// Returns this stamp with `parsed` and `failure` populated.
    pub(crate) fn with_verdict(mut self, parsed: bool, failure: Option<String>) -> Self {
        self.parsed = parsed;
        self.failure = failure;
        self
    }

    /// Returns whether `current` matches this stamp for `feature`; [`ScanFeature::Analysis`] ignores `tiers`.
    pub(crate) fn is_current(&self, feature: ScanFeature, current: &ScanStamp) -> bool {
        self.files == current.files
            && self.parser == current.parser
            && (feature == ScanFeature::Analysis || self.tiers == current.tiers)
    }

    pub(crate) fn matches_single_file(
        &self,
        feature: ScanFeature,
        file: &crate::utils::directory::DiscoveredFile,
        tiers: Option<&str>,
    ) -> bool {
        if self.parser != crate::VERSION
            || (feature == ScanFeature::Usage && self.tiers.as_deref() != tiers)
        {
            return false;
        }
        let [(stored_name, Some(stored_stamp))] = self.files.as_slice() else {
            return false;
        };
        if stored_stamp.len != file.len {
            return false;
        }
        let name_matches = match file.path.file_name() {
            Some(name) => match name.to_str() {
                Some(text) => text == stored_name,
                None => name.to_string_lossy() == stored_name.as_str(),
            },
            None => stored_name.is_empty(),
        };
        name_matches && super::stamp::modified_matches(file.modified, &stored_stamp.modified)
    }
}

/// The modification time and length of one file the read depended on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileStamp {
    /// RFC 3339 UTC with nanoseconds, the stamp format vct writes everywhere.
    pub(crate) modified: String,
    pub(crate) len: u64,
}

/// One session: its read stamps, where it ran, and what it did per day.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct SessionEntry {
    /// Per-session read stamps for standalone sources (files, Cursor stores); empty for shared-database sessions covered by [`LedgerFile::source`].
    #[serde(default, skip_serializing_if = "HalfStamps::is_empty")]
    pub(crate) scanned: HalfStamps,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) git_remote: Option<String>,
    /// Per-day entries keyed by local `YYYY-MM-DD` (one day for file sessions, one per active day for database sessions).
    #[serde(default)]
    pub(crate) days: TinyMap<String, DayEntry>,
    /// Local date (`YYYY-MM-DD`) when the source was first observed missing; cleared if it reappears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) missing_since: Option<String>,
}

impl SessionEntry {
    /// Builds a file session's entry from one parse, stamping both halves (with `tiers` kept only on the usage stamp).
    pub(crate) fn from_file_parse(
        analysis: Option<CodeAnalysis>,
        emit: bool,
        date: String,
        stamp: ScanStamp,
    ) -> Self {
        let mut entry = Self::default();
        if let Some(analysis) = analysis {
            if let Some(record) = analysis.records.first() {
                entry.cwd = non_empty(&record.folder_path);
                entry.git_remote = non_empty(&record.git_remote_url);
            }
            let mut day = DayEntry::default();
            day.add_file_records(analysis, emit);
            if !day.is_empty() {
                entry.days.insert(date, day);
            }
        }
        entry.scanned.analysis = Some(ScanStamp {
            tiers: None,
            ..stamp.clone()
        });
        entry.scanned.usage = Some(stamp);
        entry
    }

    /// Replaces `feature`'s half across all days with `days`, leaving the other half untouched.
    ///
    /// Clears prior days for `feature` rather than merging day by day: cumulative database rows (such as Hermes per-model totals) can move to a later date.
    pub(crate) fn replace_half(&mut self, feature: ScanFeature, days: TinyMap<String, DayEntry>) {
        for day in self.days.values_mut() {
            day.clear_half(feature);
        }
        for (date, day) in days {
            self.days
                .entry(date)
                .or_default()
                .replace_half(feature, day);
        }
        self.days.retain(|_, day| !day.is_empty());
    }
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// Compact normalized per-model token counters stored inline in [`DayEntry`].
///
/// Holds the nine independent billing counters inline plus optional per-level context-tier slices (no heap allocation when empty). Stream-deserializes both legacy v1 provider JSON shapes and compact flat JSON, and serializes compact flat JSON readable by v1 builds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct CompactUsage {
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) reasoning_tokens: i64,
    pub(crate) cache_read: i64,
    pub(crate) cache_creation_5m: i64,
    pub(crate) cache_creation_1h: i64,
    pub(crate) web_search_requests: i64,
    pub(crate) tool_tokens: i64,
    pub(crate) total: i64,
    pub(crate) above_tiers: Option<Box<[TierSlice]>>,
}

impl CompactUsage {
    /// Extracts normalized counters from an in-memory provider usage [`Value`].
    pub(crate) fn from_value(usage: &Value) -> Self {
        Self::from_token_counts(extract_token_counts(usage))
    }

    /// Packs normalized [`TokenCounts`] into the compact inline representation.
    pub(crate) fn from_token_counts(counts: TokenCounts) -> Self {
        Self {
            input_tokens: counts.input_tokens,
            output_tokens: counts.output_tokens,
            reasoning_tokens: counts.reasoning_tokens,
            cache_read: counts.cache_read,
            cache_creation_5m: counts.cache_creation_5m,
            cache_creation_1h: counts.cache_creation_1h,
            web_search_requests: counts.web_search_requests,
            tool_tokens: counts.tool_tokens,
            total: counts.total,
            above_tiers: compact_tier_slices(counts.above_tiers),
        }
    }

    /// Returns total cache-creation tokens (`5m + 1h`).
    #[inline]
    pub(crate) fn cache_creation(&self) -> i64 {
        self.cache_creation_5m + self.cache_creation_1h
    }

    /// Converts these counters into [`TokenCounts`] for pricing or flat JSON emission.
    pub(crate) fn to_token_counts(&self) -> TokenCounts {
        TokenCounts {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cache_read: self.cache_read,
            cache_creation: self.cache_creation(),
            cache_creation_5m: self.cache_creation_5m,
            cache_creation_1h: self.cache_creation_1h,
            web_search_requests: self.web_search_requests,
            tool_tokens: self.tool_tokens,
            total: self.total,
            above_tiers: self
                .above_tiers
                .as_deref()
                .map_or_else(Vec::new, |slices| slices.to_vec()),
        }
    }

    /// Converts these counters into the canonical flat JSON [`Value`] schema.
    pub(crate) fn to_flat_value(&self) -> Value {
        token_counts_to_flat_value(&self.to_token_counts())
    }

    /// Returns whether any token bucket or `web_search_requests` is non-zero.
    pub(crate) fn has_activity(&self) -> bool {
        self.total != 0
            || self.input_tokens != 0
            || self.output_tokens != 0
            || self.reasoning_tokens != 0
            || self.cache_read != 0
            || self.cache_creation_5m != 0
            || self.cache_creation_1h != 0
            || self.web_search_requests != 0
    }

    /// Adds all counters and tier slices from `other` into `self`.
    pub(crate) fn add_assign(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
        self.cache_read += other.cache_read;
        self.cache_creation_5m += other.cache_creation_5m;
        self.cache_creation_1h += other.cache_creation_1h;
        self.web_search_requests += other.web_search_requests;
        self.tool_tokens += other.tool_tokens;
        self.total += other.total;
        if let Some(other_tiers) = other.above_tiers.as_deref() {
            self.merge_tier_slices(other_tiers);
        }
    }

    /// Adds one database-reader token contribution (plus its context tier slice when `tier_level > 0`) directly in integer space.
    pub(crate) fn add_contribution(&mut self, tokens: UsageTokenContribution, tier_level: usize) {
        self.input_tokens += tokens.input_tokens;
        self.output_tokens += tokens.output_tokens;
        self.reasoning_tokens += tokens.reasoning_tokens;
        self.cache_read += tokens.cache_read_tokens;
        self.cache_creation_5m += tokens.cache_creation_tokens;
        self.total += tokens.input_tokens
            + tokens.output_tokens
            + tokens.reasoning_tokens
            + tokens.cache_read_tokens
            + tokens.cache_creation_tokens;
        if tier_level > 0 && tier_level <= MAX_TIER_LEVELS {
            let slice = TierSlice {
                input_tokens: tokens.input_tokens,
                output_tokens: tokens.output_tokens,
                reasoning_tokens: tokens.reasoning_tokens,
                cache_read: tokens.cache_read_tokens,
                cache_creation_5m: tokens.cache_creation_tokens,
                cache_creation_1h: 0,
            };
            if !slice.is_empty() {
                self.add_tier_slice(tier_level, &slice);
            }
        }
    }

    fn merge_tier_slices(&mut self, other: &[TierSlice]) {
        if other.is_empty() {
            return;
        }
        match self.above_tiers.as_deref_mut() {
            Some(existing) if existing.len() >= other.len() => {
                for (dst, src) in existing.iter_mut().zip(other) {
                    dst.merge(src);
                }
            }
            Some(existing) => {
                let mut merged = Vec::with_capacity(other.len());
                merged.extend_from_slice(existing);
                merged.resize(other.len(), TierSlice::default());
                for (dst, src) in merged.iter_mut().zip(other) {
                    dst.merge(src);
                }
                self.above_tiers = compact_tier_slices(merged);
            }
            None => {
                self.above_tiers = Some(other.to_vec().into_boxed_slice());
            }
        }
    }

    fn add_tier_slice(&mut self, level: usize, slice: &TierSlice) {
        let index = level - 1;
        match self.above_tiers.as_deref_mut() {
            Some(existing) if index < existing.len() => {
                existing[index].merge(slice);
            }
            Some(existing) => {
                let mut merged = Vec::with_capacity(level);
                merged.extend_from_slice(existing);
                merged.resize(level, TierSlice::default());
                merged[index].merge(slice);
                self.above_tiers = Some(merged.into_boxed_slice());
            }
            None => {
                let mut slices = vec![TierSlice::default(); level];
                slices[index] = *slice;
                self.above_tiers = Some(slices.into_boxed_slice());
            }
        }
    }
}

fn compact_tier_slices(mut slices: Vec<TierSlice>) -> Option<Box<[TierSlice]>> {
    while slices.last().is_some_and(TierSlice::is_empty) {
        slices.pop();
    }
    if slices.is_empty() {
        None
    } else {
        Some(slices.into_boxed_slice())
    }
}

struct CacheCreationSplit {
    ephemeral_5m: i64,
    ephemeral_1h: i64,
}

impl Serialize for CacheCreationSplit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("ephemeral_5m_input_tokens", &self.ephemeral_5m)?;
        map.serialize_entry("ephemeral_1h_input_tokens", &self.ephemeral_1h)?;
        map.end()
    }
}

struct ServerToolUse {
    web_search_requests: i64,
}

impl Serialize for ServerToolUse {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        map.serialize_entry("web_search_requests", &self.web_search_requests)?;
        map.end()
    }
}

struct SerializeAboveTiers<'a>(&'a [TierSlice]);

impl Serialize for SerializeAboveTiers<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (index, slice) in self.0.iter().enumerate() {
            let level = index + 1;
            for (bucket, value) in [
                ("input_tokens", slice.input_tokens),
                ("output_tokens", slice.output_tokens),
                ("reasoning_tokens", slice.reasoning_tokens),
                ("cache_read_tokens", slice.cache_read),
                ("cache_creation_5m_tokens", slice.cache_creation_5m),
                ("cache_creation_1h_tokens", slice.cache_creation_1h),
            ] {
                if value != 0 {
                    map.serialize_entry(&TierKey(level, bucket), &value)?;
                }
            }
        }
        map.end()
    }
}

/// Zero-allocation serializer for `level_<n>_<bucket>` map keys.
struct TierKey(usize, &'static str);

impl Serialize for TierKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("level_{}_{}", self.0, self.1))
    }
}

impl Serialize for CompactUsage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let cache_creation = self.cache_creation();
        let has_above_tiers = self
            .above_tiers
            .as_deref()
            .is_some_and(|slices| slices.iter().any(|slice| !slice.is_empty()));
        let mut map = serializer.serialize_map(None)?;
        // Always emit `input_tokens` so older v1 builds recognize this object as the flat usage shape in `merge_usage_values`.
        map.serialize_entry("input_tokens", &self.input_tokens)?;
        if self.output_tokens != 0 {
            map.serialize_entry("output_tokens", &self.output_tokens)?;
        }
        if self.reasoning_tokens != 0 {
            map.serialize_entry("reasoning_output_tokens", &self.reasoning_tokens)?;
        }
        if self.cache_read != 0 {
            map.serialize_entry("cache_read_input_tokens", &self.cache_read)?;
        }
        if cache_creation != 0 {
            map.serialize_entry("cache_creation_input_tokens", &cache_creation)?;
        }
        if self.cache_creation_1h != 0 {
            map.serialize_entry(
                "cache_creation",
                &CacheCreationSplit {
                    ephemeral_5m: self.cache_creation_5m,
                    ephemeral_1h: self.cache_creation_1h,
                },
            )?;
        }
        if self.web_search_requests != 0 {
            map.serialize_entry(
                "server_tool_use",
                &ServerToolUse {
                    web_search_requests: self.web_search_requests,
                },
            )?;
        }
        if self.tool_tokens != 0 {
            map.serialize_entry("tool_tokens", &self.tool_tokens)?;
        }
        if self.total != 0 {
            map.serialize_entry("total_tokens", &self.total)?;
        }
        if has_above_tiers && let Some(slices) = self.above_tiers.as_deref() {
            map.serialize_entry("above_tier", &SerializeAboveTiers(slices))?;
        }
        map.end()
    }
}

/// Borrowed-when-possible JSON object key avoiding heap allocation on `from_slice`.
struct StrKey<'de>(Cow<'de, str>);

impl<'de> Deserialize<'de> for StrKey<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct KeyVisitor;

        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = StrKey<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object key")
            }

            fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(StrKey(Cow::Borrowed(value)))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(StrKey(Cow::Owned(value.to_owned())))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(StrKey(Cow::Owned(value)))
            }
        }

        deserializer.deserialize_str(KeyVisitor)
    }
}

/// Tolerant integer decoder matching `Value::as_i64` (non-integer JSON values decode as `None`).
struct LenientI64(Option<i64>);

impl<'de> Deserialize<'de> for LenientI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct LenientI64Visitor;

        impl<'de> Visitor<'de> for LenientI64Visitor {
            type Value = LenientI64;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any JSON value")
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(LenientI64(Some(value)))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(LenientI64(i64::try_from(value).ok()))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }

            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }

            fn visit_bytes<E: de::Error>(self, _: &[u8]) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }

            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
                LenientI64::deserialize(d)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientI64(None))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientI64(None))
            }
        }

        deserializer.deserialize_any(LenientI64Visitor)
    }
}

struct LenientCacheCreation(Option<(i64, i64)>);

impl<'de> Deserialize<'de> for LenientCacheCreation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SplitVisitor;

        impl<'de> Visitor<'de> for SplitVisitor {
            type Value = LenientCacheCreation;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a cache_creation object or non-object value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut ephemeral_5m = 0i64;
                let mut ephemeral_1h = 0i64;
                while let Some(StrKey(key)) = map.next_key()? {
                    match key.as_ref() {
                        "ephemeral_5m_input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                ephemeral_5m = v;
                            }
                        }
                        "ephemeral_1h_input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                ephemeral_1h = v;
                            }
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(LenientCacheCreation(Some((ephemeral_5m, ephemeral_1h))))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientCacheCreation(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientCacheCreation(None))
            }
        }

        deserializer.deserialize_any(SplitVisitor)
    }
}

struct LenientServerToolUse(i64);

impl<'de> Deserialize<'de> for LenientServerToolUse {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StuVisitor;

        impl<'de> Visitor<'de> for StuVisitor {
            type Value = LenientServerToolUse;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a server_tool_use object or non-object value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut web_search_requests = 0i64;
                while let Some(StrKey(key)) = map.next_key()? {
                    if key.as_ref() == "web_search_requests" {
                        if let Some(v) = map.next_value::<LenientI64>()?.0 {
                            web_search_requests = v;
                        }
                    } else {
                        let _: IgnoredAny = map.next_value()?;
                    }
                }
                Ok(LenientServerToolUse(web_search_requests))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientServerToolUse(0))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientServerToolUse(0))
            }
        }

        deserializer.deserialize_any(StuVisitor)
    }
}

struct LenientAboveTier(Vec<TierSlice>);

impl<'de> Deserialize<'de> for LenientAboveTier {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AboveVisitor;

        impl<'de> Visitor<'de> for AboveVisitor {
            type Value = LenientAboveTier;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an above_tier object or non-object value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut slices = Vec::new();
                while let Some(StrKey(key)) = map.next_key()? {
                    let Some(tokens) = map.next_value::<LenientI64>()?.0 else {
                        continue;
                    };
                    let Some(rest) = key.strip_prefix("level_") else {
                        continue;
                    };
                    let Some(sep) = rest.find('_') else {
                        continue;
                    };
                    let Ok(level) = rest[..sep].parse::<usize>() else {
                        continue;
                    };
                    if level == 0 || level > MAX_TIER_LEVELS {
                        continue;
                    }
                    let bucket: fn(&mut TierSlice) -> &mut i64 = match &rest[sep + 1..] {
                        "input_tokens" => |s| &mut s.input_tokens,
                        "output_tokens" => |s| &mut s.output_tokens,
                        "reasoning_tokens" => |s| &mut s.reasoning_tokens,
                        "cache_read_tokens" => |s| &mut s.cache_read,
                        "cache_creation_5m_tokens" => |s| &mut s.cache_creation_5m,
                        "cache_creation_1h_tokens" => |s| &mut s.cache_creation_1h,
                        _ => continue,
                    };
                    if slices.len() < level {
                        slices.resize(level, TierSlice::default());
                    }
                    *bucket(&mut slices[level - 1]) = tokens;
                }
                Ok(LenientAboveTier(slices))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientAboveTier(Vec::new()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientAboveTier(Vec::new()))
            }
        }

        deserializer.deserialize_any(AboveVisitor)
    }
}

#[derive(Default)]
struct CodexRawTotal {
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

struct LenientCodexTotal(Option<CodexRawTotal>);

impl<'de> Deserialize<'de> for LenientCodexTotal {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct CodexVisitor;

        impl<'de> Visitor<'de> for CodexVisitor {
            type Value = LenientCodexTotal;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a total_token_usage object or non-object value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut raw = CodexRawTotal::default();
                while let Some(StrKey(key)) = map.next_key()? {
                    match key.as_ref() {
                        "input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                raw.input_tokens = v;
                            }
                        }
                        "cached_input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                raw.cached_input_tokens = v;
                            }
                        }
                        "cache_write_input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                raw.cache_write_input_tokens = v;
                            }
                        }
                        "output_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                raw.output_tokens = Some(v);
                            }
                        }
                        "reasoning_output_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                raw.reasoning_output_tokens = Some(v);
                            }
                        }
                        "total_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                raw.total_tokens = Some(v);
                            }
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(LenientCodexTotal(Some(raw)))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientCodexTotal(None))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientCodexTotal(None))
            }
        }

        deserializer.deserialize_any(CodexVisitor)
    }
}

impl<'de> Deserialize<'de> for CompactUsage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UsageVisitor;

        impl<'de> Visitor<'de> for UsageVisitor {
            type Value = CompactUsage;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a usage JSON object")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(CompactUsage::default())
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(CompactUsage::default())
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut flat_input = 0i64;
                let mut flat_output = 0i64;
                let mut flat_cache_read = 0i64;
                let mut flat_cache_creation = 0i64;
                let mut thoughts = 0i64;
                let mut reasoning_output = 0i64;
                let mut tool_tokens = 0i64;
                let mut published_total: Option<i64> = None;
                let mut web_search_requests = 0i64;
                let mut cc_split: Option<(i64, i64)> = None;
                let mut above_tiers: Vec<TierSlice> = Vec::new();
                let mut codex: Option<CodexRawTotal> = None;

                while let Some(StrKey(key)) = map.next_key()? {
                    match key.as_ref() {
                        "input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                flat_input = v;
                            }
                        }
                        "output_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                flat_output = v;
                            }
                        }
                        "cache_read_input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                flat_cache_read = v;
                            }
                        }
                        "cache_creation_input_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                flat_cache_creation = v;
                            }
                        }
                        "thoughts_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                thoughts = v;
                            }
                        }
                        "reasoning_output_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                reasoning_output = v;
                            }
                        }
                        "tool_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                tool_tokens = v;
                            }
                        }
                        "total_tokens" => {
                            if let Some(v) = map.next_value::<LenientI64>()?.0 {
                                published_total = Some(v);
                            }
                        }
                        "cache_creation" => {
                            cc_split = map.next_value::<LenientCacheCreation>()?.0;
                        }
                        "server_tool_use" => {
                            web_search_requests = map.next_value::<LenientServerToolUse>()?.0;
                        }
                        "above_tier" => {
                            above_tiers = map.next_value::<LenientAboveTier>()?.0;
                        }
                        "total_token_usage" => {
                            codex = map.next_value::<LenientCodexTotal>()?.0;
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }

                // Match `extract_token_counts` field normalization exactly across flat and Codex shapes.
                let mut input_tokens = flat_input;
                let mut output_tokens = flat_output;
                let mut reasoning_tokens = thoughts + reasoning_output;
                let mut cache_read = flat_cache_read;
                let (mut cache_creation_5m, mut cache_creation_1h) = match cc_split {
                    Some((ephemeral_5m, cc_1h)) => {
                        let total_cc = flat_cache_creation.max(ephemeral_5m + cc_1h);
                        (total_cc - cc_1h, cc_1h)
                    }
                    None => (flat_cache_creation, 0),
                };

                if let Some(c) = codex {
                    input_tokens =
                        (c.input_tokens - c.cached_input_tokens - c.cache_write_input_tokens)
                            .max(0);
                    cache_read = c.cached_input_tokens;
                    cache_creation_5m = c.cache_write_input_tokens;
                    cache_creation_1h = 0;
                    if let Some(out) = c.output_tokens {
                        output_tokens = out;
                    }
                    if let Some(reas) = c.reasoning_output_tokens {
                        reasoning_tokens = reas;
                    }
                    output_tokens = (output_tokens - reasoning_tokens).max(0);
                    if let Some(codex_total) = c.total_tokens {
                        return Ok(CompactUsage {
                            input_tokens,
                            output_tokens,
                            reasoning_tokens,
                            cache_read,
                            cache_creation_5m,
                            cache_creation_1h,
                            web_search_requests,
                            tool_tokens: 0,
                            total: codex_total,
                            above_tiers: compact_tier_slices(above_tiers),
                        });
                    }
                }

                let derived_total = input_tokens
                    + output_tokens
                    + reasoning_tokens
                    + cache_read
                    + cache_creation_5m
                    + cache_creation_1h
                    + tool_tokens;
                let total = published_total.map_or(derived_total, |p| p.max(derived_total));

                Ok(CompactUsage {
                    input_tokens,
                    output_tokens,
                    reasoning_tokens,
                    cache_read,
                    cache_creation_5m,
                    cache_creation_1h,
                    web_search_requests,
                    tool_tokens,
                    total,
                    above_tiers: compact_tier_slices(above_tiers),
                })
            }
        }

        deserializer.deserialize_any(UsageVisitor)
    }
}

/// What one session did on one local day.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct DayEntry {
    /// Per-model normalized token counters stored inline to avoid per-entry heap allocations and fold-time JSON parsing.
    #[serde(default, skip_serializing_if = "TinyMap::is_empty")]
    pub(crate) usage: TinyMap<String, CompactUsage>,
    /// Per-model file-operation and tool-call counters.
    #[serde(default, skip_serializing_if = "TinyMap::is_empty")]
    pub(crate) analysis: TinyMap<String, AggregatedAnalysisRow>,
    /// The cost the provider itself recorded, per model (OpenCode, Hermes).
    #[serde(default, skip_serializing_if = "TinyMap::is_empty")]
    pub(crate) stored_cost: TinyMap<String, f64>,
    #[serde(default)]
    pub(crate) active: ActiveFlags,
}

/// Per-feature active-day flags; `usage` requires non-zero tokens or stored cost, while `analysis` is true for any emitted session even with zero tool calls.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ActiveFlags {
    #[serde(default)]
    pub(crate) usage: bool,
    #[serde(default)]
    pub(crate) analysis: bool,
}

impl DayEntry {
    /// Folds a parsed file session into both halves; every non-`<synthetic>` model in `conversation_usage` receives the record's tool counters, while `advisor_usage` contributes tokens only.
    pub(crate) fn add_file_records(&mut self, analysis: CodeAnalysis, emit: bool) {
        for record in analysis.records {
            for (model, usage) in record.conversation_usage {
                let compact = CompactUsage::from_value(&usage);
                if compact.has_activity() {
                    self.active.usage = true;
                }
                if emit && !model.contains("<synthetic>") {
                    let row = self
                        .analysis
                        .entry(model.clone())
                        .or_insert_with(|| empty_row(&model));
                    row.edit_lines += record.total_edit_lines;
                    row.read_lines += record.total_read_lines;
                    row.write_lines += record.total_write_lines;
                    row.bash_count += record.tool_call_counts.bash;
                    row.edit_count += record.tool_call_counts.edit;
                    row.read_count += record.tool_call_counts.read;
                    row.todo_write_count += record.tool_call_counts.todo_write;
                    row.write_count += record.tool_call_counts.write;
                }
                merge_model_usage(&mut self.usage, model, compact);
            }
            for (model, usage) in record.advisor_usage {
                let compact = CompactUsage::from_value(&usage);
                if compact.has_activity() {
                    self.active.usage = true;
                }
                merge_model_usage(&mut self.usage, model, compact);
            }
        }
        if emit {
            self.active.analysis = true;
        }
    }

    /// Folds one database usage row into the usage half; `Some(0.0)` for `stored_cost` is recorded, while `None` omits the entry for unpriced providers.
    pub(crate) fn add_usage_row(
        &mut self,
        model: String,
        tokens: UsageTokenContribution,
        tier_level: usize,
        stored_cost: Option<f64>,
    ) {
        if stored_cost.is_some_and(|cost| cost != 0.0) || tokens.has_activity() {
            self.active.usage = true;
        }
        if let Some(cost) = stored_cost {
            if let Some(existing) = self.stored_cost.get_mut(&model) {
                *existing += cost;
            } else {
                self.stored_cost.insert(model.clone(), cost);
            }
        }
        if let Some(existing) = self.usage.get_mut(&model) {
            existing.add_contribution(tokens, tier_level);
        } else {
            let mut usage = CompactUsage::default();
            usage.add_contribution(tokens, tier_level);
            self.usage.insert(model, usage);
        }
    }

    /// Folds one database analysis row into the analysis half.
    pub(crate) fn add_analysis_records(&mut self, analysis: &CodeAnalysis) {
        for record in &analysis.records {
            for model in record.conversation_usage.keys() {
                if model.contains("<synthetic>") {
                    continue;
                }
                let row = if let Some(existing) = self.analysis.get_mut(model) {
                    existing
                } else {
                    self.analysis
                        .entry(model.clone())
                        .or_insert_with(|| empty_row(model))
                };
                row.edit_lines += record.total_edit_lines;
                row.read_lines += record.total_read_lines;
                row.write_lines += record.total_write_lines;
                row.bash_count += record.tool_call_counts.bash;
                row.edit_count += record.tool_call_counts.edit;
                row.read_count += record.tool_call_counts.read;
                row.todo_write_count += record.tool_call_counts.todo_write;
                row.write_count += record.tool_call_counts.write;
            }
        }
        self.active.analysis = true;
    }

    /// Takes `feature`'s half from `other`, keeping the other half as it is.
    pub(crate) fn replace_half(&mut self, feature: ScanFeature, other: DayEntry) {
        match feature {
            ScanFeature::Usage => {
                self.usage = other.usage;
                self.stored_cost = other.stored_cost;
                self.active.usage = other.active.usage;
            }
            ScanFeature::Analysis => {
                self.analysis = other.analysis;
                self.active.analysis = other.active.analysis;
            }
        }
    }

    /// Empties `feature`'s half, keeping the other half as it is.
    pub(crate) fn clear_half(&mut self, feature: ScanFeature) {
        self.replace_half(feature, DayEntry::default());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.usage.is_empty()
            && self.analysis.is_empty()
            && self.stored_cost.is_empty()
            && !self.active.usage
            && !self.active.analysis
    }
}

fn empty_row(model: &str) -> AggregatedAnalysisRow {
    AggregatedAnalysisRow {
        model: model.to_string(),
        edit_lines: 0,
        read_lines: 0,
        write_lines: 0,
        bash_count: 0,
        edit_count: 0,
        read_count: 0,
        todo_write_count: 0,
        write_count: 0,
    }
}

fn merge_model_usage(
    target: &mut TinyMap<String, CompactUsage>,
    model: String,
    usage: CompactUsage,
) {
    if let Some(existing) = target.get_mut(&model) {
        existing.add_assign(&usage);
    } else {
        target.insert(model, usage);
    }
}

/// Merges per-model counters into a model-keyed accumulator.
pub(crate) fn merge_analysis_rows(
    target: &mut FastHashMap<String, AggregatedAnalysisRow>,
    source: &TinyMap<String, AggregatedAnalysisRow>,
) {
    for (model, row) in source {
        let entry = match target.get_mut(model) {
            Some(existing) => existing,
            None => target
                .entry(model.clone())
                .or_insert_with(|| empty_row(model)),
        };
        entry.edit_lines += row.edit_lines;
        entry.read_lines += row.read_lines;
        entry.write_lines += row.write_lines;
        entry.bash_count += row.bash_count;
        entry.edit_count += row.edit_count;
        entry.read_count += row.read_count;
        entry.todo_write_count += row.todo_write_count;
        entry.write_count += row.write_count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{CodeAnalysisRecord, CodeAnalysisToolCalls};
    use crate::session::diagnostics::UsageTokenContribution;
    use serde_json::json;

    fn analysis_with_usage_value(value: Value) -> CodeAnalysis {
        let mut usage = FastHashMap::default();
        usage.insert("model".to_string(), value);
        CodeAnalysis {
            user: String::new(),
            extension_name: "Codex".to_string(),
            insights_version: String::new(),
            machine_id: String::new(),
            records: vec![CodeAnalysisRecord {
                total_unique_files: 0,
                total_write_lines: 0,
                total_read_lines: 0,
                total_edit_lines: 0,
                total_write_characters: 0,
                total_read_characters: 0,
                total_edit_characters: 0,
                write_file_details: Vec::new(),
                read_file_details: Vec::new(),
                edit_file_details: Vec::new(),
                run_command_details: Vec::new(),
                tool_call_counts: CodeAnalysisToolCalls::default(),
                conversation_usage: usage,
                advisor_usage: FastHashMap::default(),
                task_id: String::new(),
                timestamp: 0,
                folder_path: String::new(),
                git_remote_url: String::new(),
            }],
        }
    }

    fn analysis_with_usage(tokens: i64) -> CodeAnalysis {
        analysis_with_usage_value(json!({ "input_tokens": tokens }))
    }

    #[test]
    fn zero_usage_does_not_mark_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(analysis_with_usage(0), true);
        assert!(!day.active.usage);
        assert!(day.active.analysis);
    }

    #[test]
    fn nonzero_usage_marks_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(analysis_with_usage(1), true);
        assert!(day.active.usage);
    }

    #[test]
    fn published_total_alone_marks_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(
            analysis_with_usage_value(json!({ "total_token_usage": { "total_tokens": 7 } })),
            false,
        );
        assert!(day.active.usage);
        assert!(!day.active.analysis);
    }

    #[test]
    fn tool_only_usage_marks_an_active_day() {
        let mut day = DayEntry::default();
        day.add_file_records(
            analysis_with_usage_value(json!({
                "input_tokens": 0,
                "output_tokens": 0,
                "tool_tokens": 5,
                "total_tokens": 5
            })),
            false,
        );
        assert!(day.active.usage);
    }

    #[test]
    fn emitted_synthetic_session_keeps_analysis_active_day() {
        let mut analysis = analysis_with_usage(0);
        let usage = analysis.records[0]
            .conversation_usage
            .remove("model")
            .unwrap();
        analysis.records[0]
            .conversation_usage
            .insert("<synthetic>".to_string(), usage);

        let mut day = DayEntry::default();
        day.add_file_records(analysis, true);
        assert!(day.analysis.is_empty());
        assert!(day.active.analysis);
        assert!(!day.is_empty());
    }

    #[test]
    fn a_blank_source_leaves_an_empty_day() {
        let mut analysis = analysis_with_usage(0);
        analysis.records.clear();
        let mut day = DayEntry::default();
        day.add_file_records(analysis, false);
        assert!(day.is_empty());
    }

    #[test]
    fn one_model_at_two_levels_keeps_its_slices_apart() {
        let tokens = |input: i64| UsageTokenContribution {
            input_tokens: input,
            ..Default::default()
        };
        let mut day = DayEntry::default();
        day.add_usage_row("model".to_string(), tokens(300_000), 1, Some(0.0));
        day.add_usage_row("model".to_string(), tokens(400_000), 2, Some(0.0));
        day.add_usage_row("model".to_string(), tokens(1_000), 0, Some(0.0));

        let counts = day.usage["model"].to_token_counts();
        assert_eq!(counts.input_tokens, 701_000);
        assert_eq!(counts.above_tiers[0].input_tokens, 300_000);
        assert_eq!(counts.above_tiers[1].input_tokens, 400_000);
        assert_eq!(day.stored_cost["model"], 0.0);
    }

    #[test]
    fn replacing_one_half_leaves_the_other() {
        let mut day = DayEntry::default();
        day.add_usage_row(
            "model".to_string(),
            UsageTokenContribution {
                input_tokens: 5,
                ..Default::default()
            },
            0,
            Some(0.25),
        );
        let mut fresh = DayEntry::default();
        fresh.add_analysis_records(&analysis_with_usage(0));
        day.replace_half(ScanFeature::Analysis, fresh);

        assert!(day.active.usage);
        assert!(day.active.analysis);
        assert_eq!(day.stored_cost["model"], 0.25);
        assert!(day.analysis.contains_key("model"));
    }

    #[test]
    fn replacing_a_session_half_drops_the_days_the_read_no_longer_produced() {
        let tokens = UsageTokenContribution {
            input_tokens: 1_000,
            ..Default::default()
        };
        let mut first = DayEntry::default();
        first.add_usage_row("model".to_string(), tokens, 0, Some(0.0));
        let mut entry = SessionEntry::default();
        entry.days.insert("2026-09-01".to_string(), first.clone());
        entry
            .days
            .entry("2026-09-01".to_string())
            .or_default()
            .add_analysis_records(&analysis_with_usage(0));

        // The cumulative row moved to a later day carrying the whole total.
        let mut second = DayEntry::default();
        second.add_usage_row(
            "model".to_string(),
            UsageTokenContribution {
                input_tokens: 3_000,
                ..Default::default()
            },
            0,
            Some(0.0),
        );
        entry.replace_half(
            ScanFeature::Usage,
            TinyMap::from([("2026-09-02".to_string(), second)]),
        );

        let old = &entry.days["2026-09-01"];
        assert!(
            old.usage.is_empty(),
            "the moved day must not keep the old total"
        );
        assert!(old.active.analysis, "the other half stays");
        assert_eq!(
            entry.days["2026-09-02"].usage["model"].input_tokens,
            3_000
        );
    }

    #[test]
    fn compact_usage_stays_inline_and_small() {
        assert!(std::mem::size_of::<CompactUsage>() <= 96);
    }

    #[test]
    fn compact_usage_deserializes_legacy_v1_shapes_and_round_trips_for_v1_readers() {
        use crate::utils::merge_usage_values;

        let samples = [
            // Claude with 5m+1h cache split, server_tool_use, service_tier, and above_tier
            json!({
                "input_tokens": 120,
                "output_tokens": 45,
                "cache_read_input_tokens": 10_000,
                "cache_creation_input_tokens": 1_500,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 500,
                    "ephemeral_1h_input_tokens": 1_000
                },
                "server_tool_use": {
                    "web_search_requests": 3,
                    "web_fetch_requests": 2
                },
                "service_tier": "standard",
                "above_tier": {
                    "level_1_input_tokens": 100,
                    "level_1_cache_read_tokens": 9_000,
                    "level_2_cache_creation_1h_tokens": 1_000
                }
            }),
            // Codex nested total_token_usage with cache_write_input_tokens + above_tier
            json!({
                "total_token_usage": {
                    "input_tokens": 100_000,
                    "cached_input_tokens": 60_000,
                    "cache_write_input_tokens": 30_000,
                    "output_tokens": 2_000,
                    "reasoning_output_tokens": 500,
                    "total_tokens": 102_000
                },
                "last_token_usage": { "input_tokens": 5 },
                "model_context_window": null,
                "above_tier": {
                    "level_1_input_tokens": 10_000,
                    "level_1_cache_creation_5m_tokens": 30_000
                }
            }),
            // Gemini thoughts_tokens + tool_tokens + published total_tokens
            json!({
                "input_tokens": 100,
                "output_tokens": 20,
                "cache_read_input_tokens": 50,
                "thoughts_tokens": 30,
                "tool_tokens": 7,
                "total_tokens": 250
            }),
            // Zero-token / null-valued fields
            json!({
                "input_tokens": 0,
                "output_tokens": null,
                "cache_creation": null,
                "above_tier": { "level_0_input_tokens": 9, "level_1_unknown": 5 }
            }),
        ];

        for sample in samples {
            let expected = CompactUsage::from_value(&sample);
            let raw = serde_json::to_vec(&sample).unwrap();
            let decoded: CompactUsage = serde_json::from_slice(&raw).unwrap();
            assert_eq!(decoded, expected, "stream decode mismatch for {sample}");

            let compact_json = serde_json::to_string(&decoded).unwrap();
            let reparsed: CompactUsage = serde_json::from_str(&compact_json).unwrap();
            assert_eq!(reparsed, expected, "compact round-trip mismatch for {sample}");

            // Older v1 builds parse the serialized JSON via `extract_token_counts` and `merge_usage_values`.
            let v1_value: Value = serde_json::from_str(&compact_json).unwrap();
            assert!(
                v1_value.get("input_tokens").is_some(),
                "older v1 merge_usage_values requires input_tokens key"
            );
            assert_eq!(CompactUsage::from_value(&v1_value), expected);

            // Simulate merging two compact entries in an older v1 build.
            let mut merged_v1 = v1_value.clone();
            merge_usage_values(&mut merged_v1, &v1_value);
            let mut doubled = expected.clone();
            doubled.add_assign(&expected);
            assert_eq!(CompactUsage::from_value(&merged_v1), doubled);
        }
    }

    #[test]
    fn analysis_half_ignores_the_tier_snapshot() {
        let files = TinyMap::from([(
            "session.jsonl".to_string(),
            Some(FileStamp {
                modified: "2026-09-08T00:00:00.000000000Z".to_string(),
                len: 1,
            }),
        )]);
        let stored = ScanStamp::new(None, files.clone());
        let current = ScanStamp::new(Some("abc".to_string()), files);
        assert!(stored.is_current(ScanFeature::Analysis, &current));
        assert!(!stored.is_current(ScanFeature::Usage, &current));
    }

    #[test]
    fn file_parse_stamps_both_halves_but_tiers_only_on_usage() {
        let stamp = ScanStamp::new(Some("abc".to_string()), TinyMap::new());
        let entry = SessionEntry::from_file_parse(
            Some(analysis_with_usage(1)),
            true,
            "2026-09-08".into(),
            stamp,
        );
        assert_eq!(
            entry.scanned.usage.as_ref().unwrap().tiers.as_deref(),
            Some("abc")
        );
        assert!(entry.scanned.analysis.as_ref().unwrap().tiers.is_none());
        assert!(entry.days.contains_key("2026-09-08"));
    }
}
