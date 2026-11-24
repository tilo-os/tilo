//! A device's own write is pending until another device that keeps the workspace takes it in:
//! it stays pending while that device is away, also across the writer's restart, turns reached
//! once the other device links and keeps it, and refused when the other device refuses it.

use crate::common::{addr, device, localhost_endpoint};

use automerge::{AutoCommit, ROOT, transaction::Transactable};
use collab_types::CommitNamed as _;
use collab_types::{
    DocumentMutation, Ed25519PublicKey, OpKind, SignedOp, document_artifact_id,
    document_attestation_id,
};
use ed25519_dalek::SigningKey;
use iroh::protocol::Router;
use neighbour_wire::{NeighbourMsg, peer_handle};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tilo_peer_state::direct::{DIRECT_ALPN, DirectSync, Kept};
use tilo_peer_state::join::WorkspaceId;
use tilo_peer_state::reach::{Reach, RecordReach};
use tilo_peer_state::store::{Appetite, Store};
use tilo_peer_state::{SeedDocument, SeedState, StatePeer, seed_workspace};
use tilo_user_identity::RevocationList;
use web_time::SystemTime;

const WORKSPACE: &str = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee";
const APP: &str = "test.app";
const RECORD: &str = "tab:1";

/// Two devices of one person: A made the workspace and its record, B joined it.
struct Two {
    a: StatePeer,
    a_key: SigningKey,
    b: StatePeer,
    b_key: SigningKey,
    person: [u8; 32],
}

fn two() -> Two {
    let root = SigningKey::from_bytes(&[0x51; 32]);
    let (a_key, a_cert) = device(&root, 0x52);
    let (b_key, b_cert) = device(&root, 0x53);
    let roster = vec![a_cert.clone(), b_cert];
    let now = SystemTime::now();
    let mut a = StatePeer::new(
        WORKSPACE,
        a_cert.clone(),
        roster.clone(),
        RevocationList::new(),
        now,
    )
    .unwrap();
    a.select_exact_admissions().unwrap();
    let mut document = AutoCommit::new();
    document.put(ROOT, "value", "from a").unwrap();
    document.commit_edit("fixture", "value", &a_cert.user_public_key);
    a.install_seed(
        SeedState {
            workspace_changes: seed_workspace(WORKSPACE, &a_cert, &roster).unwrap(),
            documents: vec![SeedDocument {
                app_id: APP.into(),
                document_id: RECORD.into(),
                kind: "test".into(),
                bytes: document.save(),
            }],
        },
        &a_key,
        now,
    )
    .unwrap();
    let b = StatePeer::from_join(WORKSPACE, a_cert, roster, &[], now).unwrap();
    Two {
        a,
        a_key,
        b,
        b_key,
        person: root.verifying_key().to_bytes(),
    }
}

/// `key`'s write of `field = value` on the record as `peer` holds it, signed, and the change it
/// carries.
fn write(
    peer: &mut StatePeer,
    key: &SigningKey,
    person: &[u8; 32],
    field: &str,
    value: &str,
) -> (NeighbourMsg, [u8; 32]) {
    let mut edit = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    edit.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let heads = edit.get_heads();
    edit.put(ROOT, field, value).unwrap();
    edit.commit_edit("fixture", field, person);
    let change = edit.get_heads()[0].0;
    let message = peer
        .publication_mut()
        .publish_incremental(APP, RECORD, edit.save_after(&heads), key, SystemTime::now())
        .unwrap();
    (message, change)
}

/// The hash a write's answer names it by: its frame's.
fn hash_of(message: &NeighbourMsg) -> [u8; 32] {
    *blake3::hash(&postcard::to_allocvec(message).unwrap()).as_bytes()
}

/// A device keeping the workspace at a store under `dir`, opened as a device that starts does.
fn keeping(peer: StatePeer, key: &SigningKey, dir: &std::path::Path) -> (DirectSync, Arc<Store>) {
    let store = Arc::new(Store::open(dir, Appetite::StoreAll).unwrap());
    let sync = DirectSync::new(peer, key.clone()).with_store(Arc::clone(&store));
    (sync, store)
}

/// Wait until `a`'s record reads `done`; `what` names the wait that gave up.
async fn until_reach(
    a: &DirectSync,
    store: &Store,
    what: &str,
    done: impl Fn(&RecordReach) -> bool,
) -> RecordReach {
    let mut moved = store.reach_moved();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let reach = a.record_reach(APP, RECORD).unwrap();
            if done(&reach) {
                return reach;
            }
            let _ = moved.recv().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the record's reach moves: {what}"))
}

/// Serve `a` on an endpoint of its own and have `b` dial it there: B keeps the workspace, so it
/// answers A's writes as A answers B's.
async fn link(
    a: &DirectSync,
    a_key: &SigningKey,
    b: &DirectSync,
    b_key: &SigningKey,
) -> (Router, tokio::task::JoinHandle<Result<(), tilo_server_protocol::refusal::Refusal>>) {
    let a_endpoint = localhost_endpoint(a_key).await;
    let router = Router::builder(a_endpoint.clone())
        .accept(DIRECT_ALPN, a.clone())
        .spawn();
    let b_endpoint = localhost_endpoint(b_key).await;
    let linked = tokio::spawn({
        let (b, a_addr) = (b.clone(), addr(&a_endpoint));
        async move { b.connect(&b_endpoint, a_addr).await }
    });
    (router, linked)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_waits_for_another_keeping_device_across_a_restart_and_reaches_it_once_it_links() {
    let Two {
        a,
        a_key,
        b,
        b_key,
        person,
    } = two();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();

    // A writes while B is away: nobody else keeps the workspace's copy of it.
    let (a_sync, first_store) = keeping(a, &a_key, a_dir.path());
    let mut written = None;
    let message = a_sync
        .publish(|peer| {
            let (message, change) = write(peer, &a_key, &person, "value", "while b is away");
            written = Some(change);
            Ok(message)
        })
        .unwrap();
    let change = written.unwrap();
    let reach = a_sync.record_reach(APP, RECORD).unwrap();
    assert_eq!(
        reach.state(),
        Reach::Pending,
        "nobody else has it: {reach:?}"
    );
    assert_eq!(reach.pending.len(), 1);
    assert_eq!(reach.pending[0].write, hash_of(&message));
    assert_eq!(
        reach.of_change(&change),
        Reach::Pending,
        "the change the write carries is named, so a view of one item asks after it"
    );

    // A stops and starts again from what it kept, B still away: the write is still pending.
    let kept_state = a_sync.read(Clone::clone);
    drop(a_sync);
    drop(first_store);
    let (a_sync, a_store) = keeping(kept_state, &a_key, a_dir.path());
    assert_eq!(
        a_sync.record_reach(APP, RECORD).unwrap().of_change(&change),
        Reach::Pending,
        "a restart forgets nothing of what is pending"
    );

    // B links and takes the write in: A hears so, and the write has reached another device.
    let (b_sync, _b_store) = keeping(b, &b_key, b_dir.path());
    let (_router, _linked) = link(&a_sync, &a_key, &b_sync, &b_key).await;
    let reached = until_reach(&a_sync, &a_store, "B keeps A's write", |reach| {
        reach.state() == Reach::Reached
    })
    .await;
    assert_eq!(reached.of_change(&change), Reach::Reached);
    assert!(reached.refused.is_empty(), "{reached:?}");

    // And it stays reached when A starts again.
    let kept_state = a_sync.read(Clone::clone);
    drop(a_sync);
    drop(a_store);
    let (a_sync, _restarted) = keeping(kept_state, &a_key, a_dir.path());
    assert_eq!(
        a_sync.record_reach(APP, RECORD).unwrap().state(),
        Reach::Reached
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_the_other_device_refuses_reads_refused() {
    let Two {
        a,
        a_key,
        b,
        b_key,
        person,
    } = two();
    let a_dir = tempfile::tempdir().unwrap();
    let b_dir = tempfile::tempdir().unwrap();
    let (a_sync, a_store) = keeping(a, &a_key, a_dir.path());
    let (b_sync, _b_store) = keeping(b, &b_key, b_dir.path());
    let mut caught_up = b_sync.caught_up();
    let (_router, _linked) = link(&a_sync, &a_key, &b_sync, &b_key).await;
    tokio::time::timeout(Duration::from_secs(30), caught_up.wait_for(|up| *up))
        .await
        .expect("B catches up with A")
        .unwrap();

    // A's own write, its signature scrambled after it was signed: B refuses it for good.
    let mut author = a_sync.read(Clone::clone);
    let (mut forged, change) = write(&mut author, &a_key, &person, "forged", "value");
    let NeighbourMsg::AppDocument { envelope, .. } = &mut forged else {
        panic!("a write is a document frame");
    };
    envelope.signature.0[0] ^= 0xff;
    a_sync.send(forged.clone()).unwrap();
    let refused = until_reach(&a_sync, &a_store, "B refuses A's forged write", |reach| {
        !reach.refused.is_empty()
    })
    .await;
    assert_eq!(refused.state(), Reach::Refused, "{refused:?}");
    assert_eq!(refused.refused[0].write, hash_of(&forged));
    assert_eq!(refused.of_change(&change), Reach::Refused);
    assert!(
        refused.pending.is_empty(),
        "a refused write is not pending: {refused:?}"
    );
}

/// How many entries `dir`'s log of pending writes holds, under `reach/`.
fn log_entries(dir: &Path) -> usize {
    let Ok(bytes) = std::fs::read(dir.join("reach").join(WORKSPACE)) else {
        return 0;
    };
    let mut rest = bytes.as_slice();
    let mut entries = 0;
    while let Some((len, body)) = rest.split_first_chunk::<4>() {
        let len = usize::try_from(u32::from_le_bytes(*len)).unwrap_or(usize::MAX);
        let Some((_, after)) = body.split_at_checked(len) else {
            break;
        };
        entries += 1;
        rest = after;
    }
    entries
}

/// `count` record writes of `peer`'s record by `key`, each carrying one change, as a device
/// sends them: what a solo workspace notes and nobody answers.
fn one_change_writes(
    peer: &StatePeer,
    key: &SigningKey,
    person: &[u8; 32],
    count: usize,
) -> Vec<(NeighbourMsg, [u8; 32])> {
    let mut document = AutoCommit::load(&peer.document(APP, RECORD).unwrap()).unwrap();
    document.set_actor(automerge::ActorId::from(
        key.verifying_key().to_bytes().to_vec(),
    ));
    let mut writes = Vec::with_capacity(count);
    for index in 0..count {
        let heads = document.get_heads();
        document.put(ROOT, "value", index as i64).unwrap();
        document.commit_edit("fixture", "value", person);
        let change = document.get_heads()[0].0;
        let payload = DocumentMutation::Incremental(document.save_after(&heads))
            .encode()
            .unwrap();
        let envelope = SignedOp::new_unsigned(
            payload,
            document_artifact_id(APP, WORKSPACE, RECORD),
            Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes()),
            document_attestation_id(APP, WORKSPACE, RECORD),
            OpKind::Change,
            SystemTime::now(),
            index as u64 + 1,
        );
        writes.push((
            NeighbourMsg::AppDocument {
                peer: peer_handle(&key.verifying_key().to_bytes()),
                app_id: APP.into(),
                cluster_id: WORKSPACE.into(),
                document_id: RECORD.into(),
                envelope: Box::new(envelope),
                readers: None,
            },
            change,
        ));
    }
    writes
}

/// A solo workspace keeps one entry for a record however many of its writes wait, and the log
/// holds only what is still pending: ten thousand writes neither list nor log ten thousand
/// frames.
#[tokio::test(flavor = "multi_thread")]
async fn a_solo_workspace_s_log_keeps_one_entry_per_record_over_ten_thousand_writes() {
    let Two {
        a, a_key, person, ..
    } = two();
    let dir = tempfile::tempdir().unwrap();
    let (a_sync, store) = keeping(a, &a_key, dir.path());
    let writes = one_change_writes(&a_sync.read(Clone::clone), &a_key, &person, 10_000);
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    for (message, _) in &writes {
        store.note_own_write(workspace, message);
    }

    let reach = a_sync.record_reach(APP, RECORD).unwrap();
    assert_eq!(
        reach.state(),
        Reach::Pending,
        "nobody else keeps the workspace, so every write has reached nobody"
    );
    assert_eq!(
        reach.pending.len(),
        1,
        "a record keeps one entry, not one per write: {reach:?}"
    );
    assert_eq!(
        reach.pending[0].changes.len(),
        writes.len(),
        "the entry names every pending write's change, so a view of one of its items reads pending"
    );
    for (_, change) in [&writes[0], &writes[5_000], &writes[9_999]] {
        assert_eq!(reach.of_change(change), Reach::Pending);
    }
    let entries = log_entries(dir.path());
    assert!(
        entries <= 512,
        "the log is written again with what is still pending: {entries} entries for 10000 writes"
    );
}

/// A restart reads the one entry a record keeps back, with every change its pending writes
/// carried still named.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_reads_back_the_record_s_one_entry_for_its_pending_writes() {
    let Two {
        a, a_key, person, ..
    } = two();
    let dir = tempfile::tempdir().unwrap();
    let (a_sync, store) = keeping(a, &a_key, dir.path());
    let mut changes = Vec::new();
    for value in ["one", "two"] {
        let mut written = None;
        a_sync
            .publish(|peer| {
                let (message, change) = write(peer, &a_key, &person, "value", value);
                written = Some(change);
                Ok(message)
            })
            .unwrap();
        changes.push(written.unwrap());
    }
    assert_eq!(a_sync.record_reach(APP, RECORD).unwrap().pending.len(), 1);

    let kept_state = a_sync.read(Clone::clone);
    drop(a_sync);
    drop(store);
    let (a_sync, _store) = keeping(kept_state, &a_key, dir.path());
    let reach = a_sync.record_reach(APP, RECORD).unwrap();
    assert_eq!(
        reach.state(),
        Reach::Pending,
        "a restart forgets nothing of what is pending"
    );
    assert_eq!(reach.pending.len(), 1, "one entry per record");
    assert_eq!(
        reach.of_change(&changes[0]),
        Reach::Pending,
        "the earlier write's change is still pending through the record's entry"
    );
    assert_eq!(reach.of_change(&changes[1]), Reach::Pending);
}

/// The answer a link delivers to the newest write of a record turns every change its pending
/// writes carried reached; an answer to an earlier one of them is no answer.
#[tokio::test(flavor = "multi_thread")]
async fn an_answer_to_the_record_s_newest_write_turns_all_it_carried_reached() {
    let Two {
        a, a_key, person, ..
    } = two();
    let dir = tempfile::tempdir().unwrap();
    let (a_sync, store) = keeping(a, &a_key, dir.path());
    let workspaces = WorkspaceId::parse(WORKSPACE).unwrap();
    let writes = one_change_writes(&a_sync.read(Clone::clone), &a_key, &person, 2);
    for (message, _) in &writes {
        store.note_own_write(workspaces, message);
    }
    let mut reach = a_sync.record_reach(APP, RECORD).unwrap();
    assert_eq!(reach.pending.len(), 1);
    assert_eq!(
        reach.pending[0].write,
        hash_of(&writes[1].0),
        "the later write stands for the record"
    );

    store.answered_own_write(workspaces, hash_of(&writes[0].0), &Kept::Durable);
    reach = a_sync.record_reach(APP, RECORD).unwrap();
    assert_eq!(
        reach.state(),
        Reach::Pending,
        "an answer to the earlier write is not the record's answer"
    );

    store.answered_own_write(workspaces, hash_of(&writes[1].0), &Kept::Durable);
    reach = a_sync.record_reach(APP, RECORD).unwrap();
    assert_eq!(reach.state(), Reach::Reached, "{reach:?}");
    assert_eq!(reach.of_change(&writes[0].1), Reach::Reached);
    assert_eq!(reach.of_change(&writes[1].1), Reach::Reached);
    assert_eq!(
        log_entries(dir.path()),
        3,
        "the two writes and the answer, which a later write compacts away"
    );
}
