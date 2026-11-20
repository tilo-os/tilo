//! Turning the workspace's moderation bot on and off: a person's decision, and
//! the grant it reaches.

use super::messages::{chat_of, posts, texts_by};
use super::{
    ADA, AppCatalogue, CHAT, FakeJudge, MALLORY, MemberId, ModerationAgent, ModerationBot, Outcome,
    bot_server, flood, flood_posts,
};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{governor, restricted_until};
use tilo_action_policy::bot::MODERATION;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Party, TreePath};
use tilo_user_identity::DeviceCert;

/// The bot's answer for an undoable change at `node` in `workspace`.
async fn bot_answer(
    workspaces: &crate::workspaces::Workspaces,
    workspace: &str,
    node: &TreePath,
) -> Answer {
    let grants = workspaces.workspace_grants(workspace).await.unwrap();
    let party = Party::new(vec![Grantee::bot(MODERATION)], grants.iter());
    party
        .decide(node, ActionKind::Undoable, &Context::at_secs(2000))
        .answer
}

fn chat_node(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app(tilo_app_id::apps::CHAT)
        .kind("chat.messages")
        .resource(collab_types::document::chain_resource_id(
            &chat::parts::chain(),
        ))
        .action("post")
}

fn outside_node(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app("so.tilo.tasks")
        .kind("tasks.issues")
        .resource("chain:issues")
        .action("create")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_turns_the_moderation_bot_on_in_chat_only_and_off_again() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;

    // No person enabled it: it holds nothing, so its changes are refused.
    assert!(
        !crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap(),
        "a workspace nobody enabled has no moderation bot"
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &chat_node(&workspace)).await,
        Answer::Deny
    );

    // The person turns it on: Chat, and nothing outside Chat.
    turn_moderation(&workspaces, &workspace, &cert, true).await;
    assert!(
        crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap()
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &chat_node(&workspace)).await,
        Answer::Allow
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &outside_node(&workspace)).await,
        Answer::Deny
    );

    // And off again: the grant is gone, so its changes are refused.
    turn_moderation(&workspaces, &workspace, &cert, false).await;
    assert!(
        !crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap()
    );
    assert_eq!(
        bot_answer(&workspaces, &workspace, &chat_node(&workspace)).await,
        Answer::Deny
    );
}

/// Turn the workspace's moderation agent on or off as the person `cert`
/// certifies: write or take back its grant in the workspace's governance.
async fn turn_moderation(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert, on: bool) {
    let request_id = *blake3::hash(format!("moderation {on}").as_bytes()).as_bytes();
    if on {
        let person = hex::encode(cert.user_public_key);
        let grant = tilo_action_policy::bot::moderation_grant(workspace, &person);
        workspaces
            .put_workspace_grants(workspace, cert, request_id, vec![grant])
            .await
            .unwrap();
    } else {
        let id = tilo_action_policy::GrantId::new(tilo_action_policy::bot::MODERATION_GRANT);
        workspaces
            .remove_workspace_grants(workspace, cert, request_id, vec![id])
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_filed_in_the_workspace_restricts_and_undoes_the_flood_as_the_bot() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    let catalogue = AppCatalogue::native();
    workspaces
        .prepare_application(&workspace, &cert, &catalogue, CHAT, "")
        .await
        .unwrap();
    let general = workspaces
        .author_application(&workspace, &cert, &catalogue, CHAT, "", |doc| {
            flood(doc).map_err(|error| error.to_string())
        })
        .await
        .unwrap();
    flood_posts(&workspaces, &workspace, &cert, &general).await;

    let mut bot = ModerationBot::new(
        workspaces.clone(),
        workspace.clone(),
        cert.clone(),
        ModerationAgent::new(FakeJudge),
    );
    // A grant id alone carries no authority. A grant with the same name and
    // scope that only permits reading must not start the bot.
    let mut read_only =
        tilo_action_policy::bot::moderation_grant(&workspace, &hex::encode(cert.user_public_key));
    read_only.kinds = tilo_action_policy::KindSet::READ;
    workspaces
        .put_workspace_grants(&workspace, &cert, [77; 32], vec![read_only])
        .await
        .unwrap();
    crate::workspace_bots::enable(&workspaces, &workspace)
        .await
        .unwrap();
    assert!(
        !crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap()
    );
    // Nobody turned it on: it looks at nothing and the report stays open.
    bot.wake().await.unwrap();
    assert_eq!(
        texts_by(&posts(&workspaces, &workspace).await, MALLORY).len(),
        3,
        "the flood stands while the bot is off"
    );
    assert_eq!(bot.cursor, None, "the bot read nothing");

    // A person who governs the workspace turns it on, and it answers the
    // report left open.
    turn_moderation(&workspaces, &workspace, &cert, true).await;
    bot.wake().await.unwrap();

    let posted = posts(&workspaces, &workspace).await;
    assert!(
        texts_by(&posted, MALLORY).is_empty(),
        "the flood is taken back"
    );
    assert_eq!(texts_by(&posted, ADA), ["hello everyone"]);
    let after = chat_of(
        &workspaces
            .resource_document(&workspace, CHAT, "")
            .await
            .unwrap()
            .unwrap(),
    );
    let decision = after.moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.person, MemberId::person(hex::encode(MALLORY)));
    assert_eq!(decision.outcome, Outcome::Restrict);
    assert_eq!(
        after.moderation.open_reports().count(),
        0,
        "the report is settled"
    );
    assert!(
        restricted_until(&workspaces, &workspace, &cert, MALLORY)
            .await
            .is_some(),
        "Mallory is restricted through the workspace's governance"
    );
    // The bot's own changes file no report, so they do not wake it again.
    bot.wake().await.unwrap();
    let after_second_wake = chat_of(
        &workspaces
            .resource_document(&workspace, CHAT, "")
            .await
            .unwrap()
            .unwrap(),
    );
    assert_eq!(after_second_wake.moderation, after.moderation);
}
