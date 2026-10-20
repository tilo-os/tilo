//! Shared History chrome beside, rather than owning or replacing, the live root.
use super::{
    FrozenPreviewAssets, HistoryController, HistoryPresentation, HistoryRegistration,
    HistoryRegistrationError, HistorySourceRegistration, PreviewInput, PreviewSide, PreviewView,
    canvas::PreviewCanvas,
};
use crate::widget::ButtonRole;
use crate::{
    i18n::I18nManager,
    presence::PresenceCtx,
    text_render::TextRenderer,
    theme::Theme,
    widget::{ActivityTimeline, Button, Column, Row, TaffyTreeExt, Text, TextInput, Widget},
};
use automerge::AutoCommit;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
};
use taffy::{Dimension, FlexDirection, NodeId, Size, Style, TaffyTree};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};
use winit::event::WindowEvent;
/// Below this width History takes the whole window, its preview a page of
/// its own; at or above it, History is a panel beside the content.
const SIDE_PANEL_MIN_WIDTH: f64 = 600.0;
/// The side panel's width.
pub(super) const SIDE_PANEL_WIDTH: f64 = 360.0;

#[derive(Clone, Copy)]
enum Command {
    Live,
    /// Leave the selected change and show the live document again, History
    /// still open beside it.
    Now,
    Earlier,
    Activity,
    Before,
    After,
    PreviousPage,
    NextPage,
    Scope(usize),
    /// Open the foot's name field for the selected changes.
    Name,
    Review(review::ReviewCommand),
    FileParent(fs_protocol::FileVersionId),
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

/// The children a [`HistoryHost`] layout presents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Composition {
    toolbar: u64,
    review: bool,
    timeline: Option<bool>,
    preview: Option<super::PreviewSide>,
}

#[cfg(test)]
thread_local! {
    /// Walks of a History's owned layout subtrees on this thread, so tests
    /// can hold a focus-order recomputation to one walk, not one per node.
    pub(crate) static OWNERSHIP_WALKS: Cell<usize> = const { Cell::new(0) };
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
    bar_status: crate::widget::HeaderStatus,
    restored_source: Option<HistorySourceRegistration>,
    review: Option<review::ReviewPanel>,
    review_request: Option<review::ReviewCommand>,
    review_node: Option<NodeId>,
    clock: fn() -> Option<u64>,
    controller: Option<HistoryController>,
    files: Option<super::files_session::FileController>,
    retired_files: Vec<super::files_session::FileController>,
    requested_file_version: Option<fs_protocol::FileVersionId>,
    observed_file_binding: Option<uuid::Uuid>,
    file_target_resolved: bool,
    toolbar: Column,
    /// The side panel's foot while a change is selected: what it was, the
    /// before/after switch, Restore and Restore just this change.
    footer: Column,
    /// Over the preview while a change is selected: it is an earlier
    /// version, read-only, with the way back to now.
    banner: Row,
    timeline: ActivityTimeline,
    unavailable: crate::widget::EmptyState,
    unavailable_node: Option<NodeId>,
    open_requested: Rc<Cell<bool>>,
    /// The change to select once History next refreshes open.
    requested_entry: Option<crate::HistoryEntryId>,
    open: bool,
    /// Whether the closed toolbar offers its own History button. Off where
    /// the host's chrome (a menu bar, the Tilo shell) presents History.
    open_button: bool,
    /// Embedded hosts may provide a fallback; app windows use their existing header.
    fallback_bar: bool,
    /// The live app draws the window's header row, which holds the History
    /// control while History is closed, so this toolbar draws none then.
    app_presents_button: bool,
    /// The app's header row shows the sync status, so this bar does not.
    app_presents_status: bool,
    /// The desktop window's tab row presents the History control, open and
    /// closed: this History draws no toggle of its own, and closes from the
    /// tab, or with Escape.
    window_presents: bool,
    commands: Rc<RefCell<VecDeque<Command>>>,
    selected: Vec<crate::HistoryEntryId>,
    /// The foot's name for the selected changes, and the selection whose
    /// naming is open: another selection closes it.
    name_draft: crate::LocalState<String>,
    name_draft_for: Option<Vec<crate::HistoryEntryId>>,
    /// Descriptions of the changes, asked of the shell ([`describe`]).
    describing: describe::Describing,
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
    /// Bumped whenever the retained toolbar is replaced.
    toolbar_generation: u64,
    /// The composition the last layout build admitted, if any.
    built_composition: Option<Composition>,
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
    /// The side panel and the content beside it, its foot and the banner
    /// over the preview, while History shows as a side panel.
    panel_node: Option<NodeId>,
    content_node: Option<NodeId>,
    footer_node: Option<NodeId>,
    banner_node: Option<NodeId>,
    panel_rect: Rect,
    /// The side panel sliding in from the window's trailing edge as History
    /// opens, and the motion settings the last frame drew with.
    panel_motion: crate::motion::Presence,
    motion: crate::tokens::EffectiveMotion,
    /// What the frame being drawn painted ([`Self::draw_compared`]), and
    /// what the last presented frame did ([`Self::paint_presented`]).
    #[cfg(feature = "native")]
    painting: Option<Scene>,
    #[cfg(feature = "native")]
    painted: Option<Scene>,
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

    /// Return to the live app, as the History button does while open.
    pub(crate) fn request_close(&self) {
        if self.open {
            self.open_requested.set(true);
        }
    }

    /// Open with `entry` selected, as choosing a recent change in a History
    /// menu does. Selecting a change shows its before and after.
    pub(crate) fn request_open_at(&mut self, entry: crate::HistoryEntryId) -> bool {
        if !self.request_open() {
            return false;
        }
        self.requested_entry = Some(entry);
        true
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The window's History door as this History serves it: the same
    /// Open, Open at and Hide as the standalone door, without session rows.
    pub(crate) fn door_resource(&self) -> tilo_actions::Resource {
        crate::standalone_history_host::history_resource(!self.invalid_owner, self.is_open())
    }

    /// Run one History door operation resolved against [`Self::door_resource`]
    /// (a header History control, the History shortcut): it opens, opens at
    /// a change, or closes exactly as this History's own button does.
    pub(crate) fn invoke_door(
        &mut self,
        encoded: &tilo_actions::EncodedAction,
    ) -> Result<(), String> {
        use crate::standalone_history_host::{HISTORY_OPERATION, HistoryCommand, decode_entry};
        if encoded.type_id != HISTORY_OPERATION {
            return Err("Unsupported History operation".into());
        }
        match serde_json::from_slice(&encoded.payload) {
            Ok(HistoryCommand::Open) if self.request_open() => Ok(()),
            Ok(HistoryCommand::OpenAt { entry })
                if decode_entry(&entry).is_ok_and(|entry| self.request_open_at(entry)) =>
            {
                Ok(())
            }
            Ok(HistoryCommand::Hide) => {
                self.request_close();
                Ok(())
            }
            _ => Err("Unsupported History operation".into()),
        }
    }

    /// The changes the timeline would list now, in causal order, whether
    /// or not History is open.
    #[cfg(any(feature = "native", test))]
    pub(crate) fn current_entries(&self, doc: &AutoCommit) -> Vec<crate::widget::HistoryEntry> {
        let Some(controller) = self.controller.as_ref().filter(|_| !self.invalid_owner) else {
            return Vec::new();
        };
        self.source()
            .document(doc)
            .map(|document| controller.entries_at(&document))
            .unwrap_or_default()
    }

    /// The host presents History elsewhere, or the system surface omits
    /// document chrome: retain History custody without its closed button.
    pub(crate) fn remove_fallback_bar(&mut self) {
        self.fallback_bar = false;
        self.rebuild_toolbar();
    }

    pub(crate) fn remove_open_button(&mut self) {
        self.set_open_button(false);
    }

    /// Whether the app's header row is offered the History control while
    /// History is closed. Off where the host's chrome presents it, such as
    /// the desktop window's tab row.
    pub(crate) fn set_open_button(&mut self, shown: bool) {
        if self.open_button != shown {
            self.open_button = shown;
            self.rebuild_toolbar();
        }
    }

    /// The desktop window's tab row presents this History's control (see
    /// `window_presents`).
    pub(crate) fn set_window_presents(&mut self, presents: bool) {
        if self.window_presents != presents {
            self.window_presents = presents;
            self.rebuild_toolbar();
        }
    }

    /// Whether this History can be opened at all: its owner is valid.
    pub(crate) fn offers_control(&self) -> bool {
        !self.invalid_owner
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

        // History projects when it opens; a launch only learns the frontier.
        let controller = match source {
            HistorySourceRegistration::HostDocument { resource, .. }
            | HistorySourceRegistration::OwnedDocument { resource, .. } => Some(
                HistoryController::unprojected(resource.clone(), source.committed_heads(doc)?),
            ),
            HistorySourceRegistration::Unversioned { .. }
            | HistorySourceRegistration::CheckedFiles { .. } => None,
        };
        let unavailable = unavailable_state(match registration.sources.first() {
            Some(HistorySourceRegistration::Unversioned { reason, .. }) => reason,
            _ => "history-mode-content-unavailable",
        });
        let request = Rc::new(Cell::new(false));
        let toolbar = Column::new()
            .with_no_padding()
            .with_gap(4.0)
            .with_intrinsic_height();
        let mut host = Self {
            enclosing_owner: None,
            embedded: false,
            keyboard_active: true,
            registration,
            assets: media::AssetSession::new(),
            active_scope: 0,
            sync_status: None,
            bar_status: crate::widget::HeaderStatus::default(),
            restored_source: None,
            review: None,
            review_request: None,
            review_node: None,
            clock: crate::time::try_unix_now,
            controller,
            files: None,
            retired_files: vec![],
            requested_file_version: None,
            observed_file_binding: None,
            file_target_resolved: false,
            toolbar,
            footer: Column::new().with_no_padding().with_intrinsic_height(),
            banner: Row::new(),
            unavailable,
            unavailable_node: None,
            timeline: {
                // History is read by who did what: its strip filters by person
                // and agent.
                let mut timeline = ActivityTimeline::new().with_preview_selection();
                timeline.set_filters_people(true);
                timeline
            },
            open_requested: request,
            requested_entry: None,
            open: false,
            open_button: true,
            fallback_bar: true,
            app_presents_button: false,
            app_presents_status: false,
            window_presents: false,
            rect: Rect::ZERO,
            chrome_node: None,
            live_node: None,
            toolbar_node: None,
            body_node: None,
            timeline_node: None,
            canvas_node: None,
            panel_node: None,
            content_node: None,
            footer_node: None,
            banner_node: None,
            panel_rect: Rect::ZERO,
            panel_motion: crate::motion::Presence::closed(),
            motion: crate::tokens::EffectiveMotion::default(),
            commands: Rc::new(RefCell::new(VecDeque::new())),
            selected: vec![],
            name_draft: crate::LocalState::new(String::new()),
            name_draft_for: None,
            describing: describe::Describing::default(),
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
            toolbar_generation: 0,
            built_composition: None,
            live_root_lifetime: uuid::Uuid::new_v4(),
            control_domain: None,
            control_lease: None,
            invalid_owner: false,
            #[cfg(feature = "native")]
            painting: None,
            #[cfg(feature = "native")]
            painted: None,
        };
        host.rebuild_toolbar();
        Ok(host)
    }

    /// Draw into a scene of its own, append that to `scene`, and say whether
    /// it differs from what the last presented frame painted: a window that
    /// shows the live app under an unchanged History toolbar repaints only
    /// what the app's widgets damage.
    #[cfg(feature = "native")]
    pub(crate) fn draw_compared(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut crate::text_render::TextRenderer,
        (theme, presence): (&crate::theme::Theme, &crate::presence::PresenceCtx),
    ) -> bool {
        let mut own = self.painting.take().unwrap_or_default();
        own.reset();
        Widget::draw(self, &mut own, transform, text, theme, presence);
        scene.append(&own, None);
        let changed = self.painted.as_ref().map_or_else(
            || !own.encoding().draw_tags.is_empty(),
            |was| !was.encoding_eq(&own),
        );
        self.painting = Some(own);
        changed
    }

    /// The frame drawn last was presented: what the next frame's paint is
    /// compared with.
    #[cfg(feature = "native")]
    pub(crate) fn paint_presented(&mut self) {
        if let Some(painting) = self.painting.take() {
            self.painted = Some(painting);
        }
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
    /// Take a new sync status where it already shows, without rebuilding:
    /// closed, the status is quiet text in the live app's header row, which
    /// repaints just that text when it changes, and this bar's own layout
    /// does not depend on it. Returns `false`, changing nothing here, when
    /// the status would move or appear in this bar instead; the caller then
    /// refreshes it whole ([`Self::set_sync_status`]).
    pub(crate) fn set_sync_status_in_place(
        &mut self,
        status: Option<&str>,
        live: &mut dyn Widget,
    ) -> bool {
        if self.open || self.invalid_owner || !self.app_presents_status {
            return false;
        }
        if !crate::widget::window_header::offer_sync_status(live, status) {
            return false;
        }
        self.sync_status = status.map(str::to_owned);
        true
    }
    pub(crate) fn validate_root(&self, root: &dyn Widget) -> Result<(), HistoryRegistrationError> {
        let _phase = tilo_resource::profiling::Phase::start("ui.history_root_validation");
        self.registration
            .validate_root_with_owner(root, self.enclosing_owner.as_ref())
    }
    /// Whether this is the History of the tile that hosts `owner`'s app.
    pub(crate) fn encloses(&self, owner: &super::HistoryOwner) -> bool {
        self.enclosing_owner
            .as_ref()
            .is_some_and(|enclosing| enclosing.same_owner(owner))
    }
    /// The app the tile holding this History hosts.
    pub(crate) fn enclosing_owner(&self) -> Option<&super::HistoryOwner> {
        self.enclosing_owner.as_ref()
    }
    /// The History door actions a control in the hosted app can run now:
    /// Open while closed, Hide while open, none for an invalid owner.
    pub(crate) fn door_addresses(&self) -> std::collections::HashSet<tilo_actions::ActionAddress> {
        tilo_actions::door::survey(&[self.door_resource()])
            .into_iter()
            .filter(|descriptor| descriptor.contract.availability.is_available())
            .map(|descriptor| descriptor.address)
            .collect()
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
        self.panel_node = None;
        self.content_node = None;
        self.footer_node = None;
        self.banner_node = None;
        self.panel_rect = Rect::ZERO;
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
    /// The checked-file scope whose content asked to show a file's history
    /// while another scope is shown: opening a file's history from the app
    /// (its History command on a selected file) moves to that file's scope.
    fn file_scope_asking_to_open(&self) -> Option<usize> {
        if matches!(
            self.source(),
            HistorySourceRegistration::CheckedFiles { .. }
        ) {
            return None;
        }
        self.registration
            .definition
            .scopes
            .iter()
            .position(|scope| {
                self.registration.sources.iter().any(|source| {
                    matches!(
                        source,
                        HistorySourceRegistration::CheckedFiles { scope: key, source }
                            if *key == scope.key && source.wants_open()
                    )
                })
            })
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
            .invalidate_owner(&crate::i18n::I18nManager::lookup_plain(
                "history-mode-owner-gone",
            ));
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
        self.unavailable =
            unavailable_state(&HistoryRegistrationError::UnsupportedOwner.to_string());
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
            self.name_pending_controls(live, doc);
            // The failed entry traversal could not observe direct-take output.
            // Its uncertainty remains visible even after sealed queues recover.
            self.toolbar_dirty = true;
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
        while let Some(invocation) = crate::widget::controls::take_control_invocation(live) {
            let (origin, witness) = match self.enclosing_owner.clone() {
                Some(owner) => match &owner.0 {
                    super::owner::OwnerKind::Component(binding) => {
                        let origin = super::pending_controls::PendingControlOrigin::Component(
                            crate::bound_dispatch::BoundActionTarget {
                                input: crate::bound_dispatch::Input::Discrete,
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
        self.name_pending_controls(live, doc);
        // Capture can run with the preceding frame's raw maps still admitted.
        // Replace controls only when the runner has discarded those maps.
        self.toolbar_dirty = true;
        true
    }
    /// Admit the held controls by their declarations: each is named as its
    /// action is declared, so the review row says what it would do instead
    /// of showing internal addresses, and housekeeping is never held.
    fn name_pending_controls(&mut self, live: &dyn Widget, doc: &AutoCommit) {
        self.pending_controls.admit_declared(|origin, invocation| {
            crate::bound_dispatch::control_declaration(
                live,
                doc,
                &origin.target(),
                &invocation.address,
            )
        });
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
        while crate::widget::controls::take_control_invocation(live).is_some() {}
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
                Err(crate::i18n::I18nManager::lookup_plain(
                    "history-mode-custody-lost",
                )),
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
    /// Closed, with nothing in flight: no input captured or asked for, no
    /// focus saved or being returned, no control return and no review
    /// requested. Only input (or a refresh, which recomposes) moves a
    /// surface out of rest, so a frame that had neither leaves it here.
    pub(crate) fn at_rest(&self) -> bool {
        !self.captures_input()
            && self.saved_focus.is_none()
            && !self.returning_live
            && self.control_lease.is_none()
            && !self.has_review_request()
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
    /// The changes the timeline shows selected.
    pub(crate) fn timeline_selection(&self) -> Vec<crate::HistoryEntryId> {
        self.timeline
            .selection()
            .and_then(|(start, end)| self.timeline.entries().get(start..=end))
            .map(|entries| entries.iter().map(|entry| entry.id).collect())
            .unwrap_or_default()
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
            crate::widget::find_test_id_mut(widget, id).is_some_and(|widget| {
                widget.perform_action(
                    &crate::a11y::Action::Activate,
                    &[],
                    &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                    &automerge::ROOT,
                )
            })
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
                    || self.panel_rect.contains(point)
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
        clear_focus(&mut self.footer);
        clear_focus(&mut self.banner);
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
    fn owned_layout_roots(&self) -> [Option<NodeId>; 7] {
        [
            self.toolbar_node,
            self.timeline_node,
            self.canvas_node,
            self.unavailable_node,
            self.review_node,
            self.footer_node,
            self.banner_node,
        ]
    }
    pub(crate) fn extend_focus_denials(&self, tree: &TaffyTree<()>, denied: &mut HashSet<NodeId>) {
        if !self.captures_input() && !self.has_saved_focus() {
            return;
        }
        let owned = self.owned_nodes(tree);
        let mut pending: Vec<_> = self.chrome_node.into_iter().collect();
        while let Some(node) = pending.pop() {
            if owned.contains(&node) {
                continue;
            }
            // Ownership is exempted per surface, before combining denials.
            // An already denied parent may still have descendants exempted by
            // another surface, so its children must always be visited here.
            denied.insert(node);
            pending.extend(tree.children(node).unwrap_or_default());
        }
    }
    /// Every layout node this History owns, collected in one walk: the set
    /// [`Self::owns_node`] answers for one node, for callers that ask it of
    /// every node in a composition.
    pub(crate) fn owned_nodes(&self, tree: &TaffyTree<()>) -> HashSet<NodeId> {
        #[cfg(test)]
        OWNERSHIP_WALKS.with(|walks| walks.set(walks.get() + 1));
        let mut owned = HashSet::new();
        let mut pending: Vec<_> = self.owned_layout_roots().into_iter().flatten().collect();
        while let Some(node) = pending.pop() {
            if owned.insert(node) {
                pending.extend(tree.children(node).unwrap_or_default());
            }
        }
        owned
    }
    pub(crate) fn owns_node(&self, node: NodeId, tree: &TaffyTree<()>) -> bool {
        #[cfg(test)]
        OWNERSHIP_WALKS.with(|walks| walks.set(walks.get() + 1));
        let mut pending: Vec<_> = self.owned_layout_roots().into_iter().flatten().collect();
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
        self.refresh_without_owners(width)
            .unwrap_or_else(|| self.heads_need_refresh(doc))
    }
    /// [`Self::needs_refresh`] as far as it can be answered without reading
    /// an owner's document: `None` leaves it to the committed frontier
    /// ([`Self::heads_need_refresh`]). What other threads deliver (asset and
    /// file results) and what is queued for the surface are read here.
    pub(crate) fn refresh_without_owners(&self, width: f64) -> Option<bool> {
        if self.invalid_owner {
            return Some(false);
        }
        if self.assets.has_results() {
            return Some(true);
        }
        if self.file_scope_asking_to_open().is_some() {
            return Some(true);
        }
        if let HistorySourceRegistration::CheckedFiles { source, .. } = self.source()
            && (source.target().map(|target| target.binding) != self.observed_file_binding
                || source.wants_open()
                || self
                    .files
                    .as_ref()
                    .is_some_and(super::files_session::FileController::has_results)
                || self
                    .retired_files
                    .iter()
                    .any(super::files_session::FileController::has_results))
        {
            return Some(true);
        }
        if !self.pending_control_recovery.is_empty()
            || self.toolbar_dirty
            || !self.pending_controls.commands().borrow().is_empty()
            || self.open_requested.get()
            || self.requested_entry.is_some()
            || !self.commands.borrow().is_empty()
            || (self.width < 600.0) != (width < 600.0)
        {
            return Some(true);
        }
        let selected = self.timeline_selection();
        if !selected.is_empty() && selected != self.selected {
            return Some(true);
        }
        matches!(
            self.source(),
            HistorySourceRegistration::CheckedFiles { .. }
        )
        .then_some(false)
    }
    /// Whether the shown source's committed frontier asks for a refresh: it
    /// cannot be read, or an open History has not taken it up yet.
    pub(crate) fn heads_need_refresh(&self, doc: &AutoCommit) -> bool {
        let _phase = tilo_resource::profiling::Phase::start("ui.history_heads");
        let Ok(heads) = self.source().committed_heads(doc) else {
            return true;
        };
        // Closed history does not consume the new frontier in refresh.
        // Leave it pending until opening, instead of rebuilding every frame.
        self.open
            && self.controller.as_ref().is_some_and(|controller| {
                super::sorted_heads(heads) != controller.observed_live_heads
            })
    }
    /// Whether every document this surface answers for is a hosted
    /// component's (or the host's own), whose standing a quiet frame's
    /// witness follows: its record's frontier, and whether it can be read
    /// at all. A legacy tile's document is not followed.
    pub(crate) fn owners_followed_by_frame_witness(&self) -> bool {
        let followed =
            |owner: &super::HistoryOwner| matches!(owner.0, super::owner::OwnerKind::Component(_));
        self.enclosing_owner.as_ref().is_none_or(followed)
            && self.registration.sources.iter().all(|source| match source {
                HistorySourceRegistration::OwnedDocument { owner, .. } => followed(owner),
                HistorySourceRegistration::CheckedFiles { .. }
                | HistorySourceRegistration::HostDocument { .. }
                | HistorySourceRegistration::Unversioned { .. } => true,
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
        let started = crate::time::now();
        let mut opening = false;
        if self.open_requested.replace(false) {
            self.open = !self.open;
            opening = self.open;

            self.returning_live = !self.open;
            if !self.open {
                self.close_files_presentation();
            }
            changed = true;
        }
        let requested = self.requested_entry.take().filter(|_| self.open);
        if let Some(scope) = self.file_scope_asking_to_open() {
            self.commands.borrow_mut().push_back(Command::Scope(scope));
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
                    if matches!(command, review::ReviewCommand::Cancel)
                        && let Some(files) = &mut self.files
                    {
                        files.cancel_review();
                        self.review = None;
                    }
                    self.review_request = Some(command);
                }
                Command::FileParent(parent) => {
                    if let Some(files) = &mut self.files
                        && let Some(version) = files.selected()
                    {
                        files.select(version, Some(parent));
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
                Command::Now => {
                    self.review = None;
                    self.review_request = None;
                    self.assets.cancel();
                    self.selected.clear();
                    self.timeline.set_selection(None);
                    self.before.preview = None;
                    self.after.preview = None;
                    self.activity_page = false;
                    if let Some(controller) = &mut self.controller {
                        controller.back_to_live();
                    }
                }
                Command::Earlier => {
                    if let Ok(document) = self.source().document(doc)
                        && let Some(controller) = &mut self.controller
                    {
                        let _ = controller.show_earlier(&document);
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
                Command::Name => {
                    let seed = self
                        .selected_name()
                        .map(|named| named.label.clone())
                        .unwrap_or_default();
                    self.name_draft.set(seed);
                    self.name_draft_for = Some(self.selected.clone());
                }
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
                                if let Ok(heads) = self.source().committed_heads(doc) {
                                    self.controller = Some(HistoryController::unprojected(
                                        resource.clone(),
                                        heads,
                                    ));
                                }
                            }
                            HistorySourceRegistration::Unversioned { reason, .. } => {
                                self.unavailable = unavailable_state(reason);
                            }
                            HistorySourceRegistration::CheckedFiles { .. } => {
                                self.close_files_presentation();
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
        let selected = requested.map_or_else(|| self.timeline_selection(), |entry| vec![entry]);
        // Projecting history replays the document's change log, and the live
        // frontier moves with every keystroke. Only an open history view needs
        // it — or the copy of the document it projects from, which costs as
        // much as the document; opening one refreshes against the frontier it
        // then finds.
        let document = if self.open {
            let Ok(document) = self.source().document(doc) else {
                self.invalidate_owner();
                return;
            };
            Some(document)
        } else {
            None
        };
        if let Some(document) = &document
            && let Some(controller) = &mut self.controller
        {
            let _ = controller.refresh(document);
            if selected != self.selected
                && !selected.is_empty()
                && controller.select(document, &selected).is_ok()
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
            self.timeline
                .set_named_ranges(controller.named_ranges().to_vec());
            self.timeline.set_descriptions(describe::shown_descriptions(
                controller.descriptions(),
                &self.describing,
            ));
            if opening {
                // One line per opening, for gates and logs that keep History
                // fast on long histories.
                crate::diag!(
                    "HISTORY-OPEN listed={} earlier={} ms={}",
                    controller.entries().len(),
                    controller.has_earlier(),
                    crate::time::now()
                        .saturating_duration_since(started)
                        .as_millis()
                );
            }
            if let Some(entry) = requested
                && self.selected == [entry]
                && let Some(index) = self
                    .timeline
                    .entries()
                    .iter()
                    .position(|row| row.id == entry)
            {
                self.timeline.set_selection(Some((index, index)));
            }
        }
        if let Some(mut document) = document {
            self.queue_descriptions(&mut document);
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
        Button::new(id, label).on_click(move || queue.borrow_mut().push_back(command))
    }
    fn preview_page_toolbar(&self, mut toolbar: Column) -> Column {
        if let Some(preview) = &self.canvas().preview {
            let pages = preview.accessibility().pages;
            if pages.len() > 1 {
                let current = preview.view().page;
                let label = pages
                    .iter()
                    .find(|(id, _)| Some(id) == current.as_ref())
                    .map_or("history-mode-page", |(_, title)| title.as_str());
                toolbar = toolbar.push(Box::new(Text::new(label))).push(Box::new(
                    Row::new()
                        .push(Box::new(self.command_button(
                            "history-previous-page",
                            "history-mode-previous-page",
                            Command::PreviousPage,
                        )))
                        .push(Box::new(self.command_button(
                            "history-next-page",
                            "history-mode-next-page",
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
        let mut toolbar = Column::new()
            .with_no_padding()
            .with_gap(4.0)
            .with_intrinsic_height();
        self.bar_status.set(None);
        if self.invalid_owner {
            self.toolbar = toolbar.push(Box::new(Text::new("history-mode-unavailable")));
            self.toolbar_generation = self.toolbar_generation.wrapping_add(1);
            return;
        }
        let side_panel = self.side_panel();
        self.footer = Column::new()
            .with_no_padding()
            .with_gap(8.0)
            .with_intrinsic_height();
        self.banner = Row::new();
        if side_panel {
            toolbar = toolbar.push(Box::new(self.panel_header()));
        }
        if self.selected.is_empty() {
            if !side_panel
                && ((self.fallback_bar && self.open_button && !self.app_presents_button)
                    || (self.open && !self.window_presents))
            {
                // This bar already reserves the History control's height. Status
                // belongs beside it, not in a new row that moves the live app
                // when an ordinary input first discovers unsigned history.
                if !self.app_presents_status {
                    self.bar_status.set(self.sync_status.as_deref());
                }
                let clicked = self.open_requested.clone();
                toolbar = toolbar.push(Box::new(
                    Row::new()
                        .with_justify(taffy::JustifyContent::FlexEnd)
                        .push(Box::new(
                            crate::standalone_history_host::history_control_face(
                                "history-open",
                                crate::widget::window_header::HistoryControl::of(self.open),
                            )
                            .on_click(move || clicked.set(true)),
                        )),
                ));
            }
            for review in self.pending_controls.review() {
                // Named as the action's declaration names it; ids, owners and
                // raw arguments are internal and never shown to people.
                // While History shows, it waits; once left, it was held.
                let held = !matches!(
                    review.status,
                    super::pending_controls::PendingControlStatus::Suspended
                );
                let text = match (&review.label, held) {
                    (Some(action), false) => I18nManager::format(
                        "history-mode-pending",
                        &[("action", action.clone().into())],
                    ),
                    (Some(action), true) => I18nManager::format(
                        "history-mode-pending-held",
                        &[("action", action.clone().into())],
                    ),
                    (None, false) => I18nManager::lookup_plain("history-mode-pending-unnamed"),
                    (None, true) => I18nManager::lookup_plain("history-mode-pending-held-unnamed"),
                };
                let mut row = Row::new().push(Box::new(Text::new(&text)));
                let label = match review.status {
                    super::pending_controls::PendingControlStatus::Ready => {
                        Some("history-mode-continue")
                    }
                    super::pending_controls::PendingControlStatus::NeedsConfirmation => {
                        Some("history-mode-confirm")
                    }
                    super::pending_controls::PendingControlStatus::Suspended
                    | super::pending_controls::PendingControlStatus::Consumed => None,
                    super::pending_controls::PendingControlStatus::Refused(ref reason) => {
                        row = row.push(Box::new(Text::new(reason)));
                        None
                    }
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
                                I18nManager::lookup_plain("history-mode-confirmation-unavailable")
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
                                .with_placeholder(I18nManager::format(
                                    "history-mode-type-to-confirm",
                                    &[("phrase", expected.into())],
                                ))
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
                            Button::new(&id, label).on_click(move || callback.invoke()),
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
                            Button::new(&id, label).on_click(move || callback.invoke()),
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
                        Button::new(&cancel_id, "history-mode-cancel")
                            .on_click(move || callback.invoke()),
                    ));
                }
                toolbar = toolbar.push(Box::new(row));
            }
            for failure in &self.pending_control_capture_failures {
                toolbar = toolbar.push(Box::new(Text::new(failure)));
            }
            if self.open
                && self
                    .controller
                    .as_ref()
                    .is_some_and(HistoryController::has_earlier)
            {
                toolbar = toolbar.push(Box::new(self.command_button(
                    "history-earlier",
                    "history-mode-earlier",
                    Command::Earlier,
                )));
            }
        } else if side_panel {
            self.build_selection_panel();
            toolbar = self.preview_page_toolbar(toolbar);
        } else {
            let mut header = Row::new();
            if self.width < 600.0 && !self.activity_page {
                header = header.push(Box::new(self.command_button(
                    "history-activity",
                    "history-mode-back-to-activity",
                    Command::Activity,
                )));
            }
            header = header.push(Box::new(self.command_button(
                "history-back-live",
                "history-mode-back-to-live",
                Command::Live,
            )));
            let mut compare = Row::new();
            for (id, label, command, side) in [
                (
                    "history-before",
                    "history-mode-before",
                    Command::Before,
                    PreviewSide::Before,
                ),
                (
                    "history-after",
                    "history-mode-after",
                    Command::After,
                    PreviewSide::After,
                ),
            ] {
                let mut button = self.command_button(id, label, command);
                if self.side == side {
                    button = button.role(ButtonRole::Primary);
                }
                compare = compare.push(Box::new(button));
            }
            compare = compare.push(Box::new(Text::new(if self.side == PreviewSide::Before {
                "history-mode-showing-before"
            } else {
                "history-mode-showing-after"
            })));
            if self.offers_restore() {
                compare = compare.push(Box::new(self.command_button(
                    "history-restore",
                    "history-mode-restore",
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
                Text::new("history-mode-live-updates").with_live(crate::a11y::Politeness::Polite),
            ));
        }
        // Which history to show is a choice inside History: while it is
        // closed the live app owns the bar, and a scope switch there reads
        // as the app's own control.
        if self.open && self.registration.definition.scopes.len() > 1 {
            let mut scopes = Row::new();
            for (index, scope) in self.registration.definition.scopes.iter().enumerate() {
                let mut button = self.command_button(
                    &format!("history-scope-{}", scope.key),
                    scope.title,
                    Command::Scope(index),
                );
                if index == self.active_scope {
                    button = button.role(ButtonRole::Primary);
                }
                scopes = scopes.push(Box::new(button));
            }
            toolbar = toolbar.push(Box::new(scopes));
        }
        toolbar = self.files_toolbar(toolbar);
        if let Some(status) = self
            .sync_status
            .as_ref()
            .filter(|_| !self.app_presents_status && self.bar_status.text().is_none())
        {
            toolbar = toolbar.push(Box::new(
                Text::new(status).with_live(crate::a11y::Politeness::Polite),
            ));
        }
        self.toolbar = toolbar;
        self.toolbar_dirty = false;
        self.toolbar_generation = self.toolbar_generation.wrapping_add(1);
    }
    /// History shows as a panel at the window's trailing side, the content
    /// in place beside it: open in a window wide enough for both.
    pub(super) fn side_panel(&self) -> bool {
        self.open && self.width >= SIDE_PANEL_MIN_WIDTH
    }

    /// Whether the selected version can be restored in this scope.
    fn offers_restore(&self) -> bool {
        self.registration
            .definition
            .scopes
            .get(self.active_scope)
            .is_some_and(|scope| {
                scope.content_schema.is_some()
                    || matches!(scope.presentation, HistoryPresentation::CheckedFiles { .. })
                    || (matches!(
                        scope.presentation,
                        HistoryPresentation::Domain { .. }
                            | HistoryPresentation::DomainWithAssets { .. }
                    ) && matches!(
                        self.source(),
                        HistorySourceRegistration::HostDocument { .. }
                            | HistorySourceRegistration::OwnedDocument { .. }
                    ))
            })
    }

    /// Whether the selected changes can be taken back on their own: a
    /// document's own changes, not a file's versions or an outside effect.
    fn offers_change_restore(&self) -> bool {
        matches!(
            self.source(),
            HistorySourceRegistration::HostDocument { .. }
                | HistorySourceRegistration::OwnedDocument { .. }
        ) && self
            .selected_entries()
            .any(crate::widget::HistoryEntry::is_revertible)
    }

    /// Whether the selected changes can be named: a document's own changes.
    fn offers_naming(&self) -> bool {
        matches!(
            self.source(),
            HistorySourceRegistration::HostDocument { .. }
                | HistorySourceRegistration::OwnedDocument { .. }
        ) && self.selected_entries().any(|entry| !entry.is_genesis())
    }

    /// The oldest and newest selected change: the run a name covers.
    pub(super) fn selected_run(&self) -> Option<(crate::HistoryEntryId, crate::HistoryEntryId)> {
        let mut picked = self.selected_entries();
        let first = picked.next()?.id;
        Some((first, picked.last().map_or(first, |entry| entry.id)))
    }

    /// The name the selected run already carries.
    pub(super) fn selected_name(&self) -> Option<&crate::NamedRange> {
        let (start, end) = self.selected_run()?;
        self.timeline
            .named_ranges()
            .iter()
            .find(|named| named.start == start && named.end == Some(end))
    }

    /// The selected changes as the timeline lists them.
    fn selected_entries(&self) -> impl Iterator<Item = &crate::widget::HistoryEntry> {
        self.timeline
            .entries()
            .iter()
            .filter(|entry| self.selected.contains(&entry.id))
    }

    /// The side panel's head: its name, and the control that closes it.
    /// Where no window presents History's control and nothing is selected,
    /// that is the History control itself, shown on; otherwise it is Back to
    /// live (closing the panel returns to the live document, as that
    /// control does), so either keeps its address.
    fn panel_header(&self) -> Row {
        let close: Button = if self.selected.is_empty() && !self.window_presents {
            // No window presents History's control: the panel holds it, on.
            let clicked = self.open_requested.clone();
            crate::standalone_history_host::history_control_face(
                "history-open",
                crate::widget::window_header::HistoryControl::Open,
            )
            .on_click(move || clicked.set(true))
        } else {
            let label = I18nManager::lookup_plain(if self.selected.is_empty() {
                "history-panel-close"
            } else {
                "history-mode-back-to-live"
            });
            let close = self.commands.clone();
            Button::icon_only("history-back-live", crate::icons::Icon::X, &label)
                .on_click(move || close.borrow_mut().push_back(Command::Live))
        };
        // The panel's name is painted beside the control
        // ([`Self::draw`]): the control already carries History's name for
        // assistive technology, and one name per thing reads once.
        Row::new()
            .with_justify(taffy::JustifyContent::FlexEnd)
            .with_align_items(taffy::AlignItems::Center)
            .push(Box::new(close))
    }

    /// The panel's foot and the preview's banner for the selected change:
    /// who made it and when, Before and After, Restore and Restore just this
    /// change; over the preview, that it is an earlier version, read-only.
    fn build_selection_panel(&mut self) {
        let mut footer = Column::new()
            .with_no_padding()
            .with_gap(8.0)
            .with_intrinsic_height();
        let picked: Vec<crate::widget::HistoryEntry> = self.selected_entries().cloned().collect();
        if let Some(newest) = picked.iter().max_by_key(|entry| entry.timestamp) {
            let summary = if picked.len() > 1 {
                I18nManager::format(
                    "history-panel-selected-changes",
                    &[("count", picked.len().into())],
                )
            } else {
                self.timeline.summary_of(newest).to_owned()
            };
            footer = footer.push(Box::new(Text::new(&summary).strong()));
            let when = crate::widget::format_relative_time(newest.timestamp, crate::time::wall());
            footer = footer.push(Box::new(
                Row::new()
                    .with_gap(12.0)
                    .push(Box::new(
                        Text::new(&newest.author.name).with_role(crate::tokens::TextRole::Caption),
                    ))
                    .push(Box::new(
                        Text::new(&when)
                            .with_role(crate::tokens::TextRole::Caption)
                            .dimmed(),
                    )),
            ));
        }
        let mut compare = Row::new().with_gap(4.0);
        for (id, label, command, side) in [
            (
                "history-before",
                "history-mode-before",
                Command::Before,
                PreviewSide::Before,
            ),
            (
                "history-after",
                "history-mode-after",
                Command::After,
                PreviewSide::After,
            ),
        ] {
            compare = compare.push(Box::new(
                self.command_button(id, label, command)
                    .control_size(crate::widget::ControlSize::Small)
                    .toggled(self.side == side),
            ));
        }
        footer = footer.push(Box::new(compare));
        if self.offers_naming() {
            if self.name_draft_for.as_ref() == Some(&self.selected) {
                let commands = self.commands.clone();
                footer = footer.push(Box::new(
                    TextInput::from_state(&self.name_draft)
                        .with_id("history-name-field")
                        .with_placeholder("history-panel-name")
                        .on_submit(move |_| {
                            commands
                                .borrow_mut()
                                .push_back(Command::Review(review::ReviewCommand::Name));
                        }),
                ));
            } else {
                footer = footer.push(Box::new(
                    self.command_button("history-name", "history-panel-name", Command::Name)
                        .control_size(crate::widget::ControlSize::Small),
                ));
            }
        }
        let mut restore = Row::new().with_gap(8.0);
        if self.offers_restore() {
            restore = restore.push(Box::new(
                self.command_button(
                    "history-restore",
                    "history-mode-restore",
                    Command::Review(review::ReviewCommand::Open),
                )
                .role(ButtonRole::Primary),
            ));
        }
        if self.offers_change_restore() {
            restore = restore.push(Box::new(self.command_button(
                "history-restore-change",
                "history-mode-restore-change",
                Command::Review(review::ReviewCommand::RevertChange),
            )));
        }
        if !restore.children().is_empty() {
            footer = footer.push(Box::new(restore));
        }
        self.footer = footer;
        let now = self.commands.clone();
        self.banner = Row::new()
            .with_justify(taffy::JustifyContent::SpaceBetween)
            .with_align_items(taffy::AlignItems::Center)
            .with_padding(16.0, 6.0)
            .push(Box::new(
                Text::new(if self.side == PreviewSide::Before {
                    "history-preview-before"
                } else {
                    "history-preview-after"
                })
                .with_role(crate::tokens::TextRole::Label),
            ))
            .push(Box::new(
                Button::new("history-back-now", "history-preview-now")
                    .role(ButtonRole::Plain)
                    .on_click(move || now.borrow_mut().push_back(Command::Now)),
            ));
    }

    /// What this host's layout presents: which widgets its children are.
    pub(super) fn composition(&self) -> Composition {
        Composition {
            toolbar: self.toolbar_generation,
            review: self.review.is_some(),
            timeline: self.show_timeline().then(|| self.timeline_available()),
            preview: self.show_preview().then_some(self.side),
        }
    }
    pub(super) fn admit_composition(&mut self) {
        self.built_composition = Some(self.composition());
    }
    /// Whether this host replaced or swapped a child since its layout was
    /// built: every raw layout map into it is then stale.
    pub(super) fn composition_changed(&self) -> bool {
        self.built_composition
            .is_some_and(|built| built != self.composition())
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

mod describe;

mod media;

mod files;

/// Why a scope has no History, where its timeline would stand: History is
/// unavailable, and the reason (a message key or the owner's own text)
/// says why.
pub(super) fn unavailable_state(reason: &str) -> crate::widget::EmptyState {
    crate::widget::EmptyState::new(I18nManager::lookup_plain("history-mode-unavailable"))
        .with_icon(crate::icons::Icon::History)
        .with_message(I18nManager::lookup_plain(reason))
}
#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
#[path = "refresh_heads_tests.rs"]
mod refresh_heads_tests;
