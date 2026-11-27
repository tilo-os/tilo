//! Submitting edits: every change a view makes is an action the host checks
//! against permissions, applies, records for undo and history, and
//! replicates.

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{
    ActionAddress, ActionBinding, ActionContext, ActionError, ActionInvoker, ActionNeed,
    ControlInvocation,
};

use super::controls::Actions;
use tilo_resource::{Edit, Effect, Invoke, MemberId, Resource};
use tilo_ui_base::LocalState;
use tilo_ui_base::{
    permissions::ActionKind, permissions::LivePermissions, permissions::PermissionScope,
    permissions::TreePath, permissions::policy,
};
use ui_core::automerge::ChangeHash;

/// What a view asks whether its member may do.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Asked {
    Need(ActionNeed),
    Govern,
    /// Whether what the workspace holds has arrived, so a choice made from the
    /// record (a free seat) is made from what everyone sees.
    Arrived,
}
use tilo_ui_base::widget::Edits;
use tilo_ui_base::{widget::controls::AttributedControlInvocation, widget::controls::Origin};

/// What reacting needs: an undoable change below the reactions field,
/// wherever it is.
fn reaction_need() -> ActionNeed {
    ActionNeed::reactions()
}

/// Whether the field edit in `arguments` reacts or takes a reaction back.
fn reacts(arguments: &serde_json::Map<String, serde_json::Value>) -> bool {
    arguments
        .get(tilo_resource::EDIT_PARAM)
        .and_then(|edit| edit.get("op"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|op| op == "react" || op == "unreact")
}

/// A queued invocation and who asked for it.
type Queued = (AttributedControlInvocation, Origin);

/// Something a dispatcher submits and a button is bound to: a field edit
/// made at a path (`p.favorite.set(true)`), a verb with its arguments, or an
/// invocation built some other way. No app writes an action id or an
/// argument map.
pub trait Submit<R: Resource>: Sized {
    /// The public id of the action this submits.
    fn action_id(&self) -> &'static str;

    /// The invocation of the host API this submits.
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
/// Two APIs, one path. [`submit`](Self::submit) and [`apply`](Self::apply)
/// queue an invocation the host resolves after the event, exactly as a
/// control bound to an action would. [`apply_now`](Self::apply_now) applies
/// one typed action immediately, inside the event or the settle pass
/// ([`Widget::settle`](tilo_ui_base::widget::Widget::settle)), for an editor that must
/// see the result of its own edit before the next keystroke (the paragraph a
/// split created, the cell a table insert made). Both check the
/// member's permission, apply through the replayable executor with
/// attribution, and are undone per author.
///
/// # A drag as one undo step
///
/// A slider or a drag handle a surface draws itself applies each value it
/// passes through with [`apply_now`](Self::apply_now), so the document — and
/// every member — follows the drag live. The resource declares that
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
    /// What an action a person applies here is reported to the broker as:
    /// for a child resource, its parent's action carrying it, which is what
    /// applies; `None` for the resource's own actions, reported as invoked.
    reported_as: Option<ReportedAs<R>>,
}

/// What a dispatcher keeps that does not name the resource's action type
/// ([`DispatchState`]), dropped out of line: a dispatcher is dropped in code
/// generic over its resource, and dropping each of these fields there would
/// compile it into every app.
pub(crate) struct DispatchCore(std::mem::ManuallyDrop<DispatchState>);

impl Drop for DispatchCore {
    #[inline(never)]
    fn drop(&mut self) {
        // SAFETY: the state is dropped here once, as its owner goes, and
        // nothing reads it after.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.0) }
    }
}

impl DispatchCore {
    fn of(state: DispatchState) -> Self {
        Self(std::mem::ManuallyDrop::new(state))
    }

    /// The kind the edits here apply as, when a record holds this resource
    /// ([`Dispatcher::child`]): the record's own kind.
    fn applied_as_kind(&self) -> Option<&'static str> {
        self.applied_as.map(|(kind, _)| kind)
    }

    /// The nested editors the views opened ([`Dispatcher::child`]), at their
    /// own addresses, to publish beside this resource.
    pub(crate) fn nested_resources(&self) -> Vec<(String, tilo_actions::ResourceSpec)> {
        self.nested.borrow().clone()
    }

    /// Whether the views opened any nested editor ([`Self::nested_resources`]).
    pub(crate) fn has_nested(&self) -> bool {
        !self.nested.borrow().is_empty()
    }

    /// Name this resource `label`, as history, the palette and an agent read
    /// it: the name its app gives the state it holds now.
    pub(crate) fn name_resource(&self, label: &str) {
        // Named again by every publication: only a new name is a change
        // anyone reading it takes up.
        self.resource_label.set_if_changed(label.to_owned());
    }
}

impl std::ops::Deref for DispatchCore {
    type Target = DispatchState;

    fn deref(&self) -> &DispatchState {
        &self.0
    }
}

impl std::ops::DerefMut for DispatchCore {
    fn deref_mut(&mut self) -> &mut DispatchState {
        &mut self.0
    }
}

/// What a dispatcher keeps that does not name the resource's action type:
/// who edits, the API's queue, permissions, refusals and the undo-step
/// bookkeeping.
pub(crate) struct DispatchState {
    app_id: &'static str,
    resource_id: Rc<str>,
    /// The route of the record the resource is, as the workspace names its node
    /// (empty for the app's own record of the workspace): an action on an item
    /// of it is decided at that item's node.
    record: Rc<str>,
    /// [`Resource::KIND`] and [`Resource::LABEL`] of the resource.
    kind: &'static str,
    label: &'static str,
    /// For a resource nested in another ([`Dispatcher::child`]), the kind and
    /// label of the record that holds it. Its edits apply as that record's
    /// field edits, so history and the change's claim name that record's
    /// kind: the app's surface describes no other, and every receiver holds
    /// (and the signer refuses) a change claiming the nested kind.
    applied_as: Option<(&'static str, &'static str)>,
    /// The readable name published by the resource host, shared with surfaces.
    resource_label: LocalState<String>,
    /// Who edits through this dispatcher; `None` in a view of a past version.
    me: Option<MemberId>,
    /// Each queued invocation with the effect invocation it continues,
    /// when an effect's handler or completion queued it.
    queue: Rc<RefCell<VecDeque<Queued>>>,
    /// Who asks for what is queued now: the person, unless the app submits
    /// it on its own ([`Dispatcher::submit_on_its_own`]).
    asking: Rc<Cell<Origin>>,
    /// The references the views opened ([`Dispatcher::open_reference`]),
    /// on their way through the session's action API.
    pub(crate) opener: super::reference::Opener,
    read_only: LocalState<bool>,
    /// The member's permissions as the host keeps them, for an action
    /// that needs less than writing (a reply a commenter may add) or more.
    /// Absent outside a host: a past version, which nobody edits, or a
    /// test's detached dispatcher, which may do what writing allows.
    permissions: Rc<RefCell<Option<LivePermissions>>>,
    permission_states: PermissionStates,
    pub(crate) refused: LocalState<Option<Refused>>,
    /// An edit applied now was refused: the views that may have previewed
    /// it are presented the stored state again, as after a refusal at the
    /// API.
    represent: Rc<Cell<bool>>,
    /// The last submission of this view the API applied.
    landed: LocalState<Option<Landed>>,
    /// What the views said they did ([`Dispatcher::say_when_kept`]), not yet
    /// handed to the record to hold for the workspace.
    notices: Rc<RefCell<Vec<String>>>,
    /// The last of them the workspace kept, to say now ([`Dispatcher::said`]).
    pub(crate) said: LocalState<Option<String>>,
    /// What the effects that queued submissions here wait on: how many are
    /// still to land, per effect invocation.
    waits: Rc<RefCell<Waits>>,
    /// Why the record does not read as the resource, while it does not:
    /// nothing is submitted against a state nobody can see.
    unreadable: Rc<RefCell<Option<String>>>,
    host: Rc<RefCell<ui_core::local_dispatch::LocalDispatch>>,
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
    /// The nested editors the views opened ([`Dispatcher::child`]), each
    /// published at its own address beside this resource: what the palette,
    /// agents and scripts address. Kept once made, so an editor acts on its
    /// record after it was last shown.
    nested: Rc<RefCell<Vec<(String, tilo_actions::ResourceSpec)>>>,
}

impl Clone for DispatchCore {
    /// Out of line: a derived clone is inlined into every dispatcher's clone, which each app
    /// compiles for its resource.
    #[inline(never)]
    fn clone(&self) -> Self {
        Self(std::mem::ManuallyDrop::new(DispatchState {
            app_id: self.app_id,
            resource_id: self.resource_id.clone(),
            record: self.record.clone(),
            kind: self.kind,
            label: self.label,
            applied_as: self.applied_as,
            resource_label: self.resource_label.clone(),
            me: self.me.clone(),
            queue: self.queue.clone(),
            asking: self.asking.clone(),
            opener: self.opener.clone(),
            read_only: self.read_only.clone(),
            permissions: self.permissions.clone(),
            permission_states: self.permission_states.clone(),
            refused: self.refused.clone(),
            represent: self.represent.clone(),
            landed: self.landed.clone(),
            notices: self.notices.clone(),
            said: self.said.clone(),
            waits: self.waits.clone(),
            unreadable: self.unreadable.clone(),
            host: self.host.clone(),
            serial: self.serial.clone(),
            continuing: self.continuing.clone(),
            applied: self.applied.clone(),
            opened: self.opened.clone(),
            past: self.past,
            continuable: self.continuable.clone(),
            gesture: self.gesture.clone(),
            nested: Rc::clone(&self.nested),
        }))
    }
}

/// The published invocation a child's action is reported as.
type ReportedAs<R> =
    Rc<dyn Fn(&Invoke<R>) -> Option<ui_core::local_document_action::PublishedInvocation>>;

/// What applying one action needs.
type PermissionOf<R> = Rc<dyn Fn(&<R as Resource>::Action) -> ActionNeed>;

/// One observable per requested permission, shared by this resource's views.
type PermissionStates = Rc<RefCell<Vec<(Asked, LocalState<bool>)>>>;

/// An immediate action and the serial it was applied at.
type Serialed<R> = (u64, <R as Resource>::Action);

/// The step the last gesture written to a record joined: the gesture, the
/// step, the input event it was written in, and how that step was opened.
#[derive(Clone, Copy)]
struct Stepped {
    gesture: tilo_actions::ActionId,
    step: tilo_actions::ActionId,
    event: u64,
    opened_by_typing: bool,
    /// Opened while the person's input was handled, not by a settle after it.
    opened_by_input: bool,
}

/// Run `write` with `step` open as the thread's undo step (else the one
/// open now), the one before it open again after.
fn in_step<T>(step: Option<tilo_actions::ActionId>, write: impl FnOnce() -> T) -> T {
    let open = tilo_actions::executor::current_step();
    tilo_actions::executor::open_step(step.or(open));
    let written = write();
    tilo_actions::executor::open_step(open);
    written
}

/// Applies a group of the resource's actions now, as one action.
type GroupNow<R> = Rc<
    dyn Fn(
        &mut ui_core::local_dispatch::LocalDispatch,
        &mut Edits<'_>,
        &tilo_resource::ActionGroup<<R as Resource>::Action>,
        ActionContext,
    ) -> Result<(), ActionError>,
>;

/// Applies one typed action to the document a pass may change, through
/// the host.
type SubmitNow<R> = Rc<
    dyn Fn(
        &mut ui_core::local_dispatch::LocalDispatch,
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
            reported_as: self.reported_as.clone(),
        }
    }
}

impl DispatchCore {
    fn new(
        app_id: &'static str,
        resource_id: &str,
        (kind, label): (&'static str, &'static str),
        read_only: bool,
        me: Option<&MemberId>,
    ) -> Self {
        Self(std::mem::ManuallyDrop::new(DispatchState {
            app_id,
            resource_id: Rc::from(resource_id),
            record: Rc::from(""),
            kind,
            label,
            resource_label: LocalState::new(label.to_owned()),
            me: me.cloned(),
            queue: Rc::default(),
            asking: Rc::default(),
            opener: super::reference::Opener::default(),
            read_only: LocalState::new(read_only),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            refused: LocalState::new(None),
            represent: Rc::default(),
            landed: LocalState::new(None),
            notices: Rc::default(),
            said: LocalState::new(None),
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
            applied_as: None,
            nested: Rc::default(),
        }))
    }

    /// The core of the dispatcher of another record than this resource's,
    /// published at `id` ([`Dispatcher::for_resource`]).
    fn for_resource(
        &self,
        id: &str,
        (kind, label): (&'static str, &'static str),
        read_only: bool,
    ) -> Self {
        Self::of(DispatchState {
            app_id: self.app_id,
            me: self.me.clone(),
            resource_id: Rc::from(id),
            record: Rc::from(""),
            kind,
            label,
            resource_label: LocalState::new(label.to_owned()),
            queue: Rc::clone(&self.queue),
            asking: Rc::clone(&self.asking),
            opener: self.opener.clone(),
            read_only: LocalState::new(read_only || self.past),
            permissions: Rc::default(),
            permission_states: Rc::default(),
            refused: self.refused.clone(),
            represent: self.represent.clone(),
            landed: self.landed.clone(),
            notices: Rc::clone(&self.notices),
            said: self.said.clone(),
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
            applied_as: None,
            nested: Rc::default(),
        })
    }

    /// The core of the dispatcher of a resource nested in this one at `id`
    /// ([`Dispatcher::child`]): it shares this one's permissions and undo
    /// steps.
    fn child(&self, id: &str, (kind, label): (&'static str, &'static str)) -> Self {
        let mut child = self.clone();
        child.resource_id = Rc::from(id);
        child.kind = kind;
        child.label = label;
        child.applied_as = self.applied_as.or(Some((self.kind, self.label)));
        child.resource_label = LocalState::new(label.to_owned());
        child
    }

    fn queue_invocation(
        &self,
        address: ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) {
        #[cfg(debug_assertions)]
        not_painting();
        let on_behalf_of = super::effect::acting_for();
        if let Some(invocation) = on_behalf_of {
            self.waits.borrow_mut().submitted(invocation);
        }
        self.queue.borrow_mut().push_back((
            (ControlInvocation { address, arguments }, on_behalf_of),
            self.asking.get(),
        ));
    }

    pub(crate) fn note_continuation(
        &self,
        invocation: tilo_actions::InvocationId,
        action: &str,
        result: &Result<
            tilo_ui_base::host_operation::ResourceOperationStart,
            tilo_actions::ApiError,
        >,
    ) {
        let landed = match result {
            Ok(tilo_ui_base::host_operation::ResourceOperationStart::Completed(output)) => {
                Ok(Landed::of(action, output))
            }
            // Handed on to the broker: it applies there, answering nothing here.
            Ok(tilo_ui_base::host_operation::ResourceOperationStart::Accepted) => Ok(Landed {
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
            Asked::Arrived => !tilo_ui_base::workspace_sync::before_first_sync(),
        }
    }

    fn may(&self, need: &ActionNeed) -> bool {
        let mut node = self.node();
        if let Some(field) = &need.field {
            node = node.field(field);
        }
        self.may_scope(&PermissionScope::Action {
            node,
            kind: need.kind,
        })
    }

    /// The node below the workspace where the workspace decides this resource's
    /// changes, as far as a decision here can name it: an item of the
    /// record for a resource nested in one at `<kind>/<field>/<item>` (a
    /// note's body), so what the workspace grants on that item alone (an item
    /// shared to edit) lets it be changed here too; else the app's.
    fn node(&self) -> TreePath {
        let app = TreePath::root().app(self.app_id);
        // A sealed record is decided on that record.
        let kind = self.applied_as.map_or(self.kind, |(kind, _)| kind);
        if collab_types::sealed_kind(self.app_id, &self.record).is_some() {
            return app.kind(kind).resource(&*self.record);
        }
        if self.applied_as.is_none() {
            return app;
        }
        match self
            .resource_id
            .rsplit_once('/')
            .and_then(|(collection, item)| Some((collection.split_once('/')?, item)))
        {
            Some(((kind, field), item))
                if !kind.is_empty()
                    && !field.is_empty()
                    && !field.contains('/')
                    && !item.is_empty() =>
            {
                app.kind(kind).resource(&*self.record).item(item)
            }
            _ => app,
        }
    }

    /// Whether the member may react here: never in a past version,
    /// else as the workspace grants reactions, which the Commenter role does as
    /// it grants comments.
    pub(crate) fn may_react(&self) -> bool {
        !self.past && self.may(&reaction_need())
    }

    /// Whether the member may read what this window holds: one who may
    /// not is found nothing of it.
    pub(crate) fn may_read(&self) -> bool {
        self.may(&ActionNeed::of(ActionKind::Read))
    }

    /// Whether the workspace this record is in has anyone besides this window's
    /// person: its owner or a member, here now or not: what it holds is
    /// shared with them.
    pub(crate) fn workspace_has_others(&self) -> bool {
        self.permissions
            .borrow()
            .as_ref()
            .is_some_and(tilo_ui_base::permissions::LivePermissions::workspace_has_others)
    }

    /// React (or take a reaction back) at `path`, a [`Reactions`](tilo_resource::Reactions)
    /// field of the resource, as the person this window is for: the field
    /// edit `op` through the API, as [`Dispatcher::submit`] submits one,
    /// for code that does not know the resource's type (the window's bar).
    pub(crate) fn submit_reaction(
        &self,
        path: Vec<tilo_resource::Segment>,
        op: tilo_resource::EditOp,
    ) {
        let id = tilo_resource::FIELD_EDIT;
        if self.refusal(id, false) {
            return;
        }
        if !self.may_react() {
            self.refuse(id, self.not_allowed(&reaction_need()));
            return;
        }
        let edit = Edit::<()>::new(path, op);
        match tilo_resource::__private::json_via_bytes(&edit) {
            Ok(edit) => self.queue_invocation(
                ActionAddress::new(self.resource_id.as_ref(), id),
                serde_json::Map::from_iter([(tilo_resource::EDIT_PARAM.to_owned(), edit)]),
            ),
            Err(error) => self.refuse(id, error.to_string()),
        }
    }

    fn may_govern(&self) -> bool {
        self.may_scope(&PermissionScope::governance(
            policy::roles::governance::SETTINGS,
        ))
    }

    /// Whether the member may invite people to the workspace.
    pub(crate) fn may_invite(&self) -> bool {
        self.may_scope(&PermissionScope::governance(
            policy::roles::governance::ADD_MEMBER,
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
            (Some(field), _) if field == policy::roles::REACTIONS_FIELD => {
                format!("you may not react to this {}", self.label.to_lowercase())
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
            item: None,
            reason,
            by_workspace: false,
        }));
    }

    /// What the views said they did since last asked, for the record to
    /// hold until the workspace keeps it.
    pub(crate) fn take_notices(&self) -> Vec<String> {
        std::mem::take(&mut *self.notices.borrow_mut())
    }

    /// The workspace kept what `text` says was done.
    pub(crate) fn kept(&self, text: &str) {
        self.said.set(Some(text.to_owned()));
    }

    /// The workspace refused what `refusal` names after this device applied it;
    /// the record withdrew it.
    pub(crate) fn refuse_by_workspace(&self, refusal: &ui_core::resource::ResourceRefusal) {
        self.refused.set(Some(Refused {
            action: refusal.action.clone(),
            item: refusal.item.clone(),
            reason: refusal.reason.clone(),
            by_workspace: true,
        }));
    }

    /// Why an invocation `id` submitted now is refused before its
    /// permission is asked: the record is unreadable (unless it is an
    /// effect).
    fn refusal(&self, id: &str, effect: bool) -> bool {
        if let Some(reason) = self.unreadable()
            && !effect
        {
            self.refuse(id, reason);
            return true;
        }
        false
    }

    /// Why nothing is applied now: an effect's dispatcher submits through
    /// the queue, and an unreadable record takes nothing.
    fn applies_now(&self) -> Result<(), ActionError> {
        if let Some(reason) = self.unreadable() {
            return Err(ActionError::Refused(reason));
        }
        Ok(())
    }

    /// The context of an action of this resource, attributed to whoever the
    /// edit is applied for, and owned by this dispatcher's app. Without the
    /// app, the executor guessed one from the action type (`org.tilo.Chat` of
    /// `chat.workspace`), and every receiver checking the change against the
    /// record's app refused it as a verb that app does not publish.
    fn context(&self) -> ActionContext {
        let context = self.resource_context();
        if self.app_id.is_empty() {
            context
        } else {
            context.with_app(self.app_id, self.label)
        }
    }

    fn resource_context(&self) -> ActionContext {
        match self.applied_as {
            // As `ActionSpec::history_context` names an action a nested
            // resource re-encodes: the enclosing kind, the nested resource
            // kept as a facet.
            Some((kind, label)) => {
                let mut context =
                    ActionContext::for_resource(self.resource_id.as_ref(), kind, label);
                context.facets.push(tilo_actions::ActionFacet::new(
                    tilo_actions::NESTED_RESOURCE_FACET,
                    self.resource_label.get(),
                ));
                context
            }
            None => ActionContext::for_resource(
                self.resource_id.as_ref(),
                self.kind,
                self.resource_label.get(),
            ),
        }
    }

    /// `context` named for this resource and its editor, in the step it
    /// opens or `continues` when it is a `step`.
    fn step_context(&self, context: ActionContext, step: bool, continues: bool) -> ActionContext {
        // History names the resource and who edited it even when the
        // surface's context does not: whoever the edit is applied for (an
        // agent at its own caret, a member a scenario stages), else
        // this window's person.
        let mut named = self.context();
        if let Some(invoker) = ui_core::invocation_scope::current_invoker()
            .or_else(|| self.me.clone().map(ActionInvoker::member))
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
    pub(crate) fn me(&self) -> Option<&MemberId> {
        self.me.as_ref()
    }

    /// Whether it is a view of a past version.
    pub(crate) fn is_past(&self) -> bool {
        self.past
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
    /// The host draining it hears who asked for it.
    pub(crate) fn take_attributed(&self) -> Option<AttributedControlInvocation> {
        let (taken, origin) = self.queue.borrow_mut().pop_front()?;
        tilo_ui_base::widget::controls::note_drained_origin(origin);
        Some(taken)
    }

    pub(crate) fn has_pending(&self) -> bool {
        !self.queue.borrow().is_empty()
    }

    /// Whether an edit applied now was refused since the last call, so the
    /// views are to be presented the stored state again.
    pub(crate) fn take_represent(&self) -> bool {
        self.represent.replace(false)
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

    /// Whether the app may apply an action of a resource `declared_by` declares as itself here:
    /// an action of its own, on its own record of the workspace, never another app's record or
    /// a nested one, nor a past version.
    fn applies_own(&self, declared_by: &str) -> Result<(), ActionError> {
        if declared_by != tilo_resource::kind::DEFAULT_APP && declared_by != self.app_id {
            return Err(ActionError::Refused(format!(
                "{} applies as itself only its own actions, not {declared_by}'s",
                self.app_id
            )));
        }
        if !self.record.is_empty() || self.applied_as.is_some() || self.past {
            return Err(ActionError::Refused(format!(
                "{} applies as itself only on its own record of the workspace",
                self.app_id
            )));
        }
        Ok(())
    }
}

/// The entry of the change `doc` committed since `before`, if it committed
/// one.
fn made_since(
    doc: &ui_core::automerge::AutoCommit,
    before: &[ChangeHash],
) -> Option<tilo_actions::ActionContext> {
    let heads = doc.committed_heads();
    if heads == before {
        return None;
    }
    let document = doc.committed_document();
    heads
        .into_iter()
        .find_map(|head| tilo_actions::entry_of(document, head))
}

impl<R: Resource> Dispatcher<R> {
    /// What does not name the resource type.
    pub(crate) fn core(&self) -> &DispatchCore {
        &self.core
    }

    /// The kind of the record a caret here is anchored to: the record this
    /// resource's edits apply as ([`Self::child`] names it), which is what
    /// actions and History name the text as — a page's rich text is the
    /// page's, never the text package's.
    pub fn anchored_kind(&self) -> &'static str {
        self.core.applied_as_kind().unwrap_or(R::KIND)
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
        me: Option<&MemberId>,
    ) -> Self {
        Self {
            core: DispatchCore::new(app_id, resource_id, (R::KIND, R::LABEL), read_only, me),
            group: Rc::default(),
            permission_of: Rc::new(tilo_resource::need_of::<R>),
            submit_now: Rc::new(|host, edits, action, context| {
                host.apply(edits, R::ACTION_TYPE, action, context)
            }),
            group_now: Some(Rc::new(|host, edits, group, context| {
                host.apply(
                    edits,
                    &tilo_resource::group::group_type::<R>(),
                    group,
                    context,
                )
            })),
            last: Rc::default(),
            reported_as: None,
        }
    }

    /// A dispatcher for a surface a test builds outside any host, editing as
    /// `me`: what the surface applies now ([`Self::apply_now`]) goes into the
    /// document the test hands it, as under a host. Queued edits
    /// ([`Self::apply`], [`Self::submit`]) wait for a host that never takes
    /// them; a test that needs those runs the app
    /// ([`HeadlessApp`](super::testing::HeadlessApp)).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn detached(app_id: &'static str, me: &MemberId) -> Self {
        Self::new(app_id, R::KIND, false, Some(me))
    }

    /// What a [`detached`](Self::detached) dispatcher handed to the API
    /// and nothing took yet — a destructive verb or a removal waiting for
    /// the person's confirmation — as the actions the API would apply once
    /// they agreed. Taking them empties the queue.
    #[cfg(any(test, feature = "test-support"))]
    pub fn take_asked(&self) -> Result<Vec<R::Action>, String> {
        self.core
            .queue
            .borrow_mut()
            .drain(..)
            .map(|((invocation, _), _)| {
                tilo_resource::build_action::<R>(
                    &invocation.address.action,
                    &invocation.arguments,
                    tilo_resource::origin_now_ms(),
                )
            })
            .collect()
    }

    /// Who edits through this dispatcher: the member this view is
    /// for, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). `None` in a
    /// view of a past version, which nobody edits.
    pub fn me(&self) -> Option<&MemberId> {
        self.core.me.as_ref()
    }

    /// A dispatcher for a past version in History: nothing it is given is
    /// ever applied.
    pub(crate) fn past(mut self) -> Self {
        self.core.past();
        self
    }

    /// The dispatcher of the record the workspace names by `route` (empty for
    /// the app's own record of the workspace).
    pub(crate) fn on_resource(mut self, route: &str) -> Self {
        self.core.record = Rc::from(route);
        self
    }

    /// Whether the view shows a past version in History mode, drawn from a
    /// frozen document. Such a view must start no work: no worker, no
    /// network, no files. A surface that would fetch or watch something
    /// presents what the document holds instead.
    pub fn is_past(&self) -> bool {
        self.core.past
    }

    /// Whether edits from this member will be refused, as of now: it
    /// follows role changes while the app runs. Surfaces use it to present
    /// themselves read-only instead of letting an edit bounce.
    pub fn read_only(&self) -> bool {
        self.core.read_only.get()
    }

    /// [`Self::read_only`] as observable state, for a widget that shows or
    /// disables something while the member may only view.
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

    /// The member's access in the workspace as the host keeps it when
    /// called: who they are, the workspace's policy and what bounds them. For a
    /// runtime that decides its own invocations (a tilolang program's) by
    /// the policy the API decides by. `None` outside a host.
    pub fn access_source(&self) -> impl Fn() -> Option<policy::MemberAccess> + 'static {
        let permissions = Rc::clone(&self.core.permissions);
        move || {
            permissions
                .borrow()
                .as_ref()
                .map(|live| live.current().access().clone())
        }
    }

    /// Whether the member may govern the workspace (invite, grant, change
    /// its settings), as observable view state: for an owner's pane.
    pub fn govern_state(&self) -> LocalState<bool> {
        self.core.asked_state(Asked::Govern)
    }

    /// Whether what the workspace holds has arrived, as observable view state:
    /// false while the window's workspace has not finished its first sync with
    /// its host ([`tilo_ui_base::workspace_sync`]), when a record joined from it may
    /// still be missing the content its creator wrote. True outside a workspace
    /// that syncs with a host, and for a host's own view of a record, which
    /// holds what the workspace holds.
    pub fn arrived_state(&self) -> LocalState<bool> {
        self.core.asked_state(Asked::Arrived)
    }

    /// The same dispatcher deciding by `permissions`, for a test of a
    /// surface whose member may only comment, or only view.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_permissions(self, permissions: LivePermissions) -> Self {
        self.core.follow(permissions);
        self.core.refresh_read_only();
        self
    }

    /// Whether the member may, as of now, do what `need` is: an
    /// undoable change for most edits, one of the comments for a reply.
    pub fn may(&self, need: &ActionNeed) -> bool {
        self.core.may(need)
    }

    /// Whether the member may, as of now, change the workspace's settings.
    pub fn may_govern(&self) -> bool {
        self.core.may_govern()
    }

    /// Whether the record is shared: someone else belongs to its workspace, or
    /// is in it with `people` now. Where it is, reactions are offered.
    pub fn is_shared(&self, people: &super::People) -> bool {
        self.core.workspace_has_others() || people.others_here()
    }

    /// Whether the member may apply `action`
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

    /// The last submission of this view the API applied, with what it
    /// answered: the ids a "new" verb minted ([`Landed::minted`]), for a
    /// surface that opens what it just created.
    pub fn landed(&self) -> LocalState<Option<Landed>> {
        self.core.landed.clone()
    }

    /// Say `text` of what was just submitted here ("Added Buy milk") once
    /// it is kept: at once in a record nobody else decides, once the workspace
    /// accepts the change in a shared one, and never when the workspace refuses
    /// it ([`Self::refused`] says why instead). The text shows through
    /// [`Self::said`].
    pub fn say_when_kept(&self, text: impl Into<String>) {
        self.core.notices.borrow_mut().push(text.into());
    }

    /// What [`Self::say_when_kept`] said that was kept, for the view to
    /// show and then clear.
    pub fn said(&self) -> LocalState<Option<String>> {
        self.core.said.clone()
    }

    /// Why `action` may not be applied by this member.
    fn not_allowed(&self, action: &R::Action) -> ActionError {
        ActionError::Refused(self.core.not_allowed(&(self.permission_of)(action)))
    }

    /// Why the app may not apply `action` as itself here, if it may not: only what the app keeps
    /// for itself (`#[action(by_app)]`), only an action of its own resource, only on its own
    /// record of the workspace, and only for an invoker who may make that change themselves.
    /// The app never does for someone what they could not do: what it writes is decided again
    /// by the workspace, under the grant installing the app gave it.
    ///
    /// # Errors
    /// Why not, in the words a person reads.
    pub(crate) fn may_apply_own(&self, action: &R::Action) -> Result<(), ActionError> {
        self.applies_own_checks(action)?;
        if !self.may_apply(action) {
            return Err(self.not_allowed(action));
        }
        Ok(())
    }

    /// [`Self::may_apply_own`] for the handler of a member-own effect
    /// ([`tilo_actions::MEMBER_OWN_TAG`]): the workspace's host runs it for the member it acts for,
    /// as them, so the app applies the member's own change and the workspace decides it again.
    /// The app's own role here binds it no more.
    pub(crate) fn may_apply_own_for_member(&self, action: &R::Action) -> Result<(), ActionError> {
        self.applies_own_checks(action)
    }

    /// The checks both of the above make, of an action applied as the app itself: what it keeps
    /// for itself, and only on its own record of the workspace.
    fn applies_own_checks(&self, action: &R::Action) -> Result<(), ActionError> {
        if !R::by_app(action) {
            return Err(ActionError::Refused(
                "an app applies as itself only what it keeps for itself".into(),
            ));
        }
        self.core.applies_own(R::APP_ID)
    }

    /// Apply `action`, which [`Self::may_apply_own`] admits, to the document `edits` lends, as
    /// the app itself: the app's own write, which is no step of the person's. One a member-own
    /// effect applies is that member's (`as_member`), decided by their grants.
    ///
    /// # Errors
    /// As [`Self::may_apply_own`], or the action's own refusal; nothing is written either way.
    pub(crate) fn apply_own_now(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
        as_member: Option<ActionInvoker>,
    ) -> Result<(), ActionError> {
        let context = self.core.context();
        let (context, for_member) = if let Some(member) = as_member {
            self.may_apply_own_for_member(action)?;
            (context.with_invoker(member), true)
        } else {
            self.may_apply_own(action)?;
            let app = tilo_actions::Acting::by_app(self.core.app_id);
            (context.with_invoker(ActionInvoker::admitted(&app)), false)
        };
        self.apply_now_with(edits, action, context, true, Some(false), for_member)
    }

    /// Invoke one of the app's effects (`dispatcher.effect(&Backups::BackUp)`)
    /// through the same API as a button bound to it: permission, the
    /// effect's availability and a destructive effect's confirmation are
    /// checked, then its handler runs. A member who may only view still
    /// invokes the effects declared `kind = "read"`.
    pub fn effect<E: Effect>(&self, effect: &E) {
        self.core.queue_invocation(
            ActionAddress::new(self.core.resource_id.as_ref(), effect.id()),
            effect.arguments(),
        );
    }

    /// Whether the person at this device may do `effect` at once, as their
    /// own gesture on the surface: a shortcut, a pointer gesture, a row of
    /// the surface's own menu, whose arguments the gesture itself supplied
    /// (the clipboard they paste). Like a keystroke, such a gesture is its
    /// own confirmation, so a surface told `true` performs the effect
    /// itself. The confirmation a destructive declaration asks for stays
    /// with the callers who supply its arguments — the palette, a script,
    /// an agent — because automation is the same API: under any
    /// invocation (an invoked action, an effect's handler or completion, a
    /// dispatcher lent to one) this is `false`, and the surface submits
    /// the effect ([`Self::effect`]) for the host to check and confirm as
    /// for any caller. So it is in a past version, and when the
    /// member's role does not grant what the declaration needs.
    pub fn is_own_gesture<E: Effect>(&self, effect: &E) -> bool {
        if !self.gesture_is_own() {
            return false;
        }
        let id = effect.id();
        E::declarations()
            .iter()
            .find(|decl| decl.id == id)
            .is_some_and(|decl| {
                self.may(&ActionNeed {
                    kind: decl.action_kind,
                    field: None,
                    privileged: false,
                })
            })
    }

    /// Whether what runs now is the person's own gesture at this device,
    /// not automation: no invocation, effect or past version is acting.
    fn gesture_is_own(&self) -> bool {
        !(self.core.past
            || super::effect::acting_for().is_some()
            || ui_core::invocation_scope::current_invoker().is_some())
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
    /// API, after the event, as a control bound to it would.
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

    /// [`submit`](Self::submit) what the app does on its own, for nobody's
    /// gesture: a read receipt, keeping a record in step with what is shown.
    /// It goes through the same API and permissions; a refusal is told in
    /// [`Self::refused`] and logged, but never said over the window as if
    /// the person had pressed something and been refused.
    pub fn submit_on_its_own(&self, submit: impl Submit<R>) {
        let before = self.core.asking.replace(Origin::App);
        self.submit(submit);
        self.core.asking.set(before);
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

    /// Queue `invoke` on the resource, or on the published row it names
    /// ([`Invoke::on`]): the page an address loads in, the download it stops.
    fn queue(&self, invoke: Invoke<R>) {
        let resource = invoke
            .row()
            .unwrap_or_else(|| self.core.resource_id.as_ref());
        let address = ActionAddress::new(resource, invoke.id);
        self.core.queue_invocation(address, invoke.arguments);
    }

    /// Whether the member may take `invoke`: what its declaration, or
    /// the typed value it applies, needs. One this resource does not declare
    /// (an edit bound to a surface's view state) is left to the host, which
    /// checks what its published action needs. A surface asks before work
    /// that only feeds a write (an index it would keep with a row), so a
    /// member who may not make the write is not sent it.
    pub fn may_invoke(&self, invoke: &Invoke<R>) -> bool {
        // A reaction is decided where the workspace places it, below the
        // reactions field, as the API decides it: someone who may only
        // comment may react too.
        if invoke.id == tilo_resource::FIELD_EDIT && reacts(&invoke.arguments) {
            return self.core.may_react();
        }
        if invoke.id == tilo_resource::APPLY || invoke.id == tilo_resource::FIELD_EDIT {
            return match invoke.build(0) {
                Ok(action) => self.may_apply(&action),
                Err(error) => {
                    // What the invocation applies cannot be read here, so the
                    // API decides what its published action needs.
                    tracing::warn!(%error, action = %invoke.id, "an invocation's typed value could not be built; the api decides");
                    !self.read_only()
                }
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
    /// member who may only view can search too.
    pub fn open_search(&self, request: &ui_core::widget::search_request::SearchRequest) {
        self.core.queue.borrow_mut().push_back((
            (
                ui_core::widget::search_request::open_search_invocation(request),
                super::effect::acting_for(),
            ),
            Origin::Person,
        ));
    }

    /// Ask for the link to `resource` (this resource or one nested in it,
    /// such as an item) as a control bound to its `share-link` would: the
    /// framework copies it to the clipboard and reports it in
    /// [`Self::landed`], or why there is none in [`Self::refused`]. A link
    /// grants nothing, so a member who may only view asks too.
    pub fn share_link(&self, resource: &str) {
        self.core.queue_invocation(
            ActionAddress::new(resource, tilo_resource::share::SHARE_LINK),
            serde_json::Map::new(),
        );
    }

    /// The client of the session's action API the views' opens and
    /// reads go through.
    pub(crate) fn opener(&self) -> &super::reference::Opener {
        &self.core.opener
    }

    /// Open what `href`, a `tilo://open#…` address a link in this view's
    /// text carries, names: the app that keeps that resource shows it,
    /// started first when no window does.
    pub fn open_link(&self, href: &str) -> Result<(), String> {
        super::reference::open_link(&self.core.opener, href)
    }

    /// Open `reference`, which `app` keeps in its `record` (`None`: the
    /// app's own record of the workspace), where it lives: its app opens at
    /// it, launched first when no window shows it. The open is an action
    /// like any other, admitted as the person under their grants; a refusal
    /// is told in [`Self::refused`] as `open`.
    pub fn open_reference(
        &self,
        app: &str,
        record: Option<&str>,
        reference: &tilo_reference::ResourceRef,
    ) {
        if let Err(reason) = self.core.opener.open(app, record, reference) {
            self.core.refuse(super::reference::OPEN, reason);
        }
    }

    /// Send `invoke`, an action of this resource, to the session's host
    /// instead of applying it here. The host decides it as the person's, as
    /// it decides any caller's, may first put it to the person on its own
    /// sheet (what a run asks is answered there, never by the app), and then
    /// directs it back to be applied. A refusal is told in
    /// [`Self::refused`].
    pub fn through_host(&self, invoke: &Invoke<R>) {
        if let Some(refusal) = invoke.refusal() {
            self.core.refuse(invoke.id, refusal.to_owned());
            return;
        }
        self.core.opener.invoke_own(
            ActionAddress::new(self.core.resource_id(), invoke.id),
            invoke.arguments.clone(),
        );
    }

    /// Send `call`, another resource's published action, to the session's
    /// host, continuing `parent`, the invocation the host delivered here for
    /// someone: the host decides it as theirs. A past version calls nothing.
    pub(crate) fn call_published(
        &self,
        parent: Option<tilo_actions::InvocationId>,
        call: &super::Call,
    ) -> Result<ui_core::InvocationCall, String> {
        if self.core.past {
            return Err("a past version calls nothing".into());
        }
        let (target, argument) = call.request()?;
        Ok(self.core.opener.call(parent, target, argument))
    }

    /// Invoke `address`, an action of the window's own record, as the person, continuing
    /// `parent`: for an effect that calls another of its record's actions and waits for it.
    pub(crate) fn call_own(
        &self,
        parent: Option<tilo_actions::InvocationId>,
        address: tilo_actions::ActionAddress,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<ui_core::InvocationCall, String> {
        if self.core.past {
            return Err("a past version calls nothing".into());
        }
        Ok(self.core.opener.call_own(parent, address, arguments))
    }

    /// Read `read`, a record of the window's workspace, through the host's
    /// record API, continuing `parent` as [`Self::call_published`] does.
    pub(crate) fn read_resource(
        &self,
        parent: Option<tilo_actions::InvocationId>,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Result<ui_core::remote_actions::ResourceReadCall, String> {
        if self.core.past {
            return Err("a past version reads nothing".into());
        }
        Ok(self.core.opener.read_resource(parent, read))
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

    /// Run `invoke`, one of the actions `actions` publishes (another
    /// record's, such as a settings section), through the host as a control
    /// bound to it runs it: where another provider keeps the record, it runs
    /// there. From an effect's handler or completion it continues that
    /// effect, as its caller's. A refusal is told in [`Self::refused`].
    pub fn invoke_on<T: Resource>(&self, actions: &super::Actions<T>, invoke: Invoke<T>) {
        if let Some(refusal) = invoke.refusal() {
            self.core.refuse(invoke.id, refusal.to_owned());
            return;
        }
        // A field edit of a settings section is the setter of its field.
        let table = actions.table();
        let (key, arguments) = table.as_published(
            tilo_resource::ActionKey::new(T::APP_ID, T::KIND, invoke.id),
            invoke.arguments,
        );
        self.invoke_at(
            ActionAddress::new(actions.resource_id(), key.action),
            arguments,
        );
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry. The API applies it after the
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
    /// applied now: it goes to the host's API, which asks first, and
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
        let _capture = self.capture_invocation(&invoke);
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        if asks(&action) {
            return self.ask_first(invoke, &action);
        }
        self.apply_now(edits, &action)
    }

    /// [`Self::submit_now`] of the person's own gesture on the surface: the
    /// button they pressed, the shortcut they typed, with arguments the
    /// gesture itself supplied. It is their press, so it waits for their yes
    /// where a press of it does ([`tilo_actions::press_confirms`]): a
    /// destructive verb Undo takes back unless they hold Shift, what Undo
    /// cannot take back and what leaves the system always. That goes through
    /// the host, which asks them; everything else applies now. Under any
    /// invocation (an agent's, an effect's handler, a past version) this is
    /// [`Self::submit_now`], and the host asks as for any caller.
    ///
    /// # Errors
    /// As [`Self::submit_now`].
    pub fn submit_gesture(
        &self,
        edits: &mut Edits<'_>,
        submit: impl Submit<R>,
    ) -> Result<(), ActionError> {
        if !self.gesture_is_own() {
            return self.submit_now(edits, submit);
        }
        let invoke = submit.into_invoke().map_err(ActionError::Refused)?;
        if invoke.is_group() {
            return self.group_now(edits, &invoke);
        }
        let action = invoke
            .build(tilo_resource::origin_now_ms())
            .map_err(ActionError::Refused)?;
        // The person's press waits for their yes where a press of the action does.
        if tilo_actions::press_confirms(&action).on_press(ui_core::widget::shift_held()) {
            return self.ask_first(invoke, &action);
        }
        let applied = self.apply_now_as(edits, &action, self.core.context(), true);
        // A view that showed the edit before it was refused (a row already
        // gone) shows the stored state again.
        if applied.is_err() {
            self.core.represent.set(true);
        }
        applied
    }

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
        let _nonlocal = capture_nonlocal();
        let _capture = self.capture_invocation(invoke);
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
        // A group holding a removal asks first, as the queued API does.
        if group.actions.iter().any(asks) {
            self.submit_invoke(invoke.clone());
            return Ok(());
        }
        let mut context = self.core.context();
        if let Some(me) = &self.core.me {
            context = context.with_invoker(ActionInvoker::member(me.clone()));
        }
        group_now(&mut self.core.host.borrow_mut(), edits, &group, context)?;
        // A group is a step of its own: nothing continues it.
        *self.last.borrow_mut() = None;
        self.core.grouped();
        Ok(())
    }

    /// Apply one typed action to the document `edits` lends — an event's,
    /// or a settle pass's — now, attributed to the member, with history
    /// recording what the action did.
    /// One event's actions are one undo step, and an action that continues
    /// the previous one ([`Resource::coalesces`], the next letter of a word)
    /// joins its step. An action that needs confirming (a destructive verb,
    /// a removal) is not applied now: it is submitted to the host's API,
    /// which asks the person first ([`Self::apply`]).
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] for a member who may only view,
    /// and the action's own refusal; nothing is written either way.
    pub fn apply_now(&self, edits: &mut Edits<'_>, action: &R::Action) -> Result<(), ActionError> {
        self.apply_now_as(edits, action, self.core.context(), false)
    }

    /// Apply `action` now as content from outside: the system finishing a
    /// call the person made (an outbox settlement), never a gesture of
    /// theirs. The change is marked external and is no step, so the person's
    /// undo never takes it back and it starts no step of its own.
    ///
    /// # Errors
    /// As [`apply_now`](Self::apply_now).
    pub(crate) fn apply_now_external(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
    ) -> Result<(), ActionError> {
        let context = self
            .core
            .context()
            .with_change(tilo_actions::ChangeKind::External);
        self.apply_now_with(edits, action, context, true, Some(false), false)
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

    /// Apply `action` now to the document `edits` lends, for `invoker`: a
    /// step an API carries out for whoever invoked it that writes this
    /// record too (a container listing a new child), attributed to them.
    ///
    /// # Errors
    /// As [`apply_now`](Self::apply_now).
    pub(crate) fn apply_now_for(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<(), ActionError> {
        let context = self.core.context().with_invoker(invoker.clone());
        self.apply_now_as(edits, action, context, true)
    }

    fn apply_now_as(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
        context: ActionContext,
        confirmed: bool,
    ) -> Result<(), ActionError> {
        self.apply_now_with(edits, action, context, confirmed, None, false)
    }

    /// [`Self::apply_now_as`], as a step of the person's unless `step` says
    /// otherwise: content from outside is no step of theirs.
    /// `for_member` says the app applies the action of a member-own effect's handler: the
    /// workspace's host admitted it for the member, as them, and its decision stands in place of
    /// the app's own role here.
    fn apply_now_with(
        &self,
        edits: &mut Edits<'_>,
        action: &R::Action,
        context: ActionContext,
        confirmed: bool,
        step: Option<bool>,
        for_member: bool,
    ) -> Result<(), ActionError> {
        #[cfg(debug_assertions)]
        not_painting();
        let _nonlocal = capture_nonlocal();
        self.core.applies_now()?;
        // A member-own effect applies for the member the workspace's host admitted it for: the
        // app's own role binds those writes no more.
        if !for_member && !self.may_apply(action) {
            return Err(self.not_allowed(action));
        }
        // Applying now never skips the person's consent: an action that
        // needs confirming goes to the API, which asks first.
        if asks(action) && !confirmed {
            self.apply(action);
            return Ok(());
        }
        // Content from outside is nobody's step: it neither continues the
        // open one nor ends it.
        let step = step.unwrap_or_else(|| tilo_actions::Action::change_kind(action).is_step());
        // Only an action right after this dispatcher's last one, with nothing
        // in between, can continue it.
        let serial = self.core.serial.get();
        let continues = step
            && self
                .last
                .borrow()
                .as_ref()
                .is_some_and(|(at, previous)| *at == serial && R::coalesces(previous, action));
        let opened = self.core.gesture.get();
        let before = edits.doc().committed_heads();
        let context = self.core.step_context(context, step, continues);
        let fallback = if !ui_core::local_document_action::input_is_active()
            || ui_core::local_document_action::has_invocation()
        {
            None
        } else {
            Some(Invoke::<R>::apply(action).map_err(ActionError::Encode)?)
        };
        let _capture = fallback
            .as_ref()
            .and_then(|invoke| self.capture_invocation(invoke));
        (self.submit_now)(&mut self.core.host.borrow_mut(), edits, action, context)?;
        if !step {
            return Ok(());
        }
        // The executor writes a person's call that sets the same value again
        // into the run it continues ([`tilo_actions::run`]): it is that
        // run's step, which stays open for the next call.
        let made = made_since(edits.doc(), &before);
        let continues = continues
            || made
                .as_ref()
                .is_some_and(|entry| opened.is_some() && entry.gesture == opened);
        if continues {
            self.core.gesture.set(opened);
        }
        let continuable = R::continuable(action)
            || made
                .as_ref()
                .is_some_and(|entry| tilo_actions::run::setting(entry).is_some());
        let serial = self.core.stepped(continues, continuable);
        *self.last.borrow_mut() = Some((serial, action.clone()));
        Ok(())
    }

    /// The dispatcher of another record than this resource's — the
    /// person's own record, a settings section — published at `id`: its
    /// queued invocations go through the same API as this resource's, and
    /// what it applies now `apply` applies to that record. What an action needs
    /// is its own declared need, decided where the record is held: the window's
    /// workspace's roles do not reach it.
    pub(crate) fn for_resource<C: Resource>(
        &self,
        id: &str,
        read_only: bool,
        apply: impl Fn(&C::Action) -> Result<(), ActionError> + 'static,
    ) -> Dispatcher<C> {
        Dispatcher {
            core: self.core.for_resource(id, (C::KIND, C::LABEL), read_only),
            group: Rc::default(),
            permission_of: Rc::new(tilo_resource::need_of::<C>),
            submit_now: Rc::new(move |_, _, action: &C::Action, _| apply(action)),
            group_now: None,
            last: Rc::default(),
            reported_as: None,
        }
    }

    /// The dispatcher of a record the app keeps beside its own in the workspace,
    /// published at `id` (the parts of a chain): its queued invocations are
    /// addressed to `id`, `apply` applies one now, `apply_group` several
    /// invoked as one, and what an action needs
    /// is its own declared need, checked against the member's roles in
    /// the workspace as this resource's actions are.
    pub(crate) fn for_workspace_resource<C: Resource>(
        &self,
        id: &str,
        read_only: bool,
        apply: impl Fn(&C::Action) -> Result<(), ActionError> + 'static,
        apply_group: impl Fn(&tilo_resource::group::ActionGroup<C::Action>) -> Result<(), ActionError>
        + 'static,
    ) -> Dispatcher<C> {
        let mut core = self.core.for_resource(id, (C::KIND, C::LABEL), read_only);
        core.permissions = Rc::clone(&self.core.permissions);
        let apply = Rc::new(apply);
        // The step the last gesture written here joined, and the input event
        // it was written in: a gesture that continues it (the next letter of
        // a word) joins it, and so does another opened in the same event (a
        // key press that indents each selected line), as the window's own
        // history keeps one event's edits one step.
        let stepped: Rc<Cell<Option<Stepped>>> = Rc::default();
        Dispatcher {
            core,
            group: Rc::default(),
            permission_of: Rc::new(tilo_resource::need_of::<C>),
            // The step the action opens or continues (the letters of a word)
            // is open while it is written, so the record's history takes
            // the step back as one.
            submit_now: Rc::new(move |_, _, action: &C::Action, context: ActionContext| {
                let event = tilo_ui_base::input_generation::generation();
                // Typing is a run of its own: a correction made as a word
                // ends is a step apart from the word.
                let typing = C::continuable(action);
                let input = ui_core::local_document_action::input_is_active();
                let step = context.gesture.map(|gesture| match stepped.get() {
                    Some(last) if last.gesture == gesture => last.step,
                    // Within one event the edits after the first join its
                    // step (Markdown typed and turned into formatting), but
                    // a word's correction is a step apart from the typing
                    // that ended the word under the person's keys.
                    Some(last)
                        if last.event == event
                            && !(typing && last.opened_by_typing && last.opened_by_input) =>
                    {
                        stepped.set(Some(Stepped {
                            gesture,
                            step: last.step,
                            ..last
                        }));
                        last.step
                    }
                    _ => {
                        stepped.set(Some(Stepped {
                            gesture,
                            step: gesture,
                            event,
                            opened_by_typing: typing,
                            opened_by_input: input,
                        }));
                        gesture
                    }
                });
                in_step(step, || (*apply)(action))
            }),
            // A group's members are written together, checked as a whole (a
            // start and an end that move together never pass through an
            // event that ends before it starts), in one step of their own
            // that history takes back as one.
            group_now: Some(Rc::new(move |_, _, group, _| {
                in_step(Some(tilo_actions::ActionId::default()), || {
                    apply_group(group)
                })
            })),
            last: Rc::default(),
            reported_as: None,
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
    /// action: the same permission check, history, undo and replay.
    ///
    /// The child is published with this one, at its own address `id`, with
    /// `C`'s actions and the same `wrap`
    /// ([`tilo_resource::publish_child`]): the palette, agents and scripts
    /// reach it like any resource, and an invocation addressed to it resolves
    /// and applies here. Publishing is the framework's, from this call, so no
    /// resource can offer an editor it does not publish.
    #[must_use]
    pub fn child<C: Resource>(
        &self,
        id: &str,
        wrap: impl Fn(C::Action) -> R::Action + Send + Sync + 'static,
    ) -> Dispatcher<C> {
        let parent = Rc::clone(&self.submit_now);
        let parent_permission = Rc::clone(&self.permission_of);
        let wrap: std::sync::Arc<dyn Fn(C::Action) -> R::Action + Send + Sync> =
            std::sync::Arc::new(wrap);
        let wrap_for_permission = std::sync::Arc::clone(&wrap);
        let wrap_for_report = std::sync::Arc::clone(&wrap);
        let parent_report = self.clone();
        self.publish_child::<C>(id, std::sync::Arc::clone(&wrap));
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
            reported_as: Some(Rc::new(move |invoke: &Invoke<C>| {
                let action = invoke.build(tilo_resource::origin_now_ms()).ok()?;
                parent_report.reported(&Invoke::<R>::apply(&wrap_for_report(action)).ok()?)
            })),
        }
    }

    /// Publish the nested editor `id` beside this resource, once per address:
    /// built the first time a view opens it, and kept from then on.
    fn publish_child<C: Resource>(
        &self,
        id: &str,
        wrap: std::sync::Arc<dyn Fn(C::Action) -> R::Action + Send + Sync>,
    ) {
        let mut nested = self.core.nested.borrow_mut();
        if nested.iter().any(|(published, _)| published == id) {
            return;
        }
        let mut child =
            tilo_resource::publish_child::<C, R>(id, C::LABEL, None, move |action| wrap(action));
        // The child's actions are what it applies here; its views are the
        // app's, offered while it shows the editor, and a host refuses a view
        // operation nothing handles.
        child
            .actions
            .retain(|spec| spec.kind == tilo_actions::OperationKind::Document);
        nested.push((id.to_owned(), child));
    }

    fn capture_invocation(
        &self,
        invoke: &Invoke<R>,
    ) -> Option<ui_core::local_document_action::InvocationScope> {
        if !ui_core::local_document_action::input_is_active()
            || super::effect::acting_for().is_some()
            || ui_core::local_document_action::has_invocation()
        {
            return None;
        }
        Some(ui_core::local_document_action::with_invocation(
            self.reported(invoke)?,
        ))
    }

    /// The published invocation `invoke` is reported to the broker as.
    fn reported(
        &self,
        invoke: &Invoke<R>,
    ) -> Option<ui_core::local_document_action::PublishedInvocation> {
        match &self.reported_as {
            Some(reported_as) => reported_as(invoke),
            None => Some(ui_core::local_document_action::PublishedInvocation {
                resource: self.core.resource_id.to_string(),
                action: invoke.id.to_owned(),
                arguments: invoke.arguments.clone(),
            }),
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

    pub(crate) fn name_resource(&self, label: &str) {
        self.core.name_resource(label);
    }

    /// The id of the app whose resource this is, which presence keys name
    /// ([`ResourceSelection::presence`](super::ResourceSelection::presence)).
    pub fn app_id(&self) -> &'static str {
        self.core.app_id
    }
}

/// Whether the member `core` edits for may take the action `id` the
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

/// An edit or effect the API refused ([`Dispatcher::refused`]), or one the
/// workspace refused after this device applied it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    /// The id of the refused action or effect: for one the workspace refused,
    /// the verb's key or the edited field's path.
    pub action: String,
    /// The item it acted on, when the workspace refused it and it named one.
    pub item: Option<String>,
    /// Why, for a person.
    pub reason: String,
    /// The workspace refused it: it was applied here, never reached anyone
    /// else, and was withdrawn again. Submitting it again is a new action.
    pub by_workspace: bool,
}

/// A submission the API applied ([`Dispatcher::landed`]).
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

/// Whether `action` needs the person's confirmation before it applies, by
/// the one rule every API asks (`tilo_actions::asks_first`): its risk asks
/// for it and Undo cannot take it back. A removal that only changes the
/// document runs at once; Undo is how the person takes it back. One that
/// loses something outside the document (`irreversible`) still asks.
fn asks<A: tilo_actions::Action>(action: &A) -> bool {
    tilo_actions::asks_first(
        tilo_actions::Action::risk(action),
        tilo_actions::Action::action_kind(action),
        tilo_actions::Action::change_kind(action),
        tilo_actions::Action::irreversible(action),
    )
}

/// Debug builds stop on a document write made while the window paints. A
/// frame reads the document in `sync` and writes what it took up in
/// `settle`, both before it paints: a write dispatched from `draw` lands
/// after the frame decided what it shows, so it waits for an unrelated event
/// or is never shown (and never synced). Take it up in `update` and write it
/// in `settle`.
#[cfg(debug_assertions)]
fn not_painting() {
    debug_assert!(
        !tilo_ui_base::widget::painting(),
        "a document write was dispatched from a widget's draw: take it up in update, and write it in settle"
    );
}

/// The submissions each effect queued that have not landed yet, and the
/// effects waiting for theirs.
#[derive(Default)]
struct Waits(std::collections::HashMap<tilo_actions::InvocationId, Wait>);

#[derive(Default)]
struct Wait {
    /// Queued, not yet answered by the API.
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

/// A local document action suspended while an effect acts: what it changes
/// is that effect's, not the person's gesture.
fn capture_nonlocal() -> Option<ui_core::local_document_action::Scope> {
    super::effect::acting_for()
        .is_some()
        .then(ui_core::local_document_action::suspend)
}
