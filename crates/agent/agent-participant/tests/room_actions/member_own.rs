//! A member asks the room's host to run a record's member-own actions for
//! them: linking their own Discord account, whose code the host makes,
//! keeps only the hash of, and answers to the asking device alone.
use super::*;

/// The Discord connector's room record, by its record provider.
fn guild(room: &str) -> String {
    record_provider(&DocumentRoute {
        app_id: "so.tilo.discord".into(),
        cluster_id: room.into(),
        record: String::new(),
    })
}

/// `action` on the guild record's own resource, named the way a desktop
/// that holds no connector code names it.
fn on_guild(room: &str, action: &str, arguments: serde_json::Value) -> InvokeRequest {
    request(
        &guild(room),
        ActionAddress::new("", action),
        match arguments {
            serde_json::Value::Object(arguments) => InvokeArgument::Json(arguments),
            _ => InvokeArgument::None,
        },
    )
}

/// What `browser` gets back for `request`, waiting out a host still joining.
async fn answered(browser: &mut Browser, request: InvokeRequest) -> ReceiptStatus {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = InvocationId::new();
        match browser.invoke(attempt).await.status {
            ReceiptStatus::Rejected {
                error: ActionHostError::Unavailable { .. },
            } if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            status => return status,
        }
    }
}

fn value(status: &ReceiptStatus) -> serde_json::Value {
    match status {
        ReceiptStatus::Completed { output } => output.value.clone().unwrap_or_default(),
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_links_their_discord_account_through_the_host() {
    use super::resource_support::{admit_person, open_admitted_peer};
    use tilo_peer_state::join::MemberRole;
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Bridge host", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Bridge guest", 1);
    let host = enrol(
        &owner,
        "bridge-node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let owner_device = enrol(&owner, "owner-browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&owner, &host, state.path()).await;
    let rooms = hosted.relay.rooms().unwrap();
    let room = rooms.create("Bridged room").await.unwrap();
    rooms
        .prepare_application(
            &room.room,
            &host.cert,
            &common::catalogue(),
            "so.tilo.discord",
            "",
        )
        .await
        .unwrap();
    let mut owner_browser = Browser::open_room(&hosted, &owner_device, &host, &room.room).await;
    let connected = answered(
        &mut owner_browser,
        on_guild(
            &room.room,
            "connect",
            serde_json::json!({"guild_id": "1", "name": "Tilo"}),
        ),
    )
    .await;
    assert!(
        matches!(connected, ReceiptStatus::Completed { .. }),
        "the host's own person connects the server: {connected:?}"
    );
    admit_person(
        &hosted,
        &host,
        &room.room,
        &guest_device,
        MemberRole::Editor,
    )
    .await;
    let mut visitor = open_admitted_peer(&hosted, &guest_device, &host, &room.room).await;

    let reading = value(
        &answered(
            &mut visitor,
            on_guild(&room.room, "my-link", serde_json::json!({})),
        )
        .await,
    );
    assert_eq!(reading["connected"], true, "{reading}");
    assert!(reading["account"].is_null(), "{reading}");
    assert_eq!(reading["link"], "link-account");

    let code = value(
        &answered(
            &mut visitor,
            on_guild(&room.room, "link-account", serde_json::json!({})),
        )
        .await,
    );
    let code = code["code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 12, "the host made a code for the asking device");

    let reading = value(
        &answered(
            &mut visitor,
            on_guild(&room.room, "my-link", serde_json::json!({})),
        )
        .await,
    );
    assert_eq!(
        reading["pending"], true,
        "the record holds the member's pending link: {reading}"
    );
    // The bridge's bot reads the governor's copy of the record when `/link`
    // arrives; the pending link must be there, not only in the host's own.
    let deadline = tilo_test_env::hang_deadline();
    let guest_person = hex::encode(guest_device.cert.user_public_key);
    loop {
        let saved = rooms
            .record_document(&room.room, "so.tilo.discord", "")
            .await
            .unwrap()
            .expect("the governor holds the Discord record");
        let mut doc = automerge::AutoCommit::load(&saved).unwrap();
        let governors = tilo_relay::room_bots::opened::<discord::record::Guild>(&mut doc).unwrap();
        if governors
            .pending_links
            .iter()
            .any(|pending| pending.person == guest_person)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the governor's copy never took the member's pending link: {:?}",
            governors.pending_links.iter().collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let owners_view = value(
        &answered(
            &mut owner_browser,
            on_guild(&room.room, "my-link", serde_json::json!({})),
        )
        .await,
    );
    assert_eq!(
        owners_view["pending"], false,
        "the pending link is the member's own: {owners_view}"
    );

    let refused = answered(
        &mut visitor,
        on_guild(
            &room.room,
            "connect",
            serde_json::json!({"guild_id": "2", "name": "Other"}),
        ),
    )
    .await;
    assert!(
        !matches!(refused, ReceiptStatus::Completed { .. }),
        "a member runs only the actions offered to every member: {refused:?}"
    );

    visitor.peer.close();
    owner_browser.peer.close();
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}

/// What the room's governor commits itself (connecting the bridge, a bot
/// completing a link a member started) reaches the room actions the host
/// runs, which read the room through a session of the host's own device.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_room_actions_read_what_the_governor_commits() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Bridge host", 1);
    let host = enrol(
        &owner,
        "bridge-node",
        tilo_user_identity::cloud_enrollment::host_capabilities(),
    );
    let owner_device = enrol(&owner, "owner-browser", DeviceCapabilities::member());
    let state = tempfile::tempdir().unwrap();
    let hosted = start(&owner, &host, state.path()).await;
    let rooms = hosted.relay.rooms().unwrap();
    let room = rooms.create("Bridged room").await.unwrap();
    rooms
        .prepare_application(
            &room.room,
            &host.cert,
            &common::catalogue(),
            "so.tilo.discord",
            "",
        )
        .await
        .unwrap();
    let mut owner_browser = Browser::open_room(&hosted, &owner_device, &host, &room.room).await;
    let reading = value(
        &answered(
            &mut owner_browser,
            on_guild(&room.room, "my-link", serde_json::json!({})),
        )
        .await,
    );
    assert_eq!(reading["connected"], false, "{reading}");

    tilo_relay::room_bots::discord::connect(
        rooms,
        &room.room,
        &host.cert,
        &tilo_relay::room_bots::discord::DiscordLink {
            guild_id: "1".into(),
            only_channel: None,
            name: "Tilo".into(),
            mirror_channels: false,
            map_roles: false,
        },
    )
    .await
    .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reading = value(
            &answered(
                &mut owner_browser,
                on_guild(&room.room, "my-link", serde_json::json!({})),
            )
            .await,
        );
        if reading["connected"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the host's room actions never read the governor's commit: {reading}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    owner_browser.peer.close();
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}
