//! The session's one open of the store its apps' records are kept in.
//!
//! Every part of the session that reads or writes a workspace's records — the conversations an
//! agent answers in, the agents each workspace keeps, the workspace settings' tabs, and every
//! other part that opens a record through [`WorkspaceRecords`] — opens it through this one
//! [`RecordOwners`]. Two `RecordOwners` over one store are two record sets: each follows its own
//! document, so a record one part writes stays as it was in the other's reading until something
//! tells that reading to refresh — what the reader draws says what the record held before the
//! write. That is the byline that says Chat's own word for an agent ("Agent") where the
//! workspace's name ("Assistant") was written a moment before, because the page's record set
//! never saw the write. It is not a race a test can lean on, so the class is refused here rather
//! than left to timing: `tools/policy/check_record_owners.py` refuses a second
//! `RecordOwners::new` in the session's parts, and the tests below hold that one set of owners,
//! opened twice, is one reading of the record — and two are two.

use std::path::PathBuf;

use ui::record::RecordOwners;

/// The record store the session's apps keep, opened once for the whole session and shared by
/// every part of it.
#[derive(Clone)]
pub(crate) struct SessionOwners {
    owners: RecordOwners,
}

impl SessionOwners {
    /// The session's one open of the store under `root`.
    #[must_use]
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            owners: RecordOwners::new(root),
        }
    }

    /// The session's owners over `owners` one: what a unit test hands a part so it opens the
    /// store the test keeps, exactly as the session's own pass would.
    #[cfg(test)]
    pub(crate) fn over(owners: RecordOwners) -> Self {
        Self { owners }
    }

    /// The owners a part opens its records through: a clone shares the one registry, so a clone
    /// and its original open the same record set — and the same document — for an app and
    /// workspace, never two.
    #[must_use]
    pub(crate) fn owners(&self) -> RecordOwners {
        self.owners.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use automerge::transaction::Transactable as _;
    use durable_store::MemoryStore;
    use ui::permissions::UserId;
    use ui::record::RecordOwners;
    use ui::record::workspace::WorkspaceRecords;

    use super::SessionOwners;

    /// The workspace record every app of a workspace opens beside its own: the kind does not
    /// matter, only that both parts name the same one.
    const KIND: &str = "tilo.workspace.emoji";
    const OWNER: &str = "so.tilo.chat";
    const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000aa";

    fn in_workspace(owners: &SessionOwners) -> WorkspaceRecords {
        in_owned(&owners.owners())
    }

    /// The `value` a document's root holds, as text: what a page reads of the record a part wrote.
    fn held_value(doc: &automerge::AutoCommit) -> Option<String> {
        use automerge::ReadDoc as _;
        let (value, _) = doc.get(automerge::ROOT, "value").ok().flatten()?;
        match value {
            automerge::Value::Scalar(scalar) => match scalar.as_ref() {
                automerge::ScalarValue::Str(text) => Some(text.to_string()),
                other => Some(other.to_string()),
            },
            other => Some(other.to_string()),
        }
    }

    fn in_owned(owners: &RecordOwners) -> WorkspaceRecords {
        WorkspaceRecords::in_workspace(
            owners.clone(),
            WORKSPACE.to_owned(),
            UserId("person:me".to_owned()),
        )
    }

    /// One record, two parts: what the conversations' store wrote is read by the workspace
    /// settings' store at once, never after the frame that wrote it. This is the shape the
    /// session's parts had wrong while each built its own `RecordOwners`.
    #[test]
    fn a_record_one_part_writes_is_read_by_another_at_once() {
        let store = Rc::new(MemoryStore::new());
        let owners = SessionOwners {
            owners: RecordOwners::with_store(
                Rc::clone(&store) as Rc<dyn durable_store::DurableStore>
            ),
        };
        let writer = in_workspace(&owners);
        let reader = in_workspace(&owners);
        let id = collab_types::workspace_record_id(KIND);
        assert!(
            !reader.holds_record(OWNER, &id, KIND).expect("readable"),
            "nothing holds it before the write"
        );
        writer
            .open_record(OWNER, KIND, &|_| Ok(()))
            .expect("the record is made");
        assert!(
            reader.holds_record(OWNER, &id, KIND).expect("readable"),
            "the second part reads the record the first made, in the same pass"
        );
    }

    /// Two parts that build their own owners over one store are two record sets, and a record set
    /// follows only its own document: the page reads the agent names before the ask writes one,
    /// and after the write it still reads nothing. This is the session's bug — the byline that
    /// said "Agent" where the workspace's name was written a moment before — and one
    /// `SessionOwners` is what prevents it.
    #[test]
    fn two_owners_read_the_state_a_write_left_behind() {
        let store = Rc::new(MemoryStore::new());
        let reader = in_owned(&RecordOwners::with_store(
            Rc::clone(&store) as Rc<dyn durable_store::DurableStore>
        ));
        let writer = in_owned(&RecordOwners::with_store(
            Rc::clone(&store) as Rc<dyn durable_store::DurableStore>
        ));
        // The page's record set opens the record before anything named an agent.
        let read = reader
            .open_record(OWNER, KIND, &|_| Ok(()))
            .expect("the record is opened");
        // The ask writes the name through its own owners — the conversation's, in the bug — and
        // the page's record set never sees it.
        let wrote = writer
            .open_record(OWNER, KIND, &|_| Ok(()))
            .expect("the writer opens the same record");
        wrote
            .edit(false, |doc| {
                doc.put(automerge::ROOT, "value", "Assistant")
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .expect("the ask writes the name");
        let seen = read.read(held_value);
        assert_eq!(
            seen.flatten().as_deref(),
            None,
            "the page's own record set never saw the write: this is what one `SessionOwners` fixes"
        );
    }

    /// The same read through the session's one owners: the page reads the name the ask wrote, in
    /// the same pass, because both open the one record set.
    #[test]
    fn one_owners_reads_the_state_a_write_left() {
        let store = Rc::new(MemoryStore::new());
        let owners = SessionOwners::over(RecordOwners::with_store(
            Rc::clone(&store) as Rc<dyn durable_store::DurableStore>
        ));
        let read = in_workspace(&owners)
            .open_record(OWNER, KIND, &|_| Ok(()))
            .expect("the record is opened");
        let wrote = in_workspace(&owners)
            .open_record(OWNER, KIND, &|_| Ok(()))
            .expect("the writer opens the same record");
        wrote
            .edit(false, |doc| {
                doc.put(automerge::ROOT, "value", "Assistant")
                    .map(drop)
                    .map_err(|error| error.to_string())
            })
            .expect("the ask writes the name");
        let seen = read.read(held_value);
        assert_eq!(
            seen.flatten().as_deref(),
            Some("Assistant"),
            "the page reads the name the ask wrote, in the same pass"
        );
    }
}
