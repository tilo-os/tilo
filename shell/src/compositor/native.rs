//! Native compositor mode — *stub*.
//!
//! Phase 1 of `docs/superpowers/plans/2026-05-02-real-wayland-compositor.md`
//! only wires the `--mode native` dispatch; the actual smithay-on-TTY
//! compositor is implemented in Phase 2 and beyond. Until then, asking for
//! native mode prints an explanatory message and exits non-zero so it's
//! immediately obvious to the operator that the feature isn't available
//! yet.

/// Entry point for the (future) smithay-on-TTY compositor.
///
/// Diverges (`-> !`) so callers can use it in a `match` arm next to
/// [`super::hosted::run`] without needing to thread through a unit return
/// type.
pub fn run() -> ! {
    eprintln!("nirios compositor: native mode not yet implemented");
    std::process::exit(1);
}
