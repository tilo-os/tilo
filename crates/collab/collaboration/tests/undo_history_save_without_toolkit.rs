//! A process without the toolkit keeps a record's undo, history and save
//! state with the code native apps keep them with: the record's own undo log
//! over its document, the steps the document records with their authors, and
//! the record's on-disk copy in the room's store.

use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};
use collab_types::presence::UserId;
use collaboration::history_source::authors::{HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME};
use collaboration::history_source::steps::{self, Scope};
use collaboration::history_source::words::HistoryWords;
use collaboration::record::RecordId;
use collaboration::record::save::{RecordSaveStatus, record_document};
use collaboration::state::UndoLog;
use durable_store::DurableStore;

const PACKAGE: &str = "dev.tilo.undo-without-toolkit";
const ROOM: &str = "room-1";

/// The host's words: a person by their principal, a step by its purpose.
struct Words;

impl ResolveAuthor for Words {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        HistoryAuthor::new(UNKNOWN_AUTHOR_NAME, 0.0, actor_hex)
    }
}

impl HistoryWords for Words {
    fn sentence(&self, entry: &tilo_actions::ActionContext) -> String {
        entry.purpose.clone().unwrap_or_default()
    }
}

fn title(doc: &AutoCommit) -> Option<String> {
    let (value, _) = doc.get(ROOT, "title").ok()??;
    value.into_string().ok()
}

/// The authors of the document's steps, oldest first, as their entries
/// record them.
fn step_authors(doc: &mut AutoCommit) -> Vec<String> {
    tilo_resource::history::versions(doc)
        .iter()
        .filter_map(|version| version.context.as_ref()?.invoker.as_ref())
        .map(|invoker| invoker.actor.clone())
        .collect()
}

#[test]
fn a_record_keeps_its_undo_history_and_save_state_without_the_toolkit() {
    let profile = tempfile::tempdir().unwrap();
    let documents: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(profile.path()));
    let record = RecordId::minted().storage_key();
    let ada = UserId("ada".into());

    // The record, its undo log and the writer of its on-disk copy.
    let mut store = record_document(&documents, PACKAGE, ROOM, &record).unwrap();
    let mut doc = store.load_or_new();
    // Every change of the record is its original persistent writer's, as
    // the toolkit's record store sets it.
    doc.set_actor(store.actor().clone());
    let mut history = UndoLog::new(&mut doc);
    let mut writer = store.writer();
    // A new record holds no edits of its own, so it has nothing to save.
    assert_eq!(
        RecordSaveStatus::of_writer(&doc, &writer),
        RecordSaveStatus::Saved
    );
    writer.request(&mut doc);
    writer.flush();
    assert_eq!(
        RecordSaveStatus::of_writer(&doc, &writer),
        RecordSaveStatus::Saved
    );

    // An edit is a step by its author.
    doc.put(ROOT, "title", "Draft").unwrap();
    assert!(history.commit_action_authored(&mut doc, None, None, Some(ada.clone())));
    assert_eq!(title(&doc).as_deref(), Some("Draft"));
    let step = history
        .next_undo_as(&doc, Some(&ada))
        .expect("a step to undo");
    assert_eq!(step.author, Some(ada.clone()));
    assert_eq!(step_authors(&mut doc), ["ada"]);
    assert_eq!(
        RecordSaveStatus::of_writer(&doc, &writer),
        RecordSaveStatus::Saving
    );

    // Undo takes it back as a new change; redo brings it back.
    history
        .undo_as(&mut doc, Some(&ada), None, None)
        .expect("undo");
    assert_eq!(title(&doc), None);
    assert!(!history.can_undo_as(&doc, Some(&ada)));
    assert!(history.can_redo_as(&doc, Some(&ada)));
    history
        .redo_as(&mut doc, Some(&ada), None, None)
        .expect("redo");
    assert_eq!(title(&doc).as_deref(), Some("Draft"));
    assert!(steps::undone(&doc).is_empty(), "the redo restored the step");
    // The undo and redo are steps of their own, by the same author.
    assert_eq!(step_authors(&mut doc), ["ada", "ada", "ada"]);

    // Saved: the writer reports it once the store holds it.
    writer.request(&mut doc);
    writer.flush();
    assert_eq!(
        RecordSaveStatus::of_writer(&doc, &writer),
        RecordSaveStatus::Saved
    );
    let heads = doc.get_heads();
    drop(writer);

    // Reloaded, the record holds its history: the same heads and authors,
    // and a new session's undo log takes the step back again.
    let mut store = record_document(&documents, PACKAGE, ROOM, &record).unwrap();
    let mut reloaded = store
        .load_complete()
        .unwrap()
        .expect("the record was saved");
    reloaded.set_actor(store.actor().clone());
    assert_eq!(reloaded.get_heads(), heads);
    assert_eq!(title(&reloaded).as_deref(), Some("Draft"));
    assert_eq!(step_authors(&mut reloaded), ["ada", "ada", "ada"]);
    let mut history = UndoLog::new(&mut reloaded);
    assert!(history.can_undo_as(&reloaded, Some(&ada)));

    // An agent's undo by scope reads the same steps from the document.
    let undone = steps::undo(
        &mut reloaded,
        &Scope::Invoker("ada".into()),
        1,
        |_| Ok(None),
        &Words,
    )
    .expect("undo by scope");
    assert_eq!(undone.len(), 1);
    reloaded.commit();
    assert_eq!(title(&reloaded), None);
    history.rebase(&mut reloaded);
}
