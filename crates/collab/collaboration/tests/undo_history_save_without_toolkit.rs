//! A process without the toolkit keeps a record's undo, history and save
//! state with the code native apps keep them with: the record's own undo log
//! over its document, the steps the document records with their authors, and
//! the record's on-disk copy in the workspace's store.

use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{ActorId, AutoCommit, ObjType, ROOT, ReadDoc};
use collab_types::presence::UserId;
use collaboration::history_source::authors::{HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME};
use collaboration::history_source::steps::{self, Scope};
use collaboration::history_source::verbs::HistoryEdit;
use collaboration::history_source::words::HistoryWords;
use collaboration::record::RecordId;
use collaboration::record::save::{RecordSaveStatus, record_document};
use collaboration::state::UndoLog;
use durable_store::DurableStore;

#[global_allocator]
static HEAP: tilo_automerge::heap::ThreadCounting = tilo_automerge::heap::ThreadCounting;

const PACKAGE: &str = "dev.tilo.undo-without-toolkit";
const WORKSPACE: &str = "workspace-1";

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
        .map(tilo_actions::ActionInvoker::key)
        .collect()
}

#[test]
fn a_record_keeps_its_undo_history_and_save_state_without_the_toolkit() {
    let profile = tempfile::tempdir().unwrap();
    let documents: Rc<dyn DurableStore> = Rc::new(app_state::FsStore::new(profile.path()));
    let record = RecordId::minted().storage_key();
    let ada = UserId("ada".into());

    // The record, its undo log and the writer of its on-disk copy.
    let mut store = record_document(&documents, PACKAGE, WORKSPACE, &record).unwrap();
    let mut doc = store.load_or_new();
    // Every change of the record is its original persistent writer's, as
    // the toolkit's record store sets it.
    doc.set_actor(store.actor());
    let mut history = UndoLog::new(&mut doc);
    let mut writer = store.writer();
    // A new record holds no edits of its own, so it has nothing to save.
    assert_eq!(
        RecordSaveStatus::of_writer(&doc, &writer),
        RecordSaveStatus::Saved
    );
    doc.commit();
    writer.request(&doc);
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
    doc.commit();
    writer.request(&doc);
    writer.flush();
    assert_eq!(
        RecordSaveStatus::of_writer(&doc, &writer),
        RecordSaveStatus::Saved
    );
    let heads = doc.get_heads();
    drop(writer);

    // Reloaded, the record holds its history: the same heads and authors,
    // and a new session's undo log takes the step back again.
    let mut store = record_document(&documents, PACKAGE, WORKSPACE, &record).unwrap();
    let mut reloaded = store
        .load_complete()
        .unwrap()
        .expect("the record was saved");
    reloaded.set_actor(store.actor());
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
        |_, _| Ok(None),
        &Words,
    )
    .expect("undo by scope");
    assert_eq!(undone.len(), 1);
    reloaded.commit();
    assert_eq!(title(&reloaded), None);
    history.rebase(&mut reloaded);
}

/// Undoing a one-word step in a record that holds a mebibyte of text costs that word: the undo
/// reads and writes what the step changed, and copies nothing of the rest of the record.
#[test]
fn undo_of_a_small_step_costs_the_step_not_the_record() {
    const MEBIBYTE: usize = 1 << 20;
    let mut doc = AutoCommit::new();
    doc.set_actor(ActorId::from(b"grace".as_slice()));
    let body = doc.put_object(ROOT, "body", ObjType::Text).unwrap();
    doc.splice_text(&body, 0, 0, &"pasted ".repeat(MEBIBYTE / 7 + 1))
        .unwrap();
    doc.commit();
    doc.set_actor(ActorId::from(b"ada".as_slice()));
    doc.put(ROOT, "title", "Draft").unwrap();
    doc.commit();

    let undo = HistoryEdit::Undo {
        steps: None,
        run: None,
        by: None,
        to: None,
        step: None,
    };
    // Reading the text once: what an undo that walked the record would cost at least.
    let reading = tilo_clock::now();
    assert!(doc.text(&body).unwrap().len() > MEBIBYTE);
    let read = reading.elapsed();
    let undoing = tilo_clock::now();
    let cost = tilo_automerge::heap::allocated_by(|| {
        undo.apply_registered(&mut doc, |_| {}, &Words)
            .expect("undo");
        doc.commit();
    });
    let took = undoing.elapsed();

    assert_eq!(title(&doc), None, "the undo took the title back");
    assert_eq!(doc.length(&body), "pasted ".len() * (MEBIBYTE / 7 + 1));
    assert!(
        cost < MEBIBYTE / 8,
        "the undo allocated {cost} bytes for a one-word step"
    );
    assert!(
        took < read,
        "the undo took {took:?}, reading the text {read:?}"
    );
}
