//! The desktop's keyboard shortcuts on the action door, to read.
//!
//! The session's action broker publishes the shortcut catalogue
//! ([`settings_store::keyboard_shortcuts`]) next to each workspace's own
//! actions as one resource, [`RESOURCE`], carrying `list`: every shortcut
//! with its label, current and default chords, whether it can be rebound
//! and its category, as JSON. Chords come twice: canonically (`chords`,
//! `defaults`: what the `shortcuts` section's `rebind` takes) and as the
//! active keyboard layout's keycaps spell them (`chord_labels`,
//! `default_labels`: what a person reads, `Strg+Umschalt+T` on a German
//! layout).
//!
//! They are this device's settings, so only callers on this machine are
//! offered them, never a remote device, under the Settings application
//! ([`settings_sections::APP_ID`]). A shortcut is changed through the person's `shortcuts`
//! settings section (`settings.shortcuts`: `rebind`, `reset`, `reset-all`),
//! kept by the session like every other section, so Settings, the shortcut
//! guide, the palette, an agent and a script change it the same way.

use serde_json::{Map, Value, json};
use settings_store::hardware::{KeyChord, ShortcutConfig};
use settings_store::keyboard_shortcuts::{ShortcutEntry, catalogue};
use tilo_actions::contract::{ActionContract, ActionKind, ResultSpec, RetryPolicy};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::outcome::{ActionFailure, ActionOutput};
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionArgs};
use tilo_actions::remote::{CatalogueResource, CatalogueRow};
use tilo_actions::{ExecutionLocus, OperationKind};

use crate::shell::i18n::t;

/// The resource carrying the catalogue-wide actions.
pub const RESOURCE: &str = "keyboard-shortcuts";
pub const RESOURCE_KIND: &str = "settings.keyboard-shortcuts";
/// Every shortcut, as JSON.
pub const LIST_ACTION: &str = "list";

/// Whether `resource` is the one published here.
#[must_use]
pub fn is_resource(resource: &str) -> bool {
    resource == RESOURCE
}

/// What `entry` is called in the user's language.
#[must_use]
pub fn label(entry: &ShortcutEntry) -> String {
    t(&entry.label_key)
}

/// `chord` as people read it (`Super+K`, `Super+Page Up`), in the legends
/// of the active keyboard layout, the way the shortcut guide spells the
/// desktop's fixed chords: a German layout reads `Strg+Umschalt+T`. The
/// chord itself stays canonical; only its label follows the layout.
#[must_use]
pub fn chord_text(chord: &KeyChord) -> String {
    ui::keycaps::active().chord(chord.modifiers.iter().map(String::as_str), &chord.key)
}

/// `chord` in the desktop's canonical US spelling, whatever layout is
/// active: the form callers read back and pass to `rebind`.
#[must_use]
pub fn canonical_chord_text(chord: &KeyChord) -> String {
    ui::Keycaps::US.chord(chord.modifiers.iter().map(String::as_str), &chord.key)
}

/// One catalogue row as the door returns it.
#[must_use]
pub fn entry_json(entry: &ShortcutEntry) -> Value {
    let texts = |chords: &[KeyChord], text: fn(&KeyChord) -> String| {
        chords.iter().map(text).collect::<Vec<_>>()
    };
    json!({
        "id": entry.id,
        "label": label(entry),
        "chords": texts(&entry.chords, canonical_chord_text),
        "chord_labels": texts(&entry.chords, chord_text),
        "defaults": texts(&entry.defaults, canonical_chord_text),
        "default_labels": texts(&entry.defaults, chord_text),
        "rebindable": entry.rebindable,
        "category": entry.category.id(),
        "category_label": t(&entry.category.label_key()),
    })
}

fn list_json(config: &ShortcutConfig) -> Value {
    json!({
        "shortcuts": catalogue(config).iter().map(entry_json).collect::<Vec<_>>(),
    })
}

fn contract(result: Value) -> ActionContract {
    ActionContract {
        kind: ActionKind::Read,
        retry: RetryPolicy::Idempotent,
        result: ResultSpec::Json { schema: result },
        tags: vec![
            "configuration".into(),
            "system-setting".into(),
            "keyboard-shortcuts".into(),
        ],
        ..ActionContract::view()
    }
}

fn entry_schema() -> Value {
    json!({
        "type": "object",
        "required": ["id", "label", "chords", "chord_labels", "defaults", "default_labels", "rebindable", "category"],
    })
}

fn list_schema() -> Value {
    json!({
        "type": "object",
        "required": ["shortcuts"],
        "properties": {"shortcuts": {"type": "array", "items": entry_schema()}},
    })
}

/// The one descriptor published here: `list` on [`RESOURCE`].
#[must_use]
pub fn descriptor() -> ActionDescriptor {
    ActionDescriptor {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(RESOURCE, LIST_ACTION),
        resource_kind: RESOURCE_KIND.into(),
        resource_label: t("keyboard-shortcuts-resource"),
        action_label: t("keyboard-shortcuts-list"),
        keywords: Vec::new(),
        icon: None,
        description: Some(t("keyboard-shortcuts-list-description")),
        locus: ExecutionLocus::default(),
        kind: OperationKind::View,
        contract: contract(list_schema()),
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

#[must_use]
pub fn descriptor_at(address: &ActionAddress) -> Option<ActionDescriptor> {
    let descriptor = descriptor();
    (descriptor.address == *address).then_some(descriptor)
}

/// The published descriptor as a catalogue row of `provider`.
#[must_use]
pub fn catalogue_rows(provider: &str) -> Vec<CatalogueRow> {
    let descriptor = descriptor();
    vec![CatalogueRow {
        provider: provider.to_string(),
        app_id: settings_sections::APP_ID.to_string(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: descriptor.address.resource.clone(),
            kind: descriptor.resource_kind.clone(),
            label: descriptor.resource_label.clone(),
            presentation: None,
        },
        action: Some(descriptor),
        has_children: false,
        visibility: None,
    }]
}

fn failure(code: &str, message: String) -> ActionFailure {
    ActionFailure {
        code: code.into(),
        message,
        details: None,
        retryable: false,
    }
}

/// Answer an invocation of `address` with `argument` from `config`.
///
/// # Errors
///
/// Why the invocation names nothing published here, or why its arguments
/// do not satisfy the action's parameters.
pub fn apply(
    config: &ShortcutConfig,
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<ActionOutput, Box<ActionFailure>> {
    let descriptor = descriptor_at(address).ok_or_else(|| {
        Box::new(failure(
            "unknown_action",
            format!(
                "no keyboard shortcut action `{}` on `{}`",
                address.action, address.resource
            ),
        ))
    })?;
    if let Some(slot) = argument.filter(|slot| !slot.is_empty()) {
        let json = slot.strip_prefix(ARGUMENTS_JSON_PREFIX).ok_or_else(|| {
            Box::new(failure(
                "invalid_arguments",
                "keyboard shortcut actions take JSON arguments".into(),
            ))
        })?;
        let supplied: Map<String, Value> = serde_json::from_str(json)
            .map_err(|error| Box::new(failure("invalid_arguments", error.to_string())))?;
        ActionArgs::bind_json(&descriptor.params, &supplied)
            .map_err(|error| Box::new(failure("invalid_arguments", error.to_string())))?;
    }
    Ok(ActionOutput {
        value: Some(list_json(config)),
        resources: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_carry_canonical_chords_and_labels_in_the_active_layout() {
        let config = ShortcutConfig::default();
        let list = {
            let _german = ui::keycaps::scoped(ui::Keycaps::from_xkb("de", "nodeadkeys"));
            list_json(&config)
        };
        let row = |id: &str| {
            list["shortcuts"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == id))
                .cloned()
                .unwrap_or_default()
        };
        let mouse_keys = row("fixed-toggle-mouse-keys");
        assert_eq!(mouse_keys["chords"], json!(["Alt+Shift+Num Lock"]));
        assert_eq!(mouse_keys["chord_labels"], json!(["Alt+Umschalt+Num"]));
        let terminal = row(settings_store::hardware::ShortcutAction::OpenTerminal.id());
        assert_eq!(terminal["defaults"], json!(["Ctrl+Alt+T"]));
        assert_eq!(terminal["default_labels"], json!(["Strg+Alt+T"]));
        let chord = terminal["chords"][0].as_str().unwrap_or_default();
        assert_eq!(
            KeyChord::parse(chord),
            Ok(ShortcutConfig::default().bindings
                [&settings_store::hardware::ShortcutAction::OpenTerminal]
                .clone())
        );
    }

    /// `list` names every shortcut, fixed ones as not rebindable, and is the
    /// one action published here.
    #[test]
    fn the_catalogue_is_listed_and_nothing_else() {
        let listed = apply(
            &ShortcutConfig::default(),
            &ActionAddress::new(RESOURCE, LIST_ACTION),
            None,
        )
        .unwrap()
        .value
        .unwrap();
        let rows = listed["shortcuts"].as_array().unwrap();
        let close = rows
            .iter()
            .find(|row| row["id"] == "fixed-close-tab")
            .unwrap();
        assert_eq!(close["rebindable"], false);
        assert_eq!(close["chords"], json!(["Alt+F4", "Super+W"]));
        assert_eq!(close["category"], "windows");
        assert!(descriptor_at(&ActionAddress::new(RESOURCE, "reset-all")).is_none());
        assert!(descriptor_at(&ActionAddress::new("keyboard-shortcut:search", "rebind")).is_none());
    }
}
