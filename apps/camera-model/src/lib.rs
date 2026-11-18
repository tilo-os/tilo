//! The `camera` resource: the one recent capture it hands off and the codes it scanned.

pub mod camera;
pub mod media;
pub mod model;

pub use camera::{Camera, MAX_SCANS, SharedCapture, capture_resource_id};
pub use media::{MEDIA_PREFIX, is_media_id};
pub use model::*;
