//! A service's resources read for a device: what it holds whole, what it holds nothing of, and
//! what a chain's head names.

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;

use super::*;
use crate::apps::route::CrdtRoute;
use crate::workspace_documents::DocumentRead;
use crate::workspaces::fixtures::{
    TestGroupKeys, TestHost, device_cert, governor, test_host, test_host_holding,
};
use std::sync::Arc;
use tilo_peer_state::workspace_documents::DeviceDocuments as _;

const APP: &str = "so.tilo.notes";
const RESOURCE: &str = "note-1";
/// What one read may hand out: more than these tests' resources hold.
const MAXIMUM: usize = 1024 * 1024;

/// A read of `resource` of `APP` in `workspace` by `certificate`'s device.
fn context(
    workspace: &str,
    resource: &str,
    certificate: tilo_user_identity::DeviceCert,
) -> NativeDocumentReadContext {
    NativeDocumentReadContext::for_reader(
        CrdtRoute::record(APP, workspace.to_owned(), resource.to_owned()),
        certificate,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resource_the_service_holds_reads_from_it() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let created = server.workspaces().create("Reads").await.unwrap();
    let workspace = created.workspace;
    server
        .workspaces()
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(automerge::ROOT, "text", "hello little resource")
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();

    let reads = server.host().document_reads();
    assert!(reads.seals_documents(&workspace));
    assert_eq!(
        reads.resource_read(&workspace, APP, RESOURCE),
        ResourceRead::Sealed
    );
    assert!(reads.holds_resource(&workspace, APP, RESOURCE));
    assert!(
        !reads.seals_documents("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        "a workspace this service does not hold is the app's own"
    );

    let reading = context(&workspace, RESOURCE, cert.clone());
    reads
        .authorize(&reading)
        .expect("the workspace's device reads it");
    let NativeDocumentRead::Captured {
        revision,
        heads,
        body,
    } = reads
        .capture(&reading, None, MAXIMUM)
        .expect("the resource reads")
    else {
        panic!("the service holds the resource whole");
    };
    let mut document = automerge::AutoCommit::load(body.bytes()).expect("the resource loads");
    assert_eq!(
        document
            .get(automerge::ROOT, "text")
            .expect("the text reads")
            .expect("the text is there")
            .0
            .into_string()
            .ok()
            .as_deref(),
        Some("hello little resource")
    );
    assert_eq!(
        document
            .get_heads()
            .iter()
            .map(|head| head.0)
            .collect::<Vec<_>>(),
        heads,
        "the heads the read names are the resource's"
    );
    assert!(matches!(
        reads.capture(&reading, Some(revision), MAXIMUM).unwrap(),
        NativeDocumentRead::Unchanged(stood) if stood == revision
    ));
    assert_eq!(reads.unchanged(&reading, Some(revision)), Ok(Some(true)));
    assert_eq!(
        reads.unchanged(&reading, None),
        Ok(Some(false)),
        "a reader holding nothing does not hold the resource, decided without reading it"
    );

    let (_, stranger) = device_cert(
        &ed25519_dalek::SigningKey::from_bytes(&[0x41; 32]),
        "Someone else's laptop",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let other = context(&workspace, RESOURCE, stranger);
    assert!(
        reads.authorize(&other).is_err(),
        "a device the workspace does not certify reads nothing"
    );
    assert!(
        reads.capture(&other, None, MAXIMUM).is_err(),
        "and its capture is refused"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_resource_the_service_holds_nothing_of_is_the_apps_own() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let created = server.workspaces().create("Reads").await.unwrap();
    let workspace = created.workspace;
    let reads = server.host().document_reads();
    assert!(reads.seals_documents(&workspace));
    assert_eq!(
        reads.resource_read(&workspace, APP, "another-note"),
        ResourceRead::Own
    );
    assert!(!reads.holds_resource(&workspace, APP, "another-note"));
    assert!(matches!(
        reads
            .capture(&context(&workspace, "another-note", cert), None, MAXIMUM)
            .unwrap(),
        NativeDocumentRead::Absent
    ));
    assert!(
        reads
            .await_arriving_document(&context(&workspace, "another-note", governor().1))
            .is_none(),
        "the service keeps every resource of its workspaces, so nothing is waited for"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chains_head_is_answered_with_its_newest_part_or_with_none() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let created = server.workspaces().create("Reads").await.unwrap();
    let workspace = created.workspace;
    let base = "derived:messages@derived:default";
    let head = neighbour_wire::chain_head(base);
    let part = neighbour_wire::document_part(base, 0);
    let reads = server.host().document_reads();
    let asked = |resource: &str| {
        NativeDocumentReadContext::for_reader(
            CrdtRoute::record(APP, workspace.clone(), resource.to_owned()),
            cert.clone(),
        )
    };
    assert_eq!(
        reads.chain_head_answer(&asked(&head)),
        Some(ChainHeadAnswer::NoPart),
        "the workspace has no part of the chain, so the app starts the first"
    );
    server
        .workspaces()
        .author_workspace_document(&workspace, &cert, (APP, &part), |doc| {
            doc.put(automerge::ROOT, "part", 0_u64)
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    assert_eq!(
        reads.chain_head_answer(&asked(&head)),
        Some(ChainHeadAnswer::NewestPartOnItsWay),
        "a part came, so the chain does not start its first"
    );
    assert!(
        !reads.holds_resource(&workspace, APP, &head),
        "the head itself is no document"
    );
    assert_eq!(
        reads.chain_head_answer(&asked(RESOURCE)),
        None,
        "only a chain's head is answered so"
    );
    server.shutdown().await;
}

/// A service holding `RESOURCE` of `APP` in a new workspace it authorizes as its own device: the
/// workspace, its governor's certificate, the revision and heads its first whole read names, and
/// the document those bytes are.
async fn authored(
    scratch: &std::path::Path,
) -> (
    TestHost,
    String,
    tilo_user_identity::DeviceCert,
    [u8; 32],
    Vec<[u8; 32]>,
    automerge::AutoCommit,
) {
    let (key, cert) = governor();
    let server = test_host(scratch, &key, &cert).await;
    let created = server.workspaces().create("Own reads").await.unwrap();
    let workspace = created.workspace;
    server
        .workspaces()
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(automerge::ROOT, "text", "the first line")
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let DocumentRead::Whole {
        revision,
        heads,
        bytes,
    } = server
        .workspaces()
        .own_read(&workspace, (APP, RESOURCE), &[], MAXIMUM)
        .await
        .unwrap()
    else {
        panic!("the service holds the document whole");
    };
    let document = automerge::AutoCommit::load(&bytes).expect("the document loads");
    (server, workspace, cert, revision, heads, document)
}

/// The session's own roles read a workspace's document from the service itself, whole, and are
/// told it stands where they hold it when nothing was written since.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_reads_a_workspace_whole() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, workspace, _cert, base, read_at, mut document) = authored(scratch.path()).await;
    assert_eq!(
        document
            .get(automerge::ROOT, "text")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .ok()
            .as_deref(),
        Some("the first line")
    );
    assert_eq!(
        document
            .get_heads()
            .iter()
            .map(|head| head.0)
            .collect::<Vec<_>>(),
        read_at,
        "the heads the read names are the document's"
    );
    assert_eq!(
        server
            .workspaces()
            .own_read(&workspace, (APP, RESOURCE), &read_at, MAXIMUM)
            .await
            .unwrap(),
        DocumentRead::Unchanged(base)
    );
    server.shutdown().await;
}

/// A write since the heads a reader holds reads as only its changes; heads the service does not
/// hold read whole. What the workspace lists is the service's own.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_device_reads_what_changed_since_its_heads() {
    let scratch = tempfile::tempdir().unwrap();
    let (server, workspace, cert, base, read_at, mut document) = authored(scratch.path()).await;
    server
        .workspaces()
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(automerge::ROOT, "more", "the second line")
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    let DocumentRead::Changes {
        revision,
        heads,
        bytes,
    } = server
        .workspaces()
        .own_read(&workspace, (APP, RESOURCE), &read_at, MAXIMUM)
        .await
        .unwrap()
    else {
        panic!("the reader's heads are held, so only the changes are read");
    };
    assert_ne!(revision, base);
    tilo_automerge::ApplyIncoming::load_incoming_whole(&mut document, &bytes)
        .expect("the changes load");
    assert_eq!(
        document
            .get(automerge::ROOT, "more")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .ok()
            .as_deref(),
        Some("the second line")
    );
    assert_eq!(
        document
            .get_heads()
            .iter()
            .map(|head| head.0)
            .collect::<Vec<_>>(),
        heads
    );
    // A reader holding heads the service does not is handed the document whole.
    assert!(matches!(
        server
            .workspaces()
            .own_read(&workspace, (APP, RESOURCE), &[[0x5a; 32]], MAXIMUM)
            .await
            .unwrap(),
        DocumentRead::Whole { .. }
    ));
    let workspaces = server.workspaces();
    assert_eq!(
        workspaces.own_documents(&workspace).unwrap(),
        vec![(APP.to_owned(), RESOURCE.to_owned())]
    );
    assert!(workspaces.held_workspaces().contains(&workspace));
    assert_eq!(
        workspaces
            .own_read(&workspace, (APP, "nothing-here"), &[], MAXIMUM)
            .await
            .unwrap(),
        DocumentRead::Absent
    );
    server.shutdown().await;
}

/// A session whose service was not opened reads nothing, and says so.
#[test]
fn a_session_with_no_service_open_reads_nothing() {
    let reads = ServiceDocuments::on(crate::service::ServiceSlot::default());
    assert!(reads.read(APP, APP, RESOURCE, None).is_err());
    assert!(reads.listing(APP).is_err());
    assert!(!reads.holds(APP, APP, RESOURCE));
    assert!(reads.workspaces().is_empty());
}

/// A read asked for from a thread already driving a runtime waits on a thread of its own, rather
/// than blocking the runtime that drives it.
#[tokio::test]
async fn a_read_inside_a_runtime_waits_off_it() {
    assert_eq!(on_its_own(async { 7_u8 }), 7);
}

/// How many writes the document read below holds: a third of what a part of a mailbox an import
/// fills holds, which is plenty to tell a read of every write from a read of one.
const WRITTEN: usize = 40;

/// A part of `APP` holding a message of about the size of a mail's, written as its `at`th.
fn message(doc: &mut automerge::AutoCommit, at: usize) -> Result<(), String> {
    let text = doc
        .put_object(
            automerge::ROOT,
            format!("message {at}"),
            automerge::ObjType::Text,
        )
        .map_err(|error| error.to_string())?;
    let body = format!(
        "Message {at}. {}",
        "The venue is booked for the offsite. ".repeat(100)
    );
    doc.splice_text(&text, 0, 0, &body)
        .map_err(|error| error.to_string())
}

/// After the service restarted, a sealed document is opened from its first write once, and every
/// read after that opens only what was written since. Opened whole on every read, a part of a
/// mailbox an import writes every message into cost a full replay of its history per delivery,
/// and after a restart every retry of a delivery opened every part of the mailbox again, longer
/// than the call may wait, so the import never went on.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn after_a_restart_a_document_read_again_opens_only_what_was_written_since() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let keys = Arc::new(crate::workspaces::fixtures::TestGroupKeys::default());
    let server = crate::workspaces::fixtures::test_host_holding(
        scratch.path(),
        &key,
        &cert,
        Arc::clone(&keys),
    )
    .await;
    let workspace = server
        .workspaces()
        .create("Reads again")
        .await
        .unwrap()
        .workspace;
    for at in 0..WRITTEN {
        server
            .workspaces()
            .author_workspace_document(&workspace, &cert, (APP, RESOURCE), move |doc| {
                message(doc, at)
            })
            .await
            .unwrap();
    }
    server.shutdown().await;

    let server =
        crate::workspaces::fixtures::test_host_holding(scratch.path(), &key, &cert, keys).await;
    let workspaces = server.workspaces();
    let read = || async {
        let started = std::time::Instant::now();
        let read = workspaces
            .own_read(&workspace, (APP, RESOURCE), &[], MAXIMUM)
            .await
            .unwrap();
        assert!(matches!(read, DocumentRead::Whole { .. }), "{read:?}");
        started.elapsed()
    };
    let first = read().await;
    workspaces
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            message(doc, WRITTEN)
        })
        .await
        .unwrap();
    let again = read().await;
    eprintln!(
        "after the restart the first read took {first:?}, one after one more write {again:?}"
    );
    assert!(
        again * 4 < first,
        "a read after one write took {again:?}, the first read of {WRITTEN} writes {first:?}"
    );
    server.shutdown().await;
}

/// A record sealed under an epoch this device's group has moved past, read on a device that holds
/// no host to replay the frames: the read is on its way, and the person's other devices are asked
/// for the group's frames from the epoch the device holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_behind_its_groups_epoch_asks_the_persons_devices_for_the_frames() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let keys = Arc::new(TestGroupKeys::default());
    let server = test_host_holding(scratch.path(), &key, &cert, Arc::clone(&keys)).await;
    let created = server.workspaces().create("Reads").await.unwrap();
    let workspace = created.workspace;
    let group = collab_types::WorkspaceId::parse(&workspace).unwrap();
    // The group this device held before it moved: a fresh group of the workspace is its first
    // epoch, and no key of the epoch below is in it.
    let held =
        key_group::KeyGroup::create(group, key_group::MemberKey::generate("before").unwrap())
            .unwrap()
            .content_keys()
            .unwrap();
    // The group moves on: admitting another device commits a second epoch.
    let mut moved =
        key_group::KeyGroup::create(group, key_group::MemberKey::generate("after").unwrap())
            .unwrap();
    let joiner = key_group::MemberKey::generate("another device").unwrap();
    moved
        .add_member(joiner.fresh_key_package().unwrap())
        .unwrap();
    let moved = moved.content_keys().unwrap();
    assert_ne!(held.epoch(), moved.epoch(), "the group moved on");
    keys.hand(&workspace, moved);
    server
        .workspaces()
        .author_workspace_document(&workspace, &cert, (APP, RESOURCE), |doc| {
            doc.put(automerge::ROOT, "text", "under the new key")
                .map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    // The device comes back with the group as it held it before the move.
    let asked_from = held.epoch();
    keys.hand(&workspace, held);
    let reads = server.host().document_reads();
    let reading = context(&workspace, RESOURCE, cert);
    assert!(
        matches!(
            reads.capture(&reading, None, MAXIMUM).unwrap(),
            NativeDocumentRead::Pending
        ),
        "the record's epoch is not the one this device holds"
    );
    assert_eq!(
        keys.asked_behind(),
        vec![(group, asked_from)],
        "the read asked the person's devices for the frames from the epoch held"
    );
    server.shutdown().await;
}
