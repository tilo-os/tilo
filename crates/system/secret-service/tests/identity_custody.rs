//! Where a person's identity signing key and a sealed vault's key end up on a
//! real session: the production `runtime-identity` and `secret-vault` code,
//! not their test builds (which always choose files), against the built
//! `tilo-secret-service` on a private session bus.
//!
//! The four states a session can meet: no Secret Service on the bus, one that
//! answers, one that is locked (its vault belongs to another device
//! identity, as when the device key is unavailable), and the same service
//! restarted under a client that keeps running. Each is asserted on what the
//! code records and reports (the identity record's secret reference, the
//! `KeyProtection` Settings shows, the files on disk), and a locked store
//! must never cost an existing key.
//!
//! The bus is a `dbus-daemon` with its own configuration: no service
//! directories, so nothing but the service this test starts can own
//! `org.freedesktop.secrets`. The tools must exist: a check that cannot
//! observe its subject fails (CI installs them with
//! `tools/ci/ensure-session-bus-tools.sh`).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io::BufRead;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ed25519_dalek::{Signature, SigningKey, Verifier};
use runtime_identity::KeyProtection;
use secret_vault::Custody;

const SERVICE: &str = env!("CARGO_BIN_EXE_tilo-secret-service");
const NAME: &str = "org.freedesktop.secrets";

/// A private session bus with no activatable services, and the
/// directories the service run on it uses.
struct Bus {
    root: tempfile::TempDir,
    daemon: Child,
    address: String,
    service: Option<Child>,
}

impl Bus {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        for directory in ["run", "home", "data", "device-a", "device-b", "identities"] {
            let path = root.path().join(directory);
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let config = root.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                root.path().join("run/bus").display()
            ),
        )
        .unwrap();
        let mut daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect(
                "this test needs dbus-daemon (tools/ci/ensure-session-bus-tools.sh installs it)",
            );
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        assert!(!address.trim().is_empty(), "dbus-daemon printed no address");
        Self {
            root,
            daemon,
            address: address.trim().to_owned(),
            service: None,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("DBUS_SESSION_BUS_ADDRESS", &self.address)
            .env("XDG_RUNTIME_DIR", self.path("run"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("HOME", self.path("home"))
            .env("LC_ALL", "C.UTF-8");
        command
    }

    fn owned(&self) -> bool {
        self.command("busctl")
            .args(["--user", "--no-pager", "status", NAME])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("this test needs busctl (systemd)")
            .success()
    }

    /// Serve the person's store with the device identity under `device`.
    fn start_service(&mut self, device: &str) {
        assert!(self.service.is_none(), "a service is already running");
        let service = self
            .command(SERVICE)
            .args(["serve", "--identity-root"])
            .arg(self.path(device))
            .stdout(Stdio::null())
            .spawn()
            .expect("the service binary starts");
        self.service = Some(service);
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.owned() {
            assert!(
                Instant::now() < deadline,
                "the service never claimed {NAME}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn stop_service(&mut self) {
        if let Some(mut service) = self.service.take() {
            let _ = service.kill();
            let _ = service.wait();
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while self.owned() {
            assert!(Instant::now() < deadline, "{NAME} outlived its service");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// What `tilo-secret-service status` says of the store: its first line,
    /// `open` or `locked: <why>` (a second names any earlier import).
    fn status(&self) -> String {
        let output = self
            .command(SERVICE)
            .arg("status")
            .output()
            .expect("the service binary runs");
        assert!(
            output.status.success(),
            "status failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        if let Some(mut service) = self.service.take() {
            let _ = service.kill();
            let _ = service.wait();
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// A complete schema-1 identity record whose key is `[seed; 32]`, as
/// onboarding hands it to `import_identity`.
fn legacy_record(name: &str, seed: u8) -> Vec<u8> {
    let key = SigningKey::from_bytes(&[seed; 32]);
    let secret = [seed; 32].to_vec();
    serde_json::to_vec(&serde_json::json!({
        "display_name": name,
        "avatar_color": [10, 20, 30],
        "threshold_identity": {
            "single_device": true,
            "group_public_key": key.verifying_key().to_bytes(),
            "threshold": 1_u16,
            "total_shares": 1_u16,
            "share_index": 1_u16,
            "secret_bytes": secret,
        },
        "created_at": "2026-10-14T00:00:00Z",
        "schema_version": 1,
    }))
    .unwrap()
}

fn locator(seed: u8) -> String {
    hex::encode(
        SigningKey::from_bytes(&[seed; 32])
            .verifying_key()
            .to_bytes(),
    )
}

fn protection(identities: &Path, id: &str) -> KeyProtection {
    runtime_identity::list_identities(identities)
        .unwrap()
        .into_iter()
        .find(|summary| summary.id == id)
        .unwrap_or_else(|| panic!("{id} is not listed"))
        .protection
}

/// The record on disk names `backend` and holds no secret byte.
fn assert_record_names(record: &Path, backend: &str, seed: u8) {
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(record).unwrap()).unwrap();
    assert_eq!(json["schema_version"], 2, "{json}");
    assert_eq!(json["secret_ref"]["backend"], backend, "{json}");
    assert_eq!(json["secret_ref"]["locator"], locator(seed), "{json}");
    let secret = &json["threshold_identity"]["secret_bytes"];
    assert!(
        secret.is_null() || secret.as_array().is_some_and(Vec::is_empty),
        "the record still carries its key: {json}"
    );
}

fn key_file(identities: &Path, seed: u8) -> PathBuf {
    identities
        .join("secrets")
        .join(format!("{}.key", locator(seed)))
}

/// The key file fallback: the key alone, mode 0600, below a 0700 directory.
fn assert_private_key_file(identities: &Path, seed: u8) {
    let file = key_file(identities, seed);
    assert_eq!(std::fs::read(&file).unwrap(), [seed; 32]);
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&file), 0o600, "{}", file.display());
    assert_eq!(mode(&identities.join("secrets")), 0o700);
}

/// A host-side custody read retrieves the stored key and signs with it.
/// App launches read only public metadata, never the signing key.
fn assert_stored_key_signs(record: &Path, seed: u8) {
    let identity = runtime_identity::load_from_path(record.to_path_buf())
        .unwrap_or_else(|error| panic!("loading stored key from {}: {error}", record.display()));
    let message = b"identity custody qualification";
    let signature = identity.sign(message).unwrap();
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .verify(message, &Signature::from_bytes(&signature))
        .unwrap();
}

#[test]
fn identity_keys_prefer_the_secret_service_and_fall_back_truthfully_without_losing_a_key() {
    let mut bus = Bus::start();
    let identities = bus.path("identities");
    // This process is the client: the production `tilo-secrets` backend
    // reads the session bus address from the environment.
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("DBUS_SESSION_BUS_ADDRESS", &bus.address);

    // 1. No Secret Service answers: the key goes to the private file, and
    //    the record and Settings say so.
    let absent =
        runtime_identity::import_identity(&identities, "absent", &legacy_record("A", 1)).unwrap();
    assert_eq!(protection(&identities, "absent"), KeyProtection::BrokerFile);
    assert_record_names(&absent, "broker-file-v1", 1);
    assert_private_key_file(&identities, 1);
    assert_stored_key_signs(&absent, 1);
    assert_eq!(Custody::for_session(), Custody::LocalFile);
    assert_eq!(
        Custody::LocalFile.label(),
        "Key file on this device, no keyring"
    );

    // 2. The service answers and is open: it is preferred, and no key file
    //    is written.
    bus.start_service("device-a");
    assert_eq!(bus.status(), "open");
    let keyring =
        runtime_identity::import_identity(&identities, "keyring", &legacy_record("K", 2)).unwrap();
    assert_eq!(
        protection(&identities, "keyring"),
        KeyProtection::SecretService
    );
    assert_eq!(
        KeyProtection::SecretService.label(),
        "Secret Service + app-scoped signing broker"
    );
    assert_record_names(&keyring, "secret-service-v1", 2);
    assert!(
        !key_file(&identities, 2).exists(),
        "a key the Secret Service holds was also written to a file"
    );
    assert_stored_key_signs(&keyring, 2);
    let launch_metadata = runtime_identity::load_metadata_for_launch(&keyring).unwrap();
    let vault = bus.path("data/tilo/secrets/vault.tseal");
    let sealed = std::fs::read(&vault).unwrap();
    // The item's value is the key in hex, as `secret-tool` stored it.
    let hex_key = hex::encode([2_u8; 32]);
    assert!(
        !sealed.windows(32).any(|window| window == [2_u8; 32])
            && !sealed
                .windows(hex_key.len())
                .any(|window| window == hex_key.as_bytes()),
        "the vault holds the key in the clear"
    );
    assert_eq!(Custody::for_session(), Custody::System);

    // 3. The same service restarted under this still-running client: the
    //    next call reopens the forgotten session instead of failing.
    bus.stop_service();
    bus.start_service("device-a");
    assert_stored_key_signs(&keyring, 2);

    // 4. Locked: the store's vault belongs to another device identity. The
    //    secured key stays where it is, unreadable but untouched; a new key
    //    falls back to the file and is reported as such.
    bus.stop_service();
    bus.start_service("device-b");
    assert!(bus.status().starts_with("locked"), "{}", bus.status());
    let record_before = std::fs::read(&keyring).unwrap();
    let sealed_before = std::fs::read(&vault).unwrap();
    assert!(
        runtime_identity::load_from_path(keyring.clone()).is_err(),
        "a locked store released a key"
    );
    // Launch validation must not read the key, even while its store is locked.
    assert_eq!(
        runtime_identity::load_metadata_for_launch(&keyring).unwrap(),
        launch_metadata
    );
    assert!(
        runtime_identity::secure_identity(&identities, "keyring").is_err(),
        "securing a record whose store is locked claimed success"
    );
    let locked =
        runtime_identity::import_identity(&identities, "locked", &legacy_record("L", 3)).unwrap();
    assert_eq!(protection(&identities, "locked"), KeyProtection::BrokerFile);
    assert_record_names(&locked, "broker-file-v1", 3);
    assert_private_key_file(&identities, 3);
    assert_eq!(std::fs::read(&keyring).unwrap(), record_before);
    assert_eq!(
        protection(&identities, "keyring"),
        KeyProtection::SecretService
    );
    assert!(
        !key_file(&identities, 2).exists(),
        "the locked key was re-keyed to a file"
    );
    assert_eq!(std::fs::read(&vault).unwrap(), sealed_before);

    // 5. The device identity is back: the original key opens again.
    bus.stop_service();
    bus.start_service("device-a");
    assert_eq!(bus.status(), "open");
    assert_stored_key_signs(&keyring, 2);
    assert_stored_key_signs(&locked, 3);

    // 6. A damaged record fails its launch by name and is never re-keyed.
    let truncated = identities.join("truncated.json");
    let prefix = record_before.get(..record_before.len() / 2).unwrap();
    std::fs::write(&truncated, prefix).unwrap();
    std::fs::set_permissions(&truncated, std::fs::Permissions::from_mode(0o600)).unwrap();
    let Err(error) = runtime_identity::load_metadata_for_launch(&truncated) else {
        panic!("a truncated record launched");
    };
    assert!(
        error.to_string().contains("identity JSON malformed"),
        "the launch error does not name the damage: {error}"
    );
    assert_eq!(std::fs::read(&truncated).unwrap(), prefix);
    assert_stored_key_signs(&keyring, 2);

    drop(env);
}
