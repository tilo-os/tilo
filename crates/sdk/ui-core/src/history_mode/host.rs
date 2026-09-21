//! Shared History chrome beside, rather than owning or replacing, the live root.
use super::{
    FrozenPreviewAssets, HistoryController, HistoryPresentation, HistoryRegistration,
    HistoryRegistrationError, HistorySourceRegistration, PreviewInput, PreviewSide, PreviewView,
    canvas::PreviewCanvas,
};
use crate::{
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{ActivityTimeline, Button, Column, Row, TaffyTreeExt, Text, TextInput, Widget},
};
use automerge::{AutoCommit, ObjId, transaction::Transactable};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    rc::Rc,
};
use taffy::{Dimension, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;
#[derive(Clone, Copy)]
enum Command {
    Live,
    Activity,
    Before,
    After,
    PreviousPage,
    NextPage,
    Scope(usize),
    Review(review::ReviewCommand),
    FileParent(collab_types::FileVersionId),
    FileUndo,
    FileRedo,
    FileCheck,
    FileRetry,
    FileRefresh,
}

#[derive(Clone)]
pub(crate) struct PendingControlCallback {
    invoke: Rc<RefCell<Box<dyn FnMut()>>>,
}
impl PendingControlCallback {
    fn new(invoke: impl FnMut() + 'static) -> Self {
        Self {
            invoke: Rc::new(RefCell::new(Box::new(invoke))),
        }
    }
    fn invoke(&self) {
        (self.invoke.borrow_mut())();
    }
    #[cfg(test)]
    pub(crate) fn invoke_for_test(&self) {
        self.invoke();
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent presentation, focus and refresh flags are not mutually exclusive modes"
)]
pub struct HistoryHost {
    enclosing_owner: Option<super::HistoryOwner>,
    embedded: bool,
    keyboard_active: bool,
    registration: HistoryRegistration,
    assets: media::AssetSession,
    active_scope: usize,
    sync_status: Option<String>,
    restored_source: Option<HistorySourceRegistration>,
    review: Option<review::ReviewPanel>,
    review_request: Option<review::ReviewCommand>,
    review_node: Option<NodeId>,
    clock: fn() -> Option<u64>,
    controller: Option<HistoryController>,
    files: Option<super::files_session::FileController>,
    retired_files: Vec<super::files_session::FileController>,
    requested_file_version: Option<collab_types::FileVersionId>,
    observed_file_binding: Option<uuid::Uuid>,
    file_target_resolved: bool,
    toolbar: Column,
    timeline: ActivityTimeline,
    unavailable: Text,
    unavailable_node: Option<NodeId>,
    open_requested: Rc<Cell<bool>>,
    open: bool,
    commands: Rc<RefCell<VecDeque<Command>>>,
    selected: Vec<crate::HistoryEntryId>,
    before: PreviewCanvas,
    after: PreviewCanvas,
    side: PreviewSide,
    activity_page: bool,
    width: f64,
    cursor: Point,
    saved_focus: Option<crate::widget::layout::CompositionFocus>,
    returning_live: bool,
    pending_controls: super::pending_controls::SuspendedLocalControls,
    pending_control_capture_failures: Vec<String>,
    pending_control_recovery: Vec<super::pending_controls::SealedComponentControl>,
    suspended_control_queues: Vec<crate::bound_dispatch::CallbackBinding>,
    #[cfg(test)]
    pending_control_callbacks: HashMap<String, PendingControlCallback>,
    toolbar_dirty: bool,
    live_root_lifetime: uuid::Uuid,
    control_domain: Option<crate::ControlDomain>,
    control_lease: Option<crate::control_custody::SuspensionLease>,
    invalid_owner: bool,
    rect: Rect,
    chrome_node: Option<NodeId>,
    live_node: Option<NodeId>,
    toolbar_node: Option<NodeId>,
    body_node: Option<NodeId>,
    timeline_node: Option<NodeId>,
    canvas_node: Option<NodeId>,
}
impl HistoryHost {
    /// Request the same read-only presentation as the History button. Restore
    /// remains owned by the existing review/authority path.
    pub(crate) fn request_open(&self) -> bool {
        if self.invalid_owner {
            return false;
        }
        if !self.open {
            self.open_requested.set(true);
        }
        true
    }

    pub(crate) fn new(
        registration: HistoryRegistration,
        doc: &AutoCommit,
    ) -> Result<Self, HistoryRegistrationError> {
        registration.validate()?;
        let scope = registration
            .definition
            .scopes
            .first()
            .ok_or(HistoryRegistrationError::InvalidScopes)?;
        let source = registration
            .sources
            .iter()
            .find(|source| source.scope() == scope.key)
            .ok_or(HistoryRegistrationError::InvalidScopes)?;

        let controller = match source {
            HistorySourceRegistration::HostDocument { resource, .. }
            | HistorySourceRegistration::OwnedDocument { resource, .. } => Some(
                HistoryController::new(resource.clone(), &source.document(doc)?)
                    .map_err(|_| HistoryRegistrationError::ForeignSource)?,
            ),
            HistorySourceRegistration::Unversioned { .. }
            | HistorySourceRegistration::CheckedFiles { .. } => None,
        };
        let unavailable = Text::new(match registration.sources.first() {
            Some(HistorySourceRegistration::Unversioned { reason, .. }) => reason,
            _ => "Historical content unavailable",
        });
        let request = Rc::new(Cell::new(false));
        let clicked = request.clone();
        let toolbar = Column::new()
            .with_no_padding()
            .with_gap(4.0)
            .with_intrinsic_height()
            .push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("history-open", "History", None)
                            .fit_content(32.0)
                            .on_click(move || clicked.set(true)),
                    ))
                    .push(Box::new(Text::new(scope.title))),
            ));
        let mut host = Self {
            enclosing_owner: None,
            embedded: false,
            keyboard_active: true,
            registration,
            assets: media::AssetSession::new(),
            active_scope: 0,
            sync_status: None,
            restored_source: None,
            review: None,
            review_request: None,
            review_node: None,
            clock: review::unix_now,
            controller,
            files: None,
            retired_files: vec![],
            requested_file_version: None,
            observed_file_binding: None,
            file_target_resolved: false,
            toolbar,
            unavailable,
            unavailable_node: None,
            timeline: ActivityTimeline::new().with_preview_selection(),
            open_requested: request,
            open: false,
            rect: Rect::ZERO,
            chrome_node: None,
            live_node: None,
            toolbar_node: None,
            body_node: None,
            timeline_node: None,
            canvas_node: None,
            commands: Rc::new(RefCell::new(VecDeque::new())),
            selected: vec![],
            before: PreviewCanvas::new(),
            after: PreviewCanvas::new(),
            side: PreviewSide::After,
            activity_page: false,
            width: 900.0,
            cursor: Point::ORIGIN,
            saved_focus: None,
            returning_live: false,
            pending_controls: super::pending_controls::SuspendedLocalControls::default(),
            pending_control_capture_failures: Vec::new(),
            pending_control_recovery: Vec::new(),
            suspended_control_queues: Vec::new(),
            #[cfg(test)]
            pending_control_callbacks: HashMap::new(),
            toolbar_dirty: false,
            live_root_lifetime: uuid::Uuid::new_v4(),
            control_domain: None,
            control_lease: None,
            invalid_owner: false,
        };
        host.rebuild_toolbar();
        Ok(host)
    }
    pub(crate) fn set_control_domain(&mut self, domain: &crate::ControlDomain) {
        self.control_domain = Some(domain.clone());
    }
    pub(crate) fn sync_status_matches(&self, status: Option<&str>) -> bool {
        self.sync_status.as_deref() == status
    }
    pub(crate) fn set_sync_status(&mut self, status: Option<String>) {
        self.sync_status = status;
        self.rebuild_toolbar();
    }
    pub(crate) fn validate_root(&self, root: &dyn Widget) -> Result<(), HistoryRegistrationError> {
        self.registration
            .validate_root_with_owner(root, self.enclosing_owner.as_ref())
    }
    pub(crate) fn set_enclosing_owner(&mut self, owner: Option<super::HistoryOwner>) {
        self.embedded = true;
        let replaced = self.enclosing_owner.as_ref().is_some_and(|existing| {
            owner
                .as_ref()
                .is_none_or(|current| !existing.same_owner(current))
        });
        if replaced {
            self.invalidate_owner();
        }
        self.enclosing_owner = owner;
    }
    pub(crate) fn keyboard_is_active(&self) -> bool {
        self.keyboard_active
    }
    pub(crate) fn cancel_focus_return(&mut self) {
        self.saved_focus = None;
        self.returning_live = false;
    }
    pub(crate) fn activate_at_pointer(&mut self, event: &WindowEvent, scale: f64) {
        let point = match event {
            WindowEvent::Touch(touch) => {
                Point::new(touch.location.x / scale, touch.location.y / scale)
            }
            _ => self.cursor,
        };
        self.keyboard_active = self.rect.contains(point);
    }
    pub(crate) fn set_keyboard_active(&mut self, active: bool) {
        self.keyboard_active = active;
    }
    pub(crate) fn composition_contains(&self, target: NodeId, tree: &TaffyTree<()>) -> bool {
        let mut pending: Vec<_> = self.chrome_node.into_iter().collect();
        while let Some(node) = pending.pop() {
            if node == target {
                return true;
            }
            pending.extend(tree.children(node).unwrap_or_default());
        }
        false
    }
    /// Forget handles owned by a discarded Taffy composition. Logical History
    /// custody, selection, and saved semantic focus remain retained.
    pub(crate) fn discard_layout_nodes(&mut self) {
        self.rect = Rect::ZERO;
        self.cursor = Point::ORIGIN;
        self.keyboard_active = false;
        self.chrome_node = None;
        self.live_node = None;
        self.toolbar_node = None;
        self.body_node = None;
        self.timeline_node = None;
        self.canvas_node = None;
        self.unavailable_node = None;
        self.review_node = None;
    }
    pub(crate) fn live_interactive(&self) -> bool {
        !self.captures_input() && !self.invalid_owner
    }
    pub(crate) fn width_needs_refresh(&self, width: f64) -> bool {
        (self.width < 600.0) != (width < 600.0)
    }

    #[allow(
        clippy::indexing_slicing,
        clippy::expect_used,
        reason = "validated nonempty scope/source bijection; scope commands check bounds before assignment"
    )]
    fn source(&self) -> &HistorySourceRegistration {
        let key = self.registration.definition.scopes[self.active_scope].key;
        self.registration
            .sources
            .iter()
            .find(|source| source.scope() == key)
            .expect("validated scope")
    }
    pub(crate) fn validate(&self) -> Result<(), HistoryRegistrationError> {
        if self.invalid_owner {
            return Err(HistoryRegistrationError::UnsupportedOwner);
        }
        self.registration.validate()
    }
    pub(crate) fn invalidate_owner(&mut self) -> bool {
        if self.invalid_owner {
            return false;
        }
        self.invalid_owner = true;
        self.pending_controls
            .invalidate_owner("History owner is no longer available");
        self.assets.cancel();
        self.review = None;
        self.review_request = None;
        self.controller = None;
        if let Some(files) = &mut self.files {
            files.suspend();
        }
        self.before.preview = None;
        self.after.preview = None;
        self.selected.clear();
        self.commands.borrow_mut().clear();
        self.open_requested.set(false);
        self.open = true;
        self.returning_live = false;
        self.unavailable = Text::new(&HistoryRegistrationError::UnsupportedOwner.to_string());
        self.toolbar_dirty = true;
        true
    }
    pub(crate) fn preserve_captured_focus(
        &mut self,
        captured: Option<crate::widget::layout::CompositionFocus>,
    ) {
        if self.saved_focus.is_none() {
            self.saved_focus = captured;
        }
    }
    pub(crate) fn take_returning_focus(
        &mut self,
    ) -> Option<crate::widget::layout::CompositionFocus> {
        self.returning_live = false;
        self.saved_focus.take()
    }
    pub(crate) fn discard_saved_focus(&mut self) {
        self.saved_focus = None;
    }
    pub(crate) fn captures_input(&self) -> bool {
        self.open || self.open_requested.get() || !self.selected.is_empty()
    }
    #[allow(
        clippy::single_match_else,
        reason = "owner-bearing capture keeps the component and legacy custody variants adjacent"
    )]
    pub(crate) fn capture_pending_controls(
        &mut self,
        live: &mut dyn Widget,
        doc: &AutoCommit,
        root_lifetime: uuid::Uuid,
    ) -> bool {
        let first_entry = self.pending_controls.begin_history_entry();
        if !first_entry {
            if self.pending_control_recovery.is_empty() {
                return false;
            }
            let reachable = crate::bound_dispatch::inspect_control_owners(live);
            let mut waiting = Vec::new();
            for sealed in self.pending_control_recovery.drain(..) {
                if reachable
                    .visited
                    .iter()
                    .any(|binding| binding.same_component(&sealed.target.binding))
                {
                    self.pending_controls.capture(
                        super::pending_controls::PendingControlOrigin::Component(sealed.target),
                        sealed.invocation,
                        sealed.witness,
                    );
                } else {
                    waiting.push(sealed);
                }
            }
            self.pending_control_recovery = waiting;
            // The failed entry traversal could not observe direct-take output.
            // Its uncertainty remains visible even after sealed queues recover.
            self.rebuild_toolbar();
            return true;
        }
        if let Some(domain) = &self.control_domain {
            let (lease, controls) = domain.suspend();
            self.control_lease = Some(lease);
            for control in controls {
                // Embedded capture has already borrowed the enclosing owner's exact
                // content. Preserve its dispatch origin; resolve only the witness
                // against the checked owner document, never by address selection.
                let witness = match (&control.origin, &self.enclosing_owner) {
                    (
                        super::pending_controls::PendingControlOrigin::Legacy(target),
                        Some(owner),
                    ) if owner.same_owner(&target.owner) => owner
                        .document(doc)
                        .map_err(|error| error.to_string())
                        .and_then(|owned| {
                            crate::bound_dispatch::control_witness(
                                live,
                                &owned,
                                &crate::bound_dispatch::legacy_document::LocalResourceTarget::Outer,
                                &control.invocation,
                            )
                            .map_err(|error| error.to_string())
                        }),
                    _ => crate::bound_dispatch::control_witness(
                        live,
                        doc,
                        &control.origin.target(),
                        &control.invocation,
                    )
                    .map_err(|error| error.to_string()),
                };
                self.pending_controls.capture_registered(control, witness);
            }
        }
        self.pending_control_capture_failures.clear();
        let queues = crate::bound_dispatch::snapshot_control_queues(live);
        self.suspended_control_queues = queues.owners;
        let sealed = queues
            .controls
            .into_iter()
            .map(|(target, invocation)| {
                let witness = crate::bound_dispatch::control_witness(
                    live,
                    doc,
                    &crate::bound_dispatch::legacy_document::LocalResourceTarget::Component(
                        target.clone(),
                    ),
                    &invocation,
                )
                .map_err(|error| error.to_string());
                super::pending_controls::SealedComponentControl {
                    target,
                    invocation,
                    witness,
                }
            })
            .collect::<Vec<_>>();
        let bound = crate::bound_dispatch::capture_control_invocations(live);
        self.pending_control_recovery = sealed
            .into_iter()
            .filter(|sealed| {
                !bound
                    .visited
                    .iter()
                    .any(|binding| binding.same_component(&sealed.target.binding))
            })
            .collect();
        for (target, invocation) in bound.controls {
            let origin = super::pending_controls::PendingControlOrigin::Component(target);
            let witness =
                crate::bound_dispatch::control_witness(live, doc, &origin.target(), &invocation)
                    .map_err(|error| error.to_string());
            self.pending_controls.capture(origin, invocation, witness);
        }
        self.pending_control_capture_failures.extend(
            bound
                .failures
                .into_iter()
                .map(|error| format!("component capture refused: {error:?}; unobserved entry direct-take output cannot be recovered, later output is discarded")),
        );
        let legacy = crate::bound_dispatch::legacy_document::drain_legacy_controls(live);
        for (target, invocation) in legacy.controls {
            let origin = super::pending_controls::PendingControlOrigin::Legacy(target);
            let witness =
                crate::bound_dispatch::control_witness(live, doc, &origin.target(), &invocation)
                    .map_err(|error| error.to_string());
            self.pending_controls.capture(origin, invocation, witness);
        }
        self.pending_control_capture_failures.extend(
            legacy
                .failures
                .into_iter()
                .map(|error| format!("document capture refused: {error}")),
        );
        while let Some(invocation) = live.take_control_invocation() {
            let (origin, witness) = match self.enclosing_owner.clone() {
                Some(owner) => match &owner.0 {
                    super::owner::OwnerKind::Component(binding) => {
                        let origin = super::pending_controls::PendingControlOrigin::Component(
                            crate::bound_dispatch::BoundActionTarget {
                                binding: binding.clone(),
                                address: crate::bound_dispatch::BoundActionAddress {
                                    instance_id: binding.instance_id,
                                    address: invocation.address.clone(),
                                },
                            },
                        );
                        let witness = crate::bound_dispatch::control_witness(
                            live,
                            doc,
                            &origin.target(),
                            &invocation,
                        )
                        .map_err(|error| error.to_string());
                        (origin, witness)
                    }
                    super::owner::OwnerKind::Legacy { .. } => {
                        let origin = super::pending_controls::PendingControlOrigin::Legacy(
                            crate::bound_dispatch::legacy_document::LegacyResourceTarget {
                                owner: owner.clone(),
                                address: invocation.address.clone(),
                            },
                        );
                        let witness = owner
                            .document(doc)
                            .map_err(|error| error.to_string())
                            .and_then(|owned| {
                                crate::bound_dispatch::control_witness(
                                    live,
                                    &owned,
                                    &crate::bound_dispatch::legacy_document::LocalResourceTarget::Outer,
                                    &invocation,
                                )
                                .map_err(|error| error.to_string())
                            });
                        (origin, witness)
                    }
                },
                None => {
                    let origin =
                        super::pending_controls::PendingControlOrigin::Outer { root_lifetime };
                    let witness = crate::bound_dispatch::control_witness(
                        live,
                        doc,
                        &origin.target(),
                        &invocation,
                    )
                    .map_err(|error| error.to_string());
                    (origin, witness)
                }
            };
            self.pending_controls.capture(origin, invocation, witness);
        }
        self.rebuild_toolbar();
        true
    }
    pub(crate) fn capture_embedded_pending_controls(
        &mut self,
        live: &mut dyn Widget,
        doc: &AutoCommit,
    ) -> bool {
        let lifetime = self
            .enclosing_owner
            .as_ref()
            .and_then(super::HistoryOwner::lifetime_id)
            .unwrap_or(self.live_root_lifetime);
        self.capture_pending_controls(live, doc, lifetime)
    }
    #[allow(
        clippy::needless_pass_by_ref_mut,
        reason = "the surface-owned lifecycle door names which suspended queue is being drained"
    )]
    pub(crate) fn discard_pending_output(&mut self, live: &mut dyn Widget) {
        let _ = crate::bound_dispatch::drain_control_invocations(live);
        crate::bound_dispatch::legacy_document::discard_legacy_controls(live);
        while live.take_control_invocation().is_some() {}
        // Entry observed these exact queues even if mutable traversal failed.
        // Clear after every take: a component's fallback take may transfer a
        // request into its binding queue while returning None. Neither the
        // sealed original's live copy nor suspended output may survive return.
        for owner in &self.suspended_control_queues {
            owner.queued_controls.borrow_mut().clear();
        }
    }
    pub(crate) fn finish_return_to_live(&mut self) {
        // If custody never recovered, preserve the sealed descriptor as a
        // visible refusal instead of losing it on the next History entry.
        for sealed in self.pending_control_recovery.drain(..) {
            self.pending_controls.capture(
                super::pending_controls::PendingControlOrigin::Component(sealed.target),
                sealed.invocation,
                Err("The original component custody could not be recovered during History".into()),
            );
        }
        // Chrome is prepared while raw maps are cleared. Registered custody
        // remains suspended until layout and focus restoration have completed.
        if self.pending_controls.return_to_live() {
            self.toolbar_dirty = true;
        }
        // The caller completed the final suspended-output discard. Later live
        // emissions into these queues retain their ordinary dispatch semantics.
        self.suspended_control_queues.clear();
    }
    pub(crate) fn has_settled_control_return(&self) -> bool {
        self.control_lease.is_some() && !self.captures_input() && !self.returning_live
    }
    pub(crate) fn finish_control_return(&mut self) {
        self.control_lease = None;
    }
    /// Runner-only structural flush after every admitted raw widget map has
    /// been cleared for this composition pass.
    pub(crate) fn flush_pending_toolbar(&mut self) {
        if std::mem::take(&mut self.toolbar_dirty) {
            self.rebuild_toolbar();
        }
    }
    pub(crate) fn pending_control_reviews(
        &self,
    ) -> Vec<super::pending_controls::PendingControlReview> {
        self.pending_controls.review()
    }
    pub(crate) fn pending_control_surface_lifetime(&self) -> uuid::Uuid {
        self.pending_controls.surface_lifetime()
    }
    pub(crate) fn pending_control_token(
        &mut self,
        id: super::pending_controls::PendingControlId,
    ) -> Option<super::pending_controls::ContinuationToken> {
        self.pending_controls.token(id)
    }
    pub(crate) fn prepare_pending_control(
        &mut self,
        token: &super::pending_controls::ContinuationToken,
        confirmed: bool,
    ) -> Option<super::pending_controls::PreparedContinuation> {
        self.pending_controls.prepare(token, confirmed)
    }
    pub(crate) fn prepare_pending_control_typed(
        &mut self,
        token: &super::pending_controls::ContinuationToken,
        value: &str,
    ) -> Option<super::pending_controls::PreparedContinuation> {
        self.pending_controls
            .prepare_typed_confirmation(token, value)
    }
    pub(crate) fn pending_control_needs_confirmation(
        &mut self,
        id: super::pending_controls::PendingControlId,
    ) {
        self.pending_controls.needs_confirmation(id);
        self.toolbar_dirty = true;
    }
    pub(crate) fn pending_control_refused(
        &mut self,
        id: super::pending_controls::PendingControlId,
        reason: impl Into<String>,
    ) {
        self.pending_controls.refused(id, reason);
        self.toolbar_dirty = true;
    }
    pub(crate) fn pending_control_consumed(
        &mut self,
        id: super::pending_controls::PendingControlId,
    ) {
        self.pending_controls.consumed(id);
        self.toolbar_dirty = true;
    }
    pub(crate) fn cancel_pending_control(
        &mut self,
        token: &super::pending_controls::ContinuationToken,
    ) -> bool {
        let cancelled = self.pending_controls.cancel(token);
        if cancelled {
            self.toolbar_dirty = true;
        }
        cancelled
    }
    pub(crate) fn drain_pending_control_commands(
        &self,
    ) -> Vec<super::pending_controls::ContinuationCommand> {
        self.pending_controls.drain_commands()
    }
    #[cfg(test)]
    pub(crate) fn queue_pending_control_command_for_test(
        &self,
        command: super::pending_controls::ContinuationCommand,
    ) {
        self.pending_controls
            .commands()
            .borrow_mut()
            .push_back(command);
    }
    #[cfg(test)]
    pub(crate) fn pending_control_callback_for_test(
        &self,
        id: &str,
    ) -> Option<PendingControlCallback> {
        self.pending_control_callbacks.get(id).cloned()
    }
    #[cfg(test)]
    pub(crate) fn activate_retained_control_for_test(&mut self, id: &str) -> bool {
        fn activate(widget: &mut dyn Widget, id: &str) -> bool {
            if widget.test_id() == Some(id) {
                return widget.perform_action(
                    &crate::a11y::Action::Activate,
                    &mut AutoCommit::new(),
                    &automerge::ROOT,
                );
            }
            widget
                .children_mut()
                .into_iter()
                .any(|child| activate(child, id))
        }
        // Invokes the retained production button callback. No phase, lease,
        // descriptor, or producer behavior is replaced by this access seam.
        activate(&mut self.toolbar, id)
    }
    pub(crate) fn has_saved_focus(&self) -> bool {
        self.saved_focus.is_some()
    }
    pub(crate) fn controls_event(&self, event: &WindowEvent, scale: f64) -> bool {
        let point = if let WindowEvent::CursorMoved { position, .. } = event {
            Point::new(position.x / scale, position.y / scale)
        } else if let WindowEvent::Touch(touch) = event {
            Point::new(touch.location.x / scale, touch.location.y / scale)
        } else {
            self.cursor
        };
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
                | WindowEvent::Touch(_)
        );
        if pointer && !self.rect.contains(point) {
            return false;
        }
        if self.embedded
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_)
            )
            && !self.keyboard_active
        {
            return false;
        }
        if self.captures_input() {
            return true;
        }
        match event {
            WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::MouseWheel { .. }
            | WindowEvent::Touch(_) => {
                self.toolbar.get_rect().contains(point)
                    || (self.show_timeline() && self.timeline.get_rect().contains(point))
            }
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                fn focused(widget: &dyn Widget) -> bool {
                    widget.is_focused() || widget.children().into_iter().any(focused)
                }
                self.captures_input() || focused(&self.toolbar)
            }
            _ => false,
        }
    }
    pub(crate) unsafe fn preserve_focus(
        &mut self,
        focus: Option<NodeId>,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &crate::bound_dispatch::BoundNodeMap,
        tree: &TaffyTree<()>,
    ) {
        if self.saved_focus.is_none() && focus.is_some_and(|node| !self.owns_node(node, tree)) {
            self.saved_focus =
                unsafe { crate::widget::layout::CompositionFocus::capture(focus, nodes, bindings) };
        }
    }
    pub(crate) unsafe fn restore_focus(
        &mut self,
        nodes: &HashMap<NodeId, *mut dyn Widget>,
        bindings: &crate::bound_dispatch::BoundNodeMap,
    ) -> Option<NodeId> {
        if !self.returning_live {
            return None;
        }
        self.returning_live = false;
        fn clear_focus(widget: &mut dyn Widget) {
            widget.set_focus(false);
            for child in widget.children_mut() {
                clear_focus(child);
            }
        }
        // History chrome is retained across the return frame. Clear its local
        // focus flags before restoring the suspended live owner, otherwise the
        // closed toolbar continues to capture keyboard input from that owner.
        clear_focus(&mut self.toolbar);
        clear_focus(&mut self.timeline);
        clear_focus(&mut self.unavailable);
        clear_focus(&mut self.before);
        clear_focus(&mut self.after);
        if let Some(panel) = &mut self.review {
            clear_focus(&mut panel.dialog);
        }
        let restored = self
            .saved_focus
            .take()
            .and_then(|focus| unsafe { focus.restore(nodes, bindings, None) });
        // Focus restoration is part of exit work. The runner completes
        // custody only after all remaining frame callbacks have finished.
        restored
    }
    pub(crate) fn returning_live(&self) -> bool {
        self.returning_live
    }
    pub(crate) fn owns_node(&self, node: NodeId, tree: &TaffyTree<()>) -> bool {
        let mut pending: Vec<_> = [
            self.toolbar_node,
            self.timeline_node,
            self.canvas_node,
            self.unavailable_node,
            self.review_node,
        ]
        .into_iter()
        .flatten()
        .collect();
        while let Some(candidate) = pending.pop() {
            if candidate == node {
                return true;
            }
            pending.extend(tree.children(candidate).unwrap_or_default());
        }
        false
    }
    pub(crate) fn set_width(&mut self, width: f64) {
        let changed = (self.width < 600.0) != (width < 600.0);
        self.width = width;
        if changed {
            self.rebuild_toolbar();
        }
    }
    fn show_timeline(&self) -> bool {
        self.open && (self.width >= 600.0 || self.selected.is_empty() || self.activity_page)
    }
    fn timeline_available(&self) -> bool {
        self.controller.is_some() || self.files.is_some()
    }
    fn show_preview(&self) -> bool {
        !self.selected.is_empty() && !self.activity_page
    }
    fn canvas(&self) -> &PreviewCanvas {
        if self.side == PreviewSide::Before {
            &self.before
        } else {
            &self.after
        }
    }
    fn canvas_mut(&mut self) -> &mut PreviewCanvas {
        if self.side == PreviewSide::Before {
            &mut self.before
        } else {
            &mut self.after
        }
    }
    pub(crate) fn needs_refresh(&self, doc: &AutoCommit, width: f64) -> bool {
        if self.invalid_owner {
            return false;
        }
        if self.assets.has_results() {
            return true;
        }
        if let HistorySourceRegistration::CheckedFiles { source, .. } = self.source() {
            if source.target().map(|target| target.binding) != self.observed_file_binding
                || source.wants_open()
                || self
                    .files
                    .as_ref()
                    .is_some_and(super::files_session::FileController::has_results)
                || self
                    .retired_files
                    .iter()
                    .any(super::files_session::FileController::has_results)
            {
                return true;
            }
        }
        if !self.pending_control_recovery.is_empty()
            || self.toolbar_dirty
            || !self.pending_controls.commands().borrow().is_empty()
            || self.open_requested.get()
            || !self.commands.borrow().is_empty()
            || (self.width < 600.0) != (width < 600.0)
        {
            return true;
        }
        let selected: Vec<_> = self
            .timeline
            .selection()
            .and_then(|(start, end)| self.timeline.entries().get(start..=end))
            .map(|entries| entries.iter().map(|entry| entry.id).collect())
            .unwrap_or_default();
        if !selected.is_empty() && selected != self.selected {
            return true;
        }
        if matches!(
            self.source(),
            HistorySourceRegistration::CheckedFiles { .. }
        ) {
            return false;
        }
        let Ok(document) = self.source().document(doc) else {
            return true;
        };
        self.controller.as_ref().is_some_and(|controller| {
            super::sorted_heads(document.base_heads()) != controller.observed_live_heads
        })
    }
    pub(crate) fn review_is_open(&self) -> bool {
        self.review.is_some()
    }
    pub(crate) fn review_layout_node(&self) -> Option<NodeId> {
        self.review_node
    }
    pub(crate) fn observe_pointer(&mut self, event: &WindowEvent, scale: f64) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / scale, position.y / scale);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered refresh reconciles source, controller, immutable canvases and toolbar"
    )]
    pub(crate) fn refresh(&mut self, doc: &AutoCommit) {
        if self.invalid_owner {
            return;
        }
        let had_live_updates = self
            .controller
            .as_ref()
            .is_some_and(HistoryController::has_live_updates);
        let target_changed = self.reconcile_file_target();
        let mut changed = target_changed || std::mem::take(&mut self.toolbar_dirty);
        if self.open_requested.replace(false) {
            self.open = !self.open;
            self.returning_live = !self.open;
            if !self.open {
                self.close_files_presentation();
            }
            changed = true;
        }
        let commands: Vec<_> = self.commands.borrow_mut().drain(..).collect();
        for command in commands {
            // These callbacks were produced by the superseded presentation.
            if target_changed
                && matches!(
                    command,
                    Command::Review(_)
                        | Command::FileParent(_)
                        | Command::FileUndo
                        | Command::FileRedo
                        | Command::FileCheck
                        | Command::FileRetry
                        | Command::FileRefresh
                )
            {
                continue;
            }
            changed = true;
            match command {
                Command::Review(command) => {
                    if matches!(command, review::ReviewCommand::Cancel) {
                        if let Some(files) = &mut self.files {
                            files.cancel_review();
                            self.review = None;
                        }
                    }
                    self.review_request = Some(command);
                }
                Command::FileParent(parent) => {
                    if let Some(files) = &mut self.files {
                        if let Some(version) = files.selected() {
                            files.select(version, Some(parent));
                        }
                    }
                }
                Command::FileUndo => self.review_file_compensation(false),
                Command::FileRedo => self.review_file_compensation(true),
                Command::FileCheck => {
                    if let Some(files) = &mut self.files {
                        files.check_result();
                    }
                }
                Command::FileRetry => {
                    if let Some(files) = &mut self.files {
                        files.retry();
                    }
                }
                Command::FileRefresh => {
                    if let Some(files) = &mut self.files {
                        files.load();
                    }
                }
                Command::Live => {
                    self.close_files_presentation();
                    self.review = None;
                    self.review_request = None;
                    self.assets.cancel();
                    self.selected.clear();
                    self.timeline.set_selection(None);
                    self.before.preview = None;
                    self.after.preview = None;
                    self.open = false;
                    self.activity_page = false;
                    self.returning_live = true;
                    if let Some(controller) = &mut self.controller {
                        controller.back_to_live();
                    }
                }
                Command::PreviousPage | Command::NextPage => {
                    self.canvas_mut()
                        .navigate(if matches!(command, Command::NextPage) {
                            super::PreviewNavigation::NextPage
                        } else {
                            super::PreviewNavigation::PreviousPage
                        });
                }
                Command::Activity => self.activity_page = true,
                Command::Before => {
                    self.side = PreviewSide::Before;
                    self.activity_page = false;
                }
                Command::After => {
                    self.side = PreviewSide::After;
                    self.activity_page = false;
                }
                Command::Scope(index) => {
                    if index != self.active_scope
                        && index < self.registration.definition.scopes.len()
                    {
                        self.review = None;
                        self.review_request = None;
                        self.active_scope = index;
                        self.assets.cancel();
                        self.selected.clear();
                        self.timeline.set_selection(None);
                        self.before.preview = None;
                        self.after.preview = None;
                        self.activity_page = false;
                        self.controller = None;
                        match self.source() {
                            HistorySourceRegistration::HostDocument { resource, .. }
                            | HistorySourceRegistration::OwnedDocument { resource, .. } => {
                                if let Ok(document) = self.source().document(doc) {
                                    self.controller =
                                        HistoryController::new(resource.clone(), &document).ok();
                                }
                            }
                            HistorySourceRegistration::Unversioned { reason, .. } => {
                                self.unavailable = Text::new(reason)
                            }
                            HistorySourceRegistration::CheckedFiles { .. } => {
                                self.close_files_presentation()
                            }
                        }
                    }
                }
            }
        }
        if matches!(
            self.source(),
            HistorySourceRegistration::CheckedFiles { .. }
        ) {
            self.refresh_files(changed);
            return;
        }
        // Resolve actual current selected IDs before replacing the projection;
        // positions are never retained as version authority.
        let selected: Vec<_> = self
            .timeline
            .selection()
            .and_then(|(start, end)| self.timeline.entries().get(start..=end))
            .map(|entries| entries.iter().map(|entry| entry.id).collect())
            .unwrap_or_default();
        let Ok(document) = self.source().document(doc) else {
            self.invalidate_owner();
            return;
        };
        if let Some(controller) = &mut self.controller {
            let _ = controller.refresh(&document);
            if selected != self.selected
                && !selected.is_empty()
                && controller.select(&document, &selected).is_ok()
            {
                self.review = None;
                self.review_request = None;
                self.selected = selected;
                self.activity_page = false;
                self.side = PreviewSide::After;
                let _ = controller.select_compare_side(PreviewSide::Before);
                let before = controller.snapshot().cloned();
                let _ = controller.select_compare_side(PreviewSide::After);
                let after = controller.snapshot().cloned();
                self.assets.cancel();
                // Requests are issued below, after releasing the controller loan.
                self.assets.snapshots = [before, after];
                changed = true;
            }
            self.timeline.set_entries(controller.entries().to_vec());
        }
        self.refresh_media();
        let now_has_updates = self
            .controller
            .as_ref()
            .is_some_and(HistoryController::has_live_updates);
        if changed || had_live_updates != now_has_updates {
            self.rebuild_toolbar();
        }
    }
    fn command_button(&self, id: &str, label: &str, command: Command) -> Button {
        let queue = self.commands.clone();
        Button::new(id, label, None)
            .fit_content(32.0)
            .on_click(move || queue.borrow_mut().push_back(command))
    }
    fn preview_page_toolbar(&self, mut toolbar: Column) -> Column {
        if let Some(preview) = &self.canvas().preview {
            let pages = preview.accessibility().pages;
            if pages.len() > 1 {
                let current = preview.view().page;
                let label = pages
                    .iter()
                    .find(|(id, _)| Some(id) == current.as_ref())
                    .map_or("Page", |(_, title)| title.as_str());
                toolbar = toolbar.push(Box::new(Text::new(label))).push(Box::new(
                    Row::new()
                        .push(Box::new(self.command_button(
                            "history-previous-page",
                            "Previous page",
                            Command::PreviousPage,
                        )))
                        .push(Box::new(self.command_button(
                            "history-next-page",
                            "Next page",
                            Command::NextPage,
                        ))),
                ));
            }
        }
        toolbar
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one retained History toolbar keeps navigation, pending continuation and refusal controls together"
    )]
    fn rebuild_toolbar(&mut self) {
        #[cfg(test)]
        self.pending_control_callbacks.clear();
        let title = self
            .registration
            .definition
            .scopes
            .get(self.active_scope)
            .map_or("History", |scope| scope.title);
        let mut toolbar = Column::new()
            .with_no_padding()
            .with_gap(4.0)
            .with_intrinsic_height();
        if self.invalid_owner {
            self.toolbar = toolbar.push(Box::new(Text::new("History unavailable")));
            return;
        }
        if self.selected.is_empty() {
            let clicked = self.open_requested.clone();
            toolbar = toolbar.push(Box::new(
                Row::new()
                    .push(Box::new(
                        Button::new("history-open", "History", None)
                            .fit_content(32.0)
                            .on_click(move || clicked.set(true)),
                    ))
                    .push(Box::new(Text::new(title))),
            ));
            for review in self.pending_controls.review() {
                let mut row = Row::new().push(Box::new(Text::new(&format!(
                    "Pending #{} from {}: {} arguments {}",
                    review.id.value(),
                    review.owner,
                    review.invocation.address.encode(),
                    serde_json::Value::Object(review.invocation.arguments.clone())
                ))));
                let label = match review.status {
                    super::pending_controls::PendingControlStatus::Ready => Some("Continue"),
                    super::pending_controls::PendingControlStatus::NeedsConfirmation => {
                        Some("Confirm")
                    }
                    super::pending_controls::PendingControlStatus::Suspended => None,
                    super::pending_controls::PendingControlStatus::Refused(ref reason) => {
                        row = row.push(Box::new(Text::new(reason)));
                        None
                    }
                    super::pending_controls::PendingControlStatus::Consumed => None,
                };
                if let Some(label) = label
                    && let Some(token) = self.pending_controls.token(review.id)
                {
                    let commands = self.pending_controls.commands();
                    let id = format!("history-pending-{}", review.id.value());
                    if matches!(
                        review.status,
                        super::pending_controls::PendingControlStatus::NeedsConfirmation
                    ) {
                        let expected = match &review.confirmation {
                            Ok(tilo_actions::ConfirmationPolicy::Confirm) => "CONFIRM".to_owned(),
                            Ok(tilo_actions::ConfirmationPolicy::Phrase { phrase }) => {
                                phrase.clone()
                            }
                            Ok(tilo_actions::ConfirmationPolicy::None) => {
                                "confirmation unavailable".to_owned()
                            }
                            Err(reason) => reason.clone(),
                        };
                        let entered = Rc::new(RefCell::new(String::new()));
                        let changed = entered.clone();
                        let confirm_token = token.clone();
                        row = row.push(Box::new(
                            TextInput::local_with_language("und")
                                .with_spellcheck(false)
                                .with_autocorrect(false)
                                .with_id(&format!("{id}-phrase"))
                                .with_placeholder(&format!("Type {expected} to confirm"))
                                .on_change(move |value| {
                                    changed.borrow_mut().replace_range(.., value)
                                }),
                        ));
                        let typed = entered;
                        let callback = PendingControlCallback::new(move || {
                            commands.borrow_mut().push_back(
                                super::pending_controls::ContinuationCommand::Confirm(
                                    confirm_token.clone(),
                                    typed.borrow().clone(),
                                ),
                            )
                        });
                        #[cfg(test)]
                        self.pending_control_callbacks
                            .insert(id.clone(), callback.clone());
                        row = row.push(Box::new(
                            Button::new(&id, label, None)
                                .fit_content(32.0)
                                .on_click(move || callback.invoke()),
                        ));
                    } else {
                        let command =
                            super::pending_controls::ContinuationCommand::Review(token.clone());
                        let callback = PendingControlCallback::new(move || {
                            commands.borrow_mut().push_back(command.clone())
                        });
                        #[cfg(test)]
                        self.pending_control_callbacks
                            .insert(id.clone(), callback.clone());
                        row = row.push(Box::new(
                            Button::new(&id, label, None)
                                .fit_content(32.0)
                                .on_click(move || callback.invoke()),
                        ));
                    }
                    let commands = self.pending_controls.commands();
                    let cancel_id = format!("history-pending-cancel-{}", review.id.value());
                    let callback = PendingControlCallback::new(move || {
                        commands.borrow_mut().push_back(
                            super::pending_controls::ContinuationCommand::Cancel(token.clone()),
                        )
                    });
                    #[cfg(test)]
                    self.pending_control_callbacks
                        .insert(cancel_id.clone(), callback.clone());
                    row = row.push(Box::new(
                        Button::new(&cancel_id, "Cancel", None)
                            .fit_content(32.0)
                            .on_click(move || callback.invoke()),
                    ));
                }
                toolbar = toolbar.push(Box::new(row));
            }
            for failure in &self.pending_control_capture_failures {
                toolbar = toolbar.push(Box::new(Text::new(failure)));
            }
        } else {
            let mut header = Row::new().push(Box::new(Text::new(title)));
            if self.width < 600.0 && !self.activity_page {
                header = header.push(Box::new(self.command_button(
                    "history-activity",
                    "Back to activity",
                    Command::Activity,
                )));
            }
            header = header.push(Box::new(self.command_button(
                "history-back-live",
                "Back to live",
                Command::Live,
            )));
            let mut compare = Row::new();
            for (id, label, command, side) in [
                (
                    "history-before",
                    "Before",
                    Command::Before,
                    PreviewSide::Before,
                ),
                ("history-after", "After", Command::After, PreviewSide::After),
            ] {
                let mut button = self.command_button(id, label, command);
                if self.side == side {
                    button = button.primary();
                }
                compare = compare.push(Box::new(button));
            }
            compare = compare.push(Box::new(Text::new(if self.side == PreviewSide::Before {
                "Historical: Before"
            } else {
                "Historical: After"
            })));
            if self
                .registration
                .definition
                .scopes
                .get(self.active_scope)
                .is_some_and(|scope| {
                    scope.content_schema.is_some()
                        || matches!(scope.presentation, HistoryPresentation::CheckedFiles { .. })
                })
            {
                compare = compare.push(Box::new(self.command_button(
                    "history-restore",
                    "Restore…",
                    Command::Review(review::ReviewCommand::Open),
                )));
            }
            if self.width < 600.0 {
                toolbar = toolbar.push(Box::new(header)).push(Box::new(compare));
            } else {
                toolbar = toolbar.push(Box::new(
                    Row::new().push(Box::new(header)).push(Box::new(compare)),
                ));
            }
            toolbar = self.preview_page_toolbar(toolbar);
        }
        if self
            .controller
            .as_ref()
            .is_some_and(HistoryController::has_live_updates)
        {
            toolbar = toolbar.push(Box::new(
                Text::new("Live content has updates").with_live(crate::a11y::Politeness::Polite),
            ));
        }
        if self.registration.definition.scopes.len() > 1 {
            let mut scopes = Row::new();
            for (index, scope) in self.registration.definition.scopes.iter().enumerate() {
                let mut button = self.command_button(
                    &format!("history-scope-{}", scope.key),
                    scope.title,
                    Command::Scope(index),
                );
                if index == self.active_scope {
                    button = button.primary();
                }
                scopes = scopes.push(Box::new(button));
            }
            toolbar = toolbar.push(Box::new(scopes));
        }
        toolbar = self.files_toolbar(toolbar);
        if let Some(status) = &self.sync_status {
            toolbar = toolbar.push(Box::new(
                Text::new(status).with_live(crate::a11y::Politeness::Polite),
            ));
        }
        self.toolbar = toolbar;
        self.toolbar_dirty = false;
    }
    pub(crate) fn live_visible(&self) -> bool {
        let external = self
            .registration
            .definition
            .scopes
            .get(self.active_scope)
            .is_some_and(|scope| {
                matches!(scope.presentation, HistoryPresentation::External { .. })
            });
        !(self.invalid_owner
            || self.show_preview()
            || self.open && (self.width < 600.0 || external))
    }
}

mod surface;

mod review;

mod media;

mod files;
