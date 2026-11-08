//! Switching to the next keyboard input source: the one the person types in after it, saved for the
//! keyboard they typed on last, and named in a readout.
//! Sources belong to keyboards, not to the session: a laptop's own keyboard and a plugged-in one
//! keep their own list and their own active source ([`settings_store::keyboard_sources`]). The
//! shared list (`all`) is the fallback both read while neither has its own.

use settings_store::hardware::{
    INPUT_DEVICE_SECTION, INPUT_SECTION, INPUT_SECTIONS, KeyboardInputSource, RichInputConfig,
};
use settings_store::keyboard_sources::{ALL, KeyboardProfile};

use crate::config::Sources;

/// Type with the next input source of the keyboard last typed on: the choice is saved for that
/// keyboard, and the session types with it from now on.
/// `None` when that keyboard has a single source: there is nothing else to type in.
pub(crate) fn next() -> Option<(Sources, String)> {
    let mut base = RichInputConfig::from_section(&settings_store::load_sections(INPUT_SECTIONS));
    let scope = settings_store::keyboard_sources::active_scope();
    let mut config = settings_store::keyboard_sources::effective(&base, &scope);
    config.select_next_source()?;
    let shared = scope == ALL;
    // The shared sources are the person's everywhere; one keyboard's own are that device's.
    let mut values = serde_json::Map::new();
    if shared {
        values.insert(
            "keyboard_layout".into(),
            serde_json::Value::String(config.keyboard_layout.clone()),
        );
        base.keyboard_layout.clone_from(&config.keyboard_layout);
    } else {
        let mut inherited = KeyboardProfile::from_input(&base);
        if let Some(defaults) = base.keyboard_assignments.get(ALL) {
            inherited.options.clone_from(&defaults.options);
        }
        let mut assignments = base.keyboard_assignments.clone();
        let profile = assignments.entry(scope.clone()).or_insert(inherited);
        profile.active.clone_from(&config.keyboard_layout);
        values.insert(
            "keyboard_assignments".into(),
            serde_json::to_value(&assignments).ok()?,
        );
        base.keyboard_assignments = assignments;
    }
    let section = if shared {
        INPUT_SECTION
    } else {
        INPUT_DEVICE_SECTION
    };
    if let Err(error) = settings_store::update_section(section, values) {
        tracing::warn!(%error, "the input source choice was not kept");
    }
    let config = settings_store::keyboard_sources::effective(&base, &scope);
    let name = source_name(&config.keyboard_layout);
    Some((Sources::from_settings(&config), name))
}

/// What a person calls the source `id` : the engine by its own name, a layout by the letters it is
/// known by (`DE`, `US INTL` ).
fn source_name(id: &str) -> String {
    if let Some(engine) = settings_store::hardware::InputMethodEngine::from_source_id(id) {
        return engine.label().to_owned();
    }
    let Some(source) = KeyboardInputSource::parse(id) else {
        return id.to_owned();
    };
    let layout = source.layout.to_ascii_uppercase();
    match source.variant.as_deref() {
        Some(variant) if !variant.is_empty() => {
            format!("{layout} {}", variant.to_ascii_uppercase())
        }
        _ => layout,
    }
}

#[cfg(test)]
mod tests {
    use super::source_name;

    #[test]
    fn a_source_is_named_by_its_layout_or_its_engine() {
        assert_eq!(source_name("de"), "DE");
        assert_eq!(source_name("us:intl"), "US INTL");
        assert_eq!(
            source_name("ime/hangul"),
            settings_store::hardware::InputMethodEngine::Hangul.label()
        );
    }
}
