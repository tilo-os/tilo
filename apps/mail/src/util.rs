//! Date labels for the message list and reader header. Calendar arithmetic
//! comes from `ui::date`, so the app needs no `chrono`/`time` dependency.

use ui::date::civil_from_days;

/// `utc_min` (minutes since the epoch, UTC) on the local clock the person
/// reads, through `ui::clock` so a capture shows the pinned time.
pub fn local_minutes(utc_min: i64) -> i64 {
    let offset = ui::clock::local_now().offset().local_minus_utc();
    utc_min.saturating_add(i64::from(offset) / 60)
}

/// The message-list column: the toolkit's list style (`09:42` today,
/// `Yesterday`, `Mon` this week, `Jun 24`, `Jun 24, 2025`), both arguments
/// minutes since the epoch on the local clock ([`local_minutes`]).
pub fn format_relative(ts_min: i64, now_min: i64) -> String {
    match (wall(ts_min), wall(now_min)) {
        (Some(at), Some(now)) => ui::format::Formats::current().list_date(at, now),
        _ => String::new(),
    }
}

/// The reader header: the toolkit's timestamp style, in the person's date
/// and clock formats. `ts_min` is minutes since the epoch on the local clock.
pub fn format_absolute(ts_min: i64) -> String {
    wall(ts_min).map_or_else(String::new, |at| {
        ui::format::Formats::current().timestamp(at)
    })
}

/// Local minutes since the epoch as a wall-clock date and time.
fn wall(local_min: i64) -> Option<ui::format::NaiveDateTime> {
    let day = local_min.div_euclid(1440);
    let mins = local_min.rem_euclid(1440);
    let (y, mo, d) = civil_from_days(day);
    ui::format::NaiveDate::from_ymd_opt(i32::try_from(y).ok()?, mo, d)?.and_hms_opt(
        ui::num::u32_of_i64(mins / 60),
        ui::num::u32_of_i64(mins % 60),
        0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::date::days_from_civil;

    /// Epoch-minutes for a Y/M/D H:M (UTC).
    fn minutes_of(y: i64, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        days_from_civil(y, mo, d)
            .saturating_mul(1440)
            .saturating_add(i64::from(h).saturating_mul(60))
            .saturating_add(i64::from(mi))
    }

    #[test]
    fn date_helpers_compute_weekdays_relative_and_absolute_labels() {
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
            assert_eq!(format_relative(minutes_of(2026, 6, 28, 9, 30), now), "Sun");
            assert_eq!(
                format_relative(minutes_of(2026, 6, 20, 9, 30), now),
                "Jun 20"
            );
            assert_eq!(
                format_relative(minutes_of(2025, 6, 20, 9, 30), now),
                "Jun 20, 2025"
            );
        }

        // Phase `absolute_format`.
        {
            let s = format_absolute(minutes_of(2026, 6, 30, 13, 42));
            assert_eq!(s, "2026-06-30 13:42");
        }
    }
}
