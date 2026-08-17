//! [`EmojiPicker`] — a shared, theme-aware emoji picker primitive.
//!
//! Every app (and the on-screen keyboard) had to hand-roll its own emoji grid;
//! chat's was a static category list with no search, the OSK had none. This is
//! the one place the toolkit renders emoji: a categorised grid with a
//! recent/frequently-used tab and a **fuzzy search** field, all reading from the
//! shared [`crate::emoji`] dataset.
//!
//! It is a self-contained composite: it paints its own panel chrome (a themed
//! drop shadow + rounded surface), a real [`TextInput`] search field, a category
//! tab strip, and a scrolling emoji grid. A host opens it, positions it (either
//! through the taffy tree, or manually via [`EmojiPicker::apply_layout_external`]
//! for an immediate-mode app like chat or the OSK), draws it, forwards events,
//! and reads the chosen emoji back with [`EmojiPicker::take_picked`].

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::color_compat::ColorCompat;
use crate::emoji::{self, Group};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TextInput, Widget};

/// Outer panel padding (logical px).
const PAD: f64 = 8.0;
/// Search field height.
const SEARCH_H: f64 = 34.0;
/// Category tab strip height.
const TAB_H: f64 = 34.0;
/// Section-label band height above the grid.
const LABEL_H: f64 = 18.0;
/// Emoji cell edge (logical px). One glyph sits centred in each.
const CELL: f64 = 34.0;
/// Emoji glyph size.
const GLYPH: f32 = 22.0;
/// Default preferred popover size.
const DEFAULT_W: f64 = 340.0;
const DEFAULT_H: f64 = 400.0;
/// How many recents to keep.
const RECENT_CAP: usize = 32;
/// Max fuzzy results shown for a query.
const SEARCH_CAP: usize = 180;

/// Which tab is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Recent,
    Group(Group),
}

/// A shared emoji picker: search + recents + categorised grid.
pub struct EmojiPicker {
    rect: Rect,
    /// Real text field for fuzzy search (full caret/selection/clipboard).
    search: TextInput,
    /// Most-recently-picked emoji, front = newest. Host may seed + persist.
    recents: Vec<String>,
    /// Active category tab.
    tab: Tab,
    /// Vertical grid scroll (logical px).
    scroll: f64,
    /// Query the display list was last built for (resets scroll on change).
    last_query: String,

    // --- per-frame geometry caches (rebuilt in draw) ---
    /// Absolute rects of each category tab.
    tab_rects: Vec<(Tab, Rect)>,
    /// The emoji currently displayed + its absolute cell rect (pre-clip).
    cells: Vec<(String, Rect)>,
    grid_area: Rect,
    columns: usize,

    // --- interaction ---
    last_cursor: Point,
    hover_cell: Option<usize>,
    hover_tab: Option<usize>,
    /// Keyboard selection index into `cells`.
    sel: usize,
    is_focused: bool,

    /// One-shot: the emoji the user just chose.
    picked: Option<String>,
}

impl Default for EmojiPicker {
    fn default() -> Self {
        Self::new()
    }
}

impl EmojiPicker {
    pub fn new() -> Self {
        let mut search = TextInput::local().with_placeholder("Search emoji");
        search.set_font_size(14.0);
        Self {
            rect: Rect::ZERO,
            search,
            recents: Vec::new(),
            tab: Tab::Group(Group::Smileys),
            scroll: 0.0,
            last_query: String::new(),
            tab_rects: Vec::new(),
            cells: Vec::new(),
            grid_area: Rect::ZERO,
            columns: 1,
            last_cursor: Point::ZERO,
            hover_cell: None,
            hover_tab: None,
            sel: 0,
            is_focused: false,
            picked: None,
        }
    }

    /// The natural popover size for this picker.
    pub fn preferred_size(&self) -> (f64, f64) {
        (DEFAULT_W, DEFAULT_H)
    }

    /// Seed the recent/frequently-used list (front = newest). Trimmed to the cap.
    pub fn set_recents(&mut self, recents: Vec<String>) {
        self.recents = recents;
        self.recents.truncate(RECENT_CAP);
    }

    /// Builder form of [`set_recents`](Self::set_recents).
    pub fn with_recents(mut self, recents: Vec<String>) -> Self {
        self.set_recents(recents);
        self
    }

    /// The current recents list, so a host can persist it.
    pub fn recents(&self) -> &[String] {
        &self.recents
    }

    /// Reset transient state when the picker is (re)opened: clear the query,
    /// scroll to top, land on Recents when there are any (else Smileys), and
    /// focus the search field.
    pub fn open_reset(&mut self) {
        self.search.set_text("");
        self.last_query.clear();
        self.scroll = 0.0;
        self.sel = 0;
        self.tab = if self.recents.is_empty() {
            Tab::Group(Group::Smileys)
        } else {
            Tab::Recent
        };
        self.focus_search(true);
    }

    /// Focus (or unfocus) the internal search field.
    pub fn focus_search(&mut self, on: bool) {
        self.search.set_focus(on);
        self.is_focused = on;
    }

    /// One-shot read of the chosen emoji. `Some(ch)` exactly once per pick.
    pub fn take_picked(&mut self) -> Option<String> {
        self.picked.take()
    }

    /// Manual layout for immediate-mode hosts (chat / OSK) that place the picker
    /// by hand rather than through the taffy tree.
    pub fn apply_layout_external(&mut self, rect: Rect) {
        self.rect = rect;
    }

    /// The picker's outer rect (for click-outside dismissal by the host).
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Current query string (trimmed).
    fn query(&self) -> String {
        self.search.value().trim().to_string()
    }

    /// The emoji glyphs to show for the current tab / query, in order.
    fn display_list(&self) -> Vec<String> {
        let q = self.query();
        if !q.is_empty() {
            return emoji::search(&q, SEARCH_CAP)
                .into_iter()
                .map(|e| e.ch.to_string())
                .collect();
        }
        match self.tab {
            Tab::Recent => self.recents.clone(),
            Tab::Group(g) => emoji::in_group(g).map(|e| e.ch.to_string()).collect(),
        }
    }

    /// The section label above the grid.
    fn section_label(&self) -> &'static str {
        if !self.query().is_empty() {
            return "Results";
        }
        match self.tab {
            Tab::Recent => "Recent",
            Tab::Group(g) => g.label(),
        }
    }

    /// Record a pick: emit it + fold it to the front of recents (deduped).
    fn choose(&mut self, ch: &str) {
        self.picked = Some(ch.to_string());
        self.recents.retain(|e| e != ch);
        self.recents.insert(0, ch.to_string());
        self.recents.truncate(RECENT_CAP);
    }

    /// The list of tabs (Recent first when populated, then each group).
    fn tabs(&self) -> Vec<Tab> {
        let mut v = Vec::with_capacity(Group::ALL.len() + 1);
        if !self.recents.is_empty() {
            v.push(Tab::Recent);
        }
        for g in Group::ALL {
            v.push(Tab::Group(*g));
        }
        v
    }

    /// A short glyph for a tab.
    fn tab_glyph(tab: Tab) -> &'static str {
        match tab {
            // A clock reads as "recent" across emoji fonts.
            Tab::Recent => "\u{1f550}",
            Tab::Group(g) => g.tab_glyph(),
        }
    }

    /// Total content height of the grid for the current list + columns.
    fn content_height(&self, count: usize) -> f64 {
        let cols = self.columns.max(1);
        let rows = count.div_ceil(cols);
        rows as f64 * CELL
    }

    /// Clamp `scroll` to the laid-out content.
    fn clamp_scroll(&mut self, count: usize) {
        let view_h = self.grid_area.height();
        let max = (self.content_height(count) - view_h).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }
}

impl Widget for EmojiPicker {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.new_leaf(Style::default()).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size { width: DEFAULT_W as f32, height: DEFAULT_H as f32 }
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
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let tokens = &theme.tokens;
        let r = self.rect;
        if r.width() < 1.0 || r.height() < 1.0 {
            return;
        }
        let radius = tokens.radii.lg as f64;

        // --- panel chrome: themed drop shadow + surface + hairline border ---
        let elev = &tokens.elevation.e3;
        let off = elev.offset_y as f64;
        let shadow_rect = Rect::new(r.x0, r.y0 + off, r.x1, r.y1 + off);
        // Two stacked translucent copies of the elevation token fake a soft blur
        // without a real filter (the shared status-bubble pattern).
        for (grow, alpha) in [(elev.blur as f64, 0.5f64), (elev.blur as f64 * 0.5, 0.9)] {
            scene.fill(
                Fill::NonZero,
                transform,
                elev.color.with_alpha_factor(alpha as f32),
                None,
                &RoundedRect::from_rect(shadow_rect.inflate(grow, grow), radius + grow),
            );
        }
        let surface = RoundedRect::from_rect(r, radius);
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &surface);
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.border_standard,
            None,
            &surface,
        );

        // --- regions ---
        let search_rect = Rect::new(r.x0 + PAD, r.y0 + PAD, r.x1 - PAD, r.y0 + PAD + SEARCH_H);
        let tabs_rect = Rect::new(
            r.x0 + PAD,
            search_rect.y1 + PAD,
            r.x1 - PAD,
            search_rect.y1 + PAD + TAB_H,
        );
        let label_y = tabs_rect.y1 + 6.0;
        self.grid_area = Rect::new(r.x0 + PAD, label_y + LABEL_H, r.x1 - PAD, r.y1 - PAD);

        // --- search field (real TextInput) ---
        self.search.apply_layout_external(search_rect);
        self.search.draw(scene, transform, tr, theme, presence);

        // --- category tabs ---
        let tabs = self.tabs();
        self.tab_rects.clear();
        if !tabs.is_empty() {
            let tw = tabs_rect.width() / tabs.len() as f64;
            let searching = !self.query().is_empty();
            for (i, tab) in tabs.iter().enumerate() {
                let tx = tabs_rect.x0 + i as f64 * tw;
                let cell = Rect::new(tx, tabs_rect.y0, tx + tw, tabs_rect.y1);
                self.tab_rects.push((*tab, cell));
                let active = !searching && *tab == self.tab;
                let hovered = self.hover_tab == Some(i);
                if active {
                    let bg = RoundedRect::from_rect(cell.inflate(-3.0, -3.0), tokens.radii.md as f64);
                    scene.fill(
                        Fill::NonZero,
                        transform,
                        theme.bg_active,
                        None,
                        &bg,
                    );
                } else if hovered {
                    let bg = RoundedRect::from_rect(cell.inflate(-3.0, -3.0), tokens.radii.md as f64);
                    scene.fill(Fill::NonZero, transform, theme.bg_hover, None, &bg);
                }
                let glyph = Self::tab_glyph(*tab);
                let (gw, gh) = tr.measure(glyph, 17.0, None);
                tr.draw_text(
                    scene,
                    glyph,
                    17.0,
                    tokens.colors.text_primary,
                    transform
                        * Affine::translate((
                            cell.center().x - gw as f64 / 2.0,
                            cell.center().y - gh as f64 / 2.0,
                        )),
                    None,
                );
                if active {
                    // Accent underline anchors the active category.
                    scene.stroke(
                        &Stroke::new(2.0),
                        transform,
                        theme.accent_primary,
                        None,
                        &Line::new((cell.x0 + 6.0, cell.y1 - 2.0), (cell.x1 - 6.0, cell.y1 - 2.0)),
                    );
                }
            }
        }

        // --- section label ---
        tr.draw_text(
            scene,
            self.section_label(),
            11.0,
            tokens.colors.text_dim,
            transform * Affine::translate((r.x0 + PAD + 2.0, label_y)),
            None,
        );

        // --- emoji grid (scrolling, clipped) ---
        let list = self.display_list();
        // Reset scroll when the query changed.
        let q = self.query();
        if q != self.last_query {
            self.last_query = q;
            self.scroll = 0.0;
            self.sel = 0;
        }
        self.columns = ((self.grid_area.width() / CELL).floor() as usize).max(1);
        self.clamp_scroll(list.len());
        if self.sel >= list.len() {
            self.sel = list.len().saturating_sub(1);
        }

        self.cells.clear();
        let cols = self.columns;
        // Centre the grid horizontally in any leftover width.
        let used_w = cols as f64 * CELL;
        let x_off = ((self.grid_area.width() - used_w) / 2.0).max(0.0);
        for (i, ch) in list.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx = self.grid_area.x0 + x_off + col as f64 * CELL;
            let cy = self.grid_area.y0 + row as f64 * CELL - self.scroll;
            self.cells.push((ch.clone(), Rect::new(cx, cy, cx + CELL, cy + CELL)));
        }

        // Clip to the grid viewport so scrolled cells don't bleed over the tabs.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &self.grid_area,
        );
        if list.is_empty() {
            let msg = if !self.last_query.is_empty() {
                "No emoji found"
            } else {
                "Nothing here yet"
            };
            let (mw, _) = tr.measure(msg, 13.0, None);
            tr.draw_text(
                scene,
                msg,
                13.0,
                tokens.colors.text_dim,
                transform
                    * Affine::translate((
                        self.grid_area.center().x - mw as f64 / 2.0,
                        self.grid_area.y0 + 16.0,
                    )),
                None,
            );
        }
        for (i, (ch, cell)) in self.cells.iter().enumerate() {
            if cell.y1 < self.grid_area.y0 || cell.y0 > self.grid_area.y1 {
                continue;
            }
            let hovered = self.hover_cell == Some(i);
            let selected = self.is_focused && self.sel == i && !self.query().is_empty();
            if hovered || selected {
                let bg = RoundedRect::from_rect(cell.inflate(-2.0, -2.0), tokens.radii.md as f64);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if selected { theme.bg_active } else { theme.bg_hover },
                    None,
                    &bg,
                );
            }
            let (gw, gh) = tr.measure(ch.as_str(), GLYPH, None);
            tr.draw_text(
                scene,
                ch.as_str(),
                GLYPH,
                tokens.colors.text_primary,
                transform
                    * Affine::translate((
                        cell.center().x - gw as f64 / 2.0,
                        cell.center().y - gh as f64 / 2.0,
                    )),
                None,
            );
        }
        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        // Forward everything to the search field first (it only consumes events
        // over its own rect / while focused), so typing filters and clicking the
        // field places a caret.
        self.search.handle_event(
            event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy,
        );

        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let pt = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = pt;
                let new_cell = self
                    .cells
                    .iter()
                    .position(|(_, r)| r.contains(pt) && self.grid_area.contains(pt));
                if new_cell != self.hover_cell {
                    self.hover_cell = new_cell;
                    *handled = true;
                }
                let new_tab = self.tab_rects.iter().position(|(_, r)| r.contains(pt));
                if new_tab != self.hover_tab {
                    self.hover_tab = new_tab;
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if self.rect.contains(self.last_cursor) {
                    let dy = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => -(*y as f64) * CELL,
                        winit::event::MouseScrollDelta::PixelDelta(p) => -p.y / scale_factor,
                    };
                    self.scroll = (self.scroll + dy).max(0.0);
                    let n = self.cells.len();
                    self.clamp_scroll(n);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.last_cursor;
                // A tab click switches category + clears the query.
                if let Some((tab, _)) = self.tab_rects.iter().find(|(_, r)| r.contains(pt)).copied() {
                    self.tab = tab;
                    self.search.set_text("");
                    self.last_query.clear();
                    self.scroll = 0.0;
                    self.sel = 0;
                    *handled = true;
                    return;
                }
                // An emoji cell click picks it.
                if self.grid_area.contains(pt) {
                    if let Some(ch) = self
                        .cells
                        .iter()
                        .find(|(_, r)| r.contains(pt))
                        .map(|(ch, _)| ch.clone())
                    {
                        self.choose(&ch);
                        *handled = true;
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event: winit::event::KeyEvent { logical_key, state: ElementState::Pressed, .. },
                ..
            } if self.is_focused => {
                let cols = self.columns.max(1);
                let n = self.cells.len();
                match logical_key {
                    Key::Named(NamedKey::Enter) => {
                        if let Some(ch) = self.cells.get(self.sel).map(|(ch, _)| ch.clone()) {
                            self.choose(&ch);
                            *handled = true;
                        }
                    }
                    Key::Named(NamedKey::ArrowRight) if n > 0 => {
                        self.sel = (self.sel + 1).min(n - 1);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) if n > 0 => {
                        self.sel = self.sel.saturating_sub(1);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowDown) if n > 0 => {
                        self.sel = (self.sel + cols).min(n - 1);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) if n > 0 => {
                        self.sel = self.sel.saturating_sub(cols);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "EmojiPicker"
    }

    fn test_id(&self) -> Option<&str> {
        Some("emoji_picker")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, f: bool) {
        self.focus_search(f);
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    /// The picker traps input while shown: the host treats an outside click as
    /// dismiss and suppresses global shortcuts.
    fn modal_capture(&self) -> Option<Rect> {
        if self.rect.width() > 1.0 {
            Some(self.rect)
        } else {
            None
        }
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, Role};
        Some(AccessNode::new(Role::List).with_name("Emoji picker"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::prelude::TaffyMaxContent;

    fn laid_out() -> (EmojiPicker, TaffyTree<()>, NodeId) {
        let mut p = EmojiPicker::new();
        p.apply_layout_external(Rect::new(0.0, 0.0, DEFAULT_W, DEFAULT_H));
        // Prime the display/geometry caches with a draw.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        let mut scene = Scene::new();
        p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let root = taffy.new_with_children(Style::default(), &[node]).unwrap();
        taffy.compute_layout(root, taffy::Size::MAX_CONTENT).unwrap();
        (p, taffy, node)
    }

    fn press_at(p: &mut EmojiPicker, taffy: &TaffyTree<()>, node: NodeId, pt: Point) {
        let mods = winit::event::Modifiers::default();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let device: winit::event::DeviceId = unsafe { std::mem::zeroed() };
        let mut h = false;
        let mut f = None;
        p.handle_event(
            &WindowEvent::CursorMoved { device_id: device, position: (pt.x, pt.y).into() },
            1.0, &mods, &mut doc, &root, &mut h, &mut f, node, taffy,
        );
        p.handle_event(
            &WindowEvent::MouseInput { device_id: device, state: ElementState::Pressed, button: MouseButton::Left },
            1.0, &mods, &mut doc, &root, &mut h, &mut f, node, taffy,
        );
    }

    /// Build a KeyboardInput event via the zeroed-KeyEvent pattern the other
    /// widget tests use (KeyEvent has a private platform field).
    fn key_event(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(|t| t.into());
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn type_str(p: &mut EmojiPicker, taffy: &TaffyTree<()>, node: NodeId, s: &str) {
        let mods = winit::event::Modifiers::default();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        for ch in s.chars() {
            let mut h = false;
            let mut f = None;
            let cs = ch.to_string();
            let ev = key_event(Key::Character(cs.as_str().into()), Some(cs.as_str()));
            p.handle_event(&ev, 1.0, &mods, &mut doc, &root, &mut h, &mut f, node, taffy);
        }
    }

    #[test]
    fn clicking_a_cell_picks_that_emoji() {
        let (mut p, taffy, node) = laid_out();
        p.focus_search(true);
        // First visible cell centre.
        let (ch, cell) = {
            let (c, r) = p.cells.first().expect("at least one emoji cell");
            (c.clone(), *r)
        };
        press_at(&mut p, &taffy, node, cell.center());
        assert_eq!(p.take_picked().as_deref(), Some(ch.as_str()));
        // Picking records a recent.
        assert_eq!(p.recents().first().map(|s| s.as_str()), Some(ch.as_str()));
    }

    #[test]
    fn search_filters_the_grid_and_enter_inserts() {
        let (mut p, taffy, node) = laid_out();
        p.focus_search(true);
        type_str(&mut p, &taffy, node, "fire");
        // Re-run draw so the display list rebuilds against the query.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::empty();
        let mut scene = Scene::new();
        p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(!p.cells.is_empty(), "search 'fire' must show at least one result");
        assert!(
            p.cells.iter().any(|(ch, _)| ch == "\u{1f525}"),
            "search 'fire' must surface the fire emoji",
        );
        // Enter picks the selected (first) result.
        let mods = winit::event::Modifiers::default();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mut h = false;
        let mut f = None;
        p.handle_event(
            &key_event(Key::Named(NamedKey::Enter), None),
            1.0, &mods, &mut doc, &root, &mut h, &mut f, node, &taffy,
        );
        assert!(p.take_picked().is_some(), "Enter on a search result must pick an emoji");
    }

    #[test]
    fn tab_click_switches_category() {
        let (mut p, taffy, node) = laid_out();
        // With no recents, tabs are the groups; click the second tab (People).
        let (tab, cell) = *p.tab_rects.get(1).expect("a second category tab");
        press_at(&mut p, &taffy, node, cell.center());
        assert_eq!(p.tab, tab);
        assert!(p.take_picked().is_none(), "switching tabs must not pick an emoji");
    }

    #[test]
    fn recents_seed_a_recent_tab() {
        let mut p = EmojiPicker::new().with_recents(vec!["\u{1f525}".into()]);
        p.open_reset();
        assert_eq!(p.tab, Tab::Recent, "with recents, open lands on the Recent tab");
        assert!(p.tabs().contains(&Tab::Recent));
    }
}
