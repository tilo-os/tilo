//! A person asks the workspace's host for the link to a resource in a record the
//! host runs headless: the link names the resource and its range under the
//! web entry the host announces.
use super::resource_support::{
    admit_person, on_messages, open_admitted_peer, part_route, published_message,
};
use super::{
    ActionAddress, DeviceCapabilities, DocumentRoute, InvokeArgument, Seed, common, enrol,
    record_provider, request, start_browser,
};
use tilo_actions::remote::ReceiptStatus;
use tilo_agent_participant::ReplicaActor;
use tilo_peer_state::join::MemberRole;
use tilo_session_client::Outbox;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_host_mints_a_message_link_with_its_range() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Host fixture", 1);
    let (author, _) = tilo_user_identity::UserIdentity::generate("Writer", 1);
    let host = enrol(
        &owner,
        "share-link-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let writer_device = enrol(&author, "writer", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start_browser(&owner, &host, state.path(), Some("https://entry.example")).await;
    let workspace = hosted
        .relay
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
        MemberRole::Editor,
    )
    .await;
    let mut publisher =
        open_admitted_peer(&hosted, &writer_device, &host, &workspace.workspace).await;
    let writer = hex::encode(writer_device.cert.user_public_key);
    let mut source = common::open_in(
        &workspace.workspace,
        "so.tilo.chat",
        &writer,
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    common::add_channel(&mut doc, &writer, "general", None);
    let channel = common::channel(&doc, chat::model::ChannelKind::Text);
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    // The message is kept in the first part of the chat's messages, the
    // record it is linked in.
    let message = on_messages(
        &mut source,
        "post",
        &serde_json::json!({"channel": channel.id, "text": "Hello é, linked"}),
    );
    let (id, kind) = published_message(&source, &message);
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.workspace.clone(),
        record: source.wire_record().into(),
    };
    publisher
        .peer
        .write(route.clone(), &source.take_mutation().unwrap())
        .unwrap();
    let route = part_route(&workspace.workspace, &chat::parts::part(0));
    for (_, mutation) in source.take_record_mutations() {
        publisher.peer.write(route.clone(), &mutation).unwrap();
    }
    publisher.peer.barrier().unwrap().recv().unwrap().unwrap();
    let receipt = publisher
        .invoke(request(
            &record_provider(&route),
            ActionAddress::new(id.clone(), "share-link"),
            InvokeArgument::Json(
                serde_json::json!({"start": 6, "end": 7, "unit": "chars"})
                    .as_object()
                    .cloned()
                    .unwrap(),
            ),
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
    assert_eq!(resource.kind, kind);
    assert_eq!(kind, "chat.messages.messages");
    assert!(url.ends_with("?chars=6-7"), "{url}");
    assert!(
        url.contains("/so.tilo.chat/-/chat.messages.messages/"),
        "a part's item opens with the record the part is held beside: {url}"
    );
    publisher.peer.close();
    hosted.agents.shutdown();
    drop(hosted.web);
    hosted.relay.shutdown().await;
}
