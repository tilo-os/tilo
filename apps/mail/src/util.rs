//! Date labels for the message list and reader header. Calendar arithmetic
//! comes from `ui::date`, so the app needs no `chrono`/`time` dependency.

use ui::date::{civil_from_days, days_from_civil};

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

        // Phase `absolute_format`.
        {
            let s = format_absolute(minutes_of(2026, 6, 30, 13, 42));
            assert_eq!(s, "Jun 30, 2026, 13:42");
        }
    }
}
