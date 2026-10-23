//! The signed release feed and the checks on what it names.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read},
    path::Path,
};

/// Why an update was refused.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the release feed signature does not verify")]
    Signature,
    #[error("malformed {0}")]
    Malformed(&'static str),
    #[error("the archive's SHA-256 is {actual}, the feed names {expected}")]
    Digest { expected: String, actual: String },
    #[error("the release archive must be an https URL")]
    InsecureUrl,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// One release of the bundle, as the feed describes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    /// The bundle's `CFBundleShortVersionString`, dotted numbers.
    pub version: String,
    /// Where the zipped `Tilo.app` is downloaded from.
    pub url: String,
    /// Lowercase hex SHA-256 of that archive.
    pub sha256: String,
}

impl Release {
    /// Parse `feed` after verifying `signature_hex` over its exact bytes with
    /// `public_key_hex`. Nothing in an unverified feed is looked at.
    pub fn verify(feed: &[u8], signature_hex: &str, public_key_hex: &str) -> Result<Self, Error> {
        let key: [u8; 32] = decode_hex(public_key_hex).ok_or(Error::Malformed("public key"))?;
        let key = VerifyingKey::from_bytes(&key).map_err(|_| Error::Malformed("public key"))?;
        let signature: [u8; 64] = decode_hex(signature_hex).ok_or(Error::Malformed("signature"))?;
        key.verify_strict(feed, &Signature::from_bytes(&signature))
            .map_err(|_| Error::Signature)?;
        let release: Self =
            serde_json::from_slice(feed).map_err(|_| Error::Malformed("release feed"))?;
        if !release.url.starts_with("https://") {
            return Err(Error::InsecureUrl);
        }
        if decode_hex::<32>(&release.sha256).is_none() {
            return Err(Error::Malformed("archive digest"));
        }
        Ok(release)
    }

    /// Whether this release is newer than `current`. A version that is not
    /// dotted numbers is never newer.
    #[must_use]
    pub fn is_newer_than(&self, current: &str) -> bool {
        match (numeric_version(&self.version), numeric_version(current)) {
            (Some(candidate), Some(current)) => candidate > current,
            _ => false,
        }
    }

    /// Check the downloaded archive at `path` against the feed's digest.
    ///
    /// # Errors
    ///
    /// The archive cannot be read or has another digest.
    pub fn check_archive(&self, path: &Path) -> Result<(), Error> {
        let mut hasher = Sha256::new();
        let mut file = std::fs::File::open(path)?;
        let mut buffer = vec![0; 1 << 16];
        loop {
            match file.read(&mut buffer)? {
                0 => break,
                read => hasher.update(buffer.get(..read).unwrap_or_default()),
            }
        }
        let actual = hex::encode(hasher.finalize());
        if actual == self.sha256.to_ascii_lowercase() {
            Ok(())
        } else {
            Err(Error::Digest {
                expected: self.sha256.clone(),
                actual,
            })
        }
    }
}

/// The detached signature over `feed`, hex-encoded, for the release step.
#[must_use]
pub fn sign(feed: &[u8], key: &SigningKey) -> String {
    hex::encode(key.sign(feed).to_bytes())
}

/// A signing key from a publisher key file (`{"secret_key_hex": "…"}`, what
/// `tilo-app keys new` and `tilo-runtime-bundle key generate` write) or its
/// bare hex seed.
pub fn parse_signing_key(text: &str) -> Result<SigningKey, Error> {
    #[derive(Deserialize)]
    struct KeyFile {
        secret_key_hex: String,
    }
    let text = text.trim();
    let seed = if text.starts_with('{') {
        serde_json::from_str::<KeyFile>(text)
            .map_err(|_| Error::Malformed("signing key file"))?
            .secret_key_hex
    } else {
        text.to_owned()
    };
    decode_hex(&seed)
        .map(|seed| SigningKey::from_bytes(&seed))
        .ok_or(Error::Malformed("signing key"))
}

fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    hex::decode(text.trim()).ok()?.try_into().ok()
}

fn numeric_version(version: &str) -> Option<Vec<u64>> {
    version.split('.').map(|part| part.parse().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn feed(url: &str) -> Vec<u8> {
        serde_json::to_vec(&Release {
            version: "0.2.0".into(),
            url: url.into(),
            sha256: hex::encode(Sha256::digest(b"archive")),
        })
        .unwrap()
    }

    fn public() -> String {
        hex::encode(key().verifying_key().to_bytes())
    }

    #[test]
    fn a_feed_verifies_only_with_its_exact_bytes_and_key() {
        let feed = feed("https://example.test/Tilo.zip");
        let signature = sign(&feed, &key());
        assert_eq!(
            Release::verify(&feed, &signature, &public())
                .unwrap()
                .version,
            "0.2.0"
        );

        let mut tampered = feed.clone();
        tampered[2] ^= 1;
        assert!(matches!(
            Release::verify(&tampered, &signature, &public()),
            Err(Error::Signature)
        ));
        let other = hex::encode(SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes());
        assert!(matches!(
            Release::verify(&feed, &signature, &other),
            Err(Error::Signature)
        ));
        assert!(matches!(
            Release::verify(&feed, "zz", &public()),
            Err(Error::Malformed("signature"))
        ));
    }

    #[test]
    fn a_signed_feed_must_name_an_https_archive() {
        let feed = feed("http://example.test/Tilo.zip");
        assert!(matches!(
            Release::verify(&feed, &sign(&feed, &key()), &public()),
            Err(Error::InsecureUrl)
        ));
    }

    #[test]
    fn only_a_numerically_higher_version_is_newer() {
        let release = |version: &str| Release {
            version: version.into(),
            url: String::new(),
            sha256: String::new(),
        };
        assert!(release("0.1.13").is_newer_than("0.1.12"));
        assert!(release("0.10.0").is_newer_than("0.9.9"));
        assert!(!release("0.1.12").is_newer_than("0.1.12"));
        assert!(!release("0.1.11").is_newer_than("0.1.12"));
        assert!(!release("0.2.0-beta").is_newer_than("0.1.0"));
        assert!(!release("0.2.0").is_newer_than("dev"));
    }

    #[test]
    fn the_archive_must_have_the_feed_digest() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("Tilo.zip");
        std::fs::write(&archive, b"archive").unwrap();
        let release: Release = serde_json::from_slice(&feed("https://x.test/a")).unwrap();
        release.check_archive(&archive).unwrap();
        std::fs::write(&archive, b"other").unwrap();
        assert!(matches!(
            release.check_archive(&archive),
            Err(Error::Digest { .. })
        ));
    }

    #[test]
    fn signing_keys_parse_from_a_key_file_or_bare_hex() {
        let seed = hex::encode([7; 32]);
        let from_file = parse_signing_key(&format!("{{\"secret_key_hex\": \"{seed}\"}}")).unwrap();
        assert_eq!(from_file.to_bytes(), key().to_bytes());
        assert_eq!(
            parse_signing_key(&seed).unwrap().to_bytes(),
            key().to_bytes()
        );
        assert!(parse_signing_key("{}").is_err());
    }
}
