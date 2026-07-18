//! `vct quota <provider>`: one-shot raw quota-API fetch.
//!
//! Reads the provider's stored credentials (no token refresh — an expired token
//! just 401s and the user re-auths via that provider's own CLI), sends a single
//! request impersonating that provider's CLI, and prints the raw response body
//! in the chosen format (pretty JSON by default). A non-2xx status still prints
//! the body, then exits non-zero with the provider's login hint.

use super::{CLAUDE_LOGIN_HINT, CODEX_LOGIN_HINT, COPILOT_LOGIN_HINT, CURSOR_LOGIN_HINT, http};
use crate::cli::QuotaProvider;
use crate::display::quota::{display_quota_table, display_quota_text, print_quota_json};
use anyhow::{Result, bail};

/// Runs `vct quota <provider>`: fetch the raw body and render it.
///
/// `text` / `table` pick the output format; neither set means pretty JSON.
///
/// # Errors
///
/// Returns an error if credentials are missing, the request fails, or the API
/// answers a non-2xx status (the body is still printed first).
pub fn run(provider: QuotaProvider, text: bool, table: bool) -> Result<()> {
    let (status, body) = fetch_raw(provider)?;

    if text {
        display_quota_text(&body);
    } else if table {
        display_quota_table(&body);
    } else {
        print_quota_json(&body);
    }

    if !(200..300).contains(&status) {
        // A rejected token (401/403) is the one case a re-login fixes, so only
        // then append the provider's login hint; other statuses (429, 5xx, ...)
        // just report the code.
        if status == 401 || status == 403 {
            bail!(
                "HTTP {status} from {} ({})",
                provider_name(provider),
                login_hint(provider)
            );
        }
        bail!("HTTP {status} from {}", provider_name(provider));
    }
    Ok(())
}

/// Dispatches to the provider's raw fetcher over the shared HTTP client.
fn fetch_raw(provider: QuotaProvider) -> Result<(u16, String)> {
    let client = http::build_client()?;
    match provider {
        QuotaProvider::Claude => super::claude::fetch_claude_raw(&client),
        QuotaProvider::Codex => super::wham::fetch_codex_raw(&client),
        QuotaProvider::Copilot => super::copilot::fetch_copilot_raw(&client),
        QuotaProvider::Cursor => super::cursor::fetch_cursor_raw(&client),
    }
}

/// The provider's lowercase name, for error messages.
fn provider_name(provider: QuotaProvider) -> &'static str {
    match provider {
        QuotaProvider::Claude => "claude",
        QuotaProvider::Codex => "codex",
        QuotaProvider::Copilot => "copilot",
        QuotaProvider::Cursor => "cursor",
    }
}

/// The provider's `run: <cli> login` hint.
fn login_hint(provider: QuotaProvider) -> &'static str {
    match provider {
        QuotaProvider::Claude => CLAUDE_LOGIN_HINT,
        QuotaProvider::Codex => CODEX_LOGIN_HINT,
        QuotaProvider::Copilot => COPILOT_LOGIN_HINT,
        QuotaProvider::Cursor => CURSOR_LOGIN_HINT,
    }
}
