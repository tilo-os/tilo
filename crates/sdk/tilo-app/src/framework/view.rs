//! Building views: widgets whose values come from the resource and whose
//! edits are actions.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

use tilo_resource::{
    ActionDecl, Binds, Collection, Edit, Effect, Id, Model, Params, Path, Resource, Segment,
    Settable,
};
use ui_core::LocalState;
use ui_core::arriving::Arriving;
use ui_core::i18n::LabelKey;
use ui_core::record::RecordSaveStatus;
use ui_core::widget::ArrivingView;
use ui_core::widget::{Button, Checkbox, Element, Text, TextInput};

use super::controls::{ActionControl, Actions};
use super::dispatch::{Dispatcher, Submit};
use super::each::Each;
use super::effect::{EffectAnswer, EffectCx, EffectStatus, Effects};
use super::env::RecordEnv;
use super::form::{Attached, Form, FormField, Forms, RecordField};
use super::live::Live;
use super::reactive::{Reads, Updaters};
use super::scenario::Run;
use super::settings::ViewStorage;
use super::stage::Stages;
use super::surface::{ResourceSurface, SurfaceCx};
use super::switch::Switch;

fn disabled(mut button: Button) -> Button {
    button.set_enabled(false);
    button
}

type Bind<R> = Box<dyn Fn(&<R as Resource>::State) -> Vec<ActionDecl<R>>>;
type BindList<R> = Vec<(Weak<()>, Bind<R>)>;

/// The edits views bound to their local state
/// ([`ViewCx::bind_declarations`]), each tied to the view scope that bound
/// it.
pub(crate) struct Bindings<R: Resource>(Rc<RefCell<BindList<R>>>);

impl<R: Resource> Clone for Bindings<R> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<R: Resource> Bindings<R> {
    pub(crate) fn new() -> Self {
        Self(Rc::default())
    }

    /// Every live view's bound edits over `state`.
    pub(crate) fn declarations(&self, state: &R::State) -> Vec<ActionDecl<R>> {
        let mut bindings = self.0.borrow_mut();
        bindings.retain(|(alive, _)| alive.strong_count() > 0);
        bindings.iter().flat_map(|(_, bind)| bind(state)).collect()
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
    pub(crate) env: RecordEnv<R>,
    pub(crate) focus: Focus<R, T>,
    pub(crate) updaters: Updaters<R>,
    /// The row of an [`Each`] this scope builds, whose readers run while it
    /// is mounted; `None` outside a row.
    pub(crate) gate: Option<super::reactive::Gate>,
    pub(crate) action_bindings: Actions<R>,
    pub(crate) item: Option<Id>,
    /// Where this scope's value sits in the record: the root, or the item a
    /// row stands for. `None` inside a row of a collection the view reads
    /// in a way no path names.
    pub(crate) path: Option<Vec<Segment>>,
    pub(crate) controls: Rc<RefCell<HashMap<String, u32>>>,
    /// The forms filled in in this scope ([`Self::form`]).
    pub(crate) forms: Forms,
    pub(crate) stages: Stages,
    pub(crate) feeds: super::history::HistoryFeeds,
    pub(crate) bindings: Bindings<R>,
}

impl<R: Resource, T: 'static> std::ops::Deref for ViewCx<R, T> {
    type Target = RecordEnv<R>;
    fn deref(&self) -> &RecordEnv<R> {
        &self.env
    }
}

impl<R: Resource> ViewCx<R> {
    /// The scope of a freshly built root view: its own updaters, save status
    /// and staging, none of which another root shares.
    pub(crate) fn root(
        live: Live<R>,
        alive: &Rc<()>,
        dispatch: Dispatcher<R>,
        published: tilo_actions::Resource,
        run: Run,
        storage: Rc<ViewStorage>,
    ) -> Self {
        let published = with_history_verbs(published);
        let app = dispatch.app_id();
        let fresh = FreshScope::new();
        Self {
            env: RecordEnv {
                live,
                alive: Rc::downgrade(alive),
                dispatch,
                saved: fresh.saved,
                saves: fresh.saves,
                run,
                storage,
                effects: Effects::inert(),
            },
            focus: Rc::new(|state| Some(state)),
            updaters: Updaters::new(),
            gate: None,
            action_bindings: Actions::new(app, published),
            item: None,
            path: Some(Vec::new()),
            controls: fresh.controls,
            forms: fresh.forms,
            stages: fresh.stages,
            feeds: fresh.feeds,
            bindings: Bindings::new(),
        }
    }
}

/// The specs `specs` builds as callers see them now, at what `source`
/// says of them, kept in `kept`. Not generic, so every app's views share
/// it.
#[inline(never)]
fn current_specs(
    source: &StatusSource,
    kept: &super::effect::KeptSpecs,
    specs: fn() -> Vec<tilo_actions::ActionSpec>,
) -> Vec<tilo_actions::ActionSpec> {
    let now = source
        .borrow()
        .as_ref()
        .map(|status| status())
        .unwrap_or_default();
    super::effect::kept_specs_of(kept, &now, specs)
}

/// What a freshly built root view scope starts with that does not name its
/// resource type: made here once rather than in every app's
/// [`ViewCx::root`].
struct FreshScope {
    saved: LocalState<RecordSaveStatus>,
    saves: super::Saves,
    controls: Rc<RefCell<HashMap<String, u32>>>,
    forms: Forms,
    stages: Stages,
    feeds: super::history::HistoryFeeds,
}

impl FreshScope {
    #[inline(never)]
    fn new() -> Self {
        Self {
            saved: LocalState::new(RecordSaveStatus::Saved),
            saves: super::Saves::default(),
            controls: Rc::default(),
            forms: Forms::default(),
            stages: Stages::new(),
            feeds: super::history::HistoryFeeds::default(),
        }
    }
}

/// `published` with the history verbs every app answers, so they are
/// bindable like its own. Not generic, so it is compiled once rather than
/// for every resource.
#[inline(never)]
fn with_history_verbs(published: tilo_actions::Resource) -> Rc<tilo_actions::Resource> {
    let mut published = published;
    for spec in super::history::specs() {
        if !published.actions.iter().any(|action| action.id == spec.id) {
            published = published.with_action(spec);
        }
    }
    Rc::new(published)
}

impl<R: Resource, T: 'static> ViewCx<R, T> {
    /// The live resource state, for reads a binding does not cover.
    pub fn live(&self) -> &Live<R> {
        &self.live
    }

    /// Observe what this participant may do, without storing their role in
    /// the shared resource. Use this with [`Self::switch`] for an owner-only
    /// pane, or with a bound widget for a permission-specific label. Changes
    /// follow the host even when the document and write access stay unchanged.
    /// This controls presentation; the action door remains the authority.
    pub fn permission_state(&self, need: tilo_actions::ActionNeed) -> LocalState<bool> {
        self.dispatch.permission_state(need)
    }

    /// Whether this participant may govern the room, observed: for an
    /// owner-only pane with [`Self::switch`].
    pub fn govern_state(&self) -> LocalState<bool> {
        self.dispatch.govern_state()
    }

    /// Whether what the room holds has arrived, as state that follows the
    /// room's sync: until it does, a record this window joined may still be
    /// missing the content its creator wrote. Enable a choice made from that
    /// content (a free seat, the next number) with it, so nobody acts on a
    /// copy that is about to change under them. A view never writes on open
    /// to make up for it: only the record's creator writes what it starts
    /// with.
    pub fn arrived_state(&self) -> LocalState<bool> {
        self.dispatch.arrived_state()
    }

    /// Whether this participant may only view, as state that follows role
    /// changes while the app runs.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.dispatch.read_only_state()
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

    /// Typed process-local view state: never persisted, shared or undone.
    pub fn local<V: Send + Sync + 'static>(&self, initial: V) -> LocalState<V> {
        LocalState::new(initial)
    }

    /// Show the item a search hit opens: `open` runs with the hit's anchor
    /// (a [`SearchEntry`](tilo_resource::SearchEntry)'s `anchor`) when the
    /// person picks the hit, or when an agent invokes the resource's `open`
    /// action. Select the item, switch to its page, scroll it into view:
    /// set the [`LocalState`]s that show it, as the person's own input would.
    ///
    /// `open` may refuse an anchor it cannot show (a row that is gone) by
    /// returning `Err`; the person or agent is told why.
    pub fn on_open<O: tilo_resource::Outcome>(&self, mut open: impl FnMut(&str) -> O + 'static) {
        self.stages.push_opener(
            self.alive.clone(),
            Box::new(move |anchor| open(anchor).into_result().map_err(|refusal| refusal.0)),
        );
    }

    /// Show the item of the resource a search hit or an agent's `open`
    /// names: `open` runs with its id, and refuses one it cannot show (an
    /// item that no longer exists), which the person or agent is told.
    /// Anchors that name no item of the resource are refused for it.
    pub fn on_open_item(
        &self,
        mut open: impl FnMut(&Id) -> Result<(), tilo_resource::Refusal> + 'static,
    ) {
        self.stages.push_opener(
            self.alive.clone(),
            Box::new(move |anchor| {
                let (_, item) = tilo_resource::items::item_of_resource::<R>(anchor)
                    .ok_or_else(|| format!("`{anchor}` is not an item of {}", R::LABEL))?;
                open(&item).map_err(|refusal| refusal.0)
            }),
        );
    }

    /// `reference`, which `app` keeps in `record` (`None`: the app's own
    /// record of the workspace), as a chip that opens it where it lives
    /// ([`Dispatcher::open_reference`]): an issue reads `#12 Title`, a page
    /// its name. A click, Enter or Space, and assistive technology's
    /// activate all open it; a refusal is told like any refused action.
    pub fn reference(
        &self,
        app: &str,
        record: Option<&str>,
        reference: &tilo_reference::ResourceRef,
    ) -> ui_core::widget::Chip {
        let dispatch = self.dispatch.clone();
        let (app, record, target) = (app.to_owned(), record.map(str::to_owned), reference.clone());
        ui_core::widget::Chip::new(reference.label.clone())
            .with_id(format!("reference:{}", reference.id))
            .with_lead(super::reference::LEAD)
            .with_accessible_name(ui_core::i18n::I18nManager::format(
                "reference-open",
                &[("label", reference.label.clone().into())],
            ))
            .on_click(move || dispatch.open_reference(&app, record.as_deref(), &target))
    }

    /// Open the platform search with `request`, narrowed to this app or one
    /// part of it. Usually a control does this ([`Self::search_button`]); a
    /// custom handler calls [`Dispatcher::open_search`] on its dispatcher.
    pub fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.dispatch.open_search(request);
    }

    /// The search launcher for this view's chrome: an icon button that opens
    /// the platform search with `request`. Apps never draw a search field.
    pub fn search_button(&self, request: ui_core::search::SearchRequest) -> Button {
        ui_core::search::search_button(request)
    }

    /// Stage view state for a screenshot scenario: `stage` runs with the
    /// scenario's name (`None` for a plain `--shots`) after the scenario's
    /// actions applied and were laid out, and before the frame is captured.
    /// Set the [`LocalState`]s a scenario shows — a selection, an open
    /// dialog — the way the user's input would. It never runs in a live
    /// window.
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

    /// Keep `sink` in step with a value derived from the resource, read
    /// again after every change.
    fn follow<V: PartialEq + Default + Clone + 'static>(
        &self,
        read: impl Fn(&T) -> V + 'static,
        sink: impl FnMut(V) + 'static,
    ) -> V {
        self.follow_state(Reads::Always, self.reader(read), sink)
    }

    /// Keep `sink` in step with `read` of the state, read again after the
    /// changes `reads` names.
    fn follow_state<V: PartialEq + Clone + 'static>(
        &self,
        reads: Reads,
        read: impl Fn(&R::State) -> V + 'static,
        mut sink: impl FnMut(V) + 'static,
    ) -> V {
        let initial = self.live.with(&read);
        let mut last = initial.clone();
        self.updaters.push(
            self.alive.clone(),
            self.gate.clone(),
            reads,
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

    /// A button that submits one fully specified action: a verb with its
    /// arguments (`Snippets::NEW_SNIPPET.with(..)`), a field edit
    /// (`p.favorite.set(true)`) or a form ([`Form::submit`]).
    pub fn button(&self, label: impl Into<LabelKey>, submit: impl Submit<R>) -> Button {
        let label: LabelKey = label.into();
        let id = self.control_id(submit.action_id());
        let button = Button::new(&id, label.as_str());
        match submit.control_binding(&self.actions()) {
            Some(binding) => button.on_action(binding),
            None => disabled(button),
        }
    }

    /// The form of the verb whose parameters are `P`
    /// (`cx.form::<bookmarks::Add>()`): local values, never synced or
    /// undone, that controls bind to with the parameters' paths
    /// (`cx.input(form.paths().url)`) and a button submits as one
    /// invocation (`cx.button("bookmarks-add", form.submit())`). The same
    /// form is answered for `P` everywhere in this scope.
    pub fn form<P: Params<Resource = R>>(&self) -> Form<P> {
        self.forms.get::<P>()
    }

    /// A text field for the text at `field`: a path into the record, whose
    /// committed edits replace it ([`Path::replace`]; the keystrokes of one
    /// word are one undo step), or into a form ([`Self::form`]).
    pub fn input<M>(&self, field: impl Bindable<R, String, M>) -> TextInput {
        match field.attach(self, Path::replace) {
            Attached::Record { id, state, binding } => {
                let input = TextInput::from_state(&state).with_id(&id);
                match binding {
                    Some(binding) => input.on_action(*binding),
                    None => input.disabled(true),
                }
            }
            Attached::Local { id, state } => TextInput::from_state(&state).with_id(&id),
        }
    }

    /// A checkbox for the flag at `field`, a path into the record or a form;
    /// toggling sets it.
    pub fn checkbox<M>(
        &self,
        label: impl Into<LabelKey>,
        field: impl Bindable<R, bool, M>,
    ) -> Checkbox {
        let label: LabelKey = label.into();
        match field.attach(self, Path::set) {
            Attached::Record { id, state, binding } => {
                let checkbox = Checkbox::from_state(&state).with_id(&id).with_label(label);
                match binding {
                    Some(binding) => checkbox.on_action(*binding),
                    None => checkbox.with_enabled(false),
                }
            }
            Attached::Local { id, state } => {
                Checkbox::from_state(&state).with_id(&id).with_label(label)
            }
        }
    }

    /// Any toolkit control for the value at `field`, a path into the record
    /// or a form, setting it with each committed value:
    /// `cx.control(Slider::from_state, p.volume)`.
    pub fn control<V, C, M>(
        &self,
        make: impl FnOnce(&LocalState<V>) -> C,
        field: impl Bindable<R, V, M>,
    ) -> C
    where
        V: Settable + 'static,
        C: ActionControl<V>,
    {
        match field.attach(self, Path::set) {
            Attached::Record { state, binding, .. } => {
                let control = make(&state);
                match binding {
                    Some(binding) => control.bind_action(*binding),
                    None => control.unavailable(),
                }
            }
            Attached::Local { state, .. } => make(&state),
        }
    }

    /// Publish edits bound to this view's local state — the folder shown,
    /// the row selected — on the resource beside its declared actions, as
    /// typed builders of its action: `bind` runs whenever the published
    /// tree is read, with the current resource state, and reads the local
    /// state it captured. Invoking one dispatches what it builds through the
    /// same permission check, confirmation and undo as any other action. The
    /// binding lasts as long as this view scope (a row's ends with the row);
    /// an id the resource already publishes keeps the resource's action
    /// unless the bound one refines it ([`tilo_resource::bind_declaration`]).
    pub fn bind_declarations(&self, bind: impl Fn(&R::State) -> Vec<ActionDecl<R>> + 'static) {
        // The published tree reads what `bind` captured, mounted row or not.
        if let Some(gate) = &self.gate {
            gate.read_while_unmounted();
        }
        self.bindings
            .0
            .borrow_mut()
            .push((self.alive.clone(), Box::new(bind)));
    }

    /// The resource's published actions, for binding controls built outside
    /// the view function: the rows a table, list or tree creates on demand.
    pub fn actions(&self) -> Actions<R> {
        self.action_bindings.clone()
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

    /// A toolkit control bound to a verb: it shows `read` of the resource and
    /// invokes `action` with each committed value, for a value that is a
    /// verb's argument rather than a field
    /// (`cx.control_with(TextInput::from_state, |p| p.shape(&node), Project::SET_SHAPE.map(..))`).
    /// The control is disabled when the action is not published. A field of
    /// the record is bound by its path instead ([`Self::input`],
    /// [`Self::checkbox`], [`Self::control`]).
    pub fn control_with<V, C>(
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
        let scope = self.part_scope();
        Switch::keyed(
            key.clone(),
            Box::new(move |value: &K, alive: &Rc<()>| part(&mut scope(alive), value)),
        )
    }

    /// A part of the view showing data that arrives later: a resource read
    /// that is not here yet, a query of another app, a file read on a
    /// worker ([`Arriving`]). `part` builds what shows once the data is
    /// there, with its own bindings, again whenever a newer answer lands;
    /// until then, and when the read fails, the framework shows the
    /// placeholder and the failure ([`ArrivingView`]), with every timing.
    /// Say what the content looks like with [`ArrivingView::shaped`].
    pub fn arriving<V, E>(
        &self,
        arriving: &Arriving<V, E>,
        part: impl Fn(&mut ViewCx<R, T>, &V) -> Element + 'static,
    ) -> ArrivingView
    where
        V: Clone + 'static,
        E: std::fmt::Display + 'static,
    {
        let scope = Rc::new(self.part_scope());
        let part = Rc::new(part);
        ArrivingView::new(arriving, move |value: &V| {
            let (scope, part, value) = (Rc::clone(&scope), Rc::clone(&part), value.clone());
            Switch::keyed(
                LocalState::new(()),
                Box::new(move |(), alive: &Rc<()>| part(&mut scope(alive), &value)),
            )
        })
    }

    /// What builds a part of this scope with its own bindings, retired
    /// with the part (`alive`).
    fn part_scope(&self) -> impl Fn(&Rc<()>) -> ViewCx<R, T> + 'static {
        let env = self.env.clone();
        let focus = Rc::clone(&self.focus);
        let updaters = self.updaters.clone();
        let gate = self.gate.clone();
        let action_bindings = self.action_bindings.clone();
        let item = self.item.clone();
        let path = self.path.clone();
        let stages = self.stages.clone();
        let feeds = self.feeds.clone();
        let bindings = self.bindings.clone();
        move |alive: &Rc<()>| ViewCx {
            env: env.living(alive),
            focus: Rc::clone(&focus),
            updaters: updaters.clone(),
            gate: gate.clone(),
            action_bindings: action_bindings.clone(),
            item: item.clone(),
            path: path.clone(),
            // Each build of the part names its controls afresh.
            controls: Rc::default(),
            forms: Forms::default(),
            stages: stages.clone(),
            feeds: feeds.clone(),
            bindings: bindings.clone(),
        }
    }

    /// Host a custom widget or renderer that takes part through the
    /// [`ResourceSurface`] contract, answering its effect vocabulary `E`
    /// (none unless it implements `ResourceSurface<R, E>`).
    pub fn surface<S, E>(&self, surface: S) -> Element
    where
        S: ResourceSurface<R, E> + 'static,
        E: Effect,
    {
        SurfaceCx {
            env: self.env.clone(),
            jobs: self.effects.surface_jobs(),
            actions: self.actions(),
            stages: self.stages.clone(),
            scope: None,
        }
        .host_surface(surface)
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
        // The handler and the status the resource publishes read the scope's
        // local state, mounted row or not.
        if let Some(gate) = &self.gate {
            gate.read_while_unmounted();
        }
        // Captures and History still describe the controls they draw. Only
        // execution needs a live environment; publication is inert metadata.
        let source = Rc::clone(&status);
        let kept = super::effect::KeptSpecs::default();
        self.effects.publish::<E>(self.alive.clone(), move || {
            current_specs(&source, &kept, E::specs)
        });
        if self.effects.env().is_some() {
            self.effects.handle(self.alive.clone(), handle);
        }
        EffectsHandle { status }
    }

    /// Jobs the view starts on its own, with nobody invoking an effect: a
    /// fetch when what it shows changes. See [`Jobs`](super::Jobs).
    pub fn jobs(&self) -> super::Jobs<R> {
        self.effects.view_jobs()
    }

    /// A button that invokes one effect, through the same door (and the
    /// same confirmation, for a destructive one) as the palette.
    pub fn effect_button<E: Effect>(&self, label: impl Into<LabelKey>, effect: &E) -> Button {
        let id = self.control_id(effect.id());
        self.actions()
            .bind_effect_invocation(Button::new(&id, label.into().as_str()), effect)
    }
}

impl<R: Resource, T: Model + 'static> ViewCx<R, T> {
    /// The typed paths of this scope's value: the record's fields at the
    /// root, the item's in a row of [`Self::each`]. Controls bind to them
    /// (`row.checkbox("snippets-favorite", p.favorite)`) and edits are made
    /// at them (`p.favorite.set(true)`).
    ///
    /// # Panics
    /// Panics inside a row of a collection [`Self::each`] could not find
    /// among the fields of the value it was given.
    #[allow(
        clippy::panic,
        reason = "a row whose collection has no path is a mistake the first run of the view shows"
    )]
    pub fn paths(&self) -> T::Paths<R> {
        let Some(path) = &self.path else {
            panic!(
                "this row's collection is not a field of the value `each` read it from; read it as `|value| &value.field`"
            );
        };
        T::paths_at(Path::from_segments(path.clone()))
    }

    /// One row per item of a collection, kept in collection order:
    /// `cx.each(|snippets| &snippets.snippets, |row| ...)`. A row is built
    /// once per item and retained while the item exists, so focus and caret
    /// inside it survive other items changing; rows are added, removed and
    /// moved only when the collection's membership changes. In a row,
    /// [`Self::paths`] are rooted at its item. A row's context menu
    /// (right-click, the Menu key or Shift+F10) offers the actions the
    /// resource publishes on its item, with the item already bound.
    pub fn each<I: Model + 'static>(
        &self,
        items: impl Fn(&T) -> &Collection<I> + 'static,
        row: impl Fn(&mut ViewCx<R, I>) -> Element + 'static,
    ) -> Each<R> {
        let items: ItemsOf<T, I> = Rc::new(items);
        let located = self.path.as_ref().and_then(|base| {
            self.read(|value| {
                let value = value?;
                let relative = locate(value, items(value))?;
                let mut path = base.clone();
                path.extend(relative);
                Some(path)
            })
        });
        let focus = Rc::clone(&self.focus);
        let read_items = Rc::clone(&items);
        let ids = move |state: &R::State| -> Vec<Id> {
            (focus)(state).map_or_else(Vec::new, |value| read_items(value).ids().cloned().collect())
        };
        let stale = Rc::new(std::cell::Cell::new(false));
        let mark = Rc::clone(&stale);
        self.updaters.push(
            self.alive.clone(),
            self.gate.clone(),
            located.clone().map_or(Reads::Always, Reads::Members),
            Box::new(move |_| mark.set(true)),
        );
        let parent_focus = Rc::clone(&self.focus);
        let env = self.env.clone();
        let updaters = self.updaters.clone();
        let action_bindings = self.action_bindings.clone();
        let controls = Rc::clone(&self.controls);
        let stages = self.stages.clone();
        let feeds = self.feeds.clone();
        let bindings = self.bindings.clone();
        Each::new(
            self.live.clone(),
            self.dispatch.clone(),
            Box::new(ids),
            stale,
            self.updaters.clone(),
            Box::new(
                move |id: &Id, alive: &Rc<()>, gate: &super::reactive::Gate| {
                    let parent = Rc::clone(&parent_focus);
                    let items = Rc::clone(&items);
                    let key = id.clone();
                    let focus: Focus<R, I> = Rc::new(move |state| {
                        (parent)(state).and_then(|value| items(value).get(&key))
                    });
                    let path = located.as_ref().map(|located| {
                        let mut path = located.clone();
                        path.push(Segment::Item(id.clone()));
                        path
                    });
                    let mut cx = ViewCx {
                        env: env.living(alive),
                        focus,
                        updaters: updaters.clone(),
                        gate: Some(gate.clone()),
                        action_bindings: action_bindings.clone(),
                        item: Some(id.clone()),
                        path,
                        controls: Rc::clone(&controls),
                        forms: Forms::default(),
                        stages: stages.clone(),
                        feeds: feeds.clone(),
                        bindings: bindings.clone(),
                    };
                    row(&mut cx)
                },
            ),
        )
    }
}

/// How a scope reads the collection [`ViewCx::each`] shows.
type ItemsOf<T, I> = Rc<dyn Fn(&T) -> &Collection<I>>;

/// Where `target` sits among the fields of `value`, through nested records:
/// the path of the collection a view's `each` reads.
fn locate<T: Model + 'static, I: 'static>(
    value: &T,
    target: &Collection<I>,
) -> Option<Vec<Segment>> {
    fn walk<T: Model + 'static>(
        value: &T,
        shape: tilo_resource::Shape,
        prefix: &mut Vec<Segment>,
        target: *const u8,
    ) -> bool {
        for field in shape.fields() {
            prefix.push(Segment::Field(field.key.to_owned()));
            let found = match field.shape {
                tilo_resource::Shape::Collection(_) => value
                    .get_at(prefix)
                    .is_some_and(|at| std::ptr::eq(std::ptr::from_ref(at).cast::<u8>(), target)),
                tilo_resource::Shape::Record(_) => walk(value, field.shape, prefix, target),
                tilo_resource::Shape::Value
                | tilo_resource::Shape::Keyed(_)
                | tilo_resource::Shape::Map(_) => false,
            };
            if found {
                return true;
            }
            prefix.pop();
        }
        false
    }
    let mut path = Vec::new();
    walk(
        value,
        T::SHAPE,
        &mut path,
        std::ptr::from_ref(target).cast::<u8>(),
    )
    .then_some(path)
}

/// Binding controls to the record's fields by path.
impl<R, T: 'static> ViewCx<R, T>
where
    R: Resource<State = R> + Model,
{
    /// Keep `sink` in step with the value at `path`, read again only after
    /// changes that touched it.
    fn follow_path<V: PartialEq + Default + Clone + 'static>(
        &self,
        path: &Path<R, V>,
        sink: impl FnMut(V) + 'static,
    ) -> V {
        let read = path.clone();
        self.follow_state(
            Reads::At(path.segments().to_vec()),
            move |state| read.get(state).cloned().unwrap_or_default(),
            sink,
        )
    }

    /// A stable, addressable control name for a control bound to `path`:
    /// its fields and items joined, numbered only when the path repeats.
    fn path_control_id<V>(&self, path: &Path<R, V>) -> String {
        let base = path
            .segments()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("-");
        let mut seen = self.controls.borrow_mut();
        let count = seen.entry(base.clone()).or_insert(0);
        *count = count.saturating_add(1);
        if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        }
    }

    /// The value at `path`, as local state that follows it: for any toolkit
    /// control built from a [`LocalState`], or a part of the view that shows
    /// it. It is read again only after changes that touched it.
    pub fn value_at<V>(&self, path: &Path<R, V>) -> LocalState<V>
    where
        V: PartialEq + Default + Clone + 'static,
    {
        let state = LocalState::new(V::default());
        let sink = state.clone();
        let initial = self.follow_path(path, move |value| sink.set(value));
        state.set(initial);
        state
    }

    /// Route a serialized screen's text inputs over `ui_path` to the text
    /// field at `path` ([`UiValues::route`](ui_core::UiValues::route)): they
    /// show it and each edit replaces it.
    pub fn route_graph_text(
        &self,
        values: &ui_core::UiValues,
        ui_path: &str,
        path: Path<R, String>,
    ) {
        let shown = self.value_at(&path);
        if let Some(binding) = self.actions().edit_binding(path, Path::replace) {
            values.route(ui_path, shown, binding);
        }
    }

    /// Route a serialized screen's checked controls over `ui_path` to the
    /// flag at `path`: they show it and each toggle sets it.
    pub fn route_graph_bool(&self, values: &ui_core::UiValues, ui_path: &str, path: Path<R, bool>) {
        let shown = self.value_at(&path);
        if let Some(binding) = self.actions().edit_binding(path, Path::set) {
            values.route_bool(ui_path, shown, binding);
        }
    }

    /// A toolkit control whose value is not the field's own type: a slider
    /// (`f64`) for an optional hue (`Option<f64>`), a dropdown of names for
    /// a flag. It shows `show` of the value at `path` and sets the field to
    /// `store` of each committed value:
    /// `cx.control_map(Slider::from_state, p.hue, |hue| hue.unwrap_or(0.5), Some)`.
    pub fn control_map<V, I, C>(
        &self,
        make: impl FnOnce(&LocalState<I>) -> C,
        path: Path<R, V>,
        show: impl Fn(&V) -> I + 'static,
        store: impl Fn(I) -> V + Send + Sync + 'static,
    ) -> C
    where
        V: Settable + PartialEq + Default + Clone + 'static,
        I: PartialEq + Clone + Send + Sync + 'static,
        C: ActionControl<I>,
    {
        let read = path.clone();
        let initial = show(&V::default());
        let state = LocalState::new(initial);
        let sink = state.clone();
        let shown = self.follow_state(
            Reads::At(path.segments().to_vec()),
            move |record| show(&read.get(record).cloned().unwrap_or_default()),
            move |value| sink.set(value),
        );
        state.set(shown);
        let control = make(&state);
        let binding = self
            .actions()
            .edit_binding(path.cast::<I>(), move |path, input| {
                path.clone().cast::<V>().set(store(input))
            });
        match binding {
            Some(binding) => control.bind_action(binding),
            None => control.unavailable(),
        }
    }
}

/// What a control shows and changes: a field of the record, by its
/// [`Path`], or a field of a form ([`ViewCx::form`]), by the path into the
/// verb's parameters. `M` tells the two apart and is always inferred.
pub trait Bindable<R: Resource, V, M>: Sized {
    #[doc(hidden)]
    fn attach<T: 'static>(
        self,
        cx: &ViewCx<R, T>,
        edit: fn(&Path<R, V>, V) -> Edit<R>,
    ) -> Attached<V>;
}

impl<R, V> Bindable<R, V, RecordField> for Path<R, V>
where
    R: Resource<State = R> + Model,
    V: PartialEq + Default + Clone + Send + Sync + 'static,
{
    fn attach<T: 'static>(
        self,
        cx: &ViewCx<R, T>,
        edit: fn(&Path<R, V>, V) -> Edit<R>,
    ) -> Attached<V> {
        let id = cx.path_control_id(&self);
        let state = cx.value_at(&self);
        let binding = cx.actions().edit_binding(self, edit).map(Box::new);
        Attached::Record { id, state, binding }
    }
}

impl<R, P, V> Bindable<R, V, FormField> for Path<P, V>
where
    R: Resource,
    P: Params<Resource = R>,
    V: serde::Serialize + Default + Send + Sync + 'static,
{
    fn attach<T: 'static>(
        self,
        cx: &ViewCx<R, T>,
        _edit: fn(&Path<R, V>, V) -> Edit<R>,
    ) -> Attached<V> {
        let state = cx.form::<P>().field(&self);
        let id = std::iter::once(P::ACTION.id.to_owned())
            .chain(self.segments().iter().map(ToString::to_string))
            .collect::<Vec<_>>()
            .join("-");
        Attached::Local { id, state }
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
