//! A settings section held as a record ([`ResourceEnv::settings`](super::ResourceEnv::settings)):
//! published where the settings keep it ([`section_address`]: `settings/appearance` for the
//! person's, `devices/<device>/settings/power` for the device's) with its verbs and a setter of each field
//! (`set-<field>`, [`with_setters`]), and no generic edit; each change
//! checked by the section's model and stored in the settings document with
//! what made it and who.
//!
//! The settings are the section's only copy: the record reads the section as
//! the settings hold it, an action runs on that value and what it changed is
//! written back as one change, and undo and redo of an invoker's steps take
//! back their changes in the section's history.
//! Every section is kept by the one [`SectionApi`]; what it needs of the
//! section's type is a [`SectionKind`], a handful of functions built once
//! per type; nothing else is compiled per section.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use settings_model::SettingsSchema;
use tilo_actions::{Action, ActionError, ActionInvoker, EncodedAction};
use tilo_resource::{Doc, Fields, ModelError, Reader, Resource};
use ui_core::storage::{ChangeOrigin, Settings, SettingsDocument, SettingsRevision};

use super::live::Live;
use super::resources::{Held, ResourceApi};

/// One thing a section's record asks of the section's type.
#[derive(Clone, Copy)]
enum SectionOp<'a> {
    /// Run the action on the section as stored, and store what it made as
    /// the change the origin made.
    Apply(&'a dyn Action, &'a ChangeOrigin),
    /// The section as stored, published under an id and a label.
    Publish(&'a str, &'a str),
    Decode(&'a EncodedAction),
    /// The section as stored now.
    Stored,
}

/// What a [`SectionOp`] answers.
enum SectionOut {
    Done,
    Published(Box<tilo_actions::ResourceSpec>),
    Decoded(Box<dyn Action>),
    Value(serde_json::Value),
}

/// What a section asks of its type: the type's state in a document, as its
/// JSON, and its action. Each is the type's own function, reached through
/// this one interface so that [`section_ops`] is compiled once rather than
/// once per section type.
trait SectionType {
    /// The type's genesis in a fresh document.
    fn initialize(&self, doc: &mut Doc) -> Result<(), ModelError>;
    /// The section as stored, written at `doc`'s root.
    fn write(&self, doc: &mut Doc) -> Result<(), ModelError>;
    /// The section in `doc`'s root, as the type's state.
    fn read(&self, doc: &Doc) -> Result<Box<dyn Any>, ModelError>;
    /// The section as stored, as its model's JSON.
    fn stored_json(&self) -> serde_json::Value;
    /// The section as stored, published as `label` under `id`.
    fn publish(&self, id: &str, label: &str) -> tilo_actions::ResourceSpec;
    /// `state`, a [`Self::read`] state, stored as `origin`'s change.
    fn store(&self, state: &dyn Any, origin: &ChangeOrigin) -> Result<(), String>;
    /// The type's action from its encoded payload.
    fn decode(&self, payload: &[u8]) -> Result<Box<dyn Action>, String>;
}

impl<T> SectionType for Settings<T>
where
    T: Resource<State = T> + SettingsSchema + Fields,
{
    fn initialize(&self, doc: &mut Doc) -> Result<(), ModelError> {
        T::initialize(doc)
    }

    fn write(&self, doc: &mut Doc) -> Result<(), ModelError> {
        stored_section(self).write_fields(doc, &tilo_resource::__private::ROOT)
    }

    fn read(&self, doc: &Doc) -> Result<Box<dyn Any>, ModelError> {
        let state: T = T::read_fields(doc, &tilo_resource::__private::ROOT)?;
        Ok(Box::new(state))
    }

    fn stored_json(&self) -> serde_json::Value {
        state_json(&stored_section(self))
    }

    fn publish(&self, id: &str, label: &str) -> tilo_actions::ResourceSpec {
        tilo_resource::publish::<T>(id, label, &stored_section(self))
    }

    fn store(&self, state: &dyn Any, origin: &ChangeOrigin) -> Result<(), String> {
        let state = state
            .downcast_ref::<T>()
            .ok_or_else(|| format!("{} holds another type's state", T::KIND))?;
        self.set_by(state, Some(origin))
            .map_err(|error| error.to_string())
    }

    fn decode(&self, payload: &[u8]) -> Result<Box<dyn Action>, String> {
        let action: T::Action =
            tilo_actions::decode_json(payload).map_err(|error| error.to_string())?;
        super::resources::not_by_app::<T>(&action)?;
        Ok(Box::new(action))
    }
}

/// The section as stored, as its type's state; one copy per type, for every
/// use [`SectionType`] makes of it.
#[inline(never)]
fn stored_section<T: SettingsSchema>(settings: &Settings<T>) -> T {
    settings.get()
}

/// `state` as its model's JSON; one copy per type.
#[inline(never)]
fn state_json<T: tilo_resource::Model>(state: &T) -> serde_json::Value {
    state.to_json()
}

/// What a section does with its type ([`SectionType`]), over the section's
/// settings.
pub(crate) struct SectionKind {
    pub(crate) kind: &'static str,
    pub(crate) label: &'static str,
    action_type: &'static str,
    /// The section of the settings document the value is kept in.
    section: &'static str,
    /// The app whose settings the section is, and the document they live in.
    app_id: String,
    document: SettingsDocument,
    /// The section's fields, each set by its own verb ([`with_setters`]).
    shape: Option<tilo_resource::Shape>,
    of_type: Box<dyn SectionType>,
}

impl SectionKind {
    pub(crate) fn of<T>(settings: &Settings<T>) -> Rc<Self>
    where
        T: Resource<State = T> + SettingsSchema + Fields,
    {
        Rc::new(Self {
            kind: T::KIND,
            label: T::LABEL,
            action_type: T::ACTION_TYPE,
            section: T::SCOPE.section(),
            app_id: settings.app_id().to_owned(),
            document: settings.document().clone(),
            shape: T::shape(),
            of_type: Box::new(settings.clone()),
        })
    }

    /// Where the section is published: where the settings keep it, on the device this process
    /// reads them on.
    pub(crate) fn address(&self) -> String {
        let app =
            ui_core::settings_placement::per_app(self.section).then_some(self.app_id.as_str());
        section_address(self.section, app)
    }

    /// The section's settings as `T`, the type this kind was made of.
    fn settings<T: SettingsSchema>(&self) -> Settings<T> {
        Settings::for_app(&self.app_id).in_document(self.document.clone())
    }

    fn revision(&self) -> SettingsRevision {
        self.document.revision()
    }

    /// Run `action`, the action `id` by `by`, on the section, and store
    /// what it made.
    fn apply(
        &self,
        action: &dyn Action,
        id: &str,
        by: &tilo_actions::Actor,
    ) -> Result<(), ActionError> {
        let origin = ChangeOrigin::action(format!("{}/{id}", self.kind)).by(actor(by));
        section_ops(self, SectionOp::Apply(action, &origin)).map(drop)
    }

    fn publish(&self, id: &str) -> Result<tilo_actions::ResourceSpec, ActionError> {
        match section_ops(self, SectionOp::Publish(id, self.label))? {
            SectionOut::Published(resource) => Ok(*resource),
            _ => Err(ActionError::Refused("the section published nothing".into())),
        }
    }

    fn decode(&self, action: &EncodedAction) -> Result<Box<dyn Action>, ActionError> {
        match section_ops(self, SectionOp::Decode(action))? {
            SectionOut::Decoded(action) => Ok(action),
            _ => Err(ActionError::Refused("the section decoded nothing".into())),
        }
    }

    /// The section as stored now, as its model writes it.
    fn stored(&self) -> Result<serde_json::Value, ActionError> {
        match section_ops(self, SectionOp::Stored)? {
            SectionOut::Value(value) => Ok(value),
            _ => Err(ActionError::Refused("the section read nothing".into())),
        }
    }

    /// Take back (`undo`) or bring back the last step `by` took on the
    /// section, from the section's history: their changes, or ones under an
    /// `alias` of theirs (an id they went by before this device had an
    /// identity). Whether there was one.
    fn step(
        &self,
        undo: bool,
        by: &tilo_actions::Actor,
        aliases: &[String],
    ) -> Result<bool, String> {
        let invokers: Vec<String> = std::iter::once(actor(by))
            .chain(
                aliases
                    .iter()
                    .filter_map(|alias| {
                        tilo_failure::ignored!(
                            "an earlier id that is not a person's names nobody whose step this is",
                            tilo_actions::policy::PersonId::parse(alias).ok()
                        )
                    })
                    .map(|alias| {
                        actor(&tilo_actions::Actor::Member(
                            tilo_actions::MemberId::person(alias),
                        ))
                    }),
            )
            .collect();
        let app =
            ui_core::settings_placement::per_app(self.section).then_some(self.app_id.as_str());
        self.document
            .step(self.section, app, undo, &invokers)
            .map_err(|error| error.to_string())
    }
}

/// How the settings document names who made a change: the actor as JSON.
fn actor(by: &tilo_actions::Actor) -> String {
    serde_json::to_string(by).unwrap_or_default()
}

/// Everything a section asks of its type, as [`SectionType`] answers it: one
/// function for every section type.
fn section_ops(kind: &SectionKind, op: SectionOp<'_>) -> Result<SectionOut, ActionError> {
    let refused = |error: &dyn std::fmt::Display| ActionError::Refused(error.to_string());
    Ok(match op {
        SectionOp::Apply(action, origin) => {
            // A resource's actions apply to a record at a document's root:
            // the section's value is laid out as one for the action to run
            // on, and what it made is stored as the section's change.
            let mut record = Doc::new();
            kind.of_type
                .initialize(&mut record)
                .map_err(|error| refused(&error))?;
            kind.of_type
                .write(&mut record)
                .map_err(|error| refused(&error))?;
            record.commit();
            action.apply(&mut record)?;
            let value = kind
                .of_type
                .read(&record)
                .map_err(|error| refused(&error))?;
            kind.of_type
                .store(&*value, origin)
                .map_err(ActionError::Refused)?;
            SectionOut::Done
        }
        SectionOp::Publish(id, label) => {
            SectionOut::Published(Box::new(kind.of_type.publish(id, label)))
        }
        SectionOp::Decode(action) => SectionOut::Decoded(
            kind.of_type
                .decode(&action.payload)
                .map_err(ActionError::Refused)?,
        ),
        SectionOp::Stored => SectionOut::Value(kind.of_type.stored_json()),
    })
}

/// A settings section held as a record, whatever its type.
pub(crate) struct SectionApi {
    id: String,
    kind: Rc<SectionKind>,
    /// The person whose steps the section's undo and redo take back, and
    /// the ids they went by before this device had an identity: changes
    /// made under any of them are theirs.
    me: Option<tilo_resource::MemberId>,
    earlier: Vec<String>,
    /// The settings document's version the API last read.
    seen: Cell<Option<SettingsRevision>>,
    /// Increases whenever what the section holds may have changed.
    revision: Cell<u64>,
    /// The last publication, at the API's and the locale's revision it was
    /// built at: published again only when either moved.
    published: RefCell<Option<((u64, u64), tilo_actions::ResourceSpec)>>,
}

impl SectionApi {
    pub(crate) fn new(
        kind: Rc<SectionKind>,
        me: Option<tilo_resource::MemberId>,
        earlier: Vec<String>,
    ) -> Self {
        Self {
            id: kind.address(),
            kind,
            me,
            earlier,
            seen: Cell::new(None),
            revision: Cell::new(0),
            published: RefCell::new(None),
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        self.kind.label
    }

    /// The kind of the place the section is kept in, which publishes every type kept there.
    pub(crate) fn place_kind(&self) -> String {
        ui_core::settings_placement::kind(self.kind.section)
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Note whether the settings document moved since the API last read
    /// it (another window, the Settings app, another device changed it).
    fn notice(&self) {
        let now = Some(self.kind.revision());
        if self.seen.get() != now {
            self.seen.set(now);
            self.moved();
        }
    }

    /// Apply the encoded action `encoded` for `invoker`, and store what it
    /// made as what made it.
    fn apply(&self, encoded: &EncodedAction, invoker: &ActionInvoker) -> Result<(), ActionError> {
        let action = self.kind.decode(encoded)?;
        let id = action_id(&encoded.payload);
        self.kind.apply(action.as_ref(), &id, &invoker.acting.by)?;
        self.notice();
        Ok(())
    }

    /// Take back (`undo`) or redo the last step `by` took on the section.
    /// Whether there was one.
    pub(crate) fn step(&self, undo: bool, by: &tilo_actions::Actor) -> bool {
        // What an id they went by made is theirs to take back; what an agent
        // acting for them made is the agent's own.
        let aliases: &[String] = if by.member_id() == self.me.as_ref() {
            &self.earlier
        } else {
            &[]
        };
        match self.kind.step(undo, by, aliases) {
            Ok(stepped) => {
                self.notice();
                stepped
            }
            Err(error) => {
                tracing::warn!(kind = self.kind.kind, %error, "settings section not stepped");
                false
            }
        }
    }

    /// The section, as a record its views edit through its type: a view part
    /// over it ([`ViewCx::record`](super::ViewCx::record)).
    pub(crate) fn held<T>(self: &Rc<Self>) -> Held<T>
    where
        T: Resource<State = T> + SettingsSchema + Fields,
    {
        let settings = self.kind.settings::<T>();
        let seen = Cell::new(settings.revision());
        let live = Live::new(Reader::with_state(settings.get(), None));
        let applying = Rc::clone(self);
        let shape = self.kind.shape;
        Held::new(
            self.id.clone(),
            self.kind.label.to_owned(),
            true,
            live,
            move |live: &Live<T>| {
                let now = settings.revision();
                if now == seen.get() {
                    return;
                }
                seen.set(now);
                let stored = settings.get();
                if tilo_resource::Model::to_json(&stored)
                    != live.with(tilo_resource::Model::to_json)
                {
                    live.replace(stored);
                }
            },
            move |action: &T::Action, invoker: &ActionInvoker| {
                applying.apply(&tilo_resource::encode::<T>(action)?, invoker)
            },
        )
        .publishing(move |published| with_setters(published, shape))
    }
}

impl ResourceApi for SectionApi {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        true
    }

    fn revision(&self) -> u64 {
        self.notice();
        self.revision.get()
    }

    fn publish(&self) -> tilo_actions::ResourceSpec {
        self.notice();
        let revision = (self.revision.get(), tilo_ui_base::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published = self.kind.publish(&self.id).unwrap_or_else(|error| {
            tracing::warn!(kind = self.kind.kind, %error, "settings section unreadable");
            tilo_actions::ResourceSpec::new(self.id.as_str(), self.kind.kind, self.kind.label)
        });
        let published = with_setters(published, self.kind.shape);
        let published = super::resources::route(published, &self.id, true);
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    fn takes(&self, action: &EncodedAction) -> bool {
        action.type_id == self.kind.action_type
    }

    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String> {
        if action.type_id != self.kind.action_type {
            return Err(format!(
                "{} takes no `{}` action",
                self.kind.label, action.type_id
            ));
        }
        self.apply(action, invoker)
            .map_err(|error| error.to_string())
    }
}

/// The settings sections an app keeps as records, held by a host without a
/// window: the session, which keeps them for the palette and agents whether
/// or not the app is open. Each is published and changed as the app's
/// window does it, so a change checks the section's model and is recorded
/// in the settings document with what made it and who; admitting an
/// invocation (the invoker's grants) is the caller's, from the published
/// action.
pub struct SettingsResources {
    apis: Vec<Rc<SectionApi>>,
    /// The person the sections are changed by, whose undo and redo of
    /// their steps on each are published beside it, with their shared
    /// records held beside the sections.
    person: Option<Rc<super::person::PersonState>>,
}

impl SettingsResources {
    /// The settings sections `declare` declares, of the app `app_id`, as
    /// they are stored now.
    #[must_use]
    pub fn new(app_id: &'static str, declare: super::Declare) -> Self {
        Self::in_document(app_id, &[declare], SettingsDocument::Session)
    }

    /// The settings sections `declares` declare, of the app `app_id`, as
    /// `document` stores them.
    #[must_use]
    pub fn in_document(
        app_id: &'static str,
        declares: &[super::Declare],
        document: SettingsDocument,
    ) -> Self {
        Self::declared(
            super::resources::Sections {
                app_id,
                document,
                person: None,
                apis: Vec::new(),
            },
            declares,
        )
    }

    /// The settings sections `declares` declare, of the app `app_id` (titled
    /// `app_title`), changed by `person`: each change is stored in the
    /// session's settings document with what made it and who, and is a step
    /// of its invoker's, undone and redone like any of theirs. `earlier`
    /// are the ids they went by before this device had an identity: what
    /// was changed under one is theirs to take back. The sections' history
    /// is the settings document's, so the host holds no person records
    /// beside them.
    #[must_use]
    pub fn of_person_named(
        app_id: &'static str,
        app_title: &'static str,
        person: &tilo_resource::MemberId,
        earlier: &[String],
        declares: &[super::Declare],
    ) -> Self {
        Self::declared(
            super::resources::Sections {
                app_id,
                document: SettingsDocument::Session,
                person: Some(Rc::new(super::person::PersonState::without_resources(
                    person.clone(),
                    earlier.to_vec(),
                    app_id,
                    app_title,
                ))),
                apis: Vec::new(),
            },
            declares,
        )
    }

    fn declared(sections: super::resources::Sections, declares: &[super::Declare]) -> Self {
        let person = sections.person.clone();
        let mut records = super::Resources::sections(sections);
        for declare in declares {
            declare(&mut records);
        }
        Self {
            apis: records.into_sections(),
            person,
        }
    }

    /// Every section, published with its verbs and field setters, and with
    /// the undo and redo of its invoker's steps where a person is. Sections
    /// of two types the settings keep in one place are one resource there.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        let published = match &self.person {
            Some(person) => person.resources(),
            None => self.apis.iter().map(|api| api.publish()).collect(),
        };
        one_per_place(published, |id| {
            Some(self.apis.iter().find(|api| api.id() == id)?.place_kind())
        })
    }

    /// The section of the settings kept at `id`, and the field its setter
    /// `action` (`set-<field>`) sets: what the same setter of that section on
    /// another of the person's devices writes there. `None` for another
    /// resource or action.
    #[must_use]
    pub fn setter_at(&self, id: &str, action: &str) -> Option<(&'static str, &'static str)> {
        self.apis
            .iter()
            .filter(|api| api.id() == id)
            .find_map(|api| {
                let Some(tilo_resource::Shape::Record(fields)) = api.kind.shape else {
                    return None;
                };
                fields
                    .iter()
                    .filter(|field| !field.readonly)
                    .find(|field| ui_core::setting_verb(field.key) == action)
                    .map(|field| (api.kind.section, field.key))
            })
    }

    /// What the section published at `id` holds now, every type kept there
    /// together. `None` for a resource that is no section of these.
    #[must_use]
    pub fn stored_at(&self, id: &str) -> Option<serde_json::Value> {
        let mut held = serde_json::Map::new();
        let mut any = false;
        for api in self.apis.iter().filter(|api| api.id() == id) {
            any = true;
            match api.kind.stored() {
                Ok(serde_json::Value::Object(fields)) => held.extend(fields),
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(kind = api.kind.kind, %error, "settings section unreadable");
                }
            }
        }
        any.then_some(serde_json::Value::Object(held))
    }

    /// Run the action at `address` with its typed `arguments` for `invoker`. A change
    /// whose published action asks first, for the value it carries (a later
    /// lock), runs only once the person allowed it: it comes back with their `approval`.
    pub fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
        invoker: &ActionInvoker,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::ApiError> {
        let resources = self.resources();
        let spec = tilo_actions::api::resolve(&resources, address)
            .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
        if approval.is_none()
            && spec.confirmation_for_json(arguments) != tilo_actions::ConfirmationPolicy::None
        {
            return Err(tilo_actions::ApiError::ConfirmationRequired);
        }
        let encoded = tilo_actions::api::encode_json(&resources, address, arguments)?;
        if encoded.type_id == super::person::HISTORY_TYPE
            && let Some(person) = &self.person
        {
            return person
                .answer_for(&encoded.payload, invoker)
                .map_err(tilo_actions::ApiError::Failed);
        }
        let request: super::resources::ResourceAction = tilo_actions::decode_json(&encoded.payload)
            .map_err(|error| tilo_actions::ApiError::InvalidArguments(error.to_string()))?;
        let api = self
            .apis
            .iter()
            .find(|api| api.id() == request.record && api.takes(&request.action))
            .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
        api.apply_encoded(&request.action, invoker)
            .map(|()| tilo_actions::ActionOutput::default())
            .map_err(tilo_actions::ApiError::Failed)
    }
}

impl SettingsResources {
    /// What answers these sections' actions where the host keeps them in its
    /// own process, as `person`'s: the controls a window binds to them
    /// ([`Resources::kept_by`](super::Resources::kept_by)) are invoked here, as a
    /// session's broker invokes them where it keeps them. The person's own
    /// settings: nobody else changes them.
    #[must_use]
    pub fn served_for(
        self,
        person: &tilo_actions::MemberId,
    ) -> ui_core::app::driver::ServedProvider {
        let person = person.clone();
        Box::new(move |invocation| {
            if invocation.actor.member_id() != Some(&person) {
                return Err(tilo_actions::ApiError::Unavailable {
                    code: "permission_denied".into(),
                    reason: "Only the person they belong to changes these settings".into(),
                });
            }
            let arguments = match invocation.argument {
                tilo_actions::remote::InvokeArgument::Json(arguments) => arguments.clone(),
                tilo_actions::remote::InvokeArgument::None => serde_json::Map::new(),
                tilo_actions::remote::InvokeArgument::Text(_) => {
                    return Err(tilo_actions::ApiError::InvalidArguments(
                        "a settings change carries typed arguments".into(),
                    ));
                }
            };
            self.invoke(
                invocation.address,
                &arguments,
                invocation.approval,
                &ActionInvoker::member(person.clone()),
            )
        })
    }
}

/// `published`, each section by itself, as the places the settings keep
/// them: where the settings keep two types' sections in one place (the
/// continuity of this device and whether it takes displays' input on its
/// own), that place is one resource, of the place's kind, offering the
/// actions of both.
pub(crate) fn one_per_place(
    published: Vec<tilo_actions::ResourceSpec>,
    kind_at: impl Fn(&str) -> Option<String>,
) -> Vec<tilo_actions::ResourceSpec> {
    let mut places: Vec<tilo_actions::ResourceSpec> = Vec::with_capacity(published.len());
    for resource in published {
        let Some(place) = places.iter_mut().find(|place| place.id == resource.id) else {
            places.push(resource);
            continue;
        };
        if let Some(kind) = kind_at(place.id.as_str()) {
            place.kind = kind;
        }
        for action in resource.actions {
            if !place.actions.iter().any(|held| held.id == action.id) {
                place.actions.push(action);
            }
        }
        place.children.extend(resource.children);
        place.views.extend(resource.views);
    }
    places
}

/// Where the settings section `section` (`appearance`, `power`) is published, and changed by
/// its setters (`<address>#set-<field>`): `settings/appearance` for the person's,
/// `devices/<device>/settings/power` for this device's, `settings/apps/<app>` for an app's
/// preferences (`app`).
#[must_use]
pub fn section_address(section: &str, app: Option<&str>) -> String {
    ui_core::settings_placement::address(section, app, &tilo_ui_base::settings_device::settings_device())
        .id
}

/// The generic actions a record publishes that would set any of a section's
/// fields: a section takes each change by the verb of the field it sets.
const GENERIC: [&str; 3] = [
    tilo_resource::FIELD_EDIT,
    tilo_resource::APPLY,
    tilo_resource::GROUP,
];

/// The parameter of a field's setter carrying the field's whole new value.
pub const SETTER_VALUE: &str = "value";

/// The preferences an app keeps, `held`, as the resource `id` of kind `kind`
/// publishes them: each set by its own verb, `set-<preference>`, taking its
/// new `value` of the type it holds now (any, while it holds nothing).
#[must_use]
pub fn app_preferences(
    id: &str,
    kind: &str,
    app: &str,
    held: &serde_json::Map<String, serde_json::Value>,
) -> tilo_actions::ResourceSpec {
    let mut resource = tilo_actions::ResourceSpec::new(id, kind, app);
    resource.actions = held
        .iter()
        .map(|(preference, now)| preference_setter(kind, preference, now))
        .collect();
    resource
}

/// The verb that sets the app preference `preference`, holding `now`.
fn preference_setter(
    kind: &str,
    preference: &str,
    now: &serde_json::Value,
) -> tilo_actions::ActionSpec {
    use serde_json::Value;
    use tilo_actions::{ActionParam, ParamType};
    let ty = match now {
        Value::Bool(_) => ParamType::Boolean,
        Value::Number(_) => ParamType::Number,
        Value::String(_) => ParamType::Text,
        Value::Array(_) => ParamType::Array,
        Value::Object(_) | Value::Null => ParamType::Object,
    };
    let mut value = ActionParam::required_owned(SETTER_VALUE, ty, preference.to_owned());
    if now.is_null() {
        value = value.nullable();
    }
    let verb = ui_core::setting_verb(preference);
    let type_id = format!("{kind}#{verb}");
    tilo_actions::ActionSpec::typed(
        verb,
        format!("Set {}", preference.replace('_', " ")),
        preference,
        tilo_actions::ExecutionLocus::ClientLocal,
        vec![value],
        move |args| tilo_actions::EncodedAction {
            type_id: type_id.clone(),
            payload: Value::Object(args.to_json()).to_string().into_bytes(),
        },
    )
}

/// The surface of the settings section `T`, as the settings service publishes
/// it and a view of another language reads it: its fields, each that is set
/// only whole marked so, and the verbs it declares. It is written field by
/// field: the setter of each field that is not read only (`set-<field>`,
/// [`with_setters`]), where a record's generic edits would be, follows from
/// its fields ([`tilo_resource::Surface::with_setters`]).
#[must_use]
pub fn section_surface<T: Resource + Fields>() -> tilo_resource::Surface {
    let mut surface = T::surface();
    surface
        .verbs
        .retain(|verb| !GENERIC.contains(&verb.id.as_str()));
    surface.setters = true;
    if let Some(tilo_resource::Shape::Record(fields)) = T::shape() {
        for field in fields
            .iter()
            .filter(|field| field.trust != tilo_resource::Trust::Ordinary)
        {
            if let Some(held) = surface
                .fields
                .iter_mut()
                .find(|held| held.path == field.key)
            {
                held.whole = true;
            }
        }
    }
    surface
}

/// `published`, a section as its type publishes it, its fields shaped
/// `shape`: each field that is not read only is set by a verb of its own,
/// `set-<field>`, and nothing it publishes, nor any of its items, sets any
/// field at all. A grant names the setting it allows; a declared verb of the
/// same id is the field's setter.
pub(crate) fn with_setters(
    mut published: tilo_actions::ResourceSpec,
    shape: Option<tilo_resource::Shape>,
) -> tilo_actions::ResourceSpec {
    let edit = published
        .actions
        .iter()
        .find(|spec| spec.id == tilo_resource::FIELD_EDIT)
        .cloned();
    without_generic(&mut published);
    if let (Some(edit), Some(tilo_resource::Shape::Record(fields))) = (edit, shape) {
        for field in fields.iter().filter(|field| !field.readonly) {
            let id = ui_core::setting_verb(field.key);
            if !published.actions.iter().any(|spec| spec.id == id) {
                published.actions.push(setter(&edit, field, id));
            }
        }
    }
    published
}

fn without_generic(resource: &mut tilo_actions::ResourceSpec) {
    resource
        .actions
        .retain(|spec| !GENERIC.contains(&spec.id.as_str()));
    resource.children.iter_mut().for_each(without_generic);
}

/// The setter `id` of `field`, built of the section's own field edit `edit`:
/// it takes the field's whole new `value`, or an `edit` inside it whose path
/// starts at the field, and asks first exactly where that edit would.
fn setter(
    edit: &tilo_actions::ActionSpec,
    field: &tilo_resource::FieldShape,
    id: String,
) -> tilo_actions::ActionSpec {
    use tilo_actions::{ActionArgs, ActionParam, ParamType};
    let key = field.key;
    let mut params = Vec::new();
    // A collection is changed item by item.
    if !matches!(
        field.shape,
        tilo_resource::Shape::Collection(_) | tilo_resource::Shape::Keyed(_)
    ) {
        let schema = (field.schema)();
        let mut value = ActionParam::optional_owned(
            SETTER_VALUE,
            tilo_actions::params::param_type_of(&schema),
            field.title,
        );
        if admits_null(&schema) {
            value = value.nullable();
        }
        params.push(value.with_schema(schema));
    }
    // A field whose every change may weaken the person's security is set
    // whole, so whoever reads the inert contract reads the value it gets.
    if field.trust == tilo_resource::Trust::Ordinary {
        params.push(ActionParam::optional_owned(
            tilo_resource::EDIT_PARAM,
            ParamType::Object,
            "An edit inside the field: {path, op, ...} as `field-edit` takes it, its path starting at the field",
        ));
    }
    // What the section's own field edit is for these arguments.
    let edit_params = edit.params().to_vec();
    let rooted = std::sync::Arc::new(move |args: &ActionArgs| -> Result<ActionArgs, String> {
        let arguments = rooted_edit(key, args.to_json())?;
        ActionArgs::bind_json(&edit_params, &arguments).map_err(|error| error.to_string())
    });
    let (build, risk, undo, need) = (
        std::sync::Arc::clone(&rooted),
        std::sync::Arc::clone(&rooted),
        std::sync::Arc::clone(&rooted),
        rooted,
    );
    let (build_edit, risk_edit, undo_edit, need_edit) =
        (edit.clone(), edit.clone(), edit.clone(), edit.clone());
    let declared = edit.contract.need();
    let fallback = declared.clone();
    let mut spec = tilo_actions::ActionSpec::typed(
        id,
        format!("Set {}", key.replace('_', " ")),
        field.title,
        tilo_actions::ExecutionLocus::ClientLocal,
        params,
        move |args| match build(args).and_then(|args| {
            build_edit
                .build_json(&args.to_json())
                .map_err(|error| error.to_string())
        }) {
            Ok(encoded) => encoded,
            Err(reason) => EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
                payload: reason.into_bytes(),
            },
        },
    )
    .with_description(field.title)
    .with_need(declared)
    .with_need_of(move |args| {
        need(args).map_or_else(|_| fallback.clone(), |args| need_edit.need_for_args(&args))
    })
    .with_risk(move |args| {
        risk(args).map_or(tilo_actions::RiskLevel::Low, |args| {
            risk_edit.risk_for(&args)
        })
    })
    .with_undo_of(move |args| undo(args).is_ok_and(|args| undo_edit.undoable_for(&args)));
    let availability = edit.contract.availability.clone();
    let tags = field_tags(&edit.contract.tags, key);
    let contract = &mut spec.type_mut().contract;
    contract.availability = availability;
    contract.tags.extend(tags);
    spec
}

/// The arguments of a section's field edit for its setter of the field
/// `key` given `arguments`: the field set to the `value`, or the `edit`
/// inside it, its path from the record's root.
fn rooted_edit(
    key: &str,
    mut arguments: serde_json::Map<String, serde_json::Value>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let field = serde_json::json!({ "field": key });
    let edit = match (
        arguments.remove(SETTER_VALUE),
        arguments.remove(tilo_resource::EDIT_PARAM),
    ) {
        (Some(value), None) => serde_json::json!({ "path": [field], "op": "set", "value": value }),
        (None, Some(serde_json::Value::Object(mut edit))) => {
            let mut path = vec![field];
            match edit.remove("path") {
                Some(serde_json::Value::Array(rest)) => path.extend(rest),
                None => {}
                Some(_) => return Err("an edit's path is a list of steps".to_owned()),
            }
            edit.insert("path".to_owned(), serde_json::Value::Array(path));
            serde_json::Value::Object(edit)
        }
        (Some(_), Some(_)) => {
            return Err("give the value or an edit inside it, not both".to_owned());
        }
        (None, _) => return Err("give the field's value or an edit inside it".to_owned()),
    };
    Ok(serde_json::Map::from_iter([(
        tilo_resource::EDIT_PARAM.to_owned(),
        edit,
    )]))
}

/// Whether a value of `schema` may be null: an optional field.
fn admits_null(schema: &serde_json::Value) -> bool {
    let null = serde_json::json!("null");
    match schema.get("type") {
        Some(serde_json::Value::Array(kinds)) => kinds.contains(&null),
        Some(kind) => *kind == null,
        None => ["anyOf", "oneOf"].iter().any(|key| {
            schema
                .get(key)
                .and_then(serde_json::Value::as_array)
                .is_some_and(|alternatives| alternatives.iter().any(admits_null))
        }),
    }
}

/// The confirmation tags of a section's field edit, `tags`, that are about
/// the field `key`, their paths from the field: whoever reads a setter's
/// inert contract asks where an edit of the field would (a field set whole
/// asks for any value where a value of it may weaken the lock).
fn field_tags(tags: &[String], key: &str) -> Vec<String> {
    use tilo_actions::policy::edits::{
        FIELD_EDIT_TAG, GUARDED_FIELD_TAG, QUIET_REMOVAL_TAG, WEAKENING_FIELD_TAG,
    };
    let mut about: Vec<String> = tags
        .iter()
        .filter_map(|tag| {
            let (prefix, rest) = [GUARDED_FIELD_TAG, QUIET_REMOVAL_TAG, WEAKENING_FIELD_TAG]
                .into_iter()
                .find_map(|prefix| Some((prefix, tag.strip_prefix(prefix)?)))?;
            let (path, tail) = rest.find('\t').map_or((rest, ""), |at| rest.split_at(at));
            let below = if path == key {
                ""
            } else {
                path.strip_prefix(key)?.strip_prefix('.')?
            };
            Some(format!("{prefix}{below}{tail}"))
        })
        .collect();
    if !about.is_empty() {
        about.push(FIELD_EDIT_TAG.to_owned());
    }
    about
}

/// The public id of the action whose encoded payload is `payload`.
fn action_id(payload: &[u8]) -> String {
    tilo_failure::ignored!(
        "a payload that does not read back is named by the generic label",
        serde_json::from_slice::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| {
                let operation = value.get("op").unwrap_or(&value);
                operation
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| "action".to_owned())
    )
}
