//! `tilo-authd` end to end against an emulated TPM, a LUKS2 header on a
//! plain file and a stand-in for the old password database: moving people
//! off their old password, the admin's system keyslot, changing secrets,
//! recovery, the TPM lockout authorisation and the boot sign-in.
//!
//! Needs `swtpm`, `tpm2-tools` and `cryptsetup` (`TILO_REQUIRE_SWTPM=1` makes
//! their absence a failure). The boot sign-in test also needs the kernel
//! keyring.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use tilo_auth::accounts::Legacy;
use tilo_auth::client::Client;
use tilo_auth::daemon::volume::{Locate, SystemVolume};
use tilo_auth::daemon::{Answer, Config, Daemon, Peer, server};
use tilo_auth::handoff::{self, Handoff};
use tilo_auth::luks;
use tilo_auth::protocol::{Refusal, Reply, Request};
use tilo_auth::record::{Attention, Record, SecretKind, Store};
use tilo_auth::recovery::RecoveryKey;
use tilo_auth::secret::Secret;
use tilo_auth::userkey::UserKey;
use tilo_auth::wrapper::{Kind, Outcome, Tpm};
use tilo_rollback::exec::Real;
use tilo_rollback::lockout;
use tilo_rollback::policy::Workspace;
use tilo_seal::envelope::PasswordParams;
use tilo_test_env::require_capability;
use tilo_test_env::swtpm::Swtpm;
use zeroize::Zeroizing;

const DISK_PASSPHRASE: &str = "the installer's disk passphrase";
const ROOT: Peer = Peer { uid: 0 };
const ADA: Peer = Peer { uid: 1000 };
const BOB: Peer = Peer { uid: 1001 };
const GREETER: Peer = Peer { uid: 977 };

const FAST: PasswordParams = PasswordParams {
    m_cost_kib: 8 * 1024,
    t_cost: 1,
    p_cost: 1,
};

/// The old password database: a password per name, and who has been locked.
#[derive(Clone, Default)]
struct OldPasswords {
    passwords: Arc<Mutex<HashMap<String, String>>>,
    locked: Arc<Mutex<Vec<String>>>,
}

impl OldPasswords {
    fn with(entries: &[(&str, &str)]) -> Self {
        let old = Self::default();
        for (name, password) in entries {
            old.passwords
                .lock()
                .unwrap()
                .insert((*name).to_owned(), (*password).to_owned());
        }
        old
    }

    fn locked(&self) -> Vec<String> {
        self.locked.lock().unwrap().clone()
    }
}

impl Legacy for OldPasswords {
    fn verify(&self, name: &str, typed: &str) -> anyhow::Result<bool> {
        Ok(self.passwords.lock().unwrap().get(name).map(String::as_str) == Some(typed))
    }

    fn lock_password(&self, name: &str) -> anyhow::Result<()> {
        self.passwords.lock().unwrap().remove(name);
        self.locked.lock().unwrap().push(name.to_owned());
        Ok(())
    }
}

struct Machine {
    /// `None` is a machine without a usable TPM.
    tpm: Option<Swtpm>,
    dir: tempfile::TempDir,
    device: PathBuf,
    recovery: RecoveryKey,
    old: OldPasswords,
}

impl Machine {
    fn tcti(&self) -> String {
        match &self.tpm {
            Some(tpm) => tpm.tcti(),
            // Nothing listens on the discard port, so every TPM call fails.
            None => "swtpm:host=127.0.0.1,port=9".into(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn config(&self) -> Config {
        Config {
            socket: self.path("socket"),
            records: self.path("users"),
            accounts_root: self.path("root"),
            lockout: self.path("lockout.json"),
            lockout_legacy: self.path("lockout-auth"),
            proof_file: self.path("proof.key"),
            scratch: self.path("scratch"),
            greeter_user: "greetd".into(),
            privileged_uid: ROOT.uid,
            tcti: Some(self.tcti()),
            passphrase_params: FAST,
            volume: Locate::Fixed(SystemVolume {
                device: self.device.clone(),
                mapping: None,
            }),
            handoff: None,
            volume_key: None,
        }
    }

    fn daemon(&self) -> Daemon {
        self.daemon_with(self.config())
    }

    fn daemon_with(&self, config: Config) -> Daemon {
        Daemon::new(config, Box::new(self.old.clone())).unwrap()
    }

    fn runner(&self) -> (Real, Workspace) {
        let work = self.path("test-work");
        std::fs::create_dir_all(&work).unwrap();
        (
            Real::new().with_tcti(Some(&self.tcti())).with_cwd(&work),
            Workspace::new(work),
        )
    }

    fn record(&self, uid: u32) -> Option<Record> {
        Store::new(self.path("users")).load(uid).unwrap()
    }

    /// The disk passphrase this boot unlocked with, as tilo-rollback leaves it.
    fn leave_proof(&self) {
        std::fs::write(self.path("proof.key"), DISK_PASSPHRASE).unwrap();
    }

    /// Five tries, then a ten-minute wait per try.
    fn throttle(&self) {
        let (runner, _) = self.runner();
        lockout::tighten(
            &runner,
            lockout::Target {
                max_tries: 5,
                recovery_time: 600,
                lockout_recovery: 600,
            },
            None,
            self.dir.path(),
        )
        .unwrap();
    }

    fn tries_used(&self) -> u32 {
        let (runner, _) = self.runner();
        lockout::read(&runner).unwrap().counter
    }

    /// The user key from a person's TPM wrapper, as boot would unwrap it.
    fn user_key(&self, uid: u32, secret: &str) -> UserKey {
        let (runner, ws) = self.runner();
        let record = self.record(uid).unwrap();
        let wrapper = record.known().find(|w| w.kind() == Kind::Tpm).unwrap();
        match wrapper
            .open_with_secret(
                Some(Tpm {
                    runner: &runner,
                    ws: &ws,
                }),
                uid,
                &Secret::new(secret),
            )
            .unwrap()
        {
            Some(Outcome::Key(key)) => key,
            _ => panic!("the TPM wrapper should open with {secret:?}"),
        }
    }

    fn slots(&self) -> Vec<String> {
        let out = Command::new("cryptsetup")
            .args(["luksDump", "--dump-json-metadata"])
            .arg(&self.device)
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

    /// What the initrd's `link-volume-key=` does: cryptsetup links the
    /// volume key into the user keyring as a `user` key described
    /// `description`.
    fn link_volume_key(&self, description: &str) -> bool {
        let file = self.path("link.key");
        std::fs::write(&file, DISK_PASSPHRASE).unwrap();
        let ok = Command::new("cryptsetup")
            .args(["open", "--test-passphrase", "--key-file"])
            .arg(&file)
            .arg("--link-vk-to-keyring")
            .arg(format!("@u::%user:{description}"))
            .arg(&self.device)
            .status()
            .unwrap()
            .success();
        std::fs::remove_file(&file).unwrap();
        ok
    }

    fn opens(&self, passphrase: &str) -> bool {
        let file = self.path("probe.key");
        std::fs::write(&file, passphrase).unwrap();
        let ok = Command::new("cryptsetup")
            .args(["open", "--test-passphrase", "--key-file"])
            .arg(&file)
            .arg(&self.device)
            .status()
            .unwrap()
            .success();
        std::fs::remove_file(&file).unwrap();
        ok
    }
}

/// ada is the device admin; bob is not. The volume opens with the
/// installer's disk passphrase in slot 0 and the recovery key in slot 1.
fn machine(with_tpm: bool, old: &[(&str, &str)], ada_uid: u32) -> Option<Machine> {
    let tpm = if with_tpm {
        match Swtpm::start() {
            Ok(tpm) => Some(tpm),
            Err(why) => {
                tilo_test_env::capability_unavailable(
                    tilo_test_env::Capability::Swtpm,
                    format_args!("{why}"),
                );
                return None;
            }
        }
    } else {
        None
    };
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root/etc");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("passwd"),
        format!(
            "root:x:0:0:root:/root:/bin/bash\n\
             greetd:x:977:977::/var/lib/greetd:/sbin/nologin\n\
             ada:x:{ada_uid}:{ada_uid}:Ada Lovelace:/home/ada:/bin/bash\n\
             bob:x:1001:1001:Bob:/home/bob:/bin/bash\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("group"),
        "wheel:x:10:ada\ntilo-identity:x:960:ada\n",
    )
    .unwrap();

    let device = dir.path().join("disk.img");
    let f = std::fs::File::create(&device).unwrap();
    f.set_len(48 << 20).unwrap();
    drop(f);
    let pass = dir.path().join("format.key");
    std::fs::write(&pass, DISK_PASSPHRASE).unwrap();
    let pbkdf = ["--pbkdf", "pbkdf2", "--pbkdf-force-iterations", "1000"];
    assert!(
        Command::new("cryptsetup")
            .args(["luksFormat", "--type", "luks2", "--batch-mode"])
            .args(pbkdf)
            .arg("--key-file")
            .arg(&pass)
            .arg(&device)
            .status()
            .unwrap()
            .success()
    );
    let recovery = RecoveryKey::from_bytes([5; 32]);
    let rec = dir.path().join("recovery.key");
    std::fs::write(&rec, recovery.formatted().as_bytes()).unwrap();
    assert!(
        Command::new("cryptsetup")
            .args(["luksAddKey", "--batch-mode", "--key-file"])
            .arg(&pass)
            .args(pbkdf)
            .arg(&device)
            .arg(&rec)
            .status()
            .unwrap()
            .success()
    );
    std::fs::remove_file(&pass).unwrap();
    std::fs::remove_file(&rec).unwrap();

    Some(Machine {
        tpm,
        dir,
        device,
        recovery,
        old: OldPasswords::with(old),
    })
}

macro_rules! machine {
    ($tpm:expr, $old:expr) => {
        machine!($tpm, $old, ADA.uid)
    };
    ($tpm:expr, $old:expr, $ada_uid:expr) => {
        match machine($tpm, $old, $ada_uid) {
            Some(m) => m,
            None => return,
        }
    };
}

fn reply(answer: Answer) -> Reply {
    match answer {
        Answer::Reply(reply) => reply,
        Answer::Pending(_) => panic!("expected a reply"),
    }
}

fn secret(s: &str) -> Zeroizing<String> {
    Zeroizing::new(s.to_owned())
}

fn verify(daemon: &Daemon, peer: Peer, user: &str, typed: &str) -> Reply {
    reply(daemon.handle(
        peer,
        Request::Verify {
            user: user.into(),
            secret: secret(typed),
        },
    ))
}

fn pending(daemon: &Daemon, peer: Peer) -> Option<String> {
    match daemon.handle(peer, Request::PendingSignIn) {
        Answer::Pending(p) => p.map(|p| p.name),
        Answer::Reply(r) => panic!("expected a pending answer, got {r:?}"),
    }
}

fn refused(refusal: Refusal) -> Reply {
    Reply::refused(refusal)
}

#[test]
fn the_admin_moves_onto_the_tpm_and_gets_a_system_keyslot_on_first_sign_in() {
    let m = machine!(true, &[("ada", "123456")]);
    m.throttle();
    m.leave_proof();
    let lockout_auth = lockout::new_reset_auth().unwrap();
    std::fs::write(m.path("lockout-auth"), &*lockout_auth).unwrap();
    let daemon = m.daemon();

    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());

    let record = m.record(ADA.uid).expect("a record");
    assert!(record.enrolled());
    assert!(record.has(Kind::Tpm));
    assert!(record.unlocks_system);
    assert_eq!(record.attention, None);
    assert_eq!(m.old.locked(), ["ada"]);
    assert_eq!(record.secret, Some(SecretKind::Pin));

    // The old slots stay; one new slot opens with the derived passphrase.
    let slot = record.system_keyslot.expect("a system keyslot");
    assert_eq!(m.slots(), ["0", "1", &slot.to_string()]);
    assert!(m.opens(DISK_PASSPHRASE));
    let key = m.user_key(ADA.uid, "123456");
    assert!(m.opens(&key.system_volume_passphrase()));
    let (runner, _) = m.runner();
    let tokens = luks::tokens(&runner, &m.device).unwrap();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].1, luks::SigninToken::mirror(&record, slot));

    // The lockout authorisation is wrapped for ada, and tilo-rollback's file
    // stays until the recovery key can open it too.
    assert!(m.path("lockout.json").exists());
    assert!(m.path("lockout-auth").exists());

    // From now on the record answers, not the old password.
    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    let wrong = verify(&daemon, ADA, "ada", "000000");
    assert_eq!(wrong.error, Some(Refusal::Wrong));
    assert_eq!(wrong.tries_left, Some(4));
    assert_eq!(m.tries_used(), 1);

    // The same wrong entry again is not another try.
    let again = verify(&daemon, ADA, "ada", "000000");
    assert_eq!(again.error, Some(Refusal::Wrong));
    assert_eq!(m.tries_used(), 1);
    assert_eq!(
        verify(&daemon, ADA, "ada", "111111").error,
        Some(Refusal::Wrong)
    );
    assert_eq!(m.tries_used(), 2);

    // A later sign-in neither adds a slot nor rewrites the token.
    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    assert_eq!(m.slots().len(), 3);
    assert_eq!(luks::tokens(&runner, &m.device).unwrap(), tokens);
}

#[test]
fn the_admins_typed_password_adds_the_keyslot_when_it_is_the_disk_passphrase() {
    let m = machine!(true, &[("ada", DISK_PASSPHRASE)]);
    let daemon = m.daemon();
    assert_eq!(
        verify(&daemon, ADA, "ada", DISK_PASSPHRASE),
        Reply::success()
    );
    let record = m.record(ADA.uid).unwrap();
    assert!(record.system_keyslot.is_some());
    assert!(
        m.opens(DISK_PASSPHRASE),
        "the old passphrase is never removed"
    );
}

#[test]
fn an_admin_whose_volume_is_not_found_is_not_recorded_as_unlocking_it() {
    let m = machine!(true, &[("ada", "123456")]);
    m.leave_proof();
    let lockout_auth = lockout::new_reset_auth().unwrap();
    std::fs::write(m.path("lockout-auth"), &*lockout_auth).unwrap();
    let lost = m.daemon_with(Config {
        volume: Locate::Absent,
        ..m.config()
    });
    assert_eq!(verify(&lost, ADA, "ada", "123456"), Reply::success());
    let record = m.record(ADA.uid).unwrap();
    assert!(record.enrolled());
    assert!(!record.unlocks_system);
    assert_eq!(record.system_keyslot, None);
    assert_eq!(m.slots(), ["0", "1"]);
    assert!(
        m.path("lockout.json").exists(),
        "the admin still holds the lockout authorisation"
    );

    let found = m.daemon();
    assert_eq!(verify(&found, ADA, "ada", "123456"), Reply::success());
    let record = m.record(ADA.uid).unwrap();
    assert!(record.unlocks_system);
    assert!(record.system_keyslot.is_some());
    assert_eq!(m.slots().len(), 3);
}

#[test]
fn without_anything_that_opens_the_volume_the_keyslot_waits_for_a_later_sign_in() {
    let m = machine!(true, &[("ada", "123456")]);
    let daemon = m.daemon();
    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    let record = m.record(ADA.uid).unwrap();
    assert!(record.enrolled());
    assert_eq!(record.system_keyslot, None);
    assert_eq!(record.attention, Some(Attention::SystemKeyslotPending));
    assert_eq!(m.slots(), ["0", "1"]);

    m.leave_proof();
    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    let record = m.record(ADA.uid).unwrap();
    assert!(record.system_keyslot.is_some());
    assert_eq!(record.attention, None);
    assert_eq!(m.slots().len(), 3);
}

#[test]
fn someone_who_is_not_the_admin_gets_no_keyslot() {
    let m = machine!(true, &[("bob", "123456")]);
    m.leave_proof();
    let daemon = m.daemon();
    assert_eq!(verify(&daemon, BOB, "bob", "123456"), Reply::success());
    let record = m.record(BOB.uid).unwrap();
    assert!(record.enrolled());
    assert!(!record.unlocks_system);
    assert_eq!(record.system_keyslot, None);
    assert_eq!(m.slots(), ["0", "1"]);
    let (runner, _) = m.runner();
    assert!(luks::tokens(&runner, &m.device).unwrap().is_empty());
}

#[test]
fn without_a_tpm_only_a_strong_password_stands_alone() {
    let m = machine!(
        false,
        &[("bob", "1234"), ("ada", "correct horse battery staple")]
    );
    let daemon = m.daemon();

    assert_eq!(verify(&daemon, BOB, "bob", "1234"), Reply::success());
    let record = m.record(BOB.uid).unwrap();
    assert!(!record.enrolled());
    assert_eq!(record.attention, Some(Attention::WeakSecret));
    assert!(
        m.old.locked().is_empty(),
        "a weak password stays in the old database"
    );

    assert_eq!(
        verify(&daemon, ADA, "ada", "correct horse battery staple"),
        Reply::success()
    );
    let record = m.record(ADA.uid).unwrap();
    assert!(record.has(Kind::Passphrase));
    assert_eq!(record.secret, Some(SecretKind::Password));
    assert_eq!(m.old.locked(), ["ada"]);
    assert_eq!(
        verify(&daemon, ADA, "ada", "correct horse battery stable").error,
        Some(Refusal::Wrong)
    );
    assert_eq!(
        verify(&daemon, ADA, "ada", "correct horse battery staple"),
        Reply::success()
    );
}

#[test]
fn people_may_only_ask_about_themselves() {
    let m = machine!(false, &[("ada", "correct horse battery staple")]);
    let daemon = m.daemon();
    assert_eq!(
        verify(&daemon, BOB, "ada", "correct horse battery staple"),
        refused(Refusal::NotPermitted)
    );
    assert_eq!(
        verify(&daemon, BOB, "eve", "x"),
        refused(Refusal::NotPermitted)
    );
    assert_eq!(
        verify(&daemon, ROOT, "eve", "x"),
        refused(Refusal::NotEnrolled)
    );
    assert_eq!(verify(&daemon, ADA, "ada", ""), refused(Refusal::Wrong));
    assert_eq!(
        reply(daemon.handle(BOB, Request::ClaimBootSignIn { user: "bob".into() })),
        refused(Refusal::NotPermitted)
    );
    assert_eq!(
        reply(daemon.handle(
            ADA,
            Request::AddRecovery {
                user: "ada".into(),
                secret: secret("correct horse battery staple"),
                recovery: secret(&m.recovery.formatted()),
            }
        )),
        refused(Refusal::NotPermitted)
    );
    assert_eq!(pending(&daemon, BOB), None);
    let status = reply(daemon.handle(ADA, Request::Status { user: "ada".into() }));
    assert_eq!(status.enrolled, Some(false));
}

#[test]
fn a_secret_changes_with_the_current_one_or_with_the_recovery_key_as_root() {
    let m = machine!(true, &[("ada", "123456")]);
    m.leave_proof();
    let daemon = m.daemon();
    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    let slot = m.record(ADA.uid).unwrap().system_keyslot.unwrap();

    let change = |peer, current: Option<&str>, recovery: Option<&str>, new: &str| {
        reply(daemon.handle(
            peer,
            Request::SetSecret {
                user: "ada".into(),
                current: current.map(secret),
                recovery: recovery.map(secret),
                new: secret(new),
            },
        ))
    };
    assert_eq!(
        change(ADA, Some("000000"), None, "2468").error,
        Some(Refusal::Wrong)
    );
    assert_eq!(change(ADA, Some("123456"), None, "2468"), Reply::success());
    assert_eq!(
        verify(&daemon, ADA, "ada", "123456").error,
        Some(Refusal::Wrong)
    );
    assert_eq!(verify(&daemon, ADA, "ada", "2468"), Reply::success());

    // The user key, and with it the keyslot, stays the same.
    let record = m.record(ADA.uid).unwrap();
    assert_eq!(record.system_keyslot, Some(slot));
    assert!(m.opens(&m.user_key(ADA.uid, "2468").system_volume_passphrase()));
    let (runner, _) = m.runner();
    assert_eq!(
        luks::tokens(&runner, &m.device).unwrap()[0].1,
        luks::SigninToken::mirror(&record, slot)
    );

    // Recovery: root only, and only once a recovery wrapper exists.
    let recovery = m.recovery.formatted();
    assert_eq!(
        change(ADA, None, Some(&recovery), "1357"),
        refused(Refusal::NotPermitted)
    );
    assert_eq!(
        change(ROOT, None, Some(&recovery), "1357"),
        refused(Refusal::Wrong)
    );
    assert_eq!(
        reply(daemon.handle(
            ROOT,
            Request::AddRecovery {
                user: "ada".into(),
                secret: secret("2468"),
                recovery: secret(&recovery),
            }
        )),
        Reply::success()
    );
    assert!(m.record(ADA.uid).unwrap().has(Kind::Recovery));
    assert_eq!(
        change(ROOT, None, Some(&recovery), "1357"),
        Reply::success()
    );
    assert_eq!(verify(&daemon, ADA, "ada", "1357"), Reply::success());
}

#[test]
fn the_admins_recovery_key_must_be_the_machines() {
    let m = machine!(true, &[("ada", "123456")]);
    m.leave_proof();
    let daemon = m.daemon();
    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    let other = RecoveryKey::from_bytes([6; 32]).formatted();
    assert_eq!(
        reply(daemon.handle(
            ROOT,
            Request::AddRecovery {
                user: "ada".into(),
                secret: secret("123456"),
                recovery: secret(&other),
            }
        )),
        refused(Refusal::Wrong)
    );
    assert!(!m.record(ADA.uid).unwrap().has(Kind::Recovery));
}

#[test]
fn the_lockout_authorisation_moves_under_the_keys_and_recovery_ends_a_lockout() {
    let m = machine!(true, &[("ada", "123456")]);
    m.throttle();
    m.leave_proof();
    let (runner, _) = m.runner();
    let auth = lockout::new_reset_auth().unwrap();
    lockout::set_reset_auth(&runner, &auth, None, m.dir.path()).unwrap();
    std::fs::write(m.path("lockout-auth"), &*auth).unwrap();
    let daemon = m.daemon();

    assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
    let recovery = m.recovery.formatted();
    assert_eq!(
        reply(daemon.handle(
            ROOT,
            Request::AddRecovery {
                user: "ada".into(),
                secret: secret("123456"),
                recovery: secret(&recovery),
            }
        )),
        Reply::success()
    );
    assert!(
        !m.path("lockout-auth").exists(),
        "with both keys holding it, the plain file goes"
    );

    for wrong in ["1", "2", "3", "4", "5"] {
        let _ = verify(&daemon, ADA, "ada", wrong);
    }
    assert_eq!(
        verify(&daemon, ADA, "ada", "123456").error,
        Some(Refusal::LockedOut)
    );

    assert_eq!(
        reply(daemon.handle(
            ROOT,
            Request::SetSecret {
                user: "ada".into(),
                current: None,
                recovery: Some(secret(&recovery)),
                new: secret("2468"),
            }
        )),
        Reply::success()
    );
    assert_eq!(
        m.tries_used(),
        0,
        "recovering resets the TPM's failure counter"
    );
    assert_eq!(verify(&daemon, ADA, "ada", "2468"), Reply::success());
}

/// The tests that need the kernel keyring, which t's job sandbox refuses: there
/// they skip, and the `tilo-auth-keyring` check runs every test under
/// `keyring::` in a micro-VM, where the keyring is required.
mod keyring {
    use super::*;

    /// Where a skipped test runs after all.
    const COVERED: &str = "the tilo-auth-keyring check runs it in a micro-VM";

    fn keyring_usable() -> Result<(), String> {
        let probe = format!("tilo-authd-probe-{}", std::process::id());
        handoff::add(
            &Handoff {
                uid: 1,
                key: UserKey::from_bytes([0; 32]),
            },
            &probe,
        )
        .map_err(|e| e.to_string())?;
        handoff::take(&probe).map_err(|e| e.to_string())?;
        Ok(())
    }

    fn volume_key_linked(description: &str) -> bool {
        tilo_auth::keyring::read_one(description, 128)
            .unwrap()
            .is_some()
    }

    /// cryptsetup links the volume key readable by its possessor only, as the
    /// daemon's unit makes it (`KeyringMode=shared`).
    fn linked_volume_key(m: &Machine) -> Result<(), String> {
        let probe = format!("tilo-authd-vk-probe-{}", std::process::id());
        if !m.link_volume_key(&probe) {
            return Err("cryptsetup could not link the volume key".into());
        }
        let read = tilo_auth::keyring::read_one(&probe, 128);
        tilo_auth::keyring::forget(&probe).map_err(|e| e.to_string())?;
        match read {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err("the linked volume key is not there".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    #[test]
    fn the_linked_volume_key_adds_the_first_keyslot_once() {
        let m = machine!(true, &[("ada", "123456"), ("bob", "654321")]);
        require_capability!(
            KernelKeyring,
            linked_volume_key(&m),
            "the linked volume key cannot be read back; {COVERED}"
        );
        let description = format!("tilo-auth-test-volume-key-{}", std::process::id());
        let config = || Config {
            volume_key: Some(description.clone()),
            ..m.config()
        };

        // Whatever the first sign-in attempt of the boot answers, it is the last
        // that may use the key.
        assert!(m.link_volume_key(&description));
        let daemon = m.daemon_with(config());
        daemon.settle_volume_key();
        assert!(
            volume_key_linked(&description),
            "the volume has no sign-in yet"
        );
        assert_eq!(
            verify(&daemon, ADA, "ada", "wrong"),
            refused(Refusal::Wrong)
        );
        assert!(!volume_key_linked(&description));

        // The admin's first sign-in adds their keyslot with it, though neither
        // their password nor anything else this boot holds opens the volume.
        assert!(m.link_volume_key(&description));
        let daemon = m.daemon_with(config());
        daemon.settle_volume_key();
        assert_eq!(verify(&daemon, ADA, "ada", "123456"), Reply::success());
        assert!(!volume_key_linked(&description));
        let record = m.record(ADA.uid).unwrap();
        assert!(record.system_keyslot.is_some());
        assert_eq!(record.attention, None);
        assert_eq!(m.slots().len(), 3);

        // A volume that has a sign-in never keeps it.
        assert!(m.link_volume_key(&description));
        m.daemon_with(config()).settle_volume_key();
        assert!(!volume_key_linked(&description));
    }

    #[test]
    fn a_boot_sign_in_starts_one_session_for_its_person() {
        require_capability!(
            KernelKeyring,
            keyring_usable(),
            "the kernel keyring refused add_key; {COVERED}"
        );
        let m = machine!(true, &[("ada", "123456")]);
        m.leave_proof();
        assert_eq!(verify(&m.daemon(), ADA, "ada", "123456"), Reply::success());
        let key = m.user_key(ADA.uid, "123456");

        let description = format!("tilo-signin-test-{}", std::process::id());
        let config = || Config {
            handoff: Some(description.clone()),
            ..m.config()
        };

        // A handoff whose key opens no keyslot is dropped.
        handoff::add(
            &Handoff {
                uid: ADA.uid,
                key: UserKey::from_bytes([1; 32]),
            },
            &description,
        )
        .unwrap();
        let daemon = m.daemon_with(config());
        daemon.take_boot_handoff();
        assert_eq!(pending(&daemon, GREETER), None);

        handoff::add(&Handoff { uid: ADA.uid, key }, &description).unwrap();
        let daemon = m.daemon_with(config());
        daemon.take_boot_handoff();
        assert!(handoff::take(&description).unwrap().is_none(), "read once");

        assert_eq!(pending(&daemon, BOB), None);
        assert_eq!(pending(&daemon, GREETER).as_deref(), Some("ada"));
        let claim =
            |user: &str| reply(daemon.handle(ROOT, Request::ClaimBootSignIn { user: user.into() }));
        assert_eq!(claim("bob"), refused(Refusal::NoBootSignIn));
        assert_eq!(claim("ada"), Reply::success());
        assert_eq!(claim("ada"), refused(Refusal::NoBootSignIn));
        assert_eq!(pending(&daemon, GREETER), None);
    }
}

#[test]
fn the_socket_answers_with_the_peers_own_rights() {
    let me = rustix::process::getuid().as_raw();
    let m = machine!(false, &[("ada", "correct horse battery staple")], me);
    let daemon = Arc::new(m.daemon_with(Config {
        // Not root: the socket's peer is this test's own uid.
        privileged_uid: u32::MAX,
        ..m.config()
    }));
    let listener = server::bind(&daemon).unwrap();
    {
        let daemon = Arc::clone(&daemon);
        std::thread::spawn(move || server::serve(&daemon, &listener));
    }
    let socket: &Path = &m.path("socket");
    let mut client = Client::connect(socket).unwrap();
    assert_eq!(
        client
            .call(&Request::Verify {
                user: "ada".into(),
                secret: secret("correct horse battery staple"),
            })
            .unwrap(),
        Reply::success()
    );
    assert_eq!(
        client
            .call(&Request::Status { user: "bob".into() })
            .unwrap(),
        refused(Refusal::NotPermitted)
    );
    assert_eq!(client.pending_sign_in().unwrap(), None);
    assert_eq!(
        client
            .call(&Request::ClaimBootSignIn { user: "ada".into() })
            .unwrap(),
        refused(Refusal::NotPermitted)
    );
}
