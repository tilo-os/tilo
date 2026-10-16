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
