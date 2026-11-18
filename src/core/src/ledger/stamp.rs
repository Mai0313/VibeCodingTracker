//! Building read stamps from what is on disk right now.

use super::format::FileStamp;
use super::tiny_map::TinyMap;
use crate::models::ExtensionType;
use crate::pricing::TierThresholds;
use crate::session::antigravity;
use crate::session::sqlite::{DatabaseFingerprint, append_suffix};
use crate::utils::directory::DiscoveredFile;
use anyhow::Result;
use chrono::SecondsFormat;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Whether `provider`'s read stamp includes sidecar files beyond the discovered
/// session file itself.
pub(crate) const fn has_sidecars(provider: ExtensionType) -> bool {
    matches!(provider, ExtensionType::Grok | ExtensionType::Antigravity)
}

/// The files a session log's parse depends on: the log itself, stamped from
/// the metadata discovery already read, plus, for Grok, the sidecars the
/// parser reads beside it, and for Antigravity the conversation database its
/// model map comes from.
pub(crate) fn file_source(
    file: &DiscoveredFile,
    provider: ExtensionType,
) -> Result<TinyMap<String, Option<FileStamp>>> {
    let mut files = TinyMap::new();
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
    if provider == ExtensionType::Antigravity
        && let Some(db) = antigravity::conversation_db_path(&file.path)
    {
        let wal = append_suffix(&db, "-wal");
        files.insert("conversation.db".to_string(), optional_stamp(&db)?);
        files.insert("conversation.db-wal".to_string(), optional_stamp(&wal)?);
    }
    Ok(files)
}

/// A SQLite database and its optional WAL.
pub(crate) fn sqlite_source(db_path: &Path) -> Result<TinyMap<String, Option<FileStamp>>> {
    let mut files = TinyMap::new();
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
    files: &mut TinyMap<String, Option<FileStamp>>,
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
    if let Some(bytes) = format_modified_ascii(time) {
        return std::str::from_utf8(&bytes)
            .expect("ASCII RFC 3339 buffer is valid UTF-8")
            .to_string();
    }
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Compares `time` against a stored RFC 3339 nanosecond stamp without heap
/// allocation for normal post-epoch timestamps.
pub(crate) fn modified_matches(time: SystemTime, expected: &str) -> bool {
    if let Some(bytes) = format_modified_ascii(time) {
        return expected.as_bytes() == bytes;
    }
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339_opts(SecondsFormat::Nanos, true)
        == expected
}

/// Formats `1970-01-01T00:00:00.000000000Z` through
/// `9999-12-31T23:59:59.999999999Z` onto the stack.
fn format_modified_ascii(time: SystemTime) -> Option<[u8; 30]> {
    const MAX_YEAR_9999_SECS: u64 = 253_402_300_799;
    let duration = time.duration_since(UNIX_EPOCH).ok()?;
    let secs = duration.as_secs();
    if secs > MAX_YEAR_9999_SECS {
        return None;
    }
    let nanos = duration.subsec_nanos();

    let days = secs / 86_400;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }

    let hour = rem / 3_600;
    let rem = rem % 3_600;
    let minute = rem / 60;
    let second = rem % 60;

    let mut out = *b"0000-00-00T00:00:00.000000000Z";
    write_u32_digits(&mut out[0..4], year as u32);
    write_u8_two(&mut out[5..7], month as u8);
    write_u8_two(&mut out[8..10], day as u8);
    write_u8_two(&mut out[11..13], hour as u8);
    write_u8_two(&mut out[14..16], minute as u8);
    write_u8_two(&mut out[17..19], second as u8);
    write_u32_digits(&mut out[20..29], nanos);
    Some(out)
}

fn write_u8_two(slot: &mut [u8], value: u8) {
    slot[0] = b'0' + (value / 10);
    slot[1] = b'0' + (value % 10);
}

fn write_u32_digits(slot: &mut [u8], mut value: u32) {
    for byte in slot.iter_mut().rev() {
        *byte = b'0' + (value % 10) as u8;
        value /= 10;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ascii_modified_stamp_matches_chrono_across_boundaries() {
        for (secs, nanos) in [
            (0, 0),
            (86_399, 999_999_999),
            (951_782_400, 123_456_789), // 2000-02-29 leap day
            (1_785_058_368, 5),
            (4_102_444_799, 420_000_000),
            (253_402_300_799, 999_999_999),
        ] {
            let time = UNIX_EPOCH + Duration::new(secs, nanos);
            let expected = chrono::DateTime::<chrono::Utc>::from(time)
                .to_rfc3339_opts(SecondsFormat::Nanos, true);
            assert_eq!(format_modified(time), expected);
            assert!(modified_matches(time, &expected));
            assert!(!modified_matches(time + Duration::from_nanos(1), &expected));
        }

        // Pre-epoch falls back to chrono cleanly.
        let before_epoch = UNIX_EPOCH - Duration::from_secs(60);
        let expected = chrono::DateTime::<chrono::Utc>::from(before_epoch)
            .to_rfc3339_opts(SecondsFormat::Nanos, true);
        assert_eq!(format_modified(before_epoch), expected);
        assert!(modified_matches(before_epoch, &expected));
    }

    #[test]
    fn a_dependency_stamp_follows_the_snapshot_it_was_paired_with() {
        let dir = tempfile::tempdir().unwrap();
        let tracking = dir.path().join("tracking.db");
        std::fs::write(&tracking, b"model-a").unwrap();
        let snapshot = crate::session::sqlite::database_fingerprint(&tracking).unwrap();

        let mut paired = TinyMap::new();
        add_dependency(&mut paired, &tracking, Some(&snapshot));
        std::fs::write(&tracking, b"model-b-longer").unwrap();
        let current = crate::session::sqlite::database_fingerprint(&tracking).unwrap();
        let mut changed = TinyMap::new();
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
