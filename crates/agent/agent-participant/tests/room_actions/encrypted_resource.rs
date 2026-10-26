use super::resource_support::{
    admit_person, on_messages, open_admitted_peer, part_route, published_message,
};
use super::{
    CatalogueQuery, DeviceCapabilities, DocumentRoute, Replica, Seed, common, enrol,
    record_provider, start,
};
use tilo_actions::remote::ViewResourceQuery;
use tilo_peer_state::join::MemberRole;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn remote_encrypted_chat_primary_is_discoverable_after_host_restart() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Host fixture", 1);
    let (author, _) = tilo_user_identity::UserIdentity::generate("Remote writer", 1);
    let (viewer, _) = tilo_user_identity::UserIdentity::generate("Remote reader", 1);
    let host = enrol(
        &owner,
        "encrypted-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let writer_device = enrol(&author, "remote-writer", DeviceCapabilities::member());
    let reader_device = enrol(&viewer, "remote-reader", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let room = hosted
        .relay
        .rooms()
        .unwrap()
        .create("Remote encrypted resource")
        .await
        .unwrap();
    admit_person(
        &hosted,
        &host,
        &room.room,
        &writer_device,
        MemberRole::Editor,
    )
    .await;
    admit_person(
        &hosted,
        &host,
        &room.room,
        &reader_device,
        MemberRole::Viewer,
    )
    .await;
    let publisher = open_admitted_peer(&hosted, &writer_device, &host, &room.room).await;
    let mut source = Replica::open(
        &common::catalogue(),
        "so.tilo.chat",
        &room.room,
        &hex::encode(writer_device.cert.user_public_key),
        tilo_agent_participant::ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    let writer = hex::encode(writer_device.cert.user_public_key);
    chat::demo::apply_as(
        &mut doc,
        &writer,
        chat::Chat::CREATE_CHANNEL.with(chat::record::chat::CreateChannel {
            name: "general".into(),
            category: None,
            kind: None,
            topic: None,
        }),
    )
    .unwrap();
    let channel = chat::projection::read(&doc)
        .expect("the chat record reads")
        .channel_rows()
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Text)
        .unwrap();
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    // The message is kept in the first part of the chat's messages.
    let message = on_messages(
        &mut source,
        "post",
        &serde_json::json!({"channel": channel.id, "text": "Retained encrypted é"}),
    );
    let (id, kind) = published_message(&source, &message);
    let part = chat::parts::part(0);
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.room.clone(),
        record: source.wire_record().into(),
    };
    assert!(
        route.record.is_empty(),
        "exercise the native primary's canonical wire route"
    );
    publisher
        .peer
        .publish(route.clone(), &source.take_mutation().unwrap())
        .unwrap();
    let part_route = part_route(&room.room, &part);
    for (_, mutation) in source.take_record_mutations() {
        publisher
            .peer
            .publish(part_route.clone(), &mutation)
            .unwrap();
    }
    publisher.peer.barrier().unwrap().recv().unwrap().unwrap();
    let retained = hosted
        .relay
        .room_records()
        .retained(room.workspace_id)
        .await
        .unwrap();
    let expected_channel =
        collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record);
    assert!(retained.iter().any(|body| {
        body.author == writer_device.cert.device_public_key &&
        matches!(postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body.body),
            Ok(neighbour_wire::NeighbourMsg::AppDocument {app_id, record_id, envelope, ..})
            if app_id == route.app_id && record_id == route.record &&
                envelope.encryption.as_ref().is_some_and(|metadata| metadata.channel == expected_channel))
    }), "the real remote writer's retained primary must be encrypted on its exact channel");
    // The message's own record, the part, is read cold the same way.
    let query = CatalogueQuery {
        provider: Some(record_provider(&part_route)),
        view_resource: Some(ViewResourceQuery {
            id: id.clone(),
            kind,
        }),
        ..CatalogueQuery::default()
    };
    let mut warm_reader = open_admitted_peer(&hosted, &reader_device, &host, &room.room).await;
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
    hosted.relay.shutdown().await;
    let restored = start(&owner, &host, state.path()).await;
    let mut reader = open_admitted_peer(&restored, &reader_device, &host, &room.room).await;
    let page = reader
        .catalogue(query)
        .await
        .expect("cold exact encrypted Chat source must materialize");
    assert!(
        !page.rows.is_empty(),
        "retained remote content must produce actual resource views"
    );
    assert!(page.rows.iter().all(|row| row.resource.id == id));
    assert_eq!(page.locator.unwrap().origin, restored.web.origin);
    reader.peer.close();
    restored.agents.shutdown();
    drop(restored.web);
    restored.relay.shutdown().await;
}
