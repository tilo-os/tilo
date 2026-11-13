//! The IANA time-zone database, as its own library in the runtime bundle
//! (`libtilo_timezone_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use chrono_tz;

/// Run the database's own code: look zones up by name and convert fixed
/// instants through their rules, in and out of daylight saving time and at
/// a half-hour offset.
///
/// The engine's check binary and the app's `--probe-…` call it, so a release
/// proves the library loads and runs against its runtime.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use chrono::{Offset, TimeZone, Utc};
    use chrono_tz::{TZ_VARIANTS, Tz};

    let cases: [(&str, i32, u32, u32, i32); 4] = [
        ("Europe/Vienna", 2026, 1, 15, 3600),
        ("Europe/Vienna", 2026, 7, 1, 7200),
        ("America/New_York", 2026, 7, 1, -14_400),
        ("Australia/Adelaide", 2026, 1, 15, 37_800),
    ];
    for (name, year, month, day, expected) in cases {
        let zone: Tz = name
            .parse()
            .map_err(|error| format!("chrono-tz does not know {name}: {error}"))?;
        let instant = Utc
            .with_ymd_and_hms(year, month, day, 12, 0, 0)
            .single()
            .ok_or_else(|| format!("{year}-{month:02}-{day:02} is not a date"))?;
        let offset = zone
            .offset_from_utc_datetime(&instant.naive_utc())
            .fix()
            .local_minus_utc();
        if offset != expected {
            return Err(format!(
                "chrono-tz puts {name} {offset} s from UTC on {year}-{month:02}-{day:02}, not {expected} s"
            ));
        }
    }
    Ok(format!(
        "{} zones, {} instants converted",
        TZ_VARIANTS.len(),
        cases.len()
    ))
}
