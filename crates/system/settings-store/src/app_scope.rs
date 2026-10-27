//! What one app reaches of the session's settings.
//!
//! A sandboxed app does not see the settings document: it asks the shell,
//! which answers from its [`SettingsSource`] through these two functions.
//! The app sees and writes its own entries — `app_personal.<id>`, which
//! roams with the person, and `app_device_local.<id>`, which stays on the
//! device — and reads the [shared sections](APP_READABLE_SECTIONS) every
//! app's toolkit follows. It cannot read another app's entries, and it
//! cannot write anything else, except the [keys of a shared section an app
//! owns](OWNED_SHARED_KEYS).

use std::io::{Error, ErrorKind};

use serde_json::{Map, Value};

use crate::continuity::{APP_DEVICE_LOCAL_SECTION, APP_PERSONAL_SECTION, MAX_PERSONAL_VALUE_BYTES};
use crate::{ChangeOrigin, SettingsSource};

/// Sections every app reads and only their owners write: the person's
/// region, spelling and accessibility preferences and the desktop's look,
/// which the toolkit applies in every app.
pub const APP_READABLE_SECTIONS: &[&str] =
    &["locale", "text_assistance", "accessibility", "appearance"];

/// Keys of a shared section that one app owns and writes through the same
/// door, as `(app id, section, keys)`: Theme Studio chooses the desktop's
/// active theme. Every other key of the section stays its owner's (the
/// Settings app's), and no other app writes these.
pub const OWNED_SHARED_KEYS: &[(&str, &str, &[&str])] = &[(
    "so.tilo.themes",
    "appearance",
    &["theme", "custom_theme", "studio"],
)];

/// The capability an app's manifest declares to edit the whole settings
/// document as the device's settings app (`[capabilities]`).
pub const SYSTEM_SETTINGS_CAPABILITY: &str = "settings.system";

/// The packages the image lets edit the whole document, whatever a manifest
/// says: the device's settings app. As with the launcher's system authority,
/// the image owns the grant and the package asks for it, so an app that
/// declares [`SYSTEM_SETTINGS_CAPABILITY`] without being listed here still
/// reaches only its own entries.
pub const SYSTEM_SETTINGS_APPS: &[&str] = &["so.tilo.settings"];

/// What an app reaches of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Its own entries, and the shared sections to read.
    App,
    /// Every section, to read and write: the device's settings app.
    System,
}

impl Reach {
    /// The reach of `app_id` when its manifest declares `capabilities`.
    #[must_use]
    pub fn of(app_id: &str, capabilities: &[impl AsRef<str>]) -> Self {
        let declared = capabilities
            .iter()
            .any(|capability| capability.as_ref() == SYSTEM_SETTINGS_CAPABILITY);
        if declared && SYSTEM_SETTINGS_APPS.contains(&app_id) {
            Self::System
        } else {
            Self::App
        }
    }
}

/// [`app_view`] at `reach`: the whole document for the system's settings app.
#[must_use]
pub fn view_at(document: &Map<String, Value>, app_id: &str, reach: Reach) -> Map<String, Value> {
    match reach {
        Reach::App => app_view(document, app_id),
        Reach::System => document.clone(),
    }
}

/// [`update_for_app_by`] at `reach`: the system's settings app writes any
/// section, merged into what it holds, as the session's own writers do.
/// `origin` is recorded with each leaf the write changes.
///
/// # Errors
///
/// As [`update_for_app`] for [`Reach::App`]; the source's own error for
/// [`Reach::System`].
pub fn update_at(
    source: &dyn SettingsSource,
    app_id: &str,
    reach: Reach,
    section: &str,
    values: Map<String, Value>,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    match reach {
        Reach::App => update_for_app_by(source, app_id, section, values, origin),
        Reach::System => source.update_section_by(section, values, origin),
    }
}

/// The most one write of an app's device-local entry may carry.
pub const MAX_DEVICE_LOCAL_VALUE_BYTES: usize = 256 * 1024;

/// The part of `document` app `app_id` sees: its own entries and the
/// shared sections.
#[must_use]
pub fn app_view(document: &Map<String, Value>, app_id: &str) -> Map<String, Value> {
    let mut view = Map::new();
    for section in [APP_PERSONAL_SECTION, APP_DEVICE_LOCAL_SECTION] {
        if let Some(entry @ Value::Object(_)) = document.get(section).and_then(|s| s.get(app_id)) {
            let mut own = Map::new();
            own.insert(app_id.to_owned(), entry.clone());
            view.insert(section.to_owned(), Value::Object(own));
        }
    }
    for section in APP_READABLE_SECTIONS {
        if let Some(value @ Value::Object(_)) = document.get(*section) {
            view.insert((*section).to_owned(), value.clone());
        }
    }
    view
}

/// Merge app `app_id`'s write of `values` into section `section` of
/// `source`. The write must name only the app's own entry (`{app_id: {...}}`)
/// in `app_personal` or `app_device_local`; its keys are merged into what
/// the entry already holds, so writers of different keys keep each other's.
///
/// # Errors
///
/// [`ErrorKind::PermissionDenied`] when the write names another section or
/// another app's entry; [`ErrorKind::InvalidInput`] when the entry is not an
/// object or would outgrow what its scope carries; otherwise the source's
/// own error.
pub fn update_for_app(
    source: &dyn SettingsSource,
    app_id: &str,
    section: &str,
    values: Map<String, Value>,
) -> std::io::Result<()> {
    update_for_app_by(source, app_id, section, values, &ChangeOrigin::default())
}

/// [`update_for_app`], recording `origin` with each leaf the write changes.
///
/// # Errors
///
/// As [`update_for_app`].
pub fn update_for_app_by(
    source: &dyn SettingsSource,
    app_id: &str,
    section: &str,
    values: Map<String, Value>,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    if let Some((_, _, owned)) = OWNED_SHARED_KEYS
        .iter()
        .find(|(owner, shared, _)| *owner == app_id && *shared == section)
    {
        return update_owned_keys(source, app_id, section, owned, values, origin);
    }
    let limit = match section {
        APP_PERSONAL_SECTION => MAX_PERSONAL_VALUE_BYTES,
        APP_DEVICE_LOCAL_SECTION => MAX_DEVICE_LOCAL_VALUE_BYTES,
        _ => {
            return Err(Error::new(
                ErrorKind::PermissionDenied,
                format!("{app_id} may not write the {section} settings"),
            ));
        }
    };
    if app_id.is_empty() {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "an app without an id has no settings of its own",
        ));
    }
    let mut values = values.into_iter();
    let (Some((entry, Value::Object(keys))), None) = (values.next(), values.next()) else {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{app_id} writes one object under its own id"),
        ));
    };
    if entry != app_id {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("{app_id} may not write the settings of {entry}"),
        ));
    }
    let mut stored = match source
        .load()
        .get(section)
        .and_then(|section| section.get(app_id))
    {
        Some(Value::Object(stored)) => stored.clone(),
        _ => Map::new(),
    };
    stored.extend(keys);
    let size = serde_json::to_vec(&stored).map_err(Error::other)?.len();
    if size > limit {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!("{app_id}'s {section} settings would be {size} bytes, more than {limit}"),
        ));
    }
    let mut entry = Map::new();
    entry.insert(app_id.to_owned(), Value::Object(stored));
    source.update_section_by(section, entry, origin)
}

/// Merge `values` into the shared `section` for the app that owns the keys
/// it names.
fn update_owned_keys(
    source: &dyn SettingsSource,
    app_id: &str,
    section: &str,
    owned: &[&str],
    values: Map<String, Value>,
    origin: &ChangeOrigin,
) -> std::io::Result<()> {
    if let Some(key) = values.keys().find(|key| !owned.contains(&key.as_str())) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            format!("{app_id} may not write {section}.{key}"),
        ));
    }
    let size = serde_json::to_vec(&values).map_err(Error::other)?.len();
    if size > MAX_PERSONAL_VALUE_BYTES {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "{app_id}'s {section} write would be {size} bytes, more than {MAX_PERSONAL_VALUE_BYTES}"
            ),
        ));
    }
    source.update_section_by(section, values, origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionPaths, SessionSettings, load_at};
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    fn session(dir: &std::path::Path) -> SessionSettings {
        SessionSettings::open(SessionPaths::beside(dir.join("settings.json"))).unwrap()
    }

    #[test]
    fn the_system_settings_app_reaches_every_section_and_no_other_app_does() {
        let dir = tempfile::tempdir().unwrap();
        let session = session(dir.path());
        let asks = [SYSTEM_SETTINGS_CAPABILITY];
        assert_eq!(Reach::of("so.tilo.settings", &asks), Reach::System);
        assert_eq!(Reach::of("so.tilo.settings", &[] as &[&str]), Reach::App);
        assert_eq!(Reach::of("so.tilo.notes", &asks), Reach::App);
        update_at(
            &session,
            "so.tilo.settings",
            Reach::System,
            "appearance",
            object(json!({"text_scale": 1.3})),
            &ChangeOrigin::default(),
        )
        .unwrap();
        update_at(
            &session,
            "so.tilo.settings",
            Reach::System,
            "input",
            object(json!({"keyboard_layout": "de"})),
            &ChangeOrigin::default(),
        )
        .unwrap();
        let document = session.load();
        assert_eq!(document["appearance"]["text_scale"], 1.3);
        assert_eq!(
            view_at(&document, "so.tilo.settings", Reach::System),
            document
        );
        assert_eq!(
            update_at(
                &session,
                "so.tilo.notes",
                Reach::App,
                "appearance",
                object(json!({"text_scale": 2.0})),
                &ChangeOrigin::default(),
            )
            .unwrap_err()
            .kind(),
            ErrorKind::PermissionDenied
        );
        assert!(!view_at(&document, "so.tilo.notes", Reach::App).contains_key("input"));
    }

    #[test]
    fn an_apps_write_reaches_the_record_its_scope_names_and_the_copy() {
        let dir = crate::test_dir();
        let session = session(dir.path());
        let paths = session.paths().clone();
        update_for_app(
            &session,
            "so.tilo.monitor",
            APP_PERSONAL_SECTION,
            object(json!({"so.tilo.monitor": {"confirm_kill": false}})),
        )
        .unwrap();
        update_for_app(
            &session,
            "so.tilo.monitor",
            APP_PERSONAL_SECTION,
            object(json!({"so.tilo.monitor": {"interval_ms": 2000}})),
        )
        .unwrap();
        update_for_app(
            &session,
            "so.tilo.monitor",
            APP_DEVICE_LOCAL_SECTION,
            object(json!({"so.tilo.monitor": {"column": "cpu"}})),
        )
        .unwrap();

        assert_eq!(
            Value::Object(load_at(&paths.person)),
            json!({"app_personal": {"so.tilo.monitor": {"confirm_kill": false, "interval_ms": 2000}}}),
            "both writes' keys are kept, in the record that roams"
        );
        assert_eq!(
            Value::Object(load_at(&paths.device)),
            json!({"app_device_local": {"so.tilo.monitor": {"column": "cpu"}}})
        );
        assert_eq!(
            load_at(&paths.copy)["app_personal"]["so.tilo.monitor"]["interval_ms"],
            2000
        );
    }

    #[test]
    fn an_app_cannot_write_another_apps_entry_or_a_shared_section() {
        let dir = crate::test_dir();
        let session = session(dir.path());
        session
            .update_section(
                APP_PERSONAL_SECTION,
                object(json!({"so.tilo.notes": {"font": "serif"}})),
            )
            .unwrap();
        let before = session.load();

        let other = update_for_app(
            &session,
            "so.tilo.monitor",
            APP_PERSONAL_SECTION,
            object(json!({"so.tilo.notes": {"font": "mono"}})),
        )
        .unwrap_err();
        assert_eq!(other.kind(), ErrorKind::PermissionDenied);
        let smuggled = update_for_app(
            &session,
            "so.tilo.monitor",
            APP_PERSONAL_SECTION,
            object(json!({"so.tilo.monitor": {}, "so.tilo.notes": {"font": "mono"}})),
        )
        .unwrap_err();
        assert_eq!(smuggled.kind(), ErrorKind::InvalidInput);
        for shared in ["locale", "appearance", "input"] {
            let error = update_for_app(
                &session,
                "so.tilo.monitor",
                shared,
                object(json!({"so.tilo.monitor": {"x": 1}})),
            )
            .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::PermissionDenied, "{shared}");
        }
        assert_eq!(session.load(), before, "nothing was written");
    }

    #[test]
    fn theme_studio_writes_the_active_theme_and_nothing_else_of_the_look() {
        let dir = crate::test_dir();
        let session = session(dir.path());
        session
            .update_section("appearance", object(json!({"high_contrast": true})))
            .unwrap();

        update_for_app(
            &session,
            "so.tilo.themes",
            "appearance",
            object(json!({"theme": "Dracula", "studio": {"id": "dracula"}})),
        )
        .unwrap();
        assert_eq!(
            session.load()["appearance"],
            json!({"high_contrast": true, "theme": "Dracula", "studio": {"id": "dracula"}}),
            "the owned keys land beside the Settings app's"
        );

        let before = session.load();
        let settings_key = update_for_app(
            &session,
            "so.tilo.themes",
            "appearance",
            object(json!({"theme": "Nord", "high_contrast": false})),
        )
        .unwrap_err();
        assert_eq!(settings_key.kind(), ErrorKind::PermissionDenied);
        let other_app = update_for_app(
            &session,
            "so.tilo.notes",
            "appearance",
            object(json!({"theme": "Nord"})),
        )
        .unwrap_err();
        assert_eq!(other_app.kind(), ErrorKind::PermissionDenied);
        let other_section = update_for_app(
            &session,
            "so.tilo.themes",
            "locale",
            object(json!({"theme": "Nord"})),
        )
        .unwrap_err();
        assert_eq!(other_section.kind(), ErrorKind::PermissionDenied);
        assert_eq!(session.load(), before, "nothing was written");
    }

    #[test]
    fn a_personal_entry_too_large_to_roam_is_refused() {
        let dir = crate::test_dir();
        let session = session(dir.path());
        let words = vec!["x".repeat(1024); 80];
        let error = update_for_app(
            &session,
            "so.tilo.monitor",
            APP_PERSONAL_SECTION,
            object(json!({"so.tilo.monitor": {"words": words}})),
        )
        .unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(session.load().is_empty());
    }

    #[test]
    fn an_app_sees_its_own_entries_and_the_shared_sections_only() {
        let document = object(json!({
            "app_personal": {"so.tilo.monitor": {"confirm_kill": true}, "so.tilo.notes": {"a": 1}},
            "app_device_local": {"so.tilo.notes": {"b": 2}},
            "locale": {"region": "DE"},
            "text_assistance": {"check_spelling": false},
            "network": {"wifi": "home"}
        }));
        assert_eq!(
            Value::Object(app_view(&document, "so.tilo.monitor")),
            json!({
                "app_personal": {"so.tilo.monitor": {"confirm_kill": true}},
                "locale": {"region": "DE"},
                "text_assistance": {"check_spelling": false}
            })
        );
    }
}
