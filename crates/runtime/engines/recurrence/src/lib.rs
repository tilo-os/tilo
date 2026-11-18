//! The RFC 5545 recurrence expansion, as its own library in the runtime
//! bundle (`libtilo_recurrence_engine.so`).
//!
//! Like `runtime-dylib` it re-exports what it provides, so an app's
//! `-C prefer-dynamic` link takes it from here. Outside the thin build it is
//! empty.

#[cfg(feature = "thin")]
extern crate runtime_dylib;

/// The expansion. It reads a rule's zones from `chrono-tz`'s database, so the
/// database travels with it rather than into every app that expands a stored
/// rule.
#[cfg(feature = "thin")]
pub use rrule;

/// Exercise the expansion's own code: expand a stored rule and read the dates
/// it answers in a named zone.
///
/// The engine's check binary and the app's `--probe-…` call it, so a release
/// proves the library loads and expands against its runtime.
#[cfg(feature = "thin")]
pub fn self_check() -> Result<String, String> {
    let set: rrule::RRuleSet =
        "DTSTART;TZID=Europe/Vienna:20260329T013000\nRRULE:FREQ=DAILY;COUNT=3"
            .parse()
            .map_err(|error| format!("the expansion refused a stored rule: {error}"))?;
    let dates = set.all_unchecked();
    let first = dates
        .first()
        .ok_or_else(|| "the expansion answered no date".to_string())?;
    let written = first.format("%Y-%m-%dT%H:%M%:z").to_string();
    if dates.len() != 3 || !written.starts_with("2026-03-29T01:30") {
        return Err(format!(
            "the expansion answered {} dates, first {written}, for a daily rule from 2026-03-29 01:30 in Europe/Vienna",
            dates.len()
        ));
    }
    Ok(format!(
        "{} dates from a daily rule, first {written}",
        dates.len()
    ))
}
