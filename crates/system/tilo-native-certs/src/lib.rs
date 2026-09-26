//! Narrow native-root loader for Tilo's Rustls stack.
//!
//! Upstream `rustls-native-certs` discovers Unix roots through
//! `openssl-probe`, including process-controlled OpenSSL environment and a
//! list of distro-specific guesses. Tilo has one immutable trust bundle, so
//! accepting that discovery surface is unnecessary. This compatible subset
//! reads only the Tilo bundle path. Process environment cannot redirect trust.

use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

const TILO_CA_BUNDLE: &str = "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem";

/// Load certificates from Tilo's single system trust bundle.
pub fn load_native_certs() -> CertificateResult {
    load_certs_from_paths(Some(Path::new(TILO_CA_BUNDLE)), None)
}

/// Load certificates from an explicit PEM file.
///
/// The directory argument is deliberately rejected: hashed OpenSSL CA
/// directories are not part of Tilo's trust model.
pub fn load_certs_from_paths(file: Option<&Path>, dir: Option<&Path>) -> CertificateResult {
    let mut result = CertificateResult::default();
    if let Some(dir) = dir {
        result.errors.push(Error::new(
            io::ErrorKind::Unsupported,
            dir,
            "certificate directories are disabled by Tilo",
        ));
    }
    let Some(file) = file else { return result };
    let certificates = match CertificateDer::pem_file_iter(file) {
        Ok(certificates) => certificates,
        Err(error) => {
            let kind = match error {
                rustls_pki_types::pem::Error::Io(error) => error.kind(),
                _ => io::ErrorKind::InvalidData,
            };
            result.errors.push(Error::new(
                kind,
                file,
                "failed to open the system certificate bundle",
            ));
            return result;
        }
    };
    for certificate in certificates {
        match certificate {
            Ok(certificate) => result.certs.push(certificate),
            Err(_) => result.errors.push(Error::new(
                io::ErrorKind::InvalidData,
                file,
                "failed to parse a certificate in the system bundle",
            )),
        }
    }
    result
        .certs
        .sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
    result.certs.dedup();
    result
}

#[non_exhaustive]
#[derive(Debug, Default)]
pub struct CertificateResult {
    pub certs: Vec<CertificateDer<'static>>,
    pub errors: Vec<Error>,
}

impl CertificateResult {
    #[track_caller]
    pub fn expect(self, message: &str) -> Vec<CertificateDer<'static>> {
        assert!(self.errors.is_empty(), "{message}: {:?}", self.errors);
        self.certs
    }

    #[track_caller]
    pub fn unwrap(self) -> Vec<CertificateDer<'static>> {
        self.expect("errors occurred while loading certificates")
    }
}

#[derive(Debug)]
pub struct Error {
    context: &'static str,
    kind: io::ErrorKind,
    path: PathBuf,
}

impl Error {
    fn new(kind: io::ErrorKind, path: &Path, context: &'static str) -> Self {
        Self {
            context,
            kind,
            path: path.to_owned(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} at '{}'",
            self.context,
            self.kind,
            self.path.display()
        )
    }
}

impl StdError for Error {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn explicit_bundles_load_deduplicated_and_directories_are_rejected() {
        // Phase `explicit_bundle_loads_and_deduplicates`.
        {
            let directory = tempfile::tempdir().unwrap();
            let bundle = directory.path().join("roots.pem");
            let certificate = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
            fs::write(&bundle, format!("{certificate}{certificate}")).unwrap();
            let result = load_certs_from_paths(Some(&bundle), None);
            assert!(result.errors.is_empty());
            assert_eq!(result.certs.len(), 1);
        }
        // Phase `certificate_directories_are_rejected`.
        {
            let directory = tempfile::tempdir().unwrap();
            let result = load_certs_from_paths(None, Some(directory.path()));
            assert_eq!(result.errors.len(), 1);
        }
    }
}
