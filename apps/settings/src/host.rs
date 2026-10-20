//! What every pane reaches of its host: the settings it edits, the door its
//! controls go through, and what it is built for.
//!
//! A pane reads the person's and the device's preferences from the app's
//! settings document ([`PaneCx::settings`]): the session's records in a
//! window someone uses, a private profile's document in a test or a
//! screenshot. It never writes them itself: a control is bound to a
//! [`SettingsEffect`] ([`PaneCx::bind`]), and the effect's answer writes.

use std::any::{Any, TypeId};
use std::collections::HashMap;
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
    /// The published field edits of each settings section, by its type,
    /// which its live rows offer too ([`Actions::operation`]).
    sections: Rc<HashMap<TypeId, Box<dyn Any>>>,
}

/// A settings section a pane binds its controls to by path: the person's
/// own record, published as `settings:<kind>`.
pub trait Section:
    SettingsSchema
    + ui::tilo_resource::Resource<State = Self>
    + ui::tilo_resource::Fields
    + Clone
    + PartialEq
    + Send
    + Sync
{
}

impl<T> Section for T where
    T: SettingsSchema
        + ui::tilo_resource::Resource<State = T>
        + ui::tilo_resource::Fields
        + Clone
        + PartialEq
        + Send
        + Sync
{
}

/// The published actions of section `T`, held for the panes.
fn held<T: Section>(
    cx: &ui::framework::SurfaceCx<Settings>,
    sections: &mut HashMap<TypeId, Box<dyn Any>>,
) {
    let section = cx.settings::<T>();
    if let Some(actions) = cx.record_actions(&section) {
        sections.insert(TypeId::of::<T>(), Box::new(actions));
    }
}

/// Every section's published actions, for the panes of the surface attached
/// through `cx`.
fn sections(cx: &ui::framework::SurfaceCx<Settings>) -> HashMap<TypeId, Box<dyn Any>> {
    let mut sections = HashMap::new();
    held::<crate::prefs::AppearancePrefs>(cx, &mut sections);
    held::<crate::accessibility_settings::AccessibilitySettings>(cx, &mut sections);
    held::<crate::prefs::InputPrefs>(cx, &mut sections);
    held::<ui::text_assistance::TextAssistancePrefs>(cx, &mut sections);
    held::<crate::prefs::PowerPrefs>(cx, &mut sections);
    held::<crate::prefs::DeskWidgetsPrefs>(cx, &mut sections);
    held::<crate::prefs::UpdatePrefs>(cx, &mut sections);
    held::<crate::prefs::NotificationPrefs>(cx, &mut sections);
    held::<crate::prefs::AudioPrefs>(cx, &mut sections);
    held::<crate::prefs::DisplayPrefs>(cx, &mut sections);
    held::<tilo_locale::Locale>(cx, &mut sections);
    held::<crate::continuity::ContinuityPrivacy>(cx, &mut sections);
    held::<crate::panes::keyboard_shortcuts::DesktopControls>(cx, &mut sections);
    held::<crate::panes::agent_models::AgentProviders>(cx, &mut sections);
    held::<crate::panes::personal_apps::DefaultApps>(cx, &mut sections);
    held::<crate::panes::display::shared_displays::SharedDisplayPrefs>(cx, &mut sections);
    held::<crate::panes::display::shared_displays::AutomaticDisplayPrefs>(cx, &mut sections);
    held::<crate::panes::mouse_keyboard::keyboard_editor::OskSourceSettings>(cx, &mut sections);
    sections
}

/// What a pane reaches of its host. Cheap to clone; clones share the door.
#[derive(Clone)]
pub struct PaneCx {
    door: Option<Door>,
    storage: AppStorage,
    mode: RunMode,
    scene: Option<Rc<crate::scenarios::CaptureScene>>,
    /// The radios this device has; never read when it reaches no device.
    radios: crate::radios::Radios,
    pub(crate) provider_admin: ui::models::ProviderAdministration,
    /// The person's accounts with the services the room is connected to,
    /// when the host lent them.
    pub(crate) linked: ui::linked_accounts::LinkedAccounts,
}

/// Retype portable panel bindings without changing public Settings action ids.
struct ProviderBinding<B>(B);
impl<I, B: ui::tilo_resource::Binds<provider_settings_ui::ProviderEffect, I>>
    ui::tilo_resource::Binds<SettingsEffect, I> for ProviderBinding<B>
{
    fn action_id(&self) -> &'static str {
        self.0.action_id()
    }
    fn arguments(&self, input: I) -> Result<serde_json::Map<String, serde_json::Value>, String> {
        self.0.arguments(input)
    }
}
impl provider_settings_ui::PanelBindings for PaneCx {
    fn offline_metadata(&self) -> bool {
        true
    }
    fn bind<I: 'static, C: ActionControl<I>>(
        &self,
        control: C,
        binding: impl ui::tilo_resource::Binds<provider_settings_ui::ProviderEffect, I>,
    ) -> C {
        self.bind(control, ProviderBinding(binding))
    }
    fn bind_invocation<C: ActionControl<()>>(
        &self,
        control: C,
        effect: provider_settings_ui::ProviderEffect,
    ) -> C {
        use ui::tilo_resource::Effect;
        match crate::panes::agent_models::AgentModelsEffect::decode(
            effect.id(),
            &effect.arguments(),
        ) {
            Ok(effect) => self.bind_invocation(control, effect),
            Err(_) => control.unavailable(),
        }
    }
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
            radios: crate::radios::Radios::unread(),
            provider_admin: ui::models::ProviderAdministration::default(),
            linked: ui::linked_accounts::LinkedAccounts::default(),
        }
    }

    /// This pane reaching `linked` for the person's linked accounts.
    #[must_use]
    pub fn with_linked_accounts(mut self, linked: ui::linked_accounts::LinkedAccounts) -> Self {
        self.linked = linked;
        self
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
        let launcher_admin = crate::take_launcher_provider_admin();
        let mut provider_admin = if cx.mode().is_live() {
            launcher_admin
        } else {
            ui::models::ProviderAdministration::default()
        };
        while let Some(loan) = cx.take_lent::<ui::models::ProviderAdministration>() {
            provider_admin = loan;
        }
        let mut linked = ui::linked_accounts::LinkedAccounts::default();
        while let Some(loan) = cx.take_lent::<ui::linked_accounts::LinkedAccounts>() {
            linked = loan;
        }
        let scene = cx.fixture::<crate::scenarios::CaptureScene>();
        if let Some(state) = scene
            .as_ref()
            .and_then(|scene| scene.linked_account.clone())
        {
            linked = ui::linked_accounts::LinkedAccounts::new(Rc::new(
                crate::scenarios::CaptureBridge(state),
            ));
        }
        let mut attached = Self {
            door: Some(Door {
                actions: cx.actions(),
                dispatcher: cx.dispatcher(),
                sections: Rc::new(sections(cx)),
            }),
            storage: cx.storage().clone(),
            mode: cx.mode(),
            scene,
            provider_admin,
            linked,
            radios: crate::radios::Radios::unread(),
        };
        if attached.reaches_devices() {
            attached.radios = crate::radios::Radios::of_device();
        }
        attached
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

    /// The radios this device has, read off the UI thread and followed as
    /// adapters come and go.
    pub(crate) fn radios(&self) -> &crate::radios::Radios {
        &self.radios
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

    /// `decl`, a bound field edit or verb of section `T`, as an operation a
    /// live row offers (one app's notification rule on that app's row).
    /// `None` outside a host, or where the section is read only here.
    #[must_use]
    pub fn row_operation<T: Section>(
        &self,
        decl: &ui::tilo_resource::ActionDecl<T>,
    ) -> Option<ui::tilo_actions::ActionSpec> {
        self.section::<T>()?.operation(decl)
    }

    /// The published actions of section `T`; `None` outside a host.
    fn section<T: Section>(&self) -> Option<&Actions<T>> {
        self.door
            .as_ref()?
            .sections
            .get(&TypeId::of::<T>())?
            .downcast_ref::<Actions<T>>()
    }

    /// `control`, setting the field of section `T` at `path` to each
    /// committed value: the section's own field edit, which its check
    /// guards. Disabled outside a host.
    #[must_use]
    pub fn bind_field<T, V, C>(&self, control: C, path: ui::tilo_resource::Path<T, V>) -> C
    where
        T: Section,
        V: ui::tilo_resource::Model + ui::tilo_resource::Settable + 'static,
        C: ActionControl<V>,
    {
        match self
            .section::<T>()
            .and_then(|actions| actions.field_binding(path))
        {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control`, setting the field of section `T` at `path` to what `value`
    /// makes of each committed input (a label for a stored number); an
    /// input it refuses changes nothing. Disabled outside a host.
    #[must_use]
    pub fn bind_field_with<T, V, I, C>(
        &self,
        control: C,
        path: ui::tilo_resource::Path<T, V>,
        value: impl Fn(I) -> Result<V, String> + Send + Sync + 'static,
    ) -> C
    where
        T: Section,
        V: ui::tilo_resource::Model + ui::tilo_resource::Settable + 'static,
        I: 'static,
        C: ActionControl<I>,
    {
        match self
            .section::<T>()
            .and_then(|actions| actions.set_binding(path, value))
        {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control`, making each committed input the edit `edit` builds of the
    /// field of section `T` at `path` (one entry of a map, built whole
    /// where it is not there yet). Disabled outside a host.
    #[must_use]
    pub fn bind_edit<T, V, I, C>(
        &self,
        control: C,
        path: ui::tilo_resource::Path<T, V>,
        edit: impl Fn(I) -> ui::tilo_resource::Edit<T> + Send + Sync + 'static,
    ) -> C
    where
        T: Section,
        V: 'static,
        I: 'static,
        C: ActionControl<I>,
    {
        match self
            .section::<T>()
            .and_then(|actions| actions.edit_binding_with(path, move |_, input| Ok(edit(input))))
        {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }

    /// `control`, invoking the verb `verb` of section `T` with each committed
    /// value. Disabled outside a host.
    #[must_use]
    pub fn bind_verb<T, I, C>(&self, control: C, verb: impl Binds<T, I>) -> C
    where
        T: Section,
        I: 'static,
        C: ActionControl<I>,
    {
        match self.section::<T>() {
            Some(actions) => actions.bind(control, verb),
            None => control.unavailable(),
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
        if !door.dispatcher.may(&spec.contract.need()) {
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

    /// The binding of one fully specified `effect` that `row` offers, for a
    /// button or a row menu entry acting on that row. `None` outside a host,
    /// or where the row does not offer it now.
    pub(crate) fn row_invocation(
        &self,
        row: &ui::tilo_actions::Resource,
        effect: impl Into<SettingsEffect>,
    ) -> Option<ui::tilo_actions::ActionBinding<()>> {
        let effect = effect.into();
        let door = self.door.as_ref()?;
        let spec = row.actions.iter().find(|spec| spec.id == effect.id())?;
        if !door.dispatcher.may(&spec.contract.need()) {
            return None;
        }
        let mut args = effect.arguments();
        let asked: Vec<String> = spec
            .params()
            .iter()
            .map(|param| param.name.to_string())
            .collect();
        args.retain(|name, _| asked.contains(name));
        ui::tilo_actions::ActionBinding::new(row, spec, move |()| args.clone()).ok()
    }

    /// `control` (a button) invoking the fully specified `effect` on `row`;
    /// unavailable where the row does not offer it.
    pub(crate) fn bind_row<C: ActionControl<()>>(
        &self,
        control: C,
        row: &ui::tilo_actions::Resource,
        effect: impl Into<SettingsEffect>,
    ) -> C {
        match self.row_invocation(row, effect) {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
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
            .is_some_and(|declaration| {
                door.dispatcher
                    .may(&ui::tilo_actions::ActionNeed::of(declaration.action_kind))
            })
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
///
/// Its fields' controls show what [`Followed::shown`] hands out, which
/// follows the value in place: a change elsewhere (the palette, an agent,
/// another window) moves the control without the pane rebuilding.
pub struct Followed<T> {
    settings: StoredSettings<T>,
    seen: SettingsRevision,
    value: T,
    shown: std::cell::RefCell<Vec<Shown<T>>>,
}

/// One field of a followed value as a control shows it.
struct Shown<T> {
    /// The field's path and the type it is shown as.
    key: (Vec<ui::tilo_resource::Segment>, TypeId),
    state: Box<dyn Any>,
    show: Box<dyn Fn(&T)>,
}

impl<T: SettingsSchema + Clone + PartialEq> Followed<T> {
    fn new(settings: StoredSettings<T>) -> Self {
        let seen = settings.revision();
        let value = settings.get();
        Self {
            settings,
            seen,
            value,
            shown: std::cell::RefCell::default(),
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
        for shown in self.shown.borrow().iter() {
            (shown.show)(&self.value);
        }
        true
    }

    /// The stored settings, for the effect that writes them.
    #[must_use]
    pub fn settings(&self) -> &StoredSettings<T> {
        &self.settings
    }

    /// The document version the value was last read at.
    #[must_use]
    pub fn revision(&self) -> SettingsRevision {
        self.seen
    }
}

impl<T: Section> Followed<T> {
    /// The field at `path` as `show` makes it for a control, kept current
    /// as the value changes. Every control of the field shown as the same
    /// type shares one state.
    pub fn shown<V, S>(
        &self,
        path: &ui::tilo_resource::Path<T, V>,
        show: impl Fn(&V) -> S + 'static,
    ) -> LocalState<S>
    where
        V: 'static,
        S: Clone + Default + PartialEq + 'static,
    {
        let key = (path.segments().to_vec(), TypeId::of::<S>());
        let mut all = self.shown.borrow_mut();
        if let Some(state) = all
            .iter()
            .find(|shown| shown.key == key)
            .and_then(|shown| shown.state.downcast_ref::<LocalState<S>>())
        {
            return state.clone();
        }
        let state = LocalState::new(path.get(&self.value).map(&show).unwrap_or_default());
        let path = path.clone();
        let follows = state.clone();
        all.push(Shown {
            key,
            state: Box::new(state.clone()),
            show: Box::new(move |value: &T| {
                if let Some(now) = path.get(value).map(&show)
                    && follows.with(|was| *was != now)
                {
                    follows.set(now);
                }
            }),
        });
        state
    }
}

/// An area's live rows, kept while what they were made from holds. Every
/// publication asks each area for its rows; an area listing every installed
/// app made hundreds of action declarations each time, on every click and
/// keystroke, though nothing they show had changed.
pub(crate) struct RowsMemo<K> {
    kept: std::cell::RefCell<Option<(K, Vec<ui::framework::LiveRow>)>>,
}

impl<K> Default for RowsMemo<K> {
    fn default() -> Self {
        Self {
            kept: std::cell::RefCell::new(None),
        }
    }
}

impl<K: PartialEq> RowsMemo<K> {
    /// The rows made from `key`: those kept when they were made from it,
    /// else what `make` makes now.
    pub(crate) fn rows(
        &self,
        key: K,
        make: impl FnOnce() -> Vec<ui::framework::LiveRow>,
    ) -> Vec<ui::framework::LiveRow> {
        let mut kept = self.kept.borrow_mut();
        if let Some((_, rows)) = kept.as_ref().filter(|(was, _)| *was == key) {
            return rows.clone();
        }
        let rows = make();
        *kept = Some((key, rows.clone()));
        rows
    }
}

/// What tests reach of the sections, as an agent or assistive technology
/// does.
#[cfg(test)]
pub(crate) mod testing {
    use ui::framework::testing::TestApp;

    pub(crate) const APPEARANCE: &str = "settings.appearance";
    pub(crate) const ACCESSIBILITY: &str = "settings.accessibility";
    pub(crate) const NOTIFICATIONS: &str = "settings.notifications";
    pub(crate) const POWER: &str = "settings.power";
    pub(crate) const INPUT: &str = "settings.input";
    pub(crate) const TEXT_ASSISTANCE: &str = "settings.text-assistance";
    pub(crate) const CONTINUITY: &str = "settings.continuity";

    /// How many widgets in the tree under `w` are named `name`.
    pub(crate) fn count_named(w: &dyn ui::Widget, name: &str) -> usize {
        usize::from(w.name() == name)
            + w.children()
                .into_iter()
                .map(|child| count_named(child, name))
                .sum::<usize>()
    }

    /// Every action a person can take under `w`, by what it says, in tree
    /// order: its buttons, the rows that act when chosen, and the entries
    /// of each row's menu (not the More button that opens it).
    pub(crate) fn actions_of(w: &dyn ui::Widget, actions: &mut Vec<String>) {
        if let Some(menu) = w
            .as_any()
            .and_then(|any| any.downcast_ref::<ui::widget::RowMenu>())
        {
            actions.extend(menu.labels().into_iter().map(str::to_owned));
            return;
        }
        // A row that acts when chosen, offered or shown unavailable here.
        let acts = w.name() == "Button"
            || (w.name() == "ListRow" && (w.is_focusable() || !w.is_enabled()));
        if acts && let Some(label) = w.accessibility().and_then(|node| node.name) {
            actions.push(label);
        }
        for child in w.children() {
            actions_of(child, actions);
        }
    }

    /// The accessible names of the widgets named `name` under `w`, in tree
    /// order, appended to `labels`.
    pub(crate) fn labels_of(w: &dyn ui::Widget, name: &str, labels: &mut Vec<String>) {
        if w.name() == name
            && let Some(label) = w.accessibility().and_then(|node| node.name)
        {
            labels.push(label);
        }
        for child in w.children() {
            labels_of(child, name, labels);
        }
    }

    /// Set the field `field` of the section `kind` to `value` through the
    /// door, as an agent does.
    ///
    /// # Errors
    /// The door's refusal: the section's check, an unknown field.
    pub(crate) fn edit_field(
        app: &mut TestApp<crate::SettingsShell>,
        kind: &str,
        field: &str,
        value: impl serde::Serialize,
    ) -> Result<(), String> {
        edit_path(app, kind, &[field], value)
    }

    /// Invoke the verb `action` of the section `kind` with `arguments`
    /// through the door, as an agent does.
    ///
    /// # Errors
    /// The door's refusal.
    pub(crate) fn invoke_section(
        app: &mut TestApp<crate::SettingsShell>,
        kind: &str,
        action: &str,
        arguments: serde_json::Value,
    ) -> Result<(), String> {
        app.call_on(&format!("settings:{kind}"), action, arguments)
            .map(drop)
            .map_err(|error| format!("{error:?}"))
    }

    /// Set the field at `path` (field names, outermost first) of the section
    /// `kind` to `value` through the door, as an agent does.
    ///
    /// # Errors
    /// The door's refusal: the section's check, an unknown field.
    pub(crate) fn edit_path(
        app: &mut TestApp<crate::SettingsShell>,
        kind: &str,
        path: &[&str],
        value: impl serde::Serialize,
    ) -> Result<(), String> {
        let path: Vec<serde_json::Value> = path
            .iter()
            .map(|field| serde_json::json!({ "field": field }))
            .collect();
        app.call_on(
            &format!("settings:{kind}"),
            "field-edit",
            serde_json::json!({
                "edit": { "path": path, "op": "set", "value": value }
            }),
        )
        .map(drop)
        .map_err(|error| format!("{error:?}"))
    }
}
