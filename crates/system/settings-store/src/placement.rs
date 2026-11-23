//! Which sections of a profile's settings are the person's, kept in their record, and which this
//! device's.
//!
//! A section is the person's or this device's, never some of each. [`SECTIONS`] declares each
//! section once; one it does not name is the device's, so nothing leaves the device until someone
//! decides it should.

use settings_model::{APP_DEVICE_LOCAL_SECTION, APP_PERSONAL_SECTION};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    Person,
    Device,
}

pub use settings_model::person::MAX_PERSONAL_VALUE_BYTES;

pub const SECTIONS: &[(&str, Holder)] = &[
    ("accessibility", Holder::Person),
    ("agent_providers_person", Holder::Person),
    (APP_PERSONAL_SECTION, Holder::Person),
    ("appearance", Holder::Person),
    ("audio", Holder::Person),
    ("control_hints", Holder::Person),
    ("default_apps", Holder::Person),
    ("desktop_controls", Holder::Person),
    ("display_person", Holder::Person),
    ("identity", Holder::Person),
    ("input", Holder::Person),
    (crate::lockdown::SECTION, Holder::Person),
    ("locale", Holder::Person),
    ("notifications", Holder::Person),
    (crate::personal_dictionary::SECTION, Holder::Person),
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

#[must_use]
pub fn holder_of_key(key: &str) -> Option<Holder> {
    let (section, leaf) = key.strip_prefix("tilo.")?.split_once('.')?;
    (!section.is_empty() && !leaf.is_empty()).then(|| holder(section))
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn every_section_has_one_holder;

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
            assert_eq!(
                holder_of_key("tilo.input.tap_to_click"),
                Some(Holder::Person)
            );
            assert_eq!(
                holder_of_key("tilo.app_personal.so.tilo.monitor"),
                Some(Holder::Person)
            );
            assert_eq!(
                holder_of_key("tilo.input_device.tablets"),
                Some(Holder::Device)
            );
            assert_eq!(holder_of_key("tilo.input"), None);
            assert_eq!(holder_of_key("input.tap_to_click"), None);
        }
    }
}
