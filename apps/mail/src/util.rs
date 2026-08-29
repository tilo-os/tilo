//! Small dependency-free date helpers. We deliberately avoid pulling
//! `chrono`/`time` (extra build weight) — the app only needs civil-date math
//! for relative timestamps in the message list and absolute dates in the
//! reader header. Both use Howard Hinnant's well-known `days_from_civil` /
//! `civil_from_days` algorithms.

/// Days since the Unix epoch for a proleptic-Gregorian Y/M/D.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * i64::from(if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: (year, month, day) for a day count.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Epoch-minutes for a Y/M/D H:M (UTC).
pub fn minutes_of(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
    days_from_civil(y, mo, d) * 1440 + i64::from(h) * 60 + i64::from(mi)
}

/// Compact relative timestamp for the message-list column:
/// `09:42` (today), `Yesterday`, `Mon` (this week), or `Jun 24`.
pub fn format_relative(ts_min: i64, now_min: i64) -> String {
    let delta = now_min - ts_min;
    let today = now_min.div_euclid(1440);
    let that_day = ts_min.div_euclid(1440);
    let day_diff = today - that_day;

    if day_diff == 0 {
        // Today → clock time.
        let mins = ts_min.rem_euclid(1440);
        format!("{:02}:{:02}", mins / 60, mins % 60)
    } else if day_diff == 1 {
        "Yesterday".to_string()
    } else if (0..7).contains(&day_diff) {
        weekday(that_day).to_string()
    } else {
        let (_, mo, d) = civil_from_days(that_day);
        format!("{} {}", MONTHS[(mo - 1) as usize], d)
    }
    .pipe(|s| if delta < 0 { s } else { s })
}

/// Full absolute timestamp for the reader header, e.g. `Jun 30, 2026, 13:42`.
pub fn format_absolute(ts_min: i64) -> String {
    let day = ts_min.div_euclid(1440);
    let (y, mo, d) = civil_from_days(day);
    let mins = ts_min.rem_euclid(1440);
    format!(
        "{} {}, {}, {:02}:{:02}",
        MONTHS[(mo - 1) as usize],
        d,
        y,
        mins / 60,
        mins % 60
    )
}

/// Resolve the directory attachments are saved into. Honors
/// `QOS_MAIL_DOWNLOADS`, then `$HOME/Downloads`, then the system temp dir.
pub fn downloads_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("QOS_MAIL_DOWNLOADS")
        && !dir.is_empty() {
            return std::path::PathBuf::from(dir);
        }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty() {
            return std::path::Path::new(&home).join("Downloads");
        }
    std::env::temp_dir()
}

/// Write attachment `bytes` to the downloads directory under a sanitised
/// `filename` (final path component only, never an absolute/parent path), and
/// return the file it was written to. Real IO — this is the genuine "save
/// attachment" action, not a stub.
pub fn save_download(filename: &str, bytes: &[u8]) -> std::io::Result<std::path::PathBuf> {
    let safe = filename.rsplit(['/', '\\']).next().unwrap_or("attachment");
    let safe = if safe.trim().is_empty() {
        "attachment"
    } else {
        safe
    };
    let dir = downloads_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(safe);
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn weekday(day: i64) -> &'static str {
    // 1970-01-01 was a Thursday (index 4 if week starts Sun=0).
    const NAMES: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let idx = (day.rem_euclid(7) + 4).rem_euclid(7) as usize;
    NAMES[idx]
}

/// Tiny fluent-ish helper so `format_relative` reads cleanly.
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trips() {
        for &(y, m, d) in &[(1970, 1, 1), (2000, 2, 29), (2026, 6, 30), (1999, 12, 31)] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
        }
    }

    #[test]
    fn weekday_of_known_dates() {
        // 2026-06-30 is a Tuesday.
        assert_eq!(weekday(days_from_civil(2026, 6, 30)), "Tue");
        // 1970-01-01 is a Thursday.
        assert_eq!(weekday(days_from_civil(1970, 1, 1)), "Thu");
    }

    #[test]
    fn relative_labels() {
        let now = minutes_of(2026, 6, 30, 14, 0);
        assert_eq!(
            format_relative(minutes_of(2026, 6, 30, 9, 30), now),
            "09:30"
        );
        assert_eq!(
            format_relative(minutes_of(2026, 6, 29, 9, 30), now),
            "Yesterday"
        );
        assert_eq!(
            format_relative(minutes_of(2026, 6, 20, 9, 30), now),
            "Jun 20"
        );
    }

    #[test]
    fn absolute_format() {
        let s = format_absolute(minutes_of(2026, 6, 30, 13, 42));
        assert_eq!(s, "Jun 30, 2026, 13:42");
    }
}
