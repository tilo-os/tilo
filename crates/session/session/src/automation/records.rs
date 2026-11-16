//! A person's record changes as trigger events.
//! Each change of a record in a workspace a shortcut of this device may watch is read for the
//! changes after the heads the record was last seen at, and each one that carries a history entry
//! becomes a [`RecordEvent`]. The first look at a record only learns its heads: a shortcut saved
//! today never fires on last year's messages.
//! The records are followed as every role that follows them does: off the one stream of record
//! changes ([`Changes`]) and through the store they are kept in, on a worker of the feed's own. The
//! session's loop only takes what the worker found ([`RecordFeed::take`]).
//! A workspace a shortcut starts watching is read on the loop's pass that armed it
//! ([`RecordFeed::follow`]), so the heads it learns as its baseline are the heads at that moment: a
//! change made after the shortcut is shown is one it fires on, never one swallowed by a baseline
//! read that happened later.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use automerge::{AutoCommit, ChangeHash};
use durable_store::DurableStore;
use tilo_actions::ActionContext;
use tilo_automation::dispatch::{RecordEvent, event_of};

use crate::platform::Store;
use crate::platform::records::{Changed, Changes, document, named_checkouts};

/// What the worker keeps between reads.
#[derive(Default)]
struct Worker {
    /// The workspaces a shortcut watches, by id: only their records are read.
    workspaces: BTreeSet<String>,
    /// The heads each record was last read at, by its workspace, app and route.
    cursors: BTreeMap<(String, String, String), Vec<ChangeHash>>,
    /// What the worker found and the loop has not taken yet.
    found: Vec<RecordEvent>,
}

/// A person's record changes as trigger events, followed on a worker.
pub struct RecordFeed {
    /// What the worker keeps between reads, shared with the loop, which reads a workspace's
    /// records once when it starts watching them.
    worker: Arc<Mutex<Worker>>,
    /// The store the records are kept in; `None` on a session that keeps none.
    store: Option<Store>,
}

impl RecordFeed {
    /// Follow the records `records` keeps, as its stream notes their changes, waking the loop
    /// through `wake` when one became an event. Nothing is read until a shortcut watches a
    /// workspace ([`Self::follow`]).
    pub fn start(records: Option<(Store, Changes)>, wake: tilo_service::Wake) -> Self {
        let worker = Arc::new(Mutex::new(Worker::default()));
        let store = records.map(|(store, changes)| {
            let reader = changes.reader();
            let (following, reading) = (Arc::downgrade(&worker), Store::clone(&store));
            let spawned = std::thread::Builder::new()
                .name("tilo-automation-records".into())
                .spawn(move || {
                    // Ends with the stream, or with the feed.
                    while let Ok(changed) = reader.recv() {
                        let Some(worker) = following.upgrade() else {
                            break;
                        };
                        let mut worker = lock(&worker);
                        let mut found = worker.look(reading.as_ref(), &changed);
                        // A burst of changes is read before the loop is woken once.
                        while let Ok(more) = reader.try_recv() {
                            found |= worker.look(reading.as_ref(), &more);
                        }
                        drop(worker);
                        if found {
                            wake();
                        }
                    }
                });
            if let Err(error) = spawned {
                tracing::warn!(%error, "record changes fire no shortcut");
            }
            store
        });
        Self { worker, store }
    }

    /// Read the records of exactly `workspaces` from now on: the workspaces where an armed shortcut
    /// watches record changes. A workspace that starts being watched is read here, before this
    /// returns, so the heads learned as the baseline are the heads the person's shortcut was shown
    /// at, and every change written after it is one the worker reports.
    pub fn follow(&self, workspaces: BTreeSet<String>) {
        let mut worker = lock(&self.worker);
        if worker.workspaces == workspaces {
            return;
        }
        let added: Vec<String> = workspaces.difference(&worker.workspaces).cloned().collect();
        worker
            .cursors
            .retain(|(workspace, ..), _| workspaces.contains(workspace));
        worker.workspaces = workspaces;
        let Some(store) = &self.store else {
            return;
        };
        let indexes = store.workspace_indexes().unwrap_or_default();
        for workspace in added {
            for (app, _) in indexes.iter().filter(|(_, held)| *held == workspace) {
                for (checkout, _) in
                    named_checkouts(store.as_ref(), app, &workspace).unwrap_or_default()
                {
                    worker.look(
                        store.as_ref(),
                        &Changed {
                            workspace: workspace.clone(),
                            app: app.clone(),
                            record: checkout.id.route().to_owned(),
                            replayed: true,
                        },
                    );
                }
            }
        }
    }

    /// The events the worker found since the last call, oldest first.
    pub fn take(&self) -> Vec<RecordEvent> {
        std::mem::take(&mut lock(&self.worker).found)
    }
}

/// The worker's state, whoever holds it.
fn lock(worker: &Mutex<Worker>) -> MutexGuard<'_, Worker> {
    worker.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Worker {
    /// Read the record `changed` reached, where a shortcut watches its workspace: what changed
    /// since its heads were last read becomes events, and its heads are kept. Whether it found any.
    fn look(&mut self, store: &dyn DurableStore, changed: &Changed) -> bool {
        // The workspace's own document is its list of records, not one of them.
        if changed.record.is_empty() || !self.workspaces.contains(&changed.workspace) {
            return false;
        }
        let Some(doc) = document(store, &changed.app, &changed.workspace, &changed.record) else {
            return false;
        };
        let kind = ui::record::identity(&doc)
            .map(|identity| identity.kind.as_str().to_owned())
            .unwrap_or_default();
        let key = (
            changed.workspace.clone(),
            changed.app.clone(),
            changed.record.clone(),
        );
        let before = self.found.len();
        // A record looked at for the first time only has its heads learned.
        if let Some(since) = self.cursors.insert(key, doc.committed_heads()) {
            self.found.extend(record_events(
                &changed.workspace,
                &changed.record,
                &kind,
                &doc,
                &since,
            ));
        }
        self.found.len() > before
    }
}

/// The changes of `doc` after `since` , in causal order, as events.
#[must_use]
pub fn record_events(
    workspace: &str,
    record: &str,
    kind: &str,
    doc: &AutoCommit,
    since: &[ChangeHash],
) -> Vec<RecordEvent> {
    let committed = doc.committed_document();
    if committed.get_heads() == since {
        return Vec::new();
    }
    committed
        .get_changes(since)
        .into_iter()
        .filter_map(|change| {
            let context = ActionContext::from_change_message(change.message()?)?;
            Some(event_of(
                workspace,
                record,
                kind,
                &change.hash().to_string(),
                &context,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::{CommitOptions, Transactable as _};

    tilo_test_env::scenario! {
        fn a_committed_change_is_a_record_event_only_with_its_entry;

        phase a_committed_verb_becomes_a_record_event_with_its_actor_and_facet;
        {
            let mut doc = AutoCommit::new();
            doc.put(automerge::ROOT, "seed", 1).unwrap();
            let before = doc.get_heads();
            let context = ActionContext::for_app("so.tilo.chat", "Chat")
                .with_invoker(tilo_actions::ActionInvoker::person("bob"))
                .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                    tilo_actions::ActionKey::new("so.tilo.chat", "chat.workspace", "post"),
                    [],
                )))
                .with_facet(tilo_actions::ActionFacet::new(
                    tilo_actions::INCOMING_MESSAGE_FACET,
                    "true",
                ));
            doc.put(automerge::ROOT, "text", "hi").unwrap();
            let hash = doc
                .commit_with(CommitOptions::default().with_message(context.change_message().unwrap()))
                .unwrap();
            let events = record_events("workspace-1", "rec-1", "chat.workspace", &doc, &before);
            assert_eq!(events.len(), 1);
            let event = &events[0];
            assert_eq!(event.change, hash.to_string());
            assert_eq!(event.actor, tilo_actions::Acting::person("bob"));
            assert!(event.incoming_message);
            assert_eq!(
                tilo_automation::dispatch::verbs_of(&event.did),
                vec!["post".to_string()]
            );
            // Nothing new after the last look.
            assert!(
                record_events(
                    "workspace-1",
                    "rec-1",
                    "chat.workspace",
                    &doc,
                    &doc.committed_heads()
                )
                .is_empty()
            );
        }

        phase a_change_without_an_entry_is_no_event;
        {
            let mut doc = AutoCommit::new();
            let before = doc.get_heads();
            doc.put(automerge::ROOT, "x", 1).unwrap();
            doc.commit();
            assert!(record_events("r", "rec", "k", &doc, &before).is_empty());
        }
    }

    /// A record a file opened as, kept under a storage key its route does not spell: a change of it
    /// written through the records' store reaches the shortcuts by the record's route.
    #[test]
    fn a_change_written_to_a_file_record_becomes_an_event() {
        use durable_store::{StorageKey, WriteOp};

        const APP: &str = "so.tilo.word-processor";
        const WORKSPACE: &str = "7c307c8d-bfa5-46fa-ae08-fa2fe4790bd4";
        let dir = tempfile::tempdir().unwrap();
        let changes = Changes::default();
        let store = crate::platform::records::noting(
            Arc::new(app_state::FsStore::new(dir.path())),
            changes.clone(),
        );
        let write = |key: &StorageKey, doc: &mut AutoCommit| {
            store
                .write(key, WriteOp::Replace(doc.save()), None)
                .unwrap();
        };
        let id = ui::record::DocumentId::for_file("/run/tilo/documents/Budget for Friday.txt");
        let kind = ui::record::ResourceType::new("word-processor.document");
        let mut index = AutoCommit::new();
        assert!(ui::record::index::reference(&mut index, &id, &kind));
        index.commit();
        let named = StorageKey::workspace_index(APP, WORKSPACE);
        let key = crate::platform::records::storage_key(APP, WORKSPACE, id.route()).unwrap();
        let _leases = (store.lease(&named).unwrap(), store.lease(&key).unwrap());
        write(&named, &mut index);
        let mut doc = AutoCommit::new();
        ui::record::init(&mut doc, &id, &kind, WORKSPACE);
        doc.commit();
        write(&key, &mut doc);

        let (woke, woken) = std::sync::mpsc::channel();
        let feed = RecordFeed::start(
            Some((Store::clone(&store), changes)),
            Arc::new(move || {
                let _ = woke.send(());
            }),
        );
        feed.follow(BTreeSet::from([WORKSPACE.to_owned()]));
        let context = ActionContext::for_app(APP, "Word Processor")
            .with_invoker(tilo_actions::ActionInvoker::person("bob"))
            .with_described(tilo_actions::Described::new(tilo_actions::Did::verb(
                tilo_actions::ActionKey::new(APP, kind.as_str(), "type"),
                [],
            )));
        doc.put(automerge::ROOT, "text", "Catering").unwrap();
        doc.commit_with(CommitOptions::default().with_message(context.change_message().unwrap()));
        write(&key, &mut doc);

        woken
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the change woke the loop");
        let events = feed.take();
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0].record, id.route());
    }
}
