//! A key holder compacts a sealed record's history; a replica without the
//! key accepts only a verified compaction of exactly the writes it holds.
use super::*;
use crate::{InputDisposition, TransportOrigin, peer_handle};
use channel_crypto::{ChannelContentKeys, ChannelGroup, MemberKey};
use collab_types::{ContentRetention, EncryptionMetadata, SealedCompaction};
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const ROOM: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";
const SECRET: &str = "a sentence only key holders may read";

struct Fixture {
    /// A durable replica holding the seeded record and no channel key.
    host: StatePeer,
    /// The room's governing device, which holds the owner role.
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
    let mut room = room::candidate(None, &seed_room(ROOM, &authority, &roster).unwrap()).unwrap();
    let caps = room.get(ROOT, "capabilities").unwrap().unwrap().1;
    put_member_capability(
        &mut room,
        &caps,
        editor_cert.user_public_key,
        crate::join::MemberRole::Editor,
    )
    .unwrap();
    let room_changes = room
        .get_changes(&[])
        .iter()
        .map(|change| change.raw_bytes().to_vec())
        .collect();
    let mut native = StatePeer::new(
        ROOM,
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
                room_changes,
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
    let mut host =
        StatePeer::new(ROOM, authority.clone(), roster, RevocationList::new(), now).unwrap();
    host.select_admission_v5().unwrap();
    for message in native.bootstrap_messages(&owner, now).unwrap() {
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
            ROOM,
            self.authority.clone(),
            vec![],
            RevocationList::new(),
            self.now,
        )
    }
}

fn channel() -> collab_types::ChannelId {
    collab_types::document_channel_id(APP, ROOM, RECORD)
}

fn artifact() -> collab_types::ArtifactId {
    collab_types::document_artifact_id(APP, ROOM, RECORD)
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
    peer.core.documents[&route()].sealed.clone()
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
        cluster_id: ROOM.into(),
        record_id: RECORD.into(),
        envelope: Box::new(op),
    }
}

fn signed(
    key: &SigningKey,
    seq: u64,
    op_class: OpClass,
    payload: Vec<u8>,
    encryption: Option<EncryptionMetadata>,
    now: SystemTime,
) -> SignedOp {
    let mut op = SignedOp::new_unsigned(
        payload,
        artifact(),
        Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
        collab_types::document_attestation_id(APP, ROOM, RECORD),
        op_class,
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
        OpClass::Splice,
        payload,
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

fn mint(
    peer: &mut StatePeer,
    keys: &ChannelContentKeys,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    peer.compact_sealed_history(
        APP,
        RECORD,
        &mut |op| open(keys, op),
        &mut |plaintext| {
            keys.seal_content(artifact(), ContentRetention::Checkpoint, plaintext)
                .map_err(|error| error.to_string())
        },
        key,
        now,
    )
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
        OpClass::Compaction,
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
    assert_eq!(compaction.op_class, OpClass::Compaction);
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
                .compact_sealed_history(APP, RECORD, &mut *open, &mut *seal, &f.owner, now)
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

    let editor = f.editor.clone();
    let forgery = forged(&f.host, &editor, &valid, now);
    refused_with(&mut f.host, &editor, forgery, now, "forbidden");

    let mut other_record = envelope_of(&forged(&f.host, &f.owner, &valid, now));
    other_record.artifact = collab_types::document_artifact_id(APP, ROOM, "tab:2");
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
        OpClass::Compaction,
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
    f.host.core.documents.get_mut(&route()).unwrap().sealed = vec![one; MAX_SEALED_OPS];
    let more = write(&f.host, &f.other, &keys, &author.edit("value", "2"), now);
    let other = f.other.clone();
    refused_with(
        &mut f.host,
        &other,
        more.clone(),
        now,
        "compact_sealed_history",
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
    assert!(f.host.sealed_records_to_compact().is_empty());
    let one = held(&f.host).remove(0);
    f.host.core.documents.get_mut(&route()).unwrap().sealed = vec![one; MAX_SEALED_OPS / 4 * 3];
    assert_eq!(
        f.host.sealed_records_to_compact(),
        vec![(APP.to_owned(), RECORD.to_owned())]
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
        .durable_mutation_messages(&before, &f.owner, now, 2)
        .unwrap();
    assert_eq!(sealed(&mutation), vec![compaction.clone()]);
    assert_eq!(
        mutation.last().map(envelope_of),
        Some(compaction.clone()),
        "behind the room change that carries the record's receipt"
    );
    let bootstrap = host
        .clone()
        .durable_bootstrap_messages(&f.owner, now, 2)
        .unwrap();
    assert_eq!(sealed(&bootstrap), vec![compaction]);
    assert!(
        sealed(
            &host
                .clone()
                .durable_mutation_messages(&host, &f.owner, now, 2)
                .unwrap()
        )
        .is_empty(),
        "a compaction already published is not published again"
    );
    assert!(
        sealed(&before.durable_bootstrap_messages(&f.owner, now, 2).unwrap()).is_empty(),
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
        crate::durability::entries(peer.admitted_room().unwrap())
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
        .durable_bootstrap_messages(&f.owner, now, 3)
        .unwrap();
    assert!(!documents(&messages));
    assert_eq!(watermark(&bootstrapped), Some(envelope_of(&sealed).seq));
}

fn bounded_history(
    f: &Fixture,
    bytes: &[u8],
) -> Result<crate::BoundedStatePeer, crate::BoundedPeerError> {
    crate::BoundedStatePeer::restore(
        bytes,
        ROOM,
        &f.authority,
        &[],
        &RevocationList::new(),
        f.now,
        &crate::PeerLimits::default(),
    )
}

#[test]
fn bounded_history_replays_causal_order_and_continues_the_original_writer() {
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
    let bounded = bounded_history(&f, &before).unwrap();
    let capture = bounded
        .read()
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            &mut |op| open(&keys, op),
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
fn bounded_history_keeps_the_conflicting_original_writer_refusal() {
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
    let bounded = bounded_history(&f, &before).unwrap();
    let error = bounded
        .read()
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            &mut |op| open(&keys, op),
        )
        .err()
        .expect("a conflicting same-writer history must not be replaced or normalized");
    assert!(error.to_string().contains("duplicate seq 1"), "{error}");
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn bounded_history_validates_route_signature_admission_and_watermarks() {
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
        assert!(
            bounded_history(&f, &corrupted).is_err(),
            "bounded restore corruption {corruption}"
        );
    }
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn bounded_history_missing_admitted_write_is_refused_before_opening() {
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
    let bounded = bounded_history(&f, &incomplete).unwrap();
    let mut opened = 0;
    let result = bounded.read().capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1024 * 1024,
        &mut |op| {
            opened += 1;
            open(&keys, op)
        },
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Incomplete)
    ));
    assert_eq!(opened, 0, "incomplete coverage is not decrypted");
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn bounded_history_denial_and_capacity_precede_opening_and_missing_keys_refuse() {
    let mut f = fixture();
    let keys = f.keys();
    let mut author = Author::new(&f.host);
    let plaintext = author.edit("value", SECRET);
    let message = write(&f.host, &f.owner, &keys, &plaintext, f.now);
    admit(&mut f.host, &f.owner, message, f.now);
    let before = f.host.durable().unwrap();
    let bounded = bounded_history(&f, &before).unwrap();
    let mut opened = 0;
    let result =
        bounded
            .read()
            .capture_document([0x77; 32], (APP, RECORD), f.now, 1024 * 1024, &mut |op| {
                opened += 1;
                open(&keys, op)
            });
    assert!(matches!(result, Err(crate::PeerDocumentReadError::Peer(_))));
    assert_eq!(opened, 0, "an unauthenticated reader is not decrypted");
    let result = bounded.read().capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1,
        &mut |op| {
            opened += 1;
            open(&keys, op)
        },
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Peer(
            crate::BoundedPeerError::Capacity
        ))
    ));
    assert_eq!(
        opened, 0,
        "the base exceeds the caller's bound before opening"
    );
    let wrong_keys = fixture().keys();
    let result = bounded.read().capture_document(
        f.authority.device_public_key,
        (APP, RECORD),
        f.now,
        1024 * 1024,
        &mut |op| open(&wrong_keys, op),
    );
    assert!(matches!(
        result,
        Err(crate::PeerDocumentReadError::Opening(_))
    ));
    assert_eq!(f.host.durable().unwrap(), before);
}

#[test]
fn bounded_history_replays_compaction_and_requires_all_held_epochs() {
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
    let bounded = bounded_history(&f, &before).unwrap();
    let read = bounded.read();
    let capture = read
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            &mut |op| {
                let keys = if op.encryption.unwrap().epoch == old_keys.epoch() {
                    &old_keys
                } else {
                    &new_keys
                };
                open(keys, op)
            },
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
        read.capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            &mut |op| open(&old_keys, op),
        ),
        Err(crate::PeerDocumentReadError::Opening(_))
    ));
    let mut wire: DurableV6<'_> = postcard::from_bytes(&before).unwrap();
    wire.sealed[0].1.swap(0, 1);
    let misplaced = postcard::to_allocvec(&wire).unwrap();
    assert!(
        bounded_history(&f, &misplaced).is_err(),
        "a compaction is first, never a later write"
    );
    assert!(f.restore(&misplaced).is_err());
    assert_eq!(f.host.durable().unwrap(), before);
}
