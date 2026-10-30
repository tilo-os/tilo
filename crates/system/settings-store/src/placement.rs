//! Which of a profile's two documents holds each section.
//!
//! A section is the person's or this device's, never some of each: the
//! person's document syncs to the person's devices, the device's stays on
//! it. [`SECTIONS`] declares each section once. A section it does not name
//! (an app's own, or one added without a declaration) is the device's, so
//! nothing leaves the device until someone decides it should.

/// Who holds a section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// The person: the section follows them to each of their devices.
    Person,
    /// This device: the section stays on it.
    Device,
}

/// The section holding every app's settings that follow the person, one
/// object per app id: an app's typed settings declare where each belongs,
/// and only those that follow the person are written here.
pub const APP_PERSONAL_SECTION: &str = "app_personal";

/// The section holding every app's settings that stay on this device, one
/// object per app id.
pub const APP_DEVICE_LOCAL_SECTION: &str = "app_device_local";

/// The largest serialized value of the person's the device fabric carries.
/// An app's entry in [`APP_PERSONAL_SECTION`] is one value, so its whole
/// object must fit.
pub const MAX_PERSONAL_VALUE_BYTES: usize = 64 * 1024;

/// Every section the system writes, and who holds it.
pub const SECTIONS: &[(&str, Holder)] = &[
    // The person's.
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
    ("locale", Holder::Person),
    ("notifications", Holder::Person),
    ("quick_settings", Holder::Person),
    ("shortcuts", Holder::Person),
    ("text_assistance", Holder::Person),
    // This device's.
    ("accessibility_device", Holder::Device),
    ("agent_providers", Holder::Device),
    (APP_DEVICE_LOCAL_SECTION, Holder::Device),
    ("appearance_device", Holder::Device),
    ("audio_device", Holder::Device),
    ("clipboard", Holder::Device),
    ("clock", Holder::Device),
    ("collaboration", Holder::Device),
    ("continuity", Holder::Device),
    ("desk_widgets", Holder::Device),
    ("desktop_controls_device", Holder::Device),
    ("device", Holder::Device),
    ("device_vpn", Holder::Device),
    ("display", Holder::Device),
    ("display_profiles", Holder::Device),
    ("display_transaction", Holder::Device),
    ("history", Holder::Device),
    ("input_device", Holder::Device),
    ("mobile_data_usage", Holder::Device),
    ("power", Holder::Device),
    (crate::search_history::SECTION, Holder::Device),
    ("updates", Holder::Device),
];

/// Who holds section `name`: as [`SECTIONS`] declares it, else this device.
#[must_use]
pub fn holder(name: &str) -> Holder {
    SECTIONS
        .iter()
        .find(|(section, _)| *section == name)
        .map_or(Holder::Device, |(_, holder)| *holder)
}

/// Who holds the leaf a device-fabric key (`tilo.<section>.<leaf>`) names;
/// `None` when `key` names no leaf.
#[must_use]
pub fn holder_of_key(key: &str) -> Option<Holder> {
    let (section, leaf) = key.strip_prefix("tilo.")?.split_once('.')?;
    (!section.is_empty() && !leaf.is_empty()).then(|| holder(section))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_section_is_declared_once() {
        let mut names: Vec<_> = SECTIONS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn an_undeclared_section_stays_on_the_device() {
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
