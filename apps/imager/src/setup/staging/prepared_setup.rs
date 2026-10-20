//! Ciphertext-only input to the native staging transaction.
pub(in crate::setup) use imager_backend::staging::prepared_setup::caller_uid;
pub(super) use imager_backend::staging::prepared_setup::verify;
pub use imager_backend::staging::prepared_setup::{
    PACKAGE_PATH, RECEIPT_BYTES, RECEIPT_PATH, SLOT_BYTES,
};
/// The prepared artifact with the browser expectation this host's media
/// carry (none outside Linux), chosen by the backend.
pub type PreparedArtifact = imager_backend::staging::prepared_setup::PreparedArtifact<
    imager_backend::staging::browser::HostExpectation,
>;
