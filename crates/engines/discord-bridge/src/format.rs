//! How a number of bytes reads in a sentence, as an op's argument carries it.

/// `bytes` as the shortest whole unit it fits: "943 B", "1.4 MB".
#[must_use]
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let last = UNITS.len().saturating_sub(1);
    let mut value = as_f64(bytes);
    let mut unit = 0;
    while value >= 1000.0 && unit < last {
        value /= 1000.0;
        unit = unit.saturating_add(1);
    }
    let unit_name = UNITS.get(unit).copied().unwrap_or_default();
    if unit == 0 {
        format!("{bytes} {unit_name}")
    } else if value < 10.0 {
        format!("{value:.1} {unit_name}")
    } else {
        format!("{value:.0} {unit_name}")
    }
}

/// `bytes` as `f64`, rounded the one time the conversion rounds: each half of a `u64` converts
/// exactly, so their sum keeps every digit a size shows.
fn as_f64(bytes: u64) -> f64 {
    const HALF: u64 = 4_294_967_296;
    let high = u32::try_from(bytes / HALF).unwrap_or_default();
    let low = u32::try_from(bytes % HALF).unwrap_or_default();
    f64::from(high).mul_add(4_294_967_296.0, f64::from(low))
}
