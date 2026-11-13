//! TILO PATCH: the trust an endpoint gives relays and the other TLS services
//! it dials when [`Builder::ca_tls_config`] is not set is Tilo's system
//! store, the one every other Tilo client trusts (`tilo_net::tls::client_config`),
//! instead of a compiled-in copy of Mozilla's roots. See `TILO-PATCH.md`.
//!
//! [`Builder::ca_tls_config`]: crate::endpoint::Builder::ca_tls_config

use std::sync::Arc;

use iroh_relay::tls::CaTlsConfig;
use rustls::{
    DigitallySignedStruct, SignatureScheme,
    client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    crypto::CryptoProvider,
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use tracing::warn;

/// Trust the system store: the platform verifier, which reads Tilo's bundle
/// on Linux and asks the Keychain on macOS; Android's system roots there.
pub(crate) fn system_store() -> CaTlsConfig {
    CaTlsConfig::custom_server_cert_verifier(Arc::new(|provider| {
        Ok(match store_verifier(Arc::clone(&provider)) {
            Ok(verifier) => verifier,
            Err(error) => {
                // Without a store nothing can be trusted, but the endpoint
                // still binds: direct iroh connections do not need it.
                warn!("no TLS trust store, so no relay or TLS service is trusted: {error}");
                Arc::new(NoStore {
                    error: error.to_string(),
                    provider,
                })
            }
        })
    }))
}

#[cfg(not(target_os = "android"))]
fn store_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    Ok(Arc::new(rustls_platform_verifier::Verifier::new(provider)?))
}

/// The platform verifier needs the app's JVM context on Android and aborts
/// without it; the system store holds the roots the platform trusts.
#[cfg(target_os = "android")]
fn store_verifier(
    provider: Arc<CryptoProvider>,
) -> Result<Arc<dyn ServerCertVerifier>, rustls::Error> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    rustls::client::WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider)
        .build()
        .map(|verifier| verifier as Arc<dyn ServerCertVerifier>)
        .map_err(|error| rustls::Error::General(error.to_string()))
}

/// Refuses every server: the store could not be read.
#[derive(Debug)]
struct NoStore {
    error: String,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for NoStore {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Err(rustls::Error::General(format!(
            "no TLS trust store: {}",
            self.error
        )))
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
