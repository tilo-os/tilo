//! The one move of a host's history into its device store.
//!
//! A host before the device store kept the changes its sessions admitted in a
//! store of its own, under `<state>/workspaces/<workspace>/` ([`crate::store`]).
//! Its first start after that reads each workspace's history there once and
//! keeps what the device store keeps: every sealed, signed resource write as a
//! frame ([`Store::retain`]), and every channel's governance as the newest
//! frame of its route ([`Store::keep_channel`]). A write that was waiting for
//! an offline device is folded in the same way. Everything else the old store
//! kept is the workspace's own state, which its peer keeps already, or
//! delivery bookkeeping, which the device link does not use; the move counts
//! it by kind rather than dropping it unnoticed.
//!
//! Nothing is removed. The old files stay where they are, and a file in each
//! workspace's directory says the move ran there, so it runs once.

use std::collections::BTreeMap;
use std::path::Path;

use neighbour_wire::NeighbourMsg;
use tilo_peer_state::store::{Frame, Refusal, Retained, Store};
use tilo_server_protocol::WorkspaceId;

use crate::coedit::CoeditPayload;
use crate::store::WorkspaceStore;
use crate::transport::Envelope;

/// The file in a workspace's directory that says its history was moved.
pub const MOVED: &str = "moved-into-store";

/// What the move took into the device store, for the log and for whoever
/// checks a host's history before and after.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Moved {
    /// Workspaces whose history was moved now.
    pub workspaces: usize,
    /// What the move did in each of them.
    pub by_workspace: BTreeMap<WorkspaceId, Counts>,
    /// Signed, sealed writes by app, whether new or there already.
    pub by_app: BTreeMap<String, u64>,
    /// What the store does not keep, by kind: the workspace's own state and
    /// writes sent in the clear, which the peer keeps, and bookkeeping.
    pub not_kept: BTreeMap<String, u64>,
    /// Entries that did not decode as a change at all.
    pub unreadable: u64,
}

/// What the move did in one workspace, or in all of them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// Signed, sealed writes the store did not hold before, and serves now.
    pub frames: u64,
    /// Signed, sealed writes the store had already, byte for byte: those the
    /// device link carried as they were admitted.
    pub kept_before: u64,
    /// Signed, sealed writes kept beside another one at the same author's
    /// sequence number, with other bytes: the store serves both.
    pub beside: u64,
    /// Signed, sealed writes their author's newer checkpoint on the same
    /// record covers, which the store holds and serves instead.
    pub covered: u64,
    /// The channels whose governance the store holds once the move ran.
    pub channels: u64,
    /// Of the old checkpoints, those the store serves once the move ran.
    pub checkpoints_served: u64,
    /// Of the old checkpoints, those a newer checkpoint of their author's on
    /// the same record retired.
    pub checkpoints_retired: u64,
}

impl Moved {
    /// What the move did in every workspace together.
    #[must_use]
    pub fn total(&self) -> Counts {
        let mut total = Counts::default();
        for counts in self.by_workspace.values() {
            total.frames = total.frames.saturating_add(counts.frames);
            total.kept_before = total.kept_before.saturating_add(counts.kept_before);
            total.beside = total.beside.saturating_add(counts.beside);
            total.covered = total.covered.saturating_add(counts.covered);
            total.channels = total.channels.saturating_add(counts.channels);
            total.checkpoints_served = total
                .checkpoints_served
                .saturating_add(counts.checkpoints_served);
            total.checkpoints_retired = total
                .checkpoints_retired
                .saturating_add(counts.checkpoints_retired);
        }
        total
    }
}

/// Move every workspace's history under `state_root` into `store`, for each
/// workspace not moved already.
///
/// # Errors
///
/// A workspace's history could not be read, or the store could not keep a
/// frame: the move stops there, marks nothing for that workspace, and runs
/// again on the next start. What it kept stays kept.
pub fn move_into_store(state_root: &Path, store: &Store) -> Result<Moved, String> {
    let mut moved = Moved::default();
    let dir = state_root.join("workspaces");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(moved),
        Err(error) => return Err(format!("{}: {error}", dir.display())),
    };
    let mut workspaces = Vec::new();
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        let Some(workspace) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(WorkspaceId::parse)
        else {
            continue;
        };
        if path.join(crate::store::RETAINED_DIR).is_dir() && !path.join(MOVED).exists() {
            workspaces.push((workspace, path));
        }
    }
    workspaces.sort();
    for (workspace, path) in workspaces {
        let counts = move_workspace(state_root, workspace, store, &mut moved)?;
        tilo_files::native::atomic::write(&path.join(MOVED), b"")
            .map_err(|error| format!("{}: {error}", path.join(MOVED).display()))?;
        moved.workspaces = moved.workspaces.saturating_add(1);
        moved.by_workspace.insert(workspace, counts);
        tracing::info!(
            %workspace,
            frames = counts.frames,
            kept_before = counts.kept_before,
            beside = counts.beside,
            covered = counts.covered,
            channels = counts.channels,
            checkpoints_served = counts.checkpoints_served,
            checkpoints_retired = counts.checkpoints_retired,
            "a workspace's history moved into the device store"
        );
    }
    if moved.workspaces > 0 {
        tracing::info!(total = ?moved.total(), ?moved, "the host's history moved into the device store");
    }
    Ok(moved)
}

/// Move `workspace`'s history: what it kept for every device, then what
/// waited for a device that was offline. Each old checkpoint is then either
/// served or retired by a newer one of its author's on the same record.
fn move_workspace(
    state_root: &Path,
    workspace: WorkspaceId,
    store: &Store,
    moved: &mut Moved,
) -> Result<Counts, String> {
    let mut counts = Counts::default();
    let mut taken = Vec::new();
    let kept = WorkspaceStore::open(state_root, workspace, false)
        .map_err(|error| format!("{workspace}: {error}"))?;
    let retained = kept
        .load_retained()
        .map_err(|error| format!("{workspace}: {error}"))?;
    let mut changes: Vec<Vec<u8>> = retained;
    let pending = state_root
        .join("workspaces")
        .join(workspace.to_string())
        .join("pending");
    if let Ok(devices) = std::fs::read_dir(&pending) {
        let mut devices: Vec<_> = devices.flatten().map(|entry| entry.path()).collect();
        devices.sort();
        for device in devices {
            let Some(device) = device
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| <[u8; 32] as hex::FromHex>::from_hex(name).ok())
            else {
                continue;
            };
            let waiting = kept
                .load_pending(&device)
                .map_err(|error| format!("{workspace}: {error}"))?;
            changes.extend(waiting.into_iter().map(|entry| entry.bytes));
        }
    }
    for change in changes {
        let Some((author, body)) = change_of(&change, workspace) else {
            moved.unreadable = moved.unreadable.saturating_add(1);
            continue;
        };
        let Ok(message) = NeighbourMsg::decode(&body) else {
            moved.unreadable = moved.unreadable.saturating_add(1);
            continue;
        };
        keep(workspace, author, &message, store, moved, &mut taken)?;
    }
    // A write kept early in the move may be retired by a checkpoint kept
    // after it: what counts as moved is what the store serves now.
    for (frame, retained) in taken {
        let held = store
            .holds_frame(workspace, &frame)
            .map_err(|error| format!("{workspace}: {error}"))?;
        match retained {
            Retained::New if held => counts.frames = counts.frames.saturating_add(1),
            Retained::Beside if held => counts.beside = counts.beside.saturating_add(1),
            Retained::Duplicate if held => {
                counts.kept_before = counts.kept_before.saturating_add(1);
            }
            _ => counts.covered = counts.covered.saturating_add(1),
        }
        if frame.retention == collab_types::ContentRetention::Checkpoint {
            if held {
                counts.checkpoints_served = counts.checkpoints_served.saturating_add(1);
            } else {
                counts.checkpoints_retired = counts.checkpoints_retired.saturating_add(1);
            }
        }
    }
    counts.channels = store.channels(workspace).len() as u64;
    Ok(counts)
}

/// The author and body of `bytes`, one framed change the old store kept.
fn change_of(bytes: &[u8], workspace: WorkspaceId) -> Option<([u8; 32], Vec<u8>)> {
    let Ok(Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
        ..
    }) = crate::frame::decode(bytes.get(4..)?)
    else {
        return None;
    };
    if workspace_id != workspace {
        return None;
    }
    let body = match CoeditPayload::decode(&delta) {
        Some(
            CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. },
        ) => body,
        None => delta,
    };
    Some((author_device_public_key, body))
}

/// Keep `message`, `author`'s change, in `store` when it is one the store
/// keeps, and count it.
fn keep(
    workspace: WorkspaceId,
    author: [u8; 32],
    message: &NeighbourMsg,
    store: &Store,
    moved: &mut Moved,
    kept: &mut Vec<(Frame, Retained)>,
) -> Result<(), String> {
    match Frame::of(message) {
        Ok(frame) => {
            if let NeighbourMsg::AppDocument { app_id, .. } = message {
                let count = moved.by_app.entry(app_id.clone()).or_default();
                *count = count.saturating_add(1);
            }
            let retained = store
                .retain(workspace, &frame)
                .map_err(|error| format!("{workspace}: {error}"))?;
            kept.push((frame, retained));
            return Ok(());
        }
        Err(Refusal::NotADocument) => {}
        Err(refusal) => {
            let kind = format!("{} ({refusal})", message.kind());
            let count = moved.not_kept.entry(kind).or_default();
            *count = count.saturating_add(1);
            return Ok(());
        }
    }
    if message.governance_workspace().is_some() {
        store
            .keep_channel(workspace, author, message)
            .map_err(|error| format!("{workspace}: {error}"))?;
        return Ok(());
    }
    let count = moved.not_kept.entry(message.kind().to_owned()).or_default();
    *count = count.saturating_add(1);
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;
    use collab_types::{
        ArtifactId, AttestationId, ContentRetention, Ed25519PublicKey, EncryptionMetadata, OpKind,
        SignedOp, sign_op,
    };
    use ed25519_dalek::SigningKey;
    use tilo_peer_state::store::{Appetite, Budget};

    fn workspace() -> WorkspaceId {
        WorkspaceId::from_u128(7)
    }

    /// `key`'s write `seq` of `record`, sealed under a content key when
    /// `sealed`, else in the clear.
    fn write(key: &SigningKey, record: &str, seq: u64, sealed: bool) -> NeighbourMsg {
        signed(key, record, seq, sealed.then_some(ContentRetention::Delta))
    }

    /// `key`'s sealed checkpoint `seq` of `record`.
    fn checkpoint(key: &SigningKey, record: &str, seq: u64) -> NeighbourMsg {
        signed(key, record, seq, Some(ContentRetention::Checkpoint))
    }

    fn signed(
        key: &SigningKey,
        record: &str,
        seq: u64,
        sealed: Option<ContentRetention>,
    ) -> NeighbourMsg {
        let mut op = SignedOp::new_unsigned(
            vec![seq as u8; 48],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            AttestationId::new(),
            OpKind::Change,
            web_time::UNIX_EPOCH + std::time::Duration::from_nanos(seq),
            seq,
        );
        if let Some(retention) = sealed {
            op.encryption = Some(EncryptionMetadata::content_key(1, retention));
        }
        sign_op(&mut op, key).unwrap();
        NeighbourMsg::AppDocument {
            peer: 0,
            app_id: "test.app".into(),
            cluster_id: workspace().to_string(),
            document_id: record.into(),
            envelope: Box::new(op),
            own: None,
            readers: None,
        }
    }

    fn genesis(key: &SigningKey) -> NeighbourMsg {
        NeighbourMsg::MlsGovernance {
            workspace: workspace().to_string(),
            frame: Box::new(
                collab_types::MlsGovernanceFrame::signed(
                    collab_types::WorkspaceId::new(),
                    [0; 32],
                    0,
                    collab_types::MlsGovernanceAction::Genesis,
                    key,
                )
                .unwrap(),
            ),
            delivery: 0,
        }
    }

    /// `message` as the old store framed a change `author` sent through a
    /// session on `channel`, kept as a checkpoint when `checkpoint`.
    fn framed(
        author: &SigningKey,
        channel: [u8; 32],
        message: &NeighbourMsg,
        checkpoint: bool,
    ) -> Vec<u8> {
        let body = postcard::to_allocvec(message).unwrap();
        let payload = if checkpoint {
            CoeditPayload::Checkpoint { channel, body }
        } else {
            CoeditPayload::RetainedDelta { channel, body }
        };
        crate::transport::encode(&Envelope::Delta {
            workspace_id: workspace(),
            author_device_public_key: author.verifying_key().to_bytes(),
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
        .unwrap()
    }

    /// A host from before the device store kept its members' writes, a
    /// channel's genesis and a write in the clear in its own store, and one
    /// write for a device that was offline. Its first start after moves every
    /// signed, sealed write into the device store, which serves each byte for
    /// byte, and the genesis with them; the write in the clear is counted as
    /// what the peer keeps. A second start moves nothing again.
    #[test]
    fn a_host_from_before_the_device_store_serves_every_write_it_kept() {
        let root = tempfile::tempdir().unwrap();
        let author = SigningKey::from_bytes(&[3; 32]);
        let other = SigningKey::from_bytes(&[4; 32]);
        let old = WorkspaceStore::open(root.path(), workspace(), false).unwrap();
        let writes = vec![
            write(&author, "tab:1", 1, true),
            write(&author, "tab:1", 2, true),
            write(&other, "tab:2", 1, true),
        ];
        for (at, message) in writes.iter().enumerate() {
            let key = if at == 2 { &other } else { &author };
            old.retain_delta(
                &[9; 32],
                at as u64 + 1,
                &framed(key, [9; 32], message, false),
            )
            .unwrap();
        }
        let founded = genesis(&author);
        old.retain_checkpoint(&[8; 32], &framed(&author, [8; 32], &founded, true))
            .unwrap();
        let clear = write(&author, "tab:3", 1, false);
        old.retain_delta(&[7; 32], 9, &framed(&author, [7; 32], &clear, false))
            .unwrap();
        let waiting = write(&other, "tab:2", 2, true);
        old.enqueue_pending(&[5; 32], 1, &framed(&other, [9; 32], &waiting, false))
            .unwrap();
        drop(old);

        let store = Store::open(root.path().join("store"), Appetite::StoreAll).unwrap();
        // One write reached the device store already, carried over the link.
        store
            .retain(workspace(), &Frame::of(&writes[0]).unwrap())
            .unwrap();
        let moved = move_into_store(root.path(), &store).unwrap();
        assert_eq!(moved.workspaces, 1);
        let total = moved.total();
        assert_eq!((total.frames, total.kept_before), (3, 1), "{moved:?}");
        assert_eq!(total.channels, 1);
        assert_eq!(moved.by_workspace.get(&workspace()), Some(&total));
        assert_eq!(moved.by_app.get("test.app"), Some(&4));
        assert_eq!(moved.not_kept.values().sum::<u64>(), 1, "{moved:?}");
        assert_eq!(moved.unreadable, 0);

        let served = store
            .replay(workspace(), &[], Budget::default())
            .unwrap()
            .frames;
        let mut expected = writes;
        expected.push(waiting);
        let bytes = |messages: &[NeighbourMsg]| {
            let mut bytes: Vec<Vec<u8>> = messages
                .iter()
                .map(|message| postcard::to_allocvec(message).unwrap())
                .collect();
            bytes.sort();
            bytes
        };
        assert_eq!(
            bytes(&served),
            bytes(&expected),
            "every write, byte for byte"
        );
        assert_eq!(
            store.channels(workspace()),
            vec![(author.verifying_key().to_bytes(), founded)]
        );

        assert_eq!(
            move_into_store(root.path(), &store).unwrap(),
            Moved::default(),
            "the move runs once"
        );
        drop(store);
        let reopened = Store::open(root.path().join("store"), Appetite::StoreAll).unwrap();
        let again = reopened
            .replay(workspace(), &[], Budget::default())
            .unwrap()
            .frames;
        assert_eq!(
            bytes(&again),
            bytes(&expected),
            "and a restart serves the same"
        );
    }

    /// What a host's old history holds besides plain writes: a host's own
    /// checkpoint at sequence number 0, which a device that holds none of its
    /// record is served; a checkpoint the store holds a newer one of, which
    /// that one retires; a write at a sequence number the store holds other
    /// bytes at, which is kept beside them; and a channel's governance kept
    /// twice, one channel. A restart after the move retires nothing more.
    #[test]
    fn every_old_checkpoint_is_served_or_retired_by_a_newer_one() {
        let root = tempfile::tempdir().unwrap();
        let host = SigningKey::from_bytes(&[6; 32]);
        let author = SigningKey::from_bytes(&[3; 32]);
        let old = WorkspaceStore::open(root.path(), workspace(), false).unwrap();
        let base = checkpoint(&host, "files:1", 0);
        let stale = checkpoint(&author, "tab:1", 2);
        let other_bytes = write(&author, "tab:2", 1, true);
        for (at, message) in [&base, &stale, &other_bytes].into_iter().enumerate() {
            let key = if at == 0 { &host } else { &author };
            old.retain_checkpoint(&[at as u8; 32], &framed(key, [at as u8; 32], message, true))
                .unwrap();
        }
        let founded = genesis(&author);
        old.retain_checkpoint(&[8; 32], &framed(&author, [8; 32], &founded, true))
            .unwrap();
        old.enqueue_pending(&[5; 32], 1, &framed(&author, [8; 32], &founded, true))
            .unwrap();
        drop(old);

        let store = Store::open(root.path().join("store"), Appetite::StoreAll).unwrap();
        let newer = checkpoint(&author, "tab:1", 5);
        let linked = write(&author, "tab:2", 1, true);
        for message in [&newer, &linked] {
            store
                .retain(workspace(), &Frame::of(message).unwrap())
                .unwrap();
        }
        let moved = move_into_store(root.path(), &store).unwrap();
        let counts = moved.by_workspace[&workspace()];
        assert_eq!(
            counts,
            Counts {
                frames: 1,
                kept_before: 0,
                beside: 1,
                covered: 1,
                channels: 1,
                checkpoints_served: 1,
                checkpoints_retired: 1,
            },
            "{moved:?}"
        );

        let bytes = |messages: &[NeighbourMsg]| {
            let mut bytes: Vec<Vec<u8>> = messages
                .iter()
                .map(|message| postcard::to_allocvec(message).unwrap())
                .collect();
            bytes.sort();
            bytes
        };
        let expected = bytes(&[base, newer, linked, other_bytes]);
        let fresh = store.replay(workspace(), &[], Budget::default()).unwrap();
        assert_eq!(
            bytes(&fresh.frames),
            expected,
            "a fresh device is served all four"
        );
        let lineages = store.lineages(workspace());
        drop(store);
        let reopened = Store::open(root.path().join("store"), Appetite::StoreAll).unwrap();
        assert_eq!(
            reopened.lineages(workspace()),
            lineages,
            "a restart retires nothing more"
        );
        assert_eq!(
            bytes(
                &reopened
                    .replay(workspace(), &[], Budget::default())
                    .unwrap()
                    .frames
            ),
            expected
        );
    }
}
