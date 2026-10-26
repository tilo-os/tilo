use std::rc::Rc;

use ui::framework::Chain;
use ui::tilo_resource::Id;

use super::*;
use crate::demo::Room;
use crate::parts::{PART_TEXT_BYTES, part_index};

/// A post as long as one may be: a part fills after a few dozen of them.
fn long_post(n: usize) -> String {
    let line = format!("post {n} ");
    line.repeat(MAX_MESSAGE_TEXT_BYTES / line.len())
}

/// Post [`long_post`]s in `#general` until one no longer fits part
/// `part`; that post's id, every one before it checked to be in `part`.
fn fill_part(room: &mut Room, part: u64) -> String {
    for n in 0..=PART_TEXT_BYTES / long_post(0).len() + 1 {
        let id = room.say("u_aria", "c_general", &long_post(n)).unwrap();
        match room.messages().part_of(&Id::from_raw(&id)) {
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
    let mut room = Room::demo().unwrap();
    let welcome = "m_welcome";
    assert_eq!(room.messages().part_of(&Id::from_raw(welcome)), Some(0));
    let mine = room
        .say(crate::model::ME, "c_general", "mine, to edit later")
        .unwrap();
    assert_eq!(room.messages().part_of(&Id::from_raw(&mine)), Some(0));
    // Long posts fill the head; the one after it is full starts part 1.
    let later = fill_part(&mut room, 0);
    assert_eq!(room.messages().part_of(&Id::from_raw(&later)), Some(1));
    assert_eq!(room.part_numbers(), vec![0, 1]);

    // A reaction, an edit and a reply to messages of the full part go to it.
    assert_eq!(
        room.apply_part_as(crate::model::ME, react(welcome, "🎉"))
            .unwrap(),
        0
    );
    assert_eq!(
        room.apply_part_as(crate::model::ME, edit(&mine, "mine, edited"))
            .unwrap(),
        0
    );
    let reply = crate::demo::post("c_general", "a reply in the old thread", Some(welcome));
    assert_eq!(room.apply_part_as("u_sora", reply).unwrap(), 0);
    // A post that is not a reply goes on in the head.
    assert_eq!(
        room.apply_part_as(
            "u_sora",
            crate::demo::post("c_general", "still the head", None)
        )
        .unwrap(),
        1
    );

    // A reader sees the channel across both parts, in order.
    let rows = room.message_rows("c_general");
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
    let mut room = Room::demo().unwrap();
    // A voice channel takes no posts, and an unknown channel none either.
    assert!(room.say("u_aria", "c_standup", "hello?").is_err());
    assert!(room.say("u_aria", "c_nowhere", "hello?").is_err());
    // A mention is resolved against the record's people, whatever the
    // caller sent.
    let mut post = crate::demo::post("c_general", "@Aria Chen look", None);
    post.arguments.insert(
        "mentions".into(),
        serde_json::json!([{ "user": "u_nobody", "start": 0, "len": 3, "is_agent": false }]),
    );
    room.apply_part_as("u_milo", post).unwrap();
    let row = room.message_rows("c_general").pop().unwrap();
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
            .unwrap();
        if next != 0 {
            break;
        }
    }
    assert_eq!(next, 1, "a full head rolls the next post over");
    let first = chain
        .invoke(&crate::demo::post("c_general", "the first of part 1", None))
        .unwrap();
    assert_eq!(first, 1, "posts go on in the new head");
    assert_eq!(chain.head(), Some(1));
    // The new part is the chain's part 1, as every device names it.
    assert_eq!(part_index(&crate::parts::part(1)), Some(1));
    // A reaction to the demo's first message lands in part 0.
    assert_eq!(chain.invoke(&react("m_welcome", "👍")).unwrap(), 0);
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
    let mut room = Room::demo().unwrap();
    let general = Id::from_raw("c_general");
    fill_part(&mut room, 0);
    room.say("u_milo", "c_general", "in part 1").unwrap();
    let held = room.messages();
    let end = held.read_to_end(&general);
    assert_eq!(end, crate::parts::ReadMark { part: 1, seen: 2 });
    assert_eq!(held.unread(&general, end), 0);
    let before = crate::parts::ReadMark { part: 0, seen: 1 };
    assert_eq!(held.unread(&general, before), held.top_level(&general) - 1);
    room.apply_as(
        crate::model::ME,
        crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
            channel: general.clone(),
            part: 1,
            seen: 2,
        }),
    )
    .unwrap();
    assert_eq!(room.read().unread(&held, "c_general", crate::model::ME), 0);
    // A mark never moves back.
    room.apply_as(
        crate::model::ME,
        crate::resource::Chat::MARK_READ.with(crate::record::chat::MarkRead {
            channel: general,
            part: 0,
            seen: 1,
        }),
    )
    .unwrap();
    assert_eq!(room.read().read_mark("c_general", crate::model::ME), end);
}

/// Two devices in one fresh room, each with a store of its own and a chain
/// held in it: the poster's, and a guest's whose window was open before
/// anyone posted.
struct TwoDevices {
    room: String,
    poster: Chain<ChatPart>,
    guest: Chain<ChatPart>,
    guest_store: ui::record::RecordStore,
    guest_backend: Rc<durable_store::MemoryStore>,
    /// The record the guest's window shows beside the chain.
    shown: ui::record::RecordId,
}

impl TwoDevices {
    fn new() -> Self {
        let room = uuid::Uuid::new_v4().to_string();
        let chat = Rc::new(crate::demo::demo(crate::model::ME));
        let device = |backend: &Rc<durable_store::MemoryStore>| {
            ui::record::RecordOwners::with_store(
                Rc::clone(backend) as Rc<dyn durable_store::DurableStore>
            )
            .open("so.tilo.chat", &room)
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
        let guest =
            Chain::detached_in(guest_store.clone(), "so.tilo.chat", "u_aria", base()).unwrap();
        assert_eq!(guest.head(), None, "a fresh room has no part yet");
        let shown = ui::record::RecordCheckout::new(
            ui::record::RecordId::derived("shown", &ui::record::RecordId::room_default()),
            ui::record::RecordKind::new("test.shown"),
        );
        guest_store
            .reserve_record(&shown)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        Self {
            room,
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
    /// room's replication keeps a record no window there has open.
    fn first_post_reaches_the_guest(&self) {
        use durable_store::DurableStore as _;
        self.poster
            .invoke(&crate::demo::post("c_general", "the first post", None))
            .unwrap();
        self.guest_backend
            .write(
                &durable_store::StorageKey::record(
                    "so.tilo.chat",
                    &self.room,
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

/// The first post of a room starts part 0 on the poster's device. It reaches
/// a guest whose window already holds the chain by being kept in the
/// guest's store. The guest's window opens it as its head at its next look:
/// not in a sync, which reads the store (opening a part takes its writer),
/// but in the settle after it, so the sync of the frame that follows shows
/// the post.
#[test]
fn the_first_post_reaches_a_window_open_before_it() {
    let devices = TwoDevices::new();
    let seen = devices.guest.revision();
    devices.first_post_reaches_the_guest();
    let later = ui::time::now() + std::time::Duration::from_secs(2);
    ui::time::testing::with_monotonic_time(later, || {
        assert_eq!(devices.guest_syncs(), seen, "a sync opens nothing");
        assert_ne!(devices.guest.revision(), seen, "the settle opens the part");
        assert_eq!(devices.guest.head(), Some(0));
        assert_eq!(devices.guest_reads().last().unwrap(), "the first post");
    });
}

/// A store told that a record arrived ([`ui::record::RecordStore::note_kept`],
/// as the browser's room replication tells it) has the window look at its
/// next settle, not a moment later.
#[test]
fn a_store_told_of_a_kept_part_has_the_window_look_at_once() {
    let devices = TwoDevices::new();
    let seen = devices.guest_syncs();
    devices.first_post_reaches_the_guest();
    devices.guest_store.note_kept();
    assert_eq!(devices.guest_syncs(), seen, "a sync opens nothing");
    assert_ne!(devices.guest.revision(), seen, "the settle opens the part");
    assert_eq!(devices.guest_reads().last().unwrap(), "the first post");
}

/// On a desktop the room does not keep a part another device started: the
/// guest's window is told a peer wrote it, starts the same part itself
/// (each part's genesis is the same everywhere), and the poster's changes
/// then apply on it with nothing missing, as they arrive on its route.
#[test]
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

/// read-messages reads a channel's messages from every part of the chat's
/// messages, not the chat record (which holds none), page by page.
#[test]
fn read_messages_pages_through_a_channel_across_its_parts() {
    let mut room = Room::demo().unwrap();
    let later = fill_part(&mut room, 0);
    assert_eq!(room.messages().part_of(&Id::from_raw(&later)), Some(1));
    let held = room.messages();
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = crate::effects::read_messages(
            &held,
            ("c_general", &|id| format!("name of {id}")),
            Some(7),
            cursor.as_deref(),
        )
        .unwrap();
        for message in page["messages"].as_array().unwrap() {
            assert_eq!(
                message["authorName"],
                format!("name of {}", message["author"].as_str().unwrap()),
                "each message names its author as the window does"
            );
        }
        let ids: Vec<String> = page["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["id"].as_str().unwrap().to_owned())
            .collect();
        assert!(ids.len() <= 7);
        seen.extend(ids);
        match page["nextCursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert!(seen.contains(&"m_welcome".to_owned()), "part 0's");
    assert_eq!(seen.last(), Some(&later), "and part 1's, last");
    let unique: std::collections::BTreeSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "each once");
    assert!(
        crate::effects::read_messages(&held, ("c_general", &str::to_owned), Some(0), None).is_err()
    );
}

/// A post's step says which channel it went to, as everyone sees it
/// named: what the automatic status reads to say "Writing in #general".
#[test]
fn a_post_records_the_channel_it_went_to() {
    let mut room = Room::demo().unwrap();
    room.say("u_aria", "c_general", "where this went").unwrap();
    let bytes = room.part_bytes(0).unwrap();
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
