//! Tube's ActionBus-bound switch.
//!
//! Glyphs come directly from [`ui::widget::IconWidget`]. The switch keeps
//! Tube's app-local action dispatch while sharing the toolkit's toggle face,
//! focus treatment, and accessibility semantics.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Toggle, ToggleFace, Widget, toggle_accessibility};

use crate::action::{self, Action, ActionBus};

/// A pill toggle that renders `on` and pushes `action` through Tube's
/// [`ActionBus`] when activated.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent render/interaction cache (on-state, hover, focus, \
              dirty), not a state machine"
)]
pub struct Switch {
    on: bool,
    action: Action,
    bus: ActionBus,
    rect: Rect,
    hovered: bool,
    focused: bool,
    dirty: bool,
    test_id: Option<String>,
    accessible_name: Option<String>,
}

impl Switch {
    pub fn new(on: bool, bus: &ActionBus, action: Action) -> Self {
        Self {
            on,
            action,
            bus: bus.clone(),
            rect: Rect::ZERO,
            hovered: false,
            focused: false,
            dirty: true,
            test_id: None,
            accessible_name: None,
        }
    }

    /// Stable, addressable id so headless click-tests can flip this exact
    /// switch through the real `handle_event` path.
    #[must_use]
    pub fn id(mut self, id: &str) -> Self {
        self.test_id = Some(id.to_string());
        self
    }

    /// Spoken name for switches whose adjacent visual label is not available
    /// to an accessibility tree consumer.
    #[must_use]
    pub fn accessible_name(mut self, name: &str) -> Self {
        self.accessible_name = Some(name.to_string());
        self
    }

    fn activate(&self) {
        action::push(&self.bus, self.action.clone());
    }
}

impl Widget for Switch {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "a fresh leaf on a live taffy tree cannot fail short of OOM, and \
                      `Widget::build_layout` returns `NodeId` directly with no fallible path"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(46.0),
                    height: Dimension::length(26.0),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let face = ToggleFace::new(self.rect, self.on)
            .hovered(self.hovered)
            .focused(self.focused);
        Toggle::paint(scene, transform, theme, &face);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let hovered = self.rect.contains(point);
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.hovered && !*ctx.handled => {
                self.activate();
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn perform_action(
        &mut self,
        action: &ui::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            self.activate();
            true
        } else {
            false
        }
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(toggle_accessibility(
            self.on,
            self.focused,
            true,
            self.accessible_name.as_deref(),
        ))
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.rect
            .contains(point)
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        true
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

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn name(&self) -> &'static str {
        "Switch"
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{self, SettingFlag};
    use automerge::AutoCommit;

    fn switch() -> Switch {
        Switch::new(
            false,
            &action::new_bus(),
            Action::ToggleSetting(SettingFlag::Autoplay),
        )
        .id("switch_autoplay")
        .accessible_name("Autoplay")
    }

    #[test]
    fn switch_exposes_state_activates_via_bus_and_hit_tests_its_rect() {
        // Case `switch_preserves_id_and_exposes_switch_state`.
        {
            let mut switch = switch();
            switch.set_focus(true);

            assert_eq!(switch.test_id(), Some("switch_autoplay"));
            let node = switch.accessibility().expect("switch accessibility");
            assert_eq!(node.role, ui::a11y::Role::Switch);
            assert_eq!(node.name.as_deref(), Some("Autoplay"));
            assert_eq!(node.state.checked, Some(false));
            assert!(node.state.focusable);
            assert!(node.state.focused);
        }
        // Case `accessibility_activation_uses_tube_action_bus`.
        {
            let bus = action::new_bus();
            let mut switch =
                Switch::new(true, &bus, Action::ToggleSetting(SettingFlag::ShowComments));
            let mut state = AutoCommit::new();

            assert!(switch.perform_action(
                &ui::a11y::Action::ToggleCheck,
                &mut state,
                &automerge::ROOT,
            ));
            assert_eq!(
                action::drain(&bus),
                vec![Action::ToggleSetting(SettingFlag::ShowComments)]
            );
        }
        // Case `pointer_hit_testing_stays_inside_switch_rect`.
        {
            let mut switch = switch();
            switch.rect = Rect::new(10.0, 20.0, 56.0, 46.0);
            assert_eq!(
                switch.cursor_shape(Point::new(11.0, 21.0)),
                Some(winit::window::CursorIcon::Pointer)
            );
            assert_eq!(switch.cursor_shape(Point::new(9.0, 21.0)), None);
        }
    }
}
