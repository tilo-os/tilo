//! A settings section held as a record ([`ResourceEnv::settings`](super::ResourceEnv::settings)):
//! published as `settings:<kind>` with its field edits and verbs, each change
//! checked by the section's model and stored in the settings document with
//! what made it and who.
//!
//! The settings document is the section's only copy: the record reads the
//! section as the document stores it, an action runs on that value and what
//! it changed is written back to the document as one change, and undo and
//! redo of an invoker's steps read and write the document's own history.
//! Every section is kept by the one [`SectionApi`]; what it needs of the
//! section's type is a [`SectionKind`], a handful of functions built once
//! per type; nothing else is compiled per section.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use settings_model::SettingsSchema;
use tilo_actions::{Action, ActionError, ActionInvoker, EncodedAction};
use tilo_resource::{Doc, Fields, ModelError, Reader, Resource};
use ui_core::history_source::{ThreadWords, steps};
use ui_core::storage::{
    ChangeOrigin, InvokingAction, Settings, SettingsDocument, SettingsRevision,
};

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
            of_type: Box::new(settings.clone()),
        })
    }

    /// The section's settings as `T`, the type this kind was made of.
    fn settings<T: SettingsSchema>(&self) -> Settings<T> {
        Settings::for_app(&self.app_id).in_document(self.document.clone())
    }

    fn revision(&self) -> SettingsRevision {
        self.document.revision()
    }

    /// Run `action`, the action `id` by `by`, on the section, run by the
    /// action `invoked_by` when another action ran it, and store what it
    /// made.
    fn apply(
        &self,
        action: &dyn Action,
        id: &str,
        by: &tilo_actions::Actor,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<(), ActionError> {
        let origin = ChangeOrigin::action(format!("{}/{id}", self.kind))
            .by(actor(by))
            .invoked_by(invoked_by.cloned());
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
    /// section, reading the settings document's history: their changes
    /// through the section's actions, or ones under an `alias` of theirs
    /// (an id they went by before this device had an identity). Whether
    /// there was one.
    fn step(
        &self,
        undo: bool,
        by: &tilo_actions::Actor,
        aliases: &[String],
    ) -> Result<bool, String> {
        let verb = if undo { "undo" } else { "redo" };
        let by = actor(by);
        let origin = ChangeOrigin::action(format!("{}/{verb}", self.kind)).by(by.clone());
        let actions = format!("{}/", self.kind);
        let aliases: Vec<String> = aliases
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
            })
            .collect();
        let owns = |version: &tilo_resource::history::Version| {
            version.message.as_deref().is_some_and(|message| {
                std::iter::once(&by)
                    .chain(&aliases)
                    .any(|who| ui_core::storage::made_by(message, &actions, who))
            })
        };
        let mut stepped = false;
        self.document
            .edit_document(self.section, &origin, |doc| {
                // A section's actions queue nothing outside the document:
                // a step is taken back by reverting what it wrote.
                let steps = if undo {
                    steps::undo_where(doc, &owns, 1, |_, _| Ok(None), &ThreadWords)
                } else {
                    steps::redo_where(doc, &owns, 1, |_, _| Ok(None), &ThreadWords)
                }?;
                stepped = !steps.is_empty();
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        Ok(stepped)
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
            id: format!("settings:{}", kind.kind),
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

    /// Apply the encoded action `encoded` for `invoker`, run by the action
    /// `invoked_by` when another action ran it, and store what it made as
    /// what made it.
    fn apply(
        &self,
        encoded: &EncodedAction,
        invoker: &ActionInvoker,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<(), ActionError> {
        let action = self.kind.decode(encoded)?;
        let id = action_id(&encoded.payload);
        self.kind
            .apply(action.as_ref(), &id, &invoker.acting.by, invoked_by)?;
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
                applying.apply(&tilo_resource::encode::<T>(action)?, invoker, None)
            },
        )
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
        let revision = (self.revision.get(), ui_core::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published = self.kind.publish(&self.id).unwrap_or_else(|error| {
            tracing::warn!(kind = self.kind.kind, %error, "settings section unreadable");
            tilo_actions::ResourceSpec::new(self.id.as_str(), self.kind.kind, self.kind.label)
        });
        let published = super::resources::route(published, &self.id, true);
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String> {
        self.apply_invoked(action, invoker, None)
    }
}

impl SectionApi {
    /// [`ResourceApi::apply_encoded`], run by the action `invoked_by` when
    /// another action ran this one.
    fn apply_invoked(
        &self,
        action: &EncodedAction,
        invoker: &ActionInvoker,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<(), String> {
        if action.type_id != self.kind.action_type {
            return Err(format!(
                "{} takes no `{}` action",
                self.kind.label, action.type_id
            ));
        }
        self.apply(action, invoker, invoked_by)
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
        Self::of_person_named_in(
            app_id,
            app_title,
            person,
            earlier,
            declares,
            SettingsDocument::Session,
        )
    }

    /// [`Self::of_person_named`], kept in `document`: a host's profile of its
    /// own rather than the session's settings.
    #[must_use]
    pub fn of_person_named_in(
        app_id: &'static str,
        app_title: &'static str,
        person: &tilo_resource::MemberId,
        earlier: &[String],
        declares: &[super::Declare],
        document: SettingsDocument,
    ) -> Self {
        Self::declared(
            super::resources::Sections {
                app_id,
                document,
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

    /// Every section, published with its field edits and verbs, and with
    /// the undo and redo of its invoker's steps where a person is.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        match &self.person {
            Some(person) => person.resources(),
            None => self.apis.iter().map(|api| api.publish()).collect(),
        }
    }

    /// The section `kind` (`settings.appearance`) as stored now, as its
    /// model writes it: what a control showing one of its fields shows.
    /// `None` for a section not declared here.
    #[must_use]
    pub fn stored(&self, kind: &str) -> Option<serde_json::Value> {
        let api = self.apis.iter().find(|api| api.kind.kind == kind)?;
        api.kind
            .stored()
            .map_err(|error| tracing::warn!(kind, %error, "settings section unreadable"))
            .ok()
    }

    /// Run the action at `address` with its typed `arguments` for `invoker`,
    /// on behalf of the action `invoked_by` when another action runs it
    /// (the palette's dark mode runs the appearance field edit). The change
    /// is recorded with both, so its history tells them apart. A change
    /// whose published action asks first, for the value it carries (a later
    /// lock), runs only once the person allowed it: it comes back with their `approval`.
    pub fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        approval: Option<&tilo_actions::remote::asked::Approval>,
        invoker: &ActionInvoker,
        invoked_by: Option<&InvokingAction>,
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
            .find(|api| api.id() == request.record)
            .ok_or_else(|| tilo_actions::ApiError::Unknown(address.clone()))?;
        api.apply_invoked(&request.action, invoker, invoked_by)
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
    pub fn served_for(self, person: &tilo_actions::MemberId) -> ui_core::headless::ServedProvider {
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
                None,
            )
        })
    }
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
