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
    /// Signed, sealed writes the store did not hold before.
    pub frames: u64,
    /// Signed, sealed writes the store had already: those the device link
    /// carried as they were admitted.
    pub kept_before: u64,
    /// Channel governance frames the store keeps now.
    pub channels: u64,
    /// Signed, sealed writes by app, whether new or there already.
    pub by_app: BTreeMap<String, u64>,
    /// What the store does not keep, by kind: the workspace's own state and
    /// writes sent in the clear, which the peer keeps, and bookkeeping.
    pub not_kept: BTreeMap<String, u64>,
    /// Entries that did not decode as a change at all.
    pub unreadable: u64,
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
        let before = moved.clone();
        move_workspace(state_root, workspace, store, &mut moved)?;
        tilo_files::native::atomic::write(&path.join(MOVED), b"")
            .map_err(|error| format!("{}: {error}", path.join(MOVED).display()))?;
        moved.workspaces = moved.workspaces.saturating_add(1);
        tracing::info!(
            %workspace,
            frames = moved.frames.saturating_sub(before.frames),
            kept_before = moved.kept_before.saturating_sub(before.kept_before),
            channels = moved.channels.saturating_sub(before.channels),
            "a workspace's history moved into the device store"
        );
    }
    if moved.workspaces > 0 {
        tracing::info!(?moved, "the host's history moved into the device store");
    }
    Ok(moved)
}

/// Move `workspace`'s history: what it kept for every device, then what
/// waited for a device that was offline.
fn move_workspace(
    state_root: &Path,
    workspace: WorkspaceId,
    store: &Store,
    moved: &mut Moved,
) -> Result<(), String> {
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
        keep(workspace, author, &message, store, moved)?;
    }
    Ok(())
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
        Some(CoeditPayload::Checkpoint { body, .. } | CoeditPayload::RetainedDelta { body, .. }) => {
            body
        }
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
) -> Result<(), String> {
    match Frame::of(message) {
        Ok(frame) => {
            if let NeighbourMsg::AppDocument { app_id, .. } = message {
                let count = moved.by_app.entry(app_id.clone()).or_default();
                *count = count.saturating_add(1);
            }
            match store
                .retain(workspace, &frame)
                .map_err(|error| format!("{workspace}: {error}"))?
            {
                Retained::New => moved.frames = moved.frames.saturating_add(1),
                Retained::Duplicate => moved.kept_before = moved.kept_before.saturating_add(1),
            }
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
        if store
            .keep_channel(workspace, author, message)
            .map_err(|error| format!("{workspace}: {error}"))?
        {
            moved.channels = moved.channels.saturating_add(1);
        }
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
        let mut op = SignedOp::new_unsigned(
            vec![seq as u8; 48],
            ArtifactId::new(),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            AttestationId::new(),
            OpKind::Change,
            web_time::UNIX_EPOCH + std::time::Duration::from_nanos(seq),
            seq,
        );
        if sealed {
            op.encryption = Some(EncryptionMetadata::content_key(1, ContentRetention::Delta));
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
            old.retain_delta(&[9; 32], at as u64 + 1, &framed(key, [9; 32], message, false))
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
        assert_eq!((moved.frames, moved.kept_before), (3, 1), "{moved:?}");
        assert_eq!(moved.channels, 1);
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
        assert_eq!(bytes(&served), bytes(&expected), "every write, byte for byte");
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
        assert_eq!(bytes(&again), bytes(&expected), "and a restart serves the same");
    }
}
