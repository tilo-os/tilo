//! [`QuickTile`] — a Quick Settings control: a glyph, a name and a state line on a tile filled
//! with the on colour while the control is on, and a press anywhere switches it. A tile whose
//! control has a section of its own (the networks under Wi-Fi) gives it a chevron half that
//! turns as the section opens.
//!
//! It reports what the person did as a [`QuickTileOutcome`], and the host switches the control
//! or opens its section; in an app each target is bound to an action instead
//! ([`QuickTile::on_switch`], [`QuickTile::on_details`]).
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

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::chrome::{ChromeRole, SelectionState, selection_floor, selection_floor_ramp};
use crate::icons::{Icon, LUCIDE_DEFAULT_STROKE_PX};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::primitives::QuickTileFace;
use crate::widget::{TaffyTreeExt, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickTilePart {
    /// The tile less its chevron: switches the control.
    Switch,
    /// The chevron half: opens or closes the control's section.
    Details,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuickTileOutcome {
    /// Switch the control on or off.
    Switch,
    /// Open or close the control's section.
    Details,
}

#[derive(Clone, Debug, PartialEq)]
struct TileMetrics {
    inset: f64,
    glyph: f64,
    gap: f64,
    trailing: f64,
    rule_inset: f64,
    details: f64,
    height: f64,
    height_one: f64,
    label: TextStyle,
    detail: TextStyle,
}

impl TileMetrics {
    fn of(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let label = theme.text(TextRole::Label).with_weight(TextWeight::Medium);
        let detail = theme.text(TextRole::Caption);
        let name = f64::from(label.line_height_px());
        let line = f64::from(detail.line_height_px());
        let padding = 2.0 * f64::from(spacing.s2);
        let row = f64::from(metrics.row_double);
        Self {
            inset: f64::from(spacing.s3),
            glyph: f64::from(metrics.icon_md),
            gap: f64::from(spacing.s2),
            trailing: f64::from(spacing.s1),
            rule_inset: f64::from(spacing.s2) + f64::from(spacing.s1),
            details: QuickTile::details_width(theme),
            height: row.max(padding + name + 2.0 * line).ceil(),
            height_one: row.max(padding + name + line).ceil(),
            label,
            detail,
        }
    }

    /// The height of a tile whose state line takes `lines` (one or two).
    fn height_for(&self, lines: usize) -> f64 {
        if lines > 1 {
            self.height
        } else {
            self.height_one
        }
    }
}

/// Where a tile's name and state line stand in `rect`: the left edge of the text and the width
/// it may run to, past the glyph on the left and the switch (and its chevron half, if it has
/// one) on the right.
fn text_extent(rect: Rect, has_details: bool, metrics: &TileMetrics) -> (f64, f32) {
    let switch = if has_details {
        Rect::new(
            rect.x0,
            rect.y0,
            (rect.x1 - metrics.details).max(rect.x0),
            rect.y1,
        )
    } else {
        rect
    };
    let x = switch.x0 + metrics.inset + metrics.glyph + metrics.gap;
    let width = crate::num::f32_of_f64((switch.x1 - metrics.trailing - x).max(1.0));
    (x, width)
}

/// The state line in the room it has: whole on one line when it fits, else broken after the
/// last word that does and the rest elided on a second.
fn detail_lines(
    text: &mut TextRenderer,
    detail: &str,
    width: f32,
    style: &TextStyle,
) -> (String, Option<String>) {
    if detail.is_empty() || width <= 0.0 {
        return (String::new(), None);
    }
    if text.measure_style(detail, None, style).0 <= width {
        return (detail.to_owned(), None);
    }
    let words: Vec<&str> = detail.split_whitespace().collect();
    for cut in (1..words.len()).rev() {
        let first = words.get(..cut).unwrap_or_default().join(" ");
        if text.measure_style(&first, None, style).0 <= width {
            let rest = words.get(cut..).unwrap_or_default().join(" ");
            let second = text.elide_to_width_style(&rest, width, style);
            return (first, Some(second));
        }
    }
    (text.elide_to_width_style(detail, width, style), None)
}

/// A Quick Settings tile: a control's glyph, name and state line, on while
/// its control is, with a chevron half when the control has a section.
#[allow(
    clippy::struct_excessive_bools,
    reason = "on, enabled and the drawn-since flag are independent states of one control"
)]
#[crate::component::not_a_component(system, "Quick Settings are the system's")]
pub struct QuickTile {
    id: String,
    icon: Icon,
    label: String,
    detail: String,
    on: bool,
    enabled: bool,
    details: Option<String>,
    /// How far the chevron has turned towards the open section, 0 to 1.
    turn: f64,
    rect: Rect,
    pointer: Option<Point>,
    pressed: Option<QuickTilePart>,
    focused: Option<QuickTilePart>,
    outcome: Option<QuickTileOutcome>,
    switch_action: Option<crate::widget::ActionEmitter<()>>,
    details_action: Option<crate::widget::ActionEmitter<()>>,
    metrics: Option<TileMetrics>,
    /// How much of the room the tile is offered it draws in: half while it
    /// stands two to a row, so its state line breaks where it will when it
    /// is drawn.
    share: f32,
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
            switch_action: None,
            details_action: None,
            metrics: None,
            share: 1.0,
            dirty: true,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    /// How much of the room the tile is offered it draws in: half when it
    /// stands two to a row, so its height follows the state line it will
    /// have there ([`Self::height`] alone assumes the whole room).
    #[must_use]
    pub fn share(mut self, share: f32) -> Self {
        self.share = share.clamp(0.0, 1.0);
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
    /// Invoke `binding` when the tile is pressed, instead of reporting a
    /// [`QuickTileOutcome::Switch`].
    #[must_use]
    pub fn on_switch(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.switch_action = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    /// Invoke `binding` when the chevron is pressed, instead of reporting a
    /// [`QuickTileOutcome::Details`]. The tile has a chevron only once it is
    /// named ([`Self::with_details`]).
    #[must_use]
    pub fn on_details(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.details_action = Some(crate::widget::ActionEmitter::new(binding));
        self
    }

    /// How far the chevron has turned towards an open section, 0 to 1.
    #[must_use]
    pub fn details_open(mut self, open: f64) -> Self {
        self.set_details_open(open);
        self
    }

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

    #[must_use]
    pub fn parts(&self) -> (Rect, Option<Rect>) {
        if self.details.is_none() {
            return (self.rect, None);
        }
        let (switch, details) = Self::split(self.rect, self.details_extent());
        (switch, Some(details))
    }

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
        let outcome = match part {
            QuickTilePart::Switch => QuickTileOutcome::Switch,
            QuickTilePart::Details if self.details.is_some() => QuickTileOutcome::Details,
            QuickTilePart::Details => return false,
        };
        let bound = match outcome {
            QuickTileOutcome::Switch => self.switch_action.as_mut(),
            QuickTileOutcome::Details => self.details_action.as_mut(),
        };
        match bound {
            Some(emitter) => {
                if let Err(error) = emitter.emit(()) {
                    tracing::warn!(%error, tile = %self.id, "action-bound tile refused its arguments");
                }
            }
            None => self.outcome = Some(outcome),
        }
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
            ground: selection_floor(theme, theme.tokens.colors.surfaces.base),
            radius,
        };
        // A tile that is off stands on a quiet floor and answers the pointer
        // on the one ordered ramp, so its floor can never lift past its own
        // hover.
        let base = theme.tokens.colors.surfaces.base;
        if !self.on {
            crate::surface::card(scene, transform, self.rect, radius, face.ground);
        }
        let off_hover = selection_floor_ramp(theme, SelectionState::new(false, true, false), base);
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
            face.glyph_ink(theme),
            LUCIDE_DEFAULT_STROKE_PX,
        );
        let (text_x, width) = text_extent(self.rect, details.is_some(), metrics);
        let label = text.elide_to_width_style(&self.label, width, &metrics.label);
        let (detail, detail_second) = if self.rect.height() + 0.5 >= metrics.height_for(2) {
            detail_lines(text, &self.detail, width, &metrics.detail)
        } else {
            (
                text.elide_to_width_style(&self.detail, width, &metrics.detail),
                None,
            )
        };
        let (_, label_h) = text.measure_style(&label, None, &metrics.label);
        let (_, detail_h) = text.measure_style(&detail, None, &metrics.detail);
        let line = f64::from(metrics.detail.line_height_px());
        let detail_h = if detail_second.is_some() {
            2.0 * line
        } else {
            f64::from(detail_h)
        };
        let top = switch.center().y - (f64::from(label_h) + detail_h) / 2.0;
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
        if let Some(second) = &detail_second {
            text.draw_style(
                scene,
                second,
                ink_dim,
                transform.pre_translate((text_x, top + f64::from(label_h) + line).into()),
                None,
                &metrics.detail,
            );
        }
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

impl crate::widget::controls::ActionControls for QuickTile {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for emitter in [&mut self.switch_action, &mut self.details_action]
            .into_iter()
            .flatten()
        {
            let _ = emitter.install_custody(domain, attachment);
        }
    }
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        [&mut self.switch_action, &mut self.details_action]
            .into_iter()
            .flatten()
            .find_map(crate::widget::ActionEmitter::take)
    }
    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let mut bindings = Vec::new();
        if let Some(emitter) = &self.switch_action {
            bindings.push(emitter.metadata(self.id.clone()));
        }
        if let Some(emitter) = &self.details_action {
            bindings.push(emitter.metadata(format!("{}_details", self.id)));
        }
        bindings
    }
    fn has_action_control_bindings(&self) -> bool {
        self.switch_action.is_some() || self.details_action.is_some()
    }
    fn refresh_control_action_availability(
        &mut self,
        allowed: &std::collections::HashSet<tilo_actions::ActionAddress>,
    ) {
        for emitter in [&mut self.switch_action, &mut self.details_action]
            .into_iter()
            .flatten()
        {
            let before = emitter.is_available();
            emitter.refresh_availability(allowed);
            self.dirty |= before != emitter.is_available();
        }
    }
}

impl Widget for QuickTile {
    crate::provides!(dyn crate::widget::controls::ActionControls);

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
        self.metrics = Some(metrics);
        let width = if self.share >= 1.0 {
            taffy::Dimension::auto()
        } else {
            taffy::Dimension::percent(self.share)
        };
        taffy.widget_leaf(
            Style {
                flex_grow: 1.0,
                flex_shrink: 1.0,
                size: taffy::Size {
                    width,
                    height: taffy::Dimension::auto(),
                },
                ..Default::default()
            },
            node_map,
            self,
        )
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut crate::text_render::TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(metrics) = self.metrics.as_ref() else {
            return taffy::Size::ZERO;
        };
        let offered = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(width) => width,
            taffy::AvailableSpace::MinContent => 0.0,
            taffy::AvailableSpace::MaxContent => return taffy::Size::ZERO,
        });
        let width = known.width.unwrap_or(offered * self.share).max(0.0);
        let rect = Rect::new(0.0, 0.0, f64::from(width), metrics.height);
        let (_, room) = text_extent(rect, self.details.is_some(), metrics);
        let fits = self.detail.is_empty()
            || text.measure_style(&self.detail, None, &metrics.detail).0 <= room;
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(metrics.height_for(if fits { 1 } else { 2 })),
        }
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
