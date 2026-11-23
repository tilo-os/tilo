//! The Discord bridge program mirroring a guild into the workspace's Chat.

use super::*;
use tilo_workspace_service::agents::workspace_resource;

/// The Discord bridge as a workspace installs it.
pub(super) fn discord_bridge() -> installed::InstalledBot {
    installed::InstalledBot {
        installation: "5b8d2278-de54-4bf8-a0b8-8ae4f0587eed".into(),
        entry: "discord-bridge".into(),
        name: "Discord Bridge".into(),
        program: include_str!("../../../../../../apps/discord/bridge/program.json").to_owned(),
        inputs: serde_json::Map::new(),
        installed_at_ms: 1,
        release: String::new(),
    }
}

/// The workspace's Chat categories by name, each with its conversations' names.
async fn chat_layout(workspaces: &Workspaces, workspace: &str) -> Vec<(String, Vec<String>)> {
    let packages = crate::tl_chat::packages().unwrap();
    workspaces
        .with_tl_record(workspace, &packages, crate::tl_chat::viewer(), |chat| {
            let sidebar = crate::tl_chat::sidebar(chat)?;
            let named =
                |category: Option<&tilo_workspace_service::tl_records::runtime::ResourceRef>| {
                    sidebar
                        .channels
                        .iter()
                        .filter(|channel| channel.category.as_ref() == category)
                        .map(|channel| channel.name.clone())
                        .collect::<Vec<_>>()
                };
            Ok(std::iter::once((String::new(), named(None)))
                .chain(
                    sidebar
                        .categories
                        .iter()
                        .map(|category| (category.name.clone(), named(Some(&category.at)))),
                )
                .collect())
        })
        .await
        .unwrap()
}

/// Connect `workspace` to the Discord server the bridge last saw as `goal`, and
/// `chat` holding `general` and `voice`.
async fn bridged_lounge(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert) {
    use tilo_resource::Fields as _;
    let link = discord::DiscordLink {
        guild_id: "1000".into(),
        only_channel: None,
        name: "Fake".into(),
        mirror_channels: false,
        map_roles: false,
    };
    discord::connect(workspaces, workspace, cert, &link)
        .await
        .unwrap();
    let catalogue = AppCatalogue::native();
    // The server as the bridge last saw it: `goal`, and `chat` holding
    // `general` and `voice`.
    workspaces
        .author_application(
            workspace,
            cert,
            &catalogue,
            discord::APP,
            &workspace_resource(&catalogue, discord::APP),
            |doc| {
                let current = ::discord::Guild::read_fields(doc, &automerge::ROOT)
                    .map_err(|error| error.to_string())?;
                let mut seen = current.clone();
                let channel = |id: &str, kind: &str, name: &str, parent: Option<&str>, at: i64| {
                    serde_json::json!({"id": id, "kind": kind, "name": name, "parent": parent, "position": at})
                };
                seen.observe(
                    "2000".into(),
                    serde_json::from_value(serde_json::json!([
                        channel("1103", "text", "goal", None, 0),
                        channel("1100", "category", "chat", None, 1),
                        channel("1101", "text", "general", Some("1100"), 2),
                        channel("1102", "voice", "voice", Some("1100"), 3),
                    ]))
                    .map_err(|error| error.to_string())?,
                    Vec::new(),
                );
                seen.write_changed_fields(
                    &current,
                    doc,
                    &automerge::ROOT,
                    &tilo_resource::Delta::default(),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
}

/// `bridge` as the workspace holds it when it grants the bridge none of `actions`: what a
/// workspace that takes a grant back has installed.
fn granted_none_of(bridge: &installed::InstalledBot, actions: &[&str]) -> installed::InstalledBot {
    let mut program = bridge.shortcut().unwrap();
    program.grant.retain(|rule| {
        !rule
            .node
            .id_at(tilo_action_policy::Level::Action)
            .is_some_and(|action| actions.contains(&action))
    });
    installed::InstalledBot {
        program: String::from_utf8(tilo_automation::program::canonical_bytes(&program).unwrap())
            .unwrap(),
        ..bridge.clone()
    }
}

/// A bridge run the workspace refuses part of leaves the import half done: the
/// category it made stands, the channels it would put in it were refused. The run
/// goes again from the workspace's records and, once the workspace grants it that,
/// finishes the layout, the channels under the server's category as in Discord.
#[tokio::test(flavor = "multi_thread")]
async fn a_bridge_run_the_workspace_refuses_part_of_finishes_the_import_when_it_goes_again() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    bridged_lounge(&workspaces, &workspace, &cert).await;
    let mut runner = member_runner(
        &workspaces,
        &workspace,
        &cert,
        discord_bridge(),
        scratch.path().join("bridge.json"),
    )
    .await;
    assert!(runner.step().await.unwrap().is_empty());

    // A change to the workspace's Chat has the bridge reconcile; the workspace
    // grants it no placing of a channel in a category, and refuses the first.
    let certifier = membership::HostRootCertifier::new(owner_root());
    let placing = ["add_channel", "add_voice_channel", "move_channel"];
    membership::keep_member(
        &workspaces,
        &workspace,
        &cert,
        &certifier,
        &granted_none_of(&discord_bridge(), &placing),
    )
    .await
    .unwrap();
    let owner = hex::encode(cert.user_public_key);
    let who = crate::tl_chat::member(MemberId::person(owner));
    let packages = crate::tl_chat::packages().unwrap();
    workspaces
        .with_tl_record(
            &workspace,
            &packages,
            crate::tl_chat::viewer(),
            move |chat| crate::tl_chat::new_channel(chat, &who, ("welcome", ""), None).map(drop),
        )
        .await
        .unwrap();
    runner.step().await.unwrap();
    let half = chat_layout(&workspaces, &workspace).await;
    assert!(
        half.iter()
            .any(|(name, channels)| name == "chat" && channels.is_empty()),
        "the category stands, empty: {half:?}"
    );

    // The workspace grants it what it lacked; it goes again, from the workspace's
    // records, and finishes.
    membership::keep_member(
        &workspaces,
        &workspace,
        &cert,
        &certifier,
        &discord_bridge(),
    )
    .await
    .unwrap();
    let done = |layout: &[(String, Vec<String>)]| {
        layout.iter().any(|(name, channels)| {
            name == "chat"
                && channels.contains(&"general".to_owned())
                && channels.contains(&"voice".to_owned())
        })
    };
    for _ in 0..40 {
        if done(&chat_layout(&workspaces, &workspace).await) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        runner.step().await.unwrap();
    }
    let layout = chat_layout(&workspaces, &workspace).await;
    assert!(done(&layout), "general and voice under chat: {layout:?}");
    assert_eq!(
        layout.iter().filter(|(name, _)| name == "chat").count(),
        1,
        "one chat category, the one the refused run made: {layout:?}"
    );
    assert!(
        layout[0].1.contains(&"goal".to_owned()),
        "goal stands apart, as in Discord: {layout:?}"
    );
}
