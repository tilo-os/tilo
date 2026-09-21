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
pub use fixture::{fixture_seed, fixture_writer_insert};

#[cfg(any(target_arch = "wasm32", test))]
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

#[cfg(any(target_arch = "wasm32", test))]
mod post_ready_repair;

mod desktop;

#[cfg(any(target_arch = "wasm32", test))]
mod desktop_activity;
#[cfg(any(target_arch = "wasm32", test))]
mod desktop_host;
#[cfg(any(target_arch = "wasm32", test))]
mod desktop_navigation;
#[cfg(any(target_arch = "wasm32", test))]
mod system_search;

/// Private browser action correlation, also exercised by the ordinary host harness.
pub mod action_client;
pub mod committed_desktop;

#[cfg(any(target_arch = "wasm32", test))]
mod storage_key;

#[cfg(any(target_arch = "wasm32", test))]
mod action_palette;

#[cfg(any(target_arch = "wasm32", test))]
mod viewport;

#[cfg(any(target_arch = "wasm32", test))]
mod frame_metrics;

#[cfg(any(target_arch = "wasm32", test))]
mod semantics_status;

#[cfg(any(target_arch = "wasm32", test))]
mod report_text;

#[cfg(any(target_arch = "wasm32", test))]
const fn font_install_should_wake(peer_suspended: bool, hidden: bool) -> bool {
    !peer_suspended && !hidden
}

#[cfg(test)]
mod font_install_lifecycle_tests {
    #[test]
    fn suspended_or_hidden_mount_installs_without_waking_its_render_loop() {
        assert!(super::font_install_should_wake(false, false));
        assert!(!super::font_install_should_wake(true, false));
        assert!(!super::font_install_should_wake(false, true));
        assert!(!super::font_install_should_wake(true, true));
    }
}
