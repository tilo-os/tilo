//! Turning the workspace's moderation bot on and off: a person's decision, and
//! the grant it reaches.

use super::{ADA, FakeJudge, MALLORY, MemberId, ModerationAgent, ModerationBot, bot_server};
use crate::tl_chat;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{governor, restricted_until};
use tilo_action_policy::bot::{MODERATED_APP, MODERATION};
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Party, TreePath};
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::tl_records::runtime::Actor;

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
        .app(MODERATED_APP)
        .action("delete")
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

fn person(key: [u8; 32]) -> Actor {
    tl_chat::member(MemberId::person(hex::encode(key)))
}

/// Ada says hello in `#general`, Mallory floods it with the same link three times, and Ada
/// reports one of her posts: each through Chat's own actions, as they would from their Chat.
async fn flood(workspaces: &Workspaces, workspace: &str, owner: &DeviceCert) {
    let owner = person(owner.user_public_key);
    workspaces
        .with_tl_record(
            workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            move |chat| {
                let general = tl_chat::new_channel(chat, &owner, ("general", ""), None)?;
                tl_chat::send(chat, &person(ADA), &general, "hello everyone", None)?;
                let mut spam = Vec::new();
                for _ in 0..3 {
                    let post = "buy at spam.example";
                    spam.push(tl_chat::send(chat, &person(MALLORY), &general, post, None)?);
                }
                let reported = spam.get(1).ok_or_else(|| "Mallory posted".to_owned())?;
                tl_chat::report(chat, &person(ADA), &general, reported, "spam").map(drop)
            },
        )
        .await
        .unwrap();
}

/// What `who` wrote in the workspace's Chat, as the workspace holds it now.
async fn texts_by(workspaces: &Workspaces, workspace: &str, who: [u8; 32]) -> Vec<String> {
    workspaces
        .with_tl_record(
            workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            move |chat| {
                Ok(tl_chat::all_messages(chat)?
                    .into_iter()
                    .filter(|(_, message)| message.who == Some(person(who)))
                    .map(|(_, message)| message.text)
                    .collect())
            },
        )
        .await
        .unwrap()
}

/// The workspace's open reports, as the moderation agent reads them.
async fn open_reports(workspaces: &Workspaces, workspace: &str) -> usize {
    workspaces
        .with_tl_record(
            workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            |chat| Ok(crate::moderation::moderation(chat)?.open_reports().count()),
        )
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_filed_in_the_workspace_restricts_and_takes_the_flood_back_as_the_bot() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    flood(&workspaces, &workspace, &cert).await;

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
        texts_by(&workspaces, &workspace, MALLORY).await.len(),
        3,
        "the flood stands while the bot is off"
    );
    assert!(bot.looked.is_empty(), "the bot looked at nothing");
    assert_eq!(open_reports(&workspaces, &workspace).await, 1);

    // A person who governs the workspace turns it on, and it answers the
    // report left open.
    turn_moderation(&workspaces, &workspace, &cert, true).await;
    bot.wake().await.unwrap();

    assert!(
        texts_by(&workspaces, &workspace, MALLORY).await.is_empty(),
        "the flood is taken back"
    );
    assert_eq!(
        texts_by(&workspaces, &workspace, ADA).await,
        ["hello everyone"]
    );
    assert_eq!(
        open_reports(&workspaces, &workspace).await,
        0,
        "the report is settled"
    );
    assert!(
        restricted_until(&workspaces, &workspace, &cert, MALLORY)
            .await
            .is_some(),
        "Mallory is restricted through the workspace's governance"
    );
    assert_eq!(bot.looked.len(), 1);
    // Its own changes leave nothing new to look at.
    bot.wake().await.unwrap();
    assert_eq!(bot.looked.len(), 1);
}
