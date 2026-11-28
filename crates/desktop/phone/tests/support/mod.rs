//! What a phone test stands on: the phone's home, laid out as Android lays out an app's files,
//! and the person's laptop, which prints the ticket the phone signs in with. A flow that has a
//! phone of the person's join their workspace reads these too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    unused_imports,
    reason = "test support: a failure here is the test's failure; each test uses part of it"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ed25519_dalek::SigningKey;
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};

/// The phone's files: the directories Android hands the app (`getFilesDir()` and its cache), set
/// as the process's environment before the daemon starts, as the phone's launch sets them.
pub struct Home {
    dir: tempfile::TempDir,
    /// The variables the phone's places were set in, put back when the home goes.
    env: tilo_test_env::ScopedEnv,
}

impl Home {
    /// A home of its own for this process's phone.
    ///
    /// # Panics
    /// Called twice in one process: the daemon reads its places from the environment, once.
    pub fn new() -> Self {
        static MADE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        assert!(
            !MADE.swap(true, std::sync::atomic::Ordering::SeqCst),
            "one phone per test process"
        );
        // What the daemon says while the test drives it: the words a failure is read by, kept
        // with the test's own output.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
            )
            .with_test_writer()
            .try_init();
        let dir = tempfile::Builder::new().prefix("phone").tempdir().unwrap();
        let root = dir.path();
        let files = root.join("files");
        let cache = root.join("cache");
        let set = [
            ("HOME", root.to_path_buf()),
            ("XDG_DATA_HOME", files.join("data")),
            ("XDG_CONFIG_HOME", files.join("config")),
            ("XDG_STATE_HOME", files.join("state")),
            ("XDG_CACHE_HOME", cache.clone()),
            ("XDG_RUNTIME_DIR", cache.join("run")),
            ("TILO_IDENTITY_ROOT", files.join("data/tilo")),
        ];
        for (_, path) in &set {
            std::fs::create_dir_all(path).unwrap();
        }
        let mut env = tilo_test_env::ScopedEnv::new();
        for (name, path) in set {
            env.set(name, path);
        }
        // A phone keeps its identity's secrets in its own files: it has no keyring.
        env.set("TILO_IDENTITY_FILE_SECRETS", "1");
        for inherited in [
            "TILO_RUNTIME_PROFILE",
            "TILO_APPS_DATA_DIR",
            "TILO_APPS_SYSTEM_DIR",
            "TILO_AGENT_SOCKET",
            "TILO_API_KEY",
            "TILO_API_URL",
            "CREDENTIALS_DIRECTORY",
        ] {
            env.remove(inherited);
        }
        Self { dir, env }
    }

    /// Point the phone's daemon at `url`, the relay its server answers through: its endpoint
    /// picks that relay at once instead of looking for a public one nothing here serves.
    pub fn relay(&mut self, url: &str) {
        self.env.set("TILO_RELAY", url);
    }
}

/// The daemon's profile, where the phone keeps it.
pub fn profile() -> PathBuf {
    tilo_machine::account_daemon_profile().expect("a home for the daemon's profile")
}

/// A key made from `seed`, so each test's devices are the same devices on every run.
pub fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

/// `key`'s certificate, signed by `root`: a device of the person whose root it is.
pub fn certified(root: &SigningKey, key: &SigningKey, label: &str, primary: bool) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: if primary {
            DeviceCapabilities::primary()
        } else {
            DeviceCapabilities::member()
        },
        issued_at_ns: tilo_clock::unix_now_ns().saturating_sub(1_000_000_000),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, root).unwrap();
    cert
}

/// An endpoint of `key`'s on this machine's loopback, reached directly.
pub async fn endpoint(key: &SigningKey) -> Endpoint {
    Endpoint::builder(presets::Minimal)
        .relay_mode(RelayMode::Disabled)
        .secret_key(SecretKey::from_bytes(&key.to_bytes()))
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap()
}

/// Ask `read` until it answers `Ok`; what it answered last is the failure.
pub fn until<T>(what: &str, mut read: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(60))
        .expect("a minute from now is on the clock");
    loop {
        match read() {
            Ok(value) => return value,
            Err(last) if Instant::now() > deadline => panic!("{what}: {last}"),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// What the phone's view shows first: its device, once the daemon answers what it reads. The
/// person asks the phone to sign in from there.
pub fn shown(phone: &tilo_runtime::in_process::Device) -> tilo_runtime::in_process::Devices {
    until("the phone's device answers what its view reads", || {
        phone.devices().map_err(|error| format!("{error:#}"))
    })
}

/// The person's laptop, serving one ticket: the workspace service's stand-in device, which the
/// phone's tests and the flow's share without reaching into the sync engine.
pub use tilo_runtime::device::issuing::test_support::Issuing;
