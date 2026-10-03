//! Tilo's one TLS crypto provider and trust source.

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
/// so verification fails closed. The bundle exists only on Linux; elsewhere
/// use [`client_config`], which asks the platform.
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

/// A client config that verifies servers against the system's trust: the
/// Tilo bundle on Linux, the Keychain on macOS. This is the verifier reqwest
/// uses, so every Tilo client trusts the same servers.
///
/// # Errors
/// If the provider supports none of rustls' default protocol versions, or the
/// platform verifier cannot load the system's trust.
pub fn client_config() -> Result<ClientConfig, rustls::Error> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    Ok(ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_provider_installs_idempotently_and_builds_client_configs() {
        // Phase `client_config_builds_with_the_ring_provider`.
        {
            assert!(client_config_with_roots(RootCertStore::empty()).is_ok());
        }
        // Phase `install_is_idempotent`.
        {
            install_crypto_provider();
            install_crypto_provider();
            assert!(CryptoProvider::get_default().is_some());
        }
        // Phase `the_platform_config_builds`.
        {
            assert!(client_config().is_ok());
        }
    }
}
