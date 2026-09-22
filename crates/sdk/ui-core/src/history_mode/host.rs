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
    widget::{ActivityTimeline, Button, Column, Row, TaffyTreeExt, Text, Widget},
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
            invalid_owner: false,
        };
        host.rebuild_toolbar();
        Ok(host)
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
        self.rebuild_toolbar();
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
        let focus = self.saved_focus.take()?;
        unsafe { focus.restore(nodes, bindings, None) }
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
            return true;
        }
        if self.open_requested.get()
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
        let mut changed = target_changed;
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
                Command::PreviousPage | Command::NextPage => {
                    if let Some(preview) = &mut self.canvas_mut().preview {
                        preview.navigate(if matches!(command, Command::NextPage) {
                            super::PreviewNavigation::NextPage
                        } else {
                            super::PreviewNavigation::PreviousPage
                        });
                    }
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
                                self.unavailable = Text::new(reason);
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

    fn rebuild_toolbar(&mut self) {
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
    }
    pub(crate) fn live_visible(&self) -> bool {
        !(self.invalid_owner || self.show_preview() || self.open && self.width < 600.0)
    }
}

mod surface;

mod review;

mod media;

mod files;
