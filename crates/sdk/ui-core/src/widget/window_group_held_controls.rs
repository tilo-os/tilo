//! The window's own furniture in the row its app draws, as widgets of the window.
//!
//! A row that holds the window's bar keeps [`RowFurniture`] at its trailing end — the window's
//! new-tab door, its restore/maximize and its close — and the window paints them over the app's
//! drawing ([`RowFurniture::paint`]). Each of them is also a widget of the window: a control the
//! keyboard reaches in the window's Tab order after the app's own header controls, and a node of
//! its own for assistive technology. Focus, the focus ring, the accessibility tree and the press
//! all name the same control, so a keyboard-only person reaches what a pointer does.
//!
//! [`RowFurniture`]: crate::widget::window_header::RowFurniture
//! [`RowFurniture::paint`]: crate::widget::window_header::RowFurniture::paint

use super::WindowGroup;
use crate::widget::Widget;
use crate::widget::window_header::RowControl;
use taffy::{NodeId, TaffyTree};
use vello::kurbo::Rect;

/// One control of the window's furniture in the row its app draws: where the row places it, and
/// what pressing it asks the window to do.
pub(crate) struct HeldControl {
    /// Which of the window's functions it is.
    kind: RowControl,
    /// Where the row draws it this frame; nothing while the row has no room for it.
    rect: Rect,
    /// The window shows restore rather than maximize.
    maximized: bool,
    /// It has the keyboard.
    focused: bool,
    /// A press asked for its action, for the window to run
    /// ([`WindowGroup::drain_held_controls`]).
    pressed: bool,
    /// What it paints changed (the keyboard arrived or left): the window paints again.
    dirty: bool,
}

impl HeldControl {
    pub(super) fn new(kind: RowControl) -> Self {
        Self {
            kind,
            rect: Rect::ZERO,
            maximized: false,
            focused: false,
            pressed: false,
            dirty: false,
        }
    }

    /// Where the row draws it this frame, and which way the window's maximize control acts.
    pub(super) fn place(&mut self, rect: Rect, maximized: bool) {
        if self.rect != rect || self.maximized != maximized {
            self.rect = rect;
            self.maximized = maximized;
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

    /// Take the press a pointer, the keyboard or assistive technology made on it.
    pub(super) fn take_press(&mut self) -> bool {
        std::mem::take(&mut self.pressed)
    }

    pub(super) fn needs_repaint(&self) -> bool {
        self.dirty
    }

    pub(super) fn mark_painted(&mut self) {
        self.dirty = false;
    }

    /// The control that has the keyboard, when it is this one.
    pub(super) fn keyboard(&self) -> Option<RowControl> {
        self.focused.then_some(self.kind)
    }
}

impl Widget for HeldControl {
    /// The window lays it out where the row draws it: as a leaf of its own it takes no room and
    /// holds no geometry of the layout's ([`HeldControl::place`] is the one place it stands).
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
                .with_name(self.kind.name(self.maximized))
                .with_action_target(self.kind.target())
                .with_bounds(self.rect)
                .with_state(AccessState::new().focusable(true).focused(self.focused))
        })
    }

    /// A press — from the keyboard's Enter or Space, or from an assistive technology activating
    /// its node — asks the window to do what the control shows, as a pointer's does.
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
    /// The furniture of the row holding the bar, each control where the row places it this frame.
    pub(super) fn place_held_controls(&mut self) {
        let furniture = self.held;
        let maximized = self.maximized;
        for control in &mut self.held_controls {
            let rect = control.kind().rect(&furniture);
            control.place(rect, maximized);
        }
    }

    /// The window's own furniture in the row holding the bar, as children of the window after the
    /// app: the keyboard reaches it there, and a screen reader reads it there.
    pub(super) fn held_control_children<'a>(&'a self, children: &mut Vec<&'a dyn Widget>) {
        if self.held_bar.is_some() {
            children.extend(
                self.held_controls
                    .iter()
                    .map(|control| control as &dyn Widget),
            );
        }
    }

    /// The same furniture as a frame's own nodes: each takes no room of its own — the window puts
    /// it where the row draws it — but is a stop of the window's focus order there all the same.
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

    /// Move the furniture of the row holding the bar with the window, as a retained strip draws.
    pub(super) fn offset_held_controls(&mut self, dx: f64) {
        let shift = vello::kurbo::Vec2::new(dx, 0.0);
        self.held.shift(dx);
        if let Some(slot) = &mut self.held_bar {
            slot.row = slot.row + shift;
            slot.controls = slot.controls + shift;
        }
        self.place_held_controls();
    }

    /// Whether the keyboard stands on one of the window's own controls in the row its app draws.
    pub(super) fn held_control_focused(&self) -> bool {
        self.held_controls.iter().any(|control| control.focused)
    }

    /// Whether what is typed is the window's own: the keyboard stands on the furniture of the row
    /// the app draws, so the app under it hears nothing of it.
    pub(super) fn held_control_takes_keys(&self, event: &winit::event::WindowEvent) -> bool {
        self.held_control_focused()
            && matches!(
                event,
                winit::event::WindowEvent::KeyboardInput { .. } | winit::event::WindowEvent::Ime(_)
            )
    }

    /// Whether what the furniture paints changed, so the window paints again.
    pub(super) fn held_control_needs_repaint(&self) -> bool {
        self.held_controls.iter().any(HeldControl::needs_repaint)
    }

    pub(super) fn mark_held_controls_painted(&mut self) {
        for control in &mut self.held_controls {
            control.mark_painted();
        }
    }

    /// Run what the keyboard, a pointer or an assistive technology pressed on the window's furniture
    /// in the row its app draws, through the one path a press on it takes
    /// ([`WindowGroup::perform_held_furniture_action`]). Whether any ran.
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
