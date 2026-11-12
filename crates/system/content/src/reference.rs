//! What a record keeps to name content: its reference.
//!
//! A reference is the content's address, and for sealed content the key it
//! was sealed under: `<address>.<key>`, the address 64 hex digits (BLAKE3 of
//! the bytes stored, the ciphertext for sealed content), the key 32 bytes in
//! unpadded URL-safe base64. Whoever stores or carries content sees the
//! address alone, and holds ciphertext; whoever reads the record that keeps
//! the reference holds the key. A plain reference is the address alone, as
//! content was named before it was sealed, and as public content still is.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use tilo_seal::SymmetricKey;

use crate::valid_hash;

/// Between a sealed reference's address and its key.
const SEPARATOR: char = '.';
/// The length of a key, encoded.
const KEY_CHARS: usize = 43;

/// A parsed reference: the address content is stored under, and the key
/// sealed content opens with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentRef {
    address: String,
    key: Option<SymmetricKey>,
}

impl ContentRef {
    /// `reference` parsed, or `None` when it is no reference.
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

    /// A reference to sealed content stored under `address`, opened by `key`.
    #[must_use]
    pub fn sealed(address: &str, key: SymmetricKey) -> Self {
        Self {
            address: address.to_ascii_lowercase(),
            key: Some(key),
        }
    }

    /// The address the content is stored and carried under (lower case).
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The key sealed content opens with; `None` for plain content.
    #[must_use]
    pub const fn key(&self) -> Option<&SymmetricKey> {
        self.key.as_ref()
    }

    /// Whether the content is sealed.
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

/// Whether `reference` names content: an address, or a sealed reference.
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

/// Whether `reference` names sealed content.
#[must_use]
pub fn is_sealed(reference: &str) -> bool {
    ContentRef::parse(reference).is_some_and(|parsed| parsed.is_sealed())
}
