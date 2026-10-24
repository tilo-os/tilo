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
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource;
use ui::widget::{AppModel, Dropdown, ModelPath, Widget};

use crate::device_read::Reading;
use crate::effects::effect;
use crate::host::{Followed, PaneCx};
use crate::i18n::{t, t_args};
use crate::resource::Settings;
use crate::settings_app::SettingsApp;
use crate::widgets::{Item, block, error_row, note, rows, section, value_row};
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
}

/// The window's [`TimeDateArea`].
pub fn area(app: &crate::settings_app::SettingsApp) -> Option<&TimeDateArea> {
    app.areas().find()
}

/// What the time settings found by search are set to, as the page last
/// read the clock: nothing until it has.
pub(crate) mod values {
    use ui::LocalState;

    use super::{ClockPrefs, TimeDateEffect, TimeDatePane, area};
    use crate::effects::effect;
    use crate::host::PaneCx;
    use crate::i18n::t;
    use crate::setting::{Follows, Read};
    use crate::settings_app::SettingsApp;
    use crate::widgets::Item;

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

    /// The automatic-time switch.
    pub(crate) fn automatic_switch(cx: &PaneCx, on: bool) -> Item {
        cx.row(t("time-auto")).id("time.auto").toggle_effect(
            &LocalState::new(on),
            effect(TimeDateEffect::SET_AUTOMATIC_TIME),
        )
    }

    pub(crate) fn zone(app: &SettingsApp, follows: &Follows) -> Option<String> {
        let reading = area(app)?.reading.get()?;
        let zone = reading.status.ok()?.timezone?;
        let city = TimeDatePane::zone_name(&zone)?;
        Some(if follows.get::<ClockPrefs>(app.host()).automatic_zone {
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
}

/// The device's clock through timedated.
pub struct SystemClock;

impl Clock for SystemClock {
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

/// This device's clock settings: whether its time zone follows its
/// location. The session's zone follower reads the same leaf
/// ([`settings_store::clock::AUTOMATIC_ZONE`]).
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    ui::tilo_resource::Model,
)]
pub struct ClockPrefs {
    /// The zone follows the device's location.
    #[serde(default)]
    pub automatic_zone: bool,
}

impl ui::storage::SettingsSchema for ClockPrefs {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Shared(settings_store::clock::SECTION);
}

#[ui::tilo_resource::resource(kind = "settings.clock", label = "Clock")]
impl ClockPrefs {}

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
        let points = zones.as_deref().map(timezone_map).unwrap_or_default();
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
    /// What the clock last reported, read again off the UI thread whenever
    /// the pane opens and after every change.
    reading: Reading<ClockReading>,
    /// Why the last change failed, for the pane to show.
    notice: LocalState<Option<String>>,
}

impl TimeDateArea {
    /// Read the clock in the background unless it has been, so the page
    /// opens on what it last reported.
    pub(crate) fn warm(&self) {
        if self.reading.get().is_none() {
            self.reading.refresh();
        }
    }

    /// The area over this device's clock, or a stand-in for a capture.
    /// Nothing is read until the pane opens.
    pub fn new(cx: &PaneCx) -> Self {
        let clock: Arc<dyn Clock> = if cx.reaches_devices() {
            Arc::new(SystemClock)
        } else {
            Arc::new(SampleClock::default())
        };
        Self::over(clock)
    }

    fn over(clock: Arc<dyn Clock>) -> Self {
        let read = Arc::clone(&clock);
        Self {
            clock,
            reading: Reading::new(move || ClockReading::read(read.as_ref())),
            notice: LocalState::new(None),
        }
    }
}

/// Answer `effect`. Device changes run off the UI thread: timedated may ask
/// for authorisation.
pub(crate) fn answer(
    app: &SettingsApp,
    effect: TimeDateEffect,
    cx: &mut EffectCx<Settings, SettingsApp>,
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
        app.host()
            .settings::<ClockPrefs>()
            .update(|prefs| prefs.automatic_zone = true)
            .map_err(|error| {
                format!("the time zone could not be set to follow this device: {error}")
            })?;
        return Ok(Effected::done());
    }
    let manual_zone = matches!(effect, TimeDateEffect::SetTimeZone { .. });
    Ok(crate::device_read::job(
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
        move |app: &mut SettingsApp, outcome: Result<(), String>, _| {
            let area = crate::panes::time_date::area(app).ok_or_else(crate::area::missing)?;
            // What the clock took is read off the UI thread too.
            area.reading.refresh();
            match outcome {
                Ok(()) => {
                    area.notice.set(None);
                    if manual_zone {
                        // A zone the person picked stays: it no longer
                        // follows the device's location.
                        app.host()
                            .settings::<ClockPrefs>()
                            .update(|prefs| prefs.automatic_zone = false)
                            .map_err(|error| error.to_string())?;
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

/// Time & Date pane: what the device's clock reports, in the person's
/// formats, and its time zone.
pub struct TimeDatePane {
    cx: PaneCx,
    clock: Arc<dyn Clock>,
    locale: Followed<Locale>,
    /// Whether the zone follows the device's location.
    prefs: Followed<ClockPrefs>,
    notice: LocalState<Option<String>>,
    notice_shown: Option<String>,
    /// What the clock reports, read off the UI thread.
    reading: Reading<ClockReading>,
    /// The revision of `reading` taken in.
    reading_seen: u64,
    /// The revision of `reading` the last `update` reported. A reading
    /// lands off the UI thread at any moment; `sync` takes in only what
    /// `update` saw, so a frame that reported nothing syncs nothing, and a
    /// later landing is taken up by the frame its wake asks for.
    reading_reported: u64,
    /// The minute the last `update` read the clock at, which `sync` builds
    /// for when it is past the one shown: the clock moves on between the two
    /// as well.
    minute_reported: i64,
    /// Whether a reading has landed; until then the rows say the clock is
    /// being read.
    read: bool,
    timezone: String,
    timezones: Vec<String>,
    timezone_points: Rc<[TimeZonePoint]>,
    /// Which way the globe faces, kept across the pane's rebuilds.
    globe: SharedGlobe,
    /// The minute the shown times were told at.
    minute: std::cell::Cell<i64>,
    sync_status: os_actions::TimeSyncStatus,
    /// Why timedated could not be asked for its state.
    status_error: Option<String>,
    /// Why the zone list could not be read; there is then nothing to choose.
    timezones_error: Option<String>,
    /// The configured zone could not be found anywhere.
    timezone_unknown: bool,
    /// Why the automatic-time switch's position is unknown.
    auto_error: Option<String>,
    composed: ui::widget::Composed,
}

impl TimeDatePane {
    /// The pane over `area`'s clock and `cx`'s locale. It shows what the
    /// clock last reported, or that it is being read, and reads it again
    /// off the UI thread. Every part the clock could not read is kept as
    /// its failure and shown as one, never as a default.
    pub fn new(cx: PaneCx, area: &TimeDateArea) -> Self {
        let locale = cx.follow::<Locale>();
        let prefs = cx.follow::<ClockPrefs>();
        area.reading.refresh();
        let mut pane = Self {
            cx,
            clock: Arc::clone(&area.clock),
            locale,
            prefs,
            notice: area.notice.clone(),
            notice_shown: area.notice.get(),
            reading: area.reading.clone(),
            reading_seen: area.reading.revision(),
            reading_reported: area.reading.revision(),
            minute_reported: 0,
            read: false,
            timezone: String::new(),
            timezones: Vec::new(),
            timezone_points: Rc::from(Vec::new()),
            globe: SharedGlobe::default(),
            minute: std::cell::Cell::new(0),
            sync_status: os_actions::TimeSyncStatus::default(),
            status_error: None,
            timezones_error: None,
            timezone_unknown: false,
            auto_error: None,
            composed: ui::widget::Composed::default(),
        };
        if let Some(reading) = area.reading.get() {
            pane.adopt(reading);
        }
        pane
    }

    /// Show what the clock reported.
    fn adopt(&mut self, reading: ClockReading) {
        self.read = true;
        (self.sync_status, self.status_error) = match reading.status {
            Ok(status) => (status, None),
            Err(error) => (os_actions::TimeSyncStatus::default(), Some(error)),
        };
        self.sync_status.ntp_enabled = reading.automatic;
        self.auto_error = reading.auto_error;
        self.timezone_unknown = self.sync_status.timezone.is_none();
        self.timezone = self.sync_status.timezone.clone().unwrap_or_default();
        (self.timezones, self.timezones_error) = match reading.zones {
            Ok(zones) => (zones, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        self.timezone_points = reading.points.into();
    }

    /// The person's formats, as Region & Formats keeps them.
    fn formats(&self) -> ui::format::Formats {
        let locale = self.locale.get();
        ui::format::Formats::with(ui::format::RegionFormats {
            date: locale.date_format,
            time: locale.time_format,
            number: Some(locale.number_style),
            first_day: locale.first_day_of_week,
        })
    }

    /// The automatic-time switch, or why its position is unknown.
    fn auto_row(&self) -> Item {
        if !self.read {
            return value_row(&t("time-auto"), &t("time-reading"));
        }
        match (&self.auto_error, self.sync_status.ntp_enabled) {
            (Some(error), _) => error_row(
                &t("time-auto"),
                &t_args("time-status-error", &[("error", error.as_str().into())]),
            ),
            (None, automatic) => values::automatic_switch(&self.cx, automatic.unwrap_or(false)),
        }
    }

    /// The picker's choices, `(value, label)`: "Set automatically" first,
    /// then every zone this device accepts, by city.
    fn zone_choices(&self) -> Vec<(String, String)> {
        let automatic = match Self::zone_name(&self.timezone) {
            Some(city) if self.prefs.get().automatic_zone => t_args(
                "time-zone-automatic-now",
                &[("zone", ArgValue::from(city.as_str()))],
            ),
            _ => t("time-zone-automatic"),
        };
        let mut zones: Vec<(String, String)> = self
            .timezones
            .iter()
            .map(|zone| {
                (
                    zone.clone(),
                    Self::zone_name(zone).unwrap_or_else(|| zone.clone()),
                )
            })
            .collect();
        zones.sort_by(|a, b| a.1.cmp(&b.1));
        std::iter::once((AUTOMATIC_ZONE.to_owned(), automatic))
            .chain(zones)
            .collect()
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

    /// The one time-zone picker: a searchable list whose first choice is
    /// "Set automatically", and the globe under it, which chooses too and
    /// faces the zone chosen. Or why there is nothing to choose from.
    fn zone_rows(&self) -> Vec<Item> {
        if !self.read {
            return vec![value_row(&t("time-timezone"), &t("time-reading"))];
        }
        if let Some(error) = &self.timezones_error {
            return vec![error_row(
                &t("time-timezone"),
                &t_args("time-zones-error", &[("error", error.as_str().into())]),
            )];
        }
        let automatic = self.prefs.get().automatic_zone;
        let chosen = if automatic {
            AUTOMATIC_ZONE.to_owned()
        } else {
            self.timezone.clone()
        };
        let mut picker = Dropdown::from_state(Vec::new(), &LocalState::new(chosen))
            .with_id("time.timezone")
            .with_resolved_accessible_name(t("time-timezone"));
        picker.set_value_options(self.zone_choices());
        // The picker spans the card over the globe, as the arrangement's
        // picker does over the Displays canvas.
        let mut items = vec![block(Box::new(
            self.cx
                .bind(picker.fill(), effect(TimeDateEffect::SET_TIME_ZONE)),
        ))];
        if automatic {
            items.push(note(&t("time-zone-automatic-caption")));
        }
        // The globe places the zones this device accepts at their tzdata
        // coordinates. Without the coordinates there is nothing to place,
        // and the picker stands alone rather than over an empty globe.
        if !self.timezone_points.is_empty() {
            let globe = TimeZoneGlobe::new(
                Rc::clone(&self.timezone_points),
                self.timezone.clone(),
                Rc::clone(&self.globe),
                self.clock.unix_now(),
            )
            .on_action(self.cx.binding(effect(TimeDateEffect::SET_TIME_ZONE)));
            items.push(block(Box::new(globe)));
        }
        items
    }

    /// The minute the clock reads now.
    fn minute_now(&self) -> i64 {
        self.clock.unix_now().div_euclid(60)
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // What this composition shows changes with the minute: `sync`
        // builds again when it moves on.
        self.minute.set(self.clock.unix_now().div_euclid(60));
        let (now, today) = self.clock.now(&self.formats());
        let now = value_row(&t("time-current-time"), &now);
        let today = value_row(&t("time-current-date"), &today);
        let auto_row = self.auto_row();

        let automatic = self.sync_status.ntp_enabled.unwrap_or(false);
        let sync = match self.sync_status.synchronized {
            Some(true) => t("time-sync-ok"),
            Some(false) if automatic => t("time-sync-waiting"),
            Some(false) => t("time-sync-disabled"),
            None => t("time-sync-unknown"),
        };
        let source_name = self
            .sync_status
            .source
            .clone()
            .unwrap_or_else(|| t("time-source-unknown"));

        let clock = section(&t("time-clock-title"), rows(vec![auto_row, now, today]));
        let zone = section(&t("time-timezone"), rows(self.zone_rows()));

        let source = section(
            &t("time-source-title"),
            rows(match &self.status_error {
                _ if !self.read => vec![value_row(&t("time-sync"), &t("time-reading"))],
                Some(error) => vec![error_row(
                    &t("time-sync"),
                    &t_args("time-status-error", &[("error", error.as_str().into())]),
                )],
                None => vec![
                    value_row(&t("time-sync"), &sync),
                    value_row(&t("time-source"), &source_name),
                ],
            }),
        );

        let mut cards = vec![clock, zone, source];
        if let Some(notice) = &self.notice_shown {
            cards.insert(
                0,
                section(&t("time-notice-title"), rows(vec![note(notice)])),
            );
        }
        crate::widgets::page(cards)
    }
}

impl Widget for TimeDatePane {
    /// What `sync` takes up from outside the document: a clock reading that
    /// landed off the UI thread (after the pane opened, or after a change
    /// the clock took), an effect's notice, a new minute, the formats or the
    /// clock preferences stored. A frame that
    /// reports none of them does not sync.
    fn update(&mut self, state: &AppModel, root: &ModelPath) -> bool {
        self.reading_reported = self.reading.revision();
        self.minute_reported = self.minute_now();
        let own = self.reading_reported != self.reading_seen
            || self.notice.get() != self.notice_shown
            || self.minute_reported > self.minute.get()
            || self.locale.moved()
            || self.prefs.moved();
        let mut children = false;
        for child in self.children_mut() {
            children |= child.update(state, root);
        }
        own || children
    }

    fn sync(&mut self, state: &AppModel, root: &ModelPath) {
        let revision = self.reading_reported;
        if revision != self.reading_seen {
            self.reading_seen = revision;
            if let Some(reading) = self.reading.get() {
                self.adopt(reading);
            }
            self.composed.invalidate();
        }
        let notice = self.notice.get();
        if notice != self.notice_shown {
            self.notice_shown = notice;
            self.composed.invalidate();
        }
        // The formats, whether the zone follows the device, and a new
        // minute change what the page shows.
        if self.locale.refresh() | self.prefs.refresh() {
            self.composed.invalidate();
        }
        if self.minute_reported > self.minute.get() {
            self.composed.invalidate();
        }
        self.composed.sync(state, root);
    }

    /// The minute deadline it asked for came: what `update` read just
    /// before may still have been the old minute.
    fn serve_deadline(&mut self, _now: ui::time::Instant) -> bool {
        self.minute_reported = self.minute_now();
        true
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let children = self.composed.element().and_then(Widget::next_wake_deadline);
        if !self.cx.reaches_devices() {
            // A capture's clock stands still.
            return children;
        }
        // The times shown change on the minute.
        let into_minute = self.clock.unix_now().rem_euclid(60);
        let own = ui::time::Instant::now()
            + std::time::Duration::from_secs(u64::try_from(60 - into_minute).unwrap_or(60));
        Some(children.map_or(own, |child| child.min(own)))
    }

    crate::panes::composed_pane!();

    fn has_pending_visual_work(&self) -> bool {
        // The clock's first reading replaces "Reading the clock…".
        !self.read
            || self
                .composed
                .element()
                .is_some_and(Widget::has_pending_visual_work)
    }

    fn name(&self) -> &'static str {
        "TimeDatePane"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::clock::TimeStyle;
    use ui::prelude::TestApp;

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

    /// The pane over the capture's clock and a fresh profile, with the
    /// clock read.
    fn read_pane(cx: &PaneCx) -> TimeDatePane {
        let area = TimeDateArea::over(Arc::new(SampleClock::default()));
        let mut pane = TimeDatePane::new(cx.clone(), &area);
        let mut reading = tilo_test_env::wait_for("the clock is read", || area.reading.get());
        reading.points = fixture_points();
        reading.zones = Ok([
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
        ]
        .map(str::to_owned)
        .to_vec());
        pane.adopt(reading);
        pane
    }

    fn ids_of(widget: &dyn Widget, ids: &mut Vec<String>) {
        if let Some(id) = widget.test_id() {
            ids.push(id.to_owned());
        }
        for child in widget.children() {
            ids_of(child, ids);
        }
    }

    #[test]
    fn the_clock_and_the_date_show_in_the_region_formats_with_no_switch_of_their_own() {
        ui::i18n::I18nManager::init("en-US");
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let mut pane = read_pane(&cx);
        let composition = pane.build_composition();
        let mut ids = Vec::new();
        ids_of(composition.as_ref(), &mut ids);
        assert!(
            !ids.iter().any(|id| id == "time.h24"),
            "Time & date has no clock-format switch: {ids:?}"
        );
        // Unset, both follow the language: US English reads 2:05 PM and
        // Mar 15, 2023, exactly as Region's "Follow language" shows them.
        let text = crate::widgets::announced_text(composition.as_ref());
        let says = |text: &[String], needle: &str| text.iter().any(|line| line == needle);
        assert_eq!(
            pane.locale.get().time_format,
            ui::clock::TimeStyle::Language
        );
        assert!(
            says(&text, &ui::clock::TimeStyle::Language.example()),
            "{text:#?}"
        );
        assert!(
            says(&text, &ui::format::DateStyle::Language.example()),
            "{text:#?}"
        );

        cx.settings::<Locale>()
            .update(|locale| {
                locale.date_format = ui::format::DateStyle::Iso;
                locale.time_format = ui::clock::TimeStyle::H24;
            })
            .unwrap();
        let doc = ui::automerge::AutoCommit::new();
        pane.sync(&doc, &ui::automerge::ROOT);
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        assert!(says(&text, "2023-03-15"), "{text:#?}");
        assert!(says(&text, "14:05"), "{text:#?}");
        assert!(!says(&text, "Wednesday 15 March 2023"), "{text:#?}");
    }

    #[test]
    fn the_time_zone_is_one_searchable_picker_led_by_automatic_with_the_globe_as_its_visual() {
        ui::i18n::I18nManager::init("en-US");
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let pane = read_pane(&cx);
        let choices = pane.zone_choices();
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

        let composition = pane.build_composition();
        let mut ids = Vec::new();
        ids_of(composition.as_ref(), &mut ids);
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
        let text = crate::widgets::announced_text(composition.as_ref());
        assert!(
            text.iter().any(|line| line == &t("time-timezone")),
            "{text:#?}"
        );
        assert!(!text.iter().any(|line| line == "World clock"), "{text:#?}");

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
        let profile = tempfile::tempdir().unwrap();
        let cx = PaneCx::in_file(profile.path().join("settings.json"));
        let area = TimeDateArea::over(Arc::new(UnreadableClock));
        let mut pane = TimeDatePane::new(cx, &area);
        // The reading may land before the pane notes the revision it saw,
        // so wait for the reading itself, not for a later revision.
        let reading = tilo_test_env::wait_for("the clock is read", || area.reading.get());
        pane.adopt(reading);
        assert!(pane.timezone_unknown);
        let text = crate::widgets::announced_text(pane.build_composition().as_ref());
        let says = |needle: &str| text.iter().any(|line| line == needle);
        assert!(
            says("Can't read time settings: timedatectl not installed"),
            "{text:#?}"
        );
        assert!(
            says("Can't list time zones: timedatectl not installed"),
            "{text:#?}"
        );
        assert!(!says(&t("time-sync-unknown")), "{text:#?}");
        let mut ids = Vec::new();
        ids_of(pane.build_composition().as_ref(), &mut ids);
        assert!(
            !ids.iter().any(|id| id == "time.timezone-map"),
            "with no zones to place there is no globe, not an empty one: {ids:?}"
        );
        assert!(
            pane.minute_now() == pane.minute.get(),
            "a composition without the globe is current until the minute turns, \
             or the window would rebuild it on every pass and never settle"
        );
        assert!(
            !text.iter().any(|line| line.contains(SAMPLE_TIMEZONE)),
            "{text:#?}"
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
        let mut app = TestApp::<SettingsShell>::new().unwrap();
        let gate = crate::stall::Stall::new();
        let stalled = Arc::clone(&gate);
        app.with_widget::<SettingsApp, _>(move |settings| {
            settings.replace_area(TimeDateArea::over(Arc::new(StalledClock(stalled))));
        })
        .expect("the Settings window is mounted");

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

        crate::host::testing::edit_field(&mut app, "settings.locale", "time_format", "H12")
            .expect("the clock format is the locale's");
        assert_eq!(app.settings::<Locale>().get().time_format, TimeStyle::H12);
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("2:05 PM"), "{frame}");
    }
}
