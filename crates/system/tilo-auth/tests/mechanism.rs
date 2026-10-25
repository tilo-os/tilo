//! The wrappers, the LUKS2 token and keyslot, and the boot sign-in against an
//! emulated TPM and a LUKS2 header on a plain file: no root, no loop device,
//! never the machine's own TPM or disks.
//!
//! Needs `swtpm`, `tpm2-tools` and `cryptsetup` (`TILO_REQUIRE_SWTPM=1` makes
//! their absence a failure). The handoff test also needs the kernel keyring.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use base64::Engine as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use tilo_auth::boot::{self, BootOutcome, BootUnlock};
use tilo_auth::handoff::{self, Handoff};
use tilo_auth::luks::{self, Authority, SigninToken};
use tilo_auth::record::Record;
use tilo_auth::recovery::RecoveryKey;
use tilo_auth::secret::Secret;
use tilo_auth::tpm::{self, Unseal};
use tilo_auth::userkey::UserKey;
use tilo_auth::wrapper::{Outcome, Tpm, Wrapper};
use tilo_rollback::exec::Real;
use tilo_rollback::lockout;
use tilo_rollback::policy::Workspace;
use tilo_seal::envelope::PasswordParams;
use tilo_test_env::require_capability;
use tilo_test_env::swtpm::{Swtpm, contains};

const DISK_PASSPHRASE: &[u8] = b"the installer's disk passphrase";
const UID: u32 = 1000;

/// Cheap enough for a test; the shipped cost is the contract's.
const FAST: PasswordParams = PasswordParams {
    m_cost_kib: 8 * 1024,
    t_cost: 1,
    p_cost: 1,
};

struct Fixture {
    tpm: Swtpm,
    scratch: tempfile::TempDir,
    work: PathBuf,
    runner: Real,
    ws: Workspace,
}

impl Fixture {
    fn tpm(&self) -> Tpm<'_> {
        Tpm {
            runner: &self.runner,
            ws: &self.ws,
        }
    }

    /// Five tries, then a ten-minute wait per try, as the shipped TPM policy.
    fn throttle(&self) {
        lockout::tighten(
            &self.runner,
            lockout::Target {
                max_tries: 5,
                recovery_time: 600,
                lockout_recovery: 600,
            },
            None,
            self.scratch.path(),
        )
        .expect("tighten the emulated TPM's lockout");
    }

    fn tries_used(&self) -> u32 {
        lockout::read(&self.runner).expect("read lockout").counter
    }

    fn luks_image(&self) -> PathBuf {
        self.luks_image_named("disk.img")
    }

    fn luks_image_named(&self, name: &str) -> PathBuf {
        let img = self.scratch.path().join(name);
        let f = std::fs::File::create(&img).expect("create image");
        f.set_len(48 << 20).expect("size image");
        drop(f);
        let pass = self.scratch.path().join("format.key");
        std::fs::write(&pass, DISK_PASSPHRASE).unwrap();
        let ok = Command::new("cryptsetup")
            .args([
                "luksFormat",
                "--type",
                "luks2",
                "--batch-mode",
                "--pbkdf",
                "pbkdf2",
                "--pbkdf-force-iterations",
                "1000",
                "--key-file",
            ])
            .arg(&pass)
            .arg(&img)
            .status()
            .expect("cryptsetup luksFormat")
            .success();
        assert!(ok, "luksFormat failed");
        std::fs::remove_file(&pass).unwrap();
        img
    }

    /// A person who signs in with `secret` through the TPM and opens the
    /// system volume through their own keyslot, as an enrolled admin is.
    fn enrolled_admin(&self, device: &Path, secret: &str) -> (Record, UserKey) {
        let key = UserKey::generate().unwrap();
        let mut record = Record::new(UID, "ada", "Ada Lovelace");
        record.set_secret_wrapper(Wrapper::tpm(self.tpm(), &key, &Secret::new(secret)).unwrap());
        record.unlocks_system = true;
        let slot = luks::add_slot(
            &self.runner,
            &self.ws,
            device,
            &Authority::Passphrase(DISK_PASSPHRASE),
            key.system_volume_passphrase().as_bytes(),
        )
        .expect("add the sign-in keyslot");
        record.system_keyslot = Some(slot);
        luks::write_token(
            &self.runner,
            &self.ws,
            device,
            &SigninToken::mirror(&record, slot),
        )
        .expect("write the sign-in token");
        (record, key)
    }
}

fn fixture() -> Option<Fixture> {
    fixture_on(Swtpm::start())
}

fn fixture_on(tpm: Result<Swtpm, String>) -> Option<Fixture> {
    let tpm = match tpm {
        Ok(tpm) => tpm,
        Err(why) => {
            tilo_test_env::capability_unavailable(
                tilo_test_env::Capability::Swtpm,
                format_args!("{why}"),
            );
            return None;
        }
    };
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let runner = Real::new().with_tcti(Some(&tpm.tcti())).with_cwd(&work);
    let ws = Workspace::new(&work);
    Some(Fixture {
        tpm,
        scratch,
        work,
        runner,
        ws,
    })
}

macro_rules! fixture {
    () => {
        match fixture() {
            Some(f) => f,
            None => return,
        }
    };
}

fn key_of(outcome: Option<Outcome>) -> UserKey {
    match outcome {
        Some(Outcome::Key(key)) => key,
        Some(Outcome::Wrong(_)) => panic!("the secret was refused"),
        Some(Outcome::LockedOut(_)) => panic!("the TPM was locked out"),
        None => panic!("the wrapper could not be tried"),
    }
}

fn slots_in_use(device: &Path) -> Vec<String> {
    let out = Command::new("cryptsetup")
        .args(["luksDump", "--dump-json-metadata"])
        .arg(device)
        .output()
        .unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut slots: Vec<String> = meta["keyslots"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    slots.sort();
    slots
}

fn opens_any_slot(device: &Path, passphrase: &[u8], dir: &Path) -> bool {
    let file = dir.join("probe.key");
    std::fs::write(&file, passphrase).unwrap();
    let ok = Command::new("cryptsetup")
        .args(["open", "--test-passphrase", "--key-file"])
        .arg(&file)
        .arg(device)
        .status()
        .unwrap()
        .success();
    std::fs::remove_file(&file).unwrap();
    ok
}

#[test]
fn the_tpm_wrapper_opens_with_the_secret_and_its_auth_value_is_the_contract_digest() {
    let f = fixture!();
    let key = UserKey::generate().unwrap();
    let secret = Secret::new("123456");
    let wrapper = Wrapper::tpm(f.tpm(), &key, &secret).unwrap();

    let opened = key_of(
        wrapper
            .open_with_secret(Some(f.tpm()), UID, &secret)
            .unwrap(),
    );
    assert_eq!(opened.expose(), key.expose());

    // Unseal with plain tpm2-tools and the digest given inline: the object's
    // authorisation is the 32 bytes themselves, not the text of the file the
    // daemon passes them in.
    let Wrapper::Tpm { public, private } = &wrapper else {
        panic!("a TPM wrapper");
    };
    let b64 = base64::engine::general_purpose::STANDARD;
    let parent = tilo_auth::tpm::primary(&f.runner, &f.ws).unwrap();
    let pub_file = f.ws.put("check.pub", &b64.decode(public).unwrap()).unwrap();
    let priv_file =
        f.ws.put("check.priv", &b64.decode(private).unwrap())
            .unwrap();
    let ctx = f.ws.path("check.ctx");
    let tool = |args: &[&std::ffi::OsStr]| {
        Command::new(args[0])
            .args(&args[1..])
            .env("TPM2TOOLS_TCTI", f.tpm.tcti())
            .current_dir(&f.work)
            .output()
            .unwrap()
    };
    let loaded = tool(&[
        "tpm2_load".as_ref(),
        "-C".as_ref(),
        parent.as_os_str(),
        "-u".as_ref(),
        pub_file.as_os_str(),
        "-r".as_ref(),
        priv_file.as_os_str(),
        "-c".as_ref(),
        ctx.as_os_str(),
    ]);
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    tilo_rollback::policy::flush(&f.runner);
    let auth = format!("hex:{}", hex_encode(&*secret.auth_value()));
    let unsealed = tool(&[
        "tpm2_unseal".as_ref(),
        "-c".as_ref(),
        ctx.as_os_str(),
        "-p".as_ref(),
        auth.as_ref(),
    ]);
    assert!(
        unsealed.status.success(),
        "{}",
        String::from_utf8_lossy(&unsealed.stderr)
    );
    assert_eq!(unsealed.stdout, key.expose());
}

#[test]
fn neither_the_user_key_nor_its_authorisation_crosses_the_bus_in_the_clear() {
    let Some(f) = fixture_on(Swtpm::start_recording()) else {
        return;
    };
    let key = UserKey::generate().unwrap();
    let auth: [u8; 32] = std::array::from_fn(|i| 0xa0 ^ u8::try_from(i).unwrap());
    let sealed = tpm::seal(&f.runner, &f.ws, &key, &auth).unwrap();
    let Unseal::Key(opened) = tpm::unseal(&f.runner, &f.ws, &sealed, &auth).unwrap() else {
        panic!("the right authorisation value unseals");
    };
    assert_eq!(opened.expose(), key.expose());

    let wire = f.tpm.wire();
    assert!(
        contains(&wire, &[0x00, 0x00, 0x01, 0x5e]),
        "the log holds the exchange, TPM2_Unseal included"
    );
    assert!(
        !contains(&wire, key.expose()),
        "the user key crossed in the clear"
    );
    assert!(
        !contains(&wire, &auth),
        "the authorisation value crossed in the clear"
    );
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[test]
fn a_wrong_secret_costs_one_try_and_the_lockout_refuses_even_the_right_one() {
    let f = fixture!();
    f.throttle();
    let key = UserKey::generate().unwrap();
    let wrapper = Wrapper::tpm(f.tpm(), &key, &Secret::new("123456")).unwrap();

    match wrapper
        .open_with_secret(Some(f.tpm()), UID, &Secret::new("654321"))
        .unwrap()
    {
        Some(Outcome::Wrong(Some(tries))) => {
            assert_eq!(tries.left, 4);
            assert_eq!(tries.retry_in_s, 600);
        }
        _ => panic!("a wrong secret should be reported with the tries left"),
    }
    assert_eq!(f.tries_used(), 1);

    key_of(
        wrapper
            .open_with_secret(Some(f.tpm()), UID, &Secret::new("123456"))
            .unwrap(),
    );
    assert_eq!(f.tries_used(), 1, "the right secret costs nothing");

    for wrong in ["1", "2", "3", "4"] {
        let _ = wrapper.open_with_secret(Some(f.tpm()), UID, &Secret::new(wrong));
    }
    match wrapper
        .open_with_secret(Some(f.tpm()), UID, &Secret::new("123456"))
        .unwrap()
    {
        Some(Outcome::LockedOut(Some(tries))) => assert_eq!(tries.left, 0),
        _ => panic!("five wrong secrets should lock the TPM out"),
    }

    // Sealing costs no try, so a new secret can be set while the lockout lasts.
    let renewed = Wrapper::tpm(f.tpm(), &key, &Secret::new("2468")).unwrap();
    assert!(matches!(
        renewed
            .open_with_secret(Some(f.tpm()), UID, &Secret::new("2468"))
            .unwrap(),
        Some(Outcome::LockedOut(_))
    ));
}

#[test]
fn the_passphrase_and_recovery_wrappers_open_only_with_their_own_factor() {
    let key = UserKey::generate().unwrap();
    let secret = Secret::new("correct horse battery staple");
    let passphrase = Wrapper::passphrase(&key, UID, &secret, FAST).unwrap();
    let opened = key_of(passphrase.open_with_secret(None, UID, &secret).unwrap());
    assert_eq!(opened.expose(), key.expose());
    assert!(matches!(
        passphrase
            .open_with_secret(None, UID, &Secret::new("incorrect horse"))
            .unwrap(),
        Some(Outcome::Wrong(None))
    ));
    assert!(
        matches!(
            passphrase.open_with_secret(None, UID + 1, &secret).unwrap(),
            Some(Outcome::Wrong(None))
        ),
        "a wrapper is bound to its person"
    );

    let recovery = RecoveryKey::from_bytes([7; 32]);
    let wrapper = Wrapper::recovery(&key, UID, &recovery).unwrap();
    assert_eq!(
        wrapper
            .open_with_recovery(UID, &recovery)
            .unwrap()
            .unwrap()
            .expose(),
        key.expose()
    );
    assert!(
        wrapper
            .open_with_recovery(UID, &RecoveryKey::from_bytes([8; 32]))
            .unwrap()
            .is_none()
    );
    assert!(
        wrapper
            .open_with_secret(None, UID, &secret)
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_sign_in_keyslot_and_token_join_the_header_without_touching_the_rest() {
    let f = fixture!();
    let device = f.luks_image();
    assert_eq!(slots_in_use(&device), ["0"]);
    let (record, key) = f.enrolled_admin(&device, "123456");
    let slot = record.system_keyslot.unwrap();

    assert_eq!(slots_in_use(&device), ["0".to_owned(), slot.to_string()]);
    assert!(opens_any_slot(&device, DISK_PASSPHRASE, f.scratch.path()));
    assert!(
        luks::opens(
            &f.runner,
            &f.ws,
            &device,
            key.system_volume_passphrase().as_bytes(),
            slot
        )
        .unwrap()
    );

    let tokens = luks::tokens(&f.runner, &device).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].1, SigninToken::mirror(&record, slot));

    // Mirroring again replaces the person's token rather than adding one.
    let mut renamed = record;
    renamed.display_name = "Ada King".into();
    luks::write_token(
        &f.runner,
        &f.ws,
        &device,
        &SigninToken::mirror(&renamed, slot),
    )
    .unwrap();
    let tokens = luks::tokens(&f.runner, &device).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].1.display_name, "Ada King");

    let people = serde_json::to_value(boot::people(&f.runner, &device).unwrap()).unwrap();
    assert_eq!(
        people,
        serde_json::json!([{
            "uid": UID,
            "name": "ada",
            "display_name": "Ada King",
            "factors": ["password"],
            "secret": "password",
        }])
    );
}

#[test]
fn a_keyslot_can_be_added_with_the_volume_key_alone() {
    let f = fixture!();
    let device = f.luks_image();
    let pass = f.scratch.path().join("pass.key");
    std::fs::write(&pass, DISK_PASSPHRASE).unwrap();
    let vk_file = f.scratch.path().join("vk");
    let dumped = Command::new("cryptsetup")
        .args([
            "luksDump",
            "--batch-mode",
            "--dump-volume-key",
            "--key-file",
        ])
        .arg(&pass)
        .arg("--volume-key-file")
        .arg(&vk_file)
        .arg(&device)
        .output()
        .unwrap();
    assert!(
        dumped.status.success(),
        "{}",
        String::from_utf8_lossy(&dumped.stderr)
    );
    let vk = std::fs::read(&vk_file).unwrap();

    let key = UserKey::generate().unwrap();
    let slot = luks::add_slot(
        &f.runner,
        &f.ws,
        &device,
        &Authority::VolumeKey(&vk),
        key.system_volume_passphrase().as_bytes(),
    )
    .unwrap();
    assert!(
        luks::opens(
            &f.runner,
            &f.ws,
            &device,
            key.system_volume_passphrase().as_bytes(),
            slot
        )
        .unwrap()
    );
    assert!(opens_any_slot(&device, DISK_PASSPHRASE, f.scratch.path()));
}

#[test]
fn boot_unlock_opens_the_volume_with_the_secret_or_the_recovery_key() {
    let f = fixture!();
    f.throttle();
    let device = f.luks_image();
    let (mut record, key) = f.enrolled_admin(&device, "123456");
    let recovery = RecoveryKey::from_bytes([3; 32]);
    record.set_recovery_wrapper(Wrapper::recovery(&key, UID, &recovery).unwrap());
    let slot = record.system_keyslot.unwrap();
    luks::write_token(
        &f.runner,
        &f.ws,
        &device,
        &SigninToken::mirror(&record, slot),
    )
    .unwrap();

    let keys_dir = f.scratch.path().join("keys");
    std::fs::create_dir(&keys_dir).unwrap();
    let unlock = BootUnlock {
        runner: &f.runner,
        ws: &f.ws,
        device: &device,
        volume: "root",
        uid: None,
        keys_dir: &keys_dir,
        handoff: None,
    };

    match unlock.run(b"999999\n") {
        BootOutcome::Wrong(Some(tries)) => assert_eq!(tries.left, 4),
        other => panic!("a wrong secret should be wrong with tries left, not {other:?}"),
    }
    assert!(!keys_dir.join("root.key").exists());

    assert!(matches!(
        unlock.run(b"123456\n"),
        BootOutcome::Unlocked { uid: UID }
    ));
    let written = std::fs::read(keys_dir.join("root.key")).unwrap();
    assert_eq!(written, key.system_volume_passphrase().as_bytes());
    assert!(opens_any_slot(&device, &written, f.scratch.path()));
    std::fs::remove_file(keys_dir.join("root.key")).unwrap();

    let used = f.tries_used();
    assert!(matches!(
        unlock.run(recovery.formatted().as_bytes()),
        BootOutcome::Unlocked { uid: UID }
    ));
    assert_eq!(
        f.tries_used(),
        used,
        "the recovery key never touches the TPM"
    );
    assert!(matches!(
        unlock.run(RecoveryKey::from_bytes([4; 32]).formatted().as_bytes()),
        BootOutcome::Wrong(None)
    ));
}

#[test]
fn boot_unlock_cannot_try_on_a_volume_with_no_sign_in_token() {
    let f = fixture!();
    let device = f.luks_image();
    let keys_dir = f.scratch.path().join("keys");
    std::fs::create_dir(&keys_dir).unwrap();
    let outcome = BootUnlock {
        runner: &f.runner,
        ws: &f.ws,
        device: &device,
        volume: "root",
        uid: None,
        keys_dir: &keys_dir,
        handoff: None,
    }
    .run(b"123456");
    assert!(matches!(outcome, BootOutcome::CouldNotTry(_)));
}

#[test]
fn the_boot_unlock_command_speaks_the_contracts_exit_codes() {
    let f = fixture!();
    f.throttle();
    let device = f.luks_image();
    let (_, key) = f.enrolled_admin(&device, "123456");
    // Not there yet, as in an initrd before anything else wrote a key.
    let keys_dir = f.scratch.path().join("run/cryptsetup-keys.d");
    let secret_file = f.scratch.path().join("secret");
    let handoff = format!("tilo-signin-test-cli-{}", std::process::id());
    let run = |device: &Path, secret: &[u8]| {
        std::fs::write(&secret_file, secret).unwrap();
        Command::new(env!("CARGO_BIN_EXE_tilo-auth"))
            .arg("--tcti")
            .arg(f.tpm.tcti())
            .arg("boot-unlock")
            .arg("--device")
            .arg(device)
            .args(["--volume", "root", "--secret-file"])
            .arg(&secret_file)
            .arg("--keys-dir")
            .arg(&keys_dir)
            .arg("--handoff")
            .arg(&handoff)
            .output()
            .unwrap()
    };

    let wrong = run(&device, b"000000");
    assert_eq!(wrong.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&wrong.stderr).unwrap();
    assert_eq!(report["error"], "wrong");
    assert_eq!(report["tries_left"], 4);

    let right = run(&device, b"123456\n");
    assert_eq!(
        right.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&right.stderr)
    );
    assert_eq!(
        std::fs::read(keys_dir.join("root.key")).unwrap(),
        key.system_volume_passphrase().as_bytes()
    );

    let bare = f.luks_image_named("bare.img");
    assert_eq!(run(&bare, b"123456").status.code(), Some(2));

    let people = Command::new(env!("CARGO_BIN_EXE_tilo-auth"))
        .arg("boot-people")
        .arg("--device")
        .arg(&device)
        .output()
        .unwrap();
    assert!(people.status.success());
    let people: serde_json::Value = serde_json::from_slice(&people.stdout).unwrap();
    assert_eq!(people[0]["name"], "ada");
    let _ = handoff::take(&handoff);
}

/// The tests that need the kernel keyring, which t's job sandbox refuses: there
/// they skip, and the `tilo-auth-keyring` check runs every test under
/// `keyring::` in a micro-VM, where the keyring is required.
mod keyring {
    use super::*;

    /// Where a skipped test runs after all.
    const COVERED: &str = "the tilo-auth-keyring check runs it in a micro-VM";

    fn keyring_usable() -> Result<(), String> {
        // Probe with a key nobody else uses, so a refusing seccomp profile shows
        // up as a skip rather than a failure of the code under test.
        let probe = format!("tilo-auth-probe-{}", std::process::id());
        let handoff = Handoff {
            uid: 1,
            key: UserKey::from_bytes([0; 32]),
        };
        handoff::add(&handoff, &probe).map_err(|e| e.to_string())?;
        handoff::take(&probe).map_err(|e| e.to_string())?;
        Ok(())
    }

    #[test]
    fn the_handoff_survives_one_read_and_no_more() {
        require_capability!(
            KernelKeyring,
            keyring_usable(),
            "the kernel keyring refused add_key; {COVERED}"
        );
        let description = format!("tilo-signin-test-{}", std::process::id());
        let key = UserKey::generate().unwrap();
        handoff::add(
            &Handoff {
                uid: UID,
                key: key.clone(),
            },
            &description,
        )
        .unwrap();
        let taken = handoff::take(&description).unwrap().expect("the handoff");
        assert_eq!(taken.uid, UID);
        assert_eq!(taken.key.expose(), key.expose());
        assert!(handoff::take(&description).unwrap().is_none());

        // A second handoff replaces the first instead of sitting beside it.
        for byte in [1, 2] {
            handoff::add(
                &Handoff {
                    uid: UID,
                    key: UserKey::from_bytes([byte; 32]),
                },
                &description,
            )
            .unwrap();
        }
        assert_eq!(
            handoff::take(&description).unwrap().unwrap().key.expose(),
            &[2; 32]
        );
    }

    /// Set in the child that leaves the handoff the way tilo-root-discover
    /// does in the initrd: as a service with a session keyring of its own
    /// (KeyringMode=private), which does not link the user keyring, so
    /// nothing there is possessed.
    const CHILD: &str = "TILO_AUTH_TEST_HANDOFF_CHILD";

    #[test]
    fn a_service_whose_session_keyring_lacks_the_user_keyring_leaves_the_handoff() {
        if let Some(description) = std::env::var_os(CHILD) {
            const KEYCTL_JOIN_SESSION_KEYRING: libc::c_long = 1;
            // SAFETY: a null name joins a new anonymous session keyring.
            let joined =
                unsafe { libc::syscall(libc::SYS_keyctl, KEYCTL_JOIN_SESSION_KEYRING, 0_usize) };
            assert!(joined >= 0, "{}", std::io::Error::last_os_error());
            handoff::add(
                &Handoff {
                    uid: UID,
                    key: UserKey::from_bytes([7; 32]),
                },
                description.to_str().unwrap(),
            )
            .unwrap();
            return;
        }
        require_capability!(
            KernelKeyring,
            keyring_usable(),
            "the kernel keyring refused add_key; {COVERED}"
        );
        let description = format!("tilo-signin-test-service-{}", std::process::id());
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "keyring::a_service_whose_session_keyring_lacks_the_user_keyring_leaves_the_handoff",
                "--exact",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD, &description)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "the service could not leave the handoff:\n{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        let taken = handoff::take(&description).unwrap().expect("the handoff");
        assert_eq!(taken.uid, UID);
        assert_eq!(taken.key.expose(), &[7; 32]);
    }
}
