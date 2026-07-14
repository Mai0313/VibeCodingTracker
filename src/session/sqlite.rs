//! Shared read-only SQLite access with a stable private-copy fallback.

use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OpenFlags};
use std::ffi::OsString;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_COPY_ATTEMPTS: usize = 3;

/// The metadata used to detect changes to one SQLite source file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileFingerprint {
    pub length: u64,
    pub modified: SystemTime,
}

/// A fingerprint of a SQLite main database and its optional WAL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DatabaseFingerprint {
    pub database: FileFingerprint,
    pub wal: Option<FileFingerprint>,
}

/// Fingerprints `db_path` and its optional `-wal` sidecar.
pub(crate) fn database_fingerprint(db_path: &Path) -> Result<DatabaseFingerprint> {
    let database = std::fs::metadata(db_path)
        .with_context(|| format!("Failed to inspect SQLite DB at {}", db_path.display()))?;
    let wal_path = append_suffix(db_path, "-wal");
    let wal = optional_metadata(&wal_path)?;
    Ok(DatabaseFingerprint {
        database: file_fingerprint(database, db_path)?,
        wal: wal
            .map(|metadata| file_fingerprint(metadata, &wal_path))
            .transpose()?,
    })
}

/// Fingerprints an optional SQLite database and WAL dependency.
pub(crate) fn optional_database_fingerprint(db_path: &Path) -> Result<Option<DatabaseFingerprint>> {
    match std::fs::metadata(db_path) {
        Ok(_) => database_fingerprint(db_path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to inspect SQLite DB at {}", db_path.display())),
    }
}

/// Whether a reader failure is deterministic for the current database bytes.
///
/// Missing tables/columns, malformed stored JSON, and corrupt SQLite content
/// remain failures until the source fingerprint changes, so incremental scans
/// may cache their diagnostics. Open, permission, busy, and I/O failures are
/// deliberately excluded and retried on every refresh.
pub(crate) fn is_cacheable_sqlite_failure(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    [
        "no such table",
        "no such column",
        "has no column named",
        "invalid column",
        "malformed json",
        "file is not a database",
        "database disk image is malformed",
        "unsupported sqlite schema",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// Opens a source database read-only, falling back to a stable private copy.
///
/// `probe_table` must name a core table that exists in every supported schema.
/// The fallback connection is read-write so SQLite can recover the copied WAL,
/// but it points only at a private temporary copy and never at `db_path`.
pub(crate) fn with_readonly_connection<T>(
    db_path: &Path,
    probe_table: &str,
    temp_prefix: &str,
    label: &str,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    let probe_sql = format!(
        "SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)",
        quoted_identifier(probe_table)
    );
    if let Ok(conn) = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        && probe(&conn, &probe_sql).is_ok()
    {
        return f(&conn);
    }

    let copy = StableTempCopy::new(db_path, temp_prefix)?;
    let conn = Connection::open_with_flags(&copy.db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .with_context(|| {
            format!(
                "Failed to open {label} DB copy at {}",
                copy.db_path.display()
            )
        })?;
    probe(&conn, &probe_sql).with_context(|| {
        format!(
            "Failed to read {label} DB copy at {}",
            copy.db_path.display()
        )
    })?;
    f(&conn)
}

fn probe(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    conn.query_row(sql, [], |_| Ok(()))
}

fn quoted_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn file_fingerprint(metadata: Metadata, path: &Path) -> Result<FileFingerprint> {
    Ok(FileFingerprint {
        length: metadata.len(),
        modified: metadata
            .modified()
            .with_context(|| format!("Failed to inspect mtime for {}", path.display()))?,
    })
}

fn optional_metadata(path: &Path) -> Result<Option<Metadata>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to inspect SQLite WAL at {}", path.display())),
    }
}

/// A stable private copy of a SQLite database and its optional WAL.
#[derive(Debug)]
struct StableTempCopy {
    _dir: tempfile::TempDir,
    db_path: PathBuf,
}

impl StableTempCopy {
    fn new(src: &Path, temp_prefix: &str) -> Result<Self> {
        Self::new_with_hook(src, temp_prefix, |_, _| {})
    }

    fn new_with_hook(
        src: &Path,
        temp_prefix: &str,
        mut after_copy: impl FnMut(usize, &Path),
    ) -> Result<Self> {
        let file_name = src
            .file_name()
            .ok_or_else(|| anyhow!("Invalid SQLite DB path: {}", src.display()))?;

        for attempt in 0..MAX_COPY_ATTEMPTS {
            let before = database_fingerprint(src)?;
            let dir = tempfile::Builder::new()
                .prefix(temp_prefix)
                .tempdir()
                .context("Failed to create temp dir for SQLite DB copy")?;
            let db_path = dir.path().join(file_name);

            let copy_result = copy_snapshot(src, &db_path, &before);
            if copy_result.is_ok() {
                after_copy(attempt, src);
            }
            let after = database_fingerprint(src);

            match (copy_result, after) {
                (Ok(()), Ok(after)) if before == after => {
                    return Ok(Self { _dir: dir, db_path });
                }
                (Ok(()), Ok(_)) if attempt + 1 < MAX_COPY_ATTEMPTS => continue,
                (Ok(()), Ok(_)) => {
                    return Err(anyhow!(
                        "SQLite DB changed while being copied after {MAX_COPY_ATTEMPTS} attempts: {}",
                        src.display()
                    ));
                }
                (Err(_), Ok(after)) if before != after && attempt + 1 < MAX_COPY_ATTEMPTS => {
                    continue;
                }
                (Err(error), _) => return Err(error),
                (Ok(()), Err(error)) => return Err(error),
            }
        }

        unreachable!("copy loop always returns")
    }
}

fn copy_snapshot(src: &Path, dst: &Path, fingerprint: &DatabaseFingerprint) -> Result<()> {
    std::fs::copy(src, dst)
        .with_context(|| format!("Failed to copy SQLite DB from {}", src.display()))?;

    if fingerprint.wal.is_some() {
        let source_wal = append_suffix(src, "-wal");
        let destination_wal = append_suffix(dst, "-wal");
        std::fs::copy(&source_wal, &destination_wal)
            .with_context(|| format!("Failed to copy SQLite WAL from {}", source_wal.display()))?;
    }

    Ok(())
}

/// Appends a raw suffix to a path's final component.
pub(crate) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut os: OsString = path.as_os_str().to_owned();
    os.push(suffix);
    PathBuf::from(os)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn stable_copy_includes_committed_wal_rows() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        let source = Connection::open(&source_path).unwrap();
        source.pragma_update(None, "journal_mode", "WAL").unwrap();
        source.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        source
            .execute_batch(
                "CREATE TABLE entries (value INTEGER NOT NULL);\
                 PRAGMA wal_checkpoint(TRUNCATE);\
                 INSERT INTO entries VALUES (42);",
            )
            .unwrap();
        assert!(append_suffix(&source_path, "-wal").exists());

        let copy = StableTempCopy::new(&source_path, "vct-sqlite-test-").unwrap();
        let copied =
            Connection::open_with_flags(&copy.db_path, OpenFlags::SQLITE_OPEN_READ_WRITE).unwrap();
        let value: i64 = copied
            .query_row("SELECT value FROM entries", [], |row| row.get(0))
            .unwrap();

        assert_eq!(value, 42);
    }

    #[test]
    fn stable_copy_propagates_wal_copy_failure() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        Connection::open(&source_path).unwrap();
        std::fs::create_dir(append_suffix(&source_path, "-wal")).unwrap();

        let error = StableTempCopy::new(&source_path, "vct-sqlite-test-").unwrap_err();

        assert!(error.to_string().contains("Failed to copy SQLite WAL"));
    }

    #[test]
    fn stable_copy_retries_when_source_changes() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        std::fs::write(&source_path, b"before").unwrap();
        let attempts = Cell::new(0);

        let copy =
            StableTempCopy::new_with_hook(&source_path, "vct-sqlite-test-", |attempt, source| {
                attempts.set(attempts.get() + 1);
                if attempt == 0 {
                    std::fs::write(source, b"after source change").unwrap();
                }
            })
            .unwrap();

        assert_eq!(attempts.get(), 2);
        assert_eq!(std::fs::read(copy.db_path).unwrap(), b"after source change");
    }

    #[test]
    fn stable_copy_stops_after_three_source_changes() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        std::fs::write(&source_path, b"before").unwrap();
        let attempts = Cell::new(0);

        let error =
            StableTempCopy::new_with_hook(&source_path, "vct-sqlite-test-", |attempt, source| {
                attempts.set(attempts.get() + 1);
                std::fs::write(source, vec![0; attempt + 8]).unwrap();
            })
            .unwrap_err();

        assert_eq!(attempts.get(), MAX_COPY_ATTEMPTS);
        assert!(error.to_string().contains("after 3 attempts"));
    }
}
