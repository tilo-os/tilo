//! A machine enrols its volume's sibling unlock through its daemon, as `tilo-auth sibling enrol`
//! does: the daemon has the person's root certify the boot key, seals the sibling half to their
//! secret-sync key and publishes the enrolment in this device's entry of their devices. At boot,
//! the half the person's devices carry opens the volume.
//!
//! The machine is a real device: its daemon (`tilo-runtime`) runs in this test's process, keeps
//! the device key and the person's root, and their devices record. The enrolment reads neither
//! the key nor the root; it asks the daemon. A device without the root has its boot key certified
//! at one of the person's devices that keeps it, where they are asked: the daemon's own test of
//! two devices proves that (`tilo_runtime`'s `boot_enrol_tests`).

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "a failed unwrap or index in a test is its assertion, and a journey reads as one flow"
)]

use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use settings_store::sign_in_comforts::SignInComforts;
use tilo_api::daemon::DaemonPlace;
use tilo_auth::boot::sibling::{self, Outcome, Refusal, Siblings, Volume};
use tilo_auth::luks;
use tilo_auth::sibling::enrol::{self, Args, DaemonRegistrar, Machine, run};
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
use tilo_unlock_protocol::published::UnlockFabricState;
use tilo_unlock_protocol::{BootId, HALF_LEN, Half, RefusalReason, SlotId, open_sibling_half};
use tilo_user_identity::{
    DeviceCapabilities, DeviceCert, StatementSigner as _, sign_device_cert, verify_device_cert,
};

// The daemon opens its documents in a worker process, which this test binary serves.
tilo_automerge_isolate::test_worker!();

/// Names the test the process [`on_own_device`] starts runs, and where its daemon's profile is.
const OWN: &str = "TILO_AUTH_SIBLING_ENROL_TEST";
const PROFILE: &str = "TILO_AUTH_SIBLING_ENROL_PROFILE";

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
    // The daemon relays through it: nothing reaches a relay or a directory out on the internet.
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

/// Writes `token` at `id` of the header, as anyone with the disk can.
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

/// The half the person's devices carry for this device's enrolment of `slot`: what its daemon
/// published in its entry of their devices, opened with their secret-sync key as a device that
/// keeps their root opens it to release it.
fn carried_half(daemon: &DaemonPlace, root: &SigningIdentity, cert: &DeviceCert, slot: SlotId) -> [u8; HALF_LEN] {
    use tilo_server_protocol::devices::{DEVICES, record_value};
    let this = hex::encode(
        daemon
            .actions()
            .device_key()
            .expect("the daemon says its key"),
    );
    let records = record_value(&daemon.read_record(DEVICES.0, DEVICES.1).unwrap());
    let entry = &records["devices"][&this];
    let fact = entry.get("v").unwrap_or(entry)["facts"]["unlock"]
        .as_str()
        .expect("the device publishes its disk unlock");
    let state: UnlockFabricState = postcard::from_bytes(&hex::decode(fact).unwrap()).unwrap();
    state
        .validate(&hex::decode(&this).unwrap().try_into().unwrap())
        .expect("what it publishes is its own");
    let enrolment = state
        .enrolments
        .iter()
        .find(|enrolment| enrolment.slot == slot)
        .expect("the enrolment of the slot is published");
    assert_eq!(&enrolment.cert, cert, "the enrolment is the certificate the token keeps");
    let secret_sync = root.secret_sync_key().expect("the root derives its secret-sync key");
    *open_sibling_half(&secret_sync, cert, &enrolment.sealed_half)
        .expect("the person's root opens the half")
        .expose()
}

/// A machine enrols its volume's sibling unlock through its daemon, which keeps the person's root
/// here, and at boot the half their devices carry opens the volume, answering its passphrase
/// prompt; every way the enrolment, the header or the answer can be wrong is refused, and with
/// no daemon running the enrolment says so.
#[test]
fn a_volume_enrols_through_the_daemon_and_opens_at_boot_with_the_half_the_devices_carry() {
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
    // which their record keeps beside an earlier machine's boot key: one that carries no half.
    let root = tilo_identity::store::load_from_path(
        tilo_runtime::device::services::device_root_record().expect("the person founded here"),
    )
    .unwrap();
    let (_, laptop) = certify(&root, "The laptop", DeviceCapabilities::member());
    let (_, phone) = certify(&root, "The phone", DeviceCapabilities::member());
    let (_, earlier_boot) = certify(&root, "An earlier boot key", DeviceCapabilities::nothing());
    keep_devices(profile, &[&laptop, &phone, &earlier_boot]);
    // The device's daemon, as the `tilo-runtime` program runs it: its native endpoint is where
    // the enrolment asks it.
    let running = tilo_runtime::daemon::Running::start(profile).expect("the daemon starts");
    let here = tilo_api::daemon::this_computer().unwrap();
    assert_eq!(
        running.native_socket(),
        here.native(),
        "the daemon every client finds"
    );
    running
        .person(Duration::from_secs(120))
        .expect("the person's services run");

    // With no daemon running, nothing reads the device key in its place: the enrolment says the
    // device service isn't running, and no slot stays behind.
    let nobody = tempfile::tempdir().unwrap();
    let stopped = DaemonRegistrar::new(DaemonPlace::at(nobody.path(), nobody.path()));
    let refused = enrol::enrol(
        machine,
        &device,
        DISK_PASSPHRASE,
        Vec::new(),
        &stopped,
        Sealing::Cleartext,
        false,
    )
    .expect_err("no daemon enrols the boot");
    assert!(
        format!("{refused:#}").contains("the device service isn't running on this computer"),
        "{refused:#}"
    );
    assert_eq!(slots(&device), ["0"], "the slot is removed again");

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
        let refused = run(&wrong, machine).expect_err(why);
        assert!(format!("{refused:#}").contains(why), "{refused:#}");
    }
    assert_eq!(slots(&device), ["0"]);
    assert!(!sibling::enrolled(&runner, &device));

    // The comforts token is there first, so the sibling token is found among
    // others.
    luks::write_comforts(&runner, &ws, &device, &SignInComforts::default()).unwrap();
    run(&args(), machine).expect("the daemon enrols the volume");
    assert_eq!(slots(&device), ["0", "1"]);
    assert!(sibling::enrolled(&runner, &device));
    assert!(
        format!("{:#}", run(&args(), machine).unwrap_err())
            .contains("already has a sibling unlock")
    );
    let status = enrol::status(&runner, &device).unwrap();
    let mut siblings = status.siblings.clone();
    siblings.sort();
    assert_eq!(
        (status.keyslot, status.sealing, siblings),
        (
            1,
            Sealing::Cleartext,
            vec!["The laptop".to_owned(), "The phone".to_owned()]
        ),
        "the person's other devices carry the half, and a boot key none"
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
    // prompt to the person; the half the person's devices carry answers it.
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
    let kept = carried_half(&here, &root, &cert, slot);
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
    run(&required, machine).expect("a required TPM that is there seals it");
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
    running.stop().unwrap();
}
