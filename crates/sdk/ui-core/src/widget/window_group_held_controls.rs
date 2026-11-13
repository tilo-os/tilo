//! The window's own furniture in the row its app draws, as widgets of the window.
use super::WindowGroup;
use crate::widget::Widget;
use crate::widget::window_header::RowControl;
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Rect;

pub(crate) struct HeldControl {
    kind: RowControl,
    rect: Rect,
    resize: crate::widget::window_controls::Resize,
    focused: bool,
    pressed: bool,
    dirty: bool,
}

impl HeldControl {
    pub(super) fn new(kind: RowControl) -> Self {
        Self {
            kind,
            rect: Rect::ZERO,
            resize: crate::widget::window_controls::Resize::Maximize,
            focused: false,
            pressed: false,
            dirty: false,
        }
    }

    pub(super) fn place(&mut self, rect: Rect, resize: crate::widget::window_controls::Resize) {
        if self.rect != rect || self.resize != resize {
            self.rect = rect;
            self.resize = resize;
            self.dirty = true;
        }
    }

    /// Whether the row had room to place it: a control it holds nothing for is not there at all.
    pub(super) fn placed(&self) -> bool {
        self.rect.area() > 0.0
    }

    pub(super) fn kind(&self) -> RowControl {
        self.kind
    }

    pub(super) fn take_press(&mut self) -> bool {
        std::mem::take(&mut self.pressed)
    }

    pub(super) fn needs_repaint(&self) -> bool {
        self.dirty
    }

    pub(super) fn mark_painted(&mut self) {
        self.dirty = false;
    }

    pub(super) fn keyboard(&self) -> Option<RowControl> {
        self.focused.then_some(self.kind)
    }
}

impl Widget for HeldControl {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        use crate::widget::TaffyTreeExt;
        let style = taffy::Style {
            position: taffy::Position::Absolute,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn is_focusable(&self) -> bool {
        self.placed()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    /// What it shows, as assistive technology reads it: named and targeted as the bar's own
    /// control for the same function, so a window shows one set of functions whether its row holds
    /// the bar or stands as its slots. A control the row has no room for is not there.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        self.placed().then(|| {
            AccessNode::new(Role::Button)
                .with_name(self.kind.name(self.resize))
                .with_action_target(self.kind.target())
                .with_bounds(self.rect)
                .with_state(AccessState::new().focusable(true).focused(self.focused))
        })
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        if !self.placed() || !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        self.pressed = true;
        self.dirty = true;
        true
    }

    fn name(&self) -> &'static str {
        "HeldControl"
    }
}

impl WindowGroup {
    pub(super) fn place_held_controls(&mut self) {
        let furniture = self.held;
        let resize = self.resize();
        for control in &mut self.held_controls {
            let rect = control.kind().rect(&furniture);
            control.place(rect, resize);
        }
    }

    pub(super) fn held_control_children<'a>(&'a self, children: &mut Vec<&'a dyn Widget>) {
        if self.held_bar.is_some() {
            children.extend(
                self.held_controls
                    .iter()
                    .map(|control| control as &dyn Widget),
            );
        }
    }

    pub(super) fn build_held_controls(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &crate::theme::Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        nodes: &mut Vec<NodeId>,
    ) {
        if self.held_bar.is_some() {
            nodes.extend(
                self.held_controls
                    .iter_mut()
                    .map(|control| control.build_layout(taffy, theme, node_map)),
            );
        }
    }

    pub(super) fn offset_held_controls(&mut self, dx: f64) {
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.held.shift(dx);
        if let Some(slot) = &mut self.held_bar {
            slot.row = slot.row + shift;
            slot.controls = slot.controls + shift;
        }
        self.place_held_controls();
    }

    pub(super) fn held_control_focused(&self) -> bool {
        self.held_controls.iter().any(|control| control.focused)
    }

    pub(super) fn held_control_takes_keys(&self, event: &winit::event::WindowEvent) -> bool {
        self.held_control_focused()
            && matches!(
                event,
                winit::event::WindowEvent::KeyboardInput { .. } | winit::event::WindowEvent::Ime(_)
            )
    }

    pub(super) fn held_control_needs_repaint(&self) -> bool {
        self.held_controls.iter().any(HeldControl::needs_repaint)
    }

    pub(super) fn mark_held_controls_painted(&mut self) {
        for control in &mut self.held_controls {
            control.mark_painted();
        }
    }

    pub(super) fn drain_held_controls(&mut self) -> bool {
        let mut ran = false;
        for index in 0..self.held_controls.len() {
            if !self.held_controls[index].take_press() {
                continue;
            }
            let target = self.held_controls[index].kind().target();
            ran |= self.perform_held_furniture_action(target, &crate::a11y::Action::Activate)
                == Some(true);
        }
        ran
    }
}
