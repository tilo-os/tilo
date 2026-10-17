//! Separate selected-app data consumer admission. Home and account proofs grant nothing here.
use serde::{Deserialize, Serialize};
use std::io;
pub const SUBSET_PROTOCOL: u32 = 2;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerCapability {
    pub schema: u32,
    pub subset_protocol: u32,
    pub prepared_helper_sha256: String,
    pub qualification_script_sha256: String,
    pub native_qualification_sha256: String,
}
impl ConsumerCapability {
    /// # Errors
    /// Refuses unsupported protocols and malformed exact artifact digests.
    pub fn validate(&self) -> io::Result<()> {
        let digest = |v: &str| {
            v.len() == 64
                && v.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if self.schema != 1
            || self.subset_protocol != SUBSET_PROTOCOL
            || !digest(&self.prepared_helper_sha256)
            || !digest(&self.qualification_script_sha256)
            || !digest(&self.native_qualification_sha256)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Application consumer proof is invalid",
            ));
        }
        Ok(())
    }
}
/// Schema1 system archives preserve their established admission. Selected app data
/// requires its own selected authenticated image proof, never a Home proof.
/// # Errors
/// Refuses missing or incompatible selected consumers.
pub fn require(capability: Option<&ConsumerCapability>, required: Option<u32>) -> io::Result<()> {
    if let Some(required) = required {
        let capability = capability.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "The selected system cannot import selected application settings",
            )
        })?;
        capability.validate()?;
        if required != capability.subset_protocol {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The selected system cannot import selected application settings",
            ));
        }
    }
    Ok(())
}
/// Authenticate the actual installed consumer proof before target custody writes.
/// The check re-executes the installed probes and compares the image receipt.
/// # Errors
/// Refuses old/missing consumers, untrusted artifacts, or failed native checks.
#[cfg(target_os = "linux")]
pub fn installed() -> io::Result<ConsumerCapability> {
    use crate::installed_consumers::data;
    let value = data::applications(&data::Store::installed(), true, &mut data::invoke)
        .map_err(|_| unavailable())?;
    let proof: ConsumerCapability = serde_json::from_value(value).map_err(|_| unavailable())?;
    proof.validate()?;
    Ok(proof)
}
#[cfg(target_os = "linux")]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "The installed system cannot use the selected applications",
    )
}
