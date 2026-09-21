//! `local_user_id()` must name the *person* sitting at this machine.
//!
//! It is what `App::local_author` is built from, what every committed action is
//! stamped with, and what `undo_for_author` matches on — the thing documented
//! as stopping one collaborator reverting another's work. Matching on a value
//! two people share does not stop anything.
//!
//! These tests mutate the environment in one process, so they are serialised
//! behind a mutex rather than left to race each other.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Mutex, MutexGuard, OnceLock};

use tilo_ui::collaboration::{local_app_id, local_user_id};

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Restores whatever was there, so one test's environment is not the next
/// test's premise.
struct EnvGuard(Vec<(&'static str, Option<String>)>);

impl EnvGuard {
    fn set(vars: &[(&'static str, Option<&str>)]) -> Self {
        let saved = vars
            .iter()
            .map(|(k, _)| (*k, std::env::var(k).ok()))
            .collect();
        for (k, v) in vars {
            // SAFETY: every test that touches the environment holds `env_lock`
            // for its whole body, so no other thread is reading it here.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
        Self(saved)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.0 {
            // SAFETY: as above — the test's `env_lock` guard outlives this.
            unsafe {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

#[test]
fn a_person_is_not_the_application_they_are_running() {
    let _lock = env_lock();
    let _env = EnvGuard::set(&[
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

/// Two people on two machines both open the word processor. Their ids have to
/// differ, or `undo_for_author` is matching a value that cannot tell them
/// apart — and one person's Ctrl+Z reverts the other's work.
#[test]
fn two_people_running_one_app_are_two_people() {
    let _lock = env_lock();

    let alice = {
        let _env = EnvGuard::set(&[
            ("TILO_PACKAGE_ID", Some("so.tilo.word-processor")),
            ("TILO_USER_ID", Some("alice")),
            ("TILO_PRESENCE_HANDLE", None),
            ("TILO_USER_IDENTITY", None),
        ]);
        local_user_id()
    };
    let bob = {
        let _env = EnvGuard::set(&[
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

/// Stable across calls: an id that changed between two actions would strand the
/// first one on the undo stack, unreachable by the person who performed it.
#[test]
fn the_same_person_is_the_same_id_every_time() {
    let _lock = env_lock();
    let _env = EnvGuard::set(&[
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
