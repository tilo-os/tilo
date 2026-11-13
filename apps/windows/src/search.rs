//! Where Windows apps are found.

pub const APP_PREFIX: &str = "windows.app:";

/// The published row of app `id`.
pub fn app_resource_id(id: &str) -> String {
    format!("{APP_PREFIX}{id}")
}
