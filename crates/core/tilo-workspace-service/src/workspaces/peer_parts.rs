//! A governed workspace's durable copy of its peer state, stored in parts.
//!
//! The governor's copy of a workspace is mostly its records' history, and a
//! governance change (a person let in, a role changed) touches little of it.
//! Writing the whole copy into the workspace file with every change made each
//! change cost the workspace's whole history, and a crowd joining a long-running
//! chat queued behind those writes. The copy is kept as the parts
//! [`StatePeer::durable_parts`] makes instead: small parts inline in the
//! workspace file, large ones each in a file of its own named by its content's
//! hash. A part is written once, before the workspace file that names it, and
//! removed once the written workspace file no longer names it.
//!
//! Opening a workspace reads every part back, checks each stored one against its
//! hash, and restores the peer from the parts' concatenation: the same bytes
//! [`StatePeer::durable`] makes, validated the same way.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tilo_lock::MutexRecover as _;

use serde::{Deserialize, Serialize};
use tilo_peer_state::StatePeer;

/// Parts at least this large get a file of their own.
const STORED_PART: usize = 16 * 1024;

/// One part of a workspace's durable peer state, as the workspace file names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum PeerPart {
    Inline(Vec<u8>),
    /// In the workspace's parts directory, named by the hex of `digest`, the
    /// BLAKE3 hash of its `len` bytes.
    Stored {
        digest: [u8; 32],
        len: u64,
    },
}

/// A workspace's parts directory and what this process knows it holds.
pub(super) struct PeerParts {
    files: PartFiles,
    /// The large parts of the last state named, by their allocation: the
    /// peer hands out the same allocation for a part nothing changed, so
    /// such a part is neither hashed nor written again. Holding them keeps
    /// their addresses from being reused, and keeps every part the workspace
    /// file names at hand until it is written.
    staged: HashMap<usize, (Arc<[u8]>, [u8; 32])>,
    /// The order of the last write of the workspace file handed out.
    ordered: u64,
}

fn address(part: &Arc<[u8]>) -> usize {
    part.as_ptr() as usize
}

/// A workspace's parts directory and its workspace file, written by whoever
/// holds them: under the workspace's lock, or after it, when a copy of the
/// members' writes is kept in the background ([`super::GovernedWorkspace::keep_later`]).
/// The disk can take seconds to write a large part, and nothing that reads the
/// workspace or takes a member's write in waits for that.
#[derive(Clone)]
pub(super) struct PartFiles {
    dir: PathBuf,
    on_disk: Arc<Mutex<OnDisk>>,
}

struct OnDisk {
    /// Every part file in the directory.
    parts: BTreeSet<[u8; 32]>,
    /// The order of the newest workspace file written: an older one, written
    /// late, would name parts its successor no longer holds.
    written: u64,
}

/// A large part by its digest, with its content where it is at hand.
type Stored = ([u8; 32], Option<Arc<[u8]>>);

/// A workspace file to write: its bytes, the large parts it names with their
/// content, and its place in the order the workspace's state was named in.
pub(super) struct FileWrite {
    pub(super) path: PathBuf,
    pub(super) bytes: Vec<u8>,
    order: u64,
    /// Every large part it names, with its content where it is at hand.
    stored: Vec<Stored>,
}

impl PeerParts {
    /// The parts directory of the workspace file at `workspace_path`, as found.
    pub(super) fn open(workspace_path: &Path) -> Result<Self, String> {
        let dir = workspace_path.with_extension("parts");
        let mut on_disk = BTreeSet::new();
        match std::fs::read_dir(&dir) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let digest = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| {
                            tilo_failure::ignored!(
                                "a name that is not a digest is a temporary file, removed below",
                                hex::decode(name).ok()
                            )
                        })
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
            files: PartFiles {
                dir,
                on_disk: Arc::new(Mutex::new(OnDisk {
                    parts: on_disk,
                    written: 0,
                })),
            },
            staged: HashMap::new(),
            ordered: 0,
        })
    }

    /// `peer`'s durable state as parts for the workspace file, in memory: what
    /// [`Self::stage`] writes, or a [`FileWrite`] made of them later.
    pub(super) fn name(&mut self, peer: &StatePeer) -> Result<Vec<PeerPart>, String> {
        let encoding = crate::timing::start();
        let parts = peer.durable_parts()?;
        crate::timing::window("workspace.parts.encode", encoding);
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
            named.push(PeerPart::Stored {
                digest,
                len: part.len() as u64,
            });
            staged.insert(address(&part), (part, digest));
        }
        self.staged = staged;
        Ok(named)
    }

    /// `peer`'s durable state as parts for the workspace file, every large part
    /// in its file before this returns.
    pub(super) fn stage(&mut self, peer: &StatePeer) -> Result<Vec<PeerPart>, String> {
        let named = self.name(peer)?;
        let stored = self.stored(&named);
        self.files.write_parts(&mut self.files.held(), &stored)?;
        Ok(named)
    }

    /// The workspace file `bytes`, naming `parts`, to write at `path` in the
    /// order this is called in.
    pub(super) fn file_write(
        &mut self,
        path: &Path,
        bytes: Vec<u8>,
        parts: &[PeerPart],
    ) -> FileWrite {
        self.ordered = self.ordered.saturating_add(1);
        FileWrite {
            path: path.to_owned(),
            bytes,
            order: self.ordered,
            stored: self.stored(parts),
        }
    }

    /// Whether a workspace file this named may not be written yet: one
    /// written later is on its way, or failed.
    pub(super) fn pending(&self) -> bool {
        self.ordered > self.files.held().written
    }

    /// The files the workspace's writes go to, for a write made without the
    /// workspace.
    pub(super) fn files(&self) -> PartFiles {
        self.files.clone()
    }

    /// Every large part of `parts`, with its content where the state last
    /// named holds it: a part of an earlier state is on disk already.
    fn stored(&self, parts: &[PeerPart]) -> Vec<Stored> {
        let at_hand: HashMap<&[u8; 32], &Arc<[u8]>> = self
            .staged
            .values()
            .map(|(part, digest)| (digest, part))
            .collect();
        parts
            .iter()
            .filter_map(|part| match part {
                PeerPart::Stored { digest, .. } => {
                    Some((*digest, at_hand.get(digest).map(|part| Arc::clone(part))))
                }
                PeerPart::Inline(_) => None,
            })
            .collect()
    }

    /// The durable peer state `parts` name, whole.
    pub(super) fn load(&self, parts: &[PeerPart]) -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        for part in parts {
            match part {
                PeerPart::Inline(inline) => bytes.extend_from_slice(inline),
                PeerPart::Stored { digest, len } => {
                    let stored = std::fs::read(self.files.path(digest))
                        .map_err(|error| format!("workspace state part missing: {error}"))?;
                    if stored.len() as u64 != *len || blake3::hash(&stored).as_bytes() != digest {
                        return Err("workspace state part does not match its hash".into());
                    }
                    bytes.extend_from_slice(&stored);
                }
            }
        }
        Ok(bytes)
    }

    /// Remove every part file `parts`, the workspace file as written, does not
    /// name.
    pub(super) fn collect(&self, parts: &[PeerPart]) {
        let named = parts
            .iter()
            .filter_map(|part| match part {
                PeerPart::Stored { digest, .. } => Some(*digest),
                PeerPart::Inline(_) => None,
            })
            .collect();
        self.files.collect(&mut self.files.held(), &named);
    }
    #[cfg(test)]
    pub(super) fn file_count(&self) -> usize {
        std::fs::read_dir(&self.files.dir).map_or(0, Iterator::count)
    }
}

impl PartFiles {
    /// The files, held as a write holds them: a test's slow disk.
    #[cfg(all(test, feature = "agents"))]
    pub(super) fn busy(&self) -> impl Drop + '_ {
        self.held()
    }

    fn held(&self) -> std::sync::MutexGuard<'_, OnDisk> {
        self.on_disk.lock_recover()
    }

    fn path(&self, digest: &[u8; 32]) -> PathBuf {
        self.dir.join(hex::encode(digest))
    }

    /// Write every part of `stored` not on disk yet.
    fn write_parts(&self, held: &mut OnDisk, stored: &[Stored]) -> Result<(), String> {
        let writing = crate::timing::start();
        let mut written = 0u64;
        for (digest, part) in stored {
            if held.parts.contains(digest) {
                continue;
            }
            let part = part
                .as_ref()
                .ok_or("a workspace state part is neither on disk nor at hand")?;
            std::fs::create_dir_all(&self.dir)
                .and_then(|()| tilo_files::native::atomic::write(&self.path(digest), part))
                .map_err(|error| error.to_string())?;
            held.parts.insert(*digest);
            written = written.saturating_add(part.len() as u64);
        }
        // The bytes of parts written anew: a change's own, not the workspace's.
        crate::timing::event("workspace.parts.write", writing, written);
        Ok(())
    }

    /// Write `write`: the parts it names that are not on disk, then the
    /// workspace file, then remove the parts it no longer names. A write
    /// named before one already written is skipped: that one holds what this
    /// held, and more. Whether it was written.
    pub(super) fn write(&self, write: &FileWrite) -> Result<bool, String> {
        let named = write.stored.iter().map(|(digest, _)| *digest).collect();
        let mut held = self.held();
        if write.order <= held.written {
            return Ok(false);
        }
        self.write_parts(&mut held, &write.stored)?;
        let writing = crate::timing::start();
        tilo_files::native::atomic::write(&write.path, &write.bytes)
            .map_err(|error| error.to_string())?;
        crate::timing::event("workspace.record.write", writing, write.bytes.len() as u64);
        held.written = write.order;
        self.collect(&mut held, &named);
        drop(held);
        Ok(true)
    }

    /// Remove every part file `parts`, the workspace file as written, does not
    /// name.
    fn collect(&self, held: &mut OnDisk, named: &BTreeSet<[u8; 32]>) {
        let unnamed: Vec<[u8; 32]> = held.parts.difference(named).copied().collect();
        for digest in unnamed {
            match std::fs::remove_file(self.path(&digest)) {
                Ok(()) => {
                    held.parts.remove(&digest);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    held.parts.remove(&digest);
                }
                // Kept: the next collection tries again.
                Err(error) => tracing::debug!(%error, "workspace state part not removed"),
            }
        }
    }
}
