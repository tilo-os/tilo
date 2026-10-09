//! What every pane reaches of its host: the settings it edits, the door its
//! controls go through, and what it is built for.
//!
//! A pane reads the person's and the device's preferences from the app's
//! settings document ([`PaneCx::settings`]): the session's records in a
//! window someone uses, a private profile's document in a test or a
//! screenshot. It never writes them itself: a control is bound to a
//! [`SettingsEffect`] ([`PaneCx::bind`]), and the effect's answer writes.

use std::rc::Rc;

use ui::LocalState;
use ui::framework::{ActionControl, Actions, Dispatcher, EffectAnswer, Effected, Refused, RunMode};
use ui::storage::{AppStorage, Settings as StoredSettings, SettingsRevision, SettingsSchema};
use ui::tilo_resource::{Binds, Effect};

use crate::effects::SettingsEffect;
use crate::resource::Settings;

/// The door a hosted pane's controls go through.
#[derive(Clone)]
struct Door {
    actions: Actions<Settings>,
    dispatcher: Dispatcher<Settings>,
}

/// What a pane reaches of its host. Cheap to clone; clones share the door.
#[derive(Clone)]
pub struct PaneCx {
    door: Option<Door>,
    storage: AppStorage,
    mode: RunMode,
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
}

impl PaneCx {
    /// A pane on its own, outside any host, reading `storage`: its controls
    /// are drawn and laid out but go nowhere, and like a screenshot it
    /// reaches no device.
    #[must_use]
    pub fn detached(storage: AppStorage) -> Self {
        Self {
            door: None,
            storage,
            mode: RunMode::Capture,
            scene: None,
        }
    }

    /// A detached pane over the settings document at `path`, for a test.
    #[cfg(test)]
    pub(crate) fn in_file(path: std::path::PathBuf) -> Self {
        let id = <crate::resource::SettingsShell as ui::framework::ResourceApp>::ID;
        Self::detached(AppStorage::new(
            id,
            ui::storage::secrets(id),
            ui::storage::SettingsDocument::File(path),
        ))
    }

    /// The panes of the surface attached through `cx`.
    pub(crate) fn attached(cx: &ui::framework::SurfaceCx<Settings>) -> Self {
        Self {
            door: Some(Door {
                actions: cx.actions(),
                dispatcher: cx.dispatcher(),
            }),
            storage: cx.storage(),
            mode: cx.mode(),
            scene: cx.fixture::<crate::scenarios::CaptureScene>(),
        }
    }

    /// The app's storage: its settings document, files and secrets.
    #[must_use]
    pub fn storage(&self) -> &AppStorage {
        &self.storage
    }

    /// The typed preferences `T` in the app's settings document.
    #[must_use]
    pub fn settings<T: SettingsSchema>(&self) -> StoredSettings<T> {
        self.storage.settings::<T>()
    }

    /// Change the stored preferences `T` with `change`: what a preference
    /// effect answers with, its caller told when the document refuses it.
    ///
    /// # Errors
    ///
    /// Why the change could not be stored.
    pub fn update<T: SettingsSchema>(&self, change: impl FnOnce(&mut T)) -> EffectAnswer {
        self.settings::<T>()
            .update(change)
            .map(|_| Effected::done())
            .map_err(|error| format!("the setting could not be saved: {error}"))
    }

    /// `T`, read again whenever the settings document changes.
    #[must_use]
    pub fn follow<T: SettingsSchema + Clone + PartialEq>(&self) -> Followed<T> {
        Followed::new(self.settings::<T>())
    }

    /// What the panes are built for: a window someone uses, or a
    /// screenshot that must not reach devices, networks or clocks.
    #[must_use]
    pub fn mode(&self) -> RunMode {
        self.mode
    }

    /// Whether panes may start work that reaches outside the process.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.mode.is_live()
    }

    /// Whether the areas probe and drive this device's hardware and
    /// services, rather than the stand-ins a screenshot shows. The crate's
    /// own tests drive the stand-ins too, so they never touch the machine
    /// running them.
    #[must_use]
    pub fn reaches_devices(&self) -> bool {
        self.is_live() && !cfg!(test)
    }

    /// The stand-ins a screenshot shows in place of this device's hardware
    /// and accounts; `None` in a live window.
    #[must_use]
    pub fn scene(&self) -> Option<&crate::scenarios::CaptureScene> {
        self.scene.as_deref()
    }

    /// `control`, invoking `effect` with each committed value; disabled
    /// when the pane is outside a host.
    #[must_use]
    pub fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        effect: impl Binds<SettingsEffect, I>,
    ) -> C {
        match &self.door {
            Some(door) if Self::may_effect(door, effect.action_id()) => {
                door.actions.bind_effect(control, effect)
            }
            Some(_) | None => control.unavailable(),
        }
    }

    /// Bind to a published item operation rather than exporting a row effect
    /// on the root resource. Bound item arguments remain inside the spec.
    pub(crate) fn row_binding<I: 'static>(
        &self,
        row: &ui::tilo_actions::Resource,
        effect: impl Binds<SettingsEffect, I>,
    ) -> Option<ui::tilo_actions::ActionBinding<I>> {
        let door = self.door.as_ref()?;
        let spec = row
            .actions
            .iter()
            .find(|spec| spec.id == effect.action_id())?;
        if !door.dispatcher.may(spec.contract.permission) {
            return None;
        }
        let asked: Vec<String> = spec
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect();
        ui::tilo_actions::ActionBinding::fallible(row, spec, move |input| {
            let mut args = effect.arguments(input)?;
            args.retain(|name, _| asked.contains(name));
            Ok(args)
        })
        .ok()
    }

    /// The binding of `effect` for a control the toolkit does not know: a
    /// pane's own widget that emits a value. `None` outside a host.
    #[must_use]
    pub fn binding<I: 'static>(
        &self,
        effect: impl Binds<SettingsEffect, I>,
    ) -> Option<ui::tilo_actions::ActionBinding<I>> {
        self.door
            .as_ref()
            .filter(|door| Self::may_effect(door, effect.action_id()))
            .and_then(|door| door.actions.effect_binding(effect))
    }

    /// `control` (a button), invoking one fully specified effect.
    #[must_use]
    pub fn bind_invocation<C: ActionControl<()>>(
        &self,
        control: C,
        effect: impl Into<SettingsEffect>,
    ) -> C {
        let effect = effect.into();
        match &self.door {
            Some(door) if Self::may_effect(door, effect.id()) => {
                door.actions.bind_effect_invocation(control, &effect)
            }
            Some(_) | None => control.unavailable(),
        }
    }

    fn may_effect(door: &Door, id: &str) -> bool {
        SettingsEffect::declarations()
            .into_iter()
            .find(|declaration| declaration.id == id)
            .is_some_and(|declaration| door.dispatcher.may(declaration.permission))
    }

    /// The binding of one fully specified effect, for a shared navigation row.
    #[must_use]
    pub fn invocation_binding(
        &self,
        effect: impl Into<SettingsEffect>,
    ) -> Option<ui::tilo_actions::ActionBinding<()>> {
        let effect = effect.into();
        self.door
            .as_ref()
            .filter(|door| Self::may_effect(door, effect.id()))
            .and_then(|door| door.actions.effect(&effect))
    }

    /// Invoke `effect` through the same door as a bound control.
    pub fn invoke(&self, effect: impl Into<SettingsEffect>) {
        if let Some(door) = &self.door {
            door.dispatcher.effect(&effect.into());
        }
    }

    /// The last effect the door refused and why, for a pane to show beside
    /// the control that invoked it.
    #[must_use]
    pub fn refused(&self) -> LocalState<Option<Refused>> {
        self.door
            .as_ref()
            .map_or_else(|| LocalState::new(None), |door| door.dispatcher.refused())
    }
}

/// A typed preference a pane shows, read again when its document changes.
pub struct Followed<T> {
    settings: StoredSettings<T>,
    seen: SettingsRevision,
    value: T,
}

impl<T: SettingsSchema + Clone + PartialEq> Followed<T> {
    fn new(settings: StoredSettings<T>) -> Self {
        let seen = settings.revision();
        let value = settings.get();
        Self {
            settings,
            seen,
            value,
        }
    }

    /// The value as last read.
    #[must_use]
    pub fn get(&self) -> &T {
        &self.value
    }

    /// Read the value again if the document changed since; whether the
    /// value changed.
    pub fn refresh(&mut self) -> bool {
        let revision = self.settings.revision();
        if revision == self.seen {
            return false;
        }
        self.seen = revision;
        let stored = self.settings.get();
        if stored == self.value {
            return false;
        }
        self.value = stored;
        true
    }

    /// The stored settings, for the effect that writes them.
    #[must_use]
    pub fn settings(&self) -> &StoredSettings<T> {
        &self.settings
    }
}
