# Quota + token refresh (raw curl/jq examples)

Raw, copy-pastable `curl` + `jq` recipes for reading each provider's quota usage and refreshing its OAuth token, straight from the credential files the official CLIs already manage on disk. No browser, no headless webview, so they work over plain SSH.

These are minimal examples: they stop at "dump the JSON response". Field extraction / mapping is left out on purpose.

> Most of these hit internal / undocumented endpoints (the same ones the official clients use) and require the credential file to already be logged in.
> Refresh calls **rotate** the token and must be written back to the file, so do not run them casually while the official CLI is live.

## At a glance

| Provider    | Credential file                                     | Quota endpoint                                                         | Refresh model                                    |
| ----------- | --------------------------------------------------- | ---------------------------------------------------------------------- | ------------------------------------------------ |
| Codex       | `~/.codex/auth.json`                                | `GET  chatgpt.com/backend-api/wham/usage` + reset-credit details       | `POST auth.openai.com/oauth/token` (rotates)     |
| Claude Code | `~/.claude/.credentials.json`                       | `GET  api.anthropic.com/api/oauth/usage`                               | `POST platform.claude.com/v1/oauth/token`        |
| Copilot     | `~/.copilot/config.json`                            | `GET  api.github.com/copilot_internal/user`                            | none (long-lived `gho_` token)                   |
| Cursor      | `~/.config/cursor/auth.json`                        | `GET  cursor.com/api/usage-summary`                                    | reactive (official CLI keeps the file fresh)     |
| Antigravity | `~/.gemini/antigravity-cli/antigravity-oauth-token` | `POST cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary` | `POST oauth2.googleapis.com/token` (no rotation) |
| Grok        | `~/.grok/auth.json`                                 | `GET  cli-chat-proxy.grok.com/v1/billing?format=credits`               | `POST auth.x.ai/oauth2/token` (rotates)          |

## Client impersonation

Make each request look like the official client the **token belongs to** (a Claude Code token should look like Claude Code).
These headers are camouflage, not auth: the endpoints answer a bare bearer token.
Cadence matters more than any header, so poll slowly and back off on `429`.
Codex does not strictly need it (OpenAI treats codex-cli traffic as sanctioned), but the example still sends a codex-shaped UA + originator for parity.
Send only stable identity headers, never per-request/per-turn state.
The version strings below are hardcoded for the example; a real implementation can detect them (e.g. `claude --version`, `codex --version`, `grok --version`).

## Codex

Credential file `~/.codex/auth.json`: `.tokens.access_token`, `.tokens.account_id`, `.tokens.refresh_token`.

### Fetch quota

```bash
TOKEN=$(jq -r '.tokens.access_token' ~/.codex/auth.json)
ACCOUNT_ID=$(jq -r '.tokens.account_id' ~/.codex/auth.json)

curl -s -X GET "https://chatgpt.com/backend-api/wham/usage" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "ChatGPT-Account-Id: ${ACCOUNT_ID}" \
    -H "originator: codex_cli_rs" \
    -H "User-Agent: codex_cli_rs/0.142.5 (linux; x86_64)" | jq
```

`ChatGPT-Account-Id` is required. The `originator` and `User-Agent` are the Codex CLI's client identity (only sent for parity, not needed).
Do **not** replay the per-turn headers the real client also sends (`session-id`, `x-codex-window-id`, `x-codex-turn-metadata`, ...): a usage poll has no turn, and that metadata even leaks your repo path and git commit.

### Refresh token

```bash
AUTH="$HOME/.codex/auth.json"

curl -s https://auth.openai.com/oauth/token \
    -H 'Content-Type: application/json' \
    -d "$(jq -nc --arg rt "$(jq -r .tokens.refresh_token "$AUTH")" '{client_id:"app_EMoamEEZ73f0CkXaXp7hrann", grant_type:"refresh_token", refresh_token:$rt}')" | jq
```

Response carries `access_token` / `refresh_token` / `id_token`; write them back into `.tokens`.
The `refresh_token` rotates. `auth.json` has no expiry field, so refresh is reactive-only (fire on a 401).

### Check reset credit expiration

```bash
TOKEN=$(jq -r '.tokens.access_token' ~/.codex/auth.json)
ACCOUNT_ID=$(jq -r '.tokens.account_id' ~/.codex/auth.json)

curl -s -X GET "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "ChatGPT-Account-Id: ${ACCOUNT_ID}" \
    -H "originator: codex_cli_rs" \
    -H "User-Agent: codex_cli_rs/0.142.5 (linux; x86_64)" | jq
```

The response carries an authoritative `available_count` plus a possibly-capped `credits[]` list. Only entries with `status == "available"` are usable; `expires_at` is an RFC3339 timestamp or `null` when that credit does not expire. See [`wham_rate_limit_reset_credits_response.json`](../tests/fixtures/quota/wham_rate_limit_reset_credits_response.json) for a sanitized example.

## Claude Code

Credential file `~/.claude/.credentials.json`: `.claudeAiOauth.accessToken`, `.refreshToken`, `.scopes`, `.expiresAt` (ms).

### Fetch quota

```bash
TOKEN=$(jq -r '.claudeAiOauth.accessToken' ~/.claude/.credentials.json)

curl -s "https://api.anthropic.com/api/oauth/usage" \
    -H "Authorization: Bearer $TOKEN" \
    -H "anthropic-beta: oauth-2025-04-20" \
    -H "anthropic-version: 2023-06-01" \
    -H "x-app: cli" \
    -H "anthropic-dangerous-direct-browser-access: true" \
    -H "User-Agent: claude-cli/2.1.201 (external, cli)" | jq
```

The `anthropic-beta: oauth-2025-04-20` header unlocks the richer `limits` / `spend` fields.
This endpoint is rate-limited (expect `429` if polled faster than ~60s).
The other headers are Claude Code's client identity (see Client impersonation above); only `anthropic-beta` and the bearer token are required to get a response.

### Refresh token

```bash
CRED=~/.claude/.credentials.json
RT=$(jq -r '.claudeAiOauth.refreshToken' "$CRED")
SCOPE=$(jq -r '.claudeAiOauth.scopes | join(" ")' "$CRED")

curl -s "https://platform.claude.com/v1/oauth/token" \
    -H "Content-Type: application/json" \
    -d "$(jq -nc --arg rt "$RT" --arg scope "$SCOPE" '{grant_type:"refresh_token", client_id:"9d1c250a-e61b-44d9-88ed-5944d1962f5e", refresh_token:$rt, scope:$scope}')" | jq
```

Response carries `access_token` / `refresh_token` / `expires_in` (default 28800s = 8h) / `scope`.
Write back `accessToken`, the rotated `refreshToken`, and `expiresAt = now_ms + expires_in * 1000` into `claudeAiOauth`, preserving other keys (`designOauth`, ...).

Notes:

- Primary endpoint is `platform.claude.com`; fall back to `https://console.anthropic.com/v1/oauth/token` only on `404` / `405`.
- Omit `scope` when the file has none, so the server keeps the original grant instead of narrowing it to `user:inference`.
- The official Claude Code refreshes the token itself (~8h TTL) and rewrites the file, so guard the write with an mtime re-check to avoid clobbering a concurrent rotation. `400` / `401` (`invalid_grant`) means the refresh token was already rotated: do not retry, treat it as "needs login".

## Copilot

Credential file `~/.copilot/config.json`:
`.copilotTokens["https://github.com:<login>"]` holds a long-lived GitHub OAuth token (`gho_...`).
Note this file is **JSONC** (it starts with `//` comment lines), so strip comments before parsing it as JSON string-aware, so the `//` inside the `https://github.com:<login>` key survives.

### Fetch quota

```bash
TOKEN=$(jq -r '.copilotTokens | to_entries[] | select(.key|startswith("https://github.com")) | .value' ~/.copilot/config.json | head -1)

curl -s "https://api.github.com/copilot_internal/user" \
    -H "Authorization: token $TOKEN" \
    -H "Accept: application/json" \
    -H "Editor-Version: vscode/1.96.2" \
    -H "Editor-Plugin-Version: copilot-chat/0.26.7" \
    -H "User-Agent: GitHubCopilotChat/0.26.7" \
    -H "X-Github-Api-Version: 2025-04-01" | jq
```

Quota lives under `.quota_snapshots.premium_interactions` (premium requests), `.chat`, `.completions`; plan under `.copilot_plan`; reset under `.quota_reset_date`.
The `Editor-*` / `User-Agent` headers above impersonate the VS Code Copilot Chat plugin. Auth uses the legacy `token <t>` scheme, not `Bearer`.

Because this token belongs to the **Copilot CLI**, `vct` instead impersonates that client (the correct parity): `User-Agent: GitHubCopilotCLI/<version>` (version from `copilot --version`, cached daily) plus `Copilot-Integration-Id: copilot-cli` confirmed from the CLI bundle, which does **not** send the `Editor-Version` / `copilot-chat` headers.
The endpoint answers a bare token regardless, so the UA is cosmetic; only the token is required.

### Refresh token

None. `gho_` is long-lived and the file has no `refresh_token`. On a `401` / `403` the account is logged out: re-auth via the `copilot` CLI (GitHub device flow).

## Cursor

Credential file `~/.config/cursor/auth.json`: `.accessToken` (a WorkOS session JWT), `.refreshToken`. cursor.com is called with a synthesized session cookie: `WorkosCursorSessionToken=<userID>%3A%3A<accessToken>`, where `userID` is the JWT `sub` claim after the `|`, and `::` is percent-encoded as `%3A%3A`.

### Fetch quota

```bash
AT=$(jq -r '.accessToken' ~/.config/cursor/auth.json)
SUB=$(echo "$AT" | cut -d. -f2 | tr '_-' '/+' | sed 's/$/==/' | base64 -d 2>/dev/null | jq -r '.sub')
UID_PART=${SUB##*|}
COOKIE="WorkosCursorSessionToken=${UID_PART}%3A%3A${AT}"

# usage summary
curl -s "https://cursor.com/api/usage-summary" \
    -H "Cookie: $COOKIE" -H "Accept: application/json" | jq

# account identity (optional)
curl -s "https://cursor.com/api/auth/me" \
    -H "Cookie: $COOKIE" -H "Accept: application/json" | jq
```

Usage lives under `.individualUsage.plan` (`autoPercentUsed` / `apiPercentUsed` / `totalPercentUsed`), `.individualUsage.onDemand`, and `.teamUsage`; billing window under `.billingCycleStart` / `.billingCycleEnd`. Monetary `used` / `limit` values are in cents (divide by 100). The three `*PercentUsed` fields mean percent **used**: the same response restates those numbers in English via `autoModelSelectedDisplayMessage` / `namedModelSelectedDisplayMessage` ("You've used N% of your included total usage"), and an idle account returns `0` for all three. Use them verbatim — never invert.

### Fetch token usage (dashboard billing)

Real per-model token counts + cost per billing event, distinct from the quota summary above. Reuses the same `$COOKIE` synthesized in **Fetch quota**.

```bash
NOW_MS=$(( $(date +%s) * 1000 ))

curl -s -X POST "https://cursor.com/api/dashboard/get-filtered-usage-events" \
    -H "Cookie: $COOKIE" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json" \
    -H "Origin: https://cursor.com" \
    -H "Referer: https://cursor.com/dashboard?tab=usage" \
    -d "{\"teamId\":0,\"startDate\":0,\"endDate\":${NOW_MS},\"page\":1,\"pageSize\":5}" | jq
```

Response is `totalUsageEventsCount` + a `usageEventsDisplay[]` page; paginate `page` until you have read that many events. `startDate:0` = full history (set a millis cutoff to date-bound it); `pageSize` caps at ~1000 (HTTP 400 above that). Per event the load-bearing fields are `timestamp`, `model` (Cursor's own billing label, e.g. `default` / `gpt-5-high-fast` / `claude-opus-4-7-thinking-max`), `tokenUsage`, and cost `chargedCents` (fallback `tokenUsage.totalCents`), cents / 100 = USD.

### Refresh token

Reactive. The `accessToken` is valid ~60 days (`offline_access` scope) and the official Cursor CLI / IDE keeps `auth.json` fresh in the background, so the simplest approach is to re-read the file each poll and use the token while its JWT `exp` is in the future.
cursor.com does not return a rotating `Set-Cookie` on the usage call.
Self-refresh would go through WorkOS AuthKit (`authentication.cursor.sh`) via the cursor backend (`api2.cursor.sh`, paths like `/auth/token`), but the exact request shape is not pinned here. On a `401` re-login via `cursor login`.

## Antigravity

Credential file (Linux) `~/.gemini/antigravity-cli/antigravity-oauth-token`: plain JSON, `.token.access_token`, `.token.refresh_token`, `.token.expiry`. On macOS the same blob lives in the Keychain instead (service `gemini`, account `antigravity`), sometimes wrapped with a `go-keyring-base64:` prefix; the field names are the same once unwrapped.

Antigravity also has a second, richer source when the app (or the `agy` CLI) is **running**: a local Connect-RPC language server on loopback (`RetrieveUserQuotaSummary` on `https://127.0.0.1:<port>/...`), discovered by scanning for the process and reading its `--csrf_token`. That path needs no OAuth but only works while the app is up. The recipe below is the **app-closed** path, the one that fits this file: credential file in, HTTP out, works over plain SSH.

Unlike the other providers, the stored `access_token` is short-lived (~1h) and nothing keeps it fresh in the background, so in practice you refresh first (see **Refresh token**) and use that token to fetch.

### Fetch quota

Two base URLs: `cloudcode-pa.googleapis.com` (prod) and `daily-cloudcode-pa.googleapis.com` (canary), same schema. The authoritative endpoint is `retrieveUserQuotaSummary`: two shared pools (Gemini, and everything non-Gemini incl. Claude / GPT-OSS), each with a rolling 5-hour and a weekly window.

```bash
# The stored access_token is ~1h-lived; if this 401s, run Refresh token below and use that access_token.
TOKEN=$(jq -r '.token.access_token' ~/.gemini/antigravity-cli/antigravity-oauth-token)

curl -s -X POST "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "User-Agent: antigravity" \
    -H "Content-Type: application/json" \
    -d '{}' | jq
```

Each bucket carries `bucketId` (`gemini-5h` / `gemini-weekly` / `3p-5h` / `3p-weekly`), `remainingFraction` (0…1, where 1 = full, so used% = `(1 - remainingFraction) * 100`), and `resetTime`.

Older builds lack that RPC; the legacy fallbacks are 5h-only and take the same bearer token:

- `POST .../v1internal:fetchAvailableModels` (`User-Agent: antigravity`, body `{}`) — full model list, each with a `quotaInfo`.
- `POST .../v1internal:loadCodeAssist` (`User-Agent: agy`, body `{}`) — plan tier (`paidTier.name`) + `cloudaicompanionProject`.
- `POST .../v1internal:retrieveUserQuota` (`User-Agent: agy`, body `{"project":"<from loadCodeAssist>"}`) — per-Gemini-model request buckets.

### Refresh token

```bash
CRED=~/.gemini/antigravity-cli/antigravity-oauth-token
RT=$(jq -r '.token.refresh_token' "$CRED")

# client_id / client_secret: Antigravity's own Google OAuth installed-app credentials (see the note
# below). Fill in the real secret from the app bundle / agy binary, or copy the pair from openusage's
# AntigravityUsageClient.swift.
CLIENT_ID="1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com"
CLIENT_SECRET="<antigravity-installed-app-secret>"

curl -s "https://oauth2.googleapis.com/token" \
    -H "Content-Type: application/x-www-form-urlencoded" \
    --data-urlencode "client_id=${CLIENT_ID}" \
    --data-urlencode "client_secret=${CLIENT_SECRET}" \
    --data-urlencode "grant_type=refresh_token" \
    --data-urlencode "refresh_token=${RT}" | jq
```

Response carries `access_token` / `expires_in` (~3600s) / `scope`; use the `access_token` as the Bearer above.

`client_id` / `client_secret` are **not** yours to create: they are Antigravity's own Google OAuth **installed-application** credentials, baked into every copy of the app / `agy` CLI. Google does not treat an installed-app secret as confidential (it ships inside the client), so this is a public identifier, not a private key — but it is Google's, and the per-user `refresh_token` in the credential file is the only truly sensitive value here.

Unlike Codex / Claude above, this refresh does **not** rotate the refresh token (Google reuses installed-app refresh tokens until revoked), so there is nothing to write back to the credential file and it is safe to run alongside the live app. Cache only the returned `access_token`. A `400` / `invalid_grant` means the refresh token was revoked: re-login via the Antigravity app or `agy`.

## Grok

The current CLI is the Rust [`xai-org/grok-build`](https://github.com/xai-org/grok-build), and the calls below are exactly what its `x.ai/billing` extension sends — the same request behind the `/usage` slash command (alias `/cost`) and the status-bar credit meter.

Credential file `~/.grok/auth.json` (`$GROK_HOME/auth.json`; `GROK_AUTH_PATH` relocates it): a JSON object with one entry per login scope, keyed `"<issuer>::<client_id>"` for an xAI OAuth2 login (e.g. `https://auth.x.ai::b1a00492-…`), `"xai::api_key"` for a plain API key, or the legacy `"https://accounts.x.ai/sign-in"`. The **access token is the `key` field** (not a field named `access_token`) — a JWT — and the same entry carries `user_id`, `refresh_token`, `expires_at`, `oidc_issuer`, and `oidc_client_id`.

Only two headers are load-bearing: the bearer, and `X-XAI-Token-Auth: xai-grok-cli`, which tells the proxy's auth middleware to validate the token as a CLI session. `x-grok-client-version` feeds the proxy's version gate, so send it as well. The remaining headers (`x-userid`, `x-grok-client-mode`, `User-Agent`) are the CLI's own identity — see Client impersonation above.

### Fetch quota

```bash
AUTH=~/.grok/auth.json
TOKEN=$(jq -r 'to_entries[0].value.key' "$AUTH")       # access token lives under `.key`
USERID=$(jq -r 'to_entries[0].value.user_id' "$AUTH")

curl -s "https://cli-chat-proxy.grok.com/v1/billing?format=credits" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "X-XAI-Token-Auth: xai-grok-cli" \
    -H "x-userid: ${USERID}" \
    -H "x-grok-client-version: 0.2.112" \
    -H "x-grok-client-mode: interactive" \
    -H "User-Agent: grok-pager/0.2.112 grok-shell/0.2.112 (linux; x86_64)" | jq
```

`billing?format=credits` returns the `GetGrokCreditsConfig` message wrapped as `{"config": {…}}`. Every money value is `{"val": <cents>}`, and proto3 JSON omits zero-valued scalars, so a `$0` amount arrives as `{}` and a false flag is simply absent. Fields:

| Field                                                               | Meaning                                                                        |
| ------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `creditUsagePercent`                                                | included-allowance usage, `0.0`–`100.0` — what the status bar shows            |
| `currentPeriod`                                                     | `type` (`USAGE_PERIOD_TYPE_WEEKLY` / `…_MONTHLY`) plus RFC3339 `start` / `end` |
| `onDemandCap` / `onDemandUsed`                                      | pay-as-you-go cap and spend this period                                        |
| `prepaidBalance`                                                    | remaining purchased ("bought") credit balance                                  |
| `isUnifiedBillingUser`                                              | whether the account is on the shared weekly/monthly pool                       |
| `history[]`                                                         | past periods: `billingCycle` + `includedUsed` / `onDemandUsed` / `totalUsed`   |
| `monthlyLimit` / `used` / `billingPeriodStart` / `billingPeriodEnd` | deprecated legacy shape, still emitted by older servers                        |

The CLI fills `on_demand_enabled` and `subscription_tier` into the same object from remote settings, so a raw curl returns only `config`. Two companions live on the same base and take the same headers:

```bash
# Prepaid auto top-up rule (the CLI asks only when prepaidBalance is non-zero):
curl -s "https://cli-chat-proxy.grok.com/v1/auto-topup-rule" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "X-XAI-Token-Auth: xai-grok-cli" \
    -H "x-grok-client-version: 0.2.112" | jq

# Plan / feature flags — `subscription_tier`, `subscription_tier_display`, `on_demand_enabled`:
curl -s "https://cli-chat-proxy.grok.com/v1/settings" \
    -H "Authorization: Bearer ${TOKEN}" \
    -H "X-XAI-Token-Auth: xai-grok-cli" \
    -H "x-grok-client-version: 0.2.112" | jq
```

A `401` / `403` means the token expired: refresh (below) and retry once. There are no `x-ratelimit-*` response headers anywhere in the CLI — a rate or credit limit reaches the client only as an inference error (`429`, or `402` once credits run out).

### Refresh token

The token endpoint is never hardcoded: the CLI resolves it from OIDC discovery on the entry's issuer, so do the same.

```bash
AUTH=~/.grok/auth.json
RT=$(jq -r 'to_entries[0].value.refresh_token' "$AUTH")
# issuer / client id: the entry's own fields, else the two "::"-delimited halves of the entry key.
ISS=$(jq -r 'to_entries[0] | .value.oidc_issuer // (.key | split("::")[0])' "$AUTH")
CID=$(jq -r 'to_entries[0] | .value.oidc_client_id // (.key | split("::") | last)' "$AUTH")
TOKEN_URL=$(curl -s "${ISS}/.well-known/openid-configuration" | jq -r .token_endpoint)

curl -s "$TOKEN_URL" \
    -H "Content-Type: application/x-www-form-urlencoded" \
    --data-urlencode "grant_type=refresh_token" \
    --data-urlencode "client_id=${CID}" \
    --data-urlencode "refresh_token=${RT}" | jq
```

For an xAI login that discovery resolves to `https://auth.x.ai/oauth2/token`. Response carries `access_token` / `refresh_token` / `id_token` / `expires_in`. Write the new `access_token` back into that entry's `key`, plus the rotated `refresh_token` and a fresh `expires_at`, preserving every **other** entry in the file. A team login also sends `principal_type` / `principal_id` form fields. There is no `client_secret` (xAI's CLI OAuth client is public and secret-less), so this refresh trips none of the secret scanners.
