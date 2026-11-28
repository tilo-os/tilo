//! Where each section of the settings lives: one resource per section, in the person's own
//! workspace for theirs and in the device owner's for the device's.
//!
//! A section is the person's or this device's, never some of each. [`SECTIONS`] declares each
//! section once; one it does not name is the device's, so nothing leaves the device until someone
//! decides it should. A person's section `appearance` is the resource `settings/appearance`, a
//! device's `display` is `devices/<device>/settings/display`, and an app's preferences are one
//! resource per app, `settings/apps/<app>` (its preferences kept on the device under the device's
//! `settings/apps/<app>`). Each section is one kind, `settings.<section>`.

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Holder {
    Person,
    Device,
}

pub use settings_model::{APP_DEVICE_LOCAL_SECTION, APP_PERSONAL_SECTION};

/// The kind of an app's preferences resource.
pub const APP_KIND: &str = "settings.app";

pub use settings_model::MAX_PERSONAL_VALUE_BYTES;

pub const SECTIONS: &[(&str, Holder)] = &[
    ("accessibility", Holder::Person),
    ("agent_providers_person", Holder::Person),
    (APP_PERSONAL_SECTION, Holder::Person),
    ("appearance", Holder::Person),
    ("audio", Holder::Person),
    ("control_hints", Holder::Person),
    ("default_apps", Holder::Person),
    ("desktop_controls", Holder::Person),
    (crate::emoji::SECTION, Holder::Person),
    ("display_person", Holder::Person),
    ("identity", Holder::Person),
    ("input", Holder::Person),
    (crate::lockdown::SECTION, Holder::Person),
    ("locale", Holder::Person),
    ("notifications", Holder::Person),
    ("onboarding", Holder::Person),
    (crate::personal_dictionary::SECTION, Holder::Person),
    ("presence", Holder::Person),
    ("quick_settings", Holder::Person),
    ("shell_views", Holder::Person),
    ("shortcuts", Holder::Person),
    ("text_assistance", Holder::Person),
    ("accessibility_device", Holder::Device),
    ("agent_providers", Holder::Device),
    (APP_DEVICE_LOCAL_SECTION, Holder::Device),
    ("appearance_device", Holder::Device),
    ("audio_device", Holder::Device),
    ("clipboard", Holder::Device),
    ("clock", Holder::Device),
    ("collaboration", Holder::Device),
    ("continuity", Holder::Device),
    ("widgets", Holder::Device),
    ("desktop_controls_device", Holder::Device),
    ("device", Holder::Device),
    ("device_vpn", Holder::Device),
    ("display", Holder::Device),
    ("display_profiles", Holder::Device),
    ("display_transaction", Holder::Device),
    ("history", Holder::Device),
    ("input_device", Holder::Device),
    ("network_usage", Holder::Device),
    ("power", Holder::Device),
    (crate::search_history::SECTION, Holder::Device),
    ("updates", Holder::Device),
];

#[must_use]
pub fn holder(name: &str) -> Holder {
    SECTIONS
        .iter()
        .find(|(section, _)| *section == name)
        .map_or(Holder::Device, |(_, holder)| *holder)
}

/// A resource the settings are kept in: where, its kind and its id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    pub holder: Holder,
    pub kind: String,
    pub id: String,
}

/// The kind of section `section`: `settings.<section>`, in kebab case.
#[must_use]
pub fn kind(section: &str) -> String {
    format!("settings.{}", section.replace('_', "-"))
}

/// Where section `section` is kept on the device `device` (its key in lowercase hex); `app` the
/// app whose preferences an app section's entry is.
#[must_use]
pub fn address(section: &str, app: Option<&str>, device: &str) -> Address {
    let holder = holder(section);
    let root = match holder {
        Holder::Person => "settings".to_owned(),
        Holder::Device => format!("devices/{device}/settings"),
    };
    if let Some(app) = app {
        Address {
            holder,
            kind: APP_KIND.to_owned(),
            id: format!("{root}/apps/{app}"),
        }
    } else {
        let name = section.strip_suffix("_device").unwrap_or(section);
        Address {
            holder,
            kind: kind(section),
            id: format!("{root}/{}", name.replace('_', "-")),
        }
    }
}

/// The section, and the app for an app's preferences, that the resource `id` of `kind` held by
/// `holder` keeps: what [`address`] made it of.
#[must_use]
pub fn section_of(holder: Holder, kind: &str, id: &str) -> Option<(String, Option<String>)> {
    if kind == APP_KIND {
        let (_, app) = id.rsplit_once("/apps/")?;
        let section = match holder {
            Holder::Person => APP_PERSONAL_SECTION,
            Holder::Device => APP_DEVICE_LOCAL_SECTION,
        };
        return Some((section.to_owned(), Some(app.to_owned())));
    }
    let section = kind.strip_prefix("settings.")?.replace('-', "_");
    (self::holder(&section) == holder).then_some((section, None))
}

/// Whether `section` holds one object per app, each app's preferences a resource of its own.
#[must_use]
pub fn per_app(section: &str) -> bool {
    section == APP_PERSONAL_SECTION || section == APP_DEVICE_LOCAL_SECTION
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn every_section_has_one_holder_and_one_address;

        phase each_section_is_declared_once;
        {
            let mut names: Vec<_> = SECTIONS.iter().map(|(name, _)| *name).collect();
            names.sort_unstable();
            let count = names.len();
            names.dedup();
            assert_eq!(names.len(), count);
        }

        phase an_undeclared_section_stays_on_the_device;
        {
            assert_eq!(holder("appearance"), Holder::Person);
            assert_eq!(holder(APP_PERSONAL_SECTION), Holder::Person);
            assert_eq!(holder(APP_DEVICE_LOCAL_SECTION), Holder::Device);
            assert_eq!(holder("so.tilo.monitor"), Holder::Device);
        }

        phase every_section_reads_back_from_its_address;
        {
            let device = "ab".repeat(32);
            for (section, _) in SECTIONS {
                let app = per_app(section).then_some("so.tilo.notes");
                let at = address(section, app, &device);
                assert_eq!(
                    section_of(at.holder, &at.kind, &at.id),
                    Some(((*section).to_owned(), app.map(str::to_owned))),
                    "{section}"
                );
            }
            let at = address("appearance_device", None, &device);
            assert_eq!(at.id, format!("devices/{device}/settings/appearance"));
            assert_eq!(at.kind, "settings.appearance-device");
            assert_eq!(address("appearance", None, &device).id, "settings/appearance");
            assert_eq!(
                address(APP_PERSONAL_SECTION, Some("so.tilo.notes"), &device).id,
                "settings/apps/so.tilo.notes"
            );
        }
    }
}
