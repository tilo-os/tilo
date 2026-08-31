//! Modal colour-picker popover used by the paragraph-style inspector.
//!
//! The editor owns the document mutation; this component owns the popover's
//! lifecycle, geometry, drawing, pointer tracking, and event routing.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::ColorCompat;
use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ColorPicker, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::num;

const WIDTH: f64 = 236.0;
const HEIGHT: f64 = 176.0;

/// Result of routing an event through an open [`StyleColorPicker`].
pub(super) struct PickerEvent {
    pub(super) consumed: bool,
    pub(super) changed: Option<[u8; 3]>,
}

/// The shared per-frame draw arguments for [`StyleColorPicker::draw`],
/// bundled the same way `ui`'s widget `*DrawCtx` structs are so the method
/// stays under clippy's argument-count limit.
pub(super) struct DrawArgs<'a> {
    pub(super) scene: &'a mut Scene,
    pub(super) transform: Affine,
    pub(super) text_renderer: &'a mut TextRenderer,
    pub(super) theme: &'a Theme,
    pub(super) presence: &'a PresenceCtx,
}

/// A shared [`ColorPicker`] hosted in a modal-ish floating card.
#[derive(Default)]
pub(super) struct StyleColorPicker {
    picker: Option<ColorPicker>,
    pointer: Point,
}

impl StyleColorPicker {
    pub(super) fn is_open(&self) -> bool {
        self.picker.is_some()
    }

    pub(super) fn open(&mut self, seed: [u8; 3], pointer: Point) {
        self.picker = Some(ColorPicker::new(Color::from_rgb8(
            seed[0], seed[1], seed[2],
        )));
        self.pointer = pointer;
    }

    fn anchor(host: Rect, panel: Rect) -> Point {
        let x = (panel.x0 - WIDTH - 12.0).max(host.x0 + 12.0);
        let y = (panel.y0 + 150.0).min((host.y1 - HEIGHT - 48.0).max(host.y0 + 60.0));
        Point::new(x, y)
    }

    pub(super) fn layout(&mut self, host: Rect, panel: Rect) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = picker.build_layout(&mut taffy, &theme, &mut node_map);
        let _ = taffy.set_style(
            node,
            Style {
                size: Size {
                    width: Dimension::length(num::f32_of_f64(WIDTH)),
                    height: Dimension::length(num::f32_of_f64(HEIGHT)),
                },
                ..Default::default()
            },
        );
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(num::f32_of_f64(WIDTH)),
                height: taffy::AvailableSpace::Definite(num::f32_of_f64(HEIGHT)),
            },
        );
        picker.apply_layout(&taffy, node, Self::anchor(host, panel));
    }

    pub(super) fn draw(
        &mut self,
        args: DrawArgs,
        host: Rect,
        panel: Rect,
        surface: Color,
        border: Color,
    ) {
        if !self.is_open() {
            return;
        }
        self.layout(host, panel);
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let DrawArgs {
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        } = args;
        let rect = picker.get_rect();
        let card = Rect::new(
            rect.x0 - 10.0,
            rect.y0 - 10.0,
            rect.x1 + 10.0,
            rect.y1 + 10.0,
        );
        let shape = vello::kurbo::RoundedRect::from_rect(card, 8.0);
        scene.fill(
            Fill::NonZero,
            transform,
            Color::from_rgba8(Color::BLACK.r(), Color::BLACK.g(), Color::BLACK.b(), 0x30),
            None,
            &vello::kurbo::RoundedRect::from_rect(
                Rect::new(card.x0, card.y0 + 3.0, card.x1, card.y1 + 3.0),
                8.0,
            ),
        );
        scene.fill(Fill::NonZero, transform, surface, None, &shape);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            transform,
            border,
            None,
            &shape,
        );
        picker.draw(scene, transform, text_renderer, theme, presence);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
        host: Rect,
        panel: Rect,
    ) -> PickerEvent {
        if !self.is_open() {
            return PickerEvent {
                consumed: false,
                changed: None,
            };
        }
        self.layout(host, panel);
        let rect = self.picker.as_ref().map_or(Rect::ZERO, Widget::get_rect);

        let forward = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = Point::new(position.x / scale, position.y / scale);
                true
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if rect.contains(self.pointer) => true,
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => true,
            // A click outside the popover, or Escape, both dismiss it the
            // same way — no forwarded event, just a close.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
            | WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: Key::Named(NamedKey::Escape),
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.picker = None;
                false
            }
            _ => false,
        };

        if forward {
            let taffy: TaffyTree<()> = TaffyTree::new();
            let node = NodeId::new(0);
            let mut focused: Option<NodeId> = None;
            let mut handled = false;
            if let Some(picker) = self.picker.as_mut() {
                picker.handle_event(
                    event,
                    scale,
                    &modifiers,
                    state,
                    root,
                    &mut handled,
                    &mut focused,
                    node,
                    &taffy,
                );
            }
        }

        let changed = self
            .picker
            .as_mut()
            .and_then(ColorPicker::take_changed)
            .map(|color| [color.r(), color.g(), color.b()]);
        PickerEvent {
            consumed: true,
            changed,
        }
    }

    #[cfg(test)]
    pub(super) fn rect(&self) -> Rect {
        self.picker.as_ref().map_or(Rect::ZERO, Widget::get_rect)
    }
}
