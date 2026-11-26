//! The session's settings service.
//!
//! The person's settings sections (appearance, input, power, ...) are
//! records ([the sections](crate)): each is published where the settings
//! keep it (`settings/appearance` for the person's,
//! `devices/<device>/settings/power` for this device's) with its verbs and a
//! setter of each of its fields, `set-<field>`, and
//! every change is checked by the section's model and recorded in its
//! resource with what made it and who. The service keeps them, so
//! the palette, agents, scripts and the desktop's search change them through
//! those actions whether or not Settings is open; Settings' window is one
//! view of them. Each change is a step of the person who made it: `undo` and
//! `redo` are published beside a section and take back their last step on
//! it, as they do in Settings' window. An app's preferences are a resource
//! there too (`settings/apps/<app>`, `devices/<device>/settings/apps/<app>`),
//! each of its preferences set by `set-<preference>`.
//!
//! A setting people look for by name ([`settings_store::index`]) is the
//! section's own verb that changes it, the setter of its field or a verb the
//! section declares, named as people name the setting, with every word they
//! use for it, the page it is on and what it holds now. Search finds it
//! there: a grant on that verb is a grant on the setting, and nothing else
//! changes it.

pub mod agent_models;
mod i18n;
pub mod languages;
pub mod shortcuts_table;
pub mod tablet_choices;
pub mod theme_packages;

use serde_json::{Map, Value};
use settings_store::index::{self, Change, Setting};
use settings_store::placement;
use tilo_actions::{
    ActionAddress, ActionInvoker, ActionSpec, ActionState, ApiError, CHOICE_LABELS_ANNOTATION,
    ResourceSpec,
};
use tilo_service::{Service, Started};
use ui::framework::SettingsResources;
use ui::storage::{SettingsDocument, SettingsRevision};

/// Whether the resource `id` is one of the settings this service publishes: a section of the
/// person's settings or the device's, an app's preferences, the agent model choice among them.
#[must_use]
pub fn keeps(id: &str) -> bool {
    index::is_settings(id)
}

/// The index's copy, in the desktop's language: the settings' names. The same
/// index serves every window, so it rides along in the window's language
/// instead of deciding it.
static NAMES: tilo_ui_base::i18n::EmbeddedCatalog = {
    let catalog = tilo_ui_base::i18n::EmbeddedCatalog::new("en-US", index::CATALOGUES)
        .with_system_locale()
        .as_shared();
    #[cfg(test)]
    let catalog = catalog.with_pinned_locale("en-US");
    catalog
};

/// How a value reads on a row.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The settings service.
pub struct SettingsService {
    records: SettingsResources,
    /// The person at this device, whose changes they are: the session hands
    /// the service the person at this device as it starts it ([`Service::own_person`]).
    invoker: Option<ActionInvoker>,
    /// The settings document's version when it was last read.
    seen: Option<SettingsRevision>,
    /// What it publishes, built again only when the settings changed.
    published: Vec<ResourceSpec>,
    /// The agent model choice the canvas reads and changes, over both
    /// provider sections.
    agent_models: agent_models::Served,
}

impl Default for SettingsService {
    fn default() -> Self {
        Self::new()
    }
}

impl SettingsService {
    /// The person's settings, as the session's settings document stores
    /// them. Who the person is is the session's to say, as it starts the
    /// service ([`SettingsService::own_person`]), so what is changed here is
    /// authored by the same person as everything else they do.
    #[must_use]
    pub fn new() -> Self {
        let mut service = Self {
            records: SettingsResources::new(crate::APP_ID, crate::declare),
            invoker: None,
            seen: None,
            published: Vec::new(),
            agent_models: agent_models::Served::new(),
        };
        service.published = service.build();
        service
    }

    /// Sit this service at `person`, the id their workspaces' governance
    /// names them by, with `earlier` the ids they went by before this
    /// device had an identity: what they change is theirs, and what was
    /// changed under one of those ids is theirs to take back.
    pub fn own_person(&mut self, person: &str, earlier: &[String]) {
        let person = tilo_actions::MemberId::person(person);
        self.records = SettingsResources::of_person_named(
            crate::APP_ID,
            crate::APP_TITLE,
            &person,
            earlier,
            &[crate::declare],
        );
        self.invoker = Some(ActionInvoker::member(person));
        self.published = self.build();
    }

    /// The sections, their settings named as people look for them, as
    /// stored now.
    fn build(&self) -> Vec<ResourceSpec> {
        let device = ui::framework::settings_device();
        let mut resources = self.records.resources();
        for section in &mut resources {
            let stored = self.records.stored_at(section.id.as_str());
            for setting in index::SETTINGS {
                let Some(at) = setting
                    .at
                    .filter(|at| at.resource(&device) == section.id.as_str())
                else {
                    continue;
                };
                let action = at.action();
                let Some(spec) = section.actions.iter_mut().find(|spec| spec.id == action) else {
                    continue;
                };
                let now = match at.change {
                    Change::Field(field) => stored.as_ref().and_then(|stored| stored.get(field)),
                    Change::Verb(_) => None,
                };
                named(spec, setting, now);
            }
        }
        for (id, app, mut held) in app_preferences(&device) {
            let Some(at) = resources.iter().position(|kept| kept.id.as_str() == id) else {
                resources.push(ui::framework::app_preferences(
                    &id,
                    placement::APP_KIND,
                    &app,
                    &held,
                ));
                continue;
            };
            // A section kept among an app's preferences (the keyboard's layouts) stands at the
            // app's address: the app's other preferences are set there too, its own fields
            // only as the section sets them.
            if let Some(Value::Object(fields)) = self.records.stored_at(&id) {
                held.retain(|preference, _| !fields.contains_key(preference));
            }
            let Some(section) = resources.get_mut(at) else {
                continue;
            };
            let preferences =
                ui::framework::app_preferences(&id, placement::APP_KIND, &app, &held).actions;
            for spec in preferences {
                if !section.actions.iter().any(|kept| kept.id == spec.id) {
                    section.actions.push(spec);
                }
            }
        }
        resources.push(agent_models::Served::resource());
        // The languages this desktop speaks, as Region's page offers them.
        resources.push(
            tilo_resource::typed_resource::<languages::LanguagesReading>(
                languages::RESOURCE,
                i18n::t("settings-languages"),
            ),
        );
        resources.push(tilo_resource::typed_resource::<
            shortcuts_table::ShortcutsTableReading,
        >(
            shortcuts_table::RESOURCE,
            i18n::t("settings-shortcuts-table"),
        ));
        resources.push(tilo_resource::typed_resource::<
            tablet_choices::TabletChoicesReading,
        >(
            tablet_choices::RESOURCE,
            i18n::t("settings-tablet-choices"),
        ));
        // The theme packages installed here, as Appearance's page offers them.
        resources.push(tilo_resource::typed_resource::<
            theme_packages::ThemePackagesReading,
        >(
            theme_packages::RESOURCE,
            i18n::t("settings-theme-packages"),
        ));
        resources
    }
}

/// `spec`, the verb that changes `setting`, named as people look for the
/// setting: its name, every word for it, the page it is on, and what it holds
/// `now` with each of its values read as people name it.
fn named(spec: &mut ActionSpec, setting: &Setting, now: Option<&Value>) {
    let options = setting.at.map_or(&[][..], |at| at.options);
    let labels: Vec<(String, String)> = if !options.is_empty() {
        options
            .iter()
            .map(|option| (shown(&option.json()), NAMES.t(&setting.option_key(*option))))
            .collect()
    } else if now.is_some_and(Value::is_boolean) {
        vec![
            ("true".to_owned(), i18n::t("setting-on")),
            ("false".to_owned(), i18n::t("setting-off")),
        ]
    } else {
        Vec::new()
    };
    let ty = spec.type_mut();
    ty.label = NAMES.t(&setting.name_key());
    ty.keywords = setting.search_words();
    ty.description = index::page(setting.page).map(|page| NAMES.t(&page.title_key()));
    if let Some(now) = now {
        let now = shown(now);
        let badge = labels
            .iter()
            .find(|(value, _)| *value == now)
            .map_or(now, |(_, label)| label.clone());
        ty.state = ActionState::badge(badge);
    }
    if !labels.is_empty() {
        // Each value reads as people name it, without narrowing what the setter takes.
        let labels = labels
            .into_iter()
            .map(|(value, label)| (value, Value::String(label)));
        spec.narrow_param(ui::framework::SETTER_VALUE, |param| {
            let mut schema = param
                .schema
                .as_deref()
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            schema.insert(
                CHOICE_LABELS_ANNOTATION.to_owned(),
                Value::Object(labels.collect()),
            );
            param.schema = Some(std::sync::Arc::new(Value::Object(schema)));
        });
    }
}

/// Each app's preferences the settings hold, on `device`: its resource id, the app, and what it
/// holds, the person's (`settings/apps/<app>`) and the device's
/// (`devices/<device>/settings/apps/<app>`).
fn app_preferences(
    device: &str,
) -> impl Iterator<Item = (String, String, Map<String, Value>)> + '_ {
    [
        placement::APP_PERSONAL_SECTION,
        placement::APP_DEVICE_LOCAL_SECTION,
    ]
    .into_iter()
    .flat_map(move |section| {
        SettingsDocument::Session
            .section(section)
            .into_iter()
            .filter_map(move |(app, held)| {
                let Value::Object(held) = held else {
                    return None;
                };
                Some((
                    placement::address(section, Some(&app), device).id,
                    app,
                    held,
                ))
            })
    })
}

/// The app preference the verb `action` of the app preferences resource `resource` sets: the
/// section it is kept in, the app, and the preference. `None` for another resource.
fn preference_at(resource: &str, action: &str) -> Option<(&'static str, String, String)> {
    let (root, app) = resource.rsplit_once("/apps/")?;
    let section = if root == "settings" {
        placement::APP_PERSONAL_SECTION
    } else if index::is_settings(resource) && root.ends_with("/settings") {
        placement::APP_DEVICE_LOCAL_SECTION
    } else {
        return None;
    };
    let named = action.strip_prefix("set-")?;
    // A preference held now is named as it is held; a new one as its verb names it.
    let held = SettingsDocument::Session.section(section);
    let preference = held
        .get(app)
        .and_then(Value::as_object)
        .and_then(|held| {
            held.keys()
                .find(|preference| ui::setting_verb(preference) == action)
                .cloned()
        })
        .unwrap_or_else(|| named.replace('-', "_"));
    Some((section, app.to_owned(), preference))
}

/// What stands at `address` though `published` holds no such action: the setter of a section of
/// another of the person's devices, as this device's same section publishes it (so a later lock
/// there asks first as it does here), or the setter of an app's preference that none of its
/// preferences held before. A resource at that address with that one action, which the broker
/// admits an invocation against as it does a published one.
#[must_use]
pub fn unpublished(published: &[ResourceSpec], address: &ActionAddress) -> Option<ResourceSpec> {
    let resource = address.resource.as_str();
    let action = address.action.as_str();
    let device = index::device_of(resource);
    let here = ui::framework::settings_device();
    if !device.is_empty() && device != here {
        let on_this = ActionAddress::new(resource.replacen(device, &here, 1), action);
        let section = published
            .iter()
            .find(|kept| kept.id.as_str() == on_this.resource.as_str())
            .filter(|kept| kept.kind != placement::APP_KIND)?;
        let spec = tilo_actions::api::resolve(published, &on_this)
            .filter(|spec| spec.id.starts_with("set-"))?
            .clone();
        let mut standing = section.clone();
        standing.id = tilo_actions::ResourceId::new(resource);
        standing.children.clear();
        standing.views.clear();
        standing.actions = vec![spec];
        return Some(standing);
    }
    // A section's verbs are all published; an app's preferences gain one as the app first holds
    // it, also where a section kept among them stands.
    if !resource.contains("/apps/")
        && published
            .iter()
            .any(|kept| kept.id.as_str() == resource && kept.kind != placement::APP_KIND)
    {
        return None;
    }
    let (_, app, preference) = preference_at(resource, action)?;
    Some(ui::framework::app_preferences(
        resource,
        placement::APP_KIND,
        &app,
        &Map::from_iter([(preference, Value::Null)]),
    ))
}

impl Service for SettingsService {
    fn own_person(&mut self, person: &str, earlier: &[String]) {
        Self::own_person(self, person, earlier);
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        self.published.clone()
    }

    /// A published action, or one [`unpublished`] stands for.
    fn resolve(&self, address: &ActionAddress) -> Option<ActionSpec> {
        if let Some(spec) = tilo_actions::api::resolve(&self.published, address) {
            return Some(spec.clone());
        }
        let standing = unpublished(&self.published, address)?;
        // Of another device's section, only a field's setter is run there.
        let resource = address.resource.as_str();
        let device = index::device_of(resource);
        let here = ui::framework::settings_device();
        if !device.is_empty() && device != here {
            let on_this = resource.replacen(device, &here, 1);
            self.records.setter_at(&on_this, address.action.as_str())?;
        }
        standing.actions.into_iter().next()
    }

    fn read(&self, resource: &str) -> Option<Value> {
        if resource == agent_models::RESOURCE {
            return self.agent_models.read(resource);
        }
        if resource == languages::RESOURCE {
            return serde_json::to_value(languages::Languages::spoken()).ok();
        }
        if resource == theme_packages::RESOURCE {
            return serde_json::to_value(theme_packages::ThemePackages::installed()).ok();
        }
        if resource == shortcuts_table::RESOURCE {
            // The table read from the person's shortcuts section as it is stored now.
            let stored = self
                .records
                .stored_at(&placement::address("shortcuts", None, "").id)
                .unwrap_or(Value::Null);
            return serde_json::to_value(shortcuts_table::ShortcutsTable::of(&stored)).ok();
        }
        if resource == tablet_choices::RESOURCE {
            // Each tablet's choices read from this device's input section as it is stored now.
            let device = ui::framework::settings_device();
            let stored = self
                .records
                .stored_at(&placement::address("input_device", None, &device).id)
                .unwrap_or(Value::Null);
            return serde_json::to_value(tablet_choices::TabletChoices::of(&stored)).ok();
        }
        let published = self
            .published
            .iter()
            .find(|kept| kept.id.as_str() == resource)?;
        let stored = self.records.stored_at(resource);
        // An app's preferences, and a section kept among them as it reads them.
        if let Some((section, app, _)) = preference_at(resource, "set-") {
            let mut held = SettingsDocument::Session
                .section(section)
                .get(&app)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(Value::Object(fields)) = stored {
                held.extend(fields);
            }
            return (published.kind == placement::APP_KIND || !held.is_empty())
                .then_some(Value::Object(held));
        }
        stored
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        // What is changed here is the person's, under their own id: a host
        // that started the service without saying who sits here has none to
        // put on the change.
        let Some(invoker) = self.invoker.as_ref() else {
            return Err(ApiError::Failed(
                "the session has not said who sits here".to_owned(),
            ));
        };
        // The agent model choice is one step over both provider sections:
        // what it writes, and what its undo takes back, is theirs.
        if resource == agent_models::RESOURCE {
            let done =
                self.agent_models
                    .invoke(&self.published, resource, action, arguments, invoker);
            self.published = self.build();
            return done;
        }
        // An app's preference, which no section kept among them sets: written into the app's
        // preferences, as the person's.
        let typed = self.records.resources().iter().any(|kept| {
            kept.id.as_str() == resource && kept.actions.iter().any(|spec| spec.id == action)
        });
        let fields = match self.records.stored_at(resource) {
            Some(Value::Object(fields)) => fields,
            _ => Map::new(),
        };
        if let Some((section, app, preference)) = preference_at(resource, action)
            .filter(|(.., preference)| !typed && !fields.contains_key(preference))
        {
            let value = arguments
                .get(ui::framework::SETTER_VALUE)
                .cloned()
                .ok_or_else(|| {
                    ApiError::InvalidArguments(format!(
                        "{resource}#{action} takes the preference's value"
                    ))
                })?;
            let actor = serde_json::to_string(&invoker.acting.by).unwrap_or_default();
            let origin =
                settings_store::ChangeOrigin::action(format!("{resource}#{action}")).by(&actor);
            // An app's preferences are written whole: the others it holds stay.
            let mut preferences = SettingsDocument::Session
                .section(section)
                .get(&app)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            preferences.insert(preference, value);
            let mut values = Map::new();
            values.insert(app, Value::Object(preferences));
            let done = SettingsDocument::Session
                .update_section_by(section, values, Some(&origin))
                .map(|()| Started::Done(tilo_actions::ActionOutput::default()))
                .map_err(|error| ApiError::Failed(error.to_string()));
            self.published = self.build();
            return done;
        }
        // A section of another of the person's devices: written to that device's own resource,
        // as the same setter of this device's section writes this one's.
        let device = index::device_of(resource);
        let here = ui::framework::settings_device();
        if !device.is_empty() && device != here {
            let on_this = resource.replacen(device, &here, 1);
            let (section, field) = self
                .records
                .setter_at(&on_this, action)
                .ok_or_else(|| ApiError::Unknown(ActionAddress::new(resource, action)))?;
            let value = arguments
                .get(ui::framework::SETTER_VALUE)
                .cloned()
                .ok_or_else(|| {
                    ApiError::InvalidArguments(format!(
                        "{resource}#{action} takes the whole value on another device"
                    ))
                })?;
            let actor = serde_json::to_string(&invoker.acting.by).unwrap_or_default();
            let origin =
                settings_store::ChangeOrigin::action(format!("{resource}#{action}")).by(&actor);
            return crate::workspace::set_on_device(device, section, field, &value, &origin)
                .map(|()| Started::Done(tilo_actions::ActionOutput::default()))
                .map_err(|error| ApiError::Failed(error.to_string()));
        }
        // The service host asked the person first where the published
        // action asks it: what reaches here is confirmed.
        let allowed = approved();
        let done = self
            .records
            .invoke(
                &ActionAddress::new(resource, action),
                arguments,
                Some(&allowed),
                invoker,
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

/// The approve the host already took for this call: what reaches the service has been asked
/// of the person and allowed, so the section's own check runs with their answer.
fn approved() -> tilo_actions::remote::asked::Approval {
    tilo_actions::remote::asked::Approval::new(
        tilo_actions::remote::asked::RecordRef {
            workspace: String::new(),
            record: "invocations/confirmed".to_owned(),
            device: None,
        },
        None,
    )
}
