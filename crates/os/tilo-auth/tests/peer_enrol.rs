//! A machine with no session, or one that holds none of the person's root,
//! enrols its volume's sibling unlock by asking the person's devices over the
//! peer network: the one that holds the root answers after its person
//! approved, and a device that holds none says so and the next is asked. At
//! boot, the half that device keeps opens the volume.
//!
//! The devices here are the two sides of the peer network on a loopback relay:
//! what the answering device does with the request is the session's
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
use tilo_auth::boot::sibling::{self, Outcome, Refusal, Siblings, Volume};
use tilo_auth::luks;
use tilo_auth::sibling::enrol::{self, Args, Machine, NotEnrolled, run};
use tilo_auth::sibling::material::{Material, Network};
use tilo_auth::sibling::token::{self, TOKEN_VERSION, Token};
use tilo_auth::tpm::{self, Sealing};
use tilo_device_identity::{IdentityPaths, persist};
use tilo_identity::device_fleet::{DeviceFleet, DeviceFleetStore};
use tilo_rollback::exec::Real;
use tilo_rollback::policy::Workspace;
use tilo_test_env::SpawnUnderEnvLock as _;
use tilo_test_env::require_capability;
use tilo_test_env::swtpm::Swtpm;
use tilo_unlock_protocol::{
    BootId, ENROL_ALPN, EnrolReply, EnrolRequest, EnrolResponse, HALF_LEN, Half, RefusalReason,
    SiblingContact, SlotId, decode_frame, encode_frame, frame_len,
};
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert, verify_device_cert};

/// The person's root, which only their own devices hold.
fn person() -> SigningKey {
    SigningKey::from_bytes(&[0x21; 32])
}

fn device_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn certify(root: &SigningKey, device: &SigningKey, label: &str) -> DeviceCert {
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: label.into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        issuer: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, root).expect("the root certifies the device");
    cert
}

/// What the enrolling machine keeps: this device's key and certificate, and
/// the fleet that names the person's devices.
struct Enrolling {
    _env: tilo_test_env::ScopedEnv,
    _root: tempfile::TempDir,
}

fn enrolling(root: &SigningKey, certs: &[DeviceCert]) -> Enrolling {
    joined(root, certs, true)
}

/// A machine of the person's with no account of its own, as a server is: its
/// enrollment put the person's fleet in its identity root, and no session or
/// home directory of theirs exists to read one from.
fn enrolled_server(root: &SigningKey, certs: &[DeviceCert]) -> Enrolling {
    joined(root, certs, false)
}

fn joined(root: &SigningKey, certs: &[DeviceCert], personal: bool) -> Enrolling {
    let dir = tempfile::tempdir().expect("a directory for the identity");
    let config = dir.path().join("config");
    let system = dir.path().join("system");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_IDENTITY_ROOT", &system);
    if personal {
        env.set("XDG_CONFIG_HOME", &config);
    } else {
        env.remove("XDG_CONFIG_HOME").remove("HOME");
    }
    let paths = IdentityPaths::under(&system);
    paths.ensure_directories().expect("the identity layout");
    let key = device_key(0x61);
    persist::write_device_key(&paths.device_privkey, &key).expect("this device's key");
    let cert = certs
        .iter()
        .find(|cert| cert.device_public_key == key.verifying_key().to_bytes())
        .expect("this device's certificate");
    persist::write_device_cert(&paths.device_cert, cert).expect("this device's certificate");
    let fleet = DeviceFleet {
        schema_version: 1,
        identity_id: hex::encode(root.verifying_key().to_bytes()),
        certificates: certs.to_vec(),
        revocations: Vec::new(),
    };
    let store = if personal {
        DeviceFleetStore::at(config.join("tilo/identity"), paths)
    } else {
        DeviceFleetStore::of_device(paths, &fleet.identity_id)
    };
    let file = store.fleet_file(&fleet.identity_id);
    std::fs::create_dir_all(file.parent().expect("the fleet's directory"))
        .expect("the fleet's directory");
    std::fs::write(&file, serde_json::to_vec(&fleet).expect("the fleet"))
        .expect("the fleet is written");
    Enrolling {
        _env: env,
        _root: dir,
    }
}

/// What an answering device does with a request.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// It holds no root of the person's.
    NotKeyHolder,
    /// It holds the root and its person allowed the enrolment.
    Enrol,
}

/// One of the person's devices, answering [`ENROL_ALPN`] as itself.
#[derive(Debug)]
struct Answering {
    answer: Answer,
    root: SigningKey,
    person: [u8; 32],
    /// Who it is, as the sheet would name it.
    label: &'static str,
    asked: Arc<AtomicUsize>,
    /// The sibling halves it keeps, as a device that enrolled one does.
    kept: Arc<Mutex<Vec<[u8; HALF_LEN]>>>,
}

impl Answering {
    fn reply(&self, request: &EnrolRequest) -> EnrolReply {
        match self.answer {
            Answer::NotKeyHolder => EnrolReply::NotKeyHolder,
            Answer::Enrol => {
                let mut cert = DeviceCert {
                    user_public_key: self.person,
                    device_public_key: request.boot_public_key,
                    device_label: "Boot key".into(),
                    capabilities: DeviceCapabilities::nothing(),
                    issued_at_ns: tilo_clock::unix_now_ns(),
                    expires_at_ns: None,
                    signature: [0; 64],
                    issuer: None,
                };
                sign_device_cert(&mut cert, &self.root).expect("the person's root signs");
                self.kept
                    .lock()
                    .unwrap()
                    .push(*request.sibling_half.expose());
                EnrolReply::Enrolled(Box::new(EnrolResponse {
                    cert,
                    siblings: vec![SiblingContact {
                        device_public_key: self.root.verifying_key().to_bytes(),
                        label: self.label.into(),
                        endpoint: String::new(),
                    }],
                }))
            }
        }
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

/// The person's devices on a loopback relay: the laptop, then the phone, and a
/// server that is not among the enrolling machine's candidates.
struct Devices {
    /// The relay both sides reach each other through.
    relays: RelayMap,
    laptop: Arc<AtomicUsize>,
    phone: Arc<AtomicUsize>,
    server: Arc<AtomicUsize>,
    kept: Arc<Mutex<Vec<[u8; HALF_LEN]>>>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The relay is served on this runtime, which must outlive the exchange, and
    /// the server itself, which closes when dropped.
    _relay: tokio::runtime::Runtime,
    _relay_server: tilo_test_env::relay::Relay,
}

impl Devices {
    fn new(root: &SigningKey, laptop: Answer, phone: Answer) -> Self {
        let relay = tokio::runtime::Runtime::new().expect("a runtime for the relay");
        let server = relay.block_on(tilo_test_env::relay::relay());
        let relays = server.relays();
        let person = root.verifying_key().to_bytes();
        let laptop_asked = Arc::new(AtomicUsize::new(0));
        let phone_asked = Arc::new(AtomicUsize::new(0));
        let server_asked = Arc::new(AtomicUsize::new(0));
        let kept = Arc::new(Mutex::new(Vec::new()));
        let kept_for_thread = Arc::clone(&kept);
        let (ready, bound) = std::sync::mpsc::channel();
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let root = root.clone();
        let bound_relays = relays.clone();
        let (laptop_for_thread, phone_for_thread, server_for_thread) = (
            Arc::clone(&laptop_asked),
            Arc::clone(&phone_asked),
            Arc::clone(&server_asked),
        );
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("a runtime for the person's devices");
            runtime.block_on(async move {
                let mut routers = Vec::new();
                for (seed, answer, label, asked) in [
                    (0x31_u8, laptop, "The laptop", laptop_for_thread),
                    (0x41, phone, "The phone", phone_for_thread),
                    (0x51, Answer::Enrol, "The server", server_for_thread),
                ] {
                    let endpoint = Endpoint::builder(presets::Minimal)
                        .secret_key(iroh::SecretKey::from_bytes(&device_key(seed).to_bytes()))
                        .relay_mode(RelayMode::Custom(bound_relays.clone()))
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
                            .any(|named| Some(named) == bound_relays.urls::<Vec<_>>().first())
                        {
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                    })
                    .await;
                    routers.push(
                        Router::builder(endpoint)
                            .accept(
                                ENROL_ALPN,
                                Answering {
                                    answer,
                                    root: root.clone(),
                                    person,
                                    label,
                                    asked,
                                    kept: Arc::clone(&kept_for_thread),
                                },
                            )
                            .spawn(),
                    );
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
            relays,
            laptop: laptop_asked,
            phone: phone_asked,
            server: server_asked,
            kept,
            stop: Some(stop),
            thread: Some(thread),
            _relay: relay,
            _relay_server: server,
        }
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
/// person's devices over the relay it is told of, and at boot the half one of
/// them keeps opens the volume, answering its passphrase prompt; every way
/// the enrolment, the header or the answer can be wrong is refused.
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
    let root = person();
    let laptop = certify(&root, &device_key(0x31), "The laptop");
    let phone = certify(&root, &device_key(0x41), "The phone");
    let server = certify(&root, &device_key(0x51), "The server");
    let me = certify(&root, &device_key(0x61), "The desktop");
    let args = |relays: &RelayMap| Args {
        device: device.clone(),
        key_file: Some(key_file.clone()),
        networks: Vec::new(),
        replace_networks: false,
        session_uid: None,
        require_tpm: false,
        unattended: false,
        relay: Some(relays.urls::<Vec<_>>()[0].to_string()),
    };

    // Neither device the machine knows holds the root: no slot stays behind.
    {
        let _enrolled = enrolling(&root, &[laptop.clone(), phone.clone(), me.clone()]);
        let devices = Devices::new(&root, Answer::NotKeyHolder, Answer::NotKeyHolder);
        let refused = run(&args(&devices.relays), machine).expect_err("nobody can enrol it");
        assert_eq!(
            refused.downcast_ref::<NotEnrolled>(),
            Some(&NotEnrolled::NotKeyHolder),
            "{refused:#}"
        );
        assert_eq!(
            devices.laptop.load(Ordering::SeqCst),
            1,
            "the laptop was asked"
        );
        assert_eq!(devices.phone.load(Ordering::SeqCst), 1, "then the phone");
        assert_eq!(slots(&device), ["0"], "the slot is removed again");
    }

    // A server nobody is signed in to has no account of the person's to read
    // their devices from: its enrollment left their fleet in its own store.
    let _enrolled = enrolled_server(&root, &[laptop, phone, server, me]);
    let devices = Devices::new(&root, Answer::NotKeyHolder, Answer::Enrol);
    // What this machine cannot do is refused before a slot is added.
    for (wrong, why) in [
        (
            Args {
                relay: Some("not a relay".into()),
                ..args(&devices.relays)
            },
            "is not a packet relay",
        ),
        (
            Args {
                require_tpm: true,
                ..args(&devices.relays)
            },
            "TPM sealing is required",
        ),
        (
            Args {
                unattended: true,
                ..args(&devices.relays)
            },
            "unattended unlock needs a TPM",
        ),
        (
            Args {
                replace_networks: true,
                ..args(&devices.relays)
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
    run(&args(&devices.relays), machine).expect("the phone enrols the volume");
    assert_eq!(
        devices.laptop.load(Ordering::SeqCst),
        1,
        "the laptop was asked"
    );
    assert_eq!(
        devices.phone.load(Ordering::SeqCst),
        1,
        "the laptop holds no root, so the phone was asked"
    );
    assert_eq!(
        devices.server.load(Ordering::SeqCst),
        0,
        "the enrolment answered before the server was needed"
    );
    assert_eq!(slots(&device), ["0", "1"]);
    assert!(sibling::enrolled(&runner, &device));
    assert!(
        format!("{:#}", run(&args(&devices.relays), machine).unwrap_err())
            .contains("already has a sibling unlock")
    );
    let status = enrol::status(&runner, &device).unwrap();
    assert_eq!(
        (status.keyslot, status.sealing, status.siblings),
        (1, Sealing::Cleartext, vec!["The phone".to_owned()])
    );

    let token = luks::sibling(&runner, &device).unwrap().unwrap();
    let cert = token.cert().unwrap();
    verify_device_cert(&cert).expect("the person's root signed");
    assert_eq!(cert.user_public_key, root.verifying_key().to_bytes());
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
        ..args(&devices.relays)
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
}
