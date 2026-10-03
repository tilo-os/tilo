//! `runtime` — the Tilo app SDK umbrella crate.
//!
//! Apps typically depend on `runtime` and use the re-exported submodules
//! ([`collab`], [`fs`], [`identity`], [`portal`]) instead of pulling in
//! each sub-crate individually.
//!
//! Every service reachable through this SDK is optional: apps built
//! against `runtime` must run as Flatpaks on stock Linux with no Tilo
//! services present, so every `try_connect` / `load` returns
//! `Result<_, NotAvailable>` rather than panicking, letting the app fall
//! back to a local-only mode.
//!
//! ```ignore
//! use runtime::{collab, fs, identity};
//!
//! let me = identity::load()?;            // single-device signing works locally
//! let workspace = fs::try_connect()?;    // Cap'n Proto proxy to the FUSE tree
//! let broker = collab::try_connect()?;   // Cap'n Proto proxy to the session broker
//! ```
//!
//! ```text
//!                +------------------------+
//!                |      tilo-runtime       |   (this crate — umbrella)
//!                +------------------------+
//!                     |    |   |    |
//!         portal ---+ |    |   |    +--- identity
//!                    v    v   v
//!                   collab   fs
//! ```
//!
//! [`portal`] owns the low-level `service_fd` + `UnixCapnpClient` plumbing
//! that [`collab`] and [`fs`] each wrap with a Cap'n Proto schema and typed
//! clients; [`identity`] consumes an app-scoped signing connection while
//! private key material remains in the host launcher-side broker.
//!
//! The broker lives in the compositor, the FUSE driver in `crates/system/fs/`, and the
//! launcher is `tilo-launcher` — this umbrella is client-side only.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub use runtime_collab as collab;
pub use runtime_fs as fs;
pub use runtime_identity as identity;
pub use runtime_portal as portal;
/// XDG desktop portal client (screencast, notifications, secret); file
/// choosing and open-uri are `ui::portal`. See [`runtime_portal_xdg`] for the degradation contract.
pub use runtime_portal_xdg as portal_xdg;

/// The shared error type: every fallible SDK entry point returns
/// `Result<T, NotAvailable>`, re-exported so app code matches one import.
pub use runtime_portal::NotAvailable;

/// Are we running inside a Tilo session at all? True iff the launcher set
/// `TILO_WORKSPACE_ID` — not an authorisation gate, see [`portal::is_tilo_native`].
pub use runtime_portal::is_tilo_native;

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end: with no env vars set, every service reports unavailable
    /// rather than panicking — the Flatpak-on-stock-Linux path. Also the
    /// compile-time smoke check that the re-exports resolve: if a sub-crate
    /// renames a public type this stops compiling.
    #[test]
    fn re_exported_services_report_unavailable_when_env_is_empty() {
        // Bare `_` (not `_name`): the binding is never read, only its
        // type — the point of each line is that it fails to compile if a
        // sub-crate's signature drifts.
        let _: fn(&str) -> Option<std::os::fd::RawFd> = portal::service_fd;

        let _: fn() -> Result<collab::CollabClient, NotAvailable> = collab::try_connect;

        let _: fn() -> Result<fs::FsClient, NotAvailable> = fs::try_connect;

        let _: fn() -> Result<identity::SigningIdentity, NotAvailable> = identity::load;

        let mut env = tilo_test_env::ScopedEnv::new();
        for (key, _) in std::env::vars() {
            if key.starts_with("TILO_") {
                env.remove(&key);
            }
        }

        assert!(!is_tilo_native());
        assert!(matches!(
            collab::try_connect(),
            Err(NotAvailable::ServiceFdMissing)
        ));
        assert!(matches!(
            fs::try_connect(),
            Err(NotAvailable::ServiceFdMissing)
        ));
        assert!(matches!(
            identity::load(),
            Err(NotAvailable::ServiceFdMissing)
        ));
    }
}
