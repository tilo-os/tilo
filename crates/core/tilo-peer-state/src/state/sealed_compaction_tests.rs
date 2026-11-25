//! A key holder compacts a sealed record's history; a replica without the
//! key accepts only a verified compaction of exactly the writes it holds.
use super::*;
use crate::{InputDisposition, TransportOrigin, peer_handle};
use collab_types::CommitNamed as _;
use collab_types::{ContentRetention, EncryptionMetadata, SealedCompaction};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_automerge::ApplyIncoming;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
pub(super) const APP: &str = "test.app";
pub(super) const RECORD: &str = "tab:1";
const SECRET: &str = "a sentence only key holders may read";

pub(super) struct Fixture {
    /// A durable replica holding the seeded record and no channel key.
    pub(super) replica: StatePeer,
    /// The workspace's governing device, which holds the owner role.
    pub(super) owner: SigningKey,
    /// The owner's second device.
    pub(super) other: SigningKey,
    /// A member with the editor role.
    pub(super) editor: SigningKey,
    authority: DeviceCert,
    /// The record channel's group, whose content keys key holders use.
    group: KeyGroup,
    pub(super) now: SystemTime,
    /// What the seeding device sent the replica: the workspace and the record as it was seeded,
    /// which every device of the workspace starts from.
    seed: Vec<NeighbourMsg>,
}

pub(super) fn device(root: u8, seed: u8) -> (SigningKey, DeviceCert) {
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

/// The person whose certified device `key` is in `peer`'s workspace.
pub(super) fn person(peer: &StatePeer, key: &SigningKey) -> [u8; 32] {
    let device = key.verifying_key().to_bytes();
    peer.certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .expect("the workspace holds the device's certificate")
        .user_public_key
}

pub(super) fn fixture() -> Fixture {
    fixture_holding(&[RECORD])
}

/// [`fixture`] with each of `records` of [`APP`] seeded alike.
pub(super) fn fixture_holding(records: &[&str]) -> Fixture {
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
                documents: records
                    .iter()
                    .map(|record| SeedDocument {
                        app_id: APP.into(),
                        document_id: (*record).into(),
                        kind: "test".into(),
                        bytes: document.save(),
                    })
                    .collect(),
            },
            &owner,
            now,
        )
        .unwrap();
    let mut replica = StatePeer::new(
        WORKSPACE,
        authority.clone(),
        roster,
        RevocationList::new(),
        now,
    )
    .unwrap();
    replica.select_exact_admissions().unwrap();
    let seed = native
        .publication_mut()
        .bootstrap_messages(&owner, now)
        .unwrap();
    for message in &seed {
        replica
            .receive_from(origin(&owner), message.clone(), now)
            .unwrap();
    }
    let group = KeyGroup::create(group(), MemberKey::generate("owner").unwrap()).unwrap();
    Fixture {
        replica,
        owner,
        other,
        editor,
        authority,
        group,
        now,
        seed,
    }
}

impl Fixture {
    pub(super) fn keys(&self) -> ContentKeys {
        self.group.content_keys().unwrap()
    }

    fn rotate(&mut self) -> ContentKeys {
        let member = MemberKey::generate("member").unwrap();
        self.group
            .add_member(member.fresh_key_package().unwrap())
            .unwrap();
        self.keys()
    }

    pub(super) fn restore(&self, bytes: &[u8]) -> Result<StatePeer, String> {
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

fn group() -> collab_types::WorkspaceId {
    collab_types::WorkspaceId::parse(WORKSPACE).unwrap()
}

fn artifact() -> collab_types::ArtifactId {
    collab_types::document_artifact_id(APP, WORKSPACE, RECORD)
}

pub(super) fn origin(key: &SigningKey) -> TransportOrigin {
    TransportOrigin::ServerDelta(key.verifying_key().to_bytes())
}

fn route() -> Route {
    Route {
        app: APP.into(),
        record: RECORD.into(),
    }
}

pub(super) fn held(peer: &StatePeer) -> Vec<SignedOp> {
    peer.core.documents[&route()].sealed.to_vec()
}

pub(super) fn envelope_of(message: &NeighbourMsg) -> SignedOp {
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

pub(super) fn message(op: SignedOp) -> NeighbourMsg {
    NeighbourMsg::AppDocument {
        peer: peer_handle(op.author.as_bytes()),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: RECORD.into(),
        envelope: Box::new(op),
        readers: None,
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
/// the plaintext a key holder's sealed write opens to. Each is a window's
/// direct edit of the record, which names no finer action: it is the editing
/// device's person's, and names them.
pub(super) struct Author(pub(super) AutoCommit, pub(super) [u8; 32]);

impl Author {
    pub(super) fn new(peer: &StatePeer) -> Self {
        Self::of(
            peer,
            AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap(),
        )
    }

    /// `doc` written as `peer`'s device's person.
    pub(super) fn of(peer: &StatePeer, doc: AutoCommit) -> Self {
        Self(
            doc,
            crate::owner::testing::person_of(peer, peer.core.origin),
        )
    }

    /// Commit what is pending as a direct edit of the record, naming the person whose device it
    /// is.
    pub(super) fn commit(&mut self) {
        let message = tilo_actions::ActionContext::whole_edit_by(&tilo_actions::Acting::person(
            hex::encode(self.1),
        ));
        self.0
            .commit_with(automerge::transaction::CommitOptions::default().with_message(message));
    }

    pub(super) fn edit(&mut self, key: &str, value: &str) -> Vec<u8> {
        let heads = self.0.get_heads();
        self.0.put(ROOT, key, value).unwrap();
        self.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
            .encode()
            .unwrap()
    }

    /// [`Self::edit`] of a value held as bytes, as an app records what a file or a picture is.
    fn edit_bytes(&mut self, key: &str, value: Vec<u8>) -> Vec<u8> {
        let heads = self.0.get_heads();
        self.0.put(ROOT, key, value).unwrap();
        self.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
            .encode()
            .unwrap()
    }
}

/// `key`'s next sealed write to `peer`'s record.
pub(super) fn write(
    peer: &StatePeer,
    key: &SigningKey,
    keys: &ContentKeys,
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
/// holder saved it, the first thing the workspace keeps at the head of a sealed
/// channel and replays to a device joining the workspace, naming each
/// author's last write it holds.
fn checkpoint(
    peer: &StatePeer,
    keys: &ContentKeys,
    key: &SigningKey,
    now: SystemTime,
) -> NeighbourMsg {
    let snapshot = DocumentMutation::Snapshot(peer.document(APP, RECORD).unwrap())
        .encode()
        .unwrap();
    let (metadata, ciphertext) = keys
        .seal_content(artifact(), ContentRetention::Checkpoint, &snapshot)
        .unwrap();
    let floors = peer.core.documents[&route()]
        .channel
        .last_seq_per_author
        .iter()
        .map(|(author, seq)| (*author, *seq));
    message(signed(
        key,
        next_seq(peer, key),
        OpKind::Change,
        collab_types::SealedCheckpoint::new(floors, ciphertext)
            .encode()
            .unwrap(),
        Some(metadata),
        now,
    ))
}

pub(super) fn admit(
    peer: &mut StatePeer,
    key: &SigningKey,
    message: NeighbourMsg,
    now: SystemTime,
) {
    let report = peer.receive_from_report(origin(key), message, now).unwrap();
    assert!(
        matches!(report.primary(), InputDisposition::Accepted(_)),
        "admitted with exact evidence"
    );
}

/// `message` waits for the writes before it, which its sender is asked for.
pub(super) fn held_for_history(
    peer: &mut StatePeer,
    key: &SigningKey,
    message: NeighbourMsg,
    now: SystemTime,
) {
    let report = peer.receive_from_report(origin(key), message, now).unwrap();
    assert!(
        matches!(
            report.primary(),
            InputDisposition::RetryLater(MissingPrerequisite::CausalDependencies)
        ),
        "held for the writes before it: {:?}",
        report.primary()
    );
}

/// A key holder's opener: any epoch it holds, compactions included.
pub(super) fn open(keys: &ContentKeys, op: &SignedOp) -> Result<Vec<u8>, String> {
    let metadata = op.encryption.ok_or("not sealed")?;
    let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
    keys.open_content(op.artifact, &metadata, &ciphertext)
        .map(|plaintext| plaintext.to_vec())
        .map_err(|error| error.to_string())
}

/// A compaction of a record small enough for one op.
pub(super) fn mint(
    peer: &mut StatePeer,
    keys: &ContentKeys,
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
    keys: &ContentKeys,
    key: &SigningKey,
    now: SystemTime,
) -> Result<Vec<NeighbourMsg>, String> {
    let mut ops = peer.publication().draft_sealed_compaction(
        (APP, RECORD),
        None,
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
        .admit_sealed_compaction(APP, RECORD, None, ops, now)
}

/// A compaction of `superseded`, sealed with `keys` but not signed.
pub(super) fn wrapper(
    peer: &StatePeer,
    keys: &ContentKeys,
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

/// A correctly numbered compaction, so each forgery is refused for what it
/// forges and not for its author's sequence.
pub(super) fn forged(
    _peer: &StatePeer,
    key: &SigningKey,
    (wrapper, metadata): &(SealedCompaction, EncryptionMetadata),
    now: SystemTime,
) -> NeighbourMsg {
    let seq = wrapper
        .covered(&Ed25519PublicKey::from_bytes(
            key.verifying_key().to_bytes(),
        ))
        .unwrap_or(0);
    message(signed(
        key,
        seq,
        OpKind::Compaction,
        wrapper.encode().unwrap(),
        Some(*metadata),
        now,
    ))
}

fn refused(peer: &mut StatePeer, key: &SigningKey, message: NeighbourMsg, now: SystemTime) {
    refused_with(peer, key, message, now, "");
}

pub(super) fn refused_with(
    peer: &mut StatePeer,
    key: &SigningKey,
    message: NeighbourMsg,
    now: SystemTime,
    reason: &str,
) {
    let before = peer.durable().unwrap();
    let error = peer.receive_from(origin(key), message, now).unwrap_err();
    assert!(
        error.to_string().contains(reason),
        "{error:?} is not {reason:?}"
    );
    assert_eq!(peer.durable().unwrap(), before, "the record is unchanged");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// A key holder's opener of a write's plaintext, and the sealer a compaction
/// writes its snapshot through.
type Opener<'a> = Box<dyn FnMut(&SignedOp) -> Result<Vec<u8>, String> + 'a>;
type Sealer<'a> = Box<dyn FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + 'a>;

tilo_test_env::scenario! {
    fn a_sealed_records_history_folds_into_one_checkpoint_and_reads_again;

    phase folding_after_a_rotation_replaces_the_history_and_reads_every_change_again;
    {
        let mut f = fixture();
        let now = f.now;
        let mut author = Author::new(&f.replica);
        let before_rotation = f.keys();
        let first = write(
            &f.replica,
            &f.other,
            &before_rotation,
            &author.edit("first", SECRET),
            now,
        );
        admit(&mut f.replica, &f.other, first, now);
        let current = f.rotate();
        assert!(current.epoch() > before_rotation.epoch());
        let second = write(
            &f.replica,
            &f.owner,
            &current,
            &author.edit("second", "2"),
            now,
        );
        admit(&mut f.replica, &f.owner, second, now);
        let mut replica = f.replica.clone();

        let minted = mint(&mut f.replica, &current, &f.owner, now).unwrap();
        let compaction = envelope_of(&minted);
        assert_eq!(
            held(&f.replica),
            vec![compaction.clone()],
            "the history it held is replaced by the compaction"
        );
        assert_eq!(compaction.kind, OpKind::Compaction);
        let metadata = compaction.encryption.unwrap();
        assert_eq!(metadata.epoch, current.epoch());
        assert_eq!(metadata.retention(), Some(ContentRetention::Checkpoint));
        // It opens, under the keys now current, to the whole record as its writer
        // saved it: every change folded is in it.
        let DocumentMutation::Snapshot(bytes) =
            DocumentMutation::decode(&open(&current, &compaction).unwrap()).unwrap()
        else {
            panic!("a compaction opens to a snapshot")
        };
        let merged = AutoCommit::load(&bytes).unwrap();
        for (key, value) in [("value", "base"), ("first", SECRET), ("second", "2")] {
            assert_eq!(
                merged.get(ROOT, key).unwrap().unwrap().0.as_str(),
                Some(value),
                "the compaction holds {key}"
            );
        }
        // A replica without the keys takes the checkpoint in and merges nothing;
        // the device that folded keeps it across a restart, with no plaintext.
        admit(&mut replica, &f.owner, minted, now);
        assert_eq!(held(&replica), held(&f.replica));
        let durable = f.replica.durable().unwrap();
        assert!(
            !contains(&durable, SECRET.as_bytes()),
            "no plaintext is kept"
        );
        let restored = f.restore(&durable).unwrap();
        assert_eq!(held(&restored), held(&f.replica), "survives restart");

        // The record goes on after the fold, and a key holder reads it whole.
        let later = write(
            &f.replica,
            &f.other,
            &current,
            &author.edit("value", "3"),
            now,
        );
        admit(&mut f.replica, &f.other, later.clone(), now);
        admit(&mut replica, &f.other, later, now);
        assert_eq!(held(&replica), held(&f.replica));
        let restored = f.restore(&replica.durable().unwrap()).unwrap();
        let mut read_write = |op: &SignedOp| open(&current, op);
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
        assert_eq!(
            read.get(ROOT, "first").unwrap().unwrap().0.as_str(),
            Some(SECRET),
            "what the compaction folded is read"
        );
    }

    phase the_records_own_writer_folds_what_it_holds;
    // A device the record admits as a writer holds the channel keys, so
    // compacting is part of writing it: the editor folds the history it
    // holds, and a replica without the keys takes the checkpoint in.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let editor = f.editor.clone();
        let mut author = Author::new(&f.replica);
        for value in ["1", "2"] {
            let write = write(&f.replica, &f.other, &keys, &author.edit("value", value), now);
            admit(&mut f.replica, &f.other, write, now);
        }
        let mut replica = f.replica.clone();
        let compaction = mint(&mut f.replica, &keys, &editor, now).unwrap();
        assert_eq!(
            held(&f.replica),
            vec![envelope_of(&compaction)],
            "the writer folded the history it holds"
        );
        admit(&mut replica, &editor, compaction, now);
        assert_eq!(held(&replica), held(&f.replica));
    }

    phase a_full_record_refuses_the_write_past_its_bound_and_goes_on_after_its_writers_fold;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let editor = f.editor.clone();
        let mut author = Author::new(&f.replica);
        let mut written = 0;
        while held(&f.replica).len() < MAX_SEALED_OPS {
            let sealed = write(
                &f.replica,
                &editor,
                &keys,
                &author.edit("value", &written.to_string()),
                now,
            );
            match f.replica.receive_from_report(origin(&editor), sealed, now) {
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
        assert_eq!(written, MAX_SEALED_OPS, "the bound refuses the write past it");
        assert!(
            !f.replica.publication().sealed_to_compact().is_empty(),
            "a full record is due for its writer's checkpoint"
        );
        let checkpoint = mint(&mut f.replica, &keys, &editor, now).unwrap();
        assert_eq!(held(&f.replica), vec![envelope_of(&checkpoint)]);
        for value in MAX_SEALED_OPS..MAX_SEALED_OPS + 8 {
            let sealed = write(
                &f.replica,
                &editor,
                &keys,
                &author.edit("value", &value.to_string()),
                now,
            );
            admit(&mut f.replica, &editor, sealed, now);
        }
        assert!(f.replica.publication().sealed_to_compact().is_empty());
        let mut read_write = |op: &SignedOp| open(&keys, op);
        let opened = f
            .replica
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

    phase a_compaction_folds_its_writes_exact_evidence_into_one_mark_per_writer;
    // Every round of writes a compaction folds leaves one mark per writer,
    // not an exact entry per write: the index stays the same size however
    // long the record runs. A folded write sent again is ignored, whatever its
    // bytes; above the mark a conflicting write is still refused.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
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
                    &f.replica,
                    &key,
                    &keys,
                    &author.edit("value", &format!("{round}.{index}")),
                    now,
                );
                first.get_or_insert_with(|| next.clone());
                admit(&mut f.replica, &key, next, now);
            }
            mint(&mut f.replica, &keys, &f.owner, now).unwrap();
            sizes.push(f.replica.core.exact_admissions.len());
        }
        // A compaction is numbered as a write it folds, so its evidence is the
        // mark of that fold, never an entry: what stays is the owner's seeded
        // checkpoint, above the number the owner's first fold takes.
        assert_eq!(
            sizes,
            vec![1, 1, 1],
            "a fold adds marks, and the index does not grow"
        );
        let index = &f.replica.core.exact_admissions;
        for writer in [&f.other, &f.editor] {
            let mark = index
                .marks()
                .find(|(key, _)| key.author == writer.verifying_key().to_bytes())
                .map(|(_, mark)| mark);
            assert_eq!(
                mark,
                Some(floor(&f.replica, writer)),
                "a writer's mark is its last folded write"
            );
        }

        // The first write again, and a different write at its sequence: both
        // are in the snapshot already, so both are ignored.
        let first = first.unwrap();
        let report = f
            .replica
            .receive_from_report(origin(&f.other), first.clone(), now)
            .unwrap();
        assert!(matches!(report.primary(), InputDisposition::Duplicate(_)));
        let mut rewritten = envelope_of(&first);
        rewritten.timestamp += Duration::from_secs(1);
        collab_types::sign_op(&mut rewritten, &f.other).unwrap();
        let report = f
            .replica
            .receive_from_report(origin(&f.other), message(rewritten), now)
            .unwrap();
        assert!(matches!(report.primary(), InputDisposition::Duplicate(_)));
        assert_eq!(held(&f.replica).len(), 1, "nothing folded comes back");

        // Above the mark the evidence is exact again.
        let above = write(&f.replica, &f.other, &keys, &author.edit("value", "late"), now);
        admit(&mut f.replica, &f.other, above.clone(), now);
        let mut conflicting = envelope_of(&above);
        conflicting.timestamp += Duration::from_secs(1);
        collab_types::sign_op(&mut conflicting, &f.other).unwrap();
        assert!(
            f.replica
                .receive_from_report(origin(&f.other), message(conflicting), now)
                .is_err(),
            "a second write at an unfolded sequence conflicts"
        );

        // The marks are part of the durable state.
        let restored = f.restore(&f.replica.durable().unwrap()).unwrap();
        assert_eq!(
            restored.core.exact_admissions.encode().unwrap(),
            f.replica.core.exact_admissions.encode().unwrap()
        );
    }

    phase a_compaction_is_numbered_as_the_last_write_of_its_author_it_folds;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        for index in 0..6 {
            let message = write(
                &f.replica,
                &f.owner,
                &keys,
                &author.edit("value", &format!("own-{index}")),
                now,
            );
            admit(&mut f.replica, &f.owner, message, now);
        }
        let plain = write(
            &f.replica,
            &f.other,
            &keys,
            &author.edit("value", "a member's"),
            now,
        );
        admit(&mut f.replica, &f.other, plain, now);
        let last = floor(&f.replica, &f.owner);
        let compaction = mint(&mut f.replica, &keys, &f.owner, now).unwrap();
        assert_eq!(
            envelope_of(&compaction).seq,
            last,
            "the host's last write it folds"
        );
        // A page that opens the record takes the compaction first, then the
        // host's next write: numbering it past the fold would stop the page.
        let mut fresh = StatePeer::new(
            WORKSPACE,
            f.authority.clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        fresh.select_exact_admissions().unwrap();
        fresh
            .receive_from(origin(&f.owner), f.replica.workspace_message(), now)
            .unwrap();
        admit(&mut fresh, &f.owner, compaction, now);
        let next = write(
            &f.replica,
            &f.owner,
            &keys,
            &author.edit("value", "after the fold"),
            now,
        );
        admit(&mut f.replica, &f.owner, next.clone(), now);
        admit(&mut fresh, &f.owner, next, now);
        assert_eq!(floor(&fresh, &f.owner), last.checked_add(1).unwrap());
    }

    phase minting_aborts_without_a_trace_when_any_write_cannot_be_compacted;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        for value in ["1", "2"] {
            let write = write(&f.replica, &f.other, &keys, &author.edit("value", value), now);
            admit(&mut f.replica, &f.other, write, now);
        }
        let before = f.replica.durable().unwrap();
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
                f.replica
                    .publication()
                    .draft_sealed_compaction(
                        (APP, RECORD),
                        None,
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
                f.replica.durable().unwrap(),
                before,
                "{why} changed the record"
            );
        }
    }

    phase a_compaction_merges_writes_admitted_out_of_causal_order;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let (first, second) = (author.edit("value", "1"), author.edit("value", "2"));
        let late = write(&f.replica, &f.other, &keys, &second, now);
        admit(&mut f.replica, &f.other, late, now);
        let early = write(&f.replica, &f.owner, &keys, &first, now);
        admit(&mut f.replica, &f.owner, early, now);
        let minted = envelope_of(&mint(&mut f.replica, &keys, &f.owner, now).unwrap());
        let DocumentMutation::Snapshot(bytes) =
            DocumentMutation::decode(&open(&keys, &minted).unwrap()).unwrap()
        else {
            panic!("a snapshot")
        };
        let merged = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            merged.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("2")
        );
    }

    phase a_history_missing_a_write_opens_without_it_and_completes_once_it_returns;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let (first, second) = (author.edit("first", "1"), author.edit("second", "2"));
        let unreadable = author.edit("third", "3");
        let late = write(&f.replica, &f.other, &keys, &second, now);
        admit(&mut f.replica, &f.other, late, now);
        let foreign = KeyGroup::create(group(), MemberKey::generate("other").unwrap())
            .unwrap()
            .content_keys()
            .unwrap();
        let sealed_apart = write(&f.replica, &f.other, &foreign, &unreadable, now);
        admit(&mut f.replica, &f.other, sealed_apart, now);
        let mut read_write = |op: &SignedOp| open(&keys, op);

        let opened = f
            .replica
            .publication()
            .opened_document(APP, RECORD, &mut read_write)
            .unwrap()
            .expect("the record is held");
        let read = AutoCommit::load(&opened.document).expect("what merges opens as a whole record");
        assert_eq!(
            read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("base"),
            "the record reads without what it lacks"
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
        assert!(
            mint(&mut f.replica, &keys, &f.owner, now).is_err(),
            "a history that leaves a write out is never compacted"
        );
        let mut restored = f.restore(&f.replica.durable().unwrap()).unwrap();
        assert_eq!(held(&restored), held(&f.replica), "a restart keeps them too");

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
            Some("2"),
            "once what it waits for arrives the write is in the record"
        );
    }

    phase a_history_that_writes_a_sequence_twice_opens_with_the_second_write_named;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut first = Author::new(&f.replica);
        let mut conflicting = Author::new(&f.replica);
        let actor = automerge::ActorId::from(vec![0x42]);
        first.0.set_actor(actor.clone());
        conflicting.0.set_actor(actor);
        let one = write(
            &f.replica,
            &f.owner,
            &keys,
            &first.edit("value", "canonical"),
            now,
        );
        let two = write(
            &f.replica,
            &f.other,
            &keys,
            &conflicting.edit("value", "conflicting"),
            now,
        );
        admit(&mut f.replica, &f.owner, one, now);
        admit(&mut f.replica, &f.other, two, now);
        let mut read_write = |op: &SignedOp| open(&keys, op);

        let opened = f
            .replica
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
            held(&f.replica).len(),
            2,
            "the write the record cannot hold is kept"
        );

        // A compaction replaces the history, so it folds the sequence twice:
        // no reader can ever carry both changes.
        mint(&mut f.replica, &keys, &f.owner, now).expect("a fork does not block the compaction");
        let opened = f
            .replica
            .publication()
            .opened_document(APP, RECORD, &mut read_write)
            .unwrap()
            .unwrap();
        assert!(opened.unmerged.is_empty());
        let read = AutoCommit::load(&opened.document).unwrap();
        assert_eq!(
            read.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("canonical")
        );
    }
}

tilo_test_env::scenario! {
    fn a_chat_that_lost_a_write_at_a_hard_stop_opens_once_it_returns;

    phase a_chat_that_lost_a_write_at_a_hard_stop_opens_and_completes_once_it_returns;
    {
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
            checkpoint_writes: std::sync::Arc::default(),
            quarantine: None,
        };
        let missing = "32cc8086d6eb9538799a44d9b6e5229f22d7626ef79e2d3ec867898a01224cf1";

        let opened = {
            let mut read_write = |op: &SignedOp| Ok(plaintexts[&op.seq].clone());
            assert_eq!(
                sealed_compaction::merge_sealed(&document, &mut read_write, &mut |_, _| Ok(())).unwrap_err(),
                "sealed history does not merge into a complete record",
                "as the workspace service refused to start on it"
            );
            sealed_compaction::open_sealed(&document, &mut read_write, &mut |_, _| Ok(())).unwrap()
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
        let opened = sealed_compaction::open_sealed(&document, &mut read_write, &mut |_, _| Ok(())).unwrap();
        assert_eq!(opened.unmerged, []);
        // The same record as the whole history merged at once: the same changes, so the same
        // heads and the same contents. Its bytes may order them otherwise, as a compaction loaded
        // on its own and merged into does.
        let merged =
            sealed_compaction::merge_sealed(&document, &mut read_write, &mut |_, _| Ok(()))
                .unwrap();
        let (mut opened, mut merged) = (
            AutoCommit::load(&opened.document).unwrap(),
            AutoCommit::load(&merged).unwrap(),
        );
        let heads = |doc: &mut AutoCommit| {
            let mut heads = doc.get_heads();
            heads.sort();
            heads
        };
        assert_eq!(heads(&mut opened), heads(&mut merged));
        assert_eq!(
            serde_json::to_value(automerge::AutoSerde::from(&opened)).unwrap(),
            serde_json::to_value(automerge::AutoSerde::from(&merged)).unwrap(),
        );
    }
}

/// A public workspace's chat as its governor kept it after hard stops, opened
/// with the channel's keys: the record's base, the last member snapshot,
/// the governor's own write, then two visitors' writes that build on a
/// change of a third visitor's snapshot the governor had taken in and lost
/// at a stop. `lost` is that snapshot, as the server still retained it.
/// Its postcard `Vec<(label, bytes)>`, hex.
const LOUNGE_CHAT_GAP: &str = include_str!("fixtures/lounge-chat-gap.hex");

tilo_test_env::scenario! {
    fn a_replica_admits_only_a_whole_current_verified_compaction;

    phase forged_compactions_are_refused;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let write = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, write, now);
        let history = held(&f.replica);
        let valid = wrapper(&f.replica, &keys, &history);

        let mut tampered = envelope_of(&forged(&f.replica, &f.owner, &valid, now));
        let mut altered = valid.0.clone();
        altered.ciphertext[0] ^= 1;
        tampered.payload = altered.encode().unwrap();
        refused(&mut f.replica, &f.owner, message(tampered), now);

        let stranger = SigningKey::from_bytes(&[77; 32]);
        let mut impostor = envelope_of(&forged(&f.replica, &stranger, &valid, now));
        impostor.author = Ed25519PublicKey::from_bytes(f.owner.verifying_key().to_bytes());
        impostor.seq = next_seq(&f.replica, &f.owner);
        refused(&mut f.replica, &f.owner, message(impostor), now);

        let mut other_document = envelope_of(&forged(&f.replica, &f.owner, &valid, now));
        other_document.artifact = collab_types::document_artifact_id(APP, WORKSPACE, "tab:2");
        collab_types::sign_op(&mut other_document, &f.owner).unwrap();
        refused(&mut f.replica, &f.owner, message(other_document), now);

        let mut delta = valid.1;
        delta.scheme = collab_types::EncryptionScheme::ContentKey {
            retention: ContentRetention::Delta,
        };
        let forgery = forged(&f.replica, &f.owner, &(valid.0, delta), now);
        refused(&mut f.replica, &f.owner, forgery, now);

        let cleartext = signed(
            &f.owner,
            next_seq(&f.replica, &f.owner),
            OpKind::Compaction,
            DocumentMutation::Snapshot(f.replica.document(APP, RECORD).unwrap())
                .encode()
                .unwrap(),
            None,
            now,
        );
        refused_with(
            &mut f.replica,
            &f.owner,
            message(cleartext),
            now,
            "only ever sealed",
        );
        assert_eq!(held(&f.replica), history);
    }

    phase a_compaction_under_a_stale_epoch_is_refused;
    {
        let mut f = fixture();
        let now = f.now;
        let stale = f.keys();
        let current = f.rotate();
        let mut author = Author::new(&f.replica);
        let write = write(&f.replica, &f.other, &current, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, write, now);
        let compaction = wrapper(&f.replica, &stale, &held(&f.replica));
        let owner = f.owner.clone();
        let forgery = forged(&f.replica, &owner, &compaction, now);
        refused_with(&mut f.replica, &owner, forgery, now, "stale epoch");
    }

    phase a_document_moving_to_its_own_group_is_compacted_under_it_whatever_epoch_the_workspace_reached;
    {
        // The workspace's keys have turned over (a member joined), and the document's own group
        // is new: its epoch is below the one the document's writes were sealed under.
        let mut f = fixture();
        let now = f.now;
        let workspace_keys = f.rotate();
        let mut author = Author::new(&f.replica);
        let earlier = write(&f.replica, &f.other, &workspace_keys, &author.edit("first", SECRET), now);
        admit(&mut f.replica, &f.other, earlier, now);
        let scope = neighbour_wire::KeyScope::document(group(), APP, RECORD, None).unwrap();
        let mut document_group =
            KeyGroup::create(scope.group(), MemberKey::generate("owner").unwrap()).unwrap();
        let document_keys = document_group
            .content_keys()
            .unwrap()
            .with_workspace_history(workspace_keys.clone());
        assert!(document_keys.epoch() < workspace_keys.epoch());
        let mut replica = f.replica.clone();

        let minted = mint(&mut f.replica, &document_keys, &f.owner, now)
            .expect("the document's own group seals its compaction");
        let compaction = envelope_of(&minted);
        assert_eq!(held(&f.replica), vec![compaction.clone()], "history superseded");
        let metadata = compaction.encryption.unwrap();
        assert!(matches!(
            metadata.scheme,
            collab_types::EncryptionScheme::DocumentContentKey { group, .. } if group == scope.group()
        ));
        let snapshot = open(&document_keys, &compaction).unwrap();
        let DocumentMutation::Snapshot(bytes) = DocumentMutation::decode(&snapshot).unwrap() else {
            panic!("a compaction opens to a snapshot");
        };
        let merged = AutoCommit::load(&bytes).unwrap();
        assert_eq!(merged.get(ROOT, "first").unwrap().unwrap().0.as_str(), Some(SECRET));
        // A replica without the keys takes it from its author as the history it holds.
        admit(&mut replica, &f.owner, minted, now);
        assert_eq!(held(&replica), held(&f.replica), "another replica supersedes");

        // Within the document's own group an earlier epoch is still stale.
        let stale = document_group.content_keys().unwrap();
        document_group
            .add_member(MemberKey::generate("member").unwrap().fresh_key_package().unwrap())
            .unwrap();
        let current = document_group.content_keys().unwrap();
        assert!(current.epoch() > stale.epoch());
        let later = write(&f.replica, &f.other, &current, &author.edit("second", "2"), now);
        admit(&mut f.replica, &f.other, later, now);
        let forgery = forged(&f.replica, &f.owner, &wrapper(&f.replica, &stale, &held(&f.replica)), now);
        let owner = f.owner.clone();
        refused_with(&mut f.replica, &owner, forgery, now, "stale epoch");
    }

    phase a_compaction_founds_a_route_a_replica_holds_none_of;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let message = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, message, now);
        let compaction = mint(&mut f.replica, &keys, &f.owner, now).unwrap();

        let mut fresh = StatePeer::new(
            WORKSPACE,
            f.authority.clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        fresh.select_exact_admissions().unwrap();
        fresh
            .receive_from(origin(&f.owner), f.replica.workspace_message(), now)
            .unwrap();
        assert!(
            fresh.document(APP, RECORD).is_none(),
            "the replica holds no history of the record"
        );
        admit(&mut fresh, &f.owner, compaction.clone(), now);
        assert_eq!(held(&fresh), vec![envelope_of(&compaction)]);
    }

    phase a_sealed_checkpoint_founds_the_route_its_deltas_change;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let first = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, first, now);
        let later = write(&f.replica, &f.other, &keys, &author.edit("value", "2"), now);
        admit(&mut f.replica, &f.other, later.clone(), now);
        let saved = checkpoint(&f.replica, &keys, &f.owner, now);

        let mut fresh = StatePeer::new(
            WORKSPACE,
            f.authority.clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        fresh.select_exact_admissions().unwrap();
        fresh
            .receive_from(origin(&f.owner), f.replica.workspace_message(), now)
            .unwrap();
        // A change whose checkpoint this replica never saw waits for one.
        let waited = fresh
            .receive_from_report(origin(&f.other), later.clone(), now)
            .unwrap();
        assert!(
            matches!(
                waited.primary(),
                InputDisposition::RetryLater(MissingPrerequisite::DocumentCheckpoint)
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
        // is decided again behind it: the checkpoint holds its writer's writes up
        // to it, so it waits for nothing the checkpoint folded in.
        admit(&mut fresh, &f.owner, saved.clone(), now);
        assert_eq!(held(&fresh), vec![envelope_of(&saved), envelope_of(&later)]);
    }

    phase a_replayed_or_superseded_compaction_is_refused;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let first = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, first, now);
        let mut replica = f.replica.clone();
        let mut lagging = f.replica.clone();
        let older = mint(&mut f.replica, &keys, &f.owner, now).unwrap();
        let after = f.replica.durable().unwrap();
        let replay = f
            .replica
            .receive_from_report(origin(&f.owner), older.clone(), now)
            .unwrap();
        assert!(!matches!(replay.primary(), InputDisposition::Accepted(_)));
        assert_eq!(f.replica.durable().unwrap(), after, "a replay changes nothing");

        let second = write(&f.replica, &f.other, &keys, &author.edit("value", "2"), now);
        admit(&mut f.replica, &f.other, second.clone(), now);
        let newer = mint(&mut f.replica, &keys, &f.owner, now).unwrap();
        assert_eq!(held(&f.replica), vec![envelope_of(&newer)]);

        admit(&mut replica, &f.owner, older.clone(), now);
        admit(&mut replica, &f.other, second.clone(), now);
        admit(&mut replica, &f.owner, newer.clone(), now);
        assert_eq!(held(&replica), held(&f.replica));
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
        // Its history is not the one the newer fold replaces: it never took the
        // fold that came between.
        refused_with(
            &mut lagging,
            &owner,
            newer,
            now,
            "does not cover exactly this replica's writes",
        );
    }

    phase a_partial_compaction_is_refused;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let mut holder = f.replica.clone();
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

        admit(&mut f.replica, &f.other, writes[0].clone(), now);
        let forgery = forged(&f.replica, &owner, &wrapper(&f.replica, &keys, &all), now);
        refused_with(&mut f.replica, &owner, forgery, now, "has not admitted");

        // A write past the next one of its writer waits for those between,
        // which its sender is asked for: a replica never holds a gap a
        // compaction could cover partially. The answer admits both.
        let mut gap = f.replica.clone();
        held_for_history(&mut gap, &f.other, writes[2].clone(), now);
        assert_eq!(held(&gap), all[..1]);
        admit(&mut gap, &f.other, writes[1].clone(), now);
        assert_eq!(held(&gap), all);

        admit(&mut f.replica, &f.other, writes[1].clone(), now);
        let (mut other_set, metadata) = wrapper(&f.replica, &keys, &all[..2]);
        other_set.prefix_hash = collab_types::hash_sealed_prefix(&all[..1]).unwrap();
        let forgery = forged(&f.replica, &owner, &(other_set, metadata), now);
        refused_with(&mut f.replica, &owner, forgery, now, "partial");

        let (mut nothing, metadata) = wrapper(&f.replica, &keys, &all[..1]);
        nothing.covers = vec![(
            Ed25519PublicKey::from_bytes(owner.verifying_key().to_bytes()),
            floor(&f.replica, &owner),
        )];
        nothing.prefix_hash = collab_types::hash_sealed_prefix(std::iter::empty()).unwrap();
        let forgery = forged(&f.replica, &owner, &(nothing, metadata), now);
        refused_with(&mut f.replica, &owner, forgery, now, "supersedes nothing");

        let first = mint(&mut f.replica, &keys, &owner, now).unwrap();
        let third = write(&f.replica, &f.other, &keys, &author.edit("value", "4"), now);
        admit(&mut f.replica, &f.other, third, now);
        let history = held(&f.replica);
        assert_eq!(history[0], envelope_of(&first));
        let forgery = forged(
            &f.replica,
            &owner,
            &wrapper(&f.replica, &keys, &history[1..]),
            now,
        );
        refused_with(
            &mut f.replica,
            &owner,
            forgery,
            now,
            "leaves an earlier compaction",
        );
    }

    phase a_full_history_refuses_writes_until_a_verified_compaction_supersedes_it;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let first = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, first, now);
        let one = held(&f.replica).remove(0);
        f.replica.core.documents.get_mut(&route()).unwrap().sealed = vec![one; MAX_SEALED_OPS].into();
        let more = write(&f.replica, &f.other, &keys, &author.edit("value", "2"), now);
        let other = f.other.clone();
        refused_with(
            &mut f.replica,
            &other,
            more.clone(),
            now,
            "draft_sealed_compaction",
        );

        let compaction = forged(
            &f.replica,
            &f.owner,
            &wrapper(&f.replica, &keys, &held(&f.replica)),
            now,
        );
        admit(&mut f.replica, &f.owner, compaction.clone(), now);
        assert_eq!(held(&f.replica), vec![envelope_of(&compaction)]);
        admit(&mut f.replica, &f.other, more.clone(), now);
        assert_eq!(
            held(&f.replica),
            vec![envelope_of(&compaction), envelope_of(&more)]
        );
    }

    phase a_compacted_record_restores_and_a_tampered_stored_compaction_does_not;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let first = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.other, first.clone(), now);
        mint(&mut f.replica, &keys, &f.owner, now).unwrap();
        let second = write(&f.replica, &f.other, &keys, &author.edit("value", "2"), now);
        admit(&mut f.replica, &f.other, second, now);
        let bytes = f.replica.durable().unwrap();
        assert_eq!(held(&f.restore(&bytes).unwrap()), held(&f.replica));

        let rewrite = |change: &dyn Fn(&mut Vec<SignedOp>)| {
            let mut value: Durable<'_> = postcard::from_bytes(&bytes).unwrap();
            change(&mut value.sealed[0].1);
            postcard::to_allocvec(&value).unwrap()
        };
        let covered = envelope_of(&first);
        let owner = f.owner.clone();
        // The writes a compaction stands in for are kept ahead of it until every device with the
        // record's keys checked it: the compaction is wherever it stands in the stored entry.
        let compaction_in = |history: &[SignedOp]| {
            history
                .iter()
                .position(|op| op.kind == OpKind::Compaction)
                .unwrap()
        };
        for (tampered, why, reason) in [
            (
                rewrite(&|history| history.swap(0, 1)),
                "a compaction after a write",
                "sealed",
            ),
            (
                rewrite(&|history| history.insert(1, covered.clone())),
                "a later write the compaction covers",
                "sealed history keeps a write beside its checkpoint twice",
            ),
            (
                rewrite(&|history| {
                    let compaction = compaction_in(history);
                    history[compaction].seq += 1;
                    collab_types::sign_op(&mut history[compaction], &owner).unwrap();
                }),
                "a compaction numbered past the writes it folds",
                "sealed",
            ),
            (
                rewrite(&|history| {
                    let compaction = compaction_in(history);
                    let last = history[compaction].payload.len() - 1;
                    history[compaction].payload[last] ^= 1;
                }),
                "an altered compaction",
                "signature did not verify",
            ),
            (
                rewrite(&|history| {
                    let compaction = compaction_in(history);
                    let later = history.remove(compaction.checked_add(1).unwrap());
                    history.insert(compaction, later);
                }),
                "a later write placed ahead of the compaction",
                "none it stands in for",
            ),
        ] {
            assert_set_aside(&tampered, f.restore(&tampered), why, reason);
        }
    }
}

tilo_test_env::scenario! {
    fn a_durable_hash_keeps_no_second_copy_of_the_replica;
    // A session that stores nothing hashes what a store would write and keeps the lengths and
    // digests, not the encoding: every commit takes this hash, and keeping the encoding of every
    // record beside the replica doubled the memory of a folder's records.
    phase hashing_a_replica_that_stores_nothing_keeps_no_encoding;
    {
        let mut f = fixture();
        let (now, keys) = (f.now, f.keys());
        let mut author = Author::new(&f.replica);
        for value in ["1", "2", "3"] {
            let sealed = write(&f.replica, &f.editor, &keys, &author.edit("value", value), now);
            admit(&mut f.replica, &f.editor, sealed, now);
        }
        assert_eq!(f.replica.kept_encoded_bytes(), 0, "no record is encoded beside the replica");
        let hashed = f.replica.durable_hash().unwrap();
        assert_eq!(f.replica.kept_encoded_bytes(), 0, "hashing keeps no part");
        assert_eq!(hashed, f.replica.durable_hash().unwrap(), "one state has one digest");
        assert_eq!(
            hashed,
            f.replica.durable_hash_of_kept_parts().unwrap(),
            "the digest is the one the stored encoding has"
        );
    }
}

tilo_test_env::scenario! {
    fn a_history_falls_due_and_the_governor_compacts_behind_its_receipt;

    phase a_history_near_its_bound_is_due_for_compaction;
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let first = write(&f.replica, &f.editor, &keys, &author.edit("value", "1"), now);
        admit(&mut f.replica, &f.editor, first, now);
        assert!(f.replica.publication().sealed_to_compact().is_empty());
        let one = held(&f.replica).remove(0);
        f.replica.core.documents.get_mut(&route()).unwrap().sealed =
            vec![one; MAX_SEALED_OPS / 4 * 3].into();
        assert_eq!(
            f.replica.publication().sealed_to_compact(),
            vec![(APP.to_owned(), RECORD.to_owned())]
        );
    }

    phase a_half_full_admission_index_makes_every_sealed_history_due;
    // Many records each short of their own bound fill the workspace's exact
    // admission index all the same: once it is half full, every sealed history
    // worth folding is due, and compacting it frees the index.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        for index in 0..super::sealed_compaction::PRESSED_HISTORY {
            let next = write(
                &f.replica,
                &f.other,
                &keys,
                &author.edit("value", &index.to_string()),
                now,
            );
            admit(&mut f.replica, &f.other, next, now);
        }
        assert!(f.replica.publication().sealed_to_compact().is_empty());
        let mut sequence = 0;
        while !f.replica.core.exact_admissions.pressed() {
            sequence += 1;
            f.replica
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
            f.replica.publication().sealed_to_compact(),
            vec![(APP.to_owned(), RECORD.to_owned())]
        );
        let before = f.replica.core.exact_admissions.len();
        mint(&mut f.replica, &keys, &f.owner, now).unwrap();
        assert!(
            f.replica.core.exact_admissions.len() < before,
            "the compaction folded the record's evidence"
        );
    }

    phase captured_history_replays_causal_order_and_continues_the_original_writer;
    {
        let mut f = fixture();
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let actor = automerge::ActorId::from(vec![0x31]);
        author.0.set_actor(actor.clone());
        let first = author.edit("value", "first");
        let second = author.edit("value", "second");
        let first = write(&f.replica, &f.owner, &keys, &first, f.now);
        let second = write(&f.replica, &f.other, &keys, &second, f.now);
        // A keyless replica admits the later causal mutation first.
        admit(&mut f.replica, &f.other, second, f.now);
        admit(&mut f.replica, &f.owner, first, f.now);
        let before = f.replica.durable().unwrap();
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
            f.replica.durable().unwrap(),
            before,
            "reading never changes durable admission"
        );
    }

    phase captured_history_folds_a_writer_sequence_written_twice;
    {
        let mut f = fixture();
        let keys = f.keys();
        let mut first = Author::new(&f.replica);
        let mut conflicting = Author::new(&f.replica);
        let actor = automerge::ActorId::from(vec![0x41]);
        first.0.set_actor(actor.clone());
        conflicting.0.set_actor(actor);
        let first = first.edit("value", "canonical");
        let conflicting = conflicting.edit("value", "conflicting");
        let one = write(&f.replica, &f.owner, &keys, &first, f.now);
        let two = write(&f.replica, &f.other, &keys, &conflicting, f.now);
        admit(&mut f.replica, &f.owner, one, f.now);
        admit(&mut f.replica, &f.other, two, f.now);
        let before = f.replica.durable().unwrap();
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
        assert_eq!(f.replica.durable().unwrap(), before);
    }

    phase a_late_joiner_reads_a_sealed_history_past_one_capture;
    // A late joiner is sent a record's whole sealed history, and a key holder's whole-record
    // checkpoint re-states what it already holds: a record's history outgrows one capture of it.
    // What a joiner reads is bounded by what its caller allows, as a record held in the clear is,
    // and its history is bounded where it is admitted. A fixed bound of one capture's own left a
    // joiner refused for good while every caller allowed several times the record's size.
    {
        /// Incompressible: a sealed history is as large as what it carries.
        const WRITE: usize = 1024 * 1024;
        /// Past what one capture hands out, and what a session reading a record allows.
        const WRITES: usize = 9;
        const ROOM: usize = 32 * 1024 * 1024;
        let mut f = fixture();
        let keys = f.keys();
        let now = f.now;
        let mut author = Author::new(&f.replica);
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
            let message = write(&f.replica, &f.owner, &keys, &edit, now);
            admit(&mut f.replica, &f.owner, message, now);
        }
        let joiner = f.restore(&f.replica.durable().unwrap()).unwrap();
        let read = |maximum: usize| {
            let mut kept = KeptDocument::default();
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

    phase captured_history_validates_route_signature_admission_and_watermarks;
    {
        let mut f = fixture();
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let plaintext = author.edit("value", SECRET);
        let op = write(&f.replica, &f.owner, &keys, &plaintext, f.now);
        admit(&mut f.replica, &f.owner, op, f.now);
        let before = f.replica.durable().unwrap();
        for corruption in 0..4 {
            let mut wire: Durable<'_> = postcard::from_bytes(&before).unwrap();
            match corruption {
                0 => wire.sealed[0].1[0].signature.0[0] ^= 1,
                1 => wire.sealed[0].0.app = "other.app".into(),
                2 => {
                    wire.sealed[0].1[0].payload[0] ^= 1;
                    collab_types::sign_op(&mut wire.sealed[0].1[0], &f.owner).unwrap();
                }
                _ => {
                    assert_eq!(corruption, 3);
                    wire.documents[0].watermarks.clear();
                }
            }
            let corrupted = postcard::to_allocvec(&wire).unwrap();
            let why = format!("full restore corruption {corruption}");
            let restored = f.restore(&corrupted);
            let reason = match corruption {
                0 => "signature did not verify",
                1 => {
                    // History naming a record the state does not hold is no record's to set
                    // aside.
                    assert!(restored.is_err(), "{why}");
                    continue;
                }
                2 => "sealed history lacks exact admission evidence",
                _ => "sealed history is ahead of the record's watermark",
            };
            assert_set_aside(&corrupted, restored, &why, reason);
        }
        assert_eq!(f.replica.durable().unwrap(), before);
    }

    phase captured_history_missing_admitted_write_is_refused_before_opening;
    {
        let mut f = fixture();
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        for value in ["first", "second"] {
            let plaintext = author.edit("value", value);
            let message = write(&f.replica, &f.owner, &keys, &plaintext, f.now);
            admit(&mut f.replica, &f.owner, message, f.now);
        }
        let before = f.replica.durable().unwrap();
        let mut wire: Durable<'_> = postcard::from_bytes(&before).unwrap();
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
        assert_eq!(f.replica.durable().unwrap(), before);
    }

    phase captured_history_denial_and_capacity_precede_opening_and_missing_keys_refuse;
    {
        let mut f = fixture();
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let plaintext = author.edit("value", SECRET);
        let message = write(&f.replica, &f.owner, &keys, &plaintext, f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let before = f.replica.durable().unwrap();
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
        assert_eq!(f.replica.durable().unwrap(), before);
    }

    phase captured_history_replays_compaction_and_waits_for_epochs_not_held;
    {
        let mut f = fixture();
        let old_keys = f.keys();
        let mut author = Author::new(&f.replica);
        let plaintext = author.edit("value", "before rotation");
        let message = write(&f.replica, &f.owner, &old_keys, &plaintext, f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        mint(&mut f.replica, &old_keys, &f.owner, f.now).unwrap();
        let new_keys = f.rotate();
        let plaintext = author.edit("value", "after rotation");
        let message = write(&f.replica, &f.owner, &new_keys, &plaintext, f.now);
        admit(&mut f.replica, &f.owner, message, f.now);
        let before = f.replica.durable().unwrap();
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
        // Without the rotated key the capture is the compaction alone: the write after it waits.
        let capture = restored_peer
            .capture_document(
                f.authority.device_public_key,
                (APP, RECORD),
                f.now,
                1024 * 1024,
                Some(&mut |op| open(&old_keys, op)),
            )
            .unwrap();
        let restored = AutoCommit::load(&capture.into_bytes()).unwrap();
        assert_eq!(
            restored.get(ROOT, "value").unwrap().unwrap().0.as_str(),
            Some("before rotation")
        );
        let mut wire: Durable<'_> = postcard::from_bytes(&before).unwrap();
        wire.sealed[0].1.swap(0, 1);
        let misplaced = postcard::to_allocvec(&wire).unwrap();
        assert_set_aside(
            &misplaced,
            f.restore(&misplaced),
            "a compaction is first, never a later write",
            "sealed",
        );
        assert_eq!(f.replica.durable().unwrap(), before);
    }
}

pub(super) fn sealer(
    keys: &ContentKeys,
    record: &str,
) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, record);
    move |plaintext| {
        keys.seal_content(artifact, ContentRetention::Delta, plaintext)
            .map_err(|error| error.to_string())
    }
}

/// A reader takes a compaction without deciding it again as a change of its own, so a sealed
/// write that only calls itself a compaction is refused where it is admitted: what it carries is
/// no compaction of the record's history.
#[test]
fn a_write_dressed_as_a_compaction_is_refused() {
    let f = fixture();
    let keys = f.keys();
    let mut sealing = f.replica.clone();
    let mut document = AutoCommit::load(&sealing.document(APP, RECORD).unwrap()).unwrap();
    let heads = document.get_heads();
    document.put(ROOT, "value", "sealed").unwrap();
    document.commit_edit("fixture", "value", &person(&sealing, &f.owner));
    let first = sealing
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
    let mut reader = f.replica.clone();
    admit(&mut reader, &f.owner, first, f.now);

    let heads = document.get_heads();
    document.put(ROOT, "value", "dressed").unwrap();
    document.commit_edit("fixture", "value", &person(&sealing, &f.owner));
    let written = envelope_of(
        &sealing
            .publication_mut()
            .publish_sealed_incremental(
                APP,
                RECORD,
                document.save_after(&heads),
                &mut sealer(&keys, RECORD),
                &f.owner,
                f.now,
            )
            .unwrap(),
    );
    let dressed = signed(
        &f.owner,
        next_seq(&reader, &f.owner),
        OpKind::Compaction,
        written.payload,
        written.encryption,
        f.now,
    );
    refused(&mut reader, &f.owner, message(dressed), f.now);
}

/// What a reader skips deciding again is decided where a compaction is admitted: a whole,
/// verified compaction of the record's history, signed by a member whose role reads and writes
/// nothing, is refused there and changes nothing.
#[test]
fn a_compaction_by_a_member_who_may_not_write_is_refused() {
    let mut f = fixture();
    let now = f.now;
    let keys = f.keys();
    let mut author = Author::new(&f.replica);
    let write = write(&f.replica, &f.other, &keys, &author.edit("value", "1"), now);
    admit(&mut f.replica, &f.other, write, now);
    let viewer = SigningKey::from_bytes(&[110; 32])
        .verifying_key()
        .to_bytes();
    assert!(
        f.replica
            .governance_mut()
            .set_member_role(viewer, Role::Viewer, &f.owner, now)
            .unwrap()
    );
    let valid = wrapper(&f.replica, &keys, &held(&f.replica));
    let compaction = forged(&f.replica, &f.editor, &valid, now);
    let editor = f.editor.clone();
    refused_with(&mut f.replica, &editor, compaction, now, "not granted");
}

tilo_test_env::scenario! {
    fn the_key_holder_seals_a_record_and_it_stays_sealed;

    phase the_key_holder_seals_a_record_held_in_the_clear_from_its_next_write_on;
    {
        let f = fixture();
        let keys = f.keys();
        let mut sealing = f.replica.clone();
        assert!(!sealing.publication().is_sealed_document(APP, RECORD));
        let mut document = AutoCommit::load(&sealing.document(APP, RECORD).unwrap()).unwrap();
        let heads = document.get_heads();
        document.put(ROOT, "value", "sealed").unwrap();
        document.commit_edit("fixture", "value", &person(&sealing, &f.owner));
        let message = sealing
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
        assert!(sealing.publication().is_sealed_document(APP, RECORD));
        let opened = sealing
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
            sealing.publication_mut()
                .publish_incremental(APP, RECORD, Vec::new(), &f.owner, f.now)
                .is_err()
        );

        let mut keyless = f.replica.clone();
        admit(&mut keyless, &f.owner, message, f.now);
        assert!(keyless.publication().is_sealed_document(APP, RECORD));
    }

    phase the_key_holder_writes_a_new_records_genesis_sealed;
    // A record is born from its owner's checkpoint, which holds nothing; its
    // key holder's first write, its genesis, is sealed.
    {
        let f = fixture();
        let record = "tab:2";
        let group = KeyGroup::create(
            collab_types::WorkspaceId::parse(WORKSPACE).unwrap(),
            MemberKey::generate("owner").unwrap(),
        )
        .unwrap();
        let keys = group.content_keys().unwrap();
        let mut genesis = AutoCommit::new();
        genesis.put(ROOT, "value", "born sealed").unwrap();
        genesis.commit_edit("fixture", "value", &person(&f.replica, &f.owner));
        let mut host = f.replica.clone();
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
        assert!(host.publication().is_sealed_document(APP, record));
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

    phase a_record_its_key_holder_seals_is_never_published_in_the_clear_again;
    {
        let f = fixture();
        let mut host = f.replica.clone();
        assert!(host.seal_document(APP, RECORD));
        assert!(host.publication().is_sealed_document(APP, RECORD));
        assert!(
            host.clone()
                .publication_mut()
                .publish_incremental(APP, RECORD, Vec::new(), &f.owner, f.now)
                .is_err()
        );
        let restored = f.restore(&host.durable().unwrap()).unwrap();
        assert!(restored.publication().is_sealed_document(APP, RECORD));
        let messages = host
            .publication_mut()
            .bootstrap_messages(&f.owner, f.now)
            .unwrap();
        assert!(!messages.iter().any(
            |message| matches!(message, NeighbourMsg::AppDocument { document_id, .. } if document_id == RECORD)
        ));
        assert!(!host.seal_document(APP, "tab:2"));
    }

    phase a_seal_waits_behind_the_cleartext_writes_held_before_it;
    // A cleartext write held here for what it builds on came before the seal in
    // the record's order: the seal waits behind it, so the record's base takes
    // in every write made before it sealed, however they arrive.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        // A write in the clear names its action, as a device's executor signs it.
        let mut named = |key: &str, value: &str| {
            let heads = author.0.get_heads();
            author.0.put(ROOT, key, value).unwrap();
            author.0.commit_edit("fixture", key, &person(&f.replica, &f.owner));
            DocumentMutation::Incremental(author.0.save_after(&heads))
                .encode()
                .unwrap()
        };
        let seq = next_seq(&f.replica, &f.owner);
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
        let seal = write(&f.replica, &f.other, &keys, &sealed, now);
        // `second` waits for `first`, which arrives after the seal.
        let report = f
            .replica
            .receive_from_report(origin(&f.owner), second, now)
            .unwrap();
        assert!(matches!(report.primary(), InputDisposition::RetryLater(_)));
        f.replica
            .receive_from_report(origin(&f.other), seal, now)
            .unwrap();
        f.replica
            .receive_from_report(origin(&f.owner), first, now)
            .expect("a cleartext write before the seal is the record's base");
        assert!(f.replica.publication().is_sealed_document(APP, RECORD));
        let opened = f
            .replica
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

    phase a_checkpoint_founds_its_record_behind_sealed_changes_let_go;
    // Sealed changes that came before the record's checkpoint, more than the
    // held queue keeps: the ones let go are no base the checkpoint waits
    // behind. It founds the record, and the host is asked for what was let go.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let mut changes = Vec::new();
        for n in 0..=held::MAX_HELD_INPUTS {
            let change = write(
                &f.replica,
                &f.other,
                &keys,
                &author.edit("value", &n.to_string()),
                now,
            );
            admit(&mut f.replica, &f.other, change.clone(), now);
            changes.push(change);
        }
        let saved = checkpoint(&f.replica, &keys, &f.owner, now);

        let mut fresh = StatePeer::new(
            WORKSPACE,
            f.authority.clone(),
            vec![],
            RevocationList::new(),
            now,
        )
        .unwrap();
        fresh.select_exact_admissions().unwrap();
        fresh
            .receive_from(origin(&f.owner), f.replica.workspace_message(), now)
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
    fresh.select_exact_admissions().unwrap();
    // Every device with the record's keys checked its checkpoint, which a device that joins
    // later then reads without the writes it stands in for.
    let mut checked = f.replica.clone();
    checked_by_every_device(f, &mut checked, &keys);
    fresh
        .receive_from(origin(&f.owner), checked.workspace_message(), now)
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
    for op in held(&f.replica).into_iter().skip(last.len()) {
        let key = if op.author.as_bytes() == &f.owner.verifying_key().to_bytes() {
            &f.owner
        } else {
            &f.editor
        };
        admit(&mut fresh, key, message(op), now);
    }
    assert_eq!(held(&fresh), held(&f.replica));
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

#[path = "founding_tests.rs"]
mod founding;
#[path = "kept_open_tests.rs"]
mod kept_open;

#[path = "sealed_compaction_tests/segments.rs"]
mod segments;
use segments::compact_set;

#[path = "sealed_compaction_tests/quarantine.rs"]
mod quarantine;
use quarantine::assert_set_aside;

#[path = "sealed_compaction_budget_tests.rs"]
mod budget;

/// What `peer`'s record reads as for its authority, opened with `keys`.
pub(super) fn captured(
    f: &Fixture,
    peer: &StatePeer,
    keys: &ContentKeys,
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

pub(super) fn text(doc: &AutoCommit, key: &str) -> Option<String> {
    doc.get(ROOT, key)
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

tilo_test_env::scenario! {
    fn a_change_whose_base_was_lost_is_read_without_it_or_waits;

    phase a_change_built_on_one_its_writer_never_sent_is_read_without_it;
    // A page that held a sealed record's keys before the record itself wrote a change in that
    // moment, which it let go unsigned and unsent, and its next change, built on the first, was
    // taken: the record holds a change whose base its own writer never sent, and no device ever
    // will. A reader reads the record without it rather than refusing the whole record as
    // incomplete, and a member's own compaction still refuses to fold it away.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut page = Author::new(&f.replica);
        let _let_go = page.edit("early-1", "first");
        let built_on = page.edit("early-2", "second");
        let stuck = write(&f.replica, &f.other, &keys, &built_on, now);
        admit(&mut f.replica, &f.other, stuck, now);
        let mut member = Author::new(&f.replica);
        let later = write(&f.replica, &f.owner, &keys, &member.edit("later", "kept"), now);
        admit(&mut f.replica, &f.owner, later, now);

        let read = captured(&f, &f.replica, &keys).expect("the record reads");
        assert_eq!(text(&read, "value").as_deref(), Some("base"));
        assert_eq!(text(&read, "later").as_deref(), Some("kept"));
        assert_eq!(text(&read, "early-2"), None);
        let other = f.other.verifying_key().to_bytes();
        let opened = f
            .replica
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
            mint(&mut f.replica, &keys, &f.owner, now).is_err(),
            "only the workspace's store folds a write away"
        );
    }

    phase a_change_whose_writer_signed_its_base_keeps_the_record_waiting;
    // A change whose base is in a write its writer signed and this replica has not taken in yet may
    // still arrive: the record waits for it.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut page = Author::new(&f.replica);
        let (first, second) = (
            page.edit("early-1", "first"),
            page.edit("early-2", "second"),
        );
        let on_its_way = write(&f.replica, &f.other, &keys, &first, now);
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
        held_for_history(&mut f.replica, &f.other, after, now);
        let waiting = captured(&f, &f.replica, &keys).unwrap();
        assert_eq!(text(&waiting, "early-2"), None, "the change waits for its base");

        // Signed and on its way: its sender answers the ask with it, and the change that waited
        // goes in behind it; the record reads whole.
        admit(&mut f.replica, &f.other, on_its_way, now);
        let read = captured(&f, &f.replica, &keys).unwrap();
        assert_eq!(text(&read, "early-1").as_deref(), Some("first"));
        assert_eq!(text(&read, "early-2").as_deref(), Some("second"));
        let opened = f
            .replica
            .publication()
            .opened_document(APP, RECORD, &mut |op| open(&keys, op))
            .unwrap()
            .unwrap();
        assert!(opened.unmerged.is_empty(), "{:?}", opened.unmerged);
    }

    phase a_kept_reader_takes_a_write_it_read_without_in_once_its_base_arrives;
    // A reader that keeps a record opened between reads keeps the writes it read without, and takes
    // one in once what it builds on turns up after all.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut page = Author::new(&f.replica);
        let (first, second) = (
            page.edit("early-1", "first"),
            page.edit("early-2", "second"),
        );
        let stuck = write(&f.replica, &f.other, &keys, &second, now);
        admit(&mut f.replica, &f.other, stuck, now);
        let mut kept = KeptDocument::default();
        let reader = f.authority.device_public_key;
        let mut opener = |op: &SignedOp| open(&keys, op);
        let read = f
            .replica
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

        let brought = write(&f.replica, &f.owner, &keys, &first, now);
        admit(&mut f.replica, &f.owner, brought, now);
        let read = f
            .replica
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

    phase a_writer_reloaded_after_losing_a_change_reads_without_what_it_built_on_it;
    // A device writes a record under one Automerge writer of its own across reloads, as the shared
    // desktop does. Its page lost a change and sent the next one, built on it; reloaded, the page
    // read the record without either and wrote again, numbering its next change as the lost one was
    // numbered. The stuck write still builds on a change no device will ever send: a reader reads
    // the record with what the reloaded page wrote, and without the stuck write, after one write
    // and after two.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let actor = automerge::ActorId::from(b"one device, every load".as_slice());
        let base = f.replica.document(APP, RECORD).unwrap();
        let mut page = Author::of(
            &f.replica,
            AutoCommit::load(&base).unwrap().with_actor(actor.clone()),
        );
        let _let_go = page.edit("early-1", "first");
        let stuck = write(
            &f.replica,
            &f.other,
            &keys,
            &page.edit("early-2", "second"),
            now,
        );
        admit(&mut f.replica, &f.other, stuck, now);

        let mut reloaded = Author::of(&f.replica, AutoCommit::load(&base).unwrap().with_actor(actor));
        let again = write(&f.replica, &f.other, &keys, &reloaded.edit("again", "1"), now);
        admit(&mut f.replica, &f.other, again, now);
        let read = captured(&f, &f.replica, &keys).expect("the record reads after one write");
        assert_eq!(text(&read, "again").as_deref(), Some("1"));
        assert_eq!(text(&read, "early-2"), None);
        let opened = f
            .replica
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

        let more = write(&f.replica, &f.other, &keys, &reloaded.edit("again", "2"), now);
        admit(&mut f.replica, &f.other, more, now);
        let read = captured(&f, &f.replica, &keys).expect("the record reads after two writes");
        assert_eq!(text(&read, "again").as_deref(), Some("2"));
        assert_eq!(text(&read, "early-2"), None);
        let opened = f
            .replica
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
}

impl Author {
    /// `count` records from `from` on, each a long text, as an app writes a chain part's worth
    /// of issues in one write.
    fn texts(&mut self, from: usize, count: usize, body: &str) -> Vec<u8> {
        let heads = self.0.get_heads();
        for at in from..from.saturating_add(count) {
            let text = self
                .0
                .put_object(ROOT, format!("issue-{at}"), automerge::ObjType::Text)
                .unwrap();
            self.0.splice_text(&text, 0, 0, body).unwrap();
        }
        self.commit();
        DocumentMutation::Incremental(self.0.save_after(&heads))
            .encode()
            .unwrap()
    }
}

/// How long the record of `f` takes to open from nothing, as this device's own copy opens it
/// after a restart: the least of a few reads.
fn time_to_open(f: &Fixture, keys: &ContentKeys) -> Duration {
    (0..3)
        .map(|_| {
            let mut kept = crate::KeptDocument::default();
            let began = std::time::Instant::now();
            f.replica
                .open_document(
                    f.authority.device_public_key,
                    (APP, RECORD),
                    f.now,
                    64 * 1024 * 1024,
                    Some(&mut |op| open(keys, op)),
                    &mut kept,
                )
                .unwrap_or_else(|error| panic!("the record opens: {error}"));
            began.elapsed()
        })
        .min()
        .unwrap()
}

/// A record written in a few large writes of text (the part of a tracker's chain a moved-in
/// tracker fills) opens compacted in a fraction of what replaying its writes took. A compaction
/// used to be applied change by change onto the base it followed, in the isolate and again in the
/// process, so a compacted part opened no faster than its writes replayed.
#[test]
fn a_record_of_a_few_large_writes_opens_compacted_much_faster() {
    let mut f = fixture();
    let keys = f.keys();
    let (now, editor) = (f.now, f.editor.clone());
    let mut author = Author::new(&f.replica);
    let body = "The service keeps its work beside its peers, and reads it back. ".repeat(250);
    for batch in 0..2 {
        let edit = author.texts(batch * 10, 10, &body);
        let sealed = write(&f.replica, &editor, &keys, &edit, now);
        admit(&mut f.replica, &editor, sealed, now);
    }
    let replayed = time_to_open(&f, &keys);
    compact_set(&mut f, &keys, now);
    let compacted = time_to_open(&f, &keys);
    eprintln!("replayed in {replayed:?}, compacted in {compacted:?}");
    assert!(
        compacted * 2 <= replayed,
        "the compacted record opened in {compacted:?}, its writes replayed in {replayed:?}"
    );
}

/// A receiver holding the seeded record, as a durable host would: it has no
/// keys, so a sealed write reaches it as ciphertext it keeps, never merges.
fn sealed_host() -> (StatePeer, SigningKey, SigningKey, SystemTime) {
    let (mut native, a, b, bc, now) = crate::admission_tests::seeded();
    let mut receiver = StatePeer::new(
        native.workspace_id(),
        native.owner_certificates()[0].clone(),
        vec![bc],
        RevocationList::new(),
        now,
    )
    .unwrap();
    receiver.select_exact_admissions().unwrap();
    for message in native
        .publication_mut()
        .bootstrap_messages(&a, now)
        .unwrap()
    {
        receiver.receive_from(origin(&a), message, now).unwrap();
    }
    (receiver, a, b, now)
}

/// `key`'s sealed write number `seq` to the seeded record, under a ciphertext
/// the host cannot open.
fn sealed_write(peer: &StatePeer, key: &SigningKey, seq: u64, ciphertext: &[u8]) -> NeighbourMsg {
    let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
    let mut envelope = SignedOp::new_unsigned_with_encryption(
        ciphertext.to_vec(),
        collab_types::document_artifact_id(APP, peer.workspace_id(), RECORD),
        author,
        collab_types::document_attestation_id(APP, peer.workspace_id(), RECORD),
        OpKind::Change,
        peer_time(),
        seq,
        collab_types::EncryptionMetadata::mls_application(1),
    );
    collab_types::sign_op(&mut envelope, key).unwrap();
    NeighbourMsg::AppDocument {
        peer: peer_handle(&key.verifying_key().to_bytes()),
        app_id: APP.into(),
        cluster_id: peer.workspace_id().into(),
        document_id: RECORD.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// The seeded fixture's clock.
fn peer_time() -> SystemTime {
    crate::admission_tests::seeded().4
}

fn sealed_restore(peer: &StatePeer, now: SystemTime) -> Result<StatePeer, String> {
    sealed_restore_bytes(peer, &peer.durable()?, now)
}

fn sealed_restore_bytes(
    peer: &StatePeer,
    bytes: &[u8],
    now: SystemTime,
) -> Result<StatePeer, String> {
    StatePeer::restore(
        bytes,
        peer.workspace_id(),
        peer.owner_certificates()[0].clone(),
        vec![],
        RevocationList::new(),
        now,
    )
}

tilo_test_env::scenario! {
    fn a_sealed_history_admits_only_its_authors_whole_writes_and_restores;

    phase an_admitted_write_is_kept_as_ciphertext_and_survives_restart_with_its_floor;
    {
        let (mut host, a, b, now) = sealed_host();
        let plaintext = host.document(APP, RECORD).unwrap();
        let write = sealed_write(&host, &b, 1, b"ciphertext the host cannot open");
        let report = host.receive_from_report(origin(&b), write.clone(), now).unwrap();
        assert!(
            matches!(report.primary(), InputDisposition::Accepted(_)),
            "a sealed write is admitted with an exact receipt, not parked"
        );
        assert_eq!(host.incomplete_reason(), None);
        assert_eq!(
            host.document(APP, RECORD).unwrap(),
            plaintext,
            "ciphertext is never merged into the plaintext document"
        );
        let mut restored = sealed_restore(&host, now).unwrap();
        assert_eq!(held(&restored), held(&host));
        assert_eq!(
            held(&restored)[0].payload,
            b"ciphertext the host cannot open"
        );
        assert_eq!(restored.incomplete_reason(), None);

        // A replay is kept once, and the sequence floor survives the restart.
        let report = restored.receive_from_report(origin(&b), write, now).unwrap();
        assert!(!matches!(report.primary(), InputDisposition::Accepted(_)));
        assert_eq!(held(&restored).len(), 1, "a replay is kept once");
        let mut replayed = sealed_restore(&restored, now).unwrap();
        let again = sealed_write(&replayed, &b, 1, b"sealed");
        replayed.receive_from(origin(&b), again, now).ok();
        assert_eq!(
            held(&replayed).len(),
            1,
            "the sequence floor survives restart"
        );

        // The record stays encrypted: a plaintext write after a sealed one is a downgrade.
        assert!(
            replayed
                .publication_mut()
                .publish_incremental(APP, RECORD, Vec::new(), &a, now)
                .is_err(),
            "the record stays encrypted"
        );

        // A device outside the workspace waits for a certificate; a signature that
        // is not the named author's is refused.
        let stranger = SigningKey::from_bytes(&[77; 32]);
        let before = held(&replayed);
        let report = replayed
            .receive_from_report(
                origin(&stranger),
                sealed_write(&replayed, &stranger, 1, b"sealed"),
                now,
            )
            .unwrap();
        assert_eq!(
            report.primary(),
            &InputDisposition::RetryLater(crate::MissingPrerequisite::Certificate),
            "a device outside the workspace waits for a certificate, never admitted"
        );
        assert_eq!(held(&replayed), before);
        let mut forged = sealed_write(&replayed, &stranger, 1, b"sealed");
        if let NeighbourMsg::AppDocument { envelope, peer, .. } = &mut forged {
            envelope.author = Ed25519PublicKey::from_bytes(b.verifying_key().to_bytes());
            *peer = peer_handle(&b.verifying_key().to_bytes());
        }
        assert!(
            replayed.receive_from(origin(&b), forged, now).is_err(),
            "a signature that is not the named author's is refused"
        );
        assert_eq!(held(&replayed), before);
    }

    phase tampered_revoked_or_full_history_is_refused_and_the_record_is_unchanged;
    {
        // A changed ciphertext is refused.
        let (mut host, _, b, now) = sealed_host();
        let before = host.durable().unwrap();
        let mut write = sealed_write(&host, &b, 1, b"sealed");
        if let NeighbourMsg::AppDocument { envelope, .. } = &mut write {
            envelope.payload[0] ^= 1;
        }
        assert!(host.receive_from(origin(&b), write, now).is_err());
        assert_eq!(host.durable().unwrap(), before);

        // A revoked author's write: known without its cut it waits; with the cut
        // the held write is let go and a new one refused.
        use tilo_user_identity::{RevocationEntry, RevocationReason, sign_revocation_entry};
        let (mut holder, a, b, now) = sealed_host();
        let mut revocation = RevocationEntry {
            user_public_key: SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes(),
            revoked_device_public_key: b.verifying_key().to_bytes(),
            revoked_at_ns: 1,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut revocation, &SigningKey::from_bytes(&[1; 32])).unwrap();
        holder
            .receive_from(
                TransportOrigin::UntrustedPresence(a.verifying_key().to_bytes()),
                NeighbourMsg::IdentityHello {
                    protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                    cert: Box::new(holder.owner_certificates()[0].clone()),
                    revocations: vec![revocation.clone()],
                },
                now,
            )
            .unwrap();
        let before = held(&holder);
        let report = holder
            .receive_from_report(origin(&b), sealed_write(&holder, &b, 1, b"sealed"), now)
            .unwrap();
        assert_eq!(
            report.primary(),
            &InputDisposition::RetryLater(crate::MissingPrerequisite::RemovalCut)
        );
        assert_eq!(held(&holder), before);
        assert!(
            holder
                .governance_mut()
                .publish_governed_revocations(&[revocation], &a, now)
                .unwrap()
        );
        assert_eq!(held(&holder), before);
        assert!(
            holder
                .receive_from(origin(&b), sealed_write(&holder, &b, 1, b"sealed"), now)
                .unwrap_err()
                .to_string()
                .contains("revoked")
        );
        assert_eq!(held(&holder), before);

        // A full history refuses rather than evicts.
        let (mut host, _, b, now) = sealed_host();
        host.receive_from(origin(&b), sealed_write(&host, &b, 1, b"first"), now)
            .unwrap();
        let first = held(&host)[0].clone();
        host.core.documents.get_mut(&route()).unwrap().sealed = vec![first; MAX_SEALED_OPS].into();
        assert!(
            host.receive_from(origin(&b), sealed_write(&host, &b, 2, b"one more"), now)
                .unwrap_err()
                .to_string()
                .contains("full")
        );
        assert_eq!(held(&host).len(), MAX_SEALED_OPS);
    }

    phase a_tampered_stored_history_refuses_to_restore;
    {
        let (mut host, _, b, now) = sealed_host();
        host.receive_from(origin(&b), sealed_write(&host, &b, 1, b"sealed"), now)
            .unwrap();
        let bytes = host.durable().unwrap();
        let rewrite = |change: &dyn Fn(&mut SealedHistory)| {
            let mut value: Durable<'_> = postcard::from_bytes(&bytes).unwrap();
            change(&mut value.sealed);
            postcard::to_allocvec(&value).unwrap()
        };
        for (tampered, why, reason) in [
            (
                rewrite(&|sealed| sealed[0].1[0].payload[0] ^= 1),
                "a changed ciphertext",
                "signature did not verify",
            ),
            (
                rewrite(&|sealed| sealed[0].1[0].seq = 9),
                "a write that was never admitted",
                "signature did not verify",
            ),
        ] {
            assert_set_aside(
                &tampered,
                sealed_restore_bytes(&host, &tampered, now),
                why,
                reason,
            );
        }
        // History naming a record the state does not hold is no record's to set aside.
        let elsewhere = rewrite(&|sealed| sealed[0].0.record = "tab:2".into());
        assert!(
            sealed_restore_bytes(&host, &elsewhere, now).is_err(),
            "history for another record restores"
        );
        let mut cleartext = host.clone();
        let mut op = held(&host)[0].clone();
        op.encryption = None;
        collab_types::sign_op(&mut op, &b).unwrap();
        cleartext.core.documents.get_mut(&route()).unwrap().sealed = vec![op].into();
        let stored = cleartext.durable().unwrap();
        assert_set_aside(
            &stored,
            sealed_restore_bytes(&cleartext, &stored, now),
            "sealed history cannot carry plaintext",
            "signed payload is not a typed document mutation",
        );
    }

    phase a_keyless_owner_keeps_ownership_of_its_own_sealed_write_without_renewing_it;
    {
        let (mut member, _, b, now) = sealed_host();
        let author = b.verifying_key().to_bytes();
        let write = sealed_write(&member, &b, 1, b"sealed by this member's app");
        member
            .admit_new_local_document(APP, RECORD, envelope_of(&write), now)
            .unwrap();
        assert!(
            member
                .outbox()
                .lookup_retained_local_document(&write, author)
                .unwrap()
                .is_some()
        );
        assert!(
            !member
                .outbox()
                .retained_local_document_retired(&write, author)
                .unwrap(),
            "only a durable receipt naming it retires it"
        );
    }

    phase the_durable_parts_are_the_whole_encoding_and_restore_the_same;
    {
        let (mut host, _, b, now) = sealed_host();
        for seq in 1..=(3 * 64 + 5) {
            let write = sealed_write(&host, &b, seq, format!("sealed write {seq}").as_bytes());
            host.receive_from(origin(&b), write, now).unwrap();
        }
        let whole = host.durable_whole().unwrap();
        assert_eq!(host.durable().unwrap(), whole);
        let before = host.durable_parts().unwrap();
        assert_eq!(before.concat(), whole);
        let again = host.durable_parts().unwrap();
        assert!(
            before
                .iter()
                .zip(&again)
                .filter(|(a, b)| std::sync::Arc::ptr_eq(a, b))
                .count()
                >= 3,
            "the record, its sealed history and the settled index are encoded once"
        );
        let write = sealed_write(&host, &b, 3 * 64 + 6, b"one more");
        host.receive_from(origin(&b), write, now).unwrap();
        assert_eq!(host.durable().unwrap(), host.durable_whole().unwrap());
        let restored =
            sealed_restore_bytes(&host, &host.durable_parts().unwrap().concat(), now).unwrap();
        assert_eq!(held(&restored), held(&host));
        assert_eq!(held(&restored).len(), 3 * 64 + 6);
    }
}

/// A replica that holds three sealed writes of one device, the replica as it stood before any
/// of them, the writes, and their author's copy of the document.
fn three_writes(
    f: &Fixture,
    keys: &ContentKeys,
) -> (StatePeer, StatePeer, Vec<NeighbourMsg>, Author) {
    let before = f.replica.clone();
    let mut author = Author::new(&before);
    let mut holder = before.clone();
    let writes = ["1", "2", "3"]
        .iter()
        .map(|value| {
            let message = write(&holder, &f.other, keys, &author.edit("value", value), f.now);
            admit(&mut holder, &f.other, message.clone(), f.now);
            message
        })
        .collect();
    (holder, before, writes, author)
}

/// `peer`, a key holder, decides `compaction` once more with its keys.
fn take_behind(
    peer: &mut StatePeer,
    keys: &ContentKeys,
    compaction: &NeighbourMsg,
    now: SystemTime,
) -> Result<Option<SignedOp>, String> {
    peer.publication_mut().take_compaction_behind(
        (APP, RECORD),
        envelope_of(compaction),
        &mut |op| open(keys, op),
        now,
    )
}

tilo_test_env::scenario! {
    fn a_key_holder_behind_a_compaction_takes_it_only_when_it_holds_all_the_key_holder_holds;

    phase a_replica_that_kept_less_than_a_compaction_covers_takes_it_with_its_keys;
    {
        // One device holds every write and folds them; another was ended when it had kept
        // only the first.
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (mut holder, mut behind, writes, mut author) = three_writes(&f, &keys);
        admit(&mut behind, &f.other, writes[0].clone(), now);
        let fold = mint(&mut holder, &keys, &f.owner, now).unwrap();
        // Every device with the record's keys checked the checkpoint, which a device that kept
        // less than it covers then reads without the writes it stands in for.
        checked_by_every_device(&f, &mut holder, &keys);
        behind
            .receive_from(origin(&f.owner), holder.workspace_message(), now)
            .unwrap();
        // Without the keys it stays refused: nothing tells it from one that left writes out.
        refused_with(&mut behind, &f.owner, fold.clone(), now, "has not admitted");
        assert_eq!(
            take_behind(&mut behind, &keys, &fold, now).unwrap(),
            Some(envelope_of(&fold))
        );
        assert_eq!(
            held(&behind),
            held(&holder),
            "its history is the one a replica that took the compaction first holds"
        );
        // What is written next lands, and the whole of it reads.
        let next = write(&holder, &f.other, &keys, &author.edit("value", "4"), now);
        admit(&mut holder, &f.other, next.clone(), now);
        admit(&mut behind, &f.other, next, now);
        assert_eq!(
            text(&captured(&f, &behind, &keys).unwrap(), "value").as_deref(),
            Some("4")
        );
        let kept = behind.durable().unwrap();
        assert_eq!(held(&f.restore(&kept).unwrap()), held(&behind));
    }

    phase a_replica_that_went_on_without_the_compaction_takes_it_when_it_comes_again;
    {
        // A device that refused the compaction took the writes after it all the same, and read
        // none of them: each builds on changes that are only inside the compaction. That is how
        // a device stands that stopped hearing a conversation. Its workspace sends the
        // compaction again when the device next starts.
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (mut holder, mut behind, writes, mut author) = three_writes(&f, &keys);
        admit(&mut behind, &f.other, writes[0].clone(), now);
        let fold = mint(&mut holder, &keys, &f.owner, now).unwrap();
        // Every device with the record's keys checked the checkpoint, which a device that kept
        // less than it covers then reads without the writes it stands in for.
        checked_by_every_device(&f, &mut holder, &keys);
        behind
            .receive_from(origin(&f.owner), holder.workspace_message(), now)
            .unwrap();
        refused_with(&mut behind, &f.owner, fold.clone(), now, "has not admitted");
        let next = write(&holder, &f.other, &keys, &author.edit("value", "4"), now);
        admit(&mut holder, &f.other, next.clone(), now);
        let _ = behind.receive_from_report(origin(&f.other), next.clone(), now);
        let reads = |peer: &StatePeer| {
            captured(&f, peer, &keys)
                .ok()
                .and_then(|document| text(&document, "value"))
        };
        assert_ne!(
            reads(&behind).as_deref(),
            Some("4"),
            "what was written after the compaction does not read without it"
        );
        take_behind(&mut behind, &keys, &fold, now).unwrap();
        // The write after it is sent again with it, and is the same write.
        let _ = behind.receive_from_report(origin(&f.other), next, now);
        assert_eq!(held(&behind), held(&holder));
        assert_eq!(reads(&behind).as_deref(), Some("4"));
    }

    phase a_compaction_that_lacks_a_change_the_replica_holds_is_refused_with_keys_too;
    {
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (holder, mut behind, writes, _) = three_writes(&f, &keys);
        let unwritten = behind.clone();
        admit(&mut behind, &f.other, writes[0].clone(), now);
        admit(&mut behind, &f.other, writes[1].clone(), now);
        // It names all three writes as covered and holds none of them: its snapshot is the
        // document as it stood before any of them. Taken, the two writes this replica holds
        // would be gone from it.
        let forgery = forged(
            &unwritten,
            &f.owner,
            &wrapper(&unwritten, &keys, &held(&holder)),
            now,
        );
        let before = behind.durable().unwrap();
        let refusal = take_behind(&mut behind, &keys, &forgery, now).unwrap_err();
        assert!(refusal.contains("lacks changes"), "{refusal}");
        assert_eq!(behind.durable().unwrap(), before, "the replica is unchanged");
        // Nor without keys.
        refused_with(&mut behind, &f.owner, forgery, now, "has not admitted");
    }

    phase a_compaction_signed_by_a_device_the_workspace_does_not_admit_is_refused_with_keys_too;
    {
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let (mut holder, mut behind, writes, _) = three_writes(&f, &keys);
        admit(&mut behind, &f.other, writes[0].clone(), now);
        // The compaction a member made, signed again by a device of nobody the workspace
        // knows: what it holds is whole, and whose it is decides.
        let genuine = envelope_of(&mint(&mut holder, &keys, &f.owner, now).unwrap());
        let (stranger, _) = device(120, 121);
        let forgery = forged(
            &holder,
            &stranger,
            &(
                SealedCompaction::parse(&genuine).unwrap(),
                genuine.encryption.unwrap(),
            ),
            now,
        );
        let before = behind.durable().unwrap();
        assert!(
            take_behind(&mut behind, &keys, &forgery, now).is_err(),
            "a compaction nobody of the workspace signed was taken"
        );
        assert_eq!(behind.durable().unwrap(), before, "the replica is unchanged");
    }

    phase a_removed_device_hands_a_device_behind_it_its_history_and_nothing_it_signs_after;
    {
        let f = fixture();
        let now = f.now;
        let keys = f.keys();
        let phone = f.other.clone();
        let (_, mut governor, _, _) = three_writes(&f, &keys);
        let mut author = Author::new(&governor);
        let first = write(&governor, &phone, &keys, &author.edit("first", "written"), now);
        admit(&mut governor, &phone, first, now);
        // A device that was ended holds the phone's first write and not its second.
        let mut behind = governor.clone();
        let second = write(&governor, &phone, &keys, &author.edit("second", "written"), now);
        admit(&mut governor, &phone, second, now);
        let whole = governor.clone();
        // The phone folds both into a compaction the workspace takes, and is removed: the
        // removal's cut keeps that compaction as the phone's history.
        let taken = mint(&mut governor, &keys, &phone, now).unwrap();
        // Every device with the record's keys checked it, which a device behind it then reads
        // without the writes it stands in for.
        checked_by_every_device(&f, &mut governor, &keys);
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut removal = tilo_user_identity::RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: phone.verifying_key().to_bytes(),
            revoked_at_ns: now_ns(now),
            reason: tilo_user_identity::RevocationReason::Stolen,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut removal, &root).unwrap();
        assert!(
            governor
                .governance_mut()
                .publish_governed_revocations(std::slice::from_ref(&removal), &f.owner, now)
                .unwrap()
        );
        behind
            .receive_from(origin(&f.owner), governor.workspace_message(), now)
            .unwrap();
        // Removed, the phone signs another compaction of the same two writes, under the
        // number it used: a device behind it takes nothing of it, with its keys or without.
        let forgery = forged(&whole, &phone, &wrapper(&whole, &keys, &held(&whole)), now);
        let before = behind.durable().unwrap();
        let refusal = take_behind(&mut behind, &keys, &forgery, now).unwrap_err();
        assert!(refusal.contains("revoked"), "{refusal}");
        assert_eq!(behind.durable().unwrap(), before, "the replica is unchanged");
        // The one the workspace had taken is history: the device behind it takes it.
        take_behind(&mut behind, &keys, &taken, now).unwrap();
        assert_eq!(held(&behind), held(&governor));
        assert_eq!(
            text(&captured(&f, &behind, &keys).unwrap(), "second").as_deref(),
            Some("written")
        );
    }
}

/// The record as `peer` reads it whole for the owner's device with `keys`, and the writes that
/// read leaves out for the workspace's rules.
fn read_refused(
    peer: &StatePeer,
    f: &Fixture,
    keys: &ContentKeys,
) -> (AutoCommit, Vec<UnmergedWrite>) {
    let capture = peer
        .capture_document(
            f.authority.device_public_key,
            (APP, RECORD),
            f.now,
            1024 * 1024,
            Some(&mut |op| open(keys, op)),
        )
        .expect("the record reads");
    let refused = capture.refused().to_vec();
    (AutoCommit::load(&capture.into_bytes()).unwrap(), refused)
}

/// A history holding a write the workspace's rules refuse, and its author's follow-up to it,
/// compacts without them: a device with the key folds what every reader reads, a device without the
/// key takes the compaction in their place, and the record then reads as before, with nothing left
/// to name, and takes writes again rather than filling.
#[test]
fn a_history_with_a_refused_write_compacts_without_it_and_takes_writes_again() {
    let mut f = fixture();
    let keys = f.keys();
    kept_open::write_refused_history(&mut f, &keys);
    let (before, refused) = read_refused(&f.replica, &f, &keys);
    kept_open::reads_without_the_refused(&before);
    assert_eq!(
        refused.len(),
        2,
        "the forged change and its follow-up are named"
    );

    let mut keyless = f.replica.clone();
    let compaction = mint(&mut f.replica, &keys, &f.owner, f.now)
        .expect("a history with a refused write compacts");
    keyless
        .receive_from(origin(&f.owner), compaction, f.now)
        .expect("a device without the key takes the compaction in place of what it folds");
    assert_eq!(held(&keyless), held(&f.replica));
    assert_eq!(
        held(&f.replica).len(),
        1,
        "the history is the compaction alone"
    );
    for peer in [&f.replica, &keyless] {
        let (document, refused) = read_refused(peer, &f, &keys);
        kept_open::reads_without_the_refused(&document);
        assert_eq!(refused, [], "nothing refused is left in the history");
        // The workspace's own reader reads the compacted record alike.
        let opened = peer
            .publication()
            .opened_document(APP, RECORD, &mut |op| open(&keys, op))
            .unwrap()
            .unwrap();
        assert_eq!(opened.unmerged, []);
        assert_eq!(
            text(&AutoCommit::load(&opened.document).unwrap(), "editor"),
            text(&document, "editor")
        );
    }

    // The record takes writes again.
    let message = write(
        &f.replica,
        &f.editor,
        &keys,
        &Author::new(&f.replica).edit("after", "compacted"),
        f.now,
    );
    admit(&mut f.replica, &f.editor, message.clone(), f.now);
    admit(&mut keyless, &f.editor, message, f.now);
    for peer in [&f.replica, &keyless] {
        let (document, _) = read_refused(peer, &f, &keys);
        kept_open::reads_without_the_refused(&document);
        assert_eq!(text(&document, "after").as_deref(), Some("compacted"));
    }
}

/// A member may compact a sealed record, and a compaction carries the record whole. What it folds
/// is never compared with the writes it replaces, so a member can fold in a change it made for a
/// caller who never signed it, and every reader takes the checkpoint as it is. Readers must decide
/// what a checkpoint folds as they decide the writes it replaces.
#[test]
fn a_change_forged_into_a_members_sealed_checkpoint_is_not_read() {
    let mut f = fixture();
    let keys = f.keys();
    let message = write(
        &f.replica,
        &f.other,
        &keys,
        &Author::new(&f.replica).edit("other", "kept"),
        f.now,
    );
    admit(&mut f.replica, &f.other, message, f.now);
    // The editor folds the history into a checkpoint that also holds a change it made for the
    // owner's device, which never asked for it.
    let (read, _) = read_refused(&f.replica, &f, &keys);
    let mut folded = Author::of(&f.replica, read);
    let caller = f
        .replica
        .owner_certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == f.owner.verifying_key().to_bytes())
        .unwrap();
    kept_open::caller_change(
        &mut folded,
        &f.replica,
        (&caller, &f.owner),
        f.now,
        ("forged", None),
        false,
    );
    let mut ops = f
        .replica
        .publication()
        .draft_sealed_compaction(
            (APP, RECORD),
            None,
            Some(folded.0.save()),
            &mut |op| open(&keys, op),
            &mut |plaintext| {
                keys.seal_content(artifact(), ContentRetention::of_frame(plaintext), plaintext)
                    .map_err(|error| error.to_string())
            },
            f.editor.verifying_key().to_bytes(),
            f.now,
        )
        .unwrap();
    for op in &mut ops {
        collab_types::sign_op(op, &f.editor).unwrap();
    }
    f.replica
        .publication_mut()
        .admit_sealed_compaction(APP, RECORD, None, ops, f.now)
        .unwrap();
    let (document, _) = read_refused(&f.replica, &f, &keys);
    assert_eq!(text(&document, "other").as_deref(), Some("kept"));
    assert_eq!(
        text(&document, "value").as_deref(),
        Some("base"),
        "a change forged into a checkpoint is not read"
    );
    // The checkpoint is read as the writes it stands in for merge, by every reader alike, and no
    // device with the keys gives its word for it.
    let opened = f
        .replica
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    let opened = AutoCommit::load(&opened.document).unwrap();
    assert_eq!(text(&opened, "other").as_deref(), Some("kept"));
    assert_eq!(text(&opened, "value").as_deref(), Some("base"));
    let (_, holds) = f
        .replica
        .publication()
        .check_leading_checkpoint((APP, RECORD), &mut |op| open(&keys, op))
        .expect("the writes it stands in for are here");
    assert!(
        !holds,
        "the forged checkpoint does not hold what its writes merge to"
    );
}

/// Every device of `f`'s workspace with the record's keys checks the checkpoint leading `peer`'s
/// record against the writes it stands in for, and keeps that it did in its own row of the
/// workspace.
fn checked_by_every_device(f: &Fixture, peer: &mut StatePeer, keys: &ContentKeys) {
    let checkers = peer.document_key_holders(APP, RECORD, f.now).unwrap();
    let devices = [&f.owner, &f.other, &f.editor];
    assert!(
        !checkers.is_empty()
            && checkers.iter().all(|checker| devices
                .iter()
                .any(|key| key.verifying_key().to_bytes() == *checker)),
        "the record's keys are with the fixture's devices"
    );
    for key in devices {
        check_as(peer, key, keys, f.now);
    }
}

/// `key`'s device, one of `peer`'s workspace's devices with the record's keys, checks the
/// checkpoint leading the record against the writes it stands in for, and keeps that it did.
fn check_as(peer: &mut StatePeer, key: &SigningKey, keys: &ContentKeys, now: SystemTime) {
    let device = key.verifying_key().to_bytes();
    let cert = peer
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key == device)
        .unwrap();
    let (checkpoint, holds) = peer
        .publication()
        .check_leading_checkpoint((APP, RECORD), &mut |op| open(keys, op))
        .expect("the writes the checkpoint stands in for are here");
    assert!(holds, "the checkpoint holds what its writes merge to");
    peer.workspace_mut()
        .record_checkpoint_check(&cert, (APP, RECORD), &checkpoint, key, now)
        .unwrap();
}

/// A checkpoint that holds exactly what the writes it stands in for merge to is read, and those
/// writes stay beside it, where a reader checks it, until every device with the record's keys
/// checked it: only the next checkpoint drops them then.
#[test]
fn an_honest_checkpoint_is_read_and_its_writes_go_once_every_device_with_its_keys_checked_it() {
    let mut f = fixture();
    let keys = f.keys();
    let first = write(
        &f.replica,
        &f.other,
        &keys,
        &Author::new(&f.replica).edit("other", "kept"),
        f.now,
    );
    let first_write = envelope_of(&first);
    admit(&mut f.replica, &f.other, first, f.now);
    let compaction = envelope_of(&mint(&mut f.replica, &keys, &f.owner, f.now).unwrap());

    let read = captured(&f, &f.replica, &keys).unwrap();
    assert_eq!(text(&read, "other").as_deref(), Some("kept"));
    let opened = f
        .replica
        .publication()
        .opened_document(APP, RECORD, &mut |op| open(&keys, op))
        .unwrap()
        .unwrap();
    assert_eq!(opened.unmerged, []);
    assert!(
        f.replica
            .publication()
            .checkpoint_writes(APP, RECORD)
            .iter()
            .any(|kept| kept.signature == first_write.signature),
        "the write stays beside the checkpoint no one checked yet"
    );
    let author = compaction.author.as_bytes();
    assert!(
        !f.replica
            .publication()
            .checkpoint_reached((APP, RECORD), (*author, compaction.seq)),
        "no store drops a write no device with the keys checked the checkpoint of"
    );

    let mut checked = f.replica.clone();
    checked_by_every_device(&f, &mut checked, &keys);
    assert!(
        checked
            .publication()
            .checkpoint_reached((APP, RECORD), (*author, compaction.seq)),
        "every device with the keys checked it: a store drops its writes"
    );
    f.replica = checked;

    // The next checkpoint folds the checked one, which then leads what is kept: the writes it
    // stood in for go.
    let second = write(
        &f.replica,
        &f.editor,
        &keys,
        &Author::new(&f.replica).edit("second", "written"),
        f.now,
    );
    admit(&mut f.replica, &f.editor, second, f.now);
    mint(&mut f.replica, &keys, &f.owner, f.now).unwrap();
    let kept = f.replica.publication().checkpoint_writes(APP, RECORD);
    assert_eq!(
        kept.first().map(|op| op.signature),
        Some(compaction.signature),
        "the checked checkpoint leads what is kept"
    );
    assert!(
        kept.iter().all(|op| op.signature != first_write.signature),
        "the write it stood in for is gone"
    );
    let read = captured(&f, &f.replica, &keys).unwrap();
    assert_eq!(text(&read, "other").as_deref(), Some("kept"));
    assert_eq!(text(&read, "second").as_deref(), Some("written"));
}

/// A device with the record's keys that never checks a checkpoint keeps its writes back, however
/// long it stays away: only a device the workspace no longer gives the record's keys leaves the
/// set retirement waits on.
#[test]
fn a_device_with_the_keys_that_never_checks_holds_a_checkpoints_writes_back() {
    let mut f = fixture();
    let keys = f.keys();
    let first = write(
        &f.replica,
        &f.other,
        &keys,
        &Author::new(&f.replica).edit("other", "kept"),
        f.now,
    );
    admit(&mut f.replica, &f.other, first, f.now);
    let compaction = envelope_of(&mint(&mut f.replica, &keys, &f.owner, f.now).unwrap());
    let (author, seq) = (*compaction.author.as_bytes(), compaction.seq);

    // Every device but the editor checks it: the editor holds the record's keys and has not.
    for key in [&f.owner, &f.other] {
        check_as(&mut f.replica, key, &keys, f.now);
    }
    assert!(
        !f.replica
            .publication()
            .checkpoint_reached((APP, RECORD), (author, seq)),
        "a device with the keys that never checked keeps the writes it stands in for"
    );

    // Its own check is what lets them go.
    check_as(&mut f.replica, &f.editor, &keys, f.now);
    assert!(
        f.replica
            .publication()
            .checkpoint_reached((APP, RECORD), (author, seq)),
        "every device with the keys checked it: a store drops its writes"
    );
}

/// A device the workspace removed no longer keeps the record's checkpoints back: it has none of
/// the record's keys now, whatever it had when the checkpoint was written. Retirement waits on
/// the devices the workspace's current membership and key access give the record.
#[test]
fn a_device_removed_from_the_workspace_no_longer_holds_a_checkpoints_writes_back() {
    let mut f = fixture();
    let keys = f.keys();
    let first = write(
        &f.replica,
        &f.other,
        &keys,
        &Author::new(&f.replica).edit("other", "kept"),
        f.now,
    );
    admit(&mut f.replica, &f.other, first, f.now);
    let compaction = envelope_of(&mint(&mut f.replica, &keys, &f.owner, f.now).unwrap());
    let (author, seq) = (*compaction.author.as_bytes(), compaction.seq);

    // Every device but the editor checks it, and the editor holds the writes back.
    for key in [&f.owner, &f.other] {
        check_as(&mut f.replica, key, &keys, f.now);
    }
    assert!(
        !f.replica
            .publication()
            .checkpoint_reached((APP, RECORD), (author, seq))
    );

    // The editor's person leaves the workspace: no device of theirs reads its records any more.
    let editor = hex::encode(person(&f.replica, &f.editor));
    f.replica
        .workspace_mut()
        .apply_workspace_membership_command(
            &f.authority,
            &crate::workspace_membership::WorkspaceMembershipCommand::Member {
                change: tilo_action_policy::Change::RemoveMember { person: editor },
            },
            &f.owner,
            f.now,
        )
        .unwrap();
    assert!(
        !f.replica
            .document_key_holders(APP, RECORD, f.now)
            .unwrap()
            .contains(&f.editor.verifying_key().to_bytes()),
        "a removed device holds none of the record's keys"
    );
    assert!(
        f.replica
            .publication()
            .checkpoint_reached((APP, RECORD), (author, seq)),
        "the devices with the keys now checked it: a store drops its writes"
    );
}

/// A device that takes a record's checkpoint without the writes it stands in for, and that not
/// every device with the keys checked, does not read it on its word: it asks for its history, and
/// reads the record once those writes are here.
#[test]
fn a_reader_without_a_checkpoints_writes_fetches_them_before_reading_it() {
    let mut f = fixture();
    let keys = f.keys();
    let edit = write(
        &f.replica,
        &f.other,
        &keys,
        &Author::new(&f.replica).edit("other", "kept"),
        f.now,
    );
    admit(&mut f.replica, &f.other, edit, f.now);
    let compaction = mint(&mut f.replica, &keys, &f.owner, f.now).unwrap();

    // A device that starts from the record as it was seeded, as every device does, then takes the
    // workspace as it stands and the checkpoint; `seeded` says whether it took the seed.
    let joins = |seeded: bool| {
        let mut fresh = StatePeer::new(
            WORKSPACE,
            f.authority.clone(),
            vec![],
            RevocationList::new(),
            f.now,
        )
        .unwrap();
        fresh.select_exact_admissions().unwrap();
        for message in f.seed.iter().filter(|_| seeded) {
            fresh
                .receive_from(origin(&f.owner), message.clone(), f.now)
                .unwrap();
        }
        fresh
            .receive_from(origin(&f.owner), f.replica.workspace_message(), f.now)
            .unwrap();
        let report = fresh
            .receive_from_report(origin(&f.owner), compaction.clone(), f.now)
            .unwrap();
        (fresh, report)
    };
    let (mut fresh, report) = joins(true);
    assert!(
        matches!(report.primary(), InputDisposition::Accepted(_)),
        "{:?}",
        report.primary()
    );
    assert!(
        report.summary().lacks_history,
        "the device asks for the writes the checkpoint stands in for"
    );
    assert!(
        matches!(
            captured(&f, &fresh, &keys),
            Err(crate::PeerDocumentReadError::Incomplete)
        ),
        "the checkpoint is not read on its own word"
    );

    let writes = f.replica.publication().checkpoint_writes(APP, RECORD);
    assert!(!writes.is_empty());
    let fetch = |peer: &mut StatePeer| {
        for op in &writes {
            let key = [&f.owner, &f.other, &f.editor]
                .into_iter()
                .find(|key| op.author.as_bytes() == &key.verifying_key().to_bytes())
                .unwrap();
            admit(peer, key, message(op.clone()), f.now);
        }
    };
    // A device that has the writes and not the record they were written on waits for it: the
    // checkpoint is never read as what those writes merge to without their base. (Asked first:
    // once a device here read the checkpoint against all its writes, this process keeps that
    // verdict.)
    let (mut unseeded, _) = joins(false);
    fetch(&mut unseeded);
    assert!(
        matches!(
            captured(&f, &unseeded, &keys),
            Err(crate::PeerDocumentReadError::Incomplete)
        ),
        "a write whose base is not here is waited for, never refused"
    );

    fetch(&mut fresh);
    let read = captured(&f, &fresh, &keys).unwrap();
    assert_eq!(text(&read, "other").as_deref(), Some("kept"));
    assert_eq!(text(&read, "value").as_deref(), Some("base"));
}

tilo_test_env::scenario! {
    fn a_host_following_its_store_reopens_what_it_kept;

    phase a_write_an_earlier_fold_decided_and_handed_over_again_stays_decided;
    // A host keeps a busy record for hours: its writers fill the history, the host folds it, and
    // its store hands writes over again as it follows them. A fold that folds the one before it
    // names only the writers since, so a write the first fold decided comes back past what the
    // history's leading fold names. It is decided all the same, and the host's durable state
    // reopens with every write in it, however often it is stored on the way.
    {
        let mut f = fixture();
        let now = f.now;
        let keys = f.keys();
        let mut author = Author::new(&f.replica);
        let mut sent = Vec::new();
        let mut written = Vec::new();
        for round in 0..6 {
            // The other device writes in every other round; the host writes and folds in each.
            if round % 2 == 0 {
                for chunk in 0..24 {
                    let key = format!("chunk.{round}.{chunk}");
                    let write = write(&f.replica, &f.other, &keys, &author.edit(&key, "x"), now);
                    sent.push(write.clone());
                    written.push(key);
                    admit(&mut f.replica, &f.other, write, now);
                    // Stored as it goes, each time from what the last store made.
                    f.replica.durable_parts().unwrap();
                }
            }
            let key = format!("tree.{round}");
            let tree = write(&f.replica, &f.owner, &keys, &author.edit(&key, "x"), now);
            written.push(key);
            admit(&mut f.replica, &f.owner, tree, now);
            mint(&mut f.replica, &keys, &f.owner, now).unwrap();
            // Every device with the record's keys checks the fold, as the host checks its own.
            let mut checked = f.replica.clone();
            checked_by_every_device(&f, &mut checked, &keys);
            f.replica = checked;
            f.replica.durable_parts().unwrap();
            let history = held(&f.replica);
            // Handed over as the store follows them: decided by the writer's floor, not looked
            // up as a retry first.
            for again in &sent {
                f.replica
                    .receive_from(origin(&f.other), again.clone(), now)
                    .unwrap();
            }
            let restored = f.restore(&f.replica.durable().unwrap()).unwrap();
            assert_eq!(held(&restored), held(&f.replica), "round {round} reopens");
            assert_eq!(
                held(&f.replica),
                history,
                "a write handed over again is no new write of the history"
            );
            f.replica = restored;
        }
        let read = captured(&f, &f.replica, &keys).unwrap();
        for key in &written {
            assert_eq!(text(&read, key).as_deref(), Some("x"), "{key} is read");
        }
    }
}
