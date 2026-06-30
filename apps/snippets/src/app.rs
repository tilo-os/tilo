//! Root widget for the snippet manager.
//!
//! Owns the in-memory [`Library`], the per-peer view state (selection,
//! search, focus), and renders the three-pane layout directly into the
//! Vello scene:
//!
//! ```text
//! ┌──────────┬──────────────────┬─────────────────────────────┐
//! │ sidebar  │  snippet list    │  detail / editor pane       │
//! │ folders  │  (search above)  │  title · language · tags    │
//! │ tags     │                  │  syntax-highlighted body    │
//! └──────────┴──────────────────┴─────────────────────────────┘
//! ```
//!
//! The library is mirrored into the shared Automerge document by
//! [`crate::doc`] so two instances converge. Selection / search / the
//! open expand-modal are view state and stay local to each peer.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, Style, TaffyTree};
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::doc::SnippetsDoc;
use crate::draw;
use crate::expand::ExpandModal;
use crate::highlight;
use crate::model::{Language, Library, Selection, SmartFolder, Snippet, SnippetId};
use crate::seed;

const SIDEBAR_W: f64 = 232.0;
const LIST_W: f64 = 340.0;
const ROW_H: f64 = 64.0;
const SIDEBAR_ROW_H: f64 = 32.0;
const SEARCH_H: f64 = 52.0;
const BODY_FONT: f32 = 13.5;
const BODY_LINE: f64 = 20.0;

/// What kind of toast is currently showing (auto-dismisses on next event
/// tick after a short hold — for the headless shot it simply persists).
#[derive(Clone)]
struct Toast {
    message: String,
    /// Frames remaining before the toast fades. Decremented on idle.
    ttl: u32,
}

pub struct SnippetsApp {
    library: Library,
    selection: Selection,
    search: String,
    /// Whether the search field has keyboard focus (captures typing).
    search_focused: bool,
    /// Currently open snippet in the detail pane.
    open: Option<SnippetId>,
    /// Ordered ids currently shown in the list pane (result of `query`).
    visible: Vec<SnippetId>,
    /// The expand-with-variables modal, when open.
    expand: Option<ExpandModal>,
    toast: Option<Toast>,
    /// Whether we've mirrored the local library into the shared doc.
    seeded: bool,

    // Layout / hit-testing caches, recomputed each frame in `draw` so
    // `handle_event` can map clicks to actions without a second layout
    // pass. Each entry is (rect, action).
    rect: Rect,
    last_mouse: Point,
    hit: Vec<(Rect, Action)>,
    scroll: f64,
}

/// A clickable region recorded during draw and resolved on mouse-down.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Select(Selection),
    OpenSnippet(SnippetId),
    NewSnippet,
    Copy(SnippetId),
    Expand(SnippetId),
    ToggleFavorite(SnippetId),
    Duplicate(SnippetId),
    Delete(SnippetId),
    FocusSearch,
    ClearSearch,
    SetLanguage(SnippetId, Language),
}

impl SnippetsApp {
    /// Build the app with the starter library seeded locally. The library
    /// is pushed into the shared CRDT on the first `sync`.
    pub fn new() -> Self {
        let mut library = Library::new();
        for s in seed::starter_library() {
            library.insert(s);
        }
        let mut app = Self {
            library,
            selection: Selection::default(),
            search: String::new(),
            search_focused: false,
            open: None,
            visible: Vec::new(),
            expand: None,
            toast: None,
            seeded: false,
            rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            hit: Vec::new(),
            scroll: 0.0,
        };
        app.recompute_visible();
        // Open a substantial, templated snippet so the detail pane shows
        // off the highlighter, line gutter and variable affordance on
        // first launch. Prefer the visible snippet with the most body
        // lines that also carries placeholders.
        app.open = app
            .visible
            .iter()
            .copied()
            .max_by_key(|id| {
                let s = app.library.get(*id);
                let lines = s.map(|s| s.body.lines().count()).unwrap_or(0);
                let templated = s.map(|s| s.has_placeholders()).unwrap_or(false) as usize;
                (templated, lines)
            })
            .or_else(|| app.visible.first().copied());
        app
    }

    /// Empty constructor for the screenshot harness — identical to
    /// [`Self::new`] since the starter library is the realistic default.
    pub fn for_shot() -> Self {
        Self::new()
    }

    fn recompute_visible(&mut self) {
        self.visible = self.library.query(&self.selection, &self.search);
        // Keep the open snippet valid; otherwise open the first result.
        if let Some(open) = self.open {
            if !self.visible.contains(&open) {
                self.open = self.visible.first().copied();
            }
        } else {
            self.open = self.visible.first().copied();
        }
    }

    fn select(&mut self, sel: Selection) {
        self.selection = sel;
        self.scroll = 0.0;
        self.recompute_visible();
    }

    fn open_snippet(&mut self, id: SnippetId) {
        self.open = Some(id);
    }

    fn new_snippet(&mut self) {
        let folder = match &self.selection {
            Selection::Folder(f) => f.clone(),
            _ => String::new(),
        };
        let s = Snippet::blank(&folder);
        let id = s.id;
        self.library.insert(s);
        self.recompute_visible();
        self.open = Some(id);
        self.toast("New snippet created");
    }

    fn copy(&mut self, id: SnippetId) {
        // If the body has placeholders, route through the expand flow so
        // we never paste raw `${...}` onto the clipboard.
        let needs_expand = self
            .library
            .get(id)
            .map(|s| s.has_placeholders())
            .unwrap_or(false);
        if needs_expand {
            self.start_expand(id);
            return;
        }
        if let Some(s) = self.library.get_mut(id) {
            ui::clipboard_copy(&s.body);
            s.uses += 1;
            self.toast("Copied to clipboard");
        }
    }

    fn start_expand(&mut self, id: SnippetId) {
        if let Some(s) = self.library.get(id) {
            self.expand = Some(ExpandModal::new(s));
        }
    }

    fn toggle_favorite(&mut self, id: SnippetId) {
        if let Some(s) = self.library.get_mut(id) {
            s.favorite = !s.favorite;
        }
        self.recompute_visible();
    }

    fn duplicate(&mut self, id: SnippetId) {
        if let Some(src) = self.library.get(id).cloned() {
            let mut copy = src.clone();
            copy.id = uuid::Uuid::new_v4();
            copy.title = format!("{} (copy)", src.title);
            copy.favorite = false;
            copy.uses = 0;
            let new_id = copy.id;
            self.library.insert(copy);
            self.recompute_visible();
            self.open = Some(new_id);
            self.toast("Duplicated");
        }
    }

    fn delete(&mut self, id: SnippetId) {
        self.library.remove(id);
        if self.open == Some(id) {
            self.open = None;
        }
        self.recompute_visible();
        self.toast("Deleted");
    }

    fn set_language(&mut self, id: SnippetId, lang: Language) {
        if let Some(s) = self.library.get_mut(id) {
            s.language = lang;
        }
        self.recompute_visible();
    }

    fn toast(&mut self, message: &str) {
        self.toast = Some(Toast { message: message.to_string(), ttl: 90 });
    }

    /// Mirror the whole in-memory library into the shared doc (idempotent
    /// upserts). Pulls in any snippets a peer added that we don't have.
    fn sync_library(&mut self, doc: &mut AutoCommit) {
        let remote = SnippetsDoc::list(doc);
        if !self.seeded {
            if remote.is_empty() {
                // We're the origin — seed our starter set.
                for s in self.library.snippets.clone() {
                    SnippetsDoc::upsert(doc, &s);
                }
            } else {
                // A peer is already sharing — adopt their library.
                self.library.snippets = remote.clone();
                self.recompute_visible();
            }
            self.seeded = true;
            return;
        }
        // Steady state: push local snippets, pull remote-only ones.
        let local_ids: std::collections::HashSet<SnippetId> =
            self.library.snippets.iter().map(|s| s.id).collect();
        for s in self.library.snippets.clone() {
            SnippetsDoc::upsert(doc, &s);
        }
        let mut changed = false;
        for r in remote {
            if !local_ids.contains(&r.id) {
                self.library.insert(r);
                changed = true;
            }
        }
        if changed {
            self.recompute_visible();
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Select(sel) => self.select(sel),
            Action::OpenSnippet(id) => self.open_snippet(id),
            Action::NewSnippet => self.new_snippet(),
            Action::Copy(id) => self.copy(id),
            Action::Expand(id) => self.start_expand(id),
            Action::ToggleFavorite(id) => self.toggle_favorite(id),
            Action::Duplicate(id) => self.duplicate(id),
            Action::Delete(id) => self.delete(id),
            Action::FocusSearch => self.search_focused = true,
            Action::ClearSearch => {
                self.search.clear();
                self.recompute_visible();
            }
            Action::SetLanguage(id, lang) => self.set_language(id, lang),
        }
    }

    /// Resolve a click at `p` against the recorded hit regions. Topmost
    /// (last-recorded) match wins.
    fn action_at(&self, p: Point) -> Option<Action> {
        self.hit
            .iter()
            .rev()
            .find(|(r, _)| r.contains(p))
            .map(|(_, a)| a.clone())
    }

    fn record(&mut self, rect: Rect, action: Action) {
        self.hit.push((rect, action));
    }

    // ---- test accessors -------------------------------------------------

    #[doc(hidden)]
    pub fn open_id_for_test(&self) -> Option<SnippetId> {
        self.open
    }

    #[doc(hidden)]
    pub fn visible_count_for_test(&self) -> usize {
        self.visible.len()
    }

    #[doc(hidden)]
    pub fn set_search_for_test(&mut self, q: &str) {
        self.search = q.to_string();
        self.recompute_visible();
    }

    #[doc(hidden)]
    pub fn library_len_for_test(&self) -> usize {
        self.library.snippets.len()
    }

    #[doc(hidden)]
    pub fn expand_open_for_test(&self) -> bool {
        self.expand.is_some()
    }

    #[doc(hidden)]
    pub fn copy_for_test(&mut self, id: SnippetId) {
        self.copy(id);
    }

    #[doc(hidden)]
    pub fn snippet_uses_for_test(&self, id: SnippetId) -> u32 {
        self.library.get(id).map(|s| s.uses).unwrap_or(0)
    }
}

impl Default for SnippetsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl ui::widget::Widget for SnippetsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.sync_library(state);
    }

    fn needs_idle_sync(&self) -> bool {
        !self.seeded
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn ui::widget::Widget>,
    ) -> NodeId {
        // The app paints itself; it just needs one full-bleed leaf node.
        let node = taffy
            .new_leaf(Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn ui::widget::Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.hit.clear();
        let c = &theme.tokens.colors;
        // Whole-window background.
        draw::fill_rect(scene, t, self.rect, c.surfaces.base);

        let r = self.rect;
        let sidebar = Rect::new(r.x0, r.y0, r.x0 + SIDEBAR_W, r.y1);
        let list = Rect::new(sidebar.x1, r.y0, sidebar.x1 + LIST_W, r.y1);
        let detail = Rect::new(list.x1, r.y0, r.x1, r.y1);

        self.draw_sidebar(scene, t, tr, theme, sidebar);
        self.draw_list(scene, t, tr, theme, list);
        self.draw_detail(scene, t, tr, theme, detail);

        // Vertical pane dividers.
        draw::fill_rect(
            scene,
            t,
            Rect::new(sidebar.x1 - 0.5, r.y0, sidebar.x1 + 0.5, r.y1),
            c.border_standard,
        );
        draw::fill_rect(
            scene,
            t,
            Rect::new(list.x1 - 0.5, r.y0, list.x1 + 0.5, r.y1),
            c.border_standard,
        );

        if let Some(toast) = &self.toast {
            draw_toast(scene, t, tr, theme, self.rect, &toast.message);
        }

        // The expand modal renders above everything. While it's open the
        // panes beneath are inert, so the modal records hit regions
        // internally and the app's hit list isn't consulted.
        if let Some(mut expand) = self.expand.take() {
            expand.draw(scene, t, tr, theme, self.rect);
            self.expand = Some(expand);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Track the cursor for hit-testing.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // The expand modal is modal: it owns all input while open.
        if let Some(mut modal) = self.expand.take() {
            let outcome = modal.handle_event(event, self.last_mouse, modifiers);
            match outcome {
                ExpandOutcome::Stay => {
                    self.expand = Some(modal);
                    *handled = true;
                    return;
                }
                ExpandOutcome::Cancel => {
                    *handled = true;
                    return;
                }
                ExpandOutcome::Copy(text, id) => {
                    ui::clipboard_copy(&text);
                    if let Some(s) = self.library.get_mut(id) {
                        s.uses += 1;
                    }
                    self.toast("Expanded and copied");
                    *handled = true;
                    return;
                }
            }
        }

        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(action) = self.action_at(self.last_mouse) {
                    // Clicking anywhere that isn't the search field blurs it.
                    if !matches!(action, Action::FocusSearch) {
                        self.search_focused = false;
                    }
                    self.apply(action);
                    *handled = true;
                } else {
                    self.search_focused = false;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => *y as f64 * BODY_LINE,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).max(0.0);
                *handled = true;
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
                let mods = modifiers.state();
                // Cmd/Ctrl-F focuses search; Cmd/Ctrl-N new snippet;
                // Cmd/Ctrl-C copies the open snippet.
                if mods.control_key() || mods.super_key() {
                    if let Key::Character(ch) = logical_key {
                        match ch.to_lowercase().as_str() {
                            "f" => {
                                self.search_focused = true;
                                *handled = true;
                                return;
                            }
                            "n" => {
                                self.new_snippet();
                                *handled = true;
                                return;
                            }
                            "c" => {
                                if let Some(id) = self.open {
                                    self.copy(id);
                                }
                                *handled = true;
                                return;
                            }
                            _ => {}
                        }
                    }
                }

                if self.search_focused {
                    self.handle_search_key(logical_key, handled);
                    return;
                }

                // Escape clears search focus / search text.
                if let Key::Named(NamedKey::Escape) = logical_key {
                    if !self.search.is_empty() {
                        self.search.clear();
                        self.recompute_visible();
                        *handled = true;
                    }
                }
            }
            _ => {}
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.toast.is_some()
    }

    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool {
        // Age out the toast.
        if let Some(toast) = &mut self.toast {
            if toast.ttl == 0 {
                self.toast = None;
            } else {
                toast.ttl -= 1;
            }
        }
        false
    }

    fn name(&self) -> &str {
        "SnippetsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

/// Result of routing an event to the expand modal.
pub enum ExpandOutcome {
    /// Keep the modal open.
    Stay,
    /// User dismissed it.
    Cancel,
    /// User confirmed; the expanded text + originating snippet id.
    Copy(String, SnippetId),
}

impl SnippetsApp {
    fn handle_search_key(&mut self, key: &Key, handled: &mut bool) {
        match key {
            Key::Character(s) => {
                self.search.push_str(s.as_str());
                self.recompute_visible();
                *handled = true;
            }
            Key::Named(NamedKey::Space) => {
                self.search.push(' ');
                self.recompute_visible();
                *handled = true;
            }
            Key::Named(NamedKey::Backspace) => {
                self.search.pop();
                self.recompute_visible();
                *handled = true;
            }
            Key::Named(NamedKey::Escape) => {
                self.search.clear();
                self.search_focused = false;
                self.recompute_visible();
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                self.search_focused = false;
                *handled = true;
            }
            _ => {}
        }
    }

    // ---- sidebar --------------------------------------------------------

    fn draw_sidebar(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.base);
        let pad = 16.0;
        let mut y = area.y0 + 18.0;

        // App title.
        draw::text(
            scene,
            tr,
            t,
            "Snippets",
            area.x0 + pad,
            y,
            20.0,
            c.text_primary,
            TextWeight::Bold,
        );
        y += 34.0;

        // Smart folders.
        for smart in [SmartFolder::All, SmartFolder::Favorites, SmartFolder::MostUsed] {
            let active = matches!(&self.selection, Selection::Smart(s) if *s == smart);
            let count = self
                .library
                .query(&Selection::Smart(smart), "")
                .len();
            let row = self.sidebar_row(
                scene, t, tr, theme, area, y, smart.label(), Some(count), active, 0,
            );
            self.record(row, Action::Select(Selection::Smart(smart)));
            y += SIDEBAR_ROW_H;
        }

        y += 10.0;
        section_header(scene, t, tr, theme, "FOLDERS", area.x0 + pad, y);
        y += 22.0;
        for folder in self.library.folders() {
            let active = matches!(&self.selection, Selection::Folder(f) if *f == folder);
            let count = self
                .library
                .query(&Selection::Folder(folder.clone()), "")
                .len();
            let row =
                self.sidebar_row(scene, t, tr, theme, area, y, &folder, Some(count), active, 1);
            self.record(row, Action::Select(Selection::Folder(folder.clone())));
            y += SIDEBAR_ROW_H;
        }

        y += 10.0;
        section_header(scene, t, tr, theme, "TAGS", area.x0 + pad, y);
        y += 22.0;
        for (tag, cnt) in self.library.tags() {
            let active = matches!(&self.selection, Selection::Tag(x) if *x == tag);
            let label = format!("#{tag}");
            let row =
                self.sidebar_row(scene, t, tr, theme, area, y, &label, Some(cnt), active, 1);
            self.record(row, Action::Select(Selection::Tag(tag.clone())));
            y += SIDEBAR_ROW_H;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn sidebar_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        y: f64,
        label: &str,
        count: Option<usize>,
        active: bool,
        indent: u32,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let pad = 10.0;
        let row = Rect::new(area.x0 + 8.0, y, area.x1 - 8.0, y + SIDEBAR_ROW_H - 4.0);
        if active {
            draw::fill_round(scene, t, row, theme.radii_md(), c.accent[5]);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surface_white_5);
        }
        let fg = if active { c.text_primary } else { c.neutral[7] };
        let x = row.x0 + pad + indent as f64 * 12.0;
        let ty = y + (SIDEBAR_ROW_H - 4.0 - 15.0) / 2.0;
        draw::text(scene, tr, t, label, x, ty, 14.0, fg, TextWeight::Medium);
        if let Some(n) = count {
            let s = n.to_string();
            let w = tr.measure(&s, 12.0, None).0 as f64;
            let cfg = if active { c.accent[8] } else { c.text_dim };
            draw::text(scene, tr, t, &s, row.x1 - pad - w, ty + 1.0, 12.0, cfg, TextWeight::Regular);
        }
        row
    }

    // ---- list pane ------------------------------------------------------

    fn draw_list(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.neutral[1]);

        // Search bar.
        let pad = 12.0;
        let search_rect = Rect::new(
            area.x0 + pad,
            area.y0 + 10.0,
            area.x1 - pad,
            area.y0 + 10.0 + SEARCH_H - 16.0,
        );
        let focused = self.search_focused;
        let border = if focused { c.border_focus } else { c.border_standard };
        draw::fill_round(scene, t, search_rect, theme.radii_md(), c.surfaces.base);
        draw::stroke_round(scene, t, search_rect, theme.radii_md(), 1.5, border);
        let placeholder = self.search.is_empty();
        let shown = if placeholder {
            "Search  ·  try language:rust  or  tag:k8s".to_string()
        } else {
            self.search.clone()
        };
        let scolor = if placeholder { c.text_dim } else { c.text_primary };
        let sy = search_rect.y0 + (search_rect.height() - 15.0) / 2.0;
        draw::text(
            scene,
            tr,
            t,
            &shown,
            search_rect.x0 + 12.0,
            sy,
            14.0,
            scolor,
            TextWeight::Regular,
        );
        // Caret when focused.
        if focused {
            let tw = if placeholder {
                0.0
            } else {
                tr.measure(&self.search, 14.0, None).0 as f64
            };
            let cx = search_rect.x0 + 12.0 + tw + 1.0;
            draw::fill_rect(
                scene,
                t,
                Rect::new(cx, sy - 1.0, cx + 1.5, sy + 16.0),
                c.border_focus,
            );
        }
        self.record(search_rect, Action::FocusSearch);
        if !placeholder {
            // A small clear affordance.
            let clear = Rect::new(search_rect.x1 - 26.0, search_rect.y0, search_rect.x1, search_rect.y1);
            draw::text(
                scene,
                tr,
                t,
                "×",
                clear.x0 + 6.0,
                sy - 2.0,
                18.0,
                c.text_dim,
                TextWeight::Regular,
            );
            self.record(clear, Action::ClearSearch);
        }

        // "+ New" header row with the result count.
        let header_y = area.y0 + SEARCH_H + 4.0;
        let count_label = format!("{} snippets", self.visible.len());
        draw::text(
            scene,
            tr,
            t,
            &count_label,
            area.x0 + pad,
            header_y,
            12.0,
            c.text_dim,
            TextWeight::Medium,
        );
        let new_label = "+ New";
        let nw = tr.measure(new_label, 13.0, None).0 as f64 + 20.0;
        let new_rect = Rect::new(area.x1 - pad - nw, header_y - 4.0, area.x1 - pad, header_y + 18.0);
        draw::fill_round(scene, t, new_rect, theme.radii_md(), c.accent[5]);
        draw::text(
            scene,
            tr,
            t,
            new_label,
            new_rect.x0 + 10.0,
            new_rect.y0 + 4.0,
            13.0,
            c.text_primary,
            TextWeight::Semibold,
        );
        self.record(new_rect, Action::NewSnippet);

        // Snippet rows (scrollable region).
        let list_top = header_y + 26.0;
        let mut y = list_top - self.scroll;
        let ids = self.visible.clone();
        for id in ids {
            let Some(s) = self.library.get(id).cloned() else { continue };
            let row = Rect::new(area.x0 + 6.0, y, area.x1 - 6.0, y + ROW_H - 6.0);
            // Cull rows scrolled out of view but still record the hit so
            // clicks resolve; skip drawing if fully off-pane.
            if row.y1 >= list_top - 4.0 && row.y0 <= area.y1 {
                self.draw_list_row(scene, t, tr, theme, row, &s, self.open == Some(id));
            }
            self.record(row, Action::OpenSnippet(id));
            y += ROW_H;
        }

        if self.visible.is_empty() {
            draw::text(
                scene,
                tr,
                t,
                "No snippets match.",
                area.x0 + pad,
                list_top + 8.0,
                14.0,
                c.text_dim,
                TextWeight::Regular,
            );
        }
    }

    fn draw_list_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        s: &Snippet,
        active: bool,
    ) {
        let c = &theme.tokens.colors;
        if active {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surfaces.raised);
            draw::stroke_round(scene, t, row, theme.radii_md(), 1.0, c.accent[5]);
        } else if row.contains(self.last_mouse) {
            draw::fill_round(scene, t, row, theme.radii_md(), c.surface_white_5);
        }
        let pad = 12.0;
        // Language sigil badge.
        let badge = Rect::new(row.x0 + pad, row.y0 + 12.0, row.x0 + pad + 30.0, row.y0 + 32.0);
        draw::fill_round(scene, t, badge, 6.0, lang_tint(theme, s.language));
        let sig = s.language.sigil();
        let sw = tr.measure(sig, 11.0, None).0 as f64;
        draw::text(
            scene,
            tr,
            t,
            sig,
            badge.x0 + (badge.width() - sw) / 2.0,
            badge.y0 + 3.0,
            11.0,
            c.neutral[0],
            TextWeight::Bold,
        );

        let tx = badge.x1 + 10.0;
        // Title (+ star if favorite).
        let title = if s.favorite {
            format!("★ {}", s.title)
        } else {
            s.title.clone()
        };
        let tcolor = if s.favorite { c.semantic.warning } else { c.text_primary };
        draw::text(scene, tr, t, &title, tx, row.y0 + 10.0, 14.5, tcolor, TextWeight::Semibold);

        // Subtitle: first body line, dimmed and clipped.
        let first_line = s.body.lines().next().unwrap_or("").trim();
        let preview = clip(first_line, 42);
        draw::mono(
            scene,
            tr,
            t,
            &preview,
            tx,
            row.y0 + 30.0,
            12.0,
            c.text_dim,
            TextWeight::Regular,
        );

        // Trailing usage / placeholder hint.
        let hint = if s.has_placeholders() {
            format!("{} vars", s.placeholders().len())
        } else if s.uses > 0 {
            format!("{}×", s.uses)
        } else {
            String::new()
        };
        if !hint.is_empty() {
            let hw = tr.measure(&hint, 11.0, None).0 as f64;
            draw::text(
                scene,
                tr,
                t,
                &hint,
                row.x1 - pad - hw,
                row.y0 + 11.0,
                11.0,
                c.text_dim,
                TextWeight::Medium,
            );
        }
    }

    // ---- detail pane ----------------------------------------------------

    fn draw_detail(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        draw::fill_rect(scene, t, area, c.surfaces.base);
        let Some(open) = self.open else {
            draw::text(
                scene,
                tr,
                t,
                "Select a snippet, or press + New",
                area.x0 + 32.0,
                area.y0 + 40.0,
                15.0,
                c.text_dim,
                TextWeight::Regular,
            );
            return;
        };
        let Some(s) = self.library.get(open).cloned() else { return };
        let pad = 28.0;
        let mut y = area.y0 + 24.0;

        // Title.
        draw::text(scene, tr, t, &s.title, area.x0 + pad, y, 24.0, c.text_primary, TextWeight::Bold);
        y += 36.0;

        // Meta row: language pill + folder + tags.
        let mut x = area.x0 + pad;
        let lp = draw::pill(
            scene,
            tr,
            t,
            s.language.label(),
            x,
            y,
            22.0,
            12.0,
            c.neutral[0],
            lang_tint(theme, s.language),
        );
        // Clicking the language pill cycles to the next language.
        let next = next_language(s.language);
        self.record(lp, Action::SetLanguage(open, next));
        x = lp.x1 + 8.0;
        if !s.folder.is_empty() {
            let fp = draw::pill(scene, tr, t, &s.folder, x, y, 22.0, 12.0, c.text_dim, c.neutral[2]);
            x = fp.x1 + 8.0;
        }
        for tag in &s.tags {
            let label = format!("#{tag}");
            let tp = draw::pill(scene, tr, t, &label, x, y, 22.0, 12.0, c.accent[7], c.accent[1]);
            self.record(tp, Action::Select(Selection::Tag(tag.clone())));
            x = tp.x1 + 8.0;
        }
        y += 34.0;

        // Description.
        if !s.description.is_empty() {
            draw::text(
                scene,
                tr,
                t,
                &s.description,
                area.x0 + pad,
                y,
                14.0,
                c.text_dim,
                TextWeight::Regular,
            );
            y += 26.0;
        }

        // Action toolbar: Copy / Expand / Favorite / Duplicate / Delete.
        y += 4.0;
        let mut bx = area.x0 + pad;
        let primary_label = if s.has_placeholders() { "Fill & Copy" } else { "Copy" };
        bx = self.toolbar_button(scene, t, tr, theme, bx, y, primary_label, true, Action::Copy(open));
        if s.has_placeholders() {
            bx = self.toolbar_button(scene, t, tr, theme, bx, y, "Variables…", false, Action::Expand(open));
        }
        let star_label = if s.favorite { "★ Starred" } else { "☆ Star" };
        bx = self.toolbar_button(scene, t, tr, theme, bx, y, star_label, false, Action::ToggleFavorite(open));
        bx = self.toolbar_button(scene, t, tr, theme, bx, y, "Duplicate", false, Action::Duplicate(open));
        let _ = self.toolbar_button(scene, t, tr, theme, bx, y, "Delete", false, Action::Delete(open));
        y += 44.0;

        // Code card with syntax-highlighted body.
        let card = Rect::new(area.x0 + pad, y, area.x1 - pad, area.y1 - 20.0);
        draw::fill_round(scene, t, card, theme.radii_md(), c.neutral[0]);
        draw::stroke_round(scene, t, card, theme.radii_md(), 1.0, c.border_standard);
        self.draw_code(scene, t, tr, theme, card, &s);
    }

    /// Draw a toolbar button, record its hit region against `action`, and
    /// return the x-coordinate the next button should start at.
    #[allow(clippy::too_many_arguments)]
    fn toolbar_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        label: &str,
        primary: bool,
        action: Action,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let tw = tr.measure(label, 13.0, None).0 as f64;
        let w = tw + 24.0;
        let rect = Rect::new(x, y, x + w, y + 32.0);
        let hovered = rect.contains(self.last_mouse);
        let (bg, fg) = if primary {
            (if hovered { c.accent[4] } else { c.accent[5] }, c.text_primary)
        } else if hovered {
            (c.surfaces.raised, c.text_primary)
        } else {
            (c.neutral[2], c.neutral[7])
        };
        draw::fill_round(scene, t, rect, theme.radii_md(), bg);
        if !primary {
            draw::stroke_round(scene, t, rect, theme.radii_md(), 1.0, c.border_standard);
        }
        let ty = y + (32.0 - 14.0) / 2.0;
        draw::text(scene, tr, t, label, x + 12.0, ty, 13.0, fg, TextWeight::Semibold);
        self.record(rect, action);
        rect.x1 + 8.0
    }

    fn draw_code(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        s: &Snippet,
    ) {
        let c = &theme.tokens.colors;
        let pad = 16.0;
        let gutter_w = 36.0;
        let x0 = card.x0 + pad;
        // Gutter background.
        let gutter = Rect::new(card.x0, card.y0, card.x0 + gutter_w + pad, card.y1);
        draw::fill_round(scene, t, gutter, theme.radii_md(), c.neutral[1]);
        // Re-cover the right side so only the left edge keeps the radius.
        draw::fill_rect(
            scene,
            t,
            Rect::new(card.x0 + 10.0, card.y0, card.x0 + gutter_w + pad, card.y1),
            c.neutral[1],
        );

        let mut y = card.y0 + pad;
        let line_h = BODY_LINE;
        let code_x = x0 + gutter_w;
        for (i, line) in s.body.lines().enumerate() {
            if y > card.y1 - line_h {
                break;
            }
            // Line number.
            let num = (i + 1).to_string();
            let nw = draw::mono_width(tr, &num, 11.0);
            draw::mono(
                scene,
                tr,
                t,
                &num,
                x0 + gutter_w - 10.0 - nw,
                y,
                11.0,
                c.text_dim,
                TextWeight::Regular,
            );
            // Highlighted spans.
            let mut sx = code_x;
            for span in highlight::highlight_line(line, s.language) {
                let color = span.kind.color(theme);
                let weight = if matches!(span.kind, highlight::TokenKind::Keyword) {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                };
                draw::mono(scene, tr, t, &span.text, sx, y, BODY_FONT, color, weight);
                sx += draw::mono_width(tr, &span.text, BODY_FONT);
            }
            y += line_h;
        }
    }
}

/// Cycle to the next language in the canonical order (wraps).
fn next_language(lang: Language) -> Language {
    let i = Language::ALL.iter().position(|l| *l == lang).unwrap_or(0);
    Language::ALL[(i + 1) % Language::ALL.len()]
}

/// A per-language accent tint for the badge. Derived from theme ramps so
/// it tracks the active palette (never a hard-coded hex).
fn lang_tint(theme: &Theme, lang: Language) -> vello::peniko::Color {
    let c = &theme.tokens.colors;
    match lang {
        Language::Rust | Language::Toml => c.semantic.warning,
        Language::Shell | Language::Dockerfile => c.semantic.info,
        Language::Sql | Language::Yaml | Language::Json => c.accent[5],
        Language::Git => c.semantic.danger,
        Language::Python => c.semantic.success,
        Language::JavaScript => c.accent[6],
        Language::Markdown | Language::Html | Language::Css => c.neutral[5],
        Language::PlainText => c.neutral[4],
    }
}

fn section_header(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    label: &str,
    x: f64,
    y: f64,
) {
    draw::text(
        scene,
        tr,
        t,
        label,
        x,
        y,
        11.0,
        theme.tokens.colors.text_dim,
        TextWeight::Bold,
    );
}

fn draw_toast(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    message: &str,
) {
    let c = &theme.tokens.colors;
    let w = tr.measure(message, 14.0, None).0 as f64 + 40.0;
    let h = 40.0;
    let x = area.x1 - w - 24.0;
    let y = area.y1 - h - 24.0;
    let rect = Rect::new(x, y, x + w, y + h);
    draw::fill_round(scene, t, rect, theme.radii_md(), c.surfaces.raised);
    draw::stroke_round(scene, t, rect, theme.radii_md(), 1.0, c.accent[5]);
    draw::text(
        scene,
        tr,
        t,
        message,
        x + 20.0,
        y + (h - 15.0) / 2.0,
        14.0,
        c.text_primary,
        TextWeight::Medium,
    );
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Convenience radius accessor used throughout the panes.
trait Radii {
    fn radii_md(&self) -> f64;
}
impl Radii for Theme {
    fn radii_md(&self) -> f64 {
        self.tokens.radii.md as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_app_seeds_and_opens_a_snippet() {
        let app = SnippetsApp::new();
        assert!(app.library_len_for_test() >= 10);
        assert!(app.open_id_for_test().is_some());
        assert!(app.visible_count_for_test() >= 10);
    }

    #[test]
    fn search_narrows_results() {
        let mut app = SnippetsApp::new();
        let before = app.visible_count_for_test();
        app.set_search_for_test("language:rust");
        let after = app.visible_count_for_test();
        assert!(after < before);
        assert!(after >= 1);
    }

    #[test]
    fn copying_a_plain_snippet_bumps_uses() {
        let mut app = SnippetsApp::new();
        // Find a snippet without placeholders.
        let id = app
            .library
            .snippets
            .iter()
            .find(|s| !s.has_placeholders())
            .map(|s| s.id)
            .expect("a placeholder-free snippet exists");
        let before = app.snippet_uses_for_test(id);
        app.copy_for_test(id);
        assert_eq!(app.snippet_uses_for_test(id), before + 1);
        assert!(!app.expand_open_for_test());
    }

    #[test]
    fn copying_a_templated_snippet_opens_expand() {
        let mut app = SnippetsApp::new();
        let id = app
            .library
            .snippets
            .iter()
            .find(|s| s.has_placeholders())
            .map(|s| s.id)
            .expect("a templated snippet exists");
        app.copy_for_test(id);
        assert!(app.expand_open_for_test(), "templated copy routes through expand");
    }

    #[test]
    fn new_snippet_grows_library_and_opens_it() {
        let mut app = SnippetsApp::new();
        let before = app.library_len_for_test();
        app.new_snippet();
        assert_eq!(app.library_len_for_test(), before + 1);
        assert!(app.open_id_for_test().is_some());
    }

    #[test]
    fn next_language_wraps() {
        let last = *Language::ALL.last().unwrap();
        assert_eq!(next_language(last), Language::ALL[0]);
    }

    #[test]
    fn sync_seeds_into_empty_doc() {
        use ui::widget::Widget;
        let mut app = SnippetsApp::new();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.sync(&mut doc, &root);
        assert_eq!(SnippetsDoc::list(&doc).len(), app.library_len_for_test());
    }
}
