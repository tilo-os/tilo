//! The portable Model Providers application: a window over the panel `provider-settings-ui`
//! shares with Settings' Agent models page, so a fix there reaches both doors.
pub use provider_settings_ui::ProviderSettingsApp;

/// The shared application definition registered by native and browser hosts.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    provider_settings_ui::definition()
}
