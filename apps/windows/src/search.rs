//! Where Windows apps are found.
//!
//! The catalogue is a live projection of the compatibility service, not
//! document state; the surface's search entries are anchored at each app's
//! live row, and opening the anchor shows the app as choosing its row does.

/// The prefix of an app row's id; the rest is the app's catalogue id.
pub const APP_PREFIX: &str = "windows.app:";

/// The published row of app `id`.
pub fn app_resource_id(id: &str) -> String {
    format!("{APP_PREFIX}{id}")
}
