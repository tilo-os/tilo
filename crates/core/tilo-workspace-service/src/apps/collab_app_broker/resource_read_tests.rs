//! The record read api against a document owner that hands out what it holds: an agent
//! acting for its person reads the records both may read, and nothing else, and a grant
//! revoked in the middle of a read cuts the reads that follow and a capture under way.
#![allow(
    clippy::significant_drop_tightening,
    reason = "fixtures hold the fake owner's state while asserting on it"
)]
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tilo_actions::remote::{ResourceAddress, ResourceCopy};

use super::*;
use crate::apps::collab_app_broker::person_id_for_root;
use crate::apps::services::{
    DocumentBytes, DocumentChangeServices, LocalPersonServices, NativeDocumentReads,
    ProcessServices,
};
use crate::apps::test_support::{
    Grantee, Grants as _, KindSet, Role, TreePath, WorkspaceId, WorkspaceTree,
};

const NOTES: &str = "so.tilo.notes";
const MAIL: &str = "so.tilo.mail";

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
    /// What this device holds of a workspace's sealed records; `None` for a
    /// workspace whose every record it holds whole.
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
    fn seals_documents(&self, _: &str) -> bool {
        true
    }
    fn resource_read(&self, _: &str, _: &str, record: &str) -> ResourceRead {
        match &self.sealed {
            None => ResourceRead::Sealed,
            Some(sealed) if sealed.whole.contains(&record) => ResourceRead::Sealed,
            Some(_) => ResourceRead::Own,
        }
    }
    fn holds_resource(&self, workspace: &str, app: &str, record: &str) -> bool {
        self.resource_read(workspace, app, record) != ResourceRead::Own
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
                format!("{}/{}", route.app_id, route.document_id).into_bytes(),
            )),
        })
    }
}

struct Person(tilo_user_identity::DeviceCert);
impl LocalPersonServices for Person {
    fn certificate(&self, _: &str, person: &str) -> Result<tilo_user_identity::DeviceCert, String> {
        if person != person_id_for_root(self.0.user_public_key) {
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
            tilo_actions::remote::ResourceChange,
            tilo_actions::ActionInvoker,
        )>,
    >,
);

impl DocumentChangeServices for Origin {
    fn apply(
        &self,
        workspace: &str,
        change: &tilo_actions::remote::ResourceChange,
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
    membership: crate::apps::test_support::WorkspaceTreeHandle,
    reads: Arc<dyn tilo_actions::remote::ResourceReads>,
    person: String,
    /// The person's root key.
    root: [u8; 32],
    workspace: String,
    /// Another person's workspace the person is a viewer of, which keeps its private Notes
    /// record from them.
    shared: String,
    /// A workspace the person is no member of.
    elsewhere: String,
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
        crate::apps::test_owner::SessionOwner::initialize(&scratch.path().join("owner")).unwrap();
    let certificate = session.device();
    let root = certificate.user_public_key;
    let person = person_id_for_root(root);
    let mut workspace =
        crate::apps::test_support::Workspace::new(WorkspaceId::from_u128(71), "Record reads");
    workspace.set_owner(person.clone());
    let workspace_id = workspace.id().to_string();
    let scribe = workspace.allow_agent("scribe", KindSet::READ);
    workspace.allow_app(Grantee::agent("scribe"), NOTES, KindSet::ALL);
    // Mail's changes the workspace keeps from the agent.
    workspace.deny(
        Grantee::agent("scribe"),
        &TreePath::root().app(MAIL),
        KindSet::UNDOABLE,
    );
    // One Notes record the workspace keeps from the agent, though its person may read it.
    workspace.deny(
        Grantee::agent("scribe"),
        &TreePath::root().app(NOTES).resource("private"),
        KindSet::READ,
    );
    let mut shared =
        crate::apps::test_support::Workspace::new(WorkspaceId::from_u128(72), "Shared");
    shared.set_owner("someone else");
    shared.admit(person.clone(), Role::Viewer);
    shared.deny(
        Grantee::person(person.clone()),
        &TreePath::root().app(NOTES).resource("private"),
        KindSet::READ,
    );
    let mut elsewhere =
        crate::apps::test_support::Workspace::new(WorkspaceId::from_u128(73), "Elsewhere");
    elsewhere.set_owner("someone else");
    let (shared_id, elsewhere_id) = (shared.id().to_string(), elsewhere.id().to_string());
    let mut tree = WorkspaceTree::default();
    tree.desktop
        .workspaces
        .extend([workspace, shared, elsewhere]);
    let membership = tree.shared();
    let owner = Arc::new(owner);
    let origin = Arc::new(Origin::default());
    let services = ProcessServices {
        resource_changes: Some(Arc::clone(&origin) as Arc<dyn DocumentChangeServices>),
        committed_documents: Some(Arc::clone(&owner) as Arc<dyn NativeDocumentReads>),
        local_person: Some(Arc::new(Person(certificate))),
        ..membership.services()
    };
    let broker = AppBrokerState::test_broker_with_authority(membership.authority(), services);
    let state = Arc::clone(broker.state());
    let reads = state
        .resource_reads()
        .expect("a host that keeps the workspace's documents");
    Fixture {
        state,
        origin,
        owner,
        membership,
        reads,
        person,
        root,
        workspace: workspace_id,
        shared: shared_id,
        elsewhere: elsewhere_id,
        scribe,
        _scratch: scratch,
    }
}

impl Fixture {
    /// The agent `agent` acting for the person, as the desktop's api admits it.
    fn agent(&self, agent: &str) -> Caller {
        Caller::local_agent(agent)
            .acting_for(self.person.clone())
            .in_workspace(self.workspace.clone())
            .in_new_execution()
    }

    /// The agent `agent` under a key of its own its person's device holds for it, as an MCP
    /// client of that device acts.
    fn own_key_agent(&self, agent: &str) -> Caller {
        Caller::delegated_agent(agent, self.root, [9; 32])
            .in_workspace(self.workspace.clone())
            .named(agent)
            .held_to(Vec::new())
            .in_new_execution()
    }

    fn read(
        &self,
        caller: &Caller,
        app: &str,
        record: &str,
    ) -> Result<ResourceCopy, ActionCallError> {
        read_in(self.reads.as_ref(), caller, (&self.workspace, app, record))
    }
}

/// `caller`'s read of `record` of `app` in `workspace`, through the record API.
fn read_in(
    reads: &dyn tilo_actions::remote::ResourceReads,
    caller: &Caller,
    (workspace, app, record): (&str, &str, &str),
) -> Result<ResourceCopy, ActionCallError> {
    reads.read(
        caller,
        &ResourceAddress {
            workspace: workspace.to_owned(),
            app: app.to_owned(),
            record: record.to_owned(),
        },
    )
}

/// What a read of a record it may not read is answered: refused, or not let in at all.
fn not_read(read: &Result<ResourceCopy, ActionCallError>) -> bool {
    matches!(
        read,
        Err(ActionCallError::Refused { reason }) if reason == tilo_actions::remote::RESOURCE_READ_REFUSED
    ) || matches!(read, Err(ActionCallError::NotPermitted))
}

/// This device as its certificate authority names it before anyone signs in: it governs the
/// workspace founded on it, and acts there under its own certificate.
struct ThisDevice {
    founded: uuid::Uuid,
    certificate: tilo_user_identity::DeviceCert,
}

impl crate::apps::certificate_authority::CertificateAuthority for ThisDevice {
    fn authorize(&self, _: uuid::Uuid, _: &tilo_user_identity::DeviceCert) -> Result<(), String> {
        Ok(())
    }
    fn governs(&self, workspace: uuid::Uuid, device: Option<&[u8; 32]>) -> bool {
        device.is_none() && workspace == self.founded
    }
    fn own_device(&self) -> Option<tilo_user_identity::DeviceCert> {
        Some(self.certificate.clone())
    }
}

/// A device nobody signed in on yet: the workspace founded on it, and another workspace whose
/// owner is the same person its certificate names, which it did not found.
struct BeforeSignIn {
    reads: Arc<dyn tilo_actions::remote::ResourceReads>,
    owner: Arc<Owner>,
    /// The person the device's own certificate names.
    device: String,
    founded: String,
    other: String,
    _scratch: tempfile::TempDir,
}

fn before_sign_in() -> BeforeSignIn {
    let scratch = tempfile::tempdir().unwrap();
    let session =
        crate::apps::test_owner::SessionOwner::initialize(&scratch.path().join("owner")).unwrap();
    let certificate = session.device();
    let device = person_id_for_root(certificate.user_public_key);
    let mut founded = crate::apps::test_support::Workspace::new(WorkspaceId::from_u128(81), "Home");
    founded.set_owner(device.clone());
    let mut other =
        crate::apps::test_support::Workspace::new(WorkspaceId::from_u128(82), "Elsewhere");
    other.set_owner(device.clone());
    let (founded_id, other_id) = (founded.id().to_string(), other.id().to_string());
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.extend([founded, other]);
    let membership = tree.shared();
    let owner = Arc::new(Owner::default());
    let services = ProcessServices {
        committed_documents: Some(Arc::clone(&owner) as Arc<dyn NativeDocumentReads>),
        native_certificates: Some(Arc::new(ThisDevice {
            founded: uuid::Uuid::parse_str(&founded_id).unwrap(),
            certificate,
        })),
        ..membership.services()
    };
    let broker = AppBrokerState::test_broker_with_authority(membership.authority(), services);
    let reads = broker
        .state()
        .resource_reads()
        .expect("before sign-in the device reads under its own certificate");
    BeforeSignIn {
        reads,
        owner,
        device,
        founded: founded_id,
        other: other_id,
        _scratch: scratch,
    }
}

tilo_test_env::scenario! {
    fn records_are_read_here_as_the_workspace_allows;

    phase an_agent_reads_the_workspace_records;
    {
        let f = fixture();
        for caller in [f.agent("scribe"), f.own_key_agent("scribe")] {
            assert_eq!(
                f.read(&caller, NOTES, "").unwrap(),
                ResourceCopy::Document(format!("{NOTES}/").into_bytes()),
                "the workspace's Notes record, with no Notes window anywhere"
            );
        }
    }

    phase a_sealed_workspaces_record_this_device_holds_nothing_of_reads_as_none_yet;
    {
        let f = fixture_with(Owner {
            sealed: Some(Sealed {
                whole: vec!["kept"],
                partial: vec!["torn"],
            }),
            ..Owner::default()
        });
        let caller = f.agent("scribe");
        assert_eq!(
            f.read(&caller, NOTES, "").unwrap(),
            ResourceCopy::Absent,
            "no app ever wrote the workspace's Notes record: the workspace has none yet"
        );
        assert_eq!(
            f.read(&caller, NOTES, "kept").unwrap(),
            ResourceCopy::Document(format!("{NOTES}/kept").into_bytes()),
            "a record the workspace holds is read from its writes"
        );
        let torn = f.read(&caller, NOTES, "torn").unwrap_err();
        assert!(
            torn.to_string().contains("build on changes"),
            "writes that make no whole record are not read as an empty one: {torn}"
        );
    }

    phase a_record_either_may_not_read_stays_unreadable;
    {
        let f = fixture();
        // A record the workspace denies the agent: its person's authority does not widen it,
        // whether the agent is admitted for its person or acts under a key of its own.
        for caller in [f.agent("scribe"), f.own_key_agent("scribe")] {
            assert!(matches!(
                f.read(&caller, NOTES, "private"),
                Err(ActionCallError::Refused { .. })
            ));
        }
        // An agent the workspace grants nothing reads nothing, whatever its person may.
        assert!(f.read(&f.agent("idle"), NOTES, "").is_err());
        assert_eq!(
            f.owner.captures.load(Ordering::Acquire),
            0,
            "nothing was captured"
        );
    }

    phase a_grant_revoked_mid_read_cuts_the_reads_that_follow_and_one_under_way;
    {
        let f = fixture();
        let caller = f.agent("scribe");
        assert!(f.read(&caller, NOTES, "").is_ok());
        // Revoked while a capture is between its chunks: the capture stops.
        let (membership, grant) = (f.membership.clone(), f.scribe.clone());
        *f.owner.between.lock().unwrap() = Some(Box::new(move || {
            assert!(membership.write().workspaces[0].revoke_grant(&grant));
        }));
        assert!(
            matches!(
                f.read(&caller, NOTES, ""),
                Err(ActionCallError::Refused { .. })
            ),
            "the capture under way is cut"
        );
        // And every read after it.
        assert!(f.read(&caller, NOTES, "").is_err());
        assert_eq!(f.owner.captures.load(Ordering::Acquire), 1);
    }

    phase a_person_or_an_agent_acting_for_them_reads_records_here;
    {
        let f = fixture();
        let refused = |caller: &Caller| {
            matches!(
                f.read(caller, NOTES, ""),
                Err(ActionCallError::Refused { reason }) if reason == tilo_actions::remote::RESOURCE_READS_FOR_AGENTS
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

    phase an_app_reads_another_apps_record_as_its_person;
    // An app reads another app's records as the person whose session runs it, and never
    // acting for nobody.
    {
        const MESSAGES: &str = "so.tilo.messages";
        let f = fixture();
        let app = Caller::local_app(MESSAGES)
            .acting_for(f.person.clone())
            .in_workspace(f.workspace.clone());
        assert_eq!(
            f.read(&app, NOTES, "").unwrap(),
            ResourceCopy::Document(format!("{NOTES}/").into_bytes()),
            "it reads the record with the person's device, as them"
        );
        let alone = Caller::local_app(MESSAGES).in_workspace(f.workspace.clone());
        assert!(
            matches!(
                f.read(&alone, NOTES, ""),
                Err(ActionCallError::Refused { reason }) if reason == RESOURCE_READS_FOR_AGENTS
            ),
            "an app acting for nobody reads nothing"
        );
    }

    phase a_watched_record_wakes_its_reader_on_the_next_write;
    // A reader watching a record is woken once by the next write that lands on it, and not by a
    // write to another record.
    {
        let f = fixture();
        let person = Caller::local_person(f.person.clone())
            .in_workspace(f.workspace.clone())
            .in_new_execution();
        let address = ResourceAddress {
            workspace: f.workspace.clone(),
            app: NOTES.to_owned(),
            record: String::new(),
        };
        let (wake, woken) = tilo_actions::remote::ResourceWake::new();
        f.reads.watch(&person, &address, &wake).unwrap();
        f.state.wake_document_reads(&f.workspace, MAIL, "");
        assert!(
            wake.waiting(),
            "a write to another record wakes nobody here"
        );
        f.state.wake_document_reads(&f.workspace, NOTES, "");
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
}

tilo_test_env::scenario! {
    fn a_read_is_decided_in_the_workspace_its_address_names;

    phase a_person_reads_another_workspace_as_that_workspace_allows;
    {
        let f = fixture();
        // A connection admitted to the person's own workspace, naming others.
        let person = Caller::local_person(f.person.clone())
            .in_workspace(f.workspace.clone())
            .in_new_execution();
        assert_eq!(
            read_in(f.reads.as_ref(), &person, (&f.shared, NOTES, "")).unwrap(),
            ResourceCopy::Document(format!("{NOTES}/").into_bytes()),
            "a workspace they are a viewer of is read as it allows them"
        );
        assert!(
            f.read(&person, NOTES, "private").is_ok(),
            "their own workspace lets them read its private record"
        );
        let kept = read_in(f.reads.as_ref(), &person, (&f.shared, NOTES, "private"));
        assert!(
            not_read(&kept),
            "the shared workspace keeps the same record's id from them: decided there, not by \
             the workspace the connection was admitted to: {kept:?}"
        );
        let foreign = read_in(f.reads.as_ref(), &person, (&f.elsewhere, NOTES, ""));
        assert!(not_read(&foreign), "a workspace they are no member of: {foreign:?}");
    }

    phase the_admitted_workspace_widens_nothing_for_an_agent;
    {
        let f = fixture();
        // The agent reads its own workspace, and nothing of one its grants do not reach,
        // though its person may read there.
        let scribe = f.agent("scribe");
        assert!(f.read(&scribe, NOTES, "").is_ok());
        let shared = read_in(f.reads.as_ref(), &scribe, (&f.shared, NOTES, ""));
        assert!(not_read(&shared), "{shared:?}");
        assert_eq!(
            f.owner.captures.load(Ordering::Acquire),
            1,
            "only the allowed read was captured"
        );
    }
}

tilo_test_env::scenario! {
    fn before_sign_in_the_device_reads_the_workspace_founded_on_it;

    phase an_app_on_the_device_reads_the_founded_workspace_as_the_device;
    {
        let d = before_sign_in();
        // Settings on this device, with nobody signed in to act for.
        let settings = Caller::local_app("so.tilo.settings")
            .in_workspace(d.founded.clone())
            .in_new_execution();
        assert_eq!(
            read_in(d.reads.as_ref(), &settings, (&d.founded, NOTES, "devices/d1/settings/display"))
                .unwrap(),
            ResourceCopy::Document(format!("{NOTES}/devices/d1/settings/display").into_bytes()),
            "the founded workspace's record, read under the device's own certificate"
        );
        let device = Caller::local_person(d.device.clone()).in_workspace(d.founded.clone());
        assert!(read_in(d.reads.as_ref(), &device, (&d.founded, NOTES, "")).is_ok());
    }

    phase no_other_workspace_is_read_before_sign_in;
    {
        let d = before_sign_in();
        // The other workspace's owner is the device's own person, so its grants would allow the
        // read: it is refused because nobody signed in, and the device founded only its own.
        for caller in [
            Caller::local_app("so.tilo.settings").in_workspace(d.founded.clone()),
            Caller::local_person(d.device.clone()).in_workspace(d.other.clone()),
        ] {
            let read = read_in(d.reads.as_ref(), &caller, (&d.other, NOTES, ""));
            assert!(not_read(&read), "{read:?}");
        }
        // Nothing reads as this device but what runs on it: no agent, and no other person.
        let agent = Caller::local_agent("scribe")
            .acting_for(d.device.clone())
            .in_workspace(d.founded.clone());
        assert!(read_in(d.reads.as_ref(), &agent, (&d.founded, NOTES, "")).is_err());
        let stranger = Caller::local_person("someone else").in_workspace(d.founded.clone());
        let read = read_in(d.reads.as_ref(), &stranger, (&d.founded, NOTES, ""));
        assert!(not_read(&read), "{read:?}");
        assert_eq!(
            d.owner.captures.load(Ordering::Acquire),
            0,
            "nothing was captured"
        );
    }
}

fn change(app: &str) -> tilo_actions::remote::ResourceChange {
    tilo_actions::remote::ResourceChange {
        app: app.to_owned(),
        record: String::new(),
        resource: format!("tl:{app}:root"),
        action: "new_note".into(),
        arguments: serde_json::Map::new(),
        preview: None,
    }
}

tilo_test_env::scenario! {
    fn records_are_changed_here_only_by_an_agent_for_its_person;

    phase an_agents_change_reaches_the_origin_acting_for_its_person;
    {
        let f = fixture();
        let changes = f.state.resource_changes().expect("a host with an origin");
        let answer = changes.apply(&f.agent("scribe"), &change(NOTES)).unwrap();
        assert_eq!(answer["differs"], false);
        let handed = f.origin.0.lock().unwrap();
        let (workspace, applied, invoker) = handed.first().expect("the origin was handed the change");
        assert_eq!(workspace, &f.workspace);
        assert_eq!(applied, &change(NOTES));
        assert_eq!(
            invoker.acting.by,
            tilo_actions::Actor::member(tilo_actions::MemberId::agent("scribe"))
        );
        assert!(invoker.acting.is_agent());
        assert_eq!(invoker.acting.acted_for_person(), Some(f.person.as_str()));
    }

    phase a_change_the_workspace_denies_the_agent_never_reaches_the_origin;
    {
        let f = fixture();
        let changes = f.state.resource_changes().unwrap();
        assert!(matches!(
            changes.apply(&f.agent("scribe"), &change(MAIL)),
            Err(ActionCallError::Refused { .. })
        ));
        assert!(f.origin.0.lock().unwrap().is_empty());
    }

    phase only_an_agent_acting_for_its_person_changes_records_here;
    {
        let f = fixture();
        let changes = f.state.resource_changes().unwrap();
        let person = Caller::local_person(f.person.clone())
            .in_workspace(f.workspace.clone())
            .in_new_execution();
        for caller in [person.clone(), person.delegated_to("scribe")] {
            assert!(matches!(
                changes.apply(&caller, &change(NOTES)),
                Err(ActionCallError::Refused { reason }) if reason == tilo_actions::remote::RESOURCE_READS_FOR_AGENTS
            ));
        }
        assert!(f.origin.0.lock().unwrap().is_empty());
    }
}
