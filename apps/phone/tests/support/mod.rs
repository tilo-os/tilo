//! What a phone test stands on: the phone's home, laid out as Android lays out an app's files,
//! and the person's laptop, which prints the ticket the phone signs in with. A flow that has a
//! phone of the person's join their workspace reads these too.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    dead_code,
    reason = "test support: a failure here is the test's failure; each test uses part of it"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer as _, SigningKey};
use iroh::protocol::Router;
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};
use tilo_device_identity::login::{Approval, Approved, Approver, Asked, Entry, Ticket};
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
}

/// The daemon's profile, where the phone keeps it.
pub fn profile() -> PathBuf {
    tilo_phone::profile()
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

/// The person's laptop, serving one ticket: a device that logs in with it is certified by the
/// person's root and enters the person's workspace on their server.
pub struct Issuing {
    ticket: Ticket,
    endpoint: Endpoint,
    router: Router,
    runtime: tokio::runtime::Handle,
}

impl Issuing {
    /// The laptop of the person whose root is `person`, printing a ticket into `entry`, served
    /// on `runtime`.
    pub fn start(runtime: &tokio::runtime::Handle, person: &SigningKey, entry: Entry) -> Self {
        let laptop = key(8);
        let cert = certified(person, &laptop, "the person's laptop", true);
        let endpoint = runtime.block_on(endpoint(&laptop));
        let ticket = Ticket::new(tilo_api::daemon::enter::reached_of(&endpoint.addr()));
        let router = {
            let _entered = runtime.enter();
            serving(
                &endpoint,
                &laptop,
                ticket.clone(),
                person.verifying_key().to_bytes(),
                entry,
                Arc::new(RootSigns {
                    root: person.clone(),
                    device: laptop.clone(),
                    cert,
                }),
            )
        };
        Self {
            ticket,
            endpoint,
            router,
            runtime: runtime.clone(),
        }
    }

    /// The ticket, as the laptop shows it to the person.
    pub fn ticket(&self) -> String {
        self.ticket.encode()
    }
}

impl Drop for Issuing {
    fn drop(&mut self) {
        let router = self.router.clone();
        let endpoint = self.endpoint.clone();
        drop(self.runtime.spawn(async move {
            let _ = router.shutdown().await;
            endpoint.close().await;
        }));
    }
}

/// The laptop approving the device that brings its ticket: its person's root certifies it.
struct RootSigns {
    root: SigningKey,
    device: SigningKey,
    cert: DeviceCert,
}

impl Approver for RootSigns {
    fn approve(&self, asked: Asked) -> Approval {
        let mut cert = asked.cert.clone();
        sign_device_cert(&mut cert, &self.root).unwrap();
        Box::pin(std::future::ready(Ok(Approved {
            cert,
            approver: self.cert.clone(),
            signature: self.device.sign(&asked.digest).to_bytes(),
        })))
    }
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
pub fn shown(phone: &tilo_phone::core::Device) -> tilo_phone::core::Devices {
    until("the phone's device answers what its view reads", || {
        phone.devices().map_err(|error| format!("{error:#}"))
    })
}

/// The laptop's endpoint serving `ticket` as the device that printed it does: a device that logs
/// in with it is certified by `approver` for the person whose root key is `person`, and enters
/// `entry`. What the router serves ends with it. Called inside a runtime context.
fn serving(
    endpoint: &Endpoint,
    key: &SigningKey,
    ticket: Ticket,
    person: [u8; 32],
    entry: Entry,
    approver: Arc<dyn Approver>,
) -> Router {
    let service = tilo_peer_state::actions::ActionService::new(
        key.clone(),
        Arc::new(tilo_peer_state::actions::NoActions),
    );
    service.admit_uncertified(
        tilo_device_identity::login::SCOPE,
        Arc::new(tilo_peer_state::new_device::ServedTicket::new(
            ticket,
            person,
            entry,
            DeviceCapabilities::member(),
            approver,
            tilo_user_identity::RevocationList::new(),
            tilo_clock::unix_now_ns()
                .saturating_add(tilo_device_identity::login::TICKET_LIFETIME_NS),
        )),
    );
    Router::builder(endpoint.clone())
        .accept(tilo_peer_state::actions::ACTIONS_ALPN, service)
        .spawn()
}
