//! Making a theme the one the system shows, and handing it on as a file.
//!
//! Applying writes the person's appearance in the shared settings document
//! (`appearance.mode`, `appearance.dark_theme` / `appearance.light_theme`,
//! and for one of their own themes its document in
//! `appearance.custom_themes`), the same keys Settings writes, so the shell
//! and every app pick it up through the theme layers. A theme that misses a
//! contrast floor is never applied or exported: the person sees which pair
//! fails and fixes it here.

use ui::theme::{
    CUSTOM_THEMES_KEY, DARK_THEME_KEY, LIGHT_THEME_KEY, MODE_KEY, Preset, ThemeDocument,
};

/// What applying a theme writes into the appearance section: the theme for
/// its polarity, the mode (unless the person follows the time of day), and
/// for a custom theme its document beside the ones already there.
#[must_use]
pub fn appearance_values(
    document: &ThemeDocument,
    current: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut values = serde_json::Map::new();
    let preset = Preset::find(&document.id).filter(|preset| preset.document() == *document);
    let reference = match preset {
        Some(preset) => preset.id().to_owned(),
        None => format!("custom:{}", document.id),
    };
    let key = if document.is_dark() {
        DARK_THEME_KEY
    } else {
        LIGHT_THEME_KEY
    };
    values.insert(key.into(), serde_json::Value::String(reference));
    let automatic = current
        .get(MODE_KEY)
        .and_then(serde_json::Value::as_str)
        .is_some_and(ui::theme::is_automatic_mode);
    if !automatic {
        let mode = if document.is_dark() { "dark" } else { "light" };
        values.insert(MODE_KEY.into(), serde_json::Value::String(mode.into()));
    }
    if preset.is_none() {
        let mut customs: Vec<serde_json::Value> = current
            .get(CUSTOM_THEMES_KEY)
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|existing| {
                existing.get("id").and_then(serde_json::Value::as_str) != Some(&document.id)
            })
            .collect();
        customs.push(serde_json::to_value(document).unwrap_or(serde_json::Value::Null));
        values.insert(CUSTOM_THEMES_KEY.into(), serde_json::Value::Array(customs));
    }
    values
}

/// Refuse a document that is not admitted or misses a contrast floor,
/// naming what fails.
///
/// # Errors
///
/// The structural reason, or every failing pair.
pub fn ensure_legible(document: &ThemeDocument) -> Result<(), String> {
    document.validate().map_err(str::to_owned)?;
    let findings = document.findings();
    if findings.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Too little contrast: {}",
        findings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    ))
}

/// Make `document` the theme the system shows for its polarity.
///
/// # Errors
///
/// An illegible document, or a settings document that cannot be written.
pub fn apply(document: &ThemeDocument) -> Result<(), String> {
    ensure_legible(document)?;
    let appearance = ui::storage::SettingsSection::named("appearance");
    let values = appearance_values(document, &appearance.load());
    appearance.update(values).map_err(|error| error.to_string())
}

/// The document as a file someone else can open in their Theme Studio.
///
/// # Errors
///
/// An illegible document.
pub fn export(document: &ThemeDocument) -> Result<String, String> {
    ensure_legible(document)?;
    serde_json::to_string_pretty(document).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applying_a_preset_names_it_and_sets_the_mode() {
        let values = appearance_values(&Preset::DarkSoft.document(), &serde_json::Map::new());
        assert_eq!(values[DARK_THEME_KEY], "dark-soft");
        assert_eq!(values[MODE_KEY], "dark");
        assert!(!values.contains_key(CUSTOM_THEMES_KEY));

        let automatic = serde_json::json!({"mode": "automatic"});
        let values = appearance_values(
            &Preset::Light.document(),
            automatic.as_object().expect("an object"),
        );
        assert_eq!(values[LIGHT_THEME_KEY], "light");
        assert!(
            !values.contains_key(MODE_KEY),
            "automatic mode stays automatic"
        );
    }

    #[test]
    fn applying_a_custom_theme_stores_its_document_once_and_the_runtime_reads_it() {
        let mut document = Preset::DarkGraphite.document();
        document.id = "ember".into();
        document.name = "Ember".into();
        document.shape.window_radius = 20.0;
        let earlier = serde_json::json!({
            "custom_themes": [{"id": "ember", "stale": true}, {"id": "other"}]
        });
        let values = appearance_values(&document, earlier.as_object().expect("an object"));
        assert_eq!(values[DARK_THEME_KEY], "custom:ember");
        let customs = values[CUSTOM_THEMES_KEY].as_array().expect("a list");
        assert_eq!(customs.len(), 2, "replaced, not appended twice");

        let settings = serde_json::json!({ "appearance": values });
        let theme = ui::Theme::from_settings_json(&settings.to_string()).expect("admitted");
        assert_eq!(theme.name, "Ember");
        assert!((theme.tokens.radii.tile - 20.0).abs() < 1e-4);
    }

    #[test]
    fn an_illegible_theme_is_neither_applied_nor_exported() {
        let mut murky = Preset::Light.document();
        murky.id = "murky".into();
        murky.palette.ink.secondary = 0.85;
        let refused = ensure_legible(&murky).expect_err("dim ink on light surfaces");
        assert!(refused.contains("text_dim"), "{refused}");
        assert!(export(&murky).is_err());
        assert!(apply(&murky).is_err(), "refused before anything is written");

        let json = export(&Preset::Dark.document()).expect("legible");
        let back: ThemeDocument = serde_json::from_str(&json).expect("a document");
        assert_eq!(back, Preset::Dark.document());
    }
}
