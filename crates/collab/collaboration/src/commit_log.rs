//! The action log: the index this endpoint builds as changes and commits
//! arrive, answering which actions wrote a resource and which action a change
//! belongs to.
//!
//! A commit is the one record of an action ([`collab_types::WorkspaceCommit`]);
//! the changes it names carry what the action did. The log is neither: it is
//! the index over both, derived from them, so a peer reconstructs it from the
//! changes and commits it holds and losing it costs a rebuild, never a record.
//!
//! Two queries read it. [`CommitLog::history_of`] answers a resource's
//! history from that resource's own entry and the actions it names, never the
//! workspace's whole log, so a workspace with a hundred thousand actions
//! answers as fast as one with ten. [`CommitLog::action_of`] answers with the
//! whole action a change belongs to — every unit it wrote — which is what undo
//! takes back.
//!
//! A change tells the index what the commit cannot: its own history entry
//! names the resource it wrote and when. [`ChangeFact`] is that, read from the
//! change.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use collab_types::{CommitPart, DocumentMutation, WorkspaceCommit, claim};

/// One change of an action, as the log indexes it: the unit it belongs to, the
/// resource its own history entry names, and when it was made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeFact {
    /// The change's hash.
    pub change: [u8; 32],
    /// The app whose record the unit belongs to.
    pub app: String,
    /// The record the unit belongs to.
    pub record: String,
    /// The resource the change's history entry names, when it names one.
    pub resource: Option<String>,
    /// When the change was made, in milliseconds since the Unix epoch.
    pub at_ms: u64,
}

impl ChangeFact {
    /// The facts of the changes `mutation` carries in `app`'s `record`. Empty
    /// for a mutation that carries no change (a snapshot) and for a change
    /// that names no action: it says nothing of where it belongs.
    #[must_use]
    pub fn of_mutation(app: &str, record: &str, mutation: &DocumentMutation) -> Vec<Self> {
        let DocumentMutation::Incremental(bytes) = mutation else {
            return Vec::new();
        };
        match claim::parse_changes(bytes) {
            Ok(changes) => Self::of_changes(app, record, &changes),
            Err(error) => {
                tracing::warn!(%error, app, record, "collab: an action's changes do not parse");
                Vec::new()
            }
        }
    }

    /// The facts of the changes one signed document `payload` carries in
    /// `app`'s `record`: the document frame inside the commit frame it may
    /// arrive under.
    #[must_use]
    pub fn of_payload(app: &str, record: &str, payload: &[u8]) -> Vec<Self> {
        let Ok((inner, _)) = WorkspaceCommit::split(payload) else {
            return Vec::new();
        };
        match DocumentMutation::decode(inner) {
            Ok(mutation) => Self::of_mutation(app, record, &mutation),
            Err(error) => {
                tracing::warn!(%error, app, record, "collab: a payload's changes do not decode");
                Vec::new()
            }
        }
    }

    /// The facts of `changes` of `app`'s `record`: a document's own history,
    /// change by change.
    #[must_use]
    pub fn of_changes(app: &str, record: &str, changes: &[automerge::Change]) -> Vec<Self> {
        changes
            .iter()
            .filter_map(|change| {
                let context = change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)?;
                Some(Self {
                    change: change.hash().0,
                    app: app.to_owned(),
                    record: record.to_owned(),
                    resource: context
                        .resource
                        .as_ref()
                        .map(|resource| resource.id.clone()),
                    at_ms: u64::try_from(change.timestamp())
                        .unwrap_or(0)
                        .saturating_mul(1000),
                })
            })
            .collect()
    }
}

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
    /// The invocation it ran under, when an admitted door ran it.
    pub invocation: Option<uuid::Uuid>,
    /// The run it was invoked in, when it was invoked in one.
    pub run: Option<collab_types::CommitGroup>,
    /// Every unit it wrote, and the changes it made there.
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
            invocation: commit.invocation,
            run: commit.run.clone(),
            parts: commit.parts.clone(),
        }
    }
}

/// The actions this endpoint kept, and the index their queries read.
#[derive(Default)]
pub struct CommitLog {
    /// Each workspace's index, built as its changes and commits arrive.
    workspaces: BTreeMap<String, WorkspaceIndex>,
    /// How many reads of the index the last query made, so a test can hold a
    /// resource's history to its own entry and the actions it names.
    #[cfg(test)]
    reads: Cell<usize>,
}

/// What one workspace's index holds.
#[derive(Default)]
struct WorkspaceIndex {
    /// Every action this endpoint kept, by the commit's content address.
    commits: BTreeMap<[u8; 32], CommitHistory>,
    /// The changes of each resource of each unit, by the action that made
    /// them: what that resource's history reads, newest last.
    resources: BTreeMap<(String, String, String), BTreeSet<(u64, [u8; 32], [u8; 32])>>,
    /// The action each change is part of.
    by_change: BTreeMap<[u8; 32], [u8; 32]>,
}

impl CommitLog {
    /// An empty log: it is built as changes and commits arrive.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this endpoint kept `commit` already: a peer's retry of an
    /// action the log holds changes nothing.
    #[must_use]
    pub fn kept(&self, commit: &WorkspaceCommit) -> bool {
        self.workspaces
            .get(&commit.workspace)
            .is_some_and(|index| index.commits.contains_key(&commit.id()))
    }

    /// Keep `commit` and index the `facts` its changes tell: which resource
    /// each wrote, which action it belongs to. A fact the commit does not name
    /// is ignored, so a caller may hand every change it has.
    pub fn record(&mut self, commit: &WorkspaceCommit, facts: &[ChangeFact]) {
        let id = commit.id();
        let index = self.workspaces.entry(commit.workspace.clone()).or_default();
        if index.commits.contains_key(&id) {
            return;
        }
        index.commits.insert(id, CommitHistory::of(commit));
        for fact in facts {
            if commit.change(&fact.change).is_none() {
                continue;
            }
            index.by_change.insert(fact.change, id);
            let Some(resource) = &fact.resource else {
                continue;
            };
            index
                .resources
                .entry((fact.app.clone(), fact.record.clone(), resource.clone()))
                .or_default()
                .insert((commit.at_ms, id, fact.change));
        }
    }

    /// Every action that wrote `resource` of `app`'s `record` in `workspace`,
    /// newest first, at most `limit` of them: that resource's own index entry
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
        if limit == 0 {
            return Vec::new();
        }
        self.count(1);
        let Some(index) = self.workspaces.get(workspace) else {
            return Vec::new();
        };
        let Some(entries) =
            index
                .resources
                .get(&(app.to_owned(), record.to_owned(), resource.to_owned()))
        else {
            return Vec::new();
        };
        let mut read: BTreeSet<[u8; 32]> = BTreeSet::new();
        let mut out = Vec::new();
        for (_, id, _) in entries.iter().rev() {
            if out.len() >= limit {
                break;
            }
            if !read.insert(*id) {
                continue;
            }
            self.count(1);
            if let Some(action) = index.commits.get(id) {
                out.push(action.clone());
            }
        }
        out
    }

    /// Every action this endpoint kept in `workspace`, oldest first.
    #[must_use]
    pub fn history(&self, workspace: &str) -> Vec<CommitHistory> {
        let Some(index) = self.workspaces.get(workspace) else {
            return Vec::new();
        };
        let mut actions: Vec<&CommitHistory> = index.commits.values().collect();
        actions.sort_by_key(|action| (action.at_ms, action.commit));
        actions.into_iter().cloned().collect()
    }

    /// The whole action the change `hash` of `workspace` is part of: every unit
    /// and every change of it, so an undo of one change takes its action back
    /// whole. `None` for a change no action this endpoint kept names.
    #[must_use]
    pub fn action_of(&mut self, workspace: &str, hash: &[u8; 32]) -> Option<CommitHistory> {
        self.count(1);
        let index = self.workspaces.get(workspace)?;
        let id = index.by_change.get(hash)?;
        index.commits.get(id).cloned()
    }

    #[cfg(test)]
    fn count(&self, reads: usize) {
        self.reads.set(self.reads.get().saturating_add(reads));
    }

    #[cfg(not(test))]
    fn count(&self, _reads: usize) {}

    /// How many reads of the index the last query made.
    #[cfg(test)]
    pub(crate) fn take_reads(&self) -> usize {
        self.reads.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::transaction::Transactable as _;
    use automerge::{AutoCommit, ChangeHash};
    use collab_types::{CommitAuthor, CommitGroup, UnitMutation};
    use tilo_actions::{ActionContext, Did, EditOp, FieldEdit, InvocationId, PathStep};

    const WORKSPACE: &str = "workspace-1";
    const DEVICE: [u8; 32] = [3; 32];

    fn author() -> CommitAuthor {
        CommitAuthor {
            principal: Some("ada".to_owned()),
            device: DEVICE,
        }
    }

    /// Write one change of `field` = `value` into `doc`, as the action
    /// `field` of `resource` wrote it at `at` seconds, invoked under
    /// `invocation`.
    fn action_change(
        doc: &mut AutoCommit,
        resource: &str,
        field: &str,
        value: &str,
        invocation: InvocationId,
        at: u64,
    ) -> [u8; 32] {
        doc.put(automerge::ROOT, field, value).unwrap();
        let context = ActionContext {
            what: Some(Did::Edit(FieldEdit {
                ty: "tasks".to_owned(),
                field: field.to_owned(),
                path: vec![PathStep::Field(field.to_owned())],
                op: EditOp::Set,
                old: None,
                new: None,
                args: std::collections::BTreeMap::new(),
            })),
            invoker: Some(tilo_actions::ActionInvoker::actor("ada")),
            source_invocation: Some(invocation),
            ..ActionContext::for_resource(resource, "tasks", field)
        };
        let mut options = automerge::transaction::CommitOptions::default()
            .with_time(i64::try_from(at).unwrap_or(i64::MAX));
        options.set_message(context.change_message().unwrap());
        doc.commit_with(options).unwrap().0
    }

    /// One unit as one action left it: which app's record, and what the action
    /// wrote there.
    struct Step<'a> {
        app: &'a str,
        record: &'a str,
        mutation: DocumentMutation,
    }

    /// The unit of `app`'s `record` the action wrote into `doc` since
    /// `before`.
    fn step<'a>(
        app: &'a str,
        record: &'a str,
        doc: &mut AutoCommit,
        before: Vec<ChangeHash>,
    ) -> Step<'a> {
        Step {
            app,
            record,
            mutation: DocumentMutation::Incremental(doc.save_after(&before)),
        }
    }

    /// The commit naming `steps`, as the publisher builds it.
    fn commit_of(steps: &[Step<'_>]) -> WorkspaceCommit {
        let named: Vec<UnitMutation<'_>> = steps
            .iter()
            .map(|step| UnitMutation {
                app: step.app,
                record: step.record,
                mutation: &step.mutation,
                document: None,
            })
            .collect();
        let mut commits = WorkspaceCommit::of_units(WORKSPACE, &author(), &named).unwrap();
        let commit = commits
            .remove(0)
            .expect("a unit that changed travels under its action's commit");
        for other in commits.into_iter().flatten() {
            assert_eq!(other.id(), commit.id(), "one action, one commit");
        }
        commit
    }

    /// The facts of `steps`' changes, as the publisher has them at hand.
    fn facts_of(steps: &[Step<'_>]) -> Vec<ChangeFact> {
        steps
            .iter()
            .flat_map(|step| ChangeFact::of_mutation(step.app, step.record, &step.mutation))
            .collect()
    }

    #[test]
    fn an_action_that_writes_two_units_shows_once_in_each_resource_s_history() {
        let invocation = InvocationId::new();
        let mut tasks = AutoCommit::new();
        action_change(
            &mut tasks,
            "note:1",
            "done",
            "true",
            invocation,
            1_700_000_000,
        );
        let mut chat = AutoCommit::new();
        action_change(
            &mut chat,
            "note:2",
            "read",
            "true",
            invocation,
            1_700_000_000,
        );
        let steps = [
            step("tasks", "board", &mut tasks, Vec::new()),
            step("chat", "inbox", &mut chat, Vec::new()),
        ];
        let commit = commit_of(&steps);

        let mut log = CommitLog::new();
        log.record(&commit, &facts_of(&steps));

        let of_tasks = log.history_of(WORKSPACE, "tasks", "board", "note:1", 10);
        assert_eq!(
            of_tasks.len(),
            1,
            "the action shows in the resource it wrote"
        );
        let entry = &of_tasks[0];
        assert_eq!(entry.commit, commit.id());
        assert_eq!(entry.principal.as_deref(), Some("ada"));
        assert_eq!(entry.device, DEVICE);
        assert_eq!(entry.at_ms, commit.at_ms);
        assert_eq!(entry.invocation, Some(invocation.0));
        assert_eq!(entry.parts, commit.parts, "both units come with it");

        let of_chat = log.history_of(WORKSPACE, "chat", "inbox", "note:2", 10);
        assert_eq!(of_chat.len(), 1);
        assert_eq!(of_chat[0].commit, commit.id());
        assert_eq!(
            log.history_of(WORKSPACE, "tasks", "board", "note:2", 10)
                .len(),
            0,
            "a resource holds only the actions that wrote it"
        );
        assert_eq!(
            log.history_of(WORKSPACE, "tasks", "elsewhere", "note:1", 10)
                .len(),
            0,
            "a unit holds only its own changes"
        );
        assert_eq!(log.history(WORKSPACE).len(), 1, "one action, one entry");
    }

    #[test]
    fn a_resource_s_history_reads_newest_first_at_most_limit() {
        let mut doc = AutoCommit::new();
        let mut facts = Vec::new();
        let mut commits = Vec::new();
        for action_index in 0..5u64 {
            let invocation = InvocationId::new();
            let before = doc.get_heads();
            action_change(
                &mut doc,
                "note:1",
                &format!("field-{action_index}"),
                "value",
                invocation,
                1_700_000_000 + action_index,
            );
            let unit = step("tasks", "board", &mut doc, before);
            commits.push(commit_of(std::slice::from_ref(&unit)));
            facts.push(facts_of(std::slice::from_ref(&unit)));
        }
        let mut log = CommitLog::new();
        for (commit, facts) in commits.iter().zip(&facts) {
            log.record(commit, facts);
        }

        let all = log.history_of(WORKSPACE, "tasks", "board", "note:1", 10);
        assert_eq!(all.len(), 5, "every action that wrote it");
        let newest = commits.last().map(WorkspaceCommit::id);
        assert_eq!(
            all.first().map(|entry| entry.commit),
            newest,
            "newest first"
        );
        assert_eq!(
            log.history_of(WORKSPACE, "tasks", "board", "note:1", 2)
                .len(),
            2
        );
        assert_eq!(
            log.history(WORKSPACE).last().map(|entry| entry.commit),
            newest,
            "the workspace's history reads oldest first"
        );
    }

    #[test]
    fn an_undo_of_one_change_reads_back_every_unit_the_action_wrote() {
        let invocation = InvocationId::new();
        let mut tasks = AutoCommit::new();
        let first = action_change(
            &mut tasks,
            "note:1",
            "done",
            "true",
            invocation,
            1_700_000_000,
        );
        let mut chat = AutoCommit::new();
        let one = action_change(
            &mut chat,
            "note:2",
            "read",
            "true",
            invocation,
            1_700_000_000,
        );
        let other = action_change(
            &mut chat,
            "note:3",
            "seen",
            "true",
            invocation,
            1_700_000_000,
        );
        let steps = [
            step("tasks", "board", &mut tasks, Vec::new()),
            step("chat", "inbox", &mut chat, Vec::new()),
        ];
        let commit = commit_of(&steps);
        let mut log = CommitLog::new();
        log.record(&commit, &facts_of(&steps));

        for change in [first, one, other] {
            let action = log.action_of(WORKSPACE, &change).unwrap();
            assert_eq!(action.commit, commit.id());
            assert_eq!(action.parts.len(), 2, "both units come back with it");
            assert_eq!(action.parts[1].changes.len(), 2);
        }
        assert!(
            log.action_of(WORKSPACE, &[9; 32]).is_none(),
            "an unknown change names none"
        );

        // A fact the commit does not name is not indexed under it.
        let stray = ChangeFact {
            change: [8; 32],
            app: "tasks".to_owned(),
            record: "board".to_owned(),
            resource: Some("note:1".to_owned()),
            at_ms: 1,
        };
        log.record(&commit_of(&steps), &[stray]);
        assert!(log.action_of(WORKSPACE, &[8; 32]).is_none());
    }

    /// The log a peer constructs from what it holds: every unit's own document
    /// and the commits it received, nothing else.
    fn rebuilt(
        documents: &mut [(&str, &str, &mut AutoCommit)],
        commits: &[WorkspaceCommit],
    ) -> CommitLog {
        let mut log = CommitLog::new();
        for commit in commits {
            let facts: Vec<ChangeFact> = documents
                .iter_mut()
                .filter(|(app, record, _)| commit.covers(app, record))
                .flat_map(|(app, record, doc)| {
                    ChangeFact::of_changes(app, record, &doc.get_changes(&[]))
                })
                .collect();
            log.record(commit, &facts);
        }
        log
    }

    #[test]
    fn an_index_rebuilt_from_the_documents_reads_the_same_as_the_one_that_arrived() {
        let first = InvocationId::new();
        let mut tasks = AutoCommit::new();
        let done = action_change(&mut tasks, "note:1", "done", "true", first, 1_700_000_000);
        let mut chat = AutoCommit::new();
        let read = action_change(&mut chat, "note:2", "read", "true", first, 1_700_000_000);
        let first_steps = [
            step("tasks", "board", &mut tasks, Vec::new()),
            step("chat", "inbox", &mut chat, Vec::new()),
        ];
        let first_commit = commit_of(&first_steps);
        let first_facts = facts_of(&first_steps);

        let second = InvocationId::new();
        let before = tasks.get_heads();
        action_change(
            &mut tasks,
            "note:1",
            "title",
            "Write",
            second,
            1_700_000_001,
        );
        let later_steps = [step("tasks", "board", &mut tasks, before)];
        let later_commit = commit_of(&later_steps);

        let mut arrived = CommitLog::new();
        arrived.record(&first_commit, &first_facts);
        arrived.record(&later_commit, &facts_of(&later_steps));
        let documents = [("tasks", "board", &mut tasks), ("chat", "inbox", &mut chat)];
        let mut from_documents = {
            let mut documents = documents;
            rebuilt(
                &mut documents,
                &[first_commit.clone(), later_commit.clone()],
            )
        };

        assert_eq!(
            from_documents.history(WORKSPACE),
            arrived.history(WORKSPACE),
            "the workspace's history reads the same"
        );
        for resource in ["note:1", "note:2", "note:3"] {
            assert_eq!(
                from_documents.history_of(WORKSPACE, "tasks", "board", resource, 10),
                arrived.history_of(WORKSPACE, "tasks", "board", resource, 10),
                "task resource {resource}"
            );
            assert_eq!(
                from_documents.history_of(WORKSPACE, "chat", "inbox", resource, 10),
                arrived.history_of(WORKSPACE, "chat", "inbox", resource, 10),
                "chat resource {resource}"
            );
        }
        for change in [done, read] {
            assert_eq!(
                from_documents.action_of(WORKSPACE, &change),
                arrived.action_of(WORKSPACE, &change),
                "undo finds the same action"
            );
            assert_eq!(
                from_documents
                    .action_of(WORKSPACE, &change)
                    .map(|action| action.commit),
                Some(first_commit.id())
            );
        }
    }

    #[test]
    fn a_change_that_names_no_action_is_indexed_nowhere() {
        let mut plain = AutoCommit::new();
        plain.put(automerge::ROOT, "title", "plain").unwrap();
        plain.commit();
        let unnamed = plain
            .get_changes(&[])
            .first()
            .map(|change| change.hash().0)
            .unwrap();

        let invocation = InvocationId::new();
        let mut tasks = AutoCommit::new();
        let done = action_change(
            &mut tasks,
            "note:1",
            "done",
            "true",
            invocation,
            1_700_000_000,
        );
        let steps = [step("tasks", "board", &mut tasks, Vec::new())];
        let mut commit = commit_of(&steps);
        // The commit names the change nothing but its device accounts for.
        commit.parts[0].changes.push(unnamed);

        let facts = ChangeFact::of_changes("tasks", "board", &tasks.get_changes(&[]));
        assert_eq!(facts.len(), 1, "a change that names no action has none");
        let mut log = CommitLog::new();
        log.record(&commit, &facts);
        assert_eq!(
            log.action_of(WORKSPACE, &done).map(|a| a.commit),
            Some(commit.id())
        );
        assert!(
            log.action_of(WORKSPACE, &unnamed).is_none(),
            "a change that names no action names neither its action nor its resource"
        );
        assert!(
            log.history_of(WORKSPACE, "tasks", "board", "note:1", 10)
                .len()
                == 1
        );
        assert!(
            log.history_of(WORKSPACE, "tasks", "board", "", 10)
                .is_empty()
        );
        assert_eq!(log.history(WORKSPACE).len(), 1, "the action is still kept");
    }

    #[test]
    fn a_hundred_thousand_actions_read_in_a_bounded_number_of_index_reads() {
        fn action(seed: u8, at: u64) -> CommitHistory {
            CommitHistory {
                commit: [seed; 32],
                workspace: WORKSPACE.to_owned(),
                principal: Some("ada".to_owned()),
                device: DEVICE,
                at_ms: at,
                invocation: None,
                run: None,
                parts: Vec::new(),
            }
        }

        let mut log = CommitLog::new();
        {
            let index = log.workspaces.entry(WORKSPACE.to_owned()).or_default();
            let entries = index
                .resources
                .entry(("tasks".to_owned(), "board".to_owned(), "note:1".to_owned()))
                .or_default();
            for at in 0..100_000u64 {
                let seed = u8::try_from(at % 251).unwrap_or(0);
                entries.insert((at, [seed; 32], [seed; 32]));
                index.commits.insert([seed; 32], action(seed, at));
            }
        }
        let _ = log.take_reads();

        let history = log.history_of(WORKSPACE, "tasks", "board", "note:1", 20);
        assert_eq!(history.len(), 20, "the newest actions, one resource's own");
        // One index entry, then each action it names: the hundred thousand
        // other entries are never read.
        assert_eq!(
            log.take_reads(),
            21,
            "one index entry and one read per action it names"
        );

        let mut small = CommitLog::new();
        {
            let index = small.workspaces.entry(WORKSPACE.to_owned()).or_default();
            let entries = index
                .resources
                .entry(("tasks".to_owned(), "board".to_owned(), "note:1".to_owned()))
                .or_default();
            for at in 0..10u64 {
                entries.insert((at, [1; 32], [u8::try_from(at).unwrap_or(0); 32]));
            }
            index.commits.insert([1; 32], action(1, 0));
        }
        let _ = small.take_reads();
        assert_eq!(
            small
                .history_of(WORKSPACE, "tasks", "board", "note:1", 20)
                .len(),
            1,
            "ten changes of one action are one action"
        );
        assert_eq!(small.take_reads(), 2, "one index entry and one action");
        assert_eq!(
            small
                .history_of(WORKSPACE, "tasks", "board", "another", 20)
                .len(),
            0,
            "a resource nobody wrote reads nothing"
        );
    }

    #[test]
    fn the_run_an_action_was_invoked_in_travels_with_it() {
        let mut log = CommitLog::new();
        let commit = WorkspaceCommit::new(
            WORKSPACE,
            author(),
            1_700_000_000_000,
            Some(uuid::Uuid::from_u128(4)),
            Some(CommitGroup {
                id: "run-1".to_owned(),
                label: "Alex".to_owned(),
                kind: "agent-run".to_owned(),
            }),
            vec![CommitPart {
                app: "tasks".to_owned(),
                record: "board".to_owned(),
                changes: vec![[4; 32]],
            }],
        );
        log.record(&commit, &[]);
        let history = log.history(WORKSPACE);
        assert_eq!(history.len(), 1);
        assert_eq!(
            history[0].invocation,
            Some(uuid::Uuid::from_u128(4)),
            "the invocation it ran under"
        );
        assert_eq!(
            history[0].run.as_ref().map(|run| run.id.as_str()),
            Some("run-1")
        );
    }
}
