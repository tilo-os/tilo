//! The portable Model Providers application; all UI is shared with Settings.
//!
//! A shim, deliberately: the panel is `provider-settings-ui`, mounted here as
//! a window of its own and inside Settings' Agent models page. What it shows
//! is one surface with two doors, so a copy fix or a missing affordance is
//! made once, there — never here, and never twice.
pub use provider_settings_ui::ProviderSettingsApp;

/// The shared application definition registered by native and browser hosts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    provider_settings_ui::definition()
}
