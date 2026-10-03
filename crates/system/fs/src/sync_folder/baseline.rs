//! What the last pass left both sides as, kept durably so a restart tells
//! a change made while nothing ran from a path that was always there.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::local::Stamp;

/// One version of a workspace path, as far as the folder is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mark {
    Dir,
    Empty,
    /// Opaque content by its content id.
    Content {
        hash: [u8; 32],
        size: u64,
    },
    /// A document, by the digest of its export in its legacy format.
    Document {
        digest: [u8; 32],
    },
}

/// A path both sides agreed on at the end of the last pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Synced {
    pub dir: bool,
    /// The workspace record, which a rename keeps.
    pub record: Option<[u8; 16]>,
    pub remote: Mark,
    pub local: Stamp,
}

impl Synced {
    pub fn below(path: &str, dir: &str) -> bool {
        path.strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    entries: BTreeMap<String, Synced>,
}

pub struct Baseline {
    path: PathBuf,
    pub entries: BTreeMap<String, Synced>,
}

impl Baseline {
    /// The baseline kept at `path`; empty when there is none yet.
    pub fn open(path: &Path) -> io::Result<Self> {
        let stored = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Stored>(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Stored::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: path.to_path_buf(),
            entries: stored.entries,
        })
    }

    pub fn save(&self) -> io::Result<()> {
        let stored = Stored {
            entries: self.entries.clone(),
        };
        let bytes = serde_json::to_vec(&stored).map_err(io::Error::other)?;
        tilo_files::native::atomic::write_creating_parent_with_mode(&self.path, &bytes, 0o600)
    }

    /// Moves `from` and everything below it to `to`.
    pub fn rename(&mut self, from: &str, to: &str) {
        let moved: Vec<String> = self
            .entries
            .keys()
            .filter(|path| *path == from || Synced::below(path, from))
            .cloned()
            .collect();
        for path in moved {
            if let Some(synced) = self.entries.remove(&path) {
                let rest = path.get(from.len()..).unwrap_or_default();
                self.entries.insert(format!("{to}{rest}"), synced);
            }
        }
    }
}
