//! Shared retained coverage. Callers serialize writes with the workspace lock.
//!
//! A lineage is an authenticated envelope author plus channel, never a timestamp.
//! CURRENT selects durable shared replay coverage. Opaque records append in
//! receipt order; only proven raw Automerge ancestry permits collection. A
//! replacement generation is collected only after its CURRENT selector is synced.
//! Legacy files stay readable; an opaque checkpoint cannot prove that a legacy
//! queue entry is covered merely because its filename or timestamp is older.

use super::{RETAINED_DIR, StoreError, WorkspaceStore};
use crate::{coedit::CoeditPayload, transport::Envelope};
use automerge::ReadDoc;
use std::{
    fs, io,
    path::{Path, PathBuf},
};

fn invalid(message: &str) -> StoreError {
    io::Error::new(io::ErrorKind::InvalidData, message).into()
}

/// Validate the complete frame, workspace and retained payload before using it
/// as coverage. Authentication happens at ingress, not in this disk decoder.
fn payload(bytes: &[u8], workspace: u64) -> Option<([u8; 32], CoeditPayload)> {
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
    Some((author_device_public_key, CoeditPayload::decode(&delta)?))
}

pub(super) fn identity(bytes: &[u8], workspace: u64) -> Option<([u8; 32], [u8; 32], bool)> {
    let (author, payload) = payload(bytes, workspace)?;
    let (channel, checkpoint) = match payload {
        CoeditPayload::Checkpoint { channel, .. } => (channel, true),
        CoeditPayload::RetainedDelta { channel, .. } => (channel, false),
    };
    Some((author, channel, checkpoint))
}

fn checkpoint_key(bytes: &[u8], workspace: u64) -> Option<blake3::Hash> {
    let (author, CoeditPayload::Checkpoint { channel, body }) = payload(bytes, workspace)? else {
        return None;
    };
    let mut hash = blake3::Hasher::new();
    hash.update(b"qos-relay-retained-checkpoint-body-v1\0");
    hash.update(&author);
    hash.update(&channel);
    hash.update(&body);
    Some(hash.finalize())
}

/// Frame the pinned Automerge chunk format only; Automerge itself validates
/// checksums, columns, operations and document heads. Unknown future formats
/// fail closed. The length must be canonical ULEB128 with checked arithmetic.
fn first_chunk(bytes: &[u8]) -> Option<(&[u8], &[u8], u8)> {
    if !bytes.starts_with(&[0x85, 0x6f, 0x4a, 0x83]) {
        return None;
    }
    let kind = *bytes.get(8)?;
    if kind > 3 {
        return None;
    }
    let mut header_len = 9usize;
    let mut data_len = 0usize;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(header_len)?;
        header_len = header_len.checked_add(1)?;
        let low = usize::from(byte & 0x7f);
        data_len = data_len.checked_add(low.checked_mul(1usize.checked_shl(shift)?)?)?;
        if byte & 0x80 == 0 {
            if shift != 0 && low == 0 {
                return None;
            }
            break;
        }
        shift = shift.checked_add(7)?;
    }
    let (chunk, rest) = bytes.split_at_checked(header_len.checked_add(data_len)?)?;
    Some((chunk, rest, kind))
}

/// Every document chunk must be reconstructed in isolation, otherwise the
/// loader may skip an appended document whose declared heads are already known.
/// Valid incremental changes/bundles may lack their dependencies in isolation;
/// the later containment check resolves those against the candidate snapshot.
fn independently_valid_chunks(mut bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    while !bytes.is_empty() {
        let Some((chunk, rest, kind)) = first_chunk(bytes) else {
            return false;
        };
        match automerge::Automerge::load(chunk) {
            Ok(_) => {}
            Err(automerge::AutomergeError::MissingDeps) if kind != 0 => {}
            Err(_) => return false,
        }
        bytes = rest;
    }
    true
}

/// Only complete, strictly decoded raw Automerge snapshots prove coverage.
/// In particular `load_incremental` is unsuitable here: it accepts partial
/// input, so unchanged heads after that operation can hide malformed data.
fn covers(checkpoint: &[u8], prior: &[u8], workspace: u64) -> bool {
    let Some((author, CoeditPayload::Checkpoint { channel, body })) =
        payload(checkpoint, workspace)
    else {
        return false;
    };
    let Some((old_author, old_payload)) = payload(prior, workspace) else {
        return false;
    };
    let (old_channel, old_body) = match old_payload {
        CoeditPayload::Checkpoint { channel, body }
        | CoeditPayload::RetainedDelta { channel, body } => (channel, body),
    };
    if author != old_author || channel != old_channel || body.is_empty() || old_body.is_empty() {
        return false;
    }
    if !independently_valid_chunks(&body) || !independently_valid_chunks(&old_body) {
        return false;
    }
    let Ok(document) = automerge::Automerge::load(&body) else {
        return false;
    };
    if !document.get_missing_deps(&[]).is_empty() {
        return false;
    }
    let heads = document.get_heads();
    // A full snapshot provides dependencies for incremental tail chunks.
    // Strict load rejects truncated/opaque suffixes; checking missing deps
    // rejects well-formed changes whose unknown ancestors cannot be applied.
    let mut combined = document.save();
    combined.extend_from_slice(&old_body);
    automerge::Automerge::load(&combined).is_ok_and(|combined| {
        combined.get_missing_deps(&[]).is_empty() && combined.get_heads() == heads
    })
}

struct Record {
    path: PathBuf,
    bytes: Vec<u8>,
}

fn records(generation: &Path) -> Result<Vec<Record>, StoreError> {
    let mut paths = fs::read_dir(generation)?.collect::<Result<Vec<_>, _>>()?;
    paths.sort_by_key(fs::DirEntry::file_name);
    let mut out = Vec::new();
    let checkpoint = generation.join("checkpoint.delta");
    if checkpoint.exists() {
        out.push(Record {
            bytes: fs::read(&checkpoint)?,
            path: checkpoint,
        });
    }
    for entry in paths {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name != "checkpoint.delta"
            && name.ends_with(".delta")
            && !name.starts_with('.')
            && entry.file_type()?.is_file()
        {
            out.push(Record {
                bytes: fs::read(entry.path())?,
                path: entry.path(),
            });
        }
    }
    Ok(out)
}

fn next_record_path(generation: &Path, entries: &[Record]) -> Result<PathBuf, StoreError> {
    let last = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.path.file_name()?.to_str()?;
            let prefix = name.get(..16)?;
            u64::from_str_radix(prefix, 16).ok()
        })
        .max()
        .unwrap_or(0);
    let next = last
        .checked_add(1)
        .ok_or_else(|| invalid("retained record sequence exhausted"))?;
    Ok(generation.join(format!("{next:016x}.delta")))
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

fn commit_generation(
    lineage: &Path,
    retained: &[&Record],
    checkpoint: Option<&[u8]>,
) -> Result<PathBuf, StoreError> {
    let name = hex::encode(rand::random::<[u8; 16]>());
    let generation = lineage.join(&name);
    create_dir(&generation)?;
    // A proven raw baseline precedes uncovered dependent tails. Survivors
    // keep their relative order, and hard links avoid copying immutable bytes.
    // Opaque checkpoint histories never enter this replacement path.
    if let Some(bytes) = checkpoint {
        durable_write(&generation.join("checkpoint.delta"), bytes)?;
    }
    for (index, entry) in retained.iter().enumerate() {
        fs::hard_link(&entry.path, generation.join(format!("{index:016x}.delta")))?;
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
    ) -> Result<u64, StoreError> {
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
        let active = match current(&lineage)? {
            Some(active) => active,
            None => commit_generation(&lineage, &[], None)?,
        };
        let entries = records(&active)?;
        let mut written = 0;
        if seq.is_some() {
            // Delta identity includes the entire envelope; never collapse two
            // different publications just because their bodies happen to match.
            if !entries.iter().any(|entry| entry.bytes == bytes) {
                durable_write(&next_record_path(&active, &entries)?, bytes)?;
                written = bytes.len() as u64;
            }
        } else {
            let key = checkpoint_key(bytes, self.workspace_id);
            if entries.iter().any(|entry| {
                checkpoint_key(&entry.bytes, self.workspace_id).is_some_and(|old| Some(old) == key)
                    || covers(&entry.bytes, bytes, self.workspace_id)
            }) {
                // This also recognizes older snapshot retries after their
                // original files were collected: CURRENT proves their ancestry.
                collect_obsolete(&lineage, &active)?;
                return Ok(0);
            }
            let has_opaque_checkpoint = entries.iter().any(|entry| {
                checkpoint_key(&entry.bytes, self.workspace_id).is_some()
                    && !covers(&entry.bytes, &entry.bytes, self.workspace_id)
            });
            if covers(bytes, bytes, self.workspace_id) && !has_opaque_checkpoint {
                let uncovered: Vec<_> = entries
                    .iter()
                    .filter(|entry| !covers(bytes, &entry.bytes, self.workspace_id))
                    .collect();
                let active = commit_generation(&lineage, &uncovered, Some(bytes))?;
                written = bytes.len() as u64;
                collect_obsolete(&lineage, &active)?;
            } else {
                // No proof, or an opaque checkpoint whose relative replay
                // order must be preserved: append once without any collection.
                durable_write(&next_record_path(&active, &entries)?, bytes)?;
                written = bytes.len() as u64;
            }
        }
        Ok(written)
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
            out.extend(records(&active)?.into_iter().map(|entry| entry.bytes));
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

    fn frame(checkpoint: bool, body: Vec<u8>, timestamp: u64) -> Vec<u8> {
        let payload = if checkpoint {
            CoeditPayload::Checkpoint {
                channel: [9; 32],
                body,
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body,
            }
        };
        crate::transport::encode(&Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: timestamp,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    // Two one-change snapshots from the review probe: the malformed document
    // keeps its unrelated operations but declares the candidate's head. Its
    // four-byte SHA-256 chunk checksum was recomputed after replacing that head.
    // Fixed bytes avoid a test-only hashing dependency or an external fixture.
    fn inconsistent_heads_fixture() -> (Vec<u8>, Vec<u8>) {
        (hex::decode("856f4a83c3fea776007601100f75efc4a706295a6e71caea86e1a64e017a07c67c94f7ee52107ad9ad2e5ea2b79cc169c7a806544ae3fbd291f52ea14d0601020302130223024002560208150b2102230234014202560257018001027f007f017f017f007f007f077f0963616e6469646174657f007f01017f017f14017f0000").unwrap(), hex::decode("856f4a8360dd956a00760110fb33a1cfd10ffd7d878aa85a2d64767c017a07c67c94f7ee52107ad9ad2e5ea2b79cc169c7a806544ae3fbd291f52ea14d0601020302130223024002560208150b2102230234014202560257018001027f007f017f017f007f007f077f09756e636f76657265647f007f01017f017f14027f0000").unwrap())
    }

    #[test]
    fn chunk_framing_rejects_unknown_truncated_overflow_and_noncanonical_lengths() {
        let (valid, _) = inconsistent_heads_fixture();
        assert!(first_chunk(&valid).is_some());
        let mut unknown = valid.clone();
        unknown[8] = 4;
        let mut noncanonical = valid.clone();
        assert!(noncanonical[9] < 128);
        noncanonical[9] |= 0x80;
        noncanonical.insert(10, 0);
        let mut overflow = valid[..9].to_vec();
        overflow.extend_from_slice(&[0xff; 10]);
        let mut unterminated = valid[..9].to_vec();
        unterminated.push(0x80);
        let mut wrong_magic = valid.clone();
        wrong_magic[0] = 0;
        for invalid in [
            unknown,
            noncanonical,
            overflow,
            unterminated,
            wrong_magic,
            valid[..8].to_vec(),
            valid[..valid.len() - 1].to_vec(),
        ] {
            assert!(first_chunk(&invalid).is_none());
        }
        let mut trailing = valid;
        trailing.push(0);
        assert!(
            !independently_valid_chunks(&trailing),
            "all bytes must belong to independently valid chunks"
        );
    }

    #[test]
    fn coverage_accepts_document_change_compressed_change_and_bundle_chunks() {
        use automerge::transaction::Transactable;
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "anchor", 1).unwrap();
        let baseline = frame(true, document.save(), 1);
        document
            .put(automerge::ROOT, "large", "compressible".repeat(1024))
            .unwrap();
        let mut change = document.get_last_local_change().unwrap();
        let raw = change.raw_bytes().to_vec();
        let compressed = change.bytes().to_vec();
        let bundle = document.bundle([change.hash()]).unwrap().bytes().to_vec();
        let snapshot = document.save();
        let checkpoint = frame(true, snapshot.clone(), 3);
        for (kind, body) in [(0, snapshot), (1, raw), (2, compressed), (3, bundle)] {
            assert_eq!(
                body[8], kind,
                "fixture must exercise the advertised chunk kind"
            );
            if kind != 0 {
                assert!(
                    matches!(
                        automerge::Automerge::load(&body),
                        Err(automerge::AutomergeError::MissingDeps)
                    ),
                    "incremental fixture must genuinely require baseline dependencies"
                );
            }
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            store.retain_checkpoint(&[9; 32], &baseline).unwrap();
            store
                .retain_delta(&[9; 32], 2, &frame(false, body, 2))
                .unwrap();
            store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            assert_eq!(store.load_retained().unwrap(), vec![checkpoint.clone()]);
        }
    }

    #[test]
    fn malformed_prior_document_is_not_covered_even_after_a_valid_prefix() {
        let (candidate, malformed) = inconsistent_heads_fixture();
        assert!(automerge::Automerge::load(&candidate).is_ok());
        assert!(automerge::Automerge::load(&malformed).is_err());
        for valid_prefix in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let mut prior = if valid_prefix {
                candidate.clone()
            } else {
                Vec::new()
            };
            prior.extend_from_slice(&malformed);
            let prior_frame = frame(false, prior, 1);
            store.retain_delta(&[9; 32], 1, &prior_frame).unwrap();
            let checkpoint = frame(true, candidate.clone(), 2);
            store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let replay = store.load_retained().unwrap();
            assert!(
                replay.contains(&prior_frame),
                "malformed prior was collected through known-head fast path (prefix={valid_prefix})"
            );
            assert!(replay.contains(&checkpoint));
        }
    }

    #[test]
    fn malformed_candidate_suffix_cannot_be_omitted_as_a_covered_retry() {
        let (candidate, malformed) = inconsistent_heads_fixture();
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = frame(true, candidate.clone(), 1);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        let mut combined = candidate;
        combined.extend_from_slice(&malformed);
        let unknown = frame(true, combined, 2);
        store.retain_checkpoint(&[9; 32], &unknown).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![first, unknown]);
    }

    #[test]
    fn delayed_checkpoint_retry_cannot_erase_newer_checkpoint_and_tail_after_reopen() {
        use automerge::transaction::Transactable;
        for fresh_timestamp in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let mut document = automerge::AutoCommit::new();
            document.put(automerge::ROOT, "first", 1).unwrap();
            let first_body = document.save();
            let first = frame(true, first_body.clone(), 1);
            store.retain_checkpoint(&[9; 32], &first).unwrap();
            document.put(automerge::ROOT, "second", 2).unwrap();
            let second = frame(true, document.save(), 2);
            store.retain_checkpoint(&[9; 32], &second).unwrap();
            document.put(automerge::ROOT, "tail", 3).unwrap();
            let tail = frame(false, document.save_incremental(), 3);
            store.retain_delta(&[9; 32], 3, &tail).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            let retry = frame(true, first_body, if fresh_timestamp { 99 } else { 1 });
            store.retain_checkpoint(&[9; 32], &retry).unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            assert_eq!(
                store.load_retained().unwrap(),
                vec![second, tail],
                "delayed checkpoint retry discarded uncovered state (fresh timestamp={fresh_timestamp})"
            );
        }
    }

    #[test]
    fn opaque_checkpoints_preserve_receipt_order_and_body_retry_keeps_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let first = frame(true, b"opaque first".to_vec(), 1);
        let second = frame(true, b"opaque second".to_vec(), 2);
        let tail = frame(false, b"opaque tail".to_vec(), 3);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        store.retain_checkpoint(&[9; 32], &second).unwrap();
        store.retain_delta(&[9; 32], 3, &tail).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        store
            .retain_checkpoint(&[9; 32], &frame(true, b"opaque first".to_vec(), 99))
            .unwrap();
        store
            .retain_checkpoint(&[9; 32], &frame(true, b"opaque second".to_vec(), 100))
            .unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![first, second, tail]);
    }

    #[test]
    fn checkpoint_without_causal_tail_coverage_preserves_uncovered_tail() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let first = frame(true, document.save(), 1);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        let mut branch = document.fork();
        branch.save();
        branch.put(automerge::ROOT, "uncovered", 2).unwrap();
        let tail = frame(false, branch.save_incremental(), 2);
        store.retain_delta(&[9; 32], 2, &tail).unwrap();
        document.put(automerge::ROOT, "other", 3).unwrap();
        let second = frame(true, document.save(), 3);
        store.retain_checkpoint(&[9; 32], &second).unwrap();
        let retained = store.load_retained().unwrap();
        assert!(retained.contains(&second));
        assert!(
            retained.contains(&tail),
            "checkpoint without tail ancestry erased that tail"
        );
        assert!(
            !retained.contains(&first),
            "covered raw Automerge checkpoint was not compacted"
        );
        assert_eq!(
            retained.first(),
            Some(&second),
            "proven replacement checkpoint must precede uncovered tails"
        );
        document.merge(&mut branch).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut recovered = automerge::AutoCommit::new();
        for bytes in store.load_retained().unwrap() {
            let (_, message) = payload(&bytes, 7).unwrap();
            let body = match message {
                CoeditPayload::Checkpoint { body, .. }
                | CoeditPayload::RetainedDelta { body, .. } => body,
            };
            recovered.load_incremental(&body).unwrap();
        }
        assert_eq!(
            recovered.get_heads(),
            document.get_heads(),
            "fresh receiver lost an uncovered dependent tail"
        );
    }

    #[test]
    fn opaque_checkpoint_order_is_preserved_even_between_raw_snapshots() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let first = frame(true, document.save(), 1);
        let opaque = frame(true, b"unknown checkpoint".to_vec(), 2);
        document.put(automerge::ROOT, "next", 2).unwrap();
        let next = frame(true, document.save(), 3);
        for checkpoint in [&first, &opaque, &next] {
            store.retain_checkpoint(&[9; 32], checkpoint).unwrap();
        }
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        assert_eq!(store.load_retained().unwrap(), vec![first, opaque, next]);
    }

    #[test]
    fn causal_proof_rejects_partial_or_missing_dependency_tails_and_keeps_publication_identity() {
        use automerge::transaction::Transactable;
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "first", 1).unwrap();
        let first = frame(true, document.save(), 1);
        store.retain_checkpoint(&[9; 32], &first).unwrap();
        let mut partial = document.save();
        partial.extend_from_slice(b"invalid suffix");
        let mut branch = document.fork();
        branch
            .put(automerge::ROOT, "missing dependency", 1)
            .unwrap();
        branch.save();
        branch.put(automerge::ROOT, "orphan", 2).unwrap();
        let missing = branch.save_incremental();
        let tails = [
            frame(false, partial, 2),
            frame(false, missing, 3),
            frame(false, b"opaque".to_vec(), 4),
            frame(false, b"opaque".to_vec(), 5),
        ];
        for (index, tail) in tails.iter().enumerate() {
            store.retain_delta(&[9; 32], index as u64, tail).unwrap();
        }
        document.put(automerge::ROOT, "new", 2).unwrap();
        let checkpoint = frame(true, document.save(), 6);
        store.retain_checkpoint(&[9; 32], &checkpoint).unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        let replay = store.load_retained().unwrap();
        assert_eq!(replay.len(), 5);
        assert_eq!(replay[0], checkpoint);
        assert_eq!(&replay[1..], &tails);
    }

    #[test]
    fn failed_generation_commit_reopens_old_checkpoint_and_tail_then_resumes() {
        let tmp = tempfile::tempdir().unwrap();
        let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
        use automerge::transaction::Transactable;
        let mut document = automerge::AutoCommit::new();
        document.put(automerge::ROOT, "old", 1).unwrap();
        let old = frame(true, document.save(), 1);
        document.put(automerge::ROOT, "tail", 2).unwrap();
        let tail = frame(false, document.save_incremental(), 2);
        document.put(automerge::ROOT, "new", 3).unwrap();
        let new = frame(true, document.save(), 3);
        store.retain_checkpoint(&[9; 32], &old).unwrap();
        store.retain_delta(&[9; 32], 1, &tail).unwrap();
        let uncovered = frame(false, b"opaque survivor".to_vec(), 4);
        store.retain_delta(&[9; 32], 2, &uncovered).unwrap();
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
        assert_eq!(
            store.load_retained().unwrap(),
            vec![old, tail, uncovered.clone()]
        );
        assert_eq!(current(&lineage).unwrap().unwrap(), active);
        fs::remove_dir(blocker).unwrap();
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new.clone(), uncovered.clone()]
        );
        document.put(automerge::ROOT, "next", 4).unwrap();
        let next_tail = frame(false, document.save_incremental(), 4);
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
            vec![new.clone(), uncovered.clone(), next_tail.clone()]
        );
        store.retain_checkpoint(&[9; 32], &new).unwrap();
        assert!(!active.exists());
        assert!(lineage.join("unknown-legacy-directory").exists());
        assert_eq!(
            store.load_retained().unwrap(),
            vec![new, uncovered, next_tail]
        );
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
