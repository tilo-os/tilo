//! Test fixtures shared across the integration suite.

#![allow(dead_code)]
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
        reason = "Holding the temporary directory preserves relay state until the test handle is dropped."
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
    if std::env::var_os("QOS_RELAY_CORRELATED_DIAGNOSTICS").is_some() {
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
    let relay = Relay::start(cfg, identity).await.unwrap();
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
    reason = "The shared fixture module is included by integration targets with different helper requirements."
)]
pub fn _keep_symbols_used(_h: &RelayHandle) {}

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
