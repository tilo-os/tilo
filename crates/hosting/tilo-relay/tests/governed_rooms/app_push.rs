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
