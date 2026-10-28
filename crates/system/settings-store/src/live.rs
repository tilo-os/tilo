//! One parsed settings document shared by a whole long-lived process.
//!
//! A process with several subsystems that follow settings (the compositor's
//! input devices, switch control, rotation lock, automatic brightness, the
//! on-screen keyboard) used to poll the document once per subsystem, each on
//! its own schedule and some of them on the input path. [`LiveSettings`]
//! reads the document once per change, off the caller's thread, and hands
//! every subsystem the same parsed snapshot and the same [`generation`]
//! number, so a change made in Settings lands everywhere at once and a reader
//! between changes does no I/O at all.
//!
//! Changes arrive through the [`SettingsSource`]'s change signal. Between
//! changes the refresh thread sleeps with no timeout.
//!
//! [`generation`]: LiveSettings::generation

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::{Listener, SectionDigest, SettingsSource};
use tilo_lock::MutexRecover as _;

/// One version of the settings document.
#[derive(Debug, Default)]
pub struct Snapshot {
    /// Advances by one each time the document's content changes. Starts at 0.
    pub generation: u64,
    /// The parsed document; empty when it is missing or malformed.
    pub document: Map<String, Value>,
}

impl Snapshot {
    /// One top-level section as an object; empty when it is absent or not an
    /// object.
    #[must_use]
    pub fn section(&self, name: &str) -> Map<String, Value> {
        match self.document.get(name) {
            Some(Value::Object(section)) => section.clone(),
            _ => Map::new(),
        }
    }
}

/// The session's settings document, refreshed on change. Cloning shares it.
#[derive(Clone)]
pub struct LiveSettings(Arc<Inner>);

struct Inner {
    current: Mutex<Arc<Snapshot>>,
    changed: Condvar,
    generation: AtomicU64,
    origin: Arc<dyn SettingsSource>,
    nudge: mpsc::Sender<()>,
    /// Kept for as long as the document is followed; dropping it ends the
    /// notifications and, with them, the refresh thread.
    notified: Mutex<Option<Notified>>,
}

/// What tells the refresh thread to look.
struct Notified {
    _listener: Arc<Listener>,
    degraded: Option<String>,
}

impl LiveSettings {
    /// Follow this process's settings ([`crate::source`]). `on_change` runs
    /// on the refresh thread after each new generation is published, so it
    /// should only wake whoever reads it.
    ///
    /// The first snapshot is read on the calling thread, so it is current
    /// as soon as this returns.
    pub fn start(on_change: impl Fn() + Send + 'static) -> Self {
        let source = crate::source()
            .unwrap_or_else(|| Arc::new(crate::HostDocument::new(Map::new(), |_| Ok(()))));
        Self::start_from(source, on_change)
    }

    /// Follow `source` through its change signal.
    pub fn start_from(
        source: Arc<dyn SettingsSource>,
        on_change: impl Fn() + Send + 'static,
    ) -> Self {
        let (nudge, nudges) = mpsc::channel();
        let ring = nudge.clone();
        let listener: Arc<Listener> = Arc::new(move || {
            let _ = ring.send(());
        });
        source.changes().listen(&listener);
        // The source rings whenever its revision changes (the
        // `SettingsSource::changes` contract), so nothing needs polling.
        let notified = Notified {
            _listener: listener,
            degraded: None,
        };
        Self::spawn(source, (nudge, nudges), notified, on_change)
    }

    fn spawn(
        origin: Arc<dyn SettingsSource>,
        (nudge, nudges): (mpsc::Sender<()>, mpsc::Receiver<()>),
        mut notified: Notified,
        on_change: impl Fn() + Send + 'static,
    ) -> Self {
        let inner = Arc::new(Inner {
            current: Mutex::new(Arc::new(Snapshot {
                generation: 0,
                document: origin.load(),
            })),
            changed: Condvar::new(),
            generation: AtomicU64::new(0),
            origin,
            nudge,
            notified: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        let worker = tilo_rt::spawn_thread("tilo-settings-live", move || {
            follow(&weak, &nudges, &on_change);
        });
        if let Err(error) = worker {
            notified.degraded = Some(format!(
                "refresh thread unavailable, settings stay as read at start: {error}"
            ));
        }
        *inner.notified.lock_recover() = Some(notified);
        Self(inner)
    }

    /// Why changes may not arrive at all: no refresh thread. `None` when
    /// changes arrive as they are made.
    #[must_use]
    pub fn degraded(&self) -> Option<String> {
        self.0
            .notified
            .lock_recover()
            .as_ref()
            .and_then(|notified| notified.degraded.clone())
    }

    /// The current generation: one atomic load, for a reader deciding
    /// whether to look again.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// The current document and its generation.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.0.current.lock_recover())
    }

    /// One top-level section of the current document.
    #[must_use]
    pub fn section(&self, name: &str) -> Map<String, Value> {
        self.snapshot().section(name)
    }

    /// Block until the generation moves past `seen` or `timeout` passes, and
    /// return the snapshot current then.
    #[must_use]
    pub fn wait_for_change(&self, seen: u64, timeout: Duration) -> Arc<Snapshot> {
        let current = self.0.current.lock_recover();
        Arc::clone(
            &self
                .0
                .changed
                .wait_timeout_while(current, timeout, |current| current.generation == seen)
                .unwrap_or_else(PoisonError::into_inner)
                .0,
        )
    }

    /// Block until the generation moves past `seen`, however long that takes,
    /// and return the snapshot current then: for a reader with nothing to do
    /// until the document changes, which then costs no wakeups at all.
    #[must_use]
    pub fn wait_for_next(&self, seen: u64) -> Arc<Snapshot> {
        let current = self.0.current.lock_recover();
        Arc::clone(
            &self
                .0
                .changed
                .wait_while(current, |current| current.generation == seen)
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Ask the refresh thread to re-read now, for a caller that knows the
    /// document was written by a path that may ring nothing.
    pub fn refresh(&self) {
        let _ = self.0.nudge.send(());
    }

    /// Re-read the document on the calling thread and return the result,
    /// publishing it as a new generation if it changed — for a caller that
    /// has just written a setting and must act on it at once. Every other
    /// reader keeps to [`Self::snapshot`]. The `on_change` callback is not
    /// run: the caller is the one awake.
    #[must_use]
    pub fn refresh_now(&self) -> Arc<Snapshot> {
        let document = self.0.origin.load();
        publish(&self.0, document);
        self.snapshot()
    }
}

/// The refresh thread: wait for a nudge, re-read, and publish a new
/// generation only when the content changed. Ends when the last
/// [`LiveSettings`] is dropped.
fn follow(inner: &Weak<Inner>, nudges: &mpsc::Receiver<()>, on_change: &dyn Fn()) {
    while nudges.recv().is_ok() {
        // One read covers every notification queued so far.
        while nudges.try_recv().is_ok() {}
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let document = inner.origin.load();
        if publish(&inner, document) {
            on_change();
        }
    }
}

/// Make `document` current unless it equals the current one. Returns whether
/// a new generation was published.
fn publish(inner: &Inner, document: Map<String, Value>) -> bool {
    let mut current = inner.current.lock_recover();
    if current.document == document {
        return false;
    }
    let generation = current.generation.wrapping_add(1);
    *current = Arc::new(Snapshot {
        generation,
        document,
    });
    inner.generation.store(generation, Ordering::Release);
    drop(current);
    inner.changed.notify_all();
    true
}

/// Follows the top-level sections of a [`LiveSettings`] document, for a
/// reader that does different work per section: a write to one section
/// never pays for the work another needs.
pub struct SectionFollower {
    generation: u64,
    digest: SectionDigest,
}

impl SectionFollower {
    /// Start from the current document: what it holds now is the baseline,
    /// not a change.
    #[must_use]
    pub fn new(settings: &LiveSettings) -> Self {
        let snapshot = settings.snapshot();
        Self {
            generation: snapshot.generation,
            digest: SectionDigest::of(&snapshot.document),
        }
    }

    /// The sections that changed since the last call. Empty, with no work
    /// beyond one atomic load, while the generation has not moved.
    pub fn changed_sections(&mut self, settings: &LiveSettings) -> BTreeSet<String> {
        if settings.generation() == self.generation {
            return BTreeSet::new();
        }
        let snapshot = settings.snapshot();
        self.generation = snapshot.generation;
        let digest = SectionDigest::of(&snapshot.document);
        let changed = digest.changed_since(&self.digest);
        self.digest = digest;
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HostDocument;

    fn display(scale: f64) -> Map<String, Value> {
        let mut values = Map::new();
        values.insert("scale".into(), serde_json::json!(scale));
        values
    }

    #[cfg(target_os = "linux")] // the thread's wakeups are read from /proc
    #[test]
    fn waiting_for_the_next_change_sleeps_until_it_comes() {
        let source = Arc::new(HostDocument::new(Map::new(), |_| Ok(())));
        let waiting = LiveSettings::start_from(source.clone(), || {});
        let (tx, rx) = mpsc::channel();
        let (_waiter, tid) = tilo_test_env::idle::spawned_thread("settings-next", || {
            std::thread::Builder::new()
                .name("settings-next".into())
                .spawn(move || tx.send(waiting.wait_for_next(0).generation).unwrap())
                .unwrap()
        });
        crate::tests::blocked_count(&tid);
        tilo_test_env::idle::assert_stays_asleep(&tid, "a reader waiting for the next change");
        assert!(rx.try_recv().is_err(), "returned with no change");

        source.update_section("display", display(2.0)).unwrap();
        assert_eq!(rx.recv_timeout(tilo_test_env::hang_bound()).unwrap(), 1);
    }

    /// A source that counts how often it is looked at.
    struct Counting {
        document: HostDocument,
        looks: AtomicU64,
    }

    impl SettingsSource for Counting {
        fn load(&self) -> Map<String, Value> {
            self.looks.fetch_add(1, Ordering::SeqCst);
            self.document.load()
        }

        fn update_section(&self, name: &str, values: Map<String, Value>) -> std::io::Result<()> {
            self.document.update_section(name, values)
        }

        fn revision(&self) -> u64 {
            self.looks.fetch_add(1, Ordering::SeqCst);
            self.document.revision()
        }

        fn changes(&self) -> &crate::ChangeSignal {
            self.document.changes()
        }
    }

    #[cfg(target_os = "linux")] // the thread's wakeups are read from /proc
    #[test]
    fn a_source_follower_never_looks_between_changes() {
        let source = Arc::new(Counting {
            document: HostDocument::new(Map::new(), |_| Ok(())),
            looks: AtomicU64::new(0),
        });
        let (live, refresh) = tilo_test_env::idle::spawned_thread("tilo-settings-l", || {
            LiveSettings::start_from(source.clone(), || {})
        });
        let looks = source.looks.load(Ordering::SeqCst);
        crate::tests::blocked_count(&refresh);
        tilo_test_env::idle::assert_stays_asleep(&refresh, "follower of a ringing source");
        assert_eq!(
            source.looks.load(Ordering::SeqCst),
            looks,
            "an idle follower of a ringing source polled it"
        );
        assert!(live.degraded().is_none());

        source.update_section("display", display(2.0)).unwrap();
        let snapshot = live.wait_for_change(0, tilo_test_env::hang_bound());
        assert_eq!(snapshot.section("display"), display(2.0));
    }

    #[test]
    fn installed_source_changes_are_seen_without_a_file() {
        let source = Arc::new(HostDocument::new(Map::new(), |_| Ok(())));
        let (woken, wakes) = mpsc::channel();
        let woken = Mutex::new(woken);
        let live = LiveSettings::start_from(source.clone(), move || {
            let _ = woken.lock().unwrap().send(());
        });
        assert!(live.section("display").is_empty());

        source.update_section("display", display(2.0)).unwrap();
        wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_eq!(live.generation(), 1);
        assert_eq!(live.section("display"), display(2.0));
    }

    #[test]
    fn a_section_follower_reports_only_the_sections_a_change_touched() {
        let source = Arc::new(HostDocument::new(Map::new(), |_| Ok(())));
        source.update_section("display", display(1.0)).unwrap();
        let live = LiveSettings::start_from(source.clone(), || {});
        let mut follower = SectionFollower::new(&live);
        assert!(
            follower.changed_sections(&live).is_empty(),
            "the document at start is the baseline"
        );

        let mut input = Map::new();
        input.insert("mouse_natural_scroll".into(), Value::Bool(true));
        source.update_section("input", input).unwrap();
        let _ = live.wait_for_change(0, tilo_test_env::hang_bound());
        assert_eq!(
            follower.changed_sections(&live),
            BTreeSet::from(["input".to_string()])
        );
        assert!(follower.changed_sections(&live).is_empty());
    }
}
