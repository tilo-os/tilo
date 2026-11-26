//! The undo a widget sees and asks for: the capabilities a widget implements to report its
//! own history, the status it shows and the request it makes, and the access field that
//! holds them. The host drives undo itself ([`crate::undo`]).
pub(crate) const EDIT_OPERATION: &str = "tilo.host.edit";

pub use tilo_webview::{UndoRequest, UndoStatus};

/// Which history verb, if any, a command-modified character key means.
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

/// A widget whose own controls dispatch the actions its resource exports, and so report
/// the undo boundary around them (see [`crate::undo::record_dispatched_action`]).
pub trait CommittedActions {
    /// The document heads captured just before the widget's last dispatched action.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>>;

    /// Whether the undo step the last boundary opened stays open for the actions that
    /// continue it (the rest of a word typed a keystroke at a time). Answered by the
    /// widget itself, never its children.
    fn keeps_undo_step_open(&self) -> bool {
        false
    }
}

/// A control that shows or drives the document's undo history (an Undo button).
pub trait UndoControl {
    /// Host → widget: the live undo state, published once per frame.
    fn set_undo_status(&mut self, _status: &mut dyn FnMut() -> UndoStatus) {}

    /// Widget → host: an undo or redo the user asked for, performed through the same path
    /// as Ctrl+Z.
    fn take_undo_request(&mut self) -> Option<UndoRequest> {
        None
    }
}

/// A focused component's observed history, or an original-field observation that is not
/// available yet. Pending is never replaced by document history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalUndoStatus {
    Ready(UndoStatus),
    Pending,
}
impl LocalUndoStatus {
    pub(crate) fn key(&self) -> (bool, bool, bool) {
        match self {
            Self::Ready(status) => (true, status.can_undo, status.can_redo),
            Self::Pending => (false, false, false),
        }
    }
}

/// A component with its own editing history, offered the host's keyboard undo/redo
/// shortcut before the application document consumes it.
pub trait LocalUndo {
    /// None means this component does not own the focused history. Pending means it owns
    /// it, but cannot currently advertise an available entry.
    fn local_undo_status(&self) -> Option<LocalUndoStatus> {
        None
    }

    /// True when exported Edit operations complete through Widget's existing asynchronous
    /// operation receipts rather than on shortcut dispatch.
    fn local_undo_is_deferred(&self) -> bool {
        false
    }

    /// `true` only while focus is inside the component and it consumed the request.
    fn handle_local_undo_request(&mut self, request: UndoRequest) -> bool;

    /// Admit a deferred system Edit request without exposing the host's raw operation
    /// protocol to apps. True means queued, never already succeeded.
    fn start_local_undo_operation(
        &mut self,
        _invocation: tilo_actions::InvocationId,
        _request: UndoRequest,
    ) -> Result<bool, String> {
        Ok(false)
    }

    /// Complete only this component's previously admitted native Edit request. A lost ACK
    /// must fail explicitly; it must not become document Undo.
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
    let request = operation.request();
    if request.type_id != EDIT_OPERATION {
        return Ok(None);
    }
    let Some(local) = crate::widget::capability::capability_mut::<dyn LocalUndo>(widget) else {
        return Ok(None);
    };
    start_deferred_undo(local, request.invocation, request.payload)
}

#[inline(never)]
fn start_deferred_undo(
    local: &mut dyn LocalUndo,
    invocation: tilo_actions::InvocationId,
    payload: &[u8],
) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
    if !local.local_undo_is_deferred() {
        return Ok(None);
    }
    let direction = serde_json::from_slice(payload).map_err(|_| "Invalid native Edit request")?;
    Ok(local
        .start_local_undo_operation(invocation, direction)?
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

/// The undo half of a widget, as one embeddable field: the last
/// [`UndoStatus`] the host published, plus at most one pending
/// [`UndoRequest`].
///
/// Embed it and wire the two `Widget` methods:
///
/// ```ignore
/// struct MyToolbar { undo: crate::UndoAccess, /* … */ }
///
/// impl Widget for MyToolbar {
///     fn set_undo_status(&mut self, s: &mut dyn FnMut() -> crate::UndoStatus) { self.undo.publish(&s()); }
///     fn take_undo_request(&mut self) -> Option<crate::UndoRequest> { self.undo.take_request() }
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

    /// Ask the host to perform `which`. Returns `false` — and enqueues nothing — when the
    /// verb is unavailable, so a control that somehow got activated while empty cannot
    /// make the host attempt an undo that isn't there.
    pub fn request(&mut self, which: UndoRequest) -> bool {
        if !self.can(which) {
            return false;
        }
        self.pending = Some(which);
        true
    }

    /// Widget → host: take the pending request.
    pub fn take_request(&mut self) -> Option<UndoRequest> {
        self.pending.take()
    }
}
