# Quota + token refresh (raw curl/jq examples)

Raw, copy-pastable `curl` + `jq` recipes for reading each provider's remaining
quota and refreshing its OAuth token, straight from the credential files the
official CLIs already manage on disk. No browser, no headless webview, so they
work over plain SSH.

These are minimal examples: they stop at "dump the JSON response". Field
extraction / mapping is left out on purpose.

> Most of these hit internal / undocumented endpoints (the same ones the
> official clients use) and require the credential file to already be logged in.
> Refresh calls **rotate** the token and must be written back to the file, so do
> not run them casually while the official CLI is live.

## At a glance

| Provider    | Credential file               | Quota endpoint                              | Refresh model                                |
| ----------- | ----------------------------- | ------------------------------------------- | -------------------------------------------- |
| Codex       | `~/.codex/auth.json`          | `GET  chatgpt.com/backend-api/wham/usage`   | `POST auth.openai.com/oauth/token` (rotates) |
| Claude Code | `~/.claude/.credentials.json` | `GET  api.anthropic.com/api/oauth/usage`    | `POST platform.claude.com/v1/oauth/token`    |
| Copilot     | `~/.copilot/config.json`      | `GET  api.github.com/copilot_internal/user` | none (long-lived `gho_` token)               |
| Cursor      | `~/.config/cursor/auth.json`  | `GET  cursor.com/api/usage-summary`         | reactive (official CLI keeps the file fresh) |

## Client impersonation

Make each request look like the official client the **token belongs to** (a
Claude Code token should look like Claude Code). These headers are camouflage,
not auth: the endpoints answer a bare bearer token. Cadence matters more than any
header, so poll slowly and back off on `429`. Codex does not strictly need it
(OpenAI treats codex-cli traffic as sanctioned), but the example still sends a
codex-shaped UA + originator for parity. Send only stable identity headers, never
per-request/per-turn state. The version strings below are hardcoded for the
example; a real implementation can detect them (e.g. `claude --version`,
`codex --version`).

## Codex

Credential file `~/.codex/auth.json`: `.tokens.access_token`,
`.tokens.account_id`, `.tokens.refresh_token`.

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

`ChatGPT-Account-Id` is required. The `originator` and `User-Agent` are the Codex
CLI's client identity (only sent for parity, not needed). Do **not** replay the
per-turn headers the real client also sends (`session-id`, `x-codex-window-id`,
`x-codex-turn-metadata`, ...): a usage poll has no turn, and that metadata even
leaks your repo path and git commit.

### Refresh token

```bash
AUTH="$HOME/.codex/auth.json"

curl -s https://auth.openai.com/oauth/token \
    -H 'Content-Type: application/json' \
-d "$(jq -nc --arg rt "$(jq -r .tokens.refresh_token "$AUTH")" \
        '{client_id:"app_EMoamEEZ73f0CkXaXp7hrann", grant_type:"refresh_token", refresh_token:$rt}')" | jq
```

Response carries `access_token` / `refresh_token` / `id_token`; write them back
into `.tokens`. The `refresh_token` rotates. `auth.json` has no expiry field, so
refresh is reactive-only (fire on a 401).

## Claude Code

Credential file `~/.claude/.credentials.json`: `.claudeAiOauth.accessToken`,
`.refreshToken`, `.scopes`, `.expiresAt` (ms).

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

The `anthropic-beta: oauth-2025-04-20` header unlocks the richer `limits` /
`spend` fields. This endpoint is rate-limited (expect `429` if polled faster
than ~60s). The other headers are Claude Code's client identity (see Client
impersonation above); only `anthropic-beta` and the bearer token are required to
get a response.

### Refresh token

```bash
CRED=~/.claude/.credentials.json
RT=$(jq -r '.claudeAiOauth.refreshToken' "$CRED")
SCOPE=$(jq -r '.claudeAiOauth.scopes | join(" ")' "$CRED")

curl -s "https://platform.claude.com/v1/oauth/token" \
    -H "Content-Type: application/json" \
-d "$(jq -nc --arg rt "$RT" --arg scope "$SCOPE" \
        '{grant_type:"refresh_token", client_id:"9d1c250a-e61b-44d9-88ed-5944d1962f5e", refresh_token:$rt, scope:$scope}')" | jq
```

Response carries `access_token` / `refresh_token` / `expires_in` (default 28800s
= 8h) / `scope`. Write back `accessToken`, the rotated `refreshToken`, and
`expiresAt = now_ms + expires_in * 1000` into `claudeAiOauth`, preserving other
keys (`designOauth`, ...).

Notes:

- Primary endpoint is `platform.claude.com`; fall back to
    `https://console.anthropic.com/v1/oauth/token` only on `404` / `405`.
- Omit `scope` when the file has none, so the server keeps the original grant
    instead of narrowing it to `user:inference`.
- The official Claude Code refreshes the token itself (~8h TTL) and rewrites the
    file, so guard the write with an mtime re-check to avoid clobbering a
    concurrent rotation. `400` / `401` (`invalid_grant`) means the refresh token
    was already rotated: do not retry, treat it as "needs login".

## Copilot

Credential file `~/.copilot/config.json`:
`.copilotTokens["https://github.com:<login>"]` holds a long-lived GitHub OAuth
token (`gho_...`).

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

Quota lives under `.quota_snapshots.premium_interactions` (premium requests),
`.chat`, `.completions`; plan under `.copilot_plan`; reset under
`.quota_reset_date`. The `Editor-*` / `User-Agent` headers impersonate the VS
Code Copilot Chat plugin and are what the real client sends. Auth uses the
legacy `token <t>` scheme, not `Bearer`.

### Refresh token

None. `gho_` is long-lived and the file has no `refresh_token`. On a `401` /
`403` the account is logged out: re-auth via the `copilot` CLI (GitHub device
flow).

## Cursor

Credential file `~/.config/cursor/auth.json`: `.accessToken` (a WorkOS session
JWT), `.refreshToken`. cursor.com is called with a synthesized session cookie:
`WorkosCursorSessionToken=<userID>%3A%3A<accessToken>`, where `userID` is the
JWT `sub` claim after the `|`, and `::` is percent-encoded as `%3A%3A`.

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

Usage lives under `.individualUsage.plan` (`autoPercentUsed` / `apiPercentUsed`
/ `totalPercentUsed`), `.individualUsage.onDemand`, and `.teamUsage`; billing
window under `.billingCycleStart` / `.billingCycleEnd`. Monetary `used` / `limit`
values are in cents (divide by 100).

### Refresh token

Reactive. The `accessToken` is valid ~60 days (`offline_access` scope) and the
official Cursor CLI / IDE keeps `auth.json` fresh in the background, so the
simplest approach is to re-read the file each poll and use the token while its
JWT `exp` is in the future. cursor.com does not return a rotating `Set-Cookie`
on the usage call. Self-refresh would go through WorkOS AuthKit
(`authentication.cursor.sh`) via the cursor backend (`api2.cursor.sh`, paths
like `/auth/token`), but the exact request shape is not pinned here. On a `401`
re-login via `cursor login`.
