//! Stable session identity per provider.
//!
//! A ledger entry has to survive the source moving and the home directory
//! changing, and two machines' ledgers have to be mergeable by key, so a key
//! never carries an absolute path.

use crate::models::ExtensionType;
use std::path::Path;

/// The ledger key of a session log found under one of `roots`.
///
/// Every provider keys on the path relative to its session root, with `/` as
/// the separator on every platform. Codex keys on the bare file name instead:
/// archiving moves a rollout log from the dated `sessions/` tree into the flat
/// `archived_sessions/` root under the same name, and the name is already
/// what cross-root deduplication treats as the session's identity, so keying
/// on it is what keeps the move from reading as one session gone and another
/// one new.
pub(crate) fn session_key(provider: ExtensionType, roots: &[&Path], path: &Path) -> String {
    if provider == ExtensionType::Codex {
        return file_name(path);
    }
    relative_key(roots, path)
}

/// `path` relative to the first of `roots` that contains it, `/`-separated.
///
/// Falls back to the file name when no root contains it, which discovery never
/// produces (every path it returns was found under one of them).
pub(crate) fn relative_key(roots: &[&Path], path: &Path) -> String {
    roots
        .iter()
        .find_map(|root| path.strip_prefix(root).ok())
        .map(|relative| {
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
