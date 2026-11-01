//! A settings section held as a record ([`RecordEnv::settings`](super::RecordEnv::settings)):
//! published as `settings:<kind>` with its field edits and verbs, each change
//! checked by the section's model and stored in the settings document with
//! what made it and who.
//!
//! The settings document is the section's only copy: the record reads the
//! section as the document stores it, an action runs on that value and what
//! it changed is written back to the document as one change, and undo and
//! redo of an invoker's steps read and write the document's own history.
//! Every section is kept by the one [`SectionDoor`]; what it needs of the
//! section's type is a [`SectionKind`], a handful of functions built once
//! per type; nothing else is compiled per section.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_actions::{Action, ActionError, ActionInvoker, EncodedAction};
use tilo_resource::{Doc, Fields, Reader, Resource};
use ui_core::history_source::{ThreadWords, steps};
use ui_core::storage::{
    ChangeOrigin, InvokingAction, Settings, SettingsDocument, SettingsRevision, SettingsSchema,
};

use super::live::Live;
use super::record::{Held, RecordDoor};

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

/// What a section's record does with the section's type: one function per
/// section type ([`SectionOp`]), over the section's settings
/// ([`Self::settings`]).
pub(crate) struct SectionKind {
    pub(crate) kind: &'static str,
    pub(crate) label: &'static str,
    action_type: &'static str,
    /// The section of the settings document the value is kept in.
    section: &'static str,
    /// The app whose settings the section is, and the document they live in.
    app_id: String,
    document: SettingsDocument,
    ops: fn(&Self, SectionOp<'_>) -> Result<SectionOut, ActionError>,
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
            ops: section_ops::<T>,
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
        by: &str,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<(), ActionError> {
        let origin = ChangeOrigin::action(format!("{}/{id}", self.kind))
            .by(by.to_owned())
            .invoked_by(invoked_by.cloned());
        (self.ops)(self, SectionOp::Apply(action, &origin)).map(drop)
    }

    fn publish(&self, id: &str) -> Result<tilo_actions::ResourceSpec, ActionError> {
        match (self.ops)(self, SectionOp::Publish(id, self.label))? {
            SectionOut::Published(resource) => Ok(*resource),
            _ => Err(ActionError::Refused("the section published nothing".into())),
        }
    }

    fn decode(&self, action: &EncodedAction) -> Result<Box<dyn Action>, ActionError> {
        match (self.ops)(self, SectionOp::Decode(action))? {
            SectionOut::Decoded(action) => Ok(action),
            _ => Err(ActionError::Refused("the section decoded nothing".into())),
        }
    }

    /// The section as stored now, as its model writes it.
    fn stored(&self) -> Result<serde_json::Value, ActionError> {
        match (self.ops)(self, SectionOp::Stored)? {
            SectionOut::Value(value) => Ok(value),
            _ => Err(ActionError::Refused("the section read nothing".into())),
        }
    }

    /// Take back (`undo`) or bring back the last step `by` took on the
    /// section, reading the settings document's history: their changes
    /// through the section's actions. Whether there was one.
    fn step(&self, undo: bool, by: &str) -> Result<bool, String> {
        let verb = if undo { "undo" } else { "redo" };
        let origin = ChangeOrigin::action(format!("{}/{verb}", self.kind)).by(by.to_owned());
        let actions = format!("{}/", self.kind);
        let owns = |version: &tilo_resource::history::Version| {
            version
                .message
                .as_deref()
                .is_some_and(|message| ui_core::storage::made_by(message, &actions, by))
        };
        let mut stepped = false;
        self.document
            .edit_document(self.section, &origin, |doc| {
                // A section's actions queue nothing outside the document:
                // a step is taken back by reverting what it wrote.
                let steps = if undo {
                    steps::undo_where(doc, &owns, 1, |_| Ok(None), &ThreadWords)
                } else {
                    steps::redo_where(doc, &owns, 1, |_| Ok(None), &ThreadWords)
                }?;
                stepped = !steps.is_empty();
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        Ok(stepped)
    }
}

/// Everything a section's record asks of the section type `T`.
fn section_ops<T>(kind: &SectionKind, op: SectionOp<'_>) -> Result<SectionOut, ActionError>
where
    T: Resource<State = T> + SettingsSchema + Fields,
{
    let refused = |error: &dyn std::fmt::Display| ActionError::Refused(error.to_string());
    Ok(match op {
        SectionOp::Apply(action, origin) => {
            // A resource's actions apply to a record at a document's root:
            // the section's value is laid out as one for the action to run
            // on, and what it made is stored as the section's change.
            let root = &tilo_resource::__private::ROOT;
            let settings = kind.settings::<T>();
            let mut record = Doc::new();
            T::initialize(&mut record).map_err(|error| refused(&error))?;
            settings
                .get()
                .write_fields(&mut record, root)
                .map_err(|error| refused(&error))?;
            record.commit();
            action.apply(&mut record)?;
            let value = T::read_fields(&record, root).map_err(|error| refused(&error))?;
            settings
                .set_by(&value, Some(origin))
                .map_err(|error| refused(&error))?;
            SectionOut::Done
        }
        SectionOp::Publish(id, label) => SectionOut::Published(Box::new(
            tilo_resource::publish::<T>(id, label, &kind.settings::<T>().get()),
        )),
        SectionOp::Decode(action) => SectionOut::Decoded(Box::new(
            tilo_actions::decode_json::<T::Action>(&action.payload)
                .map_err(|error| refused(&error))?,
        )),
        SectionOp::Stored => {
            SectionOut::Value(tilo_resource::Model::to_json(&kind.settings::<T>().get()))
        }
    })
}

/// A settings section held as a record, whatever its type.
pub(crate) struct SectionDoor {
    id: String,
    kind: Rc<SectionKind>,
    /// The settings document's version the door last read.
    seen: Cell<Option<SettingsRevision>>,
    /// Increases whenever what the section holds may have changed.
    revision: Cell<u64>,
    /// The last publication, at the door's and the locale's revision it was
    /// built at: published again only when either moved.
    published: RefCell<Option<((u64, u64), tilo_actions::ResourceSpec)>>,
}

impl SectionDoor {
    pub(crate) fn new(kind: Rc<SectionKind>) -> Self {
        Self {
            id: format!("settings:{}", kind.kind),
            kind,
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

    /// Note whether the settings document moved since the door last read
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
            .apply(action.as_ref(), &id, &invoker.actor, invoked_by)?;
        self.notice();
        Ok(())
    }

    /// Take back (`undo`) or redo the last step `by` took on the section.
    /// Whether there was one.
    pub(crate) fn step(&self, undo: bool, by: &str) -> bool {
        match self.kind.step(undo, by) {
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

impl RecordDoor for SectionDoor {
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
        let published = super::record::route(published, &self.id, true);
        *self.published.borrow_mut() = Some((revision, published.clone()));
        published
    }

    fn apply_encoded(&self, action: &EncodedAction, invoker: &ActionInvoker) -> Result<(), String> {
        self.apply_invoked(action, invoker, None)
    }
}

impl SectionDoor {
    /// [`RecordDoor::apply_encoded`], run by the action `invoked_by` when
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
pub struct SettingsRecords {
    doors: Vec<Rc<SectionDoor>>,
    /// The person the sections are changed by, whose undo and redo of
    /// their steps on each are published beside it, with their shared
    /// records held beside the sections.
    person: Option<Rc<super::person::PersonState>>,
}

impl SettingsRecords {
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
            super::record::Sections {
                app_id,
                document,
                person: None,
                doors: Vec::new(),
            },
            declares,
        )
    }

    /// The settings sections `declares` declare, of the app `app_id` (titled
    /// `app_title`), as the app's window keeps them for the person whose
    /// `records` hold their shared records: each change is stored in the
    /// session's settings document with what made it and who, and is a step
    /// of its invoker's, undone and redone like any of theirs.
    #[must_use]
    pub fn of_person(
        app_id: &'static str,
        app_title: &'static str,
        declares: &[super::Declare],
        records: ui_core::record::person::PersonRecords,
    ) -> Self {
        Self::declared(
            super::record::Sections {
                app_id,
                document: SettingsDocument::Session,
                person: Some(Rc::new(super::person::PersonState::new(
                    records, None, app_id, app_title,
                ))),
                doors: Vec::new(),
            },
            declares,
        )
    }

    /// The settings sections `declares` declare, of the app `app_id` (titled
    /// `app_title`), changed by `person`: each change is stored in the
    /// session's settings document with what made it and who, and is a step
    /// of its invoker's, undone and redone like any of theirs. The
    /// sections' history is the settings document's, so the host holds no
    /// person records beside them.
    #[must_use]
    pub fn of_person_named(
        app_id: &'static str,
        app_title: &'static str,
        person: &str,
        declares: &[super::Declare],
    ) -> Self {
        Self::declared(
            super::record::Sections {
                app_id,
                document: SettingsDocument::Session,
                person: Some(Rc::new(super::person::PersonState::without_records(
                    person.to_owned(),
                    app_id,
                    app_title,
                ))),
                doors: Vec::new(),
            },
            declares,
        )
    }

    fn declared(sections: super::record::Sections, declares: &[super::Declare]) -> Self {
        let person = sections.person.clone();
        let mut records = super::Records::sections(sections);
        for declare in declares {
            declare(&mut records);
        }
        Self {
            doors: records.into_sections(),
            person,
        }
    }

    /// The person's shared records held beside the sections
    /// ([`Self::shared`]), as their routes sync them to the person's other
    /// devices; none where no person is.
    #[must_use]
    pub fn history_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        self.person
            .as_ref()
            .map(|person| person.owners())
            .unwrap_or_default()
    }

    /// The person's shared record `P` (their accounts, their contacts),
    /// held beside the sections and synced over its route
    /// ([`Self::history_owners`]), opened on first use; not published here.
    /// `None` where no person is.
    #[must_use]
    pub fn shared<P: tilo_resource::SharedRecord>(&self) -> Option<super::PersonRecord<P>> {
        self.person.as_ref()?.hold_shared::<P>()
    }

    /// Every section, published with its field edits and verbs, and with
    /// the undo and redo of its invoker's steps where a person is.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        match &self.person {
            Some(person) => person.resources(),
            None => self.doors.iter().map(|door| door.publish()).collect(),
        }
    }

    /// The section `kind` (`settings.appearance`) as stored now, as its
    /// model writes it: what a control showing one of its fields shows.
    /// `None` for a section not declared here.
    #[must_use]
    pub fn stored(&self, kind: &str) -> Option<serde_json::Value> {
        let door = self.doors.iter().find(|door| door.kind.kind == kind)?;
        door.kind
            .stored()
            .map_err(|error| tracing::warn!(kind, %error, "settings section unreadable"))
            .ok()
    }

    /// Run the action at `address` with its typed `arguments` for `invoker`,
    /// on behalf of the action `invoked_by` when another action runs it
    /// (the palette's dark mode runs the appearance field edit). The change
    /// is recorded with both, so its history tells them apart. A change
    /// whose published action asks first, for the value it carries (a later
    /// lock), runs only once it is `confirmed`.
    pub fn invoke(
        &self,
        address: &tilo_actions::ActionAddress,
        arguments: &serde_json::Map<String, serde_json::Value>,
        confirmed: bool,
        invoker: &ActionInvoker,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<tilo_actions::ActionOutput, tilo_actions::DoorError> {
        let resources = self.resources();
        let spec = tilo_actions::door::resolve(&resources, address)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        if !confirmed
            && spec.confirmation_for_json(arguments) != tilo_actions::ConfirmationPolicy::None
        {
            return Err(tilo_actions::DoorError::ConfirmationRequired);
        }
        let encoded = tilo_actions::door::encode_json(&resources, address, arguments)?;
        if encoded.type_id == super::person::HISTORY_TYPE
            && let Some(person) = &self.person
        {
            return person
                .answer_for(&encoded.payload, invoker)
                .map_err(tilo_actions::DoorError::Failed);
        }
        let request: super::record::RecordAction = tilo_actions::decode_json(&encoded.payload)
            .map_err(|error| tilo_actions::DoorError::InvalidArguments(error.to_string()))?;
        let door = self
            .doors
            .iter()
            .find(|door| door.id() == request.record)
            .ok_or_else(|| tilo_actions::DoorError::Unknown(address.clone()))?;
        door.apply_invoked(&request.action, invoker, invoked_by)
            .map(|()| tilo_actions::ActionOutput::default())
            .map_err(tilo_actions::DoorError::Failed)
    }
}

/// The public id of the action whose encoded payload is `payload`.
fn action_id(payload: &[u8]) -> String {
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
}
