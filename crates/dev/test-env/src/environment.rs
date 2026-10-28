//! One test environment: a fixture tree, the wall clock and the network a
//! test drives code through, instead of a temporary directory and the
//! developer's machine.
//!
//! [`crate::TestHome`] already points `HOME` and every XDG base directory at
//! a fresh temporary tree. [`TestEnv`] keeps that home and adds the two other
//! things a test otherwise reaches for and gets at random:
//!
//! - [`Clock`], the wall clock. A test that asserts on a timestamp, a
//!   deadline or an ordering reads it here, and it starts at
//!   [`CAPTURE_LOCAL_TIME`] (30 June 2026, 10:42 local) — the moment a
//!   `--shots` capture shows — so a fixture and its screenshots agree.
//! - [`Network`], whether anything may leave the process. It is off: a test
//!   that needs the internet says so with
//!   [`Network::allow_external`], and every other test fails loudly instead of
//!   quietly depending on what the developer's machine can reach.
//!
//! Files are the fixture tree itself: [`TestEnv::with_files`] writes the files
//! a test starts from, as it would write them in the home it built.
//!
//! ```no_run
//! let env = tilo_test_env::TestEnv::new().unwrap();
//! # struct Settings;
//! # fn read(env: &tilo_test_env::TestEnv) -> Settings { Settings }
//! // Production code that resolves the person's configuration itself:
//! let settings = read(&env);
//! assert!(env.today() >= env.clock().local().date_naive());
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::TestHome;

/// The local date and time every test environment starts at.
///
/// The same reading `--shots` pins for its captures (30 June 2026, 10:42
/// local): a fixture a test drives and a screenshot of it show one clock.
pub const CAPTURE_LOCAL_TIME: (i32, u32, u32, u32, u32) = (2026, 6, 30, 10, 42);

/// The wall clock a test reads.
///
/// It starts at [`CAPTURE_LOCAL_TIME`] and moves only when the test moves it,
/// so two runs of one test see the same instants in the same order. Read it
/// instead of the system clock: code under test that reaches for the real
/// clock is what makes a test pass at noon and fail at midnight.
#[derive(Debug)]
pub struct Clock {
    millis: std::sync::Mutex<i64>,
}

impl Clock {
    /// A clock reading [`CAPTURE_LOCAL_TIME`] in the local time zone.
    fn capture_local() -> Self {
        Self {
            // A local time zone `chrono` cannot resolve leaves the clock at
            // the epoch: a test's instant is still ordered and still moves
            // only when the test moves it, which is what it asserts on.
            millis: std::sync::Mutex::new(capture_millis().unwrap_or(0)),
        }
    }

    /// The instant this clock reads, as milliseconds since the Unix epoch.
    #[must_use]
    pub fn millis(&self) -> i64 {
        *self
            .millis
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The instant this clock reads, in UTC.
    #[must_use]
    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(self.millis()).unwrap_or_else(|| {
            chrono::DateTime::<chrono::Utc>::from_timestamp_millis(0).unwrap_or_default()
        })
    }

    /// The instant this clock reads, in the local time zone.
    #[must_use]
    pub fn local(&self) -> chrono::DateTime<chrono::Local> {
        chrono::DateTime::<chrono::Local>::from(self.now())
    }

    /// Move the clock in time: forward for `duration`, back for a negative
    /// one. A test advances to the moment a deadline is due rather than
    /// sleeping until it is.
    pub fn advance(&self, duration: Duration) {
        let delta = i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
        let mut millis = self
            .millis
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *millis = millis.saturating_add(delta);
    }

    /// Set the clock to an instant, as milliseconds since the Unix epoch.
    pub fn set_millis(&self, millis: i64) {
        *self
            .millis
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = millis;
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::capture_local()
    }
}

/// [`CAPTURE_LOCAL_TIME`] in the local time zone, as milliseconds since the
/// Unix epoch.
fn capture_millis() -> Option<i64> {
    use chrono::TimeZone as _;
    let (year, month, day, hour, minute) = CAPTURE_LOCAL_TIME;
    let local = chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, 0)?;
    chrono::Local
        .from_local_datetime(&local)
        .earliest()
        .map(|at| at.timestamp_millis())
}

/// Whether a test may reach the network.
///
/// Off by default. A test whose subject is a request says
/// [`allow_external`](Self::allow_external) first, so the dependency is
/// written down where it is taken, and a lane that requires it
/// (`crate::capability`) can still prove the host has it.
#[derive(Debug, Clone, Default)]
pub struct Network {
    external: bool,
}

impl Network {
    /// A network nothing may leave: what a test gets unless it asks.
    #[must_use]
    pub fn sealed() -> Self {
        Self { external: false }
    }

    /// Allow the test to reach hosts beyond this machine.
    pub fn allow_external(&mut self) {
        self.external = true;
    }

    /// Whether the test may reach hosts beyond this machine.
    #[must_use]
    pub fn external_allowed(&self) -> bool {
        self.external
    }
}

/// One test environment: a temporary home with every XDG base directory
/// pointed into it, a clock the test moves, and a network that starts closed.
///
/// Holds the process-wide environment lock ([`crate::lock`]) for its whole
/// life, as [`TestHome`] does, and restores every variable it set when
/// dropped.
pub struct TestEnv {
    home: TestHome,
    clock: Clock,
    network: Network,
}

impl TestEnv {
    /// Create the fixture tree and point the environment at it.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if the temporary tree cannot be created.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            home: TestHome::new()?,
            clock: Clock::default(),
            network: Network::sealed(),
        })
    }

    /// Write the files a test starts from — `(path, contents)`, relative to
    /// the home — and return the environment.
    ///
    /// Parent directories are created. A path that is absolute is written
    /// where it names, for a fixture a caller placed itself.
    ///
    /// # Errors
    ///
    /// Returns the I/O error if a file or one of its directories cannot be
    /// written.
    pub fn with_files(self, tree: &[(impl AsRef<Path>, &[u8])]) -> io::Result<Self> {
        for (path, contents) in tree {
            let path = self.home.home().join(path.as_ref());
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&path, contents)?;
        }
        Ok(self)
    }

    /// The wall clock, which moves only when the test moves it.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// Whether anything may leave the process.
    #[must_use]
    pub fn network(&self) -> &Network {
        &self.network
    }

    /// Let the test reach hosts beyond this machine
    /// ([`Network::allow_external`]).
    pub fn allow_external(&mut self) {
        self.network.allow_external();
    }

    /// The fixture tree: `HOME` and every XDG base directory below it.
    #[must_use]
    pub fn home(&self) -> &TestHome {
        &self.home
    }

    /// The temporary directory holding the home and runtime trees.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.home.root()
    }

    /// `$HOME`.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.home.home()
    }

    /// `$XDG_DATA_HOME`.
    #[must_use]
    pub fn data(&self) -> &Path {
        self.home.data()
    }

    /// `$XDG_CONFIG_HOME`.
    #[must_use]
    pub fn config(&self) -> &Path {
        self.home.config()
    }

    /// `$XDG_STATE_HOME`.
    #[must_use]
    pub fn state(&self) -> &Path {
        self.home.state()
    }

    /// `$XDG_CACHE_HOME`.
    #[must_use]
    pub fn cache(&self) -> &Path {
        self.home.cache()
    }

    /// `$XDG_RUNTIME_DIR`.
    #[must_use]
    pub fn runtime(&self) -> &Path {
        self.home.runtime()
    }

    /// Today's date on this environment's clock.
    #[must_use]
    pub fn today(&self) -> chrono::NaiveDate {
        self.clock.local().date_naive()
    }

    /// A path inside the home, whether or not anything wrote it yet.
    #[must_use]
    pub fn join(&self, path: impl AsRef<Path>) -> PathBuf {
        self.home.home().join(path)
    }

    /// The scope holding the process environment; more changes made through
    /// it are undone with the environment.
    pub fn env(&mut self) -> &mut crate::ScopedEnv {
        self.home.env()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario;
    use chrono::{Datelike as _, Timelike as _};

    #[test]
    fn a_clock_starts_at_the_capture_moment_and_moves_only_when_the_test_moves_it() {
        // Phase: a_clock_reads_the_capture_moment_of_its_own_time_zone.
        {
            let clock = Clock::default();
            let at = clock.local();
            let (year, month, day, hour, minute) = CAPTURE_LOCAL_TIME;
            assert_eq!(
                (at.year(), at.month(), at.day(), at.hour(), at.minute()),
                (year, month, day, hour, minute,),
                "the clock reads the moment a capture shows",
            );
            assert_eq!(clock.local(), at, "reading it does not move it");
        }

        // Phase: advancing_moves_the_reading_and_keeps_the_order.
        {
            let clock = Clock::default();
            let start = clock.millis();
            clock.advance(Duration::from_secs(90));
            assert_eq!(clock.millis(), start + 90_000);
            assert_eq!(
                clock.now(),
                chrono::DateTime::<chrono::Utc>::from_timestamp_millis(start + 90_000).unwrap(),
            );
            clock.set_millis(0);
            assert_eq!(clock.millis(), 0, "an instant can be set outright");
        }
    }

    #[test]
    fn a_fixture_tree_is_written_into_the_environment_and_read_back() {
        // Phase: files_are_written_under_the_home_and_its_directories_exist.
        {
            let env = TestEnv::new()
                .unwrap()
                .with_files(&[
                    ("notes/Todo.md", b"buy milk".as_slice()),
                    (".config/example.toml", b"theme = light".as_slice()),
                ])
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(env.join("notes/Todo.md")).unwrap(),
                "buy milk",
            );
            assert_eq!(
                std::fs::read_to_string(env.config().join("example.toml")).unwrap(),
                "theme = light",
                "a configuration file lands where the process reads it",
            );
            assert!(env.path().is_dir(), "the home exists");
        }

        // Phase: dropping_the_environment_removes_the_tree.
        {
            let root;
            {
                let env = TestEnv::new().unwrap();
                root = env.root().to_path_buf();
                assert!(root.is_dir());
            }
            assert!(!root.exists(), "the fixture tree is removed on drop");
        }

        // Phase: the_environment_starts_sealed_and_says_when_it_is_not.
        {
            let mut env = TestEnv::new().unwrap();
            assert!(!env.network().external_allowed(), "nothing leaves at first");
            env.allow_external();
            assert!(env.network().external_allowed());
        }
    }

    scenario! {
        fn an_environment_carries_the_clock_a_test_asserts_on;

        phase today_follows_the_environment_s_clock;
        {
            let env = TestEnv::new().unwrap();
            let (year, month, day, _, _) = CAPTURE_LOCAL_TIME;
            let first = chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap();
            assert_eq!(env.today(), first);
            env.clock().advance(Duration::from_secs(24 * 60 * 60));
            assert_eq!(
                env.today(),
                first.succ_opt().unwrap(),
                "a day later is a day later on the fixture's clock",
            );
        }
    }
}
