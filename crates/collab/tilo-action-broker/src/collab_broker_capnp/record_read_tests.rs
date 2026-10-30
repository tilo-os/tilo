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
use crate::collab_broker_capnp::principal_id_for_root;
use crate::services::{DocumentBytes, HostServices, LocalPersonServices, NativeDocumentReads};
use crate::test_support::{Grants as _, KindSet, Subject, TreePath, WorkspaceId, WorkspaceTree};

const NOTES: &str = "so.tilo.lang.notes";
const MAIL: &str = "so.tilo.lang.mail";

struct Bytes(Vec<u8>);
impl DocumentBytes for Bytes {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A document owner holding every record the room has, each captured as its route's bytes,
/// re-checking the reader's read between the two chunks of every capture (and running
/// `between` there, as a change landing mid-capture would).
#[derive(Default)]
struct Owner {
    captures: AtomicUsize,
    between: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl NativeDocumentReads for Owner {
    fn requires_room(&self, _: &str) -> bool {
        true
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

struct Fixture {
    state: Arc<CapnpBrokerState>,
    owner: Arc<Owner>,
    membership: crate::test_support::WorkspaceTreeHandle,
    reads: Arc<dyn tilo_actions::remote::RecordReads>,
    person: String,
    room: String,
    /// The grant that admits the agent `scribe`.
    scribe: tilo_action_policy::GrantId,
    _scratch: tempfile::TempDir,
}

/// A room its person owns, with agent `scribe` admitted to read it.
fn fixture() -> Fixture {
    let scratch = tempfile::tempdir().unwrap();
    let session =
        crate::test_owner::SessionOwner::initialize(&scratch.path().join("owner")).unwrap();
    let certificate = session.peer().authority().clone();
    let person = principal_id_for_root(certificate.user_public_key);
    let mut workspace = crate::test_support::Workspace::new(WorkspaceId(71), "Record reads");
    workspace.set_owner(person.clone());
    let room = workspace.room().to_string();
    let scribe = workspace.admit_agent("scribe", KindSet::READ);
    // One Notes record the room keeps from the agent, though its person may read it.
    workspace.deny(
        Subject::agent("scribe"),
        &TreePath::root().app(NOTES).resource("private"),
        KindSet::READ,
    );
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let owner = Arc::new(Owner::default());
    let services = HostServices {
        committed_documents: Some(Arc::clone(&owner) as Arc<dyn NativeDocumentReads>),
        local_person: Some(Arc::new(Person(certificate))),
        ..membership.services()
    };
    let broker = CapnpBrokerState::test_broker_with_authority(membership.authority(), services);
    let state = Arc::clone(broker.state());
    let reads = state
        .record_reads()
        .expect("a host that keeps the room's documents");
    Fixture {
        state,
        owner,
        membership,
        reads,
        person,
        room,
        scribe,
        _scratch: scratch,
    }
}

impl Fixture {
    /// The agent `agent`'s task, acting for the person, as the desktop's door admits it.
    fn task(&self, agent: &str) -> Caller {
        Caller::local_agent(agent)
            .acting_for(self.person.clone())
            .in_room(self.room.clone())
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
    tilo_action_policy::member_record_id(person, "tilolang.room")
}

#[test]
fn an_agent_task_reads_the_room_records_and_its_persons_own() {
    let f = fixture();
    let task = f.task("scribe");
    assert_eq!(
        f.read(&task, NOTES, "").unwrap(),
        RecordCopy::Document(format!("{NOTES}/").into_bytes()),
        "the room's Notes record, with no Notes window anywhere"
    );
    let own = own_record(&f.person);
    assert_eq!(
        f.read(&task, MAIL, &own).unwrap(),
        RecordCopy::Document(format!("{MAIL}/{own}").into_bytes()),
        "the person's own Mail record, read acting for them"
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
    // A record the room denies the agent: its person's authority does not widen it.
    assert!(matches!(
        f.read(&task, NOTES, "private"),
        Err(ActionHostError::Refused { .. })
    ));
    // An agent the room grants nothing reads nothing, whatever its person may.
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
fn only_an_agent_task_acting_for_its_person_reads_records_here() {
    let f = fixture();
    let refused = |caller: &Caller| {
        matches!(
            f.read(caller, NOTES, ""),
            Err(ActionHostError::Refused { reason }) if reason == tilo_actions::remote::RECORD_READS_FOR_TASKS
        )
    };
    // The person reads through their own apps' custody, not this door.
    let person = Caller::local_person(f.person.clone())
        .in_room(f.room.clone())
        .in_new_execution();
    assert!(refused(&person));
    // An agent the person's own device handed its session to carries that device's
    // delegation, which reads no record outside its app.
    let handed = person.delegated_to("scribe");
    assert!(refused(&handed));
    // An agent acting for nobody, and an app.
    let alone = Caller::local_agent("scribe")
        .in_room(f.room.clone())
        .in_new_execution();
    assert!(refused(&alone));
    assert!(refused(&Caller::local_app(NOTES).in_room(f.room.clone())));
    assert_eq!(f.owner.captures.load(Ordering::Acquire), 0);
    drop(f.state);
}
