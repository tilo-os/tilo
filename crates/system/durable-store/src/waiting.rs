//! Threads that must never wait on storage.
//!
//! A compositor's frame loop that waits for a store's commit, an fsync on
//! another thread, is a frozen desktop for as long as the disk takes: on a
//! busy machine that is seconds. Such a thread says so with
//! [`forbid_on_this_thread`]; from then until the guard drops, callers
//! that would otherwise ask for a durable barrier
//! ([`crate::DurableStore::flush_where_allowed`]) only start the commit and
//! learn of its outcome later, through [`crate::DurableStore::durability`],
//! and code that would wait on the disk itself (an app the compositor hosts
//! listing a folder) does it on a worker.
//!
//! A blocking call still reached on such a thread, such as a store's
//! [`crate::DurableStore::flush`], reports it through [`refused`]: to the observer the process
//! installed ([`observe`], which a compositor's audit lists), and, on a
//! thread a test marked with [`forbid_strictly_on_this_thread`], as a panic,
//! so the test names the caller.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    Allowed,
    Forbidden,
    /// Forbidden, and a wait reached anyway fails the test that set it.
    Strict,
}

thread_local! {
    static RULE: Cell<Rule> = const { Cell::new(Rule::Allowed) };
}

static OBSERVER: OnceLock<fn(&str)> = OnceLock::new();

/// Keeps storage waits forbidden on the thread that made it, until dropped.
/// Not `Send`: it belongs to that thread.
#[must_use = "waits are forbidden only while the guard lives"]
pub struct Forbidden {
    previous: Rule,
    _thread: PhantomData<*const ()>,
}

impl Drop for Forbidden {
    fn drop(&mut self) {
        RULE.with(|rule| rule.set(self.previous));
    }
}

fn set(next: Rule) -> Forbidden {
    Forbidden {
        previous: RULE.with(|rule| rule.replace(next)),
        _thread: PhantomData,
    }
}

/// Forbid storage waits on this thread while the guard lives. See the
/// module docs.
pub fn forbid_on_this_thread() -> Forbidden {
    set(Rule::Forbidden)
}

/// [`forbid_on_this_thread`] for a test: a blocking wait reached anyway
/// panics, naming what waited.
pub fn forbid_strictly_on_this_thread() -> Forbidden {
    set(Rule::Strict)
}

/// Whether this thread must not wait on storage.
#[must_use]
pub fn forbidden() -> bool {
    RULE.with(Cell::get) != Rule::Allowed
}

/// Tell `observer` about every storage wait reached on a thread that
/// forbids them. One observer per process; `false` when one was installed
/// already.
pub fn observe(observer: fn(&str)) -> bool {
    OBSERVER.set(observer).is_ok()
}

/// The caller is about to wait on storage (`what` says on what): a store's
/// commit, or the disk itself, such as listing a folder. Where waits are
/// forbidden the observer hears of it, and a strict test fails, naming the
/// caller.
///
/// # Panics
///
/// On a thread a test marked with [`forbid_strictly_on_this_thread`].
#[track_caller]
pub fn refused(what: &str) {
    let rule = RULE.with(Cell::get);
    if rule == Rule::Allowed {
        return;
    }
    if let Some(observer) = OBSERVER.get() {
        observer(what);
    }
    let at = std::panic::Location::caller();
    assert!(
        rule != Rule::Strict,
        "{at}: {what} is a storage wait on a thread that must never wait on storage; \
         move it to a worker, or for a commit use DurableStore::flush_where_allowed \
         and read the outcome from durability"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guard_forbids_waits_on_its_thread_until_it_drops() {
        assert!(!forbidden());
        {
            let _outer = forbid_on_this_thread();
            assert!(forbidden());
            refused("a tolerated wait");
            std::thread::spawn(|| assert!(!forbidden(), "another thread may wait"))
                .join()
                .unwrap();
            {
                let _inner = forbid_strictly_on_this_thread();
                let strict = std::panic::catch_unwind(|| refused("a wait"));
                let message = strict.expect_err("a strict thread fails the test");
                let message = message
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_default();
                assert!(message.contains("waiting.rs"), "names no caller: {message}");
            }
            assert!(forbidden(), "the outer guard still holds");
            refused("tolerated again");
        }
        assert!(!forbidden());
    }
}
