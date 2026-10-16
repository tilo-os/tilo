//! The IANA time-zone database, as an engine library beside the shared
//! runtime (`libtilo_timezone_engine.so`).
//!
//! Calendar and Clocks are the apps that name zones (`chrono-tz`): its
//! tables of every zone's rules are about 0.4 MB, much of it pointers the
//! loader relocates. Inside `libruntime_dylib.so` every app process mapped
//! and relocated them. Here they are a library of their own in the runtime
//! bundle that only those two apps name `DT_NEEDED`, and neither tapp
//! carries a copy; the runtime keeps `chrono` and `phf`, which other apps use.
//!
//! Like `runtime-dylib`, this crate re-exports the crate it provides, which
//! records it as provided by this library for the apps' `-C prefer-dynamic`
//! link. Outside the thin build (`thin`) it is empty: see the manifest.

// Load the runtime explicitly, as thin apps do: rustc ignores an `--extern`
// nothing names, and without it in the crate graph this library would link
// its own copy of every crate the runtime already provides, which the app's
// link then refuses (each crate may show up once).
#[cfg(feature = "thin")]
extern crate runtime_dylib;

#[cfg(feature = "thin")]
pub use chrono_tz;

/// Run the database's own code: look zones up by name and convert fixed
/// instants through their rules, in and out of daylight saving time and at
/// a half-hour offset.
///
/// The engine's check binary and the apps' `--probe-timezone-engine` call
/// it, so a release proves the library loads and runs against the runtime
/// it ships with, and that the apps load it themselves.
///
/// # Errors
/// Names the zone or instant the database answered wrongly.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use chrono::{Offset, TimeZone, Utc};
    use chrono_tz::{TZ_VARIANTS, Tz};

    // (zone, year, month, day, the zone's offset from UTC in seconds at noon UTC)
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
