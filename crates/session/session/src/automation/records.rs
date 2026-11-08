//! A person's record changes as trigger events.
//! Each look reads, per record in a workspace a shortcut of this device may watch, the changes
//! after the heads it last saw, and turns each one that carries a history entry into a
//! [`RecordEvent`]. The first look at a record only learns its heads: a shortcut saved today never
//! fires on last year's messages.
//! The documents belong to the apps that write them, and this process reads them the way search
//! does: a tree watch over the device's data home, and a worker that reads again whenever a record
//! or a workspace index below it changes. The session's loop only takes what the worker found
//! ([`RecordFeed::take`]); it never opens, lists or stats a path, and there is no second store: the
//! record documents read here are the ones the apps wrote.
//! A workspace a shortcut starts watching is read on the loop's pass that armed it
//! ([`RecordFeed::follow`]), so the heads it learns as its baseline are the heads at that moment: a
//! change made after the shortcut is shown is one it fires on, never one swallowed by a baseline
//! read that happened later.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use app_state::{DOCUMENT_FILE, WORKSPACES_DIR};
use automerge::{AutoCommit, ChangeHash, LoadOptions, OnPartialLoad};
use tilo_actions::ActionContext;
use tilo_automation::dispatch::{RecordEvent, event_of};
use tilo_files::native::{ChangeWatch, watch_tree_changes};

/// Ceiling on a record document this worker reads: Automerge cannot read one in part, and a file
/// past this is not a record an app wrote.
const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

/// One record's document as last read, and where it was.
#[derive(Clone, Debug)]
struct Seen {
    /// The workspace's name for the record, and its kind.
    kind: String,
    /// When the file was last written and how large it was: a document whose stamp did not move has
    /// no change to turn into an event.
    stamp: Option<(u64, u64)>,
}

/// What the worker keeps between reads.
#[derive(Default)]
struct Worker {
    /// The workspaces a shortcut watches, by id: only their records are read.
    workspaces: BTreeSet<String>,
    /// Every record read so far, by (workspace, record): its kind and stamp.
    seen: BTreeMap<(String, String), Seen>,
    /// The heads each record was last read at.
    cursors: BTreeMap<(String, String), Vec<ChangeHash>>,
}

/// A person's record changes as trigger events, followed on a worker.
pub struct RecordFeed {
    /// What the worker found and the loop has not taken yet.
    found: Arc<Mutex<Vec<RecordEvent>>>,
    /// What the worker keeps between reads: the workspaces it watches, the heads each record was
    /// read at. Shared with the loop, which reads a workspace's records once when it starts
    /// watching them.
    reading: Arc<Mutex<Worker>>,
    /// The data home the worker reads, as [`RecordFeed::follow`] reads it.
    root: PathBuf,
    worker: Option<std::thread::JoinHandle<()>>,
    /// Ends the watch, and so the worker, with the feed.
    watch: Option<ChangeWatch>,
}

impl RecordFeed {
    /// Follow the records under `root` , the data home this device's apps write: the workspaces
    /// `workspaces` names are read, and the rest are left alone until a shortcut watches one of
    /// them.
    /// The first read happens on this thread, before the watch's worker starts, so the heads it
    /// learns are the heads on disk before any change it reports.
    pub fn start(
        root: PathBuf,
        workspaces: BTreeSet<String>,
        wake: crate::automation::Wake,
    ) -> Self {
        let found = Arc::new(Mutex::new(Vec::new()));
        let worker = Arc::new(Mutex::new(Worker {
            workspaces,
            ..Worker::default()
        }));
        {
            let mut worker = worker.lock().unwrap_or_else(PoisonError::into_inner);
            worker.read(&root, &found);
        }
        let (tells, told) = mpsc::channel::<()>();
        let telling = tells;
        let names_root = root.clone();
        let reading_root = root.clone();
        let reading_found = Arc::clone(&found);
        let reading = Arc::clone(&worker);
        let watch = watch_tree_changes(
            &root,
            move |path: &Path| {
                let Some(inside) = path.strip_prefix(&names_root).ok() else {
                    return false;
                };
                let parts: Vec<&std::ffi::OsStr> = inside.iter().collect();
                // A record or a workspace index: the documents this feed reads, and the directory a
                // record of a workspace not read yet appears in.
                parts.get(1) == Some(&std::ffi::OsStr::new(WORKSPACES_DIR))
            },
            move || {
                let _ = telling.send(());
            },
        )
        .ok();
        let worker_thread = watch.as_ref().and_then(|_| {
            std::thread::Builder::new()
                .name("tilo-automation-records".into())
                .spawn(move || {
                    for () in told {
                        let mut worker = reading.lock().unwrap_or_else(PoisonError::into_inner);
                        worker.read(&reading_root, &reading_found);
                        drop(worker);
                        (wake)();
                    }
                })
                .ok()
        });
        Self {
            found,
            reading: Arc::clone(&worker),
            root,
            worker: worker_thread,
            watch,
        }
    }

    /// Read the records of exactly `workspaces` from now on: the workspaces where an armed shortcut
    /// watches record changes. The workspace that starts being watched is read here, before this
    /// returns, so the heads learned as the baseline are the heads the person's shortcut was shown
    /// at, and every change written after it is one the worker reports.
    pub fn follow(&self, workspaces: BTreeSet<String>) {
        let mut worker = self.reading.lock().unwrap_or_else(PoisonError::into_inner);
        if worker.workspaces == workspaces {
            return;
        }
        worker.workspaces = workspaces;
        worker.read(&self.root, &self.found);
    }

    /// The events the worker found since the last call, oldest first.
    pub fn take(&self) -> Vec<RecordEvent> {
        std::mem::take(&mut *self.found.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Drop for RecordFeed {
    fn drop(&mut self) {
        // Ends the watch's thread, and with it the sender it holds: the worker ends on the channel
        // closing.
        self.watch = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Worker {
    /// Read every record of the workspaces this worker watches, turn what changed since the last
    /// read into events, and keep the heads.
    fn read(&mut self, root: &Path, found: &Mutex<Vec<RecordEvent>>) {
        let mut events = Vec::new();
        let held = self.held(root);
        let mut cursors = BTreeMap::new();
        for (package, workspace, record, kind) in held {
            let key = (workspace.clone(), record.clone());
            let path = record_document(root, &package, &workspace, &record);
            let stamp = path.as_deref().and_then(stamp_of);
            let unchanged = match (&stamp, self.seen.get(&key)) {
                (Some(stamp), Some(seen)) => {
                    seen.stamp.as_ref() == Some(stamp) && seen.kind == kind
                }
                _ => false,
            };
            let mut heads = self.cursors.get(&key).cloned();
            if !unchanged
                && let Some(path) = path
                && stamp_of(&path).is_some_and(|(_, len)| len <= MAX_RECORD_BYTES)
                && let Some(doc) = load(&path)
            {
                if let Some(since) = &heads {
                    events.extend(record_events(&workspace, &record, &kind, &doc, since));
                }
                // A record looked at for the first time only has its heads learned: a shortcut
                // saved today never fires on last year's messages.
                heads = Some(doc.committed_heads());
            }
            // A record whose document could not be read this time keeps its heads: the next read of
            // it compares against them again.
            if let Some(heads) = heads.clone() {
                cursors.insert(key.clone(), heads);
            }
            self.seen.insert(key, Seen { kind, stamp });
        }
        // Records a workspace no longer holds are forgotten: a record written again from nothing is
        // not the old one.
        self.seen.retain(|key, _| cursors.contains_key(key));
        self.cursors = cursors;
        if !events.is_empty() {
            found
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend(events);
        }
    }

    /// Every record the watched workspaces hold, by the workspace indexes the apps of this device
    /// write: the app that named it, its workspace, its name, and its kind.
    fn held(&self, data_home: &Path) -> Vec<(String, String, String, String)> {
        let mut held = Vec::new();
        let mut packages = Vec::new();
        collect(&mut packages, data_home);
        let mut indexes = Vec::new();
        for package in packages {
            if let Ok(entries) = std::fs::read_dir(data_home.join(&package).join(WORKSPACES_DIR)) {
                for entry in entries.flatten() {
                    let workspace = entry.file_name().to_string_lossy().into_owned();
                    if self.workspaces.contains(&workspace) {
                        indexes.push((package.clone(), workspace));
                    }
                }
            }
        }
        for (package, workspace) in indexes {
            for (record, kind) in named_in(data_home, &package, &workspace) {
                held.push((package.clone(), workspace.clone(), record, kind));
            }
        }
        held
    }
}

/// The package directories under `root` : one per app that keeps documents here.
fn collect(packages: &mut Vec<String>, root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            packages.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    packages.sort();
}

/// Where record `record` of `package` in `workspace` keeps its document.
fn record_document(
    data_home: &Path,
    package: &str,
    workspace: &str,
    record: &str,
) -> Option<PathBuf> {
    let directory =
        app_state::DocStore::workspace_record_dir_at(data_home, package, workspace, record).ok()?;
    Some(directory.join(DOCUMENT_FILE))
}

/// When `path` was last written and how large it is, or `None` when it is gone.
fn stamp_of(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((
        u64::try_from(since.as_nanos()).unwrap_or(u64::MAX),
        meta.len(),
    ))
}

/// The document `path` holds, as far as it was written: an app appending to it may still be in the
/// middle of a change, and the changes before it are the document as of the last write that
/// completed.
fn load(path: &Path) -> Option<AutoCommit> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    let options = LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    AutoCommit::load_with_options(&bytes, options).ok()
}

/// The records `package` 's index for `workspace` names, by record id and kind.
fn named_in(data_home: &Path, package: &str, workspace: &str) -> Vec<(String, String)> {
    let directory = match app_state::workspace_index_directory(data_home, package, workspace) {
        Ok(directory) => directory,
        Err(error) => {
            tracing::warn!(%error, package, %workspace, "a workspace's index is not read");
            return Vec::new();
        }
    };
    let Some(doc) = load(&directory.join(DOCUMENT_FILE)) else {
        return Vec::new();
    };
    ui::record::index::discoverable_references(&doc)
        .into_iter()
        .map(|record| {
            (
                record.id.as_str().to_owned(),
                record.kind.as_str().to_owned(),
            )
        })
        .collect()
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

    #[test]
    fn a_committed_verb_becomes_a_record_event_with_its_actor_and_facet() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "seed", 1).unwrap();
        let before = doc.get_heads();
        let context = ActionContext::for_app("so.tilo.chat", "Chat")
            .with_invoker(tilo_actions::ActionInvoker::actor("bob"))
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
        assert_eq!(event.actor.id, "bob");
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

    #[test]
    fn a_change_without_an_entry_is_no_event() {
        let mut doc = AutoCommit::new();
        let before = doc.get_heads();
        doc.put(automerge::ROOT, "x", 1).unwrap();
        doc.commit();
        assert!(record_events("r", "rec", "k", &doc, &before).is_empty());
    }
}
