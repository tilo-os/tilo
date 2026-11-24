//! What an app writes on one device reaches the workspace's other devices: the broker commits it to
//! this device's owner, the canonical copy of the workspace here, and the owner's outgoing ownership
//! leaves on the peer network's session with the workspace's host, which hands it to every other member.
//! Two sessions' peer networks on this machine's loopback, each with its own engine, join one workspace
//! governed by a host on this machine whose certificate takes part in no channels: the workspace's
//! records are documents the owner commits.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host's server deliberately stays alive until each test ends"
)]

tilo_automerge_isolate::test_worker!();

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_desktop_model::WorkspaceId;
use tilo_user_identity::DeviceCapabilities;

use tilo_resource::Replica;
use tilo_user_identity::WorkspaceList;

use super::{Client, Entering, Entry, OwnPresence};
use crate::engine::Engine;
use crate::platform::fabric::Continuity;
use crate::workspace_keys::KeyLink;
use crate::workspaces::{ListWrite, Personal};

/// One session's side of the workspace: its engine, and its peer network.
struct Device {
    engine: std::sync::Arc<Engine>,
    network: Client,
    /// The session's question sheet: what its person is asked here.
    _questions: tilo_service::AskReceiver,
    _state: tempfile::TempDir,
}

impl Device {
    /// A session of a device certified by `root`, relaying through `host`'s server: two of them
    /// are two devices of one person. Each device mints a key no other run shares, as each host
    /// does.
    fn of(root: &SigningKey, label: &str, host: &test_server::Host) -> Self {
        let state = tempfile::tempdir().expect("the session's directory");
        let key = test_server::fresh_signing_key(label);
        let cert = test_server::certified(root, &key, label, DeviceCapabilities::member());
        let identity = tilo_network_actor::DeviceIdentity::kept(cert, key);
        let engine = std::sync::Arc::new(
            Engine::open(identity.clone(), Some(state.path()), std::iter::empty())
                .expect("the session's engine opens"),
        );
        engine.attach_link(KeyLink::new());
        let (asks, questions) = tilo_service::asks::channel();
        let network = Client::start(
            Entry {
                identity: Some(identity),
                own: OwnPresence::default(),
                hosts: Vec::new(),
                continuity: Continuity::default(),
                wake: std::sync::Arc::new(|| ()),
                relays: host.relays(),
                lan: None,
            },
            Some(std::sync::Arc::clone(&engine)),
            Some(asks),
            Vec::new(),
        )
        .expect("the session's peer network starts");
        Self {
            engine,
            network,
            _questions: questions,
            _state: state,
        }
    }

    fn person(&self) -> [u8; 32] {
        self.engine.identity().device_cert.user_public_key
    }

    /// Join the host's workspace with an invitation for this person, and wait until the workspace
    /// is held here.
    fn join(&self, host: &test_server::Host, request: u8) -> WorkspaceId {
        self.join_to(host, host.workspace(), request)
    }

    /// Join `workspace`, one `host` governs, with an invitation for this person, and wait until the
    /// workspace is held here.
    fn join_to(&self, host: &test_server::Host, workspace: &str, request: u8) -> WorkspaceId {
        let _invitation = host.invitation_to(workspace, self.person(), request);
        panic!(
            "an invitation is redeemed by the device's daemon (account/workspaces#join), which \
             this harness does not start"
        )
    }

    /// What became of this device's enrollment or entry, once its host answered.
    fn enrolled(&self) -> tilo_session_client::Enrolled {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(enrolled) = self.network.take_enrolled().pop() {
                return enrolled;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the workspace's host answers"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Enter the host's workspace as a further device of this person, at the host's own signed
    /// address: nothing is invited here, and the workspace answers with what it holds, so this
    /// device takes part in it from then on.
    fn enter(&self, host: &test_server::Host) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(host.workspace()).expect("a workspace id");
        self.network
            .enter(Entering {
                workspace_id,
                name: "Notes".into(),
                host: host.host_hint(),
                held: false,
            })
            .expect("the peer network takes the entry");
        let enrolled = self.enrolled();
        assert!(enrolled.member, "the entry is one of the person's own");
        enrolled.result.expect("the workspace admits this device");
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
    let mut effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &tilo_actions::MemberId::person(hex::encode(device.person())),
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
    device
        .engine
        .owner()
        .with_current(workspace_id, |view| {
            Ok(view.peer().admitted_workspace().is_some_and(|workspace| {
                tilo_desktop_model::projection::workspace_effect_receipts_of(workspace)
                    .iter()
                    .any(|held| held.invocation.0 == invocation)
            }))
        })
        .unwrap_or(false)
}

/// An effect an app had in a workspace this session holds: the broker the
/// session serves its apps from commits its receipt to the session's owner,
/// and the owner's ownership leaves for the workspace's other session, which
/// then holds it too.
#[test]
fn a_receipt_the_owner_commits_reaches_the_workspaces_other_session() {
    let host = test_server::Host::of(&SigningKey::from_bytes(&[0x71; 32]), "Notes");
    let writer = Device::of(&SigningKey::from_bytes(&[0x51; 32]), "The writer", &host);
    let reader = Device::of(&SigningKey::from_bytes(&[0x61; 32]), "The reader", &host);
    let workspace = writer.join(&host, 0x01);
    assert_eq!(reader.join(&host, 0x02), workspace);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let invocation = uuid::Uuid::new_v4();
    write(&runtime, &writer, workspace, invocation);
    wait_for(&reader, workspace, invocation);
}

/// Have `device` 's app write `invocation` into `workspace` , as the broker commits what an app
/// did: the owner's ownership leaves for the workspace's host, which hands it to the workspace's
/// other devices.
fn write(
    runtime: &tokio::runtime::Runtime,
    device: &Device,
    workspace_id: WorkspaceId,
    invocation: uuid::Uuid,
) {
    let publisher = tilo_workspace_service::apps::collab_outbound::PeerPublisher::with_commits(
        std::sync::Arc::clone(&device.engine)
            as std::sync::Arc<
                dyn tilo_workspace_service::apps::workspace_commits::WorkspaceCommits,
            >,
    );
    assert!(
        publisher.records_effects_for(&workspace_id.to_string()),
        "the workspace's effect receipts are the owner's to commit"
    );
    let (workspace, redacted) = receipt(device, workspace_id, invocation);
    runtime
        .block_on(publisher.record_effect(workspace, redacted))
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
    let laptop = Device::of(&root, "The laptop", &host);
    let desktop = Device::of(&root, "The desktop", &host);
    assert_eq!(laptop.person(), desktop.person(), "one person, two devices");
    let workspace = laptop.join(&host, 0x01);

    // What the laptop wrote before the desktop was up.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let earlier = uuid::Uuid::new_v4();
    write(&runtime, &laptop, workspace, earlier);

    // The desktop enters the workspace at its host: no invitation, and the workspace's whole
    // history comes with the entry.
    assert_eq!(desktop.enter(&host), workspace);
    wait_for(&desktop, workspace, earlier);

    // And back: what the desktop writes reaches the laptop.
    let later = uuid::Uuid::new_v4();
    write(&runtime, &desktop, workspace, later);
    wait_for(&laptop, workspace, later);
}

/// The session a fresh device starts from: one workspace of its own, nothing of the person's.
/// The person's list as a device of theirs holds it, with nothing listed yet.
fn empty_list() -> Replica<WorkspaceList> {
    Replica::new().as_invoker("person")
}

/// Make `writes` on `list`, as the device's daemon makes a session's, and answer how it stands.
fn written(list: &mut Replica<WorkspaceList>, writes: Vec<ListWrite>) -> WorkspaceList {
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
        kept_here: false,
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
    let hint = Personal::host_hint(&list, notes)
        .expect("the list says where the workspace's host is reached");
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
