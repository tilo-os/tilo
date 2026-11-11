//! The action log: the index this endpoint builds as changes and commits
//! arrive, answering which actions wrote a resource and which action a change
//! belongs to.
//!
//! A commit is the one record of an action ([`collab_types::WorkspaceCommit`]);
//! the changes it names carry what the action did. The log is neither: it is
//! the index over both, derived from them, so a peer reconstructs it from the
//! changes and commits it holds and losing it costs a rebuild, never a record.
//!
//! [`CommitLog::history`] answers every action of a workspace, which is what
//! history reads.

use std::collections::BTreeMap;

use collab_types::{CommitPart, WorkspaceCommit};

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
    /// Each workspace's actions, kept as its commits arrive.
    workspaces: BTreeMap<String, WorkspaceIndex>,
}

/// What one workspace's log holds.
#[derive(Default)]
struct WorkspaceIndex {
    /// Every action this endpoint kept, by the commit's content address.
    commits: BTreeMap<[u8; 32], CommitHistory>,
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

    /// Keep `commit`, unless this endpoint kept it already.
    pub fn record(&mut self, commit: &WorkspaceCommit) {
        let id = commit.id();
        let index = self.workspaces.entry(commit.workspace.clone()).or_default();
        if index.commits.contains_key(&id) {
            return;
        }
        index.commits.insert(id, CommitHistory::of(commit));
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use automerge::transaction::Transactable as _;
    use collab_types::{CommitAuthor, UnitMutation};
    use tilo_actions::{ActionContext, Did, EditOp, FieldEdit, PathStep};

    const WORKSPACE: &str = "workspace-1";
    const DEVICE: [u8; 32] = [3; 32];

    fn author() -> CommitAuthor {
        CommitAuthor {
            principal: Some("ada".to_owned()),
            device: DEVICE,
        }
    }

    /// An action writing `field` = `value` of `tasks`' `board`.
    fn commit_of(field: &str, value: &str) -> WorkspaceCommit {
        let mut doc = AutoCommit::new();
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
            ..ActionContext::for_resource("note:1", "tasks", field)
        };
        let mut options = automerge::transaction::CommitOptions::default();
        options.set_message(context.change_message().unwrap());
        doc.commit_with(options).unwrap();
        let mutation = collab_types::DocumentMutation::Incremental(doc.save());
        let units = [UnitMutation {
            app: "tasks",
            record: "board",
            mutation: &mutation,
            document: None,
        }];
        let mut commits = WorkspaceCommit::of_units(WORKSPACE, &author(), &units).unwrap();
        commits
            .remove(0)
            .expect("a unit that changed travels under its action's commit")
    }

    #[test]
    fn an_action_is_kept_once_and_reads_with_its_author() {
        let commit = commit_of("done", "true");
        let mut log = CommitLog::new();
        assert!(!log.kept(&commit));
        log.record(&commit);
        log.record(&commit);
        assert!(log.kept(&commit));
        let history = log.history(WORKSPACE);
        assert_eq!(history.len(), 1, "one action, one entry");
        assert_eq!(history[0].principal.as_deref(), Some("ada"));
        assert_eq!(history[0].device, DEVICE);
        assert!(log.history("elsewhere").is_empty());
    }

    #[test]
    fn the_actions_of_a_workspace_read_oldest_first() {
        let mut log = CommitLog::new();
        let mut first = commit_of("done", "true");
        first.at_ms = 1_700_000_000;
        let mut second = commit_of("done", "false");
        second.at_ms = 1_700_000_001;
        log.record(&second);
        log.record(&first);
        let history = log.history(WORKSPACE);
        assert_eq!(
            history
                .iter()
                .map(|action| action.at_ms)
                .collect::<Vec<_>>(),
            [1_700_000_000, 1_700_000_001]
        );
    }
}
