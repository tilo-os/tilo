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
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_user_identity::DeviceCapabilities;

use super::{Client, Enrolling, Entering, Entry, OwnPresence};
use crate::engine::Engine;
use crate::platform::fabric::Continuity;
use crate::workspace_keys::KeyLink;

/// One session's side of the workspace: its engine, and its peer network.
struct Device {
    engine: std::sync::Arc<Engine>,
    network: Client,
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
            Engine::open(
                identity.clone(),
                Some(state.path()),
                std::iter::empty(),
                crate::platform::membership_check::of_this_session(),
            )
            .expect("the session's engine opens"),
        );
        engine.attach_link(KeyLink::new());
        let network = Client::start(
            Entry {
                identity: Some(identity),
                own: OwnPresence::default(),
                hosts: Vec::new(),
                continuity: Continuity::default(),
                wake: std::sync::Arc::new(|| ()),
                personal: None,
                relays: host.relays(),
                lan: None,
            },
            Some(std::sync::Arc::clone(&engine)),
            None,
            Vec::new(),
            None,
            tilo_workspace_service::service::ServiceSlot::default(),
        )
        .expect("the session's peer network starts");
        Self {
            engine,
            network,
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
        let workspace_id = WorkspaceId::parse(workspace).expect("a workspace id");
        self.engine.bind(workspace_id);
        self.network
            .enroll(Enrolling {
                workspace_id,
                way: tilo_workspace_directory::Way::Invitation(host.invitation_to(
                    workspace,
                    self.person(),
                    request,
                )),
                name: String::new(),
                renewal: None,
            })
            .expect("the peer network takes the enrollment");
        self.enrolled()
            .result
            .expect("the workspace admits this device");
        workspace_id
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
) -> OwnerCommitRequest {
    let mut effect = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &tilo_actions::MemberId::person(hex::encode(device.person())),
        tilo_actions::ActionContext::for_resource("groceries", "note", "Groceries"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    effect.completed_at = 100;
    OwnerCommitRequest {
        workspace: workspace_id.to_string(),
        operation: CommitOperation::EffectReceipt {
            invocation,
            redacted: serde_json::to_vec(&effect).expect("a redacted receipt"),
        },
    }
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
    let publisher = crate::app_wire::owner_publisher(
        Some(std::sync::Arc::clone(&device.engine)),
        tilo_workspace_service::service::ServiceSlot::default(),
        None,
    );
    assert!(
        publisher.records_effects_for(&workspace_id.to_string()),
        "the workspace's effect receipts are the owner's to commit"
    );
    runtime
        .block_on(publisher.commit_owner(receipt(device, workspace_id, invocation), None))
        .expect("the owner commits the app's write");
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
fn fresh_session() -> tilo_desktop_surface::DesktopHandle {
    let mut minter = tilo_desktop_model::IdMinter::new(1);
    let (policy, _surface) = tilo_desktop_policy::DesktopPolicy::with_tree(
        tilo_desktop_surface::seed_tree(&mut minter, "Session", Vec::new()),
        1,
    );
    policy.handle().clone()
}

/// A computer restored from its person's root alone, with no invitation to their workspace and no
/// other device of theirs about, learns the workspace from the host that keeps it for them — the
/// list every device of theirs reads there — and enters it at that host as their further device:
/// what they wrote on their laptop before is on the restored computer. The host is someone else's,
/// as a hosting site is; the restored computer reaches it through the site's home workspace, which
/// a computer set up for a site joins on its own.
#[test]
fn a_computer_restored_from_its_root_enters_the_workspaces_its_host_lists_for_the_person() {
    let host = test_server::Host::site(&SigningKey::from_bytes(&[0x72; 32]), "Lounge");
    let notes = host.create("Notes");
    let root = SigningKey::from_bytes(&[0x52; 32]);
    let laptop = Device::of(&root, "The laptop", &host);
    let workspace = laptop.join_to(&host, &notes, 0x01);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let earlier = uuid::Uuid::new_v4();
    write(&runtime, &laptop, workspace, earlier);

    let restored = Device::of(&root, "The restored computer", &host);
    let lounge = restored.join(&host, 0x02);
    assert!(
        !restored.engine.holds(workspace),
        "nothing of the person's workspace is on the restored computer yet"
    );

    // The hosts of what the computer holds list the person's workspaces to it.
    let hosting = crate::hosted_workspaces::HostedWorkspaces::service(
        Some(&restored.engine),
        Some(restored.network.endpoint_handle()),
        runtime.handle().clone(),
    )
    .expect("a session holding workspaces reaches their hosts");
    let woken = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let wake: tilo_service::Wake = {
        let woken = std::sync::Arc::clone(&woken);
        std::sync::Arc::new(move || woken.store(true, std::sync::atomic::Ordering::Relaxed))
    };
    hosting.follow_lists(&wake);
    let deadline = tilo_test_env::hang_deadline();
    let mut listed = std::collections::BTreeMap::new();
    while !listed.contains_key(&workspace) {
        assert!(
            std::time::Instant::now() < deadline,
            "the host lists the person's workspace to the restored computer"
        );
        std::thread::sleep(Duration::from_millis(20));
        listed.append(&mut hosting.take_listed());
    }
    assert!(
        woken.load(std::sync::atomic::Ordering::Relaxed),
        "the session is woken to take the list in"
    );
    assert!(
        !listed.contains_key(&lounge),
        "a workspace the computer holds is not entered again"
    );

    // The session takes the list in and enters the workspace where the host said it answers.
    let mut personal = crate::workspaces::Personal::open(restored.engine.identity().clone(), None);
    assert!(
        personal.take_hosted(listed),
        "the person's list gains the workspace"
    );
    let adoption = personal.adopt_into(&fresh_session(), &|_| false);
    let entering = adoption
        .enter
        .iter()
        .find(|opened| opened.workspace == workspace)
        .expect("the restored computer enters the person's workspace");
    assert_eq!(entering.name, "Notes");
    let hint = personal
        .host_hint(workspace)
        .expect("the list says where the workspace's host is reached");
    assert!(
        hint.host.is_of_device(&entering.host),
        "it is entered at the host the list names"
    );
    restored.engine.bind(workspace);
    restored
        .network
        .enter(Entering {
            workspace_id: workspace,
            name: entering.name.clone(),
            host: hint,
            held: false,
        })
        .expect("the peer network takes the entry");
    let enrolled = restored.enrolled();
    assert!(enrolled.member, "the entry is one of the person's own");
    enrolled
        .result
        .expect("the host takes the restored computer in");
    wait_for(&restored, workspace, earlier);
}
