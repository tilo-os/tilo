//! `ListRow`: one row of a content list (a message, a contact, a request,
//! a file, a device), as a retained widget and as the [`ListRowFace`]
//! immediate painter with identical geometry.
//!
//! A row has up to four columns, each optional except the title:
//!
//! | Column | Holds | Set in |
//! |---|---|---|
//! | leading | an [`Avatar`](crate::widget::Avatar), an [`Icon`] or a short badge (`GET`), in a fixed-width column so a list's titles line up | the leading slot's own geometry |
//! | text | the title, then an optional secondary line of one or two lines | `Body` (`.strong()` when emphasised), `Caption` |
//! | meta | a timestamp, a size, a count | `Caption.numeric()`, right-aligned |
//! | trailing | a status dot centred on the row, a disclosure chevron | — |
//!
//! Hover (or keyboard focus) swaps the meta for the row's actions. The row
//! height comes from the measured line boxes of its text, never a constant,
//! so it grows with the text scale. Selection is the lift (HIG §9.3), and
//! the inset separator between two rows hides beside a selected row.

use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect};
use vello::peniko::{Color, Fill};
use winit::event::WindowEvent;

use crate::a11y::{AccessNode, AccessState, Role};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::tokens::colors::PresenceHue;
use crate::widget::activation::{Activation, Interaction, activation_controls};
use crate::widget::avatar::AvatarFace;
use crate::widget::{TaffyTreeExt, Widget};

/// What sits in a row's leading column.
#[derive(Clone, Copy, Debug)]
pub enum RowLeading<'a> {
    /// A person or an account.
    Avatar(AvatarFace<'a>),
    /// A kind of thing (a folder, a device, a setting).
    Icon(Icon),
    /// A short code that classifies the row: an HTTP method, a file
    /// extension, a log level. Set in `Caption.strong()` on a quiet pill.
    Badge(&'a str),
}

/// A status dot centred on the row's trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowStatus<'a> {
    /// Running, connected, healthy.
    Active,
    /// Needs attention soon.
    Warning,
    /// Failed or offline unexpectedly.
    Error,
    /// Present but inactive.
    Idle,
    /// A person's presence, in their colour.
    Presence(&'a str),
}

impl RowStatus<'_> {
    fn colour(self, theme: &Theme) -> Color {
        let colors = &theme.tokens.colors;
        match self {
            Self::Active => colors.semantic.success,
            Self::Warning => colors.semantic.warning,
            Self::Error => colors.semantic.danger,
            Self::Idle => theme.text_tertiary,
            Self::Presence(id) => colors.presence.hue(PresenceHue::of_identity(id)),
        }
    }

    fn spoken(self) -> Option<&'static str> {
        match self {
            Self::Active => Some("Active"),
            Self::Warning => Some("Needs attention"),
            Self::Error => Some("Error"),
            Self::Idle => Some("Idle"),
            Self::Presence(_) => None,
        }
    }
}

/// One of a row's hover actions: an icon button that names what it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowAction<'a> {
    pub icon: Icon,
    pub label: &'a str,
}

/// The action a swipe reveals under the row's trailing edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwipeAction<'a> {
    pub icon: Icon,
    pub label: &'a str,
    /// Deleting, archiving away, revoking: the revealed area is the danger
    /// status colour, because the completed swipe is the confirmation.
    pub destructive: bool,
}

/// The resolved geometry of every row in `theme`: what the face paints and
/// the retained row measures, from one place.
#[derive(Clone, Debug)]
struct RowMetrics {
    pad_x: f64,
    pad_y: f64,
    min_height: f64,
    column_gap: f64,
    title: TextStyle,
    title_strong: TextStyle,
    secondary: TextStyle,
    meta: TextStyle,
    badge: TextStyle,
    icon: f64,
    action: f64,
    action_gap: f64,
    dot: f64,
    radius: f64,
}

impl RowMetrics {
    fn new(theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            pad_x: f64::from(spacing.s3),
            pad_y: f64::from(spacing.s2),
            min_height: f64::from(metrics.min_pointer_target + spacing.s2),
            column_gap: f64::from(spacing.s3),
            title: theme.text(TextRole::Body),
            title_strong: theme.text(TextRole::Body).strong(),
            secondary: theme.text(TextRole::Caption),
            meta: theme.text(TextRole::Caption).numeric(),
            badge: theme.text(TextRole::Caption).strong(),
            icon: f64::from(metrics.icon_md),
            action: f64::from(metrics.bar_control_height),
            action_gap: f64::from(spacing.s1),
            dot: f64::from(spacing.s2),
            radius: f64::from(theme.tokens.radii.item),
        }
    }

    fn line(text: &mut TextRenderer, style: &TextStyle) -> f64 {
        f64::from(text.measure_style("Ag", None, style).1)
    }

    /// The text block's height: the title line plus `lines` secondary lines.
    fn text_block(&self, text: &mut TextRenderer, lines: u8) -> f64 {
        Self::line(text, &self.title) + f64::from(lines) * Self::line(text, &self.secondary)
    }

    fn height(&self, text: &mut TextRenderer, lines: u8) -> f64 {
        (self.text_block(text, lines) + 2.0 * self.pad_y)
            .max(self.min_height)
            .ceil()
    }

    /// The avatar's side: the inline size beside a one-line row, the list
    /// size beside a row with a secondary line. Fixed per kind of row, so a
    /// list's titles share one edge whatever each row's secondary text does.
    fn avatar(theme: &Theme, lines: u8) -> f64 {
        let metrics = &theme.tokens.metrics;
        if lines == 0 {
            f64::from(metrics.icon_lg)
        } else {
            f64::from(metrics.min_pointer_target)
        }
    }
}

/// Measure the leading column that fits every badge in `badges`, so a list
/// of rows with badges of different lengths keeps one title edge. Pass the
/// result to every row's [`ListRowFace::leading_column`].
pub fn badge_column_width(text: &mut TextRenderer, theme: &Theme, badges: &[&str]) -> f64 {
    let metrics = RowMetrics::new(theme);
    let pad = f64::from(theme.tokens.spacing.s1) * 2.0;
    badges
        .iter()
        .map(|badge| f64::from(text.measure_style(badge, None, &metrics.badge).0) + pad)
        .fold(0.0, f64::max)
        .ceil()
}

/// The fill a row shows: the selection lift of `theme`'s ground when
/// selected (or pressed), half of it under the pointer, nothing at rest.
fn row_fill(theme: &Theme, selected: bool, hovered: bool) -> Option<Color> {
    let state = crate::chrome::SelectionState::new(selected, hovered, false);
    (selected || hovered).then(|| {
        crate::chrome::selection_tier_on(theme, state, theme.tokens.colors.surfaces.content).fill
    })
}

/// Immediate-mode list row. Build one per row per frame, `measure` it for
/// its height, then `paint` it into the rect you laid out.
#[derive(Clone, Debug)]
pub struct ListRowFace<'a> {
    title: &'a str,
    secondary: Option<&'a str>,
    secondary_lines: u8,
    meta: Option<&'a str>,
    leading: Option<RowLeading<'a>>,
    leading_column: Option<f64>,
    status: Option<RowStatus<'a>>,
    disclosure: bool,
    emphasized: bool,
    selected: bool,
    next_selected: bool,
    separator: bool,
    actions: &'a [RowAction<'a>],
    hovered_action: Option<usize>,
    swipe: Option<(SwipeAction<'a>, f64)>,
    interaction: Interaction,
}

impl<'a> ListRowFace<'a> {
    #[must_use]
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            secondary: None,
            secondary_lines: 1,
            meta: None,
            leading: None,
            leading_column: None,
            status: None,
            disclosure: false,
            emphasized: false,
            selected: false,
            next_selected: false,
            separator: false,
            actions: &[],
            hovered_action: None,
            swipe: None,
            interaction: Interaction::default(),
        }
    }

    /// The secondary line under the title (a preview, an address, a path),
    /// elided to one line.
    #[must_use]
    pub fn secondary(mut self, text: &'a str) -> Self {
        self.secondary = Some(text);
        self
    }

    /// Let the secondary text wrap to two lines before it is elided (a
    /// message preview).
    #[must_use]
    pub fn two_line_secondary(mut self) -> Self {
        self.secondary_lines = 2;
        self
    }

    /// A timestamp, size or count, right-aligned in tabular figures.
    #[must_use]
    pub fn meta(mut self, text: &'a str) -> Self {
        self.meta = Some(text);
        self
    }

    #[must_use]
    pub fn leading(mut self, leading: RowLeading<'a>) -> Self {
        self.leading = Some(leading);
        self
    }

    /// The width every row of this list reserves for its leading column,
    /// from [`badge_column_width`]. Icons and avatars size their own column.
    #[must_use]
    pub fn leading_column(mut self, width: f64) -> Self {
        self.leading_column = Some(width);
        self
    }

    #[must_use]
    pub fn status(mut self, status: RowStatus<'a>) -> Self {
        self.status = Some(status);
        self
    }

    /// A trailing chevron: activating the row opens a page.
    #[must_use]
    pub fn disclosure(mut self) -> Self {
        self.disclosure = true;
        self
    }

    /// Unread or otherwise new: the title takes the strong weight.
    #[must_use]
    pub fn emphasized(mut self, emphasized: bool) -> Self {
        self.emphasized = emphasized;
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Draw the inset separator along the bottom edge. It hides when this
    /// row or `next_selected` (the row below) is selected.
    #[must_use]
    pub fn separator(mut self, next_selected: bool) -> Self {
        self.separator = true;
        self.next_selected = next_selected;
        self
    }

    /// Actions shown in place of the meta while the row is hovered or
    /// focused. Hit-test them with [`Self::action_rects`].
    #[must_use]
    pub fn actions(mut self, actions: &'a [RowAction<'a>]) -> Self {
        self.actions = actions;
        self
    }

    /// Which action the pointer is over.
    #[must_use]
    pub fn hovered_action(mut self, index: Option<usize>) -> Self {
        self.hovered_action = index;
        self
    }

    /// The row slid `offset` logical pixels toward its leading edge,
    /// revealing `action` behind its trailing edge.
    #[must_use]
    pub fn swiped(mut self, action: SwipeAction<'a>, offset: f64) -> Self {
        self.swipe = Some((action, offset.max(0.0)));
        self
    }

    #[must_use]
    pub fn interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = interaction;
        self
    }

    fn lines(&self) -> u8 {
        if self.secondary.is_some() {
            self.secondary_lines
        } else {
            0
        }
    }

    /// The row's height at this text scale.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        RowMetrics::new(theme).height(text, self.lines())
    }

    fn shows_actions(&self) -> bool {
        !self.actions.is_empty() && (self.interaction.hovered || self.interaction.focused)
    }

    fn trailing_width(&self, m: &RowMetrics) -> f64 {
        let mut width = 0.0;
        if self.status.is_some() {
            width += m.dot + m.column_gap;
        }
        if self.disclosure {
            width += m.icon + m.column_gap;
        }
        width
    }

    /// Where each hover action sits in `rect`, in order. Empty unless the
    /// row is hovered or focused.
    pub fn action_rects(&self, rect: Rect, theme: &Theme) -> Vec<Rect> {
        self.action_rects_with(rect, &RowMetrics::new(theme))
    }

    fn action_rects_with(&self, rect: Rect, m: &RowMetrics) -> Vec<Rect> {
        if !self.shows_actions() {
            return Vec::new();
        }
        let mut right = rect.x1 - m.pad_x - self.trailing_width(m);
        let cy = rect.center().y;
        let mut rects: Vec<Rect> = self
            .actions
            .iter()
            .rev()
            .map(|_| {
                let r = Rect::new(
                    right - m.action,
                    cy - m.action / 2.0,
                    right,
                    cy + m.action / 2.0,
                );
                right -= m.action + m.action_gap;
                r
            })
            .collect();
        rects.reverse();
        rects
    }

    fn leading_width(&self, m: &RowMetrics, text: &mut TextRenderer, theme: &Theme) -> f64 {
        match self.leading {
            None => 0.0,
            Some(RowLeading::Icon(_)) => m.icon,
            Some(RowLeading::Avatar(_)) => RowMetrics::avatar(theme, self.lines()),
            Some(RowLeading::Badge(badge)) => self
                .leading_column
                .unwrap_or_else(|| badge_column_width(text, theme, &[badge])),
        }
    }

    /// Where the title starts, relative to the row's leading edge: the inset
    /// separator starts here too.
    fn text_x(&self, rect: Rect, m: &RowMetrics, text: &mut TextRenderer, theme: &Theme) -> f64 {
        let leading = self.leading_width(m, text, theme);
        rect.x0
            + m.pad_x
            + if leading > 0.0 {
                leading + m.column_gap
            } else {
                0.0
            }
    }

    /// Paint the row into `rect`.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect) {
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        let m = RowMetrics::new(theme);
        let colors = &theme.tokens.colors;

        let offset = self
            .swipe
            .map_or(0.0, |(_, offset)| offset.min(rect.width()));
        if let Some((action, _)) = self.swipe
            && offset > 0.0
        {
            self.paint_swipe(ctx, rect, &m, action, offset);
        }
        let row = rect - vello::kurbo::Vec2::new(offset, 0.0);
        if offset > 0.0 {
            // The sliding row stays opaque over what it reveals.
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                colors.surfaces.content,
                None,
                &row,
            );
        }

        let pressed_or_selected = self.selected || self.interaction.pressed;
        if let Some(fill) = row_fill(
            theme,
            pressed_or_selected,
            self.interaction.hovered && !self.interaction.disabled,
        ) {
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &RoundedRect::from_rect(row, m.radius),
            );
        }

        let lines = self.lines();
        let block = m.text_block(ctx.text, lines);
        let text_top = row.center().y - block / 2.0;
        let title_line = RowMetrics::line(ctx.text, &m.title);

        // Leading column, centred on the row.
        let leading = self.leading_width(&m, ctx.text, theme);
        let lead_rect = Rect::new(
            row.x0 + m.pad_x,
            row.center().y - leading.max(m.icon) / 2.0,
            row.x0 + m.pad_x + leading,
            row.center().y + leading.max(m.icon) / 2.0,
        );
        match self.leading {
            None => {}
            Some(RowLeading::Icon(icon)) => {
                let r = Rect::from_center_size(lead_rect.center(), (m.icon, m.icon));
                icon.draw(ctx.scene, ctx.transform, r, theme.text_dim);
            }
            Some(RowLeading::Avatar(face)) => {
                let r = Rect::from_center_size(lead_rect.center(), (leading, leading));
                face.paint(ctx, r);
            }
            Some(RowLeading::Badge(badge)) => {
                paint_badge(ctx, &m, lead_rect, badge);
            }
        }

        // Trailing: status dot, then disclosure, at the far edge.
        let mut right = row.x1 - m.pad_x;
        if let Some(status) = self.status {
            let centre = Point::new(right - m.dot / 2.0, row.center().y);
            ctx.scene.fill(
                Fill::NonZero,
                ctx.transform,
                status.colour(theme),
                None,
                &Circle::new(centre, m.dot / 2.0),
            );
            right -= m.dot + m.column_gap;
        }
        if self.disclosure {
            let r = Rect::new(
                right - m.icon,
                row.center().y - m.icon / 2.0,
                right,
                row.center().y + m.icon / 2.0,
            );
            Icon::ChevronRight.draw(ctx.scene, ctx.transform, r, theme.text_tertiary);
            right -= m.icon + m.column_gap;
        }

        // Meta, or the hover actions in its place.
        let mut text_right = right;
        if self.shows_actions() {
            let rects = self.action_rects_with(rect, &m);
            if let Some(first) = rects.first() {
                text_right = first.x0 - m.column_gap;
            }
            let ground =
                row_fill(theme, pressed_or_selected, true).unwrap_or(colors.surfaces.content);
            for (index, (action, r)) in self.actions.iter().zip(&rects).enumerate() {
                let r = *r - vello::kurbo::Vec2::new(offset, 0.0);
                if self.hovered_action == Some(index) {
                    ctx.scene.fill(
                        Fill::NonZero,
                        ctx.transform,
                        colors.lift_on(ground, 1.0),
                        None,
                        &RoundedRect::from_rect(r, m.radius),
                    );
                }
                let glyph = Rect::from_center_size(r.center(), (m.icon, m.icon));
                action
                    .icon
                    .draw(ctx.scene, ctx.transform, glyph, theme.text_primary);
            }
        } else if let Some(meta) = self.meta {
            let available = (right - (row.x0 + m.pad_x)).max(0.0) / 2.0;
            let meta =
                ctx.text
                    .elide_to_width_style(meta, crate::num::f32_of_f64(available), &m.meta);
            let (width, height) = ctx.text.measure_style(&meta, None, &m.meta);
            let top = if lines == 0 {
                row.center().y - f64::from(height) / 2.0
            } else {
                // Sits on the title's line box, as a timestamp beside a sender.
                text_top + (title_line - f64::from(height)) / 2.0
            };
            ctx.text(
                &meta,
                &m.meta,
                theme.text_dim,
                Point::new(right - f64::from(width), top),
                None,
            );
            text_right = right - f64::from(width) - m.column_gap;
        }

        // Title and secondary text.
        let text_x = self.text_x(row, &m, ctx.text, theme);
        let width = crate::num::f32_of_f64((text_right - text_x).max(0.0));
        let title_style = if self.emphasized {
            &m.title_strong
        } else {
            &m.title
        };
        let title = ctx
            .text
            .elide_to_width_style(self.title, width, title_style);
        ctx.text(
            &title,
            title_style,
            theme.text_primary,
            Point::new(text_x, text_top),
            None,
        );
        if let Some(secondary) = self.secondary {
            let secondary_line = RowMetrics::line(ctx.text, &m.secondary);
            let kept = ctx.text.clamp_to_lines(
                secondary,
                &m.secondary,
                width,
                usize::from(self.secondary_lines),
            );
            for (index, line) in kept.iter().enumerate() {
                ctx.text(
                    line,
                    &m.secondary,
                    theme.text_dim,
                    Point::new(
                        text_x,
                        text_top + title_line + crate::num::f64_of_usize(index) * secondary_line,
                    ),
                    None,
                );
            }
        }

        if self.separator && !self.selected && !self.next_selected {
            crate::surface::separator(
                ctx.scene,
                ctx.transform,
                text_x,
                row.x1 - m.pad_x,
                rect.y1 - 0.5,
                0.0,
                colors.surfaces.content,
            );
        }

        if self.interaction.focused && !self.interaction.disabled {
            // Inside the row: rows stack edge to edge, so a ring outside one
            // would sit on its neighbours.
            crate::widget::activation::paint_focus_ring(
                ctx.scene,
                ctx.transform,
                row.inset(-2.0),
                (m.radius - 2.0).max(0.0),
                theme,
            );
        }
    }

    fn paint_swipe(
        &self,
        ctx: &mut PaintCtx<'_>,
        rect: Rect,
        m: &RowMetrics,
        action: SwipeAction<'_>,
        offset: f64,
    ) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let revealed = Rect::new(rect.x1 - offset, rect.y0, rect.x1, rect.y1);
        let fill = if action.destructive {
            colors.semantic.danger
        } else {
            colors.lift_on(colors.surfaces.content, 1.0)
        };
        ctx.scene
            .fill(Fill::NonZero, ctx.transform, fill, None, &revealed);
        let ink = if action.destructive {
            crate::widget::avatar::initial_ink(fill, theme)
        } else {
            theme.text_primary
        };
        if offset >= m.icon + 2.0 * m.pad_x {
            let glyph = Rect::new(
                revealed.x1 - m.pad_x - m.icon,
                revealed.center().y - m.icon / 2.0,
                revealed.x1 - m.pad_x,
                revealed.center().y + m.icon / 2.0,
            );
            action.icon.draw(ctx.scene, ctx.transform, glyph, ink);
        }
    }

    /// The row's accessibility node: a list item named by its title, with
    /// the secondary line, meta and status as its value.
    pub fn accessibility(&self, rect: Rect, action_target: impl Into<String>) -> AccessNode {
        let value: Vec<&str> = [
            self.secondary,
            self.meta,
            self.status.and_then(RowStatus::spoken),
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut node = AccessNode::new(Role::ListItem)
            .with_name(self.title)
            .with_action_target(action_target)
            .with_bounds(rect)
            .with_state(
                AccessState::new()
                    .focusable(true)
                    .focused(self.interaction.focused)
                    .selected(self.selected)
                    .disabled(self.interaction.disabled),
            );
        if !value.is_empty() {
            node = node.with_value(value.join(", "));
        }
        node
    }
}

fn paint_badge(ctx: &mut PaintCtx<'_>, m: &RowMetrics, column: Rect, badge: &str) {
    let theme = ctx.theme;
    let (width, height) = ctx.text.measure_style(badge, None, &m.badge);
    let pad = f64::from(theme.tokens.spacing.s1);
    let pill = Rect::from_origin_size(
        (
            column.x0,
            column.center().y - f64::from(height) / 2.0 - pad / 2.0,
        ),
        (f64::from(width) + 2.0 * pad, f64::from(height) + pad),
    );
    let raised = theme.tokens.colors.surfaces.raised;
    crate::surface::card(
        ctx.scene,
        ctx.transform,
        pill,
        f64::from(theme.tokens.radii.detail),
        raised,
    );
    ctx.text(
        badge,
        &m.badge,
        theme.text_dim,
        Point::new(pill.x0 + pad, pill.y0 + pad / 2.0),
        None,
    );
}

/// Owned form of [`RowLeading`] for the retained row.
enum Leading {
    Avatar {
        name: String,
        identity: String,
        image: Option<vello::peniko::ImageData>,
    },
    Icon(Icon),
    Badge(String),
}

struct OwnedAction {
    icon: Icon,
    label: String,
    on_activate: Box<dyn FnMut()>,
}

struct OwnedSwipe {
    icon: Icon,
    label: String,
    destructive: bool,
    on_activate: Box<dyn FnMut()>,
}

/// Distance past which a released swipe completes, as a share of the row.
const SWIPE_COMMIT: f64 = 0.4;

/// A retained list row. Construct with a stable id and a title; press
/// activates it (pointer, Enter/Space, or assistive technology).
pub struct ListRow {
    title: String,
    secondary: Option<String>,
    two_line: bool,
    meta: Option<String>,
    leading: Option<Leading>,
    status: Option<(StatusKind, String)>,
    disclosure: bool,
    emphasized: bool,
    selected: bool,
    separator: bool,
    next_selected: bool,
    actions: Vec<OwnedAction>,
    hovered_action: Option<usize>,
    pressed_action: Option<usize>,
    swipe: Option<OwnedSwipe>,
    swipe_origin: Option<(u64, f64)>,
    swipe_offset: f64,
    activation: Activation,
    metrics: Option<RowMetrics>,
    rect: Rect,
    dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StatusKind {
    Active,
    Warning,
    Error,
    Idle,
    Presence,
}

impl ListRow {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            secondary: None,
            two_line: false,
            meta: None,
            leading: None,
            status: None,
            disclosure: false,
            emphasized: false,
            selected: false,
            separator: false,
            next_selected: false,
            actions: Vec::new(),
            hovered_action: None,
            pressed_action: None,
            swipe: None,
            swipe_origin: None,
            swipe_offset: 0.0,
            activation: Activation::new(id),
            metrics: None,
            rect: Rect::ZERO,
            dirty: true,
        }
    }

    /// See [`ListRowFace::secondary`].
    #[must_use]
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = Some(text.into());
        self
    }

    /// See [`ListRowFace::two_line_secondary`].
    #[must_use]
    pub fn two_line_secondary(mut self) -> Self {
        self.two_line = true;
        self
    }

    /// See [`ListRowFace::meta`].
    #[must_use]
    pub fn meta(mut self, text: impl Into<String>) -> Self {
        self.meta = Some(text.into());
        self
    }

    /// A person's avatar in the leading column.
    #[must_use]
    pub fn avatar(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.leading = Some(Leading::Avatar {
            name: name.into(),
            identity: identity.into(),
            image: None,
        });
        self
    }

    /// The person's picture for the leading avatar.
    #[must_use]
    pub fn avatar_image(mut self, image: vello::peniko::ImageData) -> Self {
        if let Some(Leading::Avatar { image: slot, .. }) = &mut self.leading {
            *slot = Some(image);
        }
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.leading = Some(Leading::Icon(icon));
        self
    }

    /// See [`RowLeading::Badge`].
    #[must_use]
    pub fn badge(mut self, badge: impl Into<String>) -> Self {
        self.leading = Some(Leading::Badge(badge.into()));
        self
    }

    #[must_use]
    pub fn active(mut self) -> Self {
        self.status = Some((StatusKind::Active, String::new()));
        self
    }

    #[must_use]
    pub fn warning(mut self) -> Self {
        self.status = Some((StatusKind::Warning, String::new()));
        self
    }

    #[must_use]
    pub fn error(mut self) -> Self {
        self.status = Some((StatusKind::Error, String::new()));
        self
    }

    #[must_use]
    pub fn idle(mut self) -> Self {
        self.status = Some((StatusKind::Idle, String::new()));
        self
    }

    /// The presence dot of the person `identity`.
    #[must_use]
    pub fn presence(mut self, identity: impl Into<String>) -> Self {
        self.status = Some((StatusKind::Presence, identity.into()));
        self
    }

    #[must_use]
    pub fn disclosure(mut self) -> Self {
        self.disclosure = true;
        self
    }

    #[must_use]
    pub fn emphasized(mut self, emphasized: bool) -> Self {
        self.emphasized = emphasized;
        self
    }

    #[must_use]
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Set by the list the row sits in ([`crate::widget::Section`]): whether
    /// a separator runs under the row and whether the row below is
    /// selected, which hides it.
    #[cfg(test)]
    pub(crate) fn separator_state(&self) -> (bool, bool) {
        (self.separator, self.next_selected)
    }

    pub(crate) fn set_separator(&mut self, below: bool, next_selected: bool) {
        if (self.separator, self.next_selected) != (below, next_selected) {
            self.separator = below;
            self.next_selected = next_selected;
            self.dirty = true;
        }
    }

    /// Add an action revealed while the row is hovered or focused.
    #[must_use]
    pub fn action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        self.actions.push(OwnedAction {
            icon,
            label: label.into(),
            on_activate: Box::new(on_activate),
        });
        self
    }

    /// Swiping the row toward its leading edge reveals `icon`; releasing
    /// past the threshold runs `on_activate`.
    #[must_use]
    pub fn swipe_action(
        mut self,
        icon: Icon,
        label: impl Into<String>,
        destructive: bool,
        on_activate: impl FnMut() + 'static,
    ) -> Self {
        self.swipe = Some(OwnedSwipe {
            icon,
            label: label.into(),
            destructive,
            on_activate: Box::new(on_activate),
        });
        self
    }

    /// Run `f` when the row is activated.
    #[must_use]
    pub fn on_activate(mut self, f: impl FnMut() + 'static) -> Self {
        self.activation.set_callback(Box::new(f));
        self
    }

    /// Route activation through an exported resource action.
    #[must_use]
    pub fn on_action(mut self, binding: tilo_actions::ActionBinding<()>) -> Self {
        self.activation.set_action(binding);
        self
    }

    pub fn set_selected(&mut self, selected: bool) {
        if self.selected != selected {
            self.selected = selected;
            self.dirty = true;
        }
    }

    pub fn is_selected(&self) -> bool {
        self.selected
    }

    fn with_face<R>(&self, f: impl FnOnce(&ListRowFace<'_>) -> R) -> R {
        let actions: Vec<RowAction<'_>> = self
            .actions
            .iter()
            .map(|a| RowAction {
                icon: a.icon,
                label: &a.label,
            })
            .collect();
        let mut face = ListRowFace::new(&self.title)
            .emphasized(self.emphasized)
            .selected(self.selected)
            .actions(&actions)
            .hovered_action(self.hovered_action)
            .interaction(self.activation.state);
        if let Some(secondary) = &self.secondary {
            face = face.secondary(secondary);
            if self.two_line {
                face = face.two_line_secondary();
            }
        }
        if let Some(meta) = &self.meta {
            face = face.meta(meta);
        }
        if let Some(leading) = &self.leading {
            face = face.leading(match leading {
                Leading::Avatar {
                    name,
                    identity,
                    image,
                } => RowLeading::Avatar(AvatarFace::new(name, identity).image(image.as_ref())),
                Leading::Icon(icon) => RowLeading::Icon(*icon),
                Leading::Badge(badge) => RowLeading::Badge(badge),
            });
        }
        if let Some((kind, id)) = &self.status {
            face = face.status(match kind {
                StatusKind::Active => RowStatus::Active,
                StatusKind::Warning => RowStatus::Warning,
                StatusKind::Error => RowStatus::Error,
                StatusKind::Idle => RowStatus::Idle,
                StatusKind::Presence => RowStatus::Presence(id),
            });
        }
        if self.disclosure {
            face = face.disclosure();
        }
        if self.separator {
            face = face.separator(self.next_selected);
        }
        if let Some(swipe) = &self.swipe
            && self.swipe_offset > 0.0
        {
            face = face.swiped(
                SwipeAction {
                    icon: swipe.icon,
                    label: &swipe.label,
                    destructive: swipe.destructive,
                },
                self.swipe_offset,
            );
        }
        f(&face)
    }

    fn action_rects(&self) -> Vec<Rect> {
        let Some(metrics) = &self.metrics else {
            return Vec::new();
        };
        self.with_face(|face| face.action_rects_with(self.rect, metrics))
    }

    fn handle_swipe(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        use winit::event::TouchPhase;
        let WindowEvent::Touch(touch) = event else {
            return;
        };
        let x = touch.location.x / ctx.scale_factor;
        let y = touch.location.y / ctx.scale_factor;
        match touch.phase {
            TouchPhase::Started if self.rect.contains(Point::new(x, y)) => {
                self.swipe_origin = Some((touch.id, x));
            }
            TouchPhase::Moved => {
                if let Some((id, start)) = self.swipe_origin
                    && id == touch.id
                {
                    let offset = (start - x).clamp(0.0, self.rect.width());
                    if (offset - self.swipe_offset).abs() > f64::EPSILON {
                        self.swipe_offset = offset;
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                }
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                if let Some((id, _)) = self.swipe_origin
                    && id == touch.id
                {
                    let committed = touch.phase == TouchPhase::Ended
                        && self.swipe_offset >= self.rect.width() * SWIPE_COMMIT;
                    if committed && let Some(swipe) = &mut self.swipe {
                        (swipe.on_activate)();
                    }
                    if self.swipe_offset > 0.0 {
                        *ctx.handled = true;
                    }
                    self.swipe_origin = None;
                    self.swipe_offset = 0.0;
                    self.dirty = true;
                }
            }
            TouchPhase::Started => {}
        }
    }
}

impl Widget for ListRow {
    crate::provides!(dyn crate::widget::controls::ActionControls);

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(RowMetrics::new(theme));
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            min_size: taffy::Size {
                width: length(0.0_f32),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(metrics) = &self.metrics else {
            return taffy::Size::ZERO;
        };
        let lines = match (&self.secondary, self.two_line) {
            (None, _) => 0,
            (Some(_), false) => 1,
            (Some(_), true) => 2,
        };
        let height = metrics.height(text_renderer, lines);
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 0.0,
        });
        taffy::Size {
            width,
            height: crate::num::f32_of_f64(height),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
            ),
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        if rect != self.rect {
            self.dirty = true;
        }
        self.rect = rect;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if presence.should_cull(self.rect) {
            return;
        }
        let rect = self.rect;
        self.with_face(|face| {
            face.paint(
                &mut PaintCtx::new(scene, transform, text_renderer, theme),
                rect,
            );
        });
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.swipe.is_some() {
            self.handle_swipe(event, ctx);
        }
        let action_rects = self.action_rects();
        if let WindowEvent::CursorMoved { .. } = event {
            let changed = self
                .activation
                .handle_pointer(event, ctx, self.rect, &action_rects);
            let cursor = self.activation.cursor();
            let hovered = self.action_rects().iter().position(|r| r.contains(cursor));
            if changed || hovered != self.hovered_action {
                self.hovered_action = hovered;
                self.dirty = true;
            }
            return;
        }
        if let WindowEvent::MouseInput {
            state,
            button: winit::event::MouseButton::Left,
            ..
        } = event
        {
            let cursor = self.activation.cursor();
            let over = action_rects.iter().position(|r| r.contains(cursor));
            match (state, over, self.pressed_action) {
                (winit::event::ElementState::Pressed, Some(index), _) => {
                    self.pressed_action = Some(index);
                    *ctx.handled = true;
                    return;
                }
                (winit::event::ElementState::Released, _, Some(pressed)) => {
                    self.pressed_action = None;
                    *ctx.handled = true;
                    if over == Some(pressed)
                        && let Some(action) = self.actions.get_mut(pressed)
                    {
                        (action.on_activate)();
                    }
                    return;
                }
                _ => {}
            }
        }
        if self
            .activation
            .handle_pointer(event, ctx, self.rect, &action_rects)
        {
            self.dirty = true;
        }
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.activation.is_bound() && self.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        self.activation.is_bound()
    }

    fn set_focus(&mut self, focused: bool) {
        if self.activation.state.focused != focused {
            self.activation.state.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.activation.state.focused
    }

    fn is_enabled(&self) -> bool {
        !self.activation.state.disabled
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::AppModel,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        matches!(action, crate::a11y::Action::Activate) && self.activation.activate()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "ListRow"
    }

    fn test_id(&self) -> Option<&str> {
        Some(self.activation.id())
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let id = self.activation.id().to_owned();
        Some(
            self.with_face(|face| face.accessibility(self.rect, id))
                .with_semantic_key(self.activation.id()),
        )
    }
}

activation_controls!(ListRow, activation);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::headless::FrameHarness;
    use crate::widget::{Column, Element};
    use std::cell::Cell;
    use std::rc::Rc;

    fn face_height(face: &ListRowFace<'_>, scale: f32) -> f64 {
        let theme = Theme::dark().with_text_scale(scale);
        let mut text = TextRenderer::new();
        face.height(&mut text, &theme)
    }

    #[test]
    fn row_height_comes_from_measured_line_boxes_and_grows_with_text() {
        let single = ListRowFace::new("Inbox");
        let two = ListRowFace::new("Ada Lovelace").secondary("ada@example.org");
        let three = ListRowFace::new("Ada Lovelace")
            .secondary("The analytical engine weaves algebraic patterns")
            .two_line_secondary();
        let theme = Theme::dark();
        let floor = f64::from(theme.tokens.metrics.min_pointer_target + theme.tokens.spacing.s2);
        assert!(
            (face_height(&single, 1.0) - floor).abs() < 0.5,
            "a one-line row is 40"
        );
        assert!(face_height(&two, 1.0) > face_height(&single, 1.0));
        assert!(face_height(&three, 1.0) > face_height(&two, 1.0));
        assert!(face_height(&single, 2.0) > face_height(&single, 1.0));
        for face in [&two, &three] {
            assert!(
                face_height(face, 2.0) > face_height(face, 1.0) * 1.5,
                "rows grow with the text scale"
            );
        }
    }

    #[test]
    fn hover_actions_replace_the_meta_inside_the_row() {
        let theme = Theme::dark();
        let rect = Rect::new(0.0, 0.0, 400.0, 48.0);
        let actions = [
            RowAction {
                icon: Icon::Reply,
                label: "Reply",
            },
            RowAction {
                icon: Icon::Trash,
                label: "Delete message",
            },
        ];
        let idle = ListRowFace::new("Ada").meta("09:41").actions(&actions);
        assert!(idle.action_rects(rect, &theme).is_empty(), "hidden at rest");
        let hovered = idle.clone().interaction(Interaction {
            hovered: true,
            ..Interaction::default()
        });
        let rects = hovered.action_rects(rect, &theme);
        assert_eq!(rects.len(), 2);
        assert!(rects[0].x1 < rects[1].x0, "in order, not touching");
        for r in &rects {
            assert!(rect.contains(r.origin()) && r.x1 <= rect.x1);
            assert!(
                (r.center().y - rect.center().y).abs() < 0.01,
                "centred on the row"
            );
        }
    }

    #[test]
    fn separators_hide_around_the_selection() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let count = |face: ListRowFace<'_>, text: &mut TextRenderer| {
            let mut scene = Scene::new();
            face.paint(
                &mut PaintCtx::new(&mut scene, Affine::IDENTITY, text, &theme),
                Rect::new(0.0, 0.0, 300.0, 40.0),
            );
            scene.encoding().draw_tags.len()
        };
        let plain = count(ListRowFace::new("a"), &mut text);
        let separated = count(ListRowFace::new("a").separator(false), &mut text);
        let above_selection = count(ListRowFace::new("a").separator(true), &mut text);
        let selected_plain = count(ListRowFace::new("a").selected(true), &mut text);
        let selected = count(
            ListRowFace::new("a").selected(true).separator(false),
            &mut text,
        );
        assert_eq!(separated, plain + 1);
        assert_eq!(above_selection, plain);
        assert_eq!(selected, selected_plain);
    }

    #[test]
    fn a_selected_row_is_the_lift_on_every_preset() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let ground = theme.tokens.colors.surfaces.content;
            let fill = row_fill(&theme, true, false).unwrap();
            let moved = crate::surface::edge_delta(fill, ground);
            assert!(
                moved >= crate::tokens::colors::HAIRLINE_MIN_DELTA,
                "{}: selection moved the ground only {moved:.4}",
                theme.name
            );
            assert!(
                row_fill(&theme, false, false).is_none(),
                "idle is transparent"
            );
        }
    }

    #[test]
    fn badge_column_aligns_titles_across_rows() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let column = badge_column_width(&mut text, &theme, &["GET", "POST", "DELETE"]);
        let m = RowMetrics::new(&theme);
        let rect = Rect::new(0.0, 0.0, 400.0, 40.0);
        let x = |badge: &str, text: &mut TextRenderer| {
            ListRowFace::new("/users")
                .leading(RowLeading::Badge(badge))
                .leading_column(column)
                .text_x(rect, &m, text, &theme)
        };
        let get = x("GET", &mut text);
        assert!((get - x("DELETE", &mut text)).abs() < 0.01);
        assert!(get > column);
    }

    #[test]
    fn retained_row_activates_by_pointer_and_accessibility() {
        let hits = Rc::new(Cell::new(0));
        let counter = Rc::clone(&hits);
        let rows: Element = Box::new(
            Column::new().with_no_padding().push(Box::new(
                ListRow::new("contact-ada", "Ada Lovelace")
                    .secondary("ada@example.org")
                    .avatar("Ada Lovelace", "ada@example.org")
                    .meta("Today")
                    .on_activate(move || counter.set(counter.get() + 1)),
            )),
        );
        let mut harness = FrameHarness::new(crate::State::new(), rows, (400, 200));
        fn find(node: &crate::headless::InspectionNode) -> Option<[f64; 4]> {
            if node.widget == "ListRow" {
                return Some(node.rect);
            }
            node.children.iter().find_map(find)
        }
        let [x, y, w, h] = find(&harness.inspection().root).expect("row laid out");
        assert!(h >= 40.0);
        harness.click_at(x + w / 2.0, y + h / 2.0);
        assert_eq!(hits.get(), 1);
        let mut row = ListRow::new("r", "Row").on_activate(|| {});
        let node = Widget::accessibility(&row).unwrap();
        assert_eq!(node.role, Role::ListItem);
        assert_eq!(node.name.as_deref(), Some("Row"));
        let mut doc = automerge::AutoCommit::new();
        assert!(Widget::perform_action(
            &mut row,
            &crate::a11y::Action::Activate,
            &mut doc,
            &automerge::ROOT,
        ));
    }
}
