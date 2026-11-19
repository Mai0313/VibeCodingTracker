//! Stable, path-independent session keys per provider.
//!
//! Keys never contain absolute paths, allowing entries to survive home-directory changes and cross-machine ledger merges.

use crate::models::ExtensionType;
use std::path::Path;

/// Returns the ledger key for `path` under `roots`: the bare file name for [`ExtensionType::Codex`] (invariant across `sessions/` and `archived_sessions/`), and a `/`-separated path relative to `roots` for all other providers.
pub(crate) fn session_key(provider: ExtensionType, roots: &[&Path], path: &Path) -> String {
    if provider == ExtensionType::Codex {
        return file_name(path);
    }
    relative_key(roots, path)
}

/// Returns `path` relative to the first matching root in `roots` using `/` separators, falling back to the file name if no root matches.
pub(crate) fn relative_key(roots: &[&Path], path: &Path) -> String {
    roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .map(|relative| {
            if let Some(text) = relative.to_str()
                && !text.is_empty()
                && text != "."
                && !text.contains('\\')
                && !text.contains("//")
                && !text.starts_with("./")
                && !text.contains("/./")
                && !text.ends_with('/')
            {
                return text.to_string();
            }
            relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_else(|| file_name(path))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn claude_keys_keep_the_project_directory_and_subagent_nesting() {
        let root = PathBuf::from("/home/u/.claude/projects");
        let subagent = root.join("-home-u-repo/sess/subagents/agent-1.jsonl");
        assert_eq!(
            session_key(ExtensionType::ClaudeCode, &[&root], &subagent),
            "-home-u-repo/sess/subagents/agent-1.jsonl"
        );
    }

    #[test]
    fn codex_keys_on_the_file_name_whichever_root_holds_it() {
        let active = PathBuf::from("/home/u/.codex/sessions");
        let archived = PathBuf::from("/home/u/.codex/archived_sessions");
        let roots = [active.as_path(), archived.as_path()];
        let name = "rollout-2026-06-06T10-00-00-uuid.jsonl";
        assert_eq!(
            session_key(
                ExtensionType::Codex,
                &roots,
                &active.join("2026/06/06").join(name)
            ),
            name
        );
        assert_eq!(
            session_key(ExtensionType::Codex, &roots, &archived.join(name)),
            name
        );
    }

    #[test]
    fn a_path_outside_every_root_falls_back_to_its_name() {
        let root = PathBuf::from("/home/u/.gemini/tmp");
        assert_eq!(
            relative_key(&[&root], Path::new("/elsewhere/chat.jsonl")),
            "chat.jsonl"
        );
    }
}
