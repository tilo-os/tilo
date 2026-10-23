//! Change notifications: [`Dir::watch`](crate::Dir::watch) and the
//! registries the backends report into.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Waker};

use crate::VPath;
use tilo_lock::MutexRecover as _;

/// What happened to a watched path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ChangeKind {
    /// Created or replaced: read it again.
    Changed,
    /// Removed, or renamed away.
    Removed,
    /// Changes may have been missed (a native queue overflowed): read
    /// everything the watch covers again.
    Rescan,
}

/// One change below a watched path. `path` is relative to the
/// [`Dir`](crate::Dir) the watch was made on; a rescan names the watched
/// path itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Change {
    pub path: VPath,
    pub kind: ChangeKind,
}

/// Changes queued for one [`Watch`].
#[derive(Default)]
pub(crate) struct Inbox {
    state: Mutex<InboxState>,
}

#[derive(Default)]
struct InboxState {
    queue: VecDeque<Change>,
    waker: Option<Waker>,
}

impl Inbox {
    /// Queue a change from a native watch's own thread, which holds no lock
    /// the woken task could need.
    #[cfg(all(
        feature = "watch",
        not(all(target_arch = "wasm32", target_os = "unknown"))
    ))]
    pub(crate) fn push(&self, change: Change) {
        if let Some(waker) = self.queue(change) {
            waker.wake();
        }
    }

    /// Queue `change` and hand back the waiting task's waker, for the caller
    /// to wake once it holds no lock of its own.
    fn queue(&self, change: Change) -> Option<Waker> {
        let mut state = self.state.lock_recover();
        if state.queue.back() != Some(&change) {
            state.queue.push_back(change);
        }
        state.waker.take()
    }

    fn pop(&self, waker: Option<&Waker>) -> Option<Change> {
        let mut state = self.state.lock_recover();
        let next = state.queue.pop_front();
        if next.is_none()
            && let Some(waker) = waker
        {
            state.waker = Some(waker.clone());
        }
        next
    }
}

/// A live watch from [`Dir::watch`](crate::Dir::watch). Changes made through
/// any `Dir` on the same storage, and on the web by `tilo-files` in other
/// tabs of the origin, arrive in order; natively so do other processes'.
/// Dropping it stops the notifications.
#[must_use = "changes arrive only while the watch is kept"]
pub struct Watch {
    inbox: Arc<Inbox>,
    /// Only kept until the watch drops. The mutex makes the watch `Sync`
    /// whatever the backend's source is, so a task awaiting
    /// [`Watch::next`] can move between threads.
    _source: Mutex<Box<dyn std::any::Any + Send>>,
}

impl Watch {
    pub(crate) fn new(inbox: Arc<Inbox>, source: Box<dyn std::any::Any + Send>) -> Self {
        Self {
            inbox,
            _source: Mutex::new(source),
        }
    }

    /// The next queued change, without waiting.
    pub fn try_next(&self) -> Option<Change> {
        self.inbox.pop(None)
    }

    /// Waits for the next change.
    pub fn next(&self) -> Next<'_> {
        Next { watch: self }
    }
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch").finish_non_exhaustive()
    }
}

/// The future of [`Watch::next`].
#[must_use = "futures do nothing unless awaited"]
pub struct Next<'a> {
    watch: &'a Watch,
}

impl Future for Next<'_> {
    type Output = Change;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Change> {
        self.watch
            .inbox
            .pop(Some(cx.waker()))
            .map_or(Poll::Pending, Poll::Ready)
    }
}

/// Watches registered on one storage tree, keyed by full paths in it.
#[derive(Default)]
pub(crate) struct Watchers {
    entries: Mutex<Vec<Registration>>,
}

struct Registration {
    watched: String,
    /// The `Dir`'s root, as a full key, stripped from reported paths.
    root: String,
    inbox: Weak<Inbox>,
}

impl Watchers {
    /// Report changes at or below `watched` (full keys), relative to `root`.
    pub(crate) fn register(&self, watched: &VPath, root: &VPath) -> Arc<Inbox> {
        let inbox = Arc::new(Inbox::default());
        let mut entries = self.entries.lock_recover();
        entries.retain(|entry| entry.inbox.strong_count() > 0);
        entries.push(Registration {
            watched: watched.as_str().to_owned(),
            root: root.as_str().to_owned(),
            inbox: Arc::downgrade(&inbox),
        });
        inbox
    }

    /// Tell every watch covering `key` (a full key) what happened to it.
    ///
    /// Call it while still holding the lock the change was made under, and
    /// wake the returned [`Wakes`] after releasing it. Queued outside that
    /// lock, two threads' changes to one path could be queued in the other
    /// order than they were made: a write's `Changed` after the removal that
    /// followed it, for a file that is gone.
    pub(crate) fn notify(&self, key: &str, kind: ChangeKind) -> Wakes {
        let entries = self.entries.lock_recover();
        Wakes(
            entries
                .iter()
                .filter(|entry| covers(&entry.watched, key))
                .filter_map(|entry| {
                    let inbox = entry.inbox.upgrade()?;
                    let path = relative(&entry.root, key)?;
                    inbox.queue(Change { path, kind })
                })
                .collect(),
        )
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.entries
            .lock_recover()
            .iter()
            .all(|entry| entry.inbox.strong_count() == 0)
    }
}

/// The tasks waiting on watches that [`Watchers::notify`] queued changes for.
/// Woken only once the notifier's own locks are released, so a woken task
/// that reads the tree straight away does not wait on them.
#[must_use = "the watches' tasks wait until these are woken"]
pub(crate) struct Wakes(Vec<Waker>);

impl Wakes {
    pub(crate) fn and(mut self, other: Self) -> Self {
        self.0.extend(other.0);
        self
    }

    pub(crate) fn wake(self) {
        for waker in self.0 {
            waker.wake();
        }
    }
}

pub(crate) fn covers(watched: &str, key: &str) -> bool {
    watched.is_empty()
        || key == watched
        || key
            .strip_prefix(watched)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// `key` below `root`, as a path relative to it.
fn relative(root: &str, key: &str) -> Option<VPath> {
    if root.is_empty() {
        return VPath::new(key).ok();
    }
    if key == root {
        return Some(VPath::root());
    }
    VPath::new(key.strip_prefix(root)?.strip_prefix('/')?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(path: &str) -> VPath {
        VPath::new(path).unwrap()
    }

    #[test]
    fn a_watch_sees_changes_below_it_relative_to_its_root() {
        let watchers = Watchers::default();
        let inbox = watchers.register(&p("app/data/notes"), &p("app/data"));
        watchers
            .notify("app/data/notes/a.md", ChangeKind::Changed)
            .wake();
        watchers
            .notify("app/data/notes", ChangeKind::Removed)
            .wake();
        watchers
            .notify("app/data/notesx", ChangeKind::Changed)
            .wake();
        watchers
            .notify("app/data/other", ChangeKind::Changed)
            .wake();
        assert_eq!(
            inbox.pop(None),
            Some(Change {
                path: p("notes/a.md"),
                kind: ChangeKind::Changed
            })
        );
        assert_eq!(
            inbox.pop(None),
            Some(Change {
                path: p("notes"),
                kind: ChangeKind::Removed
            })
        );
        assert_eq!(inbox.pop(None), None);
    }

    #[test]
    fn awaiting_a_change_can_cross_threads() {
        fn send<T: Send>(_: &T) {}
        let watch = Watch::new(Arc::new(Inbox::default()), Box::new(()));
        send(&watch.next());
    }

    #[test]
    fn repeated_changes_collapse_and_dropped_watches_unregister() {
        let watchers = Watchers::default();
        let inbox = watchers.register(&VPath::root(), &VPath::root());
        watchers.notify("a", ChangeKind::Changed).wake();
        watchers.notify("a", ChangeKind::Changed).wake();
        assert!(inbox.pop(None).is_some());
        assert!(inbox.pop(None).is_none());
        drop(inbox);
        assert!(watchers.is_empty());
    }
}
