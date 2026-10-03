//! Submitting edits: every change a view makes is an action the host checks
//! against permissions, applies, records for undo and history, and
//! replicates.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use tilo_actions::{ActionAddress, ActionContext, ActionError, ControlInvocation};
use tilo_resource::{Doc, Invoke, Resource};
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
pub struct Dispatcher<R: Resource> {
    app_id: &'static str,
    resource_id: Rc<str>,
    queue: Rc<RefCell<VecDeque<ControlInvocation>>>,
    /// The invocations of the open group, and how many groups are open.
    group: Rc<RefCell<(usize, Vec<Invoke<R>>)>>,
    read_only: LocalState<bool>,
    host: Rc<RefCell<ui_core::ActionHost>>,
    /// The last immediate action, to tell whether the next continues it.
    last: Rc<RefCell<Option<R::Action>>>,
    /// Every immediate action since the last undo boundary continued the one
    /// before it ([`Resource::coalesces`]): one step, still open.
    continuing: Rc<Cell<bool>>,
    /// An immediate action was applied since the last boundary was taken.
    applied: Rc<Cell<bool>>,
    /// The boundary last taken opened a step that stays open.
    opened: Rc<Cell<bool>>,
    marker: std::marker::PhantomData<fn(R)>,
}

impl<R: Resource> Clone for Dispatcher<R> {
    fn clone(&self) -> Self {
        Self {
            app_id: self.app_id,
            resource_id: Rc::clone(&self.resource_id),
            queue: Rc::clone(&self.queue),
            group: Rc::clone(&self.group),
            read_only: self.read_only.clone(),
            host: Rc::clone(&self.host),
            last: Rc::clone(&self.last),
            continuing: Rc::clone(&self.continuing),
            applied: Rc::clone(&self.applied),
            opened: Rc::clone(&self.opened),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: Resource> Dispatcher<R> {
    pub(crate) fn new(app_id: &'static str, resource_id: &str, read_only: bool) -> Self {
        Self {
            app_id,
            resource_id: Rc::from(resource_id),
            queue: Rc::default(),
            group: Rc::default(),
            read_only: LocalState::new(read_only),
            host: Rc::default(),
            last: Rc::default(),
            continuing: Rc::default(),
            applied: Rc::default(),
            opened: Rc::default(),
            marker: std::marker::PhantomData,
        }
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

    /// Invoke a declared action (`dispatcher.invoke(Note::EDIT.with(text))`).
    pub fn invoke(&self, invoke: Invoke<R>) {
        if self.read_only() {
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

    /// Open the platform search with `request`, narrowed to this app or one
    /// part of it (`SearchRequest::within`). Searching is a view, so a
    /// participant who may only view can search too.
    pub fn open_search(&self, request: &ui_core::search::SearchRequest) {
        self.queue
            .borrow_mut()
            .push_back(ui_core::search::open_search_invocation(request));
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
    /// such as a drag's final geometry.
    pub fn apply(&self, action: &R::Action) {
        match Invoke::<R>::apply(action) {
            Ok(invoke) => self.invoke(invoke),
            Err(error) => tracing::warn!(%error, "action does not serialize; not submitted"),
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
    /// after the action. One event's actions are one undo step, and an
    /// action that continues the previous one ([`Resource::coalesces`], the
    /// next letter of a word) joins its step.
    ///
    /// # Errors
    /// Returns [`ActionError::Refused`] for a participant who may only view,
    /// and the action's own refusal; nothing is written either way.
    pub fn apply_now(&self, doc: &mut Doc, action: &R::Action) -> Result<(), ActionError> {
        let label = ActionContext::for_action("", "", action).action_label;
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
            return Err(ActionError::Refused(format!(
                "this {} is open to view only",
                R::LABEL.to_lowercase()
            )));
        }
        // History names the resource even when the surface's context does not.
        let context = context.enriched_by(&ActionContext::for_resource(
            "",
            self.resource_id.as_ref(),
            R::KIND,
            R::LABEL,
        ));
        self.host
            .borrow_mut()
            .try_dispatch_with_context(doc, R::ACTION_TYPE, action, context)?;
        if R::annotates(action) {
            return Ok(());
        }
        let continues = self
            .last
            .borrow()
            .as_ref()
            .is_some_and(|previous| R::coalesces(previous, action));
        *self.last.borrow_mut() = Some(action.clone());
        // The event continues the open step only if every action it applied
        // did.
        let first_of_event = !self.applied.replace(true);
        self.continuing
            .set(continues && (first_of_event || self.continuing.get()));
        Ok(())
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
