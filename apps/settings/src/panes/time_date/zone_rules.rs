//! How far a time zone's clocks are from UTC at an instant, read from the
//! installed tzdata (`/usr/share/zoneinfo`, RFC 8536 `TZif` files).
//!
//! The map and the nearby-cities list show each zone's time now. The toolkit's
//! clock knows only this device's zone; the other zones' rules are the same
//! files timedated sets the device to, read once off the UI thread, so a
//! city's time is a lookup, never a guess from its longitude.

use std::path::Path;

/// One zone's offsets from UTC: its recorded transitions, and the rule its
/// clocks follow after the last one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneRules {
    /// Instants (Unix seconds) at which the offset changes, ascending.
    transitions: Vec<i64>,
    /// For each transition, the index into `offsets` it changes to.
    kinds: Vec<u8>,
    /// The zone's offsets from UTC in seconds, east positive.
    offsets: Vec<i32>,
    /// The POSIX rule after the last transition, when the file has one.
    rule: Option<PosixRule>,
}

impl ZoneRules {
    /// The rules of `zone` (an IANA name such as `Europe/Vienna`), from the
    /// installed tzdata; `None` when the name is not a plain zone path or
    /// the file is missing or not `TZif`.
    pub fn read(zone: &str) -> Option<Self> {
        Self::read_in(Path::new("/usr/share/zoneinfo"), zone)
    }

    fn read_in(root: &Path, zone: &str) -> Option<Self> {
        let plain = !zone.is_empty()
            && zone.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+'))
            });
        if !plain {
            return None;
        }
        Self::parse(&std::fs::read(root.join(zone)).ok()?)
    }

    /// Parse a `TZif` file (versions 1 to 4).
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let first = Header::parse(bytes)?;
        if first.version == 0 {
            let (rules, _) = Self::parse_block(bytes.get(HEADER_LEN..)?, &first, 4)?;
            return Some(rules);
        }
        // Version 2 and later repeat the data with 64-bit times after the
        // version-1 block, then end with the POSIX rule.
        let rest = bytes.get(HEADER_LEN + first.block_len(4)?..)?;
        let second = Header::parse(rest)?;
        let (mut rules, used) = Self::parse_block(rest.get(HEADER_LEN..)?, &second, 8)?;
        let footer = rest.get(HEADER_LEN + used..)?;
        let footer = footer.strip_prefix(b"\n")?;
        let end = footer.iter().position(|byte| *byte == b'\n')?;
        let text = std::str::from_utf8(footer.get(..end)?).ok()?;
        rules.rule = if text.is_empty() {
            None
        } else {
            Some(PosixRule::parse(text)?)
        };
        Some(rules)
    }

    fn parse_block(data: &[u8], header: &Header, time_len: usize) -> Option<(Self, usize)> {
        let mut at = 0;
        let mut take = |len: usize| {
            let part = data.get(at..at + len);
            at += len;
            part
        };
        let times = take(header.timecnt * time_len)?;
        let kinds = take(header.timecnt)?.to_vec();
        let infos = take(header.typecnt * 6)?;
        let transitions = times
            .chunks_exact(time_len)
            .map(|chunk| match *chunk {
                [a, b, c, d] => i64::from(i32::from_be_bytes([a, b, c, d])),
                [a, b, c, d, e, f, g, h] => i64::from_be_bytes([a, b, c, d, e, f, g, h]),
                _ => 0,
            })
            .collect();
        let offsets: Vec<i32> = infos
            .chunks_exact(6)
            .filter_map(|info| match *info {
                [a, b, c, d, ..] => Some(i32::from_be_bytes([a, b, c, d])),
                _ => None,
            })
            .collect();
        if offsets.is_empty() || kinds.iter().any(|kind| usize::from(*kind) >= offsets.len()) {
            return None;
        }
        let used = header.block_len(time_len)?;
        Some((
            Self {
                transitions,
                kinds,
                offsets,
                rule: None,
            },
            used,
        ))
    }

    /// Rules of one fixed offset, for tests that must not depend on the
    /// tzdata installed where they run.
    #[cfg(test)]
    pub(crate) fn fixed(offset: i32) -> Self {
        Self {
            transitions: Vec::new(),
            kinds: Vec::new(),
            offsets: vec![offset],
            rule: None,
        }
    }

    /// The zone's offset from UTC in seconds (east positive) at `unix`.
    pub fn offset_at(&self, unix: i64) -> i32 {
        let after_last = self.transitions.last().is_none_or(|last| unix >= *last);
        if after_last && let Some(rule) = &self.rule {
            return rule.offset_at(unix);
        }
        // `partition_point` counts the transitions at or before `unix`.
        let passed = self.transitions.partition_point(|at| *at <= unix);
        let kind = passed
            .checked_sub(1)
            .and_then(|index| self.kinds.get(index))
            .map_or(0, |kind| usize::from(*kind));
        self.offsets
            .get(kind)
            .or_else(|| self.offsets.first())
            .copied()
            .unwrap_or_default()
    }
}

const HEADER_LEN: usize = 44;

/// A `TZif` header's version and counts.
struct Header {
    version: u8,
    isutcnt: usize,
    isstdcnt: usize,
    leapcnt: usize,
    timecnt: usize,
    typecnt: usize,
    charcnt: usize,
}

impl Header {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..HEADER_LEN)?;
        if header.get(..4)? != b"TZif" {
            return None;
        }
        let version = match *header.get(4)? {
            0 => 0,
            digit @ b'2'..=b'9' => digit - b'0',
            _ => return None,
        };
        let count = |index: usize| -> Option<usize> {
            let at = 20 + index * 4;
            match *header.get(at..at + 4)? {
                [a, b, c, d] => usize::try_from(u32::from_be_bytes([a, b, c, d])).ok(),
                _ => None,
            }
        };
        Some(Self {
            version,
            isutcnt: count(0)?,
            isstdcnt: count(1)?,
            leapcnt: count(2)?,
            timecnt: count(3)?,
            typecnt: count(4)?,
            charcnt: count(5)?,
        })
    }

    /// The data block's length with `time_len`-byte times.
    fn block_len(&self, time_len: usize) -> Option<usize> {
        self.timecnt
            .checked_mul(time_len + 1)?
            .checked_add(self.typecnt.checked_mul(6)?)?
            .checked_add(self.charcnt)?
            .checked_add(self.leapcnt.checked_mul(time_len + 4)?)?
            .checked_add(self.isstdcnt)?
            .checked_add(self.isutcnt)
    }
}

/// A POSIX TZ rule (`CET-1CEST,M3.5.0,M10.5.0/3`): a standard offset, and
/// optionally a daylight-saving offset with the dates it starts and ends.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PosixRule {
    /// Standard time's offset from UTC, east positive.
    standard: i32,
    daylight: Option<Daylight>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Daylight {
    offset: i32,
    start: (Date, i32),
    end: (Date, i32),
}

/// When in a year a rule applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Date {
    /// `Jn`: day 1 to 365, February 29 never counted.
    Julian(u16),
    /// `n`: day 0 to 365, February 29 counted.
    Ordinal(u16),
    /// `Mm.w.d`: weekday `d` (0 Sunday) of week `w` (5 the last) of month `m`.
    Weekday { month: u8, week: u8, day: u8 },
}

impl PosixRule {
    fn parse(text: &str) -> Option<Self> {
        let mut rest = text;
        skip_name(&mut rest)?;
        // POSIX offsets count west positive; ours count east positive.
        let standard = -parse_duration(&mut rest)?;
        if rest.is_empty() {
            return Some(Self {
                standard,
                daylight: None,
            });
        }
        skip_name(&mut rest)?;
        let offset = if rest.starts_with(',') {
            standard + 3600
        } else {
            -parse_duration(&mut rest)?
        };
        let start = parse_transition(&mut rest)?;
        let end = parse_transition(&mut rest)?;
        rest.is_empty().then_some(Self {
            standard,
            daylight: Some(Daylight { offset, start, end }),
        })
    }

    fn offset_at(&self, unix: i64) -> i32 {
        let Some(daylight) = &self.daylight else {
            return self.standard;
        };
        let year = civil_year(unix + i64::from(self.standard));
        // Each transition happens at its local time in the offset in force
        // just before it: standard before the start, daylight before the end.
        let instant = |(date, time): (Date, i32), before: i32| {
            (days_from_civil(year, 1, 1) + date.day_of_year(year)) * 86_400 + i64::from(time)
                - i64::from(before)
        };
        let start = instant(daylight.start, self.standard);
        let end = instant(daylight.end, daylight.offset);
        let in_daylight = if start <= end {
            (start..end).contains(&unix)
        } else {
            // The southern hemisphere: daylight time spans the new year.
            !(end..start).contains(&unix)
        };
        if in_daylight {
            daylight.offset
        } else {
            self.standard
        }
    }
}

impl Date {
    /// Days after January 1 of `year`.
    fn day_of_year(self, year: i64) -> i64 {
        let leap = is_leap(year);
        match self {
            Date::Julian(day) => {
                let day = i64::from(day.clamp(1, 365));
                day - 1 + i64::from(leap && day >= 60)
            }
            Date::Ordinal(day) => i64::from(day.min(365)),
            Date::Weekday { month, week, day } => {
                let month = i64::from(month.clamp(1, 12));
                let first = days_from_civil(year, month, 1);
                // 1970-01-01 was a Thursday.
                let weekday = (first + 4).rem_euclid(7);
                let mut date = (i64::from(day) - weekday).rem_euclid(7) + (i64::from(week) - 1) * 7;
                let length = month_length(year, month);
                while date >= length {
                    date -= 7;
                }
                first + date - days_from_civil(year, 1, 1)
            }
        }
    }
}

/// Skip a zone abbreviation: letters, or anything inside `<>`.
fn skip_name(rest: &mut &str) -> Option<()> {
    if let Some(quoted) = rest.strip_prefix('<') {
        let end = quoted.find('>')?;
        *rest = quoted.get(end + 1..)?;
        return Some(());
    }
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    if end < 3 {
        return None;
    }
    *rest = rest.get(end..)?;
    Some(())
}

/// `[+-]h[:mm[:ss]]` in seconds (hours up to 167, as version 3 allows).
fn parse_duration(rest: &mut &str) -> Option<i32> {
    let mut sign = 1;
    if let Some(after) = rest.strip_prefix('-') {
        sign = -1;
        *rest = after;
    } else if let Some(after) = rest.strip_prefix('+') {
        *rest = after;
    }
    let mut total = 0_i32;
    for (index, scale) in [3600, 60, 1].into_iter().enumerate() {
        if index > 0 {
            match rest.strip_prefix(':') {
                Some(after) => *rest = after,
                None => break,
            }
        }
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if end == 0 {
            return None;
        }
        let value: i32 = rest.get(..end)?.parse().ok()?;
        total += value * scale;
        *rest = rest.get(end..)?;
    }
    Some(sign * total)
}

/// `,date[/time]`; the time defaults to 02:00.
fn parse_transition(rest: &mut &str) -> Option<(Date, i32)> {
    *rest = rest.strip_prefix(',')?;
    let number = |rest: &mut &str| -> Option<u16> {
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let value = rest.get(..end)?.parse().ok()?;
        *rest = rest.get(end..)?;
        Some(value)
    };
    let date = if let Some(after) = rest.strip_prefix('J') {
        *rest = after;
        Date::Julian(number(rest)?)
    } else if let Some(after) = rest.strip_prefix('M') {
        *rest = after;
        let month = u8::try_from(number(rest)?).ok()?;
        *rest = rest.strip_prefix('.')?;
        let week = u8::try_from(number(rest)?).ok()?;
        *rest = rest.strip_prefix('.')?;
        let day = u8::try_from(number(rest)?).ok()?;
        if !(1..=12).contains(&month) || !(1..=5).contains(&week) || day > 6 {
            return None;
        }
        Date::Weekday { month, week, day }
    } else {
        Date::Ordinal(number(rest)?)
    };
    let time = match rest.strip_prefix('/') {
        Some(after) => {
            *rest = after;
            parse_duration(rest)?
        }
        None => 7200,
    };
    Some((date, time))
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn month_length(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil year a Unix instant falls in.
fn civil_year(unix: i64) -> i64 {
    let days = unix.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400;
    if month_index >= 10 { year + 1 } else { year }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A version-2 `TZif` file with `transitions` (instant, offset index)
    /// over `offsets`, ending in the POSIX `rule`.
    fn tzif(transitions: &[(i64, u8)], offsets: &[i32], rule: &str) -> Vec<u8> {
        fn header(version: u8, timecnt: usize, typecnt: usize, charcnt: usize) -> Vec<u8> {
            let mut out = b"TZif".to_vec();
            out.push(version);
            out.extend([0; 15]);
            for count in [0, 0, 0, timecnt, typecnt, charcnt] {
                out.extend(u32::try_from(count).unwrap().to_be_bytes());
            }
            out
        }
        let mut out = header(b'2', 0, 1, 4);
        // A version-1 block of one type, ignored by version-2 readers.
        out.extend(0_i32.to_be_bytes());
        out.extend([0, 0]);
        out.extend(b"UTC\0");
        out.extend(header(b'2', transitions.len(), offsets.len(), 4));
        for (at, _) in transitions {
            out.extend(at.to_be_bytes());
        }
        out.extend(transitions.iter().map(|(_, kind)| *kind));
        let standard = offsets.first().copied();
        for offset in offsets {
            out.extend(offset.to_be_bytes());
            out.extend([u8::from(Some(*offset) != standard), 0]);
        }
        out.extend(b"ABC\0");
        out.push(b'\n');
        out.extend(rule.as_bytes());
        out.push(b'\n');
        out
    }

    fn unix(year: i64, month: i64, day: i64, hour: i64) -> i64 {
        days_from_civil(year, month, day) * 86_400 + hour * 3600
    }

    #[test]
    fn central_europe_switches_at_one_utc_on_the_last_sundays() {
        let rules = ZoneRules::parse(&tzif(
            &[(unix(1996, 10, 27, 1), 0)],
            &[3600, 7200],
            "CET-1CEST,M3.5.0,M10.5.0/3",
        ))
        .unwrap();
        // 2026: daylight time from March 29 01:00 UTC to October 25 01:00 UTC.
        assert_eq!(rules.offset_at(unix(2026, 1, 15, 12)), 3600);
        assert_eq!(rules.offset_at(unix(2026, 3, 29, 0)), 3600);
        assert_eq!(rules.offset_at(unix(2026, 3, 29, 1)), 7200);
        assert_eq!(rules.offset_at(unix(2026, 7, 1, 12)), 7200);
        assert_eq!(rules.offset_at(unix(2026, 10, 25, 0)), 7200);
        assert_eq!(rules.offset_at(unix(2026, 10, 25, 1)), 3600);
    }

    #[test]
    fn recorded_transitions_answer_before_the_rule() {
        let rules = ZoneRules::parse(&tzif(
            &[(1000, 1), (2000, 0), (3000, 1)],
            &[0, 3600],
            "<+01>-1",
        ))
        .unwrap();
        assert_eq!(rules.offset_at(500), 0, "before the first: the first type");
        assert_eq!(rules.offset_at(1500), 3600);
        assert_eq!(rules.offset_at(2500), 0);
        assert_eq!(rules.offset_at(9000), 3600, "after the last: the rule");
    }

    #[test]
    fn the_southern_hemisphere_and_half_hours_and_quoted_names() {
        let adelaide = PosixRule::parse("ACST-9:30ACDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!(adelaide.offset_at(unix(2026, 1, 15, 12)), 37_800);
        assert_eq!(adelaide.offset_at(unix(2026, 7, 1, 12)), 34_200);
        let new_york = PosixRule::parse("EST5EDT,M3.2.0,M11.1.0").unwrap();
        assert_eq!(new_york.offset_at(unix(2026, 7, 1, 12)), -14_400);
        assert_eq!(new_york.offset_at(unix(2026, 12, 1, 12)), -18_000);
        let kolkata = PosixRule::parse("IST-5:30").unwrap();
        assert_eq!(kolkata.offset_at(0), 19_800);
        let quoted = PosixRule::parse("<-03>3").unwrap();
        assert_eq!(quoted.offset_at(0), -10_800);
    }

    #[test]
    fn only_plain_zone_names_are_read() {
        let root = Path::new("/nonexistent");
        for name in ["", "../etc/passwd", "Europe/../../x", "/etc/passwd", "a//b"] {
            assert!(ZoneRules::read_in(root, name).is_none(), "{name}");
        }
    }

    #[test]
    fn civil_dates_round_trip() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_year(unix(2024, 12, 31, 23)), 2024);
        assert_eq!(civil_year(unix(2025, 1, 1, 0)), 2025);
        assert_eq!(civil_year(-1), 1969);
    }
}
