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
    /// The records whose keys are not here: they read as nothing.
    sealed: Mutex<std::collections::BTreeSet<String>>,
    /// The reader is stopped before it answers, as a session ending stops it.
    stopped: Mutex<bool>,
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

/// What the workspace keeps, as this device's service lists and reads it.
struct Source {
    workspace: String,
    held: Arc<Held>,
    /// Whether the service holds the workspace: one it does not is listed by none, and its
    /// documents read as the service answers, with an error.
    holds: Arc<Mutex<bool>>,
}

impl DocumentSource for Source {
    fn workspaces(&self) -> Vec<String> {
        let holds = *self.holds.lock_recover();
        holds.then(|| self.workspace.clone()).into_iter().collect()
    }

    fn records(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        if workspace != self.workspace || !*self.holds.lock_recover() {
            return Err("this service does not hold the workspace".to_owned());
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
        if self.held.sealed.lock_recover().contains(record) {
            return Ok(None);
        }
        self.held
            .reads
            .lock_recover()
            .push(format!("{app}/{record}"));
        serde_json::to_vec(&entries)
            .map(Some)
            .map_err(|error| error.to_string())
    }
}

/// What the app's own reader answers: the entries the fake document holds, or that it was stopped
/// before it answered.
struct Reader(Arc<Held>);

impl RecordReader for Reader {
    fn entries(&self, _app: &str, doc: &[u8]) -> Result<Vec<IndexEntry>, Unread> {
        if *self.0.stopped.lock_recover() {
            return Err(Unread::Stopped("the reader was stopped".into()));
        }
        serde_json::from_slice(doc).map_err(|error| Unread::Refused(error.to_string()))
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

/// An index over the workspace `held` holds, which nothing wakes: the test reads a pass itself.
fn index(held: &Arc<Held>, content: &Arc<AppContentIndex>) -> RecordIndex {
    index_over(
        Arc::new(Source {
            workspace: "w".to_owned(),
            held: Arc::clone(held),
            holds: Arc::new(Mutex::new(true)),
        }),
        held,
        content,
    )
}

/// [`index`] over `source`, the stream noting only what the source holds, as the session's own
/// stream does.
fn index_over(
    source: Arc<Source>,
    held: &Arc<Held>,
    content: &Arc<AppContentIndex>,
) -> RecordIndex {
    let (wake, woken) = std::sync::mpsc::channel();
    std::mem::forget(woken);
    let changes = crate::platform::records::Changes::default();
    changes.holding({
        let source = Arc::clone(&source);
        Arc::new(move |workspace: &str| source.workspaces().iter().any(|held| held == workspace))
    });
    RecordIndex {
        source: source as Arc<dyn DocumentSource>,
        reader: Arc::new(Reader(Arc::clone(held))),
        content: Some(Arc::clone(content)),
        queued: Mutex::new(BTreeSet::new()),
        sealed: Mutex::new(BTreeSet::new()),
        swept: Mutex::new(BTreeSet::new()),
        wake,
        changes,
    }
}

tilo_test_env::scenario! {
    fn the_record_index_reads_what_changed_and_search_finds_it;

    phase what_a_record_holds_is_found_with_its_app_never_launched;
    {
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

    phase a_record_sealed_at_the_sweep_is_found_once_its_keys_arrive;
    // An agent whose record's keys reach this device after the sweep read it is found once the
    // channels settled, without any change to the record.
    {
        let held = Arc::new(Held::default());
        held.hold("so.tilo.chat", "agents", vec![entry("agents/a1", "Ada")]);
        held.sealed.lock_recover().insert("agents".to_owned());
        let content = Arc::new(AppContentIndex::in_memory());

        let index = index(&held, &content);
        index.sweep();
        index.pass();
        assert_eq!(content.title("w", "agents/a1"), None);

        index.settled();
        index.pass();
        assert_eq!(
            content.title("w", "agents/a1"),
            None,
            "still sealed: still waiting"
        );
        assert_eq!(index.waiting(), 0);

        held.sealed.lock_recover().clear();
        index.settled();
        index.pass();
        assert_eq!(content.title("w", "agents/a1").as_deref(), Some("Ada"));
        index.settled();
        assert_eq!(
            index.waiting(),
            0,
            "read once its keys came, it waits for nothing"
        );
    }

    phase a_deleted_message_is_gone_from_search;
    {
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

    phase a_change_reads_only_the_record_it_changed;
    {
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

    phase a_change_that_is_batched_is_read_once;
    {
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

    phase a_change_of_a_workspace_this_device_does_not_hold_waits_for_it;
    // A change of a workspace the service holds nothing of is nothing search can read: it is not
    // queued, not read, and nothing is said of it. What the workspace holds is read once the
    // service holds it, when the keys settle.
    {
        let held = Arc::new(Held::default());
        held.hold(
            "chat.messages",
            "part-1",
            vec![entry("messages/m1", "hello")],
        );
        let content = Arc::new(AppContentIndex::in_memory());
        let holds = Arc::new(Mutex::new(false));
        let index = index_over(
            Arc::new(Source {
                workspace: "w".to_owned(),
                held: Arc::clone(&held),
                holds: Arc::clone(&holds),
            }),
            &held,
            &content,
        );

        index.changed("w", "chat.messages", "part-1");
        assert_eq!(
            index.waiting(),
            0,
            "a change of an unheld workspace is not queued"
        );
        index.pass();
        assert_eq!(
            held.read("chat.messages", "part-1"),
            0,
            "and never read, so nothing is said of it"
        );

        *holds.lock_recover() = true;
        index.settled();
        index.pass();
        assert_eq!(
            content.title("w", "messages/m1").as_deref(),
            Some("hello"),
            "read once the service holds the workspace"
        );
    }

    phase a_stopped_reader_leaves_the_record_for_the_next_pass;
    // A reader stopped before it answered says nothing about what the record holds: the record waits
    // for the next pass, and what search already held of it is not dropped for an answer nobody gave.
    {
        let held = Arc::new(Held::default());
        held.hold(
            "chat.messages",
            "part-1",
            vec![entry("messages/m1", "hello")],
        );
        let content = Arc::new(AppContentIndex::in_memory());
        let index = index(&held, &content);
        index.sweep();
        index.pass();
        assert_eq!(content.title("w", "messages/m1").as_deref(), Some("hello"));

        *held.stopped.lock_recover() = true;
        held.hold(
            "chat.messages",
            "part-1",
            vec![entry("messages/m2", "I will look at it")],
        );
        index.changed("w", "chat.messages", "part-1");
        index.pass();
        assert_eq!(
            index.waiting(),
            1,
            "a reading nobody finished waits to be read"
        );
        assert_eq!(
            content.title("w", "messages/m2"),
            None,
            "a stopped reader found nothing, and nothing is published as if it had"
        );
        assert_eq!(
            content.title("w", "messages/m1").as_deref(),
            Some("hello"),
            "what search held of the record is not what a stopped reader said"
        );

        *held.stopped.lock_recover() = false;
        index.pass();
        assert_eq!(index.waiting(), 0);
        assert_eq!(
            content.title("w", "messages/m2").as_deref(),
            Some("I will look at it"),
            "read again, the record is found as it now stands"
        );
    }
}
