//! Portable manifest parsing and metadata; no native or authentication authority.
pub use crate::bundle::{
    ComponentView, InternalBundle, MAX_MANIFEST_BYTES, validate_internal_bundle,
};
