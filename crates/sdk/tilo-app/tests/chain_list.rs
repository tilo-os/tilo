#![allow(
    clippy::items_after_statements,
    reason = "a step brings the names it uses where it uses them"
)]

//! Reading back through a chain's parts on a device that joined late: it
//! holds the head only, and a view opens earlier parts one at a time as its
//! reader reaches the top, and while an item looked for is searched for in
//! them.

use std::rc::Rc;

use tilo_app::framework::{
    Chain, ChainList, ChainPager, ChainRow, Chained, Paged, PartArrival, PartLimits,
};
use tilo_app::prelude::*;
use tilo_app::resource::{
    DocumentCheckout, DocumentId, ResourceOwners, ResourceStore, ResourceType,
};
use tilo_app::tilo_resource::Collection;
use tilo_app::widget::virtual_list::ListModel;

/// Lines a part holds before the next one starts.
const PART_LINES: usize = 3;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Log {
    pub title: String,
}

#[resource(kind = "test.chain-log", catalogue = "tests/locales")]
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
    pub marked: bool,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct LogPart {
    #[model(readonly)]
    pub lines: Collection<Line>,
}

#[resource(kind = "test.chain-log-part", catalogue = "tests/locales")]
impl LogPart {
    /// Write a line.
    #[action]
    pub fn write(&mut self, cx: &mut ActionCx, text: String) {
        self.lines.push(
            cx.id(),
            Line {
                text,
                marked: false,
            },
        );
    }

    /// Mark lines.
    #[action(items = lines)]
    pub fn mark(&mut self, lines: Vec<String>) {
        for id in lines {
            if let Some(line) = self.lines.get_mut(&Id::from_raw(id)) {
                line.marked = true;
            }
        }
    }
}

impl Chained for LogPart {
    type Base = Log;

    fn chain() -> DocumentId {
        DocumentId::derived("lines", &DocumentId::workspace_default())
    }
}

/// Posts of a thread, each answering one before it: a chain every view lists
/// whole, whose answers name the post they answer wherever it is kept.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Post {
    pub text: String,
    pub answers: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ThreadPart {
    #[model(readonly)]
    pub posts: Collection<Post>,
}

#[resource(kind = "test.chain-thread-part", catalogue = "tests/locales")]
impl ThreadPart {
    /// Post something.
    #[action]
    pub fn post(&mut self, cx: &mut ActionCx, text: String, answers: String) {
        self.posts.push(cx.id(), Post { text, answers });
    }
}

impl Chained for ThreadPart {
    type Base = Log;

    fn chain() -> DocumentId {
        DocumentId::derived("thread", &DocumentId::workspace_default())
    }

    const HELD_WHOLE: bool = true;

    /// An answer names a post the thread has, in any of its parts.
    fn prepare(
        actions: &mut [Self::Action],
        _: &Log,
        parts: &[(u64, Rc<Self>)],
        _: &tilo_actions::ActionInvoker,
    ) -> Result<(), tilo_app::tilo_resource::Refusal> {
        for action in actions {
            let ThreadPartAction::Post(thread_part::Post { answers, .. }) = &action.operation
            else {
                continue;
            };
            let known = parts
                .iter()
                .any(|(_, part)| part.posts.get(&Id::from_raw(answers.as_str())).is_some());
            if !answers.is_empty() && !known {
                return Err(tilo_app::tilo_resource::Refusal::new(format!(
                    "no post named {answers}"
                )));
            }
        }
        Ok(())
    }
}

/// Parts of [`PART_LINES`] lines while it is held.
fn small_parts() -> PartLimits {
    PartLimits::scoped(PART_LINES, usize::MAX)
}

/// One device's records of the log in its workspace.
fn device() -> ResourceStore {
    device_keeping(&Rc::new(durable_store::MemoryStore::new()))
}

/// [`device`], keeping its records in `memory`.
fn device_keeping(memory: &Rc<durable_store::MemoryStore>) -> ResourceStore {
    ResourceOwners::with_store(Rc::clone(memory) as Rc<dyn durable_store::DurableStore>)
        .open("so.tilo.test-chain-log", "workspace")
        .unwrap()
}

fn chain_in(store: &ResourceStore, me: &str) -> Chain<LogPart> {
    let me = &MemberId::person(me);
    let log = Rc::new(Log::default());
    Chain::detached_in(store.clone(), "so.tilo.test-chain-log", me, move || {
        Rc::clone(&log)
    })
    .unwrap()
}

/// A writer that filled parts 0 and 1 and wrote one line into part 2, and
/// a reader that joined after: its workspace sent it the head only.
struct Joined {
    writer: ResourceStore,
    reader: ResourceStore,
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
        let checkout = DocumentCheckout::new(id, ResourceType::new(LogPart::KIND));
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

tilo_test_env::scenario! {
    fn a_late_device_reads_back_through_the_chains_parts;

    phase a_part_this_device_has_none_of_is_held_from_its_genesis_until_it_arrives;
    {
        let _parts = small_parts();
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

    phase reaching_the_top_opens_one_earlier_part_at_a_time;
    {
        let _parts = small_parts();
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

    phase an_item_looked_for_opens_earlier_parts_until_one_has_it;
    {
        let _parts = small_parts();
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

    phase a_chain_list_pages_back_when_its_first_row_shows;
    {
        let _parts = small_parts();
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

    phase a_part_the_workspace_has_none_of_shows_unavailable_until_it_arrives;
    // The workspace's hosts sent everything they hold of a part asked for, without
    // it (or none is connected): the view says the earlier lines are not
    // available, nothing spins and nothing before it is asked for, until it
    // arrives after all.
    {
        let _parts = small_parts();
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

    phase an_item_looked_for_past_an_unavailable_part_is_not_found;
    // A part looked for that the workspace has none of ends the search.
    {
        let _parts = small_parts();
        let joined = Joined::new();
        let mut pager = ChainPager::new(joined.chain.clone());
        pager.seek(|part: &LogPart| part.lines.values().any(|line| line.text == "line 1"));
        assert_eq!(pager.settle(), Paged::Opened(1));
        joined
            .reader
            .note_workspace_answered(LogPart::chain().part(1));
        assert_eq!(pager.settle(), Paged::NotFound);
    }
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
    let _parts = small_parts();
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
    let key = StorageKey::document(APP, "workspace", LogPart::chain().part(0).storage_key());
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

/// Several actions invoked as one, on a device whose head is still on its
/// way: the whole group waits for the part, shows as being sent, and lands
/// once it arrives. A group answered `ok` while nothing was written is a
/// write lost without a word: an import that answers ok and keeps nothing.
#[test]
fn a_group_while_its_part_is_on_its_way_lands_once_the_part_arrived() {
    let _parts = small_parts();
    let written = |text: &str| LogPart::WRITE.with(log_part::Write { text: text.into() });
    let theirs = Rc::new(durable_store::MemoryStore::new());
    let writer = device_keeping(&theirs);
    let writing = chain_in(&writer, "ada");
    writing.invoke(&written("one")).unwrap();
    let ours = Rc::new(durable_store::MemoryStore::new());
    let reader = device_keeping(&ours);
    reader.ask_workspace_for_missing();
    let chain = chain_in(&reader, "bo");
    let group = Invoke::group([written("two"), written("three")]);
    assert_eq!(
        chain.invoke(&group).unwrap(),
        None,
        "the group waits for the head"
    );
    assert!(chain.parts().is_empty(), "nothing was started in its place");
    assert_eq!(
        sending(&chain),
        ["two", "three"],
        "the whole group shows as what waits to be sent"
    );

    part_arrives(&theirs, &ours, &reader);
    let lines: Vec<String> = texts(&chain.parts())
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(lines, ["one", "two", "three"]);
    assert!(chain.unwritten().is_none(), "nothing waits any more");
}

/// What the writer wrote reaches the reader's device: the workspace's part
/// kept in its copy of the app's records.
fn part_arrives(
    theirs: &durable_store::MemoryStore,
    ours: &durable_store::MemoryStore,
    reader: &ResourceStore,
) {
    use durable_store::{DurableStore as _, StorageKey};
    let key = StorageKey::document(
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
    ResourceStore,
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
    let _parts = small_parts();
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
    let _parts = small_parts();
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

/// A native window of a workspace that sends a device its records only once it
/// opens them holds no part of the chain when it opens: it asks the workspace for
/// the chain's head — the name of whichever part is the newest — and writes
/// nothing until the answer comes. Writing into the part it would otherwise have
/// started, the chain's first, put the line beside everything everyone wrote in
/// the workspace's own first part, which is still there and still everyone's.
#[test]
fn a_native_window_waits_for_the_workspaces_newest_part_and_founds_no_part_of_its_own() {
    use durable_store::{DurableStore as _, MemoryStore, StorageKey};
    use tilo_app::collaboration::CollaborationManager;
    use tilo_app::collaboration::native_initial::NativeConstruction;

    const APP: &str = "so.tilo.test-chain-log";
    let _parts = small_parts();
    let written = |text: &str| LogPart::WRITE.with(log_part::Write { text: text.into() });
    let workspace = uuid::Uuid::new_v4().to_string();

    // The workspace's newest part, as a member's device wrote it: one line in part 1, after a
    // full part 0 that everyone's earlier lines are in.
    let theirs = Rc::new(MemoryStore::new());
    let writer =
        ResourceOwners::with_store(Rc::clone(&theirs) as Rc<dyn durable_store::DurableStore>)
            .open(APP, &workspace)
            .unwrap();
    let writing = chain_in(&writer, "ada");
    for n in 0..=PART_LINES {
        writing.invoke(&written(&format!("line {n}"))).unwrap();
    }
    assert_eq!(
        writing.head(),
        Some(1),
        "the workspace's log is in two parts"
    );
    let newest = LogPart::chain().part(1);
    let kept = theirs
        .read(&StorageKey::document(APP, &workspace, newest.storage_key()))
        .unwrap()
        .expect("the writer kept part 1")
        .bytes;

    // This device's window, reading its records through its broker: the workspace sends a device
    // that opens a record what it holds of it, here the chain's newest part.
    let (broker, _joins) = CollaborationManager::native_initial_fixture_for_test(
        APP,
        uuid::Uuid::parse_str(&workspace).unwrap(),
        kept,
    );
    let _construction = NativeConstruction::enter(broker).expect("the app's construction");
    let ours = Rc::new(MemoryStore::new());
    let reader =
        ResourceOwners::with_store(Rc::clone(&ours) as Rc<dyn durable_store::DurableStore>)
            .open(APP, &workspace)
            .unwrap();
    let chain = chain_in(&reader, "bo");
    assert!(chain.parts().is_empty(), "no part of it is here");
    assert!(
        chain.head_arriving(),
        "the window asks the workspace which part is the newest"
    );
    let refused = chain.stage_as(&written("two"), &tilo_resource::MemberId::person("bo"));
    assert!(
        refused.is_err(),
        "the line waits for the answer rather than starting a part: {refused:?}"
    );
    assert!(chain.parts().is_empty(), "no part was started in its place");

    // The host sends the part that is the chain's newest, which reaches this device's records: the
    // read it asked for is announced as an app is told of a record no window holds.
    reader.announce([newest]);
    let deadline = tilo_test_env::hang_deadline();
    while chain.parts().is_empty() {
        assert!(
            tilo_app::time::now() < deadline,
            "the newest part never arrived"
        );
        reader.note_kept();
        std::thread::yield_now();
    }
    assert_eq!(
        chain.head(),
        Some(1),
        "the window's head is the workspace's newest part"
    );
    assert!(!chain.head_arriving());
    assert_eq!(
        chain
            .stage_as(&written("two"), &tilo_resource::MemberId::person("bo"))
            .unwrap(),
        Some(1),
        "the line is written into the workspace's newest part"
    );
    let lines: Vec<String> = texts(&chain.parts())
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        lines,
        ["line 3", "two"],
        "after what was written there, and nothing else"
    );
    assert!(
        !reader.is_ready(&LogPart::chain().part(0)),
        "the chain's first part was never founded, or opened, here"
    );
}

tilo_test_env::scenario! {
    fn a_verb_over_lines_of_several_parts_is_one_step;

    phase it_writes_each_part_holding_some_and_undo_takes_back_all;
    {
        let _parts = small_parts();
        let store = device();
        let chain = chain_in(&store, "ada");
        for n in 0..=PART_LINES {
            chain
                .invoke(&LogPart::WRITE.with(log_part::Write {
                    text: format!("line {n}"),
                }))
                .unwrap();
        }
        assert_eq!(chain.head(), Some(1));
        let ids: Vec<String> = texts(&chain.parts())
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        // The first line of part 0 and the one line of part 1.
        let marking = vec![ids[0].clone(), ids[PART_LINES].clone()];
        let before: Vec<Rc<LogPart>> = chain.parts().into_iter().map(|(_, p)| p).collect();
        chain
            .invoke(&LogPart::MARK.with(log_part::Mark { lines: marking }))
            .unwrap();
        let marked = |chain: &Chain<LogPart>| -> Vec<bool> {
            chain
                .parts()
                .iter()
                .flat_map(|(_, part)| part.lines.iter().map(|(_, line)| line.marked).collect::<Vec<_>>())
                .collect()
        };
        assert_eq!(marked(&chain), [true, false, false, true]);
        let after: Vec<Rc<LogPart>> = chain.parts().into_iter().map(|(_, p)| p).collect();
        assert!(before.iter().zip(&after).all(|(b, a)| !Rc::ptr_eq(b, a)), "both parts written");
        assert!(chain.undo_last(), "one step");
        assert_eq!(marked(&chain), [false, false, false, false]);
        assert!(chain.redo_last());
        assert_eq!(marked(&chain), [true, false, false, true]);
    }
}

tilo_test_env::scenario! {
    fn a_group_submitted_through_the_chains_dispatcher_is_one_step;

    phase undo_takes_back_every_member_at_once;
    {
        let store = device();
        let chain = chain_in(&store, "ada");
        let group = Invoke::group([
            LogPart::WRITE.with(log_part::Write { text: "one".into() }),
            LogPart::WRITE.with(log_part::Write { text: "two".into() }),
        ]);
        let mut scratch = tilo_app::automerge::AutoCommit::new();
        chain
            .dispatcher()
            .submit_now(&mut tilo_app::widget::Edits::lend(&mut scratch), group)
            .unwrap();
        let written: Vec<String> = texts(&chain.parts()).into_iter().map(|(_, text)| text).collect();
        assert_eq!(written, ["one", "two"]);
        // Written as one change, checked as a whole: one step, not one per
        // member.
        assert!(chain.undo_last(), "one step");
        assert!(texts(&chain.parts()).is_empty(), "both members taken back");
        assert!(!chain.undo_last(), "nothing more to take back");
    }
}

/// A device that joined late holds the head of a thread every view lists
/// whole, its earlier parts still on their way. An answer to a post kept in
/// one of them is not refused as naming nothing: it waits, and lands once
/// the parts arrived. The write that names something no part has is refused
/// once every part is here.
#[test]
fn a_write_naming_an_item_of_a_part_on_its_way_waits_for_it() {
    let _parts = small_parts();
    let post = |text: &str, answers: &str| {
        ThreadPart::POST.with(thread_part::Post {
            text: text.into(),
            answers: answers.into(),
        })
    };
    use durable_store::{DurableStore as _, StorageKey};
    let theirs = Rc::new(durable_store::MemoryStore::new());
    let writer = device_keeping(&theirs);
    let writing = Chain::<ThreadPart>::detached_in(
        writer,
        "so.tilo.test-chain-log",
        &MemberId::person("ada"),
        || Rc::new(Log::default()),
    )
    .unwrap();
    for n in 0..=2 * PART_LINES {
        writing.invoke(&post(&format!("post {n}"), "")).unwrap();
    }
    assert_eq!(writing.head(), Some(2));
    let first = writing
        .parts()
        .first()
        .and_then(|(_, part)| part.posts.iter().next().map(|(id, _)| id.clone()))
        .expect("the first post");

    let ours = Rc::new(durable_store::MemoryStore::new());
    let reader = device_keeping(&ours);
    reader.ask_workspace_for_missing();
    let chain = Chain::<ThreadPart>::detached_in(
        reader.clone(),
        "so.tilo.test-chain-log",
        &MemberId::person("bo"),
        || Rc::new(Log::default()),
    )
    .unwrap();
    // The workspace's part reaches the reader's own copy of it.
    let arrives = |index: u64| {
        let key = StorageKey::document(
            "so.tilo.test-chain-log",
            "workspace",
            ThreadPart::chain().part(index).storage_key(),
        );
        let kept = theirs.read(&key).unwrap().expect("the writer kept it");
        ours.hydrate(key, kept).unwrap();
        reader.note_kept();
    };
    arrives(2);
    let held: Vec<u64> = chain.parts().iter().map(|(index, _)| *index).collect();
    assert_eq!(
        held,
        [2],
        "the head arrived, the parts before it are on their way"
    );

    assert_eq!(
        chain.invoke(&post("an answer", first.as_str())).unwrap(),
        None,
        "it waits for the parts on their way, never refused as naming nothing"
    );
    arrives(0);
    arrives(1);
    let answered: Vec<String> = chain
        .parts()
        .iter()
        .flat_map(|(_, part)| part.posts.values())
        .filter(|post| post.answers == first.as_str())
        .map(|post| post.text.clone())
        .collect();
    assert_eq!(answered, ["an answer"], "it landed once they arrived");

    let refused = chain.invoke(&post("astray", "no-such-post"));
    assert!(
        refused
            .as_ref()
            .is_err_and(|error| error.to_string().contains("no post named")),
        "with every part here it is refused: {refused:?}"
    );
}
