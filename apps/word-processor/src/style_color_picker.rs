//! Modal colour-picker popover used by the paragraph-style inspector.
//!
//! The editor owns the document mutation; this component owns the popover's
//! lifecycle, geometry, drawing, pointer tracking, and event routing.
//!
//! The shared picker runs in confirm mode: every edit previews live on the
//! focused style, *Done* keeps it, and *Cancel* — or Escape, or a click
//! outside the card — reverts the style to the colour the popover opened
//! with.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::ColorCompat;
use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ColorPicker, ColorPickerOutcome, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::num;

const WIDTH: f64 = 236.0;

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

pub(super) struct EventArgs<'a> {
    pub(super) event: &'a WindowEvent,
    pub(super) scale: f64,
    pub(super) modifiers: Modifiers,
    pub(super) state: &'a mut AutoCommit,
    pub(super) root: &'a ObjId,
    pub(super) host: Rect,
    pub(super) panel: Rect,
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
        self.picker = Some(
            ColorPicker::new(Color::from_rgb8(seed[0], seed[1], seed[2]))
                .confirming(crate::i18n::t("panel-colour")),
        );
        self.pointer = pointer;
    }

    fn height(&self) -> f64 {
        self.picker
            .as_ref()
            .map_or(0.0, |picker| picker.preferred_height(WIDTH))
    }

    fn anchor(host: Rect, panel: Rect, height: f64) -> Point {
        let x = (panel.x0 - WIDTH - 12.0).max(host.x0 + 12.0);
        let y = (panel.y0 + 150.0).min((host.y1 - height - 48.0).max(host.y0 + 60.0));
        Point::new(x, y)
    }

    pub(super) fn layout(&mut self, host: Rect, panel: Rect) {
        let height = self.height();
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
                    height: Dimension::length(num::f32_of_f64(height)),
                },
                ..Default::default()
            },
        );
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(num::f32_of_f64(WIDTH)),
                height: taffy::AvailableSpace::Definite(num::f32_of_f64(height)),
            },
        );
        picker.apply_layout(&taffy, node, Self::anchor(host, panel, height));
    }

    pub(super) fn draw(&mut self, args: DrawArgs, host: Rect, panel: Rect) {
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
        // A floating panel (HIG §9.1 Panel role): the picker sits `s3` inside
        // a panel-ground card with the panel border and elevation, instead of
        // a bar-toned card with a declared border and a literal black shadow.
        let inset = f64::from(theme.tokens.spacing.s3);
        let radius = f64::from(theme.tokens.radii.md);
        let rect = picker.get_rect();
        let card = rect.inflate(inset, inset);
        crate::widget::paint_panel(scene, transform, theme, card, radius);
        picker.draw(scene, transform, text_renderer, theme, presence);
    }

    pub(super) fn handle_event(&mut self, args: EventArgs<'_>) -> PickerEvent {
        let EventArgs {
            event,
            scale,
            modifiers,
            state,
            root,
            host,
            panel,
        } = args;
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
            // A click outside the popover, or Escape, both cancel it the same
            // way: the picker restores the opening colour, which the change
            // record below hands back to the editor.
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
                if let Some(picker) = self.picker.as_mut() {
                    picker.cancel();
                }
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
                    &mut ui::widget::EventCtx {
                        scale_factor: scale,
                        modifiers: &modifiers,
                        state,
                        path: root,
                        handled: &mut handled,
                        focus_target: &mut focused,
                        node_id: node,
                        taffy: &taffy,
                    },
                );
            }
        }

        let changed = self
            .picker
            .as_mut()
            .and_then(ColorPicker::take_changed)
            .map(|color| [color.r(), color.g(), color.b()]);
        // Done keeps the previewed colour; Cancel already reported its
        // revert through `changed`. Either way the popover closes.
        if matches!(
            self.picker.as_mut().and_then(ColorPicker::take_outcome),
            Some(ColorPickerOutcome::Confirmed(_) | ColorPickerOutcome::Cancelled)
        ) {
            self.picker = None;
        }
        PickerEvent {
            consumed: true,
            changed,
        }
    }

    /// One named part of the open picker (`color_picker.sv`,
    /// `color_picker.done`, …), in the picker's own test-target names.
    #[cfg(test)]
    pub(super) fn part(&self, name: &str) -> Option<Rect> {
        self.picker
            .as_ref()?
            .test_rects()
            .into_iter()
            .find(|(part, _)| part == name)
            .map(|(_, rect)| rect)
    }
}
