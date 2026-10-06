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
//! $XDG_CONFIG_HOME/tilo/settings.json   (falls back to ~/.config/tilo/settings.json)
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

use ui::storage::SettingsDocument;

use crate::model::ThemeSpec;

/// The section key this store owns inside the shared settings document.
pub const SECTION: &str = "appearance";
/// Theme Studio's private artifact below Settings' canonical appearance state.
pub const STUDIO_KEY: &str = "studio";

fn appearance(document: &SettingsDocument) -> ui::storage::SettingsSection {
    ui::storage::SettingsSection::named(SECTION).in_document(document.clone())
}

/// Merge `spec` into `existing` (the current settings-document text, or
/// empty) below `appearance.studio`, preserving Settings-owned preferences.
/// Returns the pretty-printed document.
pub fn to_json_merged(spec: &ThemeSpec, existing: &str) -> String {
    // Anything malformed reads as no settings, rather than being indexed
    // into: `existing` is a document another process may have written.
    let mut obj = ui::storage::parse_settings(existing);
    obj.insert("version".into(), serde_json::json!(1));
    let mut appearance = match obj.remove(SECTION) {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    appearance.extend(appearance_values(spec));
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

/// The `appearance` keys Theme Studio owns for `spec`: the active theme's
/// name, a custom theme's definition, and the full spec below
/// [`STUDIO_KEY`]. Every other key of the section is left as it is.
fn appearance_values(spec: &ThemeSpec) -> serde_json::Map<String, serde_json::Value> {
    let mut values = serde_json::Map::new();
    let active = crate::model::builtin_theme(&spec.id)
        .map_or_else(|| format!("custom:{}", spec.id), |theme| theme.name);
    values.insert("theme".into(), serde_json::json!(active));
    if !spec.builtin {
        values.insert(
            "custom_theme".into(),
            serde_json::to_value(spec.to_custom_spec()).unwrap_or(serde_json::Value::Null),
        );
    }
    values.insert(
        STUDIO_KEY.into(),
        serde_json::json!({
            "id": spec.id,
            "name": spec.name,
            "dark_base": spec.dark_base,
            "theme": serde_json::to_value(spec).unwrap_or(serde_json::Value::Null),
        }),
    );
    values
}

fn studio_id(appearance: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    appearance
        .get(STUDIO_KEY)?
        .get("id")?
        .as_str()
        .map(std::string::ToString::to_string)
}

/// A custom theme that misses a contrast floor is not applied: the runtime
/// would repair it anyway, but the person should see which pair failed and
/// fix it here rather than get colours they did not pick. Presets are swept
/// against the same floors in `ui`'s tests.
fn ensure_legible(spec: &ThemeSpec) -> std::io::Result<()> {
    if spec.builtin {
        return Ok(());
    }
    let findings = spec.to_custom_spec().contrast_findings();
    if findings.is_empty() {
        return Ok(());
    }
    let pairs: Vec<String> = findings.iter().map(ToString::to_string).collect();
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("too little contrast: {}", pairs.join("; ")),
    ))
}

/// Persist `spec` as the active OS theme in `document` (the settings
/// document the host gave the app: the person's, or a test or capture
/// profile's), preserving every other section and key.
///
/// # Errors
///
/// Returns an error if the theme is illegible, there is no settings document
/// to write, or the write fails.
pub fn save(document: &SettingsDocument, spec: &ThemeSpec) -> std::io::Result<()> {
    ensure_legible(spec)?;
    appearance(document).update(appearance_values(spec))
}

/// The active theme's id in `document`, if any.
pub fn load_active_id(document: &SettingsDocument) -> Option<String> {
    studio_id(&appearance(document).load())
}

/// The theme applied on this device, as `document` records it: the whole
/// spec Theme Studio wrote when it was applied.
pub fn applied(document: &SettingsDocument) -> Option<ThemeSpec> {
    let theme = appearance(document).load().remove(STUDIO_KEY)?;
    serde_json::from_value(theme.get("theme")?.clone()).ok()
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::{presets, seed_custom};

    #[test]
    fn an_illegible_custom_theme_is_refused_and_presets_and_their_copies_apply() {
        let dir = std::env::temp_dir().join(format!("tilo-themes-contrast-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let document = SettingsDocument::File(path.clone());

        // Case `a_custom_theme_under_a_contrast_floor_is_not_written`.
        {
            let mut murky = seed_custom();
            murky.text_dim = murky.surface_raised;
            let err = save(&document, &murky).expect_err("dim text on its own surface");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert!(
                err.to_string().contains("text_dim on surface_base"),
                "the failing pair is named: {err}"
            );
            assert!(!path.exists(), "nothing reached the settings file");
        }

        // Case `every_preset_and_an_editable_copy_of_it_apply`: duplicating a
        // preset is how most custom themes start, and the copy is measured as
        // a custom spec (derived popover and tertiary), not as the preset.
        {
            for (i, preset) in presets().into_iter().enumerate() {
                save(&document, &preset).unwrap_or_else(|e| panic!("{}: {e}", preset.name));
                let copy = crate::actions::duplicate_spec(&preset, &format!("copy-{i}"), 99);
                assert_eq!(
                    copy.to_custom_spec().contrast_findings(),
                    Vec::new(),
                    "a copy of {} must apply unchanged",
                    preset.name
                );
                save(&document, &copy).unwrap_or_else(|e| panic!("{} copy: {e}", preset.name));
            }
            save(&document, &seed_custom()).expect("the demo custom theme is legible");
        }
    }

    /// The appearance store: merge preserving foreign keys, document and file round-trips, and garbage input.
    #[test]
    fn active_theme_store_round_trips_and_merges_its_own_section() {
        // Case `merge_writes_appearance_and_preserves_foreign_keys`.
        {
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
            let runtime =
                ui::Theme::from_settings_json(&out).expect("runtime consumes custom theme");
            assert_eq!(runtime.name, spec.name);
            assert_eq!(runtime.accent_primary, spec.to_theme().accent_primary);
            assert_eq!(runtime.tokens.radii.md, spec.radius_px());
        }

        // Case `active_spec_round_trips_through_the_document`.
        {
            let spec = presets()[3].clone(); // Dracula
            let out = to_json_merged(&spec, "");
            let back = active_spec(&out).expect("appearance.theme parses back into a spec");
            assert_eq!(
                back, spec,
                "the exact ThemeSpec is recoverable from the store"
            );
            assert_eq!(active_id(&out).as_deref(), Some(spec.id.as_str()));
        }

        // Case `empty_or_garbage_document_has_no_active_theme`.
        {
            assert_eq!(active_id(""), None);
            assert_eq!(active_id("not json {"), None);
            assert_eq!(active_spec("").map(|s| s.id), None);
        }

        // Case `merge_overwrites_only_its_own_section_on_reapply`.
        {
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

        // Case `round_trips_through_a_real_file`.
        {
            // Exercise the actual disk write/read (not just the in-memory merge)
            // against an explicit temp file — no process-global env mutation, so
            // it is safe under the default parallel test runner.
            let path = std::env::temp_dir()
                .join(format!("tilo-themes-store-{}", std::process::id()))
                .join("settings.json");
            let _ = std::fs::remove_file(&path);
            // A pre-existing foreign section must survive the write.
            if let Some(p) = path.parent() {
                std::fs::create_dir_all(p).unwrap();
            }
            std::fs::write(&path, r#"{"version":1,"locale":{"currency":"GBP"}}"#).unwrap();
            let spec = seed_custom();
            let document = SettingsDocument::File(path.clone());
            save(&document, &spec).expect("save writes the settings file");
            assert_eq!(load_active_id(&document).as_deref(), Some("sunset"));
            assert_eq!(applied(&document), Some(spec.clone()));
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
}
