//! Building read stamps from what is on disk right now.

use super::format::FileStamp;
use crate::models::ExtensionType;
use crate::pricing::TierThresholds;
use crate::session::sqlite::{DatabaseFingerprint, append_suffix};
use crate::utils::directory::FileInfo;
use anyhow::Result;
use chrono::SecondsFormat;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

/// The files a session log's parse depends on: the log itself, stamped from
/// the metadata discovery already read, plus, for Grok, the sidecars the
/// parser reads beside it.
pub(crate) fn file_source(
    file: &FileInfo,
    provider: ExtensionType,
) -> Result<BTreeMap<String, Option<FileStamp>>> {
    let mut files = BTreeMap::new();
    files.insert(
        name_of(&file.path),
        Some(FileStamp {
            modified: format_modified(file.modified),
            len: file.len,
        }),
    );
    if provider == ExtensionType::Grok {
        for sidecar in ["summary.json", "updates.jsonl"] {
            let sidecar_path = file.path.with_file_name(sidecar);
            files.insert(sidecar.to_string(), optional_stamp(&sidecar_path)?);
        }
        if let Some(workspace) = file.path.parent().and_then(Path::parent) {
            let cwd = workspace.join(".cwd");
            files.insert("../.cwd".to_string(), optional_stamp(&cwd)?);
        }
    }
    Ok(files)
}

/// A SQLite database and its optional WAL.
pub(crate) fn sqlite_source(db_path: &Path) -> Result<BTreeMap<String, Option<FileStamp>>> {
    let mut files = BTreeMap::new();
    files.insert(name_of(db_path), Some(required_stamp(db_path)?));
    let wal = append_suffix(db_path, "-wal");
    files.insert(name_of(&wal), optional_stamp(&wal)?);
    Ok(files)
}

/// Adds an already-fingerprinted dependency to `files`.
///
/// Cursor's model attribution comes from a tracking database read once per
/// scan; the fingerprint captured around that read is what goes into every
/// store's stamp, so a store can never pair model map A with fingerprint B.
pub(crate) fn add_dependency(
    files: &mut BTreeMap<String, Option<FileStamp>>,
    db_path: &Path,
    fingerprint: Option<&DatabaseFingerprint>,
) {
    let wal = append_suffix(db_path, "-wal");
    match fingerprint {
        Some(fingerprint) => {
            files.insert(
                name_of(db_path),
                Some(FileStamp {
                    modified: format_modified(fingerprint.database.modified),
                    len: fingerprint.database.length,
                }),
            );
            files.insert(
                name_of(&wal),
                fingerprint.wal.as_ref().map(|stamp| FileStamp {
                    modified: format_modified(stamp.modified),
                    len: stamp.length,
                }),
            );
        }
        None => {
            files.insert(name_of(db_path), None);
            files.insert(name_of(&wal), None);
        }
    }
}

/// The snapshot fingerprint a usage scan stamps its entries with; `None` when
/// the scan classifies nothing (no snapshot, or one with no boundaries).
pub(crate) fn tiers_fingerprint(tiers: Option<&TierThresholds>) -> Option<String> {
    tiers
        .map(TierThresholds::fingerprint)
        .filter(|fingerprint| *fingerprint != 0)
        .map(|fingerprint| format!("{fingerprint:016x}"))
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn required_stamp(path: &Path) -> Result<FileStamp> {
    let metadata = fs::metadata(path)?;
    Ok(FileStamp {
        modified: format_modified(metadata.modified()?),
        len: metadata.len(),
    })
}

fn optional_stamp(path: &Path) -> Result<Option<FileStamp>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(FileStamp {
        modified: format_modified(metadata.modified()?),
        len: metadata.len(),
    }))
}

fn format_modified(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(SecondsFormat::Nanos, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dependency_stamp_follows_the_snapshot_it_was_paired_with() {
        let dir = tempfile::tempdir().unwrap();
        let tracking = dir.path().join("tracking.db");
        std::fs::write(&tracking, b"model-a").unwrap();
        let snapshot = crate::session::sqlite::database_fingerprint(&tracking).unwrap();

        let mut paired = BTreeMap::new();
        add_dependency(&mut paired, &tracking, Some(&snapshot));
        std::fs::write(&tracking, b"model-b-longer").unwrap();
        let current = crate::session::sqlite::database_fingerprint(&tracking).unwrap();
        let mut changed = BTreeMap::new();
        add_dependency(&mut changed, &tracking, Some(&current));

        assert_ne!(paired, changed);
        assert!(paired.contains_key("tracking.db"));
        assert_eq!(paired["tracking.db-wal"], None);
    }

    #[test]
    fn an_empty_snapshot_stamps_no_tiers() {
        assert_eq!(tiers_fingerprint(None), None);
        let empty = TierThresholds::from_entries(std::iter::empty());
        assert_eq!(tiers_fingerprint(Some(&empty)), None);
    }
}
