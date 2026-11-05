//! Lengths of time, as literals write them: `7d`, `-30d`, `5m` (search.md, "Types, references and
//! paths"). A duration is a value of its own: a moment or a day moves by it, two moments or two
//! days apart are one, and a filter resolves it against the time it is asked at.

/// The units a duration is written in, and how many seconds each is.
pub const UNITS: &[(&str, i64)] = &[
    ("s", 1),
    ("m", 60),
    ("h", 3600),
    ("d", 86_400),
    ("w", 604_800),
];

/// The seconds `amount` of `unit` make; `None` for a unit that is none of [`UNITS`] or an
/// amount too large.
#[must_use]
pub fn seconds(amount: i64, unit: &str) -> Option<i64> {
    let (_, each) = UNITS.iter().find(|(u, _)| *u == unit)?;
    amount.checked_mul(*each)
}

/// The seconds `secs` reads as, in the largest unit that divides it: `90s`, `30m`, `2h`, `3d`,
/// `1w`; `0s` for none.
#[must_use]
pub fn text(secs: i64) -> String {
    if secs == 0 {
        return "0s".to_owned();
    }
    let sign = if secs < 0 { "-" } else { "" };
    let secs = secs.unsigned_abs();
    let (amount, unit) = UNITS
        .iter()
        .rev()
        .find(|(_, each)| secs.is_multiple_of(u64::try_from(*each).unwrap_or(1)))
        .map_or((secs, "s"), |(unit, each)| {
            (secs / u64::try_from(*each).unwrap_or(1), *unit)
        });
    format!("{sign}{amount}{unit}")
}

/// `-7d` as `(-7, "d")`; `None` when `text` is not a duration.
#[must_use]
pub fn parse(text: &str) -> Option<(i64, &'static str)> {
    let digits = text.strip_prefix(['-', '+']).unwrap_or(text);
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    let (number, unit) = digits.split_at(end);
    let (unit, _) = UNITS.iter().find(|(u, _)| *u == unit)?;
    if number.is_empty() {
        return None;
    }
    let amount: i64 = number.parse().ok()?;
    Some((
        if text.starts_with('-') {
            -amount
        } else {
            amount
        },
        unit,
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_duration_reads_as_an_amount_of_a_unit() {
        assert_eq!(super::parse("-7d"), Some((-7, "d")));
        assert_eq!(super::parse("5m"), Some((5, "m")));
        assert_eq!(super::parse("+2w"), Some((2, "w")));
        assert_eq!(super::parse("7"), None);
        assert_eq!(super::parse("d"), None);
        assert_eq!(super::parse("7x"), None);
        assert_eq!(super::seconds(-7, "d"), Some(-604_800));
    }

    #[test]
    fn a_length_of_time_reads_in_the_largest_unit_that_divides_it() {
        assert_eq!(super::text(90), "90s");
        assert_eq!(super::text(1800), "30m");
        assert_eq!(super::text(-7200), "-2h");
        assert_eq!(super::text(259_200), "3d");
        assert_eq!(super::text(604_800), "1w");
        assert_eq!(super::text(0), "0s");
    }
}
