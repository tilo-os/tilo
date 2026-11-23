//! Turning the workspace's moderation bot on and off: a person's decision, and
//! the grant it reaches.

use super::{
    ADA, FakeJudge, MALLORY, MemberId, ModerationAgent, ModerationBot, Outcome, bot_server,
};
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
/// certifies: write or take back its grants in the workspace's governance.
async fn turn_moderation(workspaces: &Workspaces, workspace: &str, cert: &DeviceCert, on: bool) {
    let request_id = *blake3::hash(format!("moderation {on}").as_bytes()).as_bytes();
    let person = hex::encode(cert.user_public_key);
    let grants = tilo_action_policy::bot::moderation_grants(workspace, &person);
    if on {
        workspaces
            .put_workspace_grants(workspace, cert, request_id, grants)
            .await
            .unwrap();
    } else {
        let ids: Vec<_> = grants.into_iter().map(|grant| grant.id).collect();
        workspaces
            .remove_workspace_grants(workspace, cert, request_id, ids)
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

/// The node a person posts in Chat at: the app's `send` action, an undoable change.
fn post_node(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .app(MODERATED_APP)
        .action("send")
}

/// Whether `person` may post in the workspace's Chat now, as the workspace's policy decides it
/// for a person's own gesture: their role's rules and their own grants, held off by a ban or a
/// restriction.
async fn may_post(workspaces: &Workspaces, workspace: &str, person: [u8; 32]) -> Answer {
    let policy = workspaces
        .policy_of(workspace)
        .await
        .expect("the workspace's policy is here");
    policy
        .person_on_device(&hex::encode(person), false)
        .decide(
            &post_node(workspace),
            ActionKind::Undoable,
            &Context::at_time(tilo_clock::wall()),
        )
        .answer
}

/// Whether the workspace keeps `person` out now: it holds the `block:<person>` Deny a ban
/// writes ([`tilo_action_policy::roles::block_grant_id`]).
async fn banned(workspaces: &Workspaces, workspace: &str, person: [u8; 32]) -> bool {
    let id = tilo_action_policy::roles::block_grant_id(&hex::encode(person));
    workspaces
        .workspace_grants(workspace)
        .await
        .unwrap()
        .iter()
        .any(|grant| grant.id == id)
}

/// Nobody turned the bot on: a grant under its name that only permits reading does not start
/// it, and it looks at nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_grant_does_not_turn_the_bot_on() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    flood(&workspaces, &workspace, &cert).await;

    let mut bot = ModerationBot::new(
        workspaces.clone(),
        workspace.clone(),
        ModerationAgent::new(FakeJudge(Outcome::Restrict)),
    );
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
    bot.wake().await.unwrap();
    assert_eq!(
        texts_by(&workspaces, &workspace, MALLORY).await.len(),
        3,
        "the flood stands while the bot is off"
    );
    assert!(bot.looked.is_empty(), "the bot looked at nothing");
    assert_eq!(open_reports(&workspaces, &workspace).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_restricts_a_flooder_and_their_post_is_refused() {
    assert_bot_enforces(Outcome::Restrict).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_bans_a_flooder_and_they_are_out() {
    assert_bot_enforces(Outcome::Ban).await;
}

/// A person who governs the workspace turns the moderation agent on, and the bot, under the
/// grants that turn gives it, carries its verdict out: the flooder it answers about is
/// restricted from posting, or banned from the workspace, as history names the bot.
async fn assert_bot_enforces(outcome: Outcome) {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    flood(&workspaces, &workspace, &cert).await;
    super::introduce(&workspaces, &workspace, &cert, &hex::encode(MALLORY)).await;

    let mut bot = ModerationBot::new(
        workspaces.clone(),
        workspace.clone(),
        ModerationAgent::new(FakeJudge(outcome)),
    );
    turn_moderation(&workspaces, &workspace, &cert, true).await;
    assert!(
        crate::workspace_bots::enabled(&workspaces, &workspace)
            .await
            .unwrap()
    );
    assert_eq!(
        may_post(&workspaces, &workspace, MALLORY).await,
        Answer::Allow,
        "before the verdict, the member posts"
    );

    bot.wake().await.unwrap();

    // The flood is taken back and the report settled.
    assert_eq!(texts_by(&workspaces, &workspace, MALLORY).await.len(), 0);
    assert_eq!(
        texts_by(&workspaces, &workspace, ADA).await,
        ["hello everyone"]
    );
    assert_eq!(open_reports(&workspaces, &workspace).await, 0);
    let settled = decisions(&workspaces, &workspace).await;
    let [decision] = settled.as_slice() else {
        panic!("one decision on the one report, not {settled:?}")
    };
    // The grant carrying the verdict names the bot as the one who gave it, so history and the
    // workspace's governance record who did it.
    let grants = workspaces.workspace_grants(&workspace).await.unwrap();
    let mallory = hex::encode(MALLORY);
    let answer = match outcome {
        Outcome::Restrict => {
            assert!(decision.starts_with("Restricted until"), "{decision}");
            assert!(
                restricted_until(&workspaces, &workspace, &cert, MALLORY)
                    .await
                    .is_some(),
                "the member is restricted now"
            );
            assert!(!banned(&workspaces, &workspace, MALLORY).await);
            let id = tilo_action_policy::roles::restriction_grant_id(&mallory);
            grants.iter().find(|grant| grant.id == id).cloned()
        }
        Outcome::Ban => {
            assert!(decision.starts_with("Banned"), "{decision}");
            assert!(banned(&workspaces, &workspace, MALLORY).await);
            assert_eq!(
                restricted_until(&workspaces, &workspace, &cert, MALLORY).await,
                None
            );
            assert!(
                workspaces
                    .summary(&workspace)
                    .await
                    .unwrap()
                    .members
                    .iter()
                    .all(|member| member.id.person_id() != Some(mallory.as_str())
                        || member.removed),
                "the banned member is out of the workspace"
            );
            let id = tilo_action_policy::roles::block_grant_id(&mallory);
            grants.iter().find(|grant| grant.id == id).cloned()
        }
        Outcome::NoAction => unreachable!("the fake judge restricts or bans"),
    }
    .unwrap_or_else(|| panic!("the verdict's grant is in the workspace's governance"));
    assert_eq!(
        answer.granted_by,
        Grantee::bot(MODERATION),
        "the bot is named as the one who did it"
    );
    // And the member cannot post after it.
    assert_eq!(
        may_post(&workspaces, &workspace, MALLORY).await,
        Answer::Deny
    );
}

/// Turned off, the bot has no grants: the same change it makes when on is refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_bot_without_its_governance_grant_cannot_restrict() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = bot_server(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    super::introduce(&workspaces, &workspace, &cert, &hex::encode(MALLORY)).await;
    let change = tilo_action_policy::Change::Restrict {
        person: hex::encode(MALLORY),
        until_ms: u64::try_from(tilo_clock::unix_now_ms_i64()).unwrap() + 60_000,
    };
    assert!(
        workspaces
            .moderate_member(&workspace, MODERATION, change.clone())
            .await
            .is_err(),
        "a bot nobody enabled holds no governance"
    );
    assert_eq!(
        restricted_until(&workspaces, &workspace, &cert, MALLORY).await,
        None
    );

    turn_moderation(&workspaces, &workspace, &cert, true).await;
    workspaces
        .moderate_member(&workspace, MODERATION, change)
        .await
        .unwrap();
    assert!(
        restricted_until(&workspaces, &workspace, &cert, MALLORY)
            .await
            .is_some()
    );
}

/// The decisions written on the workspace's reports, as Chat holds them.
async fn decisions(workspaces: &Workspaces, workspace: &str) -> Vec<String> {
    workspaces
        .with_tl_record(
            workspace,
            &tl_chat::packages().unwrap(),
            tl_chat::viewer(),
            |chat| {
                Ok(tl_chat::reports(chat)?
                    .into_iter()
                    .map(|report| report.decision)
                    .filter(|decision| !decision.is_empty())
                    .collect())
            },
        )
        .await
        .unwrap()
}
