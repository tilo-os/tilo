use std::sync::{Arc, Mutex as StdMutex};

use tilo_device_identity::persist::IdentityPaths;
use tilo_seal::secret_service::{CallerKey, open_vault, seal_vault};

use super::*;

/// A stand-in for the device's daemon, which alone keeps the device key: it
/// seals, opens and checks proofs with the key it is given (none while the
/// device has no identity), under the person's salt and account.
#[derive(Clone)]
struct TestDaemon {
    key: Arc<StdMutex<Option<SigningKey>>>,
    salt: PathBuf,
    account: String,
}

impl TestDaemon {
    /// The daemon of a device whose key is `key`, for the person whose
    /// salt is in `secrets`.
    fn new(secrets: &Path, key: Option<SigningKey>, account: &str) -> Self {
        Self {
            key: Arc::new(StdMutex::new(key)),
            salt: secrets.join("salt"),
            account: account.to_owned(),
        }
    }

    /// The daemon of a device keyed by `seed`, for the store `store_at` makes in `dir`.
    fn seeded(dir: &Path, seed: Option<u8>) -> Self {
        Self::new(
            &dir.join("secrets"),
            seed.map(|seed| SigningKey::from_bytes(&[seed; 32])),
            "1000",
        )
    }

    fn set(&self, seed: u8) {
        *self.key.lock().unwrap() = Some(SigningKey::from_bytes(&[seed; 32]));
    }

    fn seed(&self) -> std::result::Result<Zeroizing<[u8; 32]>, Locked> {
        let bytes = self.key.lock().unwrap().as_ref().map(SigningKey::to_bytes);
        bytes.map(Zeroizing::new).ok_or(Locked::NoIdentity)
    }

    fn salt(&self) -> Zeroizing<[u8; 32]> {
        load_or_create_salt(&self.salt).unwrap()
    }
}

impl KeySource for TestDaemon {
    fn seal(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, Locked> {
        let seed = self.seed()?;
        Ok(seal_vault(seed.as_ref(), &self.salt(), self.account.as_bytes(), plain).unwrap())
    }

    fn open(&self, sealed: &[u8]) -> std::result::Result<Zeroizing<Vec<u8>>, Locked> {
        let seed = self.seed()?;
        open_vault(seed.as_ref(), &self.salt(), self.account.as_bytes(), sealed).map_err(|error| {
            match error {
                tilo_seal::Error::WrongKey => Locked::KeyChanged,
                _ => Locked::Corrupt,
            }
        })
    }

    fn check_caller(
        &self,
        challenge: &[u8],
        caller: &str,
        program: &[u8],
        proof: &[u8],
    ) -> std::result::Result<bool, Locked> {
        let seed = self.seed()?;
        Ok(CallerKey::derive(seed.as_ref())
            .unwrap()
            .verify(challenge, caller, program, proof))
    }
}

fn app(id: &str) -> CallerId {
    CallerId::parse(id).unwrap()
}

fn system() -> CallerId {
    app("binary:/usr/bin/chromium")
}

fn local() -> CallerId {
    app("local-process:/home/me/tool")
}

fn attributes(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn new_item(label: &str, pairs: &[(&str, &str)], secret: &[u8]) -> NewItem {
    NewItem {
        label: label.into(),
        attributes: attributes(pairs),
        secret: secret.to_vec(),
        content_type: "text/plain".into(),
    }
}

fn store_at(dir: &Path, keys: &TestDaemon) -> Store {
    Store::new(dir.join("secrets/vault.tseal"), Box::new(keys.clone()))
}

tilo_test_env::scenario! {
    fn the_store_keeps_items_sealed_under_the_identitys_key;

    phase a_locked_store_writes_nothing_and_opens_once_the_identity_is_there;
    {
        let dir = tempfile::tempdir().unwrap();
        let keys = TestDaemon::seeded(dir.path(), None);
        let store = store_at(dir.path(), &keys);

        assert_eq!(store.locked(), Some(Locked::NoIdentity));
        let refused = store.create_item(
            &system(),
            "default",
            new_item("a", &[("k", "v")], b"s"),
            true,
        );
        assert!(matches!(
            refused,
            Err(StoreError::Locked(Locked::NoIdentity))
        ));
        assert!(matches!(
            store.search(&system(), None, &BTreeMap::new()),
            Err(StoreError::Locked(_))
        ));
        assert!(
            !dir.path().join("secrets/vault.tseal").exists(),
            "a locked store writes nothing"
        );

        // The identity becomes available (any login method): the same store
        // object serves, with no unlock step.
        keys.set(7);
        assert_eq!(store.locked(), None);
        store
            .create_item(
                &system(),
                "default",
                new_item("a", &[("k", "v")], b"s"),
                true,
            )
            .unwrap();
    }

    phase items_persist_sealed_and_open_only_under_the_same_key;
    {
        let dir = tempfile::tempdir().unwrap();
        let keys = TestDaemon::seeded(dir.path(), Some(7));
        let first = store_at(dir.path(), &keys);
        let made = first
            .create_item(
                &system(),
                "default",
                new_item("Mail", &[("service", "imap")], b"correct horse"),
                true,
            )
            .unwrap();

        let path = dir.path().join("secrets/vault.tseal");
        let disk = std::fs::read(&path).unwrap();
        assert!(tilo_seal::envelope::is_envelope(&disk));
        assert!(!disk.windows(13).any(|w| w == b"correct horse"));
        assert!(!disk.windows(4).any(|w| w == b"Mail"));
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let again = store_at(dir.path(), &keys);
        let found = again
            .search(
                &system(),
                Some("default"),
                &attributes(&[("service", "imap")]),
            )
            .unwrap();
        assert_eq!(found, vec![made.clone()]);
        let (secret, content_type) = again.read_secret(&system(), &made).unwrap();
        assert_eq!(secret.expose(), b"correct horse");
        assert_eq!(content_type, "text/plain");

        let other = TestDaemon::seeded(dir.path(), Some(9));
        let foreign = store_at(dir.path(), &other);
        assert_eq!(foreign.locked(), Some(Locked::KeyChanged));
        assert!(
            foreign
                .create_item(&system(), "default", new_item("x", &[], b"y"), false)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), disk);

        // Damage is a distinct, non-destructive state.
        let mut damaged = disk;
        if let Some(last) = damaged.last_mut() {
            *last ^= 1;
        }
        std::fs::write(&path, &damaged).unwrap();
        assert_eq!(store_at(dir.path(), &keys).locked(), Some(Locked::Corrupt));
        assert_eq!(std::fs::read(&path).unwrap(), damaged);
    }

    phase search_lookup_replace_and_delete_work_like_libsecret_expects;
    {
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path(), &TestDaemon::seeded(dir.path(), Some(7)));
        let me = system();
        let one = store
            .create_item(
                &me,
                "default",
                new_item("one", &[("a", "1"), ("b", "x")], b"p1"),
                true,
            )
            .unwrap();
        let two = store
            .create_item(
                &me,
                "default",
                new_item("two", &[("a", "2"), ("b", "x")], b"p2"),
                true,
            )
            .unwrap();
        assert_ne!(one.id, two.id);

        let both = store.search(&me, None, &attributes(&[("b", "x")])).unwrap();
        assert_eq!(both.len(), 2);
        assert_eq!(
            store.search(&me, None, &attributes(&[("a", "2")])).unwrap(),
            vec![two]
        );
        assert!(
            store
                .search(&me, None, &attributes(&[("a", "3")]))
                .unwrap()
                .is_empty()
        );

        let replaced = store
            .create_item(
                &me,
                "default",
                new_item("one!", &[("a", "1"), ("b", "x")], b"p1b"),
                true,
            )
            .unwrap();
        assert_eq!(replaced, one);
        assert_eq!(store.read_secret(&me, &one).unwrap().0.expose(), b"p1b");
        assert_eq!(store.item_info(&me, &one).unwrap().label, "one!");
        let third = store
            .create_item(
                &me,
                "default",
                new_item("one", &[("a", "1"), ("b", "x")], b"p1c"),
                false,
            )
            .unwrap();
        assert_ne!(third, one);

        store.delete_item(&me, &one).unwrap();
        assert!(matches!(
            store.read_secret(&me, &one),
            Err(StoreError::NoSuchObject)
        ));
        assert!(matches!(
            store.delete_item(&me, &one),
            Err(StoreError::NoSuchObject)
        ));
    }

    phase the_session_collection_is_not_kept_on_disk;
    {
        let dir = tempfile::tempdir().unwrap();
        let keys = TestDaemon::seeded(dir.path(), Some(7));
        let first = store_at(dir.path(), &keys);
        let me = system();
        let kept = first
            .create_item(&me, "default", new_item("k", &[("n", "k")], b"1"), true)
            .unwrap();
        let temp = first
            .create_item(&me, "session", new_item("t", &[("n", "t")], b"2"), true)
            .unwrap();
        assert_eq!(temp.collection, SESSION_COLLECTION);
        assert_eq!(first.read_secret(&me, &temp).unwrap().0.expose(), b"2");

        let second = store_at(dir.path(), &keys);
        assert!(second.read_secret(&me, &kept).is_ok());
        assert!(matches!(
            second.read_secret(&me, &temp),
            Err(StoreError::NoSuchObject)
        ));
    }

    phase with_no_device_daemon_the_store_is_locked_saying_so_and_writes_nothing;
    // The device key is the daemon's alone: with nothing listening where the
    // daemon is reached, the store is locked with the daemon's refusal, and
    // no proof can be checked. Nothing else is asked for the key.
    {
        let dir = tempfile::tempdir().unwrap();
        let secrets = dir.path().join("home/secrets");
        let nobody = DeviceActions::at(dir.path().join("sockets/native.sock"));
        let store = Store::new(
            secrets.join("vault.tseal"),
            Box::new(DeviceDaemon::new(nobody.clone(), secrets.join("salt"), "1000")),
        );

        let Some(Locked::NotRunning(why)) = store.locked() else {
            panic!("not locked as the daemon not running: {:?}", store.locked());
        };
        assert!(
            why.starts_with("the device service isn't running on this computer"),
            "{why}"
        );
        assert!(
            why.contains(&dir.path().join("sockets/native.sock").display().to_string()),
            "{why}"
        );
        assert!(matches!(
            store.create_item(&system(), "default", new_item("a", &[], b"s"), true),
            Err(StoreError::Locked(Locked::NotRunning(_)))
        ));
        assert!(matches!(
            store.check_caller(&[1; 32], ":1.7", b"/usr/bin/tilo", &[2; 32]),
            Err(Locked::NotRunning(_))
        ));
        assert!(!secrets.join("vault.tseal").exists(), "a locked store writes nothing");

        // A vault already on disk stays as it is.
        std::fs::write(secrets.join("vault.tseal"), b"sealed elsewhere").unwrap();
        assert!(matches!(store.locked(), Some(Locked::NotRunning(_))));
        assert_eq!(std::fs::read(secrets.join("vault.tseal")).unwrap(), b"sealed elsewhere");

        // A salt that is present but damaged is never replaced, and is said
        // before the daemon is asked.
        std::fs::write(secrets.join("salt"), b"short").unwrap();
        let damaged = Store::new(
            secrets.join("vault.tseal"),
            Box::new(DeviceDaemon::new(nobody, secrets.join("salt"), "1000")),
        );
        assert!(matches!(damaged.locked(), Some(Locked::Unreadable(_))));
        assert_eq!(std::fs::read(secrets.join("salt")).unwrap(), b"short");
    }

    phase a_branched_machine_keeps_every_persons_secrets_under_its_new_key;
    // A branched machine re-keys its device identity: each person's vault opens
    // under the new key with every item it held, the old key no longer opens
    // it, and a vault the old key never sealed is left as it was.
    {
        use tilo_device_identity::branch::{self, Branch, Machine, Resealed, SealedStore as _, Settled};
        use tilo_device_identity::persist::{load_or_create_device_key, read_device_key};

        let dir = tempfile::tempdir().unwrap();
        let paths = IdentityPaths::under(dir.path().join("system"));
        let (source, _) = load_or_create_device_key(&paths).unwrap();
        let source_machine = Machine::parse("4c4c4544-0042-3510-8048-b4c04f4d3232").unwrap();
        branch::record_machine(&paths, &source_machine).unwrap();

        let (store, ada) = person_vault(&dir.path().join("home/ada"), &source, "1000");
        // The key of the accounts vault the passwords app keeps its entries in,
        // and a model provider's API key a provider profile refers to.
        let passwords = store
            .create_item(
                &system(),
                "default",
                new_item(
                    "Accounts vault",
                    &[("application", "so.tilo.accounts")],
                    b"vault key",
                ),
                true,
            )
            .unwrap();
        let provider = store
            .create_item(
                &system(),
                "default",
                new_item("Model provider", &[("locator", "anthropic")], b"sk-ant-api"),
                true,
            )
            .unwrap();
        drop(store);

        let elsewhere = SigningKey::from_bytes(&[5; 32]);
        let (stranger, bea) = person_vault(&dir.path().join("home/bea"), &elsewhere, "1001");
        stranger
            .create_item(&system(), "default", new_item("theirs", &[], b"s"), true)
            .unwrap();
        drop(stranger);
        let theirs = std::fs::read(&bea.vault).unwrap();
        let (_, nobody) = person_vault(&dir.path().join("home/cy"), &source, "1002");

        let settled = branch::settle(
            &paths,
            Machine::parse("8a1f02d6-91c4-4f0e-9b9a-2d1f5c3e7a10").as_ref(),
            &Branch {
                stores: vec![&ada, &bea, &nobody],
                machine: None,
                now_ns: 1_800_000_000_000_000_000,
                after: &|_| Ok(()),
            },
        )
        .unwrap();
        let Settled::Branched(branched) = settled else {
            panic!("not re-keyed: {settled:?}");
        };
        assert!(branched.finished());
        let outcomes: Vec<Resealed> = branched
            .stores
            .iter()
            .map(|(_, outcome)| *outcome)
            .collect();
        assert_eq!(
            outcomes,
            vec![Resealed::Moved, Resealed::Foreign, Resealed::Absent]
        );
        let device = read_device_key(&paths.device_privkey).unwrap();
        assert_ne!(device.to_bytes(), source.to_bytes());

        let (store, _) = person_vault(&dir.path().join("home/ada"), &device, "1000");
        assert_eq!(store.locked(), None);
        let (secret, _) = store.read_secret(&system(), &passwords).unwrap();
        assert_eq!(secret.expose(), b"vault key");
        let (secret, _) = store.read_secret(&system(), &provider).unwrap();
        assert_eq!(secret.expose(), b"sk-ant-api");
        assert_eq!(std::fs::read(&bea.vault).unwrap(), theirs);

        assert_eq!(ada.reseal(&[], &source).unwrap(), Resealed::Foreign);
        assert_eq!(ada.reseal(&[], &device).unwrap(), Resealed::AlreadyNew);
    }
}

tilo_test_env::scenario! {
    fn applications_are_kept_apart_recorded_and_permanent;

    phase an_imported_item_belongs_to_the_system_not_to_a_program_the_person_runs;
    {
        // Which application sees which item, and revocation, over the real
        // D-Bus interface: tests/dbus.rs,
        // `applications_are_attributed_isolated_and_revocable`.
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path(), &TestDaemon::seeded(dir.path(), Some(7)));
        let legacy = LegacyItem {
            label: "Chrome Safe Storage".into(),
            attributes: attributes(&[("application", "chrome")]),
            secret: b"old".to_vec(),
            content_type: "text/plain".into(),
            created: 1,
            modified: 1,
        };
        assert_eq!(store.import("gnome-keyring", vec![legacy]).unwrap(), 1);
        let found = store
            .search(&system(), None, &attributes(&[("application", "chrome")]))
            .unwrap();
        assert_eq!(found.len(), 1);
        assert!(
            store
                .search(&local(), None, &attributes(&[("application", "chrome")]))
                .unwrap()
                .is_empty(),
            "an application the person merely runs does not read the keyring's items"
        );
    }

    phase every_application_is_recorded_counted_and_kept_across_a_restart;
    {
        let dir = tempfile::tempdir().unwrap();
        let keys = TestDaemon::seeded(dir.path(), Some(7));
        let store = store_at(dir.path(), &keys);
        store
            .create_item(
                &system(),
                "default",
                new_item("c", &[("a", "1")], b"s"),
                true,
            )
            .unwrap();
        store.search(&local(), None, &BTreeMap::new()).unwrap();
        store.search(&local(), None, &BTreeMap::new()).unwrap();

        let record = store
            .apps()
            .unwrap()
            .into_iter()
            .find(|row| row.id == local())
            .unwrap();
        assert_eq!(record.uses, 2, "every use is counted");
        assert_eq!(
            store_at(dir.path(), &keys).apps().unwrap().len(),
            2,
            "the records are kept across a restart"
        );
        assert!(matches!(
            store.set_access(&app("binary:/usr/bin/nothing"), Access::Revoked),
            Err(StoreError::NoSuchObject)
        ));
    }

    phase identical_imported_items_collapse_into_one;
    {
        // An import runs once and never again (tests/migrate.rs); this is the
        // old keyring's own duplicates.
        let dir = tempfile::tempdir().unwrap();
        let keys = TestDaemon::seeded(dir.path(), Some(7));
        let store = store_at(dir.path(), &keys);
        let legacy = |secret: &[u8]| LegacyItem {
            label: "Wi-Fi".into(),
            attributes: attributes(&[("xdg:schema", "org.gnome.NetworkManager.Connection")]),
            secret: secret.to_vec(),
            content_type: "text/plain".into(),
            created: 5,
            modified: 6,
        };
        assert_eq!(
            store
                .import("gnome-keyring", vec![legacy(b"a"), legacy(b"a")])
                .unwrap(),
            2
        );
        let state = store.migration().unwrap().unwrap();
        assert_eq!(
            (state.source.as_str(), state.imported),
            ("gnome-keyring", 2)
        );
        assert_eq!(
            store.import("gnome-keyring", vec![legacy(b"b")]).unwrap(),
            0
        );
        let found = store_at(dir.path(), &keys)
            .search(
                &system(),
                None,
                &attributes(&[("xdg:schema", "org.gnome.NetworkManager.Connection")]),
            )
            .unwrap();
        assert_eq!(found.len(), 1, "the two identical items collapsed into one");
        assert_eq!(
            store_at(dir.path(), &keys)
                .read_secret(&system(), &found[0])
                .unwrap()
                .0
                .expose(),
            b"a"
        );
    }

    phase a_sandbox_name_is_checked_before_it_becomes_an_application;
    {
        // Its stability, separation and revocation over the real interface:
        // tests/dbus.rs,
        // `the_portal_host_gets_sandbox_secrets_from_the_same_store`.
        let dir = tempfile::tempdir().unwrap();
        let store = store_at(dir.path(), &TestDaemon::seeded(dir.path(), Some(7)));
        assert!(store.sandbox_secret("").is_err());
        assert!(store.sandbox_secret("a/b").is_err());
    }
}

/// A person's vault in `home`, sealed by a daemon keeping the device key
/// `key`, and the same vault as a branched machine's re-key sees it.
fn person_vault(home: &Path, key: &SigningKey, account: &str) -> (Store, DeviceVault) {
    let secrets = home.join(".local/share/tilo/secrets");
    let store = Store::new(
        secrets.join("vault.tseal"),
        Box::new(TestDaemon::new(&secrets, Some(key.clone()), account)),
    );
    let vault = DeviceVault {
        vault: secrets.join("vault.tseal"),
        salt: secrets.join("salt"),
        account: account.into(),
    };
    (store, vault)
}
