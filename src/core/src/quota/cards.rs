//! Normalized multi-provider quota cards for `vct quota --json`.
//!
//! Fetches every enabled provider with credentials in parallel using one shared HTTP client, reuses each provider's existing state resolution, OAuth refresh, Codex session fallback, and `~/.vct/quota/<provider>.json` cache persistence, and maps the resulting snapshots into [`QuotaCard`] entries.

use crate::config::{ProvidersConfig, quota_panel_selected};
use crate::models::{
    AntigravityQuotaSnapshot, ClaudeQuotaSnapshot, CodexQuotaSnapshot, CopilotQuotaSnapshot,
    CursorQuotaSnapshot, GrokQuotaSnapshot, QuotaSource,
};
use crate::quota::{
    ANTIGRAVITY_LOGIN_HINT, AntigravityState, CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT,
    COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, ClaudeState, CodexState, CopilotState, CursorState,
    GROK_LOGIN_HINT, GrokState, QuotaOutcome, QuotaSnapshot, antigravity_quota_present, http,
    load_antigravity_cache, load_claude_cache, load_codex_cache, load_copilot_cache,
    load_cursor_cache, load_grok_cache, save_antigravity_cache, save_claude_cache,
    save_codex_cache, save_copilot_cache, save_cursor_cache, save_grok_cache,
};
use crate::utils::{
    get_claude_credentials_path, get_copilot_config_path, get_cursor_auth_path, get_grok_auth_path,
    resolve_paths,
};
use anyhow::Result;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::panic::{AssertUnwindSafe, catch_unwind};

/// One normalized rate-limit bucket in a [`QuotaCard`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaCardBucket {
    /// Human-readable label for this bucket (for example `"5-hour"`, `"7-day"`, `"Premium"`, or a model pool name).
    pub label: String,
    /// Short window identifier (for example `"5h"`, `"7d"`, `"1m"`, `"cycle"`, or `"weekly"`).
    pub window: String,
    /// Percent of the window consumed (`0.0..=100.0`).
    pub used_percent: f64,
    /// Absolute reset time in Unix seconds, or `null` when unknown.
    pub resets_at: Option<i64>,
}

/// Normalized quota card emitted by `vct quota --json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QuotaCard {
    /// Provider identifier (`"claude"`, `"codex"`, `"copilot"`, `"cursor"`, `"grok"`, or `"antigravity"`).
    pub provider: String,
    /// Plan tier (for example `"max 20x"` or `"plus"`), or `null` when unavailable.
    pub plan: Option<String>,
    /// Unix seconds when this snapshot was produced (`0` if never fetched).
    pub fetched_at: i64,
    /// Origin of this snapshot (`"api"`, `"session_fallback"`, or `"none"`).
    pub source: QuotaSource,
    /// Whether any quota limit has been reached.
    pub limit_reached: bool,
    /// Whether credentials are unusable and require re-authentication.
    pub needs_login: bool,
    /// Error or login hint when fetching failed or credentials were rejected, or `null` on success.
    pub error: Option<String>,
    /// Normalized rate-limit buckets.
    pub buckets: Vec<QuotaCardBucket>,
}

impl QuotaCard {
    /// Maps a [`ClaudeQuotaSnapshot`] into a [`QuotaCard`].
    pub fn from_claude(snap: &ClaudeQuotaSnapshot, error: Option<String>) -> Self {
        let mut buckets = Vec::new();
        if let Some(w) = &snap.five_hour {
            buckets.push(QuotaCardBucket {
                label: "5-hour".to_string(),
                window: "5h".to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        if let Some(w) = &snap.seven_day {
            buckets.push(QuotaCardBucket {
                label: "7-day".to_string(),
                window: "7d".to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        if let (Some(w), Some(label)) = (&snap.scoped_weekly, &snap.scoped_label) {
            buckets.push(QuotaCardBucket {
                label: label.clone(),
                window: "7d".to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        Self {
            provider: "claude".to_string(),
            plan: snap.plan_type.clone(),
            fetched_at: snap.fetched_at,
            source: snap.source,
            limit_reached: snap.limit_reached,
            needs_login: snap.needs_login,
            error,
            buckets,
        }
    }

    /// Maps a [`CodexQuotaSnapshot`] into a [`QuotaCard`].
    pub fn from_codex(snap: &CodexQuotaSnapshot, error: Option<String>) -> Self {
        let mut buckets = Vec::new();
        if let Some(w) = &snap.primary {
            buckets.push(QuotaCardBucket {
                label: "5-hour".to_string(),
                window: "5h".to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        if let Some(w) = &snap.secondary {
            buckets.push(QuotaCardBucket {
                label: "7-day".to_string(),
                window: "7d".to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        Self {
            provider: "codex".to_string(),
            plan: snap.plan_type.clone(),
            fetched_at: snap.fetched_at,
            source: snap.source,
            limit_reached: snap.limit_reached == Some(true),
            needs_login: snap.needs_login,
            error,
            buckets,
        }
    }

    /// Maps a [`CopilotQuotaSnapshot`] into a [`QuotaCard`].
    pub fn from_copilot(snap: &CopilotQuotaSnapshot, error: Option<String>) -> Self {
        let mut buckets = Vec::new();
        if let Some(w) = &snap.premium {
            buckets.push(QuotaCardBucket {
                label: "Premium".to_string(),
                window: "1m".to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        Self {
            provider: "copilot".to_string(),
            plan: snap.plan_type.clone(),
            fetched_at: snap.fetched_at,
            source: snap.source,
            limit_reached: snap.limit_reached,
            needs_login: snap.needs_login,
            error,
            buckets,
        }
    }

    /// Maps a [`CursorQuotaSnapshot`] into a [`QuotaCard`].
    pub fn from_cursor(snap: &CursorQuotaSnapshot, error: Option<String>) -> Self {
        let mut buckets = Vec::new();
        for (label, window) in [
            ("Total", &snap.total),
            ("Auto", &snap.auto),
            ("API", &snap.api),
        ] {
            if let Some(w) = window {
                buckets.push(QuotaCardBucket {
                    label: label.to_string(),
                    window: "cycle".to_string(),
                    used_percent: w.used_percent,
                    resets_at: w.resets_at_unix,
                });
            }
        }
        Self {
            provider: "cursor".to_string(),
            plan: snap.plan_type.clone(),
            fetched_at: snap.fetched_at,
            source: snap.source,
            limit_reached: snap.limit_reached,
            needs_login: snap.needs_login,
            error,
            buckets,
        }
    }

    /// Maps a [`GrokQuotaSnapshot`] into a [`QuotaCard`].
    pub fn from_grok(snap: &GrokQuotaSnapshot, error: Option<String>) -> Self {
        let mut buckets = Vec::new();
        if let Some(w) = &snap.included {
            let window = match snap.period_label.as_deref() {
                Some("week") => "7d",
                Some("month") => "1m",
                Some(other) => other,
                None => "period",
            };
            buckets.push(QuotaCardBucket {
                label: "Included".to_string(),
                window: window.to_string(),
                used_percent: w.used_percent,
                resets_at: w.resets_at_unix,
            });
        }
        Self {
            provider: "grok".to_string(),
            plan: snap.plan_type.clone(),
            fetched_at: snap.fetched_at,
            source: snap.source,
            limit_reached: snap.limit_reached,
            needs_login: snap.needs_login,
            error,
            buckets,
        }
    }

    /// Maps an [`AntigravityQuotaSnapshot`] into a [`QuotaCard`].
    pub fn from_antigravity(snap: &AntigravityQuotaSnapshot, error: Option<String>) -> Self {
        let buckets = snap
            .buckets
            .iter()
            .map(|b| QuotaCardBucket {
                label: if b.group.is_empty() {
                    "Pool".to_string()
                } else {
                    b.group.clone()
                },
                window: b.window.clone(),
                used_percent: b.usage.used_percent,
                resets_at: b.usage.resets_at_unix,
            })
            .collect();
        Self {
            provider: "antigravity".to_string(),
            plan: None,
            fetched_at: snap.fetched_at,
            source: snap.source,
            limit_reached: snap.limit_reached,
            needs_login: snap.needs_login,
            error,
            buckets,
        }
    }
}

/// Presence flags for one-shot quota resolution.
#[derive(Debug, Clone, Copy, Default)]
struct PresentPanels {
    claude: bool,
    codex_auth: bool,
    codex_sessions: bool,
    copilot: bool,
    cursor: bool,
    grok: bool,
    antigravity: bool,
}

impl PresentPanels {
    fn detect(panels: &[String], providers: ProvidersConfig) -> Self {
        if panels.is_empty() {
            return Self::default();
        }
        let panel_on = |name: &str| quota_panel_selected(panels, name);
        let claude = providers.claude
            && panel_on("claude")
            && get_claude_credentials_path()
                .map(|p| p.exists())
                .unwrap_or(false);
        let (codex_auth, codex_sessions) = if providers.codex && panel_on("codex") {
            resolve_paths()
                .map(|p| {
                    (
                        p.codex_dir.join("auth.json").exists(),
                        p.codex_session_dirs().iter().any(|d| d.exists()),
                    )
                })
                .unwrap_or((false, false))
        } else {
            (false, false)
        };
        let copilot = providers.copilot
            && panel_on("copilot")
            && get_copilot_config_path()
                .map(|p| p.exists())
                .unwrap_or(false);
        let cursor = providers.cursor
            && panel_on("cursor")
            && get_cursor_auth_path().map(|p| p.exists()).unwrap_or(false);
        let grok = providers.grok
            && panel_on("grok")
            && get_grok_auth_path().map(|p| p.exists()).unwrap_or(false);
        let antigravity =
            providers.antigravity && panel_on("antigravity") && antigravity_quota_present();

        Self {
            claude,
            codex_auth,
            codex_sessions,
            copilot,
            cursor,
            grok,
            antigravity,
        }
    }

    fn codex(&self) -> bool {
        self.codex_auth || self.codex_sessions
    }

    fn any(&self) -> bool {
        self.claude || self.codex() || self.copilot || self.cursor || self.grok || self.antigravity
    }
}

/// Applies one provider's [`QuotaOutcome`] against its cached snapshot, persists updates, and converts to a [`QuotaCard`].
///
/// When `skip_empty_transient` is `true`, a transient failure with no cached snapshot returns `None` (used for session-only Codex folders with no `auth.json`).
fn resolve_one<T, R, S, M>(
    label: &'static str,
    login_hint: &'static str,
    cached: Option<T>,
    skip_empty_transient: bool,
    resolve: R,
    save: S,
    to_card: M,
) -> Option<QuotaCard>
where
    T: QuotaSnapshot,
    R: FnOnce() -> QuotaOutcome<T>,
    S: FnOnce(&T),
    M: FnOnce(&T, Option<String>) -> QuotaCard,
{
    match catch_unwind(AssertUnwindSafe(resolve)) {
        Ok(QuotaOutcome::Data(snap)) => {
            save(&snap);
            let mut card = to_card(&snap, None);
            if card.needs_login {
                card.error = Some(login_hint.to_string());
            }
            Some(card)
        }
        Ok(QuotaOutcome::NeedsLogin) => {
            let mut snap = cached.unwrap_or_default();
            snap.set_needs_login(true);
            save(&snap);
            Some(to_card(&snap, Some(login_hint.to_string())))
        }
        Ok(QuotaOutcome::Transient) => {
            if skip_empty_transient && cached.is_none() {
                return None;
            }
            let snap = cached.unwrap_or_default();
            Some(to_card(&snap, Some("failed to fetch quota".to_string())))
        }
        Err(_) => {
            log::warn!("{label} quota fetch panicked");
            let snap = cached.unwrap_or_default();
            Some(to_card(&snap, Some("quota fetch panicked".to_string())))
        }
    }
}

/// Fetches normalized quota cards in parallel for every enabled provider in `panels` that has credentials on this machine.
///
/// Reuses the same per-provider credential detection, OAuth token refresh, Codex session fallback, and `~/.vct/quota/<provider>.json` caching as the interactive Quota tab. Providers without credentials are skipped; per-provider auth or fetch failures are reported on their card rather than aborting the call.
///
/// # Errors
///
/// Returns an error only if the shared HTTP client cannot be constructed when at least one provider is present.
pub fn fetch_quota_cards(panels: &[String], providers: ProvidersConfig) -> Result<Vec<QuotaCard>> {
    let present = PresentPanels::detect(panels, providers);
    if !present.any() {
        return Ok(Vec::new());
    }
    let client = http::build_client()?;
    Ok(fetch_quota_cards_with_client(&client, present))
}

fn fetch_quota_cards_with_client(client: &Client, present: PresentPanels) -> Vec<QuotaCard> {
    std::thread::scope(|scope| {
        let claude = present.claude.then(|| {
            scope.spawn(|| {
                let mut state = ClaudeState::default();
                resolve_one(
                    "claude",
                    CLAUDE_LOGIN_HINT,
                    load_claude_cache(),
                    false,
                    || state.resolve(client),
                    |snap| {
                        let _ = save_claude_cache(snap);
                    },
                    QuotaCard::from_claude,
                )
            })
        });
        let codex = present.codex().then(|| {
            let skip_empty_transient = !present.codex_auth;
            scope.spawn(move || {
                let mut state = CodexState::default();
                resolve_one(
                    "codex",
                    CODEX_LOGIN_HINT,
                    load_codex_cache(),
                    skip_empty_transient,
                    || state.resolve(client),
                    |snap| {
                        let _ = save_codex_cache(snap);
                    },
                    QuotaCard::from_codex,
                )
            })
        });
        let copilot = present.copilot.then(|| {
            scope.spawn(|| {
                let mut state = CopilotState::default();
                resolve_one(
                    "copilot",
                    COPILOT_LOGIN_HINT,
                    load_copilot_cache(),
                    false,
                    || state.resolve(client),
                    |snap| {
                        let _ = save_copilot_cache(snap);
                    },
                    QuotaCard::from_copilot,
                )
            })
        });
        let cursor = present.cursor.then(|| {
            scope.spawn(|| {
                let mut state = CursorState::default();
                resolve_one(
                    "cursor",
                    CURSOR_LOGIN_HINT,
                    load_cursor_cache(),
                    false,
                    || state.resolve(client),
                    |snap| {
                        let _ = save_cursor_cache(snap);
                    },
                    QuotaCard::from_cursor,
                )
            })
        });
        let grok = present.grok.then(|| {
            scope.spawn(|| {
                let mut state = GrokState::default();
                resolve_one(
                    "grok",
                    GROK_LOGIN_HINT,
                    load_grok_cache(),
                    false,
                    || state.resolve(client),
                    |snap| {
                        let _ = save_grok_cache(snap);
                    },
                    QuotaCard::from_grok,
                )
            })
        });
        let antigravity = present.antigravity.then(|| {
            scope.spawn(|| {
                let mut state = AntigravityState::default();
                resolve_one(
                    "antigravity",
                    ANTIGRAVITY_LOGIN_HINT,
                    load_antigravity_cache(),
                    false,
                    || state.resolve(client),
                    |snap| {
                        let _ = save_antigravity_cache(snap);
                    },
                    QuotaCard::from_antigravity,
                )
            })
        });

        let mut cards = Vec::new();
        for handle in [claude, codex, copilot, cursor, grok, antigravity]
            .into_iter()
            .flatten()
        {
            if let Ok(Some(card)) = handle.join() {
                cards.push(card);
            }
        }
        cards
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{AntigravityBucket, QuotaWindow};
    use httpmock::prelude::*;
    use std::cell::Cell;

    fn win(pct: f64, reset: Option<i64>) -> QuotaWindow {
        QuotaWindow {
            used_percent: pct,
            resets_at_unix: reset,
        }
    }

    #[test]
    fn maps_every_provider_snapshot_to_normalized_card() {
        let claude = QuotaCard::from_claude(
            &ClaudeQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1000,
                plan_type: Some("max 20x".to_string()),
                five_hour: Some(win(25.0, Some(1500))),
                seven_day: Some(win(40.0, Some(2000))),
                scoped_weekly: Some(win(10.0, Some(2000))),
                scoped_label: Some("Opus".to_string()),
                limit_reached: false,
                needs_login: false,
                ..Default::default()
            },
            None,
        );
        assert_eq!(claude.provider, "claude");
        assert_eq!(claude.plan.as_deref(), Some("max 20x"));
        assert_eq!(claude.buckets.len(), 3);
        assert_eq!(claude.buckets[0].label, "5-hour");
        assert_eq!(claude.buckets[0].window, "5h");
        assert_eq!(claude.buckets[2].label, "Opus");
        assert_eq!(claude.buckets[2].window, "7d");

        let codex = QuotaCard::from_codex(
            &CodexQuotaSnapshot {
                source: QuotaSource::SessionFallback,
                fetched_at: 1001,
                plan_type: Some("plus".to_string()),
                primary: Some(win(100.0, Some(1600))),
                secondary: Some(win(12.0, None)),
                limit_reached: Some(true),
                needs_login: true,
                ..Default::default()
            },
            Some(CODEX_LOGIN_HINT.to_string()),
        );
        assert_eq!(codex.provider, "codex");
        assert_eq!(codex.source, QuotaSource::SessionFallback);
        assert!(codex.limit_reached);
        assert!(codex.needs_login);
        assert_eq!(codex.error.as_deref(), Some(CODEX_LOGIN_HINT));
        assert_eq!(codex.buckets.len(), 2);
        assert_eq!(codex.buckets[1].resets_at, None);

        let copilot = QuotaCard::from_copilot(
            &CopilotQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1002,
                plan_type: Some("individual".to_string()),
                premium: Some(win(33.5, Some(1700))),
                ..Default::default()
            },
            None,
        );
        assert_eq!(copilot.buckets[0].label, "Premium");
        assert_eq!(copilot.buckets[0].window, "1m");

        let cursor = QuotaCard::from_cursor(
            &CursorQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1003,
                plan_type: Some("pro".to_string()),
                total: Some(win(50.0, Some(1800))),
                auto: Some(win(20.0, Some(1800))),
                api: Some(win(80.0, Some(1800))),
                ..Default::default()
            },
            None,
        );
        assert_eq!(cursor.buckets.len(), 3);
        assert_eq!(cursor.buckets[0].label, "Total");
        assert_eq!(cursor.buckets[2].label, "API");

        let grok = QuotaCard::from_grok(
            &GrokQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1004,
                plan_type: Some("SuperGrok".to_string()),
                included: Some(win(15.0, Some(1900))),
                period_label: Some("week".to_string()),
                ..Default::default()
            },
            None,
        );
        assert_eq!(grok.buckets[0].label, "Included");
        assert_eq!(grok.buckets[0].window, "7d");

        let antigravity = QuotaCard::from_antigravity(
            &AntigravityQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1005,
                buckets: vec![AntigravityBucket {
                    group: "Gemini Models".to_string(),
                    window: "5h".to_string(),
                    usage: win(42.0, Some(2100)),
                }],
                limit_reached: false,
                needs_login: false,
            },
            None,
        );
        assert_eq!(antigravity.plan, None);
        assert_eq!(antigravity.buckets[0].label, "Gemini Models");
        assert_eq!(antigravity.buckets[0].window, "5h");
    }

    #[test]
    fn resolve_one_handles_needs_login_and_preserves_cache_on_transient() {
        let cached = ClaudeQuotaSnapshot {
            source: QuotaSource::Api,
            fetched_at: 500,
            plan_type: Some("pro".to_string()),
            five_hour: Some(win(10.0, Some(900))),
            ..Default::default()
        };

        // NeedsLogin keeps cached buckets, sets needs_login + hint, and persists cache.
        let saved = Cell::new(false);
        let card = resolve_one(
            "claude",
            CLAUDE_LOGIN_HINT,
            Some(cached.clone()),
            false,
            || QuotaOutcome::NeedsLogin,
            |snap| {
                assert!(snap.needs_login);
                saved.set(true);
            },
            QuotaCard::from_claude,
        )
        .expect("needs_login produces a card");
        assert!(saved.get());
        assert!(card.needs_login);
        assert_eq!(card.error.as_deref(), Some(CLAUDE_LOGIN_HINT));
        assert_eq!(card.buckets.len(), 1);
        assert_eq!(card.fetched_at, 500);

        // Transient preserves cached snapshot, sets error, and does not overwrite cache.
        let saved = Cell::new(false);
        let card = resolve_one(
            "claude",
            CLAUDE_LOGIN_HINT,
            Some(cached),
            false,
            || QuotaOutcome::Transient,
            |_| saved.set(true),
            QuotaCard::from_claude,
        )
        .expect("transient produces a card");
        assert!(!saved.get());
        assert!(!card.needs_login);
        assert!(card.error.is_some());
        assert_eq!(card.buckets.len(), 1);

        // Session-only Codex with no cache skips empty transient errors.
        let skipped = resolve_one(
            "codex",
            CODEX_LOGIN_HINT,
            None::<CodexQuotaSnapshot>,
            true,
            || QuotaOutcome::Transient,
            |_| panic!("must not save on transient"),
            QuotaCard::from_codex,
        );
        assert!(skipped.is_none());
    }

    #[test]
    fn parallel_httpmock_fetch_isolates_auth_failure_from_healthy_provider() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/copilot_internal/user");
            then.status(200).json_body(serde_json::json!({
                "copilot_plan": "individual",
                "quota_reset_date_utc": "2026-12-01T00:00:00Z",
                "quota_snapshots": {
                    "premium_interactions": {
                        "percent_remaining": 75.0,
                        "remaining": 225,
                        "entitlement": 300,
                        "unlimited": false
                    }
                }
            }));
        });
        server.mock(|when, then| {
            when.method(GET).path("/api/usage-summary");
            then.status(401);
        });

        let client = http::build_client().unwrap();
        let copilot_url = server.url("/copilot_internal/user");
        let cursor_url = server.url("/api/usage-summary");

        let cards: Vec<QuotaCard> = std::thread::scope(|scope| {
            let h1 = scope.spawn(|| {
                resolve_one(
                    "copilot",
                    COPILOT_LOGIN_HINT,
                    None,
                    false,
                    || {
                        let resp = client.get(&copilot_url).send().unwrap();
                        assert!(resp.status().is_success());
                        let parsed: crate::models::CopilotUserResponse = resp.json().unwrap();
                        let prem = parsed
                            .quota_snapshots
                            .unwrap()
                            .premium_interactions
                            .unwrap();
                        QuotaOutcome::Data(CopilotQuotaSnapshot {
                            source: QuotaSource::Api,
                            fetched_at: 1_700_000_000,
                            plan_type: parsed.copilot_plan,
                            premium: Some(win(100.0 - prem.percent_remaining.unwrap(), None)),
                            ..Default::default()
                        })
                    },
                    |_| {},
                    QuotaCard::from_copilot,
                )
            });
            let h2 = scope.spawn(|| {
                resolve_one(
                    "cursor",
                    CURSOR_LOGIN_HINT,
                    None,
                    false,
                    || {
                        let resp = client.get(&cursor_url).send().unwrap();
                        if resp.status().as_u16() == 401 {
                            QuotaOutcome::NeedsLogin
                        } else {
                            QuotaOutcome::Transient
                        }
                    },
                    |_| {},
                    QuotaCard::from_cursor,
                )
            });
            [h1.join().unwrap(), h2.join().unwrap()]
                .into_iter()
                .flatten()
                .collect()
        });

        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].provider, "copilot");
        assert!(!cards[0].needs_login);
        assert_eq!(cards[0].error, None);
        assert!((cards[0].buckets[0].used_percent - 25.0).abs() < 1e-9);

        assert_eq!(cards[1].provider, "cursor");
        assert!(cards[1].needs_login);
        assert_eq!(cards[1].error.as_deref(), Some(CURSOR_LOGIN_HINT));
    }

    #[test]
    fn card_json_serializes_nullable_fields_explicitly() {
        let card = QuotaCard::from_antigravity(
            &AntigravityQuotaSnapshot {
                source: QuotaSource::Api,
                fetched_at: 1234,
                buckets: vec![AntigravityBucket {
                    group: "Gemini".to_string(),
                    window: "5h".to_string(),
                    usage: win(10.0, None),
                }],
                limit_reached: false,
                needs_login: false,
            },
            None,
        );
        let value = serde_json::to_value(&card).unwrap();
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("plan") && obj["plan"].is_null());
        assert!(obj.contains_key("error") && obj["error"].is_null());
        let bucket = obj["buckets"][0].as_object().unwrap();
        assert!(bucket.contains_key("resets_at") && bucket["resets_at"].is_null());
    }
}
