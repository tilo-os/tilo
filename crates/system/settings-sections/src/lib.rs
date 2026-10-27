//! The person's and the device's settings sections as resource models:
//! appearance, accessibility, input, power and the rest. Each is a section
//! of the settings document and a record published as `settings:<kind>`
//! with its field edits and verbs. The session keeps them
//! ([`ui::framework::SettingsRecords`]), so the palette, agents and scripts
//! change them whether or not Settings is open; Settings' window is a view
//! of them.

pub mod accessibility;
pub mod continuity;
pub mod prefs;

/// The app whose settings they are, whose controls change them and under
/// which a change is admitted.
pub const APP_ID: &str = "so.tilo.settings";

/// The app's title, which the person's steps on their sections name.
pub const APP_TITLE: &str = "Settings";

/// Declare every section the session keeps.
pub fn declare(records: &mut ui::framework::Records<'_>) {
    records
        .settings::<prefs::AppearancePrefs>()
        .settings::<accessibility::AccessibilitySettings>()
        .settings::<prefs::InputPrefs>()
        .settings::<ui::text_assistance::TextAssistancePrefs>()
        .settings::<prefs::PowerPrefs>()
        .settings::<prefs::DeskWidgetsPrefs>()
        .settings::<prefs::UpdatePrefs>()
        .settings::<prefs::NotificationPrefs>()
        .settings::<prefs::AudioPrefs>()
        .settings::<prefs::DisplayPrefs>()
        .settings::<tilo_locale::Locale>()
        .settings::<continuity::ContinuityPrivacy>();
}
