//! A chain's API answering a create it cannot make yet: it says so, and the
//! same create answers what it made once its part is here.

use std::rc::Rc;

use durable_store::{DurableStore as _, StorageKey};
use tilo_actions::ActionInvoker;
use tilo_resource::Collection;
use ui_core::resource::{DocumentId, ResourceOwners, ResourceStore};

use super::super::resources::ResourceApi;
use super::{Chain, Chained};
use crate::prelude::*;

const APP: &str = "so.tilo.test-chain-api";

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Log {
    pub title: String,
}

#[resource(kind = "test.chain-api-log", catalogue = "tests/locales")]
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
    pub lines: Collection<Line>,
}

#[resource(kind = "test.chain-api-log-part", catalogue = "tests/locales")]
impl LogPart {
    /// Write a line.
    #[action(writes = lines)]
    pub fn write(&mut self, cx: &mut ActionCx, text: String) {
        self.lines.push(cx.id(), Line { text });
    }
}

impl Chained for LogPart {
    type Base = Log;

    fn chain() -> DocumentId {
        DocumentId::derived("lines", &DocumentId::workspace_default())
    }
}

fn device_keeping(memory: &Rc<durable_store::MemoryStore>, workspace: &str) -> ResourceStore {
    ResourceOwners::with_store(Rc::clone(memory) as Rc<dyn durable_store::DurableStore>)
        .open(APP, workspace)
        .unwrap()
}

fn chain_in(store: &ResourceStore, me: &str) -> Chain<LogPart> {
    let me = &MemberId::person(me);
    let log = Rc::new(Log::default());
    Chain::detached_in(store.clone(), APP, me, move || Rc::clone(&log)).unwrap()
}

fn chain_with_device_queue(store: &ResourceStore, profile: &std::path::Path) -> Chain<LogPart> {
    use super::{ChainState, QueuedWrites, WorkspaceResources, detached_dispatch};
    let author = MemberId::person("bo");
    let workspace = WorkspaceResources {
        store: store.clone(),
        me: author.clone(),
        app_id: APP,
        app_title: APP,
        device_queue: Some(
            ui_core::storage::Settings::<QueuedWrites>::for_app(APP).in_profile(profile),
        ),
    };
    let log = Rc::new(Log::default());
    let state = Rc::new(
        ChainState::open::<LogPart>(workspace, move || Rc::clone(&log), LogPart::chain()).unwrap(),
    );
    let dispatch = detached_dispatch(APP, &state, &author);
    Chain { state, dispatch }
}

/// A device whose workspace holds a part of the chain it has not sent yet:
/// the workspace's newest part is asked for as a name, and its document has
/// not arrived here.
struct Arriving {
    reader: ResourceStore,
    chain: Chain<LogPart>,
    newest: DocumentId,
    /// The app's construction, as the workspace's first documents are made.
    _construction: crate::collaboration::native_initial::NativeConstruction,
}

impl Arriving {
    fn new() -> Self {
        let workspace = uuid::Uuid::new_v4().to_string();
        // The workspace's newest part, as a member's device wrote it.
        let theirs = Rc::new(durable_store::MemoryStore::new());
        let writer =
            ResourceOwners::with_store(Rc::clone(&theirs) as Rc<dyn durable_store::DurableStore>)
                .open(APP, &workspace)
                .unwrap();
        let writing = chain_in(&writer, "ada");
        writing
            .invoke(&LogPart::WRITE.with(log_part::Write { text: "one".into() }))
            .unwrap();
        let newest = LogPart::chain().part(0);
        let kept = theirs
            .read(&StorageKey::document(APP, &workspace, newest.storage_key()))
            .unwrap()
            .expect("the writer kept the part")
            .bytes;

        // This device's window, its documents read through its broker.
        let (broker, _joins) =
            crate::collaboration::CollaborationManager::native_initial_fixture_for_test(
                APP,
                uuid::Uuid::parse_str(&workspace).unwrap(),
                kept,
            );
        let construction = crate::collaboration::native_initial::NativeConstruction::enter(broker)
            .expect("the app's construction");
        let ours = Rc::new(durable_store::MemoryStore::new());
        let reader = device_keeping(&ours, &workspace);
        reader.ask_workspace_for_missing();
        let chain = chain_in(&reader, "bo");
        assert!(chain.parts().is_empty(), "no part of it is here");
        assert!(
            chain.head_arriving(),
            "the window asks the workspace which part is the newest"
        );
        Self {
            reader,
            chain,
            newest,
            _construction: construction,
        }
    }

    /// The workspace sends the part it holds: the answer to the read the
    /// window asked for, and the part arriving in this device's documents.
    fn arrives(&self) {
        self.reader.announce([self.newest.clone()]);
        let deadline = tilo_test_env::hang_deadline();
        while self.chain.parts().is_empty() {
            assert!(
                crate::time::now() < deadline,
                "the newest part never arrived"
            );
            self.reader.note_kept();
            std::thread::yield_now();
        }
        assert!(!self.chain.head_arriving());
    }
}

#[test]
fn a_device_queue_keeps_waiting_writes_out_of_the_workspace_store() {
    let profile = tempfile::tempdir().unwrap();
    let fixture = Arriving::new();
    let written = LogPart::WRITE.with(log_part::Write { text: "two".into() });
    let chain = chain_with_device_queue(&fixture.reader, profile.path());
    assert_eq!(chain.invoke(&written).unwrap(), None);
    assert!(chain.unwritten().is_some());
    let legacy = super::unsent_checkout(&MemberId::person("bo"));
    assert!(!fixture.reader.is_ready(&legacy.id));

    drop(chain);
    let reopened = chain_with_device_queue(&fixture.reader, profile.path());
    assert!(reopened.unwritten().is_some());
    fixture.arrives();
    let lines: Vec<String> = reopened
        .parts()
        .iter()
        .flat_map(|(_, part)| part.lines.values().map(|line| line.text.clone()))
        .collect();
    assert_eq!(lines, ["one", "two"]);
    assert!(reopened.unwritten().is_none());
}

/// What still waits in the record a person's devices once kept waiting
/// writes in moves into the device queue once and is written once its part
/// is here; the record, which only reads now, is never written.
#[test]
fn earlier_waiting_writes_move_into_the_device_queue_once() {
    use ui_core::automerge::transaction::Transactable as _;
    let workspace = uuid::Uuid::new_v4().to_string();
    let ours = Rc::new(durable_store::MemoryStore::new());
    let reader = device_keeping(&ours, &workspace);
    // A write that waited the way devices kept them before: its entry, as
    // the record of them held it.
    let earlier = super::unsent_checkout(&MemberId::person("bo"));
    let action = LogPart::WRITE
        .with(log_part::Write { text: "two".into() })
        .build(tilo_resource::origin_now_ms())
        .unwrap();
    let entry =
        serde_json::to_string(&(serde_json::to_value(&action).unwrap(), invoker())).unwrap();
    let chain_key = LogPart::chain().as_str().to_owned();
    chain_in(&reader, "bo")
        .state
        .core
        .create(&earlier, |doc| {
            let list = doc
                .put_object(
                    ui_core::automerge::ROOT,
                    chain_key.as_str(),
                    ui_core::automerge::ObjType::List,
                )
                .map_err(|error| error.to_string())?;
            doc.insert(&list, 0, entry.as_str())
                .map_err(|error| error.to_string())
        })
        .unwrap();
    let heads = || {
        reader
            .read(&earlier.id, ui_core::automerge::AutoCommit::base_heads)
            .unwrap()
    };
    let kept = heads();
    let lines = |chain: &Chain<LogPart>| -> Vec<String> {
        chain
            .parts()
            .iter()
            .flat_map(|(_, part)| part.lines.values().map(|line| line.text.clone()))
            .collect()
    };

    // The device asks the workspace which part is the newest: the write waits.
    reader.ask_workspace_for_missing();
    let profile = tempfile::tempdir().unwrap();
    let chain = chain_with_device_queue(&reader, profile.path());
    assert!(chain.head_arriving());
    assert!(chain.unwritten().is_some(), "the earlier write still waits");
    reader.note_workspace_answered(LogPart::chain().chain_head());
    let deadline = tilo_test_env::hang_deadline();
    while lines(&chain).is_empty() {
        assert!(
            crate::time::now() < deadline,
            "the earlier write never landed"
        );
        reader.note_kept();
        std::thread::yield_now();
    }
    assert_eq!(lines(&chain), ["two"]);
    assert!(chain.unwritten().is_none());
    drop(chain);

    let reopened = chain_with_device_queue(&reader, profile.path());
    assert!(reopened.unwritten().is_none(), "nothing moves twice");
    reopened
        .invoke(&LogPart::WRITE.with(log_part::Write {
            text: "three".into(),
        }))
        .unwrap();
    assert_eq!(lines(&reopened), ["two", "three"]);
    assert_eq!(heads(), kept, "the earlier record is not written");
}

/// `write`'s line, as the API delivers it.
fn encoded_write(text: &str) -> tilo_actions::EncodedAction {
    let action = LogPart::WRITE
        .with(log_part::Write { text: text.into() })
        .build(tilo_resource::origin_now_ms())
        .unwrap();
    tilo_resource::encode::<LogPart>(&action).unwrap()
}

fn invoker() -> ActionInvoker {
    ActionInvoker::member(MemberId::person("bo"))
}

fn texts(chain: &Chain<LogPart>) -> Vec<(String, String)> {
    chain
        .parts()
        .iter()
        .flat_map(|(_, part)| part.lines.iter())
        .map(|(id, line)| (id.as_str().to_owned(), line.text.clone()))
        .collect()
}

/// The made items an answer carries.
fn made(output: &tilo_actions::ActionOutput) -> Vec<(String, String)> {
    output
        .value
        .as_ref()
        .and_then(|value| value.get("made"))
        .and_then(serde_json::Value::as_array)
        .map(|made| {
            made.iter()
                .map(|item| {
                    (
                        item["id"].as_str().unwrap_or_default().to_owned(),
                        item["address"].as_str().unwrap_or_default().to_owned(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A create asked of a chain whose part has not arrived says nothing was
/// made, and the same create answers its id once the part is here.
#[test]
fn a_create_that_waits_for_its_part_says_so_and_answers_its_id_once_it_is_here() {
    let arriving = Arriving::new();
    let asked = arriving
        .chain
        .state
        .apply_answering(&encoded_write("two"), &invoker());
    let reason = asked.expect_err("a create that cannot be made says why");
    assert_eq!(reason, tilo_actions::api::NOT_READY_REASON, "{reason}");
    assert!(
        arriving.chain.unwritten().is_none(),
        "nothing was kept to be made later behind the caller's back"
    );
    assert!(
        arriving
            .reader
            .wanted_resources()
            .contains(&LogPart::chain().chain_head()),
        "the newest part is asked for, so the next ask finds it"
    );

    arriving.arrives();
    let output = arriving
        .chain
        .state
        .apply_answering(&encoded_write("two"), &invoker())
        .expect("the create is made once its part is here");
    let made = made(&output);
    assert_eq!(made.len(), 1, "one issue made, with its address: {made:?}");
    let (id, address) = made[0].clone();
    assert_eq!(address, format!("{}/lines/{id}", LogPart::KIND));
    let written = texts(&arriving.chain);
    assert_eq!(
        written.last().map(|(held, _)| held.as_str()),
        Some(id.as_str()),
        "the issue reads back under the id it answered"
    );
    assert_eq!(written.last().map(|(_, text)| text.as_str()), Some("two"));
}

/// A create in a chain the workspace holds no part of starts its first part
/// at once: the workspace's answer for the chain is its answer for every part,
/// so the create is not refused while the first part is asked for again.
#[test]
fn a_create_in_a_chain_the_workspace_holds_none_of_lands_at_once() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let ours = Rc::new(durable_store::MemoryStore::new());
    let reader = device_keeping(&ours, &workspace);
    reader.ask_workspace_for_missing();
    let chain = chain_in(&reader, "bo");
    assert!(
        chain.head_arriving(),
        "which part is the newest is asked for"
    );
    reader.note_workspace_answered(LogPart::chain().chain_head());
    assert!(!chain.head_arriving(), "the workspace holds no part of it");

    let output = chain
        .state
        .apply_answering(&encoded_write("one"), &invoker())
        .expect("the create is made at once");
    assert_eq!(made(&output).len(), 1, "it answers what it made");
    assert_eq!(
        texts(&chain)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>(),
        ["one"]
    );
    assert!(
        !reader
            .wanted_resources()
            .contains(&LogPart::chain().part(0)),
        "the first part is not asked of the workspace"
    );
}

/// Several creates asked as one answer every id once the part is here, and
/// none before.
#[test]
fn a_grouped_create_that_waits_for_its_part_says_so_and_answers_every_id_once_it_is_here() {
    let arriving = Arriving::new();
    let group = |texts: [&str; 2]| {
        let actions = texts
            .into_iter()
            .map(|text| {
                LogPart::WRITE
                    .with(log_part::Write { text: text.into() })
                    .build(tilo_resource::origin_now_ms())
                    .unwrap()
            })
            .collect();
        let group = tilo_resource::group::ActionGroup { actions };
        tilo_actions::EncodedAction::encode(tilo_resource::group::group_type::<LogPart>(), &group)
            .unwrap()
    };
    let asked = arriving
        .chain
        .state
        .apply_answering(&group(["two", "three"]), &invoker());
    let reason = asked.expect_err("a group that cannot be made says why");
    assert_eq!(reason, tilo_actions::api::NOT_READY_REASON, "{reason}");
    assert!(arriving.chain.unwritten().is_none());

    arriving.arrives();
    let output = arriving
        .chain
        .state
        .apply_answering(&group(["two", "three"]), &invoker())
        .expect("the group is made once its part is here");
    let made = made(&output);
    assert_eq!(made.len(), 2, "every id made is answered: {made:?}");
    let written: Vec<String> = texts(&arriving.chain)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(written, ["one", "two", "three"]);
    let held: Vec<String> = texts(&arriving.chain)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    for (id, address) in made {
        assert!(held.contains(&id), "the issue reads back under {id}");
        assert_eq!(address, format!("{}/lines/{id}", LogPart::KIND));
    }
}
