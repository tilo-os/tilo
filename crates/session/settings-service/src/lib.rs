//! The session's settings service.
//!
//! The person's settings sections (appearance, input, power, ...) are
//! records ([`settings_sections`]): each is published as `settings:<kind>`
//! with its field edits and verbs, and every change is checked by the
//! section's model and recorded in the settings document with what made it
//! and who. The service keeps them, so the palette, agents, scripts and the
//! desktop's search change them through those actions whether or not
//! Settings is open; Settings' window is one view of them. Each change is a
//! step of the person who made it: `undo` and `redo` are published beside a
//! section and take back their last step on it, reading the settings
//! document's own history, as they do in Settings' window.
//!
//! Beside them it publishes every setting people look for by name
//! ([`settings_store::index`]) that search can change in place, under
//! [`INDEX`]: a yes-or-no setting is a switch that says whether it is on
//! and flips; a setting holding one of a few values offers each value by
//! its name; a setting set to a value (a timer, the keyboard layout) takes
//! one. Each runs the section's own field edit or verb, as the control in
//! Settings does, and asks first exactly when that edit would.

pub mod effects;
mod i18n;

use serde_json::{Map, Value};
use settings_store::index::{self, Change, Setting};
use tilo_actions::{
    ActionAddress, ActionArgs, ActionInvoker, ActionParam, ActionSpec, ActionState,
    ConfirmationPolicy, DoorError, ParamType, ResourceSpec, RiskLevel,
};
use tilo_resource::Effect as _;
use tilo_service::{Service, Started};
use ui::framework::SettingsRecords;
use ui::storage::{InvokingAction, SettingsDocument, SettingsRevision};

use effects::SettingEffect;

/// The resource holding the settings people look for by name.
pub const INDEX: &str = "settings";
pub const INDEX_KIND: &str = "settings.index";
/// The kind of one setting under [`INDEX`].
pub const SETTING_KIND: &str = "settings.setting";
/// The id of a setting's resource, by the setting's id.
const SETTING_PREFIX: &str = "setting:";
/// Between a setting's id and the name of one of its values, in the id of
/// the value's resource: each value of a choice is an action of its own,
/// on a resource of its own.
const OPTION_SEPARATOR: char = '=';

/// The index's copy, in the desktop's language: the settings' names.
static NAMES: ui::i18n::EmbeddedCatalog = {
    let catalog = ui::i18n::EmbeddedCatalog::new("en-US", index::CATALOGUES).with_system_locale();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
};

/// The id of the resource of the setting `id`.
#[must_use]
pub fn setting_resource(id: &str) -> String {
    format!("{SETTING_PREFIX}{id}")
}

/// The id of the resource of the value named `option` of the setting `id`.
#[must_use]
pub fn option_resource(id: &str, option: &str) -> String {
    format!("{SETTING_PREFIX}{id}{OPTION_SEPARATOR}{option}")
}

/// The id of the setting whose resource (or whose value's resource) is
/// `resource`; `None` for another resource.
#[must_use]
pub fn setting_of(resource: &str) -> Option<&str> {
    let id = resource.strip_prefix(SETTING_PREFIX)?;
    Some(id.split_once(OPTION_SEPARATOR).map_or(id, |(id, _)| id))
}

/// The field edit of a section setting the field at `path` (names joined by
/// dots) to `value`.
fn set_path(path: &str, value: &Value) -> Map<String, Value> {
    let path: Vec<Value> = path
        .split('.')
        .map(|field| serde_json::json!({ "field": field }))
        .collect();
    let mut arguments = Map::new();
    arguments.insert(
        "edit".into(),
        serde_json::json!({ "path": path, "op": "set", "value": value }),
    );
    arguments
}

/// The value at `path` (names joined by dots) of a section as stored.
fn at_path<'a>(stored: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .try_fold(stored, |value, field| value.get(field))
}

/// A typed value as the field holds it: JSON when it reads as JSON (`5`,
/// `true`), else the text itself (`de`).
fn typed(value: &str) -> Value {
    serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()))
}

/// How a value reads on a row.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The settings service.
pub struct SettingsService {
    records: SettingsRecords,
    /// The person at this seat, whose changes they are.
    invoker: ActionInvoker,
    /// The settings document's version when it was last read.
    seen: Option<SettingsRevision>,
    /// What it publishes, built again only when the settings changed.
    published: Vec<ResourceSpec>,
}

impl SettingsService {
    /// The person's settings, as the session's settings document stores
    /// them, changed by `person`.
    #[must_use]
    pub fn new(person: &str) -> Self {
        let mut service = Self {
            records: SettingsRecords::of_person_named(
                settings_sections::APP_ID,
                settings_sections::APP_TITLE,
                person,
                &[settings_sections::declare],
            ),
            invoker: ActionInvoker::actor(person),
            seen: None,
            published: Vec::new(),
        };
        service.published = service.build();
        service
    }

    /// The sections and the index over them, as stored now.
    fn build(&self) -> Vec<ResourceSpec> {
        let mut resources = self.records.resources();
        let index = self.index(&resources);
        resources.push(index);
        resources
    }

    /// The section `kind` (`settings.power`) as stored now.
    #[must_use]
    pub fn stored(&self, kind: &str) -> Option<Value> {
        self.records.stored(kind)
    }

    /// The published section `kind` among `sections`.
    fn section<'a>(sections: &'a [ResourceSpec], kind: &str) -> Option<&'a ResourceSpec> {
        sections.iter().find(|section| section.kind == kind)
    }

    /// The setting `setting` as published under [`INDEX`]; `None` for one
    /// search cannot change here (its section is not kept).
    fn publish(&self, setting: &Setting, sections: &[ResourceSpec]) -> Option<ResourceSpec> {
        let name = NAMES.t(&setting.name_key());
        let page = index::PAGES
            .iter()
            .find(|page| page.slug == setting.page)
            .map(|page| NAMES.t(&page.title_key()));
        let words = setting.search_words();
        let dress = |mut spec: ActionSpec, label: String, description: Option<String>| {
            let ty = spec.type_mut();
            ty.label = label;
            ty.keywords.clone_from(&words);
            ty.description = description;
            spec
        };
        let mut actions = Vec::new();
        if let Some(switch) = setting.switch {
            let section = Self::section(sections, switch.section)?;
            let on = at_path(&self.stored(switch.section)?, switch.field)?.as_bool()?;
            let mut spec = dress(SettingEffect::Switch.bound(), name.clone(), page.clone());
            spec.type_mut().state = ActionState {
                badge: Some(i18n::t(if on { "setting-on" } else { "setting-off" })),
                toggles: true,
                shortcut: None,
            };
            confirm_as(
                &mut spec,
                section,
                &set_path(switch.field, &Value::Bool(!on)),
            );
            actions.push(spec);
        }
        let mut options = Vec::new();
        if let Some(choice) = setting.choice {
            let section = Self::section(sections, choice.section)?;
            let current = self
                .stored(choice.section)
                .and_then(|stored| at_path(&stored, choice.field).cloned());
            for option in choice.options {
                let label = NAMES.t(&setting.option_key(*option));
                let effect = SettingEffect::Choose {
                    value: option.value.to_owned(),
                };
                let mut spec = dress(effect.bound(), label.clone(), Some(name.clone()));
                if current.as_ref() == Some(&option.json()) {
                    spec.type_mut().state = ActionState::badge(i18n::t("setting-current"));
                }
                confirm_as(&mut spec, section, &set_path(choice.field, &option.json()));
                options.push(
                    ResourceSpec::new(
                        option_resource(setting.id, option.name),
                        SETTING_KIND,
                        label,
                    )
                    .with_action(spec),
                );
            }
        }
        if let Some(value) = setting.value {
            let section = Self::section(sections, value.section)?;
            let stored = self.stored(value.section)?;
            let (param, target) = match value.change {
                Change::Field(field) => (
                    field_param(section, field)?,
                    at_path(&stored, field).map(shown),
                ),
                Change::Verb(verb) => (verb_param(section, verb)?, None),
            };
            let mut spec = dress(
                SettingEffect::Set {
                    value: String::new(),
                }
                .bound_asking_params(vec![param]),
                name.clone(),
                page,
            );
            if let Some(current) = target {
                spec.type_mut().state = ActionState::badge(current);
            }
            let change = value.change;
            let section = section.clone();
            spec = spec.with_risk(move |args| {
                let Some(value) = args.text("value") else {
                    return RiskLevel::Low;
                };
                change_risk(&section, change, value)
            });
            actions.push(spec);
        }
        if actions.is_empty() && options.is_empty() {
            return None;
        }
        let mut resource = ResourceSpec::new(setting_resource(setting.id), SETTING_KIND, name)
            .with_children(options);
        resource.actions = actions;
        Some(resource)
    }

    /// The settings people look for by name that search changes in place.
    fn index(&self, sections: &[ResourceSpec]) -> ResourceSpec {
        ResourceSpec::new(INDEX, INDEX_KIND, "Settings").with_children(
            index::SETTINGS
                .iter()
                .filter_map(|setting| self.publish(setting, sections)),
        )
    }

    /// What running `effect` on `setting` runs on its section: the address
    /// and the arguments.
    fn change(
        &self,
        setting: &Setting,
        effect: SettingEffect,
    ) -> Result<(ActionAddress, Map<String, Value>), DoorError> {
        let refused = |reason: &str| DoorError::InvalidArguments(reason.to_owned());
        match effect {
            SettingEffect::Switch => {
                let switch = setting.switch.ok_or_else(|| refused("not a switch"))?;
                let on = self
                    .stored(switch.section)
                    .and_then(|stored| at_path(&stored, switch.field)?.as_bool())
                    .ok_or_else(|| refused("the setting is not kept here"))?;
                Ok((
                    section_action(switch.section, "field-edit"),
                    set_path(switch.field, &Value::Bool(!on)),
                ))
            }
            SettingEffect::Choose { value } => {
                let choice = setting.choice.ok_or_else(|| refused("not a choice"))?;
                let option = choice
                    .options
                    .iter()
                    .find(|option| option.value == value)
                    .ok_or_else(|| refused("not one of the setting's values"))?;
                Ok((
                    section_action(choice.section, "field-edit"),
                    set_path(choice.field, &option.json()),
                ))
            }
            SettingEffect::Set { value } => {
                let set = setting.value.ok_or_else(|| refused("not set to a value"))?;
                Ok(match set.change {
                    Change::Field(field) => (
                        section_action(set.section, "field-edit"),
                        set_path(field, &typed(&value)),
                    ),
                    Change::Verb(verb) => {
                        let sections = self.records.resources();
                        let param = Self::section(&sections, set.section)
                            .and_then(|section| verb_name(section, verb))
                            .ok_or_else(|| refused("the setting is not kept here"))?;
                        let mut arguments = Map::new();
                        arguments.insert(param, Value::String(value));
                        (section_action(set.section, verb), arguments)
                    }
                })
            }
        }
    }
}

/// The address of the action `action` of the section `kind`.
fn section_action(kind: &str, action: &str) -> ActionAddress {
    ActionAddress::new(format!("settings:{kind}"), action)
}

/// The published action `action` of `section`.
fn section_spec<'a>(section: &'a ResourceSpec, action: &str) -> Option<&'a ActionSpec> {
    section.actions.iter().find(|spec| spec.id == action)
}

/// Ask first for `spec` exactly when the section's field edit with
/// `arguments` would.
fn confirm_as(spec: &mut ActionSpec, section: &ResourceSpec, arguments: &Map<String, Value>) {
    let asks = section_spec(section, "field-edit").map_or(ConfirmationPolicy::None, |edit| {
        edit.confirmation_for_json(arguments)
    });
    if asks != ConfirmationPolicy::None {
        spec.type_mut().contract.confirmation = asks;
    }
}

/// The risk of setting a value through `change` of `section` to `value`:
/// the section's field edit's or verb's for the arguments it gets.
fn change_risk(section: &ResourceSpec, change: Change, value: &str) -> RiskLevel {
    let (action, arguments) = match change {
        Change::Field(field) => ("field-edit", set_path(field, &typed(value))),
        Change::Verb(verb) => {
            let Some(param) = verb_name(section, verb) else {
                return RiskLevel::Low;
            };
            let mut arguments = Map::new();
            arguments.insert(param, Value::String(value.to_owned()));
            (verb, arguments)
        }
    };
    let Some(spec) = section_spec(section, action) else {
        return RiskLevel::Low;
    };
    ActionArgs::bind_json(spec.params(), &arguments)
        .map_or(RiskLevel::Low, |args| spec.risk_for(&args))
}

/// The name of the one parameter of the verb `verb` of `section`.
fn verb_name(section: &ResourceSpec, verb: &str) -> Option<String> {
    section_spec(section, verb)?
        .params()
        .first()
        .map(|param| param.name.to_string())
}

/// What a setting set through the verb `verb` asks for: the verb's one
/// parameter, as the value.
fn verb_param(section: &ResourceSpec, verb: &str) -> Option<ActionParam> {
    let param = section_spec(section, verb)?.params().first()?;
    Some(
        ActionParam::required_owned("value", ParamType::Text, param.description.to_string())
            .with_choices(param.choices.iter().map(ToString::to_string)),
    )
}

/// What a setting set through the field edit of `field` asks for: a value
/// the field declares it may hold, its choices when it names them.
fn field_param(section: &ResourceSpec, field: &str) -> Option<ActionParam> {
    section_spec(section, "field-edit")?;
    let shape = settings_sections::kept()
        .into_iter()
        .find(|(kind, _)| *kind == section.kind)
        .map(|(_, shape)| shape)?;
    let tilo_resource::Shape::Record(fields) = shape else {
        return None;
    };
    let declared = fields.iter().find(|declared| declared.key == field)?;
    let schema = (declared.schema)();
    let choices: Vec<String> = schema
        .get("enum")
        .and_then(Value::as_array)
        .map(|values| values.iter().map(shown).collect())
        .unwrap_or_default();
    let range = match (
        schema.get("minimum").and_then(Value::as_f64),
        schema.get("maximum").and_then(Value::as_f64),
    ) {
        (Some(low), Some(high)) => format!(" ({low} to {high})"),
        _ => String::new(),
    };
    Some(
        ActionParam::required_owned(
            "value",
            ParamType::Text,
            format!("{}{range}", declared.title),
        )
        .with_choices(choices),
    )
}

impl Service for SettingsService {
    fn resources(&self) -> Vec<ResourceSpec> {
        self.published.clone()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        let kind = resource.strip_prefix("settings:")?;
        self.stored(kind)
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        // The service host asked the person first where the published
        // action asks it: what reaches here is confirmed.
        if let Some(id) = setting_of(resource) {
            let setting = index::SETTINGS
                .iter()
                .find(|setting| setting.id == id)
                .ok_or_else(|| DoorError::Unknown(ActionAddress::new(resource, action)))?;
            let effect = tilo_service::decode::<SettingEffect>(
                &self.published,
                resource,
                action,
                arguments,
            )?;
            let (address, arguments) = self.change(setting, effect)?;
            let invoked_by = InvokingAction::action(format!("{resource}/{action}"));
            let done = self
                .records
                .invoke(&address, &arguments, true, &self.invoker, Some(&invoked_by))
                .map(Started::Done);
            self.published = self.build();
            return done;
        }
        let done = self
            .records
            .invoke(
                &ActionAddress::new(resource, action),
                arguments,
                true,
                &self.invoker,
                None,
            )
            .map(Started::Done);
        self.published = self.build();
        done
    }

    fn poll(&mut self) -> bool {
        // Settings' window, another device or a script may change the
        // settings document: what the service publishes follows it.
        let now = SettingsDocument::Session.revision();
        let changed = self.seen.replace(now) != Some(now);
        if changed {
            self.published = self.build();
        }
        changed
    }
}

#[cfg(test)]
mod tests;
