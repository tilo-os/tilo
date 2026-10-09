//! The HTTPS server identity every Tilo browser-facing listener builds the
//! same way: a PEM chain and key from disk, served with rustls' ring
//! provider over HTTP/2 and HTTP/1.1. Behind the light `tls` feature, so a
//! listener that is not a `MoQ` relay (the public-room front) takes it
//! without the relay stack.

use std::path::Path;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// A certificate chain (leaf first) and its private key, read from PEM
/// files.
///
/// # Errors
///
/// A file cannot be read or parsed, or the chain holds no certificate.
pub fn load_pem(
    chain: &Path,
    key: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
    let certificates = CertificateDer::pem_file_iter(chain)
        .map_err(|error| format!("certificate chain {}: {error}", chain.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("certificate chain {}: {error}", chain.display()))?;
    if certificates.is_empty() {
        return Err(format!("certificate chain {} is empty", chain.display()));
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|error| format!("private key {}: {error}", key.display()))?;
    Ok((certificates, key))
}

/// The rustls server configuration for HTTPS over TCP: safe default
/// protocol versions, no client authentication, ALPN `h2` then `http/1.1`.
///
/// # Errors
///
/// The key does not match the chain or is of an unsupported kind.
pub fn https_server_config(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<rustls::ServerConfig, String> {
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| error.to_string())?
    .with_no_client_auth()
    .with_single_cert(chain, key)
    .map_err(|error| error.to_string())?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]
mod tests {
    use super::*;

    #[test]
    fn a_pem_pair_loads_into_an_https_config_and_an_empty_chain_is_refused() {
        let generated = rcgen::generate_simple_self_signed(vec!["rooms.internal".into()]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (chain, key, empty) = (
            dir.path().join("chain.pem"),
            dir.path().join("key.pem"),
            dir.path().join("empty.pem"),
        );
        std::fs::write(&chain, generated.cert.pem()).unwrap();
        std::fs::write(&key, generated.signing_key.serialize_pem()).unwrap();
        std::fs::write(&empty, "").unwrap();

        let (certificates, private_key) = load_pem(&chain, &key).unwrap();
        assert_eq!(certificates, vec![generated.cert.der().clone()]);
        let config = https_server_config(certificates, private_key).unwrap();
        assert_eq!(
            config.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        assert!(load_pem(&empty, &key).unwrap_err().contains("is empty"));
    }
}
