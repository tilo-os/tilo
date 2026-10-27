//! The person's and the device's settings sections as resource models:
//! appearance, accessibility, input, power and the rest. Each is a section
//! of the settings document and a record published as `settings:<kind>`
//! with its field edits and verbs. The session keeps them
//! ([`ui::framework::SettingsRecords`]), so the palette, agents and scripts
//! change them whether or not Settings is open; Settings' window is a view
//! of them.

pub mod accessibility;
pub mod continuity;
pub mod desktop;
pub mod device_vpn;
pub mod osk;
pub mod prefs;
pub mod providers;
pub mod shortcuts;

/// The app whose settings they are, whose controls change them and under
/// which a change is admitted.
pub const APP_ID: &str = "so.tilo.settings";

/// The app's title, which the person's steps on their sections name.
pub const APP_TITLE: &str = "Settings";

/// Every section the session keeps, once: what [`declare`] declares and
/// [`kept`] lists.
macro_rules! sections {
    ($($section:ty),* $(,)?) => {
        /// Declare every section the session keeps.
        pub fn declare(records: &mut ui::framework::Records<'_>) {
            records$(.settings::<$section>())*;
        }

        /// Every section the session keeps: its kind and its stored shape,
        /// each field with what it declares it may hold.
        #[must_use]
        pub fn kept() -> Vec<(&'static str, ui::tilo_resource::Shape)> {
            vec![$((
                <$section as ui::tilo_resource::Resource>::KIND,
                <$section as ui::tilo_resource::Model>::SHAPE,
            )),*]
        }
    };
}

sections!(
    prefs::AppearancePrefs,
    accessibility::AccessibilitySettings,
    prefs::InputPrefs,
    ui::text_assistance::TextAssistancePrefs,
    prefs::PowerPrefs,
    prefs::DeskWidgetsPrefs,
    prefs::UpdatePrefs,
    prefs::NotificationPrefs,
    prefs::AudioPrefs,
    prefs::DisplayPrefs,
    tilo_locale::Locale,
    continuity::ContinuityPrivacy,
    shortcuts::ShortcutPrefs,
    desktop::DesktopControls,
    providers::AgentProviders,
    desktop::DefaultApps,
    desktop::ClockPrefs,
    desktop::SharedDisplayPrefs,
    desktop::AutomaticDisplayPrefs,
    osk::OskSourceSettings,
    device_vpn::DeviceVpnPrefs,
);

#[cfg(test)]
mod declared_tests;
