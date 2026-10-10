//! Seeing and driving undo from inside a widget.
//!
//! Undo itself lives on [`State`](crate::state::State) / [`UndoLog`](crate::state::UndoLog),
//! and the host ([`App`](crate::app::App)) drives it from Ctrl+Z. A
//! [`Widget`](crate::widget::Widget), though, only ever receives
//! `&mut AutoCommit` — so on its own it can neither ask *"is there anything to
//! undo?"* nor *do* it. That is why apps shipped a shortcut hint instead of an
//! Undo button: not because nobody wanted one, but because a widget had no way
//! to render one or make it work.
//!
//! This module is the missing channel, and it is deliberately two one-way
//! halves rather than a handle onto the whole `State`:
//!
//! * **Host → widget.** Once per frame the host publishes an [`UndoStatus`] to
//!   every laid-out widget via
//!   [`UndoControl::set_undo_status`](crate::undo::UndoControl::set_undo_status). A
//!   widget stores it (embed an [`UndoAccess`]) and reads it from `draw` and —
//!   the part that matters for assistive technology — from `accessibility`,
//!   which only gets `&self`.
//! * **Widget → host.** A widget records an [`UndoRequest`], and the host
//!   drains it via
//!   [`UndoControl::take_undo_request`](crate::undo::UndoControl::take_undo_request)
//!   and performs it through `App::perform_undo`
//!   — the *same* method the Ctrl+Z key handler calls. There is one undo
//!   implementation, so a button and the shortcut cannot drift apart.
//!
//! Both halves sweep the host's `node_map`, which every laid-out widget
//! registers itself in during `build_layout`. So a control works at any depth
//! with no container forwarding — the same mechanism
//! [`Widget::cursor_shape`](crate::widget::Widget::cursor_shape) and
//! [`Widget::tooltip`](crate::widget::Widget::tooltip) already use.
//!
//! There is a third channel in the same family, one step earlier: an app whose
//! edits are dispatched [`tilo_actions`](crate::action_host) actions reports the
//! bracket around them through
//! [`CommittedActions::take_committed_action`](crate::undo::CommittedActions::take_committed_action),
//! and [`record_dispatched_action`] is where a host turns that into the undo
//! entry the two halves above then see. It sweeps `node_map` for the same
//! reason they do.
//!
//! Most apps need none of this directly: drop an
//! [`UndoButton`](crate::widget::UndoButton) into a toolbar and it is wired.

// Canonical Edit operation identity is shared with portable widget dispatch.
pub(crate) const EDIT_OPERATION: &str = "tilo.host.edit";

/// What the live undo history can do *right now*, as a widget sees it.
///
/// Published by the host once per frame. Every field is derived from the real
/// [`UndoLog`](crate::state::UndoLog) at publish time and is scoped to the
/// local author exactly as Ctrl+Z is — so `can_undo` is false precisely when
/// pressing Ctrl+Z would do nothing, and a control rendered from it cannot
/// promise an action the shortcut would refuse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoStatus {
    /// There is an action this user can undo.
    pub can_undo: bool,
    /// There is an action this user can redo.
    pub can_redo: bool,
    /// Short description of the action undo would revert, derived from the
    /// change itself (`"+12 -3 chars"`, `"+1 item"`, `"2 fields"`) by the same
    /// producer the history pane uses — see
    /// [`history_source::change_summary`](crate::history_source::change_summary).
    ///
    /// `None` when there is nothing to undo. It is deliberately never a guess:
    /// a change whose effect cannot be described reports
    /// [`NO_VISIBLE_CHANGE`](crate::history_source::NO_VISIBLE_CHANGE).
    pub undo_summary: Option<String>,
    /// The mirror of [`undo_summary`](Self::undo_summary) for redo: what
    /// re-applying would put back.
    pub redo_summary: Option<String>,
}

impl UndoStatus {
    /// The status of a history in which nothing has happened yet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Whether the verb `which` is available.
    pub fn can(&self, which: UndoRequest) -> bool {
        match which {
            UndoRequest::Undo => self.can_undo,
            UndoRequest::Redo => self.can_redo,
        }
    }

    /// The summary for the verb `which`, if any.
    pub fn summary(&self, which: UndoRequest) -> Option<&str> {
        match which {
            UndoRequest::Undo => self.undo_summary.as_deref(),
            UndoRequest::Redo => self.redo_summary.as_deref(),
        }
    }

    /// The sentence an assistive technology should hear as the control's
    /// *description*: what the verb would do, or the reason it
    /// is unavailable.
    ///
    /// `"Nothing to undo"` when unavailable — never a description of an action
    /// that is not there.
    pub fn description(&self, which: UndoRequest) -> String {
        if !self.can(which) {
            return format!("Nothing to {}", which.verb());
        }
        match self.summary(which) {
            Some(s) if s != crate::history_source::NO_VISIBLE_CHANGE => {
                format!("{}: {s}", which.title())
            }
            _ => which.title().to_string(),
        }
    }
}

/// A widget's request that the host drive the history. The host performs it
/// through the one undo implementation; a widget never touches the stacks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoRequest {
    Undo,
    Redo,
}

impl UndoRequest {
    /// Lower-case verb, for composed sentences (`"Nothing to undo"`).
    pub fn verb(self) -> &'static str {
        match self {
            UndoRequest::Undo => "undo",
            UndoRequest::Redo => "redo",
        }
    }

    /// Title-case label, for a control's accessible name.
    pub fn title(self) -> &'static str {
        match self {
            UndoRequest::Undo => "Undo",
            UndoRequest::Redo => "Redo",
        }
    }

    /// The keyboard shortcut the host binds to this verb, for a tooltip — so
    /// the control teaches the shortcut rather than replacing it.
    pub fn shortcut(self) -> &'static str {
        match self {
            UndoRequest::Undo => "Ctrl+Z",
            UndoRequest::Redo => "Ctrl+Shift+Z",
        }
    }
}

/// Which history verb, if any, a command-modified character key means.
///
/// Both the live host ([`App`](crate::app::App)) and the headless
/// [`Driver`](crate::headless::Driver) resolve the key through this, so an app
/// driven by a test scenario undoes on the same keystroke the user's fingers
/// do. The binding itself is the host-shortcut table in
/// [`platform`](crate::platform::HostShortcut). `character` is winit's logical
/// key text; `shift` is read from the modifier state rather than from the
/// character's case, because a shifted "z" arrives as `"Z"` on some layouts
/// and `"z"` on others.
///
/// Returns `None` when the key is not a history binding — including when
/// the command modifier is not held.
pub fn shortcut(
    conventions: crate::platform::PlatformConventions,
    character: &str,
    command: bool,
    shift: bool,
) -> Option<UndoRequest> {
    use crate::platform::HostShortcut;
    if !command {
        return None;
    }
    match HostShortcut::resolve(conventions, character, shift)? {
        HostShortcut::Undo => Some(UndoRequest::Undo),
        HostShortcut::Redo => Some(UndoRequest::Redo),
        HostShortcut::Search | HostShortcut::History | HostShortcut::Sidebar => None,
    }
}

/// Turn the actions a widget dispatched during the last event into exactly one
/// undo boundary. Returns whether an entry was recorded.
///
/// The counterpart of [`shortcut`] for the *other* direction: the single place
/// "an edit became an undo boundary" is written down, so a widget behaves
/// identically under the live host ([`App`](crate::app::App)) and under the
/// headless [`Driver`](crate::headless::Driver). Both call this once per tick.
///
/// It matters that there is only one of these, because there are two separate
/// ways to get it wrong and both are silent:
///
/// * **Not draining at all.** [`take_committed_action`](crate::undo::CommittedActions::take_committed_action)
///   is rising-edge, so a host that never reads it records nothing — an app
///   whose edits all travel through [`tilo_actions`](crate::action_host) then has
///   an empty undo history no matter how much the user changed. The driver did
///   exactly this, which made undo unobservable to every headless test of such
///   an app.
/// * **Draining anonymously.** Undo is author-scoped: an entry with no author
///   is skipped by the local user's own Ctrl+Z, which then reverts an *older*
///   entry instead. So the boundary must be attributed, and to the same
///   identity [`State::undo_as`](crate::state::State::undo_as) is asked about.
///
/// * **Draining only the root.** This is the third host→widget channel of its
///   kind, and it was the only one that read a single widget: no container in
///   the toolkit forwards `take_committed_action`, so an app that put its
///   editor inside a `Row`, a `Column`, or a `NavShell` lost every boundary
///   with no error anywhere. Like [`UndoStatus`] and [`UndoRequest`], this
///   sweeps `node_map` — every laid-out widget registers itself there — so a
///   widget is reached at any depth.
///
/// Every bracket found is drained, even when the actions turned out to change
/// nothing: the rising edge must be cleared, or a later unrelated edit would be
/// bracketed from a stale point and undone along with it.
///
/// One tick is one boundary. When several widgets dispatched in the same tick,
/// the entry spans from the *earliest* of their brackets — the one the others
/// descend from — so a single Ctrl+Z reverts the tick rather than peeling it
/// apart in an order the user never performed.
///
/// # Safety
///
/// `root` and the `node_map` pointers must point at widgets that are alive for
/// the call — the invariant every `node_map` sweep in
/// [`App`](crate::app::App) and [`Driver`](crate::headless::Driver) already
/// relies on. Exactly one `&mut` is live at a time, so a widget that is both
/// the root and registered in the map is safe (and yields its bracket once,
/// because reading clears it).
pub fn record_dispatched_action<S: std::hash::BuildHasher>(
    state: &mut crate::state::State,
    root: *mut dyn crate::widget::Widget,
    node_map: &std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget, S>,
    author: &crate::permissions::UserId,
) -> bool {
    let mut brackets: Vec<Vec<automerge::ChangeHash>> = Vec::new();
    let mut revert = None;
    let mut visited = std::collections::HashSet::new();
    for w_ptr in std::iter::once(root).chain(node_map.values().copied()) {
        // SAFETY: see the function's safety note; the pointer is live.
        if !visited.insert(unsafe { crate::widget::node_sweep_identity(w_ptr) }) {
            continue;
        }
        // SAFETY: see the function's safety note. The reference is created and
        // dropped inside this iteration, so no two overlap.
        let w = unsafe { &mut *w_ptr };
        if let Some(before) = crate::undo::take_committed_action(w) {
            brackets.push(before);
            if revert.is_none() {
                revert = w.take_action_revert();
            }
        }
    }
    let Some(before) = earliest_bracket(state, brackets) else {
        return false;
    };
    if state.doc.get_heads() == before {
        return false;
    }
    let (undo_via, redo_via) = match revert {
        Some((undo, redo)) => (Some(undo), Some(redo)),
        None => (None, None),
    };
    state.record_committed_action_authored_with_revert(
        before,
        None,
        None,
        Some(author.clone()),
        undo_via,
        redo_via,
    );
    true
}

/// Bound host variant. The exclusive owner keeps root and both maps stable.
pub(crate) unsafe fn record_dispatched_action_bound(
    state: &mut crate::state::State,
    root: *mut dyn crate::widget::Widget,
    nodes: &std::collections::HashMap<taffy::NodeId, *mut dyn crate::widget::Widget>,
    bindings: &crate::bound_dispatch::BoundNodeMap,
    author: &crate::permissions::UserId,
    authority: &crate::permissions::PermissionContext,
) -> bool {
    let mut brackets = Vec::new();
    {
        // SAFETY: the host provides its live, exclusively owned root. This borrow
        // ends before any descendant pointer is admitted below.
        let root = unsafe { &mut *root };
        if let Ok(_entry) = crate::bound_dispatch::enter_widget(root)
            && let Some(before) = crate::undo::take_committed_action(root)
        {
            brackets.push(before);
        }
    }
    let mut visited = std::collections::HashSet::new();
    // SAFETY: the root and every node pointer are live for this exclusive sweep.
    visited.insert(unsafe { crate::widget::node_sweep_identity(root) });
    let mut bound_changed = false;
    for (&node, &pointer) in nodes {
        // SAFETY: as above; no other borrow of this node is live.
        if !visited.insert(unsafe { crate::widget::node_sweep_identity(pointer) }) {
            continue;
        }
        if let Some(binding) = bindings.binding_for(node) {
            // SAFETY: the stable host topology supplies exclusive node access;
            // metadata/lifetime checks precede delivery under its canonical Doc.
            let drained = binding.with_record_frame(&mut state.doc, false, |doc, _| unsafe {
                crate::bound_dispatch::mutate_bound_node(bindings, nodes, node, |widget| {
                    binding.drain_widget_actions(widget, doc)
                })
            });
            bound_changed |= matches!(drained, Ok(Ok(Ok(true))));
        } else if bindings.legacy_for(node).is_some() {
            // SAFETY: the exact live map has exclusive ownership for this sweep.
            bound_changed |= unsafe {
                crate::bound_dispatch::drain_legacy_node_action(
                    bindings, nodes, node, authority, author,
                )
            }
            .unwrap_or(false);
        } else {
            // SAFETY: each unique live node is visited only once this sweep.
            if let Ok(Some(before)) = unsafe {
                crate::bound_dispatch::mutate_bound_node(bindings, nodes, node, |widget| {
                    crate::undo::take_committed_action(widget)
                })
            } {
                brackets.push(before);
            }
        }
    }
    let Some(before) = earliest_bracket(state, brackets) else {
        return bound_changed;
    };
    if state.doc.get_heads() == before {
        return false;
    }
    state.record_committed_action_authored(before, None, None, Some(author.clone()));
    true
}

/// A widget whose own controls dispatch the actions its resource exports,
/// and so report the undo boundary around them (see
/// [`record_dispatched_action`]).
pub trait CommittedActions {
    /// The document heads captured just before the widget's last dispatched
    /// action. Rising-edge: reading clears it.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>>;

    /// Whether the undo step the last boundary opened stays open for the
    /// actions that continue it (the rest of a word typed a keystroke at a
    /// time). Answered by the widget itself, never its children.
    fn keeps_undo_step_open(&self) -> bool {
        false
    }
}

/// A control that shows or drives the document's undo history (an Undo
/// button). The host reaches every laid-out widget through its `node_map`, so
/// a control works at any depth with no container forwarding.
pub trait UndoControl {
    /// Host → widget: the live undo state, published once per frame. Call
    /// the provider only when the status is shown; a historical summary can
    /// need a document diff.
    fn set_undo_status(&mut self, _status: &mut dyn FnMut() -> UndoStatus) {}

    /// Widget → host: an undo or redo the user asked for, performed through
    /// the same path as Ctrl+Z. Rising-edge: reading clears it.
    fn take_undo_request(&mut self) -> Option<UndoRequest> {
        None
    }
}

/// A focused component's observed history, or an original-field observation
/// that is not available yet. Pending is never replaced by document history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalUndoStatus {
    Ready(UndoStatus),
    Pending,
}
impl LocalUndoStatus {
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn key(&self) -> (bool, bool, bool) {
        match self {
            Self::Ready(status) => (true, status.can_undo, status.can_redo),
            Self::Pending => (false, false, false),
        }
    }
}

/// A component with its own editing history, offered the host's keyboard
/// undo/redo shortcut before the application document consumes it.
pub trait LocalUndo {
    /// None means this component does not own the focused history. Pending
    /// means it owns it, but cannot currently advertise an available entry.
    fn local_undo_status(&self) -> Option<LocalUndoStatus> {
        None
    }

    /// True when exported Edit operations complete through Widget's existing
    /// asynchronous operation receipts rather than on shortcut dispatch.
    fn local_undo_is_deferred(&self) -> bool {
        false
    }

    /// `true` only while focus is inside the component and it consumed the
    /// request.
    fn handle_local_undo_request(&mut self, request: UndoRequest) -> bool;

    /// Admit a deferred system Edit request without exposing the host's raw
    /// operation protocol to apps. True means queued, never already succeeded.
    /// The component retains this invocation and reports its actual native ACK
    /// through `poll_local_undo_operation`. Required hosted custody continues to
    /// be declined by the generic adapter; this interface supplies no grant.
    /// # Errors
    /// Returns the original field's admission refusal.
    fn start_local_undo_operation(
        &mut self,
        _invocation: tilo_actions::InvocationId,
        _request: UndoRequest,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Complete only this component's previously admitted native Edit request.
    /// A lost ACK must fail explicitly; it must not become document Undo.
    fn poll_local_undo_operation(
        &mut self,
        _invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
    ) -> Option<tilo_actions::InvocationUpdate> {
        None
    }
}

pub(crate) fn has_deferred_local_history(widget: &(impl crate::widget::Widget + ?Sized)) -> bool {
    crate::widget::capability::capability::<dyn LocalUndo>(widget)
        .is_some_and(LocalUndo::local_undo_is_deferred)
}

pub(crate) fn start_local_history(
    widget: &mut (impl crate::widget::Widget + ?Sized),
    operation: &crate::widget::ResourceOperation<'_>,
) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
    let Some(request) = operation.unscoped() else {
        return Ok(None);
    };
    if request.type_id != EDIT_OPERATION {
        return Ok(None);
    }
    let Some(local) = crate::widget::capability::capability_mut::<dyn LocalUndo>(widget) else {
        return Ok(None);
    };
    if !local.local_undo_is_deferred() {
        return Ok(None);
    }
    let direction =
        serde_json::from_slice(request.payload).map_err(|_| "Invalid native Edit request")?;
    Ok(local
        .start_local_undo_operation(request.invocation, direction)?
        .then_some(crate::widget::ResourceOperationStart::Accepted))
}

pub(crate) fn poll_local_history(
    widget: &mut (impl crate::widget::Widget + ?Sized),
    invocation: tilo_actions::InvocationId,
    cancellation_requested: bool,
) -> Option<tilo_actions::InvocationUpdate> {
    crate::widget::capability::capability_mut::<dyn LocalUndo>(widget)?
        .poll_local_undo_operation(invocation, cancellation_requested)
}

/// `widget`'s pending action bracket, if it reports one.
pub fn take_committed_action(
    widget: &mut (impl crate::widget::Widget + ?Sized),
) -> Option<Vec<automerge::ChangeHash>> {
    crate::widget::capability::capability_mut::<dyn CommittedActions>(widget)?
        .take_committed_action()
}

/// Whether `widget` keeps its last undo step open.
pub fn keeps_undo_step_open(widget: &(impl crate::widget::Widget + ?Sized)) -> bool {
    crate::widget::capability::capability::<dyn CommittedActions>(widget)
        .is_some_and(CommittedActions::keeps_undo_step_open)
}

/// Publish the undo status to `widget` if it shows it.
pub fn set_undo_status(
    widget: &mut (impl crate::widget::Widget + ?Sized),
    status: &mut dyn FnMut() -> UndoStatus,
) {
    if let Some(control) = crate::widget::capability::capability_mut::<dyn UndoControl>(widget) {
        control.set_undo_status(status);
    }
}

/// The undo or redo `widget` asked for, if any.
pub fn take_undo_request(
    widget: &mut (impl crate::widget::Widget + ?Sized),
) -> Option<UndoRequest> {
    crate::widget::capability::capability_mut::<dyn UndoControl>(widget)?.take_undo_request()
}

/// Observe the same component histories the host offers its UndoRequest.
pub fn local_undo_status(widget: &dyn crate::widget::Widget) -> Option<LocalUndoStatus> {
    if let Some(local) = crate::widget::capability::capability::<dyn LocalUndo>(widget) {
        return local.local_undo_status();
    }
    widget.children().into_iter().find_map(local_undo_status)
}

pub(crate) fn local_undo_is_deferred(widget: &dyn crate::widget::Widget) -> bool {
    if let Some(local) = crate::widget::capability::capability::<dyn LocalUndo>(widget) {
        return local.local_undo_status().is_some() && local.local_undo_is_deferred();
    }
    widget
        .children()
        .into_iter()
        .find(|child| local_undo_status(*child).is_some())
        .is_some_and(local_undo_is_deferred)
}

/// Offer a keyboard undo/redo to the component histories under `widget`.
pub fn handle_local_undo_request(
    widget: &mut dyn crate::widget::Widget,
    request: UndoRequest,
) -> bool {
    if let Some(local) = crate::widget::capability::capability_mut::<dyn LocalUndo>(&mut *widget) {
        return local.handle_local_undo_request(request);
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| handle_local_undo_request(child, request))
}

/// A widget that restores its ephemeral state (caret, selection) around the
/// undo points its owner's actions make.
pub trait UndoPoints: crate::widget::Widget {
    /// Ephemeral state (caret, selection) saved with an undo point.
    fn get_state(&self) -> Option<String> {
        None
    }

    /// Restore ephemeral state saved by [`Self::get_state`].
    fn set_state(&mut self, _state: &str) {}
}

/// `widget`'s ephemeral state.
pub fn get_state(widget: &(impl crate::widget::Widget + ?Sized)) -> Option<String> {
    crate::widget::capability::capability::<dyn UndoPoints>(widget)?.get_state()
}

/// Restore `widget`'s ephemeral state.
pub fn set_state(widget: &mut (impl crate::widget::Widget + ?Sized), state: &str) {
    if let Some(points) = crate::widget::capability::capability_mut::<dyn UndoPoints>(widget) {
        points.set_state(state);
    }
}

/// The bracket the others descend from, or `None` when there were none.
///
/// A change graph has no total order over heads, so "earliest" is asked of the
/// document rather than guessed: the bracket with the most changes after it is
/// the one furthest back. The single-bracket case — every tick — skips the
/// question entirely.
fn earliest_bracket(
    state: &mut crate::state::State,
    mut brackets: Vec<Vec<automerge::ChangeHash>>,
) -> Option<Vec<automerge::ChangeHash>> {
    match brackets.len() {
        0 => None,
        1 => brackets.pop(),
        _ => brackets
            .into_iter()
            .map(|b| {
                let depth = state.doc.get_changes(&b).len();
                (depth, b)
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, b)| b),
    }
}

/// The undo half of a widget, as one embeddable field: the last
/// [`UndoStatus`] the host published, plus at most one pending
/// [`UndoRequest`].
///
/// Embed it and wire the two `Widget` methods:
///
/// ```ignore
/// struct MyToolbar { undo: tilo_ui::UndoAccess, /* … */ }
///
/// impl Widget for MyToolbar {
///     fn set_undo_status(&mut self, s: &mut dyn FnMut() -> tilo_ui::UndoStatus) { self.undo.publish(&s()); }
///     fn take_undo_request(&mut self) -> Option<tilo_ui::UndoRequest> { self.undo.take_request() }
/// }
/// ```
///
/// Then `self.undo.can_undo()` decides whether the control paints enabled, and
/// `self.undo.request(UndoRequest::Undo)` on activation asks the host to do it.
#[derive(Clone, Debug, Default)]
pub struct UndoAccess {
    status: UndoStatus,
    pending: Option<UndoRequest>,
}

impl UndoAccess {
    /// A fresh access with an empty history and nothing pending.
    pub fn new() -> Self {
        Self::default()
    }

    /// Host → widget: store the status published this frame.
    pub fn publish(&mut self, status: &UndoStatus) {
        if &self.status != status {
            self.status = status.clone();
        }
    }

    /// The last published status.
    pub fn status(&self) -> &UndoStatus {
        &self.status
    }

    pub fn can_undo(&self) -> bool {
        self.status.can_undo
    }

    pub fn can_redo(&self) -> bool {
        self.status.can_redo
    }

    /// Whether the verb `which` is currently available — what a control's
    /// enabled/disabled state must be derived from.
    pub fn can(&self, which: UndoRequest) -> bool {
        self.status.can(which)
    }

    /// Ask the host to perform `which`. Returns `false` — and enqueues
    /// nothing — when the verb is unavailable, so a control that somehow got
    /// activated while empty cannot make the host attempt an undo that isn't
    /// there.
    pub fn request(&mut self, which: UndoRequest) -> bool {
        if !self.can(which) {
            return false;
        }
        self.pending = Some(which);
        true
    }

    /// Widget → host: take the pending request. Rising-edge — reading clears
    /// it, so one activation produces exactly one undo.
    pub fn take_request(&mut self) -> Option<UndoRequest> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct NativeHistoryProbe {
        pending: Option<tilo_actions::InvocationId>,
        acknowledged: bool,
    }

    impl LocalUndo for NativeHistoryProbe {
        fn local_undo_is_deferred(&self) -> bool {
            true
        }
        fn handle_local_undo_request(&mut self, _: UndoRequest) -> bool {
            false
        }
        fn start_local_undo_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            _: UndoRequest,
        ) -> Result<bool, String> {
            if self.pending.is_some() {
                return Err("original operation pending".into());
            }
            self.pending = Some(invocation);
            Ok(true)
        }
        fn poll_local_undo_operation(
            &mut self,
            invocation: tilo_actions::InvocationId,
            cancellation_requested: bool,
        ) -> Option<tilo_actions::InvocationUpdate> {
            if self.pending != Some(invocation) {
                return None;
            }
            if cancellation_requested {
                self.pending = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: None,
                });
            }
            if !self.acknowledged {
                return None;
            }
            self.pending = None;
            Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            })
        }
    }
    impl crate::widget::Widget for NativeHistoryProbe {
        fn name(&self) -> &str {
            "NativeHistoryProbe"
        }
        crate::provides!(dyn LocalUndo);
        crate::testing::inert_widget_methods!(leaf_layout, apply_layout, draw, get_rect);
    }

    #[test]
    fn deferred_native_history_waits_for_original_ack_and_preserves_document() {
        use crate::widget::{Edits, ResourceOperation, ResourceOperationStart, Widget};
        let mut widget = NativeHistoryProbe::default();
        let mut doc = automerge::AutoCommit::new();
        let original = doc.get_heads();
        let invocation = tilo_actions::InvocationId::new();
        let other = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&UndoRequest::Undo).unwrap();
        let operation = ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            EDIT_OPERATION,
            &payload,
        );
        assert!(
            widget
                .resource_operation_handlers()
                .contains(EDIT_OPERATION)
        );
        assert_eq!(
            widget
                .start_operation(&operation, &mut Edits::lend(&mut doc), &automerge::ROOT)
                .unwrap(),
            Some(ResourceOperationStart::Accepted)
        );
        assert!(
            widget
                .poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none()
        );
        widget.acknowledged = true;
        assert!(
            widget
                .poll_resource_operation(other, false, &doc, &automerge::ROOT)
                .is_none()
        );
        assert!(
            matches!(widget.poll_resource_operation(invocation, false, &doc, &automerge::ROOT), Some(tilo_actions::InvocationUpdate::Succeeded { invocation: completed, .. }) if completed == invocation)
        );
        assert!(
            widget
                .poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none()
        );
        assert_eq!(doc.get_heads(), original);
    }

    #[test]
    fn deferred_native_history_cancellation_does_not_report_success() {
        use crate::widget::{Edits, ResourceOperation, Widget};
        let mut widget = NativeHistoryProbe::default();
        let mut doc = automerge::AutoCommit::new();
        let invocation = tilo_actions::InvocationId::new();
        let payload = serde_json::to_vec(&UndoRequest::Redo).unwrap();
        let operation = ResourceOperation::local(
            invocation,
            tilo_actions::OperationKind::Effect,
            EDIT_OPERATION,
            &payload,
        );
        widget
            .start_operation(&operation, &mut Edits::lend(&mut doc), &automerge::ROOT)
            .unwrap();
        assert!(
            matches!(widget.poll_resource_operation(invocation, true, &doc, &automerge::ROOT), Some(tilo_actions::InvocationUpdate::Cancelled { invocation: canceled, .. }) if canceled == invocation)
        );
        widget.acknowledged = true;
        assert!(
            widget
                .poll_resource_operation(invocation, false, &doc, &automerge::ROOT)
                .is_none()
        );
    }

    #[test]
    fn undo_access_publishes_status_and_refuses_unavailable_verbs_once() {
        // Case `unused_undo_status_is_not_evaluated`.
        {
            let mut spacer = crate::widget::Spacer::new();
            crate::undo::set_undo_status(&mut spacer, &mut || {
                panic!("default widget must not request a summary")
            });
        }

        // Case `undo_button_requests_and_publishes_status`.
        {
            use crate::widget::Widget;
            let mut button = crate::widget::UndoButton::undo();
            let mut calls = 0;
            crate::undo::set_undo_status(&mut button, &mut || {
                calls += 1;
                status(true, false)
            });
            assert_eq!(calls, 1);
            assert!(button.is_enabled());
            assert_eq!(
                button.accessibility().unwrap().description.as_deref(),
                Some("Undo: +5 -0 chars")
            );
        }

        // Case `a_request_for_an_unavailable_verb_is_refused_and_enqueues_nothing`.
        {
            let mut access = UndoAccess::new();
            // Nothing published yet: an empty history.
            assert!(!access.request(UndoRequest::Undo), "empty history refuses");
            assert_eq!(
                access.take_request(),
                None,
                "a refused request must not reach the host"
            );

            access.publish(&status(true, false));
            assert!(access.request(UndoRequest::Undo));
            assert_eq!(access.take_request(), Some(UndoRequest::Undo));
            // Redo is still unavailable even though undo is.
            assert!(!access.request(UndoRequest::Redo));
            assert_eq!(access.take_request(), None);
        }

        // Case `taking_a_request_clears_it_so_one_activation_is_one_undo`.
        {
            let mut access = UndoAccess::new();
            access.publish(&status(true, true));
            access.request(UndoRequest::Undo);
            assert_eq!(access.take_request(), Some(UndoRequest::Undo));
            assert_eq!(
                access.take_request(),
                None,
                "a drained request must not fire twice"
            );
        }
    }

    fn status(can_undo: bool, can_redo: bool) -> UndoStatus {
        UndoStatus {
            can_undo,
            can_redo,
            undo_summary: can_undo.then(|| "+5 -0 chars".to_string()),
            redo_summary: can_redo.then(|| "+2 -0 chars".to_string()),
        }
    }

    #[test]
    fn undo_shortcuts_and_descriptions_never_promise_a_missing_action() {
        // Case `the_shortcut_table_is_the_one_binding_both_hosts_read`.
        {
            use crate::platform::{Platform, PlatformConventions};
            let linux = PlatformConventions::new(Platform::Linux);
            let shortcut = |c, command, shift| shortcut(linux, c, command, shift);
            assert_eq!(shortcut("z", true, false), Some(UndoRequest::Undo));
            assert_eq!(shortcut("Z", true, false), Some(UndoRequest::Undo));
            // Shift is read from the modifiers, not from the character's case:
            // a shifted "z" reaches winit as either spelling.
            assert_eq!(shortcut("z", true, true), Some(UndoRequest::Redo));
            assert_eq!(shortcut("Z", true, true), Some(UndoRequest::Redo));
            assert_eq!(shortcut("y", true, false), Some(UndoRequest::Redo));
            // Without Ctrl these are ordinary characters and must reach the widget.
            assert_eq!(shortcut("z", false, false), None);
            assert_eq!(shortcut("Z", false, true), None);
            assert_eq!(shortcut("k", true, false), None);
            // ⌘Y is History on macOS, never Redo.
            let mac = PlatformConventions::new(Platform::MacOs);
            assert_eq!(super::shortcut(mac, "y", true, false), None);
        }

        // Case `description_never_promises_an_action_that_is_not_there`.
        {
            let empty = UndoStatus::empty();
            assert_eq!(empty.description(UndoRequest::Undo), "Nothing to undo");
            assert_eq!(empty.description(UndoRequest::Redo), "Nothing to redo");

            let live = status(true, true);
            assert_eq!(live.description(UndoRequest::Undo), "Undo: +5 -0 chars");
            assert_eq!(live.description(UndoRequest::Redo), "Redo: +2 -0 chars");
        }

        // Case `an_indescribable_change_falls_back_to_the_bare_verb`:
        // A change the producer could not describe must not be read out as if it
        // were a summary — the control falls back to the bare verb.
        {
            let s = UndoStatus {
                can_undo: true,
                undo_summary: Some(crate::history_source::NO_VISIBLE_CHANGE.to_string()),
                ..Default::default()
            };
            assert_eq!(s.description(UndoRequest::Undo), "Undo");
        }
    }
}

/// Label derivation for a displayed owner must not finalize its live edit.
/// Summary decoding may advance `AutoCommit`'s read cursor, so use a snapshot.
pub(crate) fn status_snapshot(
    doc: &automerge::AutoCommit,
    history: &crate::state::UndoLog,
    author: Option<&crate::permissions::UserId>,
) -> UndoStatus {
    let undo = history.next_undo_as(author);
    let redo = history.next_redo_as(author);
    let mut snapshot = doc.clone();
    // An entry an undo or redo already moved names the step it carries, not
    // the undo change that moved it last.
    let mut summary = |entry: &crate::state::UndoEntry, from: &[_], to: &[_]| {
        entry
            .step
            .as_ref()
            .and_then(tilo_actions::ActionContext::summary)
            .unwrap_or_else(|| crate::history_source::change_summary(&mut snapshot, from, to))
    };
    UndoStatus {
        can_undo: undo.is_some(),
        can_redo: redo.is_some(),
        undo_summary: undo.map(|entry| summary(entry, &entry.before, &entry.after)),
        redo_summary: redo.map(|entry| summary(entry, &entry.after, &entry.before)),
    }
}
