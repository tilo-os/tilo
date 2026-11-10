//! The record read door against a document owner that hands out what it holds: an agent task
//! acting for its person reads the records both may read, and nothing else, and a grant
//! revoked in the middle of a task cuts the reads that follow and a capture under way.
#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures hold the fake owner's state while asserting on it"
)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilo_actions::remote::{RecordAddress, RecordCopy};

use super::*;
use crate::collab_app_broker::principal_id_for_root;
use crate::services::{
    DocumentBytes, HostServices, LocalPersonServices, NativeDocumentReads, RecordChangeServices,
};
use crate::test_support::{Grants as _, KindSet, Subject, TreePath, WorkspaceSlot, WorkspaceTree};

const NOTES: &str = "so.tilo.lang.notes";
const MAIL: &str = "so.tilo.lang.mail";

struct Bytes(Vec<u8>);
impl DocumentBytes for Bytes {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A document owner holding every record the workspace has, each captured as its route's bytes,
/// re-checking the reader's read between the two chunks of every capture (and running
/// `between` there, as a change landing mid-capture would).
#[derive(Default)]
struct Owner {
    captures: AtomicUsize,
    between: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// For a workspace whose records are sealed channels: what this device holds of them.
    sealed: Option<Sealed>,
}

/// The records of a sealed workspace this device holds writes of, by record id.
struct Sealed {
    /// Records whose writes make the whole record.
    whole: Vec<&'static str>,
    /// Records whose writes build on changes this device does not hold.
    partial: Vec<&'static str>,
}

impl NativeDocumentReads for Owner {
    fn requires_workspace(&self, _: &str) -> bool {
        self.sealed.is_none()
    }
    fn seals_records(&self, _: &str) -> bool {
        self.sealed.is_some()
    }
    fn record_read(&self, _: &str, _: &str, record: &str) -> RecordRead {
        match &self.sealed {
            None => RecordRead::Committed,
            Some(sealed) if sealed.whole.contains(&record) => RecordRead::Sealed,
            Some(_) => RecordRead::Own,
        }
    }
    fn holds_record(&self, workspace: &str, app: &str, record: &str) -> bool {
        self.record_read(workspace, app, record) != RecordRead::Own
            || self
                .sealed
                .as_ref()
                .is_some_and(|sealed| sealed.partial.contains(&record))
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        context.revalidate()
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        _: Option<[u8; 32]>,
        _: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.authorize(context)?;
        let between = self.between.lock().unwrap().take();
        if let Some(between) = between {
            between();
        }
        self.authorize(context)?;
        self.captures.fetch_add(1, Ordering::AcqRel);
        let route = context.route();
        Ok(NativeDocumentRead::Captured {
            revision: [1; 32],
            heads: Vec::new(),
            body: Box::new(Bytes(
                format!("{}/{}", route.app_id, route.record_id).into_bytes(),
            )),
        })
    }
}

struct Person(tilo_user_identity::DeviceCert);
impl LocalPersonServices for Person {
    fn certificate(&self, _: &str, person: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        if person != principal_id_for_root(self.0.user_public_key) {
            return Err("not this device's person".into());
        }
        Ok(self.0.clone())
    }
}

/// The workspace's origin, recording every change it was handed and who it was for.
#[derive(Default)]
struct Origin(
    Mutex<
        Vec<(
            String,
            tilo_actions::remote::RecordChange,
            tilo_actions::ActionInvoker,
        )>,
    >,
);

impl RecordChangeServices for Origin {
    fn apply(
        &self,
        workspace: &str,
        change: &tilo_actions::remote::RecordChange,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<serde_json::Value, String> {
        self.0
            .lock()
            .unwrap()
            .push((workspace.to_owned(), change.clone(), invoker.clone()));
        Ok(serde_json::json!({"ok": {"value": null, "resources": []}, "differs": false}))
    }
}

struct Fixture {
    state: Arc<AppBrokerState>,
    origin: Arc<Origin>,
    owner: Arc<Owner>,
    membership: crate::test_support::WorkspaceTreeHandle,
    reads: Arc<dyn tilo_actions::remote::RecordReads>,
    person: String,
    workspace: String,
    /// The grant that admits the agent `scribe`.
    scribe: tilo_action_policy::GrantId,
    _scratch: tempfile::TempDir,
}

/// A workspace its person owns, with agent `scribe` admitted to read it.
fn fixture() -> Fixture {
    fixture_with(Owner::default())
}

/// [`fixture`], its records held by `owner`.
fn fixture_with(owner: Owner) -> Fixture {
    let scratch = tempfile::tempdir().unwrap();
    let session =
        crate::test_owner::SessionOwner::initialize(&scratch.path().join("owner")).unwrap();
    let certificate = session.device();
    let person = principal_id_for_root(certificate.user_public_key);
    let mut workspace = crate::test_support::Workspace::new(WorkspaceSlot(71), "Record reads");
    workspace.set_owner(person.clone());
    let workspace_id = workspace.id().to_string();
    let scribe = workspace.allow_agent("scribe", KindSet::READ);
    workspace.allow_app(Subject::agent("scribe"), NOTES, KindSet::ALL);
    // Mail's changes the workspace keeps from the agent.
    workspace.deny(
        Subject::agent("scribe"),
        &TreePath::root().app(MAIL),
        KindSet::UNDOABLE,
    );
    // One Notes record the workspace keeps from the agent, though its person may read it.
    workspace.deny(
        Subject::agent("scribe"),
        &TreePath::root().app(NOTES).resource("private"),
        KindSet::READ,
    );
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let owner = Arc::new(owner);
    let origin = Arc::new(Origin::default());
    let services = HostServices {
        record_changes: Some(Arc::clone(&origin) as Arc<dyn RecordChangeServices>),
        committed_documents: Some(Arc::clone(&owner) as Arc<dyn NativeDocumentReads>),
        local_person: Some(Arc::new(Person(certificate))),
        ..membership.services()
    };
    let broker = AppBrokerState::test_broker_with_authority(membership.authority(), services);
    let state = Arc::clone(broker.state());
    let reads = state
        .record_reads()
        .expect("a host that keeps the workspace's documents");
    Fixture {
        state,
        origin,
        owner,
        membership,
        reads,
        person,
        workspace: workspace_id,
        scribe,
        _scratch: scratch,
    }
}

impl Fixture {
    /// The agent `agent`'s task, acting for the person, as the desktop's door admits it.
    fn task(&self, agent: &str) -> Caller {
        Caller::local_agent(agent)
            .acting_for(self.person.clone())
            .in_workspace(self.workspace.clone())
            .in_new_execution()
    }

    fn read(
        &self,
        caller: &Caller,
        app: &str,
        record: &str,
    ) -> Result<RecordCopy, ActionHostError> {
        self.reads.read(
            caller,
            &RecordAddress {
                app: app.to_owned(),
                record: record.to_owned(),
            },
        )
    }
}

/// `person`'s own record of Mail, the one an app with `records: .own` keeps.
fn own_record(person: &str) -> String {
    tilo_action_policy::member_record_id(person, "tilolang.workspace")
}

#[test]
fn an_agent_task_reads_the_workspace_records_and_its_persons_own() {
    let f = fixture();
    let task = f.task("scribe");
    assert_eq!(
        f.read(&task, NOTES, "").unwrap(),
        RecordCopy::Document(format!("{NOTES}/").into_bytes()),
        "the workspace's Notes record, with no Notes window anywhere"
    );
    let own = own_record(&f.person);
    assert_eq!(
        f.read(&task, MAIL, &own).unwrap(),
        RecordCopy::Document(format!("{MAIL}/{own}").into_bytes()),
        "the person's own Mail record, read acting for them"
    );
}

#[test]
fn a_sealed_workspaces_record_this_device_holds_nothing_of_reads_as_none_yet() {
    let f = fixture_with(Owner {
        sealed: Some(Sealed {
            whole: vec!["kept"],
            partial: vec!["torn"],
        }),
        ..Owner::default()
    });
    let task = f.task("scribe");
    assert_eq!(
        f.read(&task, NOTES, "").unwrap(),
        RecordCopy::Absent,
        "no app ever wrote the workspace's Notes record: the workspace has none yet"
    );
    assert_eq!(
        f.read(&task, NOTES, "kept").unwrap(),
        RecordCopy::Document(format!("{NOTES}/kept").into_bytes()),
        "a record the workspace holds is read from its writes"
    );
    let torn = f.read(&task, NOTES, "torn").unwrap_err();
    assert!(
        torn.to_string().contains("build on changes"),
        "writes that make no whole record are not read as an empty one: {torn}"
    );
}

#[test]
fn a_record_either_may_not_read_stays_unreadable() {
    let f = fixture();
    let task = f.task("scribe");
    // Another member's own record: the person may not read it, so neither may their agent.
    let theirs = own_record(&"b2".repeat(32));
    assert!(
        matches!(
            f.read(&task, MAIL, &theirs),
            Err(ActionHostError::Refused { .. })
        ),
        "another member's own record"
    );
    // A record the workspace denies the agent: its person's authority does not widen it.
    assert!(matches!(
        f.read(&task, NOTES, "private"),
        Err(ActionHostError::Refused { .. })
    ));
    // An agent the workspace grants nothing reads nothing, whatever its person may.
    assert!(f.read(&f.task("idle"), NOTES, "").is_err());
    assert_eq!(
        f.owner.captures.load(Ordering::Acquire),
        0,
        "nothing was captured"
    );
}

#[test]
fn a_grant_revoked_mid_task_cuts_the_reads_that_follow_and_one_under_way() {
    let f = fixture();
    let task = f.task("scribe");
    assert!(f.read(&task, NOTES, "").is_ok());
    // Revoked while a capture is between its chunks: the capture stops.
    let (membership, grant) = (f.membership.clone(), f.scribe.clone());
    *f.owner.between.lock().unwrap() = Some(Box::new(move || {
        assert!(membership.write().workspaces[0].revoke_grant(&grant));
    }));
    assert!(
        matches!(
            f.read(&task, NOTES, ""),
            Err(ActionHostError::Refused { .. })
        ),
        "the capture under way is cut"
    );
    // And every read after it.
    assert!(f.read(&task, NOTES, "").is_err());
    assert_eq!(f.owner.captures.load(Ordering::Acquire), 1);
}

#[test]
fn a_person_or_an_agent_task_acting_for_them_reads_records_here() {
    let f = fixture();
    let refused = |caller: &Caller| {
        matches!(
            f.read(caller, NOTES, ""),
            Err(ActionHostError::Refused { reason }) if reason == tilo_actions::remote::RECORD_READS_FOR_TASKS
        )
    };
    // The person reads their own workspace's records, as their apps do.
    let person = Caller::local_person(f.person.clone())
        .in_workspace(f.workspace.clone())
        .in_new_execution();
    assert!(f.read(&person, NOTES, "").is_ok());
    // An agent the person's own device handed its session to carries that device's
    // delegation, which reads no record outside its app.
    let handed = person.delegated_to("scribe");
    assert!(refused(&handed));
    // An agent acting for nobody, and an app.
    let alone = Caller::local_agent("scribe")
        .in_workspace(f.workspace.clone())
        .in_new_execution();
    assert!(refused(&alone));
    assert!(refused(
        &Caller::local_app(NOTES).in_workspace(f.workspace.clone())
    ));
    assert_eq!(f.owner.captures.load(Ordering::Acquire), 1);
    drop(f.state);
}

/// A reader watching a record is woken once by the next write that lands on it, and not by a
/// write to another record.
#[test]
fn a_watched_record_wakes_its_reader_on_the_next_write() {
    let f = fixture();
    let person = Caller::local_person(f.person.clone())
        .in_workspace(f.workspace.clone())
        .in_new_execution();
    let address = RecordAddress {
        app: NOTES.to_owned(),
        record: String::new(),
    };
    let (wake, woken) = tilo_actions::remote::RecordWake::new();
    f.reads.watch(&person, &address, &wake).unwrap();
    f.state.wake_record_reads(&f.workspace, MAIL, "");
    assert!(
        wake.waiting(),
        "a write to another record wakes nobody here"
    );
    f.state.wake_record_reads(&f.workspace, NOTES, "");
    assert!(matches!(
        woken.wait_timeout(std::time::Duration::from_secs(5)),
        Ok(Some(()))
    ));
    assert!(!wake.waiting());
    let alone = Caller::local_agent("scribe")
        .in_workspace(f.workspace.clone())
        .in_new_execution();
    assert!(f.reads.watch(&alone, &address, &wake).is_err());
}

fn change(app: &str) -> tilo_actions::remote::RecordChange {
    tilo_actions::remote::RecordChange {
        app: app.to_owned(),
        record: String::new(),
        resource: format!("tl:{app}:root"),
        action: "new_note".into(),
        arguments: serde_json::Map::new(),
        preview: None,
    }
}

#[test]
fn an_agent_tasks_change_reaches_the_origin_acting_for_its_person() {
    let f = fixture();
    let changes = f.state.record_changes().expect("a host with an origin");
    let answer = changes.apply(&f.task("scribe"), &change(NOTES)).unwrap();
    assert_eq!(answer["differs"], false);
    let handed = f.origin.0.lock().unwrap();
    let (workspace, applied, invoker) = handed.first().expect("the origin was handed the change");
    assert_eq!(workspace, &f.workspace);
    assert_eq!(applied, &change(NOTES));
    assert_eq!(invoker.actor, "agent:scribe");
    assert_eq!(invoker.kind, tilo_actions::ActorKind::Agent);
    assert_eq!(invoker.on_behalf_of.as_deref(), Some(f.person.as_str()));
}

#[test]
fn a_change_the_workspace_denies_the_agent_never_reaches_the_origin() {
    let f = fixture();
    let changes = f.state.record_changes().unwrap();
    assert!(matches!(
        changes.apply(&f.task("scribe"), &change(MAIL)),
        Err(ActionHostError::Refused { .. })
    ));
    assert!(f.origin.0.lock().unwrap().is_empty());
}

#[test]
fn only_an_agent_task_acting_for_its_person_changes_records_here() {
    let f = fixture();
    let changes = f.state.record_changes().unwrap();
    let person = Caller::local_person(f.person.clone())
        .in_workspace(f.workspace.clone())
        .in_new_execution();
    for caller in [person.clone(), person.delegated_to("scribe")] {
        assert!(matches!(
            changes.apply(&caller, &change(NOTES)),
            Err(ActionHostError::Refused { reason }) if reason == tilo_actions::remote::RECORD_READS_FOR_TASKS
        ));
    }
    assert!(f.origin.0.lock().unwrap().is_empty());
}
