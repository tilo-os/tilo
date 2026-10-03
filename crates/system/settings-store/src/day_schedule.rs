//! When the day ends: the one schedule automatic light/dark and night light share.
//!
//! The `appearance` section holds the choice: follow the sun (`schedule: "sun"`, the default)
//! or fixed times (`schedule: "custom"` with `sunrise`/`sunset` as `HH:MM`). Following the sun
//! computes sunrise and sunset for today at the location the system time zone names in tzdata's
//! `zone1970.tab`; without a usable location (no zone, a zone missing from the table, polar day
//! or night) it uses the fixed times, which default to 07:00 and 19:00.
//!
//! Everything here is pure except [`timezone_location`], so callers pass the local moment in
//! and tests pin it.

use std::path::Path;

use serde_json::{Map, Value};

/// The section holding the schedule.
pub const SECTION: &str = "appearance";
/// `"sun"` or `"custom"`.
pub const SCHEDULE_KEY: &str = "schedule";
/// Custom sunrise, `HH:MM`.
pub const SUNRISE_KEY: &str = "sunrise";
/// Custom sunset, `HH:MM`.
pub const SUNSET_KEY: &str = "sunset";
/// The theme value meaning "light by day, dark by night".
pub const AUTOMATIC_THEME: &str = "Automatic";

const MINUTES_PER_DAY: i32 = 24 * 60;

/// Sunrise and sunset as minutes after local midnight.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DaySchedule {
    pub sunrise: u16,
    pub sunset: u16,
}

impl DaySchedule {
    /// The fallback when neither a location nor custom times say otherwise.
    pub const FIXED: Self = Self {
        sunrise: 7 * 60,
        sunset: 19 * 60,
    };

    /// Whether `minute_of_day` falls between sunset and the next sunrise.
    pub fn is_night(self, minute_of_day: u16) -> bool {
        if self.sunrise <= self.sunset {
            minute_of_day < self.sunrise || minute_of_day >= self.sunset
        } else {
            minute_of_day >= self.sunset && minute_of_day < self.sunrise
        }
    }
}

impl Default for DaySchedule {
    fn default() -> Self {
        Self::FIXED
    }
}

/// A local wall-clock moment: what the sun position depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalMoment {
    /// 1 for 1 January.
    pub day_of_year: u16,
    pub minute_of_day: u16,
    /// Local time minus UTC, in minutes.
    pub utc_offset_minutes: i32,
}

/// Where the sun is looked up: degrees north and east.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
}

/// The persisted choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScheduleConfig {
    /// Follow the sun when a location is known.
    pub follow_sun: bool,
    /// The fixed times: the custom schedule, and the fallback when the sun cannot be followed.
    pub fixed: DaySchedule,
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            follow_sun: true,
            fixed: DaySchedule::FIXED,
        }
    }
}

impl ScheduleConfig {
    /// Read the choice from an `appearance` section; missing or malformed values take the
    /// defaults.
    pub fn from_section(section: &Map<String, Value>) -> Self {
        let time = |key: &str, fallback: u16| {
            section
                .get(key)
                .and_then(Value::as_str)
                .and_then(parse_hhmm)
                .unwrap_or(fallback)
        };
        Self {
            follow_sun: section.get(SCHEDULE_KEY).and_then(Value::as_str) != Some("custom"),
            fixed: DaySchedule {
                sunrise: time(SUNRISE_KEY, DaySchedule::FIXED.sunrise),
                sunset: time(SUNSET_KEY, DaySchedule::FIXED.sunset),
            },
        }
    }

    /// Today's schedule at `moment`, given where the system time zone puts us.
    pub fn resolve(self, location: Option<Location>, moment: LocalMoment) -> DaySchedule {
        if self.follow_sun
            && let Some(location) = location
            && let Some(sun) = sun_times(location, moment.day_of_year, moment.utc_offset_minutes)
        {
            return sun;
        }
        self.fixed
    }
}

/// The schedule plus the location it follows, resolved once when settings change so the
/// per-frame question "is it night now?" is only arithmetic.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scheduled {
    pub config: ScheduleConfig,
    pub location: Option<Location>,
}

impl Scheduled {
    /// The schedule in `settings` (the whole document), at the system time zone's location.
    pub fn from_settings(settings: &Map<String, Value>) -> Self {
        let section = settings
            .get(SECTION)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        Self {
            config: ScheduleConfig::from_section(&section),
            location: timezone_location(),
        }
    }

    pub fn is_night(&self, moment: LocalMoment) -> bool {
        self.config
            .resolve(self.location, moment)
            .is_night(moment.minute_of_day)
    }
}

/// Whether `theme` (the `appearance.theme` value) asks for light by day and dark by night.
pub fn is_automatic(theme: &str) -> bool {
    theme == AUTOMATIC_THEME
}

/// Parse `HH:MM` (24-hour) to minutes after midnight.
pub fn parse_hhmm(value: &str) -> Option<u16> {
    let (hours, minutes) = value.trim().split_once(':')?;
    if minutes.len() != 2 {
        return None;
    }
    let hours: u16 = hours.parse().ok()?;
    let minutes: u16 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then(|| hours.saturating_mul(60).saturating_add(minutes))
}

/// Format minutes after midnight as `HH:MM`.
pub fn format_hhmm(minute_of_day: u16) -> String {
    let minute_of_day = minute_of_day % (24 * 60);
    format!("{:02}:{:02}", minute_of_day / 60, minute_of_day % 60)
}

/// Sunrise and sunset at `location` on `day_of_year`, in local minutes for `utc_offset_minutes`.
/// `None` during polar day or night, when the sun does not cross the horizon.
///
/// NOAA's low-precision solar position (equation of time and declination from the fractional
/// year), with the standard −0.833° horizon for refraction and the solar disc: within a few
/// minutes of the precise value, which is all a theme switch needs.
pub fn sun_times(
    location: Location,
    day_of_year: u16,
    utc_offset_minutes: i32,
) -> Option<DaySchedule> {
    use std::f64::consts::PI;
    let gamma = 2.0 * PI / 365.0 * (f64::from(day_of_year) - 1.0);
    let equation_of_time = 229.18
        * (0.000_075 + 0.001_868 * gamma.cos()
            - 0.032_077 * gamma.sin()
            - 0.014_615 * (2.0 * gamma).cos()
            - 0.040_849 * (2.0 * gamma).sin());
    let declination = 0.006_918 - 0.399_912 * gamma.cos() + 0.070_257 * gamma.sin()
        - 0.006_758 * (2.0 * gamma).cos()
        + 0.000_907 * (2.0 * gamma).sin()
        - 0.002_697 * (3.0 * gamma).cos()
        + 0.001_48 * (3.0 * gamma).sin();
    let latitude = location.latitude.to_radians();
    let cos_hour_angle = 90.833_f64.to_radians().cos() / (latitude.cos() * declination.cos())
        - latitude.tan() * declination.tan();
    if !(-1.0..=1.0).contains(&cos_hour_angle) {
        return None;
    }
    let hour_angle = cos_hour_angle.acos().to_degrees();
    let local = |utc_minutes: f64| -> Option<u16> {
        if !utc_minutes.is_finite() {
            return None;
        }
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a finite sunrise/sunset is within a day or two of noon, far inside i32"
        )]
        let minutes = (utc_minutes.round() as i32)
            .saturating_add(utc_offset_minutes)
            .rem_euclid(MINUTES_PER_DAY);
        u16::try_from(minutes).ok()
    };
    Some(DaySchedule {
        sunrise: local(720.0 - 4.0 * (location.longitude + hour_angle) - equation_of_time)?,
        sunset: local(720.0 - 4.0 * (location.longitude - hour_angle) - equation_of_time)?,
    })
}

/// The location tzdata publishes for the system time zone, if any.
pub fn timezone_location() -> Option<Location> {
    let zone = system_timezone()?;
    [
        "/usr/share/zoneinfo/zone1970.tab",
        "/usr/share/zoneinfo/zone.tab",
    ]
    .iter()
    .find_map(|table| std::fs::read_to_string(table).ok())
    .and_then(|table| timezone_location_in(&zone, &table))
}

/// The IANA name of the system time zone: `$TZ` when it names one, else the `/etc/localtime`
/// link target below `zoneinfo/`.
fn system_timezone() -> Option<String> {
    if let Some(tz) = std::env::var_os("TZ").and_then(|v| v.into_string().ok()) {
        let tz = tz.trim_start_matches(':');
        if tz.contains('/') && !tz.starts_with('/') {
            return Some(tz.to_string());
        }
    }
    zone_from_link(&std::fs::read_link(Path::new("/etc/localtime")).ok()?)
}

fn zone_from_link(target: &Path) -> Option<String> {
    let text = target.to_str()?;
    let (_, zone) = text.split_once("zoneinfo/")?;
    (!zone.is_empty()).then(|| zone.to_string())
}

/// Look `zone` up in a `zone1970.tab`/`zone.tab` body.
pub fn timezone_location_in(zone: &str, table: &str) -> Option<Location> {
    table
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let mut columns = line.split('\t');
            let _countries = columns.next()?;
            let coordinate = columns.next()?;
            (columns.next()? == zone).then_some(coordinate)
        })
        .and_then(parse_iso6709)
}

/// Parse the compact ISO 6709 latitude+longitude form tzdata uses: `+DDMM+DDDMM` or
/// `+DDMMSS+DDDMMSS` (and negative equivalents).
pub fn parse_iso6709(value: &str) -> Option<Location> {
    let split = value
        .char_indices()
        .skip(1)
        .find_map(|(i, c)| matches!(c, '+' | '-').then_some(i))?;
    Some(Location {
        latitude: parse_coordinate_part(value.get(..split)?, true)?,
        longitude: parse_coordinate_part(value.get(split..)?, false)?,
    })
}

fn parse_coordinate_part(value: &str, latitude: bool) -> Option<f64> {
    let sign = match value.as_bytes().first().copied()? {
        b'+' => 1.0,
        b'-' => -1.0,
        _ => return None,
    };
    let digits = value.get(1..)?;
    // DDMM[SS] for latitude, DDDMM[SS] for longitude.
    let (degrees_end, minutes_end, with_seconds) = match (latitude, digits.len()) {
        (true, 4) => (2, 4, false),
        (true, 6) => (2, 4, true),
        (false, 5) => (3, 5, false),
        (false, 7) => (3, 5, true),
        _ => return None,
    };
    let degrees: f64 = digits.get(..degrees_end)?.parse().ok()?;
    let minutes: f64 = digits.get(degrees_end..minutes_end)?.parse().ok()?;
    let seconds: f64 = if with_seconds {
        digits.get(minutes_end..)?.parse().ok()?
    } else {
        0.0
    };
    let limit = if latitude { 90.0 } else { 180.0 };
    if degrees > limit || minutes >= 60.0 || seconds >= 60.0 {
        return None;
    }
    Some(sign * (degrees + minutes / 60.0 + seconds / 3600.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BERLIN: Location = Location {
        latitude: 52.5,
        longitude: 13.366_666,
    };

    fn at(day_of_year: u16, hhmm: &str, utc_offset_minutes: i32) -> LocalMoment {
        LocalMoment {
            day_of_year,
            minute_of_day: parse_hhmm(hhmm).unwrap(),
            utc_offset_minutes,
        }
    }

    fn near(actual: u16, expected: &str) {
        let expected = parse_hhmm(expected).unwrap();
        assert!(
            actual.abs_diff(expected) <= 5,
            "{} is not within five minutes of {}",
            format_hhmm(actual),
            format_hhmm(expected)
        );
    }

    #[test]
    fn berlin_sunrise_and_sunset_match_the_almanac() {
        // 21 June, CEST: sunrise 04:43, sunset 21:33.
        let summer = sun_times(BERLIN, 172, 120).unwrap();
        near(summer.sunrise, "04:43");
        near(summer.sunset, "21:33");
        // 21 December, CET: sunrise 08:15, sunset 15:54.
        let winter = sun_times(BERLIN, 355, 60).unwrap();
        near(winter.sunrise, "08:15");
        near(winter.sunset, "15:54");
    }

    #[test]
    fn polar_day_has_no_sunset_and_falls_back_to_fixed_times() {
        let tromso = Location {
            latitude: 69.65,
            longitude: 18.96,
        };
        assert_eq!(sun_times(tromso, 172, 120), None);
        let schedule = ScheduleConfig::default().resolve(Some(tromso), at(172, "12:00", 120));
        assert_eq!(schedule, DaySchedule::FIXED);
    }

    #[test]
    fn without_a_location_the_fixed_seven_to_seven_applies() {
        let config = ScheduleConfig::default();
        let schedule = config.resolve(None, at(100, "12:00", 0));
        assert_eq!(format_hhmm(schedule.sunrise), "07:00");
        assert_eq!(format_hhmm(schedule.sunset), "19:00");
        assert!(schedule.is_night(parse_hhmm("06:59").unwrap()));
        assert!(!schedule.is_night(parse_hhmm("07:00").unwrap()));
        assert!(!schedule.is_night(parse_hhmm("18:59").unwrap()));
        assert!(schedule.is_night(parse_hhmm("19:00").unwrap()));
    }

    #[test]
    fn custom_times_win_over_the_sun_and_may_wrap_midnight() {
        let mut section = Map::new();
        section.insert(SCHEDULE_KEY.into(), "custom".into());
        section.insert(SUNRISE_KEY.into(), "08:30".into());
        section.insert(SUNSET_KEY.into(), "21:15".into());
        let config = ScheduleConfig::from_section(&section);
        let schedule = config.resolve(Some(BERLIN), at(172, "12:00", 120));
        assert_eq!(schedule.sunrise, parse_hhmm("08:30").unwrap());
        assert_eq!(schedule.sunset, parse_hhmm("21:15").unwrap());

        // A night-shift schedule: "day" from 22:00 to 06:00.
        let inverted = DaySchedule {
            sunrise: parse_hhmm("22:00").unwrap(),
            sunset: parse_hhmm("06:00").unwrap(),
        };
        assert!(!inverted.is_night(parse_hhmm("23:00").unwrap()));
        assert!(inverted.is_night(parse_hhmm("12:00").unwrap()));
    }

    #[test]
    fn malformed_settings_take_the_defaults() {
        let mut section = Map::new();
        section.insert(SUNRISE_KEY.into(), "7am".into());
        section.insert(SUNSET_KEY.into(), Value::from(19));
        assert_eq!(
            ScheduleConfig::from_section(&section),
            ScheduleConfig::default()
        );
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("07:60"), None);
        assert_eq!(format_hhmm(parse_hhmm("7:05").unwrap()), "07:05");
    }

    #[test]
    fn the_time_zone_names_a_location_in_tzdata() {
        let table = "# comment\nDE,DK,NO,SE,SJ\t+5230+01322\tEurope/Berlin\n\
                     AU\t-3352+15113\tAustralia/Sydney\tNew South Wales\n";
        let berlin = timezone_location_in("Europe/Berlin", table).unwrap();
        assert!((berlin.latitude - 52.5).abs() < 1e-9);
        assert!((berlin.longitude - 13.366_666).abs() < 1e-3);
        let sydney = timezone_location_in("Australia/Sydney", table).unwrap();
        assert!(sydney.latitude < 0.0);
        assert_eq!(timezone_location_in("Mars/Olympus", table), None);
        assert_eq!(
            zone_from_link(Path::new("../usr/share/zoneinfo/Europe/Berlin")).as_deref(),
            Some("Europe/Berlin")
        );
    }

    #[test]
    fn automatic_theme_follows_the_schedule() {
        let scheduled = Scheduled {
            config: ScheduleConfig::default(),
            location: Some(BERLIN),
        };
        assert!(!scheduled.is_night(at(172, "12:00", 120)));
        assert!(scheduled.is_night(at(172, "23:00", 120)));
        assert!(
            scheduled.is_night(at(355, "17:00", 60)),
            "dark before five in winter"
        );
        assert!(is_automatic("Automatic"));
        assert!(!is_automatic("Dark"));
    }
}
