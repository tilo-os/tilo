//! A member asks the workspace's host to run a record's member-own actions for
//! them: linking their own Discord account by signing in with Discord,
//! whose round trip the host starts and answers to the asking device alone.
use super::*;

/// The Discord connector's workspace record, by its record provider.
fn guild(workspace: &str) -> String {
    resource_provider(&DocumentRoute {
        app_id: "so.tilo.discord".into(),
        cluster_id: workspace.into(),
        document: String::new(),
    })
}

/// `action` on the guild record's own resource, named the way a desktop
/// that holds no connector code names it.
fn on_guild(workspace: &str, action: &str, arguments: serde_json::Value) -> InvokeRequest {
    request(
        &guild(workspace),
        ActionAddress::new("", action),
        match arguments {
            serde_json::Value::Object(arguments) => InvokeArgument::Json(arguments),
            _ => InvokeArgument::None,
        },
    )
}

/// What `browser` gets back for `request`: asked again while the host is still
/// joining, and followed while it has it accepted.
async fn answered(browser: &mut Browser, request: InvokeRequest) -> ReceiptStatus {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = InvocationId::new();
        let receipt = browser.invoke(attempt).await;
        match &receipt.status {
            ReceiptStatus::Rejected {
                error: ActionCallError::Unavailable { .. },
            } if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            _ => return browser.settle(receipt).await.status,
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
    use tilo_peer_state::join::Role;
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Bridge host", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Bridge guest", 1);
    let host = enrol(
        &owner,
        "bridge-node",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let owner_device = enrol(&owner, "owner-browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.server.workspaces().unwrap();
    let workspace = workspaces.create("Bridged workspace").await.unwrap();
    workspaces
        .prepare_application(
            &workspace.workspace,
            &host.cert,
            &crate::common::catalogue(),
            "so.tilo.discord",
            "",
        )
        .await
        .unwrap();
    let mut owner_browser =
        Browser::open_workspace(&hosted, &owner_device, &host, &workspace.workspace).await;
    let connected = answered(
        &mut owner_browser,
        on_guild(
            &workspace.workspace,
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
        &workspace.workspace,
        &guest_device,
        Role::Editor,
    )
    .await;
    let mut visitor = open_admitted_peer(&hosted, &guest_device, &host, &workspace.workspace).await;

    // The workspace's host offers signing in with Discord, as the bridge's bot
    // says it at each start.
    let page = "https://discord.test/oauth2/authorize?client_id=1&scope=identify";
    workspaces
        .author_application(
            &workspace.workspace,
            &host.cert,
            &crate::common::catalogue(),
            "so.tilo.discord",
            "",
            |doc| {
                let mut bot = ui::tilo_resource::Replica::<discord::guild::Guild>::from_doc(
                    std::mem::take(doc),
                )
                .as_bot_for("bridge-install", &hex::encode(host.cert.user_public_key))
                // The bot writes as Discord's: the record's store takes only what its own app
                // publishes.
                .in_app("so.tilo.discord", "Discord");
                let applied = bot
                    .apply(discord::guild::Guild::SET_SIGN_IN.with(page.to_owned()))
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, bot.doc_mut());
                applied
            },
        )
        .await
        .unwrap();

    let deadline = tilo_test_env::hang_deadline();
    let reading = loop {
        let reading = value(
            &answered(
                &mut visitor,
                on_guild(&workspace.workspace, "my-link", serde_json::json!({})),
            )
            .await,
        );
        if reading["connect"].is_string() {
            break reading;
        }
        assert!(
            Instant::now() < deadline,
            "the member is never offered Connect Discord: {reading}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(reading["connected"], true, "{reading}");
    assert!(reading["account"].is_null(), "{reading}");
    let connect = reading["connect"].as_str().unwrap().to_owned();

    let start = value(
        &answered(
            &mut visitor,
            on_guild(&workspace.workspace, &connect, serde_json::json!({})),
        )
        .await,
    );
    let url = start["url"].as_str().unwrap().to_owned();
    assert!(
        url.starts_with(page) && url.contains("&state="),
        "the host answers the consent page with a fresh state: {start}"
    );
    // The bridge's bot reads the governor's copy of the record when the
    // sign-in comes back; the round trip must be there, bound to the member
    // who started it, not only in the host's own copy.
    let guest_person = hex::encode(guest_device.cert.user_public_key);
    let owner_person = hex::encode(owner_device.cert.user_public_key);
    loop {
        let saved = workspaces
            .resource_document(&workspace.workspace, "so.tilo.discord", "")
            .await
            .unwrap()
            .expect("the governor holds the Discord record");
        let mut doc = automerge::AutoCommit::load(&saved).unwrap();
        let governors =
            tilo_workspace_service::agents::opened::<discord::guild::Guild>(&mut doc).unwrap();
        let people: Vec<_> = governors
            .connects
            .iter()
            .map(|connect| connect.person.clone())
            .collect();
        if people.contains(&guest_person) {
            assert!(
                !people.contains(&owner_person),
                "the round trip is the member's own: {people:?}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the governor's copy never took the member's sign-in: {people:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let refused = answered(
        &mut visitor,
        on_guild(
            &workspace.workspace,
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
    hosted.server.shutdown().await;
}

/// What the workspace's governor commits itself (connecting a Discord server
/// to the workspace) reaches the workspace actions the host
/// runs, which read the workspace through a session of the host's own device.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_hosts_workspace_actions_read_what_the_governor_commits() {
    let (owner, _) = tilo_user_identity::UserIdentity::generate("Bridge host", 1);
    let host = enrol(
        &owner,
        "bridge-node",
        tilo_user_identity::DeviceCapabilities::member(),
    );
    let owner_device = enrol(&owner, "owner-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&owner, &host, state.path()).await;
    let workspaces = hosted.server.workspaces().unwrap();
    let workspace = workspaces.create("Bridged workspace").await.unwrap();
    workspaces
        .prepare_application(
            &workspace.workspace,
            &host.cert,
            &crate::common::catalogue(),
            "so.tilo.discord",
            "",
        )
        .await
        .unwrap();
    let mut owner_browser =
        Browser::open_workspace(&hosted, &owner_device, &host, &workspace.workspace).await;
    let reading = value(
        &answered(
            &mut owner_browser,
            on_guild(&workspace.workspace, "my-link", serde_json::json!({})),
        )
        .await,
    );
    assert_eq!(reading["connected"], false, "{reading}");

    // The governor connects the record to a server as its owner, a commit of
    // the host's own.
    let owner_key = hex::encode(host.cert.user_public_key);
    workspaces
        .author_application(
            &workspace.workspace,
            &host.cert,
            &crate::common::catalogue(),
            "so.tilo.discord",
            "",
            |doc| {
                let mut replica = ui::tilo_resource::Replica::<discord::guild::Guild>::from_doc(
                    std::mem::take(doc),
                )
                .as_invoker(&owner_key)
                .in_app("so.tilo.discord", "Discord");
                let applied = replica
                    .apply_confirmed(discord::guild::Guild::CONNECT.with(
                        discord::guild::guild::Connect {
                            guild_id: "1".into(),
                            name: "Tilo".into(),
                            only_channel: None,
                            mirror_channels: Some(false),
                            map_roles: Some(false),
                        },
                    ))
                    .map(drop)
                    .map_err(|error| error.to_string());
                std::mem::swap(doc, replica.doc_mut());
                applied
            },
        )
        .await
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reading = value(
            &answered(
                &mut owner_browser,
                on_guild(&workspace.workspace, "my-link", serde_json::json!({})),
            )
            .await,
        );
        if reading["connected"] == true {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the host's workspace actions never read the governor's commit: {reading}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    owner_browser.peer.close();
    drop(hosted.web);
    hosted.agents.shutdown();
    hosted.server.shutdown().await;
}
