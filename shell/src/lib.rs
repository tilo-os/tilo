#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod compositor;
pub mod config;
pub mod network;
pub mod sandbox;
pub mod security;
pub mod shell;
pub mod workspace;
// xdg-desktop-portal backend (file, capture, access, print, and notification) over zbus; opt-in via `--enable-portals`.
pub mod portal;
// The `org.freedesktop.Notifications` D-Bus service — the compositor
// owns the daemon so sandboxed apps' toasts render as OS-drawn chrome.
pub mod notifications;
pub mod priority_contacts;
// First-boot setup invoked by `quick-desktop-firstboot.service` via `quick-desktop --firstboot`.
pub mod bluetooth;
pub mod bluetooth_prompt;
pub mod device_fleet;
pub mod firstboot;
pub mod identity_broker;
pub mod runtime_host;

pub mod identity_registry;
pub mod onboarding;
pub mod onboarding_view;
#[cfg(test)]
#[path = "../tests/runtime_host.rs"]
mod runtime_host_tests;
// Observability counters aggregated per peer surface — read by the OSD overlay and the flatpak-share e2e test.
pub mod diagnostics;
// End-to-end orchestration for "share a Flatpak app window to peers":
// composes the capture/encode/registry/`NeighbourMsg` pipeline behind one public entry point.
pub mod collab;
// Hand-rolled Wayland dispatch for extension protocols smithay 0.7
// doesn't wrap — currently `color-management-v1` + a small EDID parser for DRM output-side reporting.
pub mod wayland;

// These existing app tests need no Linux adapter. Keep their Linux integration
// owner and compile the same source for native library-only validation.
#[cfg(all(test, target_os = "macos"))]
#[path = "../tests/apps_integration.rs"]
mod apps_integration;
