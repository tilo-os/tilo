//! The settings as resources of the workspace: the app they are kept under, the verb that sets
//! each setting (`set-<setting>`), the grants over every setter, and the surface the store checks
//! their writes against. The session keeps them in the store (`settings_sections::workspace`).

use std::path::Path;

use serde_json::{Map, Value};

use crate::placement;

/// The app the settings' resources are kept under.
pub const APP: &str = tilo_app_id::apps::SETTINGS;

/// The app id the settings' declared kinds and verbs are named under: every section's kind, and
/// the family of its setters.
pub use tilo_actions::remote::settings_resources::SURFACE_APP;

/// The verb a setting `leaf` is written by.
pub use tilo_actions::surface::setter_id as verb;

/// The capability a package's signed manifest asks for to set any setting of the person and the
/// device: the image grants it to its settings app alone, as [`EVERY_SETTER`].
pub const SET_EVERY_SETTING: &str = "settings.system";

/// Every setter of the person's and the device's settings, as grant patterns over their
/// addresses (`*` standing for any text within one segment).
pub const EVERY_SETTER: &[&str] = &["settings/*#set-*", "devices/*/settings/*#set-*"];

/// The settings' surface: every declared section's kind, and the app preferences' kind, each
/// written by the family of setters `set-*`, one per setting. What the store checks a write of
/// the settings against, on every device that holds them.
#[must_use]
pub fn surface_json() -> &'static str {
    static JSON: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    JSON.get_or_init(|| {
        let kinds = placement::SECTIONS
            .iter()
            .filter(|(section, _)| !placement::per_app(section))
            .map(|(section, _)| placement::kind(section))
            .chain([placement::APP_KIND.to_owned()]);
        let resources: Vec<Value> = kinds
            .map(|kind| {
                serde_json::json!({
                    "kind": kind,
                    "label": kind,
                    "version": 1,
                    "verbs": [{"id": "set-*", "kind": "undoable"}],
                })
            })
            .collect();
        serde_json::json!({ "resources": resources }).to_string()
    })
}

/// What waits in `file` for a workspace: the sections set while no session held one.
#[must_use]
pub fn waiting(file: &Path) -> Map<String, Value> {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Keep `waiting` in `file` until a workspace takes it, or remove the file once nothing waits.
///
/// # Errors
/// The file could not be written or removed.
pub fn keep_waiting(file: &Path, waiting: &Map<String, Value>) -> std::io::Result<()> {
    if !waiting.is_empty() {
        let bytes = serde_json::to_vec(waiting).map_err(std::io::Error::other)?;
        return tilo_files::native::atomic::write_creating_parent(file, &bytes);
    }
    std::fs::remove_file(file).or_else(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(error),
    })
}
