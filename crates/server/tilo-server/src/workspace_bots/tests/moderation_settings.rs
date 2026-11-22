//! Turning the workspace's moderation bot on and off: a person's decision, and
//! the grant it reaches.

use super::messages::{posts, texts_by};
use super::{
    ADA, AppCatalogue, CHAT, FakeJudge, MALLORY, MemberId, ModerationAgent, ModerationBot, Outcome,
    bot_server, flood, flood_posts,
};
use super::{MODERATION_DOCUMENT, Moderation, owner_root};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{governor, restricted_until};
use tilo_action_policy::bot::MODERATION;
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Party, TreePath};
use tilo_resource::Resource as _;
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
async fn a_bot_declines_a_restriction_and_leaves_the_roles_untouched() {
    assert_membership_verdict_declined(Outcome::Restrict).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bot_declines_a_ban_and_leaves_the_roles_untouched() {
    assert_membership_verdict_declined(Outcome::Ban).await;
}

#[expect(
    clippy::future_not_send,
    reason = "the test reads its records as local replicas on the test's own thread"
)]
async fn assert_membership_verdict_declined(outcome: Outcome) {
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
        owner_root(),
        ModerationAgent::new(FakeJudge(outcome)),
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
    bot.keep_bot().await.unwrap();
    let grants = workspaces.workspace_grants(&workspace).await.unwrap();
    let summary = workspaces.summary(&workspace).await.unwrap();
    bot.wake().await.unwrap();

    let posted = posts(&workspaces, &workspace).await;
    assert_eq!(texts_by(&posted, MALLORY).len(), 3);
    assert_eq!(texts_by(&posted, ADA), ["hello everyone"]);
    let after = moderation_state(&workspaces, &workspace).await;
    let decision = after.decisions.iter().next().unwrap();
    assert_eq!(decision.person, MemberId::person(hex::encode(MALLORY)));
    assert_eq!(decision.outcome, Outcome::NoAction);
    assert_eq!(decision.model, "fake-model");
    assert!(
        decision
            .reasoning
            .contains("Workspace membership changes require a person with moderation authority")
    );
    assert!(decision.reverted.is_empty());
    assert_eq!(decision.by, MemberId::bot(MODERATION));
    let installed = workspaces
        .installed_bot(&workspace, MODERATION)
        .await
        .unwrap()
        .unwrap();
    assert!(installed.acting);
    assert_eq!(after.open_reports().count(), 0, "the report is settled");
    assert_eq!(
        restricted_until(&workspaces, &workspace, &cert, MALLORY).await,
        None
    );
    assert_eq!(
        workspaces.workspace_grants(&workspace).await.unwrap(),
        grants
    );
    let after_summary = workspaces.summary(&workspace).await.unwrap();
    assert_eq!(after_summary.members, summary.members);
    assert_eq!(after_summary.devices, summary.devices);
    // The bot's own changes file no report, so they do not wake it again.
    bot.wake().await.unwrap();
    let after_second_wake = moderation_state(&workspaces, &workspace).await;
    assert_eq!(after_second_wake, after);
}

async fn moderation_state(workspaces: &Workspaces, workspace: &str) -> Moderation {
    let bytes = workspaces
        .resource_document(workspace, CHAT, MODERATION_DOCUMENT)
        .await
        .unwrap()
        .unwrap();
    Moderation::read(&automerge::AutoCommit::load(&bytes).unwrap()).unwrap()
}

#[tokio::test]
async fn chat_moderation_history_is_copied_once_with_its_attribution() {
    Box::pin(assert_moderation_history_restored(false)).await;
}

#[tokio::test]
async fn sealed_moderation_history_waits_for_keys_without_founding_an_empty_record() {
    Box::pin(assert_moderation_history_restored(true)).await;
}

#[expect(
    clippy::future_not_send,
    reason = "the test reads its records as local replicas on the test's own thread"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one walk: the earlier history, its move, and the record each reader opens"
)]
async fn assert_moderation_history_restored(sealed: bool) {
    use crate::workspaces::fixtures::{TestGroupKeys, test_host_holding};
    use automerge::transaction::Transactable as _;
    use std::sync::Arc;
    use std::time::Duration;
    use tilo_resource::Fields as _;
    use tilo_workspace_service::device_documents::ServiceDocuments;
    use tilo_workspace_service::workspace_documents::{DeviceDocuments as _, DocumentRead};

    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let keys = Arc::new(TestGroupKeys::default());
    let start = || async {
        if sealed {
            let _catalogue = AppCatalogue::native();
            crate::surfaces::decide_receipt(&scratch.path().join("surfaces"));
            test_host_holding(scratch.path(), &key, &cert, Arc::clone(&keys)).await
        } else {
            bot_server(scratch.path(), &key, &cert).await
        }
    };
    let server = start().await;
    let workspaces = server.workspaces();
    let workspace = workspaces.create("Plaza").await.unwrap().workspace;
    workspaces
        .prepare_application(&workspace, &cert, &AppCatalogue::native(), CHAT, "")
        .await
        .unwrap();

    let mut earlier = tilo_resource::Replica::<Moderation>::new().as_author("reporter");
    earlier
        .apply(Moderation::REPORT_PERSON.with((
            MemberId::person("reported"),
            "A report from before the move".parse().unwrap(),
        )))
        .unwrap();
    let report = earlier
        .state()
        .unwrap()
        .open_reports()
        .next()
        .unwrap()
        .id
        .clone();
    let mut earlier = earlier.fork().as_author("moderator");
    earlier
        .apply(Moderation::DECIDE_REPORTS.with((
            "old-decision".to_owned(),
            MemberId::person("reported"),
            vec![report],
            tilo_moderation::Ruling {
                reasoning: "The original decision".into(),
                ..tilo_moderation::Ruling::default()
            },
        )))
        .unwrap();
    let earlier = earlier.state().unwrap();
    workspaces
        .author_application(
            &workspace,
            &cert,
            &AppCatalogue::native(),
            CHAT,
            "",
            |doc| {
                let old = doc
                    .put_object(automerge::ROOT, "moderation", automerge::ObjType::Map)
                    .map_err(|error| error.to_string())?;
                earlier
                    .write_fields(doc, &old)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();

    server.shutdown().await;
    if sealed {
        keys.pause(true);
    }
    let server = start().await;
    let workspaces = server.workspaces();
    if sealed {
        let routes = workspaces.document_routes(&workspace).await.unwrap();
        assert!(routes.contains(&(CHAT.to_owned(), String::new())));
        assert!(!routes.contains(&(CHAT.to_owned(), MODERATION_DOCUMENT.to_owned())));
        let reads = ServiceDocuments::of(workspaces.clone());
        assert!(matches!(
            reads.read(&workspace, CHAT, MODERATION_DOCUMENT, None),
            Ok(DocumentRead::Pending)
        ));
        let error = workspaces
            .resource_document(&workspace, CHAT, MODERATION_DOCUMENT)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("moderation history is still arriving")
        );
        keys.pause(false);
        tokio::time::timeout(Duration::from_secs(15), async {
            while workspaces
                .resource_document(&workspace, CHAT, MODERATION_DOCUMENT)
                .await
                .is_err()
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(moderation_state(workspaces, &workspace).await, earlier);
    server.shutdown().await;

    let server = start().await;
    assert_eq!(
        moderation_state(server.workspaces(), &workspace).await,
        earlier
    );
    server.shutdown().await;
}

#[test]
fn a_report_written_during_judgment_is_still_after_the_bots_cursor() {
    use tilo_moderation::Ruling;

    let mut first = tilo_resource::Replica::<Moderation>::new()
        .as_author("first")
        .in_app(CHAT, "Chat");
    first
        .apply(
            Moderation::REPORT_PERSON.with((MemberId::person("reported"), "spam".parse().unwrap())),
        )
        .unwrap();
    let state = first.state().unwrap();
    let report = state.open_reports().next().unwrap();
    let (_, heads) = super::reports_since("workspace", &first.save(), None).unwrap();

    let mut other = first.fork().as_author("other");
    other
        .apply(
            Moderation::REPORT_PERSON
                .with((MemberId::person("reported"), "more spam".parse().unwrap())),
        )
        .unwrap();
    let mut judge = first.fork().as_bot_for(MODERATION, "owner");
    judge
        .apply(Moderation::DECIDE_REPORTS.with((
            "decision".to_owned(),
            MemberId::person("reported"),
            vec![report.id.clone()],
            Ruling {
                reasoning: "first report considered".into(),
                ..Ruling::default()
            },
        )))
        .unwrap();
    other.sync_with(&mut judge).unwrap();

    let after = other.state().unwrap();
    let open: Vec<_> = after
        .open_reports()
        .map(|report| report.reporter.clone())
        .collect();
    assert_eq!(open, [MemberId::person("other")]);
    let (reported, _) = super::reports_since("workspace", &other.save(), Some(&heads)).unwrap();
    assert!(reported);
}
