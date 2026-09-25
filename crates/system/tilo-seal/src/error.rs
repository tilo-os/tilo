//! One error type for every sealing operation.
//!
//! Variants say *which check failed* and never carry key material,
//! plaintext, or the bytes that failed to authenticate.

/// Failure while sealing or opening.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The bytes are not a well-formed sealed value (too short, bad magic,
    /// trailing bytes, impossible lengths).
    #[error("malformed sealed data: {0}")]
    Malformed(&'static str),
    /// The format version byte names a layout this build does not read.
    #[error("unsupported sealed-data version {0}")]
    UnsupportedVersion(u8),
    /// The algorithm or key-source identifier is unknown.
    #[error("unsupported sealed-data algorithm {0}")]
    UnsupportedAlgorithm(u8),
    /// The envelope was sealed under a different key (its key id differs).
    #[error("sealed under a different key")]
    WrongKey,
    /// Authentication failed: wrong key, wrong label/context/AAD, or the
    /// bytes were modified.
    #[error("authentication failed")]
    Authentication,
    /// A key had the wrong length or is a degenerate public key.
    #[error("invalid key")]
    InvalidKey,
    /// Password-derivation parameters are outside the accepted bounds.
    #[error("unacceptable password parameters: {0}")]
    PasswordParams(&'static str),
    /// The operating system could not supply randomness.
    #[error("randomness unavailable")]
    Randomness,
    /// The underlying primitive rejected an operation it should accept.
    #[error("sealing failed")]
    Seal,
}

/// Crate-wide result alias.
pub type Result<T> = std::result::Result<T, Error>;
