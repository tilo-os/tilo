//! Compatibility facade: verification and native pin ownership live in the backend.
pub(super) use crate::capture::runtime::PACKAGE;
pub use crate::capture::runtime::TrustedCaptureService;
pub use crate::capture::runtime::installed_original_consumer_facts;
#[cfg(all(test, feature = "native-capture-qualification"))]
pub(super) use crate::capture::runtime::{CaptureServicePins, verify_sidecar};
