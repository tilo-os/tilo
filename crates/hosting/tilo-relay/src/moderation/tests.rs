#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Mutex;

use automerge::AutoCommit;
use collab_types::DocumentMutation;
use tilo_action_policy::{
    Answer, Authority, Context, Grant, KindSet, Role, RoomPolicy, Standing, Subject, TreePath,
};
use tilo_agent_participant::{AppCatalogue, Replica, ReplicaActor, Seed};
use tilo_moderation::{Case, Judge, JudgeError, ModerationAgent, Outcome, Verdict};

use super::*;

const ROOM: &str = "6f1d3a52-1c7e-4b8e-9d2a-0f4a8c3e5b71";
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
        .filter(|(_, message)| message.author.0 == author)
        .map(|(_, message)| message.text.clone())
        .collect()
}

/// The room's chat after Mallory flooded `#general` and Ada reported her,
/// and the parts of its messages.
fn flooded_room() -> (Replica, chat::demo::Room) {
    let catalogue = AppCatalogue::native();
    let mut page = Replica::open(
        &catalogue,
        "so.tilo.chat",
        ROOM,
        OWNER,
        ReplicaActor::Owner,
        Seed::New,
    )
    .unwrap();
    let mut doc: AutoCommit = page.read(Clone::clone).unwrap();
    chat::demo::apply_as(
        &mut doc,
        OWNER,
        chat::Chat::CREATE_WORKSPACE.with(chat::record::chat::CreateWorkspace {
            name: "Plaza".into(),
        }),
    )
    .unwrap();
    chat::demo::introduce(&mut doc, ADA, "Ada", "AD").unwrap();
    chat::demo::introduce(&mut doc, MALLORY, "Mallory", "MA").unwrap();
    let general = chat::projection::read(&doc)
        .channel_rows()
        .into_iter()
        .find(|channel| channel.kind == chat::model::ChannelKind::Text)
        .expect("the chat has a text channel")
        .id;
    let mut room = chat::demo::Room::of(doc);
    room.say(ADA, &general, "hello everyone").unwrap();
    let mut spam = String::new();
    for _ in 0..3 {
        spam = room.say(MALLORY, &general, "cheap links here").unwrap();
    }
    room.apply_as(
        ADA,
        chat::Chat::REPORT_MESSAGE.with(chat::record::chat::ReportMessage {
            message: spam,
            author: MALLORY.into(),
            reason: "spam".into(),
        }),
    )
    .unwrap();
    page.apply(&DocumentMutation::Snapshot(room.chat.save()))
        .unwrap();
    (page, room)
}

/// The moderation bot's replica of `page`'s record: the room owner's
/// installation, granted reading and undoable changes in the room.
fn bot_replica(page: &Replica) -> Replica {
    let bot = Subject::bot(BOT);
    let grant = Grant::new(
        "bot:moderation",
        bot.clone(),
        TreePath::room(ROOM),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Subject::person(OWNER),
    );
    let standing =
        Standing::person(RoomPolicy::new(ROOM, OWNER), OWNER).bounded_by(bot, vec![grant]);
    Replica::open(
        &AppCatalogue::native(),
        page.app_id(),
        ROOM,
        &format!("bot:{BOT}"),
        ReplicaActor::Bot {
            installation: BOT.into(),
            installer: OWNER.into(),
            standing,
        },
        Seed::Existing {
            record: page.record_id().clone(),
            changes: vec![page.read(|doc| doc.clone().save()).unwrap()],
        },
    )
    .unwrap()
}

#[test]
#[ignore = "needs the room's moderation agent to read and take back posts in the chat's message parts"]
fn a_report_restricts_through_the_room_and_undoes_the_flood_as_the_bot() {
    let (mut page, room) = flooded_room();
    assert_eq!(texts_by(&room.messages(), MALLORY).len(), 3);
    let mut bot = bot_replica(&page);
    let mut policy = RoomPolicy::new(ROOM, OWNER);
    for (person, role) in [(ADA, Role::Editor), (MALLORY, Role::Editor)] {
        policy.members.insert(person.to_owned(), role);
    }
    let now = tilo_clock::unix_now_ms_i64();
    let cx = Context::at(u64::try_from(now).unwrap());
    // The host enforces governance as the room's root, held to its policy.
    let mut govern = |change: Change| {
        let snapshot = policy.clone();
        let owner = Authority::person(snapshot.party(OWNER));
        policy
            .apply(OWNER, &owner, change, &cx, false)
            .map_err(|refusal| refusal.to_string())
    };
    let agent = ModerationAgent::new(FakeJudge(Mutex::default()));

    let mut door =
        ReplicaDoor::<chat::Chat>::new(&mut bot, |chat| &chat.moderation, &mut govern, now);
    let moderation = door.moderation().unwrap();
    assert_eq!(
        ModerationAgent::<FakeJudge>::reported(&moderation),
        [MALLORY]
    );
    let look = ModerationAgent::<FakeJudge>::look(&mut door, &moderation, MALLORY);
    assert_eq!(
        look.case.activity.len(),
        4,
        "Mallory joining and her three posts, not Ada's"
    );
    let (verdict, model) = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(agent.verdict(&look))
        .unwrap();
    let enforced = ModerationAgent::<FakeJudge>::enforce(&mut door, look, verdict, model);
    assert!(enforced.failures.is_empty(), "{:?}", enforced.failures);

    // What the bot did reaches the room like anyone's change.
    page.apply(&bot.take_mutation().unwrap()).unwrap();
    assert!(
        texts_by(&room.messages(), MALLORY).is_empty(),
        "the flood is taken back"
    );
    assert_eq!(texts_by(&room.messages(), ADA), ["hello everyone"]);
    let state = chat_state(&page);
    let decision = state.moderation.decisions.iter().next().unwrap();
    assert_eq!(decision.person, MALLORY);
    assert_eq!(decision.outcome, Outcome::Restrict);
    assert_eq!(decision.model, "fake-model");
    assert_eq!(
        decision.reverted.0[0].changes, 3,
        "her posts, not her joining"
    );
    assert!(
        state
            .people
            .find(&tilo_resource::Id::from_raw(MALLORY))
            .is_some(),
        "she keeps her place in the roster"
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
        "Mallory is restricted by the room's own expiring Deny"
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
