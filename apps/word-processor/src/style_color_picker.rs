//! The paragraph-style inspector's colour popover, in confirm mode: an edit previews
//! live on the focused style, Done keeps it, and Cancel or Escape reverts it. The
//! editor owns the document mutation.

use std::collections::HashMap;

use automerge::ObjId;
use taffy::{NodeId, Size, Style, TaffyTree};
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
use ui::widget::DrawChild;

const WIDTH: f64 = 236.0;

pub(super) struct PickerEvent {
    pub(super) consumed: bool,
    pub(super) changed: Option<[u8; 3]>,
}

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
    pub(super) state: ui::widget::Edits<'a>,
    pub(super) root: &'a ObjId,
    pub(super) host: Rect,
    pub(super) panel: Rect,
}

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
        let gutter = f64::from(ui::tokens::Spacing::BASE.s6);
        let x = (panel.x0 - WIDTH - gutter).max(host.x0 + gutter);
        let y = (panel.y0 + gutter).min((host.y1 - height - gutter).max(host.y0 + gutter));
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
                size: Size::from_lengths(num::f32_of_f64(WIDTH), num::f32_of_f64(height)),
                ..Default::default()
            },
        );
        let _ = ui::widget::solve_layout(
            &mut taffy,
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
        let inset = f64::from(theme.tokens.spacing.s3);
        let rect = picker.get_rect();
        let card = rect.inflate(inset, inset);
        ui::widget::floating_surface::paint(scene, transform, card, theme);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        picker.draw_child(scene, transform, text_renderer, &ink, presence);
    }

    pub(super) fn handle_event(&mut self, args: EventArgs<'_>) -> PickerEvent {
        let EventArgs {
            event,
            scale,
            modifiers,
            mut state,
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
                        state: state.reborrow(),
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
