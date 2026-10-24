//! A governed room's durable copy of its peer state, stored in parts.
//!
//! The governor's copy of a room is mostly its records' history, and a
//! governance change (a person let in, a role changed) touches little of it.
//! Writing the whole copy into the room file with every change made each
//! change cost the room's whole history, and a crowd joining a long-running
//! chat queued behind those writes. The copy is kept as the parts
//! [`StatePeer::durable_parts`] makes instead: small parts inline in the
//! room file, large ones each in a file of its own named by its content's
//! hash. A part is written once, before the room file that names it, and
//! removed once the written room file no longer names it.
//!
//! Opening a room reads every part back, checks each stored one against its
//! hash, and restores the peer from the parts' concatenation: the same bytes
//! [`StatePeer::durable`] makes, validated the same way.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tilo_peer_state::StatePeer;

/// Parts at least this large get a file of their own.
const STORED_PART: usize = 16 * 1024;

/// One part of a room's durable peer state, as the room file names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum PeerPart {
    Inline(Vec<u8>),
    /// In the room's parts directory, named by the hex of `digest`, the
    /// BLAKE3 hash of its `len` bytes.
    Stored {
        digest: [u8; 32],
        len: u64,
    },
}

/// A room's parts directory and what this process knows it holds.
pub(super) struct PeerParts {
    dir: PathBuf,
    /// Every part file in `dir`.
    on_disk: BTreeSet<[u8; 32]>,
    /// The large parts of the last state staged, by their allocation: the
    /// peer hands out the same allocation for a part nothing changed, so
    /// such a part is neither hashed nor written again. Holding them keeps
    /// their addresses from being reused.
    staged: HashMap<usize, (Arc<[u8]>, [u8; 32])>,
}

fn address(part: &Arc<[u8]>) -> usize {
    part.as_ptr() as usize
}

impl PeerParts {
    /// The parts directory of the room file at `room_path`, as found.
    pub(super) fn open(room_path: &Path) -> Result<Self, String> {
        let dir = room_path.with_extension("parts");
        let mut on_disk = BTreeSet::new();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let digest = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| hex::decode(name).ok())
                        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok());
                    match digest {
                        Some(digest) => {
                            on_disk.insert(digest);
                        }
                        // An interrupted write's temporary file, or not ours.
                        None => {
                            let _ = std::fs::remove_file(entry.path());
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(Self {
            dir,
            on_disk,
            staged: HashMap::new(),
        })
    }

    fn path(&self, digest: &[u8; 32]) -> PathBuf {
        self.dir.join(hex::encode(digest))
    }

    /// `peer`'s durable state as parts for the room file, every large part
    /// in its file before this returns.
    pub(super) fn stage(&mut self, peer: &StatePeer) -> Result<Vec<PeerPart>, String> {
        let encoding = crate::timing::start();
        let parts = peer.durable_parts()?;
        crate::timing::window("room.parts.encode", encoding);
        let writing = crate::timing::start();
        let mut written = 0u64;
        let mut staged = HashMap::new();
        let mut named = Vec::with_capacity(parts.len());
        for part in parts {
            if part.len() < STORED_PART {
                named.push(PeerPart::Inline(part.to_vec()));
                continue;
            }
            let digest = match self.staged.get(&address(&part)) {
                Some((known, digest)) if Arc::ptr_eq(known, &part) => *digest,
                _ => *blake3::hash(&part).as_bytes(),
            };
            if self.on_disk.insert(digest) {
                let stored = std::fs::create_dir_all(&self.dir)
                    .and_then(|()| tilo_files::native::atomic::write(&self.path(&digest), &part));
                if let Err(error) = stored {
                    self.on_disk.remove(&digest);
                    return Err(error.to_string());
                }
                written = written.saturating_add(part.len() as u64);
            }
            named.push(PeerPart::Stored {
                digest,
                len: part.len() as u64,
            });
            staged.insert(address(&part), (part, digest));
        }
        self.staged = staged;
        // The bytes of parts written anew: a change's own, not the room's.
        crate::timing::event("room.parts.write", writing, written);
        Ok(named)
    }

    /// The durable peer state `parts` name, whole.
    pub(super) fn load(&self, parts: &[PeerPart]) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        for part in parts {
            match part {
                PeerPart::Inline(inline) => bytes.extend_from_slice(inline),
                PeerPart::Stored { digest, len } => {
                    let stored = std::fs::read(self.path(digest))
                        .map_err(|error| format!("room state part missing: {error}"))?;
                    if stored.len() as u64 != *len || blake3::hash(&stored).as_bytes() != digest {
                        return Err("room state part does not match its hash".into());
                    }
                    bytes.extend_from_slice(&stored);
                }
            }
        }
        Ok(bytes)
    }

    /// Remove every part file `parts`, the room file as written, does not
    /// name.
    pub(super) fn collect(&mut self, parts: &[PeerPart]) {
        let named: BTreeSet<[u8; 32]> = parts
            .iter()
            .filter_map(|part| match part {
                PeerPart::Stored { digest, .. } => Some(*digest),
                PeerPart::Inline(_) => None,
            })
            .collect();
        let unnamed: Vec<[u8; 32]> = self.on_disk.difference(&named).copied().collect();
        for digest in unnamed {
            match std::fs::remove_file(self.path(&digest)) {
                Ok(()) => {
                    self.on_disk.remove(&digest);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.on_disk.remove(&digest);
                }
                // Kept: the next collection tries again.
                Err(error) => tracing::debug!(%error, "room state part not removed"),
            }
        }
    }

    #[cfg(test)]
    pub(super) fn files(&self) -> usize {
        std::fs::read_dir(&self.dir).map_or(0, Iterator::count)
    }
}
