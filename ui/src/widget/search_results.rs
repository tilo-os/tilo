//! `SearchResultsView` — the shared results overlay for qOS global search.
//!
//! It renders the ranked, kind-grouped [`SearchResult`](crate::search::SearchResult)s
//! that a [`SearchRegistry`](crate::search::SearchRegistry) produces: one
//! section per kind (a dim uppercase header with a count), then keyboard- and
//! pointer-navigable rows. Activating a row (Enter, or a click) latches its
//! opaque `action_id`, which the host drains with
//! [`SearchResultsView::take_activated`] and dispatches on — the framework never
//! interprets it.
//!
//! Pair it with a [`SearchField`](crate::widget::SearchField) for the input and
//! the registry for the data, and an app has a complete, OS-consistent search
//! surface without hand-rolling a list, a highlight, or a ranker. The same
//! overlay backs a future shell-level Ctrl+K driving every app's providers.
//!
//! The view is **self-laid-out** (like the command palette): the host gives it
//! a rect — via the layout pass, or directly with [`SearchResultsView::set_rect`]
//! for a free-floating overlay — and it lays its own headers and rows top-down
//! from the current groups.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
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
use crate::widget::Widget;

const PAD: f64 = 8.0; // card inner padding
const ROW_INSET: f64 = 8.0; // rows inset from the card edge
const HEADER_H: f64 = 26.0;
const ROW_H_ONE_LINE: f64 = 38.0;
const ROW_H_TWO_LINE: f64 = 50.0;
const ROW_TEXT_X: f64 = 12.0; // text/icon inset within a row
const ICON_D: f64 = 18.0;
const ICON_COL: f64 = 30.0; // icon box + gap when a row has an icon
const EMPTY_H: f64 = 92.0;

/// Where one selectable row lands, plus its flat index and its (group, result)
/// coordinates — used by both `draw` and hit-testing so they never disagree.
struct RowGeom {
    rect: Rect,
    flat: usize,
    gi: usize,
    ri: usize,
}

/// A laid-out plan for the current groups against [`SearchResultsView::rect`]:
/// section headers and selectable rows, computed once and shared.
struct LayoutPlan {
    headers: Vec<(Rect, String, usize)>, // (header rect, kind, result count)
    rows: Vec<RowGeom>,
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
        }
    }

    /// Override the centered empty-state line shown when there are no results.
    pub fn with_empty_text(mut self, text: impl Into<String>) -> Self {
        self.empty_text = text.into();
        self
    }

    /// Give the overlay a stable test id so scenarios can resolve it.
    pub fn with_test_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Replace the displayed results. Rebuilds the flat selection index and
    /// clamps the cursor so it always points at a real row (or `0` when empty).
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

    /// Convenience: set from a flat result list, bucketing by kind in first-seen
    /// order (for callers that don't go through the registry's grouping).
    pub fn set_results(&mut self, results: Vec<SearchResult>) {
        let mut groups: Vec<SearchGroup> = Vec::new();
        for r in results {
            if let Some(g) = groups.iter_mut().find(|g| g.kind == r.kind) {
                g.results.push(r);
            } else {
                groups.push(SearchGroup { kind: r.kind.clone(), results: vec![r] });
            }
        }
        self.set_groups(groups);
    }

    /// The groups currently displayed.
    pub fn groups(&self) -> &[SearchGroup] {
        &self.groups
    }

    /// Whether there are no selectable rows.
    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    /// Number of selectable rows across all groups.
    pub fn len(&self) -> usize {
        self.flat.len()
    }

    /// The highlighted flat row index.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the highlight to `index` (clamped to the row range).
    pub fn set_cursor(&mut self, index: usize) {
        if !self.flat.is_empty() {
            self.cursor = index.min(self.flat.len() - 1);
        }
    }

    /// The currently-highlighted result, if any.
    pub fn selected(&self) -> Option<&SearchResult> {
        let (gi, ri) = *self.flat.get(self.cursor)?;
        self.groups.get(gi)?.results.get(ri)
    }

    /// The highlighted row's `action_id`, if any.
    pub fn selected_action_id(&self) -> Option<&str> {
        self.selected().map(|r| r.action_id.as_str())
    }

    /// Move the highlight down one row, wrapping to the top past the end.
    pub fn move_down(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.cursor = (self.cursor + 1) % self.flat.len();
    }

    /// Move the highlight up one row, wrapping to the bottom past the top.
    pub fn move_up(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.cursor = (self.cursor + self.flat.len() - 1) % self.flat.len();
    }

    /// Latch the highlighted row's `action_id` for the host to drain. No-op when
    /// there is no selection.
    pub fn activate_selected(&mut self) {
        if let Some(id) = self.selected().map(|r| r.action_id.clone()) {
            self.activated = Some(id);
        }
    }

    /// One-shot read of the last activated `action_id`; `None` until a row is
    /// activated, and drains after one read.
    pub fn take_activated(&mut self) -> Option<String> {
        self.activated.take()
    }

    /// Position the overlay directly (for a free-floating overlay not driven by
    /// the layout pass).
    pub fn set_rect(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The overlay's natural height for the current groups — the height the host
    /// should give it. Falls back to an empty-state height with no results.
    pub fn desired_height(&self) -> f64 {
        if self.groups.is_empty() {
            return EMPTY_H;
        }
        let mut h = PAD * 2.0;
        for g in &self.groups {
            h += HEADER_H;
            for r in &g.results {
                h += Self::row_height(r);
            }
        }
        h
    }

    fn row_height(r: &SearchResult) -> f64 {
        if r.subtitle.is_some() {
            ROW_H_TWO_LINE
        } else {
            ROW_H_ONE_LINE
        }
    }

    /// Optically-centred `y` tops for a row's title (and subtitle, when
    /// present), against the row `rect`. A single-line row centres the title on
    /// the row middle; a two-line row centres the title/subtitle block as a
    /// unit — both level with the leading icon, which is box-centred on the same
    /// middle (see `draw_row`). Routes through the toolkit's canonical
    /// optical-centring helpers so list-row text lines up with box-centred
    /// glyphs everywhere. Returns `(title_top, subtitle_top?)`.
    fn row_text_ys(rect: Rect, has_sub: bool, title_size: f32, sub_size: f32) -> (f64, Option<f64>) {
        if has_sub {
            // Advance between the title top and subtitle top: the title's
            // line-box height, so the pair reads as a natural two-line block.
            let advance = title_size as f64 * 1.3;
            let (title_y, sub_y) = crate::text_render::centered_two_line_tops(
                rect.y0,
                rect.height(),
                title_size,
                sub_size,
                advance,
            );
            (title_y, Some(sub_y))
        } else {
            (
                crate::text_render::centered_text_y(rect.y0, rect.height(), title_size),
                None,
            )
        }
    }

    /// Lay out headers + rows against the current rect. Single source of truth
    /// shared by `draw` and `row_at`, so a click always lands on the row that
    /// was drawn.
    fn plan(&self) -> LayoutPlan {
        let mut headers = Vec::new();
        let mut rows = Vec::new();
        let x0 = self.rect.x0 + ROW_INSET;
        let x1 = self.rect.x1 - ROW_INSET;
        let mut y = self.rect.y0 + PAD;
        let mut flat = 0usize;
        for (gi, g) in self.groups.iter().enumerate() {
            headers.push((
                Rect::new(x0, y, x1, y + HEADER_H),
                g.kind.clone(),
                g.results.len(),
            ));
            y += HEADER_H;
            for (ri, r) in g.results.iter().enumerate() {
                let rh = Self::row_height(r);
                rows.push(RowGeom {
                    rect: Rect::new(x0, y, x1, y + rh),
                    flat,
                    gi,
                    ri,
                });
                y += rh;
                flat += 1;
            }
        }
        LayoutPlan { headers, rows }
    }

    /// The flat row index at `p`, if a row is under it.
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
        let result = &self.groups[row.gi].results[row.ri];
        let selected = row.flat == self.cursor;
        let hovered = self.hover == Some(row.flat);

        // Selection / hover backing. Selection = a filled indigo row (the
        // toolkit's active-row convention, per the command palette); hover = a
        // barely-there veneer so pointer feedback is distinct from selection.
        if selected {
            let hl = RoundedRect::from_rect(row.rect, theme.tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &hl);
        } else if hovered {
            let hl = RoundedRect::from_rect(row.rect, theme.tokens.radii.sm as f64);
            scene.fill(Fill::NonZero, transform, colors.surface_white_5, None, &hl);
        }

        let mut text_x = row.rect.x0 + ROW_TEXT_X;

        // Leading glyph (icon color follows selection so it reads on the fill).
        if let Some(icon) = result.icon {
            let icon_rect = Rect::from_center_size(
                Point::new(text_x + ICON_D / 2.0, row.rect.center().y),
                (ICON_D, ICON_D),
            );
            let icon_col = if selected { colors.text_primary } else { colors.text_dim };
            icon.draw_with_stroke(scene, transform, icon_rect, icon_col, 1.6);
            text_x += ICON_COL;
        }

        let has_sub = result.subtitle.is_some();
        let title_size = theme.tokens.typography.scale.base;
        let sub_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        let text_w = (row.rect.x1 - text_x - ROW_TEXT_X).max(0.0) as f32;

        // Title + optional subtitle, optically centred against the row so the
        // text block sits level with the box-centred leading icon rather than
        // ~0.2 em low (shared toolkit helper).
        let (title_y, sub_y) = Self::row_text_ys(row.rect, has_sub, title_size, sub_size);
        tr.draw_text_weighted(
            scene,
            &result.title,
            title_size,
            colors.text_primary,
            transform * Affine::translate((text_x, title_y)),
            Some(text_w),
            TextWeight::Medium,
        );

        // Subtitle (dim; on a selected row it stays readable on the fill).
        if let Some(sub) = &result.subtitle {
            let sy = sub_y.unwrap_or(title_y);
            let sub_col = if selected { colors.text_primary } else { colors.text_dim };
            tr.draw_text(
                scene,
                sub,
                sub_size,
                sub_col,
                transform * Affine::translate((text_x, sy)),
                Some(text_w),
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
        let _ = scene;
        let size = theme.tokens.typography.scale.sm;
        let (tw, _th) = tr.measure(&self.empty_text, size, None);
        let cx = self.rect.center().x - tw as f64 / 2.0;
        // Optically centre the single empty-state line (shared toolkit helper).
        let cy = crate::text_render::centered_text_y(self.rect.y0, self.rect.height(), size);
        tr.draw_text(
            scene,
            &self.empty_text,
            size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((cx, cy)),
            None,
        );
    }
}

impl Default for SearchResultsView {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for SearchResultsView {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: percent(1.0),
                    height: length(self.desired_height() as f32),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
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
        // Overlay card.
        let card = RoundedRect::from_rect(self.rect, theme.tokens.radii.lg as f64);
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.raised, None, &card);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.border_standard,
            None,
            &card,
        );

        if self.flat.is_empty() {
            self.draw_empty(scene, transform, text_renderer, theme);
            return;
        }

        let plan = self.plan();
        // Section headers: dim, uppercase, with a trailing count.
        let header_size = (theme.tokens.typography.scale.xs).max(10.0);
        for (hrect, kind, count) in &plan.headers {
            let label = kind.to_uppercase();
            let hy = hrect.y1 - header_size as f64 - 6.0;
            text_renderer.draw_text_weighted(
                scene,
                &label,
                header_size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((hrect.x0 + 4.0, hy)),
                None,
                TextWeight::Semibold,
            );
            let count_str = count.to_string();
            let (cw, _) = text_renderer.measure(&count_str, header_size, None);
            text_renderer.draw_text(
                scene,
                &count_str,
                header_size,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((hrect.x1 - cw as f64 - 4.0, hy)),
                None,
            );
        }
        for row in &plan.rows {
            self.draw_row(scene, transform, text_renderer, theme, row);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
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
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } => {
                if self.flat.is_empty() {
                    return;
                }
                match logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        self.move_down();
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.move_up();
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        self.activate_selected();
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "SearchResultsView"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::List).with_name("Search results"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchRegistry;

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
    fn set_groups_builds_flat_index_and_clamps_cursor() {
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

    #[test]
    fn keyboard_navigation_wraps_and_enter_activates() {
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
        v.handle_event(&key(NamedKey::ArrowDown), 1.0, &mods, &mut doc, &root, &mut handled, &mut ft, node, &taffy);
        assert!(handled, "arrow-down consumed");
        assert_eq!(v.cursor(), 1);
        // Wrap: up from the top lands on the last row.
        v.set_cursor(0);
        v.handle_event(&key(NamedKey::ArrowUp), 1.0, &mods, &mut doc, &root, &mut handled, &mut ft, node, &taffy);
        assert_eq!(v.cursor(), v.len() - 1, "up wraps to the bottom");

        // Enter activates the highlighted row and drains once.
        v.set_cursor(0);
        v.handle_event(&key(NamedKey::Enter), 1.0, &mods, &mut doc, &root, &mut handled, &mut ft, node, &taffy);
        assert_eq!(v.take_activated().as_deref(), Some("c:alice"), "enter fires the selected action-id");
        assert_eq!(v.take_activated(), None, "activation drains after one read");
    }

    #[test]
    fn clicking_a_row_selects_and_activates_it() {
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
            1.0, &mods, &mut doc, &root, &mut handled, &mut ft, node, &taffy,
        );
        v.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0, &mods, &mut doc, &root, &mut handled, &mut ft, node, &taffy,
        );
        assert!(handled, "row click consumed");
        assert_eq!(v.cursor(), 2, "click moves the highlight to the clicked row");
        assert_eq!(v.take_activated().as_deref(), Some("app:calc"), "click fires that row's action-id");
    }

    #[test]
    fn empty_results_show_empty_state_and_no_activation() {
        let mut v = SearchResultsView::new().with_empty_text("Nothing here");
        v.set_groups(vec![]);
        v.set_rect(Rect::new(0.0, 0.0, 360.0, v.desired_height()));
        assert!(v.is_empty());
        assert_eq!(v.selected(), None);
        v.activate_selected();
        assert_eq!(v.take_activated(), None, "no row ⇒ nothing to activate");
    }

    #[test]
    fn desired_height_grows_with_content() {
        let empty = SearchResultsView::new().desired_height();
        let full = view_from("").desired_height();
        assert!(full > empty, "populated overlay is taller than the empty state");
    }

    /// List-row content is optically vertically centred against the row — level
    /// with the leading icon, which `draw_row` box-centres on the row middle.
    /// Asserts on the exact `row_text_ys` production path `draw_row` calls:
    /// a single-line row centres the title's optical centre on the row middle;
    /// a two-line row centres the title/subtitle block's optical centre there.
    /// Both must lift above the naive `(h - size)/2` box-centre that read low.
    #[test]
    fn row_content_is_optically_centered_against_the_row() {
        use crate::text_render::OPTICAL_LIFT;
        let k = 0.5 + OPTICAL_LIFT;
        let title_size = 16.0f32;
        let sub_size = 13.0f32;

        // Single-line row: the icon sits on the row centre, so the title's
        // optical centre must land there too.
        let one = Rect::new(0.0, 0.0, 360.0, ROW_H_ONE_LINE);
        let (title_y, sub_y) = SearchResultsView::row_text_ys(one, false, title_size, sub_size);
        assert_eq!(sub_y, None, "a single-line row has no subtitle top");
        let optical_center = title_y + title_size as f64 * k;
        assert!(
            (optical_center - one.center().y).abs() < 0.5,
            "single-line title optical centre {optical_center} must sit on the row centre {}",
            one.center().y,
        );
        let naive = one.y0 + (one.height() - title_size as f64) / 2.0;
        assert!(title_y < naive, "single-line title must lift above the naive-low baseline");

        // Two-line row: the title/subtitle block's optical centre must land on
        // the row centre (level with the icon), and the title must still lift
        // above the naive box-centre.
        let two = Rect::new(0.0, 0.0, 360.0, ROW_H_TWO_LINE);
        let (t2, s2) = SearchResultsView::row_text_ys(two, true, title_size, sub_size);
        let s2 = s2.expect("a two-line row exposes a subtitle top");
        let ct = t2 + title_size as f64 * k;
        let cb = s2 + sub_size as f64 * k;
        let block_center = (ct + cb) / 2.0;
        assert!(
            (block_center - two.center().y).abs() < 0.5,
            "two-line block optical centre {block_center} must sit on the row centre {}",
            two.center().y,
        );
        assert!(s2 > t2, "the subtitle sits below the title");
        let naive2 = two.y0 + (two.height() - title_size as f64) / 2.0;
        assert!(t2 < naive2, "two-line title must lift above the naive-low baseline");
    }
}
