//! Actual Tilo component renderer. Authentication/state transport are installed by the peer layer.
//!
//! In wasm32 the certified peer, its transports and the Automerge admission
//! check belong to the `cloud` feature; host tests always cover them.
#![cfg_attr(
    all(target_arch = "wasm32", not(feature = "cloud")),
    allow(
        dead_code,
        reason = "without the Cloud peer the canonical desktop and live paths stay compiled but unreachable"
    )
)]
#[cfg(target_arch = "wasm32")]
mod browser;

#[cfg(target_arch = "wasm32")]
mod clipboard;

#[cfg(all(target_arch = "wasm32", feature = "cloud"))]
mod automerge_check;

#[cfg(test)]
tilo_automerge_isolate::test_worker!();

#[cfg(any(target_arch = "wasm32", test))]
mod close_recovery;

#[cfg(all(target_arch = "wasm32", feature = "cloud"))]
mod peer;

#[cfg(not(target_arch = "wasm32"))]
mod fixture;
#[cfg(not(target_arch = "wasm32"))]
pub use fixture::{fixture_seed, fixture_writer_insert};

#[cfg(any(target_arch = "wasm32", test))]
mod admitted_records;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
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

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod identity;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_cloud;
#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_invitation;
#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_onboarding;
#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_onboarding_custody;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_journal;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_delivery;
#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_delivery_auth;
#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_delivery_cursor;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_storage_batch;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_media;

#[cfg(any(all(target_arch = "wasm32", feature = "cloud"), test))]
mod peer_live;

#[cfg(test)]
mod post_ready_repair;

mod catalog;
mod desktop;
#[cfg(all(target_arch = "wasm32", feature = "dylink"))]
mod side_module;
// Side modules link against the SDK facade and its resource layer, which the
// core itself never names; rustc links only crates that are referenced.
#[cfg(all(target_arch = "wasm32", feature = "dylink"))]
extern crate tilo_app as _;

#[cfg(any(target_arch = "wasm32", test))]
mod desktop_activity;
#[cfg(any(target_arch = "wasm32", test))]
mod desktop_host;
#[cfg(any(target_arch = "wasm32", test))]
mod desktop_navigation;
#[cfg(any(target_arch = "wasm32", test))]
mod desktop_notifications;
#[cfg(any(target_arch = "wasm32", test))]
mod desktop_overlays;
#[cfg(any(target_arch = "wasm32", test))]
mod live_view;
#[cfg(any(target_arch = "wasm32", test))]
mod local_desktop;
#[cfg(any(target_arch = "wasm32", test))]
mod system_search;

/// Private browser action correlation, also exercised by the ordinary host harness.
pub mod action_client;
pub mod agent_reads;
pub mod committed_desktop;

#[cfg(any(target_arch = "wasm32", test))]
mod mount_agent;

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
