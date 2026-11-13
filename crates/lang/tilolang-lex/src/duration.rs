//! Lengths of time, as a literal writes them (`7d`, `5m`).

/// The units a duration is written in, and how many seconds each is.
pub const UNITS: &[(&str, i64)] = &[
    ("s", 1),
    ("m", 60),
    ("h", 3600),
    ("d", 86_400),
    ("w", 604_800),
];
