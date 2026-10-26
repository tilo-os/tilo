//! The desktop's keyboard shortcuts on the action door.
//!
//! The session's action broker publishes the shortcut catalogue
//! ([`settings_store::keyboard_shortcuts`]) next to each workspace's own
//! actions, so the palette, `tilo-control`, the MCP bridge, an agent and a
//! peer all read and change shortcuts the same way:
//!
//! - one resource, [`RESOURCE`], carrying `list` (every shortcut with its
//!   label, current and default chords, whether it can be rebound and its
//!   category, as JSON) and `reset-all`. Chords come twice: canonically
//!   (`chords`, `defaults`: what `rebind` takes back) and as the active
//!   keyboard layout's keycaps spell them (`chord_labels`, `default_labels`:
//!   what a person reads, `Strg+Umschalt+T` on a German layout);
//! - one resource per rebindable shortcut, `keyboard-shortcut:<id>`,
//!   carrying `rebind` (to the chord given) and `reset`.
//!
//! They are this device's settings, so only callers on this machine are
//! offered them, never a remote device. They belong to the Settings
//! application ([`APP_ID`]): the grant that lets a caller change any other
//! setting is the one that lets it rebind a key, and a caller without it is
//! shown none of this. A rebind that would take
//! a chord something else holds fails with the holder named in the
//! failure's details; nothing is ever taken from another shortcut.
//!
//! The shortcut guide and Settings render the same catalogue, and a change
//! saved here reaches the compositor's key routing through the settings
//! file like any change made in Settings.

use serde_json::{Map, Value, json};
use settings_store::hardware::{KeyChord, ShortcutAction, ShortcutConfig, ShortcutConfigError};
use settings_store::keyboard_shortcuts::{ShortcutEntry, catalogue, fixed_shortcut};
use tilo_actions::contract::{ActionContract, ActionKind, ResultSpec, RetryPolicy};
use tilo_actions::door::{ActionAddress, ActionDescriptor, InputSpec};
use tilo_actions::outcome::{ActionFailure, ActionOutput};
use tilo_actions::params::{ARGUMENTS_JSON_PREFIX, ActionArgs, ActionParam, ParamType};
use tilo_actions::remote::{CatalogueResource, CatalogueRow};
use tilo_actions::resource::ResourcePresentation;
use tilo_actions::{ExecutionLocus, OperationKind};

use crate::shell::i18n::{t, t_args};

/// The application these actions belong to, for capability checks.
pub const APP_ID: &str = "so.tilo.settings";
/// The resource carrying the catalogue-wide actions.
pub const RESOURCE: &str = "keyboard-shortcuts";
pub const RESOURCE_KIND: &str = "settings.keyboard-shortcuts";
pub const ENTRY_KIND: &str = "settings.keyboard-shortcut";
/// Every shortcut, as JSON.
pub const LIST_ACTION: &str = "list";
/// Put every desktop action back on its default chord.
pub const RESET_ALL_ACTION: &str = "reset-all";
/// Move one shortcut to another chord.
pub const REBIND_ACTION: &str = "rebind";
/// Put one shortcut back on its default chord.
pub const RESET_ACTION: &str = "reset";

const ENTRY_PREFIX: &str = "keyboard-shortcut:";

#[must_use]
pub fn entry_resource(id: &str) -> String {
    format!("{ENTRY_PREFIX}{id}")
}

/// Whether `resource` is one of the resources published here.
#[must_use]
pub fn is_resource(resource: &str) -> bool {
    resource == RESOURCE || resource.starts_with(ENTRY_PREFIX)
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

fn chords_text(chords: &[KeyChord]) -> String {
    if chords.is_empty() {
        return t("guide-unassigned");
    }
    chords
        .iter()
        .map(chord_text)
        .collect::<Vec<_>>()
        .join("  /  ")
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

fn contract(kind: ActionKind, result: Value) -> ActionContract {
    ActionContract {
        kind,
        retry: RetryPolicy::Idempotent,
        result: ResultSpec::Json { schema: result },
        tags: vec![
            "configuration".into(),
            "system-setting".into(),
            "keyboard-shortcuts".into(),
        ],
        ..if kind == ActionKind::Read {
            ActionContract::view()
        } else {
            ActionContract::default()
        }
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

fn chord_param() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "chord",
        ParamType::Text,
        "The new chord, e.g. Super+J or Ctrl+Alt+Left",
    )]
}

/// The resource a published action sits on: [`RESOURCE`] or one shortcut's.
struct ShortcutResource {
    id: String,
    kind: &'static str,
    label: String,
}

impl ShortcutResource {
    /// The parameterless `action` on this resource.
    fn action(
        &self,
        action: &str,
        action_label: String,
        description: String,
        kind: OperationKind,
        contract: ActionContract,
    ) -> ActionDescriptor {
        ActionDescriptor {
            semantic_identity: None,
            address: ActionAddress::new(self.id.clone(), action),
            resource_kind: self.kind.into(),
            resource_label: self.label.clone(),
            action_label,
            description: Some(description),
            locus: ExecutionLocus::default(),
            kind,
            contract,
            input: InputSpec::None,
            params: Vec::new(),
            view: None,
        }
    }
}

/// Every descriptor published under `config`, each with the presentation of
/// the resource it sits on: `list` and `reset-all` on [`RESOURCE`], then
/// `rebind` and `reset` on every rebindable shortcut.
#[must_use]
pub fn descriptors(
    config: &ShortcutConfig,
) -> Vec<(ActionDescriptor, Option<ResourcePresentation>)> {
    let resource = ShortcutResource {
        id: RESOURCE.into(),
        kind: RESOURCE_KIND,
        label: t("keyboard-shortcuts-resource"),
    };
    let mut out = vec![
        (
            resource.action(
                LIST_ACTION,
                t("keyboard-shortcuts-list"),
                t("keyboard-shortcuts-list-description"),
                OperationKind::View,
                contract(ActionKind::Read, list_schema()),
            ),
            None,
        ),
        (
            resource.action(
                RESET_ALL_ACTION,
                t("keyboard-shortcuts-reset-all"),
                t("keyboard-shortcuts-reset-all-description"),
                OperationKind::Effect,
                contract(ActionKind::Undoable, list_schema()),
            ),
            None,
        ),
    ];
    for entry in catalogue(config).iter().filter(|entry| entry.rebindable) {
        let name = label(entry);
        let presentation = ResourcePresentation {
            detail: Some(chords_text(&entry.chords)),
            badge: Some(t(&entry.category.label_key())),
            preview: None,
        };
        let presentation = presentation.is_valid().then_some(presentation);
        let shortcut = || [("shortcut", name.clone().into())];
        let resource = ShortcutResource {
            id: entry_resource(entry.id),
            kind: ENTRY_KIND,
            label: name.clone(),
        };
        out.push((
            ActionDescriptor {
                semantic_identity: None,
                params: chord_param(),
                ..resource.action(
                    REBIND_ACTION,
                    t_args("keyboard-shortcut-rebind", &shortcut()),
                    t_args(
                        "keyboard-shortcut-rebind-description",
                        &[
                            ("shortcut", name.clone().into()),
                            ("chords", chords_text(&entry.chords).into()),
                            ("defaults", chords_text(&entry.defaults).into()),
                        ],
                    ),
                    OperationKind::Effect,
                    contract(ActionKind::Undoable, entry_schema()),
                )
            },
            presentation.clone(),
        ));
        out.push((
            resource.action(
                RESET_ACTION,
                t_args("keyboard-shortcut-reset", &shortcut()),
                t_args(
                    "keyboard-shortcut-reset-description",
                    &[
                        ("shortcut", name.clone().into()),
                        ("defaults", chords_text(&entry.defaults).into()),
                    ],
                ),
                OperationKind::Effect,
                contract(ActionKind::Undoable, entry_schema()),
            ),
            presentation,
        ));
    }
    out
}

#[must_use]
pub fn descriptor_at(config: &ShortcutConfig, address: &ActionAddress) -> Option<ActionDescriptor> {
    descriptors(config)
        .into_iter()
        .map(|(descriptor, _)| descriptor)
        .find(|descriptor| descriptor.address == *address)
}

/// Every published descriptor as a catalogue row of `provider`. The
/// catalogue-wide actions sit at the top level; each shortcut's sit under
/// [`RESOURCE`].
#[must_use]
pub fn catalogue_rows(provider: &str, config: &ShortcutConfig) -> Vec<CatalogueRow> {
    descriptors(config)
        .into_iter()
        .map(|(descriptor, presentation)| {
            let top = descriptor.address.resource == RESOURCE;
            CatalogueRow {
                provider: provider.to_string(),
                app_id: APP_ID.to_string(),
                parent_resource: if top {
                    String::new()
                } else {
                    RESOURCE.to_string()
                },
                resource: CatalogueResource {
                    id: descriptor.address.resource.clone(),
                    kind: descriptor.resource_kind.clone(),
                    label: descriptor.resource_label.clone(),
                    presentation,
                },
                action: Some(descriptor),
                has_children: top,
                visibility: None,
            }
        })
        .collect()
}

/// One decoded invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    List,
    ResetAll,
    Rebind {
        action: ShortcutAction,
        chord: KeyChord,
    },
    Reset(ShortcutAction),
}

impl Operation {
    /// Whether applying it changes the saved table.
    #[must_use]
    pub const fn changes(&self) -> bool {
        !matches!(self, Self::List)
    }
}

fn failure(code: &str, message: String, details: Option<Value>) -> ActionFailure {
    ActionFailure {
        code: code.into(),
        message,
        details,
        retryable: false,
    }
}

/// Decode an invocation of `action` on `resource` from its argument slot.
///
/// # Errors
///
/// Why the invocation names nothing published here, or why its arguments
/// do not satisfy the action's parameters.
pub fn decode(
    config: &ShortcutConfig,
    address: &ActionAddress,
    argument: Option<&str>,
) -> Result<Operation, Box<ActionFailure>> {
    let descriptor = descriptor_at(config, address).ok_or_else(|| {
        failure(
            "unknown_action",
            format!(
                "no keyboard shortcut action `{}` on `{}`",
                address.action, address.resource
            ),
            None,
        )
    })?;
    let supplied = match argument {
        None | Some("") => Map::new(),
        Some(slot) => {
            let invalid = |message: String| failure("invalid_arguments", message, None);
            let json = slot
                .strip_prefix(ARGUMENTS_JSON_PREFIX)
                .ok_or_else(|| invalid("keyboard shortcut actions take JSON arguments".into()))?;
            serde_json::from_str(json).map_err(|error| invalid(error.to_string()))?
        }
    };
    let bound = ActionArgs::bind_json(&descriptor.params, &supplied)
        .map_err(|error| failure("invalid_arguments", error.to_string(), None))?;
    if address.resource == RESOURCE {
        return Ok(if address.action == LIST_ACTION {
            Operation::List
        } else {
            Operation::ResetAll
        });
    }
    let action = address
        .resource
        .strip_prefix(ENTRY_PREFIX)
        .and_then(ShortcutAction::from_id)
        .ok_or_else(|| failure("unknown_action", "no such shortcut".into(), None))?;
    if address.action == RESET_ACTION {
        return Ok(Operation::Reset(action));
    }
    let text = bound.text("chord").unwrap_or_default();
    let chord = KeyChord::parse(text).map_err(|reason| {
        failure(
            "invalid_chord",
            t("keyboard-shortcut-invalid"),
            Some(json!({"chord": text, "reason": reason})),
        )
    })?;
    Ok(Operation::Rebind { action, chord })
}

/// Why `chord` cannot be given, with the shortcut or action holding it.
fn refusal(
    config: &ShortcutConfig,
    chord: Option<&KeyChord>,
    error: ShortcutConfigError,
) -> ActionFailure {
    let chord_json = chord.map(canonical_chord_text);
    let chord_label = chord.map(chord_text);
    let holder = |id: &str, name: String, kind: &str| json!({"chord": chord_json, "conflict": {"kind": kind, "id": id, "label": name}});
    let message = |name: &str| {
        t_args(
            "keyboard-shortcut-conflict",
            &[
                ("chord", chord_label.clone().unwrap_or_default().into()),
                ("shortcut", name.to_string().into()),
            ],
        )
    };
    match error {
        ShortcutConfigError::Conflict(other) => {
            let name = t(&other.label_key());
            failure(
                "shortcut_conflict",
                message(&name),
                Some(holder(other.id(), name, "shortcut")),
            )
        }
        ShortcutConfigError::Fixed(id) => {
            let name =
                fixed_shortcut(id).map_or_else(|| id.to_string(), |fixed| t(fixed.label_key));
            failure(
                "shortcut_conflict",
                message(&name),
                Some(holder(id, name, "fixed")),
            )
        }
        ShortcutConfigError::TakenByAction => {
            let held = chord
                .and_then(|chord| config.action_for(chord))
                .or_else(|| {
                    // `reset-all` names no chord: report the first default
                    // a published action holds.
                    ShortcutConfig::default()
                        .bindings
                        .values()
                        .find_map(|chord| config.action_for(chord))
                });
            let name = held.map(|bound| bound.label.clone()).unwrap_or_default();
            let chord_label = held.map(|bound| chord_text(&bound.chord)).or(chord_label);
            let chord_json = held
                .map(|bound| canonical_chord_text(&bound.chord))
                .or(chord_json);
            failure(
                "shortcut_conflict",
                t_args(
                    "keyboard-shortcut-conflict",
                    &[
                        ("chord", chord_label.unwrap_or_default().into()),
                        ("shortcut", name.clone().into()),
                    ],
                ),
                Some(json!({
                    "chord": chord_json,
                    "conflict": {
                        "kind": "action",
                        "label": name,
                        "target": held.map(|bound| json!({
                            "provider": bound.target.provider,
                            "resource": bound.target.resource,
                            "action": bound.target.action,
                        })),
                    },
                })),
            )
        }
        ShortcutConfigError::ReservedSystemKey => failure(
            "invalid_chord",
            t("guide-rebind-reserved"),
            Some(json!({"chord": chord_json})),
        ),
        ShortcutConfigError::InvalidChord => failure(
            "invalid_chord",
            t("keyboard-shortcut-invalid"),
            Some(json!({"chord": chord_json})),
        ),
    }
}

/// Apply `operation` to `config`. On a refusal `config` is unchanged.
///
/// # Errors
///
/// Why the change cannot be made; a clash names the shortcut, fixed
/// desktop shortcut or published action holding the chord in the
/// failure's `details.conflict`.
pub fn apply(
    config: &mut ShortcutConfig,
    operation: &Operation,
) -> Result<ActionOutput, Box<ActionFailure>> {
    let value = match operation {
        Operation::List => list_json(config),
        Operation::ResetAll => {
            config
                .reset_all()
                .map_err(|error| refusal(config, None, error))?;
            list_json(config)
        }
        Operation::Rebind { action, chord } => {
            config
                .set(*action, chord.clone())
                .map_err(|error| refusal(config, Some(chord), error))?;
            entry_value(config, *action)
        }
        Operation::Reset(action) => {
            let default = ShortcutConfig::default().bindings.get(action).cloned();
            config
                .reset(*action)
                .map_err(|error| refusal(config, default.as_ref(), error))?;
            entry_value(config, *action)
        }
    };
    Ok(ActionOutput {
        value: Some(value),
        resources: Vec::new(),
    })
}

fn entry_value(config: &ShortcutConfig, action: ShortcutAction) -> Value {
    settings_store::keyboard_shortcuts::entry(config, action.id())
        .map_or(Value::Null, |entry| entry_json(&entry))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(resource: &str, action: &str) -> ActionAddress {
        ActionAddress::new(resource, action)
    }

    fn rebind(config: &ShortcutConfig, id: &str, chord: &str) -> Operation {
        decode(
            config,
            &address(&entry_resource(id), REBIND_ACTION),
            Some(&format!(
                "{ARGUMENTS_JSON_PREFIX}{}",
                json!({"chord": chord})
            )),
        )
        .expect("a well-formed rebind")
    }

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
        let terminal = row(ShortcutAction::OpenTerminal.id());
        assert_eq!(terminal["defaults"], json!(["Ctrl+Alt+T"]));
        assert_eq!(terminal["default_labels"], json!(["Strg+Alt+T"]));
        let chord = terminal["chords"][0].as_str().unwrap_or_default();
        assert_eq!(
            KeyChord::parse(chord),
            Ok(ShortcutConfig::default().bindings[&ShortcutAction::OpenTerminal].clone())
        );
    }

    #[test]
    fn a_clash_names_the_shortcut_holding_the_chord_and_changes_nothing() {
        let mut config = ShortcutConfig::default();
        let before = config.clone();
        let operation = rebind(&config, "quick-settings", "Super+Space");
        let error = apply(&mut config, &operation).expect_err("Super+Space opens search");
        assert_eq!(error.code, "shortcut_conflict");
        let details = error.details.expect("the holder is named");
        assert_eq!(details["conflict"]["kind"], "shortcut");
        assert_eq!(details["conflict"]["id"], "search");
        assert_eq!(details["conflict"]["label"], "Search everywhere");
        assert_eq!(details["chord"], "Super+Space");
        assert_eq!(config, before);
    }

    #[test]
    fn a_clash_with_a_fixed_chord_names_the_fixed_shortcut() {
        let mut config = ShortcutConfig::default();
        let operation = rebind(&config, "search", "Alt+Tab");
        let error = apply(&mut config, &operation).expect_err("Alt+Tab switches windows");
        let details = error.details.expect("the holder is named");
        assert_eq!(details["conflict"]["kind"], "fixed");
        assert_eq!(details["conflict"]["id"], "fixed-switch-windows");
        assert_eq!(details["conflict"]["label"], "Switch windows");
    }

    #[test]
    fn a_clash_with_a_published_action_names_what_it_runs() {
        let mut config = ShortcutConfig::default();
        config
            .set_action(
                KeyChord::parse("Super+Shift+Y").unwrap(),
                settings_store::hardware::ActionChordTarget {
                    provider: "workspace:room".into(),
                    resource: "drafts".into(),
                    action: "publish".into(),
                },
                "Run Publish",
            )
            .unwrap();
        let operation = rebind(&config, "search", "Super+Shift+Y");
        let error = apply(&mut config, &operation).expect_err("a published action holds it");
        let details = error.details.expect("the holder is named");
        assert_eq!(details["conflict"]["kind"], "action");
        assert_eq!(details["conflict"]["label"], "Run Publish");
        assert_eq!(details["conflict"]["target"]["action"], "publish");
    }

    #[test]
    fn a_rebind_and_a_reset_report_the_changed_row() {
        let mut config = ShortcutConfig::default();
        let operation = rebind(&config, "quick-settings", "Super+J");
        let output = apply(&mut config, &operation).expect("Super+J is free");
        let row = output.value.expect("the row");
        assert_eq!(row["chords"], json!(["Super+J"]));
        assert_eq!(row["defaults"], json!(["Super+Q"]));
        let reset = decode(
            &config,
            &address(&entry_resource("quick-settings"), RESET_ACTION),
            None,
        )
        .expect("reset takes nothing");
        let row = apply(&mut config, &reset).unwrap().value.unwrap();
        assert_eq!(row["chords"], json!(["Super+Q"]));
        assert_eq!(config, ShortcutConfig::default());
    }

    #[test]
    fn only_rebindable_shortcuts_offer_rebind() {
        let mut config = ShortcutConfig::default();
        assert!(
            descriptor_at(&config, &address(&entry_resource("search"), REBIND_ACTION)).is_some()
        );
        assert!(descriptor_at(&config, &address(&entry_resource("lock"), REBIND_ACTION)).is_some());
        assert!(
            descriptor_at(
                &config,
                &address(&entry_resource("fixed-close-tab"), RESET_ACTION)
            )
            .is_none()
        );
        let listed = apply(&mut config, &Operation::List).unwrap().value.unwrap();
        let rows = listed["shortcuts"].as_array().unwrap();
        let close = rows
            .iter()
            .find(|row| row["id"] == "fixed-close-tab")
            .unwrap();
        assert_eq!(close["rebindable"], false);
        assert_eq!(close["chords"], json!(["Alt+F4", "Super+W"]));
        assert_eq!(close["category"], "windows");
    }
}
