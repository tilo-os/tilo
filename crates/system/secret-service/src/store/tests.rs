use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use ed25519_dalek::SigningKey;

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

    fn another(&self) -> Box<dyn KeySource> {
        Box::new(self.clone())
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
fn the_persons_key_seals_the_vault_and_opens_it_without_this_device() {
    use tilo_user_identity::persist::{IdentityPaths, write_signing_key};
    let dir = tempfile::tempdir().unwrap();
    let persons = |root: &Path| IdentityPaths::under(root);
    let home = persons(&dir.path().join("home"));
    let away = persons(&dir.path().join("away"));
    let person = SigningKey::from_bytes(&[4; 32]);
    for paths in [&home, &away] {
        paths.ensure_directories().unwrap();
        std::fs::write(&paths.user_pubkey, person.verifying_key().to_bytes()).unwrap();
    }
    let salt = dir.path().join("data/salt");
    let key_of = |paths: &IdentityPaths| {
        Box::new(UserKey::new(
            paths.clone(),
            salt.clone(),
            Some(person.verifying_key().to_bytes()),
        ))
    };

    // No private half yet: the machine that cannot reach the person's key
    // holds the vault shut and mints no salt for a store it cannot open.
    let store = Store::new(dir.path().join("data/vault.tseal"), key_of(&home));
    assert_eq!(store.locked(), Some(Locked::NoIdentity));
    assert!(!salt.exists());

    write_signing_key(&home.user_privkey, &person).unwrap();
    let made = store
        .create_item(
            &system(),
            "default",
            new_item("Mail", &[("service", "imap")], b"correct horse"),
            true,
        )
        .unwrap();

    // The same person's key on another machine, with the same salt and the
    // same data directory, opens the very same vault: it belongs to them,
    // not to the device that wrote it.
    write_signing_key(&away.user_privkey, &person).unwrap();
    let elsewhere = Store::new(dir.path().join("data/vault.tseal"), key_of(&away));
    assert_eq!(elsewhere.locked(), None);
    let (secret, _) = elsewhere.read_secret(&system(), &made).unwrap();
    assert_eq!(secret.expose(), b"correct horse");

    // The device key alone never opens it: what sealed it is the person.
    let device = DeviceIdentityKey::new(home.clone(), salt.clone(), "1000");
    let by_device = Store::new(dir.path().join("data/vault.tseal"), Box::new(device));
    assert_eq!(by_device.locked(), Some(Locked::KeyChanged));

    // Another person's key, or another salt, is another store.
    let stranger = SigningKey::from_bytes(&[5; 32]);
    write_signing_key(&home.user_privkey, &stranger).unwrap();
    let other = Store::new(
        dir.path().join("data/vault.tseal"),
        Box::new(UserKey::new(
            home,
            salt.clone(),
            Some(stranger.verifying_key().to_bytes()),
        )),
    );
    assert_eq!(other.locked(), Some(Locked::KeyChanged));
}

#[test]
fn a_vault_keyed_by_the_device_is_copied_whole_under_the_persons_key_once() {
    use tilo_user_identity::persist::{
        IdentityPaths, load_or_create_device_key, write_signing_key,
    };
    let dir = tempfile::tempdir().unwrap();
    let system_root = dir.path().join("system");
    let paths = IdentityPaths::under(system_root);
    load_or_create_device_key(&paths).unwrap();
    let salt = dir.path().join("data/salt");
    let path = dir.path().join("data/vault.tseal");

    // What an earlier build left: a vault sealed under the device identity.
    let device = || DeviceIdentityKey::new(paths.clone(), salt.clone(), "1000");
    let before = Store::new(path.clone(), Box::new(device()));
    let made = before
        .create_item(
            &system(),
            "default",
            new_item("Mail", &[("service", "imap")], b"correct horse"),
            true,
        )
        .unwrap();
    let old_bytes = std::fs::read(&path).unwrap();

    // The person's key cannot open it and the store says so without
    // rewriting a byte; the machine relays the ciphertext until the old key
    // is available again.
    let person = SigningKey::from_bytes(&[6; 32]);
    write_signing_key(&paths.user_privkey, &person).unwrap();
    let person_key = || {
        Box::new(UserKey::new(
            paths.clone(),
            salt.clone(),
            Some(person.verifying_key().to_bytes()),
        ))
    };
    let alone = Store::new(path.clone(), person_key());
    assert_eq!(alone.locked(), Some(Locked::KeyChanged));
    assert_eq!(std::fs::read(&path).unwrap(), old_bytes);

    // Both keys present: the vault is copied whole under the person's key
    // and the store switches to it.
    let migrated = Store::open_vault_keyed_by(
        path.clone(),
        person_key(),
        Box::new(DeviceIdentityKey::new(paths.clone(), salt.clone(), "1000")),
    );
    assert_eq!(migrated.locked(), None);
    let (secret, _) = migrated.read_secret(&system(), &made).unwrap();
    assert_eq!(secret.expose(), b"correct horse");
    // Every id survived, and nothing of the old ciphertext is the store.
    assert_eq!(
        migrated
            .search(&system(), None, &attributes(&[("service", "imap")]))
            .unwrap(),
        vec![made.clone()]
    );
    let new_bytes = std::fs::read(&path).unwrap();
    assert_ne!(new_bytes, old_bytes);
    assert!(!new_bytes.windows(4).any(|w| w == b"Mail"));

    // The person's key alone opens it from now on; the device key does not.
    let reopened = Store::new(path.clone(), person_key());
    assert_eq!(reopened.locked(), None);
    let (secret, _) = reopened.read_secret(&system(), &made).unwrap();
    assert_eq!(secret.expose(), b"correct horse");
    let by_device = Store::new(path.clone(), Box::new(device()));
    assert_eq!(by_device.locked(), Some(Locked::KeyChanged));
    assert_eq!(std::fs::read(&path).unwrap(), new_bytes);
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
