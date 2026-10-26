//! The person's defaults in their settings: the
//! [`PERSONAL_DEFAULTS_SECTION`] section, which follows them to each of their
//! devices.

use desktop_policy::{DefaultAppError, PERSONAL_DEFAULTS_SECTION, PersonalDefaults};

/// The person's defaults, from their settings.
pub fn load_personal_defaults() -> PersonalDefaults {
    PersonalDefaults::from_section(&settings_store::load_section(PERSONAL_DEFAULTS_SECTION))
}

/// Write `defaults` to the person's settings.
///
/// # Errors
///
/// Returns [`DefaultAppError::Persist`] if the settings could not be
/// written.
pub fn save_personal_defaults(defaults: &PersonalDefaults) -> Result<(), DefaultAppError> {
    let serde_json::Value::Object(section) =
        serde_json::to_value(defaults).map_err(|e| DefaultAppError::Persist(e.to_string()))?
    else {
        return Err(DefaultAppError::Persist(
            "defaults are not an object".into(),
        ));
    };
    settings_store::update_section(PERSONAL_DEFAULTS_SECTION, section)
        .map_err(|e| DefaultAppError::Persist(e.to_string()))
}
