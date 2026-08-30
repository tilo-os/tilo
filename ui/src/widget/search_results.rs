//! `SearchResultsView` — the shared results overlay for qOS global search.
//!
//! Renders the ranked, kind-grouped [`SearchResult`](crate::search::SearchResult)s
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
use crate::widget::TaffyTreeExt;
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
    #[must_use]
    pub fn with_empty_text(mut self, text: impl Into<String>) -> Self {
        self.empty_text = text.into();
        self
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

    pub fn selected(&self) -> Option<&SearchResult> {
        let (gi, ri) = *self.flat.get(self.cursor)?;
        self.groups.get(gi)?.results.get(ri)
    }

    pub fn selected_action_id(&self) -> Option<&str> {
        self.selected().map(|r| r.action_id.as_str())
    }

    /// Move the highlight down one row, wrapping to the top past the end.
    pub fn move_down(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.cursor = self.cursor.saturating_add(1) % self.flat.len();
    }

    /// Move the highlight up one row, wrapping to the bottom past the top.
    pub fn move_up(&mut self) {
        if self.flat.is_empty() {
            return;
        }
        self.cursor = self
            .cursor
            .saturating_add(self.flat.len())
            .saturating_sub(1)
            % self.flat.len();
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

    /// Single source of truth shared by `draw` and `row_at`, so a click
    /// always lands on the row that was drawn.
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

        // Selection = a filled row (per the command palette convention);
        // hover = a barely-there veneer, distinct from selection.
        if selected {
            let hl = RoundedRect::from_rect(row.rect, f64::from(theme.tokens.radii.sm));
            scene.fill(Fill::NonZero, transform, colors.accent[5], None, &hl);
        } else if hovered {
            let hl = RoundedRect::from_rect(row.rect, f64::from(theme.tokens.radii.sm));
            scene.fill(Fill::NonZero, transform, colors.surface_white_5, None, &hl);
        }

        let mut text_x = row.rect.x0 + ROW_TEXT_X;

        // Leading glyph (icon color follows selection so it reads on the fill).
        if let Some(icon) = result.icon {
            let icon_rect = Rect::from_center_size(
                Point::new(text_x + ICON_D / 2.0, row.rect.center().y),
                (ICON_D, ICON_D),
            );
            let icon_col = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            icon.draw_with_stroke(scene, transform, icon_rect, icon_col, 1.6);
            text_x += ICON_COL;
        }

        let has_sub = result.subtitle.is_some();
        let title_size = theme.tokens.typography.scale.base;
        let sub_size = (theme.tokens.typography.scale.sm - 1.0).max(10.0);
        // Clamped to >= 0.0 above; a row's text width is a small on-screen
        // pixel span, well inside `f32`'s exact-integer range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "row text width is a small non-negative on-screen pixel span"
        )]
        let text_w = (row.rect.x1 - text_x - ROW_TEXT_X).max(0.0) as f32;

        let title_y = if has_sub {
            row.rect.y0 + 8.0
        } else {
            row.rect.y0 + (row.rect.height() - f64::from(title_size)) / 2.0
        };
        tr.draw_text_weighted(
            scene,
            &result.title,
            title_size,
            colors.text_primary,
            compose(transform, Affine::translate((text_x, title_y))),
            Some(text_w),
            TextWeight::Medium,
        );

        // Subtitle (dim; on a selected row it stays readable on the fill).
        if let Some(sub) = &result.subtitle {
            let sy = row.rect.y1 - f64::from(sub_size) - 9.0;
            let sub_col = if selected {
                colors.text_primary
            } else {
                colors.text_dim
            };
            tr.draw_text(
                scene,
                sub,
                sub_size,
                sub_col,
                compose(transform, Affine::translate((text_x, sy))),
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
        let (tw, th) = tr.measure(&self.empty_text, size, None);
        let cx = self.rect.center().x - f64::from(tw) / 2.0;
        let cy = self.rect.center().y - f64::from(th) / 2.0;
        tr.draw_text(
            scene,
            &self.empty_text,
            size,
            theme.tokens.colors.text_dim,
            compose(transform, Affine::translate((cx, cy))),
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
        let card = RoundedRect::from_rect(self.rect, f64::from(theme.tokens.radii.lg));
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &card,
        );
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
            let hy = hrect.y1 - f64::from(header_size) - 6.0;
            text_renderer.draw_text_weighted(
                scene,
                &label,
                header_size,
                theme.tokens.colors.text_dim,
                compose(transform, Affine::translate((hrect.x0 + 4.0, hy))),
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
                compose(
                    transform,
                    Affine::translate((hrect.x1 - f64::from(cw) - 4.0, hy)),
                ),
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

    fn name(&self) -> &'static str {
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
        v.handle_event(
            &key(NamedKey::ArrowDown),
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        assert!(handled, "arrow-down consumed");
        assert_eq!(v.cursor(), 1);
        // Wrap: up from the top lands on the last row.
        v.set_cursor(0);
        v.handle_event(
            &key(NamedKey::ArrowUp),
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        assert_eq!(v.cursor(), v.len() - 1, "up wraps to the bottom");

        // Enter activates the highlighted row and drains once.
        v.set_cursor(0);
        v.handle_event(
            &key(NamedKey::Enter),
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        assert_eq!(
            v.take_activated().as_deref(),
            Some("c:alice"),
            "enter fires the selected action-id"
        );
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
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
        );
        v.handle_event(
            &WindowEvent::MouseInput {
                device_id: unsafe { std::mem::zeroed() },
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut ft,
            node,
            &taffy,
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
        assert!(
            full > empty,
            "populated overlay is taller than the empty state"
        );
    }
}
