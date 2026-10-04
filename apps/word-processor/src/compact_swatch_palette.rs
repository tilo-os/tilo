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
use ui::tokens::TextRole;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ColorSwatch, ColorSwatchPicker, ControlSize, Widget,
};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::num;

/// Swatches per row of the grid.
const COLUMNS: usize = 4;

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
    /// The swatch of the colour in use, shown selected.
    pub(super) selected: Option<usize>,
    pub(super) colors: &'a [[u8; 3]],
    pub(super) labels: Vec<String>,
    pub(super) title: String,
    pub(super) clear_label: String,
    pub(super) anchor: Point,
    pub(super) host: Rect,
    /// The theme the panel is laid out in until it is next drawn.
    pub(super) theme: &'a Theme,
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
    clear_hovered: bool,
    anchor: Point,
    host: Rect,
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
            clear_hovered: false,
            anchor: Point::ZERO,
            host: Rect::ZERO,
        }
    }
}

impl CompactSwatchPalette {
    pub(super) fn is_open(&self) -> bool {
        self.picker.is_some()
    }

    pub(super) fn open(&mut self, request: PaletteRequest<'_>) {
        let swatches = request
            .colors
            .iter()
            .zip(request.labels)
            .map(|(rgb, name)| ColorSwatch::named(Color::from_rgb8(rgb[0], rgb[1], rgb[2]), name));
        self.kind = Some(request.kind);
        self.title = request.title;
        self.clear_label = request.clear_label;
        self.clear_focused = false;
        let picker = ColorSwatchPicker::new(swatches, COLUMNS);
        self.picker = Some(match request.selected {
            Some(index) => picker.with_selected(index),
            None => picker,
        });
        if let Some(picker) = self.picker.as_mut() {
            Widget::set_focus(picker, true);
        }
        self.pointer = request.anchor;
        self.anchor = request.anchor;
        self.host = request.host;
        self.layout(request.anchor, request.host, request.theme);
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
            && let Some(rgb) = swatch_rgb(picker, index)
        {
            self.close();
            return Some((kind, PaletteChoice::Color(rgb)));
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

    /// Size and place the panel: a title line, the shared swatch grid and
    /// the clear action, each separated by the panel's sibling rhythm, kept
    /// inside `host` next to `anchor`.
    fn layout(&mut self, anchor: Point, host: Rect, theme: &Theme) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let pad = f64::from(theme.tokens.spacing.s3);
        let gap = f64::from(theme.tokens.spacing.s2);
        let mut taffy = TaffyTree::new();
        let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = picker.build_layout(&mut taffy, theme, &mut nodes);
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::MaxContent,
                height: taffy::AvailableSpace::MaxContent,
            },
        );
        let grid = taffy
            .layout(node)
            .map_or(taffy::Size::ZERO, |layout| layout.size);
        let grid_w = f64::from(grid.width);
        let grid_h = f64::from(grid.height);
        let title_h = f64::from(theme.text(TextRole::Label).line_height_px());
        let clear_h = ButtonFace::height(ControlSize::Small, theme);
        let width = grid_w + 2.0 * pad;
        let height = pad + title_h + gap + grid_h + gap + clear_h + pad;
        let x = anchor
            .x
            .clamp(host.x0 + gap, (host.x1 - width - gap).max(host.x0 + gap));
        let y = anchor
            .y
            .clamp(host.y0 + gap, (host.y1 - height - gap).max(host.y0 + gap));
        self.rect = Rect::new(x, y, x + width, y + height);
        picker.apply_layout(&taffy, node, Point::new(x + pad, y + pad + title_h + gap));
        let clear_y = self.rect.y1 - pad - clear_h;
        self.clear_rect = Rect::new(x + pad, clear_y, x + pad + grid_w, clear_y + clear_h);
    }

    pub(super) fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.layout(self.anchor, self.host, theme);
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        ui::widget::floating_surface::paint(scene, transform, self.rect, theme);
        let ink = theme.on_ground(ui::chrome::panel_ground(theme));
        let pad = f64::from(theme.tokens.spacing.s3);
        text.draw_style(
            scene,
            &self.title,
            ink.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + pad, self.rect.y0 + pad)),
            Some(num::f32_of_f64(self.rect.width() - 2.0 * pad)),
            &theme.text(TextRole::Label),
        );
        let face = ButtonFace::new(self.clear_rect, &self.clear_label)
            .role(ButtonRole::Plain)
            .control_size(ControlSize::Small)
            .hovered(self.clear_hovered)
            .focused(self.clear_focused);
        Button::paint(scene, transform, text, &ink, &face);
        picker.draw(scene, transform, text, &ink, presence);
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
            self.clear_hovered = self.clear_rect.contains(self.pointer);
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
                && let Some(rgb) = swatch_rgb(picker, index)
            {
                self.close();
                return Some((kind, PaletteChoice::Color(rgb)));
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

/// The document colour of swatch `index`, as the RGB triple a mark stores.
fn swatch_rgb(picker: &ColorSwatchPicker, index: usize) -> Option<[u8; 3]> {
    let rgba = picker.swatch(index)?.color.to_rgba8();
    Some([rgba.r, rgba.g, rgba.b])
}
