//! An app's typed settings, files and secrets as views and surfaces reach
//! them: from their context, scoped to the app and the settings document its
//! host gave it.

use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use ui_core::LocalState;
use ui_core::storage::{AppStorage, Settings, SettingsRevision, SettingsSchema};

use super::record::{Held, RecordDoor};
use super::section::{SectionDoor, SectionKind};

/// One typed settings value, kept current while the view lives: a write
/// from this window, another window, the Settings app or another of the
/// person's devices (for [`Scope::Personal`](ui_core::storage::Scope))
/// reaches [`Self::state`] on the next frame.
///
/// Bind controls to [`Self::state`] and write with [`Self::set`]. Settings
/// are not the document: they are not undone and never seen by
/// collaborators.
///
/// A section that is a resource model (`#[derive(Model)]` and an empty
/// `#[resource]`) is also a record its views edit by path
/// ([`ViewCx::record`](super::ViewCx::record)): its field edits and verbs
/// are published as `settings:<kind>`, each change is checked by the
/// model's invariants and recorded in the settings document with what made
/// it and who.
pub struct Setting<T> {
    settings: Settings<T>,
    value: LocalState<T>,
}

impl<T> Clone for Setting<T> {
    fn clone(&self) -> Self {
        Self {
            settings: self.settings.clone(),
            value: self.value.clone(),
        }
    }
}

impl<T: SettingsSchema + Clone + PartialEq + Send + Sync> Setting<T> {
    /// The current value.
    #[must_use]
    pub fn get(&self) -> T {
        self.value.with(Clone::clone)
    }

    /// The value as state that follows every write.
    #[must_use]
    pub fn state(&self) -> LocalState<T> {
        self.value.clone()
    }

    /// Store `value`.
    ///
    /// # Errors
    ///
    /// When the document refuses it (see [`Settings::set`]); the value shown
    /// stays the stored one.
    pub fn set(&self, value: T) -> std::io::Result<()> {
        self.settings.set(&value)?;
        self.value.set(value);
        Ok(())
    }

    /// Change the stored value with `change`.
    ///
    /// # Errors
    ///
    /// As [`Self::set`].
    pub fn update(&self, change: impl FnOnce(&mut T)) -> std::io::Result<()> {
        let value = self.settings.update(change)?;
        self.value.set(value);
        Ok(())
    }

    /// The underlying settings, for code that runs off the view (a job).
    #[must_use]
    pub fn settings(&self) -> &Settings<T> {
        &self.settings
    }
}

type Refresh = Box<dyn FnMut()>;

impl<T> super::record::sealed::Hold<T> for Setting<T>
where
    T: tilo_resource::Resource<State = T>
        + tilo_resource::Fields
        + SettingsSchema
        + Clone
        + PartialEq
        + Send
        + Sync,
{
    fn held(&self, storage: super::record::sealed::Storage<'_>) -> Option<Rc<Held<T>>> {
        storage.0.section_held(&self.settings)
    }

    fn door(
        &self,
        storage: super::record::sealed::Storage<'_>,
    ) -> Option<super::record::sealed::Door> {
        storage
            .0
            .section(&self.settings)
            .map(|door| super::record::sealed::Door(door as Rc<dyn RecordDoor>))
    }
}

/// The app's storage and the settings its views follow. Shared by every
/// scope of one root; the root refreshes it each sync.
pub(crate) struct ViewStorage {
    storage: AppStorage,
    followed: RefCell<Vec<(Weak<()>, Refresh)>>,
    /// The person's own records, when the host keeps them: not in a view of
    /// a past version, which shows the record it is a version of.
    person: Option<super::person::PersonState>,
    /// This device, in a live window.
    device: Option<String>,
    /// The settings sections views and surfaces edit as records, by kind.
    sections: RefCell<HashMap<&'static str, Rc<SectionDoor>>>,
    /// Those a view part edits through its type, by type.
    held_sections: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
    /// Those sections, as the door reaches them.
    section_doors: RefCell<Vec<Rc<dyn RecordDoor>>>,
    /// The records another provider keeps, by id, with that provider
    /// ([`Records::kept_by`](super::Records::kept_by)): views show them and
    /// bind to them, the window neither publishes nor runs their actions.
    kept_by: RefCell<HashMap<String, &'static str>>,
    /// The store of the app's records in the room, for the records it
    /// keeps beside its own: not in a view of a past version.
    room: Option<super::chain::RoomRecords>,
    /// The chains the views opened ([`super::Chain`]).
    chains: super::chain::Chains,
    /// The records every app of the room opens, when the host keeps them:
    /// not in a view of a past version.
    workspace: Option<Rc<super::workspace::WorkspaceState>>,
    /// The workspace's emoji, as the window's views draw them.
    emoji: Option<Rc<super::workspace_emoji::WindowEmoji>>,
}

/// What the host gives a live window beside its record: the person's own
/// records and this device's id. Neither reaches a view of a past version.
#[derive(Default)]
pub(crate) struct Hosted {
    pub(crate) person: Option<ui_core::record::person::PersonRecords>,
    pub(crate) device: Option<String>,
    /// The store of the app's records in the room, who edits there and
    /// what they may do.
    pub(crate) room: Option<(
        ui_core::record::RecordStore,
        String,
        ui_core::permissions::LivePermissions,
    )>,
    /// The records every app of the room opens, and the window's wake
    /// scope, against which the workspace's emoji are registered.
    pub(crate) workspace: Option<(
        ui_core::record::workspace::WorkspaceRecords,
        ui_core::wake::WakeHandle,
    )>,
}

/// [`Hosted`], with the person's records opened for one app.
pub(crate) struct Attached {
    person: Option<super::person::PersonState>,
    device: Option<String>,
    room: Option<super::chain::RoomRecords>,
    workspace: Option<Rc<super::workspace::WorkspaceState>>,
    emoji: Option<Rc<super::workspace_emoji::WindowEmoji>>,
}

impl Hosted {
    pub(crate) fn attach(self, app_id: &'static str, app_title: &'static str) -> Attached {
        let permissions = self
            .room
            .as_ref()
            .map(|(_, _, permissions)| permissions.clone());
        let workspace = self.workspace.map(|(records, wake)| {
            let state = Rc::new(super::workspace::WorkspaceState::new(
                records,
                app_id,
                app_title,
                permissions,
                Some(wake.clone()),
            ));
            let emoji = super::workspace_emoji::register(&wake, Rc::clone(&state));
            (state, emoji)
        });
        let (workspace, emoji) = workspace.unzip();
        Attached {
            workspace,
            emoji,
            person: self
                .person
                .map(|records| super::person::PersonState::new(records, app_id, app_title)),
            device: self.device,
            room: self
                .room
                .map(|(store, me, permissions)| super::chain::RoomRecords {
                    store,
                    me,
                    permissions: Some(permissions),
                    app_id,
                    app_title,
                }),
        }
    }
}

impl ViewStorage {
    pub(crate) fn new(storage: AppStorage, hosted: Attached) -> Rc<Self> {
        Rc::new(Self {
            storage,
            followed: RefCell::default(),
            person: hosted.person,
            device: hosted.device,
            sections: RefCell::default(),
            held_sections: RefCell::default(),
            section_doors: RefCell::default(),
            kept_by: RefCell::default(),
            room: hosted.room,
            chains: super::chain::Chains::default(),
            workspace: hosted.workspace,
            emoji: hosted.emoji,
        })
    }

    /// The chain of `P` the app keeps beside its record, opened on first
    /// use, its actions reading the app's record from `base`. `None` where
    /// the window keeps no records in a room (a view of a past version).
    pub(crate) fn chain<P: super::Chained>(
        &self,
        base: impl Fn() -> Rc<<P::Base as tilo_resource::Resource>::State> + 'static,
    ) -> Option<Rc<super::chain::ChainState<P>>> {
        super::controls::opened_in_room::<P>();
        self.chains.open::<P>(self.room.as_ref(), base)
    }

    /// The children of `C` the parent lists, held on first use, the parent
    /// read from `parent`. `None` where the window keeps no records in a
    /// room (a view of a past version).
    pub(crate) fn children<C: super::children::ChildRecord>(
        &self,
        parent: impl Fn() -> Rc<<C::Parent as tilo_resource::Resource>::State> + 'static,
    ) -> Option<Rc<super::children::ChildrenState<C>>> {
        super::controls::opened_in_room::<C>();
        self.chains.open_children::<C>(self.room.as_ref(), parent)
    }

    /// Answer one published action of a chain the views opened, invoked by
    /// someone the window knows as `me`.
    pub(crate) fn chain_action(
        &self,
        me: Option<&str>,
        payload: &[u8],
        live: &mut ui_core::widget::Edits<'_>,
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.chains.answer(me, payload, live)
    }

    pub(crate) fn storage(&self) -> &AppStorage {
        &self.storage
    }

    /// The store of the app's records in the workspace; `None` in a view of
    /// a past version.
    pub(crate) fn room_store(&self) -> Option<&ui_core::record::RecordStore> {
        self.room.as_ref().map(|room| &room.store)
    }

    /// This device's id, in a live window.
    pub(crate) fn device(&self) -> Option<&str> {
        self.device.as_deref()
    }

    /// The person's record of `P`, opened on first use.
    pub(crate) fn person<P: tilo_resource::Resource>(
        &self,
    ) -> Option<super::person::PersonRecord<P>> {
        super::controls::opened::<P>();
        self.person.as_ref()?.open::<P>()
    }

    /// Open the record `P` every app of the room opens that this app
    /// keeps.
    pub(crate) fn workspace_record<P: tilo_resource::Resource>(&self) {
        if let Some(workspace) = &self.workspace {
            let _ = workspace.open::<P>(workspace.app_id());
        }
    }

    /// The record `P` every app of the room opens that `owner` keeps,
    /// opened on first use.
    pub(crate) fn workspace<P: tilo_resource::Resource>(
        &self,
        owner: &'static str,
    ) -> Option<super::workspace::WorkspaceRecord<P>> {
        self.workspace.as_ref()?.open::<P>(owner)
    }

    /// The shared record of the person's `P`.
    pub(crate) fn shared_person<P: tilo_resource::SharedRecord>(
        &self,
    ) -> Option<super::person::PersonRecord<P>> {
        super::controls::opened::<P>();
        self.person.as_ref()?.open_shared::<P>()
    }

    /// The records other than the app's own the views opened — the
    /// person's records, settings sections — as published resources.
    /// What [`Self::record_resources`] publishes depends on: each record's
    /// id and revision, in publication order.
    pub(crate) fn records_key(&self) -> Vec<(String, u64)> {
        let mut doors = self
            .person
            .as_ref()
            .map(super::person::PersonState::doors)
            .unwrap_or_default();
        doors.extend(self.section_doors.borrow().iter().cloned());
        doors.extend(self.chains.doors());
        doors
            .iter()
            .map(|door| (door.id().to_owned(), door.revision()))
            .collect()
    }

    /// The person's records a view asked for and could not open, as they
    /// are published: their declared actions, unavailable for why.
    pub(crate) fn unavailable_records(&self) -> Vec<tilo_actions::Resource> {
        self.person
            .as_ref()
            .map(super::person::PersonState::unavailable)
            .unwrap_or_default()
    }

    pub(crate) fn record_resources(&self) -> Vec<tilo_actions::Resource> {
        self.record_resources_unless(&|_, _| false)
            .into_iter()
            .flatten()
            .collect()
    }

    /// [`Self::record_resources`], one entry per record of
    /// [`Self::records_key`] in its order, `None` for each whose id and
    /// revision `kept` says the caller already holds as published: a record
    /// that did not move is not published again.
    pub(crate) fn record_resources_unless(
        &self,
        kept: &dyn Fn(&str, u64) -> bool,
    ) -> Vec<Option<tilo_actions::Resource>> {
        let publish =
            |door: &Rc<dyn RecordDoor>| (!kept(door.id(), door.revision())).then(|| door.publish());
        let mut resources = self
            .person
            .as_ref()
            .map(|person| person.resources_unless(kept))
            .unwrap_or_default();
        resources.extend(self.section_doors.borrow().iter().map(publish));
        resources.extend(self.chains.doors().iter().map(publish));
        let kept_by = self.kept_by.borrow();
        if !kept_by.is_empty() {
            for resource in resources.iter_mut().flatten() {
                if let Some(provider) = kept_by.get(resource.id.as_str()) {
                    run_by(resource, provider);
                }
            }
        }
        resources
    }

    /// Mark the record `id` as kept by `provider`
    /// ([`Records::kept_by`](super::Records::kept_by)).
    pub(crate) fn keep_by(&self, id: &str, provider: &'static str) {
        self.kept_by.borrow_mut().insert(id.to_owned(), provider);
    }

    /// The provider keeping the record `id`, when another one does.
    pub(crate) fn kept_provider(&self, id: &str) -> Option<&'static str> {
        self.kept_by.borrow().get(id).copied()
    }

    /// Answer one published action of those records, invoked by someone
    /// the window knows as `me`.
    pub(crate) fn record_action(
        &self,
        me: Option<&str>,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, String> {
        let mut doors = self
            .person
            .as_ref()
            .map(super::person::PersonState::doors)
            .unwrap_or_default();
        doors.extend(self.section_doors.borrow().iter().cloned());
        // Another provider runs what it keeps; the window never does.
        let kept_by = self.kept_by.borrow();
        doors.retain(|door| !kept_by.contains_key(door.id()));
        let me = self
            .person
            .as_ref()
            .map(super::person::PersonState::me)
            .or(me);
        super::record::answer(&doors, me, payload)
    }

    /// The settings section `T` held as a record, opened on first use: a
    /// record of the person's, whose edits they undo and redo, or in a
    /// window that keeps nothing of theirs (a private one) one that lives as
    /// long as the window.
    pub(crate) fn section<T>(&self, settings: &Settings<T>) -> Option<Rc<SectionDoor>>
    where
        T: tilo_resource::Resource<State = T> + tilo_resource::Fields + SettingsSchema,
    {
        super::controls::opened::<T>();
        if let Some(open) = self.sections.borrow().get(T::KIND) {
            return Some(Rc::clone(open));
        }
        self.open_section(SectionKind::of(settings))
    }

    /// [`Self::section`] of the section `kind` describes.
    fn open_section(&self, kind: Rc<SectionKind>) -> Option<Rc<SectionDoor>> {
        let name = kind.kind;
        let door = if let Some(person) = &self.person {
            person.open_section(kind, self.device())?
        } else {
            let door = SectionDoor::window(kind, true)
                .map_err(
                    |error| tracing::warn!(kind = name, %error, "settings section unavailable"),
                )
                .ok()?;
            let door = Rc::new(door);
            self.section_doors
                .borrow_mut()
                .push(Rc::clone(&door) as Rc<dyn RecordDoor>);
            door
        };
        self.sections.borrow_mut().insert(name, Rc::clone(&door));
        Some(door)
    }

    /// The settings section `T` as a record a view part edits through its
    /// type ([`ViewCx::record`](super::ViewCx::record)).
    pub(crate) fn section_held<T>(&self, settings: &Settings<T>) -> Option<Rc<Held<T>>>
    where
        T: tilo_resource::Resource<State = T> + tilo_resource::Fields + SettingsSchema,
    {
        if let Some(held) = self.held_sections.borrow().get(&TypeId::of::<T>()) {
            return Rc::clone(held).downcast::<Held<T>>().ok();
        }
        let held = self
            .section(settings)?
            .held::<T>()
            .map_err(|error| tracing::warn!(kind = T::KIND, %error, "settings section unavailable"))
            .ok()?;
        let held = Rc::new(held);
        self.held_sections
            .borrow_mut()
            .insert(TypeId::of::<T>(), Rc::clone(&held) as Rc<dyn Any>);
        Some(held)
    }

    /// Run `refresh` with every refresh while `alive` lives.
    pub(crate) fn follow_with(&self, alive: Weak<()>, refresh: impl FnMut() + 'static) {
        self.followed.borrow_mut().push((alive, Box::new(refresh)));
    }

    /// Answer an undo or redo of one of the person's records.
    pub(crate) fn person_history(
        &self,
        payload: &[u8],
    ) -> Result<tilo_actions::ActionOutput, String> {
        self.person
            .as_ref()
            .ok_or_else(|| "this window keeps no records of yours".to_owned())?
            .answer(payload)
    }

    /// The routes of the person's records the views opened, and of the
    /// records every app of the room opens.
    pub(crate) fn person_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        let mut owners = self
            .person
            .as_ref()
            .map(super::person::PersonState::owners)
            .unwrap_or_default();
        if let Some(workspace) = &self.workspace {
            owners.extend(workspace.owners());
        }
        owners
    }

    /// `T`, followed for as long as `alive` is.
    pub(crate) fn follow<T>(&self, alive: Weak<()>) -> Setting<T>
    where
        T: SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        let settings = self.storage.settings::<T>();
        let mut seen: SettingsRevision = settings.revision();
        let value = LocalState::new(settings.get());
        let setting = Setting {
            settings: settings.clone(),
            value: value.clone(),
        };
        self.followed.borrow_mut().push((
            alive,
            Box::new(move || {
                let revision = settings.revision();
                if revision == seen {
                    return;
                }
                seen = revision;
                let stored = settings.get();
                if value.with(|shown| *shown != stored) {
                    value.set(stored);
                }
            }),
        ));
        setting
    }

    /// Take up a newer head of each chain that arrived on this device: in
    /// a frame's settle, where the store takes the writer opening it needs,
    /// so the views' next sync reads it.
    pub(crate) fn settle(&self) {
        for chain in self.chains.doors() {
            let _ = chain.revision();
        }
    }

    /// Read every followed setting whose document changed, dropping those
    /// of views that are gone.
    pub(crate) fn refresh(&self) {
        let _profile = tilo_resource::profiling::Phase::start("framework.view_storage_refresh");
        if let Some(person) = &self.person {
            person.refresh();
        }
        if let Some(workspace) = &self.workspace {
            workspace.refresh();
        }
        if let Some(emoji) = &self.emoji {
            emoji.settle();
        }
        for chain in self.chains.doors() {
            let _ = chain.revision();
        }
        // Take the list so a refresh that builds a view (a switch reacting
        // to the new value) may follow more without a reentrant borrow.
        let mut current = std::mem::take(&mut *self.followed.borrow_mut());
        current.retain_mut(|(alive, refresh)| {
            if alive.strong_count() == 0 {
                return false;
            }
            refresh();
            true
        });
        let mut list = self.followed.borrow_mut();
        current.append(&mut list);
        *list = current;
    }
}

/// `resource`'s actions as `provider` runs them, its parts' too.
fn run_by(resource: &mut tilo_actions::Resource, provider: &'static str) {
    resource.actions = std::mem::take(&mut resource.actions)
        .into_iter()
        .map(|spec| spec.run_by(provider))
        .collect();
    for child in &mut resource.children {
        run_by(child, provider);
    }
}
