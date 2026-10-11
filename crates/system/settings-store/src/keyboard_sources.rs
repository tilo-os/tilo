//! Keyboard assignments retain absent devices' choices independently of discovery.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hardware::RichInputConfig;

/// The shared physical-keyboard default and the separate software keyboard.
pub const ALL: &str = "all";
pub const OSK: &str = "osk";

/// Editing a configured source changes its options, not the source being typed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceOptions {
    pub model: Option<String>,
    pub compose_key: Option<String>,
    pub caps_lock: Option<String>,
    /// Distinguish an explicitly disabled option from an inherited one.
    pub customised: bool,
}

/// An ordered source assignment. Device keys are discovery identities, never indices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyboardProfile {
    pub sources: Vec<String>,
    pub active: String,
    #[serde(default)]
    pub options: BTreeMap<String, SourceOptions>,
}

impl KeyboardProfile {
    #[must_use]
    pub fn from_input(input: &RichInputConfig) -> Self {
        Self {
            sources: input.keyboard_layouts.clone(),
            active: input.keyboard_layout.clone(),
            options: BTreeMap::new(),
        }
    }

    /// Invalid physical assignments fall back through the existing migration rules.
    #[must_use]
    pub fn effective(&self, input: &RichInputConfig) -> RichInputConfig {
        let mut result = input.clone();
        result.keyboard_layouts.clone_from(&self.sources);
        result.keyboard_layout.clone_from(&self.active);
        result.normalise_sources();
        if let Some(options) = self.options.get(&result.keyboard_layout) {
            if let Some(model) = options
                .model
                .as_ref()
                .filter(|model| matches!(model.as_str(), "pc104" | "pc105" | "jp106" | "kr106"))
            {
                result.keyboard_model.clone_from(model);
            }
            if options.customised {
                result.compose_key.clone_from(&options.compose_key);
                result.caps_lock.clone_from(&options.caps_lock);
            }
        }
        result
    }
}

/// Decode profiles individually: one obsolete assignment cannot erase its siblings.
///
/// # Errors
/// Returns the deserializer error when the surrounding value cannot be read.
pub fn lenient_profiles<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, KeyboardProfile>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_object()
        .into_iter()
        .flat_map(|object| object.iter())
        .filter(|(key, _)| {
            !key.is_empty() && key.len() <= 1024 && !key.chars().any(char::is_control)
        })
        .filter_map(|(key, value)| {
            serde_json::from_value(value.clone())
                .ok()
                .map(|profile| (key.clone(), profile))
        })
        .collect())
}

/// Return the configured assignment, falling back without creating or losing an override.
#[must_use]
pub fn effective(input: &RichInputConfig, scope: &str) -> RichInputConfig {
    if scope != ALL
        && let Some(profile) = input.keyboard_assignments.get(scope)
    {
        return profile.effective(input);
    }
    let mut profile = KeyboardProfile::from_input(input);
    if let Some(defaults) = input.keyboard_assignments.get(ALL) {
        profile.options.clone_from(&defaults.options);
    }
    profile.effective(input)
}

/// Resolve only a currently attached typing keyboard from the compositor's leased snapshot.
#[must_use]
pub fn active_scope() -> String {
    crate::hardware::InputHardwareSnapshot::load()
        .and_then(|snapshot| {
            snapshot.active_keyboard.filter(|key| {
                snapshot.devices.iter().any(|device| {
                    device.kind == crate::hardware::InputDeviceKind::Keyboard && &device.key == key
                })
            })
        })
        .unwrap_or_else(|| ALL.into())
}

/// Effective physical sources follow the keyboard in use, without modifying defaults.
#[must_use]
pub fn current(input: &RichInputConfig) -> RichInputConfig {
    effective(input, &active_scope())
}

/// Serial/physical location distinguishes identical keyboards without using event numbers.
#[must_use]
pub fn device_key(base: &str, serial: &str, physical: &str) -> String {
    let location = if serial.trim().is_empty() {
        physical.trim()
    } else {
        serial.trim()
    };
    if location.is_empty() {
        base.to_owned()
    } else {
        format!("{base}@{location}")
    }
}

/// The X11 keyboard configuration systemd-localed writes for
/// `localectl set-x11-keymap`.
const LOCALED_X11_CONFIG: &str = "/etc/X11/xorg.conf.d/00-keyboard.conf";
/// The console configuration; systemd-localed also records the X11 layout
/// here as `XKBLAYOUT`/`XKBVARIANT`.
const VCONSOLE_CONFIG: &str = "/etc/vconsole.conf";
/// Both files are a few lines; anything larger is not a keyboard config.
const SYSTEM_CONFIG_LIMIT: u64 = 64 * 1024;

/// The input sources this machine was set up with, for a person who has not
/// chosen any. In order: the session's `XKB_DEFAULT_LAYOUT` and
/// `XKB_DEFAULT_VARIANT` (the installer exports them for every session, and
/// they are what libxkbcommon applies to a keymap compiled without names),
/// then systemd-localed's X11 layout, then the console's `XKBLAYOUT`.
/// Empty when none of them names a valid layout.
#[must_use]
pub fn system_sources() -> Vec<String> {
    system_sources_from(&|name| std::env::var(name).ok(), &read_system_config)
}

/// The sources a person who has chosen none types: the machine's
/// ([`system_sources`]), else US. Never empty.
#[must_use]
pub fn default_sources() -> Vec<String> {
    let mut sources = system_sources();
    if sources.is_empty() {
        sources.push("us".into());
    }
    sources
}

/// The layout a document falls back to when it names no usable one: the
/// system's primary layout, else US.
#[must_use]
pub fn fallback_layout() -> String {
    default_sources().swap_remove(0)
}

/// [`system_sources`] over an environment `variable` reader and a
/// configuration `file` reader.
fn system_sources_from(
    variable: &dyn Fn(&str) -> Option<String>,
    file: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let variable = |name: &str| variable(name).unwrap_or_default();
    let from_environment = sources_from_names(
        &variable("XKB_DEFAULT_LAYOUT"),
        &variable("XKB_DEFAULT_VARIANT"),
    );
    if !from_environment.is_empty() {
        return from_environment;
    }
    if let Some(text) = file(LOCALED_X11_CONFIG) {
        let sources = xorg_keyboard_sources(&text);
        if !sources.is_empty() {
            return sources;
        }
    }
    file(VCONSOLE_CONFIG)
        .map(|text| vconsole_sources(&text))
        .unwrap_or_default()
}

fn read_system_config(path: &str) -> Option<String> {
    let bytes = tilo_files::native::bounded::read(
        std::path::Path::new(path),
        usize::try_from(SYSTEM_CONFIG_LIMIT).ok()?,
    )
    .ok()??;
    String::from_utf8(bytes).ok()
}

/// Source ids from XKB rule names: comma-separated layouts with one variant
/// slot each, as `XKB_DEFAULT_*`, localed and setxkbmap spell them.
/// Invalid and duplicate entries are dropped; the list is bounded to the
/// XKB group limit.
fn sources_from_names(layouts: &str, variants: &str) -> Vec<String> {
    let mut variants = variants.split(',');
    let mut sources = Vec::new();
    for layout in layouts.split(',') {
        let variant = variants.next().unwrap_or_default().trim();
        let layout = layout.trim();
        if layout.is_empty() {
            continue;
        }
        let id = if variant.is_empty() {
            layout.to_owned()
        } else {
            format!("{layout}:{variant}")
        };
        if crate::hardware::KeyboardInputSource::parse(&id).is_some()
            && !sources.contains(&id)
            && sources.len() < crate::hardware::MAX_KEYBOARD_INPUT_SOURCES
        {
            sources.push(id);
        }
    }
    sources
}

/// `XKBLAYOUT`/`XKBVARIANT` from an env-style `vconsole.conf`.
fn vconsole_sources(text: &str) -> Vec<String> {
    let value = |key: &str| {
        text.lines()
            .filter_map(|line| line.trim().split_once('='))
            .filter(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim().trim_matches(['"', '\'']).to_owned())
            .next_back()
            .unwrap_or_default()
    };
    sources_from_names(&value("XKBLAYOUT"), &value("XKBVARIANT"))
}

/// `Option "XkbLayout"`/`"XkbVariant"` from localed's xorg.conf snippet.
fn xorg_keyboard_sources(text: &str) -> Vec<String> {
    let value = |key: &str| {
        text.lines()
            .filter_map(|line| line.trim().strip_prefix("Option"))
            .filter_map(|rest| {
                let mut quoted = rest.split('"').skip(1).step_by(2);
                let name = quoted.next()?;
                let value = quoted.next()?;
                name.eq_ignore_ascii_case(key).then(|| value.to_owned())
            })
            .next_back()
            .unwrap_or_default()
    };
    sources_from_names(&value("XkbLayout"), &value("XkbVariant"))
}

/// The active input source: `layout` or `layout:variant`, under the historic
/// key older Settings builds and the compositor agree on.
pub const LAYOUT: crate::Key<String> =
    crate::Key::new(crate::hardware::INPUT_SECTION, "keyboard_layout", || {
        "us".into()
    });

/// The ordered input sources the person switches between.
pub const LAYOUTS: crate::Key<Vec<String>> =
    crate::Key::new(crate::hardware::INPUT_SECTION, "keyboard_layouts", || {
        vec!["us".into()]
    });

/// The shared sources with `wanted` active: a source already in the list is
/// switched to; a new one joins the list first, under the same limits
/// Settings applies ([`RichInputConfig::can_add_source`]).
///
/// # Errors
///
/// `wanted` is not an XKB layout (`layout` or `layout:variant`) or an
/// input-method engine, or the list has no room for it.
pub fn plan_active_source(
    input: &RichInputConfig,
    wanted: &str,
) -> Result<RichInputConfig, String> {
    let source = crate::hardware::InputSource::parse(wanted)
        .ok_or_else(|| format!("{wanted:?} is not a keyboard layout (layout or layout:variant)"))?;
    let id = source.id();
    let mut next = effective(input, ALL);
    if !next.keyboard_layouts.contains(&id) {
        if !next.can_add_source(&id) {
            return Err(format!(
                "there is no room for another keyboard layout: {}",
                next.keyboard_layouts.join(", ")
            ));
        }
        next.keyboard_layouts.push(id.clone());
    }
    next.keyboard_layout = id;
    next.normalise_sources();
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_assignments_keep_defaults_and_disconnected_profiles() {
        let mut input = RichInputConfig {
            keyboard_layouts: vec!["us".into(), "fr".into()],
            ..Default::default()
        };
        input.keyboard_assignments.insert(
            "external".into(),
            KeyboardProfile {
                sources: vec!["de".into(), "us".into()],
                active: "de".into(),
                options: BTreeMap::from([(
                    "de".into(),
                    SourceOptions {
                        model: Some("pc104".into()),
                        compose_key: Some("ralt".into()),
                        caps_lock: Some("escape".into()),
                        customised: true,
                    },
                )]),
            },
        );
        let default = effective(&input, ALL);
        assert_eq!(default.keyboard_layouts, ["us", "fr"]);
        let assigned = effective(&input, "external");
        assert_eq!(assigned.keyboard_layouts, ["de", "us"]);
        assert_eq!(assigned.keyboard_layout, "de");
        assert_eq!(assigned.keyboard_model, "pc104");
        assert_eq!(assigned.compose_key.as_deref(), Some("ralt"));
        assert_eq!(effective(&input, "unassigned"), default);
        let restored: RichInputConfig =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(restored.keyboard_assignments, input.keyboard_assignments);
    }

    #[test]
    fn the_machine_layout_is_read_from_xkb_names_localed_and_the_console() {
        assert_eq!(sources_from_names("de", ""), ["de"]);
        assert_eq!(
            sources_from_names("de,us", "nodeadkeys,"),
            ["de:nodeadkeys", "us"]
        );
        assert_eq!(
            sources_from_names(" de , ../evil, de", ""),
            ["de"],
            "invalid and repeated names are dropped"
        );
        assert!(sources_from_names("", "nodeadkeys").is_empty());
        assert_eq!(sources_from_names("a,b,c,d,e", "").len(), 4);

        let vconsole =
            "# Written by systemd-localed(8)\nKEYMAP=de\nXKBLAYOUT=\"de\"\nXKBVARIANT=nodeadkeys\n";
        assert_eq!(vconsole_sources(vconsole), ["de:nodeadkeys"]);
        assert!(
            vconsole_sources("KEYMAP=de-latin1\n").is_empty(),
            "a console keymap alone is not an XKB layout"
        );

        let xorg = r#"Section "InputClass"
        Identifier "system-keyboard"
        MatchIsKeyboard "on"
        Option "XkbLayout" "de,us"
        Option "XkbVariant" ","
EndSection
"#;
        assert_eq!(xorg_keyboard_sources(xorg), ["de", "us"]);
        assert!(xorg_keyboard_sources("Section \"InputClass\"\nEndSection\n").is_empty());
    }

    #[test]
    fn the_session_environment_wins_over_localed_and_the_console() {
        let environment = |layout: &'static str| {
            move |name: &str| match name {
                "XKB_DEFAULT_LAYOUT" => Some(layout.to_owned()),
                "XKB_DEFAULT_VARIANT" => Some("nodeadkeys".to_owned()),
                _ => None,
            }
        };
        let files = |path: &str| match path {
            LOCALED_X11_CONFIG => Some("Option \"XkbLayout\" \"fr\"\n".to_owned()),
            VCONSOLE_CONFIG => Some("XKBLAYOUT=es\n".to_owned()),
            _ => None,
        };
        let nothing = |_: &str| None;
        assert_eq!(
            system_sources_from(&environment("de"), &files),
            ["de:nodeadkeys"]
        );
        assert_eq!(system_sources_from(&environment(""), &files), ["fr"]);
        assert_eq!(
            system_sources_from(&nothing, &|path: &str| {
                (path == VCONSOLE_CONFIG).then(|| "XKBLAYOUT=es\n".to_owned())
            }),
            ["es"]
        );
        assert!(system_sources_from(&nothing, &nothing).is_empty());
    }

    #[test]
    fn keyboard_assignments_decode_valid_siblings_and_stable_identities() {
        let section = serde_json::json!({"keyboard_assignments": {
            "broken": {"sources": 9}, "usb": {"sources": ["de"], "active": "de"}
        }});
        let input = RichInputConfig::from_section(section.as_object().unwrap());
        assert!(!input.keyboard_assignments.contains_key("broken"));
        assert_eq!(effective(&input, "usb").keyboard_layout, "de");
        assert_eq!(
            device_key("keyboard", "serial", "usb1"),
            device_key("keyboard", "serial", "usb2")
        );
        assert_ne!(
            device_key("keyboard", "", "usb1"),
            device_key("keyboard", "", "usb2")
        );
    }

    #[test]
    fn choosing_a_source_switches_to_a_listed_one_and_adds_a_new_one() {
        let input = RichInputConfig {
            keyboard_layouts: vec!["us".into(), "de".into()],
            keyboard_layout: "us".into(),
            ..RichInputConfig::default()
        };
        let switched = plan_active_source(&input, "de").unwrap();
        assert_eq!(switched.keyboard_layout, "de");
        assert_eq!(switched.keyboard_layouts, vec!["us", "de"]);
        let added = plan_active_source(&input, "fr:azerty").unwrap();
        assert_eq!(added.keyboard_layout, "fr:azerty");
        assert_eq!(added.keyboard_layouts, vec!["us", "de", "fr:azerty"]);
        assert!(plan_active_source(&input, "../etc").is_err());
        assert!(plan_active_source(&input, "").is_err());
    }
}
