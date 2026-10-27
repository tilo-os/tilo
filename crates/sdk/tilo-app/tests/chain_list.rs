#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! Reading back through a chain's parts on a device that joined late: it
//! holds the head only, and a view opens earlier parts one at a time as its
//! reader reaches the top, and while an item looked for is searched for in
//! them.

use std::rc::Rc;

use tilo_app::framework::{Chain, ChainList, ChainPager, ChainRow, Chained, Paged, PartArrival};
use tilo_app::prelude::*;
use tilo_app::record::{RecordCheckout, RecordId, RecordKind, RecordOwners, RecordStore};
use tilo_app::tilo_resource::Collection;
use tilo_app::widget::virtual_list::ListModel;

/// Lines a part holds before the next one starts.
const PART_LINES: usize = 3;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Log {
    pub title: String,
}

#[resource(kind = "test.chain-log", label = "Log")]
impl Log {
    /// Rename the log.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct LogPart {
    #[model(readonly)]
    pub lines: Collection<Line>,
}

#[resource(kind = "test.chain-log-part", label = "Lines")]
impl LogPart {
    /// Write a line.
    #[action]
    pub fn write(&mut self, cx: &mut ActionCx, text: String) {
        self.lines.push(cx.id(), Line { text });
    }
}

impl Chained for LogPart {
    type Base = Log;

    fn chain() -> RecordId {
        RecordId::derived("lines", &RecordId::room_default())
    }

    fn part_for(_: &Self::Action, parts: &[(u64, Rc<Self>)]) -> Result<u64, String> {
        Ok(parts.last().map_or(0, |(head, part)| {
            if part.lines.len() >= PART_LINES {
                head.saturating_add(1)
            } else {
                *head
            }
        }))
    }
}

/// One device's records of the log in its room.
fn device() -> RecordStore {
    RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()))
        .open("so.tilo.test-chain-log", "room")
        .unwrap()
}

fn chain_in(store: &RecordStore, me: &str) -> Chain<LogPart> {
    let log = Rc::new(Log::default());
    Chain::detached_in(store.clone(), "so.tilo.test-chain-log", me, move || {
        Rc::clone(&log)
    })
    .unwrap()
}

/// A writer that filled parts 0 and 1 and wrote one line into part 2, and
/// a reader that joined after: its room sent it the head only.
struct Joined {
    writer: RecordStore,
    reader: RecordStore,
    chain: Chain<LogPart>,
}

impl Joined {
    fn new() -> Self {
        let writer = device();
        let writing = chain_in(&writer, "ada");
        for n in 0..=2 * PART_LINES {
            writing
                .invoke(&LogPart::WRITE.with(log_part::Write {
                    text: format!("line {n}"),
                }))
                .unwrap();
        }
        assert_eq!(writing.head(), Some(2));
        let reader = device();
        let chain = chain_in(&reader, "bo");
        let joined = Self {
            writer,
            reader,
            chain,
        };
        joined.chain.hold(2).unwrap();
        joined.deliver(2);
        joined
    }

    /// The writer's part `index` reaches the reader, as the room's record
    /// sync sends a part the reader opened.
    fn deliver(&self, index: u64) {
        let id = LogPart::chain().part(index);
        let mut frontier = tilo_app::DocumentFrontier::default();
        let (changes, _) = self.writer.replication_delta(&id, &mut frontier).unwrap();
        let checkout = RecordCheckout::new(id, RecordKind::new(LogPart::KIND));
        self.reader.merge_peer_changes(&checkout, changes).unwrap();
    }

    fn held(&self) -> Vec<u64> {
        self.chain.parts().iter().map(|(index, _)| *index).collect()
    }
}

fn texts(parts: &[(u64, Rc<LogPart>)]) -> Vec<(String, String)> {
    parts
        .iter()
        .flat_map(|(_, part)| part.lines.iter())
        .map(|(id, line)| (id.as_str().to_owned(), line.text.clone()))
        .collect()
}

#[test]
fn a_part_this_device_has_none_of_is_held_from_its_genesis_until_it_arrives() {
    let joined = Joined::new();
    assert_eq!(joined.held(), [2]);
    assert_eq!(joined.chain.waiting(2), None, "the head arrived");
    let seen = joined.chain.revision();
    joined.chain.hold(1).unwrap();
    assert_eq!(joined.held(), [1, 2]);
    assert_eq!(joined.chain.waiting(1), Some(PartArrival::Arriving));
    assert!(texts(&joined.chain.parts()[..1]).is_empty());
    joined.deliver(1);
    assert_eq!(joined.chain.waiting(1), None);
    assert_ne!(joined.chain.revision(), seen);
    let lines: Vec<String> = texts(&joined.chain.parts())
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(lines, ["line 3", "line 4", "line 5", "line 6"]);
}

#[test]
fn reaching_the_top_opens_one_earlier_part_at_a_time() {
    let joined = Joined::new();
    let mut pager = ChainPager::new(joined.chain.clone());
    assert!(pager.has_earlier());
    assert!(!pager.wants_settle(), "nobody read back yet");
    assert_eq!(pager.settle(), Paged::Nothing);

    pager.reached_top();
    assert!(pager.wants_settle());
    assert_eq!(pager.settle(), Paged::Opened(1));
    assert!(pager.arriving());
    assert!(pager.asked().is_some(), "the view shows it on its way");
    // Nothing more while part 1 is on its way.
    pager.reached_top();
    assert!(!pager.wants_settle());
    assert_eq!(pager.settle(), Paged::Nothing);
    assert_eq!(joined.held(), [1, 2]);

    joined.deliver(1);
    assert!(pager.asked().is_none());
    pager.reached_top();
    assert_eq!(pager.settle(), Paged::Opened(0));
    joined.deliver(0);
    assert!(!pager.loading(), "nothing is earlier than part 0");
    pager.reached_top();
    assert!(!pager.wants_settle());
    assert_eq!(joined.held(), [0, 1, 2]);
}

#[test]
fn an_item_looked_for_opens_earlier_parts_until_one_has_it() {
    let joined = Joined::new();
    let mut pager = ChainPager::new(joined.chain.clone());
    pager.seek(|part: &LogPart| part.lines.values().any(|line| line.text == "line 1"));
    assert_eq!(pager.settle(), Paged::Opened(1));
    joined.deliver(1);
    assert_eq!(pager.settle(), Paged::Opened(0));
    assert_eq!(pager.settle(), Paged::Nothing, "part 0 is on its way");
    joined.deliver(0);
    assert_eq!(pager.settle(), Paged::Found);
    assert!(!pager.seeking());

    pager.seek(|part: &LogPart| part.lines.values().any(|line| line.text == "never"));
    assert_eq!(pager.settle(), Paged::NotFound);
}

#[test]
fn a_chain_list_pages_back_when_its_first_row_shows() {
    let joined = Joined::new();
    let mut list = ChainList::new(joined.chain.clone(), texts);
    assert_eq!(list.len(), 2, "the earlier parts' row and the head's line");
    assert_eq!(list.key_at(0), Some(ChainRow::Earlier));
    assert!(list.row_state(0).is_pending());
    assert_eq!(list.item(1).map(String::as_str), Some("line 6"));

    // Rows after the first: nothing opens.
    list.request_range(1..2);
    assert_eq!(joined.held(), [2]);
    // The first row shows: the part before opens, and arrives.
    list.request_range(0..2);
    assert_eq!(joined.held(), [1, 2]);
    joined.deliver(1);
    assert!(list.poll_changed());
    assert_eq!(list.len(), 5);
    assert_eq!(list.item(1).map(String::as_str), Some("line 3"));
    let newest = list.key_at(4).unwrap();
    assert_eq!(list.index_of(&newest), Some(4));
    assert!(!list.poll_changed(), "nothing moved since");

    list.request_range(0..3);
    joined.deliver(0);
    assert!(list.poll_changed());
    assert_eq!(list.len(), 7, "every line, and no earlier row");
    assert_eq!(
        list.key_at(0).map(|key| key == ChainRow::Earlier),
        Some(false)
    );
    assert_eq!(list.item(0).map(String::as_str), Some("line 0"));
}

/// The room's hosts sent everything they hold of a part asked for, without
/// it (or none is connected): the view says the earlier lines are not
/// available, nothing spins and nothing before it is asked for, until it
/// arrives after all.
#[test]
fn a_part_the_room_has_none_of_shows_unavailable_until_it_arrives() {
    let joined = Joined::new();
    let mut list = ChainList::new(joined.chain.clone(), texts);
    list.request_range(0..2);
    assert_eq!(joined.held(), [1, 2]);
    assert!(list.row_state(0).is_pending());

    joined.reader.note_room_answered(LogPart::chain().part(1));
    assert_eq!(joined.chain.waiting(1), Some(PartArrival::Unavailable));
    assert!(list.poll_changed());
    assert!(list.row_state(0).failure().is_some(), "said, not loading");
    assert!(list.pager().unavailable());
    assert!(list.pager().asked().is_none(), "nothing spins");
    list.pager().reached_top();
    assert!(!list.pager().wants_settle(), "nothing earlier is asked for");
    assert_eq!(list.pager().settle(), Paged::Nothing);
    assert_eq!(joined.held(), [1, 2]);

    // Someone who has it shares it after all.
    joined.deliver(1);
    assert_eq!(joined.chain.waiting(1), None);
    assert!(list.poll_changed());
    assert!(list.row_state(0).is_pending(), "part 0 is still earlier");
    assert_eq!(list.item(1).map(String::as_str), Some("line 3"));
}

/// A part looked for that the room has none of ends the search.
#[test]
fn an_item_looked_for_past_an_unavailable_part_is_not_found() {
    let joined = Joined::new();
    let mut pager = ChainPager::new(joined.chain.clone());
    pager.seek(|part: &LogPart| part.lines.values().any(|line| line.text == "line 1"));
    assert_eq!(pager.settle(), Paged::Opened(1));
    joined.reader.note_room_answered(LogPart::chain().part(1));
    assert_eq!(pager.settle(), Paged::NotFound);
}
