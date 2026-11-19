//! Grok CLI quota fetcher.
//!
//! Reads the bearer token from each entry's `key` field in `~/.grok/auth.json` and queries `GET /v1/billing?format=credits` with the Grok CLI's `User-Agent` and `X-XAI-Token-Auth` headers. The plan label comes from `/v1/settings` and is cached per `auth.json` modification time.
//!
//! OAuth tokens are refreshed proactively near expiry and reactively on HTTP 401/403 against the token endpoint discovered from the entry's OIDC issuer. Rotated tokens are written back into the matching `auth.json` entry while preserving all other fields and entries, gated by [`crate::quota::refresh::RefreshCooldown`].

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
/// Fallback Grok CLI version for `User-Agent` and `x-grok-client-version` when `grok --version` cannot be resolved.
const GROK_FALLBACK_VERSION: &str = "0.2.112";
/// Required `X-XAI-Token-Auth` header value instructing the proxy to validate the bearer as a CLI session.
const GROK_TOKEN_AUTH: &str = "xai-grok-cli";
/// Access-token lifetime assumed when a refresh response omits `expires_in`.
const GROK_DEFAULT_EXPIRES_IN: i64 = 6 * 3600;
/// Login hint shown when the Grok token is expired / rejected.
pub const GROK_LOGIN_HINT: &str = "run: grok login";

/// Returns the Grok CLI version string, resolved once per process.
fn grok_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION
        .get_or_init(|| detect_cli_version("grok", "grok", GROK_FALLBACK_VERSION))
        .as_str()
}

/// Returns the cached `grok-pager/<v> grok-shell/<v> (<os>; <arch>)` `User-Agent` string.
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

/// Parsed `auth.json` login entry paired with its object key for in-place write-back.
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

    /// Returns the HTTP(S) OIDC issuer URL from `oidc_issuer` or the prefix before `::` in `entry_key`.
    fn issuer(&self) -> Option<&str> {
        self.entry
            .oidc_issuer
            .as_deref()
            .or_else(|| self.entry_key.split("::").next())
            .filter(|s| s.starts_with("http"))
    }

    /// Returns the OAuth client ID from `oidc_client_id` or the suffix after `::` in `entry_key`.
    fn client_id(&self) -> Option<&str> {
        self.entry
            .oidc_client_id
            .as_deref()
            .or_else(|| self.entry_key.rsplit("::").next())
            .filter(|s| !s.is_empty())
    }

    /// Returns the access-token expiry in Unix seconds, or `None` if absent or unparseable.
    fn expires_at_secs(&self) -> Option<i64> {
        let ms = parse_iso_timestamp(self.entry.expires_at.as_deref()?);
        (ms > 0).then_some(ms / 1000)
    }

    /// Whether this login carries everything a refresh needs.
    fn is_refreshable(&self) -> bool {
        self.refresh_token().is_some() && self.issuer().is_some() && self.client_id().is_some()
    }
}

/// Selects a usable login from `auth.json`, preferring refreshable OIDC logins, then the latest expiry, then the lexicographically smallest key; skips malformed entries.
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

/// Outcome of reading a login from `auth.json`.
enum CredentialRead {
    // Boxed to keep the enum compact across variants.
    Ok(Box<GrokCredential>),
    /// The file is readable but holds no usable login; requires re-login.
    NeedsLogin,
    /// The file is missing or unreadable (e.g. concurrent atomic rewrite); retain the last snapshot.
    Transient,
}

/// Reads `auth.json` and selects a login, distinguishing unreadable files (`Transient`) from missing/invalid credentials (`NeedsLogin`).
fn read_grok_credential(path: &Path) -> CredentialRead {
    let Ok(body) = std::fs::read_to_string(path) else {
        return CredentialRead::Transient;
    };
    match select_grok_entry(&body) {
        Some(cred) => CredentialRead::Ok(Box::new(cred)),
        None => CredentialRead::NeedsLogin,
    }
}

/// Returns a `GET` request for `url` with bearer auth, `X-XAI-Token-Auth`, optional `x-userid`, and Grok CLI identity headers.
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

/// Maps a `billing?format=credits` JSON response into a [`GrokQuotaSnapshot`] stamped at `now` (Unix seconds).
///
/// Absent proto3 numeric fields default to zero; money values are converted from cents to dollars.
///
/// # Errors
///
/// Returns an error if `body` is not valid JSON or omits the `config` object.
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
    // `credit_usage_percent` is already percent consumed; clamp only as an out-of-range guard.
    let used_percent = config.credit_usage_percent.clamp(0.0, 100.0);

    let cap = config.on_demand_cap.as_ref().map(|m| m.as_dollars());
    let used = config.on_demand_used.as_ref().map(|m| m.as_dollars());
    let balance = config.prepaid_balance.as_ref().map(|m| m.as_dollars());
    // Omit zero pay-as-you-go and prepaid fields unless an on-demand cap is configured, in which case proto3 omits zero `onDemandUsed` and it should surface as $0.
    let on_demand_cap_dollars = cap.filter(|c| *c > 0.0);
    let on_demand_dollars = if on_demand_cap_dollars.is_some() {
        Some(used.unwrap_or(0.0))
    } else {
        used.filter(|u| *u > 0.0)
    };

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
    /// HTTP 401 or 403; the token was rejected and should be refreshed.
    Unauthorized,
    /// Network, parse, or other non-2xx failure; retain the last snapshot.
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

/// Fetches the plan display tier from `settings_url`, returning `Ok(None)` when the account reports no tier and `Err` on transport/HTTP/parse failure.
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

/// Returns `token_endpoint` from `issuer`'s `/.well-known/openid-configuration` document.
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

/// Exchanges `cred`'s refresh token at `token_url`, persists the rotated credentials into `path` if its mtime still matches `expected_mtime`, and returns `(access_token, expires_at_secs)`.
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
    // Fail when write-back aborts because the server already invalidated the old refresh token, avoiding an ephemeral cached token over a broken file.
    if !wrote {
        bail!("grok token rotated but the new token could not be persisted");
    }
    Ok((access, expires_at))
}

/// Returns `(status_code, body)` from `/v1/billing?format=credits` using the stored access token.
///
/// Does not refresh the token or modify `auth.json`. Non-2xx responses are returned as `Ok((status_code, body))`.
///
/// # Errors
///
/// Returns an error if the auth path cannot be resolved, `auth.json` is missing or holds no usable login, or the HTTP request or body read fails.
pub fn fetch_grok_raw(client: &Client) -> Result<(u16, String)> {
    fetch_grok_raw_from(client, GROK_BILLING_URL, &get_grok_auth_path()?)
}

/// Returns `(status_code, body)` from `billing_url` using credentials read from `auth_path`.
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

/// Cached plan label from `/v1/settings`, invalidated when `auth.json`'s mtime changes.
struct CachedPlan {
    /// Display tier label, or `None` when the account reports none.
    label: Option<String>,
    /// `auth.json` mtime when `label` was fetched.
    mtime: Option<SystemTime>,
}

/// Outcome of obtaining a valid access token from a parsed credential.
enum EnsureToken {
    Token(String),
    NeedsLogin,
}

/// Per-worker Grok quota state holding the cached access token, discovered OIDC token endpoint, cached plan label, and refresh backoff.
#[derive(Default)]
pub struct GrokState {
    /// `(access_token, expiry_secs, auth_mtime)`; `None` expiry means non-expiring API key.
    token: Option<(String, Option<i64>, Option<SystemTime>)>,
    cooldown: RefreshCooldown,
    /// Cached `(issuer, token_endpoint)` pair.
    token_endpoint: Option<(String, String)>,
    /// Cached plan label per `auth.json` mtime; populated only after a successful settings fetch.
    plan: Option<CachedPlan>,
}

impl GrokState {
    /// Fetches a Grok quota snapshot, refreshing the OAuth token proactively near expiry and reactively on HTTP 401/403.
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
                QuotaOutcome::Data(self.with_plan(
                    client,
                    snap,
                    &token,
                    user_id.as_deref(),
                    file_mtime(&path),
                    GROK_SETTINGS_URL,
                ))
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
                                    file_mtime(&path),
                                    GROK_SETTINGS_URL,
                                ))
                            }
                            // Keep the freshly refreshed token on a transient retry failure instead of prompting for login.
                            FetchResult::Transient => QuotaOutcome::Transient,
                            // Arm with the post-refresh mtime because the refresh just rewrote the file; a stale mtime would not suppress the next tick.
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

    /// Attaches the cached plan label to `snap`, fetching it from `settings_url` when `cred_mtime` changes and leaving `plan_type` unset on failure.
    fn with_plan(
        &mut self,
        client: &Client,
        mut snap: GrokQuotaSnapshot,
        token: &str,
        user_id: Option<&str>,
        cred_mtime: Option<SystemTime>,
        settings_url: &str,
    ) -> GrokQuotaSnapshot {
        if self
            .plan
            .as_ref()
            .is_none_or(|cached| cached.mtime != cred_mtime)
        {
            // Clear the previous account's label before fetching so a failed request does not display a stale tier.
            self.plan = None;
            match fetch_grok_plan(client, token, user_id, settings_url) {
                Ok(label) => {
                    self.plan = Some(CachedPlan {
                        label,
                        mtime: cred_mtime,
                    })
                }
                Err(e) => log::warn!("grok quota: failed to read plan from settings: {e}"),
            }
        }
        snap.plan_type = self.plan.as_ref().and_then(|cached| cached.label.clone());
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
        // Invalidate the cached token when `auth.json` changes on disk (e.g. re-login), even if it is not yet expired.
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

    /// Re-reads `path` and refreshes the OAuth token once, aborting write-back if `path` changes concurrently.
    fn force_refresh(&mut self, client: &Client, path: &Path) -> Option<String> {
        let expected_mtime = file_mtime(path);
        let body = std::fs::read_to_string(path).ok()?;
        let cred = select_grok_entry(&body)?;
        // API-key logins have no OIDC issuer and cannot be refreshed.
        let Some(issuer) = cred.issuer().map(str::to_string) else {
            log::warn!("grok token expired and this login cannot be refreshed");
            return None;
        };
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

    /// Returns the OIDC token endpoint for `issuer`, discovering and caching it on first use.
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

    /// Captured billing response from an unused free account, with `creditUsagePercent` omitted and an unused `topUpMethod` field.
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
        // The API key sorts first and has a later expiry, so only the refreshable-first rule selects the OAuth login.
        let body = r#"{
          "xai::api_key": { "key": "api-key-token", "expires_at": "2030-01-01T00:00:00Z" },
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
        assert_eq!(cred.expires_at_secs(), None);
    }

    #[test]
    fn derives_issuer_and_client_id_from_the_entry_key() {
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
        // Drives the panel's staleness line, so a wrong stamp reads as "updated 56 years ago" on every otherwise-healthy tick.
        assert_eq!(snap.fetched_at, 1_000_000);
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
        // With a cap configured, $0 spend is preserved whether `onDemandUsed` is `{}` or omitted.
        let body = r#"{ "config": { "onDemandCap": { "val": 5000 }, "onDemandUsed": {} } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert_eq!(snap.on_demand_cap_dollars, Some(50.0));
        assert_eq!(snap.on_demand_dollars, Some(0.0));

        let body = r#"{ "config": { "onDemandCap": { "val": 5000 } } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert_eq!(snap.on_demand_dollars, Some(0.0));
    }

    #[test]
    fn reads_the_nanosecond_expiry_the_grok_cli_writes() {
        // Real `expires_at` values carry 9 fractional digits; failing to parse them would silently disable proactive refresh.
        let cred = select_grok_entry(&oauth_auth("2026-07-26T15:33:55.311478864Z")).unwrap();
        assert_eq!(cred.expires_at_secs(), Some(1_785_080_035));
        // An unparseable stamp reads as unknown, not as the epoch.
        let cred = select_grok_entry(&oauth_auth("not-a-timestamp")).unwrap();
        assert_eq!(cred.expires_at_secs(), None);
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
        assert!(snap.period_label.is_none());
    }

    #[test]
    fn the_current_period_outranks_the_deprecated_one() {
        // Older servers emit both fields; `currentPeriod` is authoritative over deprecated `billingPeriodEnd`.
        let body = r#"{ "config": {
          "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY", "end": "2026-07-29T00:00:00+00:00" },
          "billingPeriodEnd": "2026-01-01T00:00:00+00:00"
        } }"#;
        let snap = map_grok_billing(body, 1).unwrap();
        assert_eq!(
            snap.included.unwrap().resets_at_unix,
            iso_to_unix_secs("2026-07-29T00:00:00+00:00")
        );
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
    fn a_relogin_drops_the_plan_label_even_when_the_refetch_fails() {
        let server = MockServer::start();
        let settings = server.mock(|when, then| {
            when.method(GET).path("/settings");
            then.status(200)
                .body(r#"{"subscription_tier_display":"SuperGrok"}"#);
        });
        server.mock(|when, then| {
            when.method(GET).path("/down");
            then.status(503);
        });
        let client = build_client().unwrap();
        let mut state = GrokState::default();
        let first_login = Some(SystemTime::UNIX_EPOCH);
        let second_login = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1));
        let snap = || map_grok_billing(CREDITS, 1_000_000).unwrap();

        let labelled = state.with_plan(
            &client,
            snap(),
            "tok",
            None,
            first_login,
            &server.url("/settings"),
        );
        assert_eq!(labelled.plan_type.as_deref(), Some("SuperGrok"));

        let cached = state.with_plan(
            &client,
            snap(),
            "tok",
            None,
            first_login,
            &server.url("/settings"),
        );
        assert_eq!(cached.plan_type.as_deref(), Some("SuperGrok"));
        assert_eq!(settings.calls(), 1, "the label is cached per login");

        let after_relogin = state.with_plan(
            &client,
            snap(),
            "tok",
            None,
            second_login,
            &server.url("/down"),
        );
        assert_eq!(after_relogin.plan_type, None);
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
        // A stale expected mtime simulates a concurrent rotation by the official CLI; write-back aborts and refresh fails.
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
        // An absent file can occur during an atomic rewrite by the CLI; a readable empty/invalid file means logged out.
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

        assert!(matches!(
            state.ensure_token(&client, &missing, &cred, 1_000_000),
            EnsureToken::Token(t) if t == "access-token"
        ));

        // Write the file to disk so `force_refresh` reaches the unrefreshable-login guard rather than bailing on `read_to_string`.
        let api_key = r#"{ "xai::api_key": { "key": "t", "expires_at": "2020-01-01T00:00:00Z" } }"#;
        let path = dir.path().join("auth.json");
        std::fs::write(&path, api_key).unwrap();
        let expired = select_grok_entry(api_key).unwrap();
        let mut state = GrokState::default();
        assert!(matches!(
            state.ensure_token(&client, &path, &expired, 1_900_000_000),
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
