//! A key holder compacts a sealed record's history; a replica without the
//! key accepts only a verified compaction of exactly the writes it holds.
use super::*;
use crate::{InputDisposition, TransportOrigin, peer_handle};
use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::CommitNamed as _;
use collab_types::{ContentRetention, EncryptionMetadata, SealedCompaction};
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const SECRET: &str = "a sentence only key holders may read";

struct Fixture {
    /// A durable replica holding the seeded record and no channel key.
    host: StatePeer,
    /// The workspace's governing device, which holds the owner role.
    owner: SigningKey,
    /// The owner's second device.
    other: SigningKey,
    /// A collaborator with the editor role.
    editor: SigningKey,
    authority: DeviceCert,
    /// The record channel's group, whose content keys key holders use.
    group: ChannelGroup,
    now: SystemTime,
}

fn device(root: u8, seed: u8) -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&[root; 32]);
    let key = SigningKey::from_bytes(&[seed; 32]);
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: format!("device-{seed}"),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

fn fixture() -> Fixture {
    let (owner, authority) = device(1, 2);
    let (other, other_cert) = device(1, 3);
    let (editor, editor_cert) = device(110, 111);
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let roster = vec![authority.clone(), other_cert, editor_cert.clone()];
    let mut workspace = workspace::candidate(
        None,
        &seed_workspace(WORKSPACE, &authority, &roster).unwrap(),
    )
    .unwrap();
    crate::members::op::tests_support::set_member(
        &mut workspace,
        &(owner.clone(), authority.clone()),
        &hex::encode(editor_cert.user_public_key),
        Role::Editor,
    );
    let workspace_changes = workspace
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let mut native = StatePeer::new(
        WORKSPACE,
        authority.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "base").unwrap();
    document.commit();
    native
        .install_seed(
            SeedState {
                workspace_changes,
                documents: vec![SeedDocument {
                    app_id: APP.into(),
                    record_id: RECORD.into(),
                    kind: "test".into(),
                    bytes: document.save(),
                }],
            },
            &owner,
            now,
        )
        .unwrap();
    let mut host = StatePeer::new(
        WORKSPACE,
        authority.clone(),
        roster,
        RevocationList::new(),
        now,
    )
    .unwrap();
    host.select_admission_v5().unwrap();
    for message in native
        .publication_mut()
        .bootstrap_messages(&owner, now)
        .unwrap()
    {
        host.receive_from(origin(&owner), message, now).unwrap();
    }
    let group = ChannelGroup::create(channel(), MemberKey::generate("owner").unwrap()).unwrap();
    Fixture {
        host,
        owner,
        other,
        editor,
        authority,
        group,
        now,
    }
}

impl Fixture {
    fn keys(&self) -> ChannelContentKeys {
        self.group.content_keys().unwrap()
    }

    fn rotate(&mut self) -> ChannelContentKeys {
        let member = MemberKey::generate("member").unwrap();
        self.group
            .add_member(member.fresh_key_package().unwrap())
            .unwrap();
        self.keys()
    }

    fn restore(&self, bytes: &[u8]) -> Result<StatePeer, String> {
        StatePeer::restore(
            bytes,
            WORKSPACE,
            self.authority.clone(),
            vec![],
            RevocationList::new(),
            self.now,
        )
    }
}

fn channel() -> collab_types::ChannelId {
    collab_types::document_channel_id(APP, WORKSPACE, RECORD)
}

fn artifact() -> collab_types::ArtifactId {
    collab_types::document_artifact_id(APP, WORKSPACE, RECORD)
}

fn origin(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::RelayDelta(key.verifying_key().to_bytes())
}

fn route() -> Route {
    Route {
        app: APP.into(),
        record: RECORD.into(),
    }
}

fn held(peer: &StatePeer) -> Vec<SignedOp> {
    peer.core.documents[&route()].sealed.to_vec()
}

fn envelope_of(message: &NeighbourMsg) -> SignedOp {
    let NeighbourMsg::AppDocument { envelope, .. } = message else {
        panic!("a document write")
    };
    (**envelope).clone()
}

fn next_seq(peer: &StatePeer, key: &SigningKey) -> u64 {
    floor(peer, key).checked_add(1).unwrap()
}

fn floor(peer: &StatePeer, key: &SigningKey) -> u64 {
    peer.core.documents[&route()]
        .channel
        .last_seq_per_author
        .get(&Ed25519PublicKey::from_bytes(
            key.verifying_key().to_bytes(),
        ))
        .copied()
        .unwrap_or(0)
}

fn message(op: SignedOp) -> NeighbourMsg {
    NeighbourMsg::AppDocument {
        peer: peer_handle(op.author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        record_id: RECORD.into(),
        envelope: Box::new(op),
    }
}

fn signed(
    key: &SigningKey,
    seq: u64,
    kind: OpKind,
    payload: Vec<u8>,
    encryption: Option<EncryptionMetadata>,
    now: SystemTime,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact(),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
        kind,
        now,
        seq,
    );
    op.encryption = encryption;
    collab_types::sign_op(&mut op, key).unwrap();
    op
}

/// Edits of the seeded record, each an incremental on the one before, as
/// the plaintext a key holder's sealed write opens to.
struct Author(AutoCommit);

impl Author {
    fn new(peer: &StatePeer) -> Self {
        Self(AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap())
    }

    fn edit(&mut self, key: &str, value: &str) -> Vec<u8> {
        let heads = self.0.get_heads();
        self.0.put(ROOT, key, value).unwrap();
        self.0.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
            .encode()
            .unwrap()
    }

    /// [`Self::edit`] of a value held as bytes, as an app records what a file or a picture is.
    fn edit_bytes(&mut self, key: &str, value: Vec<u8>) -> Vec<u8> {
        let heads = self.0.get_heads();
        self.0.put(ROOT, key, value).unwrap();
        self.0.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
            .encode()
            .unwrap()
    }
}

/// `key`'s next sealed write to `peer`'s record.
fn write(
    peer: &StatePeer,
    key: &SigningKey,
    keys: &ChannelContentKeys,
    plaintext: &[u8],
    now: SystemTime,
) -> NeighbourMsg {
    let (metadata, payload) = keys
        .seal_content(artifact(), ContentRetention::Delta, plaintext)
        .unwrap();
    message(signed(
        key,
        next_seq(peer, key),
        OpKind::Change,
        payload,
        Some(metadata),
        now,
    ))
}

/// `key`'s sealed checkpoint of `peer`'s record: the whole record as the key
/// holder saved it, the first thing a host keeps at the head of a sealed
/// channel and replays to a device joining the workspace.
fn checkpoint(
    peer: &StatePeer,
    keys: &ChannelContentKeys,
    key: &SigningKey,
    now: SystemTime,
) -> NeighbourMsg {
    let snapshot = DocumentMutation::Snapshot(peer.document(APP, RECORD).unwrap())
        .encode()
        .unwrap();
    let (metadata, ciphertext) = keys
        .seal_content(artifact(), ContentRetention::Checkpoint, &snapshot)
        .unwrap();
    message(signed(
        key,
        next_seq(peer, key),
        OpKind::Change,
        ciphertext,
        Some(metadata),
        now,
    ))
}

fn admit(peer: &mut StatePeer, key: &SigningKey, message: NeighbourMsg, now: SystemTime) {
    let report = peer.receive_from_report(origin(key), message, now).unwrap();
    assert!(
        matches!(report.primary(), InputDisposition::Accepted(_)),
        "admitted with exact evidence"
    );
}

/// A key holder's opener: any epoch it holds, compactions included.
fn open(keys: &ChannelContentKeys, op: &SignedOp) -> Result<Vec<u8>, String> {
    let metadata = op.encryption.ok_or("not sealed")?;
    let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
    keys.open_content(op.artifact, &metadata, &ciphertext)
        .map(|plaintext| plaintext.to_vec())
        .map_err(|error| error.to_string())
}

/// A compaction of a record small enough for one op.
fn mint(
    peer: &mut StatePeer,
    keys: &ChannelContentKeys,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    let [compaction]: [NeighbourMsg; 1] = mint_pieces(peer, keys, key, now)?
        .try_into()
        .map_err(|_| "the compaction spans several ops")?;
    Ok(compaction)
}

/// A compaction and the segments it names, as the messages that carry them.
fn mint_pieces(
    peer: &mut StatePeer,
    keys: &ChannelContentKeys,
    key: &SigningKey,
    now: SystemTime,
) -> Result<Vec<NeighbourMsg>, String> {
    let mut ops = peer.publication().draft_sealed_compaction(
        (APP, RECORD),
        None,
        &mut |op| open(keys, op),
        &mut |plaintext| {
            keys.seal_content(artifact(), ContentRetention::of_frame(plaintext), plaintext)
                .map_err(|error| error.to_string())
        },
        key.verifying_key().to_bytes(),
        now,
    )?;
    for op in &mut ops {
        collab_types::sign_op(op, key).map_err(|error| error.to_string())?;
    }
    peer.publication_mut()
        .admit_sealed_compaction(APP, RECORD, ops, now)
}

/// A compaction of `superseded`, sealed with `keys` but not signed.
fn wrapper(
    peer: &StatePeer,
    keys: &ChannelContentKeys,
    superseded: &[SignedOp],
) -> (SealedCompaction, EncryptionMetadata) {
    let snapshot = DocumentMutation::Snapshot(peer.document(APP, RECORD).unwrap())
        .encode()
        .unwrap();
    let (metadata, ciphertext) = keys
        .seal_content(artifact(), ContentRetention::Checkpoint, &snapshot)
        .unwrap();
    (
        SealedCompaction::new(superseded, ciphertext).unwrap(),
        metadata,
    )
}

fn forged(
    peer: &StatePeer,
    key: &SigningKey,
    (wrapper, metadata): &(SealedCompaction, EncryptionMetadata),
    now: SystemTime,
) -> NeighbourMsg {
    message(signed(
        key,
        next_seq(peer, key),
        OpKind::Compaction,
        wrapper.encode().unwrap(),
        Some(*metadata),
        now,
    ))
}

fn refused(peer: &mut StatePeer, key: &SigningKey, message: NeighbourMsg, now: SystemTime) {
    refused_with(peer, key, message, now, "");
}

fn refused_with(
    peer: &mut StatePeer,
    key: &SigningKey,
    message: NeighbourMsg,
    now: SystemTime,
    reason: &str,
) {
    let before = peer.durable().unwrap();
    let error = peer.receive_from(origin(key), message, now).unwrap_err();
    assert!(error.contains(reason), "{error:?} is not {reason:?}");
    assert_eq!(peer.durable().unwrap(), before, "the record is unchanged");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_minted_compaction_after_a_rotation_opens_as_a_current_write_with_every_change() {
    let mut f = fixture();
    let now = f.now;
    let mut author = Author::new(&f.host);
    let before_rotation = f.keys();
    let first = write(
        &f.host,
        &f.other,
        &before_rotation,
        &author.edit("first", SECRET),
        now,
    );
    admit(&mut f.host, &f.other, first, now);
    let current = f.rotate();
    assert!(current.epoch() > before_rotation.epoch());
    let second = write(
        &f.host,
        &f.owner,
        &current,
        &author.edit("second", "2"),
        now,
    );
    admit(&mut f.host, &f.owner, second, now);
    let base = f.host.document(APP, RECORD).unwrap();
    let mut replica = f.host.clone();

    let minted = mint(&mut f.host, &current, &f.owner, now).unwrap();
    let compaction = envelope_of(&minted);
    assert_eq!(
        held(&f.host),
        vec![compaction.clone()],
        "history superseded"
    );
    assert_eq!(compaction.kind, OpKind::Compaction);
    let metadata = compaction.encryption.unwrap();
    assert_eq!(metadata.epoch, current.epoch());
    assert_eq!(metadata.retention(), Some(ContentRetention::Checkpoint));
    let plaintext = current
        .open_current_content(
            artifact(),
            &metadata,
            &collab_types::sealed_ciphertext(&compaction).unwrap(),
        )
        .unwrap();
    let DocumentMutation::Snapshot(bytes) = DocumentMutation::decode(&plaintext).unwrap() else {
        panic!("a compaction opens to a snapshot");
    };
    let merged = AutoCommit::load(&bytes).unwrap();
    for (key, value) in [("value", "base"), ("first", SECRET), ("second", "2")] {
        assert_eq!(
            merged.get(ROOT, key).unwrap().unwrap().0.as_str(),
            Some(value),
            "the compaction holds {key}"
        );
    }

    admit(&mut replica, &f.owner, minted, now);
    assert_eq!(held(&replica), held(&f.host), "another replica supersedes");
    assert_eq!(
        f.host.document(APP, RECORD).unwrap(),
        base,
        "nothing is merged where no key is held"
    );
    let durable = f.host.durable().unwrap();
    assert_eq!(durable[0], 6);
    assert!(
        !contains(&durable, SECRET.as_bytes()),
        "no plaintext is kept"
    );
    let restored = f.restore(&durable).unwrap();
    assert_eq!(held(&restored), held(&f.host), "survives restart");
}

#[test]
fn minting_aborts_without_a_trace_when_any_write_cannot_be_compacted() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    for value in ["1", "2"] {
        let write = write(&f.host, &f.other, &keys, &author.edit("value", value), now);
        admit(&mut f.host, &f.other, write, now);
    }
    let before = f.host.durable().unwrap();
    let keys = &keys;
    let sealer = |artifact, retention| {
        move |plaintext: &[u8]| {
            keys.seal_content(artifact, retention, plaintext)
                .map_err(|error| error.to_string())
        }
    };
    let checkpoint = sealer(artifact(), ContentRetention::Checkpoint);
    let mut opened = 0;
    let attempts: Vec<(&str, Opener<'_>, Sealer<'_>)> = vec![
        (
            "a write that does not open",
            Box::new(move |op: &SignedOp| {
                opened += 1;
                if opened == 2 {
                    return Err("stale key".to_owned());
                }
                open(keys, op)
            }),
            Box::new(checkpoint),
        ),
        (
            "a write that does not decode",
            Box::new(|_: &SignedOp| Ok(b"not a document mutation".to_vec())),
            Box::new(checkpoint),
        ),
        (
            "a snapshot sealed as a delta",
            Box::new(|op: &SignedOp| open(keys, op)),
            Box::new(sealer(artifact(), ContentRetention::Delta)),
        ),
        (
            "a snapshot sealed for another artifact",
            Box::new(|op: &SignedOp| open(keys, op)),
            Box::new(sealer(
                collab_types::ArtifactId::new(),
                ContentRetention::Checkpoint,
            )),
        ),
    ];
    for (why, mut open, mut seal) in attempts {
        assert!(
            f.host
                .publication()
                .draft_sealed_compaction(
                    (APP, RECORD),
                    None,
                    &mut *open,
                    &mut *seal,
                    f.owner.verifying_key().to_bytes(),
                    now
                )
                .is_err(),
            "{why} is compacted"
        );
        assert_eq!(
            f.host.durable().unwrap(),
            before,
            "{why} changed the record"
        );
    }
}

type Opener<'a> = Box<dyn FnMut(&SignedOp) -> Result<Vec<u8>, String> + 'a>;
type Sealer<'a> = Box<dyn FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + 'a>;

#[test]
fn a_compaction_merges_writes_admitted_out_of_causal_order() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let (first, second) = (author.edit("value", "1"), author.edit("value", "2"));
    let late = write(&f.host, &f.other, &keys, &second, now);
    admit(&mut f.host, &f.other, late, now);
    let early = write(&f.host, &f.owner, &keys, &first, now);
    admit(&mut f.host, &f.owner, early, now);
    let minted = envelope_of(&mint(&mut f.host, &keys, &f.owner, now).unwrap());
    let DocumentMutation::Snapshot(bytes) =
        DocumentMutation::decode(&open(&keys, &minted).unwrap()).unwrap()
    else {
        panic!("a snapshot");
    };
    let merged = AutoCommit::load(&bytes).unwrap();
    assert_eq!(
        merged.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some("2")
    );
}

#[test]
fn a_history_missing_a_write_opens_without_it_and_keeps_what_waits_for_it() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let (first, second) = (author.edit("first", "1"), author.edit("second", "2"));
    let unreadable = author.edit("third", "3");
    // `second` builds on `first`, which this replica never got and which
    // `second`'s own writer never sent: it reads as stuck behind that change.
    // Another device bringing it after all (below) still completes the record.
    // The write after it does not open with the keys this key holder has.
    let late = write(&f.host, &f.other, &keys, &second, now);
    admit(&mut f.host, &f.other, late, now);
    let foreign = ChannelGroup::create(channel(), MemberKey::generate("other").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    let sealed_apart = write(&f.host, &f.other, &foreign, &unreadable, now);
    admit(&mut f.host, &f.other, sealed_apart, now);
    let mut read_write = |op: &SignedOp| open(&keys, op);

    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut read_write)
        .unwrap()
        .expect("the record is held");
    let read = AutoCommit::load(&opened.document).expect("what merges opens as a whole record");
    assert_eq!(
        read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some("base"),
        "the record reads"
    );
    assert!(read.get(ROOT, "second").unwrap().is_none());
    let other = *Ed25519PublicKey::from_bytes(f.other.verifying_key().to_bytes()).as_bytes();
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [
                UnmergedWrite { author: a, seq: 1, reason: UnmergedReason::BehindUnsent { missing } },
                UnmergedWrite { author: b, seq: 2, reason: UnmergedReason::Unreadable(_) },
            ] if *a == other && *b == other && missing.len() == 1
        ),
        "{:?}",
        opened.unmerged
    );
    assert_eq!(held(&f.host).len(), 2, "every write is kept");
    assert!(
        mint(&mut f.host, &keys, &f.owner, now).is_err(),
        "a history that leaves a write out is never compacted"
    );
    // A restart keeps them too.
    let mut restored = f.restore(&f.host.durable().unwrap()).unwrap();
    assert_eq!(held(&restored), held(&f.host));

    // Once what it waits for arrives, the write is in the record.
    let early = write(&restored, &f.owner, &keys, &first, now);
    admit(&mut restored, &f.owner, early, now);
    let opened = restored
        .publication()
        .opened_document(APP, RECORD, &mut read_write)
        .unwrap()
        .unwrap();
    let read = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(
        read.get(ROOT, "second").unwrap().unwrap().0.as_str(),
        Some("2")
    );
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [UnmergedWrite {
                reason: UnmergedReason::Unreadable(_),
                ..
            }]
        ),
        "{:?}",
        opened.unmerged
    );
}

#[test]
fn a_history_that_writes_a_sequence_twice_opens_with_the_second_write_named() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut first = Author::new(&f.host);
    let mut conflicting = Author::new(&f.host);
    // Two writers of one desk used one actor: each wrote the next sequence of
    // the same writer without seeing the other's change.
    let actor = automerge::ActorId::from(vec![0x42]);
    first.0.set_actor(actor.clone());
    conflicting.0.set_actor(actor);
    let one = write(
        &f.host,
        &f.owner,
        &keys,
        &first.edit("value", "canonical"),
        now,
    );
    let two = write(
        &f.host,
        &f.other,
        &keys,
        &conflicting.edit("value", "conflicting"),
        now,
    );
    admit(&mut f.host, &f.owner, one, now);
    admit(&mut f.host, &f.other, two, now);
    let mut read_write = |op: &SignedOp| open(&keys, op);

    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut read_write)
        .unwrap()
        .expect("the record is held");
    let read = AutoCommit::load(&opened.document).expect("the record reads as a whole document");
    assert_eq!(
        read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some("canonical"),
        "the record keeps the change of the sequence that reached it first"
    );
    let other = *Ed25519PublicKey::from_bytes(f.other.verifying_key().to_bytes()).as_bytes();
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [UnmergedWrite {
                author: a,
                seq: 1,
                reason: UnmergedReason::AlreadyHeld { actor: written, seq: 1 },
            }] if *a == other && written == "42"
        ),
        "{:?}",
        opened.unmerged
    );
    assert_eq!(
        held(&f.host).len(),
        2,
        "the write the record cannot hold is kept"
    );

    // A compaction replaces the history, so it folds the write whose sequence
    // the record already holds: no reader can ever carry both changes.
    mint(&mut f.host, &keys, &f.owner, now).expect("a fork does not block the compaction");
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut read_write)
        .unwrap()
        .unwrap();
    let read = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(
        read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some("canonical")
    );
    assert!(opened.unmerged.is_empty());
}

/// A public workspace's chat as its governor kept it after hard stops, opened
/// with the channel's keys: the record's base, the last member snapshot,
/// the governor's own write, then two visitors' writes that build on a
/// change of a third visitor's snapshot the governor had taken in and lost
/// at a stop. `lost` is that snapshot, as the relay still retained it.
/// Its postcard `Vec<(label, bytes)>`, hex.
const LOUNGE_CHAT_GAP: &str = include_str!("fixtures/lounge-chat-gap.hex");

#[test]
fn a_chat_that_lost_a_write_at_a_hard_stop_opens_and_completes_once_it_returns() {
    let fixture: Vec<(String, Vec<u8>)> = postcard::from_bytes(
        &hex::decode(LOUNGE_CHAT_GAP.split_whitespace().collect::<String>()).unwrap(),
    )
    .unwrap();
    let [(_, base), history @ .., (_, lost)] = fixture.as_slice() else {
        panic!("a base, a history and the lost write")
    };
    let now = SystemTime::UNIX_EPOCH;
    // Only the plaintext matters here: each write opens to its own.
    let op = |seq: u64| {
        SignedOp::new_unsigned(
            Vec::new(),
            artifact(),
            Ed25519PublicKey::from_bytes([u8::try_from(seq).unwrap(); 32]),
            collab_types::document_attestation_id(APP, WORKSPACE, RECORD),
            OpKind::Change,
            now,
            seq,
        )
    };
    let mut plaintexts: std::collections::BTreeMap<u64, Vec<u8>> = history
        .iter()
        .zip(1..)
        .map(|((_, plaintext), seq)| (seq, plaintext.clone()))
        .collect();
    let mut document = Document {
        doc: tilo_automerge_isolate::load(base)
            .map(crate::cached_document::CachedDocument::from)
            .unwrap(),
        channel: InboundState::default(),
        encryption_required: true,
        covered: std::collections::BTreeMap::new(),
        covered_snapshot: None,
        sealed: plaintexts
            .keys()
            .copied()
            .map(op)
            .collect::<Vec<_>>()
            .into(),
    };
    let missing = "32cc8086d6eb9538799a44d9b6e5229f22d7626ef79e2d3ec867898a01224cf1";

    let opened = {
        let mut read_write = |op: &SignedOp| Ok(plaintexts[&op.seq].clone());
        assert_eq!(
            sealed_compaction::merge_sealed(&document, &mut read_write).unwrap_err(),
            "sealed history does not merge into a complete record",
            "as the workspace service refused to start on it"
        );
        sealed_compaction::open_sealed(&document, &mut read_write).unwrap()
    };
    let waiting: Vec<(u64, &UnmergedReason)> = opened
        .unmerged
        .iter()
        .map(|write| (write.seq, &write.reason))
        .collect();
    let gap = UnmergedReason::MissingDependencies {
        missing: vec![missing.to_owned()],
    };
    assert_eq!(
        waiting,
        [(3, &gap), (4, &gap)],
        "both visitors' writes wait for the lost change"
    );
    let mut read = AutoCommit::load(&opened.document).expect("the rest opens as a whole record");
    let governor = "54b2fcc8";
    assert!(
        read.get_heads()
            .iter()
            .any(|head| head.to_string().starts_with(governor)),
        "the governor's own write is read"
    );

    // The lost write returns: the record is whole again.
    plaintexts.insert(5, lost.clone());
    document.sealed.push(op(5));
    let mut read_write = |op: &SignedOp| Ok(plaintexts[&op.seq].clone());
    let opened = sealed_compaction::open_sealed(&document, &mut read_write).unwrap();
    assert_eq!(opened.unmerged, []);
    assert_eq!(
        opened.document,
        sealed_compaction::merge_sealed(&document, &mut read_write).unwrap()
    );
}

/// The record's own writers fold its sealed history, not only the workspace's
/// governor: a device the record admits as a writer of it holds the channel
/// keys, so compacting is part of writing it. A replica without the keys
/// takes the checkpoint in, and the writes after it.
#[test]
fn a_records_writer_compacts_the_history_it_holds() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    for value in ["1", "2"] {
        let write = write(&f.host, &f.other, &keys, &author.edit("value", value), now);
        admit(&mut f.host, &f.other, write, now);
    }
    let mut replica = f.host.clone();
    // The workspace's editor is one of the record's writers.
    let editor = f.editor.clone();
    let compaction = mint(&mut f.host, &keys, &editor, now).unwrap();
    assert_eq!(
        held(&f.host),
        vec![envelope_of(&compaction)],
        "the writer folded the history it holds"
    );
    admit(&mut replica, &editor, compaction, now);
    assert_eq!(
        held(&replica),
        held(&f.host),
        "a replica without the keys takes the checkpoint in"
    );

    // The record goes on: the writer's next write builds on the checkpoint,
    // and the replica follows it.
    let later = write(&f.host, &f.other, &keys, &author.edit("value", "3"), now);
    admit(&mut f.host, &f.other, later.clone(), now);
    admit(&mut replica, &f.other, later, now);
    assert_eq!(held(&replica), held(&f.host), "the write after it joins");
    assert_eq!(
        held(&replica).len(),
        2,
        "the checkpoint and the write after it"
    );

    // A key holder reads the record whole from what the replica kept.
    let restored = f.restore(&replica.durable().unwrap()).unwrap();
    let mut read_write = |op: &SignedOp| open(&keys, op);
    let opened = restored
        .publication()
        .opened_document(APP, RECORD, &mut read_write)
        .unwrap()
        .unwrap();
    let read = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(
        read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some("3")
    );
}

/// A record fills its sealed bound and the device writing it folds the
/// history itself: the write past the bound is refused first, then the
/// device publishes its own checkpoint and the record goes on. Nothing else
/// takes part.
#[test]
fn a_member_writes_past_its_bound_without_a_workspace_host() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let editor = f.editor.clone();
    let mut author = Author::new(&f.host);
    let mut written = 0;
    while held(&f.host).len() < MAX_SEALED_OPS {
        let value = written.to_string();
        let sealed = write(&f.host, &editor, &keys, &author.edit("value", &value), now);
        match f.host.receive_from_report(origin(&editor), sealed, now) {
            Ok(report) => {
                assert!(matches!(report.primary(), InputDisposition::Accepted(_)));
                written += 1;
            }
            Err(error) => {
                assert!(
                    error.to_string().contains("sealed record history is full"),
                    "{error}"
                );
                break;
            }
        }
    }
    assert_eq!(
        written, MAX_SEALED_OPS,
        "the bound refuses the write past it"
    );
    assert!(
        !f.host.publication().sealed_records_to_compact().is_empty(),
        "a full record is due for its writer's checkpoint"
    );

    let checkpoint = mint(&mut f.host, &keys, &editor, now).unwrap();
    assert_eq!(held(&f.host), vec![envelope_of(&checkpoint)]);

    // The record goes on past the bound it refused at.
    for value in MAX_SEALED_OPS..MAX_SEALED_OPS + 8 {
        let sealed = write(
            &f.host,
            &editor,
            &keys,
            &author.edit("value", &value.to_string()),
            now,
        );
        admit(&mut f.host, &editor, sealed, now);
    }
    assert_eq!(
        held(&f.host).len(),
        9,
        "the checkpoint and the writes after it"
    );
    assert!(f.host.publication().sealed_records_to_compact().is_empty());
    let mut read_write = |op: &SignedOp| open(&keys, op);
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut read_write)
        .unwrap()
        .unwrap();
    let read = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(
        read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some((MAX_SEALED_OPS + 7).to_string().as_str()),
        "every write before the checkpoint is still in the record"
    );
}

#[test]
fn forged_compactions_are_refused() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let write = write(&f.host, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, write, now);
    let history = held(&f.host);
    let valid = wrapper(&f.host, &keys, &history);

    let mut tampered = envelope_of(&forged(&f.host, &f.owner, &valid, now));
    let mut altered = valid.0.clone();
    altered.ciphertext[0] ^= 1;
    tampered.payload = altered.encode().unwrap();
    refused(&mut f.host, &f.owner, message(tampered), now);

    let stranger = SigningKey::from_bytes(&[77; 32]);
    let mut impostor = envelope_of(&forged(&f.host, &stranger, &valid, now));
    impostor.author = Ed25519PublicKey::from_bytes(f.owner.verifying_key().to_bytes());
    impostor.seq = next_seq(&f.host, &f.owner);
    refused(&mut f.host, &f.owner, message(impostor), now);

    let mut other_record = envelope_of(&forged(&f.host, &f.owner, &valid, now));
    other_record.artifact = collab_types::document_artifact_id(APP, WORKSPACE, "tab:2");
    collab_types::sign_op(&mut other_record, &f.owner).unwrap();
    refused(&mut f.host, &f.owner, message(other_record), now);

    let mut other_channel = valid.1;
    other_channel.channel = collab_types::ChannelId::new();
    let forgery = forged(&f.host, &f.owner, &(valid.0.clone(), other_channel), now);
    refused(&mut f.host, &f.owner, forgery, now);

    let mut delta = valid.1;
    delta.scheme = collab_types::EncryptionScheme::ContentKey {
        retention: ContentRetention::Delta,
    };
    let forgery = forged(&f.host, &f.owner, &(valid.0, delta), now);
    refused(&mut f.host, &f.owner, forgery, now);

    let cleartext = signed(
        &f.owner,
        next_seq(&f.host, &f.owner),
        OpKind::Compaction,
        DocumentMutation::Snapshot(f.host.document(APP, RECORD).unwrap())
            .encode()
            .unwrap(),
        None,
        now,
    );
    refused_with(
        &mut f.host,
        &f.owner,
        message(cleartext),
        now,
        "only ever sealed",
    );
    assert_eq!(held(&f.host), history);
}

#[test]
fn a_compaction_under_a_stale_epoch_is_refused() {
    let mut f = fixture();
    let now = f.now;
    let stale = f.keys();
    let current = f.rotate();
    let mut author = Author::new(&f.host);
    let write = write(&f.host, &f.other, &current, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, write, now);
    let compaction = wrapper(&f.host, &stale, &held(&f.host));
    let owner = f.owner.clone();
    let forgery = forged(&f.host, &owner, &compaction, now);
    refused_with(&mut f.host, &owner, forgery, now, "stale epoch");
}

#[test]
fn a_compaction_founds_a_route_a_replica_holds_none_of() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let message = write(&f.host, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, message, now);
    let compaction = mint(&mut f.host, &keys, &f.owner, now).unwrap();

    let mut fresh = StatePeer::new(
        WORKSPACE,
        f.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    fresh.select_admission_v5().unwrap();
    fresh
        .receive_from(origin(&f.owner), f.host.workspace_message(), now)
        .unwrap();
    assert!(
        fresh.document(APP, RECORD).is_none(),
        "the replica holds no history of the record"
    );
    admit(&mut fresh, &f.owner, compaction.clone(), now);
    assert_eq!(held(&fresh), vec![envelope_of(&compaction)]);
}

#[test]
fn a_sealed_checkpoint_founds_the_route_its_deltas_change() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let first = write(&f.host, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, first, now);
    let later = write(&f.host, &f.other, &keys, &author.edit("value", "2"), now);
    admit(&mut f.host, &f.other, later.clone(), now);
    let saved = checkpoint(&f.host, &keys, &f.owner, now);

    let mut fresh = StatePeer::new(
        WORKSPACE,
        f.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    fresh.select_admission_v5().unwrap();
    fresh
        .receive_from(origin(&f.owner), f.host.workspace_message(), now)
        .unwrap();
    // A change whose checkpoint this replica never saw waits for one.
    let waited = fresh
        .receive_from_report(origin(&f.other), later.clone(), now)
        .unwrap();
    assert!(
        matches!(
            waited.primary(),
            InputDisposition::RetryLater(MissingPrerequisite::RecordCheckpoint)
        ),
        "waited for the record's checkpoint: {:?}",
        waited.primary()
    );
    assert!(
        fresh.document(APP, RECORD).is_none(),
        "the replica holds no history of the record"
    );
    // The checkpoint is the whole record as its writer saved it, so it founds
    // the route for a replica holding none of it, and the change that waited
    // is decided again behind it.
    admit(&mut fresh, &f.owner, saved.clone(), now);
    assert_eq!(held(&fresh), vec![envelope_of(&saved), envelope_of(&later)]);
}

#[test]
fn a_replayed_or_superseded_compaction_is_refused() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let first = write(&f.host, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, first, now);
    let mut replica = f.host.clone();
    let mut lagging = f.host.clone();
    let older = mint(&mut f.host, &keys, &f.owner, now).unwrap();
    let after = f.host.durable().unwrap();
    let replay = f
        .host
        .receive_from_report(origin(&f.owner), older.clone(), now)
        .unwrap();
    assert!(!matches!(replay.primary(), InputDisposition::Accepted(_)));
    assert_eq!(f.host.durable().unwrap(), after, "a replay changes nothing");

    let second = write(&f.host, &f.other, &keys, &author.edit("value", "2"), now);
    admit(&mut f.host, &f.other, second.clone(), now);
    let newer = mint(&mut f.host, &keys, &f.owner, now).unwrap();
    assert_eq!(held(&f.host), vec![envelope_of(&newer)]);

    admit(&mut replica, &f.owner, older.clone(), now);
    admit(&mut replica, &f.other, second.clone(), now);
    admit(&mut replica, &f.owner, newer.clone(), now);
    assert_eq!(held(&replica), held(&f.host));
    let settled = replica.durable().unwrap();
    replica
        .receive_from_report(origin(&f.owner), older, now)
        .unwrap();
    assert_eq!(
        replica.durable().unwrap(),
        settled,
        "an older compaction after a newer one changes nothing"
    );

    admit(&mut lagging, &f.other, second, now);
    let owner = f.owner.clone();
    refused_with(
        &mut lagging,
        &owner,
        newer,
        now,
        "covers writes this replica has not admitted",
    );
}

#[test]
fn a_partial_compaction_is_refused() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let mut holder = f.host.clone();
    let writes: Vec<NeighbourMsg> = ["1", "2", "3"]
        .iter()
        .map(|value| {
            let message = write(&holder, &f.other, &keys, &author.edit("value", value), now);
            admit(&mut holder, &f.other, message.clone(), now);
            message
        })
        .collect();
    let all = held(&holder);
    let owner = f.owner.clone();

    admit(&mut f.host, &f.other, writes[0].clone(), now);
    let forgery = forged(&f.host, &owner, &wrapper(&f.host, &keys, &all), now);
    refused_with(&mut f.host, &owner, forgery, now, "has not admitted");

    let mut gap = f.host.clone();
    admit(&mut gap, &f.other, writes[2].clone(), now);
    let forgery = forged(&gap, &owner, &wrapper(&gap, &keys, &all), now);
    refused_with(&mut gap, &owner, forgery, now, "partial");

    admit(&mut f.host, &f.other, writes[1].clone(), now);
    let (mut other_set, metadata) = wrapper(&f.host, &keys, &all[..2]);
    other_set.prefix_hash = collab_types::hash_sealed_prefix(&all[..1]).unwrap();
    let forgery = forged(&f.host, &owner, &(other_set, metadata), now);
    refused_with(&mut f.host, &owner, forgery, now, "partial");

    let (mut nothing, metadata) = wrapper(&f.host, &keys, &all[..1]);
    nothing.covers = vec![(
        Ed25519PublicKey::from_bytes(owner.verifying_key().to_bytes()),
        floor(&f.host, &owner),
    )];
    nothing.prefix_hash = collab_types::hash_sealed_prefix(std::iter::empty()).unwrap();
    let forgery = forged(&f.host, &owner, &(nothing, metadata), now);
    refused_with(&mut f.host, &owner, forgery, now, "supersedes nothing");

    let first = mint(&mut f.host, &keys, &owner, now).unwrap();
    let third = write(&f.host, &f.other, &keys, &author.edit("value", "4"), now);
    admit(&mut f.host, &f.other, third, now);
    let history = held(&f.host);
    assert_eq!(history[0], envelope_of(&first));
    let forgery = forged(
        &f.host,
        &owner,
        &wrapper(&f.host, &keys, &history[1..]),
        now,
    );
    refused_with(
        &mut f.host,
        &owner,
        forgery,
        now,
        "leaves an earlier compaction",
    );
}

#[test]
fn a_full_history_refuses_writes_until_a_verified_compaction_supersedes_it() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let first = write(&f.host, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, first, now);
    let one = held(&f.host).remove(0);
    f.host.core.documents.get_mut(&route()).unwrap().sealed = vec![one; MAX_SEALED_OPS].into();
    let more = write(&f.host, &f.other, &keys, &author.edit("value", "2"), now);
    let other = f.other.clone();
    refused_with(
        &mut f.host,
        &other,
        more.clone(),
        now,
        "draft_sealed_compaction",
    );

    let compaction = forged(
        &f.host,
        &f.owner,
        &wrapper(&f.host, &keys, &held(&f.host)),
        now,
    );
    admit(&mut f.host, &f.owner, compaction.clone(), now);
    assert_eq!(held(&f.host), vec![envelope_of(&compaction)]);
    admit(&mut f.host, &f.other, more.clone(), now);
    assert_eq!(
        held(&f.host),
        vec![envelope_of(&compaction), envelope_of(&more)]
    );
}

#[test]
fn a_compacted_record_restores_and_a_tampered_stored_compaction_does_not() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let first = write(&f.host, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.other, first.clone(), now);
    mint(&mut f.host, &keys, &f.owner, now).unwrap();
    let second = write(&f.host, &f.other, &keys, &author.edit("value", "2"), now);
    admit(&mut f.host, &f.other, second, now);
    let bytes = f.host.durable().unwrap();
    assert_eq!(bytes[0], 6);
    assert_eq!(held(&f.restore(&bytes).unwrap()), held(&f.host));

    let rewrite = |change: &dyn Fn(&mut Vec<SignedOp>)| {
        let mut value: DurableV6<'_> = postcard::from_bytes(&bytes).unwrap();
        change(&mut value.sealed[0].1);
        postcard::to_allocvec(&value).unwrap()
    };
    let covered = envelope_of(&first);
    let owner = f.owner.clone();
    for (tampered, why) in [
        (
            rewrite(&|history| history.swap(0, 1)),
            "a compaction after a write",
        ),
        (
            rewrite(&|history| history.insert(1, covered.clone())),
            "a later write the compaction covers",
        ),
        (
            rewrite(&|history| {
                history[0].timestamp += Duration::from_secs(1);
                collab_types::sign_op(&mut history[0], &owner).unwrap();
            }),
            "a compaction without exact admission evidence",
        ),
        (
            rewrite(&|history| {
                let last = history[0].payload.len() - 1;
                history[0].payload[last] ^= 1;
            }),
            "an altered compaction",
        ),
    ] {
        assert!(f.restore(&tampered).is_err(), "{why} restores");
    }
}

#[test]
fn a_history_near_its_bound_is_due_for_compaction() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let first = write(&f.host, &f.editor, &keys, &author.edit("value", "1"), now);
    admit(&mut f.host, &f.editor, first, now);
    assert!(f.host.publication().sealed_records_to_compact().is_empty());
    let one = held(&f.host).remove(0);
    f.host.core.documents.get_mut(&route()).unwrap().sealed =
        vec![one; MAX_SEALED_OPS / 4 * 3].into();
    assert_eq!(
        f.host.publication().sealed_records_to_compact(),
        vec![(APP.to_owned(), RECORD.to_owned())]
    );
}

/// Many records each short of their own bound fill the workspace's exact
/// admission index all the same: once it is half full, every sealed history
/// worth folding is due, and compacting it frees the index.
#[test]
fn a_half_full_admission_index_makes_every_sealed_history_due() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    for index in 0..super::sealed_compaction::PRESSED_HISTORY {
        let next = write(
            &f.host,
            &f.other,
            &keys,
            &author.edit("value", &index.to_string()),
            now,
        );
        admit(&mut f.host, &f.other, next, now);
    }
    assert!(f.host.publication().sealed_records_to_compact().is_empty());
    let mut sequence = 0;
    while !f.host.core.exact_admissions.pressed() {
        sequence += 1;
        f.host
            .core
            .exact_admissions
            .insert(&crate::admission::DocumentAdmission {
                key: crate::admission::AdmissionKey {
                    workspace: WORKSPACE.into(),
                    app: APP.into(),
                    record: "elsewhere".into(),
                    author: [9; 32],
                    sequence,
                },
                operation_id: [1; 32],
            })
            .unwrap();
    }
    assert_eq!(
        f.host.publication().sealed_records_to_compact(),
        vec![(APP.to_owned(), RECORD.to_owned())]
    );
    let before = f.host.core.exact_admissions.len();
    mint(&mut f.host, &keys, &f.owner, now).unwrap();
    assert!(
        f.host.core.exact_admissions.len() < before,
        "the compaction folded the record's evidence"
    );
}

#[test]
fn the_governor_publishes_its_compaction_behind_the_records_receipt() {
    let f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut host = f.host.clone();
    let mut author = Author::new(&host);
    for value in ["1", "2", "3"] {
        let sealed = write(&host, &f.editor, &keys, &author.edit("value", value), now);
        admit(&mut host, &f.editor, sealed, now);
    }
    let mut before = host.clone();
    let compaction = envelope_of(&mint(&mut host, &keys, &f.owner, now).unwrap());
    let sealed = |messages: &[NeighbourMsg]| {
        messages
            .iter()
            .filter(|message| {
                matches!(message, NeighbourMsg::AppDocument { envelope, .. } if envelope.encryption.is_some())
            })
            .map(envelope_of)
            .collect::<Vec<_>>()
    };

    let mutation = host
        .clone()
        .publication_mut()
        .durable_mutation_messages(&before, &f.owner, now, 2)
        .unwrap();
    assert_eq!(sealed(&mutation), vec![compaction.clone()]);
    assert_eq!(
        mutation.last().map(envelope_of),
        Some(compaction.clone()),
        "behind the workspace change that carries the record's receipt"
    );
    let bootstrap = host
        .clone()
        .publication_mut()
        .durable_bootstrap_messages(&f.owner, now, 2)
        .unwrap();
    assert_eq!(sealed(&bootstrap), vec![compaction]);
    assert!(
        sealed(
            &host
                .clone()
                .publication_mut()
                .durable_mutation_messages(&host, &f.owner, now, 2)
                .unwrap()
        )
        .is_empty(),
        "a compaction already published is not published again"
    );
    assert!(
        sealed(
            &before
                .publication_mut()
                .durable_bootstrap_messages(&f.owner, now, 2)
                .unwrap()
        )
        .is_empty(),
        "other devices' sealed writes travel as themselves, never republished"
    );
}

#[test]
fn the_governor_acknowledges_sealed_writes_without_a_cleartext_checkpoint() {
    let f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut host = f.host.clone();
    let before = host.clone();
    let mut author = Author::new(&host);
    let sealed = write(&host, &f.editor, &keys, &author.edit("value", "1"), now);
    admit(&mut host, &f.editor, sealed.clone(), now);
    let watermark = |peer: &StatePeer| {
        crate::durability::entries(peer.admitted_workspace().unwrap())
            .unwrap()
            .get(&crate::coverage::route_key(APP, RECORD))
            .and_then(|receipt| {
                receipt
                    .coverage
                    .watermarks
                    .get(&hex::encode(f.editor.verifying_key().to_bytes()))
                    .copied()
            })
    };
    let documents = |messages: &[NeighbourMsg]| {
        messages
            .iter()
            .any(|message| matches!(message, NeighbourMsg::AppDocument { record_id, .. } if record_id == RECORD))
    };

    let mut published = host.clone();
    let messages = published
        .publication_mut()
        .durable_mutation_messages(&before, &f.owner, now, 2)
        .unwrap();
    assert!(
        !documents(&messages),
        "a cleartext checkpoint would downgrade the encrypted record"
    );
    assert_eq!(watermark(&published), Some(envelope_of(&sealed).seq));
    assert_eq!(held(&published), vec![envelope_of(&sealed)]);
    f.restore(&published.durable().unwrap()).unwrap();

    let mut bootstrapped = published.clone();
    let messages = bootstrapped
        .publication_mut()
        .durable_bootstrap_messages(&f.owner, now, 3)
        .unwrap();
    assert!(!documents(&messages));
    assert_eq!(watermark(&bootstrapped), Some(envelope_of(&sealed).seq));
}

#[test]
fn captured_history_replays_causal_order_and_continues_the_original_writer() {
    let mut f = fixture();
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let actor = automerge::ActorId::from(vec![0x31]);
    author.0.set_actor(actor.clone());
    let first = author.edit("value", "first");
    let second = author.edit("value", "second");
    let first = write(&f.host, &f.owner, &keys, &first, f.now);
    let second = write(&f.host, &f.other, &keys, &second, f.now);
    // A keyless replica admits the later causal mutation first.
    admit(&mut f.host, &f.other, second, f.now);
    admit(&mut f.host, &f.owner, first, f.now);
    let before = f.host.durable().unwrap();
    let restored_peer = f.restore(&before).unwrap();
    let capture = restored_peer
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            Some(&mut |op| open(&keys, op)),
        )
        .unwrap();
    let mut restored = AutoCommit::load(&capture.into_bytes()).unwrap();
    assert_eq!(restored.get_heads(), author.0.get_heads());
    assert_eq!(
        restored
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "second"
    );
    restored.set_actor(actor);
    let before_edit = restored.get_heads();
    restored.put(ROOT, "value", "third").unwrap();
    restored.commit();
    let next = restored.get_changes(&before_edit);
    assert_eq!(next.len(), 1);
    assert_eq!(
        next[0].seq(),
        3,
        "replay must retain the original writer's sequence"
    );
    author.0.merge_incoming(&mut restored).unwrap();
    assert_eq!(
        author
            .0
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "third"
    );
    assert_eq!(
        f.host.durable().unwrap(),
        before,
        "reading never changes durable admission"
    );
}

#[test]
fn captured_history_folds_a_writer_sequence_written_twice() {
    let mut f = fixture();
    let keys = f.keys();
    let mut first = Author::new(&f.host);
    let mut conflicting = Author::new(&f.host);
    let actor = automerge::ActorId::from(vec![0x41]);
    first.0.set_actor(actor.clone());
    conflicting.0.set_actor(actor);
    let first = first.edit("value", "canonical");
    let conflicting = conflicting.edit("value", "conflicting");
    let one = write(&f.host, &f.owner, &keys, &first, f.now);
    let two = write(&f.host, &f.other, &keys, &conflicting, f.now);
    admit(&mut f.host, &f.owner, one, f.now);
    admit(&mut f.host, &f.other, two, f.now);
    let before = f.host.durable().unwrap();
    let restored_peer = f.restore(&before).unwrap();
    let capture = restored_peer
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            Some(&mut |op| open(&keys, op)),
        )
        .expect("a reader carries the change that reached the record first, never both");
    let mut restored = AutoCommit::load(&capture.into_bytes()).unwrap();
    assert_eq!(
        restored.get(ROOT, "value").unwrap().unwrap().0.as_str(),
        Some("canonical")
    );
    // The reader continues that writer without writing its sequence twice.
    restored.set_actor(automerge::ActorId::from(vec![0x41]));
    let heads = restored.get_heads();
    restored.put(ROOT, "value", "next").unwrap();
    restored.commit();
    assert_eq!(restored.get_changes(&heads)[0].seq(), 2);
    assert_eq!(f.host.durable().unwrap(), before);
}

/// A late joiner is sent a record's whole sealed history, and a key holder's whole-record
/// checkpoint re-states what it already holds: a record's history outgrows one capture of it.
/// What a joiner reads is bounded by what its caller allows, as a record held in the clear is,
/// and its history is bounded where it is admitted. A fixed bound of one capture's own left a
/// joiner refused for good while every caller allowed several times the record's size.
#[test]
fn a_late_joiner_reads_a_sealed_history_past_one_capture() {
    /// Incompressible: a sealed history is as large as what it carries.
    const WRITE: usize = 1024 * 1024;
    /// Past what one capture hands out, and what a session reading a record allows.
    const WRITES: usize = 9;
    const ROOM: usize = 32 * 1024 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let now = f.now;
    let mut author = Author::new(&f.host);
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut value = vec![0_u8; WRITE];
    for byte in &mut value {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state.to_le_bytes()[0];
    }
    for round in 0..WRITES {
        let mut plaintext = value.clone();
        plaintext[0] = u8::try_from(round).unwrap();
        let edit = author.edit_bytes(&format!("value-{round}"), plaintext);
        let message = write(&f.host, &f.owner, &keys, &edit, now);
        admit(&mut f.host, &f.owner, message, now);
    }
    let joiner = f.restore(&f.host.durable().unwrap()).unwrap();
    let read = |maximum: usize| {
        let mut kept = KeptRecord::default();
        joiner
            .open_document(
                f.authority.device_public_key,
                (APP, RECORD),
                now,
                maximum,
                Some(&mut |op| open(&keys, op)),
                &mut kept,
            )
            .expect("a joiner reads its record")
            .save()
    };
    let record = read(ROOM);
    assert!(
        record.len() > 8 * 1024 * 1024,
        "the record outgrew one capture"
    );
    assert_eq!(read(ROOM), record, "the whole record either way");
}

#[test]
fn captured_history_validates_route_signature_admission_and_watermarks() {
    let mut f = fixture();
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let plaintext = author.edit("value", SECRET);
    let op = write(&f.host, &f.owner, &keys, &plaintext, f.now);
    admit(&mut f.host, &f.owner, op, f.now);
    let before = f.host.durable().unwrap();
    for corruption in 0..4 {
        let mut wire: DurableV6<'_> = postcard::from_bytes(&before).unwrap();
        match corruption {
            0 => wire.sealed[0].1[0].signature.0[0] ^= 1,
            1 => wire.sealed[0].0.app = "other.app".into(),
            2 => {
                wire.sealed[0].1[0].payload[0] ^= 1;
                collab_types::sign_op(&mut wire.sealed[0].1[0], &f.owner).unwrap();
            }
            _ => {
                assert_eq!(corruption, 3);
                wire.base.base.documents[0].watermarks.clear();
            }
        }
        let corrupted = postcard::to_allocvec(&wire).unwrap();
        assert!(
            f.restore(&corrupted).is_err(),
            "full restore corruption {corruption}"
        );
    }
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn captured_history_missing_admitted_write_is_refused_before_opening() {
    let mut f = fixture();
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    for value in ["first", "second"] {
        let plaintext = author.edit("value", value);
        let message = write(&f.host, &f.owner, &keys, &plaintext, f.now);
        admit(&mut f.host, &f.owner, message, f.now);
    }
    let before = f.host.durable().unwrap();
    let mut wire: DurableV6<'_> = postcard::from_bytes(&before).unwrap();
    wire.sealed[0].1.remove(0);
    let incomplete = postcard::to_allocvec(&wire).unwrap();
    let restored_peer = f.restore(&incomplete).unwrap();
    let mut opened = 0;
    let result = restored_peer.capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1024 * 1024,
        Some(&mut |op| {
            opened += 1;
            open(&keys, op)
        }),
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Incomplete)
    ));
    assert_eq!(opened, 0, "incomplete coverage is not decrypted");
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn captured_history_denial_and_capacity_precede_opening_and_missing_keys_refuse() {
    let mut f = fixture();
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let plaintext = author.edit("value", SECRET);
    let message = write(&f.host, &f.owner, &keys, &plaintext, f.now);
    admit(&mut f.host, &f.owner, message, f.now);
    let before = f.host.durable().unwrap();
    let restored_peer = f.restore(&before).unwrap();
    let mut opened = 0;
    let result = restored_peer.capture_document(
        [0x77; 32],
        (APP, RECORD),
        f.now,
        1024 * 1024,
        Some(&mut |op| {
            opened += 1;
            open(&keys, op)
        }),
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Membership(_))
    ));
    assert_eq!(opened, 0, "an unauthenticated reader is not decrypted");
    let result = restored_peer.capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1,
        Some(&mut |op| {
            opened += 1;
            open(&keys, op)
        }),
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Capacity)
    ));
    assert_eq!(
        opened, 0,
        "the base exceeds the caller's bound before opening"
    );
    let wrong_keys = fixture().keys();
    let result = restored_peer.capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1024 * 1024,
        Some(&mut |op| open(&wrong_keys, op)),
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Opening(_))
    ));
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn captured_history_replays_compaction_and_requires_all_held_epochs() {
    let mut f = fixture();
    let old_keys = f.keys();
    let mut author = Author::new(&f.host);
    let plaintext = author.edit("value", "before rotation");
    let message = write(&f.host, &f.owner, &old_keys, &plaintext, f.now);
    admit(&mut f.host, &f.owner, message, f.now);
    mint(&mut f.host, &old_keys, &f.owner, f.now).unwrap();
    let new_keys = f.rotate();
    let plaintext = author.edit("value", "after rotation");
    let message = write(&f.host, &f.owner, &new_keys, &plaintext, f.now);
    admit(&mut f.host, &f.owner, message, f.now);
    let before = f.host.durable().unwrap();
    let restored_peer = f.restore(&before).unwrap();
    let capture = restored_peer
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            Some(&mut |op| {
                let keys = if op.encryption.unwrap().epoch == old_keys.epoch() {
                    &old_keys
                } else {
                    &new_keys
                };
                open(keys, op)
            }),
        )
        .unwrap();
    let mut restored = AutoCommit::load(&capture.into_bytes()).unwrap();
    assert_eq!(restored.get_heads(), author.0.get_heads());
    assert_eq!(
        restored
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap(),
        "after rotation"
    );
    assert!(matches!(
        restored_peer.capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            Some(&mut |op| open(&old_keys, op)),
        ),
        Err(crate::PeerDocumentReadError::Opening(_))
    ));
    let mut wire: DurableV6<'_> = postcard::from_bytes(&before).unwrap();
    wire.sealed[0].1.swap(0, 1);
    let misplaced = postcard::to_allocvec(&wire).unwrap();
    assert!(
        f.restore(&misplaced).is_err(),
        "a compaction is first, never a later write"
    );
    assert_eq!(f.host.durable().unwrap(), before);
}

/// Every round of writes a compaction folds leaves one mark per writer,
/// not an exact entry per write: the index stays the same size however
/// long the record runs. A folded write sent again is ignored, whatever its
/// bytes; above the mark a conflicting write is still refused.
#[test]
fn a_compaction_folds_its_writes_exact_evidence_into_one_mark_per_writer() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let mut first = None;
    let mut sizes = Vec::new();
    for round in 0..3 {
        for index in 0..200 {
            let key = if index % 2 == 0 {
                f.other.clone()
            } else {
                f.editor.clone()
            };
            let next = write(
                &f.host,
                &key,
                &keys,
                &author.edit("value", &format!("{round}.{index}")),
                now,
            );
            first.get_or_insert_with(|| next.clone());
            admit(&mut f.host, &key, next, now);
        }
        mint(&mut f.host, &keys, &f.owner, now).unwrap();
        sizes.push(f.host.core.exact_admissions.len());
    }
    // The first round also keeps the owner's seeded checkpoint; the next
    // compaction, by the same owner, folds that as well.
    assert_eq!(
        sizes,
        vec![2, 1, 1],
        "only the latest compaction stays exact"
    );
    let index = &f.host.core.exact_admissions;
    for writer in [&f.other, &f.editor] {
        let mark = index
            .marks()
            .find(|(key, _)| key.author == writer.verifying_key().to_bytes())
            .map(|(_, mark)| mark);
        assert_eq!(
            mark,
            Some(floor(&f.host, writer)),
            "a writer's mark is its last folded write"
        );
    }

    // The first write again, and a different write at its sequence: both
    // are in the snapshot already, so both are ignored.
    let first = first.unwrap();
    let report = f
        .host
        .receive_from_report(origin(&f.other), first.clone(), now)
        .unwrap();
    assert!(matches!(report.primary(), InputDisposition::Duplicate(_)));
    let mut rewritten = envelope_of(&first);
    rewritten.timestamp += Duration::from_secs(1);
    collab_types::sign_op(&mut rewritten, &f.other).unwrap();
    let report = f
        .host
        .receive_from_report(origin(&f.other), message(rewritten), now)
        .unwrap();
    assert!(matches!(report.primary(), InputDisposition::Duplicate(_)));
    assert_eq!(held(&f.host).len(), 1, "nothing folded comes back");

    // Above the mark the evidence is exact again.
    let above = write(&f.host, &f.other, &keys, &author.edit("value", "late"), now);
    admit(&mut f.host, &f.other, above.clone(), now);
    let mut conflicting = envelope_of(&above);
    conflicting.timestamp += Duration::from_secs(1);
    collab_types::sign_op(&mut conflicting, &f.other).unwrap();
    assert!(
        f.host
            .receive_from_report(origin(&f.other), message(conflicting), now)
            .is_err(),
        "a second write at an unfolded sequence conflicts"
    );

    // The marks are part of the durable state.
    let restored = f.restore(&f.host.durable().unwrap()).unwrap();
    assert_eq!(
        restored.core.exact_admissions.encode().unwrap(),
        f.host.core.exact_admissions.encode().unwrap()
    );
}

fn sealer(
    keys: &ChannelContentKeys,
    record: &str,
) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, record);
    move |plaintext| {
        keys.seal_content(artifact, ContentRetention::Delta, plaintext)
            .map_err(|error| error.to_string())
    }
}

#[test]
fn the_key_holder_seals_a_record_held_in_the_clear_from_its_next_write_on() {
    let f = fixture();
    let keys = f.keys();
    let mut host = f.host.clone();
    assert!(!host.publication().is_sealed_record(APP, RECORD));
    let mut document = AutoCommit::load(&host.document(APP, RECORD).unwrap()).unwrap();
    let heads = document.get_heads();
    document.put(ROOT, "value", "sealed").unwrap();
    document.commit_edit("fixture", "value");
    let message = host
        .publication_mut()
        .publish_sealed_incremental(
            APP,
            RECORD,
            document.save_after(&heads),
            &mut sealer(&keys, RECORD),
            &f.owner,
            f.now,
        )
        .unwrap();
    assert!(envelope_of(&message).encryption.is_some());
    assert!(host.publication().is_sealed_record(APP, RECORD));
    let opened = host
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert_eq!(
        AutoCommit::load(&opened.document)
            .unwrap()
            .get(ROOT, "value")
            .unwrap()
            .unwrap()
            .0
            .to_string(),
        "\"sealed\""
    );
    assert!(
        host.publication_mut()
            .publish_incremental(APP, RECORD, Vec::new(), &f.owner, f.now)
            .is_err()
    );

    let mut keyless = f.host.clone();
    admit(&mut keyless, &f.owner, message, f.now);
    assert!(keyless.publication().is_sealed_record(APP, RECORD));
}

/// A record is born from its owner's checkpoint, which holds nothing; its
/// key holder's first write, its genesis, is sealed.
#[test]
fn the_key_holder_writes_a_new_records_genesis_sealed() {
    let f = fixture();
    let record = "tab:2";
    let group = ChannelGroup::create(
        collab_types::document_channel_id(APP, WORKSPACE, record),
        MemberKey::generate("owner").unwrap(),
    )
    .unwrap();
    let keys = group.content_keys().unwrap();
    let mut genesis = AutoCommit::new();
    genesis.put(ROOT, "value", "born sealed").unwrap();
    genesis.commit_edit("fixture", "value");
    let mut host = f.host.clone();
    host.publication_mut()
        .publish_document(APP, record, AutoCommit::new().save(), &f.owner, f.now)
        .unwrap();
    let message = host
        .publication_mut()
        .publish_sealed_incremental(
            APP,
            record,
            genesis.save_after(&[]),
            &mut sealer(&keys, record),
            &f.owner,
            f.now,
        )
        .unwrap();
    assert!(host.publication().is_sealed_record(APP, record));
    assert_eq!(
        AutoCommit::load(&host.document(APP, record).unwrap())
            .unwrap()
            .get_heads(),
        Vec::new()
    );
    let opened = host
        .publication()
        .opened_document(APP, record, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert_eq!(
        AutoCommit::load(&opened.document).unwrap().get_heads(),
        genesis.get_heads()
    );
    // Its writer reads the record as it was written, before and after a
    // restart: the empty start in the clear is content the record carries.
    let read = |host: &StatePeer| {
        let capture = host
            .capture_document(
                f.owner.verifying_key().to_bytes(),
                (APP, record),
                f.now,
                64 * 1024,
                Some(&mut |op| open(&keys, op)),
            )
            .unwrap_or_else(|error| panic!("the record does not read: {error}"));
        AutoCommit::load(&capture.into_bytes()).unwrap().get_heads()
    };
    assert_eq!(read(&host), genesis.get_heads());
    let restored = f.restore(&host.durable().unwrap()).unwrap();
    assert_eq!(read(&restored), genesis.get_heads(), "after a restart");
    assert!(envelope_of(&message).encryption.is_some());
}

#[test]
fn a_record_its_key_holder_seals_is_never_published_in_the_clear_again() {
    let f = fixture();
    let mut host = f.host.clone();
    assert!(host.seal_record(APP, RECORD));
    assert!(host.publication().is_sealed_record(APP, RECORD));
    assert!(
        host.clone()
            .publication_mut()
            .publish_incremental(APP, RECORD, Vec::new(), &f.owner, f.now)
            .is_err()
    );
    let restored = f.restore(&host.durable().unwrap()).unwrap();
    assert!(restored.publication().is_sealed_record(APP, RECORD));
    let messages = host
        .publication_mut()
        .durable_bootstrap_messages(&f.owner, f.now, 2)
        .unwrap();
    assert!(!messages.iter().any(
        |message| matches!(message, NeighbourMsg::AppDocument { record_id, .. } if record_id == RECORD)
    ));
    assert!(!host.seal_record(APP, "tab:2"));
}

/// A cleartext write held here for what it builds on came before the seal in
/// the record's order: the seal waits behind it, so the record's base takes
/// in every write made before it sealed, however they arrive.
#[test]
fn a_seal_waits_behind_the_cleartext_writes_held_before_it() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    // A write in the clear names its action, as a device's executor signs it.
    let mut named = |key: &str, value: &str| {
        let heads = author.0.get_heads();
        author.0.put(ROOT, key, value).unwrap();
        author.0.commit_edit("fixture", key);
        DocumentMutation::Incremental(author.0.save_after(&heads))
            .encode()
            .unwrap()
    };
    let seq = next_seq(&f.host, &f.owner);
    let first = message(signed(
        &f.owner,
        seq,
        OpKind::Change,
        named("first", "1"),
        None,
        now,
    ));
    let second = message(signed(
        &f.owner,
        seq.checked_add(1).unwrap(),
        OpKind::Change,
        named("second", "2"),
        None,
        now,
    ));
    let sealed = named("sealed", "yes");
    let seal = write(&f.host, &f.other, &keys, &sealed, now);
    // `second` waits for `first`, which arrives after the seal.
    let report = f
        .host
        .receive_from_report(origin(&f.owner), second, now)
        .unwrap();
    assert!(matches!(report.primary(), InputDisposition::RetryLater(_)));
    f.host
        .receive_from_report(origin(&f.other), seal, now)
        .unwrap();
    f.host
        .receive_from_report(origin(&f.owner), first, now)
        .expect("a cleartext write before the seal is the record's base");
    assert!(f.host.publication().is_sealed_record(APP, RECORD));
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    let read = AutoCommit::load(&opened.document).unwrap();
    for (key, value) in [("first", "1"), ("second", "2"), ("sealed", "yes")] {
        assert_eq!(
            read.get(ROOT, key)
                .unwrap()
                .map(|(value, _)| value.to_string()),
            Some(format!("\"{value}\"")),
            "{key}"
        );
    }
}

/// Sealed changes that came before the record's checkpoint, more than the
/// held queue keeps: the ones let go are no base the checkpoint waits
/// behind. It founds the record, and the host is asked for what was let go.
#[test]
fn a_checkpoint_founds_its_record_behind_sealed_changes_let_go() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let mut changes = Vec::new();
    for n in 0..=held::MAX_HELD_INPUTS {
        let change = write(
            &f.host,
            &f.other,
            &keys,
            &author.edit("value", &n.to_string()),
            now,
        );
        admit(&mut f.host, &f.other, change.clone(), now);
        changes.push(change);
    }
    let saved = checkpoint(&f.host, &keys, &f.owner, now);

    let mut fresh = StatePeer::new(
        WORKSPACE,
        f.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    fresh.select_admission_v5().unwrap();
    fresh
        .receive_from(origin(&f.owner), f.host.workspace_message(), now)
        .unwrap();
    let mut let_go = 0;
    for change in changes {
        let report = fresh
            .receive_from_report(origin(&f.other), change, now)
            .unwrap();
        let_go += report.summary().dropped.len();
    }
    assert!(let_go > 0, "the held queue let a change go");
    let report = fresh
        .receive_from_report(origin(&f.owner), saved.clone(), now)
        .unwrap();
    assert!(
        matches!(report.primary(), InputDisposition::Accepted(_)),
        "the checkpoint founds the record: {:?}",
        report.primary()
    );
    assert!(report.replay_wanted, "what was let go is asked for again");
    assert_eq!(held(&fresh).first(), Some(&envelope_of(&saved)));
}

/// A record whose merged state no single op can carry, written on past the bound of its history,
/// stays writable: its compaction spans a signed head and the segments it names, a keyless
/// replica takes the set only whole, in place of the writes it covers, and keeps its history
/// within the bound. A device joining late takes the set from what that replica holds, and its
/// key holder reads the whole record with every change of its history.
#[test]
fn a_record_larger_than_one_op_compacts_in_segments_and_stays_writable() {
    const BASE: usize = 10;
    const PART: usize = 1024 * 1024;
    const EDIT: usize = 256 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, owner, editor) = (f.now, f.owner.clone(), f.editor.clone());
    let mut author = Author::new(&f.host);
    let mut written = 0usize;
    let mut compactions = Vec::new();
    for index in 0..BASE {
        let edit = author.edit_bytes(&format!("part-{index}"), noise(PART, index as u64));
        let sealed = write(&f.host, &editor, &keys, &edit, now);
        written += envelope_of(&sealed).payload.len();
        admit(&mut f.host, &editor, sealed, now);
    }
    for index in 0..64u64 {
        if !f.host.publication().sealed_records_to_compact().is_empty() {
            let mut holder = f.host.clone();
            let pieces = mint_pieces(&mut holder, &keys, &owner, now).unwrap();
            assert!(pieces.len() > 3, "a snapshot of 10 MiB spans several ops");
            // The segments alone change nothing; the set is taken whole.
            let before = held(&f.host);
            for piece in pieces.iter().skip(1) {
                f.host
                    .receive_from_report(origin(&owner), piece.clone(), now)
                    .unwrap();
                assert_eq!(held(&f.host), before, "a segment alone is taken");
            }
            admit(&mut f.host, &owner, pieces[0].clone(), now);
            let history = held(&f.host);
            assert_eq!(
                history.len(),
                pieces.len(),
                "the set replaced every write it covers"
            );
            written += history.iter().map(|op| op.payload.len()).sum::<usize>();
            compactions.push(pieces);
        }
        let edit = author.edit_bytes("edit", noise(EDIT, 1000 + index));
        let sealed = write(&f.host, &editor, &keys, &edit, now);
        written += envelope_of(&sealed).payload.len();
        admit(&mut f.host, &editor, sealed, now);
        assert!(
            f.host.core.documents[&route()].sealed.within_bounds(),
            "the keyless history stays within its bound"
        );
    }
    assert!(
        written > MAX_SEALED_BYTES,
        "the record was written past its bound: {written}"
    );
    assert!(
        compactions.len() > 1,
        "the record compacted again and again"
    );

    // A replica that persisted the set restores it.
    let restored = f.restore(&f.host.durable().unwrap()).unwrap();
    assert_eq!(held(&restored), held(&f.host));

    joins_late(&f, compactions.last().unwrap(), &mut author.0);
}

/// `f`'s key holder compacts its record, and `f`'s keyless host takes the set, the segments
/// first: the payload bytes of the set.
fn compact_set(f: &mut Fixture, keys: &ChannelContentKeys, now: SystemTime) -> usize {
    let owner = f.owner.clone();
    let mut holder = f.host.clone();
    let pieces = mint_pieces(&mut holder, keys, &owner, now).unwrap();
    for piece in pieces.iter().skip(1) {
        f.host
            .receive_from_report(origin(&owner), piece.clone(), now)
            .unwrap();
    }
    admit(&mut f.host, &owner, pieces[0].clone(), now);
    pieces
        .iter()
        .map(|piece| envelope_of(piece).payload.len())
        .sum()
}

/// A record whose merged state is twice the bound of the writes a sealed history keeps stays
/// writable: its compaction set counts toward its own bound, so a compaction comes due from the
/// writes since the last one alone, never from the record's size, and the compaction work each
/// write brings stays bounded. A keyless replica still refuses writes past the bound since the
/// last compaction.
#[test]
fn a_record_twice_the_bound_of_its_writes_compacts_only_for_its_new_writes() {
    const PARTS: usize = 30;
    const PART: usize = 1024 * 1024;
    const WRITES: usize = 100;
    const EDIT: usize = 128 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, editor) = (f.now, f.editor.clone());
    let mut author = Author::new(&f.host);
    for index in 0..PARTS {
        if !f.host.publication().sealed_records_to_compact().is_empty() {
            compact_set(&mut f, &keys, now);
        }
        let edit = author.edit_bytes(&format!("part-{index}"), noise(PART, index as u64));
        let sealed = write(&f.host, &editor, &keys, &edit, now);
        admit(&mut f.host, &editor, sealed, now);
    }
    let set = compact_set(&mut f, &keys, now);
    assert!(
        set > 2 * MAX_SEALED_BYTES - 4 * PART,
        "the record's merged state is {set} bytes"
    );

    let (mut compactions, mut compacted) = (0usize, 0usize);
    for index in 0..WRITES {
        if !f.host.publication().sealed_records_to_compact().is_empty() {
            compactions += 1;
            compacted += compact_set(&mut f, &keys, now);
        }
        let edit = author.edit_bytes("edit", noise(EDIT, 5000 + index as u64));
        let sealed = write(&f.host, &editor, &keys, &edit, now);
        admit(&mut f.host, &editor, sealed, now);
        assert!(f.host.core.documents[&route()].sealed.within_bounds());
    }
    eprintln!(
        "{WRITES} writes of {EDIT} bytes to a record of {set} bytes: {compactions} compactions, \
         {compacted} bytes of compaction sets"
    );
    // The writes since the last compaction alone bring it due: one per three quarters of their
    // bound, whatever the record's size.
    assert!(
        compactions <= WRITES * EDIT / (MAX_SEALED_BYTES / 4 * 3) + 1,
        "{compactions} compactions in {WRITES} writes"
    );

    // Writes past the bound since the last compaction are refused, the set held as it was.
    let head = held(&f.host)[0].clone();
    let mut index = 0u64;
    let error = loop {
        let edit = author.edit_bytes("more", noise(PART, 9000 + index));
        let sealed = write(&f.host, &editor, &keys, &edit, now);
        if let Err(error) = f.host.receive_from_report(origin(&editor), sealed, now) {
            break error.to_string();
        }
        index += 1;
        assert!(index < 64, "writes past the bound are refused");
    };
    assert!(error.contains("sealed record history is full"), "{error}");
    let sealed = &f.host.core.documents[&route()].sealed;
    let (_, since) = sealed.writes();
    assert!(
        since <= MAX_SEALED_BYTES && since + 2 * PART > MAX_SEALED_BYTES,
        "refused after {since} bytes of writes since the compaction"
    );
    assert_eq!(held(&f.host)[0], head, "the compaction set stays");
}

/// A keyless replica refuses a compaction set larger than one document transfer: its pieces
/// alone change nothing it holds, and a stored history that leads with one does not restore.
#[test]
fn a_keyless_replica_refuses_a_compaction_set_past_its_bound() {
    const SEGMENT: usize = 1024 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, owner, editor) = (f.now, f.owner.clone(), f.editor.clone());
    let mut author = Author::new(&f.host);
    let sealed = write(&f.host, &editor, &keys, &author.edit("title", "x"), now);
    admit(&mut f.host, &editor, sealed, now);
    let before = held(&f.host);

    // A set of `count` segments of the key holder, over `before`: a keyless replica cannot
    // open them.
    let seq = next_seq(&f.host, &owner);
    let forge = |count: usize| -> Vec<SignedOp> {
        let segments: Vec<(EncryptionMetadata, Vec<u8>)> = (0..count)
            .map(|index| {
                let (metadata, ciphertext) = keys
                    .seal_content(
                        artifact(),
                        ContentRetention::Delta,
                        &noise(SEGMENT, index as u64),
                    )
                    .unwrap();
                (
                    metadata,
                    collab_types::SealedCompactionSegment::new(ciphertext)
                        .encode()
                        .unwrap(),
                )
            })
            .collect();
        let payloads: Vec<Vec<u8>> = segments
            .iter()
            .map(|(_, payload)| payload.clone())
            .collect();
        let (wrapper, metadata) = wrapper(&f.host, &keys, &before);
        let head = signed(
            &owner,
            seq,
            OpKind::Compaction,
            wrapper.segmented(&payloads).encode().unwrap(),
            Some(metadata),
            now,
        );
        std::iter::once(head)
            .chain(segments.into_iter().map(|(metadata, payload)| {
                signed(
                    &owner,
                    seq,
                    OpKind::Compaction,
                    payload,
                    Some(metadata),
                    now,
                )
            }))
            .collect()
    };
    let deliver = |host: &mut StatePeer, set: &[SignedOp]| {
        for piece in set.iter().skip(1).chain(set.first()) {
            let _ = host.receive_from_report(origin(&owner), message(piece.clone()), now);
        }
    };

    // One whose segments carry more than one transfer may.
    let set = forge(collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES / SEGMENT + 2);
    assert!(
        set.iter().map(|op| op.payload.len()).sum::<usize>()
            > collab_types::document_transfer::MAX_DOCUMENT_TRANSFER_BYTES + SEGMENT
    );
    let mut taking = f.host.clone();
    deliver(&mut taking, &set);
    assert_eq!(held(&taking), before, "the set is not taken");
    let sealed = write(&taking, &editor, &keys, &author.edit("title", "y"), now);
    admit(&mut taking, &editor, sealed, now);
    // The same set within its bound is taken.
    let within = forge(3);
    let mut taken = f.host.clone();
    deliver(&mut taken, &within);
    assert_eq!(held(&taken), within, "a set within its bound is taken");

    let error = f.host.clone().restore_sealed(&route(), set).unwrap_err();
    assert!(error.contains("outside its bounds"), "{error}");
}

/// A device joining late takes `last`, the last compaction set `f`'s keyless store took, as the
/// store holds it, the compaction first, then the writes after it; its key holder reads `whole`
/// with every change of its history.
fn joins_late(f: &Fixture, last: &[NeighbourMsg], whole: &mut AutoCommit) {
    let (now, keys) = (f.now, f.keys());
    let mut fresh = StatePeer::new(
        WORKSPACE,
        f.authority.clone(),
        vec![],
        RevocationList::new(),
        now,
    )
    .unwrap();
    fresh.select_admission_v5().unwrap();
    fresh
        .receive_from(origin(&f.owner), f.host.workspace_message(), now)
        .unwrap();
    for piece in last {
        fresh
            .receive_from_report(origin(&f.owner), piece.clone(), now)
            .unwrap();
    }
    assert_eq!(
        held(&fresh).len(),
        last.len(),
        "the late joiner took the whole set"
    );
    for op in held(&f.host).into_iter().skip(last.len()) {
        let key = if op.author.as_bytes() == &f.owner.verifying_key().to_bytes() {
            &f.owner
        } else {
            &f.editor
        };
        admit(&mut fresh, key, message(op), now);
    }
    assert_eq!(held(&fresh), held(&f.host));
    let opened = fresh
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    let mut read = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(
        read.get_heads(),
        whole.get_heads(),
        "every change of the history"
    );
    assert_eq!(
        read.get(ROOT, "part-3").unwrap().map(|(value, _)| value),
        whole.get(ROOT, "part-3").unwrap().map(|(value, _)| value),
    );
}

/// `len` incompressible bytes from `seed`: a sealed history is as large as what it carries.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
}

#[path = "kept_open_tests.rs"]
mod kept_open;

/// A key holder's whole-record checkpoint written as a change re-states the record beside the
/// writes it holds: a replica without the key cannot tell what it covers, so it keeps both, and
/// a record a host checkpoints as often as it asks fills its bound with copies of itself long
/// before what its members wrote does. What comes past the bound is refused, and only a key
/// holder's compaction, which names the writes it covers, lets the record go on.
#[test]
fn checkpoints_written_as_changes_fill_a_sealed_history_with_copies_of_the_record() {
    /// Incompressible: a sealed history is as large as what it carries.
    const WRITE: usize = 128 * 1024;
    let mut f = fixture();
    let keys = f.keys();
    let (now, owner, editor) = (f.now, f.owner.clone(), f.editor.clone());
    let mut author = Author::new(&f.host);
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    let mut written = 0;
    let refused = loop {
        let value: Vec<u8> = (0..WRITE)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect();
        let edit = author.edit_bytes(&format!("value-{written}"), value);
        let sealed = write(&f.host, &editor, &keys, &edit, now);
        if let Err(error) = f.host.receive_from_report(origin(&editor), sealed, now) {
            break error.to_string();
        }
        written += 1;
        // The host asks for a checkpoint, and a key holder writes the whole record as a change.
        let snapshot = DocumentMutation::Snapshot(author.0.save())
            .encode()
            .unwrap();
        let (metadata, ciphertext) = keys
            .seal_content(artifact(), ContentRetention::Checkpoint, &snapshot)
            .unwrap();
        let checkpoint = message(signed(
            &owner,
            next_seq(&f.host, &owner),
            OpKind::Change,
            ciphertext,
            Some(metadata),
            now,
        ));
        if let Err(error) = f.host.receive_from_report(origin(&owner), checkpoint, now) {
            break error.to_string();
        }
    };
    let error = refused;
    assert!(error.contains("sealed record history is full"), "{error}");
    assert!(
        written * WRITE < MAX_SEALED_BYTES / 4,
        "copies of the record filled the bound after {written} writes of {WRITE} bytes"
    );
    // The key holder's compaction names what it covers, so the replica drops it all for one
    // copy, and the record goes on.
    let mut holder = f.host.clone();
    let pieces = mint_pieces(&mut holder, &keys, &owner, now).unwrap();
    for piece in pieces.iter().skip(1) {
        f.host
            .receive_from_report(origin(&owner), piece.clone(), now)
            .unwrap();
    }
    admit(&mut f.host, &owner, pieces[0].clone(), now);
    assert_eq!(held(&f.host).len(), pieces.len());
    let sealed = write(&f.host, &editor, &keys, &author.edit("value", "on"), now);
    admit(&mut f.host, &editor, sealed, now);
}

/// What `peer`'s record reads as for its authority, opened with `keys`.
fn captured(
    f: &Fixture,
    peer: &StatePeer,
    keys: &ChannelContentKeys,
) -> Result<AutoCommit, crate::PeerDocumentReadError> {
    peer.capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1024 * 1024,
        Some(&mut |op| open(keys, op)),
    )
    .map(|capture| AutoCommit::load(&capture.into_bytes()).unwrap())
}

fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

/// A page that held a sealed record's keys before the record itself wrote a change in that moment,
/// which it let go unsigned and unsent, and its next change, built on the first, was taken: the
/// record holds a change whose base its own writer never sent, and no device ever will. A reader
/// reads the record without it rather than refusing the whole record as incomplete, and a member's
/// own compaction still refuses to fold it away.
#[test]
fn a_change_built_on_one_its_writer_never_sent_is_read_without_it() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut page = Author::new(&f.host);
    let _let_go = page.edit("early-1", "first");
    let built_on = page.edit("early-2", "second");
    let stuck = write(&f.host, &f.other, &keys, &built_on, now);
    admit(&mut f.host, &f.other, stuck, now);
    let mut member = Author::new(&f.host);
    let later = write(&f.host, &f.owner, &keys, &member.edit("later", "kept"), now);
    admit(&mut f.host, &f.owner, later, now);

    let read = captured(&f, &f.host, &keys).expect("the record reads");
    assert_eq!(text(&read, "value").as_deref(), Some("base"));
    assert_eq!(text(&read, "later").as_deref(), Some("kept"));
    assert_eq!(text(&read, "early-2"), None);
    let other = f.other.verifying_key().to_bytes();
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [UnmergedWrite { author, seq: 1, reason: UnmergedReason::BehindUnsent { missing } }]
                if *author == other && missing.len() == 1
        ),
        "{:?}",
        opened.unmerged
    );
    assert!(
        mint(&mut f.host, &keys, &f.owner, now).is_err(),
        "only the workspace's store folds a write away"
    );
}

/// A change whose base is in a write its writer signed and this replica has not taken in yet may
/// still arrive: the record waits for it.
#[test]
fn a_change_whose_writer_signed_its_base_keeps_the_record_waiting() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut page = Author::new(&f.host);
    let (first, second) = (
        page.edit("early-1", "first"),
        page.edit("early-2", "second"),
    );
    let on_its_way = write(&f.host, &f.other, &keys, &first, now);
    let (metadata, payload) = keys
        .seal_content(artifact(), ContentRetention::Delta, &second)
        .unwrap();
    let after = message(signed(
        &f.other,
        2,
        OpKind::Change,
        payload,
        Some(metadata),
        now,
    ));
    admit(&mut f.host, &f.other, after, now);

    assert!(matches!(
        captured(&f, &f.host, &keys),
        Err(crate::PeerDocumentReadError::Incomplete)
    ));
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [UnmergedWrite {
                seq: 2,
                reason: UnmergedReason::MissingDependencies { .. },
                ..
            }]
        ),
        "{:?}",
        opened.unmerged
    );
    // Signed and on its way: the host replays it in its writer's order, and the record reads whole
    // then (as it did before any of this).
    drop(on_its_way);
}

/// A reader that keeps a record opened between reads keeps the writes it read without, and takes
/// one in once what it builds on turns up after all.
#[test]
fn a_kept_reader_takes_a_write_it_read_without_in_once_its_base_arrives() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut page = Author::new(&f.host);
    let (first, second) = (
        page.edit("early-1", "first"),
        page.edit("early-2", "second"),
    );
    let stuck = write(&f.host, &f.other, &keys, &second, now);
    admit(&mut f.host, &f.other, stuck, now);
    let mut kept = KeptRecord::default();
    let reader = f.authority.device_public_key;
    let mut opener = |op: &SignedOp| open(&keys, op);
    let read = f
        .host
        .open_document(
            reader,
            (APP, RECORD),
            now,
            1 << 20,
            Some(&mut opener),
            &mut kept,
        )
        .expect("the record reads without the stuck write")
        .clone();
    assert_eq!(
        text(&AutoCommit::load(&read.save()).unwrap(), "early-2"),
        None
    );

    let brought = write(&f.host, &f.owner, &keys, &first, now);
    admit(&mut f.host, &f.owner, brought, now);
    let read = f
        .host
        .open_document(
            reader,
            (APP, RECORD),
            now,
            1 << 20,
            Some(&mut opener),
            &mut kept,
        )
        .expect("the record reads whole")
        .clone();
    let read = AutoCommit::load(&read.save()).unwrap();
    assert_eq!(text(&read, "early-1").as_deref(), Some("first"));
    assert_eq!(text(&read, "early-2").as_deref(), Some("second"));
}

/// A device writes a record under one Automerge writer of its own across reloads, as the shared
/// desktop does. Its page lost a change and sent the next one, built on it; reloaded, the page read
/// the record without either and wrote again, numbering its next change as the lost one was
/// numbered. The stuck write still builds on a change no device will ever send: a reader reads the
/// record with what the reloaded page wrote, and without the stuck write, after one write and after
/// two.
#[test]
fn a_writer_reloaded_after_losing_a_change_reads_without_what_it_built_on_it() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let actor = automerge::ActorId::from(b"one device, every load".as_slice());
    let base = f.host.document(APP, RECORD).unwrap();
    let mut page = Author(AutoCommit::load(&base).unwrap().with_actor(actor.clone()));
    let _let_go = page.edit("early-1", "first");
    let stuck = write(
        &f.host,
        &f.other,
        &keys,
        &page.edit("early-2", "second"),
        now,
    );
    admit(&mut f.host, &f.other, stuck, now);

    let mut reloaded = Author(AutoCommit::load(&base).unwrap().with_actor(actor));
    let again = write(&f.host, &f.other, &keys, &reloaded.edit("again", "1"), now);
    admit(&mut f.host, &f.other, again, now);
    let read = captured(&f, &f.host, &keys).expect("the record reads after one write");
    assert_eq!(text(&read, "again").as_deref(), Some("1"));
    assert_eq!(text(&read, "early-2"), None);
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [UnmergedWrite {
                seq: 1,
                reason: UnmergedReason::BehindUnsent { .. },
                ..
            }]
        ),
        "{:?}",
        opened.unmerged
    );

    let more = write(&f.host, &f.other, &keys, &reloaded.edit("again", "2"), now);
    admit(&mut f.host, &f.other, more, now);
    let read = captured(&f, &f.host, &keys).expect("the record reads after two writes");
    assert_eq!(text(&read, "again").as_deref(), Some("2"));
    assert_eq!(text(&read, "early-2"), None);
    let opened = f
        .host
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            opened.unmerged.as_slice(),
            [UnmergedWrite {
                seq: 1,
                reason: UnmergedReason::BehindUnsent { .. },
                ..
            }]
        ),
        "{:?}",
        opened.unmerged
    );
}
