//! [`QuickTile`]: a Quick Settings control. Its glyph, name and state line
//! sit on a tile that is filled with the on colour while the control is on;
//! a press anywhere on it switches the control. A tile whose control has a
//! section of its own (the networks under Wi-Fi) gives that section a
//! chevron half, set apart by a hairline, that turns as the section opens.
//!
//! The tile reports what the person did as a [`QuickTileOutcome`]
//! ([`QuickTile::take_outcome`]); the host switches the control or opens
//! its section and hands the new state back.
//!
//! ```ignore
//! let mut tile = QuickTile::new("quick_settings_network", Icon::Wifi, "Wi-Fi")
//!     .detail("Home")
//!     .on(true)
//!     .with_details("Network details");
//! // each frame:
//! match tile.take_outcome() {
//!     Some(QuickTileOutcome::Switch) => wifi.toggle(),
//!     Some(QuickTileOutcome::Details) => open_networks(),
//!     None => {}
//! }
//! ```

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::{ChromeRole, SelectionState, selection_tier};
use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::primitives::QuickTileFace;
use crate::widget::{TaffyTreeExt, Widget};

/// The quiet floor a tile that is off stands on: a step up the theme's lift
/// ramp, under the hover tier, so a tile reads as a control rather than as
/// bare text on the panel.
const OFF_FLOOR_LIFT: f32 = 0.35;

/// The two targets of a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickTilePart {
    /// The tile less its chevron: switches the control.
    Switch,
    /// The chevron half: opens or closes the control's section.
    Details,
}

/// What the person did on a tile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickTileOutcome {
    /// Switch the control on or off.
    Switch,
    /// Open or close the control's section.
    Details,
}

/// The sizes a tile is made of, from the theme it is drawn in.
#[derive(Clone, Debug, PartialEq)]
struct TileMetrics {
    inset: f64,
    glyph: f64,
    gap: f64,
    trailing: f64,
    rule_inset: f64,
    details: f64,
    height: f64,
    label: TextStyle,
    detail: TextStyle,
}

impl TileMetrics {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let label = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let detail = theme.text(TextRole::Caption);
        let lines = f64::from(label.line_height_px()) + f64::from(detail.line_height_px());
        Self {
            inset: f64::from(spacing.s3),
            glyph: f64::from(metrics.icon_md),
            gap: f64::from(spacing.s2),
            trailing: f64::from(spacing.s1),
            rule_inset: f64::from(spacing.s2) + f64::from(spacing.s1),
            details: QuickTile::details_width(theme),
            height: f64::from(metrics.row_double).max(2.0 * f64::from(spacing.s2) + lines),
            label,
            detail,
        }
    }
}

/// A Quick Settings tile: a control's glyph, name and state line, on while
/// its control is, with a chevron half when the control has a section.
#[allow(
    clippy::struct_excessive_bools,
    reason = "on, enabled and the drawn-since flag are independent states of one control"
)]
pub struct QuickTile {
    id: String,
    icon: Icon,
    label: String,
    detail: String,
    on: bool,
    enabled: bool,
    /// The chevron half's accessible name, while the control has a section.
    details: Option<String>,
    /// How far the chevron has turned towards the open section, 0 to 1.
    turn: f64,
    rect: Rect,
    pointer: Option<Point>,
    pressed: Option<QuickTilePart>,
    focused: Option<QuickTilePart>,
    outcome: Option<QuickTileOutcome>,
    metrics: Option<TileMetrics>,
    dirty: bool,
}

impl QuickTile {
    /// A tile for the control `id` (its test id), shown as `icon` and
    /// `label`.
    pub fn new(id: impl Into<String>, icon: Icon, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            detail: String::new(),
            on: false,
            enabled: true,
            details: None,
            turn: 0.0,
            rect: Rect::ZERO,
            pointer: None,
            pressed: None,
            focused: None,
            outcome: None,
            metrics: None,
            dirty: true,
        }
    }

    /// The state line under the name: what is connected, what the control
    /// is set to.
    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    #[must_use]
    pub fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Give the tile a chevron half that opens the control's section,
    /// announced as `name`.
    #[must_use]
    pub fn with_details(mut self, name: impl Into<String>) -> Self {
        self.details = Some(name.into());
        self
    }

    /// A tile that is not enabled says so to assistive technology and
    /// ignores presses: a radio change in flight.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Show `icon`, `label` and `detail`, for a host that keeps one tile
    /// per control across frames.
    pub fn set_content(&mut self, icon: Icon, label: &str, detail: &str) {
        if self.icon != icon || self.label != label || self.detail != detail {
            self.icon = icon;
            label.clone_into(&mut self.label);
            detail.clone_into(&mut self.detail);
            self.dirty = true;
        }
    }

    pub fn set_on(&mut self, on: bool) {
        self.dirty |= self.on != on;
        self.on = on;
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.dirty |= self.enabled != enabled;
        self.enabled = enabled;
    }

    /// Give the tile a chevron half announced as `name`, or take it away.
    pub fn set_details(&mut self, name: Option<String>) {
        if self.details != name {
            self.details = name;
            self.dirty = true;
        }
    }

    /// How far the section is open, 0 (closed) to 1 (open): the chevron
    /// turns a quarter turn with it.
    pub fn set_details_open(&mut self, open: f64) {
        let open = open.clamp(0.0, 1.0);
        self.dirty |= (self.turn - open).abs() > f64::EPSILON;
        self.turn = open;
    }

    /// Where the pointer is, for a host that lays its parts out by hand and
    /// passes the pointer instead of its events.
    pub fn set_pointer(&mut self, pointer: Option<Point>) {
        let before = self.hovered();
        self.pointer = pointer;
        self.dirty |= before != self.hovered();
    }

    /// Which part has keyboard focus, for a host that keeps a focus order
    /// of its own.
    pub fn set_focused_part(&mut self, part: Option<QuickTilePart>) {
        let part = part.filter(|part| *part == QuickTilePart::Switch || self.details.is_some());
        self.dirty |= self.focused != part;
        self.focused = part;
    }

    /// Place the tile at `rect` in a host that lays its parts out by hand.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.dirty |= self.rect != rect;
        self.rect = rect;
    }

    /// What the person did since the last call.
    pub fn take_outcome(&mut self) -> Option<QuickTileOutcome> {
        self.outcome.take()
    }

    #[must_use]
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// How wide the chevron half is in `theme`: one pointer target.
    #[must_use]
    pub fn details_width(theme: &Theme) -> f64 {
        f64::from(theme.tokens.metrics.min_pointer_target)
    }

    /// The switch and chevron halves of a tile at `rect` whose chevron half
    /// is `details_width` wide ([`Self::details_width`]), for a host that
    /// hit-tests the tile before it is drawn.
    #[must_use]
    pub fn split(rect: Rect, details_width: f64) -> (Rect, Rect) {
        let x = (rect.x1 - details_width).max(rect.x0);
        (
            Rect::new(rect.x0, rect.y0, x, rect.y1),
            Rect::new(x, rect.y0, rect.x1, rect.y1),
        )
    }

    /// The tile's height in `theme`: two lines on the control padding, never
    /// under the two-line row.
    #[must_use]
    pub fn height(theme: &Theme) -> f64 {
        TileMetrics::of(theme).height
    }

    fn details_extent(&self) -> f64 {
        self.metrics.as_ref().map_or(
            f64::from(crate::tokens::Metrics::BASE.min_pointer_target),
            |metrics| metrics.details,
        )
    }

    /// The switch half and, with a section, the chevron half.
    #[must_use]
    pub fn parts(&self) -> (Rect, Option<Rect>) {
        if self.details.is_none() {
            return (self.rect, None);
        }
        let (switch, details) = Self::split(self.rect, self.details_extent());
        (switch, Some(details))
    }

    /// The part at `point`, if it is on the tile.
    #[must_use]
    pub fn part_at(&self, point: Point) -> Option<QuickTilePart> {
        let (switch, details) = self.parts();
        if details.is_some_and(|details| details.contains(point)) {
            Some(QuickTilePart::Details)
        } else if switch.contains(point) {
            Some(QuickTilePart::Switch)
        } else {
            None
        }
    }

    fn hovered(&self) -> Option<QuickTilePart> {
        self.pointer.and_then(|point| self.part_at(point))
    }

    fn activate(&mut self, part: QuickTilePart) -> bool {
        if !self.enabled {
            return false;
        }
        self.outcome = Some(match part {
            QuickTilePart::Switch => QuickTileOutcome::Switch,
            QuickTilePart::Details if self.details.is_some() => QuickTileOutcome::Details,
            QuickTilePart::Details => return false,
        });
        true
    }

    fn part_node(&self, part: QuickTilePart) -> crate::a11y::AccessNode {
        use crate::a11y::{AccessNode, AccessState, Role};
        let (switch, details) = self.parts();
        let state = AccessState::new()
            .focusable(true)
            .focused(self.focused == Some(part))
            .disabled(!self.enabled);
        match part {
            QuickTilePart::Switch => AccessNode::new(Role::Switch)
                .with_name(self.label.clone())
                .with_description(self.detail.clone())
                .with_hashed_key("switch")
                .with_bounds(switch)
                .with_state(state.checked(self.on)),
            QuickTilePart::Details => AccessNode::new(Role::Button)
                .with_optional_name(self.details.clone())
                .with_hashed_key("details")
                .with_bounds(details.unwrap_or(Rect::ZERO))
                .with_state(state.expanded(self.turn >= 1.0)),
        }
    }

    fn paint_content(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        metrics: &TileMetrics,
    ) {
        let (switch, details) = self.parts();
        let radius = ChromeRole::Control.radius(theme, self.rect.height());
        let hovered = self.hovered().map(|part| match part {
            QuickTilePart::Switch => switch,
            QuickTilePart::Details => details.unwrap_or(switch),
        });
        let face = QuickTileFace {
            rect: self.rect,
            main: switch,
            expand: details,
            on: self.on,
            hovered,
            radius,
        };
        if !self.on {
            crate::surface::card(
                scene,
                transform,
                self.rect,
                radius,
                theme.tokens.colors.lift(OFF_FLOOR_LIFT),
            );
        }
        let off_hover = selection_tier(theme, SelectionState::new(false, true, false)).fill;
        face.paint_ground(scene, transform, theme, off_hover);
        let (ink, ink_dim) = face.ink(theme);
        let glyph = Rect::from_center_size(
            Point::new(
                switch.x0 + metrics.inset + metrics.glyph / 2.0,
                switch.center().y,
            ),
            (metrics.glyph, metrics.glyph),
        );
        crate::icons::draw_icon(
            scene,
            transform,
            glyph,
            self.icon,
            ink,
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let text_x = glyph.x1 + metrics.gap;
        let width = crate::num::f32_of_f64((switch.x1 - metrics.trailing - text_x).max(1.0));
        let label = text.elide_to_width_style(&self.label, width, &metrics.label);
        let detail = text.elide_to_width_style(&self.detail, width, &metrics.detail);
        let (_, label_h) = text.measure_style(&label, None, &metrics.label);
        let (_, detail_h) = text.measure_style(&detail, None, &metrics.detail);
        let top = switch.center().y - (f64::from(label_h) + f64::from(detail_h)) / 2.0;
        text.draw_style(
            scene,
            &label,
            ink,
            transform.pre_translate((text_x, top).into()),
            None,
            &metrics.label,
        );
        text.draw_style(
            scene,
            &detail,
            ink_dim,
            transform.pre_translate((text_x, top + f64::from(label_h)).into()),
            None,
            &metrics.detail,
        );
        if let Some(details) = details {
            face.paint_divider(scene, transform, theme, metrics.rule_inset);
            let chevron = Rect::from_center_size(details.center(), (metrics.glyph, metrics.glyph));
            crate::icons::draw_icon(
                scene,
                transform
                    .pre_rotate_about(std::f64::consts::FRAC_PI_2 * self.turn, chevron.center()),
                chevron,
                Icon::ChevronRight,
                ink,
                LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        match self.focused {
            Some(QuickTilePart::Switch) => {
                crate::widget::paint_focus_ring(scene, transform, switch, radius, theme);
            }
            Some(QuickTilePart::Details) => {
                if let Some(details) = details {
                    crate::widget::paint_focus_ring(scene, transform, details, radius, theme);
                }
            }
            None => {}
        }
    }

    fn handle_key(&mut self, event: &KeyEvent) -> bool {
        if event.state != ElementState::Pressed {
            return false;
        }
        let Some(part) = self.focused else {
            return false;
        };
        match &event.logical_key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => self.activate(part),
            Key::Named(NamedKey::ArrowRight) if self.details.is_some() => {
                self.set_focused_part(Some(QuickTilePart::Details));
                true
            }
            Key::Named(NamedKey::ArrowLeft) if part == QuickTilePart::Details => {
                self.set_focused_part(Some(QuickTilePart::Switch));
                true
            }
            _ => false,
        }
    }
}

impl Widget for QuickTile {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a tile's height is a small pixel count; taffy's Style takes f32"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let metrics = TileMetrics::of(theme);
        let height = metrics.height as f32;
        self.metrics = Some(metrics);
        taffy.widget_leaf(
            Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                size: taffy::Size {
                    width: taffy::Dimension::auto(),
                    height: length(height),
                },
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.apply_layout_external(crate::widget::layout_rect(taffy, node, origin));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let metrics = TileMetrics::of(theme);
        if self.metrics.as_ref() != Some(&metrics) {
            self.metrics = Some(metrics.clone());
        }
        if self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            self.paint_content(scene, transform, text_renderer, theme, &metrics);
        }
        self.dirty = false;
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.set_pointer(Some(ctx.logical(position)));
            }
            WindowEvent::CursorLeft { .. } => {
                self.set_pointer(None);
                self.pressed = None;
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let at = self.hovered();
                match state {
                    ElementState::Pressed => {
                        if let Some(part) = at.filter(|_| self.enabled) {
                            self.pressed = Some(part);
                            *ctx.focus_target = Some(ctx.node_id);
                            *ctx.handled = true;
                        }
                    }
                    ElementState::Released => {
                        if let Some(part) = self.pressed.take() {
                            *ctx.handled = true;
                            if at == Some(part) {
                                self.activate(part);
                            }
                        }
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } if self.handle_key(event) => {
                *ctx.handled = true;
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.enabled && self.part_at(point).is_some())
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.enabled
    }

    fn set_focus(&mut self, focused: bool) {
        self.set_focused_part(focused.then_some(QuickTilePart::Switch));
    }

    fn is_focused(&self) -> bool {
        self.focused.is_some()
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let part = if key_path.last() == Some(&crate::a11y::hash_key(&"details")) {
            QuickTilePart::Details
        } else {
            QuickTilePart::Switch
        };
        match action {
            Action::Activate | Action::ToggleCheck => self.activate(part),
            _ => false,
        }
    }

    fn name(&self) -> &'static str {
        "QuickTile"
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.id)
    }

    fn test_rects(&self) -> Vec<(String, Rect)> {
        let (switch, details) = self.parts();
        let mut rects = vec![(self.id.clone(), switch)];
        if let Some(details) = details {
            rects.push((format!("{}_details", self.id), details));
        }
        rects
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        if self.details.is_none() {
            return Some(self.part_node(QuickTilePart::Switch));
        }
        let mut group = AccessNode::new(Role::Group)
            .with_name(self.label.clone())
            .with_bounds(self.rect);
        group.children = vec![
            self.part_node(QuickTilePart::Switch),
            self.part_node(QuickTilePart::Details),
        ];
        Some(group.with_keyed_action_routes(&self.id))
    }
}

#[cfg(test)]
#[path = "quick_tile_tests.rs"]
mod tests;
