//! `SearchResultsView` — the shared results overlay for Tilo global search.
//!
//! Renders the ranked, kind-grouped [`SearchResult`]s
//! that a [`SearchRegistry`](crate::search::SearchRegistry) produces: one
//! section per kind, then keyboard- and pointer-navigable rows. Activating a
//! row latches its opaque `action_id`, which the host drains with
//! [`SearchResultsView::take_activated`] and dispatches on — the framework
//! never interprets it.
//!
//! The view is **self-laid-out** (like the command palette): the host gives
//! it a rect — via the layout pass, or directly with
//! [`SearchResultsView::set_rect`] for a free-floating overlay — and it lays
//! its own headers and rows top-down from the current groups.

use crate::widget::{AppModel, ModelPath};
use std::collections::HashMap;

use taffy::style_helpers::{length, percent};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::search::{SearchGroup, SearchResult};
use crate::text_render::{TextRenderer, TextWeight};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::TaffyTreeExt;
use crate::widget::Widget;
use crate::widget::data_table::{text_band, type_line_height};

const PAD: f64 = crate::tokens::Spacing::BASE.s2 as f64; // card inner padding
const ROW_INSET: f64 = 8.0; // rows inset from the card edge
const HEADER_H: f64 = 26.0;
const ROW_H_ONE_LINE: f64 = 38.0;
const ROW_H_TWO_LINE: f64 = 50.0;
const ROW_TEXT_X: f64 = 12.0; // text/icon inset within a row
const ICON_D: f64 = 18.0;
const ICON_COL: f64 = 30.0; // icon box + gap when a row has an icon
const EMPTY_H: f64 = 92.0;
/// A lifted row (selection, hover) is inset this much top and bottom, so a
/// selected row and its hovered neighbour keep one `s1` between them instead
/// of fusing into one block (HIG §7 "nothing touches").
const ROW_LIFT_INSET: f64 = crate::tokens::Spacing::BASE.s1 as f64 / 2.0;
const PREVIEW_H: f64 = 68.0;

/// Composes two affine transforms. `Affine`'s `Mul` impl is pure `f64` matrix
/// multiply-add with no overflow trap, so it cannot panic regardless of
/// operand magnitude — centralised here rather than repeating the exemption
/// at every text transform composed below.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine composition is float matrix math and cannot panic"
)]
fn compose(a: Affine, b: Affine) -> Affine {
    a * b
}

/// Used by both `draw` and hit-testing so they never disagree.
struct RowGeom {
    rect: Rect,
    flat: usize,
    gi: usize,
    ri: usize,
}

struct LayoutPlan {
    headers: Vec<(Rect, String, usize)>, // (header rect, kind, result count)
    rows: Vec<RowGeom>,
}

/// Row and header bands. The constants are the 1× defaults; `build_layout`
/// resolves them from the theme's type so a 2× title still has its 1×
/// padding instead of overrunning the next row. A free-floating overlay
/// that never goes through layout keeps the defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowBands {
    header: f64,
    one_line: f64,
    two_line: f64,
}

impl RowBands {
    const DEFAULT: Self = Self {
        header: HEADER_H,
        one_line: ROW_H_ONE_LINE,
        two_line: ROW_H_TWO_LINE,
    };

    /// Title (`base`) and subtitle (`sm`) lines plus one sibling step of
    /// padding; the caption band holds its `xs` line plus its two `s1`
    /// insets. Every value floors at the pinned 1× constant.
    fn resolve(theme: &Theme) -> Self {
        let tokens = &theme.tokens;
        let title = type_line_height(&tokens.typography, TextRole::Body);
        let subtitle = type_line_height(&tokens.typography, TextRole::Caption);
        let caption = type_line_height(&tokens.typography, TextRole::Caption);
        let pad = f64::from(tokens.spacing.s3);
        let gap = f64::from(tokens.spacing.s1);
        Self {
            header: text_band(HEADER_H, caption, 2.0 * gap),
            one_line: text_band(ROW_H_ONE_LINE, title, pad),
            two_line: text_band(ROW_H_TWO_LINE, title + gap + subtitle, pad),
        }
    }
}

/// The results overlay. Populate it with [`set_groups`](Self::set_groups) each
/// time the query changes, drive selection with the keyboard or pointer, and
/// drain activations with [`take_activated`](Self::take_activated).
pub struct SearchResultsView {
    groups: Vec<SearchGroup>,
    /// flat index → (group idx, result idx) for the selectable rows.
    flat: Vec<(usize, usize)>,
    cursor: usize,
    hover: Option<usize>,
    rect: Rect,
    last_cursor: Point,
    activated: Option<String>,
    empty_text: String,
    test_id: Option<String>,
    bands: RowBands,
}

impl SearchResultsView {
    pub fn new() -> Self {
        Self {
            groups: Vec::new(),
            flat: Vec::new(),
            cursor: 0,
            hover: None,
            rect: Rect::ZERO,
            last_cursor: Point::ZERO,
            activated: None,
            empty_text: "No results".to_string(),
            test_id: None,
            bands: RowBands::DEFAULT,
        }
    }

    /// Override the centered empty-state line shown when there are no results.
    #[must_use]
    pub fn with_empty_text(mut self, text: impl Into<String>) -> Self {
        self.empty_text = text.into();
        self
    }

    /// Update the empty-state copy without rebuilding the shared result view.
    pub fn set_empty_text(&mut self, text: impl Into<String>) {
        self.empty_text = text.into();
    }

    /// Give the overlay a stable test id so scenarios can resolve it.
    #[must_use]
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Rebuilds the flat selection index and clamps the cursor so it always
    /// points at a real row (or `0` when empty).
    pub fn set_groups(&mut self, groups: Vec<SearchGroup>) {
        self.flat.clear();
        for (gi, g) in groups.iter().enumerate() {
            for ri in 0..g.results.len() {
                self.flat.push((gi, ri));
            }
        }
        self.groups = groups;
        if self.cursor >= self.flat.len() {
            self.cursor = self.flat.len().saturating_sub(1);
        }
        // Hover no longer maps to a known row until the pointer moves again.
        self.hover = None;
    }

    /// Buckets by kind in first-seen order, for callers not going through the registry's grouping.
    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        let mut groups: Vec<SearchGroup> = Vec::new();
        for r in results {
            if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
                g.results.push(r);
            } else {
                groups.push(SearchGroup {
                    kind: r.kind.clone(),
                    results: vec![r],
                });
            }
        }
        self.set_groups(groups);
    }

    pub fn groups(&self) -> &[SearchGroup] {
        &self.groups
    }

    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    pub fn len(&self) -> usize {
        self.flat.len()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Clamped to the row range.
    pub fn set_cursor(&mut self, index: usize) {
        if !self.flat.is_empty() {
            self.cursor = index.min(self.flat.len().saturating_sub(1));
        }
    }

    /// Preserve one exact control across an asynchronous result replacement.
    /// A missing identity leaves no actionable selection; it never falls back
    /// to the row that inherited the old index.
    pub(crate) fn restore_action_selection(&mut self, id: &str) -> bool {
        let index = self
            .flat
            .iter()
            .position(|&(group, row)| self.groups[group].results[row].action_id == id);
        self.cursor = index.unwrap_or(usize::MAX);
        index.is_some()
    }

    pub fn selected(&self) -> Option<&SearchResult> {
        let (gi, ri) = *self.flat.get(self.cursor)?;
        self.groups.get(gi)?.results.get(ri)
    }

    fn preview_result(&self) -> Option<&SearchResult> {
        let at = |flat: usize| {
            let (gi, ri) = *self.flat.get(flat)?;
            self.groups
                .get(gi)?
                .results
                .get(ri)
                .filter(|result| result.preview.is_some())
        };
        self.hover.and_then(at).or_else(|| at(self.cursor))
    }

    fn has_previews(&self) -> bool {
        self.groups
            .iter()
            .flat_map(|group| &group.results)
            .any(|result| result.preview.is_some())
    }

    pub fn selected_action_id(&self) -> Option<&str> {
        self.selected().map(|r| r.action_id.as_str())
    }

    /// Move the highlight down one row, wrapping to the top past the end.
    pub fn move_down(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        if self.selected().is_none() {
            self.cursor = 0;
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(1)
            .checked_rem(self.flat.len())
            .unwrap_or(0);
    }

    /// Move the highlight up one row, wrapping to the bottom past the top.
    pub fn move_up(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        if self.selected().is_none() {
            self.cursor = self.flat.len() - 1;
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(self.flat.len())
            .saturating_sub(1)
            .checked_rem(self.flat.len())
            .unwrap_or(0);
    }

    /// No-op when there is no selection.
    pub fn activate_selected(&mut self) {
        if let Some(id) = self.selected().map(|r| r.action_id.clone()) {
            self.activated = Some(id);
        }
    }

    /// `None` until a row is activated; drains after one read.
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }

    /// For a free-floating overlay not driven by the layout pass.
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// Falls back to an empty-state height with no results.
    pub fn desired_height(&self) -> f64 {
        if self.groups.is_empty() {
            return EMPTY_H;
        }
        let mut h = PAD * 2.0;
        for g in &self.groups {
            h += self.bands.header;
            for r in &g.results {
                h += self.row_height(r);
            }
        }
        if self.has_previews() {
            h += PREVIEW_H;
        }
        h
    }

    fn row_height(&self, r: &SearchResult) -> f64 {
        if r.subtitle.is_some() {
            self.bands.two_line
        } else {
            self.bands.one_line
        }
    }

    /// Single source of truth shared by `draw` and `row_at`, so a click
    /// always lands on the row that was drawn.
    fn plan(&self) -> LayoutPlan {
        let mut headers = Vec::new();
        let mut rows = Vec::new();
        let x0 = self.rect.x0 + ROW_INSET;
        let x1 = self.rect.x1 - ROW_INSET;
        let mut y = self.rect.y0 + PAD;
        let mut flat = 0usize;
        let header_h = self.bands.header;
        for (gi, g) in self.groups.iter().enumerate() {
            headers.push((
                Rect::new(x0, y, x1, y + header_h),
                g.kind.clone(),
                g.results.len(),
            ));
            y += header_h;
            for (ri, r) in g.results.iter().enumerate() {
                let rh = self.row_height(r);
                rows.push(RowGeom {
                    rect: Rect::new(x0, y, x1, y + rh),
                    flat,
                    gi,
                    ri,
                });
                y += rh;
                flat = flat.saturating_add(1);
            }
        }
        LayoutPlan { headers, rows }
    }

    pub fn row_at(&self, p: Point) -> Option<usize> {
        self.plan()
            .rows
            .into_iter()
            .find(|r| r.rect.contains(p))
            .map(|r| r.flat)
    }

    fn draw_row(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: &RowGeom,
    ) {
        let colors = &theme.tokens.colors;
        let Some(result) = self.groups.get(row.gi).and_then(|g| g.results.get(row.ri)) else {
            return;
        };
        let selected = row.flat == self.cursor;
        let hovered = self.hover == Some(row.flat);
        let selected_ink = colors.intent.on_inverted;

        // Selection = a filled row (per the command palette convention);
        // hover = a barely-there veneer, distinct from selection.
        let lift = RoundedRect::from_rect(
            row.rect.inflate(0.0, -ROW_LIFT_INSET),
            f64::from(theme.tokens.radii.detail),
        );
        if selected {
            scene.fill(
                Fill::NonZero,
                transform,
                colors.intent.inverted,
                None,
                &lift,
            );
        } else if hovered {
            // One step off the panel it sits on, polarity chosen the way
            // `hairline_on` chooses it — the white veneer this replaces was
            // invisible over a light panel (HIG §9.5).
            let hover = crate::tokens::colors::Colors::hairline_on(
                crate::chrome::shell::panel_ground(theme),
                crate::chrome::shell::HAIRLINE_ALPHA,
            );
            scene.fill(Fill::NonZero, transform, hover, None, &lift);
        }

        let mut text_x = row.rect.x0 + ROW_TEXT_X;

        // Leading glyph (icon color follows selection so it reads on the fill).
        if let Some(icon) = result.icon {
            let icon_rect = Rect::from_center_size(
                Point::new(text_x + ICON_D / 2.0, row.rect.center().y),
                (ICON_D, ICON_D),
            );
            let icon_col = if selected {
                selected_ink
            } else {
                colors.text_dim
            };
            icon.draw_with_stroke(scene, transform, icon_rect, icon_col, 1.6);
            text_x += ICON_COL;
        }

        let tokens = &theme.tokens;
        let title_style = tokens.typography.style(TextRole::Body);
        let sub_style = tokens.typography.style(TextRole::Caption);
        // Clamped to >= 0.0 above; a row's text width is a small on-screen
        // pixel span, well inside `f32`'s exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "row text width is a small non-negative on-screen pixel span"
        )]
        let text_w = (row.rect.x1 - text_x - ROW_TEXT_X).max(0.0) as f32;

        // Title and subtitle are measured with the width they are drawn at and
        // centred as one block, so the icon (centred on the row) sits on the
        // block's centre line whether or not a subtitle is present.
        let (_, title_h) = tr.measure_style(
            &result.title,
            Some(text_w),
            &title_style.clone().with_weight(TextWeight::Medium),
        );
        let gap = f64::from(tokens.spacing.s1);
        let sub_h = result
            .subtitle
            .as_deref()
            .map(|sub| f64::from(tr.measure_style(sub, Some(text_w), &sub_style).1));
        let block_h = f64::from(title_h) + sub_h.map_or(0.0, |h| gap + h);
        let title_y = row.rect.y0 + ((row.rect.height() - block_h) / 2.0).max(0.0);
        tr.draw_style(
            scene,
            &result.title,
            if selected {
                selected_ink
            } else {
                colors.text_primary
            },
            compose(transform, Affine::translate((text_x, title_y))),
            Some(text_w),
            &title_style.clone().with_weight(TextWeight::Medium),
        );

        // Subtitle (dim; on a selected row it stays readable on the fill).
        if let Some(sub) = &result.subtitle {
            let sy = title_y + f64::from(title_h) + gap;
            let sub_col = if selected {
                selected_ink
            } else {
                colors.text_dim
            };
            tr.draw_style(
                scene,
                sub,
                sub_col,
                compose(transform, Affine::translate((text_x, sy))),
                Some(text_w),
                &sub_style,
            );
        }
    }

    fn draw_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let style = theme.text(TextRole::Caption);
        let (tw, th) = tr.measure_style(&self.empty_text, None, &style);
        let cx = self.rect.center().x - f64::from(tw) / 2.0;
        let cy = self.rect.center().y - f64::from(th) / 2.0;
        tr.draw_style(
            scene,
            &self.empty_text,
            theme.tokens.colors.text_dim,
            compose(transform, Affine::translate((cx, cy))),
            None,
            &style,
        );
    }

    fn draw_preview(
        &self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if !self.has_previews() {
            return;
        }
        let Some(result) = self.preview_result() else {
            return;
        };
        let Some(preview) = result.preview.as_deref() else {
            return;
        };
        let rect = Rect::new(
            self.rect.x0 + ROW_INSET,
            self.rect.y1 - PREVIEW_H,
            self.rect.x1 - ROW_INSET,
            self.rect.y1 - PAD,
        );
        crate::surface::separator(
            scene,
            transform,
            rect.x0,
            rect.x1,
            rect.y0,
            0.0,
            theme.tokens.colors.surfaces.base,
        );
        let title_style = theme.text(TextRole::Subheading);
        let body_style = theme.text(TextRole::Caption);
        tr.draw_style(
            scene,
            "Preview",
            theme.tokens.colors.text_dim,
            compose(
                transform,
                Affine::translate((rect.x0 + 4.0, rect.y0 + 10.0)),
            ),
            None,
            &title_style,
        );
        let normalized = preview.split_whitespace().collect::<Vec<_>>().join(" ");
        let available = (rect.width() - 8.0).max(0.0);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded on-screen preview width"
        )]
        let body = tr.elide_to_width_style(&normalized, available as f32, &body_style);
        tr.draw_style(
            scene,
            &body,
            theme.tokens.colors.text_primary,
            compose(
                transform,
                Affine::translate((rect.x0 + 4.0, rect.y0 + 32.0)),
            ),
            None,
            &body_style,
        );
    }
}

impl Default for SearchResultsView {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchResultsView {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.bands = RowBands::resolve(theme);
        // A results overlay's desired height is a small on-screen pixel value
        // (a handful of rows), well inside `f32`'s exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "desired overlay height is a small on-screen pixel value"
        )]
        let height_px = self.desired_height() as f32;
        let node = taffy.leaf_node(Style {
            size: taffy::Size {
                width: percent(1.0_f32),
                height: length(height_px),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        if self.rect.width() <= 0.0 {
            return;
        }
        // Panel role (HIG §9.1): a results overlay floats over content, so it
        // takes the panel ground and the panel edge — the declared
        // `border_standard` this replaces vanished on the dark preset's
        // raised surface.
        let radius = f64::from(theme.tokens.radii.control);
        let card = RoundedRect::from_rect(self.rect, radius);
        scene.fill(
            Fill::NonZero,
            transform,
            crate::chrome::shell::panel_ground(theme),
            None,
            &card,
        );
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            crate::chrome::shell::panel_border(theme),
            None,
            &crate::surface::edge_outline(self.rect, radius),
        );

        if self.flat.is_empty() {
            self.draw_empty(scene, transform, text_renderer, theme);
            return;
        }

        let plan = self.plan();
        // Section headers: dim, uppercase, with a trailing count.
        let header_style = theme.text(TextRole::Caption);
        let header_inset = f64::from(theme.tokens.spacing.s1);
        for (hrect, kind, count) in &plan.headers {
            let label = kind.to_uppercase();
            // The caption sits on the band's bottom edge, one small step above
            // its rows, by its measured line box rather than its font size.
            let (_, label_h) = text_renderer.measure_style(
                &label,
                None,
                &header_style.clone().with_weight(TextWeight::Semibold),
            );
            let hy = hrect.y1 - f64::from(label_h) - header_inset;
            text_renderer.draw_style(
                scene,
                &label,
                theme.tokens.colors.text_dim,
                compose(transform, Affine::translate((hrect.x0 + header_inset, hy))),
                None,
                &header_style.clone().with_weight(TextWeight::Semibold),
            );
            let count_str = count.to_string();
            let (cw, _) = text_renderer.measure_style(&count_str, None, &header_style);
            text_renderer.draw_style(
                scene,
                &count_str,
                theme.tokens.colors.text_dim,
                compose(
                    transform,
                    Affine::translate((hrect.x1 - f64::from(cw) - header_inset, hy)),
                ),
                None,
                &header_style,
            );
        }
        for row in &plan.rows {
            self.draw_row(scene, transform, text_renderer, theme, row);
        }
        self.draw_preview(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.hover = self.row_at(self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(flat) = self.row_at(self.last_cursor) {
                    self.cursor = flat;
                    self.activate_selected();
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                if self.flat.is_empty() {
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_down();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_up();
                        *ctx.handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.activate_selected();
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "SearchResultsView"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _: &mut AppModel,
        _: &ModelPath,
    ) -> bool {
        if !matches!(
            action,
            crate::a11y::Action::Focus | crate::a11y::Action::Activate
        ) {
            return false;
        }
        let Some(index) = self
            .flat
            .iter()
            .position(|&(group, row)| self.groups[group].results[row].action_id == target)
        else {
            return false;
        };
        self.cursor = index;
        if matches!(action, crate::a11y::Action::Activate) {
            self.activate_selected();
        }
        true
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let total = self.flat.len();
        let children = if self.flat.is_empty() {
            vec![
                AccessNode::new(Role::StaticText)
                    .with_name(&self.empty_text)
                    .with_live(crate::a11y::Politeness::Polite),
            ]
        } else {
            self.plan()
                .rows
                .into_iter()
                .filter_map(|row| {
                    let result = self.groups.get(row.gi)?.results.get(row.ri)?;
                    let description = result
                        .subtitle
                        .iter()
                        .chain(result.preview.iter())
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(". ");
                    let mut node = AccessNode::new(Role::ListItem)
                        .with_name(result.title.clone())
                        .with_action_target(&result.action_id)
                        .with_bounds(row.rect)
                        .with_position(row.flat.saturating_add(1), total)
                        .with_state(AccessState::new().selected(row.flat == self.cursor));
                    if !description.is_empty() {
                        node = node.with_description(description);
                    }
                    Some(node)
                })
                .collect()
        };
        Some(
            AccessNode::new(Role::List)
                .with_name("Search results")
                .with_children(children),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchRegistry;
    use automerge::AutoCommit;

    /// Relative luminance of the composited pixel at (`x`, `y`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside the test surface"
    )]
    fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        crate::a11y::relative_luminance(vello::peniko::Color::from_rgb8(r, g, b))
    }

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(name))
    }

    /// The results panel keeps a **visible derived edge** at 1× on both
    /// presets (HIG §9.1 Panel role, §9.5): before, it was a raised card under
    /// `border_standard`, three shades from the dark preset's raised surface.
    /// The hovered row's veneer was literal white at 5 %, invisible on light.
    ///
    /// Writes `$UX1X_F4_OUT/<theme>/search_results.png` plus the harness
    /// inspection when that variable is set; asserts regardless.
    #[test]
    fn ux1x_panel_edge_and_hover_register_on_both_presets() {
        use crate::offscreen::OffscreenRenderer;
        const EDGE_MIN: f64 = 0.008;
        // Ground of margin around the panel so its edge is against the app.
        let margin = 16.0;
        // Evidence first, for both presets, so a failing assertion on one
        // preset still leaves the other's PNG and inspection on disk.
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            let mut mounted = view_from("");
            let last = mounted.len() - 1;
            let (w, h) = (
                margin * 2.0 + 360.0,
                margin * 2.0 + mounted.desired_height(),
            );
            if let Some(png) = ux1x_out(&theme, "search_results.png") {
                mounted.set_cursor(0);
                mounted.hover = Some(last);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a small test viewport"
                )]
                let harness = crate::headless::FrameHarness::new_with_theme(
                    crate::State::new(),
                    Box::new(mounted),
                    (w as u32, h as u32),
                    theme.clone(),
                );
                harness.render_window_png(&png);
                if let Ok(json) = harness.inspection_json() {
                    std::fs::write(png.with_extension("inspection.json"), json)
                        .expect("write inspection");
                }
            }
        }
        for theme in [Theme::dark(), Theme::light()] {
            let theme = theme.with_reduce_motion(true);
            let mut v = view_from("");
            v.set_cursor(0);
            let panel = Rect::new(margin, margin, margin + 360.0, margin + v.desired_height());
            v.set_rect(panel);
            // Hover the last row so the veneer is under test too.
            let last = v.len() - 1;
            v.hover = Some(last);
            let (w, h) = (panel.x1 + margin, panel.y1 + margin);
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            v.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small test surface"
            )]
            let rgba = OffscreenRenderer::new(w as u32, h as u32).render(&scene, theme.bg_app);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a small test surface"
            )]
            let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

            // Panel edge against the panel ground just inside it, on the top
            // padding row above the first header (nothing else paints there).
            let y = panel.y0 + PAD / 2.0;
            let edge = lum(panel.x0, y);
            let ground = lum(panel.x0 + 3.0, y);
            assert!(
                (edge - ground).abs() >= EDGE_MIN,
                "{}: panel edge {edge:.4} vs ground {ground:.4} did not register",
                theme.name
            );

            // Hovered row against the bare panel ground beside its inset.
            let row = v
                .plan()
                .rows
                .into_iter()
                .find(|r| r.flat == last)
                .expect("the hovered row is laid out")
                .rect;
            let hover = lum(row.x1 - 3.0, row.center().y);
            let beside = lum(panel.x1 - ROW_INSET / 2.0, row.center().y);
            assert!(
                (hover - beside).abs() >= EDGE_MIN,
                "{}: hover veneer {hover:.4} vs ground {beside:.4} did not register",
                theme.name
            );
        }
    }

    fn view_from(query: &str) -> SearchResultsView {
        let mut reg = SearchRegistry::new();
        reg.register_fn("Contacts", |_q| {
            vec![
                SearchResult::new("Contacts", "Alice Ng", "c:alice")
                    .with_subtitle("alice@example.com"),
                SearchResult::new("Contacts", "Alan Turing", "c:alan"),
            ]
        });
        reg.register_fn("Apps", |_q| {
            vec![SearchResult::new("Apps", "Calculator", "app:calc")]
        });
        let mut v = SearchResultsView::new();
        v.set_groups(reg.query_grouped(query));
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        v
    }

    #[test]
    fn result_groups_index_preview_and_size_the_panel() {
        // Case `set_groups_builds_flat_index_and_clamps_cursor`.
        {
            let mut v = view_from("");
            assert_eq!(v.len(), 3, "two contacts + one app");
            v.set_cursor(99);
            assert_eq!(v.cursor(), v.len() - 1, "cursor clamps into range");
            // Shrinking the list re-clamps.
            v.set_groups(vec![SearchGroup {
                kind: "Apps".into(),
                results: vec![SearchResult::new("Apps", "Calculator", "app:calc")],
            }]);
            assert_eq!(v.cursor(), 0);
        }

        // Case `preview_reserves_fixed_space_and_is_announced`.
        {
            let mut view = SearchResultsView::new();
            view.set_results(vec![
                SearchResult::new("Notes", "Design notes", "open")
                    .with_subtitle("Edited today")
                    .with_preview("A bounded note excerpt"),
                SearchResult::new("Notes", "Plain note", "plain"),
            ]);
            let with_preview = view.desired_height();
            view.set_cursor(1);
            assert_eq!(
                view.desired_height(),
                with_preview,
                "selection never changes result geometry"
            );
            let access = Widget::accessibility(&view).expect("result list semantics");
            assert!(
                access.children[0]
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains("bounded note excerpt"))
            );
        }

        // Case `empty_results_show_empty_state_and_no_activation`.
        {
            let mut v = SearchResultsView::new().with_empty_text("Nothing here");
            v.set_groups(vec![]);
            v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
            assert!(v.is_empty());
            assert_eq!(v.selected(), None);
            let access = Widget::accessibility(&v).expect("empty result list semantics");
            assert_eq!(access.children.len(), 1);
            assert_eq!(access.children[0].role, crate::a11y::Role::StaticText);
            assert_eq!(access.children[0].name.as_deref(), Some("Nothing here"));
            assert_eq!(
                access.children[0].live,
                Some(crate::a11y::Politeness::Polite),
                "empty results remain a polite live announcement"
            );
            v.activate_selected();
            assert_eq!(v.take_activated(), None, "no row ⇒ nothing to activate");
        }

        // Case `desired_height_grows_with_content`.
        {
            let empty = SearchResultsView::new().desired_height();
            let full = view_from("").desired_height();
            assert!(
                full > empty,
                "populated overlay is taller than the empty state"
            );
        }
    }

    #[test]
    fn results_activate_from_keyboard_and_pointer() {
        // Case `keyboard_navigation_wraps_and_enter_activates`.
        {
            let mut v = view_from("");
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let taffy = TaffyTree::<()>::new();
            let mods = winit::event::Modifiers::default();
            let mut handled = false;
            let mut ft = None;
            let node = NodeId::new(0);

            let key = |k: NamedKey| {
                // SAFETY: all pub fields of `KeyEvent` tolerate the all-zero bit
                // pattern; `handle_event` only inspects `logical_key` + `state`.
                let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
                ev.logical_key = Key::Named(k);
                ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
                    winit::keyboard::NativeKeyCode::Unidentified,
                );
                ev.location = winit::keyboard::KeyLocation::Standard;
                ev.state = ElementState::Pressed;
                ev.repeat = false;
                ev.text = None;
                WindowEvent::KeyboardInput {
                    device_id: unsafe { std::mem::zeroed() },
                    event: ev,
                    is_synthetic: false,
                }
            };

            assert_eq!(v.cursor(), 0);
            v.handle_event(
                &key(NamedKey::ArrowDown),
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(handled, "arrow-down consumed");
            assert_eq!(v.cursor(), 1);
            // Wrap: up from the top lands on the last row.
            v.set_cursor(0);
            v.handle_event(
                &key(NamedKey::ArrowUp),
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert_eq!(v.cursor(), v.len() - 1, "up wraps to the bottom");

            // Enter activates the highlighted row and drains once.
            v.set_cursor(0);
            v.handle_event(
                &key(NamedKey::Enter),
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert_eq!(
                v.take_activated().as_deref(),
                Some("c:alice"),
                "enter fires the selected action-id"
            );
            assert_eq!(v.take_activated(), None, "activation drains after one read");
        }

        // Case `clicking_a_row_selects_and_activates_it`.
        {
            let mut v = view_from("");
            // The plan is the same geometry draw uses; target the third row (the App).
            let plan = v.plan();
            assert_eq!(plan.rows.len(), 3);
            let target = plan.rows[2].rect.center();

            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let taffy = TaffyTree::<()>::new();
            let mods = winit::event::Modifiers::default();
            let mut handled = false;
            let mut ft = None;
            let node = NodeId::new(0);

            v.handle_event(
                &WindowEvent::CursorMoved {
                    device_id: unsafe { std::mem::zeroed() },
                    position: winit::dpi::PhysicalPosition::new(target.x, target.y),
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            v.handle_event(
                &WindowEvent::MouseInput {
                    device_id: unsafe { std::mem::zeroed() },
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                },
                &mut crate::widget::EventCtx {
                    scale_factor: 1.0,
                    modifiers: &mods,
                    state: &mut doc,
                    path: &root,
                    handled: &mut handled,
                    focus_target: &mut ft,
                    node_id: node,
                    taffy: &taffy,
                },
            );
            assert!(handled, "row click consumed");
            assert_eq!(
                v.cursor(),
                2,
                "click moves the highlight to the clicked row"
            );
            assert_eq!(
                v.take_activated().as_deref(),
                Some("app:calc"),
                "click fires that row's action-id"
            );
        }
    }
}
