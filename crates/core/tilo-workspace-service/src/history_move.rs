//! The one move of a host's history into its device store.
//!
//! A host before the device store kept the changes its sessions admitted in a
//! store of its own, under `<state>/workspaces/<workspace>/` (read by `old_layout`).
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

mod old_layout;

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
    /// Signed, sealed writes the store found a newer frame of their author's
    /// on the same record for, but no checkpoint of it to read them against:
    /// kept rather than dropped, and served as they are.
    pub uncovered: u64,
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
            total.uncovered = total.uncovered.saturating_add(counts.uncovered);
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
        if path.join("retained").is_dir() && !path.join(MOVED).exists() {
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
            uncovered = counts.uncovered,
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
    let root = state_root.join("workspaces").join(workspace.to_string());
    let mut changes = old_layout::retained(&root.join("retained").join("v2"))
        .map_err(|error| format!("{workspace}: {error}"))?;
    changes.extend(
        old_layout::pending(&root.join("pending"))
            .map_err(|error| format!("{workspace}: {error}"))?,
    );
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
    count_taken(workspace, store, taken, &mut counts)?;
    counts.channels = store.channels(workspace).len() as u64;
    Ok(counts)
}

/// Count what the move kept, as the store serves it now. A frame the store
/// does not serve is either covered by a newer checkpoint of its author's on
/// its record, which the store holds, or has nothing covering it: that one is
/// kept rather than dropped, and counted as uncovered.
fn count_taken(
    workspace: WorkspaceId,
    store: &Store,
    taken: Vec<(Frame, Retained)>,
    counts: &mut Counts,
) -> Result<(), String> {
    let held_checkpoints =
        held_checkpoints(workspace, store).map_err(|error| format!("{workspace}: {error}"))?;
    for (frame, retained) in taken {
        let held = store
            .holds_frame(workspace, &frame)
            .map_err(|error| format!("{workspace}: {error}"))?;
        if !held {
            let covered = held_checkpoints
                .get(&frame.lineage)
                .is_some_and(|newest| *newest > frame.seq);
            if !covered {
                counts.uncovered = counts.uncovered.saturating_add(1);
                store
                    .keep_uncovered(workspace, &frame)
                    .map_err(|error| format!("{workspace}: {error}"))?;
                let kept = store
                    .holds_frame(workspace, &frame)
                    .map_err(|error| format!("{workspace}: {error}"))?;
                if kept && frame.retention == collab_types::ContentRetention::Checkpoint {
                    counts.checkpoints_served = counts.checkpoints_served.saturating_add(1);
                }
                continue;
            }
        }
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
    Ok(())
}

/// The newest checkpoint the store serves of each lineage in `workspace`.
fn held_checkpoints(
    workspace: WorkspaceId,
    store: &Store,
) -> Result<BTreeMap<tilo_peer_state::store::Lineage, u64>, tilo_peer_state::store::StoreError> {
    use tilo_peer_state::store::{Budget, advance};
    let mut newest = BTreeMap::new();
    let mut cursors = Vec::new();
    loop {
        let replay = store.replay(workspace, &cursors, Budget::default())?;
        for message in &replay.frames {
            let Ok(frame) = Frame::of(message) else {
                continue;
            };
            if frame.retention != collab_types::ContentRetention::Checkpoint {
                continue;
            }
            let at = newest.entry(frame.lineage).or_insert(frame.seq);
            *at = (*at).max(frame.seq);
        }
        advance(&mut cursors, &replay.frames);
        if !replay.more {
            return Ok(newest);
        }
    }
}

/// The author and body of `bytes`, one framed change the old store kept: the
/// serialized `Delta` envelope a session carried, read byte by byte here
/// because the type it was is gone.
fn change_of(bytes: &[u8], workspace: WorkspaceId) -> Option<([u8; 32], Vec<u8>)> {
    // The frame codec's four-byte length prefix, then postcard's bytes.
    let mut at = 4usize;
    let bytes = bytes.get(at..)?;
    at = 0;
    // The envelope's discriminant: `Delta` was the third variant.
    if varint(bytes, &mut at)? != 2 {
        return None;
    }
    let id = uuid_at(bytes, &mut at)?;
    if WorkspaceId::parse(&id.to_string()) != Some(workspace) {
        return None;
    }
    let author: [u8; 32] = bytes.get(at..at.checked_add(32)?)?.try_into().ok()?;
    at = at.checked_add(32)?;
    // When it was authored.
    let _authored_at_ns = varint(bytes, &mut at)?;
    let delta = bytes_at(bytes, &mut at)?;
    // The signature: 64 bytes, which the old store did not check here.
    at = at.checked_add(64)?;
    if at > bytes.len() {
        return None;
    }
    // A co-edit payload names its channel and keeps the body after it; a
    // change written before them is the body itself.
    let body = match delta.first() {
        Some(0 | 1) => {
            let mut inner = 1usize;
            let _channel: [u8; 32] = delta.get(inner..inner.checked_add(32)?)?.try_into().ok()?;
            inner = inner.checked_add(32)?;
            bytes_at(&delta, &mut inner)?
        }
        _ => delta.clone(),
    };
    Some((author, body))
}

/// The `LEN` bytes at `at`, advancing past them.
fn bytes_at(bytes: &[u8], at: &mut usize) -> Option<Vec<u8>> {
    let len = usize::try_from(varint(bytes, at)?).ok()?;
    let end = at.checked_add(len)?;
    let taken = bytes.get(*at..end)?.to_vec();
    *at = end;
    Some(taken)
}

/// The UUID at `at`: its length byte and sixteen bytes, advancing past them.
fn uuid_at(bytes: &[u8], at: &mut usize) -> Option<uuid::Uuid> {
    let len = varint(bytes, at)?;
    if len != 16 {
        return None;
    }
    let end = at.checked_add(16)?;
    let id = uuid::Uuid::from_slice(bytes.get(*at..end)?).ok()?;
    *at = end;
    Some(id)
}

/// One postcard varint (LEB128) at `at`, advancing past it.
fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(*at)?;
        *at = at.checked_add(1)?;
        value |= u64::from(byte & 0x7F).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift = shift.checked_add(7)?;
        if shift >= 64 {
            return None;
        }
    }
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
    if matches!(message, NeighbourMsg::SyncResp { .. }) {
        store
            .keep_workspace_change(workspace, author, message)
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
        // The old `CoeditPayload`, then the `Delta` envelope around it.
        let mut payload = vec![u8::from(!checkpoint)];
        payload.extend(channel);
        push_length(&mut payload, body.len());
        payload.extend(body);
        let mut envelope = vec![2u8];
        let id = workspace();
        envelope.push(16);
        envelope.extend(id.as_uuid().as_bytes());
        envelope.extend(author.verifying_key().to_bytes());
        envelope.push(1); // authored at 1ns
        push_length(&mut envelope, payload.len());
        envelope.extend(payload);
        envelope.extend([0u8; 64]);
        // The frame codec's four-byte length prefix.
        let mut framed = u32::try_from(envelope.len())
            .unwrap()
            .to_le_bytes()
            .to_vec();
        framed.extend(envelope);
        framed
    }

    /// A length as postcard writes it before a byte string: seven bits a byte, low bits first,
    /// the high bit set on every byte but the last.
    fn push_length(into: &mut Vec<u8>, mut length: usize) {
        while length >= 0x80 {
            into.push(u8::try_from(length & 0x7f).unwrap() | 0x80);
            length >>= 7;
        }
        into.push(u8::try_from(length).unwrap());
    }

    /// A workspace's files as a host from before the device store wrote them.
    struct Old(std::path::PathBuf);

    impl Old {
        fn new(state: &Path) -> Self {
            let root = state.join("workspaces").join(workspace().to_string());
            std::fs::create_dir_all(root.join("retained/v2")).unwrap();
            Self(root)
        }

        /// A route's generation of `records`, each kept in its file and in
        /// the generation's journal, except that the files named in `lost`
        /// are missing, as a crash leaves them.
        fn generation(&self, route: &str, records: &[(&str, Vec<u8>)], lost: &[&str]) {
            let route = self.0.join("retained/v2").join(route);
            let name = "0123456789abcdef0123456789abcdef";
            let generation = route.join(name);
            std::fs::create_dir_all(&generation).unwrap();
            std::fs::write(route.join("CURRENT"), name).unwrap();
            let mut journal = Vec::new();
            for (record, bytes) in records {
                if !lost.contains(record) {
                    std::fs::write(generation.join(record), bytes).unwrap();
                }
                let start = journal.len();
                journal.extend_from_slice(b"tgj1");
                journal.extend_from_slice(&(record.len() as u16).to_le_bytes());
                journal.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                journal.extend_from_slice(blake3::hash(bytes).as_bytes());
                journal.extend_from_slice(&[0; 16]);
                journal.extend_from_slice(record.as_bytes());
                let check = blake3::hash(&journal[start..]);
                journal.extend_from_slice(&check.as_bytes()[..8]);
                journal.extend_from_slice(bytes);
            }
            // A torn append the journal ends at.
            journal.extend_from_slice(b"tgj1\0");
            std::fs::write(generation.join(".journal"), journal).unwrap();
        }

        /// `bytes`, waiting for `device`.
        fn pending(&self, device: &[u8; 32], bytes: &[u8]) {
            let dir = self.0.join("pending").join(hex::encode(device));
            std::fs::create_dir_all(&dir).unwrap();
            let name = format!("{:016x}-{}.delta", 1, blake3::hash(bytes).to_hex());
            std::fs::write(dir.join(name), bytes).unwrap();
        }
    }

    /// A host from before the device store kept its members' writes, a
    /// channel's genesis and a write in the clear in its own store, and one
    /// write for a device that was offline. Its first start after moves every
    /// signed, sealed write into the device store, which serves each byte for
    /// byte, and the genesis with them; the write in the clear is counted as
    /// what the peer keeps. A second start moves nothing again.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one old host's whole history, laid out and then checked as served"
    )]
    fn a_host_from_before_the_device_store_serves_every_write_it_kept() {
        let root = tempfile::tempdir().unwrap();
        let author = SigningKey::from_bytes(&[3; 32]);
        let other = SigningKey::from_bytes(&[4; 32]);
        let old = Old::new(root.path());
        let writes = vec![
            write(&author, "tab:1", 1, true),
            write(&author, "tab:1", 2, true),
            write(&other, "tab:2", 1, true),
        ];
        let route = |key: &SigningKey, channel: u8| {
            format!(
                "{}-{}",
                hex::encode(key.verifying_key().to_bytes()),
                hex::encode([channel; 32])
            )
        };
        old.generation(
            &route(&author, 9),
            &[
                (
                    "0000000000000000.delta",
                    framed(&author, [9; 32], &writes[0], false),
                ),
                (
                    "0000000000000001.delta",
                    framed(&author, [9; 32], &writes[1], false),
                ),
            ],
            // The second write's file was lost to a crash: its journal entry
            // has it.
            &["0000000000000001.delta"],
        );
        old.generation(
            &route(&other, 9),
            &[(
                "0000000000000000.delta",
                framed(&other, [9; 32], &writes[2], false),
            )],
            &[],
        );
        let founded = genesis(&author);
        old.generation(
            &route(&author, 8),
            &[("checkpoint.delta", framed(&author, [8; 32], &founded, true))],
            &[],
        );
        let clear = write(&author, "tab:3", 1, false);
        old.generation(
            &route(&author, 7),
            &[(
                "0000000000000000.delta",
                framed(&author, [7; 32], &clear, false),
            )],
            &[],
        );
        let waiting = write(&other, "tab:2", 2, true);
        old.pending(&[5; 32], &framed(&other, [9; 32], &waiting, false));

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
        let old = Old::new(root.path());
        let route = |key: &SigningKey, channel: u8| {
            format!(
                "{}-{}",
                hex::encode(key.verifying_key().to_bytes()),
                hex::encode([channel; 32])
            )
        };
        let base = checkpoint(&host, "files:1", 0);
        let stale = checkpoint(&author, "tab:1", 2);
        let other_bytes = write(&author, "tab:2", 1, true);
        for (at, message) in [&base, &stale, &other_bytes].into_iter().enumerate() {
            let key = if at == 0 { &host } else { &author };
            let channel = at as u8;
            old.generation(
                &route(key, channel),
                &[(
                    "checkpoint.delta",
                    framed(key, [channel; 32], message, true),
                )],
                &[],
            );
        }
        let founded = genesis(&author);
        old.generation(
            &route(&author, 8),
            &[("checkpoint.delta", framed(&author, [8; 32], &founded, true))],
            &[],
        );
        old.pending(&[5; 32], &framed(&author, [8; 32], &founded, true));

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
                uncovered: 0,
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
        let stands = store.stands(workspace());
        drop(store);
        let reopened = Store::open(root.path().join("store"), Appetite::StoreAll).unwrap();
        assert_eq!(
            reopened.stands(workspace()),
            stands,
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

    /// A checkpoint the store does not serve is retired only by a newer
    /// checkpoint of its own author's on its own record: one with no such
    /// checkpoint is kept, and counted uncovered, never as retired.
    #[test]
    fn a_checkpoint_with_no_newer_one_on_its_record_is_kept_not_retired() {
        let root = tempfile::tempdir().unwrap();
        let author = SigningKey::from_bytes(&[3; 32]);
        let store = Store::open(root.path().join("store"), Appetite::StoreAll).unwrap();
        // A newer checkpoint of another record of the same author is no
        // covering checkpoint of this one.
        let elsewhere = checkpoint(&author, "tab:9", 5);
        store
            .retain(workspace(), &Frame::of(&elsewhere).unwrap())
            .unwrap();
        let earlier = checkpoint(&author, "tab:1", 2);
        let frame = Frame::of(&earlier).unwrap();
        let mut counts = Counts::default();
        count_taken(
            workspace(),
            &store,
            vec![(frame.clone(), Retained::New)],
            &mut counts,
        )
        .unwrap();
        assert_eq!(
            counts.checkpoints_retired, 0,
            "no newer checkpoint of its own record exists"
        );
        assert_eq!(counts.uncovered, 1, "{counts:?}");
        assert_eq!(counts.checkpoints_served, 1, "{counts:?}");
        assert!(
            store.holds_frame(workspace(), &frame).unwrap(),
            "the frame is kept rather than dropped"
        );
    }
}
