//! What an app writes on one device reaches the workspace's other devices: the broker commits it to
//! this device's owner, the canonical copy of the workspace here, and the owner's outgoing custody
//! leaves on the plane's session with the workspace's host, which hands it to every other member.
//! Two sessions' planes on this machine's loopback, each with its own engine, join one workspace
//! governed by a host on this machine whose certificate takes part in no channels: the workspace's
//! records are documents the owner commits.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the host's relay deliberately stays alive until each test ends"
)]

tilo_automerge_isolate::test_worker!();

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_desktop_model::{WorkspaceId, WorkspaceSlot};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_user_identity::DeviceCapabilities;

use super::{Client, Enrolling, Entering, Entry, OwnPresence};
use crate::channels::ChannelLink;
use crate::engine::Engine;
use crate::platform::fabric::Continuity;

/// One session's side of the workspace: its engine, and its plane.
struct Device {
    engine: std::sync::Arc<Engine>,
    plane: Client,
    _state: tempfile::TempDir,
}

impl Device {
    /// A session of a device certified by `root`, relaying through `host`'s relay: two of them
    /// are two devices of one person. Each device mints a key no other run shares, as each host
    /// does.
    fn of(root: &SigningKey, label: &str, host: &test_relay::Host) -> Self {
        let state = tempfile::tempdir().expect("the session's directory");
        let key = test_relay::fresh_signing_key(label);
        let cert = test_relay::certified(root, &key, label, DeviceCapabilities::member());
        let identity = tilo_network_actor::HostIdentity::kept(cert, key);
        let engine = std::sync::Arc::new(
            Engine::open(
                identity.clone(),
                Some(state.path()),
                std::iter::empty(),
                crate::platform::custody::of_this_desk(),
            )
            .expect("the session's engine opens"),
        );
        engine.attach_link(ChannelLink::new());
        let plane = Client::start(
            Entry {
                identity: Some(identity),
                own: OwnPresence::default(),
                hosts: Vec::new(),
                continuity: Continuity::default(),
                wake: std::sync::Arc::new(|| ()),
                fabric: None,
                relays: host.relays(),
            },
            Some(std::sync::Arc::clone(&engine)),
            None,
            Vec::new(),
            None,
        )
        .expect("the session's plane starts");
        Self {
            engine,
            plane,
            _state: state,
        }
    }

    fn person(&self) -> [u8; 32] {
        self.engine.identity().device_cert.user_public_key
    }

    /// Join the host's workspace with an invitation for this person, and wait until the workspace
    /// is held here.
    fn join(&self, host: &test_relay::Host, request: u8) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(host.workspace()).expect("a workspace id");
        let slot = WorkspaceSlot::of_workspace(workspace_id);
        self.engine.bind(slot, workspace_id);
        self.plane
            .enroll(Enrolling {
                slot,
                workspace_id,
                way: tilo_workspace_directory::Way::Invitation(
                    host.invitation(self.person(), request),
                ),
                name: String::new(),
                renewal: None,
            })
            .expect("the plane takes the enrollment");
        self.enrolled()
            .result
            .expect("the workspace admits this device");
        workspace_id
    }

    /// What became of this device's enrollment or entry, once its host answered.
    fn enrolled(&self) -> tilo_session_client::Enrolled {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(enrolled) = self.plane.take_enrolled().pop() {
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
    fn enter(&self, host: &test_relay::Host) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(host.workspace()).expect("a workspace id");
        self.plane
            .enter(Entering {
                slot: WorkspaceSlot::of_workspace(workspace_id),
                workspace_id,
                name: "Notes".into(),
                host: host.host_hint(),
                held: false,
            })
            .expect("the plane takes the entry");
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
        &hex::encode(device.person()),
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
/// and the owner's custody leaves for the workspace's other session, which
/// then holds it too.
#[test]
fn a_receipt_the_owner_commits_reaches_the_workspaces_other_session() {
    let host = test_relay::Host::of(&SigningKey::from_bytes(&[0x71; 32]), "Notes");
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
/// did: the owner's custody leaves for the workspace's host, which hands it to the workspace's
/// other devices.
fn write(
    runtime: &tokio::runtime::Runtime,
    device: &Device,
    workspace_id: WorkspaceId,
    invocation: uuid::Uuid,
) {
    let publisher =
        crate::plane::owner_publisher(Some(std::sync::Arc::clone(&device.engine)), None);
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
    let host = test_relay::Host::of(&root, "Notes");
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
