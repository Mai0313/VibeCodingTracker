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

/// Fingerprints `db_path` and its optional `-wal` sidecar, returning `Ok(None)` if `db_path` does not exist.
pub(crate) fn optional_database_fingerprint(db_path: &Path) -> Result<Option<DatabaseFingerprint>> {
    match std::fs::metadata(db_path) {
        Ok(_) => database_fingerprint(db_path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("Failed to inspect SQLite DB at {}", db_path.display())),
    }
}

/// Returns whether `error` is deterministic for the current database bytes (missing schema, malformed JSON, or corruption) rather than transient I/O or lock contention.
///
/// Matches against the flattened error chain (`{error:#}`); callers displaying cached failures should format with `{:#}` as well.
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
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

/// Runs `f` against `db_path` read-only, falling back to a read-write connection on a private temporary copy so SQLite can recover a copied WAL without touching `db_path`.
///
/// Cleanly closed WAL-mode databases (header bytes `18..20 == [2, 2]` with no `-wal` sidecar) are opened via `file:<uri>?mode=ro&immutable=1` bracketed by pre/post fingerprints so SQLite does not create `-wal`/`-shm` sidecars in the user's directory; concurrent writes trigger fallback to the private temporary copy.
///
/// `probe_table` must exist in every supported schema.
pub(crate) fn with_readonly_connection<T>(
    db_path: &Path,
    probe_table: &str,
    temp_prefix: &str,
    label: &str,
    f: impl FnMut(&Connection) -> Result<T>,
) -> Result<T> {
    with_readonly_connection_inner(db_path, probe_table, temp_prefix, label, f, || {})
}

fn with_readonly_connection_inner<T>(
    db_path: &Path,
    probe_table: &str,
    temp_prefix: &str,
    label: &str,
    mut f: impl FnMut(&Connection) -> Result<T>,
    after_immutable_read: impl FnOnce(),
) -> Result<T> {
    let probe_sql = format!(
        "SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)",
        quoted_identifier(probe_table)
    );

    if let Ok(before) = database_fingerprint(db_path)
        && before.wal.is_none()
        && header_is_wal(db_path)
    {
        if let Some(uri) = sqlite_immutable_uri(db_path) {
            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            if let Ok(conn) = Connection::open_with_flags(&uri, flags) {
                let _ = conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA query_only = ON;");
                if probe(&conn, &probe_sql).is_ok() {
                    let result = f(&conn);
                    drop(conn);
                    after_immutable_read();
                    if let Ok(after) = database_fingerprint(db_path)
                        && after == before
                    {
                        return result;
                    }
                }
            }
        }
        // Skip plain read-only open here so a cleanly closed WAL DB never gets sidecars created on disk; fall straight through to `StableTempCopy`.
    } else {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        if let Ok(conn) = Connection::open_with_flags(db_path, flags) {
            let _ = conn.execute_batch("PRAGMA temp_store = MEMORY; PRAGMA query_only = ON;");
            if probe(&conn, &probe_sql).is_ok() {
                return f(&conn);
            }
        }
    }

    // Name `db_path` in error context rather than the temp copy, which is deleted on return.
    let copy = StableTempCopy::new(db_path, temp_prefix)?;
    let rw_flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(&copy.db_path, rw_flags).with_context(|| {
        format!(
            "Failed to open a copy of the {label} DB at {}",
            db_path.display()
        )
    })?;
    let _ = conn.execute_batch("PRAGMA temp_store = MEMORY;");
    probe(&conn, &probe_sql).with_context(|| {
        format!(
            "Failed to read a copy of the {label} DB at {}",
            db_path.display()
        )
    })?;
    f(&conn)
}

/// Returns whether `db_path`'s SQLite header marks WAL format (`bytes 18..20 == [2, 2]`).
fn header_is_wal(db_path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(db_path) else {
        return false;
    };
    let mut header = [0u8; 20];
    if std::io::Read::read_exact(&mut file, &mut header).is_err() {
        return false;
    }
    &header[..16] == b"SQLite format 3\0" && header[18..20] == [2, 2]
}

/// Builds a `file:...` SQLite URI with `mode=ro&immutable=1`, percent-encoding URI delimiters and normalizing Windows separators.
fn sqlite_immutable_uri(db_path: &Path) -> Option<String> {
    sqlite_immutable_uri_from_str(db_path.to_str()?, cfg!(windows))
}

fn sqlite_immutable_uri_from_str(raw: &str, is_windows: bool) -> Option<String> {
    let mut uri = String::with_capacity(raw.len() + 32);
    let body = if is_windows {
        let stripped = raw.strip_prefix(r"\\?\").unwrap_or(raw);
        if stripped.starts_with(r"\\")
            || stripped.starts_with("UNC\\")
            || stripped.starts_with("//")
        {
            return None;
        }
        let bytes = stripped.as_bytes();
        if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
            uri.push_str("file:///");
        } else if stripped.starts_with('/') || stripped.starts_with('\\') {
            uri.push_str("file://");
        } else {
            uri.push_str("file:");
        }
        stripped
    } else if raw.starts_with('/') {
        // Empty authority (`file://`) + leading `/` keeps paths starting with `//` out of the URI authority slot.
        uri.push_str("file://");
        raw
    } else {
        uri.push_str("file:");
        raw
    };

    for ch in body.chars() {
        match ch {
            '\\' if is_windows => uri.push('/'),
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3F"),
            '#' => uri.push_str("%23"),
            other => uri.push(other),
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    Some(uri)
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
    fn a_probe_failure_keeps_its_cause_and_names_the_source() {
        let source_dir = tempfile::tempdir().unwrap();
        let source_path = source_dir.path().join("source.db");
        let source = Connection::open(&source_path).unwrap();
        source
            .execute_batch("CREATE TABLE other (value INTEGER NOT NULL);")
            .unwrap();
        drop(source);

        let error = with_readonly_connection(
            &source_path,
            "session",
            "vct-sqlite-test-",
            "Example",
            |_| Ok(()),
        )
        .unwrap_err();

        // Plain `Display` stops at the outer context; `{:#}` exposes the underlying SQLite cause.
        let flattened = format!("{error:#}");
        assert!(flattened.contains("no such table"), "{flattened}");
        assert!(
            flattened.contains(&source_path.display().to_string()),
            "{flattened}"
        );
        assert!(is_cacheable_sqlite_failure(&error));
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

    #[test]
    fn cleanly_closed_wal_db_creates_no_sidecar_files() {
        let dir = tempfile::tempdir().unwrap();
        // Include URI-special characters (`%`, `#`, spaces, and `?` on Unix where legal in filenames).
        let file_name = if cfg!(windows) {
            "wal %20#1.db"
        } else {
            "wal %20#1?v=2.db"
        };
        let db_path = dir.path().join(file_name);
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch(
                "CREATE TABLE entries (value INTEGER NOT NULL); \
                 INSERT INTO entries VALUES (7), (11);",
            )
            .unwrap();
        }
        let wal_path = append_suffix(&db_path, "-wal");
        let shm_path = append_suffix(&db_path, "-shm");
        assert!(header_is_wal(&db_path));
        assert!(!wal_path.exists());
        assert!(!shm_path.exists());

        let sum: i64 = with_readonly_connection(
            &db_path,
            "entries",
            "vct-sqlite-test-",
            "Example",
            |conn| {
                Ok(conn.query_row("SELECT SUM(value) FROM entries", [], |row| row.get(0))?)
            },
        )
        .unwrap();

        assert_eq!(sum, 18);
        assert!(
            !wal_path.exists(),
            "read-only open must not create a -wal sidecar"
        );
        assert!(
            !shm_path.exists(),
            "read-only open must not create a -shm sidecar"
        );
    }

    #[test]
    fn active_wal_db_reads_uncheckpointed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("active.db");
        let writer = Connection::open(&db_path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        writer
            .execute_batch(
                "CREATE TABLE entries (value INTEGER NOT NULL); \
                 INSERT INTO entries VALUES (10); \
                 PRAGMA wal_checkpoint(TRUNCATE); \
                 INSERT INTO entries VALUES (32);",
            )
            .unwrap();
        assert!(append_suffix(&db_path, "-wal").exists());

        let sum: i64 = with_readonly_connection(
            &db_path,
            "entries",
            "vct-sqlite-test-",
            "Example",
            |conn| {
                Ok(conn.query_row("SELECT SUM(value) FROM entries", [], |row| row.get(0))?)
            },
        )
        .unwrap();
        assert_eq!(sum, 42);

        // Even if the DB is closed leaving a `-wal` without `-shm` (copied into a directory), `StableTempCopy` recovers the uncheckpointed rows.
        let orphan_dir = tempfile::tempdir().unwrap();
        let orphan_db = orphan_dir.path().join("orphan.db");
        std::fs::copy(&db_path, &orphan_db).unwrap();
        std::fs::copy(
            append_suffix(&db_path, "-wal"),
            append_suffix(&orphan_db, "-wal"),
        )
        .unwrap();
        drop(writer);
        assert!(!append_suffix(&orphan_db, "-shm").exists());

        let recovered_sum: i64 = with_readonly_connection(
            &orphan_db,
            "entries",
            "vct-sqlite-test-",
            "Example",
            |conn| {
                Ok(conn.query_row("SELECT SUM(value) FROM entries", [], |row| row.get(0))?)
            },
        )
        .unwrap();
        assert_eq!(recovered_sum, 42);
    }

    #[test]
    fn concurrent_wal_write_during_immutable_read_falls_back_to_stable_copy() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("race.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch(
                "CREATE TABLE entries (value INTEGER NOT NULL); \
                 INSERT INTO entries VALUES (1);",
            )
            .unwrap();
        }
        assert!(!append_suffix(&db_path, "-wal").exists());

        let calls = Cell::new(0usize);
        let mut live_writer: Option<Connection> = None;
        let sum: i64 = with_readonly_connection_inner(
            &db_path,
            "entries",
            "vct-sqlite-test-",
            "Example",
            |conn| {
                calls.set(calls.get() + 1);
                Ok(conn.query_row("SELECT SUM(value) FROM entries", [], |row| row.get(0))?)
            },
            || {
                let writer = Connection::open(&db_path).unwrap();
                writer
                    .pragma_update(None, "wal_autocheckpoint", 0)
                    .unwrap();
                writer
                    .execute("INSERT INTO entries VALUES (99)", [])
                    .unwrap();
                live_writer = Some(writer);
            },
        )
        .unwrap();
        drop(live_writer);

        assert_eq!(
            calls.get(),
            2,
            "immutable read must retry via StableTempCopy when -wal appears mid-read"
        );
        assert_eq!(sum, 100);
    }

    #[test]
    fn sqlite_immutable_uri_escapes_delimiters_and_windows_paths() {
        assert_eq!(
            sqlite_immutable_uri_from_str("/tmp/a%20b?c#d.db", false).as_deref(),
            Some("file:///tmp/a%2520b%3Fc%23d.db?mode=ro&immutable=1")
        );
        assert_eq!(
            sqlite_immutable_uri_from_str("//tmp/a.db", false).as_deref(),
            Some("file:////tmp/a.db?mode=ro&immutable=1")
        );
        assert_eq!(
            sqlite_immutable_uri_from_str("rel/a.db", false).as_deref(),
            Some("file:rel/a.db?mode=ro&immutable=1")
        );
        assert_eq!(
            sqlite_immutable_uri_from_str(r"C:\Users\me\a%b#c.db", true).as_deref(),
            Some("file:///C:/Users/me/a%25b%23c.db?mode=ro&immutable=1")
        );
        assert_eq!(
            sqlite_immutable_uri_from_str(r"\\?\C:\Users\me\a.db", true).as_deref(),
            Some("file:///C:/Users/me/a.db?mode=ro&immutable=1")
        );
        assert_eq!(
            sqlite_immutable_uri_from_str(r"dir\a.db", true).as_deref(),
            Some("file:dir/a.db?mode=ro&immutable=1")
        );
        assert_eq!(
            sqlite_immutable_uri_from_str(r"\\server\share\a.db", true),
            None
        );
        assert_eq!(
            sqlite_immutable_uri_from_str(r"\\?\UNC\server\share\a.db", true),
            None
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn cleanly_closed_wal_non_utf8_path_falls_back_without_sidecars() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("seed.db");
        {
            let conn = Connection::open(&src_path).unwrap();
            conn.pragma_update(None, "journal_mode", "WAL").unwrap();
            conn.execute_batch(
                "CREATE TABLE entries (value INTEGER NOT NULL); \
                 INSERT INTO entries VALUES (7);",
            )
            .unwrap();
        }
        let non_utf8_path = dir
            .path()
            .join(OsString::from_vec(b"closed-\xff.db".to_vec()));
        std::fs::rename(&src_path, &non_utf8_path).unwrap();
        assert!(sqlite_immutable_uri(&non_utf8_path).is_none());
        assert!(header_is_wal(&non_utf8_path));
        assert!(!append_suffix(&non_utf8_path, "-wal").exists());

        let sum: i64 = with_readonly_connection(
            &non_utf8_path,
            "entries",
            "vct-sqlite-test-",
            "Example",
            |conn| Ok(conn.query_row("SELECT SUM(value) FROM entries", [], |row| row.get(0))?),
        )
        .unwrap();
        assert_eq!(sum, 7);
        assert!(
            !append_suffix(&non_utf8_path, "-wal").exists(),
            "non-UTF-8 cleanly closed WAL DB must fall back to StableTempCopy without creating -wal"
        );
        assert!(
            !append_suffix(&non_utf8_path, "-shm").exists(),
            "non-UTF-8 cleanly closed WAL DB must fall back to StableTempCopy without creating -shm"
        );
    }
}
