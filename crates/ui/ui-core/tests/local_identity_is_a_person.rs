//! `local_user_id()` must name the *person* sitting at this machine.
//!
//! It is what `App::local_author` is built from, what every committed action is
//! stamped with, and what `undo_for_author` matches on — the thing documented
//! as stopping one member reverting another's work. Matching on a value
//! two people share does not stop anything.
//!
//! These tests set the environment through `tilo_test_env`, which serialises
//! them against every other test in the process that does.

use tilo_ui_base::{collaboration::local_app_id, collaboration::local_user_id};
use tilo_ui_base::{
    local_identity::FixedIdentity, local_identity::LocalIdentity, local_identity::install_source,
};

/// Set and remove `vars` until the returned scope drops, which restores each
/// prior value, so one test's environment is not the next test's premise.
fn scoped_env(vars: &[(&str, Option<&str>)]) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    for (key, value) in vars {
        match value {
            Some(value) => {
                env.set(key, value);
            }
            None => {
                env.remove(key);
            }
        }
    }
    env
}

fn installed(key: [u8; 32]) -> FixedIdentity {
    FixedIdentity(LocalIdentity {
        group_public_key: key,
        display_name: "Ada".into(),
        avatar_color: [10, 20, 30],
        device_public_key: None,
    })
}

#[test]
fn local_identity_is_one_stable_person_distinct_from_apps_and_peers() {
    let _lock = tilo_test_env::lock();

    // With no identity installed and no identity service, the person is the OS
    // account at this machine, never the application they opened.
    {
        install_source(None);
        let _env = scoped_env(&[
            ("TILO_PACKAGE_ID", Some("so.tilo.word-processor")),
            ("TILO_SVC_IDENTITY_FD", None),
            ("USER", Some("ada")),
        ]);
        assert_ne!(
            local_user_id().id(),
            local_app_id(),
            "the person at this machine must not be named by the application they opened"
        );
        assert_eq!(local_user_id().id(), "ada");
        assert_eq!(
            local_user_id(),
            local_user_id(),
            "the person at this machine must not change between two actions"
        );
    }

    // Two accounts on two machines name two people, or `undo_for_author` is
    // matching a value that cannot tell them apart — and one person's Ctrl+Z
    // reverts the other's work.
    {
        install_source(None);
        let alice = {
            let _env = scoped_env(&[("TILO_SVC_IDENTITY_FD", None), ("USER", Some("alice"))]);
            local_user_id()
        };
        let bob = {
            let _env = scoped_env(&[("TILO_SVC_IDENTITY_FD", None), ("USER", Some("bob"))]);
            local_user_id()
        };
        assert_ne!(
            alice, bob,
            "two people on one application must be two authors"
        );
    }

    // An identity this process resolved wins over the account name: both
    // surfaces asking who the person is must name the same key.
    {
        install_source(Some(std::sync::Arc::new(installed([7; 32]))));
        let _env = scoped_env(&[("TILO_SVC_IDENTITY_FD", None), ("USER", Some("ada"))]);
        assert_eq!(local_user_id().id(), hex::encode([7; 32]));
        assert_eq!(
            local_user_id(),
            local_user_id(),
            "an identity resolved once must not change between two actions"
        );
        install_source(Some(std::sync::Arc::new(installed([8; 32]))));
        assert_eq!(local_user_id().id(), hex::encode([8; 32]));
    }

    install_source(None);
}
