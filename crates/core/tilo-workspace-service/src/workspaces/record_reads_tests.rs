//! A record is read only as the workspace it is in grants its caller: never on another
//! workspace's grants, and where the caller acts for someone, the smaller of the two grants.

#![cfg(feature = "agents")]

use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_actions::remote::{ActionCallError, Caller, ResourceAddress, ResourceCopy, ResourceReads};

use super::super::OwnWorkspaceOffer;
use super::super::fixtures::{governor, test_host};

const NOTES: &str = "tilo.notes";
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
    tokio::task::spawn_blocking(move || reads.read(&caller, &address))
        .await
        .expect("the read ran")
}

fn refused(read: &Result<ResourceCopy, ActionCallError>) -> bool {
    matches!(read, Err(ActionCallError::Refused { .. }))
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

#[tokio::test(flavor = "multi_thread")]
async fn a_foreign_workspace_grants_a_stranger_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
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
    let (key, cert) = governor();
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
    let (key, cert) = governor();
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

/// A document holding `value` under `key`.
fn snapshot(key: &str, value: &str) -> Vec<u8> {
    use automerge::transaction::Transactable as _;

    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, key, value)
        .expect("the record's field");
    document.save()
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
    let (key, cert) = governor();
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

    /// This device founds `record` of Notes in `workspace`.
    async fn write(&self, workspace: &str, record: &str) {
        self.workspaces()
            .own_write(
                workspace,
                (NOTES, record),
                None,
                &snapshot("text", "milk"),
                true,
                true,
                None,
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
                    Grantee::agent("scribe"),
                    &self.kitchen,
                    &self.person,
                ),
                Grant::new(
                    "agent:scribe:private",
                    Grantee::agent("scribe"),
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
        Caller::local_agent(agent)
            .acting_for(self.person.clone())
            .in_workspace(workspace.to_owned())
            .in_new_execution()
    }

    /// The agent `agent` under a key of its own its person's device holds for it.
    fn own_key_agent(&self, agent: &str) -> Caller {
        Caller::delegated_agent(agent, self.cert.user_public_key, [9; 32])
            .in_workspace(self.kitchen.clone())
            .named(agent)
            .held_to(Vec::new())
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
    tokio::task::spawn_blocking(move || reads.read(&caller, &address))
        .await
        .expect("the read ran")
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
/// reader reads the copy this device governs instead (`Workspaces::resource_document`), which
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
        Grantee::agent("scribe"),
        &h.kitchen,
        &h.person,
    )
    .id;
    h.workspaces()
        .remove_workspace_grants(&h.kitchen, &h.cert, [99; 32], vec![grant])
        .await
        .expect("the workspace takes the grant back");
    let (reads, workspaces, caller) = (
        h.workspaces().resource_reads(),
        h.workspaces().clone(),
        scribe.clone(),
    );
    let address = ResourceAddress {
        workspace: h.kitchen.clone(),
        app: NOTES.to_owned(),
        record: String::new(),
    };
    let handed =
        tokio::task::spawn_blocking(move || reads.hand_out(&workspaces, &caller, &address, copy))
            .await
            .expect("the hand-out ran");
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
    // An app reads another app's record as the person whose session runs it.
    let messages = Caller::local_app("so.tilo.messages")
        .acting_for(h.person.clone())
        .in_workspace(h.kitchen.clone());
    assert!(document(&h.read(&messages, &h.kitchen, "").await));
    // An agent the person's own device handed its session to reads no record outside its app;
    // nor does an agent or an app acting for nobody.
    for caller in [
        person.delegated_to("scribe"),
        Caller::local_agent("scribe")
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
    let watching = (
        std::sync::Arc::clone(&reads),
        h.person(),
        address.clone(),
        wake.clone(),
    );
    tokio::task::spawn_blocking(move || {
        let (reads, person, address, wake) = watching;
        reads.watch(&person, &address, &wake)
    })
    .await
    .unwrap()
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
    let alone = Caller::local_agent("scribe")
        .in_workspace(h.kitchen.clone())
        .in_new_execution();
    let (stranger, _) = tilo_actions::remote::ResourceWake::new();
    let refused = tokio::task::spawn_blocking(move || reads.watch(&alone, &address, &stranger))
        .await
        .unwrap();
    assert!(refused.is_err());
    h.server.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_is_decided_in_the_workspace_its_address_names() {
    let h = host().await;
    h.admit_scribe().await;
    // Garden lets the agent read its private record, which Kitchen keeps from it.
    h.grant(
        &h.garden,
        vec![reads_all(
            "agent:scribe",
            Grantee::agent("scribe"),
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

/// The device's reader with nobody signed in on it: Kitchen is the workspace founded on it;
/// Garden, though its person's too, it did not found.
fn before_sign_in(
    h: &Host,
) -> (
    std::sync::Arc<dyn ResourceReads>,
    crate::apps::collab_broker::AppActionBroker,
) {
    let membership = crate::apps::test_support::WorkspaceTree::default().shared();
    let services = crate::apps::services::ProcessServices {
        native_certificates: Some(std::sync::Arc::new(ThisDevice {
            founded: uuid::Uuid::parse_str(&h.kitchen).unwrap(),
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
async fn before_sign_in_the_device_reads_the_workspace_founded_on_it_alone() {
    let h = host().await;
    let (reads, _broker) = before_sign_in(&h);
    // Settings on this device, with nobody signed in to act for, reads the founded workspace.
    let settings = Caller::local_app("so.tilo.settings")
        .in_workspace(h.kitchen.clone())
        .in_new_execution();
    let read = read_with(std::sync::Arc::clone(&reads), &settings, &h.kitchen, "").await;
    assert!(document(&read), "{read:?}");
    let device = Caller::local_person(h.person.clone()).in_workspace(h.kitchen.clone());
    let read = read_with(std::sync::Arc::clone(&reads), &device, &h.kitchen, "").await;
    assert!(document(&read), "{read:?}");
    // Garden's owner is the device's own person, so its grants would allow the read: it is
    // refused because nobody signed in, and the device founded only Kitchen.
    for caller in [
        settings.clone(),
        Caller::local_person(h.person.clone()).in_workspace(h.garden.clone()),
    ] {
        let read = read_with(std::sync::Arc::clone(&reads), &caller, &h.garden, "").await;
        assert!(not_read(&read), "{read:?}");
    }
    // Nothing reads as this device but what runs on it: no agent, and no other person.
    let agent = Caller::local_agent("scribe")
        .acting_for(h.person.clone())
        .in_workspace(h.kitchen.clone());
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
