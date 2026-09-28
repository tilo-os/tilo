//! Compact Writer palette popover built around the shared swatch-grid widget.
//!
//! This owns only local presentation state and returns a typed choice; the
//! editor remains the single place that turns that choice into a document action.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use ui::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{ColorSwatchPicker, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::num;

const WIDTH: f64 = 190.0;
const HEADER_H: f64 = 31.0;
const FOOTER_H: f64 = 29.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteKind {
    Text,
    Highlight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PaletteChoice {
    Color([u8; 3]),
    Clear,
}

pub(super) struct PaletteRequest<'a> {
    pub(super) kind: PaletteKind,
    pub(super) colors: &'a [[u8; 3]],
    pub(super) labels: Vec<String>,
    pub(super) title: String,
    pub(super) clear_label: String,
    pub(super) anchor: Point,
    pub(super) host: Rect,
}

pub(super) struct CompactSwatchPalette {
    kind: Option<PaletteKind>,
    picker: Option<ColorSwatchPicker>,
    rect: Rect,
    clear_rect: Rect,
    pointer: Point,
    title: String,
    clear_label: String,
    clear_focused: bool,
}

impl Default for CompactSwatchPalette {
    fn default() -> Self {
        Self {
            kind: None,
            picker: None,
            rect: Rect::ZERO,
            clear_rect: Rect::ZERO,
            pointer: Point::ZERO,
            title: String::new(),
            clear_label: String::new(),
            clear_focused: false,
        }
    }
}

impl CompactSwatchPalette {
    pub(super) fn is_open(&self) -> bool {
        self.picker.is_some()
    }

    pub(super) fn open(&mut self, request: PaletteRequest<'_>) {
        let colors = request
            .colors
            .iter()
            .map(|rgb| u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2]))
            .collect();
        self.kind = Some(request.kind);
        self.title = request.title;
        self.clear_label = request.clear_label;
        self.clear_focused = false;
        self.picker =
            Some(ColorSwatchPicker::with_palette(colors, 4).with_accessible_labels(request.labels));
        if let Some(picker) = self.picker.as_mut() {
            Widget::set_focus(picker, true);
        }
        self.pointer = request.anchor;
        self.layout(request.anchor, request.host);
    }

    pub(super) fn close(&mut self) {
        self.kind = None;
        self.picker = None;
        self.rect = Rect::ZERO;
        self.clear_rect = Rect::ZERO;
    }

    pub(super) fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.picker
            .as_ref()
            .and_then(Widget::accessibility)
            .map(|picker| {
                use ui::a11y::{AccessNode, AccessState, Role};
                // The popover is a named group around the reusable list and
                // its Writer-specific clear/default footer. Keeping the
                // picker as its own child preserves its collection semantics.
                AccessNode::new(Role::Group)
                    .with_name(self.title.clone())
                    .with_bounds(self.rect)
                    .with_children(vec![
                        picker,
                        AccessNode::new(Role::Button)
                            .with_name(self.clear_label.clone())
                            .with_bounds(self.clear_rect)
                            .with_action_target("compact-swatch:clear")
                            .with_state(AccessState {
                                focusable: true,
                                focused: self.clear_focused,
                                ..Default::default()
                            }),
                    ])
            })
    }

    pub(super) fn semantic_choice(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Option<(PaletteKind, PaletteChoice)> {
        let kind = self.kind?;
        if target == "compact-swatch:clear" && matches!(action, ui::a11y::Action::Activate) {
            self.close();
            return Some((kind, PaletteChoice::Clear));
        }
        let picker = self.picker.as_mut()?;
        if Widget::perform_semantic_action(picker, target, action, state, root)
            && let Some(index) = picker.take_choice()
            && let Some(rgb) = picker.color_at(index)
        {
            self.close();
            return Some((
                kind,
                PaletteChoice::Color([
                    ((rgb >> 16) & 0xff) as u8,
                    ((rgb >> 8) & 0xff) as u8,
                    (rgb & 0xff) as u8,
                ]),
            ));
        }
        None
    }

    /// Give a palette descendant real semantic focus before the editor's
    /// general document-focus fallback runs. This keeps AT, keyboard, and
    /// pointer focus state aligned without treating a swatch as a mutation.
    pub(super) fn focus_target(
        &mut self,
        target: &str,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> bool {
        if target == "compact-swatch:clear" && self.kind.is_some() {
            self.clear_focused = true;
            if let Some(picker) = self.picker.as_mut() {
                Widget::set_focus(picker, false);
            }
            return true;
        }
        let Some(picker) = self.picker.as_mut() else {
            return false;
        };
        if Widget::perform_semantic_action(picker, target, &ui::a11y::Action::Focus, state, root) {
            self.clear_focused = false;
            return true;
        }
        false
    }

    fn layout(&mut self, anchor: Point, host: Rect) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let theme = Theme::dark();
        let mut taffy = TaffyTree::new();
        let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = picker.build_layout(&mut taffy, &theme, &mut nodes);
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(num::f32_of_f64(WIDTH - 16.0)),
                height: taffy::AvailableSpace::MaxContent,
            },
        );
        let grid_h = taffy
            .layout(node)
            .map_or(0.0, |layout| f64::from(layout.size.height));
        // Keep a visual gutter between the shared grid's own card and the
        // explicit clear action below it.
        let height = HEADER_H + grid_h + FOOTER_H + 8.0;
        let x = anchor
            .x
            .clamp(host.x0 + 8.0, (host.x1 - WIDTH - 8.0).max(host.x0 + 8.0));
        let y = anchor
            .y
            .clamp(host.y0 + 8.0, (host.y1 - height - 40.0).max(host.y0 + 8.0));
        self.rect = Rect::new(x, y, x + WIDTH, y + height);
        picker.apply_layout(&taffy, node, Point::new(x + 8.0, y + HEADER_H));
        self.clear_rect = Rect::new(
            x + 8.0,
            self.rect.y1 - FOOTER_H + 3.0,
            x + WIDTH - 8.0,
            self.rect.y1 - 4.0,
        );
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        // A floating panel: panel ground + panel border + elevation (HIG
        // §9.1), never a raised fill with the declared `border_standard`.
        let radius = f64::from(theme.tokens.radii.md);
        crate::widget::paint_panel(scene, transform, theme, self.rect, radius);
        text.draw_text(
            scene,
            &self.title,
            12.5,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + 10.0, self.rect.y0 + 8.0)),
            None,
        );
        let clear = vello::kurbo::RoundedRect::from_rect(self.clear_rect, 5.0);
        ui::surface::card(
            scene,
            transform,
            self.clear_rect,
            5.0,
            theme.tokens.colors.surfaces.base,
        );
        if self.clear_focused {
            scene.stroke(
                &vello::kurbo::Stroke::new(2.0),
                transform,
                theme.tokens.colors.border_focus,
                None,
                &clear,
            );
        }
        text.draw_text(
            scene,
            &self.clear_label,
            11.5,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.clear_rect.x0 + 8.0, self.clear_rect.y0 + 3.5)),
            Some(num::f32_of_f64(self.clear_rect.width() - 16.0)),
        );
        picker.draw(scene, transform, text, theme, presence);
    }

    pub(super) fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        root: &ObjId,
    ) -> Option<(PaletteKind, PaletteChoice)> {
        let kind = self.kind?;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale, position.y / scale);
        }
        if matches!(
            event,
            WindowEvent::KeyboardInput {
                event: KeyEvent {
                    logical_key: Key::Named(NamedKey::Escape),
                    state: ElementState::Pressed,
                    ..
                },
                ..
            }
        ) {
            self.close();
            return None;
        }
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            if matches!(logical_key, Key::Named(NamedKey::Tab)) {
                self.clear_focused = !self.clear_focused;
                if let Some(picker) = self.picker.as_mut() {
                    Widget::set_focus(picker, !self.clear_focused);
                }
                return None;
            }
            if self.clear_focused
                && matches!(logical_key, Key::Named(NamedKey::Enter | NamedKey::Space))
            {
                self.close();
                return Some((kind, PaletteChoice::Clear));
            }
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && self.clear_rect.contains(self.pointer)
        {
            self.close();
            return Some((kind, PaletteChoice::Clear));
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) && !self.rect.contains(self.pointer)
        {
            self.close();
            return None;
        }
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::new();
        let node = NodeId::new(0);
        if let Some(picker) = self.picker.as_mut() {
            picker.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: scale,
                    modifiers: &modifiers,
                    state,
                    path: root,
                    handled: &mut handled,
                    focus_target: &mut focus,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            if let Some(index) = picker.take_choice()
                && let Some(rgb) = picker.color_at(index)
            {
                self.close();
                return Some((
                    kind,
                    PaletteChoice::Color([
                        ((rgb >> 16) & 0xff) as u8,
                        ((rgb >> 8) & 0xff) as u8,
                        (rgb & 0xff) as u8,
                    ]),
                ));
            }
        }
        None
    }

    #[cfg(test)]
    pub(super) fn rect(&self) -> Rect {
        self.rect
    }

    #[cfg(test)]
    pub(super) fn swatch_rect(&self, index: usize) -> Option<Rect> {
        self.picker
            .as_ref()
            .and_then(|picker| picker.synthetic_accessibility_rect(&[index]))
    }

    #[cfg(test)]
    pub(super) fn clear_rect(&self) -> Rect {
        self.clear_rect
    }
}
