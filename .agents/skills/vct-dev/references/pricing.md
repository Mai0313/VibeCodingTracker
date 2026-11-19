# Pricing (`src/core/src/pricing/`)

## Pricing source, cache, and backoff

- Pricing comes from LiteLLM (`model_prices_and_context_window.json`), cached daily as `~/.vct/model_pricing_YYYY-MM-DD.json`. Do not switch to `models.dev` (lacks separate 1h prompt-cache write rates, lacks `search_context_cost_per_query`, drops retired first-party model ids, and flattens DeepSeek V4 peak/off-peak rates).
- The daily cache stores the **filtered raw upstream JSON** (not derived `ModelPricing`) so new fields survive without losing data. Before atomically replacing the cache, a response MUST be HTTP 2xx, a JSON object, contain zero negative/non-finite rates across all models, and include at least one model with a real price.
- Failed fetches arm a 300 s backoff keyed on `(url, cache_dir)`. Manual refresh does not bypass it, but today's cache file is checked *before* the backoff so a valid cache is still served inside a backoff window. Once a process loads a valid map, it retains it across failures until the UTC date changes.

## Lookup priority and tie-breaks

- Stages: exact → normalized → most-specific substring → Jaro-Winkler fuzzy (≥ 0.7) → `$0.00` fallback.
- Generic placeholders (`default`, `auto`, …) and model segments shorter than 4 **bytes** skip substring and fuzzy stages (unpriced beats a coincidental neighbor).
- Date suffixes require exactly 8 ASCII digits; version suffixes allow numeric/dot forms (`v2`, `v1.0`) only, never words like `vision`.
- Normalized collisions and substring tie-breaks prefer: same provider → unprefixed canonical key → model vendor's own official provider prefix (unsuffixed base ahead of versioned variants across all three) → suffix length → lexical order.
- Each `ModelPricingMap` owns its own capacity-64 LRU. `clear_pricing_cache()` advances a generation counter so existing maps lazily invalidate without cross-map contamination.

## Per-request context tiers and non-token billing

- Context tiers (`*_above_Nk_tokens` and `tiered_pricing` ranges) MUST be classified per request, never against aggregated token sums.
- `UsageScanOptions` threads a `TierThresholds` snapshot from `ModelPricingMap::tier_thresholds` into Claude, Codex, Gemini, DeepSeek, Antigravity, and OpenCode assistant-message readers. Per-request buckets fold into a flat `above_tier` integer map keyed `level_<n>_<bucket>` (internal to usage values; `analysis` never emits it so public `CodeAnalysis` JSON stays unchanged).
- Range-based models contribute each row's inclusive `min_tokens - 1` (excluding row 0) rather than the previous row's exclusive `max_tokens`, so a request falling in a gap is never promoted to a higher tier.
- Missing prices at level `n` fall back to level `n - 1`; levels past the last row bill at the last row; `MAX_TIER_LEVELS` bounds both the snapshot and extractor.
- Providers without per-request granularity (Copilot, Cursor/Grok gauges, Hermes pre-aggregated table, OpenCode legacy `session` fallback) and offline scans classify nothing (intentional lower bound).
- Live reachability note: LiteLLM's range-priced models (`dashscope/*`, `volcengine/*`) have no live path from any provider vct scans today because models.dev lacks those provider IDs for OpenCode's exact-only `CostSource::OpenCodeStored`; `ranges[1..]` is therefore exercised by tests alone.
- Claude `server_tool_use.web_search_requests` is billed per query via `ModelPricing::web_search_cost_per_query` (`search_context_cost_per_query`); `web_fetch_requests` is NOT separately billed (its content already counts as input tokens).
