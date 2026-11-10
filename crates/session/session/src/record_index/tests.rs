//! The indexer over records the session holds: what search finds of an app that never opened here,
//! and what it stops finding once a record no longer holds it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::*;

/// One workspace's records as the fakes hold them: each record's entries,
/// and every read the indexer asked for.
#[derive(Default)]
struct Held {
    docs: Mutex<BTreeMap<(String, String), Vec<IndexEntry>>>,
    reads: Mutex<Vec<String>>,
}

impl Held {
    fn hold(&self, app: &str, record: &str, entries: Vec<IndexEntry>) {
        self.docs
            .lock_recover()
            .insert((app.to_owned(), record.to_owned()), entries);
    }

    fn read(&self, app: &str, record: &str) -> usize {
        self.reads
            .lock_recover()
            .iter()
            .filter(|read| read.as_str() == format!("{app}/{record}"))
            .count()
    }
}

/// The workspace's records, as the owner lists and reads them.
struct Source {
    workspace: String,
    held: Arc<Held>,
}

impl RecordSource for Source {
    fn workspaces(&self) -> Vec<String> {
        vec![self.workspace.clone()]
    }

    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        if workspace != self.workspace {
            return Ok(Vec::new());
        }
        Ok(self.held.docs.lock_recover().keys().cloned().collect())
    }

    fn record(&self, _workspace: &str, app: &str, record: &str) -> Result<Option<Vec<u8>>, String> {
        let held = self
            .held
            .docs
            .lock_recover()
            .get(&(app.to_owned(), record.to_owned()))
            .cloned();
        let Some(entries) = held else {
            return Ok(None);
        };
        self.held
            .reads
            .lock_recover()
            .push(format!("{app}/{record}"));
        serde_json::to_vec(&entries)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

/// What the app's own reader answers: the entries the fake document holds.
struct Reader;

impl RecordReader for Reader {
    fn entries(&self, _app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, String> {
        serde_json::from_slice(doc).map_err(|error| error.to_string())
    }
}

fn entry(resource: &str, title: &str) -> IndexEntry {
    IndexEntry {
        key: resource.to_owned(),
        resource: resource.to_owned(),
        action: None,
        title: title.to_owned(),
        subtitle: None,
        group: None,
        icon: None,
        boost: 0,
        fields: Vec::new(),
        secret: false,
        references: Vec::new(),
        ..IndexEntry::default()
    }
}

/// An index over `held` that nothing wakes: the test reads a pass itself.
fn index(held: &Arc<Held>, content: &Arc<AppContentIndex>) -> RecordIndex {
    let (wake, woken) = std::sync::mpsc::channel();
    std::mem::forget(woken);
    RecordIndex {
        source: Arc::new(Source {
            workspace: "w".to_owned(),
            held: Arc::clone(held),
        }),
        reader: Arc::new(Reader),
        content: Some(Arc::clone(content)),
        queued: Mutex::new(BTreeSet::new()),
        wake,
        touched: crate::platform::records::Touched::default(),
    }
}

#[test]
fn what_a_record_holds_is_found_with_its_app_never_launched() {
    let held = Arc::new(Held::default());
    held.hold(
        "chat",
        "chat.workspace",
        vec![entry("channels/c1", "Design review")],
    );
    held.hold(
        "chat.messages",
        "part-1",
        vec![entry("messages/m1", "the wheel is loose again")],
    );
    held.hold("chat.agent", "agents", vec![entry("agents/a1", "Ada")]);
    let content = Arc::new(AppContentIndex::in_memory());

    let index = index(&held, &content);
    index.sweep();
    index.pass();

    assert_eq!(
        content.title("w", "channels/c1").as_deref(),
        Some("Design review"),
        "the conversation is found from its record, not a window"
    );
    assert_eq!(
        content.title("w", "messages/m1").as_deref(),
        Some("the wheel is loose again"),
        "so is a message"
    );
    assert_eq!(
        content.title("w", "agents/a1").as_deref(),
        Some("Ada"),
        "and an agent"
    );
}

#[test]
fn a_deleted_message_is_gone_from_search() {
    let held = Arc::new(Held::default());
    held.hold(
        "chat.messages",
        "part-1",
        vec![
            entry("messages/m1", "the wheel is loose again"),
            entry("messages/m2", "I will look at it"),
        ],
    );
    let content = Arc::new(AppContentIndex::in_memory());

    let index = index(&held, &content);
    index.sweep();
    index.pass();
    assert_eq!(
        content.title("w", "messages/m1").as_deref(),
        Some("the wheel is loose again")
    );

    held.hold(
        "chat.messages",
        "part-1",
        vec![entry("messages/m2", "I will look at it")],
    );
    index.changed("w", "chat.messages", "part-1");
    index.pass();

    assert_eq!(
        content.title("w", "messages/m1"),
        None,
        "what the record no longer holds is not found"
    );
    assert_eq!(
        content.title("w", "messages/m2").as_deref(),
        Some("I will look at it"),
        "what it still holds is"
    );
}

#[test]
fn a_change_reads_only_the_record_it_changed() {
    let held = Arc::new(Held::default());
    held.hold(
        "chat",
        "chat.workspace",
        vec![entry("channels/c1", "Design review")],
    );
    held.hold(
        "chat.messages",
        "part-1",
        vec![entry("messages/m1", "hello")],
    );
    let content = Arc::new(AppContentIndex::in_memory());

    let index = index(&held, &content);
    index.sweep();
    index.pass();
    assert_eq!(held.read("chat", "chat.workspace"), 1);
    assert_eq!(held.read("chat.messages", "part-1"), 1);

    held.hold(
        "chat.messages",
        "part-1",
        vec![entry("messages/m1", "hello, again")],
    );
    index.changed("w", "chat.messages", "part-1");
    index.pass();

    assert_eq!(
        held.read("chat.messages", "part-1"),
        2,
        "the changed record is read again"
    );
    assert_eq!(
        held.read("chat", "chat.workspace"),
        1,
        "and the workspace's record, unchanged, is not"
    );
    assert_eq!(
        content.title("w", "messages/m1").as_deref(),
        Some("hello, again")
    );
}

#[test]
fn a_change_that_is_batched_is_read_once() {
    let held = Arc::new(Held::default());
    held.hold(
        "chat.messages",
        "part-1",
        vec![entry("messages/m1", "hello")],
    );
    let content = Arc::new(AppContentIndex::in_memory());

    let index = index(&held, &content);
    index.changed("w", "chat.messages", "part-1");
    index.changed("w", "chat.messages", "part-1");
    assert_eq!(
        index.waiting(),
        1,
        "a record waits once however often it changed"
    );
    index.pass();
    assert_eq!(index.waiting(), 0);
    assert_eq!(held.read("chat.messages", "part-1"), 1);
}
