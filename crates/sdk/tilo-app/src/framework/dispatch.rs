//! Submitting edits: every change a view makes is an action the host checks
//! against permissions, applies, records for undo and history, and
//! replicates.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{
    ActionAddress, ActionBinding, ActionContext, ActionError, ActionInvoker, ActionNeed,
    ControlInvocation,
};

use super::controls::Actions;
use tilo_resource::{Edit, Effect, Invoke, Resource};
use ui_core::LocalState;
use ui_core::automerge::ChangeHash;
use ui_core::permissions::{ActionKind, LivePermissions, PermissionScope, TreePath, policy};

/// What a view asks whether its participant may do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Need(ActionNeed),
    Govern,
}
use ui_core::widget::Edits;
use ui_core::widget::controls::AttributedControlInvocation;

/// Something a dispatcher submits and a button is bound to: a field edit
/// made at a path (`p.favorite.set(true)`), a verb with its arguments, or an
/// invocation built some other way. No app writes an action id or an
/// argument map.
pub trait Submit<R: Resource>: Sized {
    /// The public id of the action this submits.
    fn action_id(&self) -> &'static str;

    /// The invocation of the host door this submits.
    ///
    /// # Errors
    /// Returns why it cannot be submitted.
    fn into_invoke(self) -> Result<Invoke<R>, String>;

    /// The binding of a button that submits this when pressed, or `None`
    /// (and a failure in tests) when the resource does not publish it.
    fn control_binding(self, actions: &Actions<R>) -> Option<ActionBinding<()>> {
        let id = self.action_id();
        match self.into_invoke() {
            Ok(invoke) => actions.control_invocation(invoke),
            Err(error) => {
                super::controls::unbound::<R>(&format!("`{id}`"), &error);
                None
            }
        }
    }
}

impl<R: Resource> Submit<R> for Invoke<R> {
    fn action_id(&self) -> &'static str {
        self.id
    }

    fn into_invoke(self) -> Result<Invoke<R>, String> {
        match self.refusal() {
            Some(refusal) => Err(refusal.to_owned()),
            None => Ok(self),
        }
    }
}

impl<R: Resource> Submit<R> for Edit<R> {
    fn action_id(&self) -> &'static str {
        tilo_resource::FIELD_EDIT
    }

    fn into_invoke(self) -> Result<Invoke<R>, String> {
        Invoke::edit(&self).map_err(|error| error.to_string())
    }
}

/// Submits a view's edits. Cloneable; clones share one queue.
///
/// Two doors, one path. [`submit`](Self::submit) and [`apply`](Self::apply)
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
    /// Everything that does not name the resource's action type, compiled
    /// once for every resource.
    core: DispatchCore,
    /// The invocations of the open group, and how many groups are open.
    group: Rc<RefCell<(usize, Vec<Invoke<R>>)>>,
    /// What applying an action needs: the resource's own
    /// [`Resource::permission`], or for a child its parent's action's.
    permission_of: PermissionOf<R>,
    /// Applies one immediate action: the resource's own, or for a child
    /// resource its parent's action carrying it.
    submit_now: SubmitNow<R>,
    /// Applies a group of the resource's own actions now as one action;
    /// `None` for a dispatcher of a nested or other record.
    group_now: Option<GroupNow<R>>,
    /// The last immediate action and its serial, to tell whether the next
    /// continues it.
    last: Rc<RefCell<Option<Serialed<R>>>>,
}

/// What a dispatcher keeps that does not name the resource's action type:
/// who edits, the door's queue, permissions, refusals and the undo-step
/// bookkeeping.
#[derive(Clone)]
pub(crate) struct DispatchCore {
    app_id: &'static str,
    resource_id: Rc<str>,
    /// [`Resource::KIND`] and [`Resource::LABEL`] of the resource.
    kind: &'static str,
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
    /// The last submission of this view the door applied.
    landed: LocalState<Option<Landed>>,
    /// What the effects that queued submissions here wait on: how many are
    /// still to land, per effect invocation.
    waits: Rc<RefCell<Waits>>,
    /// Why the record does not read as the resource, while it does not:
    /// nothing is submitted against a state nobody can see.
    unreadable: Rc<RefCell<Option<String>>>,
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
    /// The step the open undo step's changes record, decided before each
    /// change commits, so a coalesced step stays one step after a restart.
    gesture: Rc<Cell<Option<tilo_actions::ActionId>>>,
}

/// What applying one action needs.
type PermissionOf<R> = Rc<dyn Fn(&<R as Resource>::Action) -> ActionNeed>;

/// One observable per requested permission, shared by this resource's views.
type PermissionStates = Rc<RefCell<Vec<(Asked, LocalState<bool>)>>>;

/// An immediate action and the serial it was applied at.
type Serialed<R> = (u64, <R as Resource>::Action);

/// Applies a group of the resource's actions now, as one action.
type GroupNow<R> = Rc<
    dyn Fn(
        &mut ui_core::action_host::ActionHost,
        &mut Edits<'_>,
        &tilo_resource::ActionGroup<<R as Resource>::Action>,
        ActionContext,
    ) -> Result<(), ActionError>,
>;

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
            group_now: self.group_now.clone(),
            last: Rc::clone(&self.last),
        }
    }
}

impl DispatchCore {
    fn new(
        app_id: &'static str,
        resource_id: &str,
        (kind, label): (&'static str, &'static str),
        read_only: bool,
        me: Option<&str>,
    ) -> Self {
        Self {
            app_id,
            resource_id: Rc::from(resource_id),
            kind,
            label,
            resource_label: LocalState::new(label.to_owned()),
            me: me.map(Rc::from),
            custody: None,
            queue: Rc::default(),
            read_only: LocalState::new(read_only),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            refused: LocalState::new(None),
            landed: LocalState::new(None),
            waits: Rc::default(),
            unreadable: Rc::default(),
            host: Rc::default(),
            serial: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            past: false,
            continuable: Rc::default(),
            gesture: Rc::default(),
        }
    }

    /// The core of the dispatcher of another record than this resource's,
    /// published at `id` ([`Dispatcher::for_record`]).
    fn for_record(
        &self,
        id: &str,
        (kind, label): (&'static str, &'static str),
        read_only: bool,
    ) -> Self {
        Self {
            app_id: self.app_id,
            me: self.me.clone(),
            custody: self.custody.clone(),
            resource_id: Rc::from(id),
            kind,
            label,
            resource_label: LocalState::new(label.to_owned()),
            queue: Rc::clone(&self.queue),
            read_only: LocalState::new(read_only || self.past),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            refused: self.refused.clone(),
            landed: self.landed.clone(),
            waits: Rc::clone(&self.waits),
            unreadable: Rc::default(),
            host: Rc::clone(&self.host),
            serial: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            past: self.past,
            continuable: Rc::default(),
            gesture: Rc::default(),
        }
    }

    /// The core of the dispatcher of a resource nested in this one at `id`
    /// ([`Dispatcher::child`]): it shares this one's permissions and undo
    /// steps.
    fn child(&self, id: &str, (kind, label): (&'static str, &'static str)) -> Self {
        Self {
            resource_id: Rc::from(id),
            kind,
            label,
            resource_label: LocalState::new(label.to_owned()),
            ..self.clone()
        }
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
        if let Some(invocation) = on_behalf_of {
            self.waits.borrow_mut().submitted(invocation);
        }
        self.queue
            .borrow_mut()
            .push_back((ControlInvocation { address, arguments }, on_behalf_of));
    }

    pub(crate) fn note_continuation(
        &self,
        invocation: tilo_actions::InvocationId,
        action: &str,
        result: &Result<ui_core::host_operation::ResourceOperationStart, tilo_actions::DoorError>,
    ) {
        let landed = match result {
            Ok(ui_core::host_operation::ResourceOperationStart::Completed(output)) => {
                Ok(Landed::of(action, output))
            }
            // Handed on to the broker: it applies there, answering nothing here.
            Ok(ui_core::host_operation::ResourceOperationStart::Accepted) => Ok(Landed {
                action: action.to_owned(),
                answer: None,
                created: Vec::new(),
            }),
            Err(error) => Err(error.to_string()),
        };
        self.waits.borrow_mut().landed(invocation, landed);
    }

    fn past(&mut self) {
        self.past = true;
        self.read_only.set(true);
        self.permission_states = Rc::default();
    }

    pub(crate) fn follow(&self, permissions: LivePermissions) {
        *self.permissions.borrow_mut() = Some(permissions);
        self.refresh_permission_states();
    }

    fn asked_state(&self, asked: Asked) -> LocalState<bool> {
        let mut states = self.permission_states.borrow_mut();
        if let Some((_, state)) = states.iter().find(|(held, _)| *held == asked) {
            return state.clone();
        }
        let state = LocalState::new(self.may_asked(&asked));
        states.push((asked, state.clone()));
        state
    }

    pub(crate) fn refresh_permission_states(&self) {
        for (asked, state) in self.permission_states.borrow().iter() {
            let allowed = self.may_asked(asked);
            if state.get() != allowed {
                state.set(allowed);
            }
        }
    }

    fn may_asked(&self, asked: &Asked) -> bool {
        match asked {
            Asked::Need(need) => self.may(need),
            Asked::Govern => self.may_govern(),
        }
    }

    fn may(&self, need: &ActionNeed) -> bool {
        let mut node = TreePath::root().app(self.app_id);
        if let Some(field) = &need.field {
            node = node.field(field);
        }
        self.may_scope(&PermissionScope::Action {
            node,
            kind: need.kind,
        })
    }

    fn may_govern(&self) -> bool {
        self.may_scope(&PermissionScope::governance(
            policy::roles::governance::SETTINGS,
        ))
    }

    fn may_scope(&self, scope: &PermissionScope) -> bool {
        match self.permissions.borrow().as_ref() {
            Some(permissions) => permissions.check(scope),
            // A detached dispatcher may what writing allows; a past version
            // nothing.
            None => !self.read_only.get(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn refresh_read_only(&self) -> bool {
        let writes = self.permissions.borrow().as_ref().map(|permissions| {
            permissions.check(&PermissionScope::app(self.app_id, ActionKind::Undoable))
        });
        match writes {
            Some(writes) => self.set_read_only(!writes),
            None => false,
        }
    }

    pub(crate) fn set_read_only(&self, read_only: bool) -> bool {
        if self.read_only.get() == read_only {
            return false;
        }
        self.read_only.set(read_only);
        true
    }

    fn view_only(&self) -> String {
        format!("this {} is open to view only", self.label.to_lowercase())
    }

    /// Why an edit that is `need` was refused.
    fn not_allowed(&self, need: &ActionNeed) -> String {
        match (&need.field, need.kind) {
            (Some(field), _) if field == policy::roles::COMMENTS_FIELD => {
                format!("you may not comment on this {}", self.label.to_lowercase())
            }
            (None, ActionKind::Undoable) => self.view_only(),
            _ => format!("you may not do this to this {}", self.label.to_lowercase()),
        }
    }

    /// Why nothing can be submitted now, when the record is unreadable.
    fn unreadable(&self) -> Option<String> {
        self.unreadable.borrow().clone()
    }

    pub(crate) fn refuse(&self, action: &str, reason: String) {
        self.refused.set(Some(Refused {
            action: action.to_owned(),
            reason,
        }));
    }

    /// Why an invocation `id` submitted now is refused before its
    /// permission is asked: the record is unreadable (unless it is an
    /// effect) or the effect lending this dispatcher lost its custody.
    fn refusal(&self, id: &str, effect: bool) -> bool {
        if let Some(reason) = self.unreadable()
            && !effect
        {
            self.refuse(id, reason);
            return true;
        }
        if let Some(custody) = &self.custody
            && let Err(error) = custody.check()
        {
            self.refuse(id, error);
            return true;
        }
        false
    }

    /// Why nothing is applied now: an effect's dispatcher submits through
    /// the queue, and an unreadable record takes nothing.
    fn applies_now(&self) -> Result<(), ActionError> {
        if self.custody.is_some() {
            return Err(ActionError::Refused(
                "caller-scoped effects submit document changes through queued actions".into(),
            ));
        }
        if let Some(reason) = self.unreadable() {
            return Err(ActionError::Refused(reason));
        }
        Ok(())
    }

    /// The context of an action of this resource, attributed to whoever the
    /// edit is applied for.
    fn context(&self) -> ActionContext {
        ActionContext::for_resource(
            self.resource_id.as_ref(),
            self.kind,
            self.resource_label.get(),
        )
    }

    /// `context` named for this resource and its editor, in the step it
    /// opens or `continues` when it is a `step`.
    fn step_context(&self, context: ActionContext, step: bool, continues: bool) -> ActionContext {
        // History names the resource and who edited it even when the
        // surface's context does not: whoever the edit is applied for (an
        // agent at its own caret, a collaborator a scenario stages), else
        // this window's person.
        let mut named = self.context();
        if let Some(invoker) = ui_core::invocation_scope::current_invoker()
            .or_else(|| self.me.as_ref().map(|me| ActionInvoker::actor(me.as_ref())))
        {
            named = named.with_invoker(invoker);
        }
        let mut context = context.enriched_by(&named);
        // An action that continues the open step joins it; any other opens
        // its own.
        if step {
            let gesture = self.gesture.get().filter(|_| continues).unwrap_or_default();
            self.gesture.set(Some(gesture));
            context = context.in_gesture(Some(gesture));
        }
        context
    }

    /// Note an immediate action that is a step and was applied: whether it
    /// `continues` the open one and is `continuable` itself. Its serial.
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

    /// Note a group applied now: a step of its own, which nothing
    /// continues.
    fn grouped(&self) {
        self.serial.set(self.serial.get().wrapping_add(1));
        self.applied.set(true);
        self.continuing.set(false);
    }

    pub(crate) fn take_committed(&self) -> Option<Vec<ChangeHash>> {
        let bracket = self.host.borrow_mut().take_committed();
        let continuing = self.continuing.replace(false);
        self.opened
            .set(bracket.is_some() && !continuing && self.continuable.get());
        self.applied.set(false);
        if continuing { None } else { bracket }
    }
}

/// What a root reads and drives of its dispatcher, without its type.
impl DispatchCore {
    /// Who edits through this dispatcher; `None` in a view of a past version.
    pub(crate) fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }

    pub(crate) fn note_landed(&self, action: &str, output: &tilo_actions::ActionOutput) {
        self.landed.set(Some(Landed::of(action, output)));
    }

    /// Say why the record does not read as the resource (`None` once it
    /// does again); until then every submission is refused with it.
    pub(crate) fn set_unreadable(&self, reason: Option<String>) {
        *self.unreadable.borrow_mut() = reason;
    }

    /// The next queued invocation, with the effect invocation it continues.
    pub(crate) fn take_attributed(&self) -> Option<AttributedControlInvocation> {
        self.queue.borrow_mut().pop_front()
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.queue.borrow().is_empty()
    }

    /// The step the last boundary opened stays open for the actions that
    /// continue it ([`Resource::coalesces`]).
    pub(crate) fn keeps_step_open(&self) -> bool {
        self.opened.get()
    }

    pub(crate) fn resource_id(&self) -> &str {
        &self.resource_id
    }

    pub(crate) fn app_id(&self) -> &'static str {
        self.app_id
    }
}

impl<R: Resource> Dispatcher<R> {
    /// What does not name the resource type.
    pub(crate) fn core(&self) -> &DispatchCore {
        &self.core
    }

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

    /// What the effect `invocation` submitted here, once it all landed: the
    /// last submission's [`Landed`], or why one was refused. Nothing arrives
    /// when it submitted nothing.
    pub(crate) fn wait_landed(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<std::sync::mpsc::Receiver<Result<Landed, String>>> {
        self.core.waits.borrow_mut().wait(invocation)
    }

    pub(crate) fn new(
        app_id: &'static str,
        resource_id: &str,
        read_only: bool,
        me: Option<&str>,
    ) -> Self {
        Self {
            core: DispatchCore::new(app_id, resource_id, (R::KIND, R::LABEL), read_only, me),
            group: Rc::default(),
            permission_of: Rc::new(tilo_resource::need_of::<R>),
            submit_now: Rc::new(|host, edits, action, context| {
                edits.apply(host, R::ACTION_TYPE, action, context)
            }),
            group_now: Some(Rc::new(|host, edits, group, context| {
                edits.apply(
                    host,
                    &tilo_resource::group::group_type::<R>(),
                    group,
                    context,
                )
            })),
            last: Rc::default(),
        }
    }

    /// A dispatcher for a surface a test builds outside any host, editing as
    /// `me`: what the surface applies now ([`Self::apply_now`]) goes into the
    /// document the test hands it, as under a host. Queued edits
    /// ([`Self::apply`], [`Self::submit`]) wait for a host that never takes
    /// them; a test that needs those runs the app
    /// ([`TestApp`](super::testing::HeadlessApp)).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn detached(app_id: &'static str, me: &str) -> Self {
        Self::new(app_id, R::KIND, false, Some(me))
    }

    /// What a [`detached`](Self::detached) dispatcher handed to the door
    /// and nothing took yet — a destructive verb or a removal waiting for
    /// the person's confirmation — as the actions the door would apply once
    /// they agreed. Taking them empties the queue.
    ///
    /// # Errors
    /// Returns why a queued invocation does not build.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_asked(&self) -> Result<Vec<R::Action>, String> {
        self.core
            .queue
            .borrow_mut()
            .drain(..)
            .map(|(invocation, _)| {
                tilo_resource::build_action::<R>(
                    &invocation.address.action,
                    &invocation.arguments,
                    tilo_resource::origin_now_ms(),
                )
            })
            .collect()
    }

    /// Who edits through this dispatcher: the participant this view is
    /// for, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). `None` in a
    /// view of a past version, which nobody edits.
    pub fn me(&self) -> Option<&str> {
        self.core.me.as_deref()
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
        self.core.past
    }

    /// Whether edits from this participant will be refused, as of now: it
    /// follows role changes while the app runs. Surfaces use it to present
    /// themselves read-only instead of letting an edit bounce.
    pub fn read_only(&self) -> bool {
        self.core.read_only.get()
    }

    /// [`Self::read_only`] as observable state, for a widget that shows or
    /// disables something while the participant may only view.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.core.read_only.clone()
    }

    /// Whether the host currently allows what `need` is, as observable view
    /// state. It follows changes even when writing remains allowed and the
    /// resource document does not change. Bind it to a conditional pane or
    /// label; action invocation still checks the host's current authority.
    pub fn permission_state(&self, need: ActionNeed) -> LocalState<bool> {
        self.core.asked_state(Asked::Need(need))
    }

    /// Whether the participant may govern the room (invite, grant, change
    /// its settings), as observable view state: for an owner's pane.
    pub fn govern_state(&self) -> LocalState<bool> {
        self.core.asked_state(Asked::Govern)
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

    /// Whether the participant may, as of now, do what `need` is: an
    /// undoable change for most edits, one of the comments for a reply.
    pub fn may(&self, need: &ActionNeed) -> bool {
        self.core.may(need)
    }

    /// Whether the participant may, as of now, change the room's settings.
    pub fn may_govern(&self) -> bool {
        self.core.may_govern()
    }

    /// Whether the participant may apply `action`
    /// ([`tilo_resource::need_of`]).
    pub fn may_apply(&self, action: &R::Action) -> bool {
        self.core.may(&(self.permission_of)(action))
    }

    /// The last edit or effect submitted here that was refused — denied
    /// permission, unavailable, failed — and why, for a view to show. An
    /// edit waiting for the user's confirmation is not refused.
    pub fn refused(&self) -> LocalState<Option<Refused>> {
        self.core.refused.clone()
    }

    /// The last submission of this view the door applied, with what it
    /// answered: the ids a "new" verb minted ([`Landed::minted`]), for a
    /// surface that opens what it just created.
    pub fn landed(&self) -> LocalState<Option<Landed>> {
        self.core.landed.clone()
    }

    /// Record that the door refused `action`, submitted here.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn refuse(&self, action: &str, reason: String) {
        self.core.refuse(action, reason);
    }

    /// Why `action` may not be applied by this participant.
    fn not_allowed(&self, action: &R::Action) -> ActionError {
        ActionError::Refused(self.core.not_allowed(&(self.permission_of)(action)))
    }

    /// Invoke one of the app's effects (`dispatcher.effect(&Backups::BackUp)`)
    /// through the same door as a button bound to it: permission, the
    /// effect's availability and a destructive effect's confirmation are
    /// checked, then its handler runs. A participant who may only view still
    /// invokes the effects declared `kind = "read"`.
    pub fn effect<E: Effect>(&self, effect: &E) {
        self.core.queue_invocation(
            ActionAddress::new(self.core.resource_id.as_ref(), effect.id()),
            effect.arguments(),
        );
    }

    /// Invoke `effect` as the live row `row` offers it
    /// ([`LiveRow::effect`](super::LiveRow::effect)): under the row's
    /// contract, so a confirmation the row asks for (typing the name of the
    /// device it erases) is asked here too.
    pub fn effect_on<E: Effect>(&self, row: &str, effect: &E) {
        self.core
            .queue_invocation(ActionAddress::new(row, effect.id()), serde_json::Map::new());
    }

    /// Submit a field edit (`dispatcher.submit(p.favorite.set(true))`) or a
    /// verb (`dispatcher.submit(Snippets::DELETE.with(id))`) through the
    /// door, after the event, as a control bound to it would.
    ///
    /// An effect's invocation ([`Invoke::effect`]) reaches its handler as
    /// [`Self::effect`] does.
    pub fn submit(&self, submit: impl Submit<R>) {
        let id = submit.action_id();
        match submit.into_invoke() {
            Ok(invoke) => self.submit_invoke(invoke),
            Err(error) => {
                super::controls::unbound::<R>(&format!("`{id}`"), &error);
                self.core.refuse(id, error);
            }
        }
    }

    fn submit_invoke(&self, invoke: Invoke<R>) {
        if self.core.refusal(invoke.id, invoke.is_effect()) {
            return;
        }
        if invoke.is_effect() {
            self.queue(invoke);
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
        self.queue(invoke);
    }

    fn queue(&self, invoke: Invoke<R>) {
        self.core.queue_invocation(
            ActionAddress::new(self.core.resource_id.as_ref(), invoke.id),
            invoke.arguments,
        );
    }

    /// Whether the participant may take `invoke`: what its declaration, or
    /// the typed value it applies, needs. One this resource does not declare
    /// (an edit bound to a surface's view state) is left to the host, which
    /// checks what its published action needs.
    fn may_invoke(&self, invoke: &Invoke<R>) -> bool {
        if invoke.id == tilo_resource::APPLY || invoke.id == tilo_resource::FIELD_EDIT {
            return match invoke.build(0) {
                Ok(action) => self.may_apply(&action),
                Err(_) => !self.read_only(),
            };
        }
        may_declared(
            &self.core,
            &tilo_resource::vocabulary::<R>().declarations(),
            invoke.id,
            invoke.is_group(),
        )
    }

    /// Open the platform search with `request`, narrowed to this app or one
    /// part of it (`SearchRequest::within`). Searching is a view, so a
    /// participant who may only view can search too.
    pub fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.core.queue.borrow_mut().push_back((
            ui_core::search::open_search_invocation(request),
            super::effect::acting_for(),
        ));
    }

    /// Invoke an action published on a nested resource, such as one item of
    /// a collection.
    pub(crate) fn invoke_at(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        if self.read_only() {
            return;
        }
        self.core.queue_invocation(address, arguments);
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry. The door applies it after the
    /// event; if it refuses it, [`Self::refused`] says why.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::apply(action) {
            Ok(invoke) => self.submit_invoke(invoke),
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
            (Some(only), None) => self.queue(only),
            (Some(first), Some(second)) => {
                self.queue(Invoke::group([first, second].into_iter().chain(members)));
            }
        }
    }

    /// Apply a field edit (`p.title.splice(..)`) or a verb to the document
    /// `edits` lends, now, as [`Self::apply_now`] applies a typed action: for
    /// an editor that must see its own edit before the next keystroke.
    ///
    /// One that needs confirming (a destructive verb, a removal) is not
    /// applied now: it goes to the host's door, which asks first, and
    /// applies once the person agreed, as [`Self::submit`] does.
    ///
    /// # Errors
    /// Returns why the submission does not build, or as [`Self::apply_now`].
    pub fn submit_now(
        &self,
        edits: &mut Edits<'_>,
        submit: impl Submit<R>,
    ) -> Result<(), ActionError> {
        let invoke = submit.into_invoke().map_err(ActionError::Refused)?;
        if invoke.is_group() {
            return self.group_now(edits, &invoke);
        }
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        if asks(&action) {
            return self.ask_first(invoke, &action);
        }
        self.apply_now(edits, &action)
    }

    /// Hand an action that needs confirming to the host's door, which asks
    /// before it applies: nothing applied now skips the person's consent.
    fn ask_first(&self, invoke: Invoke<R>, action: &R::Action) -> Result<(), ActionError> {
        if let Some(reason) = self.core.unreadable() {
            return Err(ActionError::Refused(reason));
        }
        if !self.may_apply(action) {
            return Err(self.not_allowed(action));
        }
        self.submit_invoke(invoke);
        Ok(())
    }

    /// [`Self::submit_now`] of a group (`Invoke::group(edits)`): its members
    /// applied as one action, one change and one entry in history, all or
    /// nothing.
    fn group_now(&self, edits: &mut Edits<'_>, invoke: &Invoke<R>) -> Result<(), ActionError> {
        let Some(group_now) = &self.group_now else {
            return Err(ActionError::Refused(
                "this record's edits are applied one at a time".into(),
            ));
        };
        self.core.applies_now()?;
        let group = tilo_resource::group::build_group::<R>(
            &invoke.arguments,
            tilo_resource::origin_now_ms(),
        )
        .map_err(ActionError::Refused)?;
        if let Some(refused) = group.actions.iter().find(|action| !self.may_apply(action)) {
            return Err(self.not_allowed(refused));
        }
        // A group holding a removal asks first, as the queued door does.
        if group.actions.iter().any(asks) {
            self.submit_invoke(invoke.clone());
            return Ok(());
        }
        let mut context = self.core.context();
        if let Some(me) = &self.core.me {
            context = context.with_invoker(ActionInvoker::actor(me.as_ref()));
        }
        group_now(&mut self.core.host.borrow_mut(), edits, &group, context)?;
        // A group is a step of its own: nothing continues it.
        *self.last.borrow_mut() = None;
        self.core.grouped();
        Ok(())
    }

    /// Apply one typed action to the document `edits` lends — an event's,
    /// or a settle pass's — now, attributed to the participant, with history
    /// recording what the action did.
    /// One event's actions are one undo step, and an action that continues
    /// the previous one ([`Resource::coalesces`], the next letter of a word)
    /// joins its step. An action that needs confirming (a destructive verb,
    /// a removal) is not applied now: it is submitted to the host's door,
    /// which asks the person first ([`Self::apply`]).
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] for a participant who may only view,
    /// and the action's own refusal; nothing is written either way.
    pub fn apply_now(&self, edits: &mut Edits<'_>, action: &R::Action) -> Result<(), ActionError> {
        self.apply_now_as(edits, action, self.core.context(), false)
    }

    /// [`Self::apply_now`] of an action the person already confirmed, as
    /// the host applies it once they agreed: what a test resolving the
    /// queue as confirmed applies.
    ///
    /// # Errors
    /// As [`apply_now`](Self::apply_now).
    pub fn apply_confirmed(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
    ) -> Result<(), ActionError> {
        self.apply_now_as(edits, action, self.core.context(), true)
    }

    fn apply_now_as(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
        context: ActionContext,
        confirmed: bool,
    ) -> Result<(), ActionError> {
        self.core.applies_now()?;
        if !self.may_apply(action) {
            return Err(self.not_allowed(action));
        }
        // Applying now never skips the person's consent: an action that
        // needs confirming goes to the door, which asks first.
        if asks(action) && !confirmed {
            self.apply(action);
            return Ok(());
        }
        // Content from outside is nobody's step: it neither continues the
        // open one nor ends it.
        let step = tilo_actions::Action::change_kind(action).is_step();
        // Only an action right after this dispatcher's last one, with nothing
        // in between, can continue it.
        let serial = self.core.serial.get();
        let continues = step
            && self
                .last
                .borrow()
                .as_ref()
                .is_some_and(|(at, previous)| *at == serial && R::coalesces(previous, action));
        let context = self.core.step_context(context, step, continues);
        (self.submit_now)(&mut self.core.host.borrow_mut(), edits, action, context)?;
        if !step {
            return Ok(());
        }
        let serial = self.core.stepped(continues, R::continuable(action));
        *self.last.borrow_mut() = Some((serial, action.clone()));
        Ok(())
    }

    /// The dispatcher of another record than this resource's — the
    /// person's own record, a settings section — published at `id`: its
    /// queued invocations go through the same door as this resource's, and
    /// what it applies now `apply` applies to that record. The room's roles
    /// do not reach it; only its person changes it.
    pub(crate) fn for_record<C: Resource>(
        &self,
        id: &str,
        read_only: bool,
        apply: impl Fn(&C::Action) -> Result<(), ActionError> + 'static,
    ) -> Dispatcher<C> {
        Dispatcher {
            core: self.core.for_record(id, (C::KIND, C::LABEL), read_only),
            group: Rc::default(),
            permission_of: Rc::new(|_: &C::Action| ActionNeed::of(tilo_actions::ActionKind::Read)),
            submit_now: Rc::new(move |_, _, action: &C::Action, _| apply(action)),
            group_now: None,
            last: Rc::default(),
        }
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
            core: self.core.child(id, (C::KIND, C::LABEL)),
            group: Rc::default(),
            permission_of: Rc::new(move |action: &C::Action| {
                parent_permission(&wrap_for_permission(action.clone()))
            }),
            // History names the child and its edit; what applies is the
            // parent's action carrying it.
            group_now: None,
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
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn take_attributed(&self) -> Option<AttributedControlInvocation> {
        self.core.queue.borrow_mut().pop_front()
    }

    pub(crate) fn name_resource(&self, label: &str) {
        self.core.resource_label.set(label.to_owned());
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn resource_id(&self) -> &str {
        &self.core.resource_id
    }

    /// The id of the app whose resource this is, which presence keys name
    /// ([`ResourceSelection::presence`](super::ResourceSelection::presence)).
    pub fn app_id(&self) -> &'static str {
        self.core.app_id
    }
}

/// Whether the participant `core` edits for may take the action `id` the
/// resource's `declarations` may declare: what its declaration needs; a
/// group of edits what writing does; anything else what reading does.
fn may_declared(
    core: &DispatchCore,
    declarations: &[tilo_resource::Declaration],
    id: &str,
    group: bool,
) -> bool {
    match declarations.iter().find(|decl| decl.id == id) {
        Some(decl) => core.may(&ActionNeed {
            kind: decl.kind,
            field: decl.field.map(str::to_owned),
            privileged: false,
        }),
        None if group => !core.read_only.get(),
        None => core.may(&ActionNeed::of(ActionKind::Read)),
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

/// A submission the door applied ([`Dispatcher::landed`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Landed {
    /// The id of the applied action.
    pub action: String,
    /// What it answered, if anything.
    pub answer: Option<serde_json::Value>,
    /// The resources it created (`<kind>/<field>/<id>` for an item).
    pub created: Vec<String>,
}

impl Landed {
    fn of(action: &str, output: &tilo_actions::ActionOutput) -> Self {
        Self {
            action: action.to_owned(),
            answer: output.value.clone(),
            created: output.resources.clone(),
        }
    }

    /// The ids the action minted, in the order it minted them: the item a
    /// "new" verb created first.
    #[must_use]
    pub fn minted(&self) -> Vec<tilo_resource::Id> {
        self.created
            .iter()
            .filter_map(|resource| resource.rsplit('/').next())
            .map(tilo_resource::Id::from_raw)
            .collect()
    }
}

/// Whether `action` needs the person's confirmation before it applies.
fn asks<A: tilo_actions::Action>(action: &A) -> bool {
    tilo_actions::Action::risk(action).requires_confirmation()
}

/// The submissions each effect queued that have not landed yet, and the
/// effects waiting for theirs.
#[derive(Default)]
struct Waits(std::collections::HashMap<tilo_actions::InvocationId, Wait>);

#[derive(Default)]
struct Wait {
    /// Queued, not yet answered by the door.
    outstanding: usize,
    /// The last that landed, or the first refusal.
    result: Option<Result<Landed, String>>,
    waiter: Option<std::sync::mpsc::Sender<Result<Landed, String>>>,
}

impl Waits {
    fn submitted(&mut self, invocation: tilo_actions::InvocationId) {
        let wait = self.0.entry(invocation).or_default();
        wait.outstanding = wait.outstanding.saturating_add(1);
    }

    fn landed(&mut self, invocation: tilo_actions::InvocationId, landed: Result<Landed, String>) {
        let Some(wait) = self.0.get_mut(&invocation) else {
            return;
        };
        wait.outstanding = wait.outstanding.saturating_sub(1);
        // The first refusal stands: the effect failed with it.
        if !matches!(wait.result, Some(Err(_))) {
            wait.result = Some(landed);
        }
        self.settle(invocation);
    }

    fn wait(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<std::sync::mpsc::Receiver<Result<Landed, String>>> {
        let wait = self.0.get_mut(&invocation)?;
        let (sender, receiver) = std::sync::mpsc::channel();
        wait.waiter = Some(sender);
        self.settle(invocation);
        Some(receiver)
    }

    /// Answer the waiting effect once nothing it submitted is outstanding;
    /// forget an effect nobody waits on then.
    fn settle(&mut self, invocation: tilo_actions::InvocationId) {
        let Some(wait) = self.0.get_mut(&invocation) else {
            return;
        };
        if wait.outstanding > 0 {
            return;
        }
        if let Some(waiter) = wait.waiter.take()
            && let Some(result) = wait.result.take()
        {
            let _ = waiter.send(result);
        }
        self.0.remove(&invocation);
    }
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
