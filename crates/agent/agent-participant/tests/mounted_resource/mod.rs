//! Mounted acceptance fixture, deliberately separate from the native test suite.
use super::{
    ActionAddress, Browser, CatalogueQuery, DeviceCapabilities, DocumentRoute, Duration, Path,
    Replica, Seed, common, enrol, record_provider, start_browser,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

fn account_document(root: &str, origin: &str, name: &str) -> Vec<u8> {
    use accounts::person::PersonAccounts;
    let mut replica = ui::tilo_resource::Replica::<PersonAccounts>::new();
    replica
        .apply(PersonAccounts::PUBLISH_BROWSER_IDENTITY.with((
            root.into(),
            origin.into(),
            name.into(),
        )))
        .unwrap();
    let kind = ui::record::RecordKind::new("tilo.accounts");
    ui::record::init(
        replica.doc_mut(),
        &ui::record::person::record_id(&kind),
        &kind,
        &collab_types::person_room_of(root).unwrap().to_string(),
    );
    replica.save()
}

/// Run against an explicitly supplied, compiled web app bundle. Missing tools fail.
pub async fn run() {
    use super::resource_support::{admit_person, open_admitted_peer};
    use tilo_peer_state::join::MemberRole;
    use tilo_relay::rooms::RemoveMemberRequest;
    let bundle = std::env::args()
        .nth(1)
        .expect("compiled web bundle path is required");
    assert!(
        Path::new(&bundle)
            .join("pkg/tilo_browser_runtime_bg.wasm")
            .is_file()
    );
    let mut options = std::env::args().skip(2);
    let option = options.next();
    let disconnect_retry = option.as_deref() == Some("--disconnect-retry");
    let voice_journey = option.as_deref() == Some("--voice");
    let encrypted_restart = option.as_deref() == Some("--encrypted-restart");
    assert!(
        option.is_none() || disconnect_retry || voice_journey || encrypted_restart,
        "expected --disconnect-retry, --voice or --encrypted-restart"
    );
    assert!(
        options.next().is_none(),
        "unexpected mounted-resource argument"
    );
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join(if voice_journey {
        "tests/mounted_resource/voice.mjs"
    } else {
        "tests/mounted_resource/driver.mjs"
    });
    let mut child = tokio::process::Command::new("node")
        .args([driver.as_os_str(), Path::new(&bundle).as_os_str()])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let tls = tilo_crypto::generate_tls_fixture(std::net::Ipv4Addr::LOCALHOST.into()).unwrap();
    let credentials = serde_json::json!({
        "key": tls.server_private_key_pem,
        "cert": tls.server_certificate_pem,
    });
    input
        .write_all(format!("{credentials}\n").as_bytes())
        .await
        .unwrap();
    let page = output
        .next_line()
        .await
        .unwrap()
        .expect("browser fixture origin");
    let page: serde_json::Value = serde_json::from_str(&page).unwrap();
    let page = page["origin"].as_str().unwrap();
    let (alice, _) = tilo_user_identity::UserIdentity::generate("Fixture owner", 1);
    let (bob, _) = tilo_user_identity::UserIdentity::generate("Fixture reader", 1);
    let host = enrol(
        &alice,
        "fixture-host",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let (remote_author, _) = tilo_user_identity::UserIdentity::generate("Remote fixture writer", 1);
    let publisher = enrol(
        if encrypted_restart {
            &remote_author
        } else {
            &alice
        },
        "fixture-publisher",
        DeviceCapabilities::primary(),
    );
    let recipient = enrol(&bob, "fixture-reader", DeviceCapabilities::member());
    let (carol, _) = tilo_user_identity::UserIdentity::generate("Fixture second reader", 1);
    let second_device = enrol(
        &carol,
        "fixture-second-browser",
        DeviceCapabilities::member(),
    );
    let state = tempfile::tempdir().unwrap();
    let mut hosted = start_browser(&alice, &host, state.path(), Some(page)).await;
    let rooms = hosted.relay.rooms().unwrap();
    let room = rooms.create("Mounted resource acceptance").await.unwrap();
    let mut native = if encrypted_restart {
        assert_ne!(
            publisher.cert.user_public_key, host.cert.user_public_key,
            "the cold encrypted source belongs to another person"
        );
        admit_person(&hosted, &host, &room.room, &publisher, MemberRole::Editor).await;
        open_admitted_peer(&hosted, &publisher, &host, &room.room).await
    } else {
        Browser::open_room(&hosted, &publisher, &host, &room.room).await
    };
    let seed = if voice_journey {
        rooms
            .prepare_application(
                &room.room,
                &host.cert,
                &common::catalogue(),
                "so.tilo.chat",
                "",
            )
            .await
            .unwrap();
        let mut canonical = rooms
            .author_application(
                &room.room,
                &host.cert,
                &common::catalogue(),
                "so.tilo.chat",
                "",
                |doc| Ok(doc.clone()),
            )
            .await
            .unwrap();
        Seed::Existing {
            record: ui::record::RecordId::package_primary(),
            changes: vec![canonical.save()],
        }
    } else {
        Seed::New
    };
    let source_author = if encrypted_restart {
        hex::encode(publisher.cert.user_public_key)
    } else {
        "Fixture owner".to_string()
    };
    let mut source = Replica::open(
        &common::catalogue(),
        "so.tilo.chat",
        &room.room,
        &source_author,
        tilo_agent_participant::ReplicaActor::Owner,
        seed,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        &source_author,
        chat::Chat::CREATE_WORKSPACE.with(chat::record::chat::CreateWorkspace {
            name: "Mounted shared space".into(),
        }),
    )
    .unwrap();
    let workspace = chat::projection::read(&doc)
        .workspace_rows()
        .into_iter()
        .find(|workspace| workspace.name == "Mounted shared space")
        .unwrap()
        .id;
    let channel = chat::projection::read(&doc)
        .channel_rows()
        .into_iter()
        .find(|channel| {
            channel.workspace == workspace && channel.kind == chat::model::ChannelKind::Text
        })
        .unwrap();
    let message = chat::demo::say(
        &mut doc,
        &source_author,
        &channel.id,
        "Hello é, mounted natively",
    )
    .unwrap();
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    let reference = serde_json::json!({"id": chat::projection::item_id("messages", &message),
        "kind": "chat.workspace.messages", "label": "Fixture selection", "span": {"start": 6, "end": 7, "unit": "Chars"}});
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.room.clone(),
        record: source.wire_record().into(),
    };
    let provider = record_provider(&route);
    source
        .invoke(
            &ActionAddress::new(
                chat::projection::item_id("channels", &channel.id),
                "share-resource",
            ),
            serde_json::json!({"reference": reference, "source_provider": provider,
            "text": "Discuss this mounted resource"})
            .as_object()
            .unwrap(),
            false,
        )
        .unwrap();
    let shared = source
        .read(|doc| chat::projection::read(doc).message_rows(&channel.id))
        .unwrap()
        .into_iter()
        .find(|row| row.text == "Discuss this mounted resource")
        .unwrap();
    if voice_journey {
        let fixture_doc = source.read(Clone::clone).unwrap();
        Box::pin(rooms.author_application(
            &room.room,
            &host.cert,
            &common::catalogue(),
            "so.tilo.chat",
            "",
            |doc| {
                let mut fixture_doc = fixture_doc;
                tilo_automerge::ApplyIncoming::merge_incoming(doc, &mut fixture_doc)
                    .map_err(|error| error.to_string())?;
                Ok(())
            },
        ))
        .await
        .unwrap();
    } else {
        native
            .peer
            .publish(route.clone(), &source.take_mutation().unwrap())
            .unwrap();
    }
    if voice_journey {
        let directory_page = native.catalogue(CatalogueQuery::default()).await.unwrap();
        println!(
            "MOUNTED_RESOURCE_CATALOGUE={}",
            serde_json::json!({"route": route, "requestedProvider": provider,
                "rootProviders": directory_page.providers,
                "rootRows": directory_page.rows.iter().map(|row| serde_json::json!({
                    "provider": row.provider, "app": row.app_id,
                    "resource": row.resource.id, "parent": row.parent_resource
                })).collect::<Vec<_>>()})
        );
    }
    let query = CatalogueQuery {
        provider: Some(provider.clone()),
        view_resource: Some(tilo_actions::remote::ViewResourceQuery {
            id: chat::projection::item_id("messages", &message),
            kind: "chat.workspace.messages".into(),
        }),
        ..CatalogueQuery::default()
    };
    tilo_test_env::within("the source reaches the governed catalogue", async {
        let mut reported = false;
        loop {
            let result = native.catalogue(query.clone()).await;
            if voice_journey && !reported {
                println!(
                    "MOUNTED_RESOURCE_VIEW={}",
                    match &result {
                        Ok(page) => serde_json::json!({"rows": page.rows.len(),
                            "locatorPresent": page.locator.is_some()}),
                        Err(error) => serde_json::json!({"error": format!("{error:?}")}),
                    }
                );
                reported = true;
            }
            if result.is_ok_and(|page| !page.rows.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let voice_channel = chat::projection::read(&doc)
        .channel_rows()
        .into_iter()
        .find(|channel| {
            channel.workspace == workspace && channel.kind == chat::model::ChannelKind::Voice
        })
        .unwrap();
    if voice_journey {
        use tilo_browser_peer::{PeerEvent, VoiceEvent};
        native
            .peer
            .voice_call(
                native.workspace,
                [40; 16],
                tilo_relay_protocol::voice::CallOperation::Start {
                    app_id: route.app_id.clone(),
                    record_id: route.record.clone(),
                    channel_id: voice_channel.id.clone(),
                    allowed: tilo_relay_protocol::voice::Directions {
                        publish: true,
                        subscribe: true,
                    },
                },
            )
            .unwrap();
        tilo_test_env::within("the governor answers the mounted Chat call", async {
            loop {
                for event in native.events.drain() {
                    if let PeerEvent::Voice(VoiceEvent::CallResult {
                        request, result, ..
                    }) = event
                        && request == [40; 16]
                    {
                        assert!(result.expect("exact native call binding").active);
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
    }
    let readers = if voice_journey {
        vec![&recipient, &second_device]
    } else {
        vec![&recipient]
    };
    for recipient in readers {
        let role = if voice_journey || disconnect_retry {
            MemberRole::Editor
        } else {
            MemberRole::Viewer
        };
        admit_person(&hosted, &host, &room.room, recipient, role).await;
    }
    if encrypted_restart {
        let barrier = native.peer.barrier().unwrap();
        barrier.recv().unwrap().unwrap();
        let retained = hosted
            .relay
            .room_records()
            .retained(room.workspace_id)
            .await
            .unwrap();
        let expected =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id, &route.record);
        assert!(retained.iter().any(|body| body.author == publisher.cert.device_public_key &&
            matches!(postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body.body),
                Ok(neighbour_wire::NeighbourMsg::AppDocument {app_id, record_id, envelope, ..})
                if app_id == route.app_id && record_id == route.record &&
                    envelope.encryption.as_ref().is_some_and(|metadata| metadata.channel == expected))),
            "the compiled cold journey requires the foreign writer's actual retained ciphertext");
        native.peer.close();
        hosted.agents.shutdown();
        drop(hosted.web);
        hosted.relay.shutdown().await;
        hosted = start_browser(&alice, &host, state.path(), Some(page)).await;
        println!("MOUNTED_RESOURCE_STAGE={{\"stage\":\"remote-encrypted-source-host-restarted\"}}");
    }
    let rooms = hosted.relay.rooms().unwrap();
    let reader_identity = hex::encode(recipient.cert.user_public_key);
    let decoy = "ab".repeat(32);
    let bootstrap = serde_json::json!({
        "origin": hosted.web.origin, "room": room.room,
        "hostDevice": hex::encode(host.cert.device_public_key), "provider": provider,
        "channel": chat::projection::item_id("channels", &channel.id),
        "disconnectRetry": disconnect_retry, "voiceJourney": voice_journey, "encryptedRestart": encrypted_restart, "sourceAuthor": source_author,
        "sourcePeople": source.read(|doc| chat::projection::read(doc).user_rows().into_iter().map(|person|
            serde_json::json!({"id":person.id,"name":person.name})).collect::<Vec<_>>()).unwrap(),
        "voiceChannel": voice_channel.id, "callRow": format!("{}/call", chat::projection::item_id("channels", &voice_channel.id)),
        "secondDevice": {"seed": second_device.key.to_bytes(),
            "root": hex::encode(second_device.cert.user_public_key),
            "accounts": account_document(&hex::encode(second_device.cert.user_public_key), &hosted.web.origin, "Fixture second reader"),
            "certificate": {"cert": second_device.cert, "hostCert": host.cert}},
        "reference": reference, "shared": chat::projection::item_id("messages", &shared.id),
        "root": reader_identity, "decoy": decoy, "seed": recipient.key.to_bytes(),
        "certificate": {"cert": recipient.cert, "hostCert": host.cert},
        "accounts": account_document(&reader_identity, &hosted.web.origin, "Fixture reader"),
        "decoyAccounts": account_document(&decoy, &hosted.web.origin, "Unadmitted fixture account"),
    });
    input
        .write_all(format!("{bootstrap}\n").as_bytes())
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(150), async {
        while let Some(line) = output.next_line().await.unwrap() {
            let event: serde_json::Value = serde_json::from_str(&line).unwrap();
            if event["stage"] == "browser-offline" {
                source
                    .invoke(
                        &ActionAddress::new(
                            chat::projection::item_id("channels", &channel.id),
                            "post",
                        ),
                        serde_json::json!({"text": "Native fixture while browser disconnected"})
                            .as_object()
                            .unwrap(),
                        false,
                    )
                    .unwrap();
                native
                    .peer
                    .publish(route.clone(), &source.take_mutation().unwrap())
                    .unwrap();
                let durable = native.peer.barrier().unwrap();
                tilo_test_env::within("offline browser fixture's native post is durable", async {
                    loop {
                        match durable.try_recv() {
                            Ok(result) => {
                                result.unwrap();
                                break;
                            }
                            Err(std::sync::mpsc::TryRecvError::Empty) => {
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                                panic!("native durability barrier closed before acknowledgement");
                            }
                        }
                    }
                })
                .await;
                input
                    .write_all(b"{\"nativePublished\":true}\n")
                    .await
                    .unwrap();
                println!("MOUNTED_RESOURCE_STAGE={event}");
            } else if event["stage"] == "browser-recovered" {
                tilo_test_env::within("native replica receives the queued browser post", async {
                    loop {
                        native.pump();
                        for (_, bytes) in native.documents.get(&route).into_iter().flatten() {
                            source
                                .apply(&collab_types::DocumentMutation::Incremental(bytes.clone()))
                                .unwrap();
                        }
                        let messages = source
                            .read(|doc| chat::projection::read(doc).message_rows(&channel.id))
                            .unwrap();
                        let queued = messages
                            .iter()
                            .filter(|message| {
                                message.text == "Browser fixture queued during disconnect"
                            })
                            .count();
                        assert!(queued <= 1, "the original browser action never duplicates");
                        if queued == 1 {
                            assert_eq!(
                                messages
                                    .iter()
                                    .filter(|message| {
                                        message.text == "Native fixture while browser disconnected"
                                    })
                                    .count(),
                                1
                            );
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                })
                .await;
                input
                    .write_all(b"{\"nativeReceivedOnce\":true}\n")
                    .await
                    .unwrap();
                println!("MOUNTED_RESOURCE_STAGE={event}");
            } else if event["stage"] == "revoke" {
                let started = std::time::Instant::now();
                println!("MOUNTED_RESOURCE_REVOCATION={{\"state\":\"requested\"}}");
                let completion = rooms
                    .remove_member(
                        &room.room,
                        &host.cert,
                        RemoveMemberRequest {
                            request_id: [33; 32],
                            user_public_key: bob.verifying_key().to_bytes(),
                        },
                    )
                    .await
                    .unwrap();
                let acknowledgement = serde_json::json!({
                    "revoked": true,
                    "generation": completion.generation,
                    "elapsedMs": started.elapsed().as_millis(),
                });
                println!("MOUNTED_RESOURCE_REVOCATION={acknowledgement}");
                input
                    .write_all(format!("{acknowledgement}\n").as_bytes())
                    .await
                    .unwrap();
            } else {
                println!("MOUNTED_RESOURCE_STAGE={event}");
            }
        }
        child.wait().await.unwrap()
    })
    .await;
    native.peer.close();
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
    assert!(
        result.expect("bounded mounted journey").success(),
        "mounted journey failed"
    );
}
