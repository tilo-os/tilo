//! Submitting edits: every change a view makes is an action the host checks
//! against permissions, applies, records for undo and history, and
//! replicates.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{ActionAddress, ActionContext, ActionError, ActionInvoker, ControlInvocation};
use tilo_resource::{Doc, Effect, Invoke, Resource};
use ui_core::LocalState;
use ui_core::automerge::ChangeHash;

/// Submits a view's edits. Cloneable; clones share one queue.
///
/// Two doors, one path. [`invoke`](Self::invoke) and [`apply`](Self::apply)
/// queue an invocation the host resolves after the event, exactly as a
/// control bound to an action would. [`apply_now`](Self::apply_now) applies
/// one typed action immediately, inside the event, for an editor that must
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
///     let _ = self.dispatch.apply_now(ctx.state, &Action::SetLevel(level));
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
    /// Who edits through this dispatcher; `None` in a view of a past version.
    me: Option<Rc<str>>,
    queue: Rc<RefCell<VecDeque<ControlInvocation>>>,
    /// The invocations of the open group, and how many groups are open.
    group: Rc<RefCell<(usize, Vec<Invoke<R>>)>>,
    read_only: LocalState<bool>,
    refused: LocalState<Option<Refused>>,
    host: Rc<RefCell<ui_core::ActionHost>>,
    /// Applies one immediate action: the resource's own, or for a child
    /// resource its parent's action carrying it.
    submit_now: SubmitNow<R>,
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
    marker: std::marker::PhantomData<fn(R)>,
}

/// An immediate action and the serial it was applied at.
type Serialed<R> = (u64, <R as Resource>::Action);

/// Applies one typed action to a document through the host.
type SubmitNow<R> = Rc<
    dyn Fn(
        &mut ui_core::ActionHost,
        &mut Doc,
        &<R as Resource>::Action,
        ActionContext,
    ) -> Result<(), ActionError>,
>;

impl<R: Resource> Clone for Dispatcher<R> {
    fn clone(&self) -> Self {
        Self {
            app_id: self.app_id,
            resource_id: Rc::clone(&self.resource_id),
            me: self.me.clone(),
            queue: Rc::clone(&self.queue),
            group: Rc::clone(&self.group),
            read_only: self.read_only.clone(),
            refused: self.refused.clone(),
            host: Rc::clone(&self.host),
            submit_now: Rc::clone(&self.submit_now),
            serial: Rc::clone(&self.serial),
            last: Rc::clone(&self.last),
            continuing: Rc::clone(&self.continuing),
            applied: Rc::clone(&self.applied),
            opened: Rc::clone(&self.opened),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: Resource> Dispatcher<R> {
    pub(crate) fn new(
        app_id: &'static str,
        resource_id: &str,
        read_only: bool,
        me: Option<&str>,
    ) -> Self {
        Self {
            app_id,
            resource_id: Rc::from(resource_id),
            me: me.map(Rc::from),
            queue: Rc::default(),
            group: Rc::default(),
            read_only: LocalState::new(read_only),
            refused: LocalState::new(None),
            host: Rc::default(),
            submit_now: Rc::new(|host, doc, action, context| {
                host.try_dispatch_with_context(doc, R::ACTION_TYPE, action, context)
            }),
            serial: Rc::default(),
            last: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            marker: std::marker::PhantomData,
        }
    }

    /// Who edits through this dispatcher: the participant this view is
    /// for, the same id an action they invoke reads as its author
    /// ([`ActionCx::author`](tilo_resource::ActionCx::author)). `None` in a
    /// view of a past version, which nobody edits.
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
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

    fn view_only() -> String {
        format!("this {} is open to view only", R::LABEL.to_lowercase())
    }

    /// Record that the door refused `action`, submitted here.
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
    /// invokes the effects declared `permission = "read"`.
    pub fn effect<E: Effect>(&self, effect: &E) {
        self.queue.borrow_mut().push_back(ControlInvocation {
            address: ActionAddress::new(self.resource_id.as_ref(), effect.id()),
            arguments: effect.arguments(),
        });
    }

    /// Invoke a declared action (`dispatcher.invoke(Note::EDIT.with(text))`).
    pub fn invoke(&self, invoke: Invoke<R>) {
        if self.read_only() {
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
        self.submit(invoke);
    }

    fn submit(&self, invoke: Invoke<R>) {
        self.queue.borrow_mut().push_back(ControlInvocation {
            address: ActionAddress::new(self.resource_id.as_ref(), invoke.id),
            arguments: invoke.arguments,
        });
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
        self.queue
            .borrow_mut()
            .push_back(ControlInvocation { address, arguments });
    }

    /// Submit one typed action value — an edit with no named declaration,
    /// such as a drag's final geometry. The door applies it after the
    /// event; if it refuses it, [`Self::refused`] says why.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::apply(action) {
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
            (Some(only), None) => self.submit(only),
            (Some(first), Some(second)) => {
                self.submit(Invoke::group([first, second].into_iter().chain(members)));
            }
        }
    }

    /// Apply one typed action to `doc` — the document the surface's event
    /// handler received — now, attributed to the participant and labelled
    /// after the action (its declared label when it names a declared one).
    /// One event's actions are one undo step, and an action that continues
    /// the previous one ([`Resource::coalesces`], the next letter of a word)
    /// joins its step.
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] for a participant who may only view,
    /// and the action's own refusal; nothing is written either way.
    pub fn apply_now(&self, doc: &mut Doc, action: &R::Action) -> Result<(), ActionError> {
        let label = tilo_resource::action_label::<R>(action)
            .unwrap_or_else(|| ActionContext::for_action("", "", action).action_label);
        let context =
            ActionContext::for_resource(label, self.resource_id.as_ref(), R::KIND, R::LABEL);
        self.apply_now_with(doc, action, context)
    }

    /// [`apply_now`](Self::apply_now) with the history context the surface
    /// already has (its app, a purpose, a work group).
    ///
    /// # Errors
    /// As [`apply_now`](Self::apply_now).
    pub fn apply_now_with(
        &self,
        doc: &mut Doc,
        action: &R::Action,
        context: ActionContext,
    ) -> Result<(), ActionError> {
        if self.read_only() {
            return Err(ActionError::Refused(Self::view_only()));
        }
        // History names the resource and who edited it even when the
        // surface's context does not.
        let mut named =
            ActionContext::for_resource("", self.resource_id.as_ref(), R::KIND, R::LABEL);
        if let Some(me) = &self.me {
            named = named.with_invoker(ActionInvoker::actor(me.as_ref()));
        }
        let context = context.enriched_by(&named);
        (self.submit_now)(&mut self.host.borrow_mut(), doc, action, context)?;
        // Content from outside is nobody's step: it neither continues the
        // open one nor ends it.
        if R::annotates(action) || !tilo_actions::Action::change_kind(action).is_step() {
            return Ok(());
        }
        // Only an action right after this dispatcher's last one, with nothing
        // in between, can continue it.
        let serial = self.serial.get();
        let continues = self
            .last
            .borrow()
            .as_ref()
            .is_some_and(|(at, previous)| *at == serial && R::coalesces(previous, action));
        let serial = serial.wrapping_add(1);
        self.serial.set(serial);
        *self.last.borrow_mut() = Some((serial, action.clone()));
        // The event continues the open step only if every action it applied
        // did.
        let first_of_event = !self.applied.replace(true);
        self.continuing
            .set(continues && (first_of_event || self.continuing.get()));
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
        Dispatcher {
            app_id: self.app_id,
            me: self.me.clone(),
            resource_id: Rc::from(id),
            queue: Rc::clone(&self.queue),
            group: Rc::default(),
            read_only: self.read_only.clone(),
            refused: self.refused.clone(),
            host: Rc::clone(&self.host),
            // History names the child and its edit; what applies is the
            // parent's action carrying it.
            submit_now: Rc::new(move |host, doc, action: &C::Action, context| {
                parent(host, doc, &wrap(action.clone()), context)
            }),
            serial: Rc::clone(&self.serial),
            last: Rc::default(),
            continuing: Rc::clone(&self.continuing),
            applied: Rc::clone(&self.applied),
            opened: Rc::clone(&self.opened),
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

    pub(crate) fn take(&self) -> Option<ControlInvocation> {
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
        self.opened.set(bracket.is_some() && !continuing);
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
