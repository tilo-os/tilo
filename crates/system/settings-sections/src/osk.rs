//! The on-screen keyboard's layouts, which Settings keeps for it.

use ui::tilo_resource;

/// The app whose settings the layouts are: the keyboard reads them as its
/// own.
pub const KEYBOARD_APP_ID: &str = "so.tilo.osk";

/// The letter layouts the on-screen keyboard switches between, and the one
/// it types with.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, ui::tilo_resource::Model,
)]
#[serde(default)]
pub struct OskSourceSettings {
    /// The layout it types with.
    #[model(readonly)]
    pub layout: String,
    /// The layouts it switches between, in order.
    #[model(readonly)]
    pub layouts: Vec<String>,
}

impl Default for OskSourceSettings {
    fn default() -> Self {
        Self {
            layout: "qwerty".into(),
            layouts: Vec::new(),
        }
    }
}

impl ui::storage::SettingsSchema for OskSourceSettings {
    const SCOPE: ui::storage::Scope = ui::storage::Scope::Personal;
    const APP: Option<&'static str> = Some(KEYBOARD_APP_ID);
}

#[ui::tilo_resource::resource(kind = "settings.osk-sources", label = "On-screen keyboard layouts")]
impl OskSourceSettings {}
