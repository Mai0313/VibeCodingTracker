//! Ledger snapshots of other machines, and the export they are made from.
//!
//! `vct ledger export` prints one machine's sessions as a single JSON line.
//! The machine that fetched it stores each provider's part under
//! `<cache>/remotes/<name>/sessions/<provider>.json`, in the layout of a local
//! ledger file, and a scan folds those sessions after its own. A snapshot is
//! only ever read here: it is never merged into the local ledger files, never
//! marked missing, and replaced whole by the next fetch.

use super::format::{DayEntry, LedgerFile, LedgerHeader, SCHEMA_VERSION, SessionEntry};
use super::{ALL_PROVIDERS, LEDGER_DIR_NAME, SessionLedger, provider_file_stem, provider_path};
use crate::constants::{FastHashMap, FastHashSet};
use crate::models::ExtensionType;
use crate::utils::write_string_atomic;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The directory under `~/.vct` that holds one subdirectory per remote.
pub const REMOTES_DIR_NAME: &str = "remotes";

/// Version of the export line, and the key it starts with.
const EXPORT_VERSION: u32 = 1;

/// What every export line starts with, so it can be told from whatever a
/// remote shell printed before it.
pub const EXPORT_LINE_PREFIX: &str = "{\"vct_ledger_export\":";

/// One machine's sessions as `vct ledger export` prints them.
#[derive(Serialize)]
struct Export<'a> {
    vct_ledger_export: u32,
    providers: BTreeMap<&'static str, ExportFile<'a>>,
}

/// A provider's ledger file reduced to what another machine folds: the days.
/// Read stamps and working directories stay behind, and so do the session
/// keys, which spell out a project path for some providers; each is replaced
/// by its [`export_key`].
#[derive(Serialize)]
struct ExportFile<'a> {
    schema_version: u32,
    provider: &'static str,
    sessions: BTreeMap<String, ExportSession<'a>>,
}

#[derive(Serialize)]
struct ExportSession<'a> {
    days: &'a BTreeMap<String, DayEntry>,
}

#[derive(Deserialize)]
struct ImportedExport {
    vct_ledger_export: u32,
    providers: BTreeMap<String, Box<RawValue>>,
}

/// The snapshot of one remote, loaded from its directory.
pub(crate) struct RemoteSnapshot {
    /// `<cache>/remotes/<name>/sessions`.
    dir: PathBuf,
    books: FastHashMap<ExtensionType, RemoteBook>,
}

#[derive(Default)]
struct RemoteBook {
    /// Modification time and length of the file `sessions` was read from;
    /// `None` when there was no file.
    stamp: Option<(SystemTime, u64)>,
    sessions: BTreeMap<String, SessionEntry>,
}

impl SessionLedger {
    /// Writes this machine's own sessions as one export line.
    ///
    /// Snapshots fetched from this machine's own remotes are left out, so a
    /// chain of machines never counts a session twice.
    ///
    /// # Errors
    ///
    /// Returns an error if writing to `writer` fails.
    pub fn write_export(&self, mut writer: impl Write) -> Result<()> {
        let mut providers = BTreeMap::new();
        for provider in ALL_PROVIDERS {
            let sessions = self
                .providers
                .get(&provider)
                .into_iter()
                .flat_map(|book| &book.sessions)
                .filter(|(_, entry)| !entry.days.is_empty())
                .map(|(key, entry)| (export_key(key), ExportSession { days: &entry.days }))
                .collect();
            providers.insert(
                provider_file_stem(provider),
                ExportFile {
                    schema_version: SCHEMA_VERSION,
                    provider: provider_file_stem(provider),
                    sessions,
                },
            );
        }
        let export = Export {
            vct_ledger_export: EXPORT_VERSION,
            providers,
        };
        serde_json::to_writer(&mut writer, &export).context("Failed to write the ledger export")?;
        writeln!(writer).context("Failed to write the ledger export")
    }

    /// Brings the loaded remote snapshots in line with what is on disk.
    ///
    /// A provider file is re-read only when its modification time or length
    /// changed, so a refresh that finds nothing new costs a few `stat` calls.
    pub(crate) fn reload_remotes(&mut self) {
        let Some(root) = self
            .dir
            .as_deref()
            .and_then(Path::parent)
            .map(|cache| cache.join(REMOTES_DIR_NAME))
        else {
            return;
        };
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path().join(LEDGER_DIR_NAME))
            .collect();
        dirs.sort();

        let mut loaded = std::mem::take(&mut self.remotes);
        for dir in dirs {
            let mut snapshot = match loaded.iter().position(|snapshot| snapshot.dir == dir) {
                Some(index) => loaded.swap_remove(index),
                None => RemoteSnapshot {
                    dir,
                    books: FastHashMap::default(),
                },
            };
            snapshot.refresh();
            self.remotes.push(snapshot);
        }
    }

    /// Every remote session of `provider` that this machine does not hold
    /// itself, each one once.
    ///
    /// Two machines sharing a home directory hold the same sessions under the
    /// same keys, and so can two remotes; the local copy wins, then the first
    /// remote in directory order.
    pub(crate) fn remote_sessions(
        &self,
        provider: ExtensionType,
    ) -> impl Iterator<Item = &SessionEntry> {
        let books = || {
            self.remotes
                .iter()
                .filter_map(move |remote| remote.books.get(&provider))
        };
        // A snapshot names its sessions by export key, so the local ones are
        // compared under theirs, and only when there is something to compare.
        let local: FastHashSet<String> = if books().any(|book| !book.sessions.is_empty()) {
            self.providers
                .get(&provider)
                .into_iter()
                .flat_map(|book| book.sessions.keys())
                .map(|key| export_key(key))
                .collect()
        } else {
            FastHashSet::default()
        };
        let mut seen: FastHashSet<&str> = FastHashSet::default();
        books()
            .flat_map(|book| &book.sessions)
            .filter(move |(key, _)| !local.contains(*key) && seen.insert(key.as_str()))
            .map(|(_, entry)| entry)
    }
}

impl RemoteSnapshot {
    fn refresh(&mut self) {
        super::remove_stale_temp_files(&self.dir);
        for provider in ALL_PROVIDERS {
            let path = provider_path(&self.dir, provider);
            let stamp = std::fs::metadata(&path)
                .ok()
                .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())));
            let book = self.books.entry(provider).or_default();
            if book.stamp != stamp {
                book.sessions = read_snapshot(&path);
                book.stamp = stamp;
            }
        }
    }
}

/// The name a session travels under: the 128-bit FNV-1a hash of its ledger
/// key, in hex.
///
/// Two machines holding the same session derive the same name, which is all
/// deduplication needs, and the key itself never leaves the machine. The
/// function is fixed for good: changing it would stop every snapshot already
/// stored from matching its local twin.
fn export_key(key: &str) -> String {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let hash = key.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u128::from(byte)).wrapping_mul(PRIME)
    });
    format!("{hash:032x}")
}

fn read_snapshot(path: &Path) -> BTreeMap<String, SessionEntry> {
    let Ok(body) = std::fs::read(path) else {
        return BTreeMap::new();
    };
    match serde_json::from_slice::<LedgerFile>(&body) {
        Ok(file) if file.schema_version == SCHEMA_VERSION => file.sessions,
        Ok(file) => {
            log::warn!(
                "remote snapshot {} carries schema v{}, which this build does not read",
                path.display(),
                file.schema_version
            );
            BTreeMap::new()
        }
        Err(error) => {
            log::warn!("remote snapshot {} is unreadable: {error}", path.display());
            BTreeMap::new()
        }
    }
}

/// `<cache_dir>/remotes/<name>`.
pub fn remote_dir(cache_dir: &Path, name: &str) -> PathBuf {
    cache_dir.join(REMOTES_DIR_NAME).join(name)
}

/// Stores `output`, what a remote's `vct ledger export` printed, as the
/// snapshot of the remote called `name`.
///
/// The export is the last line starting with [`EXPORT_LINE_PREFIX`], so a
/// banner a remote shell prints first does no harm. A provider written in
/// another ledger layout keeps its previous snapshot.
///
/// # Errors
///
/// Returns an error, leaving the previous snapshot as it was, if `output`
/// holds no export line, the line is not a version-1 export, or a file cannot
/// be written.
pub fn store_export(cache_dir: &Path, name: &str, output: &str) -> Result<()> {
    let line = output
        .lines()
        .rev()
        .find(|line| line.starts_with(EXPORT_LINE_PREFIX))
        .context("the remote printed no ledger export")?;
    let export: ImportedExport =
        serde_json::from_str(line).context("the remote's ledger export is not valid JSON")?;
    if export.vct_ledger_export != EXPORT_VERSION {
        bail!(
            "the remote's ledger export is version {}, which this vct does not read; update vct on both machines",
            export.vct_ledger_export
        );
    }
    let dir = remote_dir(cache_dir, name).join(LEDGER_DIR_NAME);
    for provider in ALL_PROVIDERS {
        let Some(file) = export.providers.get(provider_file_stem(provider)) else {
            continue;
        };
        let header: LedgerHeader = serde_json::from_str(file.get())
            .context("the remote's ledger export holds a malformed provider")?;
        if header.schema_version != SCHEMA_VERSION {
            log::warn!(
                "remote {name}: the {} ledger uses schema v{}, this build reads v{SCHEMA_VERSION}; keeping the previous snapshot",
                provider_file_stem(provider),
                header.schema_version
            );
            continue;
        }
        write_string_atomic(provider_path(&dir, provider), file.get())?;
    }
    Ok(())
}

/// Removes the snapshot of every remote whose directory name is not in `keep`.
///
/// A snapshot is a cache of what its remote holds, so one whose host left the
/// config is deleted rather than folded forever. The caller has to know the
/// host really left: a config that failed to parse also reads as no hosts.
pub fn prune_remotes(cache_dir: &Path, keep: &[String]) {
    let Ok(entries) = std::fs::read_dir(cache_dir.join(REMOTES_DIR_NAME)) else {
        return;
    };
    for entry in entries.flatten() {
        // A symlink is not a directory here, so nothing outside is followed.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || keep.contains(&entry.file_name().to_string_lossy().into_owned())
        {
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(entry.path()) {
            log::warn!(
                "cannot remove the snapshot of an unconfigured remote {}: {error}",
                entry.path().display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_keys_are_the_published_fnv1a_128_values() {
        assert_eq!(export_key(""), "6c62272e07bb014262b821756295c58d");
        assert_eq!(export_key("a"), "d228cb696f1a8caf78912b704e4a8964");
    }
}
