//! Shared retained coverage. Callers serialize writes with the workspace lock.
//!
//! A lineage is an authenticated envelope author plus channel, never a timestamp.
//! CURRENT is the only commit point: its generation contains a checkpoint and
//! its later tail. Old generations are removed only after CURRENT is fsynced.
//! Legacy files stay readable; an opaque checkpoint cannot prove that a legacy
//! queue entry is covered merely because its filename or timestamp is older.

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::{coedit::CoeditPayload, transport::Envelope};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

/// Validate the complete frame, workspace and retained payload before using it
/// as coverage. Authentication happens at ingress, not in this disk decoder.
pub(super) fn identity(bytes: &[u8], workspace: u64) -> Option<([u8; 32], [u8; 32], bool)> {
    let prefix: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    if usize::try_from(u32::from_le_bytes(prefix)).ok()? != bytes.len().checked_sub(4)? {
        return None;
    }
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
        ..
    } = crate::transport::decode(bytes.get(4..)?).ok()?
    else {
        return None;
    };
    if workspace_id != workspace {
        return None;
    }
    let (channel, checkpoint) = match CoeditPayload::decode(&delta)? {
        CoeditPayload::Checkpoint { channel, .. } => (channel, true),
        CoeditPayload::RetainedDelta { channel, .. } => (channel, false),
    };
    Some((author_device_public_key, channel, checkpoint))
}

fn sync_dir(path: &Path) -> Result<(), StoreError> {
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn create_dir(path: &Path) -> Result<(), StoreError> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => {}
        Err(error) => return Err(error.into()),
    }
    // Also sync existing directories: a previous attempt may have returned
    // after creation but before the parent directory reached stable storage.
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn durable_write(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    atomic_file::write(path, bytes)?;
    // atomic_file's parent fsync is best-effort. Destructive retirement needs
    // an explicit, successful directory barrier before it can proceed.
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

fn generation_name(name: &str) -> bool {
    name.len() == 32 && name.bytes().all(|b| b.is_ascii_hexdigit())
}

fn current(lineage: &Path) -> Result<Option<PathBuf>, StoreError> {
    let name = match fs::read_to_string(lineage.join("CURRENT")) {
        Ok(name) => name,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !generation_name(&name) {
        return Err(invalid("invalid retained generation"));
    }
    let generation = lineage.join(name);
    if !generation.is_dir() {
        return Err(invalid("missing retained generation"));
    }
    Ok(Some(generation))
}

fn commit_generation(lineage: &Path, checkpoint: Option<&[u8]>) -> Result<PathBuf, StoreError> {
    let name = hex::encode(rand::random::<[u8; 16]>());
    let generation = lineage.join(&name);
    create_dir(&generation)?;
    if let Some(bytes) = checkpoint {
        durable_write(&generation.join("checkpoint.delta"), bytes)?;
    }
    sync_dir(&generation)?;
    durable_write(&lineage.join("CURRENT"), name.as_bytes())?;
    Ok(generation)
}

fn collect_obsolete(lineage: &Path, active: &Path) -> Result<(), StoreError> {
    // Re-establish the barrier even after reopening a previous interrupted
    // commit. Unknown files/directories are never maintenance candidates.
    fs::File::open(lineage.join("CURRENT"))?.sync_all()?;
    sync_dir(lineage)?;
    for entry in fs::read_dir(lineage)? {
        let entry = entry?;
        if entry.path() != active
            && entry.file_type()?.is_dir()
            && generation_name(&entry.file_name().to_string_lossy())
        {
            fs::remove_dir_all(entry.path())?;
        }
    }
    sync_dir(lineage)
}

impl WorkspaceStore {
    pub(super) fn retain_shared(
        &self,
        channel: &[u8; 32],
        seq: Option<u64>,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let (author, actual_channel, checkpoint) = identity(bytes, self.workspace_id)
            .ok_or_else(|| invalid("invalid retained envelope"))?;
        if actual_channel != *channel || checkpoint != seq.is_none() {
            return Err(invalid("retained envelope channel or kind mismatch"));
        }
        // WorkspaceStore::open may have just created this workspace's
        // layout. Persist the retained/workspace/workspaces directory entries
        // too, before declaring the first retained publication durable.
        for directory in self.root.ancestors().take(3) {
            sync_dir(directory)?;
        }
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        create_dir(&v2)?;
        let lineage = v2.join(format!("{}-{}", hex::encode(author), hex::encode(channel)));
        create_dir(&lineage)?;
        let active = current(&lineage)?;
        if let Some(seq) = seq {
            let active = match active {
                Some(active) => active,
                None => commit_generation(&lineage, None)?,
            };
            let hash = blake3::hash(bytes);
            durable_write(
                &active.join(format!("{seq:016x}-{}.delta", hash.to_hex())),
                bytes,
            )?;
        } else {
            // Retransmitting the current idempotent checkpoint must not
            // discard a tail received after the original transmission.
            if let Some(active) = &active {
                match fs::read(active.join("checkpoint.delta")) {
                    Ok(existing) if existing == bytes => {
                        return collect_obsolete(&lineage, active);
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let active = commit_generation(&lineage, Some(bytes))?;
            collect_obsolete(&lineage, &active)?;
        }
        Ok(())
    }

    pub(super) fn load_shared(&self) -> Result<Vec<Vec<u8>>, StoreError> {
        let v2 = self.root.join(RETAINED_DIR).join("v2");
        if !v2.exists() {
            return Ok(Vec::new());
        }
        let mut lineages = fs::read_dir(v2)?.collect::<Result<Vec<_>, _>>()?;
        lineages.sort_by_key(fs::DirEntry::file_name);
        let mut out = Vec::new();
        for lineage in lineages {
            if !lineage.file_type()?.is_dir() {
                continue;
            }
            let Some(active) = current(&lineage.path())? else {
                continue;
            };
            let checkpoint = active.join("checkpoint.delta");
            if checkpoint.exists() {
                out.push(fs::read(checkpoint)?);
            }
            let mut tail = fs::read_dir(active)?.collect::<Result<Vec<_>, _>>()?;
            tail.sort_by_key(fs::DirEntry::file_name);
            for entry in tail {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name != "checkpoint.delta"
                    && name.ends_with(".delta")
                    && !name.starts_with('.')
                    && entry.file_type()?.is_file()
                {
                    out.push(fs::read(entry.path())?);
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(checkpoint: bool, value: u8) -> Vec<u8> {
        let payload = if checkpoint {
            CoeditPayload::Checkpoint {
                channel: [9; 32],
                body: vec![value],
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body: vec![value],
            }
        };
        crate::transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    #[test]
    fn failed_generation_commit_reopens_old_checkpoint_and_tail_then_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let old = message(true, 1);
        let tail = message(false, 2);
        let new = message(true, 3);
        store.retain_checkpoint(&[9; 32], &old).unwrap();
        store.retain_delta(&[9; 32], 1, &tail).unwrap();
        let lineage = store.root().join("retained/v2").join(format!(
            "{}-{}",
            hex::encode([1; 32]),
            hex::encode([9; 32])
        ));
        let active = current(&lineage).unwrap().unwrap();
        // Real filesystem failure after the replacement generation is staged,
        // before CURRENT can be atomically committed. No production failpoint.
        let blocker = lineage.join(format!(".CURRENT.{}.tmp", std::process::id()));
        fs::create_dir(&blocker).unwrap();
        assert!(store.retain_checkpoint(&[9; 32], &new).is_err());
        drop(store);
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![old, tail]);
        assert_eq!(current(&lineage).unwrap().unwrap(), active);
        fs::remove_dir(blocker).unwrap();
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![new.clone()]);
        let next_tail = message(false, 4);
        store.retain_delta(&[9; 32], 2, &next_tail).unwrap();
        // A crash after CURRENT commits but before old-generation GC leaves
        // obsolete files. Only CURRENT may select replay data after reopening.
        fs::create_dir(&active).unwrap();
        fs::write(active.join("checkpoint.delta"), message(true, 0)).unwrap();
        fs::write(active.join("00000000.delta"), message(false, 0)).unwrap();
        fs::create_dir(lineage.join("unknown-legacy-directory")).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new.clone(), next_tail.clone()]
        );
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert!(!active.exists());
        assert!(lineage.join("unknown-legacy-directory").exists());
        assert_eq!(store.load_retained().unwrap(), vec![new, next_tail]);
    }

    #[test]
    fn retained_metadata_mismatch_never_mutates_existing_coverage() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let cp = message(true, 1);
        store.retain_checkpoint(&[9; 32], &cp).unwrap();
        assert!(store.retain_checkpoint(&[8; 32], &cp).is_err());
        assert!(store.retain_delta(&[9; 32], 1, &cp).is_err());
        assert!(store.retain_checkpoint(&[9; 32], b"unknown").is_err());
        let wrong_workspace = WorkspaceStore::open(tmp.path(), 8, false).unwrap();
        assert!(wrong_workspace.retain_checkpoint(&[9; 32], &cp).is_err());
        assert!(wrong_workspace.load_retained().unwrap().is_empty());
        assert_eq!(store.load_retained().unwrap(), vec![cp]);
    }

    #[test]
    fn same_timestamp_tails_do_not_overwrite_distinct_payloads() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = message(false, 1);
        let second = message(false, 2);
        store.retain_delta(&[9; 32], 1, &first).unwrap();
        store.retain_delta(&[9; 32], 1, &second).unwrap();
        store.retain_delta(&[9; 32], 1, &first).unwrap();
        let replay = store.load_retained().unwrap();
        assert_eq!(replay.len(), 2);
        assert!(replay.contains(&first));
        assert!(replay.contains(&second));
    }
}
