//! A process given `TILO_SECRETS_DIR` keeps every app's secrets in that
//! directory's sealed file store, not the system store.

use tilo_secrets::{Protection, PutOptions, SECRETS_DIR_ENV, SecretStore};

#[test]
fn the_named_directory_replaces_the_system_store() {
    let dir = tempfile::tempdir().unwrap();
    let store_dir = dir.path().join("secrets");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(SECRETS_DIR_ENV, &store_dir);

    let store = SecretStore::for_app("so.tilo.node-test");
    assert_eq!(store.protection(), Protection::LocalFile);
    store.probe_blocking().unwrap();
    store
        .put_blocking("master", &[7; 32], PutOptions::default())
        .unwrap();
    assert_eq!(
        SecretStore::for_app("so.tilo.node-test")
            .get_blocking("master")
            .unwrap()
            .unwrap()
            .expose(),
        &[7; 32]
    );
    assert!(store_dir.join("store.key").is_file());
}

/// A process that names a second directory gets that directory's store: a test suite gives each
/// test its own home and names its store while the test runs, and one name must not outlive it.
#[test]
fn a_second_named_directory_is_its_own_store() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(SECRETS_DIR_ENV, first.path().join("secrets"));
    let one = SecretStore::for_app("so.tilo.node-test");
    one.put_blocking("master", &[1; 32], PutOptions::default())
        .unwrap();
    env.set(SECRETS_DIR_ENV, second.path().join("secrets"));

    let two = SecretStore::for_app("so.tilo.node-test");
    two.probe_blocking().unwrap();
    assert!(
        two.get_blocking("master").unwrap().is_none(),
        "a directory named later holds none of the first one's secrets"
    );
    two.put_blocking("master", &[2; 32], PutOptions::default())
        .unwrap();
    assert_eq!(
        one.get_blocking("master").unwrap().unwrap().expose(),
        &[1; 32],
        "and what was kept under the first name stays there"
    );
}
