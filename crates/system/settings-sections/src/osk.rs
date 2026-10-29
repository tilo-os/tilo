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
impl OskSourceSettings {
    /// Switch between these letter layouts, in this order, typing with one
    /// of them.
    #[action(id = "set-layouts")]
    pub fn set_layouts(
        &mut self,
        #[param("`qwerty` and `azerty`, in switching order")] layouts: Vec<String>,
        #[param("The one typed with, one of them")] active: String,
    ) -> Result<(), tilo_resource::Refusal> {
        if let Some(layout) = layouts
            .iter()
            .find(|layout| !LAYOUTS.contains(&layout.as_str()))
        {
            return Err(tilo_resource::Refusal::new(format!(
                "`{layout}` is not a layout of the on-screen keyboard"
            )));
        }
        let mut seen = std::collections::HashSet::new();
        if !layouts.iter().all(|layout| seen.insert(layout)) {
            return Err(tilo_resource::Refusal::new("a layout is listed twice"));
        }
        if !layouts.contains(&active) {
            return Err(tilo_resource::Refusal::new(format!(
                "`{active}` is not one of the layouts"
            )));
        }
        self.layouts = layouts;
        self.layout = active;
        Ok(())
    }
}

/// The letter layouts the on-screen keyboard draws.
pub const LAYOUTS: [&str; 2] = ["qwerty", "azerty"];
