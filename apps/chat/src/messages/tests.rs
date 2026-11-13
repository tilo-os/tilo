use std::rc::Rc;

use ui::framework::Chain;
use ui::tilo_resource::Id;

use super::*;
use crate::demo::Workspace;
use crate::parts::{PART_TEXT_BYTES, part_index};

/// A post as long as one may be: a part fills after a few dozen of them.
fn long_post(n: usize) -> String {
    let line = format!("post {n} ");
    line.repeat(MAX_MESSAGE_TEXT_BYTES / line.len())
}

/// Post [`long_post`]s in `#general` until one no longer fits part
/// `part`; that post's id, every one before it checked to be in `part`.
fn fill_part(workspace: &mut Workspace, part: u64) -> String {
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        let id = workspace.say("u_aria", "c_general", &long_post(n)).unwrap();
        match workspace.messages().part_of(&Id::from_raw(&id)) {
            Some(at) if at == part => {}
            _ => return id,
        }
    }
    panic!("part {part} never filled");
}

fn react(message: &str, emoji: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::paths()
        .messages
        .item(&Id::from_raw(message))
        .reactions
        .react(emoji)
        .into()
}

fn edit(message: &str, text: &str) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::EDIT_MESSAGE.with(chat_part::EditMessage {
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
        .say(crate::model::ME, "c_general", "mine, to edit later")
        .unwrap();
    assert_eq!(workspace.messages().part_of(&Id::from_raw(&mine)), Some(0));
    // Long posts fill the head; the one after it is full starts part 1.
    let later = fill_part(&mut workspace, 0);
    assert_eq!(workspace.messages().part_of(&Id::from_raw(&later)), Some(1));
    assert_eq!(workspace.part_numbers(), vec![0, 1]);

    // A reaction, an edit and a reply to messages of the full part go to it.
    assert_eq!(
        workspace
            .apply_part_as(crate::model::ME, react(welcome, "🎉"))
            .unwrap(),
        0
    );
    assert_eq!(
        workspace
            .apply_part_as(crate::model::ME, edit(&mine, "mine, edited"))
            .unwrap(),
        0
    );
    let reply = crate::demo::post("c_general", "a reply in the old thread", Some(welcome));
    assert_eq!(workspace.apply_part_as("u_sora", reply).unwrap(), 0);
    // A post that is not a reply goes on in the head.
    assert_eq!(
        workspace
            .apply_part_as(
                "u_sora",
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
    assert!(workspace.say("u_aria", "c_standup", "hello?").is_err());
    assert!(workspace.say("u_aria", "c_nowhere", "hello?").is_err());
    // A mention is resolved against the record's people, whatever the
    // caller sent.
    let mut post = crate::demo::post("c_general", "@Aria Chen look", None);
    post.arguments.insert(
        "mentions".into(),
        serde_json::json!([{ "user": "u_nobody", "start": 0, "len": 3, "is_agent": false }]),
    );
    workspace.apply_part_as("u_milo", post).unwrap();
    let row = workspace.message_rows("c_general").pop().unwrap();
    assert_eq!(row.mentions.len(), 1);
    assert_eq!(row.mentions[0].user, "u_aria");
}

#[test]
fn the_window_chain_rolls_over_and_reads_across_its_parts() {
    let chat = Rc::new(crate::demo::demo(crate::model::ME));
    let chain: Chain<ChatPart> =
        Chain::detached("so.tilo.chat", crate::model::ME, move || Rc::clone(&chat)).unwrap();
    chain
        .write_start(0, &crate::demo::demo_part(crate::model::ME))
        .unwrap();
    assert_eq!(chain.head(), Some(0));
    let mut next = 0;
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        next = chain
            .stage_as(
                &crate::demo::post("c_general", &long_post(n), None),
                "u_aria",
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
    let rows = crate::demo::demo(crate::model::ME).message_rows(&held, "c_general");
    assert_eq!(rows.last().unwrap().text, "the first of part 1");
    assert!(
        rows.iter()
            .find(|row| row.id == "m_welcome")
            .unwrap()
            .reactions
            .iter()
            .any(|reaction| reaction.emoji == "👍"
                && reaction.includes(crate::model::ME)
                && reaction.count() == 1)
    );
}

#[test]
fn read_marks_are_positions_in_the_chain() {
    let mut workspace = Workspace::demo().unwrap();
    let general = Id::from_raw("c_general");
    fill_part(&mut workspace, 0);
    workspace.say("u_milo", "c_general", "in part 1").unwrap();
    let held = workspace.messages();
    let end = held.read_to_end(&general);
    assert_eq!(end, crate::parts::ReadMark { part: 1, seen: 2 });
    assert_eq!(held.unread(&general, end), 0);
    let before = crate::parts::ReadMark { part: 0, seen: 1 };
    assert_eq!(held.unread(&general, before), held.top_level(&general) - 1);
    workspace
        .apply_as(
            crate::model::ME,
            crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
                channel: general.clone(),
                part: 1,
                seen: 2,
            }),
        )
        .unwrap();
    assert_eq!(
        workspace
            .read()
            .unread(&held, "c_general", crate::model::ME),
        0
    );
    // A mark never moves back.
    workspace
        .apply_as(
            crate::model::ME,
            crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
                channel: general,
                part: 0,
                seen: 1,
            }),
        )
        .unwrap();
    assert_eq!(
        workspace.read().read_mark("c_general", crate::model::ME),
        end
    );
}

/// Two devices in one fresh workspace, each with a store of its own and a chain
/// held in it: the poster's, and a guest's whose window was open before
/// anyone posted.
struct TwoDevices {
    workspace: String,
    poster: Chain<ChatPart>,
    guest: Chain<ChatPart>,
    guest_store: ui::record::RecordStore,
    guest_backend: Rc<durable_store::MemoryStore>,
    /// The record the guest's window shows beside the chain.
    shown: ui::record::RecordId,
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
        let chat = Rc::new(crate::demo::demo(crate::model::ME));
        let device = |backend: &Rc<durable_store::MemoryStore>| {
            ui::record::RecordOwners::with_store(
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
            crate::model::ME,
            base(),
        )
        .unwrap();
        let guest_backend = Rc::new(durable_store::MemoryStore::new());
        let guest_store = device(&guest_backend);
        if asks_workspace {
            guest_store.ask_workspace_for_missing();
        }
        let guest =
            Chain::detached_in(guest_store.clone(), "so.tilo.chat", "u_aria", base()).unwrap();
        assert_eq!(guest.head(), None, "a fresh workspace has no part yet");
        let shown = ui::record::RecordCheckout::new(
            ui::record::RecordId::derived("shown", &ui::record::RecordId::workspace_default()),
            ui::record::RecordKind::new("test.shown"),
        );
        guest_store
            .reserve_record(&shown)
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

    /// The guest window's sync: it reads the record it shows, which holds
    /// the store meanwhile, and the chain's revision.
    fn guest_syncs(&self) -> u64 {
        self.guest_store
            .read(&self.shown, |_| self.guest.revision())
            .unwrap()
    }

    /// The poster's first post, its part kept in the guest's store as the
    /// workspace's replication keeps a record no window there has open.
    fn first_post_reaches_the_guest(&self) {
        use durable_store::DurableStore as _;
        self.poster
            .invoke(&crate::demo::post("c_general", "the first post", None))
            .unwrap();
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
        crate::demo::demo(crate::model::ME)
            .message_rows(&Messages::new(self.guest.parts()), "c_general")
            .into_iter()
            .map(|row| row.text)
            .collect()
    }
}

/// The first post of a workspace starts part 0 on the poster's device. It reaches
/// a guest whose window already holds the chain by being kept in the
/// guest's store, which whatever kept it tells the store
/// ([`ui::record::RecordStore::note_kept`]). Nothing looks on a clock: until
/// told, the window opens nothing, however long it waits.
fn a_kept_part_waits_for_word_of_its_arrival_not_a_clock() {
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

/// A store told that a record arrived ([`ui::record::RecordStore::note_kept`],
/// as the browser's workspace replication tells it) has the window look at its
/// next settle, not a moment later.
fn a_store_told_of_a_kept_part_has_the_window_look_at_once() {
    let devices = TwoDevices::new();
    let seen = devices.guest_syncs();
    devices.first_post_reaches_the_guest();
    devices.guest_store.note_kept();
    assert_eq!(devices.guest_syncs(), seen, "a sync opens nothing");
    assert_ne!(devices.guest.revision(), seen, "the settle opens the part");
    assert_eq!(devices.guest_reads().last().unwrap(), "the first post");
}

/// On a desktop the workspace does not keep a part another device started: the
/// guest's window is told a peer wrote it, starts the same part itself
/// (each part's genesis is the same everywhere), and the poster's changes
/// then apply on it with nothing missing, as they arrive on its route.
fn a_part_a_peer_started_is_started_here_and_takes_the_peers_changes() {
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

/// How a part another device started reaches a window that already holds the chain.
/// Each case was a test of its own, on a fixture of its own; a failure
/// names the cases that failed.
#[test]
fn a_part_reaches_another_device_when_its_store_is_told() {
    crate::testing::cases!(
        a_kept_part_waits_for_word_of_its_arrival_not_a_clock,
        a_store_told_of_a_kept_part_has_the_window_look_at_once,
        a_part_a_peer_started_is_started_here_and_takes_the_peers_changes,
    );
}

/// A direct conversation's unread count follows the chain, as the read mark
/// does, though a message written elsewhere carries an earlier time than the
/// message before it: a clock behind another device's does not make a message
/// read.
fn unread_messages_are_counted_in_the_order_the_part_holds_them() {
    let channel = Id::from_raw("c_direct");
    let mut part = ChatPart::default();
    for (id, author, ts) in [
        ("m_one", "ada", 100),
        ("m_two", "bo", 50),
        ("m_three", "bo", 200),
    ] {
        part.messages.push(
            Id::from_raw(id),
            crate::record::Message {
                channel: channel.clone(),
                author: ui::tilo_resource::Atom(author.to_owned()),
                ts,
                ..crate::record::Message::default()
            },
        );
    }
    let held = Messages::single(part);
    assert_eq!(
        held.unread_by_others(&channel, crate::parts::ReadMark { part: 0, seen: 1 }, "ada"),
        2,
        "the two after the mark, though one was written earlier"
    );
    let first = held.first_unread(&channel, crate::parts::ReadMark { part: 0, seen: 1 });
    assert_eq!(first.map(ToString::to_string).as_deref(), Some("m_two"));
    assert_eq!(
        held.unread_by_others(&channel, crate::parts::ReadMark { part: 0, seen: 3 }, "ada"),
        0
    );
}

/// Two devices write offline from the same part, take each other's changes
/// in the opposite order, and write again: both read one timeline, in the
/// same order. The order is the one the part records hold
/// ([`Messages::iter`]), which every replica computes from the same records,
/// so it stays the same whether a change arrives before or after another.
fn replicas_taking_the_same_posts_in_opposite_orders_read_one_timeline() {
    let chat = crate::demo::demo(crate::model::ME);
    // A part holding one message written with a time after every post that
    // follows: a read that ordered by a clock would show it last.
    let mut seeded = ChatPart::default();
    seeded.messages.push(
        Id::from_raw("m_seeded"),
        crate::record::Message {
            channel: Id::from_raw("c_general"),
            author: ui::tilo_resource::Atom("u_aria".to_owned()),
            ts: 4_000_000_000,
            text: "seeded".to_owned(),
            ..crate::record::Message::default()
        },
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
    // The same second, so the wall clock cannot tell these apart.
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
    let in_record: Vec<String> = read_part(&ada)
        .messages
        .ids()
        .map(ToString::to_string)
        .collect();
    assert_eq!(ours, in_record, "in the order the records hold them");
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
}

/// A post's step says which channel it went to, as everyone sees it
/// named: what the automatic status reads to say "Writing in #general".
fn a_post_records_the_channel_it_went_to() {
    let mut workspace = Workspace::demo().unwrap();
    workspace
        .say("u_aria", "c_general", "where this went")
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

/// The order messages are read and counted in, and the place a post records.
/// Each case was a test of its own, on a fixture of its own; a failure
/// names the cases that failed.
#[test]
fn the_timeline_and_unread_counts_follow_the_order_the_parts_hold() {
    crate::testing::cases!(
        unread_messages_are_counted_in_the_order_the_part_holds_them,
        replicas_taking_the_same_posts_in_opposite_orders_read_one_timeline,
        a_post_records_the_channel_it_went_to,
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
    let mut doc = crate::testing::demo_doc();
    let mut app = crate::app::ChatApp::new_ephemeral();
    app.attach_chain(devices.guest.as_reader(crate::model::ME));
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

    devices.first_post_reaches_the_guest();
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
