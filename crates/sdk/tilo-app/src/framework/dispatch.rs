//! Submitting edits: every change a view makes is an action the host checks
//! against permissions, applies, records for undo and history, and
//! replicates.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{
    ActionAddress, ActionContext, ActionError, ActionInvoker, ControlInvocation,
    PermissionRequirement,
};
use tilo_resource::{Effect, Invoke, Resource};
use ui_core::LocalState;
use ui_core::automerge::ChangeHash;
use ui_core::permissions::{LivePermissions, PermissionScope, Verb};
use ui_core::widget::Edits;
use ui_core::widget::controls::AttributedControlInvocation;

/// Submits a view's edits. Cloneable; clones share one queue.
///
/// Two doors, one path. [`invoke`](Self::invoke) and [`apply`](Self::apply)
/// queue an invocation the host resolves after the event, exactly as a
/// control bound to an action would. [`apply_now`](Self::apply_now) applies
/// one typed action immediately, inside the event or the settle pass
/// ([`Widget::settle`](ui_core::widget::Widget::settle)), for an editor that must
/// see the result of its own edit before the next keystroke (the paragraph a
/// split created, the cell a table insert made). Both check the
/// participant's permission, apply through the replayable executor with
/// attribution, and are undone per author.
///
/// # A drag as one undo step
///
/// A slider or a drag handle a surface draws itself applies each value it
/// passes through with [`apply_now`](Self::apply_now), so the document — and
/// every collaborator — follows the drag live. The resource declares that
/// one value continues the one before it ([`Resource::coalesces`]: the same
/// field of the same item), which makes the whole drag one step, and the
/// surface calls [`settle`](Self::settle) when the pointer is let go, so the
/// next drag is a step of its own:
///
/// ```ignore
/// fn coalesces(previous: &Action, next: &Action) -> bool {
///     matches!((previous, next), (Action::SetLevel(..), Action::SetLevel(..)))
/// }
///
/// // In the surface's `handle_event`:
/// WindowEvent::CursorMoved { .. } if self.dragging => {
///     let _ = self.dispatch.apply_now(&mut ctx.edits(), &Action::SetLevel(level));
/// }
/// WindowEvent::MouseInput { state: ElementState::Released, .. } => {
///     self.dragging = false;
///     self.dispatch.settle();
/// }
/// ```
///
/// A toolkit slider bound with `ViewCx::control` instead invokes its action
/// once per drag when built with `Slider::commits_on_release`.
pub struct Dispatcher<R: Resource> {
    /// Everything that does not name `R`'s actions, compiled once in the
    /// shared runtime rather than in every app.
    core: DispatchCore,
    /// The invocations of the open group, and how many groups are open.
    group: Rc<RefCell<(usize, Vec<Invoke<R>>)>>,
    /// What applying an action needs: the resource's own
    /// [`Resource::permission`], or for a child its parent's action's.
    permission_of: PermissionOf<R>,
    /// Applies one immediate action: the resource's own, or for a child
    /// resource its parent's action carrying it.
    submit_now: SubmitNow<R>,
    /// The last immediate action and its serial, to tell whether the next
    /// continues it.
    last: Rc<RefCell<Option<Serialed<R>>>>,
}

/// The part of a [`Dispatcher`] that does not depend on the resource type:
/// the queue, permissions, refusals and undo-step bookkeeping. Clones share
/// everything but the custody a dispatcher lent to an effect carries.
pub(crate) struct DispatchCore {
    app_id: &'static str,
    resource_id: Rc<str>,
    /// [`Resource::KIND`] of the resource edited here.
    kind: &'static str,
    /// [`Resource::LABEL`] of the resource edited here, for refusals.
    label: &'static str,
    /// The readable name published by the resource host, shared with surfaces.
    resource_label: LocalState<String>,
    /// Who edits through this dispatcher; `None` in a view of a past version.
    me: Option<Rc<str>>,
    /// A dispatcher lent to an admitted effect keeps its original custody.
    /// Clones cannot turn later work into the host participant's invocation.
    /// Share the immutable scope rather than embedding its owner/lease in every
    /// view dispatcher: copying the full scope also inflates construction stacks.
    custody: Option<Rc<ui_core::invocation_scope::ResourceInvocationScope>>,
    /// Each queued invocation with the effect invocation it continues,
    /// when an effect's handler or completion queued it.
    queue: Rc<RefCell<VecDeque<AttributedControlInvocation>>>,
    read_only: LocalState<bool>,
    /// The participant's permissions as the host keeps them, for an action
    /// that needs less than writing (a reply a commenter may add) or more.
    /// Absent outside a host: a past version, which nobody edits, or a
    /// test's detached dispatcher, which may do what writing allows.
    permissions: Rc<RefCell<Option<LivePermissions>>>,
    permission_states: PermissionStates,
    refused: LocalState<Option<Refused>>,
    host: Rc<RefCell<ui_core::action_host::ActionHost>>,
    /// Counts the immediate actions of the resource and its children, so a
    /// dispatcher can tell that nothing came between its last action and
    /// its next.
    serial: Rc<Cell<u64>>,
    /// Every immediate action since the last undo boundary continued the one
    /// before it ([`Resource::coalesces`]): one step, still open.
    continuing: Rc<Cell<bool>>,
    /// An immediate action was applied since the last boundary was taken.
    applied: Rc<Cell<bool>>,
    /// The boundary last taken opened a step that stays open.
    opened: Rc<Cell<bool>>,
    /// The view shows a past version in History, not the live resource.
    past: bool,
    /// The last immediate action may be continued by a later one
    /// ([`Resource::continuable`]).
    continuable: Rc<Cell<bool>>,
}

/// What applying one action needs.
type PermissionOf<R> = Rc<dyn Fn(&<R as Resource>::Action) -> PermissionRequirement>;

/// One observable per requested permission, shared by this resource's views.
type PermissionStates = Rc<RefCell<Vec<(PermissionRequirement, LocalState<bool>)>>>;

/// An immediate action and the serial it was applied at.
type Serialed<R> = (u64, <R as Resource>::Action);

/// Applies one typed action to the document a pass may change, through
/// the host.
type SubmitNow<R> = Rc<
    dyn Fn(
        &mut ui_core::action_host::ActionHost,
        &mut Edits<'_>,
        &<R as Resource>::Action,
        ActionContext,
    ) -> Result<(), ActionError>,
>;

impl<R: Resource> Clone for Dispatcher<R> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            group: Rc::clone(&self.group),
            permission_of: Rc::clone(&self.permission_of),
            submit_now: Rc::clone(&self.submit_now),
            last: Rc::clone(&self.last),
        }
    }
}

// Written out rather than derived: a derived `clone` is inlined into every
// app's generic dispatcher, and this one is compiled once in the runtime.
impl Clone for DispatchCore {
    fn clone(&self) -> Self {
        Self {
            app_id: self.app_id,
            resource_id: Rc::clone(&self.resource_id),
            kind: self.kind,
            label: self.label,
            resource_label: self.resource_label.clone(),
            me: self.me.clone(),
            custody: self.custody.clone(),
            queue: Rc::clone(&self.queue),
            read_only: self.read_only.clone(),
            permissions: Rc::clone(&self.permissions),
            permission_states: Rc::clone(&self.permission_states),
            refused: self.refused.clone(),
            host: Rc::clone(&self.host),
            serial: Rc::clone(&self.serial),
            continuing: Rc::clone(&self.continuing),
            applied: Rc::clone(&self.applied),
            opened: Rc::clone(&self.opened),
            past: self.past,
            continuable: Rc::clone(&self.continuable),
        }
    }
}

impl DispatchCore {
    fn new(app_id: &'static str, resource_id: &str, read_only: bool, me: Option<&str>) -> Self {
        Self {
            app_id,
            resource_id: Rc::from(resource_id),
            kind: "",
            label: "",
            resource_label: LocalState::new(String::new()),
            me: me.map(Rc::from),
            custody: None,
            queue: Rc::default(),
            read_only: LocalState::new(read_only),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            refused: LocalState::new(None),
            host: Rc::default(),
            serial: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            past: false,
            continuable: Rc::default(),
        }
    }

    /// The core of a resource of `kind` labelled `label`: a new one, or a
    /// child at `resource_id` sharing this one's queue, permissions and
    /// undo-step bookkeeping.
    fn of_kind(mut self, resource_id: &str, kind: &'static str, label: &'static str) -> Self {
        self.resource_id = Rc::from(resource_id);
        self.kind = kind;
        self.label = label;
        self.resource_label = LocalState::new(label.to_owned());
        self
    }

    fn queue_invocation(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        let on_behalf_of = match &self.custody {
            Some(custody) => {
                if let Err(error) = custody.check() {
                    self.refuse(&address.action, error);
                    return;
                }
                Some(custody.invocation())
            }
            None => super::effect::acting_for(),
        };
        self.queue
            .borrow_mut()
            .push_back((ControlInvocation { address, arguments }, on_behalf_of));
    }

    /// Queue invocation `id` of this resource with `arguments`.
    fn submit(&self, id: &str, arguments: serde_json::Map<String, serde_json::Value>) {
        self.queue_invocation(ActionAddress::new(self.resource_id.as_ref(), id), arguments);
    }

    /// Refuse `id` when the custody this dispatcher was lent with no longer
    /// holds. Returns whether it was refused.
    fn custody_refuses(&self, id: &str) -> bool {
        if let Some(custody) = &self.custody
            && let Err(error) = custody.check()
        {
            self.refuse(id, error);
            return true;
        }
        false
    }

    pub(crate) fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    fn past(&mut self) {
        self.past = true;
        self.read_only.set(true);
        self.permission_states = Rc::default();
    }

    pub(crate) fn is_past(&self) -> bool {
        self.past
    }

    pub(crate) fn read_only(&self) -> bool {
        self.read_only.get()
    }

    fn read_only_state(&self) -> LocalState<bool> {
        self.read_only.clone()
    }

    /// Follow the participant's permissions as the host keeps them.
    pub(crate) fn follow(&self, permissions: LivePermissions) {
        *self.permissions.borrow_mut() = Some(permissions);
        self.refresh_permission_states();
    }

    fn permission_state(&self, requirement: PermissionRequirement) -> LocalState<bool> {
        let mut states = self.permission_states.borrow_mut();
        if let Some((_, state)) = states.iter().find(|(needed, _)| *needed == requirement) {
            return state.clone();
        }
        let state = LocalState::new(self.may(requirement));
        states.push((requirement, state.clone()));
        state
    }

    pub(crate) fn refresh_permission_states(&self) {
        for (requirement, state) in self.permission_states.borrow().iter() {
            let allowed = self.may(*requirement);
            if state.get() != allowed {
                state.set(allowed);
            }
        }
    }

    pub(crate) fn may(&self, requirement: PermissionRequirement) -> bool {
        let verb = match requirement {
            PermissionRequirement::None => return true,
            PermissionRequirement::Read => Verb::Read,
            PermissionRequirement::Comment => Verb::Comment,
            PermissionRequirement::Write => Verb::Write,
            PermissionRequirement::Invite => Verb::Invite,
            PermissionRequirement::Admin => Verb::Admin,
        };
        match self.permissions.borrow().as_ref() {
            Some(permissions) => permissions.check(&PermissionScope::app(self.app_id, verb)),
            // A detached dispatcher may what writing allows; a past version
            // nothing.
            None => !self.read_only.get(),
        }
    }

    /// Re-read whether the participant may write. Returns whether it
    /// changed.
    #[cfg(any(test, feature = "test-support"))]
    fn refresh_read_only(&self) -> bool {
        let writes =
            self.permissions.borrow().as_ref().map(|permissions| {
                permissions.check(&PermissionScope::app(self.app_id, Verb::Write))
            });
        match writes {
            Some(writes) => self.set_read_only(!writes),
            None => false,
        }
    }

    /// Record a change of the participant's permissions. Returns whether
    /// anything changed.
    pub(crate) fn set_read_only(&self, read_only: bool) -> bool {
        if self.read_only.get() == read_only {
            return false;
        }
        self.read_only.set(read_only);
        true
    }

    fn refused(&self) -> LocalState<Option<Refused>> {
        self.refused.clone()
    }

    fn view_only(&self) -> String {
        format!("this {} is open to view only", self.label.to_lowercase())
    }

    /// Why an edit needing `requirement` was refused.
    fn not_allowed(&self, requirement: PermissionRequirement) -> String {
        match requirement {
            PermissionRequirement::Write => self.view_only(),
            PermissionRequirement::Comment => {
                format!("you may not comment on this {}", self.label.to_lowercase())
            }
            _ => format!("you may not do this to this {}", self.label.to_lowercase()),
        }
    }

    /// Record that the door refused `action`, submitted here.
    pub(crate) fn refuse(&self, action: &str, reason: String) {
        self.refused.set(Some(Refused {
            action: action.to_owned(),
            reason,
        }));
    }

    fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.queue.borrow_mut().push_back((
            ui_core::search::open_search_invocation(request),
            super::effect::acting_for(),
        ));
    }

    fn invoke_at(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        if self.read_only() {
            return;
        }
        self.queue_invocation(address, arguments);
    }

    /// The context an immediate action labelled `label` applies with.
    fn labelled(&self, label: String) -> ActionContext {
        ActionContext::for_resource(
            label,
            self.resource_id.as_ref(),
            self.kind,
            self.resource_label.get(),
        )
    }

    /// Whether an immediate action may apply here now: not from an effect's
    /// custody, and with `requirement` granted.
    fn admit_now(&self, requirement: PermissionRequirement) -> Result<(), ActionError> {
        if self.custody.is_some() {
            return Err(ActionError::Refused(
                "caller-scoped effects submit document changes through queued actions".into(),
            ));
        }
        if !self.may(requirement) {
            return Err(ActionError::Refused(self.not_allowed(requirement)));
        }
        Ok(())
    }

    /// `context` naming the resource and who edited it, as history does
    /// even when the surface's context does not.
    fn named(&self, context: ActionContext) -> ActionContext {
        let mut named = ActionContext::for_resource(
            "",
            self.resource_id.as_ref(),
            self.kind,
            self.resource_label.get(),
        );
        if let Some(me) = &self.me {
            named = named.with_invoker(ActionInvoker::actor(me.as_ref()));
        }
        context.enriched_by(&named)
    }

    /// Count an immediate action that is a step: whether it `continues` the
    /// last one and may be `continuable` by the next. Answers its serial.
    fn stepped(&self, continues: bool, continuable: bool) -> u64 {
        let serial = self.serial.get().wrapping_add(1);
        self.serial.set(serial);
        self.continuable.set(continuable);
        // The event continues the open step only if every action it applied
        // did.
        let first_of_event = !self.applied.replace(true);
        self.continuing
            .set(continues && (first_of_event || self.continuing.get()));
        serial
    }

    /// The next queued invocation, with the effect invocation it continues.
    pub(crate) fn take_attributed(&self) -> Option<AttributedControlInvocation> {
        self.queue.borrow_mut().pop_front()
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.queue.borrow().is_empty()
    }

    /// The undo boundary before this event's first immediate action, which
    /// opens a step; none while the event's actions continue the open step.
    pub(crate) fn take_committed(&self) -> Option<Vec<ChangeHash>> {
        let bracket = self.host.borrow_mut().take_committed();
        let continuing = self.continuing.replace(false);
        self.opened
            .set(bracket.is_some() && !continuing && self.continuable.get());
        self.applied.set(false);
        if continuing { None } else { bracket }
    }

    /// Compensating document actions for [`Self::take_committed`]'s bracket.
    pub(crate) fn take_action_revert(
        &self,
    ) -> Option<(tilo_actions::EncodedAction, tilo_actions::EncodedAction)> {
        self.host.borrow_mut().take_action_revert()
    }

    /// The step the last boundary opened stays open for the actions that
    /// continue it ([`Resource::coalesces`]).
    pub(crate) fn keeps_step_open(&self) -> bool {
        self.opened.get()
    }

    pub(crate) fn name_resource(&self, label: &str) {
        self.resource_label.set(label.to_owned());
    }

    pub(crate) fn resource_id(&self) -> &str {
        &self.resource_id
    }

    pub(crate) fn app_id(&self) -> &'static str {
        self.app_id
    }
}

impl<R: Resource> Dispatcher<R> {
    pub(crate) fn for_custody(
        &self,
        custody: ui_core::invocation_scope::ResourceInvocationScope,
    ) -> Self {
        let mut dispatcher = self.clone();
        dispatcher.core.custody = Some(Rc::new(custody));
        // An effect's group cannot absorb the person's unrelated input.
        dispatcher.group = Rc::default();
        dispatcher
    }

    pub(crate) fn new(
        app_id: &'static str,
        resource_id: &str,
        read_only: bool,
        me: Option<&str>,
    ) -> Self {
        Self {
            core: DispatchCore::new(app_id, resource_id, read_only, me).of_kind(
                resource_id,
                R::KIND,
                R::LABEL,
            ),
            group: Rc::default(),
            permission_of: Rc::new(R::permission),
            submit_now: Rc::new(|host, edits, action, context| {
                edits.apply(host, R::ACTION_TYPE, action, context)
            }),
            last: Rc::default(),
        }
    }

    /// The part of this dispatcher that does not depend on the resource
    /// type, which the root keeps.
    pub(crate) fn core(&self) -> &DispatchCore {
        &self.core
    }

    /// A dispatcher for a surface a test builds outside any host, editing as
    /// `me`: what the surface applies now ([`Self::apply_now`]) goes into the
    /// document the test hands it, as under a host. Queued edits
    /// ([`Self::apply`], [`Self::invoke`]) wait for a host that never takes
    /// them; a test that needs those runs the app
    /// ([`TestApp`](super::testing::HeadlessApp)).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn detached(app_id: &'static str, me: &str) -> Self {
        Self::new(app_id, R::KIND, false, Some(me))
    }

    /// Who edits through this dispatcher: the participant this view is
    /// for, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). `None` in a
    /// view of a past version, which nobody edits.
    pub fn me(&self) -> Option<&str> {
        self.core.me()
    }

    /// A dispatcher for a past version in History: nothing it is given is
    /// ever applied.
    pub(crate) fn past(mut self) -> Self {
        self.core.past();
        self
    }

    /// Whether the view shows a past version in History mode, drawn from a
    /// frozen document. Such a view must start no work: no worker, no
    /// network, no files. A surface that would fetch or watch something
    /// presents what the document holds instead.
    pub fn is_past(&self) -> bool {
        self.core.is_past()
    }

    /// Whether edits from this participant will be refused, as of now: it
    /// follows role changes while the app runs. Surfaces use it to present
    /// themselves read-only instead of letting an edit bounce.
    pub fn read_only(&self) -> bool {
        self.core.read_only()
    }

    /// [`Self::read_only`] as observable state, for a widget that shows or
    /// disables something while the participant may only view.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.core.read_only_state()
    }

    /// Whether the host currently grants `requirement`, as observable view
    /// state. It follows changes even when writing remains allowed and the
    /// resource document does not change. Bind it to a conditional pane or
    /// label; action invocation still checks the host's current authority.
    pub fn permission_state(&self, requirement: PermissionRequirement) -> LocalState<bool> {
        self.core.permission_state(requirement)
    }

    /// The same dispatcher deciding by `permissions`, for a test of a
    /// surface whose participant may only comment, or only view.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_permissions(self, permissions: LivePermissions) -> Self {
        self.core.follow(permissions);
        self.core.refresh_read_only();
        self
    }

    /// Whether the participant may, as of now, do what needs `requirement`:
    /// writing for most edits, commenting for a reply, administering for a
    /// change of who may do what.
    pub fn may(&self, requirement: PermissionRequirement) -> bool {
        self.core.may(requirement)
    }

    /// Whether the participant may apply `action`
    /// ([`Resource::permission`]).
    pub fn may_apply(&self, action: &R::Action) -> bool {
        self.core.may((self.permission_of)(action))
    }

    /// The last edit or effect submitted here that was refused — denied
    /// permission, unavailable, failed — and why, for a view to show. An
    /// edit waiting for the user's confirmation is not refused.
    pub fn refused(&self) -> LocalState<Option<Refused>> {
        self.core.refused()
    }

    /// Record that the door refused `action`, submitted here.
    pub(crate) fn refuse(&self, action: &str, reason: String) {
        self.core.refuse(action, reason);
    }

    /// Invoke one of the app's effects (`dispatcher.effect(&Backups::BackUp)`)
    /// through the same door as a button bound to it: permission, the
    /// effect's availability and a destructive effect's confirmation are
    /// checked, then its handler runs. A participant who may only view still
    /// invokes the effects declared `permission = "read"`.
    pub fn effect<E: Effect>(&self, effect: &E) {
        self.core.submit(effect.id(), effect.arguments());
    }

    /// Invoke `effect` as the live row `row` offers it
    /// ([`LiveRow::effect`](super::LiveRow::effect)): under the row's
    /// contract, so a confirmation the row asks for (typing the name of the
    /// device it erases) is asked here too.
    pub fn effect_on<E: Effect>(&self, row: &str, effect: &E) {
        self.core
            .queue_invocation(ActionAddress::new(row, effect.id()), serde_json::Map::new());
    }

    /// Invoke a declared action (`dispatcher.invoke(Note::EDIT.with(text))`),
    /// or an effect ([`Invoke::effect`]), which reaches its handler as
    /// [`Self::effect`] does.
    pub fn invoke(&self, invoke: Invoke<R>) {
        if self.core.custody_refuses(invoke.id) {
            return;
        }
        if invoke.is_effect() {
            self.submit(invoke);
            return;
        }
        if !self.may_invoke(&invoke) {
            self.core.refuse(invoke.id, self.core.view_only());
            return;
        }
        {
            let mut group = self.group.borrow_mut();
            if group.0 > 0 {
                group.1.push(invoke);
                return;
            }
        }
        self.submit(invoke);
    }

    fn submit(&self, invoke: Invoke<R>) {
        self.core.submit(invoke.id, invoke.arguments);
    }

    /// Whether the participant may take `invoke`: what its declaration, or
    /// the typed value it applies, needs. One this resource does not declare
    /// (an edit bound to a surface's view state) is left to the host, which
    /// checks what its published action needs.
    fn may_invoke(&self, invoke: &Invoke<R>) -> bool {
        if invoke.id == tilo_resource::APPLY {
            return match invoke.build(0) {
                Ok(action) => self.may_apply(&action),
                Err(_) => !self.read_only(),
            };
        }
        match R::declarations().iter().find(|decl| decl.id == invoke.id) {
            Some(decl) => self.may(decl.permission),
            None if invoke.is_group() => !self.read_only(),
            None => self.may(PermissionRequirement::Read),
        }
    }

    /// Open the platform search with `request`, narrowed to this app or one
    /// part of it (`SearchRequest::within`). Searching is a view, so a
    /// participant who may only view can search too.
    pub fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.core.open_search(request);
    }

    /// Invoke an action published on a nested resource, such as one item of
    /// a collection.
    pub(crate) fn invoke_at(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        self.core.invoke_at(address, arguments);
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry. The door applies it after the
    /// event; if it refuses it, [`Self::refused`] says why.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::checked_apply(action) {
            Ok(invoke) => self.invoke(invoke),
            Err(error) => self.core.refuse(tilo_resource::APPLY, error.to_string()),
        }
    }

    /// Submit every edit `edits` makes through this dispatcher (or a clone
    /// of it) as one action: one change, one version in history, one undo
    /// step. For a gesture that is several edits — split a linked clip pair,
    /// delete a selection — which the user takes back with one Ctrl+Z.
    ///
    /// One action is one undo step by default; a group is the explicit
    /// exception. Groups nest: the outermost one submits. A group of one
    /// edit submits that edit unchanged.
    pub fn group<T>(&self, edits: impl FnOnce() -> T) -> T {
        let _group = self.begin_group();
        edits()
    }

    /// Open a group that submits when the returned guard drops: the
    /// [`group`](Self::group) for code whose edits are spread over a scope,
    /// such as one input event of a custom surface.
    #[must_use = "the group submits when this guard drops"]
    pub fn begin_group(&self) -> DispatchGroup<R> {
        let mut group = self.group.borrow_mut();
        group.0 = group.0.saturating_add(1);
        drop(group);
        DispatchGroup {
            dispatcher: self.clone(),
        }
    }

    fn end_group(&self) {
        let members = {
            let mut group = self.group.borrow_mut();
            group.0 = group.0.saturating_sub(1);
            if group.0 > 0 {
                return;
            }
            std::mem::take(&mut group.1)
        };
        let mut members = members.into_iter();
        match (members.next(), members.next()) {
            (None, _) => {}
            (Some(only), None) => self.submit(only),
            (Some(first), Some(second)) => {
                self.submit(Invoke::group([first, second].into_iter().chain(members)));
            }
        }
    }

    /// Apply one typed action to the document `edits` lends — an event's,
    /// or a settle pass's — now, attributed to the participant and labelled
    /// after the action (its declared label when it names a declared one).
    /// One event's actions are one undo step, and an action that continues
    /// the previous one ([`Resource::coalesces`], the next letter of a word)
    /// joins its step.
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] for a participant who may only view,
    /// and the action's own refusal; nothing is written either way.
    pub fn apply_now(&self, edits: &mut Edits<'_>, action: &R::Action) -> Result<(), ActionError> {
        let label = tilo_resource::action_label::<R>(action)
            .unwrap_or_else(|| ActionContext::for_action("", "", action).action_label);
        self.apply_now_with(edits, action, self.core.labelled(label))
    }

    /// [`apply_now`](Self::apply_now) with the history context the surface
    /// already has (its app, a purpose, a work group).
    ///
    /// # Errors
    /// As [`apply_now`](Self::apply_now).
    pub fn apply_now_with(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
        context: ActionContext,
    ) -> Result<(), ActionError> {
        self.core.admit_now((self.permission_of)(action))?;
        let context = self.core.named(context);
        (self.submit_now)(&mut self.core.host.borrow_mut(), edits, action, context)?;
        // Content from outside is nobody's step: it neither continues the
        // open one nor ends it.
        if !tilo_actions::Action::change_kind(action).is_step() {
            return Ok(());
        }
        // Only an action right after this dispatcher's last one, with nothing
        // in between, can continue it.
        let serial = self.core.serial.get();
        let continues = self
            .last
            .borrow()
            .as_ref()
            .is_some_and(|(at, previous)| *at == serial && R::coalesces(previous, action));
        let serial = self.core.stepped(continues, R::continuable(action));
        *self.last.borrow_mut() = Some((serial, action.clone()));
        Ok(())
    }

    /// The dispatcher of a resource nested in this one at `id` — a note's
    /// rich-text body, a draft's SQL — whose every edit is this resource's
    /// action `wrap(edit)`.
    ///
    /// The child is edited like a root: its named actions,
    /// [`apply`](Self::apply), [`group`](Self::group) and
    /// [`apply_now`](Self::apply_now) take `C`'s own actions, coalesce into
    /// undo steps by `C`'s [`Resource::coalesces`], and history names them
    /// after `C`'s labels. Underneath, each applies as this resource's
    /// action: the same permission check, history, undo and replay. Queued
    /// invocations are addressed to the child resource `id`, so this
    /// resource publishes the child at the same id with the same `wrap`
    /// ([`tilo_resource::publish_child`] in [`Resource::children`]), which
    /// is also what lets the palette, agents and scripts reach it.
    #[must_use]
    pub fn child<C: Resource>(
        &self,
        id: &str,
        wrap: impl Fn(C::Action) -> R::Action + 'static,
    ) -> Dispatcher<C> {
        let parent = Rc::clone(&self.submit_now);
        let parent_permission = Rc::clone(&self.permission_of);
        let wrap = Rc::new(wrap);
        let wrap_for_permission = Rc::clone(&wrap);
        Dispatcher {
            core: self.core.clone().of_kind(id, C::KIND, C::LABEL),
            group: Rc::default(),
            permission_of: Rc::new(move |action: &C::Action| {
                parent_permission(&wrap_for_permission(action.clone()))
            }),
            // History names the child and its edit; what applies is the
            // parent's action carrying it.
            submit_now: Rc::new(move |host, edits, action: &C::Action, context| {
                parent(host, edits, &wrap(action.clone()), context)
            }),
            last: Rc::default(),
        }
    }

    /// End the gesture the last immediate actions belonged to: the pointer
    /// was let go of a slider or a drag handle. The next action starts a
    /// step of its own even when it would continue the last one
    /// ([`Resource::coalesces`]), so two drags of one slider are two undo
    /// steps while every value one drag passes through is one.
    pub fn settle(&self) {
        self.last.borrow_mut().take();
    }

    /// The next queued invocation, with the effect invocation it continues.
    pub(crate) fn take_attributed(&self) -> Option<AttributedControlInvocation> {
        self.core.take_attributed()
    }

    pub(crate) fn resource_id(&self) -> &str {
        self.core.resource_id()
    }

    /// The id of the app whose resource this is, which presence keys name
    /// ([`ResourceSelection::presence`](super::ResourceSelection::presence)).
    pub fn app_id(&self) -> &'static str {
        self.core.app_id()
    }
}

/// An edit or effect the door refused ([`Dispatcher::refused`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// The id of the refused action or effect.
    pub action: String,
    /// Why, for a person.
    pub reason: String,
}

/// An open [`Dispatcher::begin_group`]: every edit submitted until it drops
/// becomes one action.
pub struct DispatchGroup<R: Resource> {
    dispatcher: Dispatcher<R>,
}

impl<R: Resource> Drop for DispatchGroup<R> {
    fn drop(&mut self) {
        self.dispatcher.end_group();
    }
}
