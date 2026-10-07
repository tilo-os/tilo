//! Mounted acceptance fixture, deliberately separate from the native test suite.
use super::{
    ActionAddress, Browser, CatalogueQuery, DeviceCapabilities, DocumentRoute, Duration, Path,
    Replica, RevocationList, Seed, common, enrol, record_provider, start_browser,
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
    use chat::doc::{ChatDoc, NewMessage};
    use tilo_peer_state::join::{MemberRole, decode_and_verify_invitation};
    use tilo_relay::rooms::{IssueInvitationRequest, RemoveMemberRequest};
    let bundle = std::env::args()
        .nth(1)
        .expect("compiled web bundle path is required");
    assert!(
        Path::new(&bundle)
            .join("pkg/tilo_browser_runtime_bg.wasm")
            .is_file()
    );
    let mut options = std::env::args().skip(2);
    let disconnect_retry = match options.next().as_deref() {
        None => false,
        Some("--disconnect-retry") => true,
        Some(_) => panic!("expected --disconnect-retry as the optional mounted-resource argument"),
    };
    assert!(
        options.next().is_none(),
        "unexpected mounted-resource argument"
    );
    let driver = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/mounted_resource/driver.mjs");
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
    let publisher = enrol(&alice, "fixture-publisher", DeviceCapabilities::primary());
    let recipient = enrol(&bob, "fixture-reader", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start_browser(&alice, &host, state.path(), Some(page)).await;
    let rooms = hosted.relay.rooms().unwrap();
    let room = rooms.create("Mounted resource acceptance").await.unwrap();
    let mut native = Browser::open_room(&hosted, &publisher, &host, &room.room).await;
    let invitation = rooms
        .issue(
            &room.room,
            &host.cert,
            IssueInvitationRequest {
                request_id: [31; 32],
                role: if disconnect_retry {
                    MemberRole::Editor
                } else {
                    MemberRole::Viewer
                },
                recipient_user_public_key: Some(bob.verifying_key().to_bytes()),
                invitee_name: Some("Fixture reader".into()),
                inviter_name: None,
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(600_000_000_000)
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    let verified = decode_and_verify_invitation(
        &invitation.signed_invitation,
        None,
        &RevocationList::new(),
        tilo_clock::unix_now_ns(),
    )
    .unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&recipient.key.to_bytes()))
        .bind()
        .await
        .unwrap();
    tilo_relay::workspace_join::WorkspaceJoinClient::new(endpoint)
        .redeem_hint(
            verified.grant().transport.first().unwrap(),
            invitation.signed_invitation.clone(),
            recipient.cert.clone(),
            &recipient.key,
            [32; 32],
        )
        .await
        .unwrap();
    let mut source = Replica::open(
        &common::catalogue(),
        "so.tilo.chat",
        &room.room,
        "Fixture owner",
        false,
        Seed::New,
    )
    .unwrap();
    let mut doc = source.read(Clone::clone).unwrap();
    ChatDoc::create_workspace(&mut doc, "Mounted shared space");
    let channel = ChatDoc::list_channels(&doc)
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Text)
        .unwrap();
    let message = ChatDoc::push_message(
        &mut doc,
        &channel.id,
        NewMessage {
            author: "Fixture owner".into(),
            text: "Hello é, mounted natively".into(),
            ..NewMessage::default()
        },
    );
    source
        .apply(&collab_types::DocumentMutation::Snapshot(doc.save()))
        .unwrap();
    let reference = serde_json::json!({"id": format!("chat.message:{message}"),
        "kind": "chat.message", "label": "Fixture selection", "span": {"start": 6, "end": 7, "unit": "Chars"}});
    let route = DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: room.room.clone(),
        record: if source.record() == "@tilo:package-primary" {
            String::new()
        } else {
            source.record().into()
        },
    };
    let provider = record_provider(&route);
    source
        .invoke(
            &ActionAddress::new(format!("chat.channel:{}", channel.id), "share-resource"),
            serde_json::json!({"reference": reference, "sourceProvider": provider,
            "text": "Discuss this mounted resource"})
            .as_object()
            .unwrap(),
            false,
        )
        .unwrap();
    let shared = source
        .read(|doc| ChatDoc::list_messages(doc, &channel.id))
        .unwrap()
        .into_iter()
        .find(|row| row.text == "Discuss this mounted resource")
        .unwrap();
    native
        .peer
        .publish(route.clone(), &source.take_mutation().unwrap())
        .unwrap();
    let query = CatalogueQuery {
        provider: Some(provider.clone()),
        view_resource: Some(tilo_actions::remote::ViewResourceQuery {
            id: format!("chat.message:{message}"),
            kind: "chat.message".into(),
        }),
        ..CatalogueQuery::default()
    };
    tilo_test_env::within("the source reaches the governed catalogue", async {
        loop {
            if native
                .catalogue(query.clone())
                .await
                .is_ok_and(|page| !page.rows.is_empty())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let reader_identity = hex::encode(recipient.cert.user_public_key);
    let decoy = "ab".repeat(32);
    let bootstrap = serde_json::json!({
        "origin": hosted.web.origin, "room": room.room,
        "hostDevice": hex::encode(host.cert.device_public_key), "provider": provider,
        "channel": format!("chat.channel:{}", channel.id),
        "disconnectRetry": disconnect_retry,
        "reference": reference, "shared": format!("chat.message:{}", shared.id),
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
                        &ActionAddress::new(format!("chat.channel:{}", channel.id), "post"),
                        serde_json::json!({"value": "Native fixture while browser disconnected"})
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
                            .read(|doc| ChatDoc::list_messages(doc, &channel.id))
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
                rooms
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
                input.write_all(b"{\"revoked\":true}\n").await.unwrap();
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
