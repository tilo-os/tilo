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
    ChangeOrigin, Settings, SettingsDocument, SettingsRevision, SettingsSchema,
};

use super::live::Live;
use super::record::{Held, RecordDoor};

/// One thing a section's record asks of the section's type.
enum SectionOp<'a> {
    /// Make the document one of the section's model.
    Initialize(&'a mut Doc),
    /// Write the stored value into the document's fields.
    Write(&'a mut Doc),
    /// Store the value the document's fields hold, as the origin did.
    Store(&'a Doc, &'a ChangeOrigin),
    /// Whether the document's fields hold the stored value.
    HoldsStored(&'a Doc),
    /// The section the document holds, published under an id and a label.
    Publish(&'a Doc, &'a str, &'a str),
    /// The section's action an encoded one is.
    Decode(&'a EncodedAction),
}

/// What a [`SectionOp`] answers.
enum SectionOut {
    Done,
    Holds(bool),
    Published(Box<tilo_actions::Resource>),
    Decoded(Box<dyn Action>),
}

/// What a section's record does with the section's type: one function per
/// section type ([`SectionOp`]), over the section's settings
/// ([`Self::settings`]).
pub(crate) struct SectionKind {
    pub(crate) kind: &'static str,
    pub(crate) label: &'static str,
    /// The section keeps a value per device.
    pub(crate) device_local: bool,
    /// The type id the section's actions are encoded under.
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

    /// Store what `doc` holds as the action `action` by `by` changed it.
    fn store(&self, doc: &Doc, action: &str, by: &str) -> Result<(), String> {
        let origin = ChangeOrigin::action(format!("{}/{action}", self.kind)).by(by.to_owned());
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
    })
}

/// The record a section is kept in.
enum Kept {
    /// A document of the window's own, which lives as long as the window: in
    /// a window that keeps nothing of the person's (a private one).
    Window {
        doc: Box<RefCell<Doc>>,
        /// The settings document's version the record last followed.
        seen: Cell<SettingsRevision>,
    },
    /// A record of the person's: its edits are their steps, undone and
    /// redone like any of theirs.
    Person {
        record: Box<StoredRecord>,
        /// The settings document's version the record last followed.
        seen: Cell<Option<SettingsRevision>>,
        /// The record's heads when last read.
        heads: RefCell<Vec<ui_core::automerge::ChangeHash>>,
        app_id: &'static str,
        app_title: &'static str,
        me: String,
    },
}

/// A settings section held as a record, whatever its type.
pub(crate) struct SectionDoor {
    id: String,
    kind: Rc<SectionKind>,
    editable: bool,
    kept: Kept,
    /// Increases whenever what the record holds may have changed.
    revision: Cell<u64>,
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
        })
    }

    /// The section kept in the person's record `record`.
    pub(crate) fn person(
        kind: Rc<SectionKind>,
        record: StoredRecord,
        app_id: &'static str,
        app_title: &'static str,
        me: String,
    ) -> Self {
        let door = Self {
            id: format!("settings:{}", kind.kind),
            editable: true,
            kept: Kept::Person {
                heads: RefCell::new(record.heads()),
                record: Box::new(record),
                seen: Cell::new(None),
                app_id,
                app_title,
                me,
            },
            kind,
            revision: Cell::new(0),
        };
        door.follow();
        door
    }

    pub(crate) fn label(&self) -> &'static str {
        self.kind.label
    }

    fn moved(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }

    /// Take into the record what was stored elsewhere (another window, the
    /// Settings app, another device): content from outside, nobody's step.
    /// Whether the record moved since it was last read.
    pub(crate) fn follow(&self) -> bool {
        let moved = match &self.kept {
            Kept::Window { doc, seen } => {
                let revision = self.kind.revision();
                if revision == seen.get() {
                    false
                } else {
                    seen.set(revision);
                    let mut doc = doc.borrow_mut();
                    if self.kind.write(&mut doc).is_ok() {
                        doc.commit();
                    } else {
                        doc.rollback();
                    }
                    true
                }
            }
            Kept::Person {
                record,
                seen,
                heads,
                ..
            } => {
                let revision = self.kind.revision();
                if seen.get() != Some(revision) {
                    seen.set(Some(revision));
                    let holds = record
                        .read(|doc| self.kind.holds_stored(doc))
                        .unwrap_or(false);
                    if !holds && let Err(error) = record.edit(false, |doc| self.kind.write(doc)) {
                        tracing::warn!(kind = self.kind.kind, %error, "settings record did not follow the section");
                    }
                }
                let now = record.heads();
                let moved = *heads.borrow() != now;
                *heads.borrow_mut() = now;
                moved
            }
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
            Kept::Person { record, .. } => record.read(read),
        }
    }

    /// Apply the encoded action `encoded` for `invoker`, store the result as
    /// what made it, and take it back where the store refuses it.
    fn apply(&self, encoded: &EncodedAction, invoker: &ActionInvoker) -> Result<(), ActionError> {
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
                        .store(&doc, &id, &invoker.actor)
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
                record,
                seen,
                app_id,
                app_title,
                me,
                ..
            } => {
                let change = action.change_kind();
                let context =
                    ActionContext::for_resource(self.kind.kind, self.kind.kind, self.kind.label)
                        .with_app(*app_id, *app_title)
                        .with_invoker(invoker.clone())
                        .with_change(change);
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
                    })
                    .map_err(ActionError::Refused)?;
                self.write_person(record, seen, &id, &invoker.actor);
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
        let Some(written) = record.read(|doc| self.kind.store(doc, action, by)) else {
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
        let Kept::Person { record, seen, .. } = &self.kept else {
            return false;
        };
        let stepped = if undo { record.undo() } else { record.redo() };
        if stepped {
            self.write_person(record, seen, if undo { "undo" } else { "redo" }, by);
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
                applying.apply(&tilo_resource::encode::<T>(action)?, invoker)
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
        let published = self
            .read(|doc| self.kind.publish(doc, &self.id))
            .unwrap_or_else(|| Err("the section's record is unavailable".into()))
            .unwrap_or_else(|error| {
                tracing::warn!(kind = self.kind.kind, %error, "settings section unreadable");
                tilo_actions::Resource::new(self.id.as_str(), self.kind.kind, self.kind.label)
            });
        super::record::route(published, &self.id, self.editable)
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
