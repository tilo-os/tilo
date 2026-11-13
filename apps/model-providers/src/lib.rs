//! The portable Model Providers application; all UI is shared with Settings.
pub use provider_settings_ui::ProviderSettingsApp;

/// The shared application definition registered by native and browser hosts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    provider_settings_ui::definition()
}
