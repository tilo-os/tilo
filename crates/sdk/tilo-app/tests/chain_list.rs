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
        RecordId::derived("lines", &RecordId::workspace_default())
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

/// One device's records of the log in its workspace.
fn device() -> RecordStore {
    device_keeping(&Rc::new(durable_store::MemoryStore::new()))
}

/// [`device`], keeping its records in `memory`.
fn device_keeping(memory: &Rc<durable_store::MemoryStore>) -> RecordStore {
    RecordOwners::with_store(Rc::clone(memory) as Rc<dyn durable_store::DurableStore>)
        .open("so.tilo.test-chain-log", "workspace")
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
/// a reader that joined after: its workspace sent it the head only.
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

    /// The writer's part `index` reaches the reader, as the workspace's record
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
    list.request_range(1..2, None);
    assert_eq!(joined.held(), [2]);
    // The first row shows: the part before opens, and arrives.
    list.request_range(0..2, None);
    assert_eq!(joined.held(), [1, 2]);
    joined.deliver(1);
    assert!(list.poll_changed());
    assert_eq!(list.len(), 5);
    assert_eq!(list.item(1).map(String::as_str), Some("line 3"));
    let newest = list.key_at(4).unwrap();
    assert_eq!(list.index_of(&newest), Some(4));
    assert!(!list.poll_changed(), "nothing moved since");

    list.request_range(0..3, None);
    joined.deliver(0);
    assert!(list.poll_changed());
    assert_eq!(list.len(), 7, "every line, and no earlier row");
    assert_eq!(
        list.key_at(0).map(|key| key == ChainRow::Earlier),
        Some(false)
    );
    assert_eq!(list.item(0).map(String::as_str), Some("line 0"));
}

/// The workspace's hosts sent everything they hold of a part asked for, without
/// it (or none is connected): the view says the earlier lines are not
/// available, nothing spins and nothing before it is asked for, until it
/// arrives after all.
#[test]
fn a_part_the_workspace_has_none_of_shows_unavailable_until_it_arrives() {
    let joined = Joined::new();
    let mut list = ChainList::new(joined.chain.clone(), texts);
    list.request_range(0..2, None);
    assert_eq!(joined.held(), [1, 2]);
    assert!(list.row_state(0).is_pending());

    joined
        .reader
        .note_workspace_answered(LogPart::chain().part(1));
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

/// A part looked for that the workspace has none of ends the search.
#[test]
fn an_item_looked_for_past_an_unavailable_part_is_not_found() {
    let joined = Joined::new();
    let mut pager = ChainPager::new(joined.chain.clone());
    pager.seek(|part: &LogPart| part.lines.values().any(|line| line.text == "line 1"));
    assert_eq!(pager.settle(), Paged::Opened(1));
    joined
        .reader
        .note_workspace_answered(LogPart::chain().part(1));
    assert_eq!(pager.settle(), Paged::NotFound);
}

/// A device back from away, its workspace still sending the head it holds: a
/// line written meanwhile is not refused, it waits for the head and lands
/// after what was written there while the device was away, answered then.
/// (Three lines fill a part: all of these land in the head.)
#[test]
fn a_write_while_its_part_is_on_its_way_lands_once_the_part_arrived() {
    use durable_store::{DurableStore as _, MemoryStore, StorageKey};
    use std::cell::RefCell;

    const APP: &str = "so.tilo.test-chain-log";
    let written = |text: &str| LogPart::WRITE.with(log_part::Write { text: text.into() });
    let theirs = Rc::new(MemoryStore::new());
    let writer = device_keeping(&theirs);
    let writing = chain_in(&writer, "ada");
    writing.invoke(&written("one")).unwrap();
    let ours = Rc::new(MemoryStore::new());
    let reader = device_keeping(&ours);
    reader.ask_workspace_for_missing();
    let chain = chain_in(&reader, "bo");
    let answers: Rc<RefCell<Vec<Result<u64, String>>>> = Rc::default();
    let answering = Rc::clone(&answers);
    chain.invoke_then(&written("two"), move |answer| {
        answering
            .borrow_mut()
            .push(answer.map_err(|error| error.to_string()));
    });
    assert_eq!(
        chain.invoke(&written("three")).unwrap(),
        None,
        "a write nobody waits on waits for its part too, after the one before"
    );
    assert!(answers.borrow().is_empty(), "both wait for the head");
    assert!(chain.parts().is_empty(), "nothing was started in its place");
    let unwritten = chain.unwritten().expect("what waits shows");
    assert_eq!(
        texts(&[(0, unwritten)])
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["two", "three"],
        "as its part will hold it"
    );

    // The workspace's part reaches the device's own copy of its records.
    let key = StorageKey::record(APP, "workspace", LogPart::chain().part(0).storage_key());
    let kept = theirs
        .read(&key)
        .unwrap()
        .expect("the writer kept the part");
    ours.hydrate(key, kept).unwrap();
    reader.note_kept();
    let lines: Vec<String> = texts(&chain.parts())
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(*answers.borrow(), [Ok(0)], "answered once written");
    assert_eq!(lines, ["one", "two", "three"]);
    assert!(chain.unwritten().is_none(), "nothing waits any more");
}

/// What the writer wrote reaches the reader's device: the workspace's part
/// kept in its copy of the app's records.
fn part_arrives(
    theirs: &durable_store::MemoryStore,
    ours: &durable_store::MemoryStore,
    reader: &RecordStore,
) {
    use durable_store::{DurableStore as _, StorageKey};
    let key = StorageKey::record(
        "so.tilo.test-chain-log",
        "workspace",
        LogPart::chain().part(0).storage_key(),
    );
    let kept = theirs
        .read(&key)
        .unwrap()
        .expect("the writer kept the part");
    ours.hydrate(key, kept).unwrap();
    reader.note_kept();
}

/// The lines a view of `chain` shows as being sent.
fn sending(chain: &Chain<LogPart>) -> Vec<String> {
    chain
        .unwritten()
        .map(|part| {
            texts(&[(0, part)])
                .into_iter()
                .map(|(_, text)| text)
                .collect()
        })
        .unwrap_or_default()
}

/// A device back from away: the writer wrote "one" into the head the reader
/// waits for, and the reader wrote "two" meanwhile.
fn written_while_away() -> (
    Rc<durable_store::MemoryStore>,
    Rc<durable_store::MemoryStore>,
    RecordStore,
) {
    let written = |text: &str| LogPart::WRITE.with(log_part::Write { text: text.into() });
    let theirs = Rc::new(durable_store::MemoryStore::new());
    chain_in(&device_keeping(&theirs), "ada")
        .invoke(&written("one"))
        .unwrap();
    let ours = Rc::new(durable_store::MemoryStore::new());
    let reader = device_keeping(&ours);
    reader.ask_workspace_for_missing();
    let window = chain_in(&reader, "bo");
    assert_eq!(window.invoke(&written("two")).unwrap(), None, "it waits");
    assert_eq!(sending(&window), ["two"]);
    (theirs, ours, reader)
}

/// A line written while its part is on its way outlives the window it was
/// written in: every other window of the app shows it as being sent, and it
/// lands once the part arrives, once, however many windows hold the chain.
#[test]
fn a_write_waiting_for_its_part_outlives_its_window() {
    let (theirs, ours, reader) = written_while_away();
    let window = chain_in(&reader, "bo");
    let another = chain_in(&reader, "bo");
    assert_eq!(
        sending(&window),
        ["two"],
        "the closed window's line still sends"
    );
    assert_eq!(sending(&another), ["two"]);

    part_arrives(&theirs, &ours, &reader);
    for chain in [&window, &another, &window] {
        let lines: Vec<String> = texts(&chain.parts())
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        assert_eq!(lines, ["one", "two"], "written once, after what was there");
        assert!(sending(chain).is_empty(), "nothing waits any more");
    }
}

/// A line written while its part is on its way is kept with the app's
/// records: after a restart it still shows as being sent, and lands once the
/// part arrives.
#[test]
fn a_write_waiting_for_its_part_outlives_a_restart() {
    let (theirs, ours, reader) = written_while_away();
    reader.flush().unwrap();
    drop(reader);

    let reader = device_keeping(&ours);
    reader.ask_workspace_for_missing();
    let window = chain_in(&reader, "bo");
    assert_eq!(sending(&window), ["two"], "kept across the restart");
    assert!(window.parts().is_empty());

    part_arrives(&theirs, &ours, &reader);
    let lines: Vec<String> = texts(&window.parts())
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(lines, ["one", "two"]);
    assert!(sending(&window).is_empty());
}
