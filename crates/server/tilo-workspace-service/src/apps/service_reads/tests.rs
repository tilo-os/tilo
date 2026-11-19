#![expect(
    clippy::significant_drop_tightening,
    reason = "each test's server handle drives the host it reads until the test ends"
)]

//! A service's resources read for a device: what it holds whole, what it holds nothing of, and
//! what a chain's head names.

#![allow(clippy::unwrap_used, reason = "a test: a failed step is the assertion")]

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use std::sync::Arc;

use super::*;
use crate::apps::route::CrdtRoute;
use crate::workspaces::fixtures::{TestHost, device_cert, governor, test_host_taking_part};

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
    let server = test_host_taking_part(scratch.path(), &key, &cert).await;
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
    assert_eq!(reads.own_resource(&workspace, APP, RESOURCE), None);
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
    let server = test_host_taking_part(scratch.path(), &key, &cert).await;
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
async fn a_workspace_the_service_does_not_hold_is_read_from_the_devices_own_replica() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host_taking_part(scratch.path(), &key, &cert).await;
    let created = server.workspaces().create("Reads").await.unwrap();
    let held = created.workspace;
    let reads = ServiceReads::new(server.workspaces().clone()).answered_by(Arc::new(Elsewhere));

    // A workspace another device serves: this service holds no copy of it, so the device's own
    // replica of it answers.
    let elsewhere = ELSEWHERE;
    assert!(reads.seals_documents(elsewhere));
    assert!(reads.holds_resource(elsewhere, APP, RESOURCE));
    assert_eq!(
        reads.resource_read(elsewhere, APP, RESOURCE),
        ResourceRead::Sealed
    );

    // The workspace this service holds answers from the service, as before.
    assert!(reads.seals_documents(&held));
    assert!(!reads.holds_resource(&held, APP, "another-note"));
    assert_eq!(
        reads.resource_read(&held, APP, "another-note"),
        ResourceRead::Own
    );
    server.shutdown().await;
}

/// The workspace another device serves, whose documents only the device's own replica holds.
const ELSEWHERE: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

/// A device's own replica of the workspaces it joined at the devices serving them.
struct Elsewhere;

impl NativeDocumentReads for Elsewhere {
    fn seals_documents(&self, workspace: &str) -> bool {
        workspace == ELSEWHERE
    }

    fn resource_read(&self, workspace: &str, _app: &str, _resource: &str) -> ResourceRead {
        if workspace == ELSEWHERE {
            ResourceRead::Sealed
        } else {
            ResourceRead::Own
        }
    }

    fn holds_resource(&self, workspace: &str, _app: &str, _resource: &str) -> bool {
        workspace == ELSEWHERE
    }

    fn authorize(&self, _context: &NativeDocumentReadContext) -> Result<(), String> {
        Ok(())
    }

    fn capture(
        &self,
        _context: &NativeDocumentReadContext,
        _known_revision: Option<[u8; 32]>,
        _maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        Ok(NativeDocumentRead::Unchanged([0; 32]))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_chains_head_is_answered_with_its_newest_part_or_with_none() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host_taking_part(scratch.path(), &key, &cert).await;
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
    let server = test_host_taking_part(scratch, &key, &cert).await;
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
        workspaces.own_listing(&workspace).await.unwrap(),
        vec![(APP.to_owned(), RESOURCE.to_owned())]
    );
    assert!(workspaces.held_workspaces().await.contains(&workspace));
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
    let reads = ServiceDocuments::on(ServiceSlot::default());
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
