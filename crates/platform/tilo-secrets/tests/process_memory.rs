//! `keep_in_memory_for_process` in a process of its own: every app's store
//! is then in memory, shared by later stores and still app-scoped.

use tilo_secrets::{Protection, PutOptions, SecretStore, keep_in_memory_for_process};

#[test]
fn app_stores_stay_in_the_process() {
    keep_in_memory_for_process();
    let written = SecretStore::for_app("so.tilo.process-memory");
    assert_eq!(written.protection(), Protection::Memory);
    written.probe_blocking().unwrap();
    written
        .put_blocking("key", b"value", PutOptions::default())
        .unwrap();

    let reopened = SecretStore::for_app("so.tilo.process-memory");
    assert_eq!(
        reopened.get_blocking("key").unwrap().unwrap().expose(),
        b"value"
    );
    let other = SecretStore::for_app("so.tilo.other");
    assert_eq!(other.get_blocking("key").unwrap(), None);
}

#[test]
fn a_scoped_thread_keeps_its_own_store() {
    let scoped = std::thread::spawn(|| {
        let _scope = tilo_secrets::scope_thread(tilo_secrets::memory_backend());
        let store = SecretStore::for_app("so.tilo.scoped");
        store
            .put_blocking("key", b"agent's", PutOptions::default())
            .unwrap();
        (
            store.protection(),
            SecretStore::for_app("so.tilo.scoped")
                .get_blocking("key")
                .unwrap(),
        )
    })
    .join()
    .unwrap();
    assert_eq!(scoped.0, Protection::Memory);
    assert_eq!(scoped.1.unwrap().expose(), b"agent's");
}
