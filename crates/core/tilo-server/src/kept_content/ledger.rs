//! What the host keeps of each workspace's content, by the member whose answer
//! brought it and the hour it came, in a file beside the content it counts.
//!
//! The limits count bytes per member, per workspace and per hour. A host that
//! kept its ledgers in memory alone forgot, on a restart, that a member had
//! already filled their share or their hour: the next write passed. So each
//! kept item is noted here as it lands, and a workspace the host opens is
//! counted back from here — what its store still holds, and nothing else — and
//! kept current as further writes land.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The directory, under the content store's, the ledgers sit in.
pub(super) const DIRECTORY: &str = "kept";

/// One piece of content the host kept: its address, the member whose answer it
/// came from, how many bytes it is, and when it came, milliseconds since the
/// Unix epoch.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Entry {
    pub(super) hash: String,
    /// The member that answered with it, lowercase hex: the person the device
    /// belongs to, or the device itself for one that belongs to nobody.
    pub(super) member: String,
    pub(super) bytes: u64,
    pub(super) at: u64,
}

/// The ledger of each workspace the host keeps content for.
pub(super) struct Ledger {
    dir: PathBuf,
}

impl Ledger {
    /// The ledgers under `dir`, beside the content they count.
    pub(super) fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Every piece the host kept for `workspace` that `holds` says it keeps
    /// still, one per address.
    pub(super) fn read(&self, workspace: &str, holds: impl Fn(&str) -> bool) -> Vec<Entry> {
        let Some(path) = self.path(workspace) else {
            return Vec::new();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let mut kept: BTreeMap<String, Entry> = BTreeMap::new();
        for line in text.lines() {
            if let Ok(entry) = serde_json::from_str::<Entry>(line) {
                kept.insert(entry.hash.clone(), entry);
            }
        }
        let written = kept.len();
        kept.retain(|hash, _| holds(hash));
        // What the store let go of is no longer counted: a ledger much longer
        // than what stands is written again, so it stays the size of the
        // content it counts.
        if kept.len().saturating_mul(2) + 32 < written {
            self.rewrite(&path, &kept);
        }
        kept.into_values().collect()
    }

    /// Note that the host kept `entry` for `workspace`.
    pub(super) fn note(&self, workspace: &str, entry: &Entry) {
        let Some(path) = self.path(workspace) else {
            return;
        };
        let Ok(line) = serde_json::to_string(entry) else {
            return;
        };
        if let Some(dir) = path.parent()
            && let Err(error) = std::fs::create_dir_all(dir)
        {
            tracing::warn!(%error, path = %path.display(), "a workspace's ledger was not made");
            return;
        }
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = writeln!(file, "{line}") {
                    tracing::warn!(%error, path = %path.display(), "what the host kept was not noted");
                }
            }
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "what the host kept was not noted");
            }
        }
    }

    /// Write the ledger again, holding only what `kept` counts.
    fn rewrite(&self, path: &Path, kept: &BTreeMap<String, Entry>) {
        let text: String = kept
            .values()
            .filter_map(|entry| serde_json::to_string(entry).ok())
            .map(|line| format!("{line}\n"))
            .collect();
        let partial = path.with_extension("partial");
        let written = std::fs::write(&partial, text).and_then(|()| std::fs::rename(&partial, path));
        if let Err(error) = written {
            tracing::warn!(%error, path = %path.display(), "a workspace's ledger was not written again");
        }
    }

    /// The file `workspace`'s ledger is kept in; `None` for an id that names no
    /// workspace (every ledger is under this directory, never another's).
    fn path(&self, workspace: &str) -> Option<PathBuf> {
        let named = !workspace.is_empty()
            && workspace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        named.then(|| self.dir.join(format!("{workspace}.jsonl")))
    }
}
