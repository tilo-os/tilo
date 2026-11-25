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

/// Every change of the person's and the device's settings, as grant patterns over their
/// addresses (`*` standing for any text within one segment): each field's setter, and the
/// appearance's own theme verbs, which choose a theme or apply an installed theme package.
pub const EVERY_SETTER: &[&str] = &[
    "settings/*#set-*",
    "devices/*/settings/*#set-*",
    "settings/appearance#choose-theme",
    "settings/appearance#use-theme",
    "settings/appearance#apply-theme",
];

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
        tilo_canonical_json::to_string(&serde_json::json!({ "resources": resources }))
    })
}

/// One change waiting for a workspace, as the device keeps it across runs: the sections it
/// writes, and who made it — which is what makes it theirs to take back in the next run.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitingStep {
    /// The sections it writes, each with the fields it sets.
    pub sections: std::collections::BTreeMap<String, Map<String, Value>>,
    /// The actor that made it, as a change's stamp names it; none for a change that named
    /// nobody, which is nobody's to take back.
    #[serde(default)]
    pub actor: Option<String>,
    /// The action that made it, as its origin names it.
    #[serde(default)]
    pub action: Option<String>,
}

/// The file the settings wait in: its steps, oldest first.
#[derive(serde::Serialize, serde::Deserialize)]
struct WaitingFile {
    steps: Vec<WaitingStep>,
}

/// What waits in `file` for a workspace, oldest change first: the settings set while no session
/// held one, each with the step that made it.
#[must_use]
pub fn waiting(file: &Path) -> Vec<WaitingStep> {
    std::fs::read(file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<WaitingFile>(&bytes).ok())
        .map(|waiting| waiting.steps)
        .unwrap_or_default()
}

/// Keep `steps` in `file` until a workspace takes them, or remove the file once nothing waits.
///
/// # Errors
/// The file could not be written or removed.
pub fn keep_waiting(file: &Path, steps: &[WaitingStep]) -> std::io::Result<()> {
    if !steps.is_empty() {
        let bytes = serde_json::to_vec(&WaitingFile {
            steps: steps.to_vec(),
        })
        .map_err(std::io::Error::other)?;
        return tilo_files::native::atomic::write_creating_parent(file, &bytes);
    }
    std::fs::remove_file(file).or_else(|error| match error.kind() {
        std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(error),
    })
}
