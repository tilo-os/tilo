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
