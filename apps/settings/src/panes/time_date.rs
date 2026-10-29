//! Time & Date pane — the device's clock and its time zone.
//!
//! The clock format and the date format are the person's Region & Formats
//! (`locale.time_format`, `locale.date_format`): this page shows the time and
//! the date in them and has no clock-format control of its own, so "Follow
//! language" stays one choice in one place. Automatic time and the time zone
//! are this device's, applied through timedated.
//!
//! ## Live vs. sample
//!
//! - **Current time / date** are read from the device's clock ([`Clock`]),
//!   formatted per Region & Formats; a capture shows a fixed time.
//! - **Time zone** is read from timedated, falling back to `/etc/timezone`
//!   (or the `/etc/localtime` symlink target) only when the service does not
//!   report it. Choices come from `timedatectl list-timezones`; map coordinates
//!   are joined from the installed tzdata tables, never an embedded list.
//! - The zone is chosen in **one picker**: a searchable list whose first
//!   choice is "Set automatically" ([`ClockPrefs::automatic_zone`], which the
//!   session's zone follower acts on), with the globe under it as the
//!   picker's visual. Choosing on either is the same choice, and each shows
//!   it.
//! - **Set time automatically** and the time zone are [`TimeDateEffect`]
//!   jobs through `timedatectl`; the setter needs privilege, so a denial is
//!   the effect's failure, shown on the pane.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use ui::LocalState;
use ui::framework::{Actions, EffectAnswer, EffectCx, Effected, ViewCx};
use ui::tilo_resource;
use ui::widget::{Dropdown, Element};

use crate::area::AreaContext;
use crate::device_read::Reading;
use crate::effects::effect;
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use provider_settings_ui::widgets::{Item, block, error_row, note, rows, section, value_row};
use tilo_locale::Locale;
use ui::i18n::ArgValue;
use ui::lock::MutexRecover as _;
pub use zone_globe::TimeZonePoint;
use zone_globe::{SharedGlobe, TimeZoneGlobe};

crate::effects::effect_family!(TimeDateEffect => answer);

crate::area::pane_area! {
    TimeDateArea;
    fn warm(&self) {
        TimeDateArea::warm(self);
    }
    fn poll(&self) {
        TimeDateArea::poll(self);
    }
    fn first_read(&self) -> bool {
        TimeDateArea::first_read(self)
    }
    fn next_wake(&self) -> Option<ui::time::Instant> {
        TimeDateArea::next_wake(self)
    }
}

/// The window's [`TimeDateArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&TimeDateArea> {
    app.areas().find()
}

/// What the time settings found by search are set to, as the page last
/// read the clock: nothing until it has.
pub(crate) mod values {
    use provider_settings_ui::bound::SettingRows as _;
    use ui::LocalState;
    use ui::framework::{Actions, ViewCx};
    use ui::tilo_resource::{Model, Resource};

    use super::{ClockPrefs, TimeDateEffect, area, zone_name};
    use crate::effects::effect;
    use crate::i18n::t;
    use crate::resource::Settings;
    use crate::setting::{Follows, Read};
    use crate::settings_app::SettingsApp;
    use provider_settings_ui::widgets::Item;

    /// Automatic time, a switch once the clock was read and said whether
    /// it is on.
    pub(crate) fn automatic(app: &SettingsApp, _: &Follows) -> Read {
        area(app)
            .and_then(|area| area.reading.get())
            .filter(|reading| reading.auto_error.is_none())
            .map_or(Read::Value(None), |reading| {
                Read::Switch(reading.automatic.unwrap_or(false))
            })
    }

    /// The automatic-time switch, where search finds it.
    pub(crate) fn automatic_switch(cx: &ViewCx<Settings>, on: bool) -> Item {
        automatic_row(cx, &cx.actions(), on)
    }

    /// The automatic-time switch in a part over the record `R`, invoking
    /// the effect `actions` publishes.
    pub(super) fn automatic_row<R: Resource<State = R> + Model>(
        cx: &ViewCx<R>,
        actions: &Actions<Settings>,
        on: bool,
    ) -> Item {
        cx.setting(t("time-auto")).id("time.auto").toggle_effect(
            &LocalState::new(on),
            actions,
            effect(TimeDateEffect::SET_AUTOMATIC_TIME),
        )
    }

    pub(crate) fn zone(app: &SettingsApp, follows: &Follows) -> Option<String> {
        let reading = area(app)?.reading.get()?;
        let zone = reading.status.ok()?.timezone?;
        let city = zone_name(&zone)?;
        Some(if follows.get::<ClockPrefs>(app.storage()).automatic_zone {
            crate::i18n::t_args(
                "time-zone-automatic-now",
                &[("zone", ui::i18n::ArgValue::from(city.as_str()))],
            )
        } else {
            city
        })
    }
}

/// A map-friendly model over the exact IANA ids accepted by timedated.
/// Coordinates come from the same installed tzdata package; this deliberately
/// has no embedded city/coordinate database. Aliases absent from the tzdata
/// coordinate table remain available in the searchable selector.
pub fn timezone_map(zones: &[String]) -> Vec<TimeZonePoint> {
    const TABLES: &[&str] = &[
        "/usr/share/zoneinfo/zone1970.tab",
        "/usr/share/zoneinfo/zone.tab",
    ];
    // `zone1970.tab` lists one zone per region of identical clocks since
    // 1970; zones merged into another (Europe/Amsterdam into Brussels) are
    // only in `zone.tab`. Reading both, the newer first, puts every zone the
    // device accepts on the map, including the one it may be set to.
    let tables: String = TABLES
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .collect::<Vec<_>>()
        .join("\n");
    timezone_map_from_tab(zones, &tables)
}

fn timezone_map_from_tab(zones: &[String], table: &str) -> Vec<TimeZonePoint> {
    let accepted: HashSet<&str> = zones.iter().map(String::as_str).collect();
    let mut seen = HashSet::new();
    let mut points: Vec<_> = table
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let _countries = columns.next()?;
            let coordinate = columns.next()?;
            let id = columns.next()?;
            if !accepted.contains(id) || !seen.insert(id) {
                return None;
            }
            let (latitude_e6, longitude_e6) = parse_iso6709_coordinate(coordinate)?;
            let (region, city) = id.split_once('/')?;
            Some(TimeZonePoint {
                id: id.to_string(),
                region: region.replace('_', " "),
                city: city.replace('_', " ").replace('/', ", "),
                latitude_e6,
                longitude_e6,
            })
        })
        .collect();
    // Stable geographic order makes keyboard movement and screenshots
    // independent of the table's packaging order.
    points.sort_by_key(|point| (point.longitude_e6, -point.latitude_e6, point.id.clone()));
    points
}

/// Parse the compact ISO 6709 latitude+longitude form used by tzdata:
/// `+DDMM+DDDMM` or `+DDMMSS+DDDMMSS` (and negative equivalents).
fn parse_iso6709_coordinate(value: &str) -> Option<(i32, i32)> {
    let split = value
        .char_indices()
        .skip(1)
        .find_map(|(i, c)| matches!(c, '+' | '-').then_some(i))?;
    let latitude = parse_coordinate_part(value.get(..split)?, true)?;
    let longitude = parse_coordinate_part(value.get(split..)?, false)?;
    Some((
        (latitude * 1_000_000.0).round() as i32,
        (longitude * 1_000_000.0).round() as i32,
    ))
}

fn parse_coordinate_part(value: &str, latitude: bool) -> Option<f64> {
    let sign = match value.as_bytes().first().copied()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = value.get(1..)?;
    let degree_digits = if latitude { 2 } else { 3 };
    if digits.len() != degree_digits + 2 && digits.len() != degree_digits + 4 {
        return None;
    }
    let degrees: f64 = digits.get(..degree_digits)?.parse().ok()?;
    let minutes: f64 = digits.get(degree_digits..degree_digits + 2)?.parse().ok()?;
    let seconds: f64 = if digits.len() == degree_digits + 4 {
        digits.get(degree_digits + 2..)?.parse().ok()?
    } else {
        0.0
    };
    let limit = if latitude { 90.0 } else { 180.0 };
    if degrees > limit || minutes >= 60.0 || seconds >= 60.0 {
        return None;
    }
    let coordinate = degrees + minutes / 60.0 + seconds / 3600.0;
    (coordinate <= limit).then_some(sign * coordinate)
}

/// The zone a capture shows.
const SAMPLE_TIMEZONE: &str = "Europe/Amsterdam";

/// Read the configured time zone from `/etc/timezone` or the
/// `/etc/localtime` link; `None` when neither names one.
fn timezone() -> Option<String> {
    for p in ["/etc/timezone"] {
        if let Ok(s) = fs::read_to_string(Path::new(p)) {
            let t = s.trim();
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
    }
    // `/etc/localtime` is usually a symlink into zoneinfo; derive the zone
    // from its target when `/etc/timezone` is absent (common on Fedora).
    if let Ok(target) = fs::read_link("/etc/localtime") {
        let target = target.to_string_lossy();
        if let Some(idx) = target.find("zoneinfo/")
            && let Some(zone) = target.get(idx + "zoneinfo/".len()..)
        {
            return Some(zone.to_string());
        }
    }
    None
}

/// This device's clock, as the pane and the effects reach it: timedated in a
/// window someone uses, a fixed stand-in in a screenshot.
pub trait Clock: Send + Sync {
    /// What timedated reports: the zone, automatic time, synchronisation.
    ///
    /// # Errors
    ///
    /// Why timedated could not be asked.
    fn status(&self) -> Result<os_actions::TimeSyncStatus, String>;
    /// Whether automatic time is on, asked on its own when the status did
    /// not say.
    ///
    /// # Errors
    ///
    /// Why it could not be asked.
    fn ntp(&self) -> Result<bool, String>;
    /// The zones timedated accepts.
    ///
    /// # Errors
    ///
    /// Why the zone list could not be read.
    fn zones(&self) -> Result<Vec<String>, String>;
    /// Where `zones` lie on the map: their coordinates in the installed
    /// tzdata.
    fn points(&self, zones: &[String]) -> Vec<TimeZonePoint> {
        timezone_map(zones)
    }
    /// The time and the date now, in `formats` (the person's Region &
    /// Formats).
    fn now(&self, formats: &ui::format::Formats) -> (String, String);
    /// Now, in seconds since the Unix epoch: the instant every zone's
    /// time on the globe is told at.
    fn unix_now(&self) -> i64 {
        i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX)
    }
    /// # Errors
    ///
    /// The underlying `timedatectl set-ntp` failure text when automatic time
    /// could not be changed (e.g. denied by policy).
    fn set_ntp(&self, enabled: bool) -> Result<(), String>;
    /// # Errors
    ///
    /// The underlying `timedatectl set-timezone` failure text.
    fn set_timezone(&self, timezone: &str) -> Result<(), String>;
    /// Follow timedated: `changed` is called, on another thread, each time
    /// it reports a change (the zone, automatic time), for as long as the
    /// answer is held. `None` when nothing can be followed.
    fn follow(&self, _changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        None
    }
}

/// The device's clock through timedated.
pub struct SystemClock;

impl Clock for SystemClock {
    fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
        match os_actions::bus_follow::follow_system_services(
            &["org.freedesktop.timedate1"],
            changed,
        ) {
            Ok(follow) => Some(Box::new(follow)),
            Err(error) => {
                tracing::warn!(target: "tilo::settings", %error, "the clock's changes are not followed");
                None
            }
        }
    }

    fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
        crate::device_read::off_ui_thread();
        let mut status = os_actions::get_time_sync_status()?;
        if status.timezone.is_none() {
            status.timezone = timezone();
        }
        Ok(status)
    }

    fn ntp(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        os_actions::get_ntp()
    }

    fn zones(&self) -> Result<Vec<String>, String> {
        crate::device_read::off_ui_thread();
        os_actions::list_timezones()
    }

    fn now(&self, formats: &ui::format::Formats) -> (String, String) {
        // The toolkit's clock, which every clock display follows.
        let moment = ui::clock::local_now();
        (formats.time(&moment), formats.date(moment.date_naive()))
    }

    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::set_ntp(enabled)
    }

    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        os_actions::set_timezone(timezone)
    }
}

/// The clock a capture shows: a fixed time in a fixed zone, which the
/// effects change in memory only.
pub struct SampleClock {
    status: Mutex<os_actions::TimeSyncStatus>,
}

impl Default for SampleClock {
    fn default() -> Self {
        Self {
            status: Mutex::new(os_actions::TimeSyncStatus {
                timezone: Some(SAMPLE_TIMEZONE.to_owned()),
                ntp_enabled: Some(true),
                synchronized: Some(true),
                source: Some("time.example.org".to_owned()),
            }),
        }
    }
}

impl Clock for SampleClock {
    fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
        crate::device_read::off_ui_thread();
        Ok(self.status.lock_recover().clone())
    }

    fn ntp(&self) -> Result<bool, String> {
        crate::device_read::off_ui_thread();
        Ok(self.status.lock_recover().ntp_enabled.unwrap_or(true))
    }

    fn zones(&self) -> Result<Vec<String>, String> {
        crate::device_read::off_ui_thread();
        // The zones the installed tzdata maps, so a capture's globe shows
        // the world's cities; four of them where none is installed.
        let mut zones: Vec<String> = [
            "/usr/share/zoneinfo/zone1970.tab",
            "/usr/share/zoneinfo/zone.tab",
        ]
        .iter()
        .filter_map(|path| fs::read_to_string(path).ok())
        .flat_map(|table| {
            table
                .lines()
                .filter(|line| !line.starts_with('#'))
                .filter_map(|line| line.split('\t').nth(2).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect();
        zones.extend(
            [
                "America/New_York",
                "Asia/Tokyo",
                SAMPLE_TIMEZONE,
                "Europe/Berlin",
            ]
            .map(str::to_owned),
        );
        zones.sort();
        zones.dedup();
        Ok(zones)
    }

    fn now(&self, formats: &ui::format::Formats) -> (String, String) {
        sample_now(*formats)
    }

    fn unix_now(&self) -> i64 {
        // 2023-03-15 13:05 UTC: 14:05 in Amsterdam, as `now` says.
        1_678_885_500
    }

    fn set_ntp(&self, enabled: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        self.status.lock_recover().ntp_enabled = Some(enabled);
        Ok(())
    }

    fn set_timezone(&self, timezone: &str) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        self.status.lock_recover().timezone = Some(timezone.to_owned());
        Ok(())
    }
}

/// 2023-03-15 14:05, the capture's time, in `formats`: the formats' own
/// worked examples are that moment.
fn sample_now(formats: ui::format::Formats) -> (String, String) {
    (formats.region.time.example(), formats.region.date.example())
}

/// The time-zone picker's first choice: the zone follows where the device
/// is.
pub const AUTOMATIC_ZONE: &str = "automatic";

pub use crate::prefs::ClockPrefs;

/// Automatic time and the time zone.
#[derive(Clone, Debug, PartialEq, Eq, tilo_resource::Effect)]
#[effect(operation = "settings.effect")]
pub enum TimeDateEffect {
    /// Set this device's time automatically from the network, or stop.
    #[effect(job, idempotent)]
    SetAutomaticTime { on: bool },
    /// Set this device's time zone, or have it follow where the device is.
    #[effect(job, idempotent)]
    SetTimeZone {
        #[param(
            "An IANA time zone, such as `Europe/Amsterdam`, or `automatic` to follow this device's location"
        )]
        zone: String,
    },
}

/// What the clock reports: timedated's status, the zones it accepts and
/// where they lie on the map. `timedatectl` goes through the system bus,
/// which can take up to a D-Bus timeout to answer, so this is read off the
/// UI thread ([`Reading`]).
#[derive(Clone)]
struct ClockReading {
    /// What timedated reports, or why it could not be asked.
    status: Result<os_actions::TimeSyncStatus, String>,
    /// Whether automatic time is on, as the status or the separate ask
    /// said.
    automatic: Option<bool>,
    /// Why the automatic-time switch's position is unknown.
    auto_error: Option<String>,
    /// The zones to choose from, the configured one among them, or why the
    /// list could not be read.
    zones: Result<Vec<String>, String>,
    points: Vec<TimeZonePoint>,
}

impl ClockReading {
    fn read(clock: &dyn Clock) -> Self {
        let status = clock.status();
        // Automatic time is asked on its own when timedated's status did
        // not say, whether or not the status could be read.
        let mut automatic = status.as_ref().ok().and_then(|status| status.ntp_enabled);
        let mut auto_error = None;
        if automatic.is_none() {
            match clock.ntp() {
                Ok(enabled) => automatic = Some(enabled),
                Err(error) => auto_error = Some(error),
            }
        }
        let zones = clock.zones().map(|mut zones| {
            if let Some(zone) = status
                .as_ref()
                .ok()
                .and_then(|status| status.timezone.as_ref())
                && !zones.contains(zone)
            {
                zones.push(zone.clone());
                zones.sort();
            }
            zones
        });
        let points = zones
            .as_deref()
            .map(|zones| clock.points(zones))
            .unwrap_or_default();
        Self {
            status,
            automatic,
            auto_error,
            zones,
            points,
        }
    }
}

/// What the Time & Date area keeps for the window: the device's clock, what
/// it last reported, and where its last change stands.
pub struct TimeDateArea {
    clock: Arc<dyn Clock>,
    /// Whether the clock moves on: a capture's stands still.
    live: bool,
    /// What the clock last reported, read again off the UI thread whenever
    /// the page opens and after every change.
    reading: Reading<ClockReading>,
    /// Why the last change failed, for the page to show.
    notice: LocalState<Option<String>>,
    /// What tells the area the clock changed while a page shows it.
    following: std::cell::RefCell<std::rc::Weak<crate::watch::Counted>>,
    /// The change count the clock was last read for.
    changes_seen: std::cell::Cell<Option<u64>>,
    /// Which way the globe faces, kept across the page's rebuilds.
    globe: SharedGlobe,
    /// Moves whenever what the page shows of the area does: a new reading,
    /// a notice, the minute.
    shown: LocalState<u64>,
    /// What [`Self::shown`] last moved for: the reading's revision, the
    /// notice and the minute.
    seen: std::cell::RefCell<Option<(u64, Option<String>, i64)>>,
}

impl TimeDateArea {
    /// Follow the clock while a page shows it; panes share one follow.
    /// Whether it starts now: nothing was followed before.
    fn follow(&self) -> (Rc<crate::watch::Counted>, bool) {
        if let Some(held) = self.following.borrow().upgrade() {
            return (held, false);
        }
        let clock = Arc::clone(&self.clock);
        let started = crate::watch::shared(&mut self.following.borrow_mut(), move || {
            crate::watch::Counted::start(|changed| clock.follow(changed))
        });
        (started, true)
    }

    /// Whether a page shows the clock.
    fn shown_page(&self) -> bool {
        self.following.borrow().upgrade().is_some()
    }

    /// Read the clock in the background unless it has been, so the page
    /// opens on what it last reported.
    pub(crate) fn warm(&self) {
        if self.reading.get().is_none() {
            self.reading.refresh();
        }
    }

    /// The area over this device's clock, or a stand-in for a capture.
    /// Nothing is read until the page opens.
    pub fn new(cx: &AreaContext) -> Self {
        let clock: Arc<dyn Clock> = if cx.devices {
            Arc::new(SystemClock)
        } else {
            Arc::new(SampleClock::default())
        };
        Self {
            live: cx.devices,
            ..Self::over(clock)
        }
    }

    fn over(clock: Arc<dyn Clock>) -> Self {
        let read = Arc::clone(&clock);
        Self {
            clock,
            live: false,
            reading: Reading::new(move || ClockReading::read(read.as_ref())),
            notice: LocalState::new(None),
            following: std::cell::RefCell::default(),
            changes_seen: std::cell::Cell::new(None),
            globe: SharedGlobe::default(),
            shown: LocalState::new(0),
            seen: std::cell::RefCell::new(None),
        }
    }

    /// The minute the clock reads now.
    fn minute_now(&self) -> i64 {
        self.clock.unix_now().div_euclid(60)
    }

    /// Take in what moved since the last frame: timedated reported a change
    /// while a page follows it (the clock is read again), and what the page
    /// shows: a reading landed, a notice, a new minute.
    pub(crate) fn poll(&self) {
        let Some(following) = self.following.borrow().upgrade() else {
            return;
        };
        let changes = following.changes();
        if self.changes_seen.replace(changes) != changes {
            self.reading.refresh();
        }
        // A capture's clock stands still.
        let minute = if self.live { self.minute_now() } else { 0 };
        let now = (self.reading.revision(), self.notice.get(), minute);
        let mut seen = self.seen.borrow_mut();
        if seen.as_ref() != Some(&now) {
            *seen = Some(now);
            self.shown.set(self.shown.get().wrapping_add(1));
        }
    }

    /// Whether a page shows the clock's first reading as on its way: none
    /// has landed, or one landed that the page has not taken yet
    /// ([`Self::poll`]).
    pub(crate) fn first_read(&self) -> bool {
        self.shown_page()
            && (self.reading.get().is_none()
                || self.seen.borrow().as_ref().map(|seen| seen.0) != Some(self.reading.revision()))
    }

    /// When the minute shown turns, while a page shows a clock that moves.
    fn next_wake(&self) -> Option<ui::time::Instant> {
        if !self.live || !self.shown_page() {
            return None;
        }
        let into_minute = self.clock.unix_now().rem_euclid(60);
        Some(
            ui::time::Instant::now()
                + std::time::Duration::from_secs(u64::try_from(60 - into_minute).unwrap_or(60)),
        )
    }
}

/// Answer `effect`. Device changes run off the UI thread: timedated may ask
/// for authorisation.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: TimeDateEffect,
    cx: &mut EffectCx<Settings>,
) -> EffectAnswer {
    let clock = Arc::clone(
        &crate::panes::time_date::area(app)
            .ok_or_else(crate::area::missing)?
            .clock,
    );
    if let TimeDateEffect::SetTimeZone { zone } = &effect
        && zone == AUTOMATIC_ZONE
    {
        // The session's zone follower sets the zone from here on.
        return app
            .sections()
            .invoke::<ClockPrefs>(ClockPrefs::paths().automatic_zone.set(true))
            .map(|()| Effected::done())
            .map_err(|error| {
                format!("the time zone could not be set to follow this device: {error}")
            });
    }
    let manual_zone = matches!(effect, TimeDateEffect::SetTimeZone { .. });
    Ok(app.job(
        cx,
        move || match effect {
            TimeDateEffect::SetAutomaticTime { on } => clock
                .set_ntp(on)
                .map_err(|error| format!("automatic time could not be changed: {error}")),
            TimeDateEffect::SetTimeZone { zone } => {
                let zones = clock
                    .zones()
                    .map_err(|error| format!("time zones could not be read: {error}"))?;
                if !zones.contains(&zone) {
                    return Err(format!("`{zone}` is not a time zone of this device"));
                }
                clock
                    .set_timezone(&zone)
                    .map_err(|error| format!("time zone could not be changed: {error}"))
            }
        },
        move |app: &SettingsApp, outcome: Result<(), String>, _| {
            let area = crate::panes::time_date::area(app).ok_or_else(crate::area::missing)?;
            // What the clock took is read off the UI thread too.
            area.reading.refresh();
            match outcome {
                Ok(()) => {
                    area.notice.set(None);
                    if manual_zone {
                        // A zone the person picked stays: it no longer
                        // follows the device's location.
                        app.sections()
                            .invoke::<ClockPrefs>(ClockPrefs::paths().automatic_zone.set(false))?;
                    }
                    Ok(Effected::done())
                }
                Err(error) => {
                    area.notice.set(Some(error.clone()));
                    Err(error)
                }
            }
        },
    ))
}

/// What the page shows of a clock reading: every part the clock could not
/// read is kept as its failure and shown as one, never as a default.
struct Shown {
    sync_status: os_actions::TimeSyncStatus,
    /// Why timedated could not be asked for its state.
    status_error: Option<String>,
    /// Why the automatic-time switch's position is unknown.
    auto_error: Option<String>,
    timezone: String,
    /// The zones to choose from, or why the list could not be read; there
    /// is then nothing to choose.
    timezones: Result<Vec<String>, String>,
    timezone_points: Rc<[TimeZonePoint]>,
}

impl Shown {
    fn of(reading: ClockReading) -> Self {
        let (mut sync_status, status_error) = match reading.status {
            Ok(status) => (status, None),
            Err(error) => (os_actions::TimeSyncStatus::default(), Some(error)),
        };
        sync_status.ntp_enabled = reading.automatic;
        Self {
            timezone: sync_status.timezone.clone().unwrap_or_default(),
            sync_status,
            status_error,
            auto_error: reading.auto_error,
            timezones: reading.zones,
            timezone_points: reading.points.into(),
        }
    }
}

/// The person's formats, as Region & Formats keeps them.
fn formats(locale: Locale) -> ui::format::Formats {
    ui::format::Formats::with(ui::format::RegionFormats {
        date: locale.date_format,
        time: locale.time_format,
        number: Some(locale.number_style),
        first_day: locale.first_day_of_week,
    })
}

/// A zone as people name it: its city, and the region it is in
/// (`Amsterdam (Europe)`); `None` for an empty zone.
fn zone_name(zone: &str) -> Option<String> {
    if zone.is_empty() {
        return None;
    }
    Some(match zone.split_once('/') {
        Some((region, place)) => {
            let city = place.rsplit('/').next().unwrap_or(place).replace('_', " ");
            t_args(
                "time-zone-name",
                &[
                    ("city", ArgValue::from(city.as_str())),
                    ("region", ArgValue::from(region.replace('_', " ").as_str())),
                ],
            )
        }
        None => zone.replace('_', " "),
    })
}

/// The picker's choices, `(value, label)`: "Set automatically" first, then
/// every zone in `zones`, by city; `timezone` is the zone the device is in,
/// named in the first choice while the zone follows the device
/// (`automatic`).
fn zone_choices(zones: &[String], timezone: &str, automatic: bool) -> Vec<(String, String)> {
    let first = match zone_name(timezone) {
        Some(city) if automatic => t_args(
            "time-zone-automatic-now",
            &[("zone", ArgValue::from(city.as_str()))],
        ),
        _ => t("time-zone-automatic"),
    };
    let mut named: Vec<(String, String)> = zones
        .iter()
        .map(|zone| {
            (
                zone.clone(),
                zone_name(zone).unwrap_or_else(|| zone.clone()),
            )
        })
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    std::iter::once((AUTOMATIC_ZONE.to_owned(), first))
        .chain(named)
        .collect()
}

/// The automatic-time switch, or why its position is unknown; that the
/// clock is being read while nothing landed.
fn auto_row(cx: &ViewCx<ClockPrefs>, actions: &Actions<Settings>, shown: Option<&Shown>) -> Item {
    let Some(shown) = shown else {
        return value_row(&t("time-auto"), &t("time-reading"));
    };
    match (&shown.auto_error, shown.sync_status.ntp_enabled) {
        (Some(error), _) => error_row(
            &t("time-auto"),
            &t_args("time-status-error", &[("error", error.as_str().into())]),
        ),
        (None, automatic) => values::automatic_row(cx, actions, automatic.unwrap_or(false)),
    }
}

/// The one time-zone picker: a searchable list whose first choice is "Set
/// automatically", and the globe under it, which chooses too and faces the
/// zone chosen. Or why there is nothing to choose from.
fn zone_rows(
    actions: &Actions<Settings>,
    area: &TimeDateArea,
    shown: Option<&Shown>,
    automatic: bool,
) -> Vec<Item> {
    let Some(shown) = shown else {
        return vec![value_row(&t("time-timezone"), &t("time-reading"))];
    };
    let zones = match &shown.timezones {
        Ok(zones) => zones,
        Err(error) => {
            return vec![error_row(
                &t("time-timezone"),
                &t_args("time-zones-error", &[("error", error.as_str().into())]),
            )];
        }
    };
    let chosen = if automatic {
        AUTOMATIC_ZONE.to_owned()
    } else {
        shown.timezone.clone()
    };
    let mut picker = Dropdown::from_state(Vec::new(), &LocalState::new(chosen))
        .with_id("time.timezone")
        .with_resolved_accessible_name(t("time-timezone"));
    picker.set_value_options(zone_choices(zones, &shown.timezone, automatic));
    // The picker spans the card over the globe, as the arrangement's picker
    // does over the Displays canvas.
    let mut items = vec![block(Box::new(
        actions.bind_effect(picker.fill(), effect(TimeDateEffect::SET_TIME_ZONE)),
    ))];
    if automatic {
        items.push(note(&t("time-zone-automatic-caption")));
    }
    // The globe places the zones this device accepts at their tzdata
    // coordinates. Without the coordinates there is nothing to place, and
    // the picker stands alone rather than over an empty globe.
    if !shown.timezone_points.is_empty() {
        let globe = TimeZoneGlobe::new(
            Rc::clone(&shown.timezone_points),
            shown.timezone.clone(),
            Rc::clone(&area.globe),
            area.clock.unix_now(),
        )
        .on_action(actions.effect_binding(effect(TimeDateEffect::SET_TIME_ZONE)));
        items.push(block(Box::new(globe)));
    }
    items
}

/// The page over what the clock last reported, in the person's formats
/// (`formats`), the zone following the device while `automatic`.
fn page(
    cx: &ViewCx<ClockPrefs>,
    actions: &Actions<Settings>,
    area: &TimeDateArea,
    formats: ui::format::Formats,
    automatic: bool,
) -> Element {
    let shown = area.reading.get().map(Shown::of);
    let shown = shown.as_ref();
    let (now, today) = area.clock.now(&formats);
    let now = value_row(&t("time-current-time"), &now);
    let today = value_row(&t("time-current-date"), &today);
    let clock = section(
        &t("time-clock-title"),
        rows(vec![auto_row(cx, actions, shown), now, today]),
    );
    let zone = section(
        &t("time-timezone"),
        rows(zone_rows(actions, area, shown, automatic)),
    );
    let source = section(
        &t("time-source-title"),
        rows(match shown {
            None => vec![value_row(&t("time-sync"), &t("time-reading"))],
            Some(Shown {
                status_error: Some(error),
                ..
            }) => vec![error_row(
                &t("time-sync"),
                &t_args("time-status-error", &[("error", error.as_str().into())]),
            )],
            Some(shown) => {
                let status = &shown.sync_status;
                let sync = match status.synchronized {
                    Some(true) => t("time-sync-ok"),
                    Some(false) if status.ntp_enabled.unwrap_or(false) => t("time-sync-waiting"),
                    Some(false) => t("time-sync-disabled"),
                    None => t("time-sync-unknown"),
                };
                let source_name = status
                    .source
                    .clone()
                    .unwrap_or_else(|| t("time-source-unknown"));
                vec![
                    value_row(&t("time-sync"), &sync),
                    value_row(&t("time-source"), &source_name),
                ]
            }
        }),
    );
    let mut cards = vec![clock, zone, source];
    if let Some(notice) = area.notice.get() {
        cards.insert(
            0,
            section(&t("time-notice-title"), rows(vec![note(&notice)])),
        );
    }
    provider_settings_ui::widgets::page(cards)
}

/// The Time & Date page as view code over the person's formats
/// ([`Locale`]), this device's clock settings ([`ClockPrefs`]) and what the
/// area reads of the clock: built again whenever a reading lands, a change
/// fails or the minute turns ([`TimeDateArea::poll`]). timedated is
/// followed while the page is shown.
pub(crate) fn view(cx: &ViewCx<Settings>, app: &Rc<SettingsApp>) -> Option<Element> {
    let held = area(app)?;
    let (following, started) = held.follow();
    // Nothing was followed while no page showed it: the clock is read
    // again once.
    if started || held.reading.get().is_none() {
        held.reading.refresh();
    }
    held.changes_seen.set(following.changes());
    let shown = held.shown.clone();
    let actions = cx.actions();
    let app = Rc::clone(app);
    let locale = cx.settings::<Locale>();
    let prefs = cx.settings::<ClockPrefs>();
    cx.record(&locale, move |cx| {
        let (app, actions, shown, following, prefs) = (
            Rc::clone(&app),
            actions.clone(),
            shown.clone(),
            Rc::clone(&following),
            prefs.clone(),
        );
        cx.switch(&cx.value(|locale: &Locale| *locale), move |cx, locale| {
            let formats = formats(*locale);
            let (app, actions, shown, following) = (
                Rc::clone(&app),
                actions.clone(),
                shown.clone(),
                Rc::clone(&following),
            );
            cx.record(&prefs, move |cx| {
                cx.switch(&cx.value(|prefs: &ClockPrefs| *prefs), move |cx, prefs| {
                    let (app, actions, automatic) =
                        (Rc::clone(&app), actions.clone(), prefs.automatic_zone);
                    // The part keeps timedated followed while it is shown.
                    let following = Rc::clone(&following);
                    cx.switch(&shown, move |cx, _| {
                        let _followed = &following;
                        match area(&app) {
                            Some(area) => page(cx, &actions, area, formats, automatic),
                            None => provider_settings_ui::widgets::page(Vec::new()),
                        }
                    })
                })
            })
            .unwrap_or_else(|| provider_settings_ui::widgets::page(Vec::new()))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::clock::TimeStyle;
    use ui::prelude::TestApp;
    use ui::widget::Widget;

    fn fixture_points() -> Vec<TimeZonePoint> {
        let zones = vec![
            "Europe/Amsterdam".to_string(),
            "Europe/Berlin".to_string(),
            "America/New_York".to_string(),
        ];
        timezone_map_from_tab(
            &zones,
            "# installed tzdata fixture\n\
             NL\t+5222+00454\tEurope/Amsterdam\n\
             DE\t+5230+01322\tEurope/Berlin\n\
             US\t+404251-0740023\tAmerica/New_York\n\
             GB\t+513030-0000731\tEurope/London\n",
        )
    }

    /// A zone merged out of zone1970.tab (Amsterdam, only in zone.tab) still
    /// gets its dot; a zone in both tables keeps zone1970.tab's coordinates.
    #[test]
    fn a_zone_merged_out_of_zone1970_is_still_on_the_map() {
        // Case `a_zone_merged_out_of_zone1970_is_still_on_the_map`: the
        // device's own zone (here Amsterdam, only in zone.tab) gets its dot;
        // a zone in both tables keeps zone1970.tab's coordinates.
        {
            let zones = vec![
                "Europe/Amsterdam".to_string(),
                "Europe/Brussels".to_string(),
            ];
            let points = timezone_map_from_tab(
                &zones,
                "BE,LU,NL\t+5050+00420\tEurope/Brussels\n\
                 NL\t+5222+00454\tEurope/Amsterdam\n\
                 BE\t+0000+00000\tEurope/Brussels\n",
            );
            let brussels = points
                .iter()
                .find(|point| point.id == "Europe/Brussels")
                .expect("Brussels is mapped");
            assert_eq!(brussels.latitude_e6, 50_833_333);
            assert!(points.iter().any(|point| point.id == "Europe/Amsterdam"));
        }
    }

    /// tzdata parsing, keyboard access and non-empty timezones.
    #[test]
    fn timezone_map_parses_projects_and_commits_known_zones() {
        {
            let points = fixture_points();
            assert_eq!(
                points
                    .iter()
                    .map(|point| point.id.as_str())
                    .collect::<Vec<_>>(),
                ["America/New_York", "Europe/Amsterdam", "Europe/Berlin"]
            );
            let new_york = &points[0];
            assert_eq!(new_york.latitude_e6, 40_714_167);
            assert_eq!(new_york.longitude_e6, -74_006_389);
            assert!(
                points.iter().all(|point| point.id != "Europe/London"),
                "a coordinate-table entry not accepted by timedated must not enter the map"
            );
            assert_eq!(parse_iso6709_coordinate("+9001+00000"), None);
            assert_eq!(parse_iso6709_coordinate("garbage"), None);
        }

        {
            let mut map = TimeZoneGlobe::new(
                fixture_points().into(),
                "Europe/Amsterdam",
                SharedGlobe::default(),
                0,
            );
            map.set_focus(true);
            let access = map.accessibility().expect("map semantics");
            assert_eq!(access.role, ui::a11y::Role::Canvas);
            assert!(access.state.focusable);
            assert!(access.state.focused);

            let mut doc = ui::automerge::AutoCommit::new();
            let root = ui::automerge::ROOT;
            assert!(map.perform_action(
                &ui::a11y::Action::SetValue("Europe/Berlin".to_string()),
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
            assert_eq!(
                map.accessibility().and_then(|node| node.value),
                Some("Europe/Berlin".to_owned())
            );
            assert!(!map.perform_action(
                &ui::a11y::Action::SetValue("Mars/Olympus_Mons".to_string()),
                &[],
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
        }
    }

    /// How the picker names the capture's zone.
    const SAMPLE_ZONE_NAME: &str = "Amsterdam (Europe)";

    /// The zones [`FixtureClock`] lists.
    const FIXTURE_ZONES: [&str; 10] = [
        "Africa/Nairobi",
        "America/New_York",
        "America/Argentina/Buenos_Aires",
        "Asia/Tokyo",
        "Australia/Sydney",
        "Europe/Amsterdam",
        "Europe/Berlin",
        "Europe/London",
        "Pacific/Auckland",
        "UTC",
    ];

    /// The capture's clock over a fixed list of zones, three of them on the
    /// map, whatever tzdata the host has installed.
    #[derive(Default)]
    struct FixtureClock(SampleClock);

    impl Clock for FixtureClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            self.0.status()
        }
        fn ntp(&self) -> Result<bool, String> {
            self.0.ntp()
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            Ok(FIXTURE_ZONES.map(str::to_owned).to_vec())
        }
        fn points(&self, _: &[String]) -> Vec<TimeZonePoint> {
            fixture_points()
        }
        fn now(&self, formats: &ui::format::Formats) -> (String, String) {
            self.0.now(formats)
        }
        fn unix_now(&self) -> i64 {
            self.0.unix_now()
        }
        fn set_ntp(&self, enabled: bool) -> Result<(), String> {
            self.0.set_ntp(enabled)
        }
        fn set_timezone(&self, timezone: &str) -> Result<(), String> {
            self.0.set_timezone(timezone)
        }
    }

    /// A Settings window whose Time & Date area reads `clock`, on its page,
    /// once the page shows what the clock reported.
    fn window_over(clock: Arc<dyn Clock>) -> TestApp<SettingsShell> {
        ui::i18n::I18nManager::init("en-US");
        crate::area::replace_in_next_window(move |_| TimeDateArea::over(clock));
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        crate::effects::show_pane(&mut app, crate::categories::Category::TimeDate);
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(&t("time-clock-title"))
                        && !frame.contains(&t("time-reading")))),
            "the clock is read"
        );
        app.settle();
        app
    }

    fn ids_of(widget: &dyn Widget, ids: &mut Vec<String>) {
        if let Some(id) = widget.test_id() {
            ids.push(id.to_owned());
        }
        for child in widget.children() {
            ids_of(child, ids);
        }
    }

    /// Every control id the window shows.
    fn shown_ids(app: &mut TestApp<SettingsShell>) -> Vec<String> {
        let mut ids = Vec::new();
        ids_of(app.harness().root_widget(), &mut ids);
        ids
    }

    /// A clock whose timedated's changes are followed by hand.
    #[derive(Default)]
    struct FollowedClock {
        sample: SampleClock,
        reads: std::sync::atomic::AtomicUsize,
        follower: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    }

    impl Clock for FollowedClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.sample.status()
        }
        fn ntp(&self) -> Result<bool, String> {
            self.sample.ntp()
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            self.sample.zones()
        }
        fn now(&self, formats: &ui::format::Formats) -> (String, String) {
            self.sample.now(formats)
        }
        fn unix_now(&self) -> i64 {
            self.sample.unix_now()
        }
        fn set_ntp(&self, enabled: bool) -> Result<(), String> {
            self.sample.set_ntp(enabled)
        }
        fn set_timezone(&self, timezone: &str) -> Result<(), String> {
            self.sample.set_timezone(timezone)
        }
        fn follow(&self, changed: Box<dyn Fn() + Send + Sync>) -> Option<Box<dyn std::any::Any>> {
            *self.follower.lock_recover() = Some(Arc::from(changed));
            Some(Box::new(()))
        }
    }

    /// The open page reads the clock again when timedated reports a change,
    /// and only then: a zone set elsewhere shows.
    #[test]
    fn the_shown_page_reads_the_clock_again_when_timedated_reports_a_change() {
        let clock = Arc::new(FollowedClock::default());
        let mut app = window_over(Arc::clone(&clock) as Arc<dyn Clock>);
        let reads = || clock.reads.load(std::sync::atomic::Ordering::Relaxed);
        let before = reads();
        assert!(before >= 1, "the page read the clock");
        let changed = clock
            .follower
            .lock_recover()
            .clone()
            .expect("timedated is followed while the page shows");

        clock.sample.status.lock_recover().timezone = Some("Asia/Tokyo".to_owned());
        changed();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Tokyo (Asia)"))),
            "the zone set elsewhere shows"
        );
        let after = reads();
        assert!(after > before, "a change reads the clock again");
        for _ in 0..3 {
            app.settle();
        }
        assert_eq!(reads(), after, "nothing reads while nothing changes");
    }

    #[test]
    fn the_clock_and_the_date_show_in_the_region_formats_with_no_switch_of_their_own() {
        let mut app = window_over(Arc::new(FixtureClock::default()));
        let ids = shown_ids(&mut app);
        assert!(
            !ids.iter().any(|id| id == "time.h24"),
            "Time & date has no clock-format switch: {ids:?}"
        );
        // Unset, both follow the language: US English reads 2:05 PM and
        // Mar 15, 2023, exactly as Region's "Follow language" shows them.
        assert_eq!(
            app.settings::<Locale>().get().time_format,
            ui::clock::TimeStyle::Language
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(
            frame.contains(&ui::clock::TimeStyle::Language.example()),
            "{frame}"
        );
        assert!(
            frame.contains(&ui::format::DateStyle::Language.example()),
            "{frame}"
        );

        app.settings::<Locale>()
            .update(|locale| {
                locale.date_format = ui::format::DateStyle::Iso;
                locale.time_format = ui::clock::TimeStyle::H24;
            })
            .unwrap();
        app.settle();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("2023-03-15"), "{frame}");
        assert!(frame.contains("14:05"), "{frame}");
        assert!(!frame.contains("Wednesday 15 March 2023"), "{frame}");
    }

    #[test]
    fn the_time_zone_is_one_searchable_picker_led_by_automatic_with_the_globe_as_its_visual() {
        ui::i18n::I18nManager::init("en-US");
        let zones = FIXTURE_ZONES.map(str::to_owned);
        let choices = zone_choices(&zones, SAMPLE_TIMEZONE, false);
        assert_eq!(
            choices.first(),
            Some(&(AUTOMATIC_ZONE.to_owned(), t("time-zone-automatic"))),
            "automatic detection is the first choice"
        );
        assert!(
            choices.len() > ui::widget::FILTER_MIN_OPTIONS,
            "the device's zones make a list the picker searches"
        );
        assert!(
            choices
                .iter()
                .any(|(value, label)| value == SAMPLE_TIMEZONE && label == SAMPLE_ZONE_NAME)
        );

        let mut app = window_over(Arc::new(FixtureClock::default()));
        let ids = shown_ids(&mut app);
        assert_eq!(
            ids.iter()
                .filter(|id| id.as_str() == "time.timezone")
                .count(),
            1,
            "one zone control: {ids:?}"
        );
        assert!(ids.iter().any(|id| id == "time.timezone-map"), "{ids:?}");
        assert!(
            !ids.iter().any(|id| id.starts_with("time.nearby")),
            "no second list of cities that sets the zone: {ids:?}"
        );
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("time-timezone")), "{frame}");
        assert!(!frame.contains("World clock"), "{frame}");

        // Automatic is a choice of the same picker; a zone picked turns it
        // off again.
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
            zone: AUTOMATIC_ZONE.into(),
        }))
        .expect("the zone follows the device");
        assert!(app.settings::<ClockPrefs>().get().automatic_zone);
        assert_eq!(
            app.settings::<ClockPrefs>()
                .document()
                .section(settings_store::clock::SECTION)
                [settings_store::clock::AUTOMATIC_ZONE.name()],
            true,
            "the session's zone follower reads this leaf"
        );
        app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
            zone: "Europe/Berlin".into(),
        }))
        .expect("a known zone is applied");
        assert!(!app.settings::<ClockPrefs>().get().automatic_zone);
    }

    /// A clock whose timedated cannot be asked: no `timedatectl`, no system
    /// bus.
    struct UnreadableClock;

    impl Clock for UnreadableClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            Err("timedatectl not installed".into())
        }
        fn ntp(&self) -> Result<bool, String> {
            Err("timedatectl not installed".into())
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            Err("timedatectl not installed".into())
        }
        fn now(&self, formats: &ui::format::Formats) -> (String, String) {
            sample_now(*formats)
        }
        fn set_ntp(&self, _: bool) -> Result<(), String> {
            Err("timedatectl not installed".into())
        }
        fn set_timezone(&self, _: &str) -> Result<(), String> {
            Err("timedatectl not installed".into())
        }
    }

    /// A host whose timedated cannot be asked shows each unread value as
    /// that failure: the automatic-time switch is not offered at a guessed
    /// position, the zone is not a sample zone and synchronisation is not
    /// "unknown".
    #[test]
    fn unreadable_time_settings_are_shown_as_errors() {
        let mut app = window_over(Arc::new(UnreadableClock));
        let frame = app.harness().inspection_json().unwrap();
        assert!(
            frame.contains("Can't read time settings: timedatectl not installed"),
            "{frame}"
        );
        assert!(
            frame.contains("Can't list time zones: timedatectl not installed"),
            "{frame}"
        );
        assert!(!frame.contains(&t("time-sync-unknown")), "{frame}");
        assert!(!frame.contains(SAMPLE_TIMEZONE), "{frame}");
        let ids = shown_ids(&mut app);
        assert!(
            !ids.iter().any(|id| id == "time.timezone-map"),
            "with no zones to place there is no globe, not an empty one: {ids:?}"
        );
        assert!(
            !ids.iter().any(|id| id == "time.auto"),
            "no switch at a guessed position: {ids:?}"
        );
        let shown = |app: &mut TestApp<SettingsShell>| {
            app.with_widget::<crate::window::Inspected, _>(|settings| {
                area(settings).unwrap().shown.get()
            })
            .expect("the Settings window is mounted")
        };
        let before = shown(&mut app);
        app.settle();
        app.settle();
        assert_eq!(
            shown(&mut app),
            before,
            "a page without the globe is current until something it shows \
             changes, or the window would rebuild it on every pass and never settle"
        );
    }

    /// A clock whose timedated does not answer until it is let go, as
    /// `timedatectl` does behind a filtered system bus that holds the call
    /// until the D-Bus timeout.
    struct StalledClock(std::sync::Arc<crate::stall::Stall>);

    impl StalledClock {
        fn wait(&self) {
            self.0.wait();
        }
    }

    impl Clock for StalledClock {
        fn status(&self) -> Result<os_actions::TimeSyncStatus, String> {
            self.wait();
            SampleClock::default().status()
        }
        fn ntp(&self) -> Result<bool, String> {
            self.wait();
            Ok(true)
        }
        fn zones(&self) -> Result<Vec<String>, String> {
            self.wait();
            SampleClock::default().zones()
        }
        fn now(&self, formats: &ui::format::Formats) -> (String, String) {
            SampleClock::default().now(formats)
        }
        fn set_ntp(&self, _: bool) -> Result<(), String> {
            self.wait();
            Ok(())
        }
        fn set_timezone(&self, _: &str) -> Result<(), String> {
            self.wait();
            Ok(())
        }
    }

    /// Opening Time & Date while timedated takes its time is answered and
    /// drawn at once: the pane says the clock is being read, and shows the
    /// zone once timedated answers.
    #[test]
    fn a_stalled_timedated_never_holds_the_window() {
        ui::i18n::I18nManager::init("en-US");
        let gate = crate::stall::Stall::new();
        let stalled = Arc::clone(&gate);
        crate::area::replace_in_next_window(move |_| {
            TimeDateArea::over(Arc::new(StalledClock(stalled)))
        });
        let mut app = TestApp::<SettingsShell>::new().unwrap();

        crate::effects::show_pane(&mut app, crate::categories::Category::TimeDate);
        app.harness()
            .poll_frame()
            .expect("the window draws its next frame");
        gate.assert_held("opening the pane");
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains(&t("time-reading")), "{frame}");
        assert!(!frame.contains(SAMPLE_ZONE_NAME), "{frame}");

        gate.release();
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains(SAMPLE_ZONE_NAME))),
            "the zone timedated reports is shown"
        );
    }

    /// Automatic time and the zone are this device's, changed through its
    /// clock; a zone the device does not know is refused. The clock format
    /// is the person's locale, set in Region & Formats.
    #[test]
    fn time_effects_reach_the_clock_and_the_locale() {
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
            zone: "Europe/Berlin".into(),
        }))
        .expect("a known zone is applied");
        app.effect(&SettingsEffect::from(TimeDateEffect::SetAutomaticTime {
            on: false,
        }))
        .expect("automatic time is turned off");
        app.effect(&SettingsEffect::from(
            crate::effects::NavigationEffect::OpenPane {
                pane: "time_date".into(),
            },
        ))
        .unwrap();
        // The clock is read off the UI thread: the pane shows the zone once
        // the reading lands.
        assert!(
            app.harness()
                .wait_until(tilo_test_env::hang_bound(), |harness| harness
                    .inspection_json()
                    .is_ok_and(|frame| frame.contains("Berlin (Europe)"))),
            "the zone the clock took is shown"
        );
        assert!(
            app.effect(&SettingsEffect::from(TimeDateEffect::SetTimeZone {
                zone: "Mars/Olympus_Mons".into(),
            }))
            .is_err()
        );

        crate::testing::edit_field(&mut app, "settings.locale", "time_format", "H12")
            .expect("the clock format is the locale's");
        assert_eq!(app.settings::<Locale>().get().time_format, TimeStyle::H12);
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("2:05 PM"), "{frame}");
    }
}
