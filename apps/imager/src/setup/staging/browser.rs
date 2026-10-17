//! Application receipt bridge to native ciphertext-only staging.

use crate::setup::browser_transfer::Expectation;
pub use imager_backend::staging::browser::{
    PACKAGE_PATH, RECEIPT_BYTES, RECEIPT_PATH, SELECTION_BYTES, SELECTION_PATH, SLOT_BYTES, verify,
};

pub type BrowserArtifact = imager_backend::staging::browser::BrowserArtifact<Expectation>;
