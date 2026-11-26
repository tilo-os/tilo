//! What an app writes on one device reaches the workspace's other devices: the broker commits the
//! app's effect receipt into the daemon's copy of the workspace here, which syncs it with the device
//! hosting the workspace, which hands it to every other member. Each device's daemon on this
//! machine's loopback joins one workspace hosted by a host on this machine whose certificate takes
//! part in no channels: the workspace's records are documents the owner commits.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host's server deliberately stays alive until each test ends"
)]

tilo_automerge_isolate::test_worker!();

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_app_host::collab_outbound::PeerPublisher;
use tilo_app_host::commits::ServiceCommits;
use tilo_app_host::workspace_commits::WorkspaceCommits;
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use tilo_workspace_service::service::ServiceSlot;
use tilo_workspace_service::workspaces::MemberDevice;
use tilo_workspace_service::workspaces::fixtures::{TestHost, test_host_in_the_clear};

use tilo_resource::AppNamed;
use tilo_resource::Replica;
use tilo_user_identity::WorkspaceList;

use crate::workspaces::{ListWrite, Personal};

/// A device: the daemon that keeps the workspaces its person joined here. A session reads and
/// writes its workspaces through its device's daemon, so what the daemon holds is what its person
/// has on this device.
struct Device {
    /// The device's workspaces, kept by its daemon and synced with the devices hosting them.
    daemon: TestHost,
    /// The daemon's service, which the broker commits an app's effect receipts through.
    service: ServiceSlot,
    /// The person and device this machine is, as its daemon signs.
    cert: DeviceCert,
    _state: tempfile::TempDir,
}

impl Device {
    /// A device certified by `root`: two of them are two devices of one person. Each device mints
    /// a key no other run shares, as each host does.
    fn of(root: &SigningKey, label: &str) -> Self {
        let state = tempfile::tempdir().expect("the device's directory");
        let key = test_server::fresh_signing_key(label);
        let cert = test_server::certified(root, &key, label, DeviceCapabilities::member());
        let daemon = tilo_rt::app()
            .block_on(test_host_in_the_clear(
                &state.path().join("workspaces"),
                &key,
                &cert,
            ))
            .expect("the app runtime drives the daemon");
        // The workspaces this device joins are member copies it signs for, as the device's daemon
        // holds them.
        daemon
            .workspaces()
            .set_member_device(Arc::new(MemberDevice {
                cert: cert.clone(),
                signer: Arc::new(key),
            }));
        let service = ServiceSlot::default();
        service.install(daemon.shared());
        Self {
            daemon,
            service,
            cert,
            _state: state,
        }
    }

    fn person(&self) -> [u8; 32] {
        self.cert.user_public_key
    }

    /// The workspaces this device's daemon keeps.
    fn copies(&self) -> tilo_workspace_service::workspaces::Workspaces {
        self.daemon.workspaces().clone()
    }

    /// Join the host's workspace with an invitation for this person, and wait until the workspace
    /// is held here.
    fn join(&self, host: &test_server::Host, request: u8) -> WorkspaceId {
        self.join_to(host, host.workspace(), request)
    }

    /// Join `workspace`, one `host` serves, with an invitation for this person, and wait until the
    /// workspace is held here.
    fn join_to(&self, host: &test_server::Host, workspace: &str, request: u8) -> WorkspaceId {
        let invitation = host.invitation_to(workspace, self.person(), request);
        let copies = self.copies();
        let joined = tilo_rt::app()
            .block_on(tilo_test_env::within(
                "the workspace's host admits this device",
                async move { copies.join_member(&invitation).await },
            ))
            .expect("the app runtime drives the daemon")
            .expect("the device's daemon redeems the invitation");
        assert_eq!(
            joined.to_string(),
            workspace,
            "the daemon holds the workspace the invitation is to"
        );
        WorkspaceId::parse(workspace).expect("a workspace id")
    }

    /// Enter the host's workspace as a further device of this person, at the host's own signed
    /// address: nothing is invited here, and the workspace answers with what it holds, so this
    /// device takes part in it from then on.
    fn enter(&self, host: &test_server::Host) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(host.workspace()).expect("a workspace id");
        let named = workspace_id.to_string();
        let held = named.clone();
        let hint = host.host_hint();
        let copies = self.copies();
        let entered = tilo_rt::app()
            .block_on(tilo_test_env::within(
                "the workspace's host admits this device",
                async move {
                    let workspace =
                        tilo_server_protocol::WorkspaceId::parse(&named).expect("a workspace id");
                    copies.hold_as_member(workspace, &hint).await
                },
            ))
            .expect("the app runtime drives the daemon")
            .expect("the workspace admits this device");
        assert_eq!(
            entered.to_string(),
            workspace_id.to_string(),
            "the device holds the workspace it entered"
        );
        let copies = self.copies();
        let member = tilo_rt::app()
            .block_on(async move { copies.is_member(&held).await })
            .expect("the app runtime drives the daemon")
            .expect("the daemon answers which workspaces it holds");
        assert!(member, "the entry is one of the person's own");
        workspace_id
    }
}

/// The receipt of an effect this device's app had in `workspace` , as the broker records it:
/// redacted, its actor this device's person.
fn receipt(
    device: &Device,
    workspace_id: WorkspaceId,
    invocation: uuid::Uuid,
) -> (String, Vec<u8>) {
    let mut effect = tilo_actions::receipt::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &tilo_actions::MemberId::person(hex::encode(device.person())).into(),
        tilo_actions::ActionContext::for_resource("groceries", "note", "Groceries"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    effect.completed_at = 100;
    (
        workspace_id.to_string(),
        serde_json::to_vec(&effect).expect("a redacted receipt"),
    )
}

/// Whether `device` 's copy of `workspace` holds the receipt of `invocation` .
fn holds_receipt(device: &Device, workspace_id: WorkspaceId, invocation: uuid::Uuid) -> bool {
    let named = workspace_id.to_string();
    let copies = device.copies();
    tilo_rt::app()
        .block_on(async move {
            copies.state(&named).await.map(|state| {
                state.admitted_workspace().is_some_and(|workspace| {
                    tilo_desktop_model::projection::workspace_effect_receipts_of(workspace)
                        .iter()
                        .any(|held| held.invocation.0 == invocation)
                })
            })
        })
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// An effect an app had in a workspace this device's daemon holds: the broker the daemon serves
/// the session's apps from commits its receipt to the daemon's copy, which syncs it with the
/// device hosting the workspace, and the other device's daemon holds it too.
#[test]
fn a_receipt_the_owner_commits_reaches_the_workspaces_other_session() {
    let host = test_server::Host::of(&SigningKey::from_bytes(&[0x71; 32]), "Notes");
    let writer = Device::of(&SigningKey::from_bytes(&[0x51; 32]), "The writer");
    let reader = Device::of(&SigningKey::from_bytes(&[0x61; 32]), "The reader");
    let workspace = writer.join(&host, 0x01);
    assert_eq!(reader.join(&host, 0x02), workspace);

    let invocation = uuid::Uuid::new_v4();
    write(&writer, workspace, invocation);
    wait_for(&reader, workspace, invocation);
}

/// Have `device` 's app write `invocation` into `workspace` , as the broker commits what an app
/// did: the device's daemon takes the receipt into its copy of the workspace, which leaves for the
/// device hosting the workspace, which hands it to the workspace's other devices.
fn write(device: &Device, workspace_id: WorkspaceId, invocation: uuid::Uuid) {
    let publisher = PeerPublisher::with_commits(Arc::new(ServiceCommits::new(
        device.service.clone(),
    )) as Arc<dyn WorkspaceCommits>);
    assert!(
        publisher.records_effects_for(&workspace_id.to_string()),
        "the workspace's effect receipts are the owner's to commit"
    );
    let (workspace, redacted) = receipt(device, workspace_id, invocation);
    tilo_rt::app()
        .block_on(async move { publisher.record_effect(workspace, redacted).await })
        .expect("the app runtime drives the daemon")
        .expect("the record takes the app's write");
    assert!(
        holds_receipt(device, workspace_id, invocation),
        "the write is the writing device's owner's"
    );
}

/// Wait until `device` 's copy of the workspace holds the receipt of `invocation` .
fn wait_for(device: &Device, workspace_id: WorkspaceId, invocation: uuid::Uuid) {
    let deadline = tilo_test_env::hang_deadline();
    while !holds_receipt(device, workspace_id, invocation) {
        assert!(
            std::time::Instant::now() < deadline,
            "the write reaches the workspace's other device"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// One person's further device takes their workspace in at its host without an invitation: a device
/// certified by a member's root is that member's device, so the workspace admits it and answers
/// with what it holds. What was written on the person's other device before this one came up is
/// here, and what this one writes reaches the other.
#[test]
fn a_person_s_further_device_enters_their_workspace_at_its_host() {
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let host = test_server::Host::of(&root, "Notes");
    let laptop = Device::of(&root, "The laptop");
    let desktop = Device::of(&root, "The desktop");
    assert_eq!(laptop.person(), desktop.person(), "one person, two devices");
    let workspace = laptop.join(&host, 0x01);

    // What the laptop wrote before the desktop was up.
    let earlier = uuid::Uuid::new_v4();
    write(&laptop, workspace, earlier);

    // The desktop enters the workspace at its host: no invitation, and the workspace's whole
    // history comes with the entry.
    assert_eq!(desktop.enter(&host), workspace);
    wait_for(&desktop, workspace, earlier);

    // And back: what the desktop writes reaches the laptop.
    let later = uuid::Uuid::new_v4();
    write(&desktop, workspace, later);
    wait_for(&laptop, workspace, later);
}

/// The session a fresh device starts from: one workspace of its own, nothing of the person's.
/// The person's list as a device of theirs holds it, with nothing listed yet.
fn empty_list() -> Replica<WorkspaceList, AppNamed> {
    Replica::new()
        .in_app("so.tilo.model", "Model")
        .as_invoker("person")
}

/// Make `writes` on `list`, as the device's daemon makes a session's, and answer how it stands.
fn written(list: &mut Replica<WorkspaceList, AppNamed>, writes: Vec<ListWrite>) -> WorkspaceList {
    for write in writes {
        list.apply(write)
            .expect("the list takes the session's write");
    }
    list.state().expect("the list reads")
}

/// The workspaces `list` names, in the person's order.
fn listed_ids(list: &WorkspaceList) -> Vec<String> {
    list.listed()
        .map(|(workspace, _)| workspace.to_string())
        .collect()
}

fn fresh_session() -> tilo_session_core::desktop::state::DesktopHandle {
    let mut minter = tilo_desktop_model::IdMinter::new(1);
    let policy = tilo_desktop_policy::DesktopPolicy::with_tree(
        tilo_session_core::desktop::seed_tree(&mut minter, "Session", Vec::new()),
        1,
    );
    policy.handle().clone()
}

/// The workspace ids `session`'s shell shows, in its order.
fn on_shell(session: &tilo_session_core::desktop::state::DesktopHandle) -> Vec<WorkspaceId> {
    session.read(|tree| {
        tree.workspaces
            .iter()
            .map(tilo_desktop_model::Workspace::id)
            .collect()
    })
}

/// The shell's first workspace goes when the person has a workspace of their own, and one their
/// governance names them the owner of is theirs however it came to this device: a computer restored
/// from their words that joined their own site's workspace does not found a first workspace beside
/// it, and needs no host's list to say so.
#[test]
fn a_joined_workspace_the_person_owns_is_theirs_before_any_host_answers() {
    let session = fresh_session();
    let [seed] = on_shell(&session)
        .try_into()
        .expect("a fresh shell shows one workspace");
    let key = test_server::fresh_signing_key("A restored computer");
    let cert = test_server::certified(
        &SigningKey::from_bytes(&[0x5a; 32]),
        &key,
        "A restored computer",
        DeviceCapabilities::member(),
    );
    let mut personal = Personal::open(tilo_network_actor::DeviceIdentity::kept(cert, key));
    personal.hold_seed(seed);
    let site = WorkspaceId::parse("0b7a5f4e-2c1d-4e8a-9f3b-6d5c4b3a2918").expect("a workspace id");
    personal.joined(site);
    let mut tree = session.read(Clone::clone);
    tree.workspaces
        .push(tilo_desktop_model::Workspace::new(site, "Lounge"));
    session.replace_tree(tree);

    // No host has answered a list, and the workspace's governance names the person its owner: they
    // have a workspace of their own, so the one this shell started with goes.
    let owned = std::collections::BTreeSet::from([site]);
    assert!(personal.settle_seed(&session, &WorkspaceList::default(), &[site], None, &owned));
    assert_eq!(personal.seed(), None);
    assert_eq!(
        on_shell(&session),
        [site],
        "the workspace the shell started with is taken off it, unfounded"
    );
}

/// What the device's daemon publishes of the person's workspaces on a host is what the session takes
/// them from: one its list names with the host's signed address is listed and entered there, as a
/// device restored from the person's root learns of their workspaces; and once the workspace names
/// this device among its devices no more (the person left it, or was taken out of it), it is handed
/// to the session to let go of, and nothing else of the person's goes with it.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one test's whole flow, from its setup to what it proves"
)]
fn a_workspace_the_daemon_holds_elsewhere_is_listed_and_let_go_once_this_device_is_out_of_it() {
    use tilo_peer_state::workspace_view::{LIST, Listed, WorkspaceReading, resource};

    let notes = WorkspaceId::new();
    // The host: a server of the site's, which runs the workspace's work.
    let server = SigningKey::from_bytes(&[0x76; 32]);
    let server_cert = test_server::certified(
        &SigningKey::from_bytes(&[0x77; 32]),
        &server,
        "The site",
        DeviceCapabilities::member(),
    );
    let signed = tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1 {
        version: 1,
        holds_keys: true,
        runs_work: true,
        label: String::new(),
        cloud: false,
        origin: None,
        workspace_id: tilo_peer_state::join::WorkspaceId::parse(&notes.to_string())
            .expect("a workspace id"),
        endpoint_addr: iroh::EndpointAddr::new(
            iroh::EndpointId::from_bytes(&server_cert.device_public_key).expect("a key"),
        ),
        user_public_key: server_cert.user_public_key,
        device_certificate: server_cert.clone(),
        packet_relay_url: "iroh:direct".into(),
    }
    .transport_hint(&server)
    .expect("the host signs where it answers");
    let root = SigningKey::from_bytes(&[0x56; 32]);
    let key = test_server::fresh_signing_key("The restored computer");
    let cert = test_server::certified(
        &root,
        &key,
        "The restored computer",
        DeviceCapabilities::member(),
    );
    let device = cert.device_public_key;
    let person = cert.user_public_key;
    let mut personal = Personal::open(tilo_network_actor::DeviceIdentity::kept(cert, key));
    let view = crate::workspace_view::WorkspaceView::new(
        person,
        device,
        std::sync::Arc::new(Unasked) as std::sync::Arc<dyn crate::workspace_view::ShellAsks>,
    );
    let written_host =
        hex::encode(postcard::to_allocvec(&signed).expect("the host's address encodes"));
    let daemon_list = serde_json::to_string(&[Listed {
        id: notes.to_string(),
        name: "Notes".to_owned(),
        labels: Vec::new(),
        own: false,
        host: Some(written_host),
    }])
    .expect("the list encodes");
    view.apply(LIST, Some(&daemon_list))
        .expect("the list reads");

    let on_hosts = crate::workspaces::on_hosts(&view, &device, &std::collections::BTreeSet::new());
    assert_eq!(
        on_hosts.get(&notes).map(|entry| entry.name.as_str()),
        Some("Notes"),
        "the daemon's workspace on a host is taken"
    );
    assert!(
        crate::workspaces::on_hosts(&view, &device, &std::collections::BTreeSet::from([notes]))
            .is_empty(),
        "a workspace this session holds is not entered again"
    );
    let mut list = empty_list();
    let taken = Personal::take_hosted(&list.state().expect("the list reads"), on_hosts);
    let list = written(&mut list, taken);
    assert_eq!(
        listed_ids(&list),
        [notes.to_string()],
        "the person's list gains the workspace"
    );
    assert!(
        !personal.made_here(Some(&list), notes),
        "a workspace taken from the daemon's list is not one this device founds, nor claims"
    );
    // Where the device's daemon enters it, as it reads the list.
    let address = list
        .get(notes)
        .and_then(tilo_user_identity::ListedWorkspace::address)
        .expect("the list says where the workspace's host is reached");
    let hint: tilo_peer_state::join::TransportHint =
        postcard::from_bytes(&address).expect("the host's address reads");
    assert!(
        hint.host.is_of_device(&server_cert.device_public_key),
        "it is entered at the host the daemon named"
    );

    // A workspace that names this device among its devices keeps it; one that names it no more
    // lets it go.
    let devices = |devices: Vec<[u8; 32]>| {
        serde_json::to_string(&WorkspaceReading {
            id: notes.to_string(),
            roster_devices: devices,
            ..WorkspaceReading::default()
        })
        .expect("a reading encodes")
    };
    let named = resource(&notes.to_string());
    view.apply(&named, Some(&devices(vec![device, [9; 32]])))
        .expect("the reading reads");
    assert!(personal.taken_out(&view).is_empty());
    view.apply(&named, Some(&devices(vec![[9; 32]])))
        .expect("the reading reads");
    assert_eq!(personal.taken_out(&view), [notes]);
    assert!(personal.taken_out(&view).is_empty(), "it is let go of once");
}

/// The daemon's asks, which these tests never make.
struct Unasked;

impl crate::workspace_view::ShellAsks for Unasked {
    fn account(
        &self,
        _: &str,
        _: &str,
        _: serde_json::Map<String, serde_json::Value>,
    ) -> tilo_session_client::engine::ServiceAnswer {
        Box::pin(std::future::ready(Err("nothing is asked here".to_owned())))
    }

    fn invoke(
        &self,
        _: Option<String>,
        _: tilo_actions::remote::InvokeRequest,
    ) -> tilo_session_client::engine::ServiceAnswer {
        Box::pin(std::future::ready(Err("nothing is asked here".to_owned())))
    }
}
