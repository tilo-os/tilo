//! Date labels for the message list and reader header. Calendar arithmetic
//! comes from `ui::date`, so the app needs no `chrono`/`time` dependency.

use ui::date::{civil_from_days, days_from_civil};

/// `utc_min` (minutes since the epoch, UTC) on the local clock the person
/// reads, through `ui::clock` so a capture shows the pinned time.
pub fn local_minutes(utc_min: i64) -> i64 {
    let offset = ui::clock::local_now().offset().local_minus_utc();
    utc_min.saturating_add(i64::from(offset) / 60)
}

/// Gravatar URL for an address. `d=404` makes the CDN return a 404 (not a
/// generic silhouette) when the address has no avatar, so the store settles
/// as failed and the initials stay.
pub fn gravatar_url(email: &str) -> String {
    let hash = hex::encode(<md5::Md5 as md5::Digest>::digest(
        email.trim().to_ascii_lowercase().as_bytes(),
    ));
    format!("https://www.gravatar.com/avatar/{hash}?s=96&d=404")
}

/// Epoch-minutes for a Y/M/D H:M (UTC).
pub fn minutes_of(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
    days_from_civil(y, mo, d)
        .saturating_mul(1440)
        .saturating_add(i64::from(h).saturating_mul(60))
        .saturating_add(i64::from(mi))
}

/// Compact relative timestamp for the message-list column:
/// `09:42` (today), `Yesterday`, `Mon` (this week), or `Jun 24`.
pub fn format_relative(ts_min: i64, now_min: i64) -> String {
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
        format!("{} {}", month_name(mo), d)
    }
}

/// Full absolute timestamp for the reader header, e.g. `Jun 30, 2026, 13:42`.
pub fn format_absolute(ts_min: i64) -> String {
    let day = ts_min.div_euclid(1440);
    let (y, mo, d) = civil_from_days(day);
    let mins = ts_min.rem_euclid(1440);
    format!(
        "{} {}, {}, {:02}:{:02}",
        month_name(mo),
        d,
        y,
        mins / 60,
        mins % 60
    )
}

fn weekday(day: i64) -> &'static str {
    ui::format::weekday_abbrev(usize::try_from(ui::date::weekday(day)).unwrap_or(0))
}

/// Short month name for a 1-based month number.
fn month_name(month: u32) -> &'static str {
    ui::format::month_abbrev(usize::try_from(month.saturating_sub(1)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn date_helpers_compute_weekdays_relative_and_absolute_labels() {
        // Phase `weekday_of_known_dates`.
        {
            // 2026-06-30 is a Tuesday.
            assert_eq!(weekday(days_from_civil(2026, 6, 30)), "Tue");
            // 1970-01-01 is a Thursday.
            assert_eq!(weekday(days_from_civil(1970, 1, 1)), "Thu");
        }

        // Phase `relative_labels`.
        {
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

        // Phase `gravatar_url_lowercases_and_trims`.
        {
            // Canonical Gravatar example hash for "MyEmailAddress@example.com".
            let u = gravatar_url("  MyEmailAddress@example.com ");
            assert!(u.contains("0bc83cb571cd1c50ba6f3e8a78ef1346"), "{u}");
        }

        // Phase `absolute_format`.
        {
            let s = format_absolute(minutes_of(2026, 6, 30, 13, 42));
            assert_eq!(s, "Jun 30, 2026, 13:42");
        }
    }
}
