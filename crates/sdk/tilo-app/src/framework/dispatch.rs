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
    app_id: &'static str,
    resource_id: Rc<str>,
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
    /// The invocations of the open group, and how many groups are open.
    group: Rc<RefCell<(usize, Vec<Invoke<R>>)>>,
    read_only: LocalState<bool>,
    /// The participant's permissions as the host keeps them, for an action
    /// that needs less than writing (a reply a commenter may add) or more.
    /// Absent outside a host: a past version, which nobody edits, or a
    /// test's detached dispatcher, which may do what writing allows.
    permissions: Rc<RefCell<Option<LivePermissions>>>,
    permission_states: PermissionStates,
    /// What applying an action needs: the resource's own
    /// [`Resource::permission`], or for a child its parent's action's.
    permission_of: PermissionOf<R>,
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
    /// Applies one immediate action: the resource's own, or for a child
    /// resource its parent's action carrying it.
    submit_now: SubmitNow<R>,
    /// Applies a group of the resource's own actions now as one action;
    /// `None` for a dispatcher of a nested or other record.
    group_now: Option<GroupNow<R>>,
    /// Counts the immediate actions of the resource and its children, so a
    /// dispatcher can tell that nothing came between its last action and
    /// its next.
    serial: Rc<Cell<u64>>,
    /// The last immediate action and its serial, to tell whether the next
    /// continues it.
    last: Rc<RefCell<Option<Serialed<R>>>>,
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
    /// The gesture the open step's changes record, so the step stays one
    /// undo step in the document's history.
    gesture: Rc<Cell<Option<tilo_actions::ActionId>>>,
    marker: std::marker::PhantomData<fn(R)>,
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
            app_id: self.app_id,
            resource_id: Rc::clone(&self.resource_id),
            resource_label: self.resource_label.clone(),
            me: self.me.clone(),
            custody: self.custody.clone(),
            queue: Rc::clone(&self.queue),
            group: Rc::clone(&self.group),
            read_only: self.read_only.clone(),
            permissions: Rc::clone(&self.permissions),
            permission_states: Rc::clone(&self.permission_states),
            permission_of: Rc::clone(&self.permission_of),
            refused: self.refused.clone(),
            landed: self.landed.clone(),
            waits: Rc::clone(&self.waits),
            unreadable: Rc::clone(&self.unreadable),
            host: Rc::clone(&self.host),
            submit_now: Rc::clone(&self.submit_now),
            group_now: self.group_now.clone(),
            serial: Rc::clone(&self.serial),
            last: Rc::clone(&self.last),
            continuing: Rc::clone(&self.continuing),
            applied: Rc::clone(&self.applied),
            opened: Rc::clone(&self.opened),
            past: self.past,
            continuable: Rc::clone(&self.continuable),
            gesture: Rc::clone(&self.gesture),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: Resource> Dispatcher<R> {
    pub(crate) fn for_custody(
        &self,
        custody: ui_core::invocation_scope::ResourceInvocationScope,
    ) -> Self {
        let mut dispatcher = self.clone();
        dispatcher.custody = Some(Rc::new(custody));
        // An effect's group cannot absorb the person's unrelated input.
        dispatcher.group = Rc::default();
        dispatcher
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

    /// Hand the door's result for a submission the effect `invocation`
    /// queued to that effect, once it waits for its submissions to land.
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

    /// What the effect `invocation` submitted here, once it all landed: the
    /// last submission's [`Landed`], or why one was refused. Nothing arrives
    /// when it submitted nothing.
    pub(crate) fn wait_landed(
        &self,
        invocation: tilo_actions::InvocationId,
    ) -> Option<std::sync::mpsc::Receiver<Result<Landed, String>>> {
        self.waits.borrow_mut().wait(invocation)
    }

    pub(crate) fn new(
        app_id: &'static str,
        resource_id: &str,
        read_only: bool,
        me: Option<&str>,
    ) -> Self {
        Self {
            app_id,
            resource_id: Rc::from(resource_id),
            resource_label: LocalState::new(R::LABEL.to_owned()),
            me: me.map(Rc::from),
            custody: None,
            queue: Rc::default(),
            group: Rc::default(),
            read_only: LocalState::new(read_only),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            permission_of: Rc::new(tilo_resource::need_of::<R>),
            refused: LocalState::new(None),
            landed: LocalState::new(None),
            waits: Rc::default(),
            unreadable: Rc::default(),
            host: Rc::default(),
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
            serial: Rc::default(),
            last: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            past: false,
            continuable: Rc::default(),
            gesture: Rc::default(),
            marker: std::marker::PhantomData,
        }
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
        self.me.as_deref()
    }

    /// A dispatcher for a past version in History: nothing it is given is
    /// ever applied.
    pub(crate) fn past(mut self) -> Self {
        self.past = true;
        self.read_only.set(true);
        self.permission_states = Rc::default();
        self
    }

    /// Whether the view shows a past version in History mode, drawn from a
    /// frozen document. Such a view must start no work: no worker, no
    /// network, no files. A surface that would fetch or watch something
    /// presents what the document holds instead.
    pub fn is_past(&self) -> bool {
        self.past
    }

    /// Whether edits from this participant will be refused, as of now: it
    /// follows role changes while the app runs. Surfaces use it to present
    /// themselves read-only instead of letting an edit bounce.
    pub fn read_only(&self) -> bool {
        self.read_only.get()
    }

    /// [`Self::read_only`] as observable state, for a widget that shows or
    /// disables something while the participant may only view.
    pub fn read_only_state(&self) -> LocalState<bool> {
        self.read_only.clone()
    }

    /// Follow the participant's permissions as the host keeps them.
    pub(crate) fn follow(&self, permissions: LivePermissions) {
        *self.permissions.borrow_mut() = Some(permissions);
        self.refresh_permission_states();
    }

    /// Whether the host currently allows what `need` is, as observable view
    /// state. It follows changes even when writing remains allowed and the
    /// resource document does not change. Bind it to a conditional pane or
    /// label; action invocation still checks the host's current authority.
    pub fn permission_state(&self, need: ActionNeed) -> LocalState<bool> {
        self.asked_state(Asked::Need(need))
    }

    /// Whether the participant may govern the room (invite, grant, change
    /// its settings), as observable view state: for an owner's pane.
    pub fn govern_state(&self) -> LocalState<bool> {
        self.asked_state(Asked::Govern)
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

    /// The same dispatcher deciding by `permissions`, for a test of a
    /// surface whose participant may only comment, or only view.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_permissions(self, permissions: LivePermissions) -> Self {
        self.follow(permissions);
        self.refresh_read_only();
        self
    }

    /// Whether the participant may, as of now, do what `need` is: an
    /// undoable change for most edits, one of the comments for a reply.
    pub fn may(&self, need: &ActionNeed) -> bool {
        let mut node = TreePath::root().app(self.app_id);
        if let Some(field) = &need.field {
            node = node.field(field);
        }
        self.may_scope(&PermissionScope::Action {
            node,
            kind: need.kind,
        })
    }

    /// Whether the participant may, as of now, change the room's settings.
    pub fn may_govern(&self) -> bool {
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

    /// Whether the participant may apply `action`
    /// ([`tilo_resource::need_of`]).
    pub fn may_apply(&self, action: &R::Action) -> bool {
        self.may(&(self.permission_of)(action))
    }

    /// Re-read whether the participant may write. Returns whether it
    /// changed.
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

    /// Record a change of the participant's permissions. Returns whether
    /// anything changed.
    pub(crate) fn set_read_only(&self, read_only: bool) -> bool {
        if self.read_only.get() == read_only {
            return false;
        }
        self.read_only.set(read_only);
        true
    }

    /// The last edit or effect submitted here that was refused — denied
    /// permission, unavailable, failed — and why, for a view to show. An
    /// edit waiting for the user's confirmation is not refused.
    pub fn refused(&self) -> LocalState<Option<Refused>> {
        self.refused.clone()
    }

    /// The last submission of this view the door applied, with what it
    /// answered: the ids a "new" verb minted ([`Landed::minted`]), for a
    /// surface that opens what it just created.
    pub fn landed(&self) -> LocalState<Option<Landed>> {
        self.landed.clone()
    }

    pub(crate) fn note_landed(&self, action: &str, output: &tilo_actions::ActionOutput) {
        self.landed.set(Some(Landed::of(action, output)));
    }

    fn view_only() -> String {
        format!("this {} is open to view only", R::LABEL.to_lowercase())
    }

    /// Why an edit that is `need` was refused.
    fn not_allowed(need: &ActionNeed) -> String {
        match (&need.field, need.kind) {
            (Some(field), _) if field == policy::roles::COMMENTS_FIELD => {
                format!("you may not comment on this {}", R::LABEL.to_lowercase())
            }
            (None, ActionKind::Undoable) => Self::view_only(),
            _ => format!("you may not do this to this {}", R::LABEL.to_lowercase()),
        }
    }

    /// Record that the door refused `action`, submitted here.
    /// Say why the record does not read as the resource (`None` once it
    /// does again); until then every submission is refused with it.
    pub(crate) fn set_unreadable(&self, reason: Option<String>) {
        *self.unreadable.borrow_mut() = reason;
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

    /// Invoke one of the app's effects (`dispatcher.effect(&Backups::BackUp)`)
    /// through the same door as a button bound to it: permission, the
    /// effect's availability and a destructive effect's confirmation are
    /// checked, then its handler runs. A participant who may only view still
    /// invokes the effects declared `kind = "read"`.
    pub fn effect<E: Effect>(&self, effect: &E) {
        self.queue_invocation(
            ActionAddress::new(self.resource_id.as_ref(), effect.id()),
            effect.arguments(),
        );
    }

    /// Invoke `effect` as the live row `row` offers it
    /// ([`LiveRow::effect`](super::LiveRow::effect)): under the row's
    /// contract, so a confirmation the row asks for (typing the name of the
    /// device it erases) is asked here too.
    pub fn effect_on<E: Effect>(&self, row: &str, effect: &E) {
        self.queue_invocation(ActionAddress::new(row, effect.id()), serde_json::Map::new());
    }

    /// Submit a field edit (`dispatcher.submit(p.favorite.set(true))`) or a
    /// verb (`dispatcher.submit(Snippets::DELETE.with(id))`) through the
    /// door, after the event, as a control bound to it would.
    pub fn submit(&self, submit: impl Submit<R>) {
        let id = submit.action_id();
        match submit.into_invoke() {
            Ok(invoke) => self.invoke(invoke),
            Err(error) => {
                super::controls::unbound::<R>(&format!("`{id}`"), &error);
                self.refuse(id, error);
            }
        }
    }

    /// Invoke a declared action (`dispatcher.invoke(Note::EDIT.with(text))`),
    /// or an effect ([`Invoke::effect`]), which reaches its handler as
    /// [`Self::effect`] does.
    pub fn invoke(&self, invoke: Invoke<R>) {
        if let Some(refusal) = invoke.refusal() {
            self.refuse(invoke.id, refusal.to_owned());
            return;
        }
        if let Some(reason) = self.unreadable()
            && !invoke.is_effect()
        {
            self.refuse(invoke.id, reason);
            return;
        }
        if let Some(custody) = &self.custody
            && let Err(error) = custody.check()
        {
            self.refuse(invoke.id, error);
            return;
        }
        if invoke.is_effect() {
            self.queue(invoke);
            return;
        }
        if !self.may_invoke(&invoke) {
            self.refuse(invoke.id, Self::view_only());
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
        self.queue_invocation(
            ActionAddress::new(self.resource_id.as_ref(), invoke.id),
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
        match R::declarations().iter().find(|decl| decl.id == invoke.id) {
            Some(decl) => self.may(&ActionNeed {
                kind: decl.kind,
                field: decl.field.map(str::to_owned),
            }),
            None if invoke.is_group() => !self.read_only(),
            None => self.may(&ActionNeed::of(ActionKind::Read)),
        }
    }

    /// Open the platform search with `request`, narrowed to this app or one
    /// part of it (`SearchRequest::within`). Searching is a view, so a
    /// participant who may only view can search too.
    pub fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.queue.borrow_mut().push_back((
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
        self.queue_invocation(address, arguments);
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry. The door applies it after the
    /// event; if it refuses it, [`Self::refused`] says why.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::checked_apply(action) {
            Ok(invoke) => self.invoke(invoke),
            Err(error) => self.refuse(tilo_resource::APPLY, error.to_string()),
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
        self.apply_now(edits, &action)
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
        if self.custody.is_some() {
            return Err(ActionError::Refused(
                "caller-scoped effects submit document changes through queued actions".into(),
            ));
        }
        if let Some(reason) = self.unreadable() {
            return Err(ActionError::Refused(reason));
        }
        let group = tilo_resource::group::build_group::<R>(
            &invoke.arguments,
            tilo_resource::origin_now_ms(),
        )
        .map_err(ActionError::Refused)?;
        if let Some(refused) = group.actions.iter().find(|action| !self.may_apply(action)) {
            return Err(ActionError::Refused(Self::not_allowed(&(self
                .permission_of)(
                refused
            ))));
        }
        let mut context = ActionContext::for_resource(
            self.resource_id.as_ref(),
            R::KIND,
            self.resource_label.get(),
        );
        if let Some(me) = &self.me {
            context = context.with_invoker(ActionInvoker::actor(me.as_ref()));
        }
        group_now(&mut self.host.borrow_mut(), edits, &group, context)?;
        // A group is a step of its own: nothing continues it.
        self.serial.set(self.serial.get().wrapping_add(1));
        *self.last.borrow_mut() = None;
        self.applied.set(true);
        self.continuing.set(false);
        Ok(())
    }

    /// Apply one typed action to the document `edits` lends — an event's,
    /// or a settle pass's — now, attributed to the participant, with history
    /// recording what the action did.
    /// One event's actions are one undo step, and an action that continues
    /// the previous one ([`Resource::coalesces`], the next letter of a word)
    /// joins its step.
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] for a participant who may only view,
    /// and the action's own refusal; nothing is written either way.
    pub fn apply_now(&self, edits: &mut Edits<'_>, action: &R::Action) -> Result<(), ActionError> {
        let context = ActionContext::for_resource(
            self.resource_id.as_ref(),
            R::KIND,
            self.resource_label.get(),
        );
        self.apply_now_with(edits, action, context)
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
        if self.custody.is_some() {
            return Err(ActionError::Refused(
                "caller-scoped effects submit document changes through queued actions".into(),
            ));
        }
        if let Some(reason) = self.unreadable() {
            return Err(ActionError::Refused(reason));
        }
        if !self.may_apply(action) {
            return Err(ActionError::Refused(Self::not_allowed(&(self
                .permission_of)(
                action
            ))));
        }
        // History names the resource and who edited it even when the
        // surface's context does not.
        let mut named = ActionContext::for_resource(
            self.resource_id.as_ref(),
            R::KIND,
            self.resource_label.get(),
        );
        if let Some(me) = &self.me {
            named = named.with_invoker(ActionInvoker::actor(me.as_ref()));
        }
        let mut context = context.enriched_by(&named);
        // Content from outside is nobody's step: it neither continues the
        // open one nor ends it.
        let step = tilo_actions::Action::change_kind(action).is_step();
        // Only an action right after this dispatcher's last one, with nothing
        // in between, can continue it.
        let serial = self.serial.get();
        let continues = step
            && self
                .last
                .borrow()
                .as_ref()
                .is_some_and(|(at, previous)| *at == serial && R::coalesces(previous, action));
        // A change another may continue records its gesture, so the step it
        // opens or continues stays one undo step after a restart too.
        if step && R::continuable(action) {
            let gesture = self.gesture.get().filter(|_| continues).unwrap_or_default();
            self.gesture.set(Some(gesture));
            context = context.in_gesture(Some(gesture));
        } else if step {
            context = context.in_gesture(continues.then(|| self.gesture.get()).flatten());
        }
        (self.submit_now)(&mut self.host.borrow_mut(), edits, action, context)?;
        if !step {
            return Ok(());
        }
        let serial = serial.wrapping_add(1);
        self.serial.set(serial);
        *self.last.borrow_mut() = Some((serial, action.clone()));
        self.continuable.set(R::continuable(action));
        // The event continues the open step only if every action it applied
        // did.
        let first_of_event = !self.applied.replace(true);
        self.continuing
            .set(continues && (first_of_event || self.continuing.get()));
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
            app_id: self.app_id,
            me: self.me.clone(),
            custody: self.custody.clone(),
            resource_id: Rc::from(id),
            resource_label: LocalState::new(C::LABEL.to_owned()),
            queue: Rc::clone(&self.queue),
            group: Rc::default(),
            read_only: LocalState::new(read_only || self.past),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            permission_of: Rc::new(|_: &C::Action| ActionNeed::of(tilo_actions::ActionKind::Read)),
            refused: self.refused.clone(),
            landed: self.landed.clone(),
            waits: Rc::clone(&self.waits),
            unreadable: Rc::default(),
            host: Rc::clone(&self.host),
            submit_now: Rc::new(move |_, _, action: &C::Action, _| apply(action)),
            group_now: None,
            serial: Rc::default(),
            last: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            past: self.past,
            continuable: Rc::default(),
            gesture: Rc::default(),
            marker: std::marker::PhantomData,
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
            app_id: self.app_id,
            me: self.me.clone(),
            custody: self.custody.clone(),
            resource_id: Rc::from(id),
            resource_label: LocalState::new(C::LABEL.to_owned()),
            queue: Rc::clone(&self.queue),
            group: Rc::default(),
            read_only: self.read_only.clone(),
            permissions: Rc::clone(&self.permissions),
            permission_states: Rc::clone(&self.permission_states),
            permission_of: Rc::new(move |action: &C::Action| {
                parent_permission(&wrap_for_permission(action.clone()))
            }),
            refused: self.refused.clone(),
            landed: self.landed.clone(),
            waits: Rc::clone(&self.waits),
            unreadable: Rc::clone(&self.unreadable),
            host: Rc::clone(&self.host),
            // History names the child and its edit; what applies is the
            // parent's action carrying it.
            group_now: None,
            submit_now: Rc::new(move |host, edits, action: &C::Action, context| {
                parent(host, edits, &wrap(action.clone()), context)
            }),
            serial: Rc::clone(&self.serial),
            last: Rc::default(),
            continuing: Rc::clone(&self.continuing),
            applied: Rc::clone(&self.applied),
            opened: Rc::clone(&self.opened),
            past: self.past,
            continuable: Rc::clone(&self.continuable),
            gesture: Rc::clone(&self.gesture),
            marker: std::marker::PhantomData,
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

    /// The id of the app whose resource this is, which presence keys name
    /// ([`ResourceSelection::presence`](super::ResourceSelection::presence)).
    pub fn app_id(&self) -> &'static str {
        self.app_id
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
