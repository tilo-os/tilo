//! Compositor binary entry point.
//!
//! Phase 1 of `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! splits the launch surface in two: pick a [`Mode`] from `--mode`, then
//! delegate to the matching `compositor::*::run` function. Today only
//! [`Mode::Hosted`] does anything useful; [`Mode::Native`] is a stub that
//! exits non-zero so we can wire smithay-on-TTY in later phases without
//! touching `main.rs` again.

use quick_desktop::compositor::{self, Mode};

fn main() {
    tracing_subscriber::fmt::init();
    match compositor::parse_mode_from_args() {
        Mode::Hosted => compositor::hosted::run(),
        Mode::Native => compositor::native::run(),
    }
}
