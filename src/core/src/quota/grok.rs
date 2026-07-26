//! Grok CLI quota fetcher.
//!
//! Reads the bearer from `~/.grok/auth.json` and calls the CLI's own billing
//! endpoint (`GET /v1/billing?format=credits`), impersonating the Grok CLI via
//! its `grok-pager` / `grok-shell` User-Agent and the `X-XAI-Token-Auth` header
//! the proxy's auth middleware requires. The plan label comes from the companion
//! `/v1/settings` endpoint and is fetched at most once per worker.
//!
//! The access token lives in each entry's **`key`** field (not `access_token`)
//! and expires in hours, so refresh works like Claude's: proactive near expiry,
//! reactive on a rejected request, with the rotated token written back into that
//! entry — preserving every other entry — and a cooldown so a revoked token
//! cannot spin the token endpoint. The token endpoint itself is never
//! hardcoded: it is resolved by OIDC discovery on the entry's issuer, the same
//! way the official CLI resolves it.

use crate::models::{
    GrokAuthEntry, GrokBillingResponse, GrokQuotaSnapshot, GrokRefreshResponse,
    GrokSettingsResponse, QuotaSource, QuotaWindow,
};
use crate::quota::http::{detect_cli_version, iso_to_unix_secs};
use crate::quota::provider::QuotaOutcome;
use crate::quota::refresh::{
    EXPIRY_SKEW_SECS, RefreshCooldown, file_mtime, is_expiring, send_refresh,
    update_json_file_in_place,
};
use crate::utils::{get_grok_auth_path, parse_iso_timestamp, rfc3339_utc_nanos};
use anyhow::{Context, Result, bail};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::json;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

/// The Grok credits/billing endpoint.
const GROK_BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
/// The Grok remote-settings endpoint, which carries the plan label.
const GROK_SETTINGS_URL: &str = "https://cli-chat-proxy.grok.com/v1/settings";
/// Fallback Grok CLI version for the User-Agent and the proxy's version gate
/// when `grok --version` cannot be resolved (CLI absent or unreadable). Bump
/// occasionally.
const GROK_FALLBACK_VERSION: &str = "0.2.112";
/// Tells the proxy's auth middleware to validate the bearer as a CLI session.
/// Without it the request is rejected regardless of the token.
const GROK_TOKEN_AUTH: &str = "xai-grok-cli";
/// Access-token lifetime assumed when a refresh response omits `expires_in`.
const GROK_DEFAULT_EXPIRES_IN: i64 = 6 * 3600;
/// Login hint shown when the Grok token is expired / rejected.
pub const GROK_LOGIN_HINT: &str = "run: grok login";

/// The installed Grok CLI's version, detected once and cached for the day.
fn grok_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| detect_cli_version("grok", "grok_version.json", GROK_FALLBACK_VERSION))
        .as_str()
}

/// Builds the Grok CLI's request User-Agent, e.g.
/// `grok-pager/0.2.112 grok-shell/0.2.112 (linux; x86_64)`.
fn grok_ua() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| {
        let v = grok_version();
        format!(
            "grok-pager/{v} grok-shell/{v} ({}; {})",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })
    .as_str()
}

/// One usable Grok login: the entry plus the `auth.json` key it was stored under.
///
/// The file holds one entry per login scope, keyed `"<issuer>::<client_id>"` for
/// an xAI OAuth2 login, `"xai::api_key"` for a plain API key, or a legacy
/// sign-in URL. The key is retained so a refresh writes back into the same
/// entry and leaves every sibling untouched.
pub(crate) struct GrokCredential {
    pub(crate) entry_key: String,
    pub(crate) entry: GrokAuthEntry,
}

impl GrokCredential {
    /// The bearer token (the entry's `key` field).
    fn access_token(&self) -> &str {
        self.entry.key.as_deref().unwrap_or_default()
    }

    /// The account id sent as `x-userid`.
    fn user_id(&self) -> Option<&str> {
        self.entry.user_id.as_deref().filter(|s| !s.is_empty())
    }

    /// The refresh token, when this login can be refreshed at all.
    fn refresh_token(&self) -> Option<&str> {
        self.entry
            .refresh_token
            .as_deref()
            .filter(|s| !s.is_empty())
    }

    /// The OIDC issuer: the entry's own field, else the first `::`-delimited
    /// half of the entry key. Only a URL counts, so an `xai::api_key` entry
    /// (whose first half is `xai`) never yields a bogus issuer.
    fn issuer(&self) -> Option<&str> {
        self.entry
            .oidc_issuer
            .as_deref()
            .or_else(|| self.entry_key.split("::").next())
            .filter(|s| s.starts_with("http"))
    }

    /// The OAuth client id: the entry's own field, else the last `::`-delimited
    /// half of the entry key.
    fn client_id(&self) -> Option<&str> {
        self.entry
            .oidc_client_id
            .as_deref()
            .or_else(|| self.entry_key.rsplit("::").next())
            .filter(|s| !s.is_empty())
    }

    /// The access-token expiry in Unix seconds, when the entry records one.
    /// An API-key login has none, which reads as "does not expire".
    fn expires_at_secs(&self) -> Option<i64> {
        let ms = parse_iso_timestamp(self.entry.expires_at.as_deref()?);
        (ms > 0).then_some(ms / 1000)
    }

    /// Whether this login carries everything a refresh needs.
    fn is_refreshable(&self) -> bool {
        self.refresh_token().is_some() && self.issuer().is_some() && self.client_id().is_some()
    }
}

/// Picks the login to use from an `auth.json` body.
///
/// The file can hold several logins and JSON object order is not preserved in
/// Rust, so the choice is made explicitly rather than by position: among entries
/// carrying a token, a refreshable OIDC login wins over a bare API key (it is
/// the subscription the quota describes, and it can be renewed), then the latest
/// expiry, then the entry key — so the same file always resolves to the same
/// login. A malformed entry is skipped rather than failing the whole file.
pub(crate) fn select_grok_entry(body: &str) -> Option<GrokCredential> {
    let root: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut candidates: Vec<GrokCredential> = root
        .as_object()?
        .iter()
        .filter_map(|(entry_key, value)| {
            let entry: GrokAuthEntry = serde_json::from_value(value.clone()).ok()?;
            Some(GrokCredential {
                entry_key: entry_key.clone(),
                entry,
            })
        })
        .filter(|c| !c.access_token().is_empty())
        .collect();

    candidates.sort_by(|a, b| {
        b.is_refreshable()
            .cmp(&a.is_refreshable())
            .then(
                b.expires_at_secs()
                    .unwrap_or(0)
                    .cmp(&a.expires_at_secs().unwrap_or(0)),
            )
            .then(a.entry_key.cmp(&b.entry_key))
    });
    candidates.into_iter().next()
}

/// The outcome of reading a login for one worker tick.
enum CredentialRead {
    // Boxed so the outcome enum stays pointer-sized: the credential is one
    // per tick, but every arm would otherwise carry its whole footprint.
    Ok(Box<GrokCredential>),
    /// The file exists but holds no usable login (logged out / cleared): an auth
    /// failure, so nudge login rather than sitting on stale numbers.
    NeedsLogin,
    /// The file is absent or unreadable — e.g. an atomic rewrite by the official
    /// CLI is in flight — so keep last-known-good.
    Transient,
}

/// Reads `auth.json` and picks the login, splitting a transient file error from
/// an auth failure the way the Claude and Cursor workers do.
fn read_grok_credential(path: &Path) -> CredentialRead {
    let Ok(body) = std::fs::read_to_string(path) else {
        return CredentialRead::Transient;
    };
    match select_grok_entry(&body) {
        Some(cred) => CredentialRead::Ok(Box::new(cred)),
        None => CredentialRead::NeedsLogin,
    }
}

/// Builds a request carrying the Grok CLI's identity headers.
///
/// Only the bearer and `X-XAI-Token-Auth` are load-bearing; the version gate
/// reads `x-grok-client-version`, and the rest is the client identity.
fn grok_request(client: &Client, url: &str, token: &str, user_id: Option<&str>) -> RequestBuilder {
    let mut req = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, grok_ua())
        .header("X-XAI-Token-Auth", GROK_TOKEN_AUTH)
        .header("x-grok-client-version", grok_version())
        .header("x-grok-client-mode", "interactive")
        .bearer_auth(token);
    if let Some(uid) = user_id {
        req = req.header("x-userid", uid);
    }
    req
}

/// Maps a `USAGE_PERIOD_TYPE_*` value to a short gauge label.
fn period_label(period_type: &str) -> Option<&'static str> {
    match period_type {
        "USAGE_PERIOD_TYPE_WEEKLY" => Some("week"),
        "USAGE_PERIOD_TYPE_MONTHLY" => Some("month"),
        _ => None,
    }
}

/// Maps a `billing?format=credits` body into a [`GrokQuotaSnapshot`] (pure).
///
/// Proto3 JSON omits zero-valued scalars, so an absent percent or money field
/// reads as zero. Money is in cents.
///
/// # Errors
///
/// Returns an error if the body is not valid JSON or carries no `config`
/// object (an error payload rather than a credits response).
pub fn map_grok_billing(body: &str, now: i64) -> Result<GrokQuotaSnapshot> {
    let resp: GrokBillingResponse =
        serde_json::from_str(body).context("Failed to parse Grok billing response")?;
    let config = resp
        .config
        .context("Grok billing response carried no config object")?;

    let resets_at_unix = config
        .current_period
        .as_ref()
        .and_then(|p| p.end.as_deref())
        .or(config.billing_period_end.as_deref())
        .and_then(iso_to_unix_secs);
    // Clamped only as an out-of-range guard: the field is already percent
    // consumed, the same number the CLI status bar shows.
    let used_percent = config.credit_usage_percent.clamp(0.0, 100.0);

    let cap = config.on_demand_cap.as_ref().map(|m| m.as_dollars());
    let used = config.on_demand_used.as_ref().map(|m| m.as_dollars());
    let balance = config.prepaid_balance.as_ref().map(|m| m.as_dollars());
    // Pay-as-you-go and prepaid credit are opt-in extras: show them only once
    // they carry a value, so a plain subscription panel stays three lines.
    let on_demand_cap_dollars = cap.filter(|c| *c > 0.0);
    let on_demand_dollars = used
        .filter(|u| *u > 0.0)
        .or(on_demand_cap_dollars.and(used));

    Ok(GrokQuotaSnapshot {
        source: QuotaSource::Api,
        fetched_at: now,
        // Set by the worker from the settings endpoint, not the billing body.
        plan_type: None,
        included: Some(QuotaWindow {
            used_percent,
            resets_at_unix,
        }),
        period_label: config
            .current_period
            .as_ref()
            .and_then(|p| p.period_type.as_deref())
            .and_then(period_label)
            .map(str::to_string),
        on_demand_dollars,
        on_demand_cap_dollars,
        prepaid_balance_dollars: balance.filter(|b| *b > 0.0),
        limit_reached: used_percent >= 100.0,
        needs_login: false,
    })
}

/// Outcome of a single billing request.
enum FetchResult {
    Ok(GrokQuotaSnapshot),
    /// 401/403 → the token is rejected; refresh and retry.
    Unauthorized,
    /// Network / other non-success error → keep last-known-good.
    Transient,
}

/// Calls the billing API at `billing_url` with `token`.
fn fetch_grok_billing(
    client: &Client,
    token: &str,
    user_id: Option<&str>,
    now: i64,
    billing_url: &str,
) -> FetchResult {
    let resp = match grok_request(client, billing_url, token, user_id).send() {
        Ok(r) => r,
        Err(e) => {
            log::warn!("grok quota fetch: request failed: {e}");
            return FetchResult::Transient;
        }
    };
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return FetchResult::Unauthorized;
    }
    if !status.is_success() {
        log::warn!("grok quota fetch: HTTP {status}");
        return FetchResult::Transient;
    }
    match resp.text() {
        Ok(text) => match map_grok_billing(&text, now) {
            Ok(snap) => FetchResult::Ok(snap),
            Err(e) => {
                log::warn!("grok quota fetch: failed to parse billing response: {e}");
                FetchResult::Transient
            }
        },
        Err(e) => {
            log::warn!("grok quota fetch: failed to read response body: {e}");
            FetchResult::Transient
        }
    }
}

/// Reads the plan label from the remote-settings endpoint.
///
/// `Ok(None)` means the settings were read but carry no display tier (so the
/// Plan line is simply omitted and nothing is retried); `Err` means the fetch
/// itself failed and is worth retrying.
fn fetch_grok_plan(
    client: &Client,
    token: &str,
    user_id: Option<&str>,
    settings_url: &str,
) -> Result<Option<String>> {
    let resp = grok_request(client, settings_url, token, user_id)
        .send()
        .context("Failed to send Grok settings request")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("grok settings returned status {status}");
    }
    let body = resp
        .text()
        .context("Failed to read Grok settings response body")?;
    let settings: GrokSettingsResponse =
        serde_json::from_str(&body).context("Failed to parse Grok settings response")?;
    Ok(settings
        .subscription_tier_display
        .filter(|s| !s.trim().is_empty()))
}

/// Resolves an issuer's OAuth token endpoint from its OIDC discovery document.
///
/// The Grok CLI never hardcodes the endpoint, so neither do we — a login against
/// a different issuer (team / enterprise) keeps working.
///
/// # Errors
///
/// Returns an error if the document cannot be fetched, is not successful, or
/// carries no `token_endpoint`.
fn discover_token_endpoint(client: &Client, issuer: &str) -> Result<String> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let resp = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, grok_ua())
        .send()
        .context("Failed to send Grok OIDC discovery request")?;
    let status = resp.status();
    if !status.is_success() {
        bail!("grok OIDC discovery returned status {status}");
    }
    let doc: serde_json::Value = resp
        .json()
        .context("Failed to parse Grok OIDC discovery document")?;
    doc.get("token_endpoint")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .context("grok OIDC discovery carried no token_endpoint")
}

/// Refreshes the Grok token and writes it back. Returns `(access, expires_secs)`.
///
/// The rotated access token replaces the entry's `key`, alongside the new
/// refresh token and a fresh `expires_at` in the CLI's own RFC3339 nanosecond
/// format. Every sibling entry and unknown field is preserved.
fn refresh_grok(
    client: &Client,
    path: &Path,
    cred: &GrokCredential,
    expected_mtime: Option<SystemTime>,
    token_url: &str,
) -> Result<(String, i64)> {
    let refresh_token = cred
        .refresh_token()
        .context("grok login carries no refresh token")?;
    let client_id = cred
        .client_id()
        .context("grok login carries no OAuth client id")?;

    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("client_id", client_id),
        ("refresh_token", refresh_token),
    ];
    // A team login re-sends its principal so the rotated token keeps that scope.
    if let (Some(kind), Some(id)) = (
        cred.entry
            .principal_type
            .as_deref()
            .filter(|s| !s.is_empty()),
        cred.entry.principal_id.as_deref().filter(|s| !s.is_empty()),
    ) {
        form.push(("principal_type", kind));
        form.push(("principal_id", id));
    }

    let req = client
        .post(token_url)
        .header(reqwest::header::USER_AGENT, grok_ua())
        .form(&form);
    let (status, text) = send_refresh(req)?;
    if !status.is_success() {
        bail!("grok token refresh returned status {status}");
    }
    let resp: GrokRefreshResponse =
        serde_json::from_str(&text).context("Failed to parse Grok refresh response")?;
    let access = resp
        .access_token
        .filter(|s| !s.is_empty())
        .context("grok refresh response had no access_token")?;
    let expires_at =
        chrono::Local::now().timestamp() + resp.expires_in.unwrap_or(GROK_DEFAULT_EXPIRES_IN);
    let stamp = rfc3339_utc_nanos(expires_at).context("refreshed grok expiry is out of range")?;

    let new_refresh = resp.refresh_token;
    let entry_key = cred.entry_key.as_str();
    let wrote = update_json_file_in_place(path, expected_mtime, |root| {
        let entry = root
            .get_mut(entry_key)
            .and_then(|v| v.as_object_mut())
            .context("auth.json no longer holds the login being refreshed")?;
        entry.insert("key".into(), json!(access));
        if let Some(r) = &new_refresh
            && !r.is_empty()
        {
            entry.insert("refresh_token".into(), json!(r));
        }
        entry.insert("expires_at".into(), json!(stamp));
        Ok(())
    })?;
    // The refresh already rotated the token server-side; failing to persist it
    // leaves `auth.json` holding a refresh token the server has invalidated, so
    // treat it as a refresh failure rather than caching an ephemeral token over
    // a broken login.
    if !wrote {
        bail!("grok token rotated but the new token could not be persisted");
    }
    Ok((access, expires_at))
}

/// Fetches the raw `billing?format=credits` response for `vct quota grok`.
///
/// Uses the stored access token verbatim (no refresh, no file writes) and
/// returns `(status_code, body)`. A non-2xx status is left for the caller to
/// surface — the body (often a JSON error) is still returned.
///
/// # Errors
///
/// Returns an error if `auth.json` is missing, holds no usable login, or the
/// request cannot be sent.
pub fn fetch_grok_raw(client: &Client) -> Result<(u16, String)> {
    fetch_grok_raw_from(client, GROK_BILLING_URL, &get_grok_auth_path()?)
}

/// The injectable core of [`fetch_grok_raw`]: reads the login from an explicit
/// `auth.json` path and fetches the raw billing body from an explicit
/// `billing_url`. Production passes [`GROK_BILLING_URL`] + `~/.grok/auth.json`;
/// tests point them at a local mock server and a temp auth file.
pub(crate) fn fetch_grok_raw_from(
    client: &Client,
    billing_url: &str,
    auth_path: &Path,
) -> Result<(u16, String)> {
    let body = std::fs::read_to_string(auth_path).with_context(|| {
        format!(
            "no Grok credentials at {} ({GROK_LOGIN_HINT})",
            auth_path.display()
        )
    })?;
    let cred = select_grok_entry(&body).with_context(|| {
        format!(
            "no usable Grok login in {} ({GROK_LOGIN_HINT})",
            auth_path.display()
        )
    })?;
    let resp = grok_request(client, billing_url, cred.access_token(), cred.user_id())
        .send()
        .context("Failed to send Grok billing request")?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .context("Failed to read Grok billing response body")?;
    Ok((status, text))
}

/// The result of obtaining a usable access token.
///
/// There is no transient arm: the credential file is read by the caller, so the
/// only way to get here without a token is an auth failure.
enum EnsureToken {
    Token(String),
    NeedsLogin,
}

/// Per-worker Grok state: an in-memory access token, the discovered token
/// endpoint, the plan label, and the refresh backoff.
#[derive(Default)]
pub struct GrokState {
    /// Cached token: `(access_token, expiry_secs, auth-file mtime)`. The expiry
    /// is `None` for an API-key login, which never expires. The mtime pins the
    /// cache to the file it came from, so a re-login invalidates it.
    token: Option<(String, Option<i64>, Option<SystemTime>)>,
    cooldown: RefreshCooldown,
    /// Discovered `(issuer, token_endpoint)`, so discovery runs once per issuer.
    token_endpoint: Option<(String, String)>,
    /// Plan label, fetched from the settings endpoint at most once per worker.
    plan: Option<String>,
    /// Whether the settings endpoint has answered at all; a failed fetch leaves
    /// this false so the next tick retries.
    plan_fetched: bool,
}

impl GrokState {
    /// One worker tick: ensure a token, fetch billing, refresh reactively on 401.
    pub fn resolve(&mut self, client: &Client) -> QuotaOutcome<GrokQuotaSnapshot> {
        let now = chrono::Local::now().timestamp();
        let path = match get_grok_auth_path() {
            Ok(p) => p,
            Err(_) => return QuotaOutcome::Transient,
        };
        let cred = match read_grok_credential(&path) {
            CredentialRead::Ok(cred) => *cred,
            CredentialRead::NeedsLogin => return QuotaOutcome::NeedsLogin,
            CredentialRead::Transient => return QuotaOutcome::Transient,
        };
        let user_id = cred.user_id().map(str::to_string);

        let token = match self.ensure_token(client, &path, &cred, now) {
            EnsureToken::Token(t) => t,
            EnsureToken::NeedsLogin => return QuotaOutcome::NeedsLogin,
        };

        match fetch_grok_billing(client, &token, user_id.as_deref(), now, GROK_BILLING_URL) {
            FetchResult::Ok(snap) => {
                self.cooldown.clear();
                QuotaOutcome::Data(self.with_plan(client, snap, &token, user_id.as_deref()))
            }
            FetchResult::Unauthorized => {
                let mtime = file_mtime(&path);
                if self.cooldown.active(now, mtime) {
                    self.token = None;
                    return QuotaOutcome::NeedsLogin;
                }
                match self.force_refresh(client, &path) {
                    Some(fresh) => {
                        match fetch_grok_billing(
                            client,
                            &fresh,
                            user_id.as_deref(),
                            now,
                            GROK_BILLING_URL,
                        ) {
                            FetchResult::Ok(snap) => {
                                self.cooldown.clear();
                                QuotaOutcome::Data(self.with_plan(
                                    client,
                                    snap,
                                    &fresh,
                                    user_id.as_deref(),
                                ))
                            }
                            // A transient retry error keeps the freshly
                            // refreshed token; do not nag to re-login.
                            FetchResult::Transient => QuotaOutcome::Transient,
                            // Arm with the CURRENT mtime: the refresh just
                            // rewrote the file, so a stale one would never
                            // suppress the next tick.
                            FetchResult::Unauthorized => {
                                self.cooldown.arm(now, file_mtime(&path));
                                self.token = None;
                                QuotaOutcome::NeedsLogin
                            }
                        }
                    }
                    None => {
                        self.cooldown.arm(now, file_mtime(&path));
                        self.token = None;
                        QuotaOutcome::NeedsLogin
                    }
                }
            }
            FetchResult::Transient => QuotaOutcome::Transient,
        }
    }

    /// Attaches the plan label, fetching it from the settings endpoint the first
    /// time it is needed. A failure only costs the Plan line.
    fn with_plan(
        &mut self,
        client: &Client,
        mut snap: GrokQuotaSnapshot,
        token: &str,
        user_id: Option<&str>,
    ) -> GrokQuotaSnapshot {
        if !self.plan_fetched {
            match fetch_grok_plan(client, token, user_id, GROK_SETTINGS_URL) {
                Ok(plan) => {
                    self.plan_fetched = true;
                    self.plan = plan;
                }
                Err(e) => log::warn!("grok quota: failed to read plan from settings: {e}"),
            }
        }
        snap.plan_type = self.plan.clone();
        snap
    }

    /// Returns a usable access token, refreshing proactively near expiry.
    fn ensure_token(
        &mut self,
        client: &Client,
        path: &Path,
        cred: &GrokCredential,
        now: i64,
    ) -> EnsureToken {
        // Cached token still valid AND the credential file unchanged since we
        // cached it? A re-login rewrites `auth.json`, so the cached token must
        // be dropped even while it is unexpired.
        if let Some((tok, expiry, cred_mtime)) = &self.token
            && !is_expiring(*expiry, now, EXPIRY_SKEW_SECS)
            && *cred_mtime == file_mtime(path)
        {
            return EnsureToken::Token(tok.clone());
        }

        let expires = cred.expires_at_secs();
        if !is_expiring(expires, now, EXPIRY_SKEW_SECS) {
            let tok = cred.access_token().to_string();
            self.token = Some((tok.clone(), expires, file_mtime(path)));
            return EnsureToken::Token(tok);
        }

        let mtime = file_mtime(path);
        if self.cooldown.active(now, mtime) {
            return EnsureToken::NeedsLogin;
        }
        match self.force_refresh(client, path) {
            Some(t) => EnsureToken::Token(t),
            None => {
                // Re-read: a partial write could have changed the mtime.
                self.cooldown.arm(now, file_mtime(path));
                EnsureToken::NeedsLogin
            }
        }
    }

    /// Re-reads the credential file and refreshes once.
    ///
    /// The mtime is captured before the read so the write-back guards on the
    /// exact file version whose refresh token we send — otherwise we could
    /// consume a token the CLI just wrote and then abort its write.
    fn force_refresh(&mut self, client: &Client, path: &Path) -> Option<String> {
        let expected_mtime = file_mtime(path);
        let body = std::fs::read_to_string(path).ok()?;
        let cred = select_grok_entry(&body)?;
        if !cred.is_refreshable() {
            log::warn!("grok token expired and this login cannot be refreshed");
            return None;
        }
        let issuer = cred.issuer()?.to_string();
        let token_url = self.token_endpoint(client, &issuer)?;
        match refresh_grok(client, path, &cred, expected_mtime, &token_url) {
            Ok((access, expires_at)) => {
                self.cooldown.clear();
                self.token = Some((access.clone(), Some(expires_at), file_mtime(path)));
                Some(access)
            }
            Err(e) => {
                log::warn!("grok token refresh failed: {e}");
                None
            }
        }
    }

    /// The issuer's token endpoint, discovered once and cached per issuer.
    fn token_endpoint(&mut self, client: &Client, issuer: &str) -> Option<String> {
        if let Some((cached_issuer, url)) = &self.token_endpoint
            && cached_issuer == issuer
        {
            return Some(url.clone());
        }
        match discover_token_endpoint(client, issuer) {
            Ok(url) => {
                self.token_endpoint = Some((issuer.to_string(), url.clone()));
                Some(url)
            }
            Err(e) => {
                log::warn!("grok OIDC discovery failed: {e}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::http::build_client;
    use httpmock::prelude::*;

    /// An `auth.json` with a single xAI OAuth login.
    fn oauth_auth(expires_at: &str) -> String {
        format!(
            r#"{{
              "https://auth.x.ai::client-1": {{
                "auth_mode": "oidc",
                "key": "access-token",
                "refresh_token": "refresh-token",
                "expires_at": "{expires_at}",
                "oidc_issuer": "https://auth.x.ai",
                "oidc_client_id": "client-1",
                "user_id": "user-1",
                "principal_type": "User",
                "principal_id": "principal-1",
                "email": "user@example.com"
              }}
            }}"#
        )
    }

    /// The body a real (free, unused) account returns, captured verbatim — note
    /// the omitted `creditUsagePercent` and the extra `topUpMethod` this
    /// fetcher does not read.
    const CREDITS: &str =
        include_str!("../../../../tests/fixtures/quota/grok_billing_credits_response.json");

    // ---- credential selection ----

    #[test]
    fn selects_the_only_oauth_login() {
        let cred = select_grok_entry(&oauth_auth("2036-01-01T00:00:00Z")).unwrap();
        assert_eq!(cred.entry_key, "https://auth.x.ai::client-1");
        assert_eq!(cred.access_token(), "access-token");
        assert_eq!(cred.user_id(), Some("user-1"));
        assert_eq!(cred.issuer(), Some("https://auth.x.ai"));
        assert_eq!(cred.client_id(), Some("client-1"));
        assert!(cred.is_refreshable());
    }

    #[test]
    fn prefers_a_refreshable_login_over_an_api_key() {
        // The API key sorts first alphabetically and never expires, so only the
        // refreshable-first rule keeps the subscription login winning.
        let body = r#"{
          "xai::api_key": { "key": "api-key-token" },
          "https://auth.x.ai::c": {
            "key": "oauth-token", "refresh_token": "rt",
            "oidc_issuer": "https://auth.x.ai", "oidc_client_id": "c",
            "expires_at": "2026-01-01T00:00:00Z"
          }
        }"#;
        let cred = select_grok_entry(body).unwrap();
        assert_eq!(cred.access_token(), "oauth-token");
    }

    #[test]
    fn api_key_login_is_usable_but_not_refreshable() {
        let cred = select_grok_entry(r#"{ "xai::api_key": { "key": "api-key-token" } }"#).unwrap();
        assert_eq!(cred.access_token(), "api-key-token");
        assert!(!cred.is_refreshable());
        // `xai` is not a URL, so it must never be mistaken for an issuer.
        assert_eq!(cred.issuer(), None);
        // No expiry recorded means the token does not expire.
        assert_eq!(cred.expires_at_secs(), None);
    }

    #[test]
    fn derives_issuer_and_client_id_from_the_entry_key() {
        // A legacy entry without the explicit oidc_* fields.
        let body =
            r#"{ "https://auth.x.ai::legacy-client": { "key": "t", "refresh_token": "rt" } }"#;
        let cred = select_grok_entry(body).unwrap();
        assert_eq!(cred.issuer(), Some("https://auth.x.ai"));
        assert_eq!(cred.client_id(), Some("legacy-client"));
        assert!(cred.is_refreshable());
    }

    #[test]
    fn picks_the_latest_expiry_among_equal_logins() {
        let body = r#"{
          "https://auth.x.ai::a": {
            "key": "older", "refresh_token": "rt",
            "oidc_issuer": "https://auth.x.ai", "oidc_client_id": "a",
            "expires_at": "2026-01-01T00:00:00Z"
          },
          "https://auth.x.ai::b": {
            "key": "newer", "refresh_token": "rt",
            "oidc_issuer": "https://auth.x.ai", "oidc_client_id": "b",
            "expires_at": "2027-01-01T00:00:00Z"
          }
        }"#;
        assert_eq!(select_grok_entry(body).unwrap().access_token(), "newer");
    }

    #[test]
    fn tokenless_or_malformed_entries_are_skipped() {
        // An empty file, a logged-out entry, and a garbage entry each yield no
        // usable login (which the worker maps to a login hint, not stale data).
        assert!(select_grok_entry("{}").is_none());
        assert!(select_grok_entry(r#"{ "xai::api_key": { "key": "" } }"#).is_none());
        assert!(select_grok_entry(r#"{ "a": 42 }"#).is_none());
        assert!(select_grok_entry("not json").is_none());
        // A broken sibling never hides a usable login.
        let mixed = r#"{ "broken": [1,2], "xai::api_key": { "key": "usable" } }"#;
        assert_eq!(select_grok_entry(mixed).unwrap().access_token(), "usable");
    }

    #[test]
    fn credential_debug_redacts_secrets() {
        let cred = select_grok_entry(&oauth_auth("2036-01-01T00:00:00Z")).unwrap();
        let s = format!("{:?}", cred.entry);
        assert!(!s.contains("access-token"));
        assert!(!s.contains("refresh-token"));
        assert!(!s.contains("user-1"));
        assert!(!s.contains("principal-1"));
        assert!(s.contains("<redacted>"));
        // Non-secret fields stay visible.
        assert!(s.contains("https://auth.x.ai"));
    }

    // ---- billing mapping ----

    #[test]
    fn maps_an_idle_weekly_account() {
        let snap = map_grok_billing(CREDITS, 1_000_000).unwrap();
        assert_eq!(snap.source, QuotaSource::Api);
        // `creditUsagePercent` is omitted at zero — that means 0%, not an error.
        let included = snap.included.as_ref().unwrap();
        assert_eq!(included.used_percent, 0.0);
        assert!(included.resets_at_unix.unwrap() > 0);
        assert_eq!(snap.period_label.as_deref(), Some("week"));
        // Zero money is not rendered.
        assert!(snap.on_demand_dollars.is_none());
        assert!(snap.on_demand_cap_dollars.is_none());
        assert!(snap.prepaid_balance_dollars.is_none());
        assert!(!snap.limit_reached);
        assert!(!snap.needs_login);
    }

    #[test]
    fn maps_money_from_cents_and_labels_a_monthly_period() {
        let body = r#"{ "config": {
          "creditUsagePercent": 42.5,
          "currentPeriod": { "type": "USAGE_PERIOD_TYPE_MONTHLY", "end": "2026-08-01T00:00:00+00:00" },
          "onDemandCap": { "val": 5000 },
          "onDemandUsed": { "val": 1840 },
          "prepaidBalance": { "val": 250 }
        } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert_eq!(snap.included.as_ref().unwrap().used_percent, 42.5);
        assert_eq!(snap.period_label.as_deref(), Some("month"));
        assert_eq!(snap.on_demand_dollars, Some(18.40));
        assert_eq!(snap.on_demand_cap_dollars, Some(50.0));
        assert_eq!(snap.prepaid_balance_dollars, Some(2.50));
    }

    #[test]
    fn an_untouched_cap_still_reports_zero_spend() {
        // With a cap configured the on-demand line is meaningful even at $0, so
        // it is shown rather than hidden as it is on a cap-less account.
        let body = r#"{ "config": { "onDemandCap": { "val": 5000 }, "onDemandUsed": {} } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert_eq!(snap.on_demand_cap_dollars, Some(50.0));
        assert_eq!(snap.on_demand_dollars, Some(0.0));
    }

    #[test]
    fn omitted_zero_money_objects_are_tolerated() {
        // proto3 renders a $0 amount as `{}`; that must read as zero, not fail.
        let body =
            r#"{ "config": { "onDemandCap": {}, "onDemandUsed": {}, "prepaidBalance": {} } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert!(snap.on_demand_dollars.is_none());
        assert!(snap.prepaid_balance_dollars.is_none());
        assert_eq!(snap.included.as_ref().unwrap().used_percent, 0.0);
    }

    #[test]
    fn falls_back_to_the_legacy_period_end() {
        let body = r#"{ "config": { "billingPeriodEnd": "2026-08-01T00:00:00+00:00" } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert!(snap.included.as_ref().unwrap().resets_at_unix.unwrap() > 0);
        // An unknown period type leaves the gauge unlabelled rather than lying.
        assert!(snap.period_label.is_none());
    }

    #[test]
    fn flags_limit_and_clamps_out_of_range_percentages() {
        let snap = map_grok_billing(r#"{ "config": { "creditUsagePercent": 150 } }"#, 1).unwrap();
        assert_eq!(snap.included.as_ref().unwrap().used_percent, 100.0);
        assert!(snap.limit_reached);
        let snap = map_grok_billing(r#"{ "config": { "creditUsagePercent": -5 } }"#, 1).unwrap();
        assert_eq!(snap.included.as_ref().unwrap().used_percent, 0.0);
        assert!(!snap.limit_reached);
    }

    #[test]
    fn a_null_percent_reads_as_zero_rather_than_failing() {
        let body = r#"{ "config": { "creditUsagePercent": null, "currentPeriod": null } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert_eq!(snap.included.as_ref().unwrap().used_percent, 0.0);
        assert!(snap.included.as_ref().unwrap().resets_at_unix.is_none());
    }

    #[test]
    fn a_body_without_config_is_an_error() {
        // An error payload must not map to an all-zero "everything is fine" panel.
        assert!(map_grok_billing(r#"{ "error": "Invalid credentials" }"#, 1).is_err());
        assert!(map_grok_billing("not json", 1).is_err());
    }

    #[test]
    fn labels_only_the_known_period_types() {
        assert_eq!(period_label("USAGE_PERIOD_TYPE_WEEKLY"), Some("week"));
        assert_eq!(period_label("USAGE_PERIOD_TYPE_MONTHLY"), Some("month"));
        assert_eq!(period_label("USAGE_PERIOD_TYPE_UNSPECIFIED"), None);
    }

    // ---- HTTP-layer tests against a local mock server (no real API) ----

    #[test]
    fn fetch_grok_billing_maps_200_and_401() {
        let server = MockServer::start();
        let ok = server.mock(|when, then| {
            when.method(GET)
                .path("/billing")
                .header("authorization", "Bearer tok")
                .header("x-xai-token-auth", "xai-grok-cli")
                .header("x-userid", "user-1");
            then.status(200).body(CREDITS);
        });
        server.mock(|when, then| {
            when.method(GET).path("/rejected");
            then.status(401);
        });
        let client = build_client().unwrap();

        match fetch_grok_billing(
            &client,
            "tok",
            Some("user-1"),
            1_000_000,
            &server.url("/billing"),
        ) {
            FetchResult::Ok(snap) => {
                assert_eq!(snap.period_label.as_deref(), Some("week"));
                assert_eq!(snap.included.as_ref().unwrap().used_percent, 0.0);
            }
            _ => panic!("expected FetchResult::Ok"),
        }
        ok.assert();
        assert!(matches!(
            fetch_grok_billing(&client, "tok", None, 0, &server.url("/rejected")),
            FetchResult::Unauthorized
        ));
    }

    #[test]
    fn fetch_grok_plan_reads_the_display_tier() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/settings");
            then.status(200)
                .body(r#"{"subscription_tier_display":"SuperGrok","on_demand_enabled":null}"#);
        });
        server.mock(|when, then| {
            when.method(GET).path("/tierless");
            then.status(200).body(r#"{"telemetry_enabled":true}"#);
        });
        server.mock(|when, then| {
            when.method(GET).path("/down");
            then.status(503);
        });
        let client = build_client().unwrap();

        assert_eq!(
            fetch_grok_plan(&client, "tok", None, &server.url("/settings")).unwrap(),
            Some("SuperGrok".to_string())
        );
        // Settings answered but carry no tier: no Plan line, and nothing to retry.
        assert_eq!(
            fetch_grok_plan(&client, "tok", None, &server.url("/tierless")).unwrap(),
            None
        );
        // A failed fetch is an error so the worker retries it later.
        assert!(fetch_grok_plan(&client, "tok", None, &server.url("/down")).is_err());
    }

    #[test]
    fn discovers_the_token_endpoint_from_the_issuer() {
        let server = MockServer::start();
        let discovery = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200).json_body(serde_json::json!({
                "issuer": "https://auth.x.ai",
                "token_endpoint": "https://auth.x.ai/oauth2/token"
            }));
        });
        let client = build_client().unwrap();

        let url = discover_token_endpoint(&client, &server.base_url()).unwrap();
        discovery.assert();
        assert_eq!(url, "https://auth.x.ai/oauth2/token");
        // A trailing slash on the issuer must not double up the path separator.
        assert!(discover_token_endpoint(&client, &format!("{}/", server.base_url())).is_ok());
    }

    #[test]
    fn discovery_without_a_token_endpoint_is_an_error() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200)
                .json_body(serde_json::json!({ "issuer": "x" }));
        });
        let client = build_client().unwrap();
        assert!(discover_token_endpoint(&client, &server.base_url()).is_err());
    }

    #[test]
    fn refresh_writes_the_rotated_token_back_into_its_entry() {
        let server = MockServer::start();
        let token = server.mock(|when, then| {
            when.method(POST)
                .path("/token")
                .body_includes("grant_type=refresh_token")
                .body_includes("client_id=client-1")
                .body_includes("refresh_token=refresh-token")
                // A team login re-sends its principal.
                .body_includes("principal_type=User");
            then.status(200).json_body(serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "fresh-refresh",
                "expires_in": 3600
            }));
        });

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        // A sibling login that must survive the write untouched.
        let mut root: serde_json::Value =
            serde_json::from_str(&oauth_auth("2020-01-01T00:00:00Z")).unwrap();
        root["xai::api_key"] = serde_json::json!({ "key": "sibling" });
        let body = root.to_string();
        std::fs::write(&path, &body).unwrap();

        let client = build_client().unwrap();
        let cred = select_grok_entry(&body).unwrap();
        let (access, expires_at) = refresh_grok(
            &client,
            &path,
            &cred,
            file_mtime(&path),
            &server.url("/token"),
        )
        .expect("refresh should succeed");

        token.assert();
        assert_eq!(access, "fresh-access");
        assert!(expires_at > chrono::Local::now().timestamp());

        let written: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let entry = &written["https://auth.x.ai::client-1"];
        assert_eq!(entry["key"], "fresh-access");
        assert_eq!(entry["refresh_token"], "fresh-refresh");
        // The expiry keeps the CLI's RFC3339 nanosecond string shape.
        let stamp = entry["expires_at"].as_str().unwrap();
        assert!(stamp.ends_with('Z') && stamp.contains('.'));
        assert!(chrono::DateTime::parse_from_rfc3339(stamp).is_ok());
        // Untouched fields and the sibling login survive.
        assert_eq!(entry["email"], "user@example.com");
        assert_eq!(written["xai::api_key"]["key"], "sibling");
    }

    #[test]
    fn a_rejected_refresh_never_writes_the_file() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(400)
                .json_body(serde_json::json!({ "error": "invalid_grant" }));
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let body = oauth_auth("2020-01-01T00:00:00Z");
        std::fs::write(&path, &body).unwrap();

        let client = build_client().unwrap();
        let cred = select_grok_entry(&body).unwrap();
        assert!(
            refresh_grok(
                &client,
                &path,
                &cred,
                file_mtime(&path),
                &server.url("/token")
            )
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }

    #[test]
    fn a_concurrent_rewrite_aborts_the_write_back() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/token");
            then.status(200).json_body(serde_json::json!({
                "access_token": "fresh-access",
                "refresh_token": "fresh-refresh"
            }));
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let body = oauth_auth("2020-01-01T00:00:00Z");
        std::fs::write(&path, &body).unwrap();

        let client = build_client().unwrap();
        let cred = select_grok_entry(&body).unwrap();
        // A stale expected mtime stands in for the official CLI having rotated
        // the token first: the write aborts and the refresh reports failure
        // rather than caching a token whose rotation could not be persisted.
        let result = refresh_grok(
            &client,
            &path,
            &cred,
            Some(SystemTime::UNIX_EPOCH),
            &server.url("/token"),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
    }

    #[test]
    fn fetch_grok_raw_from_reads_the_login_and_returns_the_body() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(GET).path("/billing");
            then.status(200).body(CREDITS);
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(&path, oauth_auth("2036-01-01T00:00:00Z")).unwrap();

        let client = build_client().unwrap();
        let (status, body) =
            fetch_grok_raw_from(&client, &server.url("/billing"), &path).expect("raw fetch");
        assert_eq!(status, 200);
        assert!(body.contains("currentPeriod"));
    }

    #[test]
    fn fetch_grok_raw_from_reports_a_missing_or_empty_credential_file() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("auth.json");
        let client = build_client().unwrap();
        assert!(fetch_grok_raw_from(&client, "http://127.0.0.1:1/x", &missing).is_err());

        std::fs::write(&missing, "{}").unwrap();
        let err = fetch_grok_raw_from(&client, "http://127.0.0.1:1/x", &missing).unwrap_err();
        assert!(format!("{err}").contains(GROK_LOGIN_HINT));
    }

    // ---- worker state ----

    #[test]
    fn unreadable_credentials_stay_transient_while_a_cleared_file_needs_login() {
        // An absent file is transient (the CLI rewrites it atomically), so the
        // panel keeps its last-known-good rather than nagging; a readable file
        // with no usable login is an auth failure worth a login hint.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        assert!(matches!(
            read_grok_credential(&path),
            CredentialRead::Transient
        ));

        std::fs::write(&path, "{}").unwrap();
        assert!(matches!(
            read_grok_credential(&path),
            CredentialRead::NeedsLogin
        ));
        std::fs::write(&path, "{ not valid json").unwrap();
        assert!(matches!(
            read_grok_credential(&path),
            CredentialRead::NeedsLogin
        ));

        std::fs::write(&path, oauth_auth("2036-01-01T00:00:00Z")).unwrap();
        assert!(matches!(read_grok_credential(&path), CredentialRead::Ok(_)));
    }

    #[test]
    fn a_live_token_is_used_as_is_and_a_dead_one_asks_for_login() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.json");
        let client = build_client().unwrap();
        let mut state = GrokState::default();
        let cred = select_grok_entry(&oauth_auth("2036-01-01T00:00:00Z")).unwrap();

        // An unexpired token is used as-is: no refresh, no network.
        assert!(matches!(
            state.ensure_token(&client, &missing, &cred, 1_000_000),
            EnsureToken::Token(t) if t == "access-token"
        ));

        // An expired API-key login has nothing to refresh with, so the worker
        // asks for a login instead of sitting on a dead token.
        let expired = select_grok_entry(
            r#"{ "xai::api_key": { "key": "t", "expires_at": "2020-01-01T00:00:00Z" } }"#,
        )
        .unwrap();
        let mut state = GrokState::default();
        assert!(matches!(
            state.ensure_token(&client, &missing, &expired, 1_900_000_000),
            EnsureToken::NeedsLogin
        ));
    }

    #[test]
    fn a_cached_token_is_dropped_when_the_credential_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(&path, oauth_auth("2036-01-01T00:00:00Z")).unwrap();
        let client = build_client().unwrap();
        let mut state = GrokState::default();

        let cred = select_grok_entry(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(matches!(
            state.ensure_token(&client, &path, &cred, 1_000_000),
            EnsureToken::Token(_)
        ));
        assert!(state.token.is_some());

        // A re-login rewrites the file; the cached token must not survive it.
        let relogin = oauth_auth("2036-01-01T00:00:00Z").replace("access-token", "second-account");
        std::fs::write(&path, &relogin).unwrap();
        let cred = select_grok_entry(&relogin).unwrap();
        assert!(matches!(
            state.ensure_token(&client, &path, &cred, 1_000_100),
            EnsureToken::Token(t) if t == "second-account"
        ));
    }

    #[test]
    fn the_token_endpoint_is_discovered_once_per_issuer() {
        let server = MockServer::start();
        let discovery = server.mock(|when, then| {
            when.method(GET).path("/.well-known/openid-configuration");
            then.status(200).json_body(
                serde_json::json!({ "token_endpoint": "https://auth.x.ai/oauth2/token" }),
            );
        });
        let client = build_client().unwrap();
        let mut state = GrokState::default();

        let first = state.token_endpoint(&client, &server.base_url());
        let second = state.token_endpoint(&client, &server.base_url());
        assert_eq!(first, second);
        assert_eq!(discovery.calls(), 1, "discovery is cached per issuer");
    }
}
