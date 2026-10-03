//! Building views: widgets whose values come from the resource and whose
//! edits are actions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::{Rc, Weak};

use tilo_actions::ActionBinding;
use tilo_resource::{Binds, Collection, Effect, Id, Invoke, Resource};
use ui_core::LocalState;
use ui_core::record::RecordSaveStatus;
use ui_core::widget::{Button, Checkbox, Element, Text, TextInput};

use super::controls::{ActionControl, Actions};
use super::dispatch::Dispatcher;
use super::each::Each;
use super::effect::{EffectAnswer, EffectCx, EffectStatus, Effects};
use super::live::Live;
use super::settings::{Setting, ViewStorage};
use super::stage::Stages;
use super::surface::{ResourceSurface, SurfaceCx, SurfaceHost};
use super::switch::Switch;

fn disabled(mut button: Button) -> Button {
    button.set_enabled(false);
    button
}

type Update<R> = Box<dyn FnMut(&<R as Resource>::State)>;
type Updater<R> = (Weak<()>, Update<R>);

/// Model-to-widget refreshers registered by bound controls. Each is tied to
/// the lifetime of the view scope that created it (a row of an [`Each`]).
pub(crate) struct Updaters<R: Resource>(Rc<RefCell<Vec<Updater<R>>>>);

impl<R: Resource> Clone for Updaters<R> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<R: Resource> Updaters<R> {
    pub(crate) fn new() -> Self {
        Self(Rc::default())
    }

    pub(crate) fn run(&self, state: &R::State) {
        // Take the list so an updater may register more (a rebuilt row)
        // without a reentrant borrow.
        let mut current = std::mem::take(&mut *self.0.borrow_mut());
        current.retain_mut(|(alive, update)| {
            if alive.strong_count() == 0 {
                return false;
            }
            update(state);
            true
        });
        let mut list = self.0.borrow_mut();
        current.append(&mut list);
        *list = current;
    }

    fn push(&self, alive: Weak<()>, update: Update<R>) {
        self.0.borrow_mut().push((alive, update));
    }
}

/// Projects the resource state onto the value a view scope reads: the whole
/// state at the root, one item inside an [`Each`] row.
pub(crate) type Focus<R, T> = Rc<dyn for<'a> Fn(&'a <R as Resource>::State) -> Option<&'a T>>;

/// Everything a view needs: reads of the resource state, bindings of controls
/// to actions, local state, and custom surfaces.
///
/// `T` is what this scope reads — the resource state at the root, one item in
/// a row built by [`ViewCx::each`].
pub struct ViewCx<R: Resource, T: 'static = <R as Resource>::State> {
    pub(crate) live: Live<R>,
    pub(crate) focus: Focus<R, T>,
    pub(crate) updaters: Updaters<R>,
    pub(crate) alive: Weak<()>,
    pub(crate) dispatch: Dispatcher<R>,
    pub(crate) published: Rc<tilo_actions::Resource>,
    pub(crate) item: Option<Id>,
    pub(crate) controls: Rc<RefCell<HashMap<String, u32>>>,
    pub(crate) saved: LocalState<RecordSaveStatus>,
    pub(crate) stages: Stages,
    pub(crate) feeds: super::history::HistoryFeeds,
    pub(crate) storage: Rc<ViewStorage>,
    pub(crate) effects: Effects<R>,
}

impl<R: Resource> ViewCx<R> {
    pub(crate) fn root(
        live: Live<R>,
        updaters: Updaters<R>,
        alive: &Rc<()>,
        dispatch: Dispatcher<R>,
        published: tilo_actions::Resource,
        saved: LocalState<RecordSaveStatus>,
        stages: Stages,
        storage: Rc<ViewStorage>,
    ) -> Self {
        // The history verbs every app answers are bindable like its own.
        let mut published = published;
        for spec in super::history::specs() {
            if !published.actions.iter().any(|action| action.id == spec.id) {
                published = published.with_action(spec);
            }
        }
        Self {
            live,
            focus: Rc::new(|state| Some(state)),
            updaters,
            alive: Rc::downgrade(alive),
            dispatch,
            published: Rc::new(published),
            item: None,
            controls: Rc::default(),
            saved,
            stages,
            feeds: super::history::HistoryFeeds::default(),
            storage,
            effects: Effects::inert(),
        }
    }
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// The live resource state, for reads a binding does not cover.
    pub fn live(&self) -> &Live<R> {
        &self.live
    }

    /// Where edits go. Custom widgets keep a clone.
    pub fn dispatcher(&self) -> Dispatcher<R> {
        self.dispatch.clone()
    }

    /// Who this view is for: the participant reading and editing through
    /// it, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). Compare the
    /// authors the resource records with it ("your message", "you reacted")
    /// instead of keeping the reader in the resource state, which is the
    /// same for everyone. `None` in a view of a past version.
    pub fn me(&self) -> Option<&str> {
        self.dispatch.me()
    }

    /// Whether this participant may only view, when the view is built.
    /// Bound controls already follow permission changes on their own; to show
    /// or disable something else while the participant may only view, bind to
    /// [`Self::read_only_state`], which follows role changes while the app
    /// runs.
    pub fn read_only(&self) -> bool {
        self.dispatch.read_only()
    }

    /// Whether this participant may only view, as state that follows role
    /// changes while the app runs.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.dispatch.read_only_state()
    }

    /// Whether the record's latest edits are durable, for a status line.
    /// Saving is automatic; there is nothing to save by hand.
    pub fn save_status(&self) -> LocalState<RecordSaveStatus> {
        self.saved.clone()
    }

    /// The item this scope stands for, inside an [`Each`] row.
    pub fn item(&self) -> Option<&Id> {
        self.item.as_ref()
    }

    /// The resource's last `last` versions, oldest first, kept current as
    /// the document moves: an in-app history list of what was done and by
    /// whom, each version addressable by the `undo`, `restore` and
    /// `checkpoint` actions every app publishes.
    pub fn history(&self, last: usize) -> LocalState<Vec<super::history::HistoryVersion>> {
        self.feeds.follow(self.alive.clone(), last)
    }

    /// Read the scope's value now (at build time). For values that must
    /// follow the resource, use a binding such as [`Self::text`].
    pub fn read<V>(&self, read: impl FnOnce(Option<&T>) -> V) -> V {
        self.live.with(|state| read((self.focus)(state)))
    }

    /// The app's typed settings of type `S`, kept current while this scope
    /// lives. `S` declares whether they follow the person or stay on the
    /// device ([`Scope`](ui_core::storage::Scope)).
    pub fn settings<S>(&self) -> Setting<S>
    where
        S: ui_core::storage::SettingsSchema + Clone + PartialEq + Send + Sync,
    {
        self.storage.follow(self.alive.clone())
    }

    /// The app's files, secrets and settings documents, as its host gave
    /// them. A secret of one document item is
    /// `cx.storage().secrets().item(purpose, id)`.
    pub fn storage(&self) -> &ui_core::storage::AppStorage {
        self.storage.storage()
    }

    /// Typed process-local view state: never persisted, shared or undone.
    pub fn local<V: Send + Sync + 'static>(&self, initial: V) -> LocalState<V> {
        LocalState::new(initial)
    }

    /// Stage view state for a screenshot scenario: `stage` runs with the
    /// scenario's name (`None` for a plain `--shots`) after the scenario's
    /// actions applied and before the frame is captured. Set the
    /// [`LocalState`]s a scenario shows — a selection, an open dialog — the
    /// way the user's input would. It never runs in a live window.
    pub fn on_stage(&self, stage: impl FnMut(Option<&str>) + 'static) {
        self.stages.push(self.alive.clone(), Box::new(stage));
    }

    fn reader<V: Default + 'static>(
        &self,
        read: impl Fn(&T) -> V + 'static,
    ) -> impl Fn(&R::State) -> V + 'static {
        let focus = Rc::clone(&self.focus);
        move |state| (focus)(state).map(&read).unwrap_or_default()
    }

    /// A stable, addressable control name: the action id, qualified by the
    /// row's item, numbered only when the same binding repeats.
    fn control_id(&self, action: &str) -> String {
        let base = match &self.item {
            Some(item) => format!("{action}-{item}"),
            None => action.to_owned(),
        };
        let mut seen = self.controls.borrow_mut();
        let count = seen.entry(base.clone()).or_insert(0);
        *count = count.saturating_add(1);
        if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        }
    }

    fn binding<I: 'static>(&self, binds: impl Binds<R, I>) -> Option<ActionBinding<I>> {
        self.actions().binding(binds)
    }

    /// Keep `sink` in step with a value derived from the resource.
    fn follow<V: PartialEq + Default + Clone + 'static>(
        &self,
        read: impl Fn(&T) -> V + 'static,
        mut sink: impl FnMut(V) + 'static,
    ) -> V {
        let read = self.reader(read);
        let initial = self.live.with(&read);
        let mut last = initial.clone();
        self.updaters.push(
            self.alive.clone(),
            Box::new(move |state| {
                let next = read(state);
                if next != last {
                    last.clone_from(&next);
                    sink(next);
                }
            }),
        );
        initial
    }

    /// Text that always shows `read` of the resource.
    pub fn text(&self, read: impl Fn(&T) -> String + 'static) -> Text {
        let live = self.live.clone();
        let read = self.reader(read);
        Text::dynamic(move || live.with(&read))
    }

    /// A text field showing `read` of the resource; each committed edit
    /// invokes `action` with the field's new text.
    pub fn input(
        &self,
        read: impl Fn(&T) -> String + 'static,
        action: impl Binds<R, String>,
    ) -> TextInput {
        let id = self.control_id(action.action_id());
        let state = LocalState::new(String::new());
        let sink = state.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        state.set(initial);
        let input = TextInput::from_state(&state).with_id(&id);
        match self.binding(action) {
            Some(binding) => input.on_action(binding),
            None => input.disabled(true),
        }
    }

    /// A checkbox showing `read` of the resource; toggling invokes `action`
    /// with the new value.
    pub fn checkbox(
        &self,
        label: impl Into<String>,
        read: impl Fn(&T) -> bool + 'static,
        action: impl Binds<R, bool>,
    ) -> Checkbox {
        let id = self.control_id(action.action_id());
        let state = LocalState::new(false);
        let sink = state.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        state.set(initial);
        let checkbox = Checkbox::from_state(&state).with_id(&id).with_label(label);
        match self.binding(action) {
            Some(binding) => checkbox.on_action(binding),
            None => checkbox.with_enabled(false),
        }
    }

    /// A button that invokes one fully specified action.
    pub fn button(&self, label: &str, invoke: Invoke<R>) -> Button {
        let id = self.control_id(invoke.id);
        let button = Button::new(&id, label, None);
        let Some(spec) = self
            .published
            .actions
            .iter()
            .find(|spec| spec.id == invoke.id)
            .cloned()
        else {
            return disabled(button);
        };
        match ActionBinding::static_json(&self.published, &spec, invoke.arguments) {
            Ok(binding) => button.on_action(binding),
            Err(error) => {
                tracing::warn!(%error, action = invoke.id, "button not bound");
                disabled(button)
            }
        }
    }

    /// One row per item of a collection, kept in collection order. A row is
    /// built once per item and retained while the item exists, so focus and
    /// caret inside it survive other items changing. A row's context menu
    /// (right-click, the Menu key or Shift+F10) offers the actions the
    /// resource publishes on its item, with the item already bound.
    pub fn each<I: 'static>(
        &self,
        items: fn(&T) -> &Collection<I>,
        row: impl Fn(&mut ViewCx<R, I>) -> Element + 'static,
    ) -> Each<R> {
        let focus = Rc::clone(&self.focus);
        let ids = move |state: &R::State| -> Vec<Id> {
            (focus)(state).map_or_else(Vec::new, |value| items(value).ids().cloned().collect())
        };
        let parent_focus = Rc::clone(&self.focus);
        let live = self.live.clone();
        let updaters = self.updaters.clone();
        let dispatch = self.dispatch.clone();
        let published = Rc::clone(&self.published);
        let controls = Rc::clone(&self.controls);
        let saved = self.saved.clone();
        let stages = self.stages.clone();
        let feeds = self.feeds.clone();
        let storage = Rc::clone(&self.storage);
        let effects = self.effects.clone();
        Each::new(
            self.live.clone(),
            self.dispatch.clone(),
            Box::new(ids),
            Box::new(move |id: &Id, alive: &Rc<()>| {
                let parent = Rc::clone(&parent_focus);
                let key = id.clone();
                let focus: Focus<R, I> =
                    Rc::new(move |state| (parent)(state).and_then(|value| items(value).get(&key)));
                let mut cx = ViewCx {
                    live: live.clone(),
                    focus,
                    updaters: updaters.clone(),
                    alive: Rc::downgrade(alive),
                    dispatch: dispatch.clone(),
                    published: Rc::clone(&published),
                    item: Some(id.clone()),
                    controls: Rc::clone(&controls),
                    saved: saved.clone(),
                    stages: stages.clone(),
                    feeds: feeds.clone(),
                    storage: Rc::clone(&storage),
                    effects: effects.clone(),
                };
                row(&mut cx)
            }),
        )
    }

    /// The resource's published actions, for binding controls built outside
    /// the view function: the rows a table, list or tree creates on demand.
    pub fn actions(&self) -> Actions<R> {
        Actions::new(Rc::clone(&self.published))
    }

    /// A value that follows `read` of the resource, for any toolkit control
    /// built from a [`LocalState`] (`Slider::from_state`, `Dropdown::from_state`).
    pub fn value<V>(&self, read: impl Fn(&T) -> V + 'static) -> LocalState<V>
    where
        V: PartialEq + Default + Clone + 'static,
    {
        let state = LocalState::new(V::default());
        let sink = state.clone();
        let initial = self.follow(read, move |value| sink.set(value));
        state.set(initial);
        state
    }

    /// Any action-bound toolkit control, showing `read` of the resource and
    /// invoking `action` with each committed value:
    /// `cx.control(Slider::from_state, |p| p.volume, Profile::SET_VOLUME)`.
    /// The control is disabled when the action is not published.
    pub fn control<V, C>(
        &self,
        make: impl FnOnce(&LocalState<V>) -> C,
        read: impl Fn(&T) -> V + 'static,
        action: impl Binds<R, V>,
    ) -> C
    where
        V: PartialEq + Default + Clone + 'static,
        C: ActionControl<V>,
    {
        let control = make(&self.value(read));
        self.actions().bind(control, action)
    }

    /// A part of the view chosen by view-local state — the detail pane of the
    /// selected item, the page of a wizard. The part is built again, with its
    /// own bindings, whenever `key` changes; the previous one is dropped.
    pub fn switch<K>(
        &self,
        key: &LocalState<K>,
        part: impl Fn(&mut ViewCx<R, T>, &K) -> Element + 'static,
    ) -> Element
    where
        K: Clone + PartialEq + 'static,
    {
        let live = self.live.clone();
        let focus = Rc::clone(&self.focus);
        let updaters = self.updaters.clone();
        let dispatch = self.dispatch.clone();
        let published = Rc::clone(&self.published);
        let item = self.item.clone();
        let saved = self.saved.clone();
        let stages = self.stages.clone();
        let feeds = self.feeds.clone();
        let storage = Rc::clone(&self.storage);
        let effects = self.effects.clone();
        Box::new(Switch::new(
            key.clone(),
            Box::new(move |value: &K, alive: &Rc<()>| {
                let mut cx = ViewCx {
                    live: live.clone(),
                    focus: Rc::clone(&focus),
                    updaters: updaters.clone(),
                    alive: Rc::downgrade(alive),
                    dispatch: dispatch.clone(),
                    published: Rc::clone(&published),
                    item: item.clone(),
                    // Each build of the part names its controls afresh.
                    controls: Rc::default(),
                    saved: saved.clone(),
                    stages: stages.clone(),
                    feeds: feeds.clone(),
                    storage: Rc::clone(&storage),
                    effects: effects.clone(),
                };
                part(&mut cx, value)
            }),
        ))
    }

    /// Host a custom widget or renderer that takes part through the
    /// [`ResourceSurface`] contract, answering its effect vocabulary `E`
    /// (none unless it implements `ResourceSurface<R, E>`).
    pub fn surface<S, E>(&self, mut surface: S) -> Element
    where
        S: ResourceSurface<R, E> + 'static,
        E: Effect,
    {
        surface.attach(SurfaceCx {
            dispatch: self.dispatch.clone(),
            live: self.live.clone(),
            storage: Rc::clone(&self.storage),
            effects: self.effects.clone(),
            alive: self.alive.clone(),
        });
        surface.present(&self.live.get());
        let host = SurfaceHost::new(
            self.live.clone(),
            self.dispatch.clone(),
            &self.effects,
            self.alive.clone(),
            surface,
        );
        let pending = host.staging();
        self.stages.push(
            self.alive.clone(),
            Box::new(move |scenario| {
                *pending.borrow_mut() = Some(scenario.map(str::to_owned));
            }),
        );
        Box::new(host)
    }

    /// Answer the effect vocabulary `E` — work with no document delta that a
    /// button, the palette, an agent or a script invokes — with `handle`,
    /// and publish `E` on the resource while this part of the view lives.
    ///
    /// `handle` receives each invocation as a typed value. It finishes at
    /// once ([`Effected::done`](super::Effected::done)) or starts a job
    /// through the [`EffectCx`] (for an effect declared `job`); a job's
    /// completion runs on the UI thread and changes the document through the
    /// context's dispatcher. Handlers never run in History mode. Say which
    /// effects are unavailable or relabelled right now with
    /// [`EffectsHandle::status`].
    pub fn effects<E: Effect>(
        &self,
        handle: impl FnMut(E, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) -> EffectsHandle {
        let status: StatusSource = Rc::default();
        if self.effects.env().is_some() {
            let source = Rc::clone(&status);
            self.effects.publish::<E>(self.alive.clone(), move || {
                let now = source
                    .borrow()
                    .as_ref()
                    .map(|status| status())
                    .unwrap_or_default();
                super::effect::published_specs::<E>(&now)
            });
            self.effects.handle(self.alive.clone(), handle);
        }
        EffectsHandle { status }
    }

    /// Drain a projection: what the resource's actions queued for the world
    /// outside the document — an outbox entry, a filesystem operation, the
    /// device configuration the document describes.
    ///
    /// `pending` lists the entries waiting in a state, each under a stable
    /// key; `run` carries one out, at once or as a job through its
    /// [`EffectCx`], and records the outcome with an action (acknowledging
    /// the entry, so it is no longer pending). The framework runs it in the
    /// live app after every change, on every participant's device — never
    /// in History mode — and runs an entry again only when the document
    /// changed and still lists it, so `run` should be safe to repeat.
    pub fn drain<K, I>(
        &self,
        pending: impl Fn(&R::State) -> Vec<(K, I)> + 'static,
        run: impl FnMut(I, &mut EffectCx<R>) -> EffectAnswer + 'static,
    ) where
        K: Eq + Hash + Clone + 'static,
        I: 'static,
    {
        self.effects.drain(self.alive.clone(), pending, run);
    }

    /// A button that invokes one effect, through the same door (and the
    /// same confirmation, for a destructive one) as the palette.
    pub fn effect_button<E: Effect>(&self, label: &str, effect: &E) -> Button {
        let id = self.control_id(effect.id());
        self.actions()
            .bind_effect_invocation(Button::new(&id, label, None), effect)
    }
}

type StatusSource = Rc<RefCell<Option<Box<dyn Fn() -> Vec<EffectStatus>>>>>;

/// A registered effect handler ([`ViewCx::effects`]).
pub struct EffectsHandle {
    status: StatusSource,
}

impl EffectsHandle {
    /// Publish the effects as `status` says they are whenever the resource
    /// is published: unavailable while offline, relabelled while recording.
    /// Read the view's [`LocalState`]s in it.
    pub fn status(self, status: impl Fn() -> Vec<EffectStatus> + 'static) {
        *self.status.borrow_mut() = Some(Box::new(status));
    }
}
