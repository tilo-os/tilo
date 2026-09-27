//! Confinement for Tilo apps on stock macOS.
//!
//! Every app runs sandboxed. Which sandbox is the one decision this crate
//! makes:
//!
//! - **App Sandbox** — the app is validly signed by an Apple Developer ID
//!   (optionally a configured team), notarized, and its signature carries
//!   `com.apple.security.app-sandbox = true`. It is exec'd directly and the
//!   kernel enforces the sandbox its entitlements describe.
//! - **Seatbelt** — everything else: unsigned, ad-hoc, tampered, signed by
//!   anyone else, or signed without the entitlement. It is exec'd under a
//!   deny-default SBPL profile this crate generates from the same policy
//!   inputs the Linux launcher confines with.
//!
//! There is no third outcome. Any failure while inspecting a signature is a
//! Seatbelt verdict, and a Seatbelt profile that cannot be applied refuses
//! the launch.
//!
//! Apple signing is additional to, never a substitute for, the registry's
//! signed index (`docs/app-model/trust.md`): the caller verifies a payload
//! against the Ed25519 root before it reaches [`launch`].
//!
//! - [`classify`] — the signature verdict as data and the pure decision
//!   over it.
//! - [`policy`] — the grants: payload, runtime, container, user
//!   capabilities and egress, resolved to real paths.
//! - [`sbpl`] — the grants rendered as a parameterised SBPL profile.
//! - `codesign` / `launch` (macOS only) — Security.framework inspection and
//!   the spawn paths for each mode.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod classify;
pub mod policy;
pub mod sbpl;

#[cfg(target_os = "macos")]
pub mod codesign;
#[cfg(target_os = "macos")]
pub mod launch;

pub use classify::{
    CodeSignature, MacLaunchMode, SeatbeltReason, TeamId, TrustRequirement, decide,
};
#[cfg(target_os = "macos")]
pub use codesign::{classify, inspect};
#[cfg(target_os = "macos")]
pub use launch::{LaunchError, LaunchRequest, LaunchedApp, launch};
