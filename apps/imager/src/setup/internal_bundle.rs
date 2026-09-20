//! Portable format compatibility and one-shot local verification.
pub use ui::install_artifacts::format::{
    ComponentView, InternalBundle, MAX_MANIFEST_BYTES, validate_internal_bundle,
};
pub use ui::install_artifacts::verify_local_components as verify_internal_components;
