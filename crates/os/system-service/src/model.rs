//! What the system resources read, and how the machine's own readings become them.

pub use tilo_system_resources::model::*;

/// The mode the power daemon's profile `profile` is.
#[must_use]
pub fn power_mode(profile: os_actions::PowerProfile) -> PowerMode {
    match profile {
        os_actions::PowerProfile::Saver => PowerMode::Saver,
        os_actions::PowerProfile::Balanced => PowerMode::Balanced,
        os_actions::PowerProfile::Performance => PowerMode::Performance,
    }
}

/// The power daemon's profile for the mode `mode`.
#[must_use]
pub fn power_profile(mode: PowerMode) -> os_actions::PowerProfile {
    match mode {
        PowerMode::Saver => os_actions::PowerProfile::Saver,
        PowerMode::Balanced => os_actions::PowerProfile::Balanced,
        PowerMode::Performance => os_actions::PowerProfile::Performance,
    }
}

/// The keyboard light as the kernel reads it.
#[must_use]
pub fn keyboard_light(light: os_actions::KeyboardBacklight) -> KeyboardLight {
    KeyboardLight {
        level: Some(light.level),
        max: light.max,
        ..KeyboardLight::default()
    }
}

/// The light's own reading of `light`, when there is a light.
#[must_use]
pub fn backlight_of(light: &KeyboardLight) -> Option<os_actions::KeyboardBacklight> {
    light.level.map(|level| os_actions::KeyboardBacklight {
        level,
        max: light.max,
    })
}
