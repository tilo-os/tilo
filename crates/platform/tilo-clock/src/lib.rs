//! Reading the time the same way on every target: [`now`] and [`wall`] are the
//! sanctioned monotonic and wall-clock reads, and [`time`] names the clock types
//! instead of `std::time`'s, which panic in a browser page. [`Clock`] is an
//! injectable time source, for deadline behaviour a test reaches without sleeping.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The clock as a person reads it: [`local::now`] is the wall clock in their zone. A device
/// reads the zone from its time-zone database; a page asks the browser, which answers the
/// zone's offset for each instant and so knows an hour's change across a year.
#[cfg(feature = "local")]
pub mod local {
    use jiff::civil::{Date, DateTime};
    use jiff::{Timestamp, Zoned};

    /// The person's zone.
    #[derive(Clone, Debug)]
    pub struct Zone(platform::Zone);

    impl Zone {
        /// The zone of the device or page this is read on: the one [`install`] handed the thread
        /// it is read on, where it was handed one.
        #[must_use]
        pub fn here() -> Self {
            if let Some(zone) = told::installed() {
                return zone;
            }
            Self(platform::Zone::here())
        }

        /// The zone `name` (`Europe/Vienna`) names, as this device or page knows it: a device
        /// from its zone database, a page from the browser, which knows every zone's offsets
        /// without one. `None` where neither knows the name.
        #[must_use]
        pub fn named(name: &str) -> Option<Self> {
            if !crate::is_zone_name(name) {
                return None;
            }
            #[cfg(all(target_family = "wasm", not(tilo_no_js)))]
            {
                platform::Zone::named(name).map(Self)
            }
            #[cfg(any(not(target_family = "wasm"), tilo_no_js))]
            {
                jiff::tz::TimeZone::get(name)
                    .ok()
                    .map(|zone| Self(platform::Zone::of(zone)))
            }
        }

        /// The zone `name` names, read from its `TZif` `data`: what a process that opens no file
        /// reads a zone from ([`tzif`]). `None` where the data is not a zone's.
        #[cfg(not(target_family = "wasm"))]
        #[must_use]
        pub fn from_tzif(name: &str, data: &[u8]) -> Option<Self> {
            jiff::tz::TimeZone::tzif(name, data).ok().map(Self::of)
        }

        /// The zone a device's database names, for a test that reads a time somewhere else.
        #[cfg(not(target_family = "wasm"))]
        #[must_use]
        pub fn of(zone: jiff::tz::TimeZone) -> Self {
            Self(platform::Zone::of(zone))
        }

        /// The instant `at` as the person reads it: the zone's offset then, which a page asks
        /// the browser for rather than reading a database it does not have.
        #[must_use]
        pub fn at(&self, at: Timestamp) -> Zoned {
            self.0.at(at)
        }

        /// The zone's name (`Europe/Vienna`), where it has one.
        #[must_use]
        pub fn name(&self) -> Option<String> {
            self.0.name()
        }

        /// The instant `local` names in the zone: the earlier one where the clocks went back,
        /// none where they skipped it.
        #[must_use]
        pub fn instant_of(&self, local: DateTime) -> Option<Timestamp> {
            self.0.instant_of(local)
        }
    }

    pub use told::install;
    #[cfg(not(target_family = "wasm"))]
    pub use zone_data::tzif;

    /// The names of the zones this device or page knows (`Europe/Vienna`), sorted: a device's
    /// zone database, a page's browser.
    #[must_use]
    pub fn zone_names() -> Vec<String> {
        #[cfg(all(target_family = "wasm", not(tilo_no_js)))]
        let mut names = crate::page::zone_names();
        #[cfg(any(not(target_family = "wasm"), tilo_no_js))]
        let mut names: Vec<String> = jiff::tz::db()
            .available()
            .map(|name| name.as_str().to_owned())
            .filter(|name| crate::is_zone_name(name))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// A zone a thread is told rather than reads from its system: a worker that renders a
    /// view for a person whose clock is elsewhere, or a page whose person chose a zone other
    /// than the browser's.
    mod told {
        use std::cell::RefCell;

        use super::Zone;

        thread_local! {
            /// The zone [`install`] handed this thread, if any.
            static INSTALLED: RefCell<Option<Zone>> = const { RefCell::new(None) };
        }

        pub(super) fn installed() -> Option<Zone> {
            INSTALLED.with(|installed| installed.borrow().clone())
        }

        /// Read local times in `zone` from now on, on this thread, in place of the system's;
        /// `None` goes back to the system's. A worker that serves several people at once
        /// renders each one's views on a thread of its own, so each thread keeps its zone.
        pub fn install(zone: Option<Zone>) {
            INSTALLED.with(|installed| *installed.borrow_mut() = zone);
        }
    }

    /// The zone data a confined worker, which opens no file once it confined itself, is
    /// handed: the person's zone as its `TZif` data.
    #[cfg(not(target_family = "wasm"))]
    mod zone_data {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use std::sync::{Arc, Mutex, PoisonError};

        use super::Zone;

        /// The zones [`tzif`] read, by name, and the names it found none for.
        type Read = HashMap<String, Option<Arc<[u8]>>>;
        static READ: Mutex<Option<Read>> = Mutex::new(None);

        /// Most names [`tzif`] remembers; past it, it reads again rather than grow.
        const MAX_REMEMBERED: usize = 1_024;

        /// The `TZif` data of the zone `name` (`Pacific/Auckland`), from the system's zone
        /// database: what [`Zone::from_tzif`] reads it from in a process that opens no file.
        /// `None` where `name` is no zone the database holds.
        #[must_use]
        pub fn tzif(name: &str) -> Option<Arc<[u8]>> {
            if !crate::is_zone_name(name) {
                return None;
            }
            let known = READ
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .and_then(|read| read.get(name).cloned());
            if let Some(known) = known {
                return known;
            }
            let found = databases()
                .iter()
                .find_map(|database| std::fs::read(database.join(name)).ok())
                .filter(|data| Zone::from_tzif(name, data).is_some())
                .map(Arc::<[u8]>::from);
            let mut remembered = READ.lock().unwrap_or_else(PoisonError::into_inner);
            let read = remembered.get_or_insert_with(HashMap::new);
            if read.len() >= MAX_REMEMBERED {
                read.clear();
            }
            read.insert(name.to_owned(), found.clone());
            drop(remembered);
            found
        }

        /// Where the system keeps its zone database, where jiff looks for it.
        fn databases() -> Vec<PathBuf> {
            let mut databases: Vec<PathBuf> = std::env::var_os("TZDIR")
                .map(PathBuf::from)
                .filter(|dir| dir.is_absolute())
                .into_iter()
                .collect();
            databases.extend(
                [
                    "/usr/share/zoneinfo",
                    "/usr/share/lib/zoneinfo",
                    "/etc/zoneinfo",
                ]
                .into_iter()
                .map(PathBuf::from),
            );
            databases
        }

        #[cfg(test)]
        mod tests {
            use super::tzif;
            use crate::is_zone_name;

            #[test]
            fn only_a_zones_name_is_read() {
                assert!(is_zone_name("Pacific/Auckland"));
                assert!(is_zone_name("America/Argentina/Buenos_Aires"));
                assert!(is_zone_name("Etc/GMT+12"));
                assert!(is_zone_name("UTC"));
                assert!(!is_zone_name(""));
                assert!(!is_zone_name("/etc/passwd"));
                assert!(!is_zone_name("../../etc/passwd"));
                assert!(!is_zone_name("Europe/../../x"));
                assert!(!is_zone_name("Europe//Vienna"));
                assert!(!is_zone_name("Europe/Vi enna"));
                assert!(tzif("../../etc/passwd").is_none());
            }

            /// A zone read from its data reads an instant as the database's zone does.
            #[test]
            fn a_zone_read_from_its_data_is_the_databases() {
                let Some(data) = tzif("Pacific/Auckland") else {
                    // A system without a zone database has nothing to read.
                    return;
                };
                let zone = super::Zone::from_tzif("Pacific/Auckland", &data).unwrap();
                let at = jiff::Timestamp::from_second(1_773_583_500).unwrap();
                assert_eq!(zone.name().as_deref(), Some("Pacific/Auckland"));
                assert_eq!(
                    zone.at(at).offset().seconds(),
                    13 * 3600,
                    "summer in Auckland"
                );
            }
        }
    }

    /// The current instant, read from the device or page's wall clock.
    #[must_use]
    pub fn now_instant() -> Timestamp {
        Timestamp::try_from(crate::to_std(crate::wall())).unwrap_or(Timestamp::UNIX_EPOCH)
    }

    /// The current local date and time.
    #[must_use]
    pub fn now() -> Zoned {
        at(now_instant())
    }

    /// The instant `at` as the person reads it.
    #[must_use]
    pub fn at(at: Timestamp) -> Zoned {
        Zone::here().at(at)
    }

    /// Today's local date.
    #[must_use]
    pub fn today() -> Date {
        now().date()
    }

    /// The instant `local` names in the person's zone.
    #[must_use]
    pub fn instant_of(local: DateTime) -> Option<Timestamp> {
        Zone::here().instant_of(local)
    }

    /// The instant `local` names, from the zone's offset at each instant: the offset a page has
    /// is the browser's answer, so the offsets either side of a change of clocks are read a day
    /// out, and the instants they name are kept when they read back as `local` at that offset.
    #[cfg(any(test, all(target_family = "wasm", not(tilo_no_js))))]
    fn resolve(
        local: DateTime,
        offset_at: impl Fn(Timestamp) -> jiff::tz::Offset,
    ) -> Option<Timestamp> {
        /// Wider than any offset from UTC, so a change of clocks is inside it.
        const A_DAY: jiff::SignedDuration = jiff::SignedDuration::from_hours(26);
        let bare = local.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp();
        let around = [
            bare.checked_sub(A_DAY).ok()?,
            bare,
            bare.checked_add(A_DAY).ok()?,
        ];
        let instants = around.map(|instant| {
            let offset = offset_at(instant);
            bare.checked_sub(jiff::SignedDuration::from_secs(i64::from(offset.seconds())))
                .ok()
        });
        instants
            .into_iter()
            .flatten()
            .filter(|at| read(*at, &offset_at).datetime() == local)
            .min()
    }

    /// The instant `at` as a zone of the offsets `offset_at` reads it.
    #[cfg(any(test, all(target_family = "wasm", not(tilo_no_js))))]
    fn read(at: Timestamp, offset_at: impl Fn(Timestamp) -> jiff::tz::Offset) -> Zoned {
        at.to_zoned(jiff::tz::TimeZone::fixed(offset_at(at)))
    }

    /// The system's zone database.
    #[cfg(any(not(target_family = "wasm"), tilo_no_js))]
    mod platform {
        use jiff::civil::DateTime;
        use jiff::tz::{AmbiguousOffset, TimeZone};
        use jiff::{Timestamp, Zoned};

        #[derive(Clone, Debug)]
        pub(super) struct Zone(TimeZone);

        impl Zone {
            pub(super) fn here() -> Self {
                Self(TimeZone::system())
            }

            pub(super) fn of(zone: TimeZone) -> Self {
                Self(zone)
            }

            pub(super) fn at(&self, at: Timestamp) -> Zoned {
                at.to_zoned(self.0.clone())
            }

            pub(super) fn name(&self) -> Option<String> {
                self.0.iana_name().map(str::to_owned)
            }

            pub(super) fn instant_of(&self, local: DateTime) -> Option<Timestamp> {
                let named = self.0.to_ambiguous_timestamp(local);
                if matches!(named.offset(), AmbiguousOffset::Gap { .. }) {
                    return None;
                }
                named.earlier().ok()
            }
        }
    }

    /// The browser's zone, or a zone the person chose that the browser reads for them: the offset
    /// the browser reports for each instant, and the zone's name.
    #[cfg(all(target_family = "wasm", not(tilo_no_js)))]
    mod platform {
        use std::rc::Rc;

        use jiff::civil::DateTime;
        use jiff::tz::Offset;
        use jiff::{Timestamp, Zoned};

        /// A zone's offset in seconds at an instant in milliseconds, as the browser reads it.
        type Offsets = Box<dyn Fn(i64) -> Option<i32>>;

        #[derive(Clone)]
        pub(super) enum Zone {
            /// The browser's own.
            Browser,
            /// A zone by its name, and its offsets.
            Named(Rc<(String, Offsets)>),
        }

        impl std::fmt::Debug for Zone {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::Browser => f.write_str("Browser"),
                    Self::Named(named) => f.debug_tuple("Named").field(&named.0).finish(),
                }
            }
        }

        impl Zone {
            pub(super) const fn here() -> Self {
                Self::Browser
            }

            pub(super) fn named(name: &str) -> Option<Self> {
                let offsets = crate::page::zone_offsets(name)?;
                Some(Self::Named(Rc::new((name.to_owned(), Box::new(offsets)))))
            }

            pub(super) fn at(&self, at: Timestamp) -> Zoned {
                super::read(at, |at| self.offset(at))
            }

            pub(super) fn name(&self) -> Option<String> {
                match self {
                    Self::Browser => crate::page::zone_name(),
                    Self::Named(named) => Some(named.0.clone()),
                }
            }

            pub(super) fn instant_of(&self, local: DateTime) -> Option<Timestamp> {
                super::resolve(local, |at| self.offset(at))
            }

            /// The zone's offset from UTC at `at`.
            fn offset(&self, at: Timestamp) -> Offset {
                let seconds = match self {
                    Self::Browser => Some(crate::page::offset_seconds(at.as_millisecond())),
                    Self::Named(named) => (named.1)(at.as_millisecond()),
                };
                seconds
                    .and_then(|seconds| Offset::from_seconds(seconds).ok())
                    .unwrap_or(Offset::UTC)
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use jiff::Timestamp;
        use jiff::civil::{Date, DateTime, Time};
        use jiff::tz::{Offset, TimeZone};

        use super::{read, resolve};

        /// The instant of `local` on the UTC clock.
        fn instant(local: DateTime) -> Timestamp {
            local
                .to_zoned(TimeZone::UTC)
                .expect("the test's wall times are inside the calendar")
                .timestamp()
        }

        /// The instant the clocks go forward an hour, 02:00 becoming 03:00.
        fn spring() -> Timestamp {
            instant(Date::new(2026, 3, 29).unwrap().at(1, 0, 0, 0))
        }

        /// The instant they go back, 03:00 becoming 02:00.
        fn autumn() -> Timestamp {
            instant(Date::new(2026, 10, 25).unwrap().at(1, 0, 0, 0))
        }

        /// A zone whose clocks keep +1 and go to +2 between [`spring`] and [`autumn`].
        fn offsets(at: Timestamp) -> Offset {
            if (spring()..autumn()).contains(&at) {
                jiff::tz::offset(2)
            } else {
                jiff::tz::offset(1)
            }
        }

        fn wall(at: Timestamp) -> DateTime {
            read(at, offsets).datetime()
        }

        fn seconds(at: Timestamp) -> i64 {
            at.as_second()
        }

        #[test]
        fn a_clock_reads_local_on_each_side_of_a_change_of_clocks() {
            let before = spring()
                .checked_sub(jiff::SignedDuration::from_secs(1))
                .unwrap();
            assert_eq!((wall(before).hour(), wall(before).minute()), (1, 59));
            assert_eq!(read(before, offsets).offset().seconds(), 3600);
            assert_eq!(
                (wall(spring()).hour(), wall(spring()).minute()),
                (3, 0),
                "02:00 is skipped"
            );
            assert_eq!(read(spring(), offsets).offset().seconds(), 7200);
            let before = autumn()
                .checked_sub(jiff::SignedDuration::from_secs(1))
                .unwrap();
            assert_eq!(read(before, offsets).offset().seconds(), 7200);
            assert_eq!((wall(before).hour(), wall(before).minute()), (2, 59));
            assert_eq!(wall(autumn()).hour(), 2, "02:00 comes twice");
            assert_eq!(read(autumn(), offsets).offset().seconds(), 3600);
        }

        /// A zone a person chose reads by its name, as the database the device or page has
        /// knows it, and a thread told it reads its clock there until told otherwise.
        #[test]
        fn a_zone_is_read_by_its_name_and_a_thread_told_it_reads_there() {
            let Some(vienna) = super::Zone::named("Europe/Vienna") else {
                // A system without a zone database knows no zone by its name.
                return;
            };
            assert!(
                super::zone_names()
                    .iter()
                    .any(|name| name == "Europe/Vienna")
            );
            assert!(super::Zone::named("../../etc/passwd").is_none());
            assert!(super::Zone::named("Nowhere/Atlantis").is_none());
            let winter = instant(Date::new(2026, 1, 15).unwrap().at(12, 0, 0, 0));
            assert_eq!(vienna.at(winter).offset().seconds(), 3600);
            std::thread::spawn(move || {
                super::install(Some(vienna));
                assert_eq!(super::Zone::here().name().as_deref(), Some("Europe/Vienna"));
                assert_eq!(super::at(winter).datetime().hour(), 13);
                super::install(None);
                assert!(super::told::installed().is_none());
            })
            .join()
            .unwrap();
        }

        #[test]
        fn a_wall_time_the_clocks_skipped_names_no_instant() {
            let skipped = Date::new(2026, 3, 29).unwrap().at(2, 30, 0, 0);
            assert_eq!(resolve(skipped, offsets), None);
        }

        #[test]
        fn a_wall_time_the_clocks_passed_twice_names_the_earlier_instant() {
            let twice = Date::new(2026, 10, 25).unwrap().at(2, 30, 0, 0);
            let at = resolve(twice, offsets).expect("02:30 stands twice");
            assert_eq!(wall(at), twice, "the earlier one still reads 02:30");
            assert_eq!(
                seconds(at),
                seconds(autumn()) - 1800,
                "at +2, before the change"
            );
        }

        #[test]
        fn a_wall_time_away_from_a_change_names_its_one_instant() {
            let noon = Date::new(2026, 7, 1).unwrap().at(12, 0, 0, 0);
            let at = resolve(noon, offsets).expect("noon of a July day stands once");
            assert_eq!(wall(at), noon);
            assert_eq!(at.as_second() % 60, 0);
            let midnight = Date::new(2026, 1, 1).unwrap().to_datetime(Time::midnight());
            assert_eq!(
                read(resolve(midnight, offsets).unwrap(), offsets).datetime(),
                midnight
            );
        }

        /// A page and a device read the wall clock through the same two calls: the instant they
        /// hand to `at`, and the local date and time it reads as.
        #[test]
        fn the_wall_clock_is_one_reading_as_an_instant_and_in_the_persons_zone() {
            tilo_test_env::in_own_process(
                module_path!(),
                "the_wall_clock_is_one_reading_as_an_instant_and_in_the_persons_zone",
                || {
                    // 2026-10-04 03:54:00 UTC.
                    let held = crate::UNIX_EPOCH + crate::time::Duration::from_secs(1_791_086_040);
                    let _held = crate::testing::hold_wall_everywhere(
                        crate::testing::OwnProcess::of_a_test_in_its_own_process(),
                        held,
                    );
                    let instant = super::now_instant();
                    assert_eq!(instant.as_second(), 1_791_086_040);
                    assert_eq!(
                        super::now().timestamp(),
                        instant,
                        "the local reading is that instant"
                    );
                    assert_eq!(
                        super::at(instant),
                        super::now(),
                        "and the instant reads back as the same local time"
                    );
                },
            );
        }
    }
}

/// The clock under one module path: the types under `std::time`'s names, for signatures that
/// need one, and every reading, made through [`now`] and [`wall`]. The UI toolkit and the process
/// services name the clock through this module and never `std::time`, so a process whose clock
/// must be virtualised (a deterministic replay, a test) has one boundary to provide.
pub mod time {
    pub use crate::imp::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};
    /// A clock held at one wall time; tilo-clock's own tests hold it per thread instead.
    #[cfg(not(test))]
    pub use crate::pin_wall;
    pub use crate::{
        from_std, is_synchronised, local_zone_name, now, to_std, try_unix_now, try_unix_now_ms,
        try_unix_now_ns, unix_now, unix_now_i64, unix_now_ms, unix_now_ms_i64, unix_now_ns, wall,
        wall_frozen,
    };

    /// A file-system timestamp, as `std::fs::Metadata::modified` reports it, as a [`SystemTime`].
    pub use crate::from_std as from_file_time;

    /// Scoped clock readings used by production-path input and deadline tests.
    #[cfg(any(test, feature = "test-support"))]
    pub use crate::testing;
}

#[cfg(any(not(target_family = "wasm"), tilo_no_js))]
mod imp {
    pub use std::time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

    #[inline]
    pub const fn to_std(time: SystemTime) -> std::time::SystemTime {
        time
    }

    #[inline]
    pub const fn from_std(time: std::time::SystemTime) -> SystemTime {
        time
    }
}

#[cfg(all(target_family = "wasm", not(tilo_no_js)))]
mod imp {
    pub use web_time::{Duration, Instant, SystemTime, SystemTimeError, UNIX_EPOCH};

    pub fn to_std(time: SystemTime) -> std::time::SystemTime {
        let converted = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => std::time::UNIX_EPOCH.checked_add(after),
            Err(before) => std::time::UNIX_EPOCH.checked_sub(before.duration()),
        };
        converted.unwrap_or(std::time::UNIX_EPOCH)
    }

    pub fn from_std(time: std::time::SystemTime) -> SystemTime {
        let converted = match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(after) => UNIX_EPOCH.checked_add(after),
            Err(before) => UNIX_EPOCH.checked_sub(before.duration()),
        };
        converted.unwrap_or(UNIX_EPOCH)
    }
}

// The page's lint pass bans std's clocks (tools/ci/wasm_clippy_conf.py), and this crate is the
// one place a build that is not the page reads them: in a page, `Instant` and `SystemTime` are
// `web_time`'s.
#[allow(
    clippy::disallowed_methods,
    reason = "off the page this crate is where std's monotonic clock is read"
)]
#[must_use]
#[inline]
pub fn now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(at) = testing::monotonic_override() {
        return at;
    }
    Instant::now()
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use super::time::{Instant, SystemTime};
    use std::cell::Cell;

    thread_local! {
        static MONOTONIC: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub(super) fn monotonic_override() -> Option<Instant> {
        MONOTONIC.with(Cell::get)
    }

    struct Restore(Option<Instant>);

    impl Drop for Restore {
        fn drop(&mut self) {
            MONOTONIC.with(|clock| clock.set(self.0));
        }
    }

    pub fn with_monotonic_time<T>(at: Instant, operation: impl FnOnce() -> T) -> T {
        let _restore = Restore(MONOTONIC.with(|clock| clock.replace(Some(at))));
        operation()
    }

    struct RestoreWall(Option<u64>);

    impl Drop for RestoreWall {
        fn drop(&mut self) {
            super::FROZEN_HERE_MS.with(|here| here.set(self.0));
        }
    }

    /// A test's right to pin its process's wall clock: only
    /// `tilo_test_env::in_own_process` gives one, and `policy/ui-clock-contract`
    /// refuses [`hold_wall_everywhere`] elsewhere.
    #[derive(Clone, Copy)]
    pub struct OwnProcess(());

    impl OwnProcess {
        #[doc(hidden)]
        #[must_use]
        pub const fn of_a_test_in_its_own_process() -> Self {
            Self(())
        }
    }

    /// A process-wide wall-clock pin, back to the real clock when it drops.
    pub struct WallHold {
        _process: OwnProcess,
        pinned: std::sync::Arc<super::AtomicU64>,
        replaced: Option<std::sync::Arc<super::AtomicU64>>,
    }

    impl Drop for WallHold {
        fn drop(&mut self) {
            let mut process = super::PROCESS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if process
                .as_ref()
                .is_some_and(|pin| std::sync::Arc::ptr_eq(pin, &self.pinned))
            {
                *process = self.replaced.take();
            }
        }
    }

    /// Pin the wall clock of `process` to `at` until the test ends; every
    /// thread reads `at`.
    pub fn hold_wall_everywhere(process: OwnProcess, at: SystemTime) -> WallHold {
        let pinned = std::sync::Arc::new(super::AtomicU64::new(super::wall_ms(at)));
        // A second pin takes over from the first.
        let replaced = super::PROCESS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .replace(pinned.clone());
        WallHold {
            _process: process,
            pinned,
            replaced,
        }
    }

    thread_local! {
        static FOLLOWED: std::cell::RefCell<Option<std::sync::Arc<std::sync::atomic::AtomicU64>>> =
            const { std::cell::RefCell::new(None) };
    }

    pub(super) fn followed_ms() -> u64 {
        FOLLOWED.with(|followed| {
            followed
                .borrow()
                .as_ref()
                .map_or(0, |wall| wall.load(std::sync::atomic::Ordering::Relaxed))
        })
    }

    /// A wall clock a test pins for the threads it owns; threads that follow
    /// none keep their own.
    #[derive(Clone, Debug, Default)]
    pub struct SharedWall(std::sync::Arc<std::sync::atomic::AtomicU64>);

    impl SharedWall {
        /// This thread reads the pinned time from now on, until
        /// [`Self::release`].
        pub fn follow(&self) {
            FOLLOWED.with(|followed| *followed.borrow_mut() = Some(self.0.clone()));
        }

        pub fn pin(&self, at: SystemTime) {
            self.0
                .store(super::wall_ms(at), std::sync::atomic::Ordering::Relaxed);
        }

        pub fn release(&self) {
            self.0.store(0, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Pin the wall clock of this thread alone to `at`: the other tests of the
    /// process and a runtime's workers keep their own clock.
    pub fn with_wall<T>(at: SystemTime, operation: impl FnOnce() -> T) -> T {
        let ms = super::wall_ms(at);
        let _restore = RestoreWall(super::FROZEN_HERE_MS.with(|here| here.replace(Some(ms))));
        operation()
    }
}

#[cfg(test)]
mod scoped_monotonic_tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_held_clock_is_the_holding_threads_alone_and_ends_with_its_scope;

        phase a_shared_wall_pins_the_threads_following_it_until_released;
        {
            const PINNED: u64 = 123_456;
            let wall = testing::SharedWall::default();
            let follower = {
                let wall = wall.clone();
                move || {
                    wall.follow();
                    unix_now()
                }
            };
            assert_ne!(std::thread::spawn(follower.clone()).join().unwrap(), PINNED);
            wall.pin(UNIX_EPOCH + Duration::from_secs(PINNED));
            assert_eq!(std::thread::spawn(follower.clone()).join().unwrap(), PINNED);
            assert_ne!(
                unix_now(),
                PINNED,
                "a thread following none keeps its clock"
            );
            wall.release();
            assert_ne!(std::thread::spawn(follower).join().unwrap(), PINNED);
        }

        phase scoped_time_is_thread_local_nested_and_restored_after_unwind;
        {
            let at = now().checked_add(Duration::from_secs(60)).unwrap();
            testing::with_monotonic_time(at, || {
                assert_eq!(now(), at);
                let later = at.checked_add(Duration::from_secs(1)).unwrap();
                testing::with_monotonic_time(later, || assert_eq!(now(), later));
                assert_eq!(now(), at);
                assert!(std::thread::spawn(now).join().unwrap() < at);
                let panicked = std::panic::catch_unwind(|| {
                    testing::with_monotonic_time(later, || panic!("unwind the clock scope"));
                });
                assert!(panicked.is_err());
                assert_eq!(now(), at);
            });
            assert!(now() < at);
        }

        phase scoped_wall_time_is_thread_local_nested_and_restored_after_unwind;
        {
            let at = UNIX_EPOCH + Duration::from_secs(4_000_000_000);
            testing::with_wall(at, || {
                assert_eq!(wall(), at);
                let later = at + Duration::from_secs(3);
                testing::with_wall(later, || assert_eq!(wall(), later));
                assert_eq!(wall(), at);
                assert!(std::thread::spawn(wall).join().unwrap() < at);
                let panicked = std::panic::catch_unwind(|| {
                    testing::with_wall(later, || panic!("unwind the clock scope"));
                });
                assert!(panicked.is_err());
                assert_eq!(wall(), at);
            });
            assert!(wall() < at);
        }

        phase a_clock_lent_at_the_epoch_is_a_clock_the_code_reads;
        {
            // `0` is the epoch, not the absence of a hold: a bare wasm32 has no
            // `std` clock, so falling back to the machine's would trap a module lent
            // this clock.
            let (ms, held, frozen) = with_wall_ms(0, || (unix_now_ms(), wall(), wall_frozen()));
            assert_eq!(ms, 0, "the lent time is the time the code reads");
            assert_eq!(held, UNIX_EPOCH);
            assert!(frozen, "a hold at the epoch is a hold");
            assert!(
                unix_now_ms() > 0,
                "the thread's own clock runs again after the hold"
            );
        }

        phase two_threads_holding_different_wall_times_at_once_each_read_their_own;
        {
            let first = UNIX_EPOCH + Duration::from_secs(4_000_000_000);
            let second = UNIX_EPOCH + Duration::from_secs(4_100_000_000);
            let together = std::sync::Arc::new(std::sync::Barrier::new(2));
            let hold = |at: SystemTime, other: SystemTime| {
                let together = std::sync::Arc::clone(&together);
                std::thread::spawn(move || {
                    testing::with_wall(at, || {
                        together.wait();
                        assert_eq!(wall(), at);
                        assert_ne!(wall(), other);
                        together.wait();
                    });
                    together.wait();
                    assert!(wall() < first, "the hold ended with its scope");
                })
            };
            let threads = [hold(first, second), hold(second, first)];
            for thread in threads {
                thread.join().unwrap();
            }
        }
    }
}

thread_local! {
    /// The wall time this thread holds; `None` while it reads its own clock.
    static FROZEN_HERE_MS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

fn frozen_ms() -> Option<u64> {
    match FROZEN_HERE_MS.with(std::cell::Cell::get) {
        Some(here) => Some(here),
        None => match followed_ms() {
            0 => match pinned_ms() {
                0 => None,
                pinned => Some(pinned),
            },
            followed => Some(followed),
        },
    }
}

/// Hold this thread's wall clock at `now_ms`; nested holds restore the one
/// before them. This is how a host lends a clock to code that has none.
pub fn with_wall_ms<T>(now_ms: u64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<u64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FROZEN_HERE_MS.with(|here| here.set(self.0));
        }
    }
    let _restore = Restore(FROZEN_HERE_MS.with(|here| here.replace(Some(now_ms))));
    f()
}

static PROCESS: std::sync::Mutex<Option<std::sync::Arc<AtomicU64>>> = std::sync::Mutex::new(None);

fn pinned_ms() -> u64 {
    PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map_or(0, |ms| ms.load(Ordering::Relaxed))
}

#[cfg(any(test, feature = "test-support"))]
fn followed_ms() -> u64 {
    testing::followed_ms()
}

#[cfg(not(any(test, feature = "test-support")))]
const fn followed_ms() -> u64 {
    0
}

#[allow(
    clippy::disallowed_methods,
    reason = "off the page this crate is where std's wall clock is read"
)]
#[must_use]
#[inline]
pub fn wall() -> SystemTime {
    match frozen_ms() {
        Some(ms) => UNIX_EPOCH
            .checked_add(Duration::from_millis(ms))
            .unwrap_or(UNIX_EPOCH),
        None => SystemTime::now(),
    }
}

/// Pin the wall clock of the whole process to `at` for a screenshot capture,
/// so every frame shows the same date. The monotonic clock keeps running.
#[cfg(not(test))]
pub fn pin_wall(at: SystemTime) {
    let pinned = std::sync::Arc::new(AtomicU64::new(wall_ms(at)));
    *PROCESS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(pinned);
}

/// `at` as milliseconds since the epoch, never `0` (which means the clock runs).
fn wall_ms(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(1, |since| saturating_u64(since.as_millis()).max(1))
}

/// Whether a clock is held here, so a wall read answers the hold.
#[must_use]
pub fn wall_frozen() -> bool {
    frozen_ms().is_some()
}

/// Whether the device's clock is synchronised, when the platform can say;
/// `None` when nothing here knows.
#[must_use]
pub fn is_synchronised() -> Option<bool> {
    #[cfg(all(target_os = "linux", not(target_family = "wasm")))]
    {
        let output = std::process::Command::new("timedatectl")
            .args(["show", "-p", "NTPSynchronized", "--value"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        match String::from_utf8_lossy(&output.stdout).trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        }
    }
    #[cfg(not(all(target_os = "linux", not(target_family = "wasm"))))]
    {
        None
    }
}

/// Whether `name` names a zone as the time-zone database does (`Pacific/Auckland`, `UTC`), and
/// so names no file outside the database either.
#[must_use]
pub fn is_zone_name(name: &str) -> bool {
    /// The longest name taken: the database's longest is about 30 bytes.
    const MAX_NAME: usize = 64;
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part.chars().all(|character| {
                    character.is_ascii_alphanumeric() || "_+-.".contains(character)
                })
        })
}

#[must_use]
pub fn local_zone_name() -> Option<String> {
    #[cfg(all(unix, not(target_family = "wasm")))]
    {
        if let Ok(tz) = std::env::var("TZ") {
            let tz = tz.trim_start_matches(':');
            if !tz.is_empty() && !tz.starts_with('/') {
                return Some(tz.to_owned());
            }
        }
        let link = std::fs::read_link("/etc/localtime").ok()?;
        let (_, zone) = link.to_str()?.split_once("zoneinfo/")?;
        (!zone.is_empty()).then(|| zone.to_owned())
    }
    // A page has no TZ or /etc/localtime: the browser names its zone (`Intl`), and an offset
    // that is right for the instant asked about rather than the one in force now.
    #[cfg(all(target_family = "wasm", not(tilo_no_js)))]
    {
        page::zone_name()
    }
    #[cfg(not(any(
        all(unix, not(target_family = "wasm")),
        all(target_family = "wasm", not(tilo_no_js))
    )))]
    {
        None
    }
}

/// What the browser answers about the person's zone: the only zone a page has.
#[cfg(all(target_family = "wasm", not(tilo_no_js)))]
mod page {
    use wasm_bindgen::JsValue;

    thread_local! {
        /// The name the browser gave first: asking builds a formatter, and the zone's name
        /// changes only when the person changes it in the system's settings, as a device's does.
        static ZONE_NAME: std::cell::OnceCell<Option<String>> = const { std::cell::OnceCell::new() };
    }

    /// The browser's IANA name for the person's zone (`Europe/Vienna`).
    pub(crate) fn zone_name() -> Option<String> {
        ZONE_NAME.with(|name| name.get_or_init(read_zone_name).clone())
    }

    fn read_zone_name() -> Option<String> {
        let format = js_sys::Intl::DateTimeFormat::default();
        let resolved = js_sys::Intl::DateTimeFormat::resolved_options(&format);
        js_sys::Reflect::get(&resolved, &JsValue::from_str("timeZone"))
            .ok()?
            .as_string()
    }

    /// The offsets of the zone `name` (`Europe/Vienna`) as the browser reads them: a function
    /// from an instant in milliseconds to the offset then, in seconds east of UTC. `None` where
    /// the browser knows no such zone.
    #[cfg(feature = "local")]
    pub(crate) fn zone_offsets(name: &str) -> Option<impl Fn(i64) -> Option<i32> + use<>> {
        use wasm_bindgen::JsCast as _;
        let intl = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("Intl")).ok()?;
        let make = js_sys::Reflect::get(&intl, &JsValue::from_str("DateTimeFormat"))
            .ok()?
            .dyn_into::<js_sys::Function>()
            .ok()?;
        let options = js_sys::Object::new();
        js_sys::Reflect::set(
            &options,
            &JsValue::from_str("timeZone"),
            &JsValue::from_str(name),
        )
        .ok()?;
        // "GMT+05:45", "GMT-03:00", or "GMT" at UTC itself: the offset, whatever the language.
        js_sys::Reflect::set(
            &options,
            &JsValue::from_str("timeZoneName"),
            &JsValue::from_str("longOffset"),
        )
        .ok()?;
        let arguments = js_sys::Array::of2(&js_sys::Array::of1(&"en-US".into()), &options);
        // A zone the browser does not know is a RangeError, which the constructor throws.
        let format: js_sys::Intl::DateTimeFormat = js_sys::Reflect::construct(&make, &arguments)
            .ok()?
            .unchecked_into();
        Some(move |unix_ms: i64| {
            let date = js_sys::Date::new(&millis(unix_ms));
            let parts = format.format_to_parts(&date);
            parts.iter().find_map(|part| {
                let kind = js_sys::Reflect::get(&part, &JsValue::from_str("type")).ok()?;
                if kind.as_string().as_deref() != Some("timeZoneName") {
                    return None;
                }
                let value = js_sys::Reflect::get(&part, &JsValue::from_str("value")).ok()?;
                offset_named(&value.as_string()?)
            })
        })
    }

    /// The browser's names for the zones it knows.
    #[cfg(feature = "local")]
    pub(crate) fn zone_names() -> Vec<String> {
        let names = (|| {
            let intl = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("Intl")).ok()?;
            let list: js_sys::Function = wasm_bindgen::JsCast::dyn_into(
                js_sys::Reflect::get(&intl, &JsValue::from_str("supportedValuesOf")).ok()?,
            )
            .ok()?;
            let names = list.call1(&intl, &JsValue::from_str("timeZone")).ok()?;
            Some(
                js_sys::Array::from(&names)
                    .iter()
                    .filter_map(|name| name.as_string())
                    .collect::<Vec<_>>(),
            )
        })();
        names.unwrap_or_default()
    }

    /// Seconds east of UTC in a long offset the browser wrote (`GMT+05:45`, `GMT-03:00`, `GMT`).
    #[cfg(feature = "local")]
    fn offset_named(written: &str) -> Option<i32> {
        let offset = written.strip_prefix("GMT")?;
        if offset.is_empty() {
            return Some(0);
        }
        let mut chars = offset.chars();
        let sign = match chars.next()? {
            '+' => 1,
            '-' | '\u{2212}' => -1,
            _ => return None,
        };
        let offset = chars.as_str();
        let mut parts = offset.split(':');
        let hours: i32 = parts.next()?.parse().ok()?;
        let minutes: i32 = parts
            .next()
            .map_or(Some(0), |minutes| minutes.parse().ok())?;
        let seconds: i32 = parts
            .next()
            .map_or(Some(0), |seconds| seconds.parse().ok())?;
        hours
            .checked_mul(3600)?
            .checked_add(minutes.checked_mul(60)?)?
            .checked_add(seconds)?
            .checked_mul(sign)
    }

    /// The zone's offset from UTC at `unix_ms`, in seconds east of it.
    #[cfg(feature = "local")]
    pub(crate) fn offset_seconds(unix_ms: i64) -> i32 {
        let date = js_sys::Date::new(&millis(unix_ms));
        // `getTimezoneOffset` counts minutes west of UTC.
        let west = minutes(js_sys::Date::get_timezone_offset(&date));
        west.saturating_mul(60).saturating_neg()
    }

    #[cfg(feature = "local")]
    #[allow(
        clippy::cast_precision_loss,
        reason = "the browser's clock takes milliseconds as a double, which holds them exactly"
    )]
    fn millis(unix_ms: i64) -> JsValue {
        JsValue::from_f64(unix_ms as f64)
    }

    #[cfg(feature = "local")]
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the browser's offset is a whole number of minutes, within an hour's twelve"
    )]
    fn minutes(west_of_utc: f64) -> i32 {
        i32::try_from(west_of_utc as i64).unwrap_or(0)
    }
}

#[must_use]
#[inline]
pub fn to_std(time: SystemTime) -> std::time::SystemTime {
    imp::to_std(time)
}

#[must_use]
#[inline]
pub fn from_std(time: std::time::SystemTime) -> SystemTime {
    imp::from_std(time)
}

fn since_epoch() -> Option<Duration> {
    wall().duration_since(UNIX_EPOCH).ok()
}

fn saturating_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

pub fn try_unix_now() -> Option<u64> {
    since_epoch().map(|duration| duration.as_secs())
}

pub fn try_unix_now_ms() -> Option<u64> {
    since_epoch().map(|duration| saturating_u64(duration.as_millis()))
}

pub fn try_unix_now_ns() -> Option<u64> {
    since_epoch().map(|duration| saturating_u64(duration.as_nanos()))
}

pub fn unix_now() -> u64 {
    try_unix_now().unwrap_or(0)
}

pub fn unix_now_ms() -> u64 {
    try_unix_now_ms().unwrap_or(0)
}

pub fn unix_now_ns() -> u64 {
    try_unix_now_ns().unwrap_or(0)
}

pub fn unix_now_i64() -> i64 {
    i64::try_from(unix_now()).unwrap_or(i64::MAX)
}

pub fn unix_now_ms_i64() -> i64 {
    i64::try_from(unix_now_ms()).unwrap_or(i64::MAX)
}

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        unix_now_ms()
    }
}

/// Milliseconds since this process first read a monotonic clock; never moves
/// backwards.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonotonicClock;

impl Clock for MonotonicClock {
    #[allow(
        clippy::disallowed_methods,
        reason = "off the page this crate is where std's monotonic clock is read"
    )]
    fn now_ms(&self) -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        saturating_u64(START.get_or_init(now).elapsed().as_millis())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedClock(pub u64);

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
}

#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    pub const fn new(now_ms: u64) -> Self {
        Self(AtomicU64::new(now_ms))
    }

    pub fn set(&self, now_ms: u64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, delta_ms: u64) {
        let _ = self
            .0
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                Some(now.saturating_add(delta_ms))
            });
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_threads_own_wall_clock_shadows_the_process_pin_only_on_that_thread() {
        const OTHER: u64 = 4_000_000_000;
        let at = UNIX_EPOCH + Duration::from_secs(OTHER);
        tilo_test_env::in_own_process(
            module_path!(),
            "a_threads_own_wall_clock_shadows_the_process_pin_only_on_that_thread",
            || {
                let process = testing::OwnProcess::of_a_test_in_its_own_process();
                let _held = testing::hold_wall_everywhere(process, at);
                assert_eq!(unix_now(), OTHER);
                assert!(wall_frozen());
                testing::with_wall(at + Duration::from_secs(1), || {
                    assert_eq!(wall(), at + Duration::from_secs(1), "the thread's own");
                    assert_eq!(
                        std::thread::spawn(wall).join().unwrap(),
                        at,
                        "another thread keeps the process's"
                    );
                });
                assert_eq!(wall(), at, "the process's, after the thread's scope");
            },
        );
    }

    /// The zone a device is set to is resolved by name, not read as whatever
    /// `/etc/localtime` points at: the person's day is their region's, so a
    /// machine set to UTC does not move an evening in Los Angeles to tomorrow.
    #[test]
    #[cfg(feature = "local")]
    fn a_named_zone_is_read_as_itself_whatever_the_machines_own() {
        tilo_test_env::in_own_process(
            module_path!(),
            "a_named_zone_is_read_as_itself_whatever_the_machines_own",
            || {
                let mut zone = tilo_test_env::ScopedEnv::new();
                zone.set("TZ", "America/Los_Angeles");
                // 11 November, nine in the evening in Los Angeles: the 12th in UTC.
                let _held = testing::hold_wall_everywhere(
                    testing::OwnProcess::of_a_test_in_its_own_process(),
                    UNIX_EPOCH + Duration::from_secs(1_794_459_600),
                );
                let now = local::now();
                assert_eq!(
                    now.time_zone().iana_name(),
                    Some("America/Los_Angeles"),
                    "the named zone is the one the clock stands in"
                );
                assert_eq!(
                    local::today().to_string(),
                    "2026-11-11",
                    "their day, not UTC's"
                );
                assert_eq!(
                    local::instant_of(jiff::civil::date(2026, 11, 11).at(21, 0, 0, 0)),
                    jiff::Timestamp::from_second(1_794_459_600).ok(),
                    "and the evening they read is the instant it names"
                );
            },
        );
    }

    tilo_test_env::scenario! {
        fn clocks_read_units_that_agree_and_a_hold_ends_with_its_scope;

        phase a_frozen_wall_clock_holds_while_the_monotonic_clock_runs;
        {
            let at = UNIX_EPOCH + Duration::from_secs(1_782_816_120);
            let before = now();
            testing::with_wall(at, || {
                assert!(wall_frozen());
                assert_eq!(wall(), at);
                assert_eq!(unix_now(), 1_782_816_120);
                assert_eq!(wall(), at, "it stays put");
                assert!(now() >= before, "the monotonic clock is not frozen");
            });
            assert!(!wall_frozen(), "the scope ended");
        }

        phase clocks_agree_on_units_move_only_as_told_and_never_backwards;
        {
            assert!(!wall_frozen(), "no other test's pinned time is read here");
            {
                let secs = unix_now();
                let ms = unix_now_ms();
                let ns = unix_now_ns();
                assert!(secs > 1_600_000_000);
                assert!(ms / 1_000 >= secs);
                assert!(ns / 1_000_000 >= ms);
                assert_eq!(unix_now_i64(), i64::try_from(unix_now()).unwrap());
            }
            {
                assert_eq!(FixedClock(7).now_ms(), 7);
                let clock = ManualClock::new(10);
                assert_eq!(clock.now_ms(), 10);
                clock.advance(5);
                assert_eq!(clock.now_ms(), 15);
                clock.set(3);
                assert_eq!(clock.now_ms(), 3);
                clock.set(u64::MAX - 1);
                clock.advance(10);
                assert_eq!(clock.now_ms(), u64::MAX);
            }
            {
                let first = MonotonicClock.now_ms();
                assert!(MonotonicClock.now_ms() >= first);
            }
        }

        phase now_is_monotonic;
        {
            let earlier = now();
            assert!(now() >= earlier);
        }

        phase wall_is_after_the_epoch;
        {
            assert!(wall().duration_since(UNIX_EPOCH).is_ok());
        }

        phase std_conversions_round_trip_on_both_sides_of_the_epoch;
        {
            let offset = Duration::from_millis(1_700_000_000_123);
            for time in [UNIX_EPOCH + offset, UNIX_EPOCH - offset, UNIX_EPOCH] {
                assert_eq!(from_std(to_std(time)), time);
            }
            assert_eq!(to_std(UNIX_EPOCH + offset), std::time::UNIX_EPOCH + offset);
        }
    }
}
