//! Cold discovery of a remote person's actual encrypted native primary.
use super::resource_support::{admit_person, open_admitted_peer};
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
    let state = tempfile::tempdir().unwrap();
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
        false,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    let writer = hex::encode(writer_device.cert.user_public_key);
    chat::demo::apply_as(
        &mut doc,
        &writer,
        chat::Chat::CREATE_WORKSPACE.with(chat::record::chat::CreateWorkspace {
            name: "Remote encrypted space".into(),
        }),
    )
    .unwrap();
    let workspace = chat::projection::read(&doc)
        .workspace_rows()
        .into_iter()
        .find(|workspace| workspace.name == "Remote encrypted space")
        .unwrap()
        .id;
    let channel = chat::projection::read(&doc)
        .channel_rows()
        .into_iter()
        .find(|channel| {
            channel.workspace == workspace && channel.kind == chat::model::ChannelKind::Text
        })
        .unwrap();
    let message = chat::demo::say(&mut doc, &writer, &channel.id, "Retained encrypted é").unwrap();
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
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
        matches!(postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(&body.body),
            Ok(tilo_peer_state::NeighbourMsg::AppDocument {app_id, record_id, envelope, ..})
            if app_id == route.app_id && record_id == route.record &&
                envelope.encryption.as_ref().is_some_and(|metadata| metadata.channel == expected_channel))
    }), "the real remote writer's retained primary must be encrypted on its exact channel");
    let id = chat::projection::item_id("messages", &message);
    let query = CatalogueQuery {
        provider: Some(record_provider(&route)),
        view_resource: Some(ViewResourceQuery {
            id: id.clone(),
            kind: "chat.workspace.messages".into(),
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
