//! The person's and the device's settings sections as resource models:
//! appearance, accessibility, input, power and the rest. Each is a section
//! of the settings document and a record published where the settings keep
//! it (`settings/appearance`, `devices/<device>/settings/power`), with its
//! verbs and a setter of each field, `set-<field>`. The session keeps them
//! ([`ui::framework::SettingsResources`]), so the palette, agents and scripts
//! change them whether or not Settings is open; Settings' window is a view
//! of them.

pub mod accessibility;
pub mod continuity;
pub mod control_hints;
pub mod desktop;
pub mod device_vpn;
pub mod identity;
#[cfg(any(test, feature = "test-support"))]
pub mod in_memory;
pub mod network_usage;
pub mod osk;
pub mod personal_dictionary;
pub mod prefs;
pub mod providers;
pub mod search;
pub mod service;
pub mod shell_views;
pub mod shortcuts;
pub mod workspace;

/// The app whose settings they are, whose controls change them and under
/// which a change is admitted.
pub use ui::apps::SETTINGS as APP_ID;

pub const APP_TITLE: &str = "Settings";

/// Where the section of kind `kind` (`settings.appearance`) is published, and changed by its
/// setters (`<address>#set-<field>`), on the device this process reads its settings on:
/// `settings/appearance`, `devices/<device>/settings/power`. `None` for a kind that is no
/// section the session keeps.
#[must_use]
pub fn address(kind: &str) -> Option<String> {
    let (_, section, app) = places().into_iter().find(|(held, ..)| *held == kind)?;
    Some(ui::framework::section_address(section, app))
}

/// What stands for the device's key in a device section's address in [`surface`]: a reader
/// puts the key of the device whose settings it reads there.
pub use ui::settings_device::DEVICE_PLACEHOLDER as DEVICE;

/// What the settings provider publishes, as a view of another language reads it: one resource
/// per place the settings keep a section, each with its fields and the verbs it declares, its
/// setters following from its fields (two types kept in one place are one resource, of the
/// place's kind, with the fields and verbs of both), and the agent model choice; and the root a view names
/// each by (`settings.appearance`, `settings.power`), with its kind and where it is published,
/// a device section's address holding [`DEVICE`] where the device's key goes
/// (`devices/{device}/settings/power`).
#[must_use]
pub fn surface() -> (Vec<ui::tilo_resource::Surface>, Vec<Root>) {
    let mut places: Vec<(String, ui::tilo_resource::Surface)> = Vec::new();
    for (section, app, surface) in described() {
        let published = ui::settings_placement::address(section, app, DEVICE).id;
        let Some((_, place)) = places.iter_mut().find(|(held, _)| *held == published) else {
            places.push((published, surface));
            continue;
        };
        place.kind = ui::settings_placement::kind(section);
        place.fields.extend(surface.fields);
        for verb in surface.verbs {
            if !place.verbs.iter().any(|held| held.id == verb.id) {
                place.verbs.push(verb);
            }
        }
        for ty in surface.types {
            if !place.types.iter().any(|held| held.name == ty.name) {
                place.types.push(ty);
            }
        }
    }
    places.push((
        service::agent_models::RESOURCE.to_owned(),
        service::agent_models::surface(),
    ));
    places.push((
        service::shortcuts_table::RESOURCE.to_owned(),
        service::shortcuts_table::surface(),
    ));
    places.push((
        service::languages::RESOURCE.to_owned(),
        service::languages::surface(),
    ));
    places.push((
        service::tablet_choices::RESOURCE.to_owned(),
        service::tablet_choices::surface(),
    ));
    places
        .into_iter()
        .map(|(published, place)| {
            let name = place
                .kind
                .strip_prefix("settings.")
                .unwrap_or(&place.kind)
                .replace('-', "_");
            let root = Root {
                name,
                kind: place.kind.clone(),
                published,
            };
            (place, root)
        })
        .unzip()
}

/// A resource of [`surface`] as a view names it.
#[derive(Debug, serde::Serialize)]
pub struct Root {
    /// Its name in a view: `appearance` for `settings.appearance`.
    pub name: String,
    pub kind: String,
    /// Where it is published.
    pub published: String,
}

/// [`surface`] as its committed file, `published.toml` beside this crate, holds it: the
/// resources in the form an app's committed surface takes, then the roots.
///
/// # Errors
/// A resource that holds what a surface file cannot.
pub fn surface_file() -> Result<String, String> {
    use std::fmt::Write as _;
    let (resources, roots) = surface();
    let resources = serde_json::to_value(resources).map_err(|error| error.to_string())?;
    let mut text = ui::tilo_resource::surface::file::render(
        "# What the settings provider publishes, written by this crate's tests\n\
         # (`UPDATE_SNAPSHOTS=1`); review changes like an API change.\n\n",
        resources.as_array().map_or(&[][..], Vec::as_slice),
    )?;
    for Root {
        name,
        kind,
        published,
    } in roots
    {
        let _ = write!(
            text,
            "[[roots]]\nname = {name:?}\nkind = {kind:?}\npublished = {published:?}\n\n"
        );
    }
    Ok(text)
}

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
        pub fn declare(records: &mut ui::framework::Resources<'_>) {
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

        /// Every section the session keeps: its kind, the section of the
        /// settings it is kept in, and the app whose preferences it is when
        /// it is one's.
        #[must_use]
        pub fn places() -> Vec<(&'static str, &'static str, Option<&'static str>)> {
            vec![$({
                let section = <$section as settings_model::SettingsSchema>::SCOPE.section();
                let app = ui::settings_placement::per_app(section)
                    .then(|| <$section as settings_model::SettingsSchema>::APP.unwrap_or(APP_ID));
                (<$section as ui::tilo_resource::Resource>::KIND, section, app)
            }),*]
        }

        /// Every section the session keeps as a view of another language
        /// reads it: each with the section of the settings it is kept in, the
        /// app whose preferences it is when it is one's, and its surface.
        fn described() -> Vec<(&'static str, Option<&'static str>, ui::tilo_resource::Surface)> {
            vec![$({
                let section = <$section as settings_model::SettingsSchema>::SCOPE.section();
                let app = ui::settings_placement::per_app(section)
                    .then(|| <$section as settings_model::SettingsSchema>::APP.unwrap_or(APP_ID));
                (section, app, ui::framework::section_surface::<$section>())
            }),*]
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
    settings_model::text_assistance::TextAssistancePrefs,
    personal_dictionary::PersonalDictionary,
    prefs::PowerPrefs,
    prefs::WidgetsPrefs,
    control_hints::ControlHintsPrefs,
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
    shell_views::ShellViews,
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
#[cfg(test)]
mod input_tests;
#[cfg(test)]
mod surface_tests;
