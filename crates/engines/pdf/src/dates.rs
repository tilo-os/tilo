//! The dates a PDF writes: `D:YYYYMMDDHHmmSSZ`, and how a reader's milliseconds since 1970
//! are one of them, in UTC.

/// The year, month and day for a count of days since 1970-01-01 (Howard Hinnant's civil
/// calendar), and back.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The moment a count of milliseconds since 1970 is, as a PDF date; empty for none.
#[must_use]
pub fn pdf_date(ms: i64) -> String {
    if ms <= 0 {
        return String::new();
    }
    let secs = ms.div_euclid(1_000);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// The milliseconds since 1970 a PDF date names, or nothing when it names no date.
#[must_use]
pub fn parse_pdf_date(date: &str) -> Option<i64> {
    let digits: String = date
        .strip_prefix("D:")
        .unwrap_or(date)
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let field = |from: usize, len: usize, default: i64| -> i64 {
        digits
            .get(from..from + len)
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    };
    if digits.len() < 4 {
        return None;
    }
    let (year, month, day) = (field(0, 4, 1970), field(4, 2, 1), field(6, 2, 1));
    let (hour, minute, second) = (field(8, 2, 0), field(10, 2, 0), field(12, 2, 0));
    let days = days_from_civil(year, month, day);
    Some(((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000)
}
