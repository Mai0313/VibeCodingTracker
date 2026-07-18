//! Shared helpers for the integration test binaries.
//!
//! The whole point of this module is hermetic isolation **without** mutating
//! process-global environment: [`TempHome`] builds a [`HelperPaths`] rooted at a
//! `TempDir` via [`resolve_paths_from_home`], so a test can drop fixture session
//! files into a fake home and call the `*_from_paths` aggregation entry points
//! directly. No `HOME`/`XDG_*`/`VCT_OFFLINE` is ever touched, so every test runs
//! in parallel and behaves identically locally and in CI.
//!
//! Each integration test binary compiles this module independently and uses only
//! a subset of the helpers, so `dead_code` is expected and allowed.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use vct_core::utils::{HelperPaths, resolve_paths_from_home};

/// Absolute path to a file under the repo's `tests/fixtures/` directory.
///
/// `name` is relative to that root, so it carries the category segment:
/// `sessions/claude_code.jsonl`, `quota/wham_usage_response.json`.
///
/// Uses `CARGO_MANIFEST_DIR` so it resolves the same on any machine and
/// regardless of the test's working directory.
pub fn fixture(name: &str) -> PathBuf {
    // From `src/test-support` up to the repo root, then into `tests/fixtures`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Reads a fixture file under `tests/fixtures/` to a `String`.
pub fn fixture_str(name: &str) -> String {
    std::fs::read_to_string(fixture(name))
        .unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"))
}

/// Appends a valid standalone JSON blob large enough to change a Cursor store fingerprint.
pub fn append_cursor_json_blob(path: &Path, id: &str) {
    let connection = rusqlite::Connection::open(path).expect("open Cursor store");
    let payload = serde_json::json!({
        "role": "assistant",
        "content": [],
        "padding": "x".repeat(8 * 1024),
    })
    .to_string()
    .into_bytes();
    connection
        .execute(
            "INSERT INTO blobs (id, data) VALUES (?1, ?2)",
            rusqlite::params![id, payload],
        )
        .expect("append Cursor JSON blob");
}

/// A temporary fake home directory plus the [`HelperPaths`] rooted inside it.
///
/// Every provider directory (`.claude`, `.codex`, `.gemini`, `.copilot`,
/// `.cursor`, `.grok`, `.local/share/opencode`, `.config/cursor`) and the `~/.vct` cache
/// resolve under `dir`, matching exactly what production would compute for a
/// user whose `HOME` was this directory.
pub struct TempHome {
    pub dir: TempDir,
    pub paths: HelperPaths,
}

impl Default for TempHome {
    fn default() -> Self {
        Self::new()
    }
}

impl TempHome {
    /// Creates an empty fake home.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("create temp home");
        let paths = resolve_paths_from_home(dir.path());
        Self { dir, paths }
    }

    /// The fake home root (what production would read from `$HOME`).
    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    /// Writes `content` to `path`, creating any missing parent directories.
    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, content).expect("write file");
    }

    /// Drops a Claude Code session log at `~/.claude/projects/<project>/<file>`.
    pub fn put_claude_session(&self, project: &str, file: &str, content: &str) -> PathBuf {
        let p = self.paths.claude_session_dir.join(project).join(file);
        Self::write_file(&p, content);
        p
    }

    /// Drops a Codex rollout log at `~/.codex/sessions/<rel>`.
    pub fn put_codex_session(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.paths.codex_session_dir.join(rel);
        Self::write_file(&p, content);
        p
    }

    /// Drops a Gemini chat log at `~/.gemini/tmp/<project>/chats/<file>`.
    ///
    /// The `chats` parent segment is required by `is_gemini_session_file`.
    pub fn put_gemini_session(&self, project: &str, file: &str, content: &str) -> PathBuf {
        let p = self
            .paths
            .gemini_session_dir
            .join(project)
            .join("chats")
            .join(file);
        Self::write_file(&p, content);
        p
    }

    /// Drops a Grok session entry point at
    /// `~/.grok/sessions/<workspace>/<session>/signals.json`.
    pub fn put_grok_session(&self, workspace: &str, session: &str, content: &str) -> PathBuf {
        let p = self
            .paths
            .grok_session_dir
            .join(workspace)
            .join(session)
            .join("signals.json");
        Self::write_file(&p, content);
        p
    }

    /// Copies the sanitized Grok fixture, including its metadata and update log.
    pub fn put_grok_fixture_session(&self, workspace: &str, session: &str) -> PathBuf {
        let signals = self.put_grok_session(
            workspace,
            session,
            &fixture_str("sessions/grok/signals.json"),
        );
        let dir = signals.parent().expect("Grok fixture session directory");
        Self::write_file(
            &dir.join("summary.json"),
            &fixture_str("sessions/grok/summary.json"),
        );
        Self::write_file(
            &dir.join("updates.jsonl"),
            &fixture_str("sessions/grok/updates.jsonl"),
        );
        signals
    }

    /// Seeds one Cursor chat store with an assistant tool call and context gauge.
    pub fn put_cursor_session(
        &self,
        project: &str,
        conversation: &str,
        model: &str,
        timestamp_ms: i64,
        context_tokens: i64,
    ) -> PathBuf {
        fn varint(mut value: u64, out: &mut Vec<u8>) {
            loop {
                let mut byte = (value & 0x7f) as u8;
                value >>= 7;
                if value != 0 {
                    byte |= 0x80;
                }
                out.push(byte);
                if value == 0 {
                    break;
                }
            }
        }

        fn tag(field: u64, wire: u64, out: &mut Vec<u8>) {
            varint((field << 3) | wire, out);
        }

        let message = r#"{"role":"assistant","content":[{"type":"tool-call","toolName":"Write","toolCallId":"write","args":{"path":"/repo/output.rs","contents":"first\nsecond"}}]}"#;
        let mut node = Vec::new();
        tag(1, 2, &mut node);
        varint(1, &mut node);
        node.push(0);
        tag(4, 2, &mut node);
        varint(message.len() as u64, &mut node);
        node.extend_from_slice(message.as_bytes());
        let mut context = Vec::new();
        tag(1, 0, &mut context);
        varint(context_tokens as u64, &mut context);
        tag(5, 2, &mut node);
        varint(context.len() as u64, &mut node);
        node.extend_from_slice(&context);
        tag(26, 0, &mut node);
        varint(timestamp_ms as u64, &mut node);

        let path = self
            .paths
            .cursor_chats_dir
            .join(project)
            .join(conversation)
            .join("store.db");
        std::fs::create_dir_all(path.parent().unwrap()).expect("create Cursor store directory");
        let connection = rusqlite::Connection::open(&path).expect("create Cursor store");
        connection
            .execute_batch(
                "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB); \
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);",
            )
            .expect("create Cursor store schema");
        connection
            .execute(
                "INSERT INTO blobs (id, data) VALUES ('assistant', ?1)",
                rusqlite::params![node],
            )
            .expect("insert Cursor assistant node");
        connection
            .execute(
                "INSERT INTO meta (key, value) VALUES ('metadata', ?1)",
                rusqlite::params![serde_json::json!({ "lastUsedModel": model }).to_string()],
            )
            .expect("insert Cursor metadata");
        path
    }

    /// Writes an arbitrary file under the fake home (relative to `HOME`).
    pub fn put(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.home().join(rel);
        Self::write_file(&p, content);
        p
    }

    /// Seeds today's pricing cache under `~/.vct` with a cost-fields JSON map so
    /// `fetch_model_pricing` loads it from cache instead of hitting the network.
    ///
    /// `models` is `{ "<model>": { "<cost_field>": <number>, ... }, ... }`.
    pub fn seed_pricing_cache(&self, models: &serde_json::Value) -> PathBuf {
        std::fs::create_dir_all(&self.paths.cache_dir).expect("create cache dir");
        let path = self.paths.cache_dir.join(format!(
            "model_pricing_{}.json",
            chrono::Utc::now().date_naive().format("%Y-%m-%d")
        ));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(models).expect("serialize pricing cache"),
        )
        .expect("write pricing cache");
        path
    }
}
