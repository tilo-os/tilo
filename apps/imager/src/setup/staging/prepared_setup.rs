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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NoBrowserTransfer {}

#[cfg(not(target_os = "linux"))]
impl imager_backend::staging::browser::Expectation for NoBrowserTransfer {
    fn validate(&self) -> std::io::Result<()> {
        match *self {}
    }
    fn filename(&self) -> &str {
        match *self {}
    }
    fn sealed_bytes(&self) -> u64 {
        match *self {}
    }
    fn ciphertext_sha256(&self) -> [u8; 32] {
        match *self {}
    }
}
