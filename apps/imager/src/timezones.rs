//! The time zones the install offers: this machine's own database, and a short list of the ones
//! nearly every machine has where it reads none, so a picker is never empty. Read once.

use std::sync::OnceLock;

/// The zones this machine knows (`Europe/Berlin`), sorted: its IANA database, or [`FALLBACK`]
/// where it has none. [`UTC`] is always offered.
pub(crate) fn offered() -> &'static [String] {
    static ZONES: OnceLock<Vec<String>> = OnceLock::new();
    ZONES.get_or_init(|| {
        let mut zones = tilo_clock::local::zone_names();
        if zones.is_empty() {
            zones = FALLBACK.iter().map(|zone| (*zone).to_owned()).collect();
        }
        zones.sort();
        zones.dedup();
        zones
    })
}

/// The zones offered where this machine reads no database.
const FALLBACK: &[&str] = &[
    "UTC",
    "Africa/Nairobi",
    "America/Chicago",
    "America/Denver",
    "America/Los_Angeles",
    "America/New_York",
    "America/Sao_Paulo",
    "Asia/Dubai",
    "Asia/Kolkata",
    "Asia/Shanghai",
    "Asia/Tokyo",
    "Australia/Sydney",
    "Europe/Amsterdam",
    "Europe/Berlin",
    "Europe/London",
    "Europe/Madrid",
    "Europe/Moscow",
    "Europe/Paris",
    "Pacific/Auckland",
];
