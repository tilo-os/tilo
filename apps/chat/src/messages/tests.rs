use crate::projection::ChatProjection as _;
use std::rc::Rc;
use ui::tilo_resource::MemberId;

use ui::framework::Chain;
use ui::tilo_resource::Id;

use super::*;
use crate::demo::Workspace;
use crate::parts::part_index;
use ui::framework::PART_BYTES as PART_TEXT_BYTES;

/// A post as long as one may be: a part fills after a few dozen of them.
fn long_post(n: usize) -> String {
    let line = format!("post {n} ");
    line.repeat(MAX_MESSAGE_TEXT_BYTES / line.len())
}

/// Post [`long_post`]s in `#general` until one no longer fits part
/// `part`; that post's id, every one before it checked to be in `part`.
fn fill_part(workspace: &mut Workspace, part: u64) -> String {
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        let id = workspace
            .say(&MemberId::person("u_aria"), "c_general", &long_post(n))
            .unwrap();
        match workspace.messages().part_of(&Id::from_raw(&id)) {
            Some(at) if at == part => {}
            _ => return id,
        }
    }
    panic!("part {part} never filled");
}

fn react(message: &str, emoji: &str) -> ui::tilo_resource::Invoke<ChatMessages> {
    ChatMessages::paths()
        .messages
        .item(&Id::from_raw(message))
        .reactions
        .react(emoji)
        .into()
}

fn edit(message: &str, text: &str) -> ui::tilo_resource::Invoke<ChatMessages> {
    ChatMessages::EDIT_MESSAGE.with(chat_messages::EditMessage {
        message: Id::from_raw(message),
        text: text.into(),
        body: None,
        mentions: None,
    })
}

#[test]
fn a_full_part_rolls_posts_over_and_changes_land_where_the_message_is() {
    let mut workspace = Workspace::demo().unwrap();
    let welcome = "m_welcome";
    assert_eq!(
        workspace.messages().part_of(&Id::from_raw(welcome)),
        Some(0)
    );
    let mine = workspace
        .say(
            &MemberId::person(crate::model::ME),
            "c_general",
            "mine, to edit later",
        )
        .unwrap();
    assert_eq!(workspace.messages().part_of(&Id::from_raw(&mine)), Some(0));
    // Long posts fill the head; the one after it is full starts part 1.
    let later = fill_part(&mut workspace, 0);
    assert_eq!(workspace.messages().part_of(&Id::from_raw(&later)), Some(1));
    assert_eq!(workspace.part_numbers(), vec![0, 1]);

    // A device retrying a named post still finds the original in the full part.
    let mut retry = crate::demo::post("c_general", "mine, to edit later", None);
    retry.arguments.insert("id".into(), mine.clone().into());
    let refusal = workspace
        .apply_part_as(&MemberId::person(crate::model::ME), retry)
        .expect_err("the same message cannot also be written to part 1");
    assert!(
        refusal
            .to_string()
            .contains("a message with that id is already here")
    );

    // A reaction, an edit and a reply to messages of the full part go to it.
    assert_eq!(
        workspace
            .apply_part_as(&MemberId::person(crate::model::ME), react(welcome, "🎉"))
            .unwrap(),
        0
    );
    assert_eq!(
        workspace
            .apply_part_as(
                &MemberId::person(crate::model::ME),
                edit(&mine, "mine, edited")
            )
            .unwrap(),
        0
    );
    let reply = crate::demo::post("c_general", "a reply in the old thread", Some(welcome));
    assert_eq!(
        workspace
            .apply_part_as(&MemberId::person("u_sora"), reply)
            .unwrap(),
        0
    );
    // A post that is not a reply goes on in the head.
    assert_eq!(
        workspace
            .apply_part_as(
                &MemberId::person("u_sora"),
                crate::demo::post("c_general", "still the head", None)
            )
            .unwrap(),
        1
    );

    // A reader sees the channel across both parts, in order.
    let rows = workspace.message_rows("c_general");
    let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids.first(), Some(&welcome));
    let at = |id: &str| ids.iter().position(|row| *row == id).unwrap();
    assert!(at(&mine) < at(&later));
    let welcome_row = rows.iter().find(|row| row.id == welcome).unwrap();
    assert!(
        welcome_row
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == "🎉")
    );
    assert_eq!(
        rows.iter().find(|row| row.id == mine).unwrap().text,
        "mine, edited"
    );
    assert!(
        rows.iter()
            .any(|row| row.parent.as_deref() == Some(welcome))
    );
    assert_eq!(rows.last().unwrap().text, "still the head");
}

#[test]
fn a_post_is_read_against_the_chat_record_where_it_is_invoked() {
    let mut workspace = Workspace::demo().unwrap();
    // A voice channel takes no posts, and an unknown channel none either.
    assert!(
        workspace
            .say(&MemberId::person("u_aria"), "c_standup", "hello?")
            .is_err()
    );
    assert!(
        workspace
            .say(&MemberId::person("u_aria"), "c_nowhere", "hello?")
            .is_err()
    );
    // A mention is resolved against the resource's people, whatever the
    // caller sent.
    let mut post = crate::demo::post("c_general", "@Aria Chen look", None);
    post.arguments.insert(
        "mentions".into(),
        serde_json::json!([{ "user": { "person": "u_nobody" }, "start": 0, "len": 3, "is_agent": false }]),
    );
    workspace
        .apply_part_as(&MemberId::person("u_milo"), post)
        .unwrap();
    let row = workspace.message_rows("c_general").pop().unwrap();
    assert_eq!(row.mentions.len(), 1);
    assert_eq!(row.mentions[0].user, MemberId::person("u_aria"));
}

#[test]
fn the_window_chain_rolls_over_and_reads_across_its_parts() {
    let chat = Rc::new(crate::demo::demo());
    let chain: Chain<ChatMessages> = Chain::detached(
        "so.tilo.chat",
        &ui::tilo_resource::MemberId::person(crate::model::ME),
        move || Rc::clone(&chat),
    )
    .unwrap();
    chain
        .write_start(0, &crate::demo::demo_part(crate::model::ME))
        .unwrap();
    assert_eq!(chain.head(), Some(0));
    let mut next = 0;
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        next = chain
            .stage_as(
                &crate::demo::post("c_general", &long_post(n), None),
                &MemberId::person("u_aria"),
            )
            .unwrap()
            .expect("written into a part held here");
        if next != 0 {
            break;
        }
    }
    assert_eq!(next, 1, "a full head rolls the next post over");
    let first = chain
        .invoke(&crate::demo::post("c_general", "the first of part 1", None))
        .unwrap();
    assert_eq!(first, Some(1), "posts go on in the new head");
    assert_eq!(chain.head(), Some(1));
    // The new part is the chain's part 1, as every device names it.
    assert_eq!(part_index(&crate::parts::part(1)), Some(1));
    // A reaction to the demo's first message lands in part 0.
    assert_eq!(chain.invoke(&react("m_welcome", "👍")).unwrap(), Some(0));
    let held = Messages::new(chain.parts());
    assert_eq!(held.parts().len(), 2);
    assert_eq!(held.part_of(&Id::from_raw("m_welcome")), Some(0));
    let rows = crate::demo::demo().message_rows(&held, "c_general");
    assert_eq!(rows.last().unwrap().text, "the first of part 1");
    assert!(
        rows.iter()
            .find(|row| row.id == "m_welcome")
            .unwrap()
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == "👍"
                && reaction.includes(&ui::tilo_resource::MemberId::person(crate::model::ME))
                && reaction.count() == 1)
    );
}

#[test]
fn read_marks_are_positions_in_the_chain() {
    let mut workspace = Workspace::demo().unwrap();
    let general = Id::from_raw("c_general");
    fill_part(&mut workspace, 0);
    workspace
        .say(&MemberId::person("u_milo"), "c_general", "in part 1")
        .unwrap();
    let held = workspace.messages();
    let end = held.read_to_end(&general);
    assert_eq!(end, crate::parts::ReadMark { part: 1, seen: 2 });
    assert_eq!(held.unread(&general, end), 0);
    let before = crate::parts::ReadMark { part: 0, seen: 1 };
    assert_eq!(held.unread(&general, before), held.top_level(&general) - 1);
    workspace
        .apply_as(
            crate::model::ME,
            crate::resource::Chat::MARK_READ.with(crate::chat::chat::MarkRead {
                channel: general.clone(),
                part: 1,
                seen: 2,
            }),
        )
        .unwrap();
    assert_eq!(
        workspace
            .read()
            .unread(&held, "c_general", &MemberId::person(crate::model::ME)),
        0
    );
    // A mark never moves back.
    workspace
        .apply_as(
            crate::model::ME,
            crate::resource::Chat::MARK_READ.with(crate::chat::chat::MarkRead {
                channel: general,
                part: 0,
                seen: 1,
            }),
        )
        .unwrap();
    assert_eq!(
        workspace
            .read()
            .read_mark("c_general", &MemberId::person(crate::model::ME)),
        end
    );
}

/// Two devices in one fresh workspace, each with a store of its own and a chain
/// held in it: the poster's, and a guest's whose window was open before
/// anyone posted.
struct TwoDevices {
    workspace: String,
    poster: Chain<ChatMessages>,
    guest: Chain<ChatMessages>,
    guest_store: ui::resource::ResourceStore,
    guest_backend: Rc<durable_store::MemoryStore>,
    /// The resource the guest's window shows beside the chain.
    shown: ui::resource::DocumentId,
}

impl TwoDevices {
    fn new() -> Self {
        Self::opened(false)
    }

    /// [`Self::new`], the guest back from away: what it does not hold yet,
    /// it asks the workspace for, and waits for.
    fn back_from_away() -> Self {
        Self::opened(true)
    }

    fn opened(asks_workspace: bool) -> Self {
        let workspace = uuid::Uuid::new_v4().to_string();
        let chat = Rc::new(crate::demo::demo());
        let device = |backend: &Rc<durable_store::MemoryStore>| {
            ui::resource::ResourceOwners::with_store(
                Rc::clone(backend) as Rc<dyn durable_store::DurableStore>
            )
            .open("so.tilo.chat", &workspace)
            .unwrap()
        };
        let base = || {
            let chat = Rc::clone(&chat);
            move || Rc::clone(&chat)
        };
        let poster = Chain::detached_in(
            device(&Rc::new(durable_store::MemoryStore::new())),
            "so.tilo.chat",
            &ui::tilo_resource::MemberId::person(crate::model::ME),
            base(),
        )
        .unwrap();
        let guest_backend = Rc::new(durable_store::MemoryStore::new());
        let guest_store = device(&guest_backend);
        if asks_workspace {
            guest_store.ask_workspace_for_missing();
        }
        let guest = Chain::detached_in(
            guest_store.clone(),
            "so.tilo.chat",
            &ui::tilo_resource::MemberId::person("u_aria"),
            base(),
        )
        .unwrap();
        assert_eq!(guest.head(), None, "a fresh workspace has no part yet");
        let shown = ui::resource::DocumentCheckout::new(
            ui::resource::DocumentId::derived(
                "shown",
                &ui::resource::DocumentId::workspace_default(),
            ),
            ui::resource::ResourceType::new("test.shown"),
        );
        guest_store
            .reserve_resource(&shown)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        Self {
            workspace,
            poster,
            guest,
            guest_store,
            guest_backend,
            shown: shown.id,
        }
    }

    /// The guest window's sync: it reads the resource it shows, which holds
    /// the store meanwhile, and the chain's revision.
    fn guest_syncs(&self) -> u64 {
        self.guest_store
            .read(&self.shown, |_| self.guest.revision())
            .unwrap()
    }

    /// The poster's first post, written: what the guest has not been handed yet.
    fn write_first_post(&self) {
        self.poster
            .invoke(&crate::demo::post("c_general", "the first post", None))
            .unwrap();
    }

    /// The poster's part kept in the guest's store, as the workspace's
    /// replication keeps a resource no window there has open.
    fn first_post_reaches_the_guest(&self) {
        self.write_first_post();
        self.deliver_first_post();
    }

    /// [`Self::first_post_reaches_the_guest`] for a post [`Self::write_first_post`] wrote before
    /// something else was written here.
    fn deliver_first_post(&self) {
        use durable_store::DurableStore as _;
        self.guest_backend
            .write(
                &durable_store::StorageKey::document(
                    "so.tilo.chat",
                    &self.workspace,
                    crate::parts::part(0).storage_key(),
                ),
                durable_store::WriteOp::Replace(self.poster.part_bytes(0).unwrap()),
                None,
            )
            .unwrap();
    }

    /// What the guest's window shows of `#general`.
    fn guest_reads(&self) -> Vec<String> {
        crate::demo::demo()
            .message_rows(&Messages::new(self.guest.parts()), "c_general")
            .into_iter()
            .map(|row| row.text)
            .collect()
    }
}

tilo_test_env::scenario! {
    /// How a part another device started reaches a window that already holds the chain.
    /// Each case was a test of its own, on a fixture of its own; a failure
    /// names the cases that failed.
    fn a_part_reaches_another_device_when_its_store_is_told;

    phase a_kept_part_waits_for_word_of_its_arrival_not_a_clock;
    // The first post of a workspace starts part 0 on the poster's device. It reaches
    // a guest whose window already holds the chain by being kept in the
    // guest's store, which whatever kept it tells the store
    // ([`ui::resource::ResourceStore::note_kept`]). Nothing looks on a clock: until
    // told, the window opens nothing, however long it waits.
    {
        let devices = TwoDevices::new();
        let seen = devices.guest_syncs();
        devices.first_post_reaches_the_guest();
        let later = ui::time::now() + std::time::Duration::from_secs(60);
        ui::time::testing::with_monotonic_time(later, || {
            assert_eq!(devices.guest_syncs(), seen);
            assert_eq!(devices.guest.revision(), seen, "no clock looks for it");
            assert_eq!(devices.guest.head(), None);
        });
        devices.guest_store.note_kept();
        assert_eq!(devices.guest.head(), Some(0));
        assert_eq!(devices.guest_reads().last().unwrap(), "the first post");
    }

    phase a_store_told_of_a_kept_part_has_the_window_look_at_once;
    // A store told that a resource arrived ([`ui::resource::ResourceStore::note_kept`],
    // as the browser's workspace replication tells it) has the window look at its
    // next settle, not a moment later.
    {
        let devices = TwoDevices::new();
        let seen = devices.guest_syncs();
        devices.first_post_reaches_the_guest();
        devices.guest_store.note_kept();
        assert_eq!(devices.guest_syncs(), seen, "a sync opens nothing");
        assert_ne!(devices.guest.revision(), seen, "the settle opens the part");
        assert_eq!(devices.guest_reads().last().unwrap(), "the first post");
    }

    phase a_part_a_peer_started_is_started_here_and_takes_the_peers_changes;
    // On a desktop the workspace does not keep a part another device started: the
    // guest's window is told a peer wrote it, starts the same part itself
    // (each part's genesis is the same everywhere), and the poster's changes
    // then apply on it with nothing missing, as they arrive on its route.
    {
        let devices = TwoDevices::new();
        devices
            .poster
            .invoke(&crate::demo::post("c_general", "the first post", None))
            .unwrap();
        let seen = devices.guest_syncs();
        devices.guest_store.announce([crate::parts::part(0)]);
        assert_eq!(devices.guest_syncs(), seen, "a sync opens nothing");
        assert_eq!(devices.guest.head(), Some(0), "the settle starts the part");
        let mut written =
            ui::automerge::AutoCommit::load(&devices.poster.part_bytes(0).unwrap()).unwrap();
        let missing = devices
            .guest_store
            .with(&crate::parts::part(0), |doc| {
                let changes = written.get_changes(&doc.get_heads());
                tilo_automerge::ApplyIncoming::apply_incoming(doc, changes).unwrap();
                doc.get_missing_deps(&[])
            })
            .unwrap();
        assert!(
            missing.is_empty(),
            "the poster's part builds on the same genesis"
        );
        assert_eq!(devices.guest_reads().last().unwrap(), "the first post");
    }
}

tilo_test_env::scenario! {
    /// The order messages are read and counted in, and the place a post resources.
    /// Each case was a test of its own, on a fixture of its own; a failure
    /// names the cases that failed.
    fn the_timeline_and_unread_counts_follow_the_order_the_messages_were_written_in;

    phase unread_messages_are_counted_where_they_were_written;
    // The part holds these in the order they were placed in it; the conversation reads them by
    // when each was written, and a read mark is a place in that order.
    {
        let channel = Id::from_raw("c_direct");
        let mut part = ChatMessages::default();
        for (id, author, ts) in [
            ("m_one", "ada", 100),
            ("m_two", "bo", 50),
            ("m_three", "bo", 200),
        ] {
            part.messages.push(
                Id::from_raw(id),
                crate::chat::Message::new(
                    channel.clone(),
                    MemberId::person(author),
                    ts,
                    String::new(),
                ),
            );
        }
        let held = Messages::single(part);
        let order: Vec<&str> = held.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(order, ["m_two", "m_one", "m_three"], "oldest written first");
        let read = crate::parts::ReadMark { part: 0, seen: 1 };
        assert_eq!(
            held.first_unread(&channel, read)
                .map(ToString::to_string)
                .as_deref(),
            Some("m_one"),
            "the first one written after the mark"
        );
        assert_eq!(
            held.unread_by_others(&channel, read, &MemberId::person("ada")),
            1,
            "the one after the mark that Bo wrote"
        );
        assert_eq!(
            held.unread_by_others(&channel, crate::parts::ReadMark { part: 0, seen: 3 }, &MemberId::person("ada")),
            0
        );
    }

    phase replicas_taking_the_same_posts_in_opposite_orders_read_one_timeline;
    // Two devices write offline from the same part, take each other's changes
    // in the opposite order, and write again: both read one timeline, in the
    // same order. The order is when each message was written ([`Messages::iter`]),
    // which every replica reads from the messages themselves, so it stays the
    // same whether a change arrives before or after another.
    {
        let chat = crate::demo::demo();
        // A part holding one message written before every post that follows it: it
        // reads first, whichever order the posts reached this replica.
        let mut seeded = ChatMessages::default();
        seeded.messages.push(
            Id::from_raw("m_seeded"),
            crate::chat::Message::new(
                Id::from_raw("c_general"),
                MemberId::person("u_aria"),
                1_000,
                "seeded".to_owned(),
            ),
        );
        let mut workspace = Workspace::default();
        workspace.write_part(0, &seeded).unwrap();
        let base = workspace.part_bytes(0).unwrap();
        let replica = |actor: u8| {
            let mut doc = ui::automerge::AutoCommit::load(&base).unwrap();
            doc.set_actor(ui::automerge::ActorId::from(vec![actor; 16]));
            doc
        };
        let mut ada = replica(1);
        let mut bob = replica(2);
        let say = |doc: &mut ui::automerge::AutoCommit, author: &str, text: &str| {
            crate::demo::apply_part_doc(
                doc,
                &chat,
                author,
                crate::demo::post("c_general", text, None),
            )
            .unwrap();
        };
        let take = |mine: &mut ui::automerge::AutoCommit, theirs: &mut ui::automerge::AutoCommit| {
            let changes = theirs.get_changes(&mine.get_heads());
            tilo_automerge::ApplyIncoming::apply_incoming(mine, changes).unwrap();
        };
        say(&mut ada, "u_aria", "Ada one");
        say(&mut bob, "u_milo", "Bob one");
        // Each takes the other's change before writing again: what Ada has read
        // first, Bob reads second.
        take(&mut ada, &mut bob);
        take(&mut bob, &mut ada);
        say(&mut ada, "u_aria", "Ada two");
        say(&mut bob, "u_milo", "Bob two");
        take(&mut ada, &mut bob);
        take(&mut bob, &mut ada);
        let timeline = |doc: &ui::automerge::AutoCommit| -> Vec<String> {
            chat.message_rows(&Messages::single(read_part(doc)), "c_general")
                .into_iter()
                .map(|message| message.id)
                .collect()
        };
        let ours = timeline(&ada);
        assert_eq!(timeline(&bob), ours, "both devices read one order");
        assert_eq!(
            ours.first().map(String::as_str),
            Some("m_seeded"),
            "the message written first reads first: {ours:?}"
        );
        let unique: std::collections::BTreeSet<&String> = ours.iter().collect();
        assert_eq!(unique.len(), ours.len(), "each message once: {ours:?}");
        let texts: std::collections::BTreeSet<String> = chat
            .message_rows(&Messages::single(read_part(&ada)), "c_general")
            .into_iter()
            .map(|message| message.text)
            .collect();
        for text in ["Ada one", "Ada two", "Bob one", "Bob two"] {
            assert!(texts.contains(text), "{text} missing from {texts:?}");
        }
        // Writing over the converged part (which re-keys whatever two replicas
        // wrote at the same place) keeps both devices' order the same.
        say(&mut ada, "u_aria", "Ada three");
        take(&mut bob, &mut ada);
        assert_eq!(timeline(&ada), timeline(&bob), "and after a write over it");
        let last = chat
            .message_rows(&Messages::single(read_part(&ada)), "c_general")
            .into_iter()
            .next_back()
            .map(|message| message.text);
        assert_eq!(
            last.as_deref(),
            Some("Ada three"),
            "the message written last reads last"
        );
    }

    phase a_post_records_the_channel_it_went_to;
    // A post's step says which channel it went to, as everyone sees it
    // named: what the automatic status reads to say "Writing in #general".
    {
        let mut workspace = Workspace::demo().unwrap();
        workspace
            .say(&MemberId::person("u_aria"), "c_general", "where this went")
            .unwrap();
        let bytes = workspace.part_bytes(0).unwrap();
        let mut doc = ui::automerge::AutoCommit::load(&bytes).unwrap();
        let places: Vec<String> = ui::HistorySource::new()
            .entries(&mut doc)
            .iter()
            .filter_map(|entry| {
                entry
                    .context
                    .as_ref()?
                    .facets
                    .iter()
                    .find(|facet| facet.key == tilo_actions::PLACE_FACET)
                    .map(|facet| facet.value.clone())
            })
            .collect();
        assert!(places.iter().any(|place| place == "#general"), "{places:?}");
    }
}

/// Two devices whose clocks are ten minutes apart read one order: a reply
/// written on the device whose clock is behind reads after the message it
/// answers, because that device had seen it when it wrote.
#[test]
fn a_reply_written_on_a_clock_behind_reads_after_the_message_it_answers() {
    let chat = crate::demo::demo();
    let mut workspace = Workspace::default();
    workspace.write_part(0, &ChatMessages::default()).unwrap();
    let base = workspace.part_bytes(0).unwrap();
    let replica = |actor: u8| {
        let mut doc = ui::automerge::AutoCommit::load(&base).unwrap();
        doc.set_actor(ui::automerge::ActorId::from(vec![actor; 16]));
        doc
    };
    let mut ada = replica(1);
    let mut bob = replica(2);
    let behind = 1_800_000_000_000;
    let ahead = behind + 10 * 60 * 1_000;
    crate::demo::apply_part_doc_as(
        &mut ada,
        &chat,
        |replica| replica.as_invoker("u_aria"),
        Some(ahead),
        crate::demo::post("c_general", "what Ada wrote", None),
    )
    .unwrap();
    let written = read_part(&ada)
        .messages
        .iter()
        .find(|(_, message)| message.text == "what Ada wrote")
        .map(|(id, _)| id.clone())
        .unwrap();
    // Bob takes Ada's message, then answers it on a clock ten minutes behind hers.
    let changes = ada.get_changes(&bob.get_heads());
    tilo_automerge::ApplyIncoming::apply_incoming(&mut bob, changes).unwrap();
    crate::demo::apply_part_doc_as(
        &mut bob,
        &chat,
        |replica| replica.as_invoker("u_milo"),
        Some(behind),
        crate::demo::post("c_general", "Bob's answer", Some(written.as_str())),
    )
    .unwrap();
    let changes = bob.get_changes(&ada.get_heads());
    tilo_automerge::ApplyIncoming::apply_incoming(&mut ada, changes).unwrap();
    let order = |doc: &ui::automerge::AutoCommit| -> Vec<String> {
        Messages::single(read_part(doc))
            .iter()
            .map(|(_, message)| message.text.clone())
            .collect()
    };
    for device in [&ada, &bob] {
        assert_eq!(
            order(device),
            ["what Ada wrote", "Bob's answer"],
            "the answer reads after the message it answers"
        );
    }
    let answer = read_part(&bob)
        .messages
        .iter()
        .find(|(_, message)| message.text == "Bob's answer")
        .map(|(_, message)| message.ts)
        .unwrap();
    assert!(
        answer > ahead,
        "the answer carries a time after what it had seen"
    );
}

/// A device back from away shows a conversation before the part its messages
/// go to arrived from the workspace. A message sent from the conversation's
/// box meanwhile is not refused: it shows at once, as being sent, and is
/// written once the part is here, after what was posted there meanwhile.
#[test]
fn a_message_sent_while_its_part_arrives_shows_as_sending_and_lands_after_it() {
    use crate::app::{Focus, Hit};

    let devices = TwoDevices::back_from_away();
    // The poster wrote while the guest's window was away: that message was written before the
    // one the guest sends, though it reaches the guest's part after it.
    devices.write_first_post();
    let mut doc = crate::testing::demo_doc();
    let mut app = crate::app::ChatApp::new_ephemeral();
    app.attach_chain(
        devices
            .guest
            .as_reader(&ui::tilo_resource::MemberId::person(crate::model::ME)),
    );
    app.active_channel = "c_general".into();
    app.focus = Focus::Main;
    app.thread_open = None;
    crate::testing::write_draft(&mut app, &mut doc, "back again");
    app.on_hit(Hit::Send, &mut ui::widget::Edits::lend(&mut doc));
    assert_eq!(app.not_sent, None, "nothing is refused");
    assert_eq!(
        app.composer_main.value(),
        "",
        "sent, as far as the box knows"
    );
    app.reload(&doc);
    let shown: Vec<(String, bool)> = app
        .messages
        .iter()
        .map(|row| (row.text.clone(), row.sending))
        .collect();
    assert_eq!(
        shown,
        [("back again".to_owned(), true)],
        "shown as being sent"
    );

    devices.deliver_first_post();
    devices.guest_store.note_kept();
    app.reload(&doc);
    let shown: Vec<(String, bool)> = app
        .messages
        .iter()
        .map(|row| (row.text.clone(), row.sending))
        .collect();
    assert_eq!(
        shown,
        [
            ("the first post".to_owned(), false),
            ("back again".to_owned(), false)
        ],
        "written after what was posted meanwhile"
    );
}

/// A send from a box answers which message it posted even while the part its messages go to is
/// still arriving, as it is in a Chat just started to post a canvas ask: what asked stands the
/// turn it opened by that message (canvas.md §3), and the message lands under that id once the
/// part is here.
#[test]
fn a_send_while_its_part_arrives_answers_the_message_it_posts() {
    use ui::floating_surface::{Recipient, RecipientKind};

    let devices = TwoDevices::back_from_away();
    let mut app = crate::app::ChatApp::new_ephemeral();
    app.attach_chain(
        devices
            .guest
            .as_reader(&ui::tilo_resource::MemberId::person(crate::model::ME)),
    );
    let chat = crate::demo::demo();
    let agent = "a5".repeat(16);
    let to = [Recipient::found(RecipientKind::Agent, agent, "Ada")];
    let sent = app
        .send_message(
            crate::direct::SendMessage {
                to: &to,
                attached: Vec::new(),
                text: "what is on my task list?",
                parent: None,
                body: None,
                opens: false,
            },
            &chat,
        )
        .expect("the box sends");
    let ui::framework::Effected::Done(output) = sent else {
        panic!("a send answers at once");
    };
    let message = output
        .value
        .as_ref()
        .and_then(|value| value["message"].as_str())
        .map(str::to_owned)
        .expect("the send names the message it posts though its part is still arriving");

    devices.first_post_reaches_the_guest();
    devices.guest_store.note_kept();
    let doc = crate::testing::demo_doc();
    app.active_channel = output
        .value
        .as_ref()
        .and_then(|value| value["conversation"].as_str())
        .expect("the send names its conversation")
        .to_owned();
    app.reload(&doc);
    let held: Vec<(String, String)> = app
        .messages
        .iter()
        .map(|row| (row.id.clone(), row.text.clone()))
        .collect();
    assert!(
        held.contains(&(message.clone(), "what is on my task list?".to_owned())),
        "the message lands under the id the send answered ({message}): {held:?}"
    );
}

/// History written elsewhere comes into a conversation that already holds a
/// newer message: each imported message keeps the time it was written there
/// and reads in that order among them, says where it came from, and importing
/// it again changes nothing. A post written here after them still reads last.
#[test]
fn imported_history_keeps_its_times_in_a_conversation_holding_newer_messages() {
    let chat = crate::demo::demo();
    let mut workspace = Workspace::default();
    workspace.write_part(0, &ChatMessages::default()).unwrap();
    let mut doc = ui::automerge::AutoCommit::load(&workspace.part_bytes(0).unwrap()).unwrap();
    let today = 1_800_000_000_000;
    let day = 86_400_000;
    let mut write = |now: Option<i64>, invoke: ui::tilo_resource::Invoke<ChatMessages>| {
        crate::demo::apply_part_doc_as(
            &mut doc,
            &chat,
            |replica| replica.as_invoker("u_aria"),
            now,
            invoke,
        )
        .unwrap();
    };
    write(Some(today), crate::demo::post("c_general", "today", None));
    let first = crate::demo::import(
        "c_general",
        "ten days ago",
        ("Ada", "Slack"),
        today - 10 * day,
    );
    write(None, first.clone());
    write(
        None,
        crate::demo::import(
            "c_general",
            "three days ago",
            ("Ada", "Slack"),
            today - 3 * day,
        ),
    );
    write(None, first);
    write(
        Some(today + 1_000),
        crate::demo::post("c_general", "later today", None),
    );
    let part = read_part(&doc);
    let read: Vec<(String, i64)> = Messages::single(part.clone())
        .iter()
        .map(|(_, message)| (message.text.clone(), message.ts))
        .collect();
    assert_eq!(
        read,
        [
            ("ten days ago".to_owned(), today - 10 * day),
            ("three days ago".to_owned(), today - 3 * day),
            ("today".to_owned(), today),
            ("later today".to_owned(), today + 1_000),
        ],
        "each at the time it was written, imported again or not"
    );
    let (_, imported) = part
        .messages
        .iter()
        .find(|(_, message)| message.text == "ten days ago")
        .unwrap();
    assert_eq!(
        imported
            .origin
            .as_ref()
            .map(|origin| origin.source.as_str()),
        Some("Slack"),
        "it says where it came from"
    );
}
