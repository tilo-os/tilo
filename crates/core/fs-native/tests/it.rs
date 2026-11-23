//! Integration tests against a real Btrfs volume and the running kernel.
//! One test binary instead of one per file.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion or fixture arithmetic, not a hazard"
)]

mod support;

mod capture_test;
mod git_test;
mod hydrate_test;
mod journal_test;
mod probe_test;
