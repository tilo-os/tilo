use ui::tilo_actions::DoorError;
use ui::tilo_resource::{Id, Invoke, Replica, Resource};

use super::*;
use crate::messages::{Messages, NewMessage, chat_part};
use crate::parts::ChatPart;

const ADA: &str = "u_ada";
const BO: &str = "u_bo";

pub(super) fn id(raw: &str) -> Id {
    Id::from_raw(raw)
}

fn begin(name: &str, initials: &str) -> Invoke<Chat> {
    Chat::BEGIN_CHAT.with(chat::BeginChat {
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
        use ui::framework::{Chained as _, Submit as _};
        let invoke = invoke.into_invoke().map_err(DoorError::InvalidArguments)?;
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
    post_to("c_chat", text)
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
    let rows = ada.rows("c_chat");
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
        message: NewMessage {
            channel: id("c_chat"),
            text: text.into(),
            parent: Some(parent.clone()),
            body: None,
            attachment: None,
            mentions: None,
            written: None,
        },
    })
}

#[test]
fn a_new_chat_starts_with_its_creator_and_the_rooms_two_channels() {
    let ada = room();
    let chat = ada.state().unwrap();
    let channels = chat.channel_rows();
    assert_eq!(
        channels
            .iter()
            .map(|channel| (channel.id.as_str(), channel.name.as_str(), channel.kind))
            .collect::<Vec<_>>(),
        [
            (CHAT_CHANNEL, "chat", ChannelKind::Text),
            (VOICE_CHANNEL, "voice", ChannelKind::Voice)
        ]
    );
    assert!(
        chat.categories.is_empty(),
        "no categories until someone makes one"
    );
    let people = chat.user_rows();
    assert_eq!(
        people
            .iter()
            .map(|person| (person.id.as_str(), person.name.as_str()))
            .collect::<Vec<_>>(),
        [(ADA, "Ada"), (BO, "Bo")]
    );
}

/// The room's two channels as a replica of `chat` lists them.
fn room_channels(chat: &Chat) -> Vec<(String, String, ChannelKind)> {
    chat.channel_rows()
        .into_iter()
        .map(|channel| (channel.id, channel.name, channel.kind))
        .collect()
}

// USER REQUIREMENT (2026-10-19): "Chat runs *in a workspace*, not the other way around."
// First open in a workspace: exactly one text channel `#chat` and one voice
// channel `#voice`, nothing else, created once per room so concurrent first
// opens on several devices converge on exactly those two.
#[test]
fn first_opens_on_two_devices_converge_on_exactly_chat_and_voice() {
    let mut genesis = Replica::<Chat>::new().as_author(ADA);
    // Each device seeds the record it found empty, at once: Ada's laptop
    // and phone, and Bo in the same room.
    let mut laptop = genesis.fork().as_author(ADA);
    let mut phone = genesis.fork().as_author(ADA);
    let mut bo = genesis.fork().as_author(BO);
    laptop.apply(begin("Ada", "AD")).unwrap();
    phone.apply(begin("Ada", "AD")).unwrap();
    bo.apply(begin("Bo", "BO")).unwrap();
    // A device opening the room after it synced makes nothing more.
    let mut first = genesis.fork().as_author(ADA);
    first.apply(begin("Ada", "AD")).unwrap();
    let mut second = first.fork().as_author(BO);
    second.apply(begin("Bo", "BO")).unwrap();
    laptop.sync_with(&mut phone).unwrap();
    laptop.sync_with(&mut bo).unwrap();
    first.sync_with(&mut second).unwrap();
    let expected = vec![
        (
            CHAT_CHANNEL.to_owned(),
            "chat".to_owned(),
            ChannelKind::Text,
        ),
        (
            VOICE_CHANNEL.to_owned(),
            "voice".to_owned(),
            ChannelKind::Voice,
        ),
    ];
    for replica in [&laptop, &phone, &bo, &first, &second] {
        let chat = replica.state().expect("the merged chat opens");
        assert_eq!(room_channels(&chat), expected);
        assert!(chat.categories.is_empty());
        assert!(
            chat.channels.quarantined().is_empty(),
            "every channel is complete: {:?}",
            chat.channels.quarantined()
        );
        for (_, channel) in chat.channels.iter() {
            assert_eq!(channel.category, None);
            assert!(channel.topic.is_empty() && !channel.archived);
        }
    }
    assert_eq!(
        laptop.state().unwrap().channels,
        bo.state().unwrap().channels,
        "every device holds the same two"
    );
}

#[test]
fn posting_records_its_author_time_and_mentions() {
    let mut ada = Talk::new();
    ada.set_now_ms(1_234_000);
    ada.apply(post("hi @Bo, the deck is up")).unwrap();
    let chat = ada.chat.state().unwrap();
    let rows = ada.rows("c_chat");
    assert_eq!(rows.len(), 1);
    let message = &rows[0];
    assert_eq!(message.author, ADA, "the poster is whoever invoked");
    assert_eq!(message.ts, 1234, "the time is the action's");
    assert_eq!(message.mentions.len(), 1);
    assert_eq!(message.mentions[0].user, BO);
    assert_eq!(chat.unread(&ada.messages(), "c_chat", BO), 1);
}

#[test]
fn a_member_cannot_backdate_a_post() {
    let mut ada = Talk::new();
    ada.set_now_ms(1_234_000);
    ada.apply(ChatPart::POST.with(chat_part::Post {
        message: NewMessage {
            written: Some(5),
            ..post_fields("c_chat", "from the past")
        },
    }))
    .unwrap();
    assert_eq!(
        ada.rows("c_chat")[0].ts,
        1234,
        "only a message a bot brings over keeps when it was written"
    );
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
        ada.apply(post_to("c_voice", "hello")).is_err(),
        "a voice channel has no timeline"
    );
    assert!(ada.apply(post_to("c_gone", "hello")).is_err());
    assert!(
        ada.apply(ChatPart::POST.with(chat_part::Post {
            message: NewMessage {
                channel: id("c_chat"),
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
                written: None,
            },
        }))
        .is_err(),
        "an attachment is a content address"
    );
    assert!(ada.messages().is_empty());
}

fn post_to(channel: &str, text: &str) -> Invoke<ChatPart> {
    ChatPart::POST.with(chat_part::Post {
        message: post_fields(channel, text),
    })
}

fn post_fields(channel: &str, text: &str) -> NewMessage {
    NewMessage {
        channel: id(channel),
        text: text.into(),
        parent: None,
        body: None,
        attachment: None,
        mentions: None,
        written: None,
    }
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
        message: NewMessage {
            channel: id("c_chat"),
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
            written: None,
        },
    }))
    .unwrap();
    let rows = ada.rows("c_chat");
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
    let rows = ada.rows("c_chat");
    assert_eq!(crate::model::reply_count(&rows, plan.as_str()), 1);
    assert_eq!(ada.messages().top_level(&id("c_chat")), 1);
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
        talk.rows("c_chat")[0]
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
    let row = ada.rows("c_chat").remove(0);
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
            name: name.into(),
            category: Some(id(&projects)),
            kind: None,
            topic: None,
        }))
        .unwrap();
    }
    assert!(
        ada.apply(Chat::CREATE_CHANNEL.with(chat::CreateChannel {
            name: "x".into(),
            category: Some(id("cat_gone")),
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
fn a_channel_is_filed_only_under_a_category_that_exists() {
    let mut ada = room();
    let create = |category| {
        Chat::CREATE_CHANNEL.with(chat::CreateChannel {
            name: "project".into(),
            category: Some(category),
            kind: None,
            topic: None,
        })
    };
    let before = ada.state().unwrap();
    assert!(ada.apply(create(id("missing"))).is_err());
    let after = ada.state().unwrap();
    assert_eq!(
        after.channels, before.channels,
        "no hidden channel is created"
    );
    ada.apply(Chat::CREATE_CATEGORY.with(chat::CreateCategory {
        name: "Work".into(),
    }))
    .unwrap();
    let work = ada
        .state()
        .unwrap()
        .categories
        .ids()
        .next()
        .cloned()
        .unwrap();
    ada.apply(create(work.clone())).unwrap();
    let chat = ada.state().unwrap();
    assert!(
        chat.channels
            .values()
            .any(|channel| { channel.name == "project" && channel.category == Some(work.clone()) })
    );
}

#[test]
fn deleting_a_channel_takes_its_read_marks() {
    let mut ada = Talk::new();
    ada.apply(post("hello")).unwrap();
    ada.chat
        .apply(Chat::MARK_READ.with(chat::MarkRead {
            channel: id("c_chat"),
            part: 0,
            seen: 1,
        }))
        .unwrap();
    ada.chat
        .apply_confirmed(Chat::DELETE_CHANNEL.with(chat::DeleteChannel {
            channel: id("c_chat"),
        }))
        .unwrap();
    let chat = ada.chat.state().unwrap();
    assert_eq!(
        chat.read_mark("c_chat", ADA),
        crate::parts::ReadMark::default()
    );
    assert!(
        chat.message_rows(&ada.messages(), "c_chat").len() == 1
            && chat.channels.get(&id("c_chat")).is_none(),
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
            channel: id("c_chat"),
            part: 0,
            seen,
        })
    };
    bo.chat.apply(mark(2)).unwrap();
    let chat = bo.chat.state().unwrap();
    let held = bo.messages();
    assert_eq!(chat.unread(&held, "c_chat", BO), 1);
    assert_eq!(chat.unread(&held, "c_chat", ADA), 3, "Ada read nothing");
    bo.chat.apply(mark(3)).unwrap();
    let three = crate::parts::ReadMark { part: 0, seen: 3 };
    assert_eq!(bo.chat.state().unwrap().read_mark("c_chat", BO), three);
    bo.chat.apply(mark(1)).unwrap();
    assert_eq!(
        bo.chat.state().unwrap().read_mark("c_chat", BO),
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
    assert_eq!(chat.message_rows(&held, "c_chat").len(), 2);
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
        paths.channels.item(&id("c_chat")).name.set("lobby".into()),
        paths
            .channels
            .item(&id("c_chat"))
            .topic
            .set("Say hi".into()),
        paths.channels.item(&id("c_voice")).archived.set(true),
        paths
            .people
            .item(&id(ADA))
            .status
            .set(crate::model::Presence::Dnd),
    ] {
        ada.apply(Invoke::edit(&edit).unwrap()).unwrap();
    }
    let chat = ada.state().unwrap();
    let general = chat.channels.get(&id("c_chat")).unwrap();
    assert_eq!(
        (general.name.as_str(), general.topic.as_str()),
        ("lobby", "Say hi")
    );
    assert!(chat.channels.get(&id("c_voice")).unwrap().archived);
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
        .item(&id("c_chat"))
        .topic
        .set("Planning".into());
    bo.chat.apply(Invoke::edit(&rename).unwrap()).unwrap();
    bo.apply(reply(&base, "threaded")).unwrap();
    ada.sync_with(&mut bo);
    assert_eq!(ada.chat.state().unwrap(), bo.chat.state().unwrap());
    assert_eq!(ada.part.state().unwrap(), bo.part.state().unwrap());
    let rows = ada.rows("c_chat");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].reactions[0].users, [ADA, BO]);
    assert_eq!(
        ada.chat
            .state()
            .unwrap()
            .channels
            .get(&id("c_chat"))
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
            .message_rows(&Messages::single(right), "c_chat")
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

    let general = item("channels", &id("c_chat"));
    assert!(actions(&general).iter().any(|id| id == "open-channel"));
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

/// The value map of the item `id` of the collection `field` in `doc`.
fn stored_value(doc: &automerge::AutoCommit, field: &str, id: &str) -> automerge::ObjId {
    use automerge::ReadDoc as _;
    let (_, map) = doc.get(automerge::ROOT, field).unwrap().unwrap();
    let (_, entry) = doc.get(&map, id).unwrap().unwrap();
    let (_, value) = doc.get(&entry, "v").unwrap().unwrap();
    value
}

#[test]
fn a_chat_with_a_malformed_channel_opens_shows_the_rest_and_deletes_it() {
    let mut ada = room();
    let voice = stored_value(ada.doc(), "channels", VOICE_CHANNEL);
    // A peer's damaged write: a kind no channel has.
    ui::tilo_resource::Slot::Key(&voice, "kind")
        .put_scalar(
            ada.doc_mut(),
            automerge::ScalarValue::Str("teleport".into()),
        )
        .unwrap();
    ada.doc_mut().commit();

    let chat = ada
        .state()
        .expect("one malformed channel never makes the chat unopenable");
    assert_eq!(
        room_channels(&chat),
        [(
            CHAT_CHANNEL.to_owned(),
            "chat".to_owned(),
            ChannelKind::Text
        )],
        "the rest of the room shows"
    );
    let held = chat.channels.quarantined();
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].id.as_str(), VOICE_CHANNEL);
    assert!(held[0].reason.contains("kind"), "{}", held[0].reason);

    // Posting and reading go on around it.
    ada.apply(Chat::INTRODUCE.with(chat::Introduce {
        name: "Ada L".into(),
        initials: "AL".into(),
        color: 3,
    }))
    .unwrap();
    assert_eq!(
        ada.state().unwrap().channels.quarantined().len(),
        1,
        "kept for repair"
    );

    ada.apply_confirmed(Chat::DELETE_CHANNEL.with(chat::DeleteChannel {
        channel: id(VOICE_CHANNEL),
    }))
    .unwrap();
    let chat = ada.state().unwrap();
    assert!(
        chat.channels.quarantined().is_empty(),
        "repaired by deleting it"
    );
    assert_eq!(room_channels(&chat).len(), 1);
}

/// The agent conversation the user's chat holds, under the id version 3
/// derived from its workspace and the agent.
const OLD_AGENT_CHANNEL: &str = "c_agent_9b1d2c3e4f5a6b7c";

/// A chat as version 3 stored it: the starter every device made, its
/// workspace and its categories and channels naming it, and a direct
/// conversation with the agent `scribe`. `workspace` is left out of the
/// category `cat_home_text`, as two devices' concurrent starters left the
/// user's chat. `voice_name` is what the `Voice` category's name holds.
fn version_3_starter(
    with_workspace_on_text_category: bool,
    voice_name: automerge::ScalarValue,
) -> automerge::AutoCommit {
    use ui::tilo_resource::{Slot, write_key};
    let mut doc = Replica::<Chat>::new().doc().clone();
    let root = automerge::ROOT;
    let map = |doc: &mut automerge::AutoCommit, obj: &automerge::ObjId, key: &str| {
        Slot::Key(obj, key)
            .object(doc, automerge::ObjType::Map)
            .unwrap()
    };
    let order = |doc: &mut automerge::AutoCommit, obj: &automerge::ObjId, key: &str| {
        Slot::Key(obj, "k")
            .put_scalar(doc, automerge::ScalarValue::Str(key.into()))
            .unwrap();
    };
    Slot::Key(&root, ui::tilo_resource::evolution::VERSION_KEY)
        .put_scalar(&mut doc, automerge::ScalarValue::Uint(3))
        .unwrap();
    let workspaces = map(&mut doc, &root, "workspaces");
    let home = map(&mut doc, &workspaces, "ws_home");
    order(&mut doc, &home, "a0");
    let home = map(&mut doc, &home, "v");
    write_key(&mut doc, &home, "name", &"My Workspace".to_owned()).unwrap();
    write_key(&mut doc, &home, "glyph", &"MW".to_owned()).unwrap();
    let entry = |doc: &mut automerge::AutoCommit, field: &str, id: &str, key: &str| {
        let (_, items) = automerge::ReadDoc::get(doc, &root, field).unwrap().unwrap();
        let entry = map(doc, &items, id);
        order(doc, &entry, key);
        map(doc, &entry, "v")
    };
    let text = entry(&mut doc, "categories", "cat_home_text", "a0");
    if with_workspace_on_text_category {
        write_key(&mut doc, &text, "workspace", &Id::from_raw("ws_home")).unwrap();
    }
    write_key(&mut doc, &text, "name", &"Channels".to_owned()).unwrap();
    let voice = entry(&mut doc, "categories", "cat_home_voice", "a1");
    write_key(&mut doc, &voice, "workspace", &Id::from_raw("ws_home")).unwrap();
    Slot::Key(&voice, "name")
        .put_scalar(&mut doc, voice_name)
        .unwrap();
    for (id, category, name, kind, key) in [
        (
            "c_general",
            Some("cat_home_text"),
            "general",
            ChannelKind::Text,
            "a0",
        ),
        (
            "c_lounge",
            Some("cat_home_voice"),
            "Lounge",
            ChannelKind::Voice,
            "a1",
        ),
        (OLD_AGENT_CHANNEL, None, "Scribe", ChannelKind::Text, "a2"),
    ] {
        let value = entry(&mut doc, "channels", id, key);
        write_key(&mut doc, &value, "workspace", &Id::from_raw("ws_home")).unwrap();
        write_key(&mut doc, &value, "category", &category.map(Id::from_raw)).unwrap();
        write_key(&mut doc, &value, "name", &name.to_owned()).unwrap();
        write_key(&mut doc, &value, "topic", &String::new()).unwrap();
        write_key(&mut doc, &value, "kind", &kind).unwrap();
        write_key(&mut doc, &value, "archived", &false).unwrap();
        if id == OLD_AGENT_CHANNEL {
            write_key(
                &mut doc,
                &value,
                "conversation_with",
                &Some(Atom("scribe".into())),
            )
            .unwrap();
        }
    }
    doc.commit();
    doc
}

/// The user's chat as version 3 left it: `Voice` named as it should be.
fn users_chat(with_workspace_on_text_category: bool) -> automerge::AutoCommit {
    version_3_starter(
        with_workspace_on_text_category,
        automerge::ScalarValue::Str("Voice".into()),
    )
}

/// Every key of the collection `field` in `doc`: its items' ids.
fn stored_ids(doc: &automerge::AutoCommit, field: &str) -> Vec<String> {
    use automerge::ReadDoc as _;
    let (_, map) = doc.get(automerge::ROOT, field).unwrap().unwrap();
    doc.keys(&map).collect()
}

/// The room's channels, each with its category's name, as the record
/// lists them.
fn channels_with_categories(chat: &Chat) -> Vec<(String, String, Option<String>)> {
    chat.channels
        .iter()
        .map(|(id, channel)| {
            (
                id.to_string(),
                channel.name.clone(),
                channel
                    .category
                    .as_ref()
                    .and_then(|category| chat.categories.get(category))
                    .map(|category| category.name.clone()),
            )
        })
        .collect()
}

// USER REQUIREMENT (2026-10-19): "Chat runs *in a workspace*, not the other way around."
// The user's chat did not open: "this cannot be opened",
// "categories.cat_home_text.v.workspace: required value is missing". And:
// "why are you *deleting* existing stuff?" — the migration deletes nothing
// the person has and adds nothing to a room that has channels.
#[test]
fn the_migration_opens_the_users_chat_with_everything_in_it() {
    use ui::tilo_resource::evolution;
    for whole in [false, true] {
        let mut doc = users_chat(whole);
        assert!(
            evolution::migrate::<Chat>(&mut doc).unwrap(),
            "migrated once"
        );
        assert_eq!(evolution::stored_version(&doc), <Chat as Resource>::VERSION);
        let chat = evolution::read_checked::<Chat>(&doc).expect("the user's chat opens");
        assert!(chat.categories.quarantined().is_empty());
        assert!(chat.channels.quarantined().is_empty());
        assert_eq!(
            channels_with_categories(&chat),
            [
                (
                    "c_general".to_owned(),
                    "general".to_owned(),
                    Some("Channels".to_owned())
                ),
                (
                    "c_lounge".to_owned(),
                    "Lounge".to_owned(),
                    Some("Voice".to_owned())
                ),
                (OLD_AGENT_CHANNEL.to_owned(), "Scribe".to_owned(), None),
            ],
            "every channel stays, in the category it was in"
        );
        assert_eq!(
            chat.categories
                .iter()
                .map(|(id, category)| (id.to_string(), category.name.clone()))
                .collect::<Vec<_>>(),
            [
                ("cat_home_text".to_owned(), "Channels".to_owned()),
                ("cat_home_voice".to_owned(), "Voice".to_owned()),
            ],
            "every category stays, the one that did not read among them"
        );
        assert!(
            !chat.channels.contains(&id(CHAT_CHANNEL))
                && !chat.channels.contains(&id(VOICE_CHANNEL)),
            "a room that has channels gets no #chat or #voice"
        );
        assert!(
            automerge::ReadDoc::get(&doc, automerge::ROOT, "workspaces")
                .unwrap()
                .is_none(),
            "only the workspace structure goes"
        );
    }
}

// USER REQUIREMENT (2026-10-19): "not at all what IDs are for" — ids are
// stable identity: removing the workspace layer rewrites none of them.
#[test]
fn every_id_in_the_users_chat_survives_the_migration_and_its_messages_still_show() {
    use ui::tilo_resource::evolution;
    let mut doc = users_chat(false);
    let before: Vec<Vec<String>> = ["channels", "categories"]
        .iter()
        .map(|field| stored_ids(&doc, field))
        .collect();
    evolution::migrate::<Chat>(&mut doc).unwrap();
    let after: Vec<Vec<String>> = ["channels", "categories"]
        .iter()
        .map(|field| stored_ids(&doc, field))
        .collect();
    assert_eq!(after, before, "no channel or category is re-keyed");

    // Messages live in parts the migration never opens; under their old
    // channel ids they show where they were written.
    let chat = evolution::read_checked::<Chat>(&doc).unwrap();
    let mut part = ChatPart::default();
    for (message, channel, text) in [
        ("m_general", "c_general", "said in general"),
        ("m_scribe", OLD_AGENT_CHANNEL, "said to the agent"),
    ] {
        part.messages.push(
            id(message),
            crate::record::Message {
                channel: id(channel),
                author: Atom(ADA.into()),
                text: text.into(),
                ..crate::record::Message::default()
            },
        );
    }
    let held = Messages::single(part);
    for (channel, text, message) in [
        ("c_general", "said in general", "m_general"),
        (OLD_AGENT_CHANNEL, "said to the agent", "m_scribe"),
    ] {
        let rows = chat.message_rows(&held, channel);
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.text.as_str()))
                .collect::<Vec<_>>(),
            [(message, text)]
        );
    }
}

/// The agent conversation the user had keeps its id: opening it again
/// shows what was said there.
#[test]
fn an_agent_conversation_reopens_under_its_existing_id_with_its_history() {
    use ui::tilo_resource::evolution;
    let mut doc = users_chat(false);
    evolution::migrate::<Chat>(&mut doc).unwrap();
    let mut room = crate::demo::Room::of(doc);
    room.say(ADA, OLD_AGENT_CHANNEL, "said to the agent")
        .unwrap();
    let mut app = crate::app::ChatApp::new_ephemeral();
    app.attach_room(&room);
    app.room_agents = vec!["scribe".into()];
    app.reload(&room);
    assert!(app.open_agent_conversation("scribe"));
    assert_eq!(
        app.active_channel, OLD_AGENT_CHANNEL,
        "the conversation the chat holds, not a new id"
    );
    app.reload(&room);
    assert!(
        app.messages
            .iter()
            .any(|message| message.text == "said to the agent"),
        "its history shows"
    );
}

/// A category that does not read at all is kept as it is, held out of the
/// chat for its repair, through the migration and every later write.
#[test]
fn an_unreadable_category_is_kept_through_the_migration() {
    use ui::tilo_resource::evolution;
    let mut doc = version_3_starter(false, automerge::ScalarValue::Int(5));
    evolution::migrate::<Chat>(&mut doc).unwrap();
    let chat = evolution::read_checked::<Chat>(&doc).expect("the chat opens");
    assert_eq!(
        chat.categories
            .quarantined()
            .iter()
            .map(|held| held.id.as_str())
            .collect::<Vec<_>>(),
        ["cat_home_voice"]
    );
    assert_eq!(chat.channels.len(), 3, "every channel stays");
    let mut ada = Replica::<Chat>::from_doc(doc).as_author(ADA);
    ada.apply(Chat::INTRODUCE.with(chat::Introduce {
        name: "Ada".into(),
        initials: "AD".into(),
        color: 1,
    }))
    .unwrap();
    assert!(
        stored_ids(ada.doc(), "categories").contains(&"cat_home_voice".to_owned()),
        "a later write keeps it"
    );
    assert_eq!(ada.state().unwrap().categories.quarantined().len(), 1);
}
