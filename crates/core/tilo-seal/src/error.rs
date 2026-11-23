//! One error type for every sealing operation.

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
    #[error("unsupported sealed-data algorithm {0}")]
    UnsupportedAlgorithm(u8),
    #[error("sealed under a different key")]
    WrongKey,
    /// Authentication failed: wrong key, wrong label/context/AAD, or the
    /// bytes were modified.
    #[error("authentication failed")]
    Authentication,
    #[error("invalid key")]
    InvalidKey,
    #[error("unacceptable password parameters: {0}")]
    PasswordParams(&'static str),
    #[error("randomness unavailable")]
    Randomness,
    #[error("sealing failed")]
    Seal,
}

pub type Result<T> = std::result::Result<T, Error>;
