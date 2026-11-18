//! The camera's resource lives in `camera-model`; this module is the app's window onto it and
//! the tests that run its verbs through the action API.

pub use camera_model::camera::{Camera, MAX_SCANS, SharedCapture, capture_resource_id};

#[cfg(test)]
mod tests;
