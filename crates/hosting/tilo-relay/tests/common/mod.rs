//! Test fixtures shared across the integration suite.

#![allow(
    dead_code,
    reason = "each integration module uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::PathBuf;

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use tempfile::TempDir;

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

use tilo_relay::{
    RelayConfig, RelayHandle, WorkspaceBinding,
    daemon::{Relay, RelayIdentity},
};

/// A "user" (root keypair) + on-disk identity root usable by the
/// tests.
pub struct TestUser {
    pub user_sk: SigningKey,
    pub user_pk: [u8; 32],
}

impl TestUser {
    pub fn fresh(label: &str) -> Self {
        let (sk, u) = UserIdentity::generate(label, 1);
        Self {
            user_sk: sk,
            user_pk: u.public_key,
        }
    }

    /// Enrol a device (mints a keypair + signed cert).
    pub fn enrol(&self, label: &str, caps: DeviceCapabilities) -> TestDevice {
        let (dev_sk, dev) = DeviceIdentity::generate(label, 100);
        let mut cert = DeviceCert {
            user_public_key: self.user_pk,
            device_public_key: dev.public_key,
            device_label: dev.label,
            capabilities: caps,
            issued_at_ns: 200,
            expires_at_ns: None,
            signature: [0u8; 64],
        };
        sign_device_cert(&mut cert, &self.user_sk).unwrap();
        TestDevice { sk: dev_sk, cert }
    }
}

/// A "device" — signing key + signed cert.
pub struct TestDevice {
    pub sk: SigningKey,
    pub cert: DeviceCert,
}

impl TestDevice {
    pub fn device_pubkey(&self) -> [u8; 32] {
        self.cert.device_public_key
    }
}

/// Spin up a relay bound to `workspace_ids`. Every listed
/// workspace is created pinned + with `members` as the membership
/// set.
pub struct TestRelay {
    pub relay: Relay,
    pub state_root: PathBuf,
    #[allow(
        dead_code,
        reason = "keeps the temporary state root alive for the relay's lifetime"
    )]
    tmp: TempDir,
}

pub async fn spawn_relay(
    user: &TestUser,
    relay_dev: TestDevice,
    workspaces: &[u64],
    members: &[[u8; 32]],
) -> TestRelay {
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let relay = spawn_relay_on(&state_root, user, relay_dev, workspaces, members).await;
    TestRelay {
        relay,
        state_root,
        tmp,
    }
}

/// Start a relay bound to `workspaces` (all pinned) with `members`,
/// persisting to a caller-owned `state_root`. Unlike [`spawn_relay`]
/// the caller keeps the directory alive, so it can drop the returned
/// `Relay` and call this again on the same `state_root` to model a
/// host that restarts — the on-disk pending queues + iroh secret
/// survive the restart. The relay device cert is minted fresh on
/// each call (only its *user* identity must be stable), mirroring a
/// host that re-enrols a device key on reinstall.
pub async fn spawn_relay_on(
    state_root: &std::path::Path,
    user: &TestUser,
    relay_dev: TestDevice,
    workspaces: &[u64],
    members: &[[u8; 32]],
) -> Relay {
    spawn_relay_with(state_root, user, relay_dev, workspaces, members, None).await
}

/// [`spawn_relay`] for a host that serves apps through `plane` and asks to
/// advertise `app_serving`.
pub async fn spawn_relay_serving_apps(
    user: &TestUser,
    relay_dev: TestDevice,
    workspaces: &[u64],
    members: &[[u8; 32]],
    plane: std::sync::Arc<dyn tilo_relay::AppServingPlane>,
) -> TestRelay {
    let tmp = tempfile::tempdir().unwrap();
    let state_root = tmp.path().to_path_buf();
    let relay = spawn_relay_with(
        &state_root,
        user,
        relay_dev,
        workspaces,
        members,
        Some(plane),
    )
    .await;
    TestRelay {
        relay,
        state_root,
        tmp,
    }
}

async fn spawn_relay_with(
    state_root: &std::path::Path,
    user: &TestUser,
    relay_dev: TestDevice,
    workspaces: &[u64],
    members: &[[u8; 32]],
    plane: Option<std::sync::Arc<dyn tilo_relay::AppServingPlane>>,
) -> Relay {
    let mut cfg = RelayConfig {
        state_root: state_root.to_path_buf(),
        ..Default::default()
    };
    for id in workspaces {
        cfg.workspaces.push(WorkspaceBinding {
            id: *id,
            pin: true,
            replication: tilo_relay::ReplicationIntent::FullWorkspace,
            admitted_users: Vec::new(),
        });
    }
    let identity = RelayIdentity {
        user_public_key: user.user_pk,
        device_signing_key: relay_dev.sk.clone(),
        device_cert: relay_dev.cert.clone(),
        revocations: RevocationList::new(),
    };
    cfg.services.app_serving = plane.is_some();
    let relay = Relay::start_with_app_serving(cfg, identity, plane)
        .await
        .unwrap();
    for id in workspaces {
        relay
            .set_workspace_membership(*id, true, members.to_vec(), state_root.to_path_buf())
            .await
            .unwrap();
    }
    relay
}

/// Bind a plain iroh endpoint the tests can use to dial a relay.
pub async fn client_endpoint() -> Endpoint {
    Endpoint::builder(presets::N0).bind().await.unwrap()
}

/// Poll `cond` up to `attempts * 20ms` waiting for it to return
/// true.
pub async fn wait_for(mut cond: impl FnMut() -> bool, attempts: u32) -> bool {
    for _ in 0..attempts {
        if cond() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    cond()
}

#[allow(
    dead_code,
    reason = "names RelayHandle so the fixture import stays exercised"
)]
pub fn _keep_symbols_used(_h: &RelayHandle) {}
