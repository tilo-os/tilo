//! What a record keeps to name content: its reference.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tilo_seal::SymmetricKey;

use crate::valid_hash;

const SEPARATOR: char = '.';
const KEY_CHARS: usize = 43;

/// A parsed reference: the address content is stored under, and the key
/// sealed content opens with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRef {
    address: String,
    key: Option<SymmetricKey>,
}

impl ContentRef {
    #[must_use]
    pub fn parse(reference: &str) -> Option<Self> {
        let reference = reference.trim();
        let (address, key) = match reference.split_once(SEPARATOR) {
            Some((address, key)) => (address, Some(decode_key(key)?)),
            None => (reference, None),
        };
        valid_hash(address).then(|| Self {
            address: address.to_ascii_lowercase(),
            key,
        })
    }

    #[must_use]
    pub fn sealed(address: &str, key: SymmetricKey) -> Self {
        Self {
            address: address.to_ascii_lowercase(),
            key: Some(key),
        }
    }

    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    #[must_use]
    pub const fn key(&self) -> Option<&SymmetricKey> {
        self.key.as_ref()
    }

    #[must_use]
    pub const fn is_sealed(&self) -> bool {
        self.key.is_some()
    }
}

impl std::fmt::Display for ContentRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.address)?;
        if let Some(key) = &self.key {
            write!(
                f,
                "{SEPARATOR}{}",
                URL_SAFE_NO_PAD.encode(key.expose_secret())
            )?;
        }
        Ok(())
    }
}

fn decode_key(key: &str) -> Option<SymmetricKey> {
    if key.len() != KEY_CHARS {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(key).ok()?;
    SymmetricKey::from_slice(&bytes).ok()
}

#[must_use]
pub fn valid_reference(reference: &str) -> bool {
    ContentRef::parse(reference).is_some()
}

/// The address `reference` names its content by, as stored and carried
/// (lower case): what a host keeps, serves and asks for. `None` for no
/// reference.
#[must_use]
pub fn content_address(reference: &str) -> Option<String> {
    ContentRef::parse(reference).map(|parsed| parsed.address)
}

#[must_use]
pub fn is_sealed(reference: &str) -> bool {
    ContentRef::parse(reference).is_some_and(|parsed| parsed.is_sealed())
}
