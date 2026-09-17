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
use automerge::{AutoCommit, ObjId};
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
}

pub(crate) struct HistoryHost {
    registration: HistoryRegistration,
    controller: Option<HistoryController>,
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
        if registration.definition.scopes.len() != 1 {
            return Err(HistoryRegistrationError::UnsupportedScopes);
        }
        let scope = registration
            .definition
            .scopes
            .first()
            .ok_or(HistoryRegistrationError::InvalidScopes)?;
        let controller = match registration.sources.first() {
            Some(HistorySourceRegistration::HostDocument { resource, .. }) => Some(
                HistoryController::new(resource.clone(), doc)
                    .map_err(|_| HistoryRegistrationError::ForeignSource)?,
            ),
            _ => None,
        };
        let unavailable = Text::new(match registration.sources.first() {
            Some(HistorySourceRegistration::Unversioned { reason, .. }) => *reason,
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
        Ok(Self {
            registration,
            controller,
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
        self.controller = None;
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
    pub(crate) fn discard_saved_focus(&mut self) {
        self.saved_focus = None;
    }
    pub(crate) fn captures_input(&self) -> bool {
        self.open || !self.selected.is_empty()
    }
    pub(crate) fn has_saved_focus(&self) -> bool {
        self.saved_focus.is_some()
    }
    pub(crate) fn controls_event(&self, event: &WindowEvent, scale: f64) -> bool {
        if !self.live_visible() {
            return true;
        }
        let point = if let WindowEvent::CursorMoved { position, .. } = event {
            Point::new(position.x / scale, position.y / scale)
        } else if let WindowEvent::Touch(touch) = event {
            Point::new(touch.location.x / scale, touch.location.y / scale)
        } else {
            self.cursor
        };
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
    pub(crate) fn refresh(&mut self, doc: &AutoCommit) {
        if self.invalid_owner {
            return;
        }
        let had_live_updates = self
            .controller
            .as_ref()
            .is_some_and(HistoryController::has_live_updates);
        let mut changed = false;
        if self.open_requested.replace(false) {
            self.open = !self.open;
            self.returning_live = !self.open;
            changed = true;
        }
        let commands: Vec<_> = self.commands.borrow_mut().drain(..).collect();
        for command in commands {
            changed = true;
            match command {
                Command::Live => {
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
                Command::Activity => self.activity_page = true,
                Command::Before => {
                    self.side = PreviewSide::Before;
                    self.activity_page = false;
                }
                Command::After => {
                    self.side = PreviewSide::After;
                    self.activity_page = false;
                }
            }
        }
        // Resolve actual current selected IDs before replacing the projection;
        // positions are never retained as version authority.
        let selected: Vec<_> = self
            .timeline
            .selection()
            .and_then(|(start, end)| self.timeline.entries().get(start..=end))
            .map(|entries| entries.iter().map(|entry| entry.id).collect())
            .unwrap_or_default();
        if let Some(controller) = &mut self.controller {
            let _ = controller.refresh(doc);
            if selected != self.selected
                && !selected.is_empty()
                && controller.select(doc, &selected).is_ok()
            {
                self.selected = selected;
                self.activity_page = false;
                self.side = PreviewSide::After;
                if let Some(scope) = self.registration.definition.scopes.first()
                    && let HistoryPresentation::Domain { create } = scope.presentation
                {
                    let _ = controller.select_compare_side(PreviewSide::Before);
                    self.before.preview = controller.snapshot().and_then(|snapshot| {
                        create(PreviewInput {
                            snapshot: snapshot.clone(),
                            initial_view: PreviewView::default(),
                            assets: FrozenPreviewAssets::default(),
                        })
                        .ok()
                    });
                    let _ = controller.select_compare_side(PreviewSide::After);
                    self.after.preview = controller.snapshot().and_then(|snapshot| {
                        create(PreviewInput {
                            snapshot: snapshot.clone(),
                            initial_view: PreviewView::default(),
                            assets: FrozenPreviewAssets::default(),
                        })
                        .ok()
                    });
                }
                changed = true;
            }
            self.timeline.set_entries(controller.entries().to_vec());
        }
        let has_live_updates = self
            .controller
            .as_ref()
            .is_some_and(HistoryController::has_live_updates);
        if changed || had_live_updates != has_live_updates {
            self.rebuild_toolbar();
        }
    }
    fn command_button(&self, id: &str, label: &str, command: Command) -> Button {
        let queue = self.commands.clone();
        Button::new(id, label, None)
            .fit_content(32.0)
            .on_click(move || queue.borrow_mut().push_back(command))
    }
    fn rebuild_toolbar(&mut self) {
        let title = self
            .registration
            .definition
            .scopes
            .first()
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
            if self.width < 600.0 {
                toolbar = toolbar.push(Box::new(header)).push(Box::new(compare));
            } else {
                toolbar = toolbar.push(Box::new(
                    Row::new().push(Box::new(header)).push(Box::new(compare)),
                ));
            }
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
        self.toolbar = toolbar;
    }
    pub(crate) fn live_visible(&self) -> bool {
        !self.invalid_owner && !self.show_preview() && !(self.open && self.width < 600.0)
    }
}

mod surface;
