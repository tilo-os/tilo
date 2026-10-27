use sha2::{Digest as _, Sha256};
use unicode_normalization::UnicodeNormalization as _;
use zeroize::Zeroizing;

const AUTH_VALUE_DOMAIN: &[u8] = b"tilo-signin-v1\0";

/// A secret a person typed, in Unicode NFC so the same characters typed on
/// another keyboard layout or input method give the same bytes.
pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(typed: &str) -> Self {
        Self(Zeroizing::new(typed.nfc().collect()))
    }

    /// The secret as a file holds it: one trailing newline is not part of it.
    pub fn from_file_bytes(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        Some(Self::new(text.strip_suffix('\n').unwrap_or(text)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The TPM authorisation value of the person's sealed user key.
    pub fn auth_value(&self) -> Zeroizing<[u8; 32]> {
        let mut hasher = Sha256::new();
        hasher.update(AUTH_VALUE_DOMAIN);
        hasher.update(self.0.as_bytes());
        Zeroizing::new(hasher.finalize().into())
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composed_and_decomposed_input_is_one_secret() {
        let composed = Secret::new("caf\u{e9}");
        let decomposed = Secret::new("cafe\u{301}");
        assert_eq!(composed.as_bytes(), decomposed.as_bytes());
        assert_eq!(*composed.auth_value(), *decomposed.auth_value());
        assert_ne!(*composed.auth_value(), *Secret::new("cafe").auth_value());
    }

    #[test]
    fn the_auth_value_is_the_contract_digest() {
        let mut hasher = Sha256::new();
        hasher.update(b"tilo-signin-v1\x00123456");
        let expected: [u8; 32] = hasher.finalize().into();
        assert_eq!(*Secret::new("123456").auth_value(), expected);
    }

    #[test]
    fn a_file_loses_one_trailing_newline_only() {
        assert_eq!(Secret::from_file_bytes(b"pin\n").unwrap().as_str(), "pin");
        assert_eq!(
            Secret::from_file_bytes(b"pin\n\n").unwrap().as_str(),
            "pin\n"
        );
        assert_eq!(Secret::from_file_bytes(b"pin").unwrap().as_str(), "pin");
        assert!(Secret::from_file_bytes(&[0xff]).is_none());
    }
}
