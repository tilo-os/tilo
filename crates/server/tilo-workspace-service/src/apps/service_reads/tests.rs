#![expect(
    clippy::significant_drop_tightening,
    reason = "each test's server handle drives the host it reads until the test ends"
)]

//! A service's resources read for a device: what it holds whole, what it holds nothing of, and
//! what a chain's head names.

#![allow(clippy::unwrap_used, reason = "a test: a failed step is the assertion")]

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;

use super::*;
use crate::apps::route::CrdtRoute;
use crate::workspaces::fixtures::{device_cert, governor, test_host_taking_part};

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
