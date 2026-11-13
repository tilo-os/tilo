//! An enrollment whose proof carries a certificate custody has replaced
//! since (renewed when it was due or ran out, or issued again when the
//! person restored their identity) presents the new one: the host ends up
//! holding the certificate the device presents from now on, not the one it
//! replaced, and the device stays the same member.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay serves the whole test"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common::endpoint;
use tilo_peer_state::join::WorkspaceId;

use std::time::Duration;

use ed25519_dalek::SigningKey;
use tilo_identity_host::custody::HostIdentity;
use tilo_peer_state::join::{self, Role};
use tilo_relay::workspace_join::WorkspaceJoinClient;
use tilo_relay::workspaces::IssueInvitationRequest;
use tilo_relay::{Relay, RelayConfig};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert};

const SECOND: u64 = 1_000_000_000;

fn certificate(
    root: &SigningKey,
    key: &SigningKey,
    caps: DeviceCapabilities,
    issued_at_ns: u64,
    expires_at_ns: Option<u64>,
) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "device".into(),
        capabilities: caps,
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

fn custody(cert: &DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity::kept(cert.clone(), key.clone())
}

/// A workspace on a relay with an invitation for Bob, which Bob's device proved
/// with `held` and the host admitted, but whose answer it never took in:
/// the proof stays retained, to be sent again.
struct Admitted {
    relay: Relay,
    _state: tempfile::TempDir,
    _directory: tempfile::TempDir,
    workspace: String,
    workspace_id: WorkspaceId,
    invitation: String,
    owner: tilo_peer_state::owner::NativeWorkspaceOwner,
    client: WorkspaceJoinClient,
    desk_key: SigningKey,
}

impl Admitted {
    async fn start(held: impl FnOnce(&SigningKey, &SigningKey) -> DeviceCert) -> Self {
        let alice = SigningKey::from_bytes(&[1; 32]);
        let bob = SigningKey::from_bytes(&[2; 32]);
        let host_key = SigningKey::from_bytes(&tilo_test_env::node_key());
        let laptop_key = SigningKey::from_bytes(&[4; 32]);
        let desk_key = SigningKey::from_bytes(&[5; 32]);
        let host = certificate(&alice, &host_key, DeviceCapabilities::host(), 1, None);
        let laptop = certificate(&alice, &laptop_key, DeviceCapabilities::primary(), 1, None);
        let held = held(&bob, &desk_key);
        let now = tilo_clock::unix_now_ns();
        let state = tempfile::tempdir().unwrap();
        // The host reaches no packet relay: the link it hands out then names the addresses it
        // has here, which the device dials directly. A host with a public relay names only that
        // relay, which this test's device would reach over the internet or not at all.
        let relay = Relay::start_with_relay_mode(
            RelayConfig {
                state_root: state.path().to_path_buf(),
                ..RelayConfig::default()
            },
            tilo_relay::daemon::RelayIdentity {
                user_public_key: alice.verifying_key().to_bytes(),
                device_signing_key: host_key.clone(),
                device_cert: host.clone(),
                revocations: RevocationList::new(),
            },
            iroh::RelayMode::Disabled,
        )
        .await
        .unwrap();
        let workspaces = relay.workspaces().unwrap();
        let workspace = workspaces.create("Design review").await.unwrap();
        workspaces
            .enter(&workspace.workspace, &laptop)
            .await
            .unwrap();
        let invitation = workspaces
            .issue(
                &workspace.workspace,
                &laptop,
                IssueInvitationRequest {
                    request_id: [9; 32],
                    role: Role::Editor,
                    recipient_user_public_key: Some(bob.verifying_key().to_bytes()),
                    invitee_name: None,
                    expires_at_ns: now.saturating_add(600 * SECOND),
                    inviter_name: None,
                },
            )
            .await
            .unwrap();
        let invitation = join::encode_invitation_text(&invitation.signed_invitation);
        let directory = tempfile::tempdir().unwrap();
        let owner =
            tilo_peer_state::owner::open_file(&directory.path().join("joined-workspaces.json"))
                .unwrap();
        let client = WorkspaceJoinClient::new(endpoint(&desk_key).await);
        let unanswered = tilo_peer_state::owner::enroll::prepare_and_complete(
            client.clone(),
            custody(&held, &desk_key),
            workspace.workspace_id,
            None,
            invitation.clone(),
            owner.clone(),
        )
        .await
        .unwrap();
        assert_eq!(unanswered.bootstrap().device_cert, held);
        Self {
            relay,
            _state: state,
            _directory: directory,
            workspace: workspace.workspace,
            workspace_id: workspace.workspace_id,
            invitation,
            owner,
            client,
            desk_key,
        }
    }

    /// The workspace changes after the host admitted the proof, so the host no
    /// longer replays its answer: the device takes the workspace in through the
    /// membership exchange.
    async fn moves_on(&self) {
        self.relay
            .workspaces()
            .unwrap()
            .rename(&self.workspace, "Lounge")
            .await
            .unwrap();
    }

    /// Send the retained proof again with `custody`'s certificate, and take
    /// the workspace in: the host must answer for `current` in place of
    /// `replaced`, and the device be the member it admitted.
    async fn retry(&self, current: &DeviceCert, replaced: &DeviceCert) {
        let enrollment = tilo_peer_state::owner::enroll::prepare_and_complete(
            self.client.clone(),
            custody(current, &self.desk_key),
            self.workspace_id,
            None,
            self.invitation.clone(),
            self.owner.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            &enrollment.bootstrap().device_cert,
            current,
            "the host answers for the certificate custody holds now"
        );
        assert!(
            enrollment.bootstrap().certified_roster.contains(current)
                && !enrollment.bootstrap().certified_roster.contains(replaced),
            "the host holds it in place of the certificate it replaced"
        );
        self.owner.install(&enrollment, tilo_clock::wall()).unwrap();
        let joined = tilo_desktop_model::WorkspaceId::parse(&self.workspace).unwrap();
        let role = self
            .owner
            .with_current(joined, |view| {
                Ok(view
                    .peer()
                    .permissions(self.desk_key.verifying_key().to_bytes(), tilo_clock::wall())
                    .ok()
                    .map(|member| member.role))
            })
            .unwrap();
        assert_eq!(role, Some(collab_types::Role::Editor), "the same member");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retained_proof_with_a_certificate_due_for_renewal_presents_the_renewal() {
    let now = tilo_clock::unix_now_ns();
    let mut due = None;
    let admitted = Admitted::start(|bob, desk_key| {
        // Two thirds through its lifetime: still valid, and due for renewal.
        let cert = certificate(
            bob,
            desk_key,
            DeviceCapabilities::primary(),
            now.saturating_sub(400 * SECOND),
            Some(now.saturating_add(200 * SECOND)),
        );
        due = Some(cert.clone());
        cert
    })
    .await;
    let due = due.unwrap();
    // Its custody renews the certificate before it tries again.
    let bob = SigningKey::from_bytes(&[2; 32]);
    let renewed = certificate(
        &bob,
        &admitted.desk_key,
        DeviceCapabilities::primary(),
        now,
        Some(now.saturating_add(600 * SECOND)),
    );
    admitted.retry(&renewed, &due).await;
}

/// The certificate the host admitted ran out before the device took the
/// workspace in. The host does not refuse the retained proof for it, forever:
/// the device proves its membership again with the renewal.
#[test]
fn a_retained_proof_whose_certificate_ran_out_is_answered_for_the_renewal() {
    // The certificate lapses because this test moves the process's wall clock
    // past its expiry, never because the setup happened to take that long. The
    // host and the device read that clock from their own threads, so the test
    // takes the process to itself, with a runtime of its own.
    tilo_test_env::in_own_process(
        module_path!(),
        "a_retained_proof_whose_certificate_ran_out_is_answered_for_the_renewal",
        || {
            let process = tilo_clock::testing::OwnProcess::of_a_test_in_its_own_process();
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .expect("a runtime for this test alone")
                .block_on(a_lapsed_certificate_is_answered_by_the_renewal(process));
        },
    );
}

async fn a_lapsed_certificate_is_answered_by_the_renewal(process: tilo_clock::testing::OwnProcess) {
    let started = tilo_clock::wall();
    let held = tilo_clock::testing::hold_wall_everywhere(process, started);
    let lifetime = 3 * SECOND;
    let now = tilo_clock::unix_now_ns();
    let mut lapsing = None;
    let admitted = Admitted::start(|bob, desk_key| {
        let cert = certificate(
            bob,
            desk_key,
            DeviceCapabilities::primary(),
            now.saturating_sub(SECOND),
            Some(now.saturating_add(lifetime)),
        );
        lapsing = Some(cert.clone());
        cert
    })
    .await;
    let lapsing = lapsing.unwrap();
    // The device is away for longer than the certificate lasts.
    drop(held);
    let lapsed_at = started
        .checked_add(Duration::from_nanos(lifetime.saturating_add(SECOND)))
        .expect("a wall time past the certificate's expiry");
    let _lapsed = tilo_clock::testing::hold_wall_everywhere(process, lapsed_at);
    let bob = SigningKey::from_bytes(&[2; 32]);
    let now = tilo_clock::unix_now_ns();
    let renewed = certificate(
        &bob,
        &admitted.desk_key,
        DeviceCapabilities::primary(),
        now,
        Some(now.saturating_add(600 * SECOND)),
    );
    admitted.retry(&renewed, &lapsing).await;
}

/// The person restored their identity while a join was in flight: the
/// device keeps its key and its root, and custody holds a certificate
/// issued again for it, which never expires like the one before. The
/// retained proof is still this device's, and is answered for the new
/// certificate. The laptop's Lounge (2026-10-23): admitted days before,
/// the workspace moved on since, and the identity restored before the device
/// took the workspace in.
#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_an_identity_restore_keeps_the_retained_join() {
    let mut before = None;
    let admitted = Admitted::start(|bob, desk_key| {
        let cert = certificate(bob, desk_key, DeviceCapabilities::primary(), 1, None);
        before = Some(cert.clone());
        cert
    })
    .await;
    let before = before.unwrap();
    admitted.moves_on().await;
    let bob = SigningKey::from_bytes(&[2; 32]);
    let restored = certificate(
        &bob,
        &admitted.desk_key,
        DeviceCapabilities::primary(),
        tilo_clock::unix_now_ns(),
        None,
    );
    admitted.retry(&restored, &before).await;
}
