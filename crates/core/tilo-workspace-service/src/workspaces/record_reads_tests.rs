//! A record is read only as the workspace it is in grants its caller: never on another
//! workspace's grants, and where the caller acts for someone, the smaller of the two grants.

#![cfg(feature = "agents")]

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath, roles};
use tilo_actions::remote::{
    ActionCallError, Caller, DEVICE_SETTINGS, ResourceAddress, ResourceCopy, ResourceReads,
};
use tilo_user_identity::DeviceCapabilities;

use super::super::OwnWorkspaceOffer;
use super::super::fixtures::{
    device_cert, entry_join_as, hosting_device, record_grants, test_host,
};

const NOTES: &str = "tilo.notes";
/// The kind Notes' records here are of: the one the app manifests in these tests declare.
const NOTES_KIND: &str = "tilo.notes.Note";
/// Someone no workspace here admitted.
const STRANGER: [u8; 32] = [9; 32];

/// `caller`'s read of a note in `workspace`, through the device's one reader.
async fn read(
    workspaces: &super::super::Workspaces,
    caller: Caller,
    workspace: &str,
) -> Result<ResourceCopy, ActionCallError> {
    let reads = workspaces.resource_reads();
    let address = ResourceAddress {
        workspace: workspace.to_owned(),
        app: NOTES.to_owned(),
        record: "groceries".to_owned(),
    };
    reads.read(&caller, &address).await
}

fn refused(read: &Result<ResourceCopy, ActionCallError>) -> bool {
    matches!(read, Err(ActionCallError::Refused { .. }))
}

/// What a write decision on a record it may not change is answered: refused, saying why.
fn not_changed(write: &Result<(), ActionCallError>) -> bool {
    matches!(
        write,
        Err(ActionCallError::Refused { reason }) if reason.contains("change this record")
    )
}

/// A grant of `grantee` reading all of `workspace`, given by the person `by` names.
fn reads_all(id: &str, grantee: Grantee, workspace: &str, by: &str) -> Grant {
    Grant::new(
        id,
        grantee,
        TreePath::workspace(workspace),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person(by),
    )
}

/// The address of the device setting `record` in the person's own workspace `own`.
fn setting(own: &str, record: &str) -> ResourceAddress {
    ResourceAddress {
        workspace: own.to_owned(),
        app: DEVICE_SETTINGS.to_owned(),
        record: record.to_owned(),
    }
}

/// A grant of `grantee` reading all of `workspace` and changing nothing of it.
fn reads_only(id: &str, grantee: Grantee, workspace: &str, by: &str) -> Grant {
    Grant::new(
        id,
        grantee,
        TreePath::workspace(workspace),
        KindSet::READ,
        Answer::Allow,
        Grantee::person(by),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_workspace_grants_a_stranger_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let workspace = workspaces.create("Kitchen").await.unwrap().workspace;

    let stranger = Caller::authenticated(STRANGER, [8; 32]).in_workspace(workspace.clone());
    assert!(
        refused(&read(workspaces, stranger, &workspace).await),
        "a device the workspace never admitted reads nothing of it"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grant_in_one_workspace_reads_nothing_of_another() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let granting = workspaces.create("Kitchen").await.unwrap().workspace;
    let other = workspaces.create("Garden").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);
    workspaces
        .put_workspace_grants(
            &granting,
            &cert,
            [6; 32],
            vec![reads_all(
                "bot:moderation",
                Grantee::bot("moderation"),
                &granting,
                &person,
            )],
        )
        .await
        .expect("the kitchen grants the bot its reads");

    // Run in the kitchen, the bot reads the garden on the garden's grants alone: it has none.
    let bot = Caller::local_bot("moderation").in_workspace(granting.clone());
    assert!(
        refused(&read(workspaces, bot, &other).await),
        "the kitchen's grant reads nothing of the garden"
    );
    server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_app_acting_for_someone_reads_no_more_than_they_may() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let workspace = workspaces.create("Kitchen").await.unwrap().workspace;
    let person = hex::encode(cert.user_public_key);
    workspaces
        .put_workspace_grants(
            &workspace,
            &cert,
            [6; 32],
            vec![reads_all(
                "app:notes",
                Grantee::app(NOTES),
                &workspace,
                &person,
            )],
        )
        .await
        .expect("the workspace grants Notes its reads");

    // Notes may read the whole workspace, but the person it acts for here is no member: the
    // smaller grant, theirs, reads nothing.
    let notes = Caller::local_app(NOTES)
        .acting_for(hex::encode(STRANGER))
        .in_workspace(workspace.clone());
    assert!(
        refused(&read(workspaces, notes, &workspace).await),
        "an app reads no more than the person it acts for"
    );
    server.shutdown().await;
}

fn document(read: &Result<ResourceCopy, ActionCallError>) -> bool {
    matches!(read, Ok(ResourceCopy::Document(bytes)) if !bytes.is_empty())
}

/// What a read of a record it may not read is answered: refused, or not let in at all.
fn not_read(read: &Result<ResourceCopy, ActionCallError>) -> bool {
    matches!(
        read,
        Err(ActionCallError::Refused { .. } | ActionCallError::NotPermitted)
    )
}

/// A device holding two workspaces its person founded, Kitchen and Garden, each with a Notes
/// record and a private one.
struct Host {
    _dir: tempfile::TempDir,
    server: super::super::fixtures::TestHost,
    cert: tilo_user_identity::DeviceCert,
    person: String,
    kitchen: String,
    garden: String,
    asked: std::sync::atomic::AtomicU8,
}

async fn host() -> Host {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let kitchen = workspaces.create("Kitchen").await.unwrap().workspace;
    let garden = workspaces.create("Garden").await.unwrap().workspace;
    let host = Host {
        _dir: dir,
        person: hex::encode(cert.user_public_key),
        server,
        cert,
        kitchen,
        garden,
        asked: std::sync::atomic::AtomicU8::new(10),
    };
    for workspace in [host.kitchen.clone(), host.garden.clone()] {
        for record in ["", "private"] {
            host.write(&workspace, record).await;
        }
    }
    host
}

impl Host {
    fn workspaces(&self) -> &super::super::Workspaces {
        self.server.workspaces()
    }

    /// This device founds `record` of Notes in `workspace`, in the workspace's store, of the
    /// kind the app manifests here declare a read of.
    async fn write(&self, workspace: &str, record: &str) {
        super::super::fixtures::write_record_of_kind(
            self.workspaces(),
            workspace,
            (NOTES, NOTES_KIND),
            record,
            |document| {
                use automerge::transaction::Transactable as _;
                document
                    .put(automerge::ROOT, "text", "milk")
                    .map(drop)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .expect("the device writes its record");
    }

    /// `workspace` keeps `grants`, as its person decided.
    async fn grant(&self, workspace: &str, grants: Vec<Grant>) {
        let asked = self
            .asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.workspaces()
            .put_workspace_grants(workspace, &self.cert, [asked; 32], grants)
            .await
            .expect("the workspace keeps the grants");
    }

    /// Kitchen lets agent `scribe` read it, all but its private Notes record.
    async fn admit_scribe(&self) {
        self.grant(
            &self.kitchen,
            vec![
                reads_all(
                    "agent:scribe",
                    Grantee::agent("scribe", self.person.clone()),
                    &self.kitchen,
                    &self.person,
                ),
                Grant::new(
                    "agent:scribe:private",
                    Grantee::agent("scribe", &self.person),
                    TreePath::workspace(&self.kitchen)
                        .app(NOTES)
                        .resource("private"),
                    KindSet::READ,
                    Answer::Deny,
                    Grantee::person(&self.person),
                ),
            ],
        )
        .await;
    }

    /// The agent `agent` acting for the person, admitted in `workspace`.
    fn agent(&self, agent: &str, workspace: &str) -> Caller {
        Caller::local_agent(agent, self.person.clone())
            .in_workspace(workspace.to_owned())
            .in_new_execution()
    }

    /// The agent `agent` under a key of its own its person's device holds for it.
    fn own_key_agent(&self, agent: &str) -> Caller {
        Caller::delegated_agent(agent, self.cert.user_public_key, [9; 32])
            .in_workspace(self.kitchen.clone())
            .named(agent)
            .in_new_execution()
    }

    /// The person themself, as a connection admitted in Kitchen.
    fn person(&self) -> Caller {
        Caller::local_person(self.person.clone())
            .in_workspace(self.kitchen.clone())
            .in_new_execution()
    }

    /// `caller`'s read of Notes' `record` in `workspace`, through the device's one reader.
    async fn read(
        &self,
        caller: &Caller,
        workspace: &str,
        record: &str,
    ) -> Result<ResourceCopy, ActionCallError> {
        read_with(
            self.workspaces().resource_reads(),
            caller,
            workspace,
            record,
        )
        .await
    }
}

/// `caller`'s read of Notes' `record` in `workspace` through `reads`.
async fn read_with(
    reads: std::sync::Arc<dyn ResourceReads>,
    caller: &Caller,
    workspace: &str,
    record: &str,
) -> Result<ResourceCopy, ActionCallError> {
    let caller = caller.clone();
    let address = ResourceAddress {
        workspace: workspace.to_owned(),
        app: NOTES.to_owned(),
        record: record.to_owned(),
    };
    reads.read(&caller, &address).await
}

/// `caller`'s write decision for `app`'s `record` in `workspace` through `reads`.
async fn write_with(
    reads: std::sync::Arc<dyn ResourceReads>,
    caller: &Caller,
    workspace: &str,
    app: &str,
    record: &str,
) -> Result<(), ActionCallError> {
    let caller = caller.clone();
    let address = ResourceAddress {
        workspace: workspace.to_owned(),
        app: app.to_owned(),
        record: record.to_owned(),
    };
    reads.may_write(&caller, &address).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_reads_the_workspace_records_its_grant_and_its_person_allow() {
    let h = host().await;
    h.admit_scribe().await;
    for caller in [h.agent("scribe", &h.kitchen), h.own_key_agent("scribe")] {
        let read = h.read(&caller, &h.kitchen, "").await;
        assert!(document(&read), "{read:?}");
        // A record the workspace denies the agent: its person's authority does not widen it.
        let private = h.read(&caller, &h.kitchen, "private").await;
        assert!(not_read(&private), "{private:?}");
    }
    // An agent the workspace grants nothing reads nothing, whatever its person may.
    let idle = h.read(&h.agent("idle", &h.kitchen), &h.kitchen, "").await;
    assert!(not_read(&idle), "{idle:?}");
    h.server.shutdown().await;
}

/// A record nobody wrote reads as none yet, and one the workspace holds reads whole.
///
/// The broker's reader also answered a record whose writes this device held only in part
/// ("the writes this device holds of the record build on changes it does not hold"): it captured
/// records from a document owner that kept a sealed workspace's writes as they arrived. The one
/// reader reads the copy this device hosts instead (`Workspaces::resource_document`), which
/// holds a record only as the writes its governance took make it, so no such record exists to
/// read here.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_this_device_holds_nothing_of_reads_as_none_yet() {
    let h = host().await;
    let person = h.person();
    assert_eq!(
        h.read(&person, &h.kitchen, "never").await.unwrap(),
        ResourceCopy::Absent,
        "no app ever wrote the record: the workspace has none yet"
    );
    let read = h.read(&person, &h.kitchen, "").await;
    assert!(
        document(&read),
        "a record the workspace holds is read: {read:?}"
    );
    h.server.shutdown().await;
}

/// A grant taken back between the decision to read and the copy being handed out cuts the copy,
/// decided again where the reader hands it out, and every read after it.
#[tokio::test(flavor = "multi_thread")]
async fn a_grant_revoked_mid_read_cuts_the_copy_under_way_and_the_reads_that_follow() {
    let h = host().await;
    h.admit_scribe().await;
    let scribe = h.agent("scribe", &h.kitchen);
    assert!(document(&h.read(&scribe, &h.kitchen, "").await));
    // The read was decided and the copy taken, as the reader takes it.
    let copy = h
        .workspaces()
        .resource_document(&h.kitchen, NOTES, "")
        .await
        .expect("the record reads");
    assert!(copy.is_some());
    // The workspace takes the grant back before the copy is handed out.
    let grant = reads_all(
        "agent:scribe",
        Grantee::agent("scribe", "bo"),
        &h.kitchen,
        &h.person,
    )
    .id;
    h.workspaces()
        .remove_workspace_grants(&h.kitchen, &h.cert, [99; 32], vec![grant])
        .await
        .expect("the workspace takes the grant back");
    let address = ResourceAddress {
        workspace: h.kitchen.clone(),
        app: NOTES.to_owned(),
        record: String::new(),
    };
    let handed = h
        .workspaces()
        .resource_reads()
        .hand_out(h.workspaces(), &scribe, &address, copy)
        .await;
    assert!(not_read(&handed), "the copy under way is cut: {handed:?}");
    let read = h.read(&scribe, &h.kitchen, "").await;
    assert!(not_read(&read), "and every read after it: {read:?}");
    h.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_person_and_who_acts_for_them_read_here_and_nobody_acting_for_nobody() {
    let h = host().await;
    h.admit_scribe().await;
    let person = h.person();
    assert!(document(&h.read(&person, &h.kitchen, "").await));
    // The device's reader of its workspaces knows no app's declared grant, so it reads nothing for
    // an app: an app reads through a reader that bounds it by what it declares.
    let messages = Caller::local_app("so.tilo.messages")
        .acting_for(h.person.clone())
        .in_workspace(h.kitchen.clone());
    assert!(not_read(&h.read(&messages, &h.kitchen, "").await));
    // An agent the person's own device handed its session to reads as they do, narrowed by what
    // their answers deny it: `scribe` reads here, but not the record the workspace's answers deny
    // it. An agent the answers name nowhere, or an app acting for nobody, reads nothing.
    let scribe = person.clone().delegated_to("scribe");
    assert!(document(&h.read(&scribe, &h.kitchen, "").await));
    assert!(not_read(&h.read(&scribe, &h.kitchen, "private").await));
    for caller in [
        Caller::local_agent("a-stranger", "bo")
            .in_workspace(h.kitchen.clone())
            .in_new_execution(),
        Caller::local_app("so.tilo.messages").in_workspace(h.kitchen.clone()),
    ] {
        let read = h.read(&caller, &h.kitchen, "").await;
        assert!(not_read(&read), "{caller:?}: {read:?}");
    }
    h.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_watched_record_wakes_its_reader_on_its_next_write_alone() {
    let h = host().await;
    let reads = h.workspaces().resource_reads();
    let address = ResourceAddress {
        workspace: h.kitchen.clone(),
        app: NOTES.to_owned(),
        record: "groceries".to_owned(),
    };
    let (wake, woken) = tilo_actions::remote::ResourceWake::new();
    reads
        .watch(&h.person(), &address, &wake)
        .await
        .expect("the person watches their record");
    h.write(&h.kitchen, "other").await;
    assert!(
        wake.waiting(),
        "a write to another record wakes nobody here"
    );
    h.write(&h.kitchen, "groceries").await;
    tilo_test_env::within("the write wakes the reader", woken).await;
    assert!(!wake.waiting());
    // Nobody acting for nobody watches it.
    let alone = Caller::local_agent("scribe", "bo")
        .in_workspace(h.kitchen.clone())
        .in_new_execution();
    let (stranger, _) = tilo_actions::remote::ResourceWake::new();
    let refused = reads.watch(&alone, &address, &stranger).await;
    assert!(refused.is_err());
    h.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_is_decided_in_the_workspace_its_address_names() {
    let h = host().await;
    h.admit_scribe().await;
    // Garden lets the person's agent read its private record, which Kitchen keeps from it.
    h.grant(
        &h.garden,
        vec![reads_all(
            "agent:scribe",
            Grantee::agent("scribe", h.person.clone()),
            &h.garden,
            &h.person,
        )],
    )
    .await;
    // Admitted in Kitchen, the agent reads Garden's private record as Garden allows, and
    // Kitchen's as Kitchen does: each decided there, not by the workspace it was admitted to.
    let scribe = h.agent("scribe", &h.kitchen);
    assert!(document(&h.read(&scribe, &h.garden, "private").await));
    assert!(not_read(&h.read(&scribe, &h.kitchen, "private").await));
    // Admitted in Garden, Kitchen still keeps it.
    let in_garden = h.agent("scribe", &h.garden);
    assert!(not_read(&h.read(&in_garden, &h.kitchen, "private").await));
    // The person reads both of their workspaces, and nothing of one this device does not hold.
    let person = h.person();
    assert!(document(&h.read(&person, &h.garden, "").await));
    let elsewhere = uuid::Uuid::new_v4().to_string();
    assert!(not_read(&h.read(&person, &elsewhere, "").await));
    h.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_admitted_workspace_widens_nothing_for_an_agent() {
    let h = host().await;
    h.admit_scribe().await;
    // The agent reads its own workspace, and nothing of one its grants do not reach, though its
    // person may read there.
    let scribe = h.agent("scribe", &h.kitchen);
    assert!(document(&h.read(&scribe, &h.kitchen, "").await));
    let garden = h.read(&scribe, &h.garden, "").await;
    assert!(not_read(&garden), "{garden:?}");
    h.server.shutdown().await;
}

/// This device as its certificate authority names it before anyone signs in: it acts under its
/// own certificate.
struct ThisDevice {
    certificate: tilo_user_identity::DeviceCert,
}

impl crate::apps::certificate_authority::CertificateAuthority for ThisDevice {
    fn authorize(&self, _: uuid::Uuid, _: &tilo_user_identity::DeviceCert) -> Result<(), String> {
        Ok(())
    }
    fn own_device(&self) -> Option<tilo_user_identity::DeviceCert> {
        Some(self.certificate.clone())
    }
}

/// The device's reader with nobody signed in on it: its person owns Kitchen, and in Garden is
/// a member whom someone else made neither owner nor administrator.
fn before_sign_in(
    h: &Host,
) -> (
    std::sync::Arc<dyn ResourceReads>,
    crate::apps::collab_broker::AppActionBroker,
) {
    let mut tree = crate::apps::test_support::WorkspaceTree::default();
    let held = |id: &str, name: &str| {
        crate::apps::test_support::Workspace::new(
            crate::apps::test_support::WorkspaceId::parse(id).unwrap(),
            name,
        )
    };
    let mut kitchen = held(&h.kitchen, "Kitchen");
    kitchen.policy.set_owner(h.person.clone());
    let mut garden = held(&h.garden, "Garden");
    garden.policy.set_owner(hex::encode([5; 32]));
    garden
        .policy
        .add_member(h.person.clone(), crate::apps::test_support::Role::Editor);
    tree.workspaces.extend([kitchen, garden]);
    let membership = tree.shared();
    let services = crate::apps::services::ProcessServices {
        native_certificates: Some(std::sync::Arc::new(ThisDevice {
            certificate: h.cert.clone(),
        })),
        ..membership.services()
    };
    let broker = crate::apps::collab_app_broker::AppBrokerState::test_broker_with_authority(
        membership.authority(),
        services,
    );
    let reads = super::RecordReads::with_apps(h.workspaces(), broker.state(), None);
    (reads, broker)
}

#[tokio::test(flavor = "multi_thread")]
async fn before_sign_in_the_device_reads_only_where_its_person_governs() {
    let h = host().await;
    let (reads, _broker) = before_sign_in(&h);
    // Settings on this device, with nobody signed in to act for, reads the workspace its person
    // owns.
    let settings = Caller::local_app("so.tilo.settings")
        .in_workspace(h.kitchen.clone())
        .in_new_execution();
    let read = read_with(std::sync::Arc::clone(&reads), &settings, &h.kitchen, "").await;
    assert!(document(&read), "{read:?}");
    let device = Caller::local_person(h.person.clone()).in_workspace(h.kitchen.clone());
    let read = read_with(std::sync::Arc::clone(&reads), &device, &h.kitchen, "").await;
    assert!(document(&read), "{read:?}");
    // Garden's grants would allow its member's read, but an app that names nobody reads as the
    // device only where the signed member list makes its person owner or administrator. A caller
    // that names the person is decided by Garden's governance, as any other.
    let read = read_with(std::sync::Arc::clone(&reads), &settings, &h.garden, "").await;
    assert!(not_read(&read), "{read:?}");
    // Nothing reads as this device but what runs on it: no agent, and no other person.
    let agent = Caller::local_agent("scribe", h.person.clone()).in_workspace(h.kitchen.clone());
    assert!(
        read_with(std::sync::Arc::clone(&reads), &agent, &h.kitchen, "")
            .await
            .is_err()
    );
    let stranger = Caller::local_person("someone else").in_workspace(h.kitchen.clone());
    let read = read_with(reads, &stranger, &h.kitchen, "").await;
    assert!(not_read(&read), "{read:?}");
    h.server.shutdown().await;
}

/// The reader of a device's dispatch, its apps declaring what `declared` says of each: Notes its
/// notes alone, every other app nothing of its own.
fn dispatch_reads(
    h: &Host,
) -> (
    std::sync::Arc<dyn ResourceReads>,
    crate::apps::collab_broker::AppActionBroker,
) {
    let membership = crate::apps::test_support::WorkspaceTree::default().shared();
    let declared: crate::apps::app_actions::AppResourceKindsResolver =
        std::sync::Arc::new(|app: &str| {
            if app == NOTES {
                vec!["Note".to_owned()]
            } else {
                Vec::new()
            }
        });
    let services = crate::apps::services::ProcessServices {
        app_resource_kinds: Some(declared),
        ..membership.services()
    };
    let broker = crate::apps::collab_app_broker::AppBrokerState::test_broker_with_authority(
        membership.authority(),
        services,
    );
    let reads = super::RecordReads::with_apps(h.workspaces(), broker.state(), None);
    (reads, broker)
}

/// An app reads as the one actor the dispatch decides its calls as: its own records as its own,
/// a kind in its own node only where it declares it, and as the person it acts for elsewhere.
#[tokio::test(flavor = "multi_thread")]
async fn an_app_reads_its_own_records_and_no_kind_it_did_not_declare() {
    let h = host().await;
    let (reads, _broker) = dispatch_reads(&h);
    let app = |app: &str| {
        Caller::local_app(app)
            .acting_for(h.person.clone())
            .in_workspace(h.kitchen.clone())
    };
    // Notes reads its own record, an ordinary one; another app reads it as its person.
    for caller in [app(NOTES), app("so.tilo.messages")] {
        let read = read_with(std::sync::Arc::clone(&reads), &caller, &h.kitchen, "").await;
        assert!(document(&read), "{read:?}");
    }
    // Mail's mailbox is kept apart under its kind, which this Mail does not declare.
    let (mail, address) = (
        app("so.tilo.mail"),
        ResourceAddress {
            workspace: h.kitchen.clone(),
            app: "so.tilo.mail".to_owned(),
            record: String::new(),
        },
    );
    let read = reads.read(&mail, &address).await;
    assert!(not_read(&read));
    h.server.shutdown().await;
}

/// An app changes its own record as the person it acts for may change it there, and changes
/// nothing of a kind it did not declare.
#[tokio::test(flavor = "multi_thread")]
async fn an_app_changes_its_own_records_and_no_kind_it_did_not_declare() {
    let h = host().await;
    let (reads, _broker) = dispatch_reads(&h);
    let app = |app: &str| {
        Caller::local_app(app)
            .acting_for(h.person.clone())
            .in_workspace(h.kitchen.clone())
    };
    // Notes changes its own ordinary record, the person it acts for owning the workspace.
    let write = write_with(
        std::sync::Arc::clone(&reads),
        &app(NOTES),
        &h.kitchen,
        NOTES,
        "",
    )
    .await;
    assert!(write.is_ok(), "{write:?}");
    // Mail's mailbox is kept apart under its kind, which this Mail does not declare.
    let write = write_with(
        std::sync::Arc::clone(&reads),
        &app("so.tilo.mail"),
        &h.kitchen,
        "so.tilo.mail",
        "",
    )
    .await;
    assert!(not_changed(&write), "{write:?}");
    h.server.shutdown().await;
}

/// What an app keeps open of a record is bounded by what its manifest declares, whatever its
/// person may read: another app's record only where it declares that read, and that one never
/// changed; its own as its person may.
#[tokio::test(flavor = "multi_thread")]
async fn an_app_keeps_open_only_the_records_its_manifest_declares() {
    static NOTE_KINDS: &[u8] =
        b"{\"resources\":[{\"kind\":\"tilo.notes.Note\",\"label\":\"Note\",\"version\":1}]}";
    static READS_NOTES: &[u8] = b"{\"reads\":[\"tilo.notes.Note\"],\"resources\":[{\"kind\":\"test.reader.Card\",\"label\":\"Card\",\"version\":1}]}";
    collab_types::app_surface::register_package(NOTES, NOTE_KINDS);
    collab_types::app_surface::register_package("test.notes-reader", READS_NOTES);
    let h = host().await;
    let (reads, _broker) = dispatch_reads(&h);
    let app = |app: &str| {
        Caller::local_app(app)
            .acting_for(h.person.clone())
            .in_workspace(h.kitchen.clone())
    };
    let decide_read = |caller: Caller| {
        let (reads, address) = (
            std::sync::Arc::clone(&reads),
            ResourceAddress {
                workspace: h.kitchen.clone(),
                app: NOTES.to_owned(),
                record: String::new(),
            },
        );
        async move { reads.may_read(&caller, &address).await }
    };
    // Notes reads and changes its own record.
    let read = decide_read(app(NOTES)).await;
    assert!(read.is_ok(), "{read:?}");
    let write = write_with(
        std::sync::Arc::clone(&reads),
        &app(NOTES),
        &h.kitchen,
        NOTES,
        "",
    )
    .await;
    assert!(write.is_ok(), "{write:?}");
    // An app that declares it reads notes reads the record, and changes nothing of it.
    let read = decide_read(app("test.notes-reader")).await;
    assert!(read.is_ok(), "{read:?}");
    let write = write_with(
        std::sync::Arc::clone(&reads),
        &app("test.notes-reader"),
        &h.kitchen,
        NOTES,
        "",
    )
    .await;
    assert!(not_changed(&write), "{write:?}");
    // An app that declares no read of notes keeps it open neither way, its person's grants
    // notwithstanding.
    let read = decide_read(app("so.tilo.messages")).await;
    assert!(
        matches!(&read, Err(ActionCallError::Refused { reason }) if reason.contains("declares no read")),
        "{read:?}"
    );
    let write = write_with(reads, &app("so.tilo.messages"), &h.kitchen, NOTES, "").await;
    assert!(not_changed(&write), "{write:?}");
    h.server.shutdown().await;
}

/// A grant that reads a record alone changes nothing of it: a change needs the grant to allow one
/// at the record's node, whichever app asks.
#[tokio::test(flavor = "multi_thread")]
async fn a_grant_that_reads_a_record_alone_changes_nothing_of_it() {
    let h = host().await;
    let (reads, _broker) = dispatch_reads(&h);
    let bo = |app: &str| {
        Caller::local_app(app)
            .acting_for("bo")
            .in_workspace(h.kitchen.clone())
    };
    h.grant(
        &h.kitchen,
        vec![reads_only(
            "bo:reads",
            Grantee::person("bo"),
            &h.kitchen,
            &h.person,
        )],
    )
    .await;
    // Bo reads Notes' record through another app, and changes nothing of it, its own app's
    // included.
    for app in ["so.tilo.messages", NOTES] {
        let read = read_with(std::sync::Arc::clone(&reads), &bo(app), &h.kitchen, "").await;
        assert!(document(&read), "{app}: {read:?}");
        let write = write_with(
            std::sync::Arc::clone(&reads),
            &bo(app),
            &h.kitchen,
            NOTES,
            "",
        )
        .await;
        assert!(not_changed(&write), "{app}: {write:?}");
    }
    // The same grant allowing changes lets the app change the record.
    h.grant(
        &h.kitchen,
        vec![reads_all(
            "bo:changes",
            Grantee::person("bo"),
            &h.kitchen,
            &h.person,
        )],
    )
    .await;
    let write = write_with(reads, &bo(NOTES), &h.kitchen, NOTES, "").await;
    assert!(write.is_ok(), "{write:?}");
    h.server.shutdown().await;
}

/// A device's own settings, as the store answers them once the reader let its caller through.
struct Settings;

impl ResourceReads for Settings {
    fn read<'a>(
        &'a self,
        _: &'a Caller,
        _: &'a ResourceAddress,
    ) -> tilo_actions::remote::ReadsAnswer<'a, ResourceCopy> {
        Box::pin(std::future::ready(Ok(ResourceCopy::reading(
            serde_json::json!({"layout": "us"}),
            None,
        ))))
    }
}

/// A device's settings are read by its person from any of their devices, and by whoever the
/// person grants that setting in their own workspace; a role in a served workspace reaches none,
/// a grant on one setting reaches no other, and the person can take a grant back.
#[tokio::test(flavor = "multi_thread")]
async fn a_devices_own_settings_are_read_by_its_person_and_whoever_they_grant() {
    use tilo_server_protocol::workspaces::Role;

    let h = host().await;
    let workspaces = h.workspaces();
    let own = tilo_server_protocol::servers::person_workspace_id(&h.cert.user_public_key);
    // The person's own workspace exists here, where the grants that reach their devices are
    // written.
    if workspaces.summary(&own).await.is_err() {
        workspaces
            .create_workspace("Servers", Some(&own))
            .await
            .expect("the person's own workspace is founded");
    }
    // Bob joins Kitchen as a member and is made one of its administrators, with no grant naming
    // him for a setting; Carol joins too and is made one as well.
    let bob_root = SigningKey::from_bytes(&[2; 32]);
    let (bob_key, bob_cert) = device_cert(&bob_root, "Bob's", DeviceCapabilities::primary());
    entry_join_as(
        workspaces,
        &h.cert,
        &h.kitchen,
        (&bob_key, &bob_cert),
        1,
        Role::Editor,
    )
    .await;
    let carol_root = SigningKey::from_bytes(&[3; 32]);
    let (carol_key, carol_cert) = device_cert(&carol_root, "Carol's", DeviceCapabilities::member());
    entry_join_as(
        workspaces,
        &h.cert,
        &h.kitchen,
        (&carol_key, &carol_cert),
        2,
        Role::Editor,
    )
    .await;
    let bob = hex::encode(bob_root.verifying_key().to_bytes());
    let carol = hex::encode(carol_root.verifying_key().to_bytes());
    record_grants(
        workspaces,
        &h.kitchen,
        &[
            roles::administrator_grant(&h.kitchen, &bob, Grantee::person(h.person.clone())),
            roles::administrator_grant(&h.kitchen, &carol, Grantee::person(h.person.clone())),
        ],
    )
    .await;
    // What reaches the device is a grant in the person's own workspace, on one setting's node:
    // Carol's administrator role in Kitchen reaches nothing of it.
    record_grants(
        workspaces,
        &own,
        &[Grant::new(
            "carol-server",
            Grantee::person(carol.clone()),
            crate::workspaces::server_controls::settings_node(
                &own,
                &workspaces.device_cert().device_public_key,
                "server",
            ),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(h.person.clone()),
        )],
    )
    .await;

    workspaces.publish_own(
        DEVICE_SETTINGS,
        super::super::OwnReaders::OwnRecords,
        std::sync::Arc::new(Settings),
    );
    let reads = workspaces.resource_reads();

    // The person's own laptop: a device of theirs this computer never saw, reading any setting.
    let laptop = Caller::authenticated(h.cert.user_public_key, [21; 32]).in_workspace(own.clone());
    for record in ["server", "displays"] {
        assert!(
            reads.read(&laptop, &setting(&own, record)).await.is_ok(),
            "the person reads {record} from another of their devices"
        );
    }

    // Bob administers a workspace this device serves, and no grant names him: he reads nothing,
    // and neither does his agent.
    an_administrator_with_no_grant_reads_nothing(
        &reads, &own, &h.kitchen, &bob_root, &bob_key, &bob,
    )
    .await;

    // Carol was granted one setting in the person's own workspace, so she reads that one. Her
    // administrator role in Kitchen, which this device serves, reached nothing of it.
    a_granted_setting_reads_alone_until_it_is_taken_back(
        workspaces,
        &reads,
        &own,
        &h.kitchen,
        &carol_root,
        &carol_key,
    )
    .await;
    h.server.shutdown().await;
}

/// An administrator of a workspace this device serves, named by no grant, reads no setting of
/// this device, and neither does his agent.
async fn an_administrator_with_no_grant_reads_nothing(
    reads: &super::RecordReads,
    own: &str,
    kitchen: &str,
    bob_root: &SigningKey,
    bob_key: &SigningKey,
    bob: &str,
) {
    let bob_caller = Caller::authenticated(
        bob_root.verifying_key().to_bytes(),
        bob_key.verifying_key().to_bytes(),
    )
    .in_workspace(kitchen.to_owned());
    let Err(refusal) = reads.read(&bob_caller, &setting(own, "server")).await else {
        unreachable!("an administrator with no grant is refused a setting");
    };
    assert!(
        matches!(refusal, ActionCallError::Refused { .. }),
        "an administrator with no grant is refused: {refusal:?}"
    );
    assert!(
        refusal.to_string().contains("Only this device's person"),
        "the refusal says who reads them: {refusal}"
    );
    let agent = Caller::local_agent("bob-agent", bob.to_owned()).in_workspace(kitchen.to_owned());
    assert!(
        reads.read(&agent, &setting(own, "server")).await.is_err(),
        "an administrator's agent reads none of them"
    );
}

/// Carol, granted one setting in the person's own workspace, reads that one, and none of it once
/// the person takes the grant back.
async fn a_granted_setting_reads_alone_until_it_is_taken_back(
    workspaces: &super::super::Workspaces,
    reads: &super::RecordReads,
    own: &str,
    kitchen: &str,
    carol_root: &SigningKey,
    carol_key: &SigningKey,
) {
    let carol_caller = Caller::authenticated(
        carol_root.verifying_key().to_bytes(),
        carol_key.verifying_key().to_bytes(),
    )
    .in_workspace(kitchen.to_owned());
    assert!(
        reads
            .read(&carol_caller, &setting(own, "server"))
            .await
            .is_ok(),
        "whoever the person granted the setting reads it"
    );
    assert!(
        reads
            .read(&carol_caller, &setting(own, "displays"))
            .await
            .is_err(),
        "a grant on one setting reaches no other"
    );

    // The person takes the grant back: Carol reads nothing of that setting.
    crate::workspaces::fixtures::revoke_grants(
        workspaces,
        own,
        &[tilo_action_policy::GrantId::new("carol-server")],
    )
    .await;
    assert!(
        reads
            .read(&carol_caller, &setting(own, "server"))
            .await
            .is_err(),
        "the setting reads nothing after the person revokes the grant"
    );
}

/// The person's shell lists the workspace's invocations through the device's own reader, as its
/// listing service reads them once a second: every read is answered, never refused, whichever
/// workspace the shell shows — one this device holds, or one it does not — and what it lists is
/// the person's own calls alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_persons_shell_lists_their_invocations_without_a_refusal() {
    use tilo_actions::remote::{
        ReadAnswer, RemoteActionMessage, ResourceReadRequest, workspace_invocations,
    };
    let h = host().await;
    let reads = h.workspaces().resource_reads();
    let person = Caller::authenticated(h.cert.user_public_key, h.cert.device_public_key)
        .in_workspace(h.kitchen.clone());
    let listing = |workspace: &str| ResourceReadRequest {
        workspace: workspace.to_owned(),
        app: workspace_invocations::PROVIDER.to_owned(),
        record: String::new(),
        joined: false,
        wait_ms: Some(0),
        after: None,
        answer: ReadAnswer::Reading,
    };
    let reading = |answer: RemoteActionMessage| match answer {
        RemoteActionMessage::ResourceReading { result, .. } => {
            let reading = result.unwrap_or_else(|error| {
                panic!("the person's shell lists the workspace's invocations: {error}")
            });
            serde_json::from_str::<serde_json::Value>(&reading)
                .unwrap_or_else(|error| panic!("the listing is a reading: {error}"))
        }
        _ => panic!("the read is answered as a reading"),
    };
    // The items of a listing's reading: the page of calls it holds.
    let items = |reading: &serde_json::Value| {
        reading
            .pointer("/records/0/value/items")
            .and_then(serde_json::Value::as_array)
            .unwrap_or_else(|| panic!("the listing is a page: {reading}"))
            .clone()
    };
    // The workspace in front, which this device holds.
    let held = reading(
        std::sync::Arc::clone(&reads)
            .answer_read(&person, 0, listing(&h.kitchen))
            .await,
    );
    assert!(
        items(&held).is_empty(),
        "the person's page of waiting calls is read, and holds none yet: {held}"
    );
    // The workspace the shell shows after a switch to one this device keeps no copy of: the
    // listing still answers the person's own calls, instead of a policy refusing a record the
    // device keeps none of.
    let elsewhere = uuid::Uuid::new_v4().to_string();
    let moved = reading(
        std::sync::Arc::clone(&reads)
            .answer_read(&person, 0, listing(&elsewhere))
            .await,
    );
    assert!(
        items(&moved).is_empty(),
        "the page is read in a workspace this device keeps no copy of: {moved}"
    );
    // Nobody else's calls are in it: another person reads their own list, which holds none of
    // the person's.
    let stranger = Caller::local_person("someone else").in_workspace(h.kitchen.clone());
    let theirs = reading(reads.answer_read(&stranger, 0, listing(&h.kitchen)).await);
    assert!(
        items(&theirs).is_empty(),
        "another person's list holds none of the person's calls: {theirs}"
    );
    h.server.shutdown().await;
}
