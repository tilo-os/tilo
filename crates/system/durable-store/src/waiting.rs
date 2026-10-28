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
//!
//! Work such a thread would have waited for is done for it elsewhere (a
//! store's own thread opening or reading a key) while it is told "not yet".
//! The thread's wake ([`Forbidden::waking`], [`decline_on_this_thread`]:
//! for a frame loop, its redraw) is how it hears that the work is done, so
//! an idle thread asks again without polling; [`wake`] hands it to whoever
//! does that work.
//!
//! A commit wait ([`refused_commit`]: waiting for another thread's fsync)
//! is the one a frame loop can never bound, and the one a caller can always
//! avoid by asking durability instead. A test of code that still reads the
//! disk on its thread marks it with [`forbid_commit_waits_strictly`]: reads
//! are reported as usual, and only a commit wait fails the test.

use std::cell::{Cell, RefCell};
use std::marker::PhantomData;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Rule {
    Allowed,
    /// Waits allowed, but a store does its disk work elsewhere and answers
    /// "not yet" ([`decline_on_this_thread`]).
    Declined,
    Forbidden,
    /// Forbidden, and a wait reached anyway fails the test that set it.
    Strict,
    /// Commit waits forbidden, failing the test that set it; the code under
    /// test still reads the disk here, and other waits are only reported.
    StrictCommits,
}

thread_local! {
    static RULE: Cell<Rule> = const { Cell::new(Rule::Allowed) };
    static WAKE: RefCell<Option<crate::ReadyWake>> = const { RefCell::new(None) };
}

static OBSERVER: OnceLock<fn(&str)> = OnceLock::new();

/// Keeps storage waits forbidden on the thread that made it, until dropped.
/// Not `Send`: it belongs to that thread.
#[must_use = "waits are forbidden only while the guard lives"]
pub struct Forbidden {
    previous: Rule,
    /// The wake this guard replaced, when it set one.
    previous_wake: Replaced,
    _thread: PhantomData<*const ()>,
}

/// What a guard put back on its thread's wake when it drops.
enum Replaced {
    /// It set no wake: the thread's stays.
    Nothing,
    /// It set one over this (none, where the thread had none).
    Wake(Option<crate::ReadyWake>),
}

impl Forbidden {
    /// Wake this thread through `wake` when work it was told "not yet"
    /// about is done, while the guard lives.
    pub fn waking(mut self, wake: crate::ReadyWake) -> Self {
        let replaced = WAKE.with(|slot| slot.replace(Some(wake)));
        if matches!(self.previous_wake, Replaced::Nothing) {
            self.previous_wake = Replaced::Wake(replaced);
        }
        self
    }
}

impl Drop for Forbidden {
    fn drop(&mut self) {
        RULE.with(|rule| rule.set(self.previous));
        if let Replaced::Wake(previous) =
            std::mem::replace(&mut self.previous_wake, Replaced::Nothing)
        {
            WAKE.with(|slot| *slot.borrow_mut() = previous);
        }
    }
}

fn set(next: Rule) -> Forbidden {
    Forbidden {
        previous: RULE.with(|rule| rule.replace(next)),
        previous_wake: Replaced::Nothing,
        _thread: PhantomData,
    }
}

/// From now on, have stores do this thread's disk work (opening, reading)
/// elsewhere and answer "not yet", waking it through `wake` once the work
/// is done: for a frame loop, before its loop forbids every wait
/// ([`forbid_on_this_thread`]). Its own waits, such as a flush it asks for,
/// stay allowed. For the rest of the thread's life, or until a guard made
/// before this drops; a forbidden thread keeps its rule and takes the new
/// wake.
pub fn decline_on_this_thread(wake: crate::ReadyWake) {
    RULE.with(|rule| match rule.get() {
        Rule::Allowed => rule.set(Rule::Declined),
        Rule::StrictCommits => rule.set(Rule::Forbidden),
        Rule::Declined | Rule::Forbidden | Rule::Strict => {}
    });
    WAKE.with(|slot| *slot.borrow_mut() = Some(wake));
}

/// Whether this thread must not touch the disk at all, so a store does its
/// disk work elsewhere and answers "not yet" ([`wake`]): a thread that
/// declined it, and every forbidden thread but one under
/// [`forbid_commit_waits_strictly`], whose code still reads the disk itself.
#[must_use]
pub fn disk_forbidden() -> bool {
    matches!(
        RULE.with(Cell::get),
        Rule::Declined | Rule::Forbidden | Rule::Strict
    )
}

/// How to wake this thread once work it must not wait for is done: `None`
/// where it may touch the disk ([`disk_forbidden`]). A thread that set no
/// wake gets one that does nothing; it hears of the work only when it next
/// asks.
#[must_use]
pub fn wake() -> Option<crate::ReadyWake> {
    if !disk_forbidden() {
        return None;
    }
    Some(WAKE.with(|slot| slot.borrow().clone()).unwrap_or_else(|| {
        static NOTHING: OnceLock<crate::ReadyWake> = OnceLock::new();
        std::sync::Arc::clone(NOTHING.get_or_init(|| std::sync::Arc::new(|| {})))
    }))
}

/// Whether `a` and `b` are the same wake, so it is called once however
/// often the thread asked.
#[must_use]
pub fn same_wake(a: &crate::ReadyWake, b: &crate::ReadyWake) -> bool {
    std::ptr::addr_eq(std::sync::Arc::as_ptr(a), std::sync::Arc::as_ptr(b))
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

/// [`forbid_on_this_thread`] for a test of code that still reads the disk
/// here: a commit wait reached anyway panics, naming what waited, and other
/// waits are only reported.
pub fn forbid_commit_waits_strictly() -> Forbidden {
    set(Rule::StrictCommits)
}

/// Whether this thread must not wait on storage.
#[must_use]
pub fn forbidden() -> bool {
    !matches!(RULE.with(Cell::get), Rule::Allowed | Rule::Declined)
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
    refuse(what, false);
}

/// [`refused`] for a wait on another thread's commit (a store's writer
/// committing and syncing): also fails a test marked with
/// [`forbid_commit_waits_strictly`].
///
/// # Panics
///
/// On a thread a test marked with [`forbid_strictly_on_this_thread`] or
/// [`forbid_commit_waits_strictly`].
#[track_caller]
pub fn refused_commit(what: &str) {
    refuse(what, true);
}

#[track_caller]
fn refuse(what: &str, commit: bool) {
    let rule = RULE.with(Cell::get);
    if matches!(rule, Rule::Allowed | Rule::Declined) {
        return;
    }
    if let Some(observer) = OBSERVER.get() {
        observer(what);
    }
    let at = std::panic::Location::caller();
    assert!(
        rule != Rule::Strict && !(commit && rule == Rule::StrictCommits),
        "{at}: {what} is a storage wait on a thread that must never wait on storage; \
         move it to a worker, or for a commit use DurableStore::flush_where_allowed \
         and read the outcome from durability"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forbidden_thread_carries_its_wake_while_the_guard_lives() {
        assert!(wake().is_none(), "a thread that may wait needs no wake");
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&woken);
        let mine: crate::ReadyWake = std::sync::Arc::new(move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        {
            let _loop = forbid_strictly_on_this_thread().waking(std::sync::Arc::clone(&mine));
            let handed = wake().expect("a forbidden thread has a wake");
            assert!(same_wake(&handed, &mine));
            handed();
            std::thread::spawn(|| assert!(wake().is_none(), "another thread's own"))
                .join()
                .unwrap();
        }
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(wake().is_none(), "gone with the guard");
        {
            let _reads = forbid_commit_waits_strictly();
            assert!(
                !disk_forbidden() && wake().is_none(),
                "this code still reads the disk"
            );
        }
        std::thread::spawn(move || {
            decline_on_this_thread(std::sync::Arc::clone(&mine));
            assert!(
                disk_forbidden() && !forbidden(),
                "its own waits stay allowed"
            );
            refused("a flush it asked for");
            assert!(same_wake(&wake().unwrap(), &mine));
        })
        .join()
        .unwrap();
    }

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
            {
                let _commits = forbid_commit_waits_strictly();
                assert!(forbidden());
                refused("a read, only reported");
                let commit = std::panic::catch_unwind(|| refused_commit("a commit"));
                assert!(commit.is_err(), "a commit wait fails the test");
            }
            assert!(forbidden(), "the outer guard still holds");
            refused("tolerated again");
        }
        assert!(!forbidden());
    }
}
