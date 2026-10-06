//! Positive local ingress receipts for continuity. Raw bytes never mint authority.
//! Associations name one immutable copy; equal bytes in another slot stay opaque.
use super::{StoreError, WorkspaceStore, bounded::invalid};
use crate::{auth::AuthenticatedPeer, transport::Envelope};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const SUFFIX: &str = ".continuity-v1";
const MAX_ASSOCIATION: u64 = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Evidence {
    workspace: u64,
    author: [u8; 32],
    user: [u8; 32],
    channel: [u8; 32],
    digest: [u8; 32],
}
impl Evidence {
    /// The caller must freshly authorize this source before accepting its input.
    pub(crate) fn authenticated(envelope: &Envelope, peer: &AuthenticatedPeer) -> Option<Self> {
        let Envelope::Delta { workspace_id, .. } = envelope else {
            return None;
        };
        let bytes = crate::transport::encode(envelope).ok()?;
        let evidence = Self::decode(&bytes, *workspace_id)?;
        (evidence.author == peer.device_public_key && evidence.user == peer.user_public_key)
            .then_some(evidence)
    }
    fn decode(bytes: &[u8], workspace: u64) -> Option<Self> {
        let (author, crate::coedit::CoeditPayload::Checkpoint { channel, body }) =
            super::retained::payload(bytes, workspace)?
        else {
            return None;
        };
        let (msg, trailing) =
            postcard::take_from_bytes::<tilo_peer_state::NeighbourMsg>(&body).ok()?;
        if !trailing.is_empty() {
            return None;
        }
        let tilo_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
            user_public_key, ..
        } = &msg
        else {
            return None;
        };
        if tilo_peer_state::transport::retention(&msg, false)
            != Some((channel, tilo_peer_state::transport::Retention::Checkpoint))
        {
            return None;
        }
        Some(Self {
            workspace,
            author,
            user: *user_public_key,
            channel,
            digest: *blake3::hash(bytes).as_bytes(),
        })
    }
    pub(crate) fn accept(self, sequence: u64) -> Receipt {
        Receipt {
            id: rand::random(),
            sequence,
            evidence: self,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub(crate) id: [u8; 16],
    pub(crate) sequence: u64,
    evidence: Evidence,
}
impl Receipt {
    pub(super) fn group(&self) -> String {
        format!(
            "{:016x}-{}-{}-{}",
            self.evidence.workspace,
            hex::encode(self.evidence.author),
            hex::encode(self.evidence.user),
            hex::encode(self.evidence.channel)
        )
    }
    fn validates(&self, store: &WorkspaceStore, bytes: &[u8]) -> bool {
        self.sequence != 0
            && self.evidence.workspace == store.workspace_id
            && self.evidence.digest == *blake3::hash(bytes).as_bytes()
            && Evidence::decode(bytes, store.workspace_id).as_ref() == Some(&self.evidence)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Association {
    version: u8,
    copy_id: [u8; 16],
    location: PathBuf,
    receipt: Receipt,
}

pub(super) fn marker(raw: &Path) -> PathBuf {
    let mut name = raw.as_os_str().to_owned();
    name.push(SUFFIX);
    name.into()
}

/// An orphan marker still reserves its numeric slot. Its contents grant nothing.
pub(super) fn reserved_slot(name: &str) -> Option<u64> {
    let raw = name.strip_suffix(SUFFIX)?;
    let raw = raw.strip_suffix(".delta")?;
    (raw.len() == 16 && raw.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| u64::from_str_radix(raw, 16).ok())
        .flatten()
}
fn hex_name(value: &str, len: usize) -> bool {
    value.len() == len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
#[expect(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "store-generated names use exact lowercase suffixes; any other casing is foreign data"
)]
fn location(store: &WorkspaceStore, raw: &Path, receipt: &Receipt) -> Option<PathBuf> {
    let relative = raw.strip_prefix(store.root()).ok()?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let parts = relative
        .iter()
        .map(|part| part.to_str())
        .collect::<Option<Vec<_>>>()?;
    let allowed = match parts.as_slice() {
        ["retained", "v2", lineage, generation, file] => {
            *lineage
                == format!(
                    "{}-{}",
                    hex::encode(receipt.evidence.author),
                    hex::encode(receipt.evidence.channel)
                )
                && hex_name(generation, 32)
                && file.ends_with(".delta")
        }
        ["shell-selected-v1", generation, file] => {
            hex_name(generation, 32) && file.ends_with(".delta")
        }
        ["pending", device, file] => hex_name(device, 64) && file.ends_with(".delta"),
        ["store-work-v1", replay, "pins", file] => {
            replay
                .strip_prefix("replay-")
                .is_some_and(|id| hex_name(id, 32))
                && file.ends_with(".frame")
        }
        _ => false,
    };
    allowed.then(|| relative.to_path_buf())
}
fn marker_bytes(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > MAX_ASSOCIATION {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_ASSOCIATION + 1)
        .read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= MAX_ASSOCIATION).then_some(bytes))
}
pub(super) fn read_receipt(path: &Path) -> Result<Option<Receipt>, StoreError> {
    let Some(encoded) = marker_bytes(path)? else {
        return Ok(None);
    };
    Ok(postcard::take_from_bytes::<Receipt>(&encoded)
        .ok()
        .and_then(|(receipt, trailing)| trailing.is_empty().then_some(receipt)))
}
fn association(
    store: &WorkspaceStore,
    raw: &Path,
    stats: &mut super::bounded::WorkStats,
) -> Result<Option<(Association, Vec<u8>)>, StoreError> {
    let Some(encoded) = marker_bytes(&marker(raw))? else {
        return Ok(None);
    };
    let Ok((association, trailing)) = postcard::take_from_bytes::<Association>(&encoded) else {
        return Ok(None);
    };
    if association.version != 1
        || !trailing.is_empty()
        || location(store, raw, &association.receipt).as_ref() != Some(&association.location)
    {
        return Ok(None);
    }
    let Some(bytes) = super::replay::frame_bytes(raw)? else {
        return Ok(None);
    };
    super::bounded::increment(&mut stats.preparation_reads)?;
    account_validation(stats, bytes.len());
    if !association.receipt.validates(store, &bytes) {
        return Ok(None);
    }
    Ok(Some((association, encoded)))
}
// Exact validation owns at most the framed input plus two decoded byte-vector
// copies at a time. Each growable decode vector has capacity below twice its
// logical length. Reserve that overlap conservatively, including enum storage.
pub(super) fn account_validation(stats: &mut super::bounded::WorkStats, len: usize) {
    let overhead = std::mem::size_of::<Envelope>()
        .saturating_add(std::mem::size_of::<crate::coedit::CoeditPayload>())
        .saturating_add(std::mem::size_of::<tilo_peer_state::NeighbourMsg>());
    stats.peak_frame_bytes = stats
        .peak_frame_bytes
        .max(len.saturating_mul(5).saturating_add(overhead));
    // This fits the sorter's existing two MAX_METADATA lookahead reservations.
    stats.metadata(2, super::bounded::MAX_METADATA * 2, 4);
}
pub(super) fn load_measured(
    store: &WorkspaceStore,
    raw: &Path,
    stats: &mut super::bounded::WorkStats,
) -> Result<Option<Receipt>, StoreError> {
    Ok(association(store, raw, stats)?.map(|(association, _)| association.receipt))
}
#[cfg(test)]
fn load(store: &WorkspaceStore, raw: &Path) -> Result<Option<Receipt>, StoreError> {
    load_measured(store, raw, &mut super::bounded::WorkStats::default())
}

/// The marker is durable BEFORE the caller publishes a new raw append. An
/// interrupted marker write reserves the slot and never labels later bytes.
pub(super) fn write(
    store: &WorkspaceStore,
    raw: &Path,
    bytes: &[u8],
    receipt: &Receipt,
) -> Result<(), StoreError> {
    if !receipt.validates(store, bytes) {
        return Err(invalid("continuity receipt/frame mismatch"));
    }
    let location =
        location(store, raw, receipt).ok_or_else(|| invalid("invalid continuity copy location"))?;
    let encoded = postcard::to_stdvec(&Association {
        version: 1,
        copy_id: rand::random(),
        location,
        receipt: receipt.clone(),
    })
    .map_err(|_| invalid("continuity association encoding failed"))?;
    if encoded.len() as u64 > MAX_ASSOCIATION {
        return Err(invalid("continuity association too large"));
    }
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker(raw))?;
    output.write_all(&encoded)?;
    output.sync_all()?;
    File::open(
        raw.parent()
            .ok_or_else(|| invalid("continuity copy has no parent"))?,
    )?
    .sync_all()?;
    #[cfg(test)]
    super::test_barrier::check(store.root(), "continuity-marker-durable")?;
    Ok(())
}

/// Copy only the source slot's own proof, rebinding it to the exact destination.
/// A hash-equal unmarked source remains unmarked.
pub(super) fn copy_measured(
    store: &WorkspaceStore,
    source: &Path,
    destination: &Path,
    stats: &mut super::bounded::WorkStats,
    held_frame_bytes: usize,
) -> Result<Option<Receipt>, StoreError> {
    let mut measured = super::bounded::WorkStats::default();
    let result = copy_inner(store, source, destination, &mut measured);
    stats.preparation_reads = stats
        .preparation_reads
        .checked_add(measured.preparation_reads)
        .ok_or_else(|| invalid("continuity read counter exhausted"))?;
    if measured.peak_frame_bytes != 0 {
        stats.peak_frame_bytes = stats
            .peak_frame_bytes
            .max(measured.peak_frame_bytes.saturating_add(held_frame_bytes));
    }
    stats.metadata(
        measured.peak_metadata_entries,
        measured.peak_metadata_bytes,
        measured.peak_open_files,
    );
    result
}
fn copy_inner(
    store: &WorkspaceStore,
    source: &Path,
    destination: &Path,
    stats: &mut super::bounded::WorkStats,
) -> Result<Option<Receipt>, StoreError> {
    let Some((association, encoded)) = association(store, source, stats)? else {
        return Ok(None);
    };
    let bytes = super::replay::frame_bytes(destination)?
        .ok_or_else(|| invalid("invalid continuity copy frame"))?;
    super::bounded::increment(&mut stats.preparation_reads)?;
    account_validation(stats, bytes.len());
    write(store, destination, &bytes, &association.receipt)?;
    if destination
        .extension()
        .is_some_and(|extension| extension == "frame")
    {
        // Preserve the exact source association separately for authorized cleanup.
        let capture = destination.with_extension("source-association");
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(capture)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
    }
    Ok(Some(association.receipt))
}

#[cfg(test)]
fn copy(
    store: &WorkspaceStore,
    source: &Path,
    destination: &Path,
) -> Result<Option<Receipt>, StoreError> {
    copy_measured(
        store,
        source,
        destination,
        &mut super::bounded::WorkStats::default(),
        0,
    )
}

/// Return true for captured marked data even when identities no longer match:
/// that means preserve it, never fall back to generic path-only cleanup.
pub(super) fn cleanup_pending(
    store: &WorkspaceStore,
    source: &Path,
    pin: &Path,
    stats: &mut super::bounded::WorkStats,
) -> Result<bool, StoreError> {
    let Some(captured) = marker_bytes(&pin.with_extension("source-association"))? else {
        return Ok(marker(source).try_exists()?);
    };
    let captured_matches = || -> Result<bool, StoreError> {
        Ok(marker_bytes(&marker(source))?.as_ref() == Some(&captured))
    };
    if !captured_matches()? {
        return Ok(true);
    }
    if source.try_exists()? {
        if !super::replay::same_inode(source, pin)? || !super::replay::equal_files(source, pin)? {
            return Ok(true);
        }
        // Revalidate the actual source association before the raw unlink.
        if association(store, source, stats)?.is_none() {
            return Ok(true);
        }
        fs::remove_file(source)?;
    }
    let parent = source
        .parent()
        .ok_or_else(|| invalid("pending copy has no parent"))?;
    File::open(parent)?.sync_all()?;
    #[cfg(test)]
    super::test_barrier::check(store.root(), "continuity-pending-raw-removed")?;
    if captured_matches()? {
        fs::remove_file(marker(source))?;
        File::open(parent)?.sync_all()?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        bounded::{Step, WorkLimit},
        replay::{ReplayBudget, ReplayId, ReplayLimits},
    };

    struct Fixture {
        tmp: tempfile::TempDir,
        store: WorkspaceStore,
        peer: AuthenticatedPeer,
    }
    impl Fixture {
        fn new() -> Self {
            use tilo_user_identity::{
                DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
                sign_device_cert,
            };
            let (root, user) = UserIdentity::generate("continuity", 1);
            let (key, device) = DeviceIdentity::generate("device", 1);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: device.public_key,
                device_label: "device".into(),
                capabilities: DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).unwrap();
            let peer = crate::auth::accept_hello(
                &crate::auth::Hello {
                    cert,
                    client_nonce: [2; 32],
                },
                &user.public_key,
                &crate::policy::TrustPolicy::AllowMatchingUser,
                &RevocationList::new(),
                2,
            )
            .unwrap();
            let proof = crate::auth::build_hello_ack2(&key, &[1; 32]);
            crate::auth::verify_hello_ack2(&proof, &peer.device_public_key, &[1; 32]).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let store = WorkspaceStore::open(tmp.path(), 7, false).unwrap();
            Self { tmp, store, peer }
        }
        fn frame(&self, snapshot: Vec<u8>, time: u64) -> ([u8; 32], Envelope, Vec<u8>, Receipt) {
            let message = tilo_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key: self.peer.user_public_key,
                publisher: 7,
                snapshot,
            };
            let (channel, _) = tilo_peer_state::transport::retention(&message, false).unwrap();
            let envelope = Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: self.peer.device_public_key,
                authored_at_ns: time,
                signature: [0; 64],
                delta: crate::coedit::CoeditPayload::Checkpoint {
                    channel,
                    body: postcard::to_stdvec(&message).unwrap(),
                }
                .encode()
                .unwrap(),
            };
            let receipt = Evidence::authenticated(&envelope, &self.peer)
                .unwrap()
                .accept(time);
            let bytes = crate::transport::encode(&envelope).unwrap();
            (channel, envelope, bytes, receipt)
        }
        fn append(&self, channel: [u8; 32], bytes: Vec<u8>, receipt: Receipt) {
            let mut build = self
                .store
                .start_retain(&channel, None, bytes)
                .unwrap()
                .with_continuity(Some(receipt));
            loop {
                if matches!(build.step(WorkLimit::default()).unwrap(), Step::Done(_)) {
                    break;
                }
            }
        }
        fn lineage(&self, channel: [u8; 32]) -> PathBuf {
            self.store.root().join("retained/v2").join(format!(
                "{}-{}",
                hex::encode(self.peer.device_public_key),
                hex::encode(channel)
            ))
        }
    }
    fn capture(store: &WorkspaceStore, pending_only: bool) -> super::super::replay::ReplaySnapshot {
        let id = ReplayId(rand::random());
        let mut build = if pending_only {
            store.start_pending_replay_capture(id, [9; 32])
        } else {
            store.start_replay_capture(id, [9; 32])
        }
        .unwrap();
        let mut snapshot = loop {
            if let Step::Done(snapshot) = store
                .capture_replay_step(&mut build, WorkLimit::default())
                .unwrap()
            {
                break snapshot;
            }
        };
        loop {
            if matches!(
                store
                    .prepare_replay_step(&mut snapshot, WorkLimit::default())
                    .unwrap(),
                Step::Done(_)
            ) {
                break;
            }
        }
        snapshot
    }
    fn replay(store: &WorkspaceStore, pending_only: bool) -> Vec<Envelope> {
        let mut snapshot = capture(store, pending_only);
        let budget = ReplayBudget::new(32 * 1024 * 1024);
        let mut output = Vec::new();
        loop {
            let mut batch = store
                .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
                .unwrap();
            while let Some(envelope) = batch.front() {
                output.push(envelope.clone());
                batch.pop_written();
            }
            if batch.eof {
                break;
            }
        }
        output
    }

    #[test]
    fn receipt_evidence_requires_complete_wrappers_and_authenticated_group() {
        let fixture = Fixture::new();
        let (_, original, raw, _) = fixture.frame(vec![1], 1);
        assert!(Evidence::authenticated(&original, &fixture.peer).is_some());
        for case in ["outer-suffix", "body-suffix", "channel", "root", "author"] {
            let mut changed = original.clone();
            let Envelope::Delta {
                delta,
                author_device_public_key,
                ..
            } = &mut changed
            else {
                unreachable!()
            };
            let mut payload = crate::coedit::CoeditPayload::decode(delta).unwrap();
            let crate::coedit::CoeditPayload::Checkpoint { channel, body } = &mut payload else {
                unreachable!()
            };
            match case {
                "outer-suffix" => {}
                "body-suffix" => body.push(0),
                "channel" => channel[0] ^= 1,
                "root" => {
                    let mut message: tilo_peer_state::NeighbourMsg =
                        postcard::from_bytes(body).unwrap();
                    let tilo_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
                        user_public_key,
                        ..
                    } = &mut message
                    else {
                        unreachable!()
                    };
                    user_public_key[0] ^= 1;
                    *channel = tilo_peer_state::transport::retention(&message, false)
                        .unwrap()
                        .0;
                    *body = postcard::to_stdvec(&message).unwrap();
                }
                "author" => author_device_public_key[0] ^= 1,
                _ => unreachable!(),
            }
            *delta = payload.encode().unwrap();
            if case == "outer-suffix" {
                delta.push(0);
            }
            assert!(
                Evidence::authenticated(&changed, &fixture.peer).is_none(),
                "{case} gained evidence"
            );
        }
        let mut suffixed = raw;
        suffixed.push(0);
        let len = u32::try_from(suffixed.len() - 4).unwrap();
        suffixed[..4].copy_from_slice(&len.to_le_bytes());
        assert!(
            Evidence::decode(&suffixed, 7).is_none(),
            "transport suffix gained evidence"
        );
    }

    #[test]
    fn corrupted_receipt_group_does_not_reclassify_bound_frame() {
        let fixture = Fixture::new();
        let (_, _, bytes, receipt) = fixture.frame(Vec::new(), 1);
        let source = fixture
            .store
            .enqueue_continuity_pending(&[9; 32], &bytes, &receipt)
            .unwrap();
        let mut changed: Association =
            postcard::from_bytes(&fs::read(marker(&source)).unwrap()).unwrap();
        changed.receipt.evidence.user[0] ^= 1;
        fs::write(marker(&source), postcard::to_stdvec(&changed).unwrap()).unwrap();
        assert!(load(&fixture.store, &source).unwrap().is_none());
        assert_eq!(
            replay(&fixture.store, true).len(),
            1,
            "corrupted proof must preserve opaque history"
        );
    }

    #[test]
    fn receipt_association_cannot_move_to_equal_unmarked_slot_or_generation() {
        let fixture = Fixture::new();
        let (_, _, bytes, receipt) = fixture.frame(vec![1], 1);
        let source = fixture
            .store
            .enqueue_continuity_pending(&[9; 32], &bytes, &receipt)
            .unwrap();
        assert_eq!(
            load(&fixture.store, &source).unwrap(),
            Some(receipt.clone())
        );
        let selected = fixture
            .store
            .root()
            .join("shell-selected-v1")
            .join(hex::encode([1; 16]));
        fs::create_dir_all(&selected).unwrap();
        let target = selected.join("0000000000000000.delta");
        fs::write(&target, &bytes).unwrap();
        // Merely copying a sidecar cannot authenticate this slot.
        fs::write(marker(&target), fs::read(marker(&source)).unwrap()).unwrap();
        assert!(load(&fixture.store, &target).unwrap().is_none());
        let legitimate = selected.join("0000000000000001.delta");
        fs::write(&legitimate, &bytes).unwrap();
        assert_eq!(
            copy(&fixture.store, &source, &legitimate).unwrap(),
            Some(receipt.clone())
        );
        assert_eq!(load(&fixture.store, &legitimate).unwrap(), Some(receipt));
        let next_generation = fixture
            .store
            .root()
            .join("shell-selected-v1")
            .join(hex::encode([2; 16]));
        fs::create_dir_all(&next_generation).unwrap();
        let moved = next_generation.join("0000000000000001.delta");
        fs::write(&moved, &bytes).unwrap();
        fs::write(marker(&moved), fs::read(marker(&legitimate)).unwrap()).unwrap();
        assert!(load(&fixture.store, &moved).unwrap().is_none());
        let unmarked = selected.join("0000000000000002.delta");
        fs::write(&unmarked, &bytes).unwrap();
        let equal_copy = selected.join("0000000000000003.delta");
        fs::write(&equal_copy, bytes).unwrap();
        assert!(
            copy(&fixture.store, &unmarked, &equal_copy)
                .unwrap()
                .is_none()
        );
        assert!(!marker(&equal_copy).exists());
    }

    #[test]
    fn durable_orphan_marker_reserves_slot_even_when_unreadable() {
        let fixture = Fixture::new();
        let (channel, _, bytes, receipt) = fixture.frame(vec![1], 1);
        let fault =
            super::super::test_barrier::fail(fixture.store.root(), "continuity-marker-durable");
        let mut build = fixture
            .store
            .start_retain(&channel, None, bytes.clone())
            .unwrap()
            .with_continuity(Some(receipt));
        loop {
            match build.step(WorkLimit::default()) {
                Ok(Step::More) => {}
                Err(_) => break,
                Ok(Step::Done(_)) => panic!("marker failure did not interrupt raw publication"),
            }
        }
        drop(fault);
        let active = super::super::retained::current(&fixture.lineage(channel))
            .unwrap()
            .unwrap();
        let orphan = active.join("0000000000000001.delta");
        assert!(!orphan.exists());
        assert!(marker(&orphan).exists());
        fs::write(marker(&orphan), b"interrupted marker").unwrap();
        // Generic append must also respect the reservation, without trusting it.
        fixture.store.retain_checkpoint(&channel, &bytes).unwrap();
        assert!(!orphan.exists());
        let appended = active.join("0000000000000002.delta");
        assert_eq!(fs::read(&appended).unwrap(), bytes);
        assert!(load(&fixture.store, &appended).unwrap().is_none());
        assert_eq!(fs::read(marker(&orphan)).unwrap(), b"interrupted marker");
    }

    #[test]
    fn staged_receipt_only_supersedes_after_current_publication() {
        let fixture = Fixture::new();
        let (channel, old_envelope, old, receipt) = fixture.frame(vec![1], 1);
        fixture.append(channel, old.clone(), receipt);
        let (_, closed_envelope, closed, receipt) = fixture.frame(Vec::new(), 2);
        let scratch = fixture
            .store
            .root()
            .join("store-work-v1")
            .join(format!("retain-{}", hex::encode([3; 16])));
        let mut stage = fixture
            .store
            .start_staged_checkpoint(&channel, closed.clone(), scratch)
            .unwrap()
            .with_continuity(Some(receipt));
        loop {
            if matches!(stage.step(WorkLimit::default()).unwrap(), Step::Done(_)) {
                break;
            }
        }
        let reopened = WorkspaceStore::open(fixture.tmp.path(), 7, false).unwrap();
        assert_eq!(replay(&reopened, false), vec![old_envelope.clone()]);
        let failed = tilo_files::native::atomic::test_support::with_failure(
            tilo_files::native::atomic::test_support::Checkpoint::BeforePublish,
            || stage.promote(),
        );
        assert!(failed.is_err());
        let reopened = WorkspaceStore::open(fixture.tmp.path(), 7, false).unwrap();
        assert_eq!(replay(&reopened, false), vec![old_envelope]);
        stage.promote().unwrap();
        let reopened = WorkspaceStore::open(fixture.tmp.path(), 7, false).unwrap();
        assert_eq!(replay(&reopened, false), vec![closed_envelope]);
        assert_eq!(reopened.load_retained().unwrap(), vec![old, closed]);
    }

    #[test]
    fn staged_receipt_copy_accounts_still_held_incoming_frame() {
        let fixture = Fixture::new();
        let (channel, _, old, receipt) = fixture.frame(vec![1; 64 * 1024], 1);
        fixture.append(channel, old.clone(), receipt);
        let (_, _, next, receipt) = fixture.frame(vec![2; 16 * 1024], 2);
        let scratch = fixture
            .store
            .root()
            .join("store-work-v1")
            .join(format!("retain-{}", hex::encode([4; 16])));
        let mut stage = fixture
            .store
            .start_staged_checkpoint(&channel, next.clone(), scratch)
            .unwrap()
            .with_continuity(Some(receipt));
        loop {
            if matches!(stage.step(WorkLimit::default()).unwrap(), Step::Done(_)) {
                break;
            }
        }
        assert!(
            stage.stats().peak_frame_bytes >= 5 * old.len() + next.len(),
            "copy validation omitted the concurrently held incoming frame"
        );
        assert!(stage.stats().peak_frame_bytes <= 6 * (old.len() + next.len()));
        stage.promote().unwrap();
        assert_eq!(fixture.store.load_retained().unwrap(), vec![old, next]);
    }

    #[test]
    fn pending_only_capture_chooses_newest_receipt_and_coalesces_its_copies() {
        let fixture = Fixture::new();
        let (_, _, old, receipt) = fixture.frame(vec![1], 1);
        fixture
            .store
            .enqueue_continuity_pending(&[9; 32], &old, &receipt)
            .unwrap();
        let (_, closed_envelope, closed, receipt) = fixture.frame(Vec::new(), 2);
        for _ in 0..2 {
            fixture
                .store
                .enqueue_continuity_pending(&[9; 32], &closed, &receipt)
                .unwrap();
        }
        assert_eq!(replay(&fixture.store, true), vec![closed_envelope]);
        assert_eq!(
            fixture.store.count_pending(&[9; 32]).unwrap(),
            3,
            "capture must not acknowledge pending entries"
        );
    }

    #[test]
    fn pending_cleanup_crash_leaves_marker_and_never_reuses_filename() {
        let fixture = Fixture::new();
        let (_, _, bytes, receipt) = fixture.frame(Vec::new(), 1);
        let source = fixture
            .store
            .enqueue_continuity_pending(&[9; 32], &bytes, &receipt)
            .unwrap();
        let mut snapshot = capture(&fixture.store, true);
        let fault = super::super::test_barrier::fail(
            fixture.store.root(),
            "continuity-pending-raw-removed",
        );
        assert!(
            fixture
                .store
                .cleanup_replay_step(&mut snapshot, WorkLimit::default())
                .is_err()
        );
        assert!(!source.exists(), "raw must disappear before marker unlink");
        assert!(
            marker(&source).exists(),
            "marker vanished before raw unlink durability boundary"
        );
        drop(fault);
        let replacement = fixture
            .store
            .enqueue_continuity_pending(&[9; 32], &bytes, &receipt)
            .unwrap();
        assert_ne!(source, replacement);
        loop {
            if matches!(
                fixture
                    .store
                    .cleanup_replay_step(&mut snapshot, WorkLimit::default())
                    .unwrap(),
                Step::Done(())
            ) {
                break;
            }
        }
        assert!(!marker(&source).exists());
        assert_eq!(load(&fixture.store, &replacement).unwrap(), Some(receipt));
        assert_eq!(fs::read(replacement).unwrap(), bytes);
    }

    #[test]
    fn pending_cleanup_preserves_changed_association_at_captured_path() {
        let fixture = Fixture::new();
        let (_, _, bytes, receipt) = fixture.frame(Vec::new(), 1);
        let source = fixture
            .store
            .enqueue_continuity_pending(&[9; 32], &bytes, &receipt)
            .unwrap();
        let mut snapshot = capture(&fixture.store, true);
        let mut changed: Association =
            postcard::from_bytes(&fs::read(marker(&source)).unwrap()).unwrap();
        changed.copy_id = rand::random();
        fs::write(marker(&source), postcard::to_stdvec(&changed).unwrap()).unwrap();
        loop {
            if matches!(
                fixture
                    .store
                    .cleanup_replay_step(&mut snapshot, WorkLimit::default())
                    .unwrap(),
                Step::Done(())
            ) {
                break;
            }
        }
        assert_eq!(load(&fixture.store, &source).unwrap(), Some(receipt));
        assert_eq!(fs::read(source).unwrap(), bytes);
    }
    #[test]
    fn marked_history_capture_accounts_validation_and_one_bounded_sort_buffer() {
        let fixture = Fixture::new();
        let mut largest = 0;
        let mut latest = None;
        for sequence in 1..=160 {
            let (_, envelope, bytes, receipt) =
                fixture.frame(vec![u8::try_from(sequence).unwrap(); 64 * 1024], sequence);
            largest = largest.max(bytes.len());
            fixture
                .store
                .enqueue_continuity_pending(&[9; 32], &bytes, &receipt)
                .unwrap();
            latest = Some(envelope);
        }
        let mut build = fixture
            .store
            .start_pending_replay_capture(ReplayId(rand::random()), [9; 32])
            .unwrap();
        let mut snapshot = loop {
            let step = fixture
                .store
                .capture_replay_step(&mut build, WorkLimit::default())
                .unwrap();
            if let Step::Done(snapshot) = step {
                break snapshot;
            }
            let stats = build.stats().unwrap();
            assert!(stats.peak_metadata_entries <= super::super::bounded::RUN_ENTRIES + 1);
            assert!(stats.peak_frame_bytes <= largest * 6);
        };
        loop {
            let step = fixture
                .store
                .prepare_replay_step(&mut snapshot, WorkLimit::default())
                .unwrap();
            let stats = snapshot.stats();
            assert!(stats.peak_metadata_entries <= super::super::bounded::RUN_ENTRIES + 1);
            assert!(
                stats.peak_metadata_bytes
                    <= super::super::bounded::RUN_BYTES + 4 * super::super::bounded::MAX_METADATA
            );
            assert!(stats.peak_frame_bytes <= largest * 6);
            if matches!(step, Step::Done(_)) {
                break;
            }
        }
        assert!(
            snapshot.stats().preparation_reads >= 4 * 160,
            "positive proof reads during capture, election and classification were omitted"
        );
        assert!(
            snapshot.stats().peak_frame_bytes >= largest,
            "marked frame hydration was omitted from bounded accounting"
        );
        let budget = ReplayBudget::new(largest * 16);
        let mut batch = fixture
            .store
            .read_replay_batch(&mut snapshot, ReplayLimits::default(), &budget)
            .unwrap();
        assert_eq!(batch.front(), latest.as_ref());
        batch.pop_written();
        assert!(batch.front().is_none());
        assert!(batch.eof);
    }
}
