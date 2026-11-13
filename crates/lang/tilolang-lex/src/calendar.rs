//! Calendar literals: a date `2026-10-24`, a month `2026-08`, an instant `2026-10-21T09:30:00Z`
//! and a wall-clock time of no zone, checked as the lexer reads one.

pub fn check_date(text: &str) -> Result<(), String> {
    let b = text.as_bytes();
    let digits = |r: std::ops::Range<usize>| -> Option<u32> {
        let part = text.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let shape = b.len() == 10 && b.get(4) == Some(&b'-') && b.get(7) == Some(&b'-');
    let (Some(year), Some(month), Some(day)) = (digits(0..4), digits(5..7), digits(8..10)) else {
        return Err(format!(
            "`{text}` is not a date: write it `YYYY-MM-DD`, such as `2026-10-24`"
        ));
    };
    if !shape {
        return Err(format!(
            "`{text}` is not a date: write it `YYYY-MM-DD`, such as `2026-10-24`"
        ));
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => {
            return Err(format!(
                "`{text}` has no month {month}; months are 01 to 12"
            ));
        }
    };
    if day == 0 || day > days {
        return Err(format!(
            "`{text}` has no day {day}; that month has {days} days"
        ));
    }
    Ok(())
}

pub fn check_time(text: &str) -> Result<(), String> {
    clock(text, Zone::Utc)
}

pub fn check_wall_clock(text: &str) -> Result<(), String> {
    clock(text, Zone::Person)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Zone {
    Utc,
    /// The person's own time of day; resolved to an instant where it is used.
    Person,
}

fn clock(text: &str, zone: Zone) -> Result<(), String> {
    let shape = || match zone {
        Zone::Utc => format!(
            "`{text}` is not a time: write it in UTC as `YYYY-MM-DDTHH:MM:SSZ`, such as `2026-10-21T09:30:00Z`"
        ),
        Zone::Person => format!(
            "`{text}` is not a wall-clock time: write it as `YYYY-MM-DDTHH:MM[:SS]`, such as `2026-10-21T09:30`"
        ),
    };
    let (Some(date), Some(clock)) = (text.get(..10), text.get(10..)) else {
        return Err(shape());
    };
    let b = clock.as_bytes();
    let ok = b.first() == Some(&b'T')
        && b.get(3) == Some(&b':')
        && b.get(6) == Some(&b':')
        && match zone {
            Zone::Utc => b.len() == 10 && b.get(9) == Some(&b'Z'),
            Zone::Person => matches!(b.len(), 6 | 9),
        };
    let part = |r: std::ops::Range<usize>| -> Option<u32> {
        let p = clock.get(r)?;
        p.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let sec = if b.len() == 9 { part(7..9) } else { Some(0) };
    let (Some(h), Some(m), Some(sec)) = (part(1..3), part(4..6), sec) else {
        return Err(shape());
    };
    if !ok {
        return Err(shape());
    }
    check_date(date)?;
    if h > 23 || m > 59 || sec > 59 {
        return Err(format!(
            "`{text}` is not a time of day: hours run 00 to 23, minutes and seconds 00 to 59"
        ));
    }
    Ok(())
}
