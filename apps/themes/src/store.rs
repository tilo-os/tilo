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
//!   "appearance": {                           // owned here
//!     "id":   "sunset",
//!     "name": "Sunset",
//!     "dark_base": true,
//!     "theme": { …full ThemeSpec… }           // the exact tokens to materialise
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
//! [`save`] is a **merge** write: it reads the current document, overwrites
//! only the `appearance` section, and preserves every other key (locale,
//! audio, display, …). So applying a theme never clobbers another app's
//! settings.
//!
//! # Live propagation
//!
//! Persisting the active theme makes it the look on the *next* session /
//! app launch. Pushing it into already-running apps instantly is a separate
//! step: the compositor exposes [`ui::App::set_theme`] as the runtime hook,
//! but there is no OS-wide "theme changed" broadcast socket yet — see the
//! note in [`crate::app`] where Apply is dispatched.

use std::path::PathBuf;

use crate::model::ThemeSpec;

/// The section key this store owns inside the shared settings document.
pub const SECTION: &str = "appearance";

/// Resolve the shared settings path. Delegating to [`settings_store`] is what
/// keeps this in step with the Settings app rather than by hand.
pub fn store_path() -> Option<PathBuf> {
    settings_store::settings_path()
}

/// Merge `spec` into `existing` (the current settings-document text, or
/// empty) as the `appearance` section, preserving every foreign key.
/// Returns the pretty-printed document.
pub fn to_json_merged(spec: &ThemeSpec, existing: &str) -> String {
    let mut root: serde_json::Value =
        serde_json::from_str(existing).unwrap_or(serde_json::Value::Null);
    if !root.is_object() {
        root = serde_json::json!({});
    }
    let obj = root.as_object_mut().expect("just ensured object");
    obj.insert("version".into(), serde_json::json!(1));
    obj.insert(
        SECTION.into(),
        serde_json::json!({
            "id": spec.id,
            "name": spec.name,
            "dark_base": spec.dark_base,
            "theme": serde_json::to_value(spec).unwrap_or(serde_json::Value::Null),
        }),
    );
    serde_json::to_string_pretty(&root).unwrap_or_else(|_| "{}".into())
}

/// The active theme's id in a settings document, if the `appearance`
/// section is present and well-formed.
pub fn active_id(text: &str) -> Option<String> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    root.get(SECTION)?
        .get("id")?
        .as_str()
        .map(std::string::ToString::to_string)
}

/// Parse the active [`ThemeSpec`] from a settings document, if present.
pub fn active_spec(text: &str) -> Option<ThemeSpec> {
    let root: serde_json::Value = serde_json::from_str(text).ok()?;
    let theme = root.get(SECTION)?.get("theme")?.clone();
    serde_json::from_value(theme).ok()
}

/// Merge `spec` into the settings document at `path`, preserving foreign
/// sections, and write it back. Creates the parent directory if needed.
/// The path-explicit core of [`save`], factored out so it can be tested
/// against a temp file without mutating process-global environment.
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
mod tests {
    use super::*;
    use crate::model::{presets, seed_custom};

    #[test]
    fn merge_writes_appearance_and_preserves_foreign_keys() {
        let existing = r#"{"version":1,"locale":{"currency":"EUR"},"audio":{"event_sounds":true}}"#;
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
        // Applying a second theme replaces the appearance section wholesale
        // (no stale keys from the previous theme leak through).
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
