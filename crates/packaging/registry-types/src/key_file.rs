//! The publisher key file, `{"secret_key_hex": "<64 hex digits>"}`: the one
//! form an Ed25519 signing key takes outside a secret store — a file a
//! command names, a CI secret, a backup. Every tool that reads or writes a
//! key does it here, so a key one tool wrote is a key every other reads.

use std::path::{Path, PathBuf};

use ed25519_dalek::SigningKey;
use serde::Deserialize;

/// Why a key file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum KeyFileError {
    #[error("read key file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("a key file is {{\"secret_key_hex\": \"<64 hex digits>\"}}: {0}")]
    Shape(#[source] serde_json::Error),
    #[error("a key file's secret_key_hex is 32 bytes of hex")]
    Seed,
}

#[derive(Deserialize)]
struct KeyFile {
    secret_key_hex: String,
}

/// The key in a key file's contents.
///
/// # Errors
///
/// When `text` is not a key file holding a 32-byte seed.
pub fn parse(text: &str) -> Result<SigningKey, KeyFileError> {
    let file: KeyFile = serde_json::from_str(text).map_err(KeyFileError::Shape)?;
    let seed: [u8; 32] = hex::decode(file.secret_key_hex.trim())
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(KeyFileError::Seed)?;
    Ok(SigningKey::from_bytes(&seed))
}

/// The key in the key file at `path`.
///
/// # Errors
///
/// When the file cannot be read or is no key file.
pub fn load(path: &Path) -> Result<SigningKey, KeyFileError> {
    let text = std::fs::read_to_string(path).map_err(|source| KeyFileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse(&text)
}

/// `key` as a key file's contents, which [`parse`] reads back.
#[must_use]
pub fn encode(key: &SigningKey) -> String {
    format!(
        "{{\"secret_key_hex\":\"{}\"}}\n",
        hex::encode(key.to_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_file_round_trips_and_nothing_else_is_a_key() {
        let key = SigningKey::from_bytes(&[7; 32]);
        assert_eq!(parse(&encode(&key)).unwrap().to_bytes(), key.to_bytes());
        assert!(matches!(
            parse(&hex::encode(key.to_bytes())),
            Err(KeyFileError::Shape(_))
        ));
        assert!(matches!(
            parse(r#"{"secret_key_hex": "0707"}"#),
            Err(KeyFileError::Seed)
        ));
    }
}
