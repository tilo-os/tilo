//! Change notification for what this host's authorization decisions read.
//!
//! The collaboration broker re-checks what it already admitted (native
//! context subscribers, membership surfaces, the devices a channel participant
//! lets read) when something those decisions read may have changed, instead
//! of on a fixed clock: the workspace tree, the device fleet and
//! identity registry, the native owner's task store, the devices the network
//! authenticated. Every writer of those calls [`announce`]. A consumer holds a
//! [`Watch`], which wakes it once on the next announcement after it armed it,
//! and compares [`epoch`] with the epoch its last check read: a change made
//! while it was busy with that check is never missed.
//!
//! Time changes decisions as well: a grant, a certificate or a revocation
//! takes effect at an instant known in advance, which no writer announces. A
//! consumer registers the earliest such instant its last check read with
//! [`wake_at`], and is announced to when the wall clock reaches it. On Linux
//! the timer is an absolute `CLOCK_REALTIME` timerfd that is also cancelled by
//! any discontinuous change of the clock (it being set, a step, resume from
//! suspend), so an expiry that passed while the machine slept or the clock
//! jumped is announced at once, not after the monotonic clock caught up.
//!
//! Other processes write some of the inputs: the identity directory (its
//! registry, records and fleets) and the home-peer registry. On Linux they are
//! watched for changes ([`identity_files_watched`],
//! [`tilo_files::native::watch_changes`]); reads are not changes, so a check
//! reading the files does not announce itself.
//!
//! Where a signal cannot be held (no timerfd or inotify, as on macOS
//! development builds, or a directory that cannot be watched),
//! [`timers_reliable`] or [`identity_files_watched`] says so, and the consumer
//! keeps its one-second re-check for the inputs concerned while they exist.
//!
//! The epoch is process-wide rather than per broker, like the relay's
//! `authority_epoch`: writers announce without a handle threaded to
//! them. Two hosts in one process (only tests do that) wake each other; a
//! spurious announcement costs one re-check and never grants anything.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use tilo_lock::MutexRecover as _;

static EPOCH: AtomicU64 = AtomicU64::new(0);
static WATCHES: Mutex<Vec<Weak<WatchState>>> = Mutex::new(Vec::new());

/// Registered instants kept at most. Every consumer registers its earliest
/// instant again after each check, and each announcement makes it check, so
/// dropping the latest instants of an overfull set loses no consumer's next.
const MAX_DEADLINES: usize = 1024;

struct WatchState {
    armed: AtomicBool,
    wake: Box<dyn Fn() + Send + Sync>,
}

/// Wakes its owner once on the next [`announce`] after each [`Self::arm`].
///
/// Dropping it unregisters the wake.
pub struct Watch(Arc<WatchState>);

impl Watch {
    /// A watch that runs `wake` on announcements. It is not armed yet.
    /// `wake` must not block: it runs on the announcing thread, which may
    /// hold the lock of what it changed.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        let state = Arc::new(WatchState {
            armed: AtomicBool::new(false),
            wake: Box::new(wake),
        });
        let mut watches = WATCHES.lock_recover();
        watches.retain(|watch| watch.strong_count() > 0);
        watches.push(Arc::downgrade(&state));
        drop(watches);
        Self(state)
    }

    /// Wake once on the next announcement, and return [`epoch`] as of
    /// arming. A value other than the epoch the caller last checked at means
    /// a change already arrived, which this arming will not wake for.
    pub fn arm(&self) -> u64 {
        self.0.armed.store(true, Ordering::SeqCst);
        epoch()
    }
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watch")
            .field("armed", &self.0.armed.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// How many announcements this process has made.
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}

/// Announce that something an authorization decision reads may have
/// changed. Cheap: an atomic increment, and a wake for each armed watch.
pub fn announce() {
    // `fetch_add` wraps; consumers only compare epochs for equality.
    EPOCH.fetch_add(1, Ordering::SeqCst);
    let due: Vec<Arc<WatchState>> = {
        let watches = WATCHES.lock_recover();
        watches
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|watch| watch.armed.swap(false, Ordering::SeqCst))
            .collect()
    };
    for watch in due {
        (watch.wake)();
    }
}

/// [`announce`] once the wall clock reaches `unix_ns` (Unix nanoseconds), or
/// at once if it already has. Registering an instant again costs nothing.
pub fn wake_at(unix_ns: u64) {
    if let Some(clock) = clock() {
        clock.wake_at(unix_ns);
    }
}

/// Whether [`wake_at`] is announced on time even across suspend and clock
/// changes. Without it a consumer with time-bound inputs keeps its
/// one-second re-check.
pub fn timers_reliable() -> bool {
    clock().is_some_and(|clock| clock.platform.reliable())
}

/// Whether writes to the identity directory and the home-peer registry, by
/// this process or any other, are announced. Without it a consumer that
/// reads the device fleet keeps its one-second re-check while it does.
///
/// Each call also starts watching a directory that came into existence since
/// the last one.
pub fn identity_files_watched() -> bool {
    clock().is_some_and(Clock::identity_files_watched)
}

/// The process's clock and file watcher, started on first use; `None` when
/// its thread could not be started.
fn clock() -> Option<&'static Clock> {
    static CLOCK: OnceLock<Option<&'static Clock>> = OnceLock::new();
    *CLOCK.get_or_init(Clock::start)
}

/// The thread that turns registered instants, clock changes and identity
/// file writes into announcements. It blocks until one of them happens.
struct Clock {
    deadlines: Mutex<BTreeSet<u64>>,
    platform: platform::Clock,
}

impl Clock {
    fn start() -> Option<&'static Self> {
        let clock: &'static Self = Box::leak(Box::new(Self {
            deadlines: Mutex::new(BTreeSet::new()),
            platform: platform::Clock::open(),
        }));
        std::thread::Builder::new()
            .name("tilo-policy-clk".into())
            .spawn(move || clock.platform.run(clock))
            .map_err(|error| tracing::warn!(%error, "policy clock thread could not start"))
            .ok()?;
        Some(clock)
    }

    fn wake_at(&self, unix_ns: u64) {
        let mut deadlines = self.deadlines.lock_recover();
        let earliest = deadlines.first().copied();
        if !deadlines.insert(unix_ns) {
            return;
        }
        while deadlines.len() > MAX_DEADLINES {
            deadlines.pop_last();
        }
        if earliest.is_none_or(|earliest| unix_ns < earliest) {
            self.platform.arm(Some(unix_ns));
        }
        drop(deadlines);
    }

    /// The timer fired or the clock changed: drop every instant that has
    /// passed, arm the next, and announce.
    fn fire(&self) {
        let now = tilo_clock::unix_now_ns();
        let mut deadlines = self.deadlines.lock_recover();
        let later = deadlines.split_off(&now.saturating_add(1));
        *deadlines = later;
        self.platform.arm(deadlines.first().copied());
        drop(deadlines);
        announce();
    }

    fn identity_files_watched(&self) -> bool {
        self.platform.sync_watches()
    }
}

/// The directories whose writes change what the device fleet and identity
/// registry answer, each with whether its subdirectories are watched too.
#[cfg(target_os = "linux")]
fn identity_directories() -> Vec<(std::path::PathBuf, bool)> {
    let mut directories = Vec::with_capacity(2);
    if let Ok(registry) = crate::registry::IdentityRegistryStore::from_env() {
        directories.push((registry.identity_dir().to_path_buf(), true));
    }
    let paths = crate::provisioning::collaboration_identity_paths();
    if let Some(root) = paths.user_pubkey.parent().and_then(std::path::Path::parent)
        && let Some(peers) = tilo_relay_protocol::home_peer_registry_path(root).parent()
    {
        directories.push((peers.to_path_buf(), false));
    }
    directories
}

#[cfg(target_os = "linux")]
mod platform {
    //! timerfd for instants and clock changes, waited on by a `poll` that
    //! never times out, and change watches for identity files.

    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use tilo_files::native::{ChangeWatch, watch_changes, watch_presence, watch_tree_changes};
    use tilo_lock::MutexRecover as _;

    pub(super) struct Clock {
        timer: Option<OwnedFd>,
        watches: Mutex<Watches>,
        /// An identity directory reported a change since the last sync: one
        /// may have been created or removed, which moves its watch.
        changed: Arc<AtomicBool>,
        /// The thread stopped waiting on the timer: it no longer holds.
        stopped: AtomicBool,
    }

    #[derive(Default)]
    struct Watches {
        /// Each identity directory, the directory it is watched through
        /// (itself, or its parent while it does not exist), and the watch.
        armed: Vec<(PathBuf, PathBuf, ChangeWatch)>,
        /// The identity directories the last sync covered, and whether it
        /// covered all of them.
        synced: Option<(Vec<(PathBuf, bool)>, bool)>,
    }

    impl Clock {
        pub(super) fn open() -> Self {
            // SAFETY: plain syscall; a returned descriptor is checked and
            // owned exactly once.
            let timer = unsafe {
                let fd = libc::timerfd_create(
                    libc::CLOCK_REALTIME,
                    libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
                );
                (fd >= 0).then(|| OwnedFd::from_raw_fd(fd))
            };
            if timer.is_none() {
                tracing::warn!("policy clock degraded: consumers keep a one-second re-check");
            }
            Self {
                timer,
                watches: Mutex::new(Watches::default()),
                changed: Arc::new(AtomicBool::new(true)),
                stopped: AtomicBool::new(false),
            }
        }

        pub(super) fn reliable(&self) -> bool {
            self.timer.is_some() && !self.stopped.load(Ordering::SeqCst)
        }

        /// Arm the timer for `at` (Unix nanoseconds), or disarm it.
        pub(super) fn arm(&self, at: Option<u64>) {
            let Some(timer) = &self.timer else {
                return;
            };
            let value = at.map_or(
                libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                |at| {
                    // Zero would disarm: an instant at the epoch fires at
                    // its first nanosecond instead.
                    let at = at.max(1);
                    libc::timespec {
                        tv_sec: libc::time_t::try_from(at / 1_000_000_000)
                            .unwrap_or(libc::time_t::MAX),
                        tv_nsec: libc::c_long::try_from(at % 1_000_000_000).unwrap_or(0),
                    }
                },
            );
            let spec = libc::itimerspec {
                it_interval: libc::timespec {
                    tv_sec: 0,
                    tv_nsec: 0,
                },
                it_value: value,
            };
            // SAFETY: `timer` is a live timerfd and `spec` a valid value.
            let armed = unsafe {
                libc::timerfd_settime(
                    timer.as_raw_fd(),
                    libc::TFD_TIMER_ABSTIME | libc::TFD_TIMER_CANCEL_ON_SET,
                    &raw const spec,
                    std::ptr::null_mut(),
                )
            };
            if armed != 0 {
                tracing::warn!(
                    error = %std::io::Error::last_os_error(),
                    "policy clock could not arm its timer"
                );
            }
        }

        /// Watch every identity directory that exists, or its parent while
        /// it does not, so its creation is seen; one that holds the identity
        /// registry with every directory below it. `false` when one of them
        /// cannot be watched. Touches the filesystem only when a watched
        /// directory changed, the directories moved (a test's environment)
        /// or the last sync was incomplete; otherwise it is a lock and a
        /// comparison, cheap enough for every policy check.
        pub(super) fn sync_watches(&self) -> bool {
            let directories = super::identity_directories();
            let mut watches = self.watches.lock_recover();
            let changed = self.changed.swap(false, Ordering::SeqCst);
            if !changed
                && let Some((synced, true)) = &watches.synced
                && *synced == directories
            {
                return true;
            }
            let mut complete = true;
            let mut armed = Vec::with_capacity(directories.len());
            for (directory, with_children) in &directories {
                let through = if directory.is_dir() {
                    directory.clone()
                } else if let Some(parent) = directory.parent().filter(|parent| parent.is_dir()) {
                    parent.to_path_buf()
                } else {
                    complete = false;
                    continue;
                };
                if let Some(index) = watches
                    .armed
                    .iter()
                    .position(|(target, held, _)| target == directory && *held == through)
                {
                    armed.push(watches.armed.swap_remove(index));
                    continue;
                }
                match self.watch(directory, &through, *with_children) {
                    Ok(watch) => armed.push((directory.clone(), through, watch)),
                    Err(error) => {
                        tracing::debug!(
                            directory = %through.display(),
                            %error,
                            "identity directory could not be watched"
                        );
                        complete = false;
                    }
                }
            }
            watches.armed = armed;
            watches.synced = Some((directories, complete));
            drop(watches);
            complete
        }

        /// A watch on `through`, `directory` itself or its parent, that
        /// announces each change. The next sync moves it once the directory
        /// appears or goes.
        fn watch(
            &self,
            directory: &Path,
            through: &Path,
            with_children: bool,
        ) -> std::io::Result<ChangeWatch> {
            let changed = Arc::clone(&self.changed);
            let announce = move || {
                changed.store(true, Ordering::SeqCst);
                super::announce();
            };
            if through != directory {
                let target = directory.to_path_buf();
                return watch_presence(through, move |entry| entry == target, announce);
            }
            if with_children {
                watch_tree_changes(directory, |_| true, announce)
            } else {
                watch_changes(directory, |_| true, announce)
            }
        }

        pub(super) fn run(&self, clock: &super::Clock) {
            let Some(timer) = &self.timer else {
                return;
            };
            let mut fds = [libc::pollfd {
                fd: timer.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            }];
            loop {
                // SAFETY: `fds` holds one initialised entry.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, -1) };
                if ready < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    self.stop(&error);
                    return;
                }
                let [entry] = &mut fds;
                if entry.revents == 0 {
                    continue;
                }
                if entry.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                    self.stop(&std::io::Error::other("descriptor failed"));
                    return;
                }
                entry.revents = 0;
                drain(entry.fd);
                // Expired, or cancelled by a clock change: either way the
                // clock moved past what was armed.
                clock.fire();
            }
        }

        /// Stop for good: consumers fall back to their one-second re-check,
        /// and one last announcement makes them notice.
        fn stop(&self, error: &std::io::Error) {
            tracing::warn!(%error, "policy clock stopped; consumers keep a one-second re-check");
            self.stopped.store(true, Ordering::SeqCst);
            super::announce();
        }
    }

    /// Read whatever `fd` holds; its content is not needed.
    fn drain(fd: libc::c_int) {
        let mut buffer = [0_u8; 4096];
        loop {
            // SAFETY: `buffer` is writable for its whole length.
            let read = unsafe { libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len()) };
            if read <= 0 {
                return;
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    //! Without timerfd: wait on the wall clock in steps of at most a second
    //! while an instant is registered, and report the timer unreliable.

    use std::sync::{Condvar, Mutex, PoisonError};
    use std::time::Duration;
    use tilo_lock::MutexRecover as _;

    pub(super) struct Clock {
        next: Mutex<Option<u64>>,
        changed: Condvar,
    }

    impl Clock {
        pub(super) fn open() -> Self {
            Self {
                next: Mutex::new(None),
                changed: Condvar::new(),
            }
        }

        pub(super) const fn reliable(&self) -> bool {
            false
        }

        pub(super) fn arm(&self, at: Option<u64>) {
            *self.next.lock_recover() = at;
            self.changed.notify_one();
        }

        pub(super) const fn sync_watches(&self) -> bool {
            false
        }

        pub(super) fn run(&self, clock: &super::Clock) {
            let mut next = self.next.lock_recover();
            loop {
                match *next {
                    None => {
                        next = self
                            .changed
                            .wait(next)
                            .unwrap_or_else(PoisonError::into_inner);
                    }
                    Some(at) => {
                        let now = tilo_clock::unix_now_ns();
                        if now >= at {
                            drop(next);
                            clock.fire();
                            next = self.next.lock_recover();
                            continue;
                        }
                        let wait = Duration::from_nanos(at.saturating_sub(now))
                            .min(Duration::from_secs(1));
                        next = self
                            .changed
                            .wait_timeout(next, wait)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn watch() -> (Watch, mpsc::Receiver<()>) {
        let (sender, receiver) = mpsc::channel();
        let sender = Mutex::new(sender);
        let watch = Watch::new(move || {
            let _ = sender.lock_recover().send(());
        });
        (watch, receiver)
    }

    #[test]
    fn an_armed_watch_wakes_once_and_an_unarmed_one_not_at_all() {
        let (watch, woke) = watch();
        announce();
        assert!(woke.try_recv().is_err(), "not armed yet");
        let armed_at = watch.arm();
        announce();
        announce();
        tilo_test_env::recv("the armed watch to wake", &woke);
        assert!(woke.try_recv().is_err(), "one wake per arming");
        assert_ne!(epoch(), armed_at, "the epoch moved past the arming");
    }

    #[test]
    fn a_change_announced_before_arming_shows_in_the_epoch() {
        let (watch, _woke) = watch();
        let checked_at = epoch();
        announce();
        assert_ne!(
            watch.arm(),
            checked_at,
            "arming reports the change it will not wake for"
        );
    }

    #[test]
    fn a_dropped_watch_is_not_woken() {
        let (watch, woke) = watch();
        let _ = watch.arm();
        drop(watch);
        announce();
        assert!(woke.try_recv().is_err());
    }

    #[test]
    fn an_instant_is_announced_when_the_wall_clock_reaches_it() {
        let (watch, woke) = watch();
        let at = tilo_clock::unix_now_ns().saturating_add(300_000_000);
        // Other tests announce too: wait until the announcement comes at or
        // after the instant.
        wake_at(at);
        loop {
            let _ = watch.arm();
            tilo_test_env::recv("the instant to be announced", &woke);
            if tilo_clock::unix_now_ns() >= at {
                break;
            }
        }
    }

    #[test]
    fn an_instant_already_past_is_announced_at_once() {
        let (watch, woke) = watch();
        let _ = watch.arm();
        wake_at(tilo_clock::unix_now_ns().saturating_sub(1_000_000_000));
        tilo_test_env::recv("the past instant to be announced", &woke);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_linux_clock_follows_clock_changes() {
        assert!(timers_reliable());
    }
}
