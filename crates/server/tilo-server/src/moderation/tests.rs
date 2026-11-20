#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use chat::projection::ChatProjection as _;
use std::sync::Mutex;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_action_policy::{Authority, Context, MemberAccess, Role, WorkspacePolicy};
use tilo_agent_session::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_moderation::{Case, Judge, JudgeError, ModerationAgent, Outcome, Verdict};

use super::*;
use tilo_actions::MemberId;

const WORKSPACE: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
const OWNER: &str = "owner";
const ADA: &str = "ada";
const MALLORY: &str = "mallory";
const BOT: &str = "moderation";

/// Restricts for half an hour and takes back what the person did.
struct FakeJudge(Mutex<Vec<Case>>);

impl Judge for FakeJudge {
    fn model(&self) -> &'static str {
        "fake-model"
    }

    fn judge(
        &self,
        case: &Case,
    ) -> impl std::future::Future<Output = Result<Verdict, JudgeError>> + Send {
        self.0.lock().unwrap().push(case.clone());
        async {
            Ok(Verdict {
                outcome: Outcome::Restrict,
                minutes: Some(30),
                revert: true,
                reasoning: "They flooded the channel with the same link.".into(),
            })
        }
    }
}

fn chat_state(replica: &Replica) -> chat::Chat {
    replica.read(|doc| chat::Chat::read(doc).unwrap()).unwrap()
}

fn texts_by(messages: &chat::messages::Messages, author: &str) -> Vec<String> {
    messages
        .iter()
        .filter(|(_, message)| message.author.person_id() == Some(author))
        .map(|(_, message)| message.text.clone())
        .collect()
}

/// The workspace's chat after Mallory flooded `#general` and Ada reported her,
/// her posts in the parts of its messages the page holds beside it.
fn flooded_workspace() -> Replica {
    let catalogue = AppCatalogue::native();
    let mut page = Replica::open(
        &catalogue,
        "so.tilo.chat",
        WORKSPACE,
        &MemberId::person(OWNER),
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc: AutoCommit = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        OWNER,
        chat::Chat::CREATE_CHANNEL.with(chat::chat::chat::CreateChannel {
            name: "general".parse().unwrap(),
            category: None,
            kind: None,
            topic: None,
        }),
    )
    .unwrap();
    let general = chat::projection::read(&doc)
        .channel_rows()
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Text)
        .expect("the chat has a text channel")
        .id;
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    // Each posts through the chain of the chat's messages, as from their
    // own page.
    let mut say = |author: &str, text: &str| -> String {
        let before = message_ids(&page);
        let post = serde_json::json!({ "channel": general, "text": text });
        page.invoke_with(
            Some(tilo_actions::ActionInvoker::person(author)),
            None,
            &tilo_actions::api::ActionAddress::new(
                collab_types::document::chain_resource_id(&chat::parts::chain()),
                "post",
            ),
            post.as_object().unwrap(),
            false,
        )
        .unwrap();
        message_ids(&page)
            .into_iter()
            .find(|id| !before.contains(id))
            .unwrap()
    };
    say(ADA, "hello everyone");
    let mut spam = String::new();
    for _ in 0..3 {
        spam = say(MALLORY, "cheap links here");
    }
    let mut doc: AutoCommit = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        ADA,
        chat::Chat::REPORT_MESSAGE.with(chat::chat::chat::ReportMessage {
            message: spam,
            author: MemberId::person(MALLORY),
            reason: "spam".parse().unwrap(),
        }),
    )
    .unwrap();
    page.apply(&DocumentMutation::Snapshot(doc.save())).unwrap();
    // What the page did is the workspace's now.
    let _ = page.take_mutation();
    let _ = page.take_document_mutations();
    page
}

/// The ids of the messages `replica` holds beside its chat record.
fn message_ids(replica: &Replica) -> Vec<String> {
    messages(replica)
        .iter()
        .map(|(id, _)| id.to_string())
        .collect()
}

/// The messages `replica` holds in the parts beside its chat record.
fn messages(replica: &Replica) -> chat::messages::Messages {
    chat::messages::Messages::new(
        replica
            .resources_beside()
            .into_iter()
            .filter_map(|record| {
                let index = chat::parts::part_index(&record)?;
                let part = replica.read_resource(&record, chat::messages::read_part)?;
                Some((index, std::rc::Rc::new(part)))
            })
            .collect(),
    )
}

/// The moderation bot's replica of `page`'s record and the parts beside
/// it: the workspace owner's installation, granted reading and undoable changes
/// in the workspace.
fn bot_replica(page: &Replica) -> Replica {
    let grant = tilo_action_policy::bot::moderation_grant(WORKSPACE, OWNER);
    let access = MemberAccess::person(WorkspacePolicy::new(WORKSPACE, OWNER), OWNER).acting_as(
        tilo_action_policy::Actor::Member(MemberId::bot(BOT)),
        [grant],
    );
    let mut bot = Replica::open(
        &AppCatalogue::native(),
        page.app_id(),
        WORKSPACE,
        &MemberId::bot(BOT),
        ReplicaActor::Bot {
            installation: BOT.into(),
            installer: OWNER.into(),
            access,
        },
        Seed::Existing {
            record: page.document_id().clone(),
            changes: vec![page.read(|doc| doc.clone().save()).unwrap()],
        },
    )
    .unwrap();
    for record in page.resources_beside() {
        let saved = page
            .read_resource(&record, |doc| doc.clone().save())
            .unwrap();
        bot.apply_document(&record, &DocumentMutation::Snapshot(saved))
            .unwrap();
    }
    bot
}

#[test]
fn a_member_reports_but_cannot_edit_the_report_or_decide_it() {
    use serde_json::json;
    use tilo_resource::{Edit, EditOp, Segment};

    let member_actor = |policy| ReplicaActor::Member {
        access: MemberAccess::person(policy, ADA),
        sandbox: Vec::new(),
        unix_time: 2000,
        agent_name: None,
        device: None,
    };
    let mut policy = WorkspacePolicy::new(WORKSPACE, OWNER);
    policy.members.insert(ADA.into(), Role::Editor);
    let mut page = Replica::open(
        &AppCatalogue::native(),
        tilo_app_id::apps::CHAT,
        WORKSPACE,
        &MemberId::person(ADA),
        member_actor(policy.clone()),
        Seed::New,
    )
    .unwrap();
    let invoke = |page: &mut Replica, verb: &str, args: serde_json::Value| {
        page.invoke_with(
            None,
            None,
            &ActionAddress::new(chat::Chat::KIND, verb),
            args.as_object().unwrap(),
            true,
        )
    };
    invoke(
        &mut page,
        "report-person",
        json!({"person": {"person": MALLORY}, "reason": "spam"}),
    )
    .unwrap();
    let before = chat_state(&page).moderation;
    let report = before.open_reports().next().unwrap();
    assert_eq!(report.reporter, MemberId::person(ADA));
    let decision = json!({
        "id": "d1",
        "person": {"person": MALLORY},
        "reports": [&report.id],
        "ruling": {"outcome": "no-action", "reasoning": "Keep the post."},
    });
    let error = invoke(&mut page, "decide-reports", decision.clone()).unwrap_err();
    assert_eq!(
        error,
        tilo_actions::api::ApiError::Unknown(ActionAddress::new(
            chat::Chat::KIND,
            "decide-reports"
        )),
    );

    let edit = Edit::<chat::Chat>::new(
        vec![
            Segment::Field("moderation".into()),
            Segment::Field("reports".into()),
            Segment::Item(tilo_resource::Id::from_raw(report.id.clone())),
            Segment::Field("decision".into()),
        ],
        EditOp::Set { value: json!("d1") },
    );
    invoke(
        &mut page,
        tilo_resource::resource::FIELD_EDIT,
        json!({"edit": edit}),
    )
    .unwrap_err();
    assert_eq!(chat_state(&page).moderation, before);

    // The same action address and arguments settle the existing report when
    // the workspace authorizes the caller as a Moderator.
    policy.members.insert(ADA.into(), Role::Moderator);
    page.renew(member_actor(policy)).unwrap();
    invoke(&mut page, "decide-reports", decision).unwrap();
    let after = chat_state(&page).moderation;
    assert_eq!(after.open_reports().count(), 0);
    assert_eq!(
        after.decisions.iter().next().unwrap().reports.as_slice(),
        std::slice::from_ref(&report.id)
    );
}

#[test]
fn a_report_restricts_through_the_workspace_and_undoes_the_flood_as_the_bot() {
    let mut page = flooded_workspace();
    assert_eq!(texts_by(&messages(&page), MALLORY).len(), 3);
    let mut bot = bot_replica(&page);
    let mut policy = WorkspacePolicy::new(WORKSPACE, OWNER);
    for (person, role) in [(ADA, Role::Editor), (MALLORY, Role::Editor)] {
        policy.members.insert(person.to_owned(), role);
    }
    let now = tilo_clock::unix_now_ms_i64();
    let cx = Context::at(u64::try_from(now).unwrap());
    // The host enforces governance as the workspace's root, held to its policy.
    let mut govern = |change: Change| {
        let snapshot = policy.clone();
        let owner = Authority::person(snapshot.party(OWNER));
        policy
            .apply(OWNER, &owner, change, &cx, false)
            .map_err(|refusal| refusal.to_string())
    };
    let agent = ModerationAgent::new(FakeJudge(Mutex::default()));

    let mut api =
        ReplicaApi::<chat::Chat>::new(&mut bot, |chat| &chat.moderation, &mut govern, now);
    let moderation = api.moderation().unwrap();
    assert_eq!(
        ModerationAgent::<FakeJudge>::reported(&moderation),
        [MemberId::person(MALLORY)]
    );
    let look =
        ModerationAgent::<FakeJudge>::look(&mut api, &moderation, &MemberId::person(MALLORY));
    assert_eq!(
        look.case.activity.len(),
        3,
        "Mallory's three posts, not Ada's"
    );
    let (verdict, model) = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(agent.verdict(&look))
        .unwrap();
    let enforced = ModerationAgent::<FakeJudge>::enforce(&mut api, look, verdict, model);
    assert!(enforced.failures.is_empty(), "{:?}", enforced.failures);

    // What the bot did reaches the workspace like anyone's change.
    page.apply(&bot.take_mutation().unwrap().unwrap()).unwrap();
    for (record, mutation) in bot.take_document_mutations().unwrap() {
        page.apply_document(&record, &mutation).unwrap();
    }
    assert!(
        texts_by(&messages(&page), MALLORY).is_empty(),
        "the flood is taken back"
    );
    assert_eq!(texts_by(&messages(&page), ADA), ["hello everyone"]);
    let state = chat_state(&page);
    let decision = state.moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.person, MemberId::person(MALLORY));
    assert_eq!(decision.outcome, Outcome::Restrict);
    assert_eq!(decision.model, "fake-model");
    assert_eq!(
        decision.reverted[0].changes, 3,
        "her posts, not her joining"
    );
    assert_eq!(
        state.moderation.open_reports().count(),
        0,
        "the report is settled"
    );
    assert!(
        policy
            .restricted_until(MALLORY, &cx)
            .is_some_and(|until| until > cx.now_unix_ms),
        "Mallory is restricted by the workspace's own expiring Deny"
    );
}

#[test]
fn only_a_report_wakes_the_agent() {
    let report = Did::Verb(tilo_actions::entry::VerbCall::new(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "report-person"),
        [],
    ));
    assert!(files_a_report(&report));
    let post = Did::Verb(tilo_actions::entry::VerbCall::new(
        tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
        [],
    ));
    assert!(!files_a_report(&post));
}
