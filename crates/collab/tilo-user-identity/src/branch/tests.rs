use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use ed25519_dalek::SigningKey;

use super::*;
use crate::cert::{DeviceCapabilities, verify_device_cert};
use crate::persist::{load_or_create_device_key, read_device_cert, read_device_key};

const SOURCE: &str = "4c4c4544-0042-3510-8048-b4c04f4d3232";
const BRANCH: &str = "8a1f02d6-91c4-4f0e-9b9a-2d1f5c3e7a10";
const NOW: u64 = 1_800_000_000_000_000_000;
const THIRD: &str = "0f6c5a31-2b7d-4e15-a0c3-91d2b8e4f657";

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn machine(text: &str) -> Machine {
    Machine::parse(text).unwrap()
}

fn public(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

/// A store sealed to whichever device key it names, as a vault is: what it
/// holds opens only under that key.
struct FileStore {
    path: PathBuf,
}

impl FileStore {
    fn seal(&self, key: &SigningKey, secret: &str) {
        let mut bytes = public(key).to_vec();
        bytes.extend_from_slice(secret.as_bytes());
        fs::write(&self.path, bytes).unwrap();
    }

    fn open(&self, key: &SigningKey) -> Option<String> {
        let bytes = fs::read(&self.path).ok()?;
        let (sealed_to, secret) = bytes.split_at(32);
        (sealed_to == public(key)).then(|| String::from_utf8(secret.to_vec()).unwrap())
    }
}

impl SealedStore for FileStore {
    fn name(&self) -> String {
        "vault".into()
    }

    fn reseal(&self, from: &[SigningKey], to: &SigningKey) -> Result<Resealed, String> {
        if !self.path.exists() {
            return Ok(Resealed::Absent);
        }
        if self.open(to).is_some() {
            return Ok(Resealed::AlreadyNew);
        }
        for key in from {
            if let Some(secret) = self.open(key) {
                self.seal(to, &secret);
                return Ok(Resealed::Moved);
            }
        }
        Ok(Resealed::Foreign)
    }
}

#[derive(Default)]
struct Names {
    renewed: Cell<u32>,
}

impl MachineIdentity for Names {
    fn renew(&self) -> io::Result<()> {
        self.renewed.set(self.renewed.get().saturating_add(1));
        Ok(())
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: IdentityPaths,
    store: FileStore,
    names: Names,
    source: SigningKey,
}

impl Fixture {
    /// A machine set up on `SOURCE`, its key certified by `root`'s
    /// person as "Studio", its vault holding a secret.
    fn new(root: &SigningKey) -> Self {
        let dir = tempdir();
        let paths = IdentityPaths::under(dir.path());
        let (source, _) = load_or_create_device_key(&paths).unwrap();
        let mut cert = DeviceCert {
            user_public_key: public(root),
            device_public_key: public(&source),
            device_label: "Studio".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: NOW.saturating_sub(1_000),
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        persist::write_public_key(&paths.user_pubkey, &public(root)).unwrap();
        persist::write_device_cert(&paths.device_cert, &cert).unwrap();
        persist::write_device_cert(&paths.device_cert_for_user(&public(root)), &cert).unwrap();
        record_machine(&paths, &machine(SOURCE)).unwrap();
        let store = FileStore {
            path: dir.path().join("vault"),
        };
        store.seal(&source, "model api key");
        Self {
            _dir: dir,
            paths,
            store,
            names: Names::default(),
            source,
        }
    }

    fn settle_on(
        &self,
        on: Option<&str>,
        after: &dyn Fn(Stage) -> io::Result<()>,
    ) -> Result<Settled, PersistError> {
        let current = on.map(machine);
        settle(
            &self.paths,
            current.as_ref(),
            &Branch {
                stores: vec![&self.store],
                machine: Some(&self.names),
                now_ns: NOW,
                after,
            },
        )
    }

    fn settle(&self, on: &str) -> Settled {
        self.settle_on(Some(on), &|_| Ok(())).unwrap()
    }

    fn device(&self) -> SigningKey {
        read_device_key(&self.paths.device_privkey).unwrap()
    }
}

fn branched(settled: Settled) -> Branched {
    match settled {
        Settled::Branched(branched) => *branched,
        other => panic!("not re-keyed: {other:?}"),
    }
}

#[test]
fn machine_identities_parse_and_match_in_either_byte_order() {
    let machine = machine(SOURCE);
    assert_eq!(Machine::parse(&SOURCE.to_uppercase()), Some(machine));
    assert_eq!(Machine::parse(&format!("{SOURCE}\n")), Some(machine));
    // Firmware without a UUID of its own reports a placeholder.
    assert_eq!(Machine::parse("00000000-0000-0000-0000-000000000000"), None);
    assert_eq!(Machine::parse("ffffffff-ffff-ffff-ffff-ffffffffffff"), None);
    assert_eq!(Machine::parse("Not Settable"), None);
    assert_eq!(Machine::parse(""), None);
    let swapped = Machine::parse("44454c4c-4200-1035-8048-b4c04f4d3232").unwrap();
    assert!(machine.is(&swapped));
    assert!(!machine.is(&Machine::parse(BRANCH).unwrap()));

    let dir = tempdir();
    assert_eq!(Machine::read(&dir.path().join("missing")).unwrap(), None);
    fs::write(dir.path().join("uuid"), format!("{BRANCH}\n")).unwrap();
    assert_eq!(
        Machine::read(&dir.path().join("uuid")).unwrap(),
        Some(Machine::parse(BRANCH).unwrap())
    );
}

/// The machine the key was set up on keeps its key; an install from before
/// the record gets the machine recorded and keeps its key too; a machine
/// that names none is never re-keyed.
#[test]
fn only_a_recorded_other_machine_re_keys() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    assert!(matches!(fixture.settle(SOURCE), Settled::Same));
    assert_eq!(fixture.device().to_bytes(), fixture.source.to_bytes());

    fs::remove_file(machine_record(&fixture.paths)).unwrap();
    assert!(matches!(fixture.settle(BRANCH), Settled::Recorded));
    assert_eq!(fixture.device().to_bytes(), fixture.source.to_bytes());
    assert!(matches!(fixture.settle(BRANCH), Settled::Same));

    fs::remove_file(machine_record(&fixture.paths)).unwrap();
    assert!(matches!(
        fixture.settle_on(None, &|_| Ok(())).unwrap(),
        Settled::NoMachine
    ));
    assert!(!machine_record(&fixture.paths).exists());
    assert_eq!(fixture.device().to_bytes(), fixture.source.to_bytes());
    assert_eq!(fixture.names.renewed.get(), 0);
    assert_eq!(
        fixture.store.open(&fixture.source).as_deref(),
        Some("model api key")
    );

    let empty = tempdir();
    let paths = IdentityPaths::under(empty.path());
    let settled = settle(
        &paths,
        Some(&machine(SOURCE)),
        &Branch {
            stores: vec![],
            machine: None,
            now_ns: NOW,
            after: &|_| Ok(()),
        },
    )
    .unwrap();
    assert!(matches!(settled, Settled::NoDevice));
}

/// A branch gets a key of its own, its vault opens under it with the same
/// secret, the old key is gone and the source's certificate with it.
#[test]
fn a_branch_becomes_a_new_device_with_its_secrets() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    let result = branched(fixture.settle(BRANCH));

    let device = fixture.device();
    assert_ne!(public(&device), public(&fixture.source));
    assert_eq!(result.device, public(&device));
    assert_eq!(result.previous, public(&fixture.source));
    assert!(result.finished());
    assert_eq!(result.stores, vec![("vault".into(), Resealed::Moved)]);
    assert_eq!(
        fixture.store.open(&device).as_deref(),
        Some("model api key")
    );
    assert_eq!(fixture.store.open(&fixture.source), None);
    assert!(!journal_path(&fixture.paths).exists(), "the old key stayed");
    assert_eq!(fixture.names.renewed.get(), 1);
    assert_eq!(
        result.label,
        format!("Studio branch {}", hex::encode(&result.device[..2]))
    );

    // No root here: the source's certificate is gone and the new device
    // waits, under its name, to be certified.
    assert!(matches!(result.certification, Certification::Pending(_)));
    assert!(!fixture.paths.device_cert.exists());
    assert!(!fixture.paths.device_cert_for_user(&public(&root)).exists());
    assert_eq!(pending_label(&fixture.paths).unwrap(), Some(result.label));
    // The person's public key stays: it is still theirs.
    assert_eq!(
        persist::read_public_key(&fixture.paths.user_pubkey).unwrap(),
        public(&root)
    );

    // Booting again on the branch is the branch's own machine now.
    assert!(matches!(fixture.settle(BRANCH), Settled::Same));
    assert_eq!(fixture.device().to_bytes(), device.to_bytes());

    // A branch of the branch is named after the machine the person named.
    let again = branched(fixture.settle(SOURCE));
    assert_eq!(
        again.label,
        format!("Branch {}", hex::encode(&again.device[..2]))
    );
}

/// A boot that stops after any stage resumes at the next one with the same
/// new key, and ends where an uninterrupted re-key ends.
#[test]
fn a_re_key_interrupted_anywhere_resumes() {
    for stop in [
        Stage::Journal,
        Stage::Stores,
        Stage::Key,
        Stage::Certificate,
        Stage::Machine,
        Stage::Record,
    ] {
        let root = SigningKey::from_bytes(&[7; 32]);
        let fixture = Fixture::new(&root);
        let crash = |stage: Stage| {
            if stage == stop {
                Err(io::Error::other("power lost"))
            } else {
                Ok(())
            }
        };
        let interrupted = fixture.settle_on(Some(BRANCH), &crash);
        assert!(interrupted.is_err(), "{stop:?}");
        let device_after_crash = fixture.device();

        if stop == Stage::Record {
            // Everything was done: this boot is the branch's own.
            assert!(matches!(fixture.settle(BRANCH), Settled::Same), "{stop:?}");
        } else {
            let resumed = branched(fixture.settle(BRANCH));
            assert!(resumed.finished(), "{stop:?}");
            if stop != Stage::Journal && stop != Stage::Stores {
                assert_eq!(resumed.device, public(&device_after_crash), "{stop:?}");
            }
        }
        let device = fixture.device();
        assert_ne!(public(&device), public(&fixture.source), "{stop:?}");
        assert_eq!(
            fixture.store.open(&device).as_deref(),
            Some("model api key"),
            "{stop:?}"
        );
        assert!(!journal_path(&fixture.paths).exists(), "{stop:?}");
        assert!(matches!(fixture.settle(BRANCH), Settled::Same), "{stop:?}");
    }
}

/// A store that cannot be re-sealed now keeps the old key in the journal
/// until a later boot re-seals it; the device acts as itself meanwhile.
#[test]
fn a_store_that_fails_keeps_the_old_key_until_it_is_resealed() {
    struct Unavailable<'a>(&'a FileStore, RefCell<bool>);
    impl SealedStore for Unavailable<'_> {
        fn name(&self) -> String {
            "vault".into()
        }
        fn reseal(&self, from: &[SigningKey], to: &SigningKey) -> Result<Resealed, String> {
            if *self.1.borrow() {
                return Err("home not mounted".into());
            }
            self.0.reseal(from, to)
        }
    }
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    let store = Unavailable(&fixture.store, RefCell::new(true));
    let run = || {
        settle(
            &fixture.paths,
            Some(&machine(BRANCH)),
            &Branch {
                stores: vec![&store],
                machine: None,
                now_ns: NOW,
                after: &|_| Ok(()),
            },
        )
        .unwrap()
    };
    let first = branched(run());
    assert!(!first.finished());
    assert_eq!(
        first.failed,
        vec![("vault".into(), "home not mounted".into())]
    );
    assert_eq!(public(&fixture.device()), first.device);
    assert!(journal_path(&fixture.paths).exists());
    assert_eq!(
        fixture.store.open(&fixture.source).as_deref(),
        Some("model api key")
    );

    *store.1.borrow_mut() = false;
    let second = branched(run());
    assert!(second.finished());
    assert_eq!(second.device, first.device);
    assert_eq!(
        fixture.store.open(&fixture.device()).as_deref(),
        Some("model api key")
    );
    assert!(!journal_path(&fixture.paths).exists());
}

/// A store under another identity's key is not touched.
#[test]
fn a_store_of_another_identity_is_left_alone() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    let stranger = SigningKey::from_bytes(&[42; 32]);
    fixture.store.seal(&stranger, "theirs");
    let before = fs::read(&fixture.store.path).unwrap();
    let result = branched(fixture.settle(BRANCH));
    assert!(result.finished());
    assert_eq!(result.stores, vec![("vault".into(), Resealed::Foreign)]);
    assert_eq!(fs::read(&fixture.store.path).unwrap(), before);
}

/// A copy of a machine taken while it was re-keying gets a key of its own
/// too, and finds its stores under either of that machine's keys.
#[test]
fn a_copy_of_a_machine_mid_re_key_gets_its_own_key() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    let crash = |stage: Stage| {
        if stage == Stage::Key {
            Err(io::Error::other("copied here"))
        } else {
            Ok(())
        }
    };
    assert!(fixture.settle_on(Some(BRANCH), &crash).is_err());
    let first_branch_key = fixture.device();
    assert_eq!(
        fixture.store.open(&first_branch_key).as_deref(),
        Some("model api key")
    );

    let copy = branched(fixture.settle(THIRD));
    let device = fixture.device();
    assert!(copy.finished());
    assert_ne!(public(&device), public(&first_branch_key));
    assert_ne!(public(&device), public(&fixture.source));
    assert_eq!(
        fixture.store.open(&device).as_deref(),
        Some("model api key")
    );
    assert!(matches!(fixture.settle(THIRD), Settled::Same));
}

/// A journal left on a machine that names none is left alone.
#[test]
fn a_journal_on_a_machine_without_identity_is_not_resumed() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    let crash = |stage: Stage| {
        if stage == Stage::Journal {
            Err(io::Error::other("stopped"))
        } else {
            Ok(())
        }
    };
    assert!(fixture.settle_on(Some(BRANCH), &crash).is_err());
    assert!(matches!(
        fixture.settle_on(None, &|_| Ok(())).unwrap(),
        Settled::NoMachine
    ));
    assert_eq!(fixture.device().to_bytes(), fixture.source.to_bytes());
    assert!(journal_path(&fixture.paths).exists());
}

/// With the person's root on the machine, the branch certifies itself with
/// what the source device was granted, under its new name.
#[test]
fn a_machine_holding_the_root_certifies_its_branch() {
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    persist::write_signing_key(&fixture.paths.user_privkey, &root).unwrap();
    let result = branched(fixture.settle(BRANCH));
    let Certification::Certified(cert) = &result.certification else {
        panic!("not certified: {:?}", result.certification);
    };
    verify_device_cert(cert).unwrap();
    assert_eq!(cert.user_public_key, public(&root));
    assert_eq!(cert.device_public_key, public(&fixture.device()));
    assert_eq!(cert.capabilities, DeviceCapabilities::member());
    assert_eq!(cert.device_label, result.label);
    assert_eq!(cert.expires_at_ns, None);
    assert_eq!(&read_device_cert(&fixture.paths.device_cert).unwrap(), cert);
    assert_eq!(
        &read_device_cert(&fixture.paths.device_cert_for_user(&public(&root))).unwrap(),
        cert
    );
    assert_eq!(pending_label(&fixture.paths).unwrap(), None);
}

/// A root on the machine that is not the device's person's signs nothing,
/// and neither does an unreadable one or a source certificate that does
/// not verify.
#[test]
fn nothing_but_the_persons_own_root_certifies_a_branch() {
    // Another person's root.
    let root = SigningKey::from_bytes(&[7; 32]);
    let fixture = Fixture::new(&root);
    let stranger = SigningKey::from_bytes(&[9; 32]);
    persist::write_signing_key(&fixture.paths.user_privkey, &stranger).unwrap();
    let result = branched(fixture.settle(BRANCH));
    assert!(matches!(result.certification, Certification::Pending(_)));
    assert!(!fixture.paths.device_cert.exists());
    assert!(
        !fixture
            .paths
            .device_cert_for_user(&stranger.verifying_key().to_bytes())
            .exists()
    );

    // A root anyone could read.
    let fixture = Fixture::new(&root);
    persist::write_signing_key(&fixture.paths.user_privkey, &root).unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(
            &fixture.paths.user_privkey,
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
    }
    let result = branched(fixture.settle(BRANCH));
    assert!(matches!(result.certification, Certification::Pending(_)));
    assert!(!fixture.paths.device_cert.exists());

    // A source certificate that does not verify grants nothing to copy.
    let fixture = Fixture::new(&root);
    persist::write_signing_key(&fixture.paths.user_privkey, &root).unwrap();
    let mut forged = read_device_cert(&fixture.paths.device_cert).unwrap();
    forged.capabilities = DeviceCapabilities::primary();
    persist::write_device_cert(&fixture.paths.device_cert, &forged).unwrap();
    let result = branched(fixture.settle(BRANCH));
    assert!(matches!(result.certification, Certification::Pending(_)));
    assert!(!fixture.paths.device_cert.exists());
}

#[test]
fn branch_labels_fit_a_certificate() {
    let key = SigningKey::from_bytes(&[1; 32]);
    let suffix = hex::encode(&key.verifying_key().to_bytes()[..2]);
    assert_eq!(relabel("Studio", &key), format!("Studio branch {suffix}"));
    assert_eq!(
        relabel("Studio branch 1a2b", &key),
        format!("Studio branch {suffix}")
    );
    assert_eq!(relabel("", &key), format!("Branch {suffix}"));
    let long = "é".repeat(60);
    let label = relabel(&long, &key);
    assert!(label.len() <= MAX_LABEL_BYTES);
    assert!(label.ends_with(&suffix));
}

#[test]
fn keys_hand_over_to_a_re_sealer_exactly() {
    let from = [
        SigningKey::from_bytes(&[1; 32]),
        SigningKey::from_bytes(&[2; 32]),
    ];
    let to = SigningKey::from_bytes(&[3; 32]);
    let (taken, new) = take_over(&hand_over(&from, &to)).unwrap();
    assert_eq!(
        taken.iter().map(SigningKey::to_bytes).collect::<Vec<_>>(),
        vec![[1; 32], [2; 32]]
    );
    assert_eq!(new.to_bytes(), [3; 32]);
    let bytes = hand_over(&from, &to);
    assert!(take_over(&bytes[..bytes.len().saturating_sub(1)]).is_err());
    assert!(take_over(&[0]).is_err());
    assert!(take_over(&[]).is_err());
    for outcome in [
        Resealed::Moved,
        Resealed::AlreadyNew,
        Resealed::Absent,
        Resealed::Foreign,
    ] {
        assert_eq!(Resealed::parse(outcome.as_str()), Some(outcome));
    }
    assert_eq!(Resealed::parse("unlocked"), None);
}
