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
pub mod identity;
pub mod network_usage;
pub mod osk;
pub mod prefs;
pub mod providers;
pub mod search;
pub mod shortcuts;

/// The app whose settings they are, whose controls change them and under
/// which a change is admitted.
pub const APP_ID: &str = "so.tilo.settings";

/// The app's title, which the person's steps on their sections name.
pub const APP_TITLE: &str = "Settings";

/// Bring a section's record of version 1 up to 2: the leaves another section
/// holds now (the device's part of a section the person holds, or the
/// person's of one the device holds) are no longer read, and nothing else
/// changed.
///
/// # Errors
/// Never: there is nothing to rewrite.
#[allow(
    clippy::unnecessary_wraps,
    reason = "the signature every resource migration has"
)]
pub(crate) fn leaves_moved_out(
    _doc: &mut ui::tilo_resource::Doc,
    _from: u32,
) -> Result<(), ui::tilo_resource::ModelError> {
    Ok(())
}

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
    prefs::AppearanceDevicePrefs,
    accessibility::AccessibilitySettings,
    accessibility::AccessibilityDevicePrefs,
    prefs::InputPrefs,
    prefs::InputDevicePrefs,
    ui::text_assistance::TextAssistancePrefs,
    prefs::PowerPrefs,
    prefs::DeskWidgetsPrefs,
    search::SearchHistory,
    prefs::UpdatePrefs,
    prefs::NotificationPrefs,
    prefs::AudioPrefs,
    prefs::AudioDevicePrefs,
    prefs::DisplayPrefs,
    prefs::NightLightPrefs,
    tilo_locale::Locale,
    continuity::ContinuityPrivacy,
    shortcuts::ShortcutPrefs,
    desktop::DesktopControls,
    desktop::DesktopControlsDevice,
    providers::AgentProviders,
    providers::HostedAgentProviders,
    desktop::DefaultApps,
    desktop::ClockPrefs,
    desktop::SharedDisplayPrefs,
    desktop::AutomaticDisplayPrefs,
    desktop::PendingDisplayChange,
    osk::OskSourceSettings,
    device_vpn::DeviceVpnPrefs,
    identity::IdentityPrefs,
    network_usage::NetworkUsage,
);

#[cfg(test)]
mod declared_tests;
