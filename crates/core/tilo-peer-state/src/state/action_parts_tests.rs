//! An action that wrote two records reaches another device of the workspace as one change.
use super::*;
use automerge::{AutoCommit, ROOT};
use collab_types::{CommitAuthor, CommitNamed as _, CommitPart, ContentRetention, WorkspaceCommit};
use key_group::{ContentKeys, KeyGroup, MemberKey};
use tilo_user_identity::RevocationList;

const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const APP: &str = "test.app";
const RECORDS: [&str; 2] = ["task:child", "task:parent"];

/// The owner's device `a`, holding the workspace and both records, and the owner's second
/// device `b`, which writes: `a`'s state, `b`'s key and the time.
fn workspace() -> (StatePeer, SigningKey, SystemTime) {
    let root = SigningKey::from_bytes(&[1; 32]);
    let now = SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_secs(2000))
        .unwrap();
    let device = |n: u8| {
        let key = SigningKey::from_bytes(&[n; 32]);
        let cert = crate::fixture::cert(
            &root,
            &key,
            tilo_user_identity::DeviceCapabilities::member(),
        );
        (key, cert)
    };
    let (a, a_cert) = device(2);
    let (b, b_cert) = device(3);
    let roster = vec![a_cert.clone(), b_cert];
    let mut peer = StatePeer::new(
        WORKSPACE,
        a_cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    let documents = RECORDS
        .iter()
        .map(|record| {
            let mut document = AutoCommit::new();
            document.put(ROOT, "title", *record).unwrap();
            document.commit_edit("task", "title", &a_cert.user_public_key);
            SeedDocument {
                app_id: APP.into(),
                document_id: (*record).into(),
                kind: "task".into(),
                bytes: document.save(),
            }
        })
        .collect();
    peer.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, &roster).unwrap(),
            documents,
        },
        &a,
        now,
    )
    .unwrap();
    (peer, b, now)
}

/// `value` written to `record` as `key`'s device, against `peer` as it holds the record: the
/// change and its hash.
fn change(peer: &StatePeer, key: &SigningKey, record: &str, value: &str) -> (Vec<u8>, [u8; 32]) {
    let mut document = AutoCommit::load(&peer.document(APP, record).unwrap()).unwrap();
    document.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = document.get_heads();
    document.put(ROOT, "title", value).unwrap();
    let person = SigningKey::from_bytes(&[1; 32]).verifying_key().to_bytes();
    let hash = document.commit_edit("task", "title", &person).unwrap();
    (document.save_after(&heads), hash.0)
}

/// `changes` to `record`, drafted against `writer` as `key`'s device, framed under `commit`
/// where it is one part of an action, and signed: the write as it arrives. `earlier` counts the
/// writer's writes to the record `writer` has not taken in.
fn write(
    writer: &StatePeer,
    key: &SigningKey,
    record: &str,
    changes: Vec<u8>,
    commit: Option<&WorkspaceCommit>,
    earlier: u64,
    now: SystemTime,
) -> NeighbourMsg {
    sealed_write(writer, key, (record, None), changes, commit, earlier, now)
}

/// [`write`], sealed with `keys` where it names some.
fn sealed_write(
    writer: &StatePeer,
    key: &SigningKey,
    (record, keys): (&str, Option<&ContentKeys>),
    changes: Vec<u8>,
    commit: Option<&WorkspaceCommit>,
    earlier: u64,
    now: SystemTime,
) -> NeighbourMsg {
    let frame = DocumentMutation::Incremental(changes).encode().unwrap();
    let frame = match commit {
        Some(commit) => commit.frame(&frame).unwrap(),
        None => frame,
    };
    let device = key.verifying_key().to_bytes();
    let artifact = collab_types::document_artifact_id(APP, WORKSPACE, record);
    let mut seal = keys.map(|keys| {
        move |plaintext: &[u8]| {
            keys.seal_content(artifact, ContentRetention::of_frame(plaintext), plaintext)
                .map_err(|error| error.to_string())
        }
    });
    let mut envelope = writer
        .draft_checked_write(
            (APP, record),
            None,
            &frame,
            seal.as_mut().map(|seal| seal as &mut SealCheckpoint<'_>),
            device,
            earlier,
            now,
        )
        .unwrap_or_else(|_| panic!("{record} drafts"));
    let signature = ed25519_dalek::Signer::sign(key, &envelope.signing_bytes().unwrap());
    envelope.signature = collab_types::Signature::from_bytes(signature.to_bytes());
    NeighbourMsg::AppDocument {
        peer: peer_handle(&device),
        app_id: APP.into(),
        cluster_id: WORKSPACE.into(),
        document_id: record.into(),
        envelope: Box::new(envelope),
        readers: None,
    }
}

/// The action that wrote `heads`, one per record of [`RECORDS`], made on `key`'s device.
fn action(key: &SigningKey, heads: &[[u8; 32]]) -> WorkspaceCommit {
    WorkspaceCommit::new(
        uuid::Uuid::parse_str(WORKSPACE).unwrap(),
        CommitAuthor {
            actor: None,
            device: key.verifying_key().to_bytes(),
        },
        2_000_000,
        None,
        None,
        RECORDS
            .iter()
            .zip(heads)
            .map(|(record, head)| CommitPart {
                app: APP.into(),
                document: (*record).into(),
                heads: vec![*head],
            })
            .collect(),
    )
}

fn title(peer: &StatePeer, record: &str) -> String {
    let document = AutoCommit::load(&peer.document(APP, record).unwrap()).unwrap();
    document
        .get(ROOT, "title")
        .unwrap()
        .unwrap()
        .0
        .into_string()
        .unwrap()
}

fn receive(
    peer: &mut StatePeer,
    key: &SigningKey,
    message: NeighbourMsg,
    now: SystemTime,
) -> ReceiveOutcome {
    peer.receive_from(crate::fixture::origin(&key), message, now)
        .unwrap()
}

#[test]
fn an_action_across_two_records_arrives_as_one_change() {
    let (mut a, b, now) = workspace();
    let writer = a.clone();
    let (child, child_head) = change(&writer, &b, RECORDS[0], "child, named");
    let (parent, parent_head) = change(&writer, &b, RECORDS[1], "parent, with a child");
    let commit = action(&b, &[child_head, parent_head]);
    let first = write(&writer, &b, RECORDS[0], child, Some(&commit), 0, now);
    let second = write(&writer, &b, RECORDS[1], parent, Some(&commit), 0, now);

    let waiting = receive(&mut a, &b, first, now);
    assert!(
        !waiting.changed && waiting.deferred,
        "half an action changes nothing: {waiting:?}"
    );
    assert_eq!(title(&a, RECORDS[0]), RECORDS[0]);
    assert_eq!(title(&a, RECORDS[1]), RECORDS[1]);

    let whole = receive(&mut a, &b, second, now);
    assert!(whole.changed && !whole.deferred, "{whole:?}");
    assert_eq!(title(&a, RECORDS[0]), "child, named");
    assert_eq!(title(&a, RECORDS[1]), "parent, with a child");
    assert!(whole.dropped.is_empty(), "{:?}", whole.dropped);
}

#[test]
fn an_action_its_author_wrote_past_is_let_go_by_name() {
    let (mut a, b, now) = workspace();
    let writer = a.clone();
    let (child, child_head) = change(&writer, &b, RECORDS[0], "child, named");
    let (_, parent_head) = change(&writer, &b, RECORDS[1], "parent, with a child");
    let commit = action(&b, &[child_head, parent_head]);
    let part = write(&writer, &b, RECORDS[0], child, Some(&commit), 0, now);
    assert!(receive(&mut a, &b, part, now).deferred);

    // The action's write to the parent never comes; its author writes the child again.
    let (again, _) = change(&writer, &b, RECORDS[0], "child, renamed");
    let later = write(&writer, &b, RECORDS[0], again, None, 1, now);
    let taken = receive(&mut a, &b, later, now);
    assert!(taken.changed, "{taken:?}");
    assert_eq!(title(&a, RECORDS[0]), "child, renamed");
    let [let_go] = taken.dropped.as_slice() else {
        panic!("the action's waiting write is let go: {:?}", taken.dropped)
    };
    let reason = let_go.to_string();
    assert!(
        reason.contains(&hex::encode(commit.id())) && reason.contains(WORKSPACE),
        "the refusal names the action: {reason}"
    );
    assert_eq!(let_go.record, RECORDS[0]);
}

#[test]
fn a_commit_naming_more_records_than_can_wait_is_refused_by_name() {
    let (mut a, b, now) = workspace();
    let writer = a.clone();
    let (child, child_head) = change(&writer, &b, RECORDS[0], "child, named");
    let mut commit = action(&b, &[child_head, child_head]);
    commit.parts.truncate(1);
    for n in 0..MAX_HELD_INPUTS {
        commit.parts.push(CommitPart {
            app: APP.into(),
            document: format!("task:{n}"),
            heads: vec![[7; 32]],
        });
    }
    let part = write(&writer, &b, RECORDS[0], child, Some(&commit), 0, now);
    let refused = a
        .receive_from(crate::fixture::origin(&b), part, now)
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains(&hex::encode(commit.id())),
        "the refusal names the action: {refused}"
    );
    assert_eq!(title(&a, RECORDS[0]), RECORDS[0]);
}

/// `record` as `peer`'s device reads it with `keys`, through a read of the record and through an
/// opening of its whole history: its title in each, which agree.
fn sealed_title(peer: &StatePeer, keys: &ContentKeys, record: &str, now: SystemTime) -> String {
    let open = |op: &SignedOp| {
        let metadata = op.encryption.ok_or("not sealed")?;
        let ciphertext = collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
        keys.open_content(op.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    };
    let reader = SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes();
    let read = peer
        .capture_document(
            reader,
            (APP, record),
            now,
            1024 * 1024,
            Some(&mut |op| open(op)),
        )
        .unwrap();
    let opened = peer
        .publication()
        .opened_document(APP, record, &mut |op| open(op))
        .unwrap()
        .unwrap();
    let [read, opened] = [read.into_bytes(), opened.document].map(|bytes| {
        let document = AutoCommit::load(&bytes).unwrap();
        document
            .get(ROOT, "title")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap()
    });
    assert_eq!(read, opened, "both reads of {record} agree");
    read
}

#[test]
fn a_sealed_action_across_two_records_reads_as_one_change() {
    let (mut a, b, now) = workspace();
    a.select_exact_admissions().unwrap();
    let group = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
    let keys = KeyGroup::create(group, MemberKey::generate("owner").unwrap())
        .unwrap()
        .content_keys()
        .unwrap();
    let writer = a.clone();
    let (child, child_head) = change(&writer, &b, RECORDS[0], "child, named");
    let (parent, parent_head) = change(&writer, &b, RECORDS[1], "parent, with a child");
    let commit = action(&b, &[child_head, parent_head]);
    let sealed = |record, changes| {
        sealed_write(
            &writer,
            &b,
            (record, Some(&keys)),
            changes,
            Some(&commit),
            0,
            now,
        )
    };
    let first = sealed(RECORDS[0], child);
    let second = sealed(RECORDS[1], parent);

    // Sealed, the write is taken in as it comes: only its readers know the action it is part of,
    // and they read the record without it until the rest of the action is here.
    let taken = receive(&mut a, &b, first, now);
    assert!(
        taken.changed && !taken.deferred && taken.rejected == 0,
        "{taken:?}"
    );
    assert!(a.publication().is_sealed_document(APP, RECORDS[0]));
    assert_eq!(sealed_title(&a, &keys, RECORDS[0], now), RECORDS[0]);
    assert_eq!(title(&a, RECORDS[1]), RECORDS[1]);

    let whole = receive(&mut a, &b, second, now);
    assert!(!whole.deferred, "{whole:?}");
    assert_eq!(sealed_title(&a, &keys, RECORDS[0], now), "child, named");
    assert_eq!(
        sealed_title(&a, &keys, RECORDS[1], now),
        "parent, with a child"
    );
}
