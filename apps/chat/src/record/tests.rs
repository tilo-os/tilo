use ui::tilo_actions::DoorError;
use ui::tilo_resource::{Id, Invoke, Replica, Resource};

use super::*;
use crate::messages::{Messages, chat_part};
use crate::parts::ChatPart;

const ADA: &str = "u_ada";
const BO: &str = "u_bo";

pub(super) fn id(raw: &str) -> Id {
    Id::from_raw(raw)
}

fn begin(name: &str, initials: &str) -> Invoke<Chat> {
    Chat::BEGIN_CHAT.with(chat::BeginChat {
        workspace: None,
        workspace_name: None,
        name: name.into(),
        initials: initials.into(),
        color: 0,
    })
}

/// A starter chat Ada made, with Bo introduced.
pub(super) fn room() -> Replica<Chat> {
    let mut ada = Replica::<Chat>::new().as_author(ADA);
    ada.set_now_ms(1_000_000);
    ada.apply(begin("Ada", "AD")).unwrap();
    let mut bo = ada.fork().as_author(BO);
    bo.apply(Chat::INTRODUCE.with(chat::Introduce {
        name: "Bo".into(),
        initials: "BO".into(),
        color: 2,
    }))
    .unwrap();
    ada.sync_with(&mut bo).unwrap();
    ada
}

/// Ada's starter chat with Bo introduced, and the part of its messages,
/// one replica of each: verbs on the messages are read against the chat
/// record where they are invoked, as the window's chain reads them.
pub(super) struct Talk {
    pub(super) chat: Replica<Chat>,
    pub(super) part: Replica<ChatPart>,
    now_ms: i64,
}

impl Talk {
    pub(super) fn new() -> Self {
        let chat = room();
        let mut part = Replica::<ChatPart>::new().as_author(ADA);
        part.set_now_ms(1_000_000);
        Self {
            chat,
            part,
            now_ms: 1_000_000,
        }
    }

    /// Another replica of both, as `author`.
    pub(super) fn fork_as(&mut self, author: &str) -> Self {
        Self {
            chat: self.chat.fork().as_author(author),
            part: self.part.fork().as_author(author),
            now_ms: self.now_ms,
        }
    }

    pub(super) fn sync_with(&mut self, other: &mut Self) {
        self.chat.sync_with(&mut other.chat).unwrap();
        self.part.sync_with(&mut other.part).unwrap();
    }

    pub(super) fn set_now_ms(&mut self, now_ms: i64) {
        self.now_ms = now_ms;
        self.chat.set_now_ms(now_ms);
        self.part.set_now_ms(now_ms);
    }

    /// Apply a verb on the messages, as the person confirming it would.
    pub(super) fn apply(&mut self, invoke: Invoke<ChatPart>) -> Result<(), DoorError> {
        use ui::framework::Chained as _;
        if let Some(refusal) = invoke.refusal() {
            return Err(DoorError::InvalidArguments(refusal.to_owned()));
        }
        let mut action = invoke
            .build(self.now_ms)
            .map_err(DoorError::InvalidArguments)?;
        ChatPart::prepare(&mut action, &self.chat.state().unwrap()).map_err(DoorError::Failed)?;
        self.part.apply_action(&action).map(drop)
    }

    pub(super) fn messages(&self) -> Messages {
        Messages::single(self.part.state().unwrap())
    }

    pub(super) fn rows(&self, channel: &str) -> Vec<crate::model::Message> {
        self.chat
            .state()
            .unwrap()
            .message_rows(&self.messages(), channel)
    }

    /// The id of the message saying `text`.
    pub(super) fn said(&self, text: &str) -> Id {
        self.part
            .state()
            .unwrap()
            .messages
            .iter()
            .find(|(_, message)| message.text == text)
            .map_or_else(|| panic!("no message says {text:?}"), |(id, _)| id.clone())
    }
}

pub(super) fn post(text: &str) -> Invoke<ChatPart> {
    post_to("c_general", text)
}

#[test]
fn a_bridges_post_keeps_the_name_it_showed_and_a_persons_does_not() {
    let shown = tilo_actions::Appearance {
        name: "Grace".into(),
        avatar: Some("ab".repeat(32)),
        persona: None,
    };
    let mut ada = Talk::new();
    let mut bridge = ada
        .part
        .fork()
        .as_bot_for("5d0c7a3e-2b41-4f6a-9e18-c3a7b2d4e6f1", ADA)
        .showing(shown.clone());
    bridge.apply(post("from Discord")).unwrap();
    ada.part.sync_with(&mut bridge).unwrap();
    let mut posing = ada.part.fork().as_author(BO).showing(shown.clone());
    posing.apply(post("not Grace")).unwrap();
    ada.part.sync_with(&mut posing).unwrap();
    let rows = ada.rows("c_general");
    let of = |text: &str| rows.iter().find(|row| row.text == text).unwrap();
    let bridged = of("from Discord");
    assert_eq!(bridged.author, "bot:5d0c7a3e-2b41-4f6a-9e18-c3a7b2d4e6f1");
    assert_eq!(bridged.appearance.as_ref(), Some(&shown), "shown as Grace");
    assert_eq!(
        of("not Grace").appearance,
        None,
        "only a bot shows another name"
    );
}

fn reply(parent: &Id, text: &str) -> Invoke<ChatPart> {
    ChatPart::POST.with(chat_part::Post {
        channel: id("c_general"),
        text: text.into(),
        parent: Some(parent.clone()),
        body: None,
        attachment: None,
        mentions: None,
    })
}

#[test]
fn a_new_chat_starts_with_its_creator_and_a_starter_workspace() {
    let ada = room();
    let chat = ada.state().unwrap();
    assert_eq!(
        chat.workspace_rows()
            .iter()
            .map(|workspace| (workspace.id.as_str(), workspace.name.as_str()))
            .collect::<Vec<_>>(),
        [("ws_home", "My Workspace")]
    );
    let channels = chat.channel_rows();
    assert_eq!(
        channels
            .iter()
            .map(|channel| (channel.name.as_str(), channel.kind))
            .collect::<Vec<_>>(),
        [
            ("general", ChannelKind::Text),
            ("Lounge", ChannelKind::Voice)
        ]
    );
    let people = chat.user_rows();
    assert_eq!(
        people
            .iter()
            .map(|person| (person.id.as_str(), person.name.as_str()))
            .collect::<Vec<_>>(),
        [(ADA, "Ada"), (BO, "Bo")]
    );
    assert!(
        !chat.channels.is_empty(),
        "a starter workspace, and no fabricated conversation: messages are kept in parts"
    );
}

#[test]
fn a_hosted_chat_starts_in_its_workspace_and_is_never_rebound() {
    let mut ada = Replica::<Chat>::new().as_author(ADA);
    let hosted = |workspace: &str| {
        Chat::BEGIN_CHAT.with(chat::BeginChat {
            workspace: Some(workspace.into()),
            workspace_name: Some("Design Team".into()),
            name: "Ada".into(),
            initials: "AD".into(),
            color: 1,
        })
    };
    ada.apply(hosted("room-1")).unwrap();
    ada.apply(hosted("room-1")).unwrap();
    assert!(ada.apply(hosted("room-2")).is_err());
    let chat = ada.state().unwrap();
    let workspaces = chat.workspace_rows();
    assert_eq!(workspaces.len(), 1);
    assert_eq!(
        (workspaces[0].id.as_str(), workspaces[0].glyph.as_str()),
        ("room-1", "DT")
    );
    assert!(
        chat.channels.is_empty(),
        "the host's workspace starts empty"
    );
}

#[test]
fn posting_records_its_author_time_and_mentions() {
    let mut ada = Talk::new();
    ada.set_now_ms(1_234_000);
    ada.apply(post("hi @Bo, the deck is up")).unwrap();
    let chat = ada.chat.state().unwrap();
    let rows = ada.rows("c_general");
    assert_eq!(rows.len(), 1);
    let message = &rows[0];
    assert_eq!(message.author, ADA, "the poster is whoever invoked");
    assert_eq!(message.ts, 1234, "the time is the action's");
    assert_eq!(message.mentions.len(), 1);
    assert_eq!(message.mentions[0].user, BO);
    assert_eq!(chat.unread(&ada.messages(), "c_general", BO), 1);
}

#[test]
fn a_post_that_says_nothing_or_is_misplaced_is_refused() {
    let mut ada = Talk::new();
    assert!(ada.apply(post("   ")).is_err(), "an empty message");
    assert!(
        ada.apply(post(&"x".repeat(MAX_MESSAGE_TEXT_BYTES + 1)))
            .is_err(),
        "a document, not a turn"
    );
    assert!(
        ada.apply(post_to("c_lounge", "hello")).is_err(),
        "a voice channel has no timeline"
    );
    assert!(ada.apply(post_to("c_gone", "hello")).is_err());
    assert!(
        ada.apply(ChatPart::POST.with(chat_part::Post {
            channel: id("c_general"),
            text: "look".into(),
            parent: None,
            body: None,
            attachment: Some(AttachmentUpdate {
                kind: "image".into(),
                name: "shot.png".into(),
                blob: "not-a-hash".into(),
                width: 10,
                height: 10,
                size: 0,
            }),
            mentions: None,
        }))
        .is_err(),
        "an attachment is a content address"
    );
    assert!(ada.messages().is_empty());
}

fn post_to(channel: &str, text: &str) -> Invoke<ChatPart> {
    ChatPart::POST.with(chat_part::Post {
        channel: id(channel),
        text: text.into(),
        parent: None,
        body: None,
        attachment: None,
        mentions: None,
    })
}

#[test]
fn a_reply_to_a_missing_or_deleted_message_is_refused() {
    let mut ada = Talk::new();
    assert!(
        ada.apply(reply(&id("missing"), "Where did it go?"))
            .is_err()
    );
    assert!(ada.messages().is_empty());

    ada.apply(post("The plan")).unwrap();
    let parent = ada.said("The plan");
    ada.apply(ChatPart::DELETE_MESSAGE.with(chat_part::DeleteMessage {
        message: parent.clone(),
    }))
    .unwrap();
    assert!(ada.apply(reply(&parent, "My answer")).is_err());
    assert!(
        ada.messages().is_empty(),
        "a reply must not be accepted into a thread nobody can open"
    );
}

#[test]
fn an_attachment_is_posted_by_its_content_address() {
    let mut ada = Talk::new();
    let blob = "ab".repeat(32);
    ada.apply(ChatPart::POST.with(chat_part::Post {
        channel: id("c_general"),
        text: String::new(),
        parent: None,
        body: None,
        attachment: Some(AttachmentUpdate {
            kind: "file".into(),
            name: "notes.md".into(),
            blob: blob.clone(),
            width: 0,
            height: 0,
            size: 1200,
        }),
        mentions: None,
    }))
    .unwrap();
    let rows = ada.rows("c_general");
    assert_eq!(
        rows[0].attachment,
        Some(crate::model::Attachment::File {
            name: "notes.md".into(),
            blob,
            size: 1200,
        })
    );
}

#[test]
fn threads_answer_top_level_messages_of_their_channel() {
    let mut ada = Talk::new();
    ada.apply(post("the plan")).unwrap();
    let plan = ada.said("the plan");
    ada.apply(reply(&plan, "sounds good")).unwrap();
    let answer = ada.said("sounds good");
    assert!(
        ada.apply(reply(&answer, "nested")).is_err(),
        "a reply answers a top-level message"
    );
    let rows = ada.rows("c_general");
    assert_eq!(crate::model::reply_count(&rows, plan.as_str()), 1);
    assert_eq!(ada.messages().top_level(&id("c_general")), 1);
}

#[test]
fn reactions_are_each_persons_own() {
    let mut ada = Talk::new();
    ada.apply(post("ship it")).unwrap();
    let message = ada.said("ship it");
    let react = |emoji: &str| {
        ChatPart::REACT.with(chat_part::React {
            message: message.clone(),
            emoji: emoji.into(),
        })
    };
    let mut bo = ada.fork_as(BO);
    ada.apply(react("🎉")).unwrap();
    bo.apply(react("🎉")).unwrap();
    bo.apply(react("👍")).unwrap();
    ada.sync_with(&mut bo);
    let reactions = |talk: &Talk| {
        talk.rows("c_general")[0]
            .reactions
            .iter()
            .map(|reaction| (reaction.emoji.clone(), reaction.users.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        reactions(&ada),
        [
            ("🎉".to_owned(), vec![ADA.to_owned(), BO.to_owned()]),
            ("👍".to_owned(), vec![BO.to_owned()]),
        ],
        "both first reactions with one emoji count"
    );
    bo.apply(ChatPart::UNREACT.with(chat_part::Unreact {
        message: message.clone(),
        emoji: "👍".into(),
    }))
    .unwrap();
    bo.apply(ChatPart::UNREACT.with(chat_part::Unreact {
        message: message.clone(),
        emoji: "🎉".into(),
    }))
    .unwrap();
    assert_eq!(
        reactions(&bo),
        [("🎉".to_owned(), vec![ADA.to_owned()])],
        "taking back a reaction leaves the others'"
    );
    assert!(bo.apply(react("")).is_err());
}

#[test]
fn an_edit_keeps_the_earlier_text_and_a_delete_removes_the_message() {
    let mut ada = Talk::new();
    ada.set_now_ms(10_000);
    ada.apply(post("teh plan")).unwrap();
    let message = ada.said("teh plan");
    ada.set_now_ms(20_000);
    let edit = |text: &str| {
        ChatPart::EDIT_MESSAGE.with(chat_part::EditMessage {
            message: message.clone(),
            text: text.into(),
            body: None,
            mentions: None,
        })
    };
    ada.apply(edit("the plan, @Bo")).unwrap();
    ada.apply(edit("the plan, @Bo")).unwrap();
    let row = ada.rows("c_general").remove(0);
    assert!(row.edited);
    assert_eq!(row.text, "the plan, @Bo");
    assert_eq!(
        row.history,
        [crate::model::EditRecord {
            ts: 20,
            text: "teh plan".into(),
        }],
        "an unchanged edit records nothing"
    );
    assert_eq!(row.mentions.len(), 1, "the edit's mentions are resolved");
    ada.apply(ChatPart::DELETE_MESSAGE.with(chat_part::DeleteMessage {
        message: message.clone(),
    }))
    .unwrap();
    assert!(ada.messages().is_empty());
    assert!(
        ada.apply(ChatPart::DELETE_MESSAGE.with(chat_part::DeleteMessage { message }))
            .is_err()
    );
}

#[test]
fn channels_and_categories_are_made_moved_and_deleted() {
    let mut ada = room();
    ada.apply(Chat::CREATE_CATEGORY.with(chat::CreateCategory {
        workspace: id("ws_home"),
        name: "Projects".into(),
    }))
    .unwrap();
    let projects = ada
        .state()
        .unwrap()
        .category_rows()
        .into_iter()
        .find(|category| category.name == "Projects")
        .unwrap()
        .id;
    for name in ["#alpha", "beta"] {
        ada.apply(Chat::CREATE_CHANNEL.with(chat::CreateChannel {
            workspace: id("ws_home"),
            name: name.into(),
            category: Some(id(&projects)),
            kind: None,
            topic: None,
        }))
        .unwrap();
    }
    assert!(
        ada.apply(Chat::CREATE_CHANNEL.with(chat::CreateChannel {
            workspace: id("ws_gone"),
            name: "x".into(),
            category: None,
            kind: None,
            topic: None,
        }))
        .is_err()
    );
    let names = |replica: &Replica<Chat>| {
        replica
            .state()
            .unwrap()
            .channel_rows()
            .into_iter()
            .filter(|channel| channel.category == projects)
            .map(|channel| channel.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&ada),
        ["alpha", "beta"],
        "a leading # is not the name"
    );
    let beta = ada
        .state()
        .unwrap()
        .channel_rows()
        .into_iter()
        .find(|channel| channel.name == "beta")
        .unwrap()
        .id;
    ada.apply(Chat::MOVE_CHANNEL.with(chat::MoveChannel {
        channel: id(&beta),
        up: true,
    }))
    .unwrap();
    assert_eq!(names(&ada), ["beta", "alpha"]);
    ada.apply(Chat::SET_CHANNEL_CATEGORY.with(chat::SetChannelCategory {
        channel: id(&beta),
        category: None,
    }))
    .unwrap();
    assert_eq!(names(&ada), ["alpha"], "filed under no category");
    ada.apply(Chat::SET_CHANNEL_CATEGORY.with(chat::SetChannelCategory {
        channel: id(&beta),
        category: Some(id(&projects)),
    }))
    .unwrap();
    assert_eq!(names(&ada), ["beta", "alpha"], "back where it was listed");
    assert!(
        ada.apply(Chat::SET_CHANNEL_CATEGORY.with(chat::SetChannelCategory {
            channel: id(&beta),
            category: Some(id("cat_gone")),
        }))
        .is_err()
    );
    ada.apply_confirmed(Chat::DELETE_CATEGORY.with(chat::DeleteCategory {
        category: id(&projects),
    }))
    .unwrap();
    let chat = ada.state().unwrap();
    assert!(chat.categories.get(&id(&projects)).is_none());
    assert!(
        chat.channel_rows()
            .iter()
            .filter(|channel| channel.name == "alpha" || channel.name == "beta")
            .all(|channel| channel.category.is_empty()),
        "its channels stay, uncategorised"
    );
}

#[test]
fn a_channel_category_must_belong_to_its_workspace() {
    let mut ada = room();
    ada.apply(Chat::CREATE_WORKSPACE.with(chat::CreateWorkspace {
        name: "Other workspace".into(),
    }))
    .unwrap();
    let other_category = ada
        .state()
        .unwrap()
        .categories
        .iter()
        .find(|(_, category)| category.workspace != id("ws_home"))
        .map(|(id, _)| id.clone())
        .unwrap();
    let create = |category| {
        Chat::CREATE_CHANNEL.with(chat::CreateChannel {
            workspace: id("ws_home"),
            name: "project".into(),
            category: Some(category),
            kind: None,
            topic: None,
        })
    };
    let before = ada.state().unwrap();
    assert!(ada.apply(create(other_category)).is_err());
    assert!(ada.apply(create(id("missing"))).is_err());
    let after = ada.state().unwrap();
    assert_eq!(
        after.channels, before.channels,
        "no hidden channel is created"
    );

    ada.apply(create(id("cat_home_text"))).unwrap();
    let chat = ada.state().unwrap();
    assert!(chat.channels.values().any(|channel| {
        channel.name == "project"
            && channel.workspace == id("ws_home")
            && channel.category == Some(id("cat_home_text"))
    }));
}

#[test]
fn deleting_a_channel_takes_its_read_marks() {
    let mut ada = Talk::new();
    ada.apply(post("hello")).unwrap();
    ada.chat
        .apply(Chat::MARK_READ.with(chat::MarkRead {
            channel: id("c_general"),
            part: 0,
            seen: 1,
        }))
        .unwrap();
    ada.chat
        .apply_confirmed(Chat::DELETE_CHANNEL.with(chat::DeleteChannel {
            channel: id("c_general"),
        }))
        .unwrap();
    let chat = ada.chat.state().unwrap();
    assert_eq!(
        chat.read_mark("c_general", ADA),
        crate::parts::ReadMark::default()
    );
    assert!(
        chat.message_rows(&ada.messages(), "c_general").len() == 1
            && chat.channels.get(&id("c_general")).is_none(),
        "its messages stay in their part, in a channel nobody shows"
    );
}

#[test]
fn a_read_mark_is_the_readers_own_and_never_counts_unseen_messages() {
    let mut ada = Talk::new();
    for text in ["one", "two", "three"] {
        ada.apply(post(text)).unwrap();
    }
    let mut bo = ada.fork_as(BO);
    let mark = |seen: u32| {
        Chat::MARK_READ.with(chat::MarkRead {
            channel: id("c_general"),
            part: 0,
            seen,
        })
    };
    bo.chat.apply(mark(2)).unwrap();
    let chat = bo.chat.state().unwrap();
    let held = bo.messages();
    assert_eq!(chat.unread(&held, "c_general", BO), 1);
    assert_eq!(chat.unread(&held, "c_general", ADA), 3, "Ada read nothing");
    bo.chat.apply(mark(3)).unwrap();
    let three = crate::parts::ReadMark { part: 0, seen: 3 };
    assert_eq!(bo.chat.state().unwrap().read_mark("c_general", BO), three);
    bo.chat.apply(mark(1)).unwrap();
    assert_eq!(
        bo.chat.state().unwrap().read_mark("c_general", BO),
        three,
        "a mark never moves back"
    );
}

#[test]
fn an_agent_records_its_work_on_its_own_reply_to_the_summons() {
    const AGENT: &str = "agent-task:22222222-2222-4222-8222-222222222222";
    let mut ada = Talk::new();
    ada.apply(post("@Tide chart the revenue")).unwrap();
    let summons = ada.said("@Tide chart the revenue");
    let mut agent = ada.fork_as(AGENT);
    let work = |state: &str, outcome: &str| {
        ChatPart::RECORD_WORK.with(chat_part::RecordWork {
            message: summons.clone(),
            work: WorkUpdate {
                state: state.into(),
                app: None,
                did: Some("Add function".into()),
                about: Some("Revenue".into()),
                route: Some("model".into()),
                why: Some("Calculator graphs functions".into()),
                trace: vec![StepUpdate {
                    id: "00000000-0000-4000-8000-000000000007".into(),
                    sequence: 0,
                    resource: "calculator.workspace".into(),
                    label: "Add function".into(),
                    outcome: outcome.into(),
                    why: None,
                }],
            },
            say: Some("On it.".into()),
        })
    };
    agent.apply(work("doing", "asked")).unwrap();
    agent.apply(work("done", "applied")).unwrap();
    let chat = agent.chat.state().unwrap();
    let held = agent.messages();
    let reply = chat
        .message_row(&held, reply_id(&summons, AGENT).as_str())
        .expect("the agent's reply is made once");
    assert_eq!(reply.author, AGENT);
    assert_eq!(reply.parent.as_deref(), Some(summons.as_str()));
    assert_eq!(reply.text, "On it.");
    assert!(
        reply
            .author
            .starts_with(tilo_actions::AGENT_TASK_PARTICIPANT_PREFIX)
    );
    let work = reply.work.expect("the reply carries the work");
    assert!(matches!(
        work.state,
        crate::model::AgentWorkState::Done { .. }
    ));
    assert_eq!(work.trace.len(), 1, "a step is rewritten, never repeated");
    assert_eq!(
        work.trace[0].outcome,
        crate::model::AgentStepOutcome::Applied
    );
    assert_eq!(chat.message_rows(&held, "c_general").len(), 2);
    assert!(
        agent
            .apply(ChatPart::RECORD_WORK.with(chat_part::RecordWork {
                message: summons,
                work: WorkUpdate {
                    state: "dancing".into(),
                    app: None,
                    did: None,
                    about: None,
                    route: None,
                    why: None,
                    trace: Vec::new(),
                },
                say: None,
            }))
            .is_err(),
        "an unknown state is refused"
    );
}

#[test]
fn an_agent_is_named_once_for_everyone() {
    const AGENT: &str = "agent-task:33333333-3333-4333-8333-333333333333";
    let mut ada = room();
    ada.apply(Chat::NAME_AGENT.with(chat::NameAgent {
        participant: AGENT.into(),
        name: "Tide".into(),
    }))
    .unwrap();
    assert!(
        ada.apply(Chat::NAME_AGENT.with(chat::NameAgent {
            participant: BO.into(),
            name: "Not an agent".into(),
        }))
        .is_err()
    );
    let names = ada.state().unwrap().agent_authors();
    assert_eq!(names.get(AGENT).map(String::as_str), Some("Tide"));
    assert_eq!(names.len(), 1);
}

#[test]
fn plain_changes_are_field_edits_at_their_paths() {
    let mut talk = Talk::new();
    talk.apply(post("pin me")).unwrap();
    let message = talk.said("pin me");
    talk.apply(ChatPart::PIN.with(chat_part::Pin {
        message: message.clone(),
        pinned: true,
    }))
    .unwrap();
    assert!(talk.messages().get(&message).unwrap().pinned);
    let mut ada = talk.chat;
    let paths = Chat::paths();
    for edit in [
        paths
            .channels
            .item(&id("c_general"))
            .name
            .set("lobby".into()),
        paths
            .channels
            .item(&id("c_general"))
            .topic
            .set("Say hi".into()),
        paths.channels.item(&id("c_lounge")).archived.set(true),
        paths
            .people
            .item(&id(ADA))
            .status
            .set(crate::model::Presence::Dnd),
    ] {
        ada.apply(Invoke::edit(&edit).unwrap()).unwrap();
    }
    let chat = ada.state().unwrap();
    let general = chat.channels.get(&id("c_general")).unwrap();
    assert_eq!(
        (general.name.as_str(), general.topic.as_str()),
        ("lobby", "Say hi")
    );
    assert!(chat.channels.get(&id("c_lounge")).unwrap().archived);
    assert_eq!(chat.user_rows()[0].status, crate::model::Presence::Dnd);
    let messages = ChatPart::paths().messages.item(&message);
    for edit in [
        messages.text.set("rewritten".into()),
        messages.pinned.set(false),
    ] {
        assert!(
            talk.part.apply(Invoke::edit(&edit).unwrap()).is_err(),
            "a message's text changes only through edit-message, its pin through pin"
        );
    }
}

#[test]
fn two_replicas_converge_on_concurrent_posts_reactions_and_renames() {
    let mut ada = Talk::new();
    ada.apply(post("base")).unwrap();
    let base = ada.said("base");
    let mut bo = ada.fork_as(BO);
    ada.apply(post("from ada")).unwrap();
    bo.apply(post("from bo")).unwrap();
    let react = || {
        ChatPart::REACT.with(chat_part::React {
            message: base.clone(),
            emoji: "🎉".into(),
        })
    };
    ada.apply(react()).unwrap();
    bo.apply(react()).unwrap();
    let rename = Chat::paths()
        .channels
        .item(&id("c_general"))
        .topic
        .set("Planning".into());
    bo.chat.apply(Invoke::edit(&rename).unwrap()).unwrap();
    bo.apply(reply(&base, "threaded")).unwrap();
    ada.sync_with(&mut bo);
    assert_eq!(ada.chat.state().unwrap(), bo.chat.state().unwrap());
    assert_eq!(ada.part.state().unwrap(), bo.part.state().unwrap());
    let rows = ada.rows("c_general");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].reactions[0].users, [ADA, BO]);
    assert_eq!(
        ada.chat
            .state()
            .unwrap()
            .channels
            .get(&id("c_general"))
            .unwrap()
            .topic,
        "Planning"
    );
}

#[test]
fn replaying_the_log_reproduces_the_record() {
    let mut ada = Talk::new();
    let mut fresh = ada.part.fork();
    ada.set_now_ms(5_000);
    ada.apply(post("one")).unwrap();
    let one = ada.said("one");
    ada.apply(reply(&one, "two")).unwrap();
    ada.apply(ChatPart::REACT.with(chat_part::React {
        message: one,
        emoji: "👍".into(),
    }))
    .unwrap();
    for envelope in ada.part.log().to_vec() {
        fresh.replay(&envelope).unwrap();
    }
    assert_eq!(
        ada.part.heads(),
        fresh.heads(),
        "the same changes, byte for byte"
    );
    let (left, right) = (ada.part.state().unwrap(), fresh.state().unwrap());
    assert_eq!(left, right);
    assert_eq!(
        ada.chat
            .state()
            .unwrap()
            .message_rows(&Messages::single(right), "c_general")
            .len(),
        2
    );
}

#[test]
fn the_record_offers_each_item_its_views_with_no_window_open() {
    let mut talk = Talk::new();
    talk.apply(post("the deck is up")).unwrap();
    let message = talk.said("the deck is up");
    let mut children = <Chat as ui::tilo_resource::Resource>::children(&talk.chat.state().unwrap());
    let part = talk.part.state().unwrap();
    children.extend(<ChatPart as ui::tilo_resource::Resource>::children(&part));
    let ada = talk.chat;
    let item = |field: &str, raw: &Id| {
        let wanted = if field == "messages" {
            ui::tilo_resource::items::item_resource_id::<ChatPart>(field, raw)
        } else {
            ui::tilo_resource::items::item_resource_id::<Chat>(field, raw)
        };
        children
            .iter()
            .find(|child| child.id.as_str() == wanted)
            .unwrap_or_else(|| panic!("{wanted} is published"))
            .clone()
    };
    let actions = |resource: &ui::tilo_actions::Resource| {
        resource
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect::<Vec<_>>()
    };

    let said = item("messages", &message);
    for effect in ["reveal-message", "reveal-message-range", "read-message"] {
        assert!(
            actions(&said).iter().any(|id| id == effect),
            "{effect}: {:?}",
            actions(&said)
        );
    }
    let range = said
        .actions
        .iter()
        .find(|action| action.id == "reveal-message-range")
        .unwrap();
    assert_eq!(
        range
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect::<Vec<_>>(),
        ["start", "end", "unit"],
        "the message is the item; only the range is asked"
    );
    let view = said.views.iter().find(|view| view.id == "message").unwrap();
    assert!(view.primary && view.range.is_some(), "{view:?}");

    let general = item("channels", &id("c_general"));
    assert!(actions(&general).iter().any(|id| id == "open-channel"));
    let workspace = ada
        .state()
        .unwrap()
        .workspaces
        .ids()
        .next()
        .cloned()
        .unwrap();
    let workspace = item("workspaces", &workspace);
    assert!(actions(&workspace).iter().any(|id| id == "open-workspace"));
    assert!(workspace.views.iter().any(|view| view.id == "workspace"));
    let person = item("people", &id(BO));
    assert!(actions(&person).iter().any(|id| id == "open-person"));
}

#[test]
fn only_the_author_edits_or_deletes_and_a_moderator_removes() {
    let mut ada = Talk::new();
    ada.apply(post("mine")).unwrap();
    let message = ada.said("mine");
    let mut bo = ada.fork_as(BO);
    assert!(
        bo.apply(ChatPart::EDIT_MESSAGE.with(chat_part::EditMessage {
            message: message.clone(),
            text: "yours now".into(),
            body: None,
            mentions: None,
        }))
        .is_err(),
        "nobody edits another's message"
    );
    assert!(
        bo.apply(ChatPart::DELETE_MESSAGE.with(chat_part::DeleteMessage {
            message: message.clone(),
        }))
        .is_err(),
        "nobody deletes another's message"
    );
    assert_eq!(bo.messages().len(), 1);
    bo.apply(ChatPart::REMOVE_MESSAGE.with(chat_part::RemoveMessage { message }))
        .unwrap();
    assert!(bo.messages().is_empty());
}

#[test]
fn removing_a_message_sits_under_the_moderation_node() {
    let field = |id: &str| {
        ChatPart::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .map(|decl| decl.field)
    };
    assert_eq!(
        field("remove-message"),
        Some(Some(ui::tilo_actions::MODERATION_FIELD))
    );
    assert_eq!(field("delete-message"), Some(None));
}

#[test]
fn a_guest_reports_a_message_or_a_person_and_only_moderators_decide() {
    let mut talk = Talk::new();
    talk.apply(post("buy cheap links")).unwrap();
    let message = talk.said("buy cheap links");
    let mut ada = talk.chat;
    let mut bo = ada.fork().as_author(BO);
    bo.apply(Chat::REPORT_MESSAGE.with(chat::ReportMessage {
        message: message.to_string(),
        author: ADA.into(),
        reason: "spam".into(),
    }))
    .unwrap();
    let state = bo.state().unwrap();
    let report = state.moderation.open_reports().next().unwrap();
    assert_eq!(report.person, ADA, "a message report names its author");
    assert_eq!(report.item, Some(message.to_string()));
    assert!(
        bo.apply(Chat::REPORT_PERSON.with(chat::ReportPerson {
            person: id(BO),
            reason: "me".into(),
        }))
        .is_err(),
        "nobody reports themselves"
    );
    bo.apply(Chat::REPORT_PERSON.with(chat::ReportPerson {
        person: id(ADA),
        reason: "spamming the channel".into(),
    }))
    .unwrap();
    assert_eq!(
        bo.state().unwrap().moderation.open_reports_on(ADA).count(),
        2
    );

    let field = |id: &str| {
        Chat::declarations()
            .into_iter()
            .find(|decl| decl.id == id)
            .map(|decl| decl.field)
    };
    assert_eq!(field("report-message"), Some(None));
    assert_eq!(field("report-person"), Some(None));
    for verb in ["decide-reports", "overturn-decision"] {
        assert_eq!(
            field(verb),
            Some(Some(ui::tilo_actions::MODERATION_FIELD)),
            "{verb}"
        );
    }
}
