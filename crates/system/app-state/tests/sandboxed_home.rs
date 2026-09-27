//! A sandboxed app is given one directory, its home, and may open nothing
//! above it: the launcher binds it at `/run/tilo/app`, points `HOME` at it
//! and confines the app with Landlock. The durable store must still make
//! its writes durable there, syncing the directories it created up to that
//! home and not the ones above it, which the sandbox refuses.
//!
//! A separate test binary: Landlock confines the thread that installs it,
//! and every thread it starts, for good.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use app_state::FsStore;
use durable_store::{Durability, DurableStore, StorageKey, WriteOp};
use landlock::{
    ABI, Access, AccessFs, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    path_beneath_rules,
};

/// Confine the calling thread to `home`, as the launcher confines an app.
fn confine_to(home: &Path) {
    let abi = ABI::V5;
    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .unwrap()
        .create()
        .unwrap()
        .add_rules(path_beneath_rules([home], AccessFs::from_all(abi)))
        .unwrap()
        .restrict_self()
        .unwrap();
    assert_eq!(
        status.ruleset,
        RulesetStatus::FullyEnforced,
        "this kernel cannot confine the test as the sandbox confines an app, so it cannot observe \
         what it checks"
    );
}

#[test]
fn a_store_in_a_sandboxed_home_becomes_durable_without_opening_what_lies_above() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("run").join("tilo").join("app");
    std::fs::create_dir_all(&home).unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home);
    let profile = home.join(".local").join("share");

    let outcome = std::thread::spawn(move || {
        confine_to(&home);
        assert!(
            std::fs::read_dir(home.parent().unwrap()).is_err(),
            "the directory above the home is refused, as in the sandbox"
        );
        let store = FsStore::with_debounce(&profile, std::time::Duration::ZERO);
        let key = StorageKey::record("so.tilo.notes", "room-a", "page");
        let _lease = store.lease(&key).unwrap();
        let revision = store
            .write(&key, WriteOp::Replace(b"written".to_vec()), None)
            .unwrap();
        store.flush(Some(&key)).map_err(|error| error.to_string())?;
        Ok::<_, String>((
            store.durability(&key, revision),
            store.read(&key).unwrap().map(|stored| stored.bytes),
        ))
    })
    .join()
    .expect("the confined thread ran");
    drop(env);

    let (durability, bytes) = outcome.unwrap_or_else(|error| {
        panic!("the store could not make its write durable in a sandboxed home: {error}")
    });
    assert_eq!(durability, Durability::Durable);
    assert_eq!(bytes.as_deref(), Some(&b"written"[..]));
}
