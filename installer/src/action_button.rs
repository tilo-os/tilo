//! [`ActionButton`] — the installer's primary/secondary call-to-action.
//!
//! `ui::Button` is fixed at 120×40 px and always paints the same neutral
//! "raised surface" fill, so it can't express a wizard's visual hierarchy
//! (a prominent accent "Next"/"Begin" vs. a quieter "Back") or fill a nav
//! row. `ActionButton` is a leaf widget that:
//!
//!   * grows to share its row's width (so a lone button fills the row and
//!     a Back/Next pair splits it evenly),
//!   * styles itself **primary** (accent fill, white label) or
//!     **secondary** (raised fill, neutral label),
//!   * supports a **disabled** state (dimmed, non-interactive, dropped
//!     from the focus order),
//!   * fires a CRDT boolean flag on click / Space — the rising-edge
//!     pattern the [`crate::installer_app::InstallerApp`] consumes, exactly
//!     like `ui::Button`'s `on_click_path`.
//!
//! Every colour is read from the [`Theme`]; nothing is hard-coded.

use std::collections::HashMap;

use automerge::{transaction::Transactable, AutoCommit, ObjId};
use taffy::style_helpers::length;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;

const HEIGHT: f32 = 44.0;
const LABEL_SIZE: f32 = 15.0;

pub struct ActionButton {
    label: String,
    /// CRDT path of the boolean flag raised when the button fires.
    click_path: String,
    primary: bool,
    disabled: bool,
    rect: Rect,
    is_hovered: bool,
    is_pressed: bool,
    is_focused: bool,
}

impl ActionButton {
    /// A secondary (neutral) button firing `click_path` on activation.
    pub fn new(label: &str, click_path: &str) -> Self {
        Self {
            label: label.to_string(),
            click_path: click_path.to_string(),
            primary: false,
            disabled: false,
            rect: Rect::ZERO,
            is_hovered: false,
            is_pressed: false,
            is_focused: false,
        }
    }

    /// Promote to the accent-filled primary call-to-action.
    pub fn primary(mut self) -> Self {
        self.primary = true;
        self
    }

    /// Mark disabled: dimmed, non-interactive, and removed from the focus
    /// order so keyboard nav skips it.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn fire(&self, state: &mut AutoCommit, root: &ObjId) {
        if self.disabled {
            return;
        }
        let _ = state.put(root, self.click_path.as_str(), true);
    }
}

impl Widget for ActionButton {
    fn sync(&mut self, _state: &mut AutoCommit, _root: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::auto(),
                    height: Dimension::length(HEIGHT),
                },
                // Grow to share the nav row's width; basis 0 so two
                // buttons split evenly and a lone button fills the row.
                flex_grow: 1.0,
                flex_basis: length(0.0),
                flex_shrink: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let shape = RoundedRect::from_rect(self.rect, theme.radius_standard);

        // Fill + label colour by variant and interaction state.
        let (bg, label_color) = if self.disabled {
            (theme.bg_primary, theme.text_dim)
        } else if self.primary {
            let bg = if self.is_pressed {
                theme.tokens.colors.accent[3]
            } else if self.is_hovered {
                theme.accent_hover
            } else {
                theme.accent_primary
            };
            (bg, Color::WHITE)
        } else {
            let bg = if self.is_pressed {
                theme.bg_active
            } else if self.is_hovered {
                theme.bg_hover
            } else {
                theme.bg_primary
            };
            (bg, theme.text_primary)
        };
        scene.fill(Fill::NonZero, transform, bg, None, &shape);

        // Border: focus ring wins; secondary buttons keep a hairline.
        if self.is_focused && !self.disabled {
            scene.stroke(&Stroke::new(2.0), transform, theme.border_focus, None, &shape);
        } else if !self.primary {
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                theme.border_standard,
                None,
                &shape,
            );
        }

        let (tw, th) = text_renderer.measure(&self.label, LABEL_SIZE, None);
        let tx = self.rect.center().x - tw as f64 / 2.0;
        let ty = self.rect.y0 + (self.rect.height() - th as f64) / 2.0;
        text_renderer.draw_text(
            scene,
            &self.label,
            LABEL_SIZE,
            label_color,
            transform * Affine::translate((tx, ty)),
            None,
        );
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if self.disabled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                let hover = self.rect.contains(logical);
                if hover != self.is_hovered {
                    self.is_hovered = hover;
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: element_state,
                button: MouseButton::Left,
                ..
            } => {
                if self.is_hovered {
                    let pressed = *element_state == ElementState::Pressed;
                    if pressed != self.is_pressed {
                        self.is_pressed = pressed;
                        *handled = true;
                        if pressed {
                            *focus_target = Some(node_id);
                        } else {
                            // Fire on release (matches ui::Button).
                            self.fire(state, root);
                        }
                    }
                } else if self.is_pressed {
                    self.is_pressed = false;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Space),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                self.fire(state, root);
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "ActionButton"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.label.as_str())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        !self.disabled
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }
}
