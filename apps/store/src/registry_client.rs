//! HTTP client for a Tilo registry — [`app_cli::client`], re-exported.
//!
//! The store used to carry its own copy of this: the same two
//! endpoints, the same signature check, the same trust classification,
//! differing only in the `User-Agent`. The desktop's search box needs
//! exactly the same thing — it has to know what a registry offers in
//! order to offer it — so the code moved into a crate both can depend
//! on rather than being written twice.
//!
//! Keeping it duplicated was the real risk. The two copies decide
//! whether a fetched entry's publisher signature verifies, and a fix
//! applied to one of them silently would not reach the other: the store
//! would refuse an entry the palette still offered, or the reverse, and
//! neither would look like a bug from inside its own crate.
//!
//! Everything the store used from this module keeps its path, so
//! `store::registry_client::fetch_entries` still resolves.

pub use app_cli::client::{
    RegistryError, TrustLevel, VerifiedEntry, classify_snapshot, fetch_entries, fetch_handshake,
    fetch_snapshot,
};
