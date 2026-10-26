//! A developer pushes a build of an app, and a bot's program, into a
//! running room from their own machine (`tilo-app push`): one of their
//! devices dials the room's host by the room's link and invokes the room's
//! `install-app` and `request-bot`, which the room decides like any of its
//! governance. Every member reads the version the room runs from its
//! governance.
use tilo_action_policy::{Answer, Grant, KindSet, Subject, roles, roles::governance};
use tilo_relay_session::rooms::govern::{self, RoomLink};

use super::*;

fn release(
    publisher: [u8; 32],
    version: &str,
    surface: u8,
) -> serde_json::Map<String, serde_json::Value> {
    let serde_json::Value::Object(release) = serde_json::json!({
        "app": "so.tilo.notes",
        "version": version,
        "surface": hex::encode([surface; 32]),
        "package": hex::encode([surface ^ 0xff; 32]),
        "publisher": hex::encode(publisher),
        "dev": true,
    }) else {
        unreachable!("an object")
    };
    release
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_developer_pushes_an_app_version_and_a_bot_program_into_a_running_room() {
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = ada.enrol("ada-laptop", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Lounge").await.unwrap();
    super::topology_namespace::admit(&relay, &host, &laptop, &room.room).await;
    let address = relay.node_addr();
    let link: RoomLink = RoomLink {
        relay: address.id,
        direct: address.ip_addrs().next().copied(),
        workspace_id: room.workspace_id,
        room: room.room.clone(),
    }
    .to_string()
    .parse()
    .unwrap();
    let push = |action: &'static str, argument: serde_json::Map<String, serde_json::Value>| {
        let link = link.clone();
        let (cert, key) = (laptop.cert.clone(), laptop.sk.clone());
        async move {
            let client = govern::connect(&link, cert, key).await?;
            govern::invoke(&client, link.workspace_id, &link.room, action, argument).await
        }
    };

    // An Editor by role alone installs no version of an app.
    let refused = push(
        governance::INSTALL_APP,
        release(laptop.cert.user_public_key, "1.5.0-dev.1", 1),
    )
    .await
    .unwrap_err();
    assert!(refused.contains("no such action"), "{refused}");
    assert!(rooms.app_versions(&room.room).await.unwrap().is_empty());

    // The owner lets Ada install apps: she is the room's developer.
    let who = hex::encode(laptop.cert.user_public_key);
    let grant = |action: &str| {
        Grant::new(
            format!("developer:{action}:{who}"),
            Subject::Person(who.clone()),
            roles::governance_action(&room.room, action),
            KindSet::ALL,
            Answer::Allow,
            Subject::Person(hex::encode(host.cert.user_public_key)),
        )
    };
    rooms
        .put_room_grants(
            &room.room,
            &host.cert,
            [7; 32],
            vec![
                grant(governance::INSTALL_APP),
                grant(governance::INSTALL_BOT),
            ],
        )
        .await
        .unwrap();

    let answer = push(
        governance::INSTALL_APP,
        release(laptop.cert.user_public_key, "1.5.0-dev.1", 1),
    )
    .await
    .unwrap();
    assert_eq!(answer["running"]["release"]["version"], "1.5.0-dev.1");
    assert_eq!(answer["running"]["installed_by"], who);
    push(
        governance::INSTALL_APP,
        release(laptop.cert.user_public_key, "1.5.0-dev.2", 2),
    )
    .await
    .unwrap();
    let running = rooms.app_versions(&room.room).await.unwrap();
    assert_eq!(running.len(), 1);
    assert_eq!(running[0].release.version, "1.5.0-dev.2");
    assert!(running[0].release.dev, "a developer's own build says so");

    // Her bot's program goes the same way: the room keeps it with the
    // request, for its host to run.
    let program = include_str!("../../../../../apps/discord/bridge/program.json");
    let serde_json::Value::Object(bot) = serde_json::json!({
        "entry": "dev.ada.bridge",
        "version": "0.1.0-dev.1",
        "program": program,
    }) else {
        unreachable!("an object")
    };
    // The bridge's program reaches outside the room, which an Editor does
    // not hold: the host would run it as the room's root, so the room
    // refuses it from her.
    let refused = push("request-bot", bot).await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(rooms.bot_requests(&room.room).await.unwrap().is_empty());
    relay.shutdown().await;
}

/// Two versions of the fixture app's surface: the second adds a field.
const V1: &str = "[[resources]]\nkind = \"fixture\"\nlabel = \"Fixture\"\nversion = 1\n\n\
    [[resources.fields]]\nowner = \"fixture\"\npath = \"topic\"\nstored = \"value\"\n\
    title = \"Topic\"\ntype = \"String\"\n";
const V2: &str = "[[resources]]\nkind = \"fixture\"\nlabel = \"Fixture\"\nversion = 1\n\n\
    [[resources.fields]]\nowner = \"fixture\"\npath = \"topic\"\nstored = \"value\"\n\
    title = \"Topic\"\ntype = \"String\"\n\n\
    [[resources.fields]]\nowner = \"fixture\"\npath = \"notes\"\nstored = \"value\"\n\
    title = \"Notes\"\ntype = \"String\"\n";

/// The version Bob's device runs, as its installed app would say: 1 or 2.
static BOB_RUNS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

fn surface_of(version: u8) -> [u8; 32] {
    *blake3::hash(if version == 1 { V1 } else { V2 }.as_bytes()).as_bytes()
}

/// The topic the room's host holds.
async fn hosted_topic(rooms: &tilo_relay::rooms::Rooms, room: &str) -> Option<String> {
    let bytes = rooms.record_document(room, APP, "").await.unwrap()?;
    let doc = AutoCommit::load(&bytes).unwrap();
    doc.get(ROOT, "topic")
        .unwrap()
        .and_then(|(value, _)| value.into_string().ok())
}

/// Ada pushes her v2 into the room while Bob runs v1. Bob's edit made with
/// v1 is held; his device sees the room's version, fetches its package
/// from the room by hash, checks it, installs it, and the same edit, sent
/// again with v2, is in the room.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_follows_the_version_the_room_runs_and_its_held_edit_goes_in() {
    use std::sync::atomic::Ordering;
    let _surfaces = crate::common::install_process_surfaces(
        collab_types::Surfaces::from_source(std::sync::Arc::new(
            collab_types::surface::ContentSurfaces::new(|_, wanted| {
                [V1, V2]
                    .into_iter()
                    .find(|surface| blake3::hash(surface.as_bytes()).to_hex().as_str() == wanted)
                    .map(|surface| surface.as_bytes().to_vec())
            }),
        )),
        |_| Some(surface_of(BOB_RUNS.load(Ordering::SeqCst))),
    );
    BOB_RUNS.store(1, Ordering::SeqCst);
    let alice = TestUser::fresh("Alice");
    let ada = TestUser::fresh("Ada");
    let bob = TestUser::fresh("Bob");
    let host = alice.enrol("alice-home", DeviceCapabilities::host());
    let laptop = ada.enrol("ada-laptop", DeviceCapabilities::member());
    let phone = bob.enrol("bob-phone", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let relay = start(state.path(), &alice, &host).await;
    let rooms = relay.rooms().unwrap();
    let room = rooms.create("Lounge").await.unwrap();
    super::topology_namespace::admit(&relay, &host, &laptop, &room.room).await;
    super::topology_namespace::admit(&relay, &host, &phone, &room.room).await;
    let who = hex::encode(laptop.cert.user_public_key);
    rooms
        .put_room_grants(
            &room.room,
            &host.cert,
            [7; 32],
            vec![Grant::new(
                format!("developer:install-app:{who}"),
                Subject::Person(who.clone()),
                roles::governance_action(&room.room, governance::INSTALL_APP),
                KindSet::ALL,
                Answer::Allow,
                Subject::Person(hex::encode(host.cert.user_public_key)),
            )],
        )
        .await
        .unwrap();

    // Bob's replica, from the room's history; he edits with v1.
    let mut bob_peer = StatePeer::new(
        &room.room,
        host.cert.clone(),
        vec![host.cert.clone()],
        RevocationList::new(),
        SystemTime::now(),
    )
    .unwrap();
    let bob_session = session(&relay, &phone, room.workspace_id).await;
    let mut presence = Vec::new();
    receive_until(
        "Bob is in the room",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| role(peer, phone.device_pubkey()) == Some(collab_types::Role::Editor),
    )
    .await;
    crate::common::wait_until_subscribed(&relay, room.workspace_id, &phone.device_pubkey()).await;
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "topic", "v1").unwrap();
    doc.commit_edit("fixture", "topic");
    let first = bob_peer
        .publication_mut()
        .publish_incremental(APP, "", doc.save_after(&[]), &phone.sk, SystemTime::now())
        .unwrap();
    publish(&bob_session, &first);
    tilo_test_env::within("Bob's v1 edit in the room", async {
        while hosted_topic(&rooms, &room.room).await.as_deref() != Some("v1") {
            tokio::task::yield_now().await;
        }
    })
    .await;

    // Ada hands the room her v2 package and makes it the room's version.
    let package = b"a developer's v2 build of the fixture app".repeat(9000);
    let address = relay.node_addr();
    let link = RoomLink {
        relay: address.id,
        direct: address.ip_addrs().next().copied(),
        workspace_id: room.workspace_id,
        room: room.room.clone(),
    };
    let client = govern::connect(&link, laptop.cert.clone(), laptop.sk.clone())
        .await
        .unwrap();
    let file = govern::upload(&client, room.workspace_id, &room.room, &package)
        .await
        .unwrap();
    let mut v2 = release(laptop.cert.user_public_key, "2.0.0-dev.1", 2);
    v2.insert("app".into(), APP.into());
    v2.insert("surface".into(), hex::encode(surface_of(2)).into());
    v2.insert("file".into(), file.clone().into());
    govern::invoke(
        &client,
        room.workspace_id,
        &room.room,
        governance::INSTALL_APP,
        v2,
    )
    .await
    .unwrap();

    // Bob, still on v1, edits: the room holds it.
    receive_until(
        "Bob reads the room's version",
        &bob_session,
        &mut bob_peer,
        &mut presence,
        |peer, _| peer.governance().app_version(APP).is_some(),
    )
    .await;
    let before = doc.get_heads();
    doc.put(ROOT, "topic", "while behind").unwrap();
    doc.commit_edit("fixture", "topic");
    let held = bob_peer
        .publication_mut()
        .publish_incremental(
            APP,
            "",
            doc.save_after(&before),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&bob_session, &held);

    // His device follows: the room names a file his install lacks.
    let release = bob_peer.governance().app_version(APP).unwrap().release;
    let installed = hex::encode([1_u8 ^ 0xff; 32]);
    let wanted = release
        .file_to_fetch(Some(&installed))
        .expect("Bob's v1 is not the room's version")
        .to_owned();
    assert_eq!(wanted, file);
    bob_session
        .publish_presence(
            postcard::to_stdvec(&NeighbourMsg::RecordContentWant {
                requester: tilo_peer_state::peer_handle(&phone.device_pubkey()),
                app_id: APP.into(),
                cluster_id: room.room.clone(),
                hash: wanted.clone(),
            })
            .unwrap(),
        )
        .unwrap();
    let mut fetched = Vec::new();
    tilo_test_env::within("the room hands Bob the package", async {
        while fetched.len() < package.len() {
            if let Some(Inbound::RecordContent { frame, .. }) = bob_session.recv().await
                && let Ok(NeighbourMsg::RecordContentPiece {
                    offset,
                    bytes,
                    hash,
                    ..
                }) = postcard::from_bytes(&frame)
                && hash == wanted
                && offset == fetched.len() as u64
            {
                fetched.extend_from_slice(&bytes);
            }
        }
    })
    .await;
    release
        .check_file(&fetched)
        .expect("the package is the room's version");
    assert!(release.check_file(b"something else").is_err());
    assert_eq!(
        hosted_topic(&rooms, &room.room).await.as_deref(),
        Some("v1"),
        "an edit made with another version than the room's is held"
    );

    // Installed: his device runs v2, and the same edit goes out again.
    BOB_RUNS.store(2, Ordering::SeqCst);
    let again = bob_peer
        .publication_mut()
        .publish_incremental(
            APP,
            "",
            doc.save_after(&before),
            &phone.sk,
            SystemTime::now(),
        )
        .unwrap();
    publish(&bob_session, &again);
    tilo_test_env::within("Bob's edit made behind is in the room", async {
        while hosted_topic(&rooms, &room.room).await.as_deref() != Some("while behind") {
            tokio::task::yield_now().await;
        }
    })
    .await;
    relay.shutdown().await;
}
