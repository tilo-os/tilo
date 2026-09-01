//! The shared **active-theme store** — the on-disk contract the compositor
//! and every system app read to pick up the user's chosen OS theme.
//!
//! # Why this exists
//!
//! The Themes app lets the user browse, edit and author OS themes, but a
//! theme only *becomes the desktop's look* when it is applied. "Apply"
//! writes the selected [`ThemeSpec`] here — into the same shared
//! preferences document the Settings app owns — so the compositor and the
//! other system apps can read the active theme at session start.
//!
//! # The file (a format contract, not a crate dependency)
//!
//! The active theme lives in the shared per-user settings document, in its
//! own namespaced section so it grows independently of every other writer:
//!
//! ```text
//! $XDG_CONFIG_HOME/qos/settings.json   (falls back to ~/.config/qos/settings.json)
//! ```
//!
//! ```json
//! {
//!   "version": 1,
//!   "locale":  { … },                        // owned by Settings' region store
//!   "appearance": {                           // active choice owned by Settings
//!     "theme": "custom:sunset",
//!     "custom_theme": { …versioned ui::CustomThemeSpec… },
//!     "high_contrast": false,
//!     "studio": {                             // editable artifact owned here
//!       "id":   "sunset",
//!       "name": "Sunset",
//!       "dark_base": true,
//!       "theme": { …full ThemeSpec… }
//!     }
//!   }
//! }
//! ```
//!
//! The `theme` object is a whole serialised [`ThemeSpec`], so a reader can
//! reconstruct the exact palette/shape/typography without this crate — it
//! is a byte-for-byte JSON contract, identical in spirit to the Settings
//! app's `SystemPrefs`/`RegionSettings` sections.
//!
//! # Non-destructive writes
//!
//! [`save`] is a **merge** write: it reads the current document, updates the
//! nested `appearance.studio` artifact and active base theme, and preserves
//! accessibility, scale and every foreign section. Settings likewise
//! preserves this nested artifact.
//!
//! The runtime contract is consumed directly by `ui::Theme`, so Theme Studio,
//! Settings, applications, and shell chrome cannot drift in reconstruction.

use std::path::PathBuf;

use crate::model::ThemeSpec;

/// The section key this store owns inside the shared settings document.
pub const SECTION: &str = "appearance";
/// Theme Studio's private artifact below Settings' canonical appearance state.
pub const STUDIO_KEY: &str = "studio";

/// Resolve the shared settings path. Delegating to [`settings_store`] is what
/// keeps this in step with the Settings app rather than by hand.
pub fn store_path() -> Option<PathBuf> {
    settings_store::settings_path()
}

/// Merge `spec` into `existing` (the current settings-document text, or
/// empty) below `appearance.studio`, preserving Settings-owned preferences.
/// Returns the pretty-printed document.
pub fn to_json_merged(spec: &ThemeSpec, existing: &str) -> String {
    let root: serde_json::Value = serde_json::from_str(existing).unwrap_or(serde_json::Value::Null);
    // Coerce to a JSON object, discarding anything malformed rather than
    // indexing into it and panicking — `existing` is a document another
    // process (or an older schema version) may have written.
    let mut obj = match root {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    obj.insert("version".into(), serde_json::json!(1));

    let mut appearance = match obj.remove(SECTION) {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };

    let active = crate::model::builtin_theme(&spec.id)
        .map_or_else(|| format!("custom:{}", spec.id), |theme| theme.name);
    appearance.insert("theme".into(), serde_json::json!(active));
    if !spec.builtin {
        appearance.insert(
            "custom_theme".into(),
            serde_json::to_value(spec.to_custom_spec()).unwrap_or(serde_json::Value::Null),
        );
    }
    appearance.insert(
        STUDIO_KEY.into(),
        serde_json::json!({
            "id": spec.id,
            "name": spec.name,
            "dark_base": spec.dark_base,
            "theme": serde_json::to_value(spec).unwrap_or(serde_json::Value::Null),
        }),
    );
    obj.insert(SECTION.into(), serde_json::Value::Object(appearance));

    serde_json::to_string_pretty(&serde_json::Value::Object(obj)).unwrap_or_else(|_| "{}".into())
}

/// The active theme's id in a settings document, if the `appearance`
/// section is present and well-formed.
pub fn active_id(text: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    root.get(SECTION)?
        .get(STUDIO_KEY)?
        .get("id")?
        .as_str()
        .map(std::string::ToString::to_string)
}

/// Parse the active [`ThemeSpec`] from a settings document, if present.
pub fn active_spec(text: &str) -> Option<ThemeSpec> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    let theme = root.get(SECTION)?.get(STUDIO_KEY)?.get("theme")?.clone();
    serde_json::from_value(theme).ok()
}

/// Merge `spec` into the settings document at `path`, preserving foreign
/// sections, and write it back. Creates the parent directory if needed.
/// The path-explicit core of [`save`], factored out so it can be tested
/// against a temp file without mutating process-global environment.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created or the file cannot be written.
pub fn save_to(path: &std::path::Path, spec: &ThemeSpec) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    std::fs::write(path, to_json_merged(spec, &existing))
}

/// The active theme id in the settings document at `path`, if any.
pub fn load_active_id_from(path: &std::path::Path) -> Option<String> {
    active_id(&std::fs::read_to_string(path).ok()?)
}

/// Persist `spec` as the active OS theme, preserving foreign sections.
/// Creates the `qos` config directory if needed. Returns the path written.
///
/// # Errors
///
/// Returns an error if the config directory cannot be determined or the file cannot be written.
pub fn save(spec: &ThemeSpec) -> std::io::Result<PathBuf> {
    let path = store_path().ok_or_else(|| std::io::Error::other("no config dir"))?;
    save_to(&path, spec)?;
    Ok(path)
}

/// Load the active theme's id from the shared settings path, if any.
pub fn load_active_id() -> Option<String> {
    load_active_id_from(&store_path()?)
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::{presets, seed_custom};

    #[test]
    fn merge_writes_appearance_and_preserves_foreign_keys() {
        let existing = r#"{"version":1,"locale":{"currency":"EUR"},"audio":{"event_sounds":true},"appearance":{"high_contrast":true,"text_scale":1.25}}"#;
        let spec = seed_custom();
        let out = to_json_merged(&spec, existing);
        // Our section landed with the id + a nested full theme.
        assert_eq!(active_id(&out).as_deref(), Some("sunset"));
        assert!(
            out.contains("\"appearance\""),
            "appearance section present: {out}"
        );
        // Foreign sections survive untouched.
        assert!(
            out.contains("\"currency\": \"EUR\""),
            "locale survived: {out}"
        );
        assert!(
            out.contains("\"event_sounds\": true"),
            "audio survived: {out}"
        );
        let root: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(root["appearance"]["high_contrast"], true);
        assert_eq!(root["appearance"]["text_scale"], 1.25);
        assert_eq!(root["appearance"]["theme"], "custom:sunset");
        let runtime = ui::Theme::from_settings_json(&out).expect("runtime consumes custom theme");
        assert_eq!(runtime.name, spec.name);
        assert_eq!(runtime.accent_primary, spec.to_theme().accent_primary);
        assert_eq!(runtime.tokens.radii.md, spec.radius_px());
    }

    #[test]
    fn active_spec_round_trips_through_the_document() {
        let spec = presets()[3].clone(); // Dracula
        let out = to_json_merged(&spec, "");
        let back = active_spec(&out).expect("appearance.theme parses back into a spec");
        assert_eq!(
            back, spec,
            "the exact ThemeSpec is recoverable from the store"
        );
        assert_eq!(active_id(&out).as_deref(), Some(spec.id.as_str()));
    }

    #[test]
    fn empty_or_garbage_document_has_no_active_theme() {
        assert_eq!(active_id(""), None);
        assert_eq!(active_id("not json {"), None);
        assert_eq!(active_spec("").map(|s| s.id), None);
    }

    #[test]
    fn merge_overwrites_only_its_own_section_on_reapply() {
        // Applying a second theme replaces only Studio's artifact (no stale
        // editor keys leak through) while Settings-owned fields survive.
        let first = to_json_merged(&presets()[0].clone(), "");
        let second = to_json_merged(&presets()[4].clone(), &first);
        assert_eq!(
            active_id(&second).as_deref(),
            Some(presets()[4].id.as_str())
        );
        let back = active_spec(&second).unwrap();
        assert_eq!(back, presets()[4].clone());
    }

    #[test]
    fn round_trips_through_a_real_file() {
        // Exercise the actual disk write/read (not just the in-memory merge)
        // against an explicit temp file — no process-global env mutation, so
        // it is safe under the default parallel test runner.
        let path = std::env::temp_dir()
            .join(format!("qos-themes-store-{}", std::process::id()))
            .join("settings.json");
        let _ = std::fs::remove_file(&path);
        // A pre-existing foreign section must survive the write.
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(&path, r#"{"version":1,"locale":{"currency":"GBP"}}"#).unwrap();
        let spec = seed_custom();
        save_to(&path, &spec).expect("save_to writes the settings file");
        assert_eq!(load_active_id_from(&path).as_deref(), Some("sunset"));
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            active_spec(&text),
            Some(spec),
            "the exact theme reads back off disk"
        );
        assert!(
            text.contains("GBP"),
            "foreign locale section survived the write"
        );
        let _ = std::fs::remove_file(&path);
    }
}
