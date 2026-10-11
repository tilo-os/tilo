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
        storage.0.section(&self.settings)
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
    /// The settings sections views edit as records, by type.
    sections: RefCell<HashMap<TypeId, Rc<dyn Any>>>,
    /// Those sections, as the door reaches them.
    section_doors: RefCell<Vec<Rc<dyn RecordDoor>>>,
}

/// What the host gives a live window beside its record: the person's own
/// records and this device's id. Neither reaches a view of a past version.
#[derive(Default)]
pub(crate) struct Hosted {
    pub(crate) person: Option<ui_core::record::person::PersonRecords>,
    pub(crate) device: Option<String>,
}

/// [`Hosted`], with the person's records opened for one app.
pub(crate) struct Attached {
    person: Option<super::person::PersonState>,
    device: Option<String>,
}

impl Hosted {
    pub(crate) fn attach(self, app_id: &'static str, app_title: &'static str) -> Attached {
        Attached {
            person: self
                .person
                .map(|records| super::person::PersonState::new(records, app_id, app_title)),
            device: self.device,
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
            section_doors: RefCell::default(),
        })
    }

    pub(crate) fn storage(&self) -> &AppStorage {
        &self.storage
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

    /// The shared record of the person's `P`.
    pub(crate) fn shared_person<P: tilo_resource::SharedRecord>(
        &self,
    ) -> Option<super::person::PersonRecord<P>> {
        super::controls::opened::<P>();
        self.person.as_ref()?.open_shared::<P>()
    }

    /// The records other than the app's own the views opened — the
    /// person's records, settings sections — as published resources.
    pub(crate) fn record_resources(&self) -> Vec<tilo_actions::Resource> {
        let mut resources = self
            .person
            .as_ref()
            .map(super::person::PersonState::resources)
            .unwrap_or_default();
        resources.extend(
            self.section_doors
                .borrow()
                .iter()
                .map(|door| door.publish()),
        );
        resources
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
    pub(crate) fn section<T>(&self, settings: &Settings<T>) -> Option<Rc<Held<T>>>
    where
        T: tilo_resource::Resource<State = T> + tilo_resource::Fields + SettingsSchema,
    {
        super::controls::opened::<T>();
        if let Some(person) = &self.person {
            return person.open_section(settings.clone(), self.device());
        }
        if let Some(open) = self.sections.borrow().get(&TypeId::of::<T>()) {
            return Rc::clone(open).downcast::<Held<T>>().ok();
        }
        let held = super::record::section(settings.clone(), true)
            .map_err(|error| tracing::warn!(kind = T::KIND, %error, "settings section unavailable"))
            .ok()?;
        let held = Rc::new(held);
        self.sections
            .borrow_mut()
            .insert(TypeId::of::<T>(), Rc::clone(&held) as Rc<dyn Any>);
        self.section_doors
            .borrow_mut()
            .push(Rc::clone(&held) as Rc<dyn RecordDoor>);
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

    /// The routes of the person's records the views opened.
    pub(crate) fn person_owners(&self) -> Vec<ui_core::history_mode::HistoryOwner> {
        self.person
            .as_ref()
            .map(super::person::PersonState::owners)
            .unwrap_or_default()
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

    /// Read every followed setting whose document changed, dropping those
    /// of views that are gone.
    pub(crate) fn refresh(&self) {
        let _profile = tilo_resource::profiling::Phase::start("framework.view_storage_refresh");
        if let Some(person) = &self.person {
            person.refresh();
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
