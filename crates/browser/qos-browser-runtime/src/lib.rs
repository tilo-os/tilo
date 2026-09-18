//! Actual qOS component renderer. Authentication/state transport are installed by the peer layer.
#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
mod clipboard;

#[cfg(any(target_arch = "wasm32", test))]
mod close_recovery;

#[cfg(target_arch = "wasm32")]
mod peer;

#[cfg(not(target_arch = "wasm32"))]
mod fixture;
#[cfg(not(target_arch = "wasm32"))]
pub use fixture::{fixture_seed,fixture_writer_insert};

mod admitted_records;

#[cfg(any(target_arch = "wasm32", test))]
mod admission_health;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn revision_wire_value(revision: u64) -> String {
    revision.to_string()
}

#[cfg(test)]
mod revision_wire_tests {
    #[test]
    fn revisions_above_javascript_safe_integer_range_are_decimal_strings() {
        let revision = 9_007_199_254_740_993_u64;
        assert_eq!(super::revision_wire_value(revision), "9007199254740993");
    }
}

#[cfg(any(target_arch = "wasm32", test))]
mod identity;

#[cfg(any(target_arch = "wasm32", test))]
mod peer_journal;

mod desktop;

#[cfg(any(target_arch = "wasm32", test))]
mod desktop_host;

/// Private browser action correlation, also exercised by the ordinary host harness.
pub mod action_client;

#[cfg(any(target_arch = "wasm32", test))]
mod storage_key;

#[cfg(any(target_arch = "wasm32", test))]
mod action_palette;
