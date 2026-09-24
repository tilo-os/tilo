//! Network client defaults shared by every Tilo process.
//!
//! Tilo has one TLS backend (`ring`) and one trust source: the immutable
//! system bundle read by `tilo-native-certs`. Building those per crate is how
//! apps drifted onto bundled Mozilla roots, so clients take them from here.

use std::sync::Arc;

use rustls::crypto::CryptoProvider;
use rustls::{ClientConfig, RootCertStore};

/// The workspace's only TLS crypto provider.
pub fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Install [`crypto_provider`] as the process default.
///
/// Libraries that consult `CryptoProvider::get_default()` rather than taking a
/// provider need this before they build a client. Repeated calls are harmless.
pub fn install_crypto_provider() {
    if CryptoProvider::get_default().is_none() {
        // Losing a race to another installer leaves the same provider in place.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

/// Roots from the Tilo system trust bundle.
///
/// Unparsable entries are skipped; an unreadable bundle yields an empty store,
/// so verification fails closed.
pub fn system_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    roots
}

/// A client config that verifies servers against `roots`.
///
/// # Errors
/// Only if the provider supports none of rustls' default protocol versions.
pub fn client_config_with_roots(roots: RootCertStore) -> Result<ClientConfig, rustls::Error> {
    Ok(ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// A client config that verifies servers against the system trust bundle.
///
/// # Errors
/// Only if the provider supports none of rustls' default protocol versions.
pub fn client_config() -> Result<ClientConfig, rustls::Error> {
    client_config_with_roots(system_roots())
}

/// HTTP clients on Tilo's TLS defaults.
///
/// reqwest is built without a bundled crypto provider and verifies through
/// `rustls-platform-verifier`, which on Linux reads the same system bundle as
/// [`system_roots`]. Its builders panic unless a process default provider is
/// installed, so every client starts from these functions.
#[cfg(feature = "http")]
pub mod http {
    /// An async client builder with the crypto provider installed.
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the sanctioned constructor; it installs the provider first"
    )]
    pub fn client() -> reqwest::ClientBuilder {
        crate::install_crypto_provider();
        reqwest::Client::builder()
    }

    /// A blocking client builder with the crypto provider installed.
    #[cfg(feature = "blocking")]
    #[allow(
        clippy::disallowed_methods,
        reason = "this is the sanctioned constructor; it installs the provider first"
    )]
    pub fn blocking_client() -> reqwest::blocking::ClientBuilder {
        crate::install_crypto_provider();
        reqwest::blocking::Client::builder()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_config_builds_with_the_ring_provider() {
        assert!(client_config_with_roots(RootCertStore::empty()).is_ok());
    }

    #[test]
    fn install_is_idempotent() {
        install_crypto_provider();
        install_crypto_provider();
        assert!(CryptoProvider::get_default().is_some());
    }
}
