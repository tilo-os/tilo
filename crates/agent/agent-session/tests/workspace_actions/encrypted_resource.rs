//! A record a remote member wrote, sealed, is read cold after its host restarts: its own
//! document and an item of a part beside it.
use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    ActionAddress, CatalogueQuery, DeviceCapabilities, DocumentRoute, Seed, enrol,
    resource_provider, start,
};
use tilo_actions::remote::ViewResourceQuery;
use tilo_agent_session::ReplicaActor;
use tilo_peer_state::join::Role;
use tilo_session_client::Outbox;

/// The tracker, which keeps its issues in parts beside its own record.
const TASKS: &str = "so.tilo.tasks";
/// The kind of an issue its tracker's parts publish.
const ISSUE: &str = "tasks.issues.issues";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remote_encrypted_record_and_its_part_are_read_cold_after_a_host_restart() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Host fixture", 1);
    let (author, _) = tilo_user_identity::UserIdentity::generate("Remote writer", 1);
    let (viewer, _) = tilo_user_identity::UserIdentity::generate("Remote reader", 1);
    let host = enrol(
        &owner,
        "encrypted-host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let writer_device = enrol(&author, "remote-writer", DeviceCapabilities::member());
    let reader_device = enrol(&viewer, "remote-reader", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspace = hosted
        .server
        .workspaces()
        .unwrap()
        .create("Remote encrypted resource")
        .await
        .unwrap();
    admit_person(
        &hosted,
        &host,
        &workspace.workspace,
        &writer_device,
        Role::Editor,
    )
    .await;
    admit_person(
        &hosted,
        &host,
        &workspace.workspace,
        &reader_device,
        Role::Viewer,
    )
    .await;
    let publisher = open_admitted_peer(&hosted, &writer_device, &host, &workspace.workspace).await;
    let mut source = crate::common::open_in(
        &workspace.workspace,
        TASKS,
        &tilo_actions::MemberId::person(hex::encode(writer_device.cert.user_public_key)),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    // The issue is kept in the first part of the tracker's issues.
    let chain = tasks::issue_list::chain();
    let made = source
        .invoke(
            &ActionAddress::new(ui::resource::chain_resource_id(&chain), "create"),
            serde_json::json!({"title": "Retained encrypted é"})
                .as_object()
                .unwrap(),
            false,
        )
        .expect("the issue is filed");
    let id = made
        .resources
        .first()
        .expect("the create names the issue")
        .clone();
    let kind = ISSUE.to_owned();
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.workspace.clone(),
        document: source.wire_document().into(),
    };
    assert!(
        route.document.is_empty(),
        "exercise the native primary's canonical wire route"
    );
    publisher
        .peer
        .write(route.clone(), &source.take_mutation().unwrap().unwrap())
        .unwrap();
    let part_route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.workspace.clone(),
        document: chain.part(0).as_str().to_owned(),
    };
    for (_, mutation) in source.take_document_mutations().unwrap() {
        publisher.peer.write(part_route.clone(), &mutation).unwrap();
    }
    crate::workspace_actions::saved(&publisher.peer.barrier().unwrap()).unwrap();
    let retained = hosted
        .server
        .workspace_documents()
        .retained(workspace.workspace_id)
        .unwrap();
    assert!(
        retained.iter().any(|body| {
            body.author == writer_device.cert.device_public_key
                && matches!(postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body.body),
            Ok(neighbour_wire::NeighbourMsg::AppDocument {app_id, document_id, envelope, ..})
            if app_id == route.app_id && document_id == route.document &&
                envelope.encryption.as_ref().is_some())
        }),
        "the real remote writer's retained primary must be encrypted on its exact channel"
    );
    // The issue's own record, the part, is read cold the same way.
    let query = CatalogueQuery {
        provider: Some(resource_provider(&part_route)),
        view_resource: Some(ViewResourceQuery {
            id: id.clone(),
            kind,
        }),
        ..CatalogueQuery::default()
    };
    let mut warm_reader =
        open_admitted_peer(&hosted, &reader_device, &host, &workspace.workspace).await;
    let warm = warm_reader
        .catalogue(query.clone())
        .await
        .expect("the original host reads this exact encrypted reference");
    assert!(!warm.rows.is_empty());
    warm_reader.peer.close();
    drop(warm_reader);
    publisher.peer.close();
    drop(publisher);
    drop(source);
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.server.shutdown().await;
    let restored = start(&owner, &host, state.path()).await;
    let mut reader =
        open_admitted_peer(&restored, &reader_device, &host, &workspace.workspace).await;
    let page = reader
        .catalogue(query)
        .await
        .expect("cold exact encrypted source must materialize");
    assert!(
        !page.rows.is_empty(),
        "retained remote content must produce actual resource views"
    );
    assert!(page.rows.iter().all(|row| row.resource.id == id));
    assert_eq!(page.locator.unwrap().origin, restored.web.origin);
    reader.peer.close();
    restored.agents.shutdown();
    drop(restored.web);
    restored.server.shutdown().await;
}
