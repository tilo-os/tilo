//! The action log: every workspace commit this device kept, as an ordinary
//! workspace record.
//!
//! A commit is the one record of an action ([`collab_types::WorkspaceCommit`]):
//! who made it, when, what it ran and every unit it wrote. History and undo
//! read actions, so the log keeps the commits themselves beside the index a
//! query needs: which actions wrote a resource, in order. The index is per
//! resource ([`CommitLog::history_of`]), so reading one resource's history
//! reads that resource's index entry and the actions it names, never the
//! workspace's whole log: a workspace with a hundred thousand actions answers
//! as fast as one with ten.
//!
//! The log is a record of its own, sealed per workspace: it lives at
//! `StorageKey::record(LOG_PACKAGE, workspace, LOG_RECORD)` in the device's
//! durable store, written append-only as each action arrives and read back
//! whole when the workspace is first asked about. A restarted device reads
//! the history it had. The document is not the workspace's data: no peer
//! replicates it, and losing it costs the index, never a record.
//!
//! Layout of the document (root map keys):
//!
//! - `m/<commit id>` — the commit's canonical bytes.
//! - `i/<app>/<record>/<resource>/<n>` — `<at>:<commit id>` for the `n`th
//!   action that wrote the resource, and `n/i/<app>/<record>/<resource>`, how
//!   many there are: what [`CommitLog::history_of`] reads, newest first.
//! - `u/<app>/<record>` — the same for a unit, whatever resource it wrote.
//! - `w` — the same for the workspace.
//! - `c/<change hash>` — the id of the action the change is part of: what
//!   undo reads to take back a whole action.

#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ROOT, ReadDoc as _};
use collab_types::{CommitAction, CommitPart, CommitResource, WorkspaceCommit};
use durable_store::{DurableStore, StorageKey, WriteOp};

/// The package the log's record belongs to.
pub const LOG_PACKAGE: &str = "tilo.workspace";
/// The record, within a workspace, that holds its action log.
pub const LOG_RECORD: &str = "actions";

/// One action, as history reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitHistory {
    /// The commit's content address.
    pub commit: [u8; 32],
    /// The workspace whose units the action wrote.
    pub workspace: String,
    /// The principal that invoked it, when one did.
    pub principal: Option<String>,
    /// The device that signed it.
    pub device: [u8; 32],
    /// When it was made, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// What it ran: its name, its action id and type, its item, its
    /// invocation and the run it was invoked in.
    pub action: CommitAction,
    /// Every unit it wrote, with the resources and fields it touched there.
    pub parts: Vec<CommitPart>,
}

impl CommitHistory {
    /// The action as history reads it, from the commit that names it.
    #[must_use]
    pub fn of(commit: &WorkspaceCommit) -> Self {
        Self {
            commit: commit.id(),
            workspace: commit.workspace.clone(),
            principal: commit.author.principal.clone(),
            device: commit.author.device,
            at_ms: commit.at_ms,
            action: commit.action.clone(),
            parts: commit.parts.clone(),
        }
    }

    /// What the action did to `resource` of `app`'s `record`: the fields it
    /// wrote there, with their values before and after when the action
    /// records them.
    #[must_use]
    pub fn resource(&self, app: &str, record: &str, resource: &str) -> Option<&CommitResource> {
        self.parts
            .iter()
            .find(|part| part.is(app, record))
            .and_then(|part| part.resource(resource))
    }
}

/// The actions this device kept, and the index their queries read.
#[derive(Default)]
pub struct CommitLog {
    /// Where the log's record lives; `None` keeps it in memory alone (a
    /// host with no store, and tests).
    store: Option<Arc<dyn DurableStore + Send + Sync>>,
    /// Each workspace's log document, loaded when the workspace is first
    /// asked about and kept current as actions arrive.
    workspaces: BTreeMap<String, WorkspaceLog>,
    /// How many reads of the document the last query made, so a test can
    /// hold a resource's history to its index entry and the actions it names.
    #[cfg(test)]
    reads: Cell<usize>,
}

/// One workspace's log document.
struct WorkspaceLog {
    doc: AutoCommit,
    /// Whether the record already holds bytes of this document: the first
    /// write makes them, later ones only append what changed since.
    stored: bool,
}

const COMMITS: &str = "m";
const BY_CHANGE: &str = "c";
const WORKSPACE: &str = "w";
const BY_RESOURCE: &str = "i";
const BY_UNIT: &str = "u";

impl CommitLog {
    /// An empty log kept in memory alone.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A log kept as a record of `workspace` in `store`: the history a
    /// restart reads.
    #[must_use]
    pub fn stored(store: Arc<dyn DurableStore + Send + Sync>) -> Self {
        Self {
            store: Some(store),
            ..Self::default()
        }
    }

    /// The log's document for `workspace`: the stored one, or an empty one.
    fn open(&mut self, workspace: &str) {
        if self.workspaces.contains_key(workspace) {
            return;
        }
        let stored = self
            .store
            .as_ref()
            .and_then(|store| key(workspace).as_ref().and_then(|key| store.read(key).ok()))
            .flatten()
            .and_then(|stored| match AutoCommit::load(&stored.bytes) {
                Ok(doc) => Some(doc),
                Err(error) => {
                    tracing::warn!(
                        %workspace,
                        %error,
                        "collab: the action log does not read; starting it over"
                    );
                    None
                }
            });
        let log = match stored {
            Some(doc) => WorkspaceLog { doc, stored: true },
            None => WorkspaceLog {
                doc: AutoCommit::new(),
                stored: false,
            },
        };
        self.workspaces.insert(workspace.to_owned(), log);
    }

    /// The document of `workspace`'s log, opened on first use.
    fn doc(&mut self, workspace: &str) -> &mut WorkspaceLog {
        self.open(workspace);
        self.workspaces
            .entry(workspace.to_owned())
            .or_insert_with(|| WorkspaceLog {
                doc: AutoCommit::new(),
                stored: false,
            })
    }

    /// Whether this device kept `commit` already: a peer's retry of an
    /// action the log holds changes nothing.
    pub fn kept(&mut self, commit: &WorkspaceCommit) -> bool {
        let id = hex(&commit.id());
        let log = self.doc(&commit.workspace);
        log.doc
            .get(ROOT, format!("{COMMITS}/{id}"))
            .ok()
            .flatten()
            .is_some()
    }

    /// Keep `commit`, unless this device kept it already.
    pub fn record(&mut self, commit: &WorkspaceCommit) {
        if self.kept(commit) {
            return;
        }
        let id = hex(&commit.id());
        let at = commit.at_ms;
        let changes: Vec<String> = commit
            .parts
            .iter()
            .flat_map(|part| part.changes.iter().map(|change| hex(change)))
            .collect();
        let resources: Vec<String> = commit
            .parts
            .iter()
            .flat_map(|part| {
                part.resources.iter().map(|resource| {
                    format!(
                        "{BY_RESOURCE}/{}/{}/{}",
                        part.app, part.record, resource.resource
                    )
                })
            })
            .collect();
        let units: Vec<String> = commit
            .parts
            .iter()
            .map(|part| format!("{BY_UNIT}/{}/{}", part.app, part.record))
            .collect();
        {
            let log = self.doc(&commit.workspace);
            let doc = &mut log.doc;
            if let Err(error) = doc.put(ROOT, format!("{COMMITS}/{id}"), commit.encode()) {
                tracing::warn!(%error, "collab: an action does not enter the log");
                return;
            }
            let mut indexed: Vec<String> = Vec::new();
            for key in units.iter().chain(resources.iter()) {
                if indexed.contains(key) {
                    continue;
                }
                if let Err(error) = append(doc, key, at, &id) {
                    tracing::warn!(%error, %key, "collab: an action does not enter the index");
                }
                indexed.push(key.clone());
            }
            if let Err(error) = append(doc, WORKSPACE, at, &id) {
                tracing::warn!(%error, "collab: an action does not enter the workspace index");
            }
            for change in &changes {
                if let Err(error) = doc.put(ROOT, format!("{BY_CHANGE}/{change}"), id.as_str()) {
                    tracing::warn!(%error, "collab: a change does not find its action");
                }
            }
        }
        self.persist(&commit.workspace);
    }

    /// Append what changed in `workspace`'s log to its record, so a restart
    /// reads it.
    fn persist(&mut self, workspace: &str) {
        let Some(store) = self.store.as_ref() else {
            return;
        };
        let Some(key) = key(workspace) else {
            return;
        };
        let Some(log) = self.workspaces.get_mut(workspace) else {
            return;
        };
        // The store writes a record only while the caller holds its lease:
        // this device claims the log for this one write, so two devices
        // sharing a store never interleave one log's bytes.
        let lease = match store.lease(&key) {
            Ok(lease) => lease,
            Err(error) => {
                tracing::warn!(%workspace, %error, "collab: the action log is held by another writer");
                return;
            }
        };
        let bytes = log.doc.save_incremental();
        if bytes.is_empty() {
            return;
        }
        let op = if log.stored {
            WriteOp::Append(bytes)
        } else {
            WriteOp::Replace(bytes)
        };
        match store.write(&key, op, None) {
            Ok(_) => log.stored = true,
            Err(error) => {
                tracing::warn!(%workspace, %error, "collab: the action log does not reach the store");
            }
        }
        drop(lease);
    }

    /// Every action that wrote `resource` of `app`'s `record` in
    /// `workspace`, newest first, at most `limit` of them: one index entry
    /// and the actions it names, never the workspace's whole log.
    #[must_use]
    pub fn history_of(
        &mut self,
        workspace: &str,
        app: &str,
        record: &str,
        resource: &str,
        limit: usize,
    ) -> Vec<CommitHistory> {
        let key = format!("{BY_RESOURCE}/{app}/{record}/{resource}");
        self.history_at(workspace, &key, limit)
    }

    /// Every action that wrote the unit of `app`'s `record` in `workspace`,
    /// newest first, at most `limit` of them.
    #[must_use]
    pub fn history_of_unit(
        &mut self,
        workspace: &str,
        app: &str,
        record: &str,
        limit: usize,
    ) -> Vec<CommitHistory> {
        let key = format!("{BY_UNIT}/{app}/{record}");
        self.history_at(workspace, &key, limit)
    }

    /// Every action this device kept in `workspace`, oldest first: one
    /// action with its author, its time and every unit change it made.
    #[must_use]
    pub fn history(&mut self, workspace: &str) -> Vec<CommitHistory> {
        let mut actions = self.history_at(workspace, WORKSPACE, usize::MAX);
        actions.reverse();
        actions
    }

    /// The whole action the change `hash` of `workspace` is part of: every
    /// unit and every change of it, so an undo of one change takes the action
    /// back whole. `None` for a change no action this device kept names.
    #[must_use]
    pub fn action_of(&mut self, workspace: &str, hash: &[u8; 32]) -> Option<CommitHistory> {
        self.count(1);
        let id = {
            let log = self.doc(workspace);
            let (automerge::Value::Scalar(value), _) = log
                .doc
                .get(ROOT, format!("{BY_CHANGE}/{}", hex(hash)))
                .ok()
                .flatten()?
            else {
                return None;
            };
            value.as_str()?.to_owned()
        };
        let doc = &self.workspaces.get(workspace)?.doc;
        self.read(doc, &id)
    }

    /// The last `limit` entries of the index at `key`, newest first: one
    /// read of how many there are, then each of the actions it names. The
    /// entries before them are never read, so the cost follows `limit`, not
    /// the workspace's history.
    fn history_at(&mut self, workspace: &str, key: &str, limit: usize) -> Vec<CommitHistory> {
        if limit == 0 {
            return Vec::new();
        }
        self.open(workspace);
        self.count(1);
        let Some(doc) = self.workspaces.get(workspace).map(|log| &log.doc) else {
            return Vec::new();
        };
        let Some(count) = doc
            .get(ROOT, count_key(key))
            .ok()
            .flatten()
            .and_then(|(value, _)| value.as_u64())
        else {
            return Vec::new();
        };
        let limit = u64::try_from(limit).unwrap_or(u64::MAX);
        let mut out = Vec::new();
        let mut at = count;
        while at > 0 && (count - at) < limit {
            at -= 1;
            self.count(1);
            let entry = doc
                .get(ROOT, entry_key(key, at))
                .ok()
                .flatten()
                .and_then(|(value, _)| value.as_str().map(str::to_owned));
            let Some(id) = entry
                .as_deref()
                .and_then(|entry| entry.split_once(':').map(|(_, id)| id.to_owned()))
            else {
                continue;
            };
            if let Some(action) = self.read(doc, &id) {
                out.push(action);
            }
        }
        out
    }

    /// The action `id` names, read from the log's commits.
    fn read(&self, doc: &AutoCommit, id: &str) -> Option<CommitHistory> {
        self.count(1);
        let (automerge::Value::Scalar(value), _) =
            doc.get(ROOT, format!("{COMMITS}/{id}")).ok().flatten()?
        else {
            return None;
        };
        let commit = WorkspaceCommit::decode(value.as_bytes()?).ok()?;
        Some(CommitHistory::of(&commit))
    }

    #[cfg(test)]
    fn count(&self, reads: usize) {
        self.reads.set(self.reads.get().saturating_add(reads));
    }

    #[cfg(not(test))]
    fn count(&self, _reads: usize) {}

    /// How many reads of the document the last query made.
    #[cfg(test)]
    pub(crate) fn take_reads(&self) -> usize {
        self.reads.take()
    }
}

/// Append the action to the index at `key`: entry number `count`, then the
/// new count. One write each, whatever the index already holds.
fn append(
    doc: &mut AutoCommit,
    key: &str,
    at: u64,
    id: &str,
) -> Result<(), automerge::AutomergeError> {
    let count = doc
        .get(ROOT, count_key(key))?
        .and_then(|(value, _)| value.as_u64())
        .unwrap_or(0);
    doc.put(ROOT, entry_key(key, count), format!("{at}:{id}"))?;
    doc.put(ROOT, count_key(key), count.saturating_add(1))?;
    Ok(())
}

/// The key holding `key`'s entry number `at`.
fn entry_key(key: &str, at: u64) -> String {
    format!("{key}/{at}")
}

/// The key holding how many entries the index at `key` holds.
fn count_key(key: &str) -> String {
    format!("n/{key}")
}

/// The record key of `workspace`'s action log.
fn key(workspace: &str) -> Option<StorageKey> {
    if workspace.is_empty() {
        return None;
    }
    Some(StorageKey::record(LOG_PACKAGE, workspace, LOG_RECORD))
}

/// A hash as its index key spells it.
fn hex(bytes: &[u8; 32]) -> String {
    hex::encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{CommitAction, CommitAuthor, CommitField, CommitGroup};

    const WORKSPACE: &str = "workspace-1";

    fn action(name: &str, id: &str) -> CommitAction {
        CommitAction {
            name: name.to_owned(),
            item: Some("note:2/i1".to_owned()),
            invocation: None,
            id: id.to_owned(),
            ty: "tl.action".to_owned(),
            group: Some(CommitGroup {
                id: "run-1".to_owned(),
                label: "Alex".to_owned(),
                kind: "agent-run".to_owned(),
            }),
        }
    }

    fn author() -> CommitAuthor {
        CommitAuthor {
            principal: Some("ada".to_owned()),
            device: [3; 32],
        }
    }

    /// A commit of one part: `resource` of `app`'s `record`, writing `field`
    /// from `before` to `after`, under the change hash `seed`.
    fn commit(seed: u8, app: &str, record: &str, resource: &str, field: &str) -> WorkspaceCommit {
        WorkspaceCommit::new(
            WORKSPACE,
            author(),
            1_700_000_000_000 + u64::from(seed),
            action("Complete note", "tasks/note/complete"),
            vec![CommitPart {
                app: app.to_owned(),
                record: record.to_owned(),
                changes: vec![[seed; 32]],
                resources: vec![CommitResource {
                    resource: resource.to_owned(),
                    item: Some("note:2/i1".to_owned()),
                    fields: vec![CommitField {
                        field: field.to_owned(),
                        before: Some("false".to_owned()),
                        after: Some("true".to_owned()),
                    }],
                }],
            }],
        )
    }

    #[test]
    fn an_action_that_writes_two_apps_shows_once_in_each_resource_s_history() {
        let mut log = CommitLog::new();
        let one = commit(1, "tasks", "board", "note:1", "done");
        log.record(&one);
        let mut two = commit(2, "chat", "inbox", "note:2", "read");
        // The same action wrote both: one commit, two units.
        two.parts[0].changes = vec![[2; 32]];
        log.record(&two);

        let tasks = log.history_of(WORKSPACE, "tasks", "board", "note:1", 10);
        assert_eq!(tasks.len(), 1, "the action shows in the resource it wrote");
        let entry = &tasks[0];
        assert_eq!(entry.commit, one.id());
        assert_eq!(entry.principal.as_deref(), Some("ada"));
        assert_eq!(entry.device, [3; 32]);
        assert_eq!(entry.at_ms, 1_700_000_000_001);
        assert_eq!(entry.action.name, "Complete note");
        assert_eq!(entry.action.id, "tasks/note/complete");
        assert_eq!(
            entry.action.group.as_ref().map(|group| group.id.as_str()),
            Some("run-1")
        );
        let touched = entry.resource("tasks", "board", "note:1").unwrap();
        assert_eq!(touched.fields[0].field, "done");
        assert_eq!(touched.fields[0].before.as_deref(), Some("false"));
        assert_eq!(touched.fields[0].after.as_deref(), Some("true"));

        let chat = log.history_of(WORKSPACE, "chat", "inbox", "note:2", 10);
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].commit, two.id());
        assert_eq!(
            log.history_of(WORKSPACE, "tasks", "board", "note:2", 10)
                .len(),
            0,
            "a resource holds only the actions that wrote it"
        );
        assert_eq!(log.history(WORKSPACE).len(), 2);
    }

    #[test]
    fn an_undo_of_one_change_reads_back_every_unit_the_action_wrote() {
        let mut log = CommitLog::new();
        let mut both = commit(4, "tasks", "board", "note:1", "done");
        both.parts.push(CommitPart {
            app: "chat".to_owned(),
            record: "inbox".to_owned(),
            changes: vec![[5; 32], [6; 32]],
            resources: vec![CommitResource {
                resource: "note:2".to_owned(),
                item: None,
                fields: vec![],
            }],
        });
        log.record(&both);

        for change in [[4; 32], [5; 32], [6; 32]] {
            let action = log.action_of(WORKSPACE, &change).unwrap();
            assert_eq!(action.commit, both.id());
            assert_eq!(action.parts.len(), 2, "both units come back with it");
            assert_eq!(action.parts[1].changes.len(), 2);
        }
        assert!(
            log.action_of(WORKSPACE, &[9; 32]).is_none(),
            "an unknown change names none"
        );
    }

    #[test]
    fn a_restarted_device_reads_the_history_it_had() {
        let profile = tempfile::tempdir().unwrap();
        let store: Arc<dyn DurableStore + Send + Sync> =
            Arc::new(app_state::FsStore::new(profile.path()));
        let key = key(WORKSPACE).unwrap();
        let one = commit(7, "tasks", "board", "note:1", "done");
        {
            let mut log = CommitLog::stored(Arc::clone(&store));
            log.record(&one);
            store.flush(Some(&key)).unwrap();
        }
        let mut restarted = CommitLog::stored(Arc::clone(&store));
        let history = restarted.history_of(WORKSPACE, "tasks", "board", "note:1", 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].commit, one.id());
        assert_eq!(history[0].action.name, "Complete note");
        assert_eq!(
            restarted
                .action_of(WORKSPACE, &[7; 32])
                .map(|action| action.commit),
            Some(one.id()),
            "undo finds the action after the restart too"
        );
        // A second action appends to the same record; the first stays.
        let mut log = CommitLog::stored(Arc::clone(&store));
        log.record(&commit(8, "tasks", "board", "note:1", "done"));
        store.flush(Some(&key)).unwrap();
        let mut restarted = CommitLog::stored(Arc::clone(&store));
        let history = restarted.history_of(WORKSPACE, "tasks", "board", "note:1", 10);
        assert_eq!(history.len(), 2);
        assert_eq!(
            history[0].commit,
            commit(8, "tasks", "board", "note:1", "done").id()
        );
    }

    /// The index holds `count` actions on one resource without making
    /// `count` documents: the read count is what this test is about.
    #[cfg(test)]
    fn fill(doc: &mut AutoCommit, resource: &str, count: usize) -> String {
        let id = hex(&[1u8; 32]);
        doc.put(
            ROOT,
            format!("{COMMITS}/{id}"),
            commit(1, "tasks", "board", "note:1", "done").encode(),
        )
        .unwrap();
        let key = format!("{BY_RESOURCE}/tasks/board/{resource}");
        for at in 0..count {
            doc.put(ROOT, entry_key(&key, at as u64), format!("{at}:{id}"))
                .unwrap();
        }
        doc.put(ROOT, count_key(&key), count as u64).unwrap();
        id
    }

    #[test]
    fn a_hundred_thousand_actions_read_in_a_bounded_number_of_index_reads() {
        let mut log = CommitLog::new();
        let log_doc = log.doc(WORKSPACE);
        fill(&mut log_doc.doc, "note:1", 100_000);
        let _ = log.take_reads();

        let history = log.history_of(WORKSPACE, "tasks", "board", "note:1", 20);
        assert_eq!(history.len(), 20, "the newest actions, one resource's own");
        // One index entry, then each of the twenty actions it names: the
        // hundred thousand other actions are never read.
        let reads = log.take_reads();
        assert_eq!(
            reads, 41,
            "one index entry and two reads per action it names"
        );

        let small = {
            let mut log = CommitLog::new();
            fill(&mut log.doc(WORKSPACE).doc, "note:1", 10);
            let _ = log.take_reads();
            assert_eq!(
                log.history_of(WORKSPACE, "tasks", "board", "note:1", 20)
                    .len(),
                10
            );
            log.take_reads()
        };
        assert_eq!(small, 21, "ten actions cost what ten actions cost");
        assert_eq!(
            log.history_of(WORKSPACE, "tasks", "board", "another", 20)
                .len(),
            0,
            "a resource nobody wrote reads nothing"
        );
    }
}
