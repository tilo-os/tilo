//! Native root loader for Tilo's Rustls stack.
//!
//! It reads only the Tilo trust bundle, or on Android the system store at its
//! fixed read-only path, so process environment cannot redirect trust.

use rustls_pki_types::CertificateDer;
use rustls_pki_types::pem::PemObject;
use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(not(target_os = "android"))]
const TILO_CA_BUNDLE: &str = "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem";

/// Android's system stores, one PEM root per file: the Conscrypt module's, then
/// the system image's.
#[cfg(target_os = "android")]
const ANDROID_CA_STORES: [&str; 2] = [
    "/apex/com.android.conscrypt/cacerts",
    "/system/etc/security/cacerts",
];

#[cfg(not(target_os = "android"))]
pub fn load_native_certs() -> CertificateResult {
    load_certs_from_paths(Some(Path::new(TILO_CA_BUNDLE)), None)
}

/// The roots Android itself trusts: the first store that holds any. Roots a user
/// added or disabled in Settings are not consulted.
#[cfg(target_os = "android")]
pub fn load_native_certs() -> CertificateResult {
    let mut result = CertificateResult::default();
    for store in ANDROID_CA_STORES {
        result = load_pem_store(Path::new(store));
        if !result.certs.is_empty() {
            break;
        }
    }
    result
}

/// Every root in a directory of one PEM file per certificate.
#[cfg(any(target_os = "android", test))]
fn load_pem_store(store: &Path) -> CertificateResult {
    let mut result = CertificateResult::default();
    let entries = match std::fs::read_dir(store) {
        Ok(entries) => entries,
        Err(error) => {
            result.errors.push(Error::new(
                error.kind(),
                store,
                "failed to open the system certificate store",
            ));
            return result;
        }
    };
    for entry in entries.flatten() {
        extend_from_pem_file(&mut result, &entry.path());
    }
    result.finish()
}

/// Load certificates from an explicit PEM file. The directory argument is
/// rejected: hashed OpenSSL CA directories are not part of Tilo's trust model.
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
    extend_from_pem_file(&mut result, file);
    result.finish()
}

/// Why this program trusts no certificate authority, in words that name the
/// file it reads them from and what is wrong with it: missing, unreadable, or
/// holding no certificate. `None` where that file gives it at least one.
///
/// A program in this state can check no server's identity, so every secured
/// connection it tries fails before it leaves the machine. What it says then
/// must point at its own file, never at the server it was asked to reach.
#[cfg(not(target_os = "android"))]
#[must_use]
pub fn lacks() -> Option<String> {
    lacks_in(Path::new(TILO_CA_BUNDLE))
}

/// [`lacks`], for the bundle at `file`.
#[must_use]
pub fn lacks_in(file: &Path) -> Option<String> {
    let found = load_certs_from_paths(Some(file), None);
    if !found.certs.is_empty() {
        return None;
    }
    let wrong = match found.errors.first().map(|error| error.kind) {
        Some(io::ErrorKind::NotFound) => "is missing".to_owned(),
        Some(io::ErrorKind::InvalidData) | None => "holds no certificate".to_owned(),
        Some(kind) => format!("cannot be read ({kind})"),
    };
    Some(format!(
        "this program has no certificate authorities to check a server's identity with: the \
         one file it reads them from, {}, {wrong} where this program was started. It can make \
         no secured connection until that file is in place; the address it was asked to \
         reach is not at fault",
        file.display()
    ))
}

fn extend_from_pem_file(result: &mut CertificateResult, file: &Path) {
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
            return;
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
}

#[non_exhaustive]
#[derive(Debug, Default)]
pub struct CertificateResult {
    pub certs: Vec<CertificateDer<'static>>,
    pub errors: Vec<Error>,
}

impl CertificateResult {
    fn finish(mut self) -> Self {
        self.certs
            .sort_unstable_by(|left, right| left.as_ref().cmp(right.as_ref()));
        self.certs.dedup();
        self
    }

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
        {
            let directory = tempfile::tempdir().unwrap();
            let bundle = directory.path().join("roots.pem");
            let certificate = "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n";
            fs::write(&bundle, format!("{certificate}{certificate}")).unwrap();
            let result = load_certs_from_paths(Some(&bundle), None);
            assert!(result.errors.is_empty());
            assert_eq!(result.certs.len(), 1);
        }
        {
            let directory = tempfile::tempdir().unwrap();
            let result = load_certs_from_paths(None, Some(directory.path()));
            assert_eq!(result.errors.len(), 1);
        }
    }

    /// A program whose trust bundle gives it no authority says which file
    /// that is and what is wrong with it, and that the address it was asked
    /// to reach is not at fault; one whose bundle holds a certificate lacks
    /// nothing.
    #[test]
    fn a_program_without_authorities_says_which_file_it_lacks() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("tls-ca-bundle.pem");
        let said = lacks_in(&missing).expect("a missing bundle is lacking");
        assert!(
            said.contains(missing.to_str().unwrap()) && said.contains("is missing"),
            "{said}"
        );
        assert!(said.contains("is not at fault"), "{said}");

        let empty = directory.path().join("empty.pem");
        fs::write(&empty, "").unwrap();
        let said = lacks_in(&empty).expect("an empty bundle is lacking");
        assert!(
            said.contains(empty.to_str().unwrap()) && said.contains("holds no certificate"),
            "{said}"
        );

        let folder = directory.path().join("a-folder.pem");
        fs::create_dir(&folder).unwrap();
        let said = lacks_in(&folder).expect("a folder is no bundle");
        assert!(said.contains(folder.to_str().unwrap()), "{said}");

        let bundle = directory.path().join("roots.pem");
        fs::write(
            &bundle,
            "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert_eq!(lacks_in(&bundle), None);
    }

    #[test]
    fn a_store_of_one_root_per_file_loads_each_file() {
        let store = tempfile::tempdir().unwrap();
        let root = |body: &str| {
            format!(
                "Certificate:\n    Data:\n        Version: 3 (0x2)\n\
                 -----BEGIN CERTIFICATE-----\n{body}\n-----END CERTIFICATE-----\n"
            )
        };
        fs::write(store.path().join("0a1b2c3d.0"), root("AQID")).unwrap();
        fs::write(store.path().join("4e5f6a7b.0"), root("BAUG")).unwrap();
        fs::write(store.path().join("4e5f6a7b.1"), root("BAUG")).unwrap();
        let result = load_pem_store(store.path());
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.certs.len(), 2);

        let missing = load_pem_store(&store.path().join("absent"));
        assert!(missing.certs.is_empty());
        assert_eq!(missing.errors.len(), 1);
    }
}
