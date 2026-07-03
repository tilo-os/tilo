//! `qos-runtime` — the qOS app SDK umbrella crate.
//!
//! Apps typically depend on `qos-runtime` and use the re-exported
//! submodules ([`collab`], [`fs`], [`identity`], [`portal`]) instead
//! of pulling in each sub-crate individually.
//!
//! ## Design intent
//!
//! Every service reachable through this SDK is optional. Apps built
//! against `qos-runtime` MUST run as Flatpaks on stock Linux with no
//! qOS services present — every `try_connect` / `load` returns
//! `Result<_, NotAvailable>` rather than panicking, so the app can
//! fall back to a local-only mode.
//!
//! ## Quick start
//!
//! ```ignore
//! use qos_runtime::{collab, fs, identity};
//!
//! // Signing identity (single-device signing works locally).
//! let me = identity::load()?;
//!
//! // Workspace filesystem — a Cap'n Proto proxy to the FUSE tree.
//! let workspace = fs::try_connect()?;
//!
//! // Collab broker — Cap'n Proto proxy to apps/collab-broker.
//! let broker = collab::try_connect()?;
//! ```
//!
//! ## Layering
//!
//! ```text
//!                +------------------------+
//!                |      qos-runtime       |   (this crate — umbrella)
//!                +------------------------+
//!                     |    |   |    |
//!         portal ---+ |    |   |    +--- identity
//!                    v    v   v
//!                   collab   fs
//! ```
//!
//! - [`portal`] owns the low-level `service_fd` + `UnixCapnpClient`
//!   plumbing. Higher layers wrap it.
//! - [`collab`] and [`fs`] each carry a Cap'n Proto schema + client
//!   wrappers.
//! - [`identity`] reads the on-disk identity record and does local
//!   signing. Does not talk to a service.
//!
//! ## What lives elsewhere
//!
//! Broker daemon → `apps/collab-broker`. FUSE driver → `fs/`.
//! Launcher → `qos-launcher`. This umbrella is client-side only.

pub use qos_runtime_collab as collab;
pub use qos_runtime_fs as fs;
pub use qos_runtime_identity as identity;
pub use qos_runtime_portal as portal;

/// The shared error type — every fallible SDK entry point returns
/// `Result<T, NotAvailable>`. Re-exported at the top level so app
/// code can match on a single import.
pub use qos_runtime_portal::NotAvailable;

/// Convenience: are we running inside a qOS session at all? True
/// iff the launcher set `QOS_WORKSPACE_ID`. Not an authorisation
/// gate — see [`portal::is_qos_native`] for details.
pub use qos_runtime_portal::is_qos_native;

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time smoke test: the re-exports resolve. If a
    /// sub-crate renames a public type this stops compiling.
    #[test]
    fn re_exports_resolve() {
        // portal
        let _fd_name: fn(&str) -> Option<std::os::fd::RawFd> = portal::service_fd;

        // collab
        let _collab_try: fn() -> Result<collab::CollabClient, NotAvailable> = collab::try_connect;

        // fs
        let _fs_try: fn() -> Result<fs::FsClient, NotAvailable> = fs::try_connect;

        // identity
        let _id_load: fn() -> Result<identity::SigningIdentity, NotAvailable> = identity::load;
    }

    /// End-to-end: with no env vars set, every service reports
    /// unavailable rather than panicking. This is the flatpak-on-
    /// stock-Linux path.
    #[test]
    fn every_service_reports_unavailable_when_env_is_empty() {
        for (key, _) in std::env::vars() {
            if key.starts_with("QOS_") {
                // SAFETY: this test process' env is not touched by
                // any other thread during test setup.
                unsafe { std::env::remove_var(&key) };
            }
        }

        assert!(!is_qos_native());
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
