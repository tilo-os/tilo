//! Colour swatches: one named swatch type, one painter, one picker.
//!
//! * [`ColorSwatch`] is a colour with an optional name; a swatch without a
//!   name is spoken and shown as its `#RRGGBB` hex.
//! * [`ColorSwatchFace`] paints a swatch and answers its hit-test, tooltip and
//!   radio-style accessibility, for dense toolbars and custom canvases that
//!   keep their own layout.
//! * [`ColorSwatchPicker`] is a grid of swatches as a widget. Clicking a
//!   swatch records its index; the host reads it with
//!   [`ColorSwatchPicker::take_choice`] and writes the colour to its model.
//!   [`super::ColorPicker::with_swatches`] uses the same swatches as quick
//!   picks beside its full colour field.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;
use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::a11y::{AccessNode, AccessState, Role};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{TaffyTreeExt, Widget};

/// One colour a person can pick.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSwatch {
    pub color: Color,
    /// Spoken and tooltip name; `None` uses the `#RRGGBB` hex.
    pub name: Option<String>,
}

impl ColorSwatch {
    #[must_use]
    pub fn new(color: Color) -> Self {
        Self { color, name: None }
    }

    #[must_use]
    pub fn named(color: Color, name: impl Into<String>) -> Self {
        Self {
            color,
            name: Some(name.into()),
        }
    }

    /// The name a screen reader speaks and the tooltip shows.
    pub fn spoken_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| crate::color::to_hex(self.color))
    }
}

/// An immediate-mode swatch: paint, hit-test, tooltip and accessibility from
/// one declaration.
pub struct ColorSwatchFace {
    rect: Rect,
    target_rect: Rect,
    color: Color,
    name: String,
    marker: Option<String>,
    selected: bool,
    hovered: bool,
    focused: bool,
}

impl ColorSwatchFace {
    pub fn new(rect: Rect, color: Color, name: impl Into<String>) -> Self {
        Self {
            rect,
            target_rect: rect,
            color,
            name: name.into(),
            marker: None,
            selected: false,
            hovered: false,
            focused: false,
        }
    }

    /// The face of a [`ColorSwatch`].
    pub fn of(rect: Rect, swatch: &ColorSwatch) -> Self {
        Self::new(rect, swatch.color, swatch.spoken_name())
    }

    /// A short mark painted on the swatch (a letter for "automatic").
    #[must_use]
    pub fn marker(mut self, marker: impl Into<String>) -> Self {
        self.marker = Some(marker.into());
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    #[must_use]
    pub fn hovered(mut self, hovered: bool) -> Self {
        self.hovered = hovered;
        self
    }

    /// Keyboard focus: the ring in your colour.
    #[must_use]
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// A pointer target larger than the painted swatch.
    #[must_use]
    pub fn target_rect(mut self, target_rect: Rect) -> Self {
        self.target_rect = target_rect;
        self
    }

    pub fn hit_test(&self, point: Point) -> bool {
        self.target_rect.contains(point)
    }

    pub fn tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        self.hit_test(point).then(|| (self.rect, self.name.clone()))
    }

    pub fn accessibility(&self, action_target: impl Into<String>) -> AccessNode {
        AccessNode::new(Role::RadioButton)
            .with_name(self.name.clone())
            .with_action_target(action_target)
            .with_bounds(self.target_rect)
            .with_state(AccessState::new().focusable(true).checked(self.selected))
    }

    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.item);
        let shape = RoundedRect::from_rect(self.rect, radius);
        scene.fill(Fill::NonZero, transform, self.color, None, &shape);
        let outline = crate::surface::edge_outline(self.rect, radius);
        // The chosen swatch and keyboard focus are rings in your colour; a
        // hovered one a ring that reads on its own fill; at rest an edge
        // derived from the fill (HIG §9.5), so a white swatch on paper or a
        // black one on a dark panel keeps its outline.
        let (width, ring) = if self.selected || self.focused {
            (2.0, colors.intent.you)
        } else if self.hovered {
            (
                1.5,
                colors.ink_on_any(&[self.color], colors.text_primary, 3.0),
            )
        } else {
            (1.0, crate::surface::edge_on(self.color))
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(width),
            transform,
            ring,
            None,
            &outline,
        );
        if let Some(marker) = &self.marker {
            let style = theme.text(TextRole::Caption).strong();
            let ink = colors.ink_on_any(&[self.color], colors.text_primary, 4.5);
            let (width, height) = text.measure_style(marker, None, &style);
            text.draw_style(
                scene,
                marker,
                ink,
                transform.pre_translate(vello::kurbo::Vec2::new(
                    self.rect.center().x - f64::from(width) / 2.0,
                    self.rect.center().y - f64::from(height) / 2.0,
                )),
                None,
                &style,
            );
        }
    }
}

/// A grid of [`ColorSwatch`]es as a widget.
pub struct ColorSwatchPicker {
    swatches: Vec<ColorSwatch>,
    columns: usize,
    selected: Option<usize>,
    choice: Option<usize>,
    rect: Rect,
    swatch_rects: Vec<Rect>,
    /// Edge of a swatch cell and the grid's padding, from the theme seen in
    /// `build_layout`.
    cell: f64,
    pad: f64,
    last_cursor: Point,
    hovered: Option<usize>,
    focused: bool,
    focus_index: usize,
}

impl ColorSwatchPicker {
    /// `columns` swatches per row (at least one).
    pub fn new(swatches: impl IntoIterator<Item = ColorSwatch>, columns: usize) -> Self {
        let metrics = crate::tokens::Metrics::BASE;
        Self {
            swatches: swatches.into_iter().collect(),
            columns: columns.max(1),
            selected: None,
            choice: None,
            rect: Rect::ZERO,
            swatch_rects: Vec::new(),
            cell: f64::from(metrics.min_pointer_target),
            pad: f64::from(crate::tokens::Spacing::BASE.s1),
            last_cursor: Point::ZERO,
            hovered: None,
            focused: false,
            focus_index: 0,
        }
    }

    #[must_use]
    pub fn with_selected(mut self, idx: usize) -> Self {
        self.selected = Some(idx);
        self
    }

    pub fn take_choice(&mut self) -> Option<usize> {
        self.choice.take()
    }

    pub fn swatch(&self, idx: usize) -> Option<&ColorSwatch> {
        self.swatches.get(idx)
    }

    fn rows(&self) -> usize {
        self.swatches.len().div_ceil(self.columns)
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "row/column indices are bounded by the on-screen swatch \
                  grid, far below f64's exact-integer range"
    )]
    fn recompute_swatches(&mut self) {
        self.swatch_rects.clear();
        // Each swatch fills its pointer-target cell less a gap, so cells
        // touch and the whole grid is clickable.
        let gap = self.pad;
        for i in 0..self.swatches.len() {
            let r = i.checked_div(self.columns).unwrap_or(0) as f64;
            let c = i.checked_rem(self.columns).unwrap_or(0) as f64;
            let x = self.rect.x0 + self.pad + c * self.cell;
            let y = self.rect.y0 + self.pad + r * self.cell;
            self.swatch_rects.push(Rect::new(
                x + gap / 2.0,
                y + gap / 2.0,
                x + self.cell - gap / 2.0,
                y + self.cell - gap / 2.0,
            ));
        }
    }

    /// `(width, height)` of the grid — shared by `build_layout` and
    /// `measure` so they can't disagree.
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        reason = "column/row counts describe a small, on-screen swatch grid, \
                  far below f32's exact-integer range"
    )]
    fn grid_size(&self) -> (f32, f32) {
        let w = self.columns as f64 * self.cell + self.pad * 2.0;
        let h = self.rows() as f64 * self.cell + self.pad * 2.0;
        (w as f32, h as f32)
    }

    fn choose(&mut self, index: usize) {
        self.selected = Some(index);
        self.choice = Some(index);
    }
}

impl Widget for ColorSwatchPicker {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.cell = f64::from(theme.tokens.metrics.min_pointer_target);
        self.pad = f64::from(theme.tokens.spacing.s1);
        let (w, h) = self.grid_size();
        let node = taffy.leaf_node(Style {
            flex_shrink: 0.0,
            size: taffy::Size {
                width: length(w),
                height: length(h),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _k: taffy::Size<Option<f32>>,
        _a: taffy::Size<taffy::AvailableSpace>,
        _tr: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let (width, height) = self.grid_size();
        taffy::Size { width, height }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.recompute_swatches();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        for (i, (swatch, rect)) in self.swatches.iter().zip(&self.swatch_rects).enumerate() {
            ColorSwatchFace::of(*rect, swatch)
                .selected(self.selected == Some(i))
                .focused(self.focused && self.focus_index == i)
                .hovered(self.hovered == Some(i))
                .paint(scene, transform, text_renderer, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hovered = self
                    .swatch_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor));
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(i) = self
                    .swatch_rects
                    .iter()
                    .position(|r| r.contains(self.last_cursor))
                {
                    self.choose(i);
                    self.focused = true;
                    self.focus_index = i;
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    winit::event::KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.focused && !self.swatches.is_empty() => {
                let last = self.swatches.len().saturating_sub(1);
                let next = match logical_key {
                    Key::Named(NamedKey::ArrowLeft) => self.focus_index.saturating_sub(1),
                    Key::Named(NamedKey::ArrowRight) => {
                        self.focus_index.saturating_add(1).min(last)
                    }
                    Key::Named(NamedKey::ArrowUp) => self.focus_index.saturating_sub(self.columns),
                    Key::Named(NamedKey::ArrowDown) => {
                        self.focus_index.saturating_add(self.columns).min(last)
                    }
                    Key::Named(NamedKey::Home) => 0,
                    Key::Named(NamedKey::End) => last,
                    Key::Named(NamedKey::Enter | NamedKey::Space) => {
                        self.choose(self.focus_index);
                        *ctx.handled = true;
                        return;
                    }
                    _ => return,
                };
                self.focus_index = next;
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ColorSwatchPicker"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Each swatch names itself on hover.
    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.swatches
            .iter()
            .zip(&self.swatch_rects)
            .find_map(|(swatch, rect)| ColorSwatchFace::of(*rect, swatch).tooltip_target(point))
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.focus_index = self
                .selected
                .unwrap_or(0)
                .min(self.swatches.len().saturating_sub(1));
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn accessibility(&self) -> Option<AccessNode> {
        // Each swatch is drawn (not a child widget), so it becomes a
        // synthetic `ListItem` named like its tooltip.
        let count = self.swatches.len();
        let value = self
            .selected
            .and_then(|i| self.swatches.get(i))
            .map(ColorSwatch::spoken_name);
        let items: Vec<AccessNode> = self
            .swatches
            .iter()
            .enumerate()
            .map(|(i, swatch)| {
                AccessNode::new(Role::ListItem)
                    .with_action_target(format!("swatch:{i}"))
                    .with_name(swatch.spoken_name())
                    // The cells are custom-painted synthetic descendants, so
                    // export their real logical-pixel rects for AT hit testing
                    // and overlay inspection just like the shared pointer path.
                    .with_bounds(self.swatch_rects.get(i).copied().unwrap_or(Rect::ZERO))
                    .with_state(AccessState {
                        focusable: true,
                        selected: self.selected == Some(i),
                        focused: self.focused && self.focus_index == i,
                        ..Default::default()
                    })
                    .with_position(i.saturating_add(1), count)
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_optional_value(value)
                .with_state(AccessState {
                    focusable: self.is_focusable(),
                    focused: self.is_focused(),
                    ..Default::default()
                })
                .with_children(items),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [swatch] = path else {
            return None;
        };
        self.swatch_rects.get(*swatch).copied()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        let Some(index) = target
            .strip_prefix("swatch:")
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|index| *index < self.swatches.len())
        else {
            return false;
        };
        if matches!(action, crate::a11y::Action::Focus) {
            self.focused = true;
            self.focus_index = index;
            return true;
        }
        if !matches!(action, crate::a11y::Action::Activate) {
            return false;
        }
        self.choose(index);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::from_rgb8(r, g, b)
    }

    fn palette() -> Vec<ColorSwatch> {
        vec![
            ColorSwatch::named(rgb(0x11, 0x22, 0x33), "Ink"),
            ColorSwatch::new(rgb(0x44, 0x55, 0x66)),
            ColorSwatch::named(rgb(0x77, 0x88, 0x99), "Slate"),
        ]
    }

    fn lay_out(w: &mut dyn Widget) {
        let theme = Theme::dark();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = w.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(300.0),
                    height: taffy::AvailableSpace::Definite(300.0),
                },
            )
            .unwrap();
        w.apply_layout(&taffy, node, Point::ZERO);
    }

    fn dispatch(w: &mut dyn Widget, e: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.leaf_node(Style::default());
        let mut handled = false;
        let mut ft = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        w.handle_event(
            e,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    fn named_press(key: NamedKey) -> WindowEvent {
        use winit::keyboard::{NativeKeyCode, PhysicalKey};
        let mut event: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        event.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        event.logical_key = Key::Named(key);
        event.text = None;
        event.location = winit::keyboard::KeyLocation::Standard;
        event.state = ElementState::Pressed;
        event.repeat = false;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event,
            is_synthetic: false,
        }
    }

    #[test]
    fn one_face_drives_hit_help_and_checked_semantics() {
        let visual = Rect::new(10.0, 10.0, 30.0, 30.0);
        let target = Rect::new(6.0, 4.0, 34.0, 36.0);
        let face = ColorSwatchFace::new(visual, rgb(0xff, 0xff, 0xff), "Text color #FFFFFF")
            .selected(true)
            .target_rect(target);
        assert!(face.hit_test(Point::new(7.0, 20.0)));
        assert_eq!(
            face.tooltip_target(Point::new(7.0, 20.0)),
            Some((visual, "Text color #FFFFFF".to_owned())),
        );
        let node = face.accessibility("text-color:ffffff");
        assert_eq!(node.role, Role::RadioButton);
        assert_eq!(node.state.checked, Some(true));
        assert_eq!(node.bounds.expect("target bounds").to_rect(), target);
    }

    #[test]
    fn the_picker_names_each_swatch_and_chooses_by_pointer_keyboard_and_semantics() {
        let mut picker = ColorSwatchPicker::new(palette(), 2);
        lay_out(&mut picker);
        let tree = Widget::accessibility(&picker).expect("picker semantics");
        let names: Vec<_> = tree.children.iter().map(|c| c.name.clone()).collect();
        assert_eq!(
            names,
            [
                Some("Ink".to_owned()),
                Some("#445566".to_owned()),
                Some("Slate".to_owned())
            ],
            "a nameless swatch is spoken as its hex"
        );
        let centre = picker.swatch_rects[2].center();
        assert_eq!(
            Widget::tooltip_target_at(&picker, centre).map(|(_, name)| name),
            Some("Slate".to_owned())
        );

        dispatch(
            &mut picker,
            &WindowEvent::CursorMoved {
                device_id: unsafe { std::mem::zeroed() },
                position: winit::dpi::PhysicalPosition::new(centre.x, centre.y),
            },
        );
        assert!(dispatch(
            &mut picker,
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            }
        ));
        assert_eq!(picker.take_choice(), Some(2));

        Widget::set_focus(&mut picker, true);
        assert!(dispatch(&mut picker, &named_press(NamedKey::Home)));
        assert!(dispatch(&mut picker, &named_press(NamedKey::ArrowRight)));
        assert!(dispatch(&mut picker, &named_press(NamedKey::Space)));
        assert_eq!(picker.take_choice(), Some(1));

        assert!(Widget::perform_semantic_action(
            &mut picker,
            "swatch:0",
            &crate::a11y::Action::Activate,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert_eq!(picker.take_choice(), Some(0));
        assert_eq!(
            picker.swatch(0).map(|s| s.color),
            Some(rgb(0x11, 0x22, 0x33))
        );
        assert!(
            Widget::accessibility(&picker).unwrap().children[0]
                .state
                .selected
        );
    }

    #[test]
    fn swatch_cells_are_pointer_targets() {
        let mut picker = ColorSwatchPicker::new(palette(), 2);
        lay_out(&mut picker);
        let target = f64::from(Theme::dark().tokens.metrics.min_pointer_target);
        let pitch = picker.swatch_rects[1].x0 - picker.swatch_rects[0].x0;
        assert_eq!(pitch, target, "each swatch owns a full pointer-target cell");
        assert!(picker.swatch_rects[2].y0 > picker.swatch_rects[0].y0);
    }
}
