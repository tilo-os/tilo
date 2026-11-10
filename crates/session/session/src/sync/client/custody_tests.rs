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
use tilo_user_identity::{DeviceCapabilities, DeviceCert};

use super::{Client, Enrolling, Entering, Entry, OwnPresence};
use crate::channels::ChannelLink;
use crate::engine::Engine;
use crate::platform::fabric::Continuity;

/// `key` 's certificate as a device of the person whose root key is `root` .
fn certified(
    root: &SigningKey,
    key: &SigningKey,
    label: &str,
    capabilities: DeviceCapabilities,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.to_owned(),
        capabilities,
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("a certified device");
    cert
}

/// A host on this machine governing one workspace, certified so that it takes part in no channels:
/// what its members record is committed in the clear.
struct Host {
    relay: Option<tilo_relay::daemon::Relay>,
    workspaces: tilo_relay::workspaces::Workspaces,
    cert: DeviceCert,
    workspace: String,
    runtime: tokio::runtime::Runtime,
    _state: tempfile::TempDir,
}

impl Drop for Host {
    fn drop(&mut self) {
        if let Some(relay) = self.relay.take() {
            self.runtime.block_on(relay.shutdown());
        }
    }
}

impl Host {
    /// A host of the person whose root key is `[0x71; 32]` .
    fn start() -> Self {
        Self::of(&SigningKey::from_bytes(&[0x71; 32]))
    }

    /// A host of the person `owner_key` , on this machine.
    fn of(owner_key: &SigningKey) -> Self {
        let state = tempfile::tempdir().expect("the host's directory");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("the host's runtime");
        let host_key = SigningKey::from_bytes(&[0x72; 32]);
        let cert = certified(
            owner_key,
            &host_key,
            "The notes host",
            DeviceCapabilities {
                can_own_workspaces: false,
                ..tilo_user_identity::cloud_enrollment::host_capabilities()
            },
        );
        let (relay, workspaces, workspace) = runtime.block_on(async {
            let relay = tilo_relay::daemon::Relay::start(
                tilo_relay::RelayConfig {
                    state_root: state.path().join("host"),
                    ..tilo_relay::RelayConfig::default()
                },
                tilo_relay::daemon::RelayIdentity {
                    user_public_key: owner_key.verifying_key().to_bytes(),
                    device_signing_key: host_key.clone(),
                    device_cert: cert.clone(),
                    revocations: tilo_user_identity::RevocationList::new(),
                },
            )
            .await
            .expect("the host starts");
            let workspaces = relay
                .workspaces()
                .expect("the host governs workspaces")
                .clone();
            let workspace = workspaces
                .create("Notes")
                .await
                .expect("the workspace")
                .workspace;
            (relay, workspaces, workspace)
        });
        Self {
            relay: Some(relay),
            workspaces,
            cert,
            workspace,
            runtime,
            _state: state,
        }
    }

    /// An invitation to the workspace that only `person` 's devices redeem.
    fn invitation(&self, person: [u8; 32], request: u8) -> String {
        let issued = self
            .runtime
            .block_on(self.workspaces.issue(
                &self.workspace,
                &self.cert,
                tilo_workspace_join::invitations::IssueInvitationRequest {
                    request_id: [request; 32],
                    role: tilo_workspace_join::MemberRole::Editor,
                    recipient_user_public_key: Some(person),
                    invitee_name: None,
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                    inviter_name: None,
                },
            ))
            .expect("an invitation to the workspace");
        tilo_workspace_join::encode_invitation_text(&issued.signed_invitation)
    }

    /// Where this host is reached for its workspace, as it signed it: what a device of the person
    /// is told to dial.
    fn host_hint(&self) -> tilo_peer_state::join::TransportHint {
        self.runtime
            .block_on(self.workspaces.hosts(&self.workspace))
            .expect("the host signs its own address")
            .into_iter()
            .next()
            .expect("the host serves its workspace")
    }
}

/// One session's side of the workspace: its engine, and its plane.
struct Device {
    engine: std::sync::Arc<Engine>,
    plane: Client,
    _state: tempfile::TempDir,
}

impl Device {
    /// A session of a device of the person whose root key is `[seed; 32]`,
    /// whose plane runs with its workspaces' channel link as the session's
    /// does.
    fn start(seed: u8, label: &str) -> Self {
        let root = SigningKey::from_bytes(&[seed; 32]);
        Self::of(
            &root,
            SigningKey::from_bytes(&[seed.wrapping_add(1); 32]),
            label,
        )
    }

    /// A session of `key` , a device certified by `root` : two of them are two devices of one
    /// person.
    fn of(root: &SigningKey, key: SigningKey, label: &str) -> Self {
        let cert = certified(root, &key, label, DeviceCapabilities::member());
        let identity = tilo_network_actor::HostIdentity::kept(cert, key);
        let state = tempfile::tempdir().expect("the session's directory");
        let engine = std::sync::Arc::new(
            Engine::open(identity.clone(), Some(state.path()), std::iter::empty())
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
    fn join(&self, host: &Host, request: u8) -> WorkspaceId {
        let workspace_id = WorkspaceId::parse(&host.workspace).expect("a workspace id");
        let slot = WorkspaceSlot::of_workspace(workspace_id);
        self.engine.bind(slot, workspace_id);
        self.plane
            .enroll(Enrolling {
                slot,
                workspace_id,
                invitation: host.invitation(self.person(), request),
                renewal: None,
            })
            .expect("the plane takes the enrollment");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(enrolled) = self.plane.take_enrolled().pop() {
                enrolled.result.expect("the workspace admits this device");
                return workspace_id;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the workspace's host answers the enrollment"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Enter the host's workspace as a further device of this person, at the host's own signed
    /// address: nothing is invited here, and the workspace answers with what it holds, so this
    /// device takes part in it from then on. Answers the workspace, and how long the entry took.
    fn enter(&self, host: &Host) -> (WorkspaceId, Duration) {
        let workspace_id = WorkspaceId::parse(&host.workspace).expect("a workspace id");
        let started = std::time::Instant::now();
        self.plane
            .enter(Entering {
                slot: WorkspaceSlot::of_workspace(workspace_id),
                workspace_id,
                name: "Notes".into(),
                host: host.host_hint(),
                held: false,
            })
            .expect("the plane takes the entry");
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(enrolled) = self.plane.take_enrolled().pop() {
                assert!(enrolled.member, "the entry is one of the person's own");
                enrolled.result.expect("the workspace admits this device");
                return (workspace_id, started.elapsed());
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the workspace's host answers the entry"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
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
#[allow(
    clippy::significant_drop_tightening,
    reason = "the host is both sessions' host: it outlives every join made on it"
)]
fn a_receipt_the_owner_commits_reaches_the_workspaces_other_session() {
    let host = Host::start();
    let writer = Device::start(0x51, "The writing device");
    let reader = Device::start(0x61, "The reading device");
    let workspace = writer.join(&host, 0x01);
    assert_eq!(reader.join(&host, 0x02), workspace);

    // The broker the writer's session serves its apps from.
    let publisher =
        crate::plane::owner_publisher(Some(std::sync::Arc::clone(&writer.engine)), None);
    assert!(
        publisher.records_effects_for(&workspace.to_string()),
        "the workspace's effect receipts are the owner's to commit"
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("the broker's runtime");
    let invocation = uuid::Uuid::new_v4();
    runtime
        .block_on(publisher.commit_owner(receipt(&writer, workspace, invocation), None))
        .expect("the owner commits the app's receipt");
    assert!(
        holds_receipt(&writer, workspace, invocation),
        "the receipt is the writer's owner's"
    );

    let deadline = tilo_test_env::hang_deadline();
    while !holds_receipt(&reader, workspace, invocation) {
        assert!(
            std::time::Instant::now() < deadline,
            "the receipt reaches the workspace's other session"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
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

/// Wait until `device` 's copy of the workspace holds the receipt of `invocation` , answering how
/// long that took.
fn wait_for(
    device: &Device,
    workspace_id: WorkspaceId,
    invocation: uuid::Uuid,
) -> std::time::Duration {
    let started = std::time::Instant::now();
    let deadline = tilo_test_env::hang_deadline();
    while !holds_receipt(device, workspace_id, invocation) {
        assert!(
            std::time::Instant::now() < deadline,
            "the write reaches the workspace's other device"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    started.elapsed()
}

/// One person's further device takes their workspace in at its host without an invitation: a device
/// certified by a member's root is that member's device, so the workspace admits it and answers
/// with what it holds. What was written on the person's other device before this one came up is
/// here, and what this one writes reaches the other.
#[test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the host is both devices' host: it outlives every join made on it"
)]
fn a_person_s_further_device_enters_their_workspace_at_its_host() {
    let root = SigningKey::from_bytes(&[0x71; 32]);
    let host = Host::of(&root);
    let laptop = Device::of(&root, SigningKey::from_bytes(&[0xa1; 32]), "The laptop");
    let desktop = Device::of(&root, SigningKey::from_bytes(&[0xa2; 32]), "The desktop");
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
    let (entered_workspace, entry) = desktop.enter(&host);
    assert_eq!(entered_workspace, workspace);
    let history = wait_for(&desktop, workspace, earlier);
    eprintln!("entry {entry:?}, history {history:?}");

    // And back: what the desktop writes reaches the laptop.
    let later = uuid::Uuid::new_v4();
    write(&runtime, &desktop, workspace, later);
    let back = wait_for(&laptop, workspace, later);
    eprintln!("back {back:?}");
}
