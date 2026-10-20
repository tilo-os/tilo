//! Ciphertext-only input to the native staging transaction.
pub(in crate::setup) use imager_backend::staging::prepared_setup::caller_uid;
pub(super) use imager_backend::staging::prepared_setup::verify;
pub use imager_backend::staging::prepared_setup::{
    PACKAGE_PATH, RECEIPT_BYTES, RECEIPT_PATH, SLOT_BYTES,
};
#[cfg(target_os = "linux")]
pub type PreparedArtifact = imager_backend::staging::prepared_setup::PreparedArtifact<
    crate::setup::browser_transfer::Expectation,
>;
#[cfg(not(target_os = "linux"))]
pub type PreparedArtifact =
    imager_backend::staging::prepared_setup::PreparedArtifact<NoBrowserTransfer>;

/// The browser transfer is Linux's: elsewhere no prepared medium carries a
/// browser artifact, so its expectation cannot exist.
#[cfg(not(target_os = "linux"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoBrowserTransfer(std::convert::Infallible);

#[cfg(not(target_os = "linux"))]
impl serde::Serialize for NoBrowserTransfer {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {}
    }
}

#[cfg(not(target_os = "linux"))]
impl<'de> serde::Deserialize<'de> for NoBrowserTransfer {
    fn deserialize<D: serde::Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom(
            "a browser transfer exists only on Linux",
        ))
    }
}

#[cfg(not(target_os = "linux"))]
impl imager_backend::staging::browser::Expectation for NoBrowserTransfer {
    fn validate(&self) -> std::io::Result<()> {
        match self.0 {}
    }
    fn filename(&self) -> &str {
        match self.0 {}
    }
    fn sealed_bytes(&self) -> u64 {
        match self.0 {}
    }
    fn ciphertext_sha256(&self) -> [u8; 32] {
        match self.0 {}
    }
}
