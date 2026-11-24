//! A machine with no session, or one that holds none of the person's root,
//! enrols its volume's sibling unlock by asking the person's devices over the
//! peer network: the one that holds the root answers after its person
//! approved, and a device that holds none says so and the next is asked. At
//! boot, the half that device keeps opens the volume.
//!
//! The enrolling machine is a real device: its daemon (`tilo-runtime`) runs in
//! this test's process, keeps the device key and the person's devices record,
//! and dials the person's devices on its own endpoint. The enrolment reads
//! neither the key nor an endpoint of its own; it asks the daemon, as
//! `tilo-auth sibling enrol` does. The answering devices are the other side of
//! the peer network on a loopback relay: what an answering device does with the
//! request is the session's
//! (`crates/desktop/session/tests/network_unlock_enrolment.rs` proves that).

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "a failed unwrap or index in a test is its assertion, and a journey reads as one flow"
)]

use std::net::Ipv4Addr;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use iroh::endpoint::{Connection, Endpoint, RecvStream, SendStream, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{RelayMap, RelayMode};
use settings_store::sign_in_comforts::SignInComforts;
use tilo_api::daemon::DaemonPlace;
use tilo_auth::boot::sibling::{self, Outcome, Refusal, Siblings, Volume};
use tilo_auth::luks;
use tilo_auth::sibling::enrol::{self, Args, Machine, NotEnrolled, PeerRegistrar, run_with_peers};
use tilo_auth::sibling::material::{Material, Network};
use tilo_auth::sibling::token::{self, TOKEN_VERSION, Token};
use tilo_auth::tpm::{self, Sealing};
use tilo_identity::store::SigningIdentity;
use tilo_rollback::exec::Real;
use tilo_rollback::policy::Workspace;
use tilo_runtime::RuntimeServices as _;
use tilo_runtime::device::services::Daemon;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_test_env::require_capability;
use tilo_test_env::swtpm::Swtpm;
use tilo_unlock_protocol::{
    BootId, ENROL_ALPN, EnrolReply, EnrolRequest, EnrolResponse, HALF_LEN, Half, RefusalReason,
    SiblingContact, SlotId, decode_frame, encode_frame, frame_len,
};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, StatementSigner as _, sign_device_cert, verify_device_cert,
};

// The daemon opens its documents in a worker process, which this test binary serves.
tilo_automerge_isolate::test_worker!();

/// Names the test the process [`on_own_device`] starts runs, and where its daemon's profile is.
const OWN: &str = "TILO_AUTH_PEER_ENROL_TEST";
const PROFILE: &str = "TILO_AUTH_PEER_ENROL_PROFILE";

/// Run `body` on a device of its own: in a process of its own, with a home, identity store and
/// runtime directory of its own, and its person founded there. `body` is handed the daemon's
/// profile. A device is one per process, as a computer has one daemon.
fn on_own_device(body: impl FnOnce(&Path)) {
    let test = std::thread::current()
        .name()
        .expect("libtest names a test's thread after the test")
        .to_owned();
    if std::env::var(OWN).ok().as_deref() != Some(test.as_str()) {
        own_process(&test);
        return;
    }
    // The daemon's errors go to this process's error output, which the test's own log keeps;
    // `RUST_LOG` adds to it.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "warn".to_owned()),
        ))
        .with_writer(std::io::stderr)
        .try_init();
    let profile = PathBuf::from(std::env::var_os(PROFILE).expect("the profile is named"));
    tilo_runtime::device::services::found_account_person(
        &tilo_identity::provisioning::collaboration_identity_paths(),
        "",
        tilo_runtime::device::services::this_account_uid(),
    )
    .expect("the person is founded on this device");
    body(&profile);
}

/// Keep `certs` in the person's devices record, as the daemon of the device on `profile` keeps a
/// key their root certified there, and stop it again: what the device keeps from then on.
fn keep_devices(profile: &Path, certs: &[&DeviceCert]) {
    let services = Daemon::start_reporting(profile, &mut |_| {}).expect("the daemon starts");
    // The person's own workspace, which keeps their devices record, is founded when first asked
    // for, as their own `tilo` asks for it.
    services
        .workspace()
        .expect("the person's workspace, founded when first asked for");
    let keeping = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for cert in certs {
        keeping.block_on(
            services
                .workspaces()
                .keep_issued_device(cert, &cert.device_label),
        );
    }
    drop(keeping);
    // The daemon stops outside any runtime, as a computer stops it.
    drop(services);
}

/// Run `test` again, alone, in a process whose home, identity, runtime directory and profile are
/// a new directory's, relaying through a packet relay on this machine's loopback.
fn own_process(test: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    // A short path: the daemon's sockets are named below it.
    let root = tempfile::Builder::new()
        .prefix("tilo-a-")
        .tempdir_in(std::fs::canonicalize(std::env::temp_dir()).unwrap())
        .unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let place = |name: &str| {
        let dir = root.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        dir
    };
    // The daemon and the person's devices relay through it: nothing reaches a relay or a
    // directory out on the internet.
    let relay = tokio::runtime::Runtime::new().expect("a runtime for the relay");
    let server = relay.block_on(tilo_test_relay::relay());
    let log = root.path().join("out");
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture", "--test-threads", "1"])
        .env(OWN, test)
        .env(PROFILE, place("p"))
        // The daemon's profile is named apart from the account's own, as a test's or a VM host's
        // is: it keeps its sockets there, and every client of this computer finds them there
        // (`tilo_paths::machine::DAEMON_PROFILE_VAR`).
        .env("TILO_RUNTIME_PROFILE", place("p"))
        .env("HOME", place("h"))
        .env("XDG_CONFIG_HOME", place("c"))
        .env("XDG_DATA_HOME", place("d"))
        .env("XDG_CACHE_HOME", place("k"))
        .env("XDG_STATE_HOME", place("s"))
        .env("XDG_RUNTIME_DIR", place("r"))
        .env("TILO_IDENTITY_ROOT", place("i"))
        .env(tilo_identity::provisioning::FILE_SECRETS_ENV, "1")
        .env(settings_store::relay::ENV, server.url().to_string())
        // A device here is a person's computer, not a machine configured as a server, and
        // takes no services or runtimes a launcher handed this process.
        .env_remove("TILO_SERVER_CONFIG")
        .env_remove("TILO_RUNTIME_BUNDLE_DIR")
        .env_remove("TILO_SVC_COLLAB_FD")
        .env_remove("TILO_SVC_IDENTITY_FD")
        .stdout(std::fs::File::create(&log).unwrap())
        .stderr(std::process::Stdio::inherit())
        .status_locked()
        .unwrap();
    let out = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        status.success() && out.contains("1 passed"),
        "{test} on its own device: {status}\n{out}"
    );
    drop(server);
}

/// The relays this device's daemon was started with, as its person's devices take them as home.
fn relays() -> RelayMap {
    let url: iroh::RelayUrl = std::env::var(settings_store::relay::ENV)
        .expect("the device relays through the test's relay")
        .parse()
        .unwrap();
    let mut relay = iroh::RelayConfig::from(url);
    relay.quic = None;
    std::iter::once(relay).collect()
}

/// A new key of the person's, named `label`, certified by their `root` with `capabilities`.
fn certify(
    root: &SigningIdentity,
    label: &str,
    capabilities: DeviceCapabilities,
) -> (SigningKey, DeviceCert) {
    let mut secret = [0; 32];
    getrandom::fill(&mut secret).unwrap();
    let key = SigningKey::from_bytes(&secret);
    // Issued a minute ago: a certificate issued later than a check's own now is not valid yet.
    let now = tilo_clock::unix_now_ns().saturating_sub(60 * 1_000_000_000);
    let mut cert = DeviceCert {
        user_public_key: root.public_key(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities,
        issued_at_ns: now,
        expires_at_ns: None,
        issuer: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut cert, root).expect("the root certifies the device");
    (key, cert)
}

/// The devices of `among` the person's devices record lists, in its order: the order
/// `PeerRegistrar::find` asks them in. The daemon answers it once the person's services it starts
/// run.
fn listed(daemon: &DaemonPlace, among: &[&DeviceCert]) -> Vec<[u8; 32]> {
    use tilo_server_protocol::devices::{DEVICES, record_value};
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let reading = daemon.read_record(DEVICES.0, DEVICES.1);
        let records = reading.as_ref().map(record_value).unwrap_or_default();
        if let Some(entries) = records["devices"].as_object() {
            return entries
                .keys()
                .filter_map(|key| {
                    among
                        .iter()
                        .find(|cert| hex::encode(cert.device_public_key) == *key)
                        .map(|cert| cert.device_public_key)
                })
                .collect();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon lists the person's devices: {reading:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What an answering device does with a request.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// It holds no root of the person's.
    NoRoot,
    /// It holds the root and its person allowed the enrolment.
    Enrol,
}

/// One of the person's devices, answering [`ENROL_ALPN`] as itself.
struct Answering {
    answer: Answer,
    root: Arc<SigningIdentity>,
    /// Who it is, as the sheet would name it.
    label: &'static str,
    /// Its own key, which the sibling half it keeps is kept under.
    device: [u8; 32],
    asked: Arc<AtomicUsize>,
    /// The sibling halves it keeps, as a device that enrolled one does.
    kept: Arc<Mutex<Vec<[u8; HALF_LEN]>>>,
}

impl Answering {
    fn reply(&self, request: &EnrolRequest) -> EnrolReply {
        match self.answer {
            Answer::NoRoot => EnrolReply::NotKeyHolder,
            Answer::Enrol => {
                let mut cert = DeviceCert {
                    user_public_key: self.root.public_key(),
                    device_public_key: request.boot_public_key,
                    device_label: "Boot key".into(),
                    capabilities: DeviceCapabilities::nothing(),
                    issued_at_ns: tilo_clock::unix_now_ns(),
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                };
                sign_device_cert(&mut cert, self.root.as_ref()).expect("the person's root signs");
                self.kept
                    .lock()
                    .unwrap()
                    .push(*request.sibling_half.expose());
                EnrolReply::Enrolled(Box::new(EnrolResponse {
                    cert,
                    siblings: vec![SiblingContact {
                        device_public_key: self.device,
                        label: self.label.into(),
                        endpoint: String::new(),
                    }],
                }))
            }
        }
    }
}

impl std::fmt::Debug for Answering {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label)
    }
}

impl ProtocolHandler for Answering {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return Ok(());
        };
        self.asked.fetch_add(1, Ordering::SeqCst);
        let Some(request) = read_frame::<EnrolRequest>(&mut recv).await else {
            return Ok(());
        };
        write_reply(&mut send, &self.reply(&request)).await;
        Ok(())
    }
}

async fn read_frame<T: serde::de::DeserializeOwned>(recv: &mut RecvStream) -> Option<T> {
    let mut prefix = [0_u8; 4];
    recv.read_exact(&mut prefix).await.ok()?;
    let mut body = vec![0_u8; frame_len(prefix).ok()?];
    recv.read_exact(&mut body).await.ok()?;
    decode_frame(&body).ok()
}

async fn write_reply(send: &mut SendStream, reply: &EnrolReply) {
    if let Ok(frame) = encode_frame(reply) {
        let _ = send.write_all(&frame).await;
    }
    let _ = send.finish();
    // The answer is read before the connection goes away.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), send.stopped()).await;
}

/// The person's devices on the loopback relay the daemon relays through, each answering as it
/// was told.
struct Devices {
    /// How often each was asked, by its key.
    asked: Vec<([u8; 32], Arc<AtomicUsize>)>,
    kept: Arc<Mutex<Vec<[u8; HALF_LEN]>>>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Devices {
    fn new(root: &Arc<SigningIdentity>, devices: Vec<(SigningKey, Answer, &'static str)>) -> Self {
        let relays = relays();
        let kept = Arc::new(Mutex::new(Vec::new()));
        let asked: Vec<([u8; 32], Arc<AtomicUsize>)> = devices
            .iter()
            .map(|(key, _, _)| {
                (
                    key.verifying_key().to_bytes(),
                    Arc::new(AtomicUsize::new(0)),
                )
            })
            .collect();
        let answering: Vec<(SigningKey, Answering)> = devices
            .into_iter()
            .zip(&asked)
            .map(|((key, answer, label), (device, asked))| {
                (
                    key,
                    Answering {
                        answer,
                        root: Arc::clone(root),
                        label,
                        device: *device,
                        asked: Arc::clone(asked),
                        kept: Arc::clone(&kept),
                    },
                )
            })
            .collect();
        let (ready, bound) = std::sync::mpsc::channel();
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a runtime for the person's devices");
            runtime.block_on(async move {
                let mut routers = Vec::new();
                for (key, answering) in answering {
                    let endpoint = Endpoint::builder(presets::Minimal)
                        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
                        .relay_mode(RelayMode::Custom(relays.clone()))
                        .alpns(vec![ENROL_ALPN.to_vec()])
                        .bind_addr((Ipv4Addr::LOCALHOST, 0))
                        .expect("a loopback address")
                        .bind()
                        .await
                        .expect("the device's endpoint binds");
                    tilo_test_env::within("the device's home relay", async {
                        while !endpoint
                            .addr()
                            .relay_urls()
                            .any(|named| Some(named) == relays.urls::<Vec<_>>().first())
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    })
                    .await;
                    routers.push(Router::builder(endpoint).accept(ENROL_ALPN, answering).spawn());
                }
                ready.send(()).expect("the test waits for the devices");
                let _ = stopped.recv();
                for router in &routers {
                    let _ = router.shutdown().await;
                }
            });
        });
        bound.recv().expect("the person's devices are up");
        Self {
            asked,
            kept,
            stop: Some(stop),
            thread: Some(thread),
        }
    }

    /// How often `device` was asked.
    fn asked(&self, device: &[u8; 32]) -> usize {
        self.asked
            .iter()
            .find(|(key, _)| key == device)
            .expect("one of the person's devices")
            .1
            .load(Ordering::SeqCst)
    }
}

impl Drop for Devices {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

const DISK_PASSPHRASE: &[u8] = b"the installer's disk passphrase";

/// A LUKS2 volume in `dir` that the disk passphrase opens, from a key file
/// beside it.
fn luks_image(dir: &Path) -> (PathBuf, PathBuf) {
    let image = dir.join("disk.img");
    let file = std::fs::File::create(&image).unwrap();
    file.set_len(48 << 20).unwrap();
    drop(file);
    let key = dir.join("disk.key");
    std::fs::write(&key, DISK_PASSPHRASE).unwrap();
    assert!(
        Command::new("cryptsetup")
            .args(["luksFormat", "--type", "luks2", "--batch-mode"])
            .args(["--luks2-keyslots-size", "2M", "--pbkdf", "pbkdf2"])
            .args(["--pbkdf-force-iterations", "1000", "--key-file"])
            .arg(&key)
            .arg(&image)
            .status_locked()
            .unwrap()
            .success()
    );
    (image, key)
}

fn header(device: &Path) -> serde_json::Value {
    let out = Command::new("cryptsetup")
        .args(["luksDump", "--dump-json-metadata"])
        .arg(device)
        .output_locked()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

fn slots(device: &Path) -> Vec<String> {
    let mut slots: Vec<String> = header(device)["keyslots"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    slots.sort();
    slots
}

/// Writes `token` at `id` of the header, as anyone holding the disk can.
fn import(dir: &Path, device: &Path, id: u32, token: &serde_json::Value) {
    let file = dir.join("token.json");
    std::fs::write(&file, token.to_string()).unwrap();
    assert!(
        Command::new("cryptsetup")
            .args(["token", "import", "--token-replace", "--token-id"])
            .arg(id.to_string())
            .arg("--json-file")
            .arg(&file)
            .arg(device)
            .status_locked()
            .unwrap()
            .success()
    );
}

/// The volume as early boot reaches it.
struct Disk<'a> {
    runner: &'a Real,
    ws: &'a Workspace,
    device: &'a Path,
}

impl Volume for Disk<'_> {
    fn opens(&self, keyslot: u32, passphrase: &str) -> bool {
        luks::opens(
            self.runner,
            self.ws,
            self.device,
            passphrase.as_bytes(),
            keyslot,
        )
        .unwrap()
    }

    fn uuid(&self) -> anyhow::Result<[u8; 16]> {
        luks::uuid(self.runner, self.device)
    }
}

/// A sibling's answer to this boot, and what it was asked.
struct Answers {
    answer: Result<[u8; HALF_LEN], Refusal>,
    slot: SlotId,
    asked: Mutex<Vec<BootId>>,
}

impl Siblings for Answers {
    async fn release(
        &self,
        _token: &Token,
        cert: &DeviceCert,
        boot_key: &SigningKey,
        boot_id: BootId,
        slot: SlotId,
    ) -> Result<Half, Refusal> {
        assert_eq!(boot_key.verifying_key().to_bytes(), cert.device_public_key);
        assert_eq!(slot, self.slot);
        self.asked.lock().unwrap().push(boot_id);
        self.answer.clone().map(Half::from_bytes)
    }
}

/// A machine with no session enrols its volume's sibling unlock by asking the
/// person's devices through its daemon, and at boot the half one of them keeps
/// opens the volume, answering its passphrase prompt; every way the
/// enrolment, the header or the answer can be wrong is refused, and with no
/// daemon running the enrolment says so.
#[test]
fn a_volume_joins_through_the_persons_devices_and_opens_at_boot_with_the_half_they_keep() {
    require_capability!(
        Swtpm,
        match Command::new("cryptsetup").arg("--version").output_locked() {
            Ok(out) if out.status.success() => Ok(()),
            other => Err(format!("cryptsetup cannot run: {other:?}")),
        },
        "the header is written with cryptsetup"
    );
    on_own_device(enrols_and_opens);
}

fn enrols_and_opens(profile: &Path) {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let runner = Real::new().with_cwd(&work);
    let ws = Workspace::new(&work);
    let (device, key_file) = luks_image(dir.path());
    let machine = Machine {
        runner: &runner,
        has_tpm: false,
    };
    let args = || Args {
        device: device.clone(),
        key_file: Some(key_file.clone()),
        networks: Vec::new(),
        replace_networks: false,
        session_uid: None,
        require_tpm: false,
        unattended: false,
    };

    // The person this device was founded for, their root kept here, and their other devices,
    // which their record keeps beside an earlier machine's boot key: one that can enrol nothing,
    // and is never asked.
    let root = Arc::new(
        tilo_identity::store::load_from_path(
            tilo_runtime::device::services::device_root_record().expect("the person founded here"),
        )
        .unwrap(),
    );
    let (laptop_key, laptop) = certify(&root, "The laptop", DeviceCapabilities::member());
    let (phone_key, phone) = certify(&root, "The phone", DeviceCapabilities::member());
    let (boot_key, earlier_boot) =
        certify(&root, "An earlier boot key", DeviceCapabilities::nothing());
    keep_devices(profile, &[&laptop, &phone, &earlier_boot]);
    // The device's daemon, as the `tilo-runtime` program runs it: its native endpoint is where
    // the enrolment asks it.
    let running = tilo_runtime::daemon::Running::start(profile).expect("the daemon starts");
    let here = tilo_api::daemon::this_computer().unwrap();
    assert_eq!(running.native_socket(), here.native(), "the daemon every client finds");
    assert_eq!(listed(&here, &[&earlier_boot]).len(), 1, "the record keeps the boot key");
    // The daemon asks them in the order their record lists them.
    let order = listed(&here, &[&laptop, &phone]);
    assert_eq!(order.len(), 2, "the record keeps both devices");
    let (first, second) = (order[0], order[1]);
    let named = |device: [u8; 32]| -> (SigningKey, &'static str) {
        if device == laptop.device_public_key {
            (laptop_key.clone(), "The laptop")
        } else {
            (phone_key.clone(), "The phone")
        }
    };
    let boot = earlier_boot.device_public_key;

    // With no daemon running, nothing reads the device key in its place: the enrolment says the
    // device service isn't running, and no slot stays behind.
    let nobody = tempfile::tempdir().unwrap();
    let stopped = DaemonPlace::at(nobody.path(), nobody.path());
    let refused = PeerRegistrar::asking(stopped.clone())
        .err()
        .expect("no daemon lists the devices");
    assert!(
        format!("{refused:#}").contains("the device service isn't running on this computer"),
        "{refused:#}"
    );
    let refused = run_with_peers(
        &args(),
        machine,
        Some(PeerRegistrar::new(stopped, vec![first, second])),
    )
    .expect_err("no daemon asks the devices");
    assert!(
        format!("{refused:#}").contains("the device service isn't running on this computer"),
        "{refused:#}"
    );
    assert_eq!(slots(&device), ["0"], "the slot is removed again");

    // Neither device keeps the root: each is asked once, and no slot stays behind.
    {
        let devices = Devices::new(
            &root,
            vec![
                (named(first).0, Answer::NoRoot, named(first).1),
                (named(second).0, Answer::NoRoot, named(second).1),
                (boot_key.clone(), Answer::Enrol, "An earlier boot key"),
            ],
        );
        let refused = run_with_peers(&args(), machine, None).expect_err("nobody can enrol it");
        assert_eq!(
            refused.downcast_ref::<NotEnrolled>(),
            Some(&NotEnrolled::NotKeyHolder),
            "{refused:#}"
        );
        assert_eq!(devices.asked(&first), 1, "the first device was asked");
        assert_eq!(devices.asked(&second), 1, "then the second");
        assert_eq!(devices.asked(&boot), 0, "a boot key is no device to ask");
        assert_eq!(slots(&device), ["0"], "the slot is removed again");
    }

    // The first device keeps no root, so the second is asked, whose person approves.
    let devices = Devices::new(
        &root,
        vec![
            (named(first).0, Answer::NoRoot, named(first).1),
            (named(second).0, Answer::Enrol, named(second).1),
            (boot_key, Answer::Enrol, "An earlier boot key"),
        ],
    );
    // What this machine cannot do is refused before a slot is added.
    for (wrong, why) in [
        (
            Args {
                require_tpm: true,
                ..args()
            },
            "TPM sealing is required",
        ),
        (
            Args {
                unattended: true,
                ..args()
            },
            "unattended unlock needs a TPM",
        ),
        (
            Args {
                replace_networks: true,
                ..args()
            },
            "no sibling unlock whose networks could be replaced",
        ),
    ] {
        let refused = run_with_peers(&wrong, machine, None).expect_err(why);
        assert!(format!("{refused:#}").contains(why), "{refused:#}");
    }
    assert_eq!(slots(&device), ["0"]);
    assert!(!sibling::enrolled(&runner, &device));

    // The comforts token is there first, so the sibling token is found among
    // others.
    luks::write_comforts(&runner, &ws, &device, &SignInComforts::default()).unwrap();
    run_with_peers(&args(), machine, None).expect("the second device enrols the volume");
    assert_eq!(devices.asked(&first), 1, "the first device was asked");
    assert_eq!(
        devices.asked(&second),
        1,
        "the first keeps no root, so the second was asked"
    );
    assert_eq!(devices.asked(&boot), 0, "a boot key is no device to ask");
    assert_eq!(slots(&device), ["0", "1"]);
    assert!(sibling::enrolled(&runner, &device));
    assert!(
        format!("{:#}", run_with_peers(&args(), machine, None).unwrap_err())
            .contains("already has a sibling unlock")
    );
    let status = enrol::status(&runner, &device).unwrap();
    assert_eq!(
        (status.keyslot, status.sealing, status.siblings),
        (1, Sealing::Cleartext, vec![named(second).1.to_owned()])
    );

    let token = luks::sibling(&runner, &device).unwrap().unwrap();
    let cert = token.cert().unwrap();
    verify_device_cert(&cert).expect("the person's root signed");
    assert_eq!(cert.user_public_key, root.public_key());
    assert_eq!(cert.capabilities, DeviceCapabilities::nothing());
    let material = || {
        let token = luks::sibling(&runner, &device).unwrap().unwrap();
        let json = tpm::unseal_material(&runner, token.sealing, &token.material).unwrap();
        Material::from_json(&json).unwrap()
    };
    assert_eq!(
        material().boot_key().unwrap().verifying_key().to_bytes(),
        cert.device_public_key
    );

    // Its networks are chosen again, and its secrets never show.
    let home = Network {
        id: "Home".into(),
        keyfile: "[wifi-security]\npsk=hunter2\n".into(),
    };
    enrol::replace_networks(&runner, &device, vec![home]).unwrap();
    assert_eq!(enrol::status(&runner, &device).unwrap().networks, ["Home"]);
    let shown = format!("{:?}", material());
    let half = hex::encode(material().device_half().unwrap().expose());
    assert!(
        !shown.contains("hunter2") && !shown.contains(&half),
        "{shown}"
    );

    // A header changed behind its back is refused, never half believed.
    let (id, _) = token::find(&header(&device)).unwrap().unwrap();
    let good = serde_json::to_value(luks::sibling(&runner, &device).unwrap().unwrap()).unwrap();
    let changed = |field: &str, value: serde_json::Value| {
        let mut token = good.clone();
        token[field] = value;
        token
    };
    let material_of = |value: serde_json::Value| {
        let json = serde_json::to_vec(&value).unwrap();
        tpm::seal_material(&runner, Sealing::Cleartext, &json).unwrap()
    };
    let older = changed("tilo_version", (TOKEN_VERSION - 1).into());
    import(dir.path(), &device, id, &older);
    assert!(
        luks::sibling(&runner, &device).unwrap().is_some(),
        "found, to be removed"
    );
    for refused in [
        enrol::status(&runner, &device).map(|_| ()),
        enrol::replace_networks(&runner, &device, Vec::new()),
    ] {
        let refused = refused.expect_err("an older token is set up again");
        assert!(
            format!("{refused:#}").contains("set it up again"),
            "{refused:#}"
        );
    }
    for tampered in [
        changed("keyslots", serde_json::json!(["0", "1"])),
        changed("siblings", serde_json::json!([])),
        changed("material", "not base64!".into()),
        changed(
            "material",
            material_of(serde_json::json!({"device_half": "11", "boot_secret": "22"})).into(),
        ),
        changed(
            "material",
            material_of(serde_json::json!({
                "device_half": "11".repeat(HALF_LEN),
                "boot_secret": "22".repeat(HALF_LEN),
                "networks": [{"id": "../etc/passwd", "keyfile": ""}],
            }))
            .into(),
        ),
    ] {
        import(dir.path(), &device, id, &tampered);
        assert!(enrol::status(&runner, &device).is_err(), "{tampered}");
    }
    import(dir.path(), &device, id, &good);
    import(dir.path(), &device, id + 5, &good);
    assert!(
        luks::sibling(&runner, &device).is_err(),
        "two tokens fail closed"
    );
    assert!(
        Command::new("cryptsetup")
            .args(["token", "remove", "--token-id"])
            .arg((id + 5).to_string())
            .arg(&device)
            .status_locked()
            .unwrap()
            .success()
    );

    // At boot: a refusal or a half that does not open the volume leaves the
    // prompt to the person; the half the phone keeps answers it.
    let ask_dir = dir.path().join("ask");
    std::fs::create_dir(&ask_dir).unwrap();
    let prompt = dir.path().join("prompt");
    let receiver = UnixDatagram::bind(&prompt).unwrap();
    receiver.set_nonblocking(true).unwrap();
    std::fs::write(
        ask_dir.join("ask.x"),
        format!(
            "[Ask]\nSocket={}\nNotAfter=0\nId=cryptsetup:{}\n",
            prompt.display(),
            device.display()
        ),
    )
    .unwrap();
    let token = luks::sibling(&runner, &device).unwrap().unwrap();
    let disk = Disk {
        runner: &runner,
        ws: &ws,
        device: &device,
    };
    let slot = SlotId {
        volume_uuid: luks::uuid(&runner, &device).unwrap(),
        keyslot: 1,
    };
    let kept = devices.kept.lock().unwrap()[0];
    let boot = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let attempt = |answer: Result<[u8; HALF_LEN], Refusal>| {
        let answers = Answers {
            answer,
            slot,
            asked: Mutex::default(),
        };
        let outcome = boot
            .block_on(sibling::attempt(
                &device,
                &token,
                &material(),
                BootId([9; 16]),
                &answers,
                &disk,
                &ask_dir,
                Duration::from_secs(1),
            ))
            .unwrap();
        assert_eq!(answers.asked.into_inner().unwrap(), [BootId([9; 16])]);
        outcome
    };
    for (answer, refusal) in [
        (Err(Refusal::Denied), Refusal::Denied),
        (
            Err(Refusal::Refused(RefusalReason::Revoked)),
            Refusal::Refused(RefusalReason::Revoked),
        ),
        (Ok([0x44; HALF_LEN]), Refusal::WrongHalf),
    ] {
        assert_eq!(attempt(answer), Outcome::Refused(refusal));
    }
    assert!(
        receiver.recv(&mut [0_u8; 8]).is_err(),
        "the prompt was answered"
    );
    assert_eq!(attempt(Ok(kept)), Outcome::Unlocked);
    let mut answered = [0_u8; 128];
    let n = receiver.recv(&mut answered).unwrap();
    let passphrase = answered[..n].strip_prefix(b"+").expect("an answer");
    assert!(luks::opens(&runner, &ws, &device, passphrase, 1).unwrap());

    // Removing it takes its slot and token, and the disk passphrase stays.
    enrol::remove(&runner, &device, Some(DISK_PASSPHRASE)).unwrap();
    assert_eq!(slots(&device), ["0"]);
    assert!(!sibling::enrolled(&runner, &device));
    assert!(luks::comforts(&runner, &device).unwrap().is_some());

    // A machine with a TPM that requires one enrols with it: the material is
    // sealed to the TPM, and the key that attests its boot goes along.
    let swtpm = match Swtpm::start() {
        Ok(swtpm) => swtpm,
        Err(why) => {
            tilo_test_env::capability_unavailable(
                tilo_test_env::Capability::Swtpm,
                format_args!("{why}"),
            );
            return;
        }
    };
    let mut systemd_tpm = tilo_test_env::ScopedEnv::new();
    systemd_tpm.set("SYSTEMD_TPM2_DEVICE", swtpm.tcti());
    let with_tpm = Real::new().with_tcti(Some(&swtpm.tcti())).with_cwd(&work);
    let sealed = dir.path().join("sealed");
    std::fs::create_dir(&sealed).unwrap();
    let (sealed, key_file) = luks_image(&sealed);
    let required = Args {
        device: sealed.clone(),
        key_file: Some(key_file),
        require_tpm: true,
        ..args()
    };
    let machine = Machine {
        runner: &with_tpm,
        has_tpm: true,
    };
    run_with_peers(&required, machine, None)
        .expect("a required TPM that is there seals it");
    let status = enrol::status(&with_tpm, &sealed).unwrap();
    assert_eq!(status.sealing, Sealing::Tpm2);
    let token = luks::sibling(&with_tpm, &sealed).unwrap().unwrap();
    let json = tpm::unseal_material(&with_tpm, token.sealing, &token.material).unwrap();
    assert!(Material::from_json(&json).unwrap().attestation.is_some());
    assert!(
        tpm::unseal_material(&runner, Sealing::Cleartext, &token.material)
            .is_ok_and(|blob| Material::from_json(&blob).is_err()),
        "the sealed material is no cleartext"
    );
}
