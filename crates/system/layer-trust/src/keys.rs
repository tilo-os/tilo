//! Ed25519 keys in the formats OpenSSL writes, and the enrolled authorities.
//!
//! A public key is a PEM `PUBLIC KEY` (`SubjectPublicKeyInfo`); a secret key a
//! PEM `PRIVATE KEY` (PKCS#8 v1). Both are what
//! `openssl genpkey -algorithm ed25519` and `openssl pkey -pubout` write, so a
//! key can be inspected or made with standard tools. A key's fingerprint is
//! `sha256:` and the hex SHA-256 of its `SubjectPublicKeyInfo` DER, which is
//! `openssl pkey -pubin -outform DER | sha256sum`.
//!
//! A signature is the base64 of the 64-byte Ed25519 signature over
//! [`crate::SIGNING_DOMAIN`] followed by the manifest's exact bytes.

use std::fmt;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
#[cfg(unix)]
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use sha2::{Digest as _, Sha256};

use crate::SIGNING_DOMAIN;

/// `SubjectPublicKeyInfo` DER of an Ed25519 key, up to the 32 key bytes.
const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];
/// PKCS#8 v1 DER of an Ed25519 key, up to the 32 seed bytes.
const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];
/// A key file larger than this is not a key.
#[cfg(unix)]
const MAX_KEY_BYTES: u64 = 4096;
/// An authority's name: the file stem of its `.pub`.
const MAX_NAME: usize = 32;

/// Why a key could not be read or made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyError(pub String);

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for KeyError {}

fn pem_body(text: &str, label: &str) -> Result<Vec<u8>, KeyError> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut inside = false;
    let mut body = String::new();
    let mut ended = false;
    for line in text.lines().map(str::trim) {
        if line == begin && !inside && !ended {
            inside = true;
        } else if line == end && inside {
            inside = false;
            ended = true;
        } else if inside {
            body.push_str(line);
        } else if !line.is_empty() {
            return Err(KeyError(format!(
                "not a PEM {label}: unexpected text outside it"
            )));
        }
    }
    if !ended {
        return Err(KeyError(format!("not a PEM {label}")));
    }
    STANDARD
        .decode(body.as_bytes())
        .map_err(|error| KeyError(format!("not a PEM {label}: {error}")))
}

fn pem(label: &str, der: &[u8]) -> String {
    let encoded = STANDARD.encode(der);
    let mut text = format!("-----BEGIN {label}-----\n");
    let mut rest = encoded.as_str();
    while !rest.is_empty() {
        let (line, tail) = rest.split_at(rest.len().min(64));
        text.push_str(line);
        text.push('\n');
        rest = tail;
    }
    text.push_str("-----END ");
    text.push_str(label);
    text.push_str("-----\n");
    text
}

fn key_bytes(der: &[u8], prefix: &[u8]) -> Option<[u8; 32]> {
    der.strip_prefix(prefix)?.try_into().ok()
}

/// An authority's public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey(VerifyingKey);

impl PublicKey {
    /// # Errors
    /// Not a PEM `PUBLIC KEY`, or not an Ed25519 key.
    pub fn from_pem(text: &str) -> Result<Self, KeyError> {
        let der = pem_body(text, "PUBLIC KEY")?;
        let bytes = key_bytes(&der, &SPKI_PREFIX)
            .ok_or_else(|| KeyError("not an Ed25519 public key".to_owned()))?;
        VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|error| KeyError(format!("not a valid Ed25519 public key: {error}")))
    }

    fn spki(&self) -> Vec<u8> {
        let mut der = SPKI_PREFIX.to_vec();
        der.extend_from_slice(self.0.as_bytes());
        der
    }

    pub fn to_pem(&self) -> String {
        pem("PUBLIC KEY", &self.spki())
    }

    /// `sha256:<hex>` of the `SubjectPublicKeyInfo` DER.
    pub fn fingerprint(&self) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(self.spki())))
    }

    /// Whether `signature` (the text of a `.sig` file) is this key's
    /// signature over `manifest`.
    pub fn verifies(&self, manifest: &[u8], signature: &str) -> bool {
        let Ok(raw) = STANDARD.decode(signature.trim().as_bytes()) else {
            return false;
        };
        let Ok(signature) = Signature::from_slice(&raw) else {
            return false;
        };
        let mut message = SIGNING_DOMAIN.to_vec();
        message.extend_from_slice(manifest);
        self.0.verify_strict(&message, &signature).is_ok()
    }
}

/// A signing key; it lives only on the build host.
pub struct SecretKey(SigningKey);

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretKey")
            .field(&self.public().fingerprint())
            .finish()
    }
}

impl SecretKey {
    /// A new key from the operating system's randomness.
    ///
    /// # Errors
    /// No randomness is available.
    pub fn generate() -> Result<Self, KeyError> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed)
            .map_err(|error| KeyError(format!("no randomness for a new key: {error}")))?;
        let key = SigningKey::from_bytes(&seed);
        seed.fill(0);
        Ok(Self(key))
    }

    /// The key with this seed; for tests and fixtures.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    /// # Errors
    /// Not a PEM `PRIVATE KEY`, or not an Ed25519 key.
    pub fn from_pem(text: &str) -> Result<Self, KeyError> {
        let der = pem_body(text, "PRIVATE KEY")?;
        let seed = key_bytes(&der, &PKCS8_PREFIX)
            .ok_or_else(|| KeyError("not an Ed25519 private key".to_owned()))?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    pub fn to_pem(&self) -> String {
        let mut der = PKCS8_PREFIX.to_vec();
        der.extend_from_slice(self.0.as_bytes());
        let text = pem("PRIVATE KEY", &der);
        der.fill(0);
        text
    }

    pub fn public(&self) -> PublicKey {
        PublicKey(self.0.verifying_key())
    }

    /// The `.sig` text for `manifest`: base64 and a newline.
    pub fn sign(&self, manifest: &[u8]) -> String {
        let mut message = SIGNING_DOMAIN.to_vec();
        message.extend_from_slice(manifest);
        format!("{}\n", STANDARD.encode(self.0.sign(&message).to_bytes()))
    }
}

/// One enrolled extension authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authority {
    /// The file stem of its `.pub`, e.g. `dev`.
    pub name: String,
    pub key: PublicKey,
}

/// Whether `name` can name an authority (and its `.pub` file).
pub fn is_authority_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// The authorities enrolled in `dir`, by name, and why any other `.pub` there
/// was not taken. A key counts only as a regular file owned by `owner`
/// (root on a device) that no one else can write: a key anyone else could
/// replace would make them an authority.
#[cfg(unix)]
pub fn load_authorities(dir: &Path, owner: u32) -> (Vec<Authority>, Vec<String>) {
    let mut authorities = Vec::new();
    let mut problems = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return (authorities, problems);
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".pub"))
            .map(str::to_owned)
        else {
            continue;
        };
        let shown = path.display();
        if !is_authority_name(&name) {
            problems.push(format!("{shown}: not an authority name (a-z, 0-9, -)"));
            continue;
        }
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) => {
                problems.push(format!("{shown}: {error}"));
                continue;
            }
        };
        if !meta.file_type().is_file() || meta.uid() != owner || meta.mode() & 0o022 != 0 {
            problems.push(format!(
                "{shown}: not a regular file owned by uid {owner} that only its owner can write"
            ));
            continue;
        }
        if meta.len() > MAX_KEY_BYTES {
            problems.push(format!("{shown}: too large to be a key"));
            continue;
        }
        match fs::read_to_string(&path)
            .map_err(|error| KeyError(error.to_string()))
            .and_then(|text| PublicKey::from_pem(&text))
        {
            Ok(key) => authorities.push(Authority { name, key }),
            Err(error) => problems.push(format!("{shown}: {error}")),
        }
    }
    (authorities, problems)
}

/// The first of `authorities` whose signature `signature` is over `manifest`.
pub fn signed_by<'a>(
    authorities: &'a [Authority],
    manifest: &[u8],
    signature: &str,
) -> Option<&'a Authority> {
    authorities
        .iter()
        .find(|authority| authority.key.verifies(manifest, signature))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn keys_round_trip_through_pem_and_sign_only_the_manifest() {
        let key = SecretKey::from_seed(&[7; 32]);
        let again = SecretKey::from_pem(&key.to_pem()).unwrap();
        assert_eq!(again.public(), key.public());
        let public = PublicKey::from_pem(&key.public().to_pem()).unwrap();
        assert_eq!(public, key.public());
        assert!(
            key.public()
                .to_pem()
                .starts_with("-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA")
        );
        assert!(key.public().fingerprint().starts_with("sha256:"));
        assert_eq!(key.public().fingerprint().len(), 7 + 64);

        let signature = key.sign(b"manifest");
        assert!(public.verifies(b"manifest", &signature));
        assert!(!public.verifies(b"manifest ", &signature), "changed bytes");
        let other = SecretKey::from_seed(&[8; 32]);
        assert!(
            !other.public().verifies(b"manifest", &signature),
            "another key"
        );
        // The bare message, without the domain, is not what was signed.
        let bare = format!(
            "{}\n",
            STANDARD.encode(
                SigningKey::from_bytes(&[7; 32])
                    .sign(b"manifest")
                    .to_bytes()
            )
        );
        assert!(!public.verifies(b"manifest", &bare));
        assert!(!public.verifies(b"manifest", "not base64"));
        assert!(PublicKey::from_pem("garbage").is_err());
        assert!(SecretKey::from_pem(&key.public().to_pem()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn only_owner_written_keys_are_authorities() {
        let dir = tempfile::tempdir().unwrap();
        let owner = fs::metadata(dir.path()).unwrap().uid();
        let dev = SecretKey::from_seed(&[1; 32]).public();
        fs::write(dir.path().join("dev.pub"), dev.to_pem()).unwrap();
        fs::set_permissions(
            dir.path().join("dev.pub"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let loose = SecretKey::from_seed(&[2; 32]).public();
        fs::write(dir.path().join("loose.pub"), loose.to_pem()).unwrap();
        fs::set_permissions(
            dir.path().join("loose.pub"),
            fs::Permissions::from_mode(0o666),
        )
        .unwrap();
        fs::write(dir.path().join("Bad Name.pub"), dev.to_pem()).unwrap();
        fs::write(dir.path().join("README"), "not a key").unwrap();
        let (authorities, problems) = load_authorities(dir.path(), owner);
        assert_eq!(authorities.len(), 1);
        assert_eq!(authorities[0].name, "dev");
        assert_eq!(authorities[0].key, dev);
        assert_eq!(problems.len(), 2, "{problems:?}");
        let (none, _) = load_authorities(dir.path(), owner.wrapping_add(1));
        assert!(
            none.is_empty(),
            "a key another account owns is no authority"
        );
        let (missing, problems) = load_authorities(&dir.path().join("absent"), owner);
        assert!(missing.is_empty() && problems.is_empty());
    }
}
