//! `local_user_id()` must name the *person* sitting at this machine.
//!
//! It is what `App::local_author` is built from, what every committed action is
//! stamped with, and what `undo_for_author` matches on — the thing documented
//! as stopping one collaborator reverting another's work. Matching on a value
//! two people share does not stop anything.
//!
//! These tests set the environment through `tilo_test_env`, which serialises
//! them against every other test in the process that does.

use tilo_ui::collaboration::{local_app_id, local_user_id};

/// Set and remove `vars` until the returned scope drops, which restores each
/// prior value, so one test's environment is not the next test's premise.
fn scoped_env(vars: &[(&str, Option<&str>)]) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    for (key, value) in vars {
        match value {
            Some(value) => env.set(key, value),
            None => env.remove(key),
        };
    }
    env
}

#[test]
fn local_identity_is_one_stable_person_distinct_from_apps_and_peers() {
    // Case `a_person_is_not_the_application_they_are_running`.
    {
        let _lock = tilo_test_env::lock();
        let _env = scoped_env(&[
            ("TILO_PACKAGE_ID", Some("so.tilo.word-processor")),
            ("TILO_USER_ID", None),
            ("TILO_PRESENCE_HANDLE", None),
            ("TILO_USER_IDENTITY", None),
        ]);

        assert_ne!(
            local_user_id().0,
            local_app_id(),
            "the person at this machine must not be named by the application they opened"
        );
    }

    // Case `two_people_running_one_app_are_two_people`.
    // Two people on two machines both open the word processor. Their ids have to
    // differ, or `undo_for_author` is matching a value that cannot tell them
    // apart — and one person's Ctrl+Z reverts the other's work.
    {
        let _lock = tilo_test_env::lock();

        let alice = {
            let _env = scoped_env(&[
                ("TILO_PACKAGE_ID", Some("so.tilo.word-processor")),
                ("TILO_USER_ID", Some("alice")),
                ("TILO_PRESENCE_HANDLE", None),
                ("TILO_USER_IDENTITY", None),
            ]);
            local_user_id()
        };
        let bob = {
            let _env = scoped_env(&[
                ("TILO_PACKAGE_ID", Some("so.tilo.word-processor")),
                ("TILO_USER_ID", Some("bob")),
                ("TILO_PRESENCE_HANDLE", None),
                ("TILO_USER_IDENTITY", None),
            ]);
            local_user_id()
        };

        assert_ne!(
            alice, bob,
            "two people on one application must be two authors"
        );
    }

    // Case `the_same_person_is_the_same_id_every_time`.
    // Stable across calls: an id that changed between two actions would strand the
    // first one on the undo stack, unreachable by the person who performed it.
    {
        let _lock = tilo_test_env::lock();
        let _env = scoped_env(&[
            ("TILO_PACKAGE_ID", Some("so.tilo.word-processor")),
            ("TILO_USER_ID", None),
            ("TILO_PRESENCE_HANDLE", None),
            ("TILO_USER_IDENTITY", None),
        ]);

        let first = local_user_id();
        let second = local_user_id();
        assert_eq!(
            first, second,
            "the person at this machine must not change between two actions"
        );
        assert!(
            !first.0.trim().is_empty(),
            "an empty id would collide with every other empty id"
        );
    }
}
