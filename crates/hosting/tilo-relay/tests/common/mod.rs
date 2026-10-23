#![allow(
    dead_code,
    reason = "each integration module uses a different subset of these fixtures"
)]
#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

pub mod compute;

use std::path::PathBuf;

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Endpoint, presets};
use tempfile::TempDir;

use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity, sign_device_cert,
};

use tilo_relay::{
    RelayConfig, WorkspaceBinding,
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
    let relay = Box::pin(spawn_relay_on(
        &state_root,
        user,
        relay_dev,
        workspaces,
        members,
    ))
    .await;
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
    Box::pin(spawn_relay_with(
        state_root, user, relay_dev, workspaces, members, None,
    ))
    .await
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
    let relay = Box::pin(spawn_relay_with(
        &state_root,
        user,
        relay_dev,
        workspaces,
        members,
        Some(plane),
    ))
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
    if std::env::var_os("TILO_RELAY_CORRELATED_DIAGNOSTICS").is_some() {
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter(
                "tilo_relay::peer=info,tilo_relay::client=info,tilo_relay::workspace_io=info",
            )
            .with_writer(|| PhaseWriter)
            .try_init();
    }
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
            shell_authority: None,
            checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
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
    Endpoint::builder(presets::Minimal).bind().await.unwrap()
}

/// Bounds a wait that ends as soon as its condition holds; only a hang reaches it.
pub const HANG_BOUND: std::time::Duration = std::time::Duration::from_secs(30);

/// Wait until `relay` has accepted `peer`'s subscription to `workspace`.
///
/// For a federation link this is the moment from which a delta published on
/// `relay` is forwarded to the relay that dialled it; a delta published
/// earlier is not, so a test waits for it instead of sleeping and hoping the
/// handshake finished in time.
pub async fn wait_until_subscribed(relay: &Relay, workspace: u64, peer: &[u8; 32]) {
    tilo_test_env::within("the federated peer subscribed to the workspace", async {
        while !relay.is_subscribed(workspace, peer).await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// Poll `cond` until it holds, failing the test naming `what` if it never
/// does.
pub async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    tilo_test_env::within(what, async {
        while !cond() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
}

fn phase_bytes() -> &'static std::sync::Mutex<std::collections::VecDeque<u8>> {
    static BUFFER: std::sync::OnceLock<std::sync::Mutex<std::collections::VecDeque<u8>>> =
        std::sync::OnceLock::new();
    BUFFER.get_or_init(|| {
        std::sync::Mutex::new(std::collections::VecDeque::with_capacity(128 * 1024))
    })
}
struct PhaseWriter;
impl std::io::Write for PhaseWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut buffer = phase_bytes().lock().unwrap();
        for byte in bytes {
            if buffer.len() == 128 * 1024 {
                buffer.pop_front();
            }
            buffer.push_back(*byte);
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn print_phase_diagnostic() {
    let bytes = phase_bytes()
        .lock()
        .unwrap()
        .iter()
        .copied()
        .collect::<Vec<_>>();
    eprintln!("{}", String::from_utf8_lossy(&bytes));
}

/// The app surfaces a process decides received changes against
/// (`tilo_peer_state::decide_with_surfaces`) are one per process, and a
/// single-process `cargo test` run shares them across this binary's tests.
/// A test that installs surfaces holds [`install_process_surfaces`]'s guard;
/// one that depends on none holds [`no_process_surfaces`]. Neither runs
/// beside the other, and the surfaces are taken off again when the guard
/// drops.
pub struct ProcessSurfaces {
    _lock: std::sync::MutexGuard<'static, ()>,
}

fn process_surfaces_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Decide every change of this process against `surfaces`, naming `of`'s
/// surface for each app, until the guard drops.
pub fn install_process_surfaces(
    surfaces: collab_types::Surfaces,
    of: impl Fn(&str) -> Option<[u8; 32]> + Send + Sync + 'static,
) -> ProcessSurfaces {
    let lock = process_surfaces_lock();
    tilo_peer_state::decide_with_surfaces(surfaces, of);
    ProcessSurfaces { _lock: lock }
}

/// Run with no process-wide app surfaces installed: changes are decided by
/// their record.
pub fn no_process_surfaces() -> ProcessSurfaces {
    let lock = process_surfaces_lock();
    tilo_peer_state::decide_with_surfaces(collab_types::Surfaces::default(), |_| None);
    ProcessSurfaces { _lock: lock }
}

impl Drop for ProcessSurfaces {
    fn drop(&mut self) {
        tilo_peer_state::decide_with_surfaces(collab_types::Surfaces::default(), |_| None);
    }
}
