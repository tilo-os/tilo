//! The IANA time-zone database, as its own library in the runtime bundle
//! (`libtilo_timezone_engine.so`).
//!
//! Like `runtime-dylib` it exports what it provides, which records it
//! as provided by this library for the apps' `-C prefer-dynamic` link.
//! Outside the thin build it is empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// The database's canonical spelling of the zone named `name`, in any letter
/// case.
#[cfg(feature = "thin")]
#[must_use]
pub fn canonical(name: &str) -> Option<&'static str> {
    jiff_tzdb::get(name).map(|(name, _)| name)
}

/// Every zone name in the database.
#[cfg(feature = "thin")]
pub fn available() -> impl Iterator<Item = &'static str> {
    jiff_tzdb::available()
}

/// The zone named `name`, from the database built into Tilo, so every device
/// resolves a stored wall time the same way, whatever zoneinfo it has.
#[cfg(feature = "thin")]
#[must_use]
pub fn named(name: &str) -> Option<jiff::tz::TimeZone> {
    let (name, data) = jiff_tzdb::get(name)?;
    jiff::tz::TimeZone::tzif(name, data).ok()
}

/// Run the database's own code: look zones up by name and convert fixed
/// instants through their rules, in and out of daylight saving time and at
/// a half-hour offset.
///
/// The engine's check binary and the app's `--probe-…` call it, so a release
/// proves the library loads and runs against its runtime.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    let cases: [(&str, i16, i8, i8, i32); 4] = [
        ("Europe/Vienna", 2026, 1, 15, 3600),
        ("Europe/Vienna", 2026, 7, 1, 7200),
        ("America/New_York", 2026, 7, 1, -14_400),
        ("Australia/Adelaide", 2026, 1, 15, 37_800),
    ];
    for (name, year, month, day, expected) in cases {
        let zone = named(name).ok_or_else(|| format!("the database does not know {name}"))?;
        let instant = date(year, month, day)
            .at(12, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .map_err(|error| format!("{year}-{month:02}-{day:02}: {error}"))?
            .timestamp();
        let offset = zone.to_offset(instant).seconds();
        if offset != expected {
            return Err(format!(
                "the database puts {name} {offset} s from UTC on {year}-{month:02}-{day:02}, not {expected} s"
            ));
        }
    }
    Ok(format!(
        "{} zones, {} instants converted",
        available().count(),
        cases.len()
    ))
}
