//! Tilo's one TLS crypto provider and trust source.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

pub fn crypto_provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Install [`crypto_provider`] as the process default: libraries that read
/// `CryptoProvider::get_default()` need this before they build a client.
/// Repeated calls are harmless.
pub fn install_crypto_provider() {
    if CryptoProvider::get_default().is_none() {
        // Another installer winning the race installs this same provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
}

/// Roots from the Tilo system trust bundle. An unreadable bundle yields an
/// empty store, so verification fails closed.
pub fn system_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    roots
}

pub fn client_config_with_roots(roots: RootCertStore) -> Result<ClientConfig, rustls::Error> {
    Ok(ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

#[cfg(not(target_os = "android"))]
pub fn client_config() -> Result<ClientConfig, rustls::Error> {
    use rustls_platform_verifier::BuilderVerifierExt as _;
    Ok(ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .with_platform_verifier()?
        .with_no_client_auth())
}

/// The platform verifier aborts without the app's JVM context and its own
/// Kotlin classes; Android's system store holds the same roots.
#[cfg(target_os = "android")]
pub fn client_config() -> Result<ClientConfig, rustls::Error> {
    client_config_with_roots(system_roots())
}

/// A test's own certificates (DER), trusted as a chain root and as that exact
/// server certificate; production trust is [`client_config`] alone.
pub fn client_config_also_trusting(
    extra: Vec<CertificateDer<'static>>,
) -> Result<ClientConfig, rustls::Error> {
    Ok(ClientConfig::builder_with_provider(crypto_provider())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(server_verifier_also_trusting(extra)?)
        .with_no_client_auth())
}

/// Verifies as [`client_config`] does, and also trusts each of `extra` (DER) as
/// a chain root and as that exact server certificate, so a self-signed server
/// (which a chain check does not take as its own root) verifies. Verification
/// stays on: a server must still present one of these or a chain to a trusted
/// root, for its name, and hold its key.
pub fn server_verifier_also_trusting(
    extra: Vec<CertificateDer<'static>>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    let provider = crypto_provider();
    // A self-signed leaf is no root to a chain check, so it is pinned as well.
    let roots: Vec<CertificateDer<'static>> = extra
        .iter()
        .filter(|cert| RootCertStore::empty().add((*cert).clone()).is_ok())
        .cloned()
        .collect();
    let chain = chain_verifier(roots, Arc::clone(&provider))?;
    Ok(Arc::new(AlsoTrusting {
        pinned: extra,
        chain,
        provider,
    }))
}

#[cfg(not(target_os = "android"))]
fn chain_verifier(
    roots: Vec<CertificateDer<'static>>,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    Ok(Arc::new(
        rustls_platform_verifier::Verifier::new_with_extra_roots(roots, provider)?,
    ))
}

#[cfg(target_os = "android")]
fn chain_verifier(
    roots: Vec<CertificateDer<'static>>,
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    let mut store = system_roots();
    for root in roots {
        store.add(root)?;
    }
    rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(store), provider)
        .build()
        .map(|verifier| verifier as Arc<dyn ServerCertVerifier>)
        .map_err(|error| rustls::Error::General(error.to_string()))
}

#[derive(Debug)]
struct AlsoTrusting {
    pinned: Vec<CertificateDer<'static>>,
    chain: Arc<dyn ServerCertVerifier>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AlsoTrusting {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if self
            .pinned
            .iter()
            .any(|pinned| pinned.as_ref() == end_entity.as_ref())
        {
            return Ok(ServerCertVerified::assertion());
        }
        self.chain
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Trusts the server certificate `accept` admits, given its DER, instead of a
/// chain to a root. The handshake signature is still verified, so the server
/// holds the admitted certificate's key.
pub fn pinned_client_config(
    accept: impl Fn(&[u8]) -> bool + Send + Sync + 'static,
) -> Result<ClientConfig, rustls::Error> {
    let provider = crypto_provider();
    Ok(ClientConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned { accept, provider }))
        .with_no_client_auth())
}

struct Pinned<F> {
    accept: F,
    provider: Arc<CryptoProvider>,
}

impl<F> std::fmt::Debug for Pinned<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pinned").finish_non_exhaustive()
    }
}

impl<F: Fn(&[u8]) -> bool + Send + Sync> ServerCertVerifier for Pinned<F> {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if (self.accept)(end_entity.as_ref()) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "certificate is not the pinned one".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_provider_installs_idempotently_and_builds_client_configs() {
        {
            assert!(client_config_with_roots(RootCertStore::empty()).is_ok());
            assert!(pinned_client_config(|_| true).is_ok());
            assert!(client_config_also_trusting(Vec::new()).is_ok());
        }
        {
            install_crypto_provider();
            install_crypto_provider();
            assert!(CryptoProvider::get_default().is_some());
        }
        {
            assert!(client_config().is_ok());
        }
    }
}
