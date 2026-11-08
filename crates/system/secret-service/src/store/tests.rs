use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use super::*;

/// A key source the test controls.
#[derive(Clone)]
struct Switch {
    state: Arc<AtomicU8>,
}

impl Switch {
    const ABSENT: u8 = 0;

    fn new(state: u8) -> Self {
        Self {
            state: Arc::new(AtomicU8::new(state)),
        }
    }

    fn set(&self, state: u8) {
        self.state.store(state, Ordering::SeqCst);
    }
}

impl KeySource for Switch {
    fn release(&self) -> std::result::Result<SymmetricKey, Locked> {
        match self.state.load(Ordering::SeqCst) {
            Self::ABSENT => Err(Locked::NoIdentity),
            byte => Ok(SymmetricKey::from_bytes([byte; 32])),
        }
    }
}

fn app(id: &str) -> AppId {
    AppId::parse(id).unwrap()
}

fn system() -> AppId {
    app("binary:/usr/bin/chromium")
}

fn other_system() -> AppId {
    app("binary:/usr/bin/1password")
}

fn local() -> AppId {
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

fn store_at(dir: &Path, keys: &Switch) -> Store {
    Store::new(dir.join("secrets/vault.tseal"), Box::new(keys.clone()))
}

#[test]
fn the_store_is_locked_until_the_identity_is_there_and_then_needs_no_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Switch::new(Switch::ABSENT);
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

#[test]
fn items_persist_sealed_and_open_only_under_the_same_key() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Switch::new(7);
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

    // Another device identity: locked, and the file is left untouched.
    let other = Switch::new(9);
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

#[test]
fn search_lookup_replace_and_delete_work_like_libsecret_expects() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path(), &Switch::new(7));
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
    // Without replace it adds.
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

#[test]
fn the_session_collection_is_not_kept_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Switch::new(7);
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

#[test]
fn an_application_sees_only_what_it_made_or_the_system_shared() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(dir.path(), &Switch::new(7));
    let (chromium, onepassword, tool) = (system(), other_system(), local());

    let mine = store
        .create_item(
            &chromium,
            "default",
            new_item("c", &[("app", "c")], b"cs"),
            true,
        )
        .unwrap();
    // Another application, same attributes: a different item, not a hit.
    assert!(
        store
            .search(&onepassword, None, &attributes(&[("app", "c")]))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.read_secret(&onepassword, &mine),
        Err(StoreError::NoSuchObject)
    ));
    assert!(matches!(
        store.delete_item(&tool, &mine),
        Err(StoreError::NoSuchObject)
    ));
    // A replace by another app does not overwrite the first app's item.
    let theirs = store
        .create_item(
            &onepassword,
            "default",
            new_item("o", &[("app", "c")], b"os"),
            true,
        )
        .unwrap();
    assert_ne!(theirs, mine);
    assert_eq!(
        store.read_secret(&chromium, &mine).unwrap().0.expose(),
        b"cs"
    );

    // Items brought from the old keyring: system apps read them, an
    // application the person merely runs does not.
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
        .search(&chromium, None, &attributes(&[("application", "chrome")]))
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(
        store
            .search(&tool, None, &attributes(&[("application", "chrome")]))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn every_application_is_recorded_and_can_be_revoked_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Switch::new(7);
    let store = store_at(dir.path(), &keys);
    let (chromium, tool) = (system(), local());
    store
        .create_item(
            &chromium,
            "default",
            new_item("c", &[("a", "1")], b"s"),
            true,
        )
        .unwrap();
    store.search(&tool, None, &BTreeMap::new()).unwrap();
    store.search(&tool, None, &BTreeMap::new()).unwrap();

    let apps = store.apps().unwrap();
    assert_eq!(apps.len(), 2);
    let record = apps.iter().find(|row| row.id == tool).unwrap();
    assert_eq!(record.uses, 2);
    assert_eq!(record.access, Access::Allowed);
    assert_eq!(apps.iter().find(|row| row.id == chromium).unwrap().items, 1);
    assert!(record.name.contains("not part of the system"));

    // Recorded on disk: a new process sees the ledger.
    assert_eq!(store_at(dir.path(), &keys).apps().unwrap().len(), 2);

    store.set_access(&chromium, Access::Revoked).unwrap();
    assert!(matches!(
        store.search(&chromium, None, &BTreeMap::new()),
        Err(StoreError::Revoked)
    ));
    assert!(matches!(
        store.create_item(&chromium, "default", new_item("c", &[], b"s"), false),
        Err(StoreError::Revoked)
    ));
    // Others are unaffected, and revocation survives a restart.
    assert!(store.search(&tool, None, &BTreeMap::new()).is_ok());
    assert!(matches!(
        store_at(dir.path(), &keys).search(&chromium, None, &BTreeMap::new()),
        Err(StoreError::Revoked)
    ));

    store.set_access(&chromium, Access::Allowed).unwrap();
    assert_eq!(
        store
            .search(&chromium, None, &BTreeMap::new())
            .unwrap()
            .len(),
        1
    );

    store.forget_app(&chromium).unwrap();
    assert!(
        store
            .search(&chromium, None, &BTreeMap::new())
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        store.set_access(&app("binary:/usr/bin/nothing"), Access::Revoked),
        Err(StoreError::NoSuchObject)
    ));
}

#[test]
fn migration_runs_once_and_never_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Switch::new(7);
    let store = store_at(dir.path(), &keys);
    let legacy = |secret: &[u8]| LegacyItem {
        label: "Wi-Fi".into(),
        attributes: attributes(&[("xdg:schema", "org.gnome.NetworkManager.Connection")]),
        secret: secret.to_vec(),
        content_type: "text/plain".into(),
        created: 5,
        modified: 6,
    };
    assert!(store.migration().unwrap().is_none());
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

    // A second import, here or after a restart, changes nothing.
    assert_eq!(
        store.import("gnome-keyring", vec![legacy(b"b")]).unwrap(),
        0
    );
    let restarted = store_at(dir.path(), &keys);
    assert_eq!(
        restarted
            .import("gnome-keyring", vec![legacy(b"c")])
            .unwrap(),
        0
    );
    let found = restarted
        .search(
            &system(),
            None,
            &attributes(&[("xdg:schema", "org.gnome.NetworkManager.Connection")]),
        )
        .unwrap();
    // The two identical items collapsed into one.
    assert_eq!(found.len(), 1);
    assert_eq!(
        restarted
            .read_secret(&system(), &found[0])
            .unwrap()
            .0
            .expose(),
        b"a"
    );
}

#[test]
fn sandbox_master_secrets_are_stable_separate_and_revocable() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Switch::new(7);
    let store = store_at(dir.path(), &keys);
    let one = store.sandbox_secret("so.tilo.notes", None).unwrap();
    assert_eq!(
        *one,
        *store_at(dir.path(), &keys)
            .sandbox_secret("so.tilo.notes", None)
            .unwrap()
    );
    assert_ne!(*one, *store.sandbox_secret("so.tilo.chat", None).unwrap());
    assert!(store.sandbox_secret("", None).is_err());
    assert!(store.sandbox_secret("a/b", None).is_err());

    // A secret an earlier portal store kept is adopted once, then ignored.
    assert_eq!(
        *store
            .sandbox_secret("so.tilo.adopt", Some(&[9; 32]))
            .unwrap(),
        [9; 32]
    );
    assert_eq!(
        *store
            .sandbox_secret("so.tilo.adopt", Some(&[1; 32]))
            .unwrap(),
        [9; 32]
    );

    let id = AppId::sandbox("so.tilo.notes").unwrap();
    assert!(store.apps().unwrap().iter().any(|row| row.id == id));
    store.set_access(&id, Access::Revoked).unwrap();
    assert!(matches!(
        store.sandbox_secret("so.tilo.notes", None),
        Err(StoreError::Revoked)
    ));
    // Forgetting drops the secret: the next one is a different secret.
    store.forget_app(&id).unwrap();
    assert_ne!(*one, *store.sandbox_secret("so.tilo.notes", None).unwrap());
}

#[test]
fn the_device_identity_releases_a_key_bound_to_device_person_and_salt() {
    use tilo_user_identity::persist::load_or_create_device_key;
    let dir = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(dir.path().join("system"));
    let salt = dir.path().join("home/secrets/salt");
    let keys = DeviceIdentityKey::new(paths.clone(), salt.clone(), "1000");

    // Before enrolment: no identity, no salt file minted.
    assert_eq!(keys.release().err(), Some(Locked::NoIdentity));
    assert!(!salt.exists());

    load_or_create_device_key(&paths).unwrap();
    let first = keys.release().unwrap();
    assert_eq!(
        first.expose_secret(),
        keys.release().unwrap().expose_secret()
    );
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&salt).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // Another person on the same device, or another salt, derives another key.
    let other_person = DeviceIdentityKey::new(paths.clone(), salt.clone(), "1001");
    assert_ne!(
        first.expose_secret(),
        other_person.release().unwrap().expose_secret()
    );
    let other_home = DeviceIdentityKey::new(paths, dir.path().join("elsewhere/salt"), "1000");
    assert_ne!(
        first.expose_secret(),
        other_home.release().unwrap().expose_secret()
    );

    // A salt that is present but damaged is never replaced.
    std::fs::write(&salt, b"short").unwrap();
    assert!(matches!(keys.release(), Err(Locked::Unreadable(_))));
    assert_eq!(std::fs::read(&salt).unwrap(), b"short");
}

/// A person's vault in `home` under the device key `paths` holds, and the
/// same vault as a branched machine's re-key sees it.
fn person_vault(home: &Path, paths: &IdentityPaths, account: &str) -> (Store, DeviceVault) {
    let secrets = home.join(".local/share/tilo/secrets");
    let store = Store::new(
        secrets.join("vault.tseal"),
        Box::new(DeviceIdentityKey::new(
            paths.clone(),
            secrets.join("salt"),
            account,
        )),
    );
    let vault = DeviceVault {
        vault: secrets.join("vault.tseal"),
        salt: secrets.join("salt"),
        account: account.into(),
    };
    (store, vault)
}

/// A branched machine re-keys its device identity: each person's vault opens
/// under the new key with every item it held, the old key no longer opens
/// it, and a vault the old key never sealed is left as it was.
#[test]
fn a_branched_machine_keeps_every_persons_secrets_under_its_new_key() {
    use tilo_user_identity::branch::{self, Branch, Machine, Resealed, SealedStore as _, Settled};
    use tilo_user_identity::persist::{load_or_create_device_key, read_device_key};

    let dir = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(dir.path().join("system"));
    let (source, _) = load_or_create_device_key(&paths).unwrap();
    let source_machine = Machine::parse("4c4c4544-0042-3510-8048-b4c04f4d3232").unwrap();
    branch::record_machine(&paths, &source_machine).unwrap();

    let (store, ada) = person_vault(&dir.path().join("home/ada"), &paths, "1000");
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

    // Another person's vault on the same disk, sealed under another device.
    let elsewhere = IdentityPaths::under(dir.path().join("elsewhere"));
    load_or_create_device_key(&elsewhere).unwrap();
    let (stranger, bea) = person_vault(&dir.path().join("home/bea"), &elsewhere, "1001");
    stranger
        .create_item(&system(), "default", new_item("theirs", &[], b"s"), true)
        .unwrap();
    drop(stranger);
    let theirs = std::fs::read(&bea.vault).unwrap();
    let (_, nobody) = person_vault(&dir.path().join("home/cy"), &paths, "1002");

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

    let (store, _) = person_vault(&dir.path().join("home/ada"), &paths, "1000");
    assert_eq!(store.locked(), None);
    let (secret, _) = store.read_secret(&system(), &passwords).unwrap();
    assert_eq!(secret.expose(), b"vault key");
    let (secret, _) = store.read_secret(&system(), &provider).unwrap();
    assert_eq!(secret.expose(), b"sk-ant-api");
    assert_eq!(std::fs::read(&bea.vault).unwrap(), theirs);

    // The source's key opens the vault no more; the new one does.
    assert_eq!(ada.reseal(&[], &source).unwrap(), Resealed::Foreign);
    assert_eq!(ada.reseal(&[], &device).unwrap(), Resealed::AlreadyNew);
}
