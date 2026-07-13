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
use vibe_coding_tracker::utils::{HelperPaths, get_current_date, resolve_paths_from_home};

/// Absolute path to a file under the repo's `examples/` directory.
///
/// Uses `CARGO_MANIFEST_DIR` so it resolves the same on any machine and
/// regardless of the test's working directory.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join(name)
}

/// Reads a fixture file under `examples/` to a `String`.
pub fn fixture_str(name: &str) -> String {
    std::fs::read_to_string(fixture(name))
        .unwrap_or_else(|e| panic!("failed to read fixture {name}: {e}"))
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
            &fixture_str("grok_session/signals.json"),
        );
        let dir = signals.parent().expect("Grok fixture session directory");
        Self::write_file(
            &dir.join("summary.json"),
            &fixture_str("grok_session/summary.json"),
        );
        Self::write_file(
            &dir.join("updates.jsonl"),
            &fixture_str("grok_session/updates.jsonl"),
        );
        signals
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
        let path = self
            .paths
            .cache_dir
            .join(format!("model_pricing_{}.json", get_current_date()));
        std::fs::write(
            &path,
            serde_json::to_string_pretty(models).expect("serialize pricing cache"),
        )
        .expect("write pricing cache");
        path
    }
}
