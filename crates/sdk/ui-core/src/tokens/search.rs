//! Ranking weights for the platform search. They are not spacing and not pixels: a hit's
//! score is the sum of the weights its query matches.

/// How much a match on a short query's usage contributes.
pub const SHORT_QUERY_USAGE: f32 = 6.0;

/// How much a second, identical text choice contributes.
pub const SAME_TEXT_CHOICE: f32 = 8.0;
