//! A settings section held as a record ([`RecordEnv::settings`](super::RecordEnv::settings)):
//! published as `settings:<kind>` with its field edits and verbs, each change
//! checked by the section's model and stored in the settings document with
//! what made it and who.
//!
//! Every section is kept by the one [`SectionDoor`]: the window's own record,
//! or a record of the person's, whose edits are their steps. What it needs of
//! the section's type is a [`SectionKind`], a handful of functions built once
//! per type; nothing else is compiled per section.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_actions::{Action, ActionContext, ActionError, ActionInvoker, ChangeKind, EncodedAction};
use tilo_resource::{Doc, Fields, Reader, Resource};
use ui_core::record::person::PersonRecord as StoredRecord;
use ui_core::storage::{
    ChangeOrigin, InvokingAction, Settings, SettingsDocument, SettingsRevision, SettingsSchema,
};

use super::live::Live;
use super::record::{Held, RecordDoor};

/// One thing a section's record asks of the section's type.
enum SectionOp<'a> {
    Initialize(&'a mut Doc),
    Write(&'a mut Doc),
    Store(&'a Doc, &'a ChangeOrigin),
    /// Whether the document's fields hold the stored value.
    HoldsStored(&'a Doc),
    /// The section the document holds, published under an id and a label.
    Publish(&'a Doc, &'a str, &'a str),
    Decode(&'a EncodedAction),
    /// The section as stored now.
    Stored,
}

/// What a [`SectionOp`] answers.
enum SectionOut {
    Done,
    Holds(bool),
    Published(Box<tilo_actions::Resource>),
    Decoded(Box<dyn Action>),
    Value(serde_json::Value),
}

/// What a section's record does with the section's type: one function per
/// section type ([`SectionOp`]), over the section's settings
/// ([`Self::settings`]).
pub(crate) struct SectionKind {
    pub(crate) kind: &'static str,
    pub(crate) label: &'static str,
    /// The section keeps a value per device.
    pub(crate) device_local: bool,
    /// The storage version its records are written at, and how an older
    /// record is brought up to it before it is read.
    version: u32,
    migrate: fn(&mut Doc, u32) -> Result<(), tilo_resource::ModelError>,
    action_type: &'static str,
    /// The app whose settings the section is, and the document they live in.
    app_id: String,
    document: SettingsDocument,
    ops: fn(&Self, SectionOp<'_>) -> Result<SectionOut, String>,
}

impl SectionKind {
    pub(crate) fn of<T>(settings: &Settings<T>) -> Rc<Self>
    where
        T: Resource<State = T> + SettingsSchema + Fields,
    {
        Rc::new(Self {
            kind: T::KIND,
            label: T::LABEL,
            device_local: matches!(T::SCOPE, ui_core::storage::Scope::DeviceLocal),
            version: T::VERSION,
            migrate: T::migrate,
            action_type: T::ACTION_TYPE,
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

    /// Make `doc` a document of the section holding its stored value.
    pub(crate) fn fill(&self, doc: &mut Doc) -> Result<(), String> {
        (self.ops)(self, SectionOp::Initialize(doc))?;
        self.write(doc)
    }

    fn write(&self, doc: &mut Doc) -> Result<(), String> {
        (self.ops)(self, SectionOp::Write(doc)).map(drop)
    }

    fn holds_stored(&self, doc: &Doc) -> bool {
        matches!(
            (self.ops)(self, SectionOp::HoldsStored(doc)),
            Ok(SectionOut::Holds(true))
        )
    }

    /// Store what `doc` holds as the action `action` by `by` changed it,
    /// run by the action `invoked_by` when another action ran it.
    fn store(
        &self,
        doc: &Doc,
        action: &str,
        by: &str,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<(), String> {
        let origin = ChangeOrigin::action(format!("{}/{action}", self.kind))
            .by(by.to_owned())
            .invoked_by(invoked_by.cloned());
        (self.ops)(self, SectionOp::Store(doc, &origin)).map(drop)
    }

    fn publish(&self, doc: &Doc, id: &str) -> Result<tilo_actions::Resource, String> {
        match (self.ops)(self, SectionOp::Publish(doc, id, self.label))? {
            SectionOut::Published(resource) => Ok(*resource),
            _ => Err("the section published nothing".into()),
        }
    }

    fn decode(&self, action: &EncodedAction) -> Result<Box<dyn Action>, String> {
        match (self.ops)(self, SectionOp::Decode(action))? {
            SectionOut::Decoded(action) => Ok(action),
            _ => Err("the section decoded nothing".into()),
        }
    }

    /// The section as stored now, as its model writes it.
    fn stored(&self) -> Result<serde_json::Value, String> {
        match (self.ops)(self, SectionOp::Stored)? {
            SectionOut::Value(value) => Ok(value),
            _ => Err("the section read nothing".into()),
        }
    }
}

/// Everything a section's record asks of the section type `T`.
fn section_ops<T>(kind: &SectionKind, op: SectionOp<'_>) -> Result<SectionOut, String>
where
    T: Resource<State = T> + SettingsSchema + Fields,
{
    let root = &tilo_resource::__private::ROOT;
    Ok(match op {
        SectionOp::Initialize(doc) => {
            T::initialize(doc).map_err(|error| error.to_string())?;
            SectionOut::Done
        }
        SectionOp::Write(doc) => {
            kind.settings::<T>()
                .get()
                .write_fields(doc, root)
                .map_err(|error| error.to_string())?;
            SectionOut::Done
        }
        SectionOp::Store(doc, origin) => {
            let value = T::read_fields(doc, root).map_err(|error| error.to_string())?;
            kind.settings::<T>()
                .set_by(&value, Some(origin))
                .map_err(|error| error.to_string())?;
            SectionOut::Done
        }
        SectionOp::HoldsStored(doc) => {
            SectionOut::Holds(T::read_fields(doc, root).is_ok_and(|held| {
                tilo_resource::Model::to_json(&held)
                    == tilo_resource::Model::to_json(&kind.settings::<T>().get())
            }))
        }
        SectionOp::Publish(doc, id, label) => {
            let state = T::read(doc).map_err(|error| error.to_string())?;
            SectionOut::Published(Box::new(tilo_resource::publish::<T>(id, label, &state)))
        }
        SectionOp::Decode(action) => SectionOut::Decoded(Box::new(
            tilo_actions::decode_json::<T::Action>(&action.payload)
                .map_err(|error| error.to_string())?,
        )),
        SectionOp::Stored => {
            SectionOut::Value(tilo_resource::Model::to_json(&kind.settings::<T>().get()))
        }
    })
}

enum Kept {
    /// A document of the window's own, which lives as long as the window: in
    /// a window that keeps nothing of the person's (a private one).
    Window {
        doc: Box<RefCell<Doc>>,
        /// The settings document's version the record last followed.
        seen: Cell<SettingsRevision>,
    },
    /// A record of the person's: its edits are their steps, undone and
    /// redone like any of theirs. Opened only when something needs it (a
    /// write creates it, an undo or redo opens it where it exists); until
    /// then the section reads as stored, from `scratch`.
    Person {
        opener: Box<Opener>,
        record: RefCell<Option<Box<StoredRecord>>>,
        /// The stored value, while the record is not open.
        scratch: Box<RefCell<Doc>>,
        scratch_seen: Cell<SettingsRevision>,
        /// The settings document's version the record last followed.
        seen: Cell<Option<SettingsRevision>>,
        /// The record's heads when last read.
        heads: RefCell<Vec<ui_core::automerge::ChangeHash>>,
        app_id: &'static str,
        app_title: &'static str,
        me: String,
    },
}

/// How a section's person record is opened when it is first needed.
pub(crate) struct Opener {
    pub(crate) records: ui_core::record::person::PersonRecords,
    /// The record kind it is kept under.
    pub(crate) name: String,
    /// The routes of the person's open records, which the runtime syncs.
    pub(crate) owners: Rc<RefCell<Vec<ui_core::history_mode::HistoryOwner>>>,
}

/// A settings section held as a record, whatever its type.
pub(crate) struct SectionDoor {
    id: String,
    kind: Rc<SectionKind>,
    editable: bool,
    kept: Kept,
    /// Increases whenever what the record holds may have changed.
    revision: Cell<u64>,
    /// The last publication, at the door's and the locale's revision it was
    /// built at: published again only when either moved.
    published: RefCell<Option<((u64, u64), tilo_actions::Resource)>>,
}

impl SectionDoor {
    /// The section kept in a document of the window's own.
    ///
    /// # Errors
    /// The stored value does not make a document of the section.
    pub(crate) fn window(kind: Rc<SectionKind>, editable: bool) -> Result<Self, String> {
        let mut doc = Doc::new();
        kind.fill(&mut doc)?;
        doc.commit();
        Ok(Self {
            id: format!("settings:{}", kind.kind),
            editable,
            kept: Kept::Window {
                seen: Cell::new(kind.revision()),
                doc: Box::new(RefCell::new(doc)),
            },
            kind,
            revision: Cell::new(0),
            published: RefCell::new(None),
        })
    }

    /// The section kept in the person's record, which `opener` opens when
    /// it is first needed.
    ///
    /// # Errors
    /// The stored value does not make a document of the section.
    pub(crate) fn person(
        kind: Rc<SectionKind>,
        opener: Opener,
        app_id: &'static str,
        app_title: &'static str,
        me: String,
    ) -> Result<Self, String> {
        let mut scratch = Doc::new();
        kind.fill(&mut scratch)?;
        scratch.commit();
        Ok(Self {
            id: format!("settings:{}", kind.kind),
            editable: true,
            kept: Kept::Person {
                opener: Box::new(opener),
                record: RefCell::new(None),
                scratch: Box::new(RefCell::new(scratch)),
                scratch_seen: Cell::new(kind.revision()),
                seen: Cell::new(None),
                heads: RefCell::new(Vec::new()),
                app_id,
                app_title,
                me,
            },
            kind,
            revision: Cell::new(0),
            published: RefCell::new(None),
        })
    }

    /// The person's record, opened now if `create` (making it where it does
    /// not exist) or if it exists; `None` for a window's own section, or a
    /// person record that does not exist and is not to be made.
    fn person_record(&self, create: bool) -> Option<std::cell::Ref<'_, Box<StoredRecord>>> {
        let Kept::Person { opener, record, .. } = &self.kept else {
            return None;
        };
        if record.borrow().is_none() {
            let kind = ui_core::record::RecordKind::new(opener.name.clone());
            let opened = if create {
                opener
                    .records
                    .open_record(kind, &|doc| self.kind.fill(doc))
                    .map(Some)
            } else {
                opener.records.open_existing_record(kind)
            };
            match opened {
                Ok(Some(opened)) => {
                    let stored = opened
                        .read(tilo_resource::evolution::stored_version)
                        .unwrap_or(1);
                    if stored < self.kind.version {
                        let migrate = self.kind.migrate;
                        if let Err(error) = opened.edit(false, |doc| {
                            migrate(doc, stored).map_err(|error| error.to_string())
                        }) {
                            tracing::warn!(
                                kind = self.kind.kind,
                                %error,
                                "settings record could not be brought up to its storage version"
                            );
                            return None;
                        }
                    }
                    opener.owners.borrow_mut().push(opened.history_owner());
                    *record.borrow_mut() = Some(Box::new(opened));
                    self.moved();
                }
                Ok(None) => return None,
                Err(error) => {
                    tracing::warn!(kind = self.kind.kind, %error, "person record unavailable");
                    return None;
                }
            }
        }
        std::cell::Ref::filter_map(record.borrow(), Option::as_ref).ok()
    }

    pub(crate) fn label(&self) -> &'static str {
        self.kind.label
    }

    /// Open the person's record the section is kept in, making it where it
    /// does not exist yet: a holder that syncs it holds it from the start,
    /// so what the person's other devices change reaches it.
    fn hold(&self) {
        drop(self.person_record(true));
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Take into the record what was stored elsewhere (another window, the
    /// Settings app, another device): content from outside, nobody's step.
    /// Whether the record moved since it was last read.
    pub(crate) fn follow(&self) -> bool {
        let moved = match &self.kept {
            Kept::Window { doc, seen } => follow_doc(&self.kind, doc, seen),
            Kept::Person {
                record,
                scratch,
                scratch_seen,
                seen,
                heads,
                ..
            } => match record.borrow().as_deref() {
                None => follow_doc(&self.kind, scratch, scratch_seen),
                Some(record) => {
                    let revision = self.kind.revision();
                    if seen.get() != Some(revision) {
                        seen.set(Some(revision));
                        let holds = record
                            .read(|doc| self.kind.holds_stored(doc))
                            .unwrap_or(false);
                        if !holds && let Err(error) = record.edit(false, |doc| self.kind.write(doc))
                        {
                            tracing::warn!(kind = self.kind.kind, %error, "settings record did not follow the section");
                        }
                    }
                    let now = record.heads();
                    let moved = *heads.borrow() != now;
                    *heads.borrow_mut() = now;
                    moved
                }
            },
        };
        if moved {
            self.moved();
        }
        moved
    }

    /// Read the record as it is now.
    pub(crate) fn read<T>(&self, read: impl FnOnce(&Doc) -> T) -> Option<T> {
        match &self.kept {
            Kept::Window { doc, .. } => Some(read(&doc.borrow())),
            Kept::Person {
                record, scratch, ..
            } => match record.borrow().as_deref() {
                Some(record) => record.read(read),
                None => Some(read(&scratch.borrow())),
            },
        }
    }

    /// Apply the encoded action `encoded` for `invoker`, run by the action
    /// `invoked_by` when another action ran it, store the result as what
    /// made it, and take it back where the store refuses it.
    fn apply(
        &self,
        encoded: &EncodedAction,
        invoker: &ActionInvoker,
        invoked_by: Option<&InvokingAction>,
    ) -> Result<(), ActionError> {
        if !self.editable {
            return Err(ActionError::Refused(format!(
                "{} is changed in another app",
                self.kind.label
            )));
        }
        self.follow();
        let action = self.kind.decode(encoded).map_err(ActionError::Refused)?;
        let id = action_id(&encoded.payload);
        match &self.kept {
            Kept::Window { doc, seen } => {
                let mut doc = doc.borrow_mut();
                let changed = action.apply(&mut doc).and_then(|()| {
                    self.kind
                        .store(&doc, &id, &invoker.actor, invoked_by)
                        .map_err(ActionError::Refused)
                });
                match changed {
                    Ok(()) => {
                        doc.commit();
                        seen.set(self.kind.revision());
                    }
                    Err(error) => {
                        doc.rollback();
                        return Err(error);
                    }
                }
            }
            Kept::Person {
                seen,
                app_id,
                app_title,
                me,
                ..
            } => {
                let record = self.person_record(true).ok_or_else(|| {
                    ActionError::Refused(format!("{} could not be kept", self.kind.label))
                })?;
                let change = action.change_kind();
                let context =
                    ActionContext::for_resource(self.kind.kind, self.kind.kind, self.kind.label)
                        .with_app(*app_id, *app_title)
                        .with_invoker(invoker.clone())
                        .with_change(change);
                // The value is stored from the document the edit made, in
                // the same loan: reading the record again afterwards can
                // find it busy, and a change that reached the record but not
                // the settings document applied nowhere. A store the
                // settings document refuses is the person's to see (the
                // action is refused), never a silent no-op; the record
                // follows the stored value again on its next read.
                record
                    .edit(change == ChangeKind::Content, |doc| {
                        let writer =
                            ui_core::history_source::authors::device_actor(me, doc.get_actor());
                        tilo_actions::apply_local_with_context(
                            doc,
                            action.as_ref(),
                            encoded.clone(),
                            Some(&writer),
                            context,
                        )
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                        .and_then(|()| self.kind.store(doc, &id, &invoker.actor, invoked_by))
                    })
                    .map_err(ActionError::Refused)?;
                seen.set(Some(self.kind.revision()));
            }
        }
        self.follow();
        self.moved();
        Ok(())
    }

    /// Write the person's record's value to the settings document, as
    /// `action` by `by`.
    fn write_person(
        &self,
        record: &StoredRecord,
        seen: &Cell<Option<SettingsRevision>>,
        action: &str,
        by: &str,
    ) {
        let Some(written) = record.read(|doc| self.kind.store(doc, action, by, None)) else {
            tracing::warn!(
                kind = self.kind.kind,
                "settings section not written: its record is busy"
            );
            return;
        };
        if let Err(error) = written {
            tracing::warn!(kind = self.kind.kind, %error, "settings section not written");
        }
        seen.set(Some(self.kind.revision()));
    }

    /// Take back (`undo`) or redo one of the person's steps on the record,
    /// for `by`. Whether there was one.
    pub(crate) fn step(&self, undo: bool, by: &str) -> bool {
        let Kept::Person { seen, .. } = &self.kept else {
            return false;
        };
        let Some(record) = self.person_record(false) else {
            return false;
        };
        let stepped = if undo { record.undo() } else { record.redo() };
        if stepped {
            self.write_person(&record, seen, if undo { "undo" } else { "redo" }, by);
        }
        drop(record);
        if stepped {
            self.follow();
            self.moved();
        }
        stepped
    }

    /// The section, as a record its views edit through its type: a view part
    /// over it ([`ViewCx::record`](super::ViewCx::record)).
    ///
    /// # Errors
    /// The record does not read as `T`.
    pub(crate) fn held<T: Resource<State = T>>(self: &Rc<Self>) -> Result<Held<T>, String> {
        let state = self
            .read(Reader::<T>::read)
            .ok_or("the section's record is unavailable")?
            .map_err(|error| error.to_string())?;
        let (following, applying) = (Rc::clone(self), Rc::clone(self));
        Ok(Held::new(
            self.id.clone(),
            self.kind.label.to_owned(),
            self.editable,
            Live::new(state),
            move |live: &Live<T>| {
                following.follow();
                if let Some(Err(error)) = following.read(|doc| live.sync(doc)) {
                    tracing::warn!(%error, kind = T::KIND, "settings section unreadable");
                }
            },
            move |action: &T::Action, invoker: &ActionInvoker| {
                applying.apply(&tilo_resource::encode::<T>(action)?, invoker, None)
            },
        ))
    }
}

impl RecordDoor for SectionDoor {
    fn id(&self) -> &str {
        &self.id
    }

    fn editable(&self) -> bool {
        self.editable
    }

    fn revision(&self) -> u64 {
        self.follow();
        self.revision.get()
    }

    fn publish(&self) -> tilo_actions::Resource {
        self.follow();
        let revision = (self.revision.get(), ui_core::i18n::I18nManager::revision());
        if let Some((at, published)) = self.published.borrow().as_ref()
            && *at == revision
        {
            return published.clone();
        }
        let published = self
            .read(|doc| self.kind.publish(doc, &self.id))
            .unwrap_or_else(|| Err("the section's record is unavailable".into()))
            .unwrap_or_else(|error| {
                tracing::warn!(kind = self.kind.kind, %error, "settings section unreadable");
                tilo_actions::Resource::new(self.id.as_str(), self.kind.kind, self.kind.label)
            });
        let published = super::record::route(published, &self.id, self.editable);
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
    /// The person's records the sections are kept in, whose undo and redo
    /// are published beside them.
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
    /// `document` stores them now, each in a document of the host's own.
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
    /// `app_title`), kept in the person's `records` as the app's window
    /// keeps them, on the device `device`: each change is a step of the
    /// person's, undone and redone like any of theirs, and stored in the
    /// session's settings document with what made it and who.
    #[must_use]
    pub fn of_person(
        app_id: &'static str,
        app_title: &'static str,
        declares: &[super::Declare],
        records: ui_core::record::person::PersonRecords,
        device: Option<String>,
    ) -> Self {
        Self::declared(
            super::record::Sections {
                app_id,
                document: SettingsDocument::Session,
                person: Some((
                    Rc::new(super::person::PersonState::new(records, app_id, app_title)),
                    device,
                )),
                doors: Vec::new(),
            },
            declares,
        )
    }

    fn declared(sections: super::record::Sections, declares: &[super::Declare]) -> Self {
        let person = sections
            .person
            .as_ref()
            .map(|(person, _)| Rc::clone(person));
        let mut records = super::Records::sections(sections);
        for declare in declares {
            declare(&mut records);
        }
        Self {
            doors: records.into_sections(),
            person,
        }
    }

    /// The person's records the sections are kept in, each opened (made
    /// where it does not exist yet), as their routes sync them to the
    /// person's other devices; none where the sections are not kept in the
    /// person's records.
    #[must_use]
    pub fn history_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        let Some(person) = &self.person else {
            return Vec::new();
        };
        for door in &self.doors {
            door.hold();
        }
        person.owners()
    }

    /// The person's shared record `P` (their accounts, their contacts),
    /// held beside the sections and synced with them
    /// ([`Self::history_owners`]), opened on first use; not published here.
    /// `None` where the sections are not kept in the person's records.
    #[must_use]
    pub fn shared<P: tilo_resource::SharedRecord>(&self) -> Option<super::PersonRecord<P>> {
        self.person.as_ref()?.hold_shared::<P>()
    }

    /// Every section, published with its field edits and verbs, and the
    /// person's undo and redo of each kept in their records.
    #[must_use]
    pub fn resources(&self) -> Vec<tilo_actions::Resource> {
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

/// Take into `doc`, a document of the section's own, what was stored since
/// it last followed at `seen`. Whether it moved.
fn follow_doc(kind: &SectionKind, doc: &RefCell<Doc>, seen: &Cell<SettingsRevision>) -> bool {
    let revision = kind.revision();
    if revision == seen.get() {
        return false;
    }
    seen.set(revision);
    let mut doc = doc.borrow_mut();
    if kind.write(&mut doc).is_ok() {
        doc.commit();
    } else {
        doc.rollback();
    }
    true
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

/// The record kind a settings section of `kind` is kept in: one per device
/// for a device-local section. A record kind is a plain label: letters,
/// digits, `.`, `_`, `-`.
pub(crate) fn record_kind(kind: &str, device_local: bool, device: Option<&str>) -> String {
    match device.filter(|_| device_local) {
        Some(device) => {
            let device: String = device
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            format!("settings.{kind}.device-{device}")
        }
        None => format!("settings.{kind}"),
    }
}
