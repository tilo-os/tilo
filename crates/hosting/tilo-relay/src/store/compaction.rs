//! Replacing a document channel's retained entries with a snapshot.
//!
//! A channel's entries live in one lineage per author (`<author>-<channel>`).
//! Compaction reads every lineage's current generation without the owner
//! ([`WorkspaceStore::channel_entries`]; retained files are immutable) and
//! decides what a snapshot contains. [`WorkspaceStore::replace_channel`]
//! then runs on the persistence owner: it writes the snapshot's lineage
//! first and only afterwards gives each other lineage a generation without
//! the entries the caller named. The caller names entries by content, so an
//! entry retained after the read, or a generation rebuilt since, never loses
//! anything the snapshot does not hold. A crash between the two steps leaves
//! the snapshot and the history side by side.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use super::retained::{create_dir, current, durable_write, sync_dir};
use super::{RETAINED_DIR, StoreError, WorkspaceStore};

/// One retained file of a channel, read back.
#[derive(Clone, Debug)]
pub(crate) struct ChannelEntry {
    /// Authenticated author of the lineage it is in.
    pub author: [u8; 32],
    /// The retained co-edit body (for documents, the desktop's peer-sync
    /// message, which only the injected `ShellPayloads` adapter reads).
    pub body: Vec<u8>,
    /// Digest of the whole file, which [`WorkspaceStore::replace_channel`]
    /// removes by.
    pub digest: [u8; 32],
    /// File size.
    pub len: usize,
}

/// What to write for the channel's new snapshot.
pub(crate) struct SnapshotWrite {
    /// Author of the lineage the snapshot goes to.
    pub author: [u8; 32],
    /// Framed envelopes, the first kept as the lineage's checkpoint.
    pub frames: Vec<Vec<u8>>,
}

/// What [`WorkspaceStore::replace_channel`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Replaced {
    /// Entries removed.
    pub removed: usize,
    /// Entries left in the channel, the snapshot's own included.
    pub kept: usize,
}

pub(crate) fn file_digest(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn lineage_name(author: &[u8; 32], channel: &[u8; 32]) -> String {
    format!("{}-{}", hex::encode(author), hex::encode(channel))
}

/// Record files of a generation in replay order: the checkpoint, then the
/// tail by name.
fn record_names(generation: &Path) -> Result<Vec<OsString>, StoreError> {
    let mut names = Vec::new();
    let mut checkpoint = false;
    for entry in fs::read_dir(generation)? {
        let entry = entry?;
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if !entry.file_type()?.is_file() || text.starts_with('.') || !text.ends_with(".delta") {
            continue;
        }
        if name == "checkpoint.delta" {
            checkpoint = true;
        } else {
            names.push(name);
        }
    }
    names.sort();
    if checkpoint {
        names.insert(0, OsString::from("checkpoint.delta"));
    }
    Ok(names)
}

fn has_sidecars(generation: &Path) -> Result<bool, StoreError> {
    for entry in fs::read_dir(generation)? {
        if entry?
            .file_name()
            .to_string_lossy()
            .ends_with(".continuity-v1")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

impl WorkspaceStore {
    fn channel_lineages(&self, channel: &[u8; 32]) -> Result<Vec<(PathBuf, [u8; 32])>, StoreError> {
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        let suffix = format!("-{}", hex::encode(channel));
        let mut lineages = Vec::new();
        let entries = match fs::read_dir(&v2) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(lineages),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(author) = name
                .to_str()
                .and_then(|name| name.strip_suffix(&suffix))
                .and_then(|author| hex::decode(author).ok())
                .and_then(|author| <[u8; 32]>::try_from(author).ok())
            else {
                continue;
            };
            if entry.file_type()?.is_dir() {
                lineages.push((entry.path(), author));
            }
        }
        lineages.sort();
        Ok(lineages)
    }

    /// Every entry retained for `channel`, over all its authors. Reads only
    /// immutable files and needs no owner; a generation that disappears
    /// under the read fails it.
    ///
    /// # Errors
    ///
    /// Returns an error when a lineage cannot be read.
    pub(crate) fn channel_entries(
        &self,
        channel: &[u8; 32],
    ) -> Result<Vec<ChannelEntry>, StoreError> {
        let mut out = Vec::new();
        for (lineage, author) in self.channel_lineages(channel)? {
            let Some(active) = current(&lineage)? else {
                continue;
            };
            for name in record_names(&active)? {
                let bytes = fs::read(active.join(&name))?;
                let body = super::retained::body_of(&bytes, self.workspace_id).unwrap_or_default();
                out.push(ChannelEntry {
                    author,
                    body,
                    digest: file_digest(&bytes),
                    len: bytes.len(),
                });
            }
        }
        Ok(out)
    }

    /// Write `snapshot` (when given) as its author's lineage for `channel`,
    /// then remove every entry of the channel for which `remove` holds,
    /// given its file bytes. Must run on the persistence owner.
    ///
    /// The snapshot lineage keeps nothing but the snapshot and those of its
    /// previous entries `remove` spares. A lineage holding continuity
    /// receipts is left as it is.
    ///
    /// # Errors
    ///
    /// Returns an error when a write or directory barrier fails; what was
    /// already switched stays switched, and nothing is removed from a lineage
    /// before the snapshot is durable.
    pub(crate) fn replace_channel(
        &self,
        channel: &[u8; 32],
        snapshot: Option<SnapshotWrite>,
        remove: &dyn Fn(&[u8]) -> bool,
    ) -> Result<Replaced, StoreError> {
        self.tail_cache.invalidate();
        // Rewrites lineages of every author on the channel.
        self.forget_channel_listing(channel);
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        create_dir(&v2)?;
        let mut outcome = Replaced::default();
        let mut done: HashSet<PathBuf> = HashSet::new();
        if let Some(snapshot) = snapshot {
            let lineage = v2.join(lineage_name(&snapshot.author, channel));
            create_dir(&lineage)?;
            let survivors = match current(&lineage)? {
                Some(active) => survivors(&active, remove, &mut outcome)?,
                None => Vec::new(),
            };
            let mut frames = snapshot.frames.into_iter();
            let first = frames.next();
            let written: Vec<(String, Vec<u8>)> = first
                .map(|bytes| ("checkpoint.delta".to_owned(), bytes))
                .into_iter()
                .chain(
                    frames
                        .enumerate()
                        .map(|(index, bytes)| (format!("{index:016x}.delta"), bytes)),
                )
                .collect();
            let offset = written.len().saturating_sub(1);
            outcome.kept = outcome.kept.saturating_add(written.len());
            rewrite(self, &lineage, &written, &survivors, offset)?;
            done.insert(lineage);
        }
        for (lineage, _) in self.channel_lineages(channel)? {
            if done.contains(&lineage) {
                continue;
            }
            let Some(active) = current(&lineage)? else {
                continue;
            };
            if has_sidecars(&active)? {
                outcome.kept = outcome.kept.saturating_add(record_names(&active)?.len());
                continue;
            }
            let before = outcome.removed;
            let kept = survivors(&active, remove, &mut outcome)?;
            if outcome.removed != before {
                rewrite(self, &lineage, &[], &kept, 0)?;
            }
        }
        Ok(outcome)
    }
}

/// The records of `active` that `remove` spares, counted into `outcome`.
fn survivors(
    active: &Path,
    remove: &dyn Fn(&[u8]) -> bool,
    outcome: &mut Replaced,
) -> Result<Vec<PathBuf>, StoreError> {
    let mut survivors = Vec::new();
    for name in record_names(active)? {
        let path = active.join(&name);
        if remove(&fs::read(&path)?) {
            outcome.removed = outcome.removed.saturating_add(1);
        } else {
            outcome.kept = outcome.kept.saturating_add(1);
            survivors.push(path);
        }
    }
    Ok(survivors)
}

/// Switch `lineage` to a new generation holding `written` and then the
/// `survivors` (hard links, in order), and collect the old generations
/// (later, while a capture is leased: [`WorkspaceStore::collect_or_defer`]).
fn rewrite(
    store: &WorkspaceStore,
    lineage: &Path,
    written: &[(String, Vec<u8>)],
    survivors: &[PathBuf],
    offset: usize,
) -> Result<(), StoreError> {
    let name = hex::encode(rand::random::<[u8; 16]>());
    let generation = lineage.join(&name);
    create_dir(&generation)?;
    for (file, bytes) in written {
        durable_write(&generation.join(file), bytes)?;
    }
    for (index, source) in survivors.iter().enumerate() {
        let target = if source
            .file_name()
            .is_some_and(|name| name == "checkpoint.delta")
            && written.is_empty()
        {
            generation.join("checkpoint.delta")
        } else {
            generation.join(format!("{:016x}.delta", index.saturating_add(offset)))
        };
        fs::hard_link(source, target)?;
    }
    sync_dir(&generation)?;
    durable_write(&lineage.join("CURRENT"), name.as_bytes())?;
    if let Some(mut gc) = store.collect_or_defer(lineage.to_path_buf(), false)? {
        while matches!(
            gc.step(super::bounded::WorkLimit::default())?,
            super::bounded::Step::More
        ) {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coedit::CoeditPayload;

    fn frame(author: u8, checkpoint: bool, body: &[u8]) -> Vec<u8> {
        let payload = if checkpoint {
            CoeditPayload::Checkpoint {
                channel: [9; 32],
                body: body.to_vec(),
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body: body.to_vec(),
            }
        };
        crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [author; 32],
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    #[test]
    fn replacing_removes_only_named_content_and_keeps_what_arrived_after_the_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        for (seq, (author, body)) in [(1, b"a1"), (2, b"a2"), (1, b"a3")].into_iter().enumerate() {
            store
                .retain_delta(&[9; 32], seq as u64, &frame(author, false, body))
                .unwrap();
        }
        let read = store.channel_entries(&[9; 32]).unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].body, b"a1");
        // Retained after the read: never removed, even by an author the
        // snapshot folded.
        store
            .retain_delta(&[9; 32], 9, &frame(1, false, b"late"))
            .unwrap();
        let folded: HashSet<[u8; 32]> = read.iter().map(|entry| entry.digest).collect();
        let snapshot = frame(5, true, b"snapshot");
        let outcome = store
            .replace_channel(
                &[9; 32],
                Some(SnapshotWrite {
                    author: [5; 32],
                    frames: vec![snapshot.clone(), frame(5, false, b"segment")],
                }),
                &|bytes| folded.contains(&file_digest(bytes)),
            )
            .unwrap();
        assert_eq!(
            outcome,
            Replaced {
                removed: 3,
                kept: 3
            }
        );
        let left: Vec<Vec<u8>> = store
            .channel_entries(&[9; 32])
            .unwrap()
            .into_iter()
            .map(|entry| entry.body)
            .collect();
        assert_eq!(
            left,
            vec![b"late".to_vec(), b"snapshot".to_vec(), b"segment".to_vec()]
        );
        // Appends after a rewrite continue after the survivors.
        store
            .retain_delta(&[9; 32], 10, &frame(1, false, b"next"))
            .unwrap();
        let bodies: Vec<Vec<u8>> = store
            .channel_entries(&[9; 32])
            .unwrap()
            .into_iter()
            .map(|entry| entry.body)
            .collect();
        assert_eq!(bodies[..2], [b"late".to_vec(), b"next".to_vec()]);
        let replayed = WorkspaceStore::open(tmp.path(), 7, false)
            .unwrap()
            .load_retained()
            .unwrap();
        assert!(replayed.contains(&snapshot));
        assert_eq!(replayed.len(), 4);
    }

    #[test]
    fn a_second_snapshot_replaces_the_first_only_when_named() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = frame(5, true, b"first");
        store
            .replace_channel(
                &[9; 32],
                Some(SnapshotWrite {
                    author: [5; 32],
                    frames: vec![first.clone()],
                }),
                &|_| false,
            )
            .unwrap();
        let second = frame(5, true, b"second");
        store
            .replace_channel(
                &[9; 32],
                Some(SnapshotWrite {
                    author: [5; 32],
                    frames: vec![second],
                }),
                &|_| false,
            )
            .unwrap();
        let bodies: Vec<Vec<u8>> = store
            .channel_entries(&[9; 32])
            .unwrap()
            .into_iter()
            .map(|entry| entry.body)
            .collect();
        assert_eq!(bodies, vec![b"second".to_vec(), b"first".to_vec()]);
        let digest = file_digest(&first);
        store
            .replace_channel(
                &[9; 32],
                Some(SnapshotWrite {
                    author: [5; 32],
                    frames: vec![frame(5, true, b"third")],
                }),
                &|bytes| file_digest(bytes) == digest,
            )
            .unwrap();
        let bodies: Vec<Vec<u8>> = store
            .channel_entries(&[9; 32])
            .unwrap()
            .into_iter()
            .map(|entry| entry.body)
            .collect();
        assert_eq!(bodies, vec![b"third".to_vec(), b"second".to_vec()]);
    }
}
