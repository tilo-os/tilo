//! Compositor binary entry point.
//!
//! Phase 1 of `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! split the launch surface in two: pick a [`Mode`] from `--mode`, then
//! delegate to the matching `compositor::*::run` function.
//!
//! As of Phase 7, [`Mode::Native`] is the default — no flag at all spins up
//! the real smithay server (auto-binding `wayland-N`) with the vello+wgpu
//! winit backend from Phase 4. [`Mode::Hosted`] remains as an explicit dev
//! opt-in (`--mode hosted`) for the legacy iced-on-winit path when DRM or a
//! spare Wayland session is unavailable.

use quick_desktop::compositor::hosted::HostedOptions;
use quick_desktop::compositor::{self, Mode};

fn main() {
    tracing_subscriber::fmt::init();
    let enable_peer_sync = compositor::parse_peer_sync_from_args();
    let enable_portals = compositor::parse_portals_from_args();
    match compositor::parse_mode_from_args() {
        Mode::Hosted => compositor::hosted::run_with_full_options(HostedOptions {
            enable_peer_sync,
            enable_portals,
        }),
        // `--mode native` doesn't take the portal flag yet — Sandbox
        // Phase 4.3 only wires the hosted-mode boot path. Surface a
        // warning so users who pass `--enable-portals --mode native`
        // don't silently get nothing.
        Mode::Native => {
            if enable_portals {
                tracing::warn!(
                    "--enable-portals is hosted-mode only in v1 of Sandbox Phase 4.3; \
                     native mode currently ignores it"
                );
            }
            compositor::native::run();
        }
    }
}
