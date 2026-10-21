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

fn service() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-secret-service")
}
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
        for directory in [
            "run",
            "home",
            "data",
            "device-a",
            "device-b",
            "device-other",
            "identities",
        ] {
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
        let command = self.service_command(device);
        self.spawn_service(command);
    }

    fn service_command(&self, device: &str) -> Command {
        let mut command = self.command(service());
        command
            .args(["serve", "--identity-root"])
            .arg(self.path(device))
            .stdout(Stdio::null());
        command
    }

    fn spawn_service(&mut self, mut command: Command) {
        assert!(self.service.is_none(), "a service is already running");
        self.service = Some(command.spawn().expect("the service binary starts"));
        self.wait_for_owner();
    }

    /// Until something on the bus owns the Secret Service's name.
    fn wait_for_owner(&self) {
        let deadline = tilo_test_env::hang_deadline();
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
        let deadline = tilo_test_env::hang_deadline();
        while self.owned() {
            assert!(Instant::now() < deadline, "{NAME} outlived its service");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// What `tilo-secret-service status` says of the store: its first line,
    /// `open` or `locked: <why>` (a second names any earlier import).
    fn status(&self) -> String {
        let output = self
            .command(service())
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
    // App launch needs only public metadata; custody must still refuse the
    // identity host's attempt to retrieve the locked signing key.
    let metadata = runtime_identity::load_metadata_for_launch(&keyring)
        .expect("a locked provider must not block public launch metadata");
    assert_eq!(
        metadata.group_public_key,
        SigningKey::from_bytes(&[2; 32]).verifying_key().to_bytes()
    );
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

    // 6. A damaged record fails loading by name and is never re-keyed.
    let truncated = identities.join("truncated.json");
    let prefix = record_before.get(..record_before.len() / 2).unwrap();
    std::fs::write(&truncated, prefix).unwrap();
    std::fs::set_permissions(&truncated, std::fs::Permissions::from_mode(0o600)).unwrap();
    let Err(error) = runtime_identity::load_metadata_for_launch(&truncated) else {
        panic!("a truncated record launched");
    };
    assert!(
        error.to_string().contains("identity JSON malformed"),
        "the loading error does not name the damage: {error}"
    );
    assert_eq!(std::fs::read(&truncated).unwrap(), prefix);
    assert_stored_key_signs(&keyring, 2);

    drop(env);
}

// ---- Callers the store cannot examine -------------------------------------
//
// Loading the device key makes a process non-dumpable, and the kernel then
// refuses its `/proc/<pid>/exe` to the store. The compositor is such a
// process. These tests run the production client code in child processes of
// this test binary (`custody_child`), so only a child turns non-dumpable.

/// Which client a child process of this test is, and what it is given.
#[cfg(target_os = "linux")]
const CHILD: &str = "TILO_CUSTODY_CHILD";
#[cfg(target_os = "linux")]
const CHILD_DEVICE: &str = "TILO_CUSTODY_DEVICE";
#[cfg(target_os = "linux")]
const CHILD_IDENTITIES: &str = "TILO_CUSTODY_IDENTITIES";
#[cfg(target_os = "linux")]
const CHILD_RUN: &str = "TILO_CUSTODY_RUN";
#[cfg(target_os = "linux")]
const CHILD_TEST: &str = "custody_child";

/// Run without capabilities, as the service's unit does
/// (`CapabilityBoundingSet=`) and as a person's session processes do. Used
/// as a `pre_exec` hook, so the whole process starts without them even when
/// the test runs as root: a CI container's root that kept `CAP_SYS_PTRACE`
/// could otherwise read the very link the kernel refuses a person's own
/// processes, and a store without capabilities may not examine a process
/// that holds some, whatever its dumpability. Either would make the
/// refusals below observe something else.
#[cfg(target_os = "linux")]
fn drop_capabilities() -> std::io::Result<()> {
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    for capability in 0..64 {
        // SAFETY: scalar arguments only. Without CAP_SETPCAP (any process
        // but root) or past the last capability this fails harmlessly:
        // there is then nothing in the bounding set to drop.
        unsafe { libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) };
    }
    let header = Header {
        version: 0x2008_0522,
        pid: 0,
    };
    let none = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    // SAFETY: capset(2) reads one version-3 header and two data words, both
    // alive for the call.
    if unsafe { libc::syscall(libc::SYS_capset, &raw const header, none.as_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn dumpable() -> bool {
    // SAFETY: PR_GET_DUMPABLE takes no pointers.
    unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) == 1 }
}

/// Turn non-dumpable without holding any device key.
#[cfg(target_os = "linux")]
fn turn_non_dumpable() {
    // SAFETY: PR_SET_DUMPABLE takes scalar arguments only.
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0);
}

/// Read the device key under `root` as the compositor does at startup,
/// which leaves this process non-dumpable.
#[cfg(target_os = "linux")]
fn load_device_key_like_the_compositor(root: &Path) {
    tilo_user_identity::persist::load_or_create_device_key(
        &tilo_user_identity::persist::IdentityPaths::under(root),
    )
    .unwrap();
    assert!(
        !dumpable(),
        "loading the device key left the process dumpable"
    );
}

#[cfg(target_os = "linux")]
fn child_env(name: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(name).unwrap_or_else(|| panic!("{name} is not set")))
}

/// Wait until `path` exists.
#[cfg(target_os = "linux")]
fn wait_for_file(path: &Path) {
    let deadline = tilo_test_env::hang_deadline();
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "linux")]
fn block_on<T>(work: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(work)
}

#[cfg(target_os = "linux")]
async fn sandbox_secret(connection: &zbus::Connection) -> zbus::Result<Vec<u8>> {
    let store = zbus::Proxy::new(connection, NAME, "/so/tilo/Secrets1", "so.tilo.Secrets1").await?;
    store
        .call("SandboxSecret", &("so.tilo.notes", Vec::<u8>::new()))
        .await
}

#[cfg(target_os = "linux")]
async fn session_connection() -> zbus::Connection {
    zbus::Connection::session().await.unwrap()
}

#[cfg(target_os = "linux")]
fn assert_access_denied<T: std::fmt::Debug>(answer: zbus::Result<T>) {
    match answer {
        Err(zbus::Error::MethodError(name, _, _))
            if name.as_str() == "org.freedesktop.Secret.Error.AccessDenied" => {}
        other => panic!("expected the store to refuse, got {other:?}"),
    }
}

/// The store's library served in-process with this test binary as the
/// portal host, which no spawned `tilo-secret-service` can be given: the
/// management interface's refusal and admission of a caller the kernel
/// will not show are then observable without the compositor's own binary.
#[cfg(target_os = "linux")]
fn serve_with_this_binary_as_portal_host() {
    use std::sync::Arc;
    use tilo_secret_service::app::AppId;
    use tilo_secret_service::service::{BUS_NAME, BusResolver, Core, Managers, publish};
    use tilo_secret_service::store::{DeviceIdentityKey, Store};
    let device = child_env(CHILD_DEVICE);
    let run = child_env(CHILD_RUN);
    let paths = tilo_user_identity::persist::IdentityPaths::under(&device);
    tilo_user_identity::persist::load_or_create_device_key(&paths).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let keys = DeviceIdentityKey::new(paths, run.join("salt"), &uid.to_string());
    let store = Arc::new(Store::new(run.join("vault.tseal"), Box::new(keys)));
    assert!(store.locked().is_none(), "the store did not open");
    let exe = std::fs::read_link("/proc/self/exe").unwrap();
    let host = AppId::parse(&format!("local-process:{}", exe.display())).unwrap();
    block_on(async move {
        let connection = zbus::Connection::session().await.unwrap();
        let core = Core::new(
            store,
            Arc::new(BusResolver::new(connection.clone(), uid)),
            Managers {
                settings: Vec::new(),
                portal: vec![host],
            },
        );
        publish(&connection, &core).await.unwrap();
        connection.request_name(BUS_NAME).await.unwrap();
        std::future::pending::<()>().await;
    });
}

/// A child process of this test binary: one client the tests below name by
/// `CHILD`. Run as an ordinary test it is no child and has nothing to do.
#[cfg(target_os = "linux")]
#[test]
fn custody_child() {
    let Some(role) = std::env::var_os(CHILD) else {
        return;
    };
    match &*role.to_string_lossy() {
        // The control: dumpable, so the store examines it by its executable
        // and keeps its key. A refusal of the others is then theirs alone.
        "dumpable-import" => {
            assert!(dumpable(), "the control child is not dumpable");
            runtime_identity::import_identity(
                &child_env(CHILD_IDENTITIES),
                "dumpable",
                &legacy_record("C", 7),
            )
            .unwrap();
        }
        // Non-dumpable without the device key: the store cannot examine it,
        // and it has nothing to prove with.
        "unproven-import" => {
            turn_non_dumpable();
            runtime_identity::import_identity(
                &child_env(CHILD_IDENTITIES),
                "unproven",
                &legacy_record("U", 4),
            )
            .unwrap();
        }
        // Non-dumpable after reading another machine's device key: its proof
        // is refused.
        "other-device-import" => {
            load_device_key_like_the_compositor(&child_env(CHILD_DEVICE));
            runtime_identity::import_identity(
                &child_env(CHILD_IDENTITIES),
                "other-device",
                &legacy_record("O", 5),
            )
            .unwrap();
        }
        // The compositor's case: non-dumpable because it read this machine's
        // device key. It stores and reads its identity key in the store, and
        // keeps reading it after the store restarts under it.
        "device-import" => {
            load_device_key_like_the_compositor(&child_env(CHILD_DEVICE));
            let record = runtime_identity::import_identity(
                &child_env(CHILD_IDENTITIES),
                "device",
                &legacy_record("D", 6),
            )
            .unwrap();
            assert_stored_key_signs(&record, 6);
            let run = child_env(CHILD_RUN);
            std::fs::write(run.join("child-imported"), b"").unwrap();
            wait_for_file(&run.join("service-restarted"));
            assert_stored_key_signs(&record, 6);
            assert!(!dumpable(), "the client turned dumpable again");
        }
        "serve" => serve_with_this_binary_as_portal_host(),
        // The control: dumpable, so the store names it by its executable,
        // the portal host's, and answers without a proof.
        "manager-dumpable" => block_on(async {
            assert!(dumpable(), "the control child is not dumpable");
            let connection = session_connection().await;
            let proved = tilo_secrets::prove_device_caller(&connection).await;
            assert!(!proved.unwrap(), "a dumpable client proved");
            assert_eq!(sandbox_secret(&connection).await.unwrap().len(), 32);
        }),
        "manager-unproven" => block_on(async {
            turn_non_dumpable();
            let connection = session_connection().await;
            let proved = tilo_secrets::prove_device_caller(&connection).await;
            assert!(!proved.unwrap(), "proved without a key");
            assert_access_denied(sandbox_secret(&connection).await);
        }),
        "manager-other-device" => block_on(async {
            load_device_key_like_the_compositor(&child_env(CHILD_DEVICE));
            let connection = session_connection().await;
            assert_access_denied(tilo_secrets::prove_device_caller(&connection).await);
            assert_access_denied(sandbox_secret(&connection).await);
        }),
        "manager-device" => block_on(async {
            load_device_key_like_the_compositor(&child_env(CHILD_DEVICE));
            let connection = session_connection().await;
            // Refused until this connection proves the device identity, as
            // the compositor's portal backend first meets it.
            assert_access_denied(sandbox_secret(&connection).await);
            let proved = tilo_secrets::prove_device_caller(&connection).await;
            assert!(proved.unwrap(), "the proof was not made");
            let secret = sandbox_secret(&connection).await.unwrap();
            assert_eq!(secret.len(), 32);
            // The proof holds for the connection: no second one is needed.
            assert_eq!(sandbox_secret(&connection).await.unwrap(), secret);
            // It names that connection only: another one proves on its own.
            let other = session_connection().await;
            assert_access_denied(sandbox_secret(&other).await);
            assert!(!dumpable(), "the client turned dumpable again");
        }),
        other => panic!("unknown custody child {other}"),
    }
}

#[cfg(target_os = "linux")]
impl Bus {
    /// Start this test binary again as the child `role`.
    fn child(&self, role: &str, device: &str) -> Child {
        use std::os::unix::process::CommandExt as _;
        let mut command = self.command(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD_TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, role)
            .env(CHILD_DEVICE, self.path(device))
            .env(CHILD_IDENTITIES, self.path("identities"))
            .env(CHILD_RUN, self.path("run"))
            .stdout(Stdio::null());
        // SAFETY: the hook makes only async-signal-safe system calls.
        unsafe { command.pre_exec(drop_capabilities) };
        command
            .spawn()
            .expect("this test binary starts again as a child")
    }

    /// Serve the store with the device identity under `device`, without
    /// capabilities, as its unit runs it.
    fn start_service_without_capabilities(&mut self, device: &str) {
        use std::os::unix::process::CommandExt as _;
        let mut command = self.service_command(device);
        // SAFETY: the hook makes only async-signal-safe system calls.
        unsafe { command.pre_exec(drop_capabilities) };
        self.spawn_service(command);
    }

    /// Run the child `role` to its end; it asserts its own part.
    fn run_child(&self, role: &str, device: &str) {
        let status = self.child(role, device).wait().unwrap();
        assert!(status.success(), "custody child {role} failed: {status}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_client_holding_the_device_key_keeps_its_identity_key_out_of_files_though_the_store_cannot_examine_it()
 {
    let mut bus = Bus::start();
    let identities = bus.path("identities");
    bus.start_service_without_capabilities("device-a");
    assert_eq!(bus.status(), "open");

    // 0. The control: a dumpable client is examined and its key kept in the
    //    store. Every refusal below is therefore the kernel refusing a
    //    non-dumpable process, nothing else about these children.
    bus.run_child("dumpable-import", "device-a");
    assert_record_names(&identities.join("dumpable.json"), "secret-service-v1", 7);
    assert!(!key_file(&identities, 7).exists());

    // 1. Non-dumpable with no device key: the store cannot examine the
    //    client and refuses it, and the key goes to the private file as
    //    before. With the control above, this is what shows the store
    //    really cannot read a non-dumpable child's executable here: had it
    //    read it, the key would be in the store.
    bus.run_child("unproven-import", "device-a");
    assert_record_names(&identities.join("unproven.json"), "broker-file-v1", 4);
    assert_private_key_file(&identities, 4);

    // 2. Non-dumpable after reading another device's key: the proof is
    //    refused, and the key goes to the file.
    bus.run_child("other-device-import", "device-other");
    assert_record_names(&identities.join("other-device.json"), "broker-file-v1", 5);
    assert_private_key_file(&identities, 5);

    // 3. Non-dumpable after reading this device's key, as the compositor
    //    is: the store holds the key, and no file does.
    let mut child = bus.child("device-import", "device-a");
    wait_for_file(&bus.path("run/child-imported"));
    assert_record_names(&identities.join("device.json"), "secret-service-v1", 6);
    assert_eq!(
        protection(&identities, "device"),
        KeyProtection::SecretService
    );
    assert!(
        !key_file(&identities, 6).exists(),
        "a key the Secret Service holds was also written to a file"
    );

    // 4. The store restarts under the still-running client and forgets
    //    every proof: the client proves again and reads its key.
    bus.stop_service();
    bus.start_service_without_capabilities("device-a");
    std::fs::write(bus.path("run/service-restarted"), b"").unwrap();
    let status = child.wait().unwrap();
    assert!(
        status.success(),
        "custody child device-import failed: {status}"
    );
    assert!(!key_file(&identities, 6).exists());
}

#[cfg(target_os = "linux")]
#[test]
fn a_non_dumpable_portal_host_reaches_the_management_interface_only_by_proof() {
    let bus = Bus::start();
    let mut service = bus.child("serve", "device-a");
    bus.wait_for_owner();
    for (role, device) in [
        ("manager-dumpable", "device-a"),
        ("manager-unproven", "device-a"),
        ("manager-other-device", "device-other"),
        ("manager-device", "device-a"),
    ] {
        bus.run_child(role, device);
    }
    let _ = service.kill();
    let _ = service.wait();
}
