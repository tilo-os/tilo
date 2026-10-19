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
        arguments
            .as_object()
            .cloned()
            .map_or(InvokeArgument::None, InvokeArgument::Json),
    )
}

/// What `browser` gets back for `request`, waiting out a host still joining.
async fn answered(browser: &mut Browser, request: InvokeRequest) -> ReceiptStatus {
    let deadline = Instant::now().checked_add(Duration::from_secs(30)).unwrap();
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
