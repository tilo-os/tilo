//! Inert fixed-width staging metadata shared by store custody and the HTTP adapter.
//! A valid file record proves neither current authorization nor native admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageIdentity {
    pub workspace: u64,
    pub device: [u8; 32],
    pub delivery: [u8; 16],
    pub digest: [u8; 32],
    pub total: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StageManifest {
    pub(crate) identity: StageIdentity,
    pub(crate) prefix: u64,
    pub(crate) expires_at_ns: u64,
    pub(crate) prefix_digest: [u8; 32],
}
#[derive(Debug)]
pub(crate) struct StageManifestError;
impl std::fmt::Display for StageManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid bounded staging metadata")
    }
}
impl std::error::Error for StageManifestError {}
impl StageManifest {
    fn validate(&self) -> Result<(), StageManifestError> {
        let identity = &self.identity;
        if identity.workspace == 0
            || identity.device == [0; 32]
            || identity.delivery == [0; 16]
            || identity.digest == [0; 32]
            || identity.total == 0
            || identity.total > 64 * 1024 * 1024
            || self.prefix > identity.total
            || self.expires_at_ns == 0
            || (self.prefix == 0 && self.prefix_digest != *blake3::hash(&[]).as_bytes())
        {
            return Err(StageManifestError);
        }
        Ok(())
    }
    /// Encode fixed widths without architecture-dependent padding or native-endian fields.
    pub(crate) fn encode(&self) -> Result<[u8; 148], StageManifestError> {
        self.validate()?;
        let mut bytes = [0; 148];
        bytes[..4].copy_from_slice(b"QUP1");
        bytes[4..12].copy_from_slice(&self.identity.workspace.to_le_bytes());
        bytes[12..44].copy_from_slice(&self.identity.device);
        bytes[44..60].copy_from_slice(&self.identity.delivery);
        bytes[60..92].copy_from_slice(&self.identity.digest);
        bytes[92..100].copy_from_slice(&self.identity.total.to_le_bytes());
        bytes[100..108].copy_from_slice(&self.prefix.to_le_bytes());
        bytes[108..116].copy_from_slice(&self.expires_at_ns.to_le_bytes());
        bytes[116..148].copy_from_slice(&self.prefix_digest);
        Ok(bytes)
    }
    /// Decode inert metadata; the store additionally checks file identity and actual bytes.
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, StageManifestError> {
        let bytes: &[u8; 148] = bytes.try_into().map_err(|_| StageManifestError)?;
        if &bytes[..4] != b"QUP1" {
            return Err(StageManifestError);
        }
        let value = Self {
            identity: StageIdentity {
                workspace: u64::from_le_bytes(bytes[4..12].try_into().unwrap()),
                device: bytes[12..44].try_into().unwrap(),
                delivery: bytes[44..60].try_into().unwrap(),
                digest: bytes[60..92].try_into().unwrap(),
                total: u64::from_le_bytes(bytes[92..100].try_into().unwrap()),
            },
            prefix: u64::from_le_bytes(bytes[100..108].try_into().unwrap()),
            expires_at_ns: u64::from_le_bytes(bytes[108..116].try_into().unwrap()),
            prefix_digest: bytes[116..148].try_into().unwrap(),
        };
        value.validate()?;
        Ok(value)
    }
}
#[cfg(test)]
mod tests;
