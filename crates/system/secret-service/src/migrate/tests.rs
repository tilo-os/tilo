use std::collections::BTreeMap;
use std::sync::Mutex;

use tilo_seal::SymmetricKey;

use super::*;
use crate::app::AppId;
use crate::store::{KeySource, Locked, NewItem};

struct Fixed;
impl KeySource for Fixed {
    fn release(&self) -> Result<SymmetricKey, Locked> {
        Ok(SymmetricKey::from_bytes([3; 32]))
    }
}

struct Fake {
    answers: Mutex<Vec<Result<Vec<LegacyItem>, SourceError>>>,
    asked: Mutex<u32>,
}

impl Fake {
    fn new(answers: Vec<Result<Vec<LegacyItem>, SourceError>>) -> Self {
        Self {
            answers: Mutex::new(answers),
            asked: Mutex::new(0),
        }
    }
}

impl LegacySource for Fake {
    async fn load(&self) -> Result<Vec<LegacyItem>, SourceError> {
        {
            let mut asked = self.asked.lock().unwrap();
            *asked = asked.saturating_add(1);
        }
        self.answers.lock().unwrap().remove(0)
    }
}

fn legacy(label: &str, secret: &[u8]) -> LegacyItem {
    LegacyItem {
        label: label.into(),
        attributes: BTreeMap::from([("application".to_owned(), label.to_owned())]),
        secret: secret.to_vec(),
        content_type: "text/plain".into(),
        created: 1,
        modified: 2,
    }
}

fn store(dir: &std::path::Path) -> Store {
    Store::new(dir.join("vault"), Box::new(Fixed))
}

#[tokio::test]
async fn a_locked_keyring_is_retried_and_an_unlocked_one_imported_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let source = Fake::new(vec![
        Err(SourceError::Unavailable(
            "the login keyring is locked".into(),
        )),
        Ok(vec![legacy("chrome", b"one"), legacy("wifi", b"two")]),
        Ok(vec![legacy("chrome", b"changed")]),
    ]);

    // A fingerprint login: locked, nothing recorded.
    assert_eq!(
        migrate_once(&store, &source).await,
        Outcome::Deferred("the login keyring is locked".into())
    );
    assert!(store.migration().unwrap().is_none());

    // The first password login imports.
    assert_eq!(migrate_once(&store, &source).await, Outcome::Imported(2));
    // Every later start leaves it alone, without even asking the keyring.
    assert_eq!(migrate_once(&store, &source).await, Outcome::AlreadyDone);
    let restarted = Store::new(dir.path().join("vault"), Box::new(Fixed));
    assert_eq!(
        migrate_once(&restarted, &source).await,
        Outcome::AlreadyDone
    );
    assert_eq!(*source.asked.lock().unwrap(), 2);

    let system = AppId::parse("binary:/usr/bin/chromium").unwrap();
    let found = restarted
        .search(
            &system,
            None,
            &BTreeMap::from([("application".to_owned(), "chrome".to_owned())]),
        )
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        restarted
            .read_secret(&system, &found[0])
            .unwrap()
            .0
            .expose(),
        b"one"
    );
    // Imported items did not displace what applications keep themselves.
    restarted
        .create_item(
            &system,
            "default",
            NewItem {
                label: "mine".into(),
                attributes: BTreeMap::new(),
                secret: b"x".to_vec(),
                content_type: "text/plain".into(),
            },
            false,
        )
        .unwrap();
    assert_eq!(restarted.migration().unwrap().unwrap().imported, 2);
}

#[tokio::test]
async fn no_older_keyring_is_recorded_as_done() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let source = Fake::new(vec![Err(SourceError::Absent)]);
    assert_eq!(migrate_once(&store, &source).await, Outcome::Imported(0));
    assert_eq!(migrate_once(&store, &source).await, Outcome::AlreadyDone);
}

#[tokio::test]
async fn the_real_source_needs_both_the_keyring_file_and_its_running_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let source = GnomeKeyring {
        keyrings: dir.path().join("keyrings"),
        control: None,
    };
    assert_eq!(source.load().await.err(), Some(SourceError::Absent));

    std::fs::create_dir_all(dir.path().join("keyrings")).unwrap();
    std::fs::write(dir.path().join("keyrings/login.keyring"), b"x").unwrap();
    assert!(matches!(
        source.load().await,
        Err(SourceError::Unavailable(_))
    ));
    let dead = GnomeKeyring {
        keyrings: dir.path().join("keyrings"),
        control: Some(dir.path().join("no-such/control")),
    };
    assert!(matches!(
        dead.load().await,
        Err(SourceError::Unavailable(_))
    ));
}
