//! Portable prepared-media contracts and owned authenticated ciphertext custody.
use serde::{Deserialize, Serialize};
pub mod filesystem;
#[cfg(target_os = "linux")]
use std::sync::atomic::Ordering;
use std::{io, path::PathBuf, sync::atomic::AtomicBool};

pub const DIRECTORY: &str = ".tilo-installer";
pub const FILE: &str = "prepared-setup.sealed";
pub const RECEIPT_FILE: &str = "receipt.json";
pub const MAX_PLAINTEXT: u64 = 32 * 1024 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IdentityKind {
    #[default]
    SingleDeviceBackup,
    EnrolledDevice {
        device_public_key: [u8; 32],
    },
    CurrentDevice {
        device_public_key: [u8; 32],
    },
}
impl IdentityKind {
    pub fn is_single(&self) -> bool {
        matches!(self, Self::SingleDeviceBackup)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PortableHomePlan {
    pub schema: u32,
    pub selection: PortableHomeSelection,
    #[serde(default)]
    pub applications: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PortableHomeSelection {
    FullHome,
}

/// Syntax only on every platform: successful validation is not authentication, and native review
/// still verifies the encrypted manifest. Refuses an unsupported schema or protocol version, empty,
/// overlong or control-containing bindings, duplicate ids, more than 64 accounts, or a protocol
/// supplied without accounts.
pub fn validate_manifest_binding(
    schema: u32,
    setup_binding: &str,
    account_ids: &[String],
    account_custody_protocol: Option<u32>,
) -> io::Result<()> {
    let valid_text = |value: &str| {
        !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
    };
    let unique: std::collections::BTreeSet<_> = account_ids.iter().collect();
    if schema != 1
        || !valid_text(setup_binding)
        || account_ids.len() > 64
        || unique.len() != account_ids.len()
        || account_ids.iter().any(|id| !valid_text(id))
        || account_custody_protocol.is_some_and(|protocol| protocol != 1)
        || (account_ids.is_empty() && account_custody_protocol.is_some())
    {
        return Err(io::Error::other(
            "Prepared setup custody is invalid or changed",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct DeliveryReceipt {
    pub relative_path: PathBuf,
    pub sealed_bytes: u64,
    pub ciphertext_sha256: String,
}
impl DeliveryReceipt {
    pub fn expectation(&self) -> serde_json::Value {
        serde_json::json!({"schema": 1, "sealed_bytes": self.sealed_bytes,
            "ciphertext_sha256": self.ciphertext_sha256})
    }
}

#[derive(Debug)]
pub enum PreparedMediaError {
    Unsupported,
    Native(io::Error),
}
impl std::fmt::Display for PreparedMediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => {
                f.write_str("Prepared media custody is unsupported on this platform")
            }
            Self::Native(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for PreparedMediaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Native(error) => Some(error),
            Self::Unsupported => None,
        }
    }
}
impl From<io::Error> for PreparedMediaError {
    fn from(error: io::Error) -> Self {
        Self::Native(error)
    }
}
impl From<PreparedMediaError> for io::Error {
    fn from(error: PreparedMediaError) -> Self {
        match error {
            PreparedMediaError::Native(error) => error,
            PreparedMediaError::Unsupported => Self::new(io::ErrorKind::Unsupported, error),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiphertextInfo {
    pub sealed_bytes: u64,
    pub ciphertext_sha256: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryProgress {
    pub completed: u64,
    pub total: Option<u64>,
}
pub type Observer<'a> = dyn FnMut(DeliveryProgress) -> io::Result<()> + 'a;

#[cfg(target_os = "linux")]
mod native;

#[cfg(target_os = "linux")]
fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Prepared media operation cancelled",
        ));
    }
    Ok(())
}

/// Opaque ownership of password-authenticated ciphertext. Construction and descriptors are private
/// to the backend, and it grants no adoption, import, extraction or install authority.
pub struct PreparedMediaHandle {
    info: CiphertextInfo,
    #[cfg(target_os = "linux")]
    custody: native::Custody,
}
impl PreparedMediaHandle {
    /// Authenticates the complete bounded stream before admitting custody: no password, plaintext or
    /// key survives admission. Archive content policy stays a separate prerequisite for adoption.
    ///
    /// # Errors
    ///
    /// Returns [`PreparedMediaError::Unsupported`] outside Linux, else a native error for
    /// cancellation, unsafe paths, I/O, invalid size or framing, failed password, another package
    /// purpose, out-of-bounds footer metadata or changed ciphertext.
    pub fn review(
        package: &std::path::Path,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> Result<Self, PreparedMediaError> {
        #[cfg(target_os = "linux")]
        {
            native::review(package, password, cancel).map_err(Into::into)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (package, password, cancel);
            Err(PreparedMediaError::Unsupported)
        }
    }
    pub fn info(&self) -> &CiphertextInfo {
        &self.info
    }
    pub fn ciphertext_digest(&self) -> [u8; 32] {
        self.info.ciphertext_sha256
    }
    /// Rechecks the held file's length and digest against the authenticated review.
    ///
    /// # Errors
    ///
    /// Returns [`PreparedMediaError::Unsupported`] outside Linux, else a native error for
    /// cancellation, I/O or changed ciphertext.
    pub fn verify_held_ciphertext(
        &mut self,
        cancel: &AtomicBool,
    ) -> Result<(), PreparedMediaError> {
        #[cfg(target_os = "linux")]
        {
            check_cancel(cancel)?;
            self.custody.verify(&self.info, cancel)?;
            Ok(())
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = cancel;
            Err(PreparedMediaError::Unsupported)
        }
    }
    /// Verifies held custody before and after the copy. Output may be partially written on failure.
    pub fn copy_ciphertext(
        &mut self,
        output: &mut std::fs::File,
        cancel: &AtomicBool,
    ) -> Result<[u8; 32], PreparedMediaError> {
        self.copy_ciphertext_reporting(output, cancel, &mut |_| Ok(()))
    }
    /// Copies while reporting progress and rechecking held custody. Output may be partially written
    /// on failure.
    pub fn copy_ciphertext_reporting(
        &mut self,
        output: &mut std::fs::File,
        cancel: &AtomicBool,
        observer: &mut Observer<'_>,
    ) -> Result<[u8; 32], PreparedMediaError> {
        #[cfg(target_os = "linux")]
        {
            self.verify_held_ciphertext(cancel)?;
            let digest = self.custody.copy(&self.info, output, cancel, observer)?;
            self.verify_held_ciphertext(cancel)?;
            if digest != self.info.ciphertext_sha256 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Prepared ciphertext differs from authenticated review",
                )
                .into());
            }
            Ok(digest)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (output, cancel, observer);
            Err(PreparedMediaError::Unsupported)
        }
    }
}

#[cfg(test)]
#[path = "prepared_media/tests.rs"]
mod tests;
