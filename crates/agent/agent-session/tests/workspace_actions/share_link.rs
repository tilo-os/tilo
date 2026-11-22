//! A person asks the workspace's host for the link to a resource in a record the
//! host runs headless: the link names the resource under the web entry the host
//! announces, and an item a part keeps opens with the record the part is held
//! beside.
use super::resource_support::{admit_person, open_admitted_peer};
use super::{
    ActionAddress, DeviceCapabilities, DocumentRoute, InvokeArgument, Seed, enrol, request,
    resource_provider, start_browser,
};
use tilo_actions::remote::ReceiptStatus;
use tilo_agent_session::ReplicaActor;
use tilo_peer_state::join::Role;
use tilo_session_client::Outbox;

/// The tracker, which keeps its issues in parts beside its own record.
const TASKS: &str = "so.tilo.tasks";
/// The kind of an issue its tracker's parts publish.
const ISSUE: &str = "tasks.issues.issues";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_mints_a_link_to_an_item_a_part_keeps() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Host fixture", 1);
    let (author, _) = tilo_user_identity::UserIdentity::generate("Writer", 1);
    let host = enrol(
        &owner,
        "share-link-host",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let writer_device = enrol(&author, "writer", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start_browser(&owner, &host, state.path(), Some("https://entry.example")).await;
    let workspace = hosted
        .server
        .workspaces()
        .unwrap()
        .create("Links")
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
    let mut publisher =
        open_admitted_peer(&hosted, &writer_device, &host, &workspace.workspace).await;
    let writer = hex::encode(writer_device.cert.user_public_key);
    let mut source = crate::common::open_in(
        &workspace.workspace,
        TASKS,
        &tilo_actions::MemberId::person(writer.as_str()),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    // The issue is kept in the first part of the tracker's issues, the record
    // it is linked in.
    let chain = tasks::issue_list::chain();
    let made = source
        .invoke(
            &ActionAddress::new(ui::resource::chain_resource_id(&chain), "create"),
            serde_json::json!({"title": "Hello é, linked"})
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
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.workspace.clone(),
        document: source.wire_document().into(),
    };
    publisher
        .peer
        .write(route.clone(), &source.take_mutation().unwrap().unwrap())
        .unwrap();
    let route = DocumentRoute {
        app_id: TASKS.into(),
        cluster_id: workspace.workspace.clone(),
        document: chain.part(0).as_str().to_owned(),
    };
    for (_, mutation) in source.take_document_mutations().unwrap() {
        publisher.peer.write(route.clone(), &mutation).unwrap();
    }
    publisher.peer.barrier().unwrap().recv().unwrap().unwrap();
    let receipt = publisher
        .invoke(request(
            &resource_provider(&route),
            ActionAddress::new(id.clone(), "share-link"),
            InvokeArgument::Json(serde_json::Map::new()),
        ))
        .await;
    let ReceiptStatus::Completed { output } = &receipt.status else {
        panic!("the host minted no link: {receipt:?}");
    };
    let url = output.value.as_ref().unwrap()["url"].as_str().unwrap();
    assert!(url.starts_with("https://entry.example/app/#/@"), "{url}");
    assert!(url.contains(&workspace.workspace), "{url}");
    let entries = tilo_reference::link::WebEntries::new(vec![
        tilo_reference::link::WebEntry::new("https://entry.example/").unwrap(),
    ]);
    let link = tilo_reference::link::Link::parse(url, &entries).unwrap();
    let resource = link.target.resource.unwrap();
    assert_eq!(resource.id, id, "{url}");
    assert_eq!(resource.kind, ISSUE);
    assert!(
        url.contains(&format!("/{TASKS}/-/{ISSUE}/")),
        "a part's item opens with the record the part is held beside: {url}"
    );
    publisher.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.server.shutdown().await;
}
