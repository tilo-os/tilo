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
//! Changes arrive through the installed [`SettingsSource`]'s change signal
//! when the host provides the document, else through the directory watch of
//! [`crate::watch`]. Between changes the refresh thread sleeps with no
//! timeout. Only a follower whose file watch could not start falls back to a
//! slow stamp poll, so it still notices a write within [`POLL_INTERVAL`].
//!
//! [`generation`]: LiveSettings::generation

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::{Listener, SectionDigest, SettingsSource, SettingsStamp};

/// How often the document's stamp is compared by a follower that has no
/// notification to wait for: a settings file whose watch could not start.
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

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
    origin: Origin,
    nudge: mpsc::Sender<()>,
    /// Kept for as long as the document is followed; dropping it ends the
    /// notifications and, with them, the refresh thread.
    notified: Mutex<Option<Notified>>,
}

/// What tells the refresh thread to look.
struct Notified {
    /// How often to compare stamps when nothing rings: `None` while a watch
    /// or a change signal reports every change.
    poll: Option<Duration>,
    _watch: Option<crate::watch::SettingsWatch>,
    _listener: Option<Arc<Listener>>,
    degraded: Option<String>,
}

/// Where the document comes from.
enum Origin {
    Source(Arc<dyn SettingsSource>),
    File(Option<PathBuf>),
}

impl Origin {
    fn load(&self) -> Map<String, Value> {
        match self {
            Self::Source(source) => source.load(),
            Self::File(Some(path)) => crate::load_at(path),
            Self::File(None) => Map::new(),
        }
    }

    fn stamp(&self) -> Option<SettingsStamp> {
        match self {
            Self::Source(source) => Some(SettingsStamp::Revision(source.revision())),
            Self::File(Some(path)) => crate::stamp_at(path),
            Self::File(None) => None,
        }
    }
}

impl LiveSettings {
    /// Follow this process's settings document: the installed
    /// [`SettingsSource`] when the host provides one, else the configured
    /// settings file. `on_change` runs on the refresh thread after each new
    /// generation is published, so it should only wake whoever reads it.
    ///
    /// The first snapshot is read on the calling thread, so it is current
    /// as soon as this returns.
    pub fn start(on_change: impl Fn() + Send + 'static) -> Self {
        match crate::installed_source() {
            Some(source) => Self::start_from(source, on_change),
            None => Self::start_file(crate::settings_path(), on_change),
        }
    }

    /// Follow the settings file at `path`, ignoring any installed source.
    pub fn start_at(path: PathBuf, on_change: impl Fn() + Send + 'static) -> Self {
        Self::start_file(Some(path), on_change)
    }

    /// Follow a host-provided document through its change signal.
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
            poll: None,
            _watch: None,
            _listener: Some(listener),
            degraded: None,
        };
        Self::spawn(Origin::Source(source), (nudge, nudges), notified, on_change)
    }

    fn start_file(path: Option<PathBuf>, on_change: impl Fn() + Send + 'static) -> Self {
        let (nudge, nudges) = mpsc::channel();
        let watch = path.as_ref().map(|path| {
            let notify = nudge.clone();
            crate::watch::watch_settings_at(path, move || {
                let _ = notify.send(());
            })
        });
        let (watch, degraded) = match watch {
            Some(Ok(watch)) => (Some(watch), None),
            Some(Err(error)) => (None, Some(format!("file watch unavailable: {error}"))),
            None => (None, None),
        };
        // Only a file nobody watches is polled; with no file at all there is
        // nothing to look at.
        let poll = (path.is_some() && watch.is_none()).then_some(POLL_INTERVAL);
        let notified = Notified {
            poll,
            _watch: watch,
            _listener: None,
            degraded,
        };
        Self::spawn(Origin::File(path), (nudge, nudges), notified, on_change)
    }

    fn spawn(
        origin: Origin,
        (nudge, nudges): (mpsc::Sender<()>, mpsc::Receiver<()>),
        mut notified: Notified,
        on_change: impl Fn() + Send + 'static,
    ) -> Self {
        let stamp = origin.stamp();
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
        let poll = notified.poll;
        let worker = tilo_rt::spawn_thread("tilo-settings-live", move || {
            follow(&weak, (stamp, poll), &nudges, &on_change);
        });
        if let Err(error) = worker {
            notified.degraded = Some(format!(
                "refresh thread unavailable, settings stay as read at start: {error}"
            ));
        }
        *inner
            .notified
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(notified);
        Self(inner)
    }

    /// Why changes may arrive late or not at all — no file watch (the stamp
    /// poll still notices a write within [`POLL_INTERVAL`]) or no refresh
    /// thread. `None` when changes arrive as they are made.
    #[must_use]
    pub fn degraded(&self) -> Option<String> {
        self.0
            .notified
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
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
        Arc::clone(
            &self
                .0
                .current
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
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
        let current = self
            .0
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
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
        let current = self
            .0
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(
            &self
                .0
                .changed
                .wait_while(current, |current| current.generation == seen)
                .unwrap_or_else(PoisonError::into_inner),
        )
    }

    /// Ask the refresh thread to re-read now rather than on the next poll —
    /// for a caller that knows the document was written by a path that may
    /// ring nothing.
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

/// The refresh thread: wait for a nudge (or, with `poll`, at most that
/// long), re-read when told to or when the stamp moved, and publish a new
/// generation only when the content changed. Without `poll` it sleeps until
/// something rings. Ends when the last [`LiveSettings`] is dropped.
fn follow(
    inner: &Weak<Inner>,
    (mut stamp, poll): (Option<SettingsStamp>, Option<Duration>),
    nudges: &mpsc::Receiver<()>,
    on_change: &dyn Fn(),
) {
    loop {
        let asked = match poll {
            Some(poll) => match nudges.recv_timeout(poll) {
                Ok(()) => true,
                Err(RecvTimeoutError::Timeout) => false,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            None => match nudges.recv() {
                Ok(()) => true,
                Err(mpsc::RecvError) => return,
            },
        };
        // One read covers every notification queued so far.
        while nudges.try_recv().is_ok() {}
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let current = inner.origin.stamp();
        if !asked && current == stamp {
            continue;
        }
        stamp = current;
        let document = inner.origin.load();
        if publish(&inner, document) {
            on_change();
        }
    }
}

/// Make `document` current unless it equals the current one. Returns whether
/// a new generation was published.
fn publish(inner: &Inner, document: Map<String, Value>) -> bool {
    let mut current = inner.current.lock().unwrap_or_else(PoisonError::into_inner);
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

    #[test]
    fn an_atomic_rewrite_bumps_generation_once_and_updates_the_snapshot() {
        let root = crate::tests::tmpdir();
        let path = root.join("tilo").join("settings.json");
        crate::update_section_at(&path, "display", display(1.0)).unwrap();
        let live = LiveSettings::start_at(path.clone(), || {});
        assert_eq!(live.generation(), 0);
        assert_eq!(live.section("display"), display(1.0));

        crate::update_section_at(&path, "display", display(1.5)).unwrap();
        let snapshot = live.wait_for_change(0, tilo_test_env::hang_bound());
        assert_eq!(snapshot.generation, 1);
        assert_eq!(snapshot.section("display"), display(1.5));
        // Every notification of that one write is coalesced or found equal.
        std::thread::sleep(POLL_INTERVAL.saturating_add(Duration::from_millis(200)));
        assert_eq!(live.generation(), 1, "one write is one generation");

        // A rewrite with identical content is not a change.
        crate::update_section_at(&path, "display", display(1.5)).unwrap();
        std::thread::sleep(POLL_INTERVAL.saturating_add(Duration::from_millis(200)));
        assert_eq!(live.generation(), 1);
        std::fs::remove_dir_all(root).unwrap();
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

    #[test]
    fn a_source_follower_never_looks_between_changes() {
        let source = Arc::new(Counting {
            document: HostDocument::new(Map::new(), |_| Ok(())),
            looks: AtomicU64::new(0),
        });
        let live = LiveSettings::start_from(source.clone(), || {});
        let looks = source.looks.load(Ordering::SeqCst);
        // Longer than the poll interval: a stamp poll would have looked.
        std::thread::sleep(POLL_INTERVAL.saturating_add(Duration::from_millis(500)));
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

    /// The compositor's chain for a hand edit of settings.json: the
    /// session takes the copy in (the host's watch, or the view's own poll of
    /// the copy), the live document and the host view both carry it, and the keyboard
    /// settings read after the change type the new layout.
    #[test]
    fn a_hand_edited_keyboard_layout_reaches_the_live_document_and_its_readers() {
        let dir = tempfile::tempdir().unwrap();
        let paths = crate::SessionPaths::beside(dir.path().join("settings.json"));
        let session = Arc::new(crate::SessionSettings::open(paths.clone()).unwrap());
        // No copy watch in this test: the view polls the copy itself, as a
        // host that could not watch it does.
        let view = session
            .host_view_polling_copy(std::time::Duration::from_millis(50))
            .unwrap();
        let live = LiveSettings::start_from(Arc::clone(&view), || {});
        let mut follower = SectionFollower::new(&live);

        let mut input = Map::new();
        input.insert("keyboard_layout".into(), serde_json::json!("de"));
        input.insert("keyboard_layouts".into(), serde_json::json!(["de"]));
        crate::update_section_at(&paths.copy, "input", input).unwrap();
        let snapshot = live.wait_for_change(0, tilo_test_env::hang_bound());

        assert_eq!(
            follower.changed_sections(&live),
            BTreeSet::from(["input".to_string()])
        );
        let section = |document: &Map<String, Value>| match document.get("input") {
            Some(Value::Object(section)) => section.clone(),
            _ => Map::new(),
        };
        for document in [snapshot.document.clone(), view.load()] {
            let config = crate::hardware::RichInputConfig::from_section(&section(&document));
            let effective =
                crate::keyboard_sources::effective(&config, crate::keyboard_sources::ALL);
            assert_eq!(effective.keyboard_layouts, ["de"]);
            assert_eq!(effective.keyboard_layout, "de");
        }
    }

    #[test]
    fn a_missing_file_reads_as_empty_and_its_creation_is_a_change() {
        let root = crate::tests::tmpdir();
        let path = root.join("tilo").join("settings.json");
        let live = LiveSettings::start_at(path.clone(), || {});
        let mut follower = SectionFollower::new(&live);
        assert!(live.snapshot().document.is_empty());
        assert!(follower.changed_sections(&live).is_empty());

        crate::update_section_at(&path, "display", display(1.25)).unwrap();
        let _ = live.wait_for_change(0, tilo_test_env::hang_bound());
        assert_eq!(
            follower.changed_sections(&live),
            BTreeSet::from(["display".to_string()])
        );

        std::fs::remove_file(&path).unwrap();
        let _ = live.wait_for_change(1, tilo_test_env::hang_bound());
        assert!(live.snapshot().document.is_empty());
        assert_eq!(
            follower.changed_sections(&live),
            BTreeSet::from(["display".to_string()])
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
