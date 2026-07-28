//! `StoreApp` — the root widget.
//!
//! Composition sketch:
//!
//! ```text
//! ┌ Store header (search box + refresh)  ─────────────────────────┐
//! │ [    Search…                       ] [ Refresh ]              │
//! ├───────────────────────────────────────────────────────────────┤
//! │ [ All | qOS | Flatpak | Installed ]      ← filter chip row    │
//! ├───────────────────────────────────────────────────────────────┤
//! │ Categories: [ All | Games | Productivity | System | Utils ]   │
//! ├───────────────────────────────────────────────────────────────┤
//! │  ┌───────────────────────────────────────────────────────┐    │
//! │  │  [icon]  Notes                v0.1.0 · qOS main [Add] │    │
//! │  │  [ ⎔  ]  A CRDT notes app                             │    │
//! │  └───────────────────────────────────────────────────────┘    │
//! │  ... more cards ...                                           │
//! ├───────────────────────────────────────────────────────────────┤
//! │ Status: "Installing dev.qos.notes 0.1.0 from qOS main…"       │
//! └───────────────────────────────────────────────────────────────┘
//! ```
//!
//! The widget is intentionally read-mostly: the source-of-truth for
//! the item list is refreshed by an outside "refresh" call
//! ([`StoreApp::set_items`]) rather than on every frame, so a shot
//! or a test can inject a deterministic catalog without spinning
//! up an HTTP server.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use automerge::transaction::Transactable;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::widget::toggle_group::Segment;
use ui::widget::{Column, Container, Row, Spacer, Text, TextInput, ToggleGroup, Widget};
use ui::{Theme, presence::PresenceCtx, permissions::PermissionContext, text_render::TextRenderer};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::model::{ActionKind, SourceFilter, StoreItem};
use crate::sources::distinct_categories;

/// One-shot CRDT keys the sub-widgets write on click. The names are
/// prefixed with `store.` so the toplevel `state.doc` root object
/// can hold them without colliding with sibling widgets when the
/// app is hosted inside a tiled shell.
const CATEGORY_SELECT_KEY: &str = "store.category_select";
const REFRESH_KEY: &str = "store.refresh_pressed";
/// CRDT `Text` object the header search box binds to. `sync` seeds
/// the object (the toolkit's `TextInput` binds to an existing `Text`
/// but never lazily creates one) and reads the typed query back out.
const SEARCH_QUERY_KEY: &str = "store.search_query";
const ACTION_CLICK_KEY_PREFIX: &str = "store.action_click.";
/// Secondary "mark installed" (pin) toggle, one per item id.
const PIN_CLICK_KEY_PREFIX: &str = "store.pin_click.";
/// Secondary "remove" affordance on a cached qOS card, one per item id.
const REMOVE_CLICK_KEY_PREFIX: &str = "store.remove_click.";

/// Root widget.
pub struct StoreApp {
    /// Merged item feed.
    items: Vec<StoreItem>,
    /// Active source-side filter (`All | qOS | Flatpak | Installed`).
    active_filter: SourceFilter,
    /// Active category or `None` for "all categories".
    active_category: Option<String>,
    /// Current search query. Persisted; the display is a placeholder
    /// stub until we wire a real `TextInput` (kept a follow-up for
    /// scope — the filter/chip pipeline is what proves the layout).
    query: String,
    /// Rolling status line the worker overwrites while an action
    /// runs. Rendered in a small band at the bottom of the surface.
    status_line: String,
    /// Whether `flatpak` is available on the host — drives whether
    /// Flatpak items show "Install" or the "install flatpak first"
    /// helper.
    flatpak_available: bool,
    /// Which item the user has expanded to the detail sheet, if any.
    /// Keyed by [`StoreItem::id`]. Reserved for the follow-up
    /// slide-in-detail sheet; kept on the widget so the CRDT sync
    /// can restore the open state across window rebuilds.
    #[allow(dead_code)]
    focused_detail: Option<String>,
    /// Marked dirty when a chip / category selection flips so the
    /// host re-runs `build_layout`.
    structure_dirty: bool,
    /// The rect the toplevel container resolves into during
    /// `apply_layout` — needed for `get_rect`.
    rect: Rect,
    /// Root of the built layout so events can be routed to the
    /// composed sub-tree.
    root: Option<Box<dyn Widget>>,
    /// Actions the user has clicked this frame that a worker still
    /// needs to execute (download / install / run / pin / remove).
    /// Drained by the host each frame via [`take_pending_actions`].
    pending_actions: Vec<(String, ActionKind)>,
    /// Segment index the source-filter `ToggleGroup` last committed.
    /// The `on_change` closure is `'static` and can't borrow `self`,
    /// so it parks the click here; `sync` reads it back and flips
    /// [`active_filter`]. Shared (`Rc`) because the closure is rebuilt
    /// into a fresh `ToggleGroup` every `build_layout`.
    filter_click: Rc<Cell<Option<usize>>>,
}

impl Default for StoreApp {
    fn default() -> Self {
        Self::new()
    }
}

impl StoreApp {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            active_filter: SourceFilter::All,
            active_category: None,
            query: String::new(),
            status_line: String::new(),
            flatpak_available: crate::flathub_client::flatpak_available(),
            focused_detail: None,
            structure_dirty: true,
            rect: Rect::ZERO,
            root: None,
            pending_actions: Vec::new(),
            filter_click: Rc::new(Cell::new(None)),
        }
    }

    /// Drain the actions clicked since the last frame. The host's
    /// worker executes each one (off the UI thread) and streams a
    /// fresh catalog + status back.
    pub fn take_pending_actions(&mut self) -> Vec<(String, ActionKind)> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Replace the item feed. Idempotent — the widget re-derives its
    /// category chip row from `items` on the next `build_layout`.
    pub fn set_items(&mut self, items: Vec<StoreItem>) {
        self.items = items;
        self.structure_dirty = true;
    }

    pub fn items(&self) -> &[StoreItem] {
        &self.items
    }

    /// Override the flatpak-availability probe result. Tests use
    /// this so they don't have to depend on the host's `flatpak`
    /// binary.
    pub fn set_flatpak_available(&mut self, available: bool) {
        self.flatpak_available = available;
        self.structure_dirty = true;
    }

    pub fn active_filter(&self) -> SourceFilter {
        self.active_filter
    }

    pub fn set_active_filter(&mut self, f: SourceFilter) {
        if self.active_filter != f {
            self.active_filter = f;
            self.structure_dirty = true;
        }
    }

    pub fn active_category(&self) -> Option<&str> {
        self.active_category.as_deref()
    }

    pub fn set_active_category(&mut self, c: Option<String>) {
        if self.active_category != c {
            self.active_category = c;
            self.structure_dirty = true;
        }
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, q: String) {
        if self.query != q {
            self.query = q;
            self.structure_dirty = true;
        }
    }

    pub fn set_status_line(&mut self, s: impl Into<String>) {
        self.status_line = s.into();
    }

    pub fn status_line(&self) -> &str {
        &self.status_line
    }

    /// The visible-after-filters item list. Kept public because
    /// tests + the detail-sheet reducer both need it.
    pub fn visible_items(&self) -> Vec<&StoreItem> {
        let cat = self.active_category.as_deref();
        let mut hits: Vec<&StoreItem> = self
            .items
            .iter()
            .filter(|i| self.active_filter.accepts(i))
            .filter(|i| match cat {
                Some(c) => i.categories.iter().any(|k| k == c),
                None => true,
            })
            .filter(|i| i.matches_query(&self.query))
            .collect();
        // Ranking: when idle, marked-installed (pinned) apps sort
        // first, then merely-cached, then everything else —
        // alphabetised within each band so the list is stable across
        // refreshes. When a query is active the best fuzzy match wins
        // regardless of install state.
        if self.query.trim().is_empty() {
            hits.sort_by(|a, b| {
                b.install_rank()
                    .cmp(&a.install_rank())
                    .then_with(|| {
                        a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase())
                    })
            });
        } else {
            let q = self.query.clone();
            hits.sort_by(|a, b| b.fuzzy_score(&q).cmp(&a.fuzzy_score(&q)));
        }
        hits
    }

    /// Category chips: `All` first, then every distinct category
    /// derived from the item feed.
    fn category_labels(&self) -> Vec<String> {
        let mut all = vec!["All".to_string()];
        all.extend(distinct_categories(&self.items));
        all
    }

    // ---- layout ----

    /// Compose the whole tree. Called from `build_layout`; also used
    /// by tests to inspect the composition graph directly.
    fn compose(&self) -> Box<dyn Widget> {
        let header = self.build_header();
        let filter_row = self.build_filter_row();
        let category_row = self.build_category_row();
        let list = self.build_list();
        let status = self.build_status_bar();
        let column = Column::new()
            .with_intrinsic_height()
            .push(header)
            .push(filter_row)
            .push(category_row)
            .push(list)
            .push(status);
        Box::new(
            Container::new()
                .filling()
                .with_child(Box::new(column)),
        )
    }

    fn build_header(&self) -> Box<dyn Widget> {
        // Live search box: a real `TextInput` bound to a CRDT `Text`
        // object at `SEARCH_QUERY_KEY`. `sync` seeds the object and
        // reads the typed value back into `self.query`, which drives
        // `visible_items`' fuzzy filter.
        let search = TextInput::new(vec![])
            .with_id("store.search")
            .with_on_change_path(SEARCH_QUERY_KEY)
            .with_placeholder("Search apps…");
        // Refresh button writes REFRESH_KEY; the outer host drives
        // the real refetch when it sees the bit go high.
        let refresh_btn =
            ui::widget::Button::new("store.refresh", "Refresh", Some(REFRESH_KEY));
        let row = Row::new()
            .no_wrap()
            .push(Box::new(search))
            .push(Box::new(Spacer::new()))
            .push(Box::new(refresh_btn));
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(row)),
        )
    }

    fn build_filter_row(&self) -> Box<dyn Widget> {
        let segs: Vec<Segment> = SourceFilter::ALL
            .iter()
            .map(|f| Segment::new(f.label()))
            .collect();
        let selected = SourceFilter::ALL
            .iter()
            .position(|f| *f == self.active_filter)
            .unwrap_or(0);
        // The `on_change` callback runs inside `handle_event` and is
        // `'static`, so it can't borrow `self`. It parks the clicked
        // segment index in the shared `filter_click` cell; `sync`
        // reads it back next frame and flips `active_filter`, which
        // re-filters the catalog.
        let sink = self.filter_click.clone();
        let group = ToggleGroup::new(segs, selected).on_change(move |idx| {
            sink.set(Some(idx));
        });
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(group)),
        )
    }

    fn build_category_row(&self) -> Box<dyn Widget> {
        let labels = self.category_labels();
        // One Button per category. The active category is tracked in
        // `active_category`; the button label is the plain category
        // name (no bracket ornament).
        let mut row = Row::new();
        for label in labels {
            let key = format!("{CATEGORY_SELECT_KEY}.{label}");
            let btn = ui::widget::Button::new(&format!("store.cat.{label}"), &label, Some(&key));
            row = row.push(Box::new(btn));
        }
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(row)),
        )
    }

    fn build_list(&self) -> Box<dyn Widget> {
        let visible = self.visible_items();
        if visible.is_empty() {
            let text = if self.items.is_empty() {
                "Loading store…"
            } else {
                "No apps match this filter."
            };
            return Box::new(
                Container::new()
                    .with_background()
                    .with_child(Box::new(
                        Text::new(text)
                            .with_size(13.0)
                            .with_color(Theme::dark().text_dim),
                    )),
            );
        }
        // The card list is a plain column of cards. A `Scrollable`
        // wrapper is deliberately omitted because the retained
        // toolkit's scrollable + intrinsic-height card interaction
        // is unstable in headless render paths (see the settings
        // pane comment); we render a fixed head of items for the
        // initial cut and pick this up as a follow-up.
        const MAX_ROWS: usize = 30;
        let mut column = Column::new().with_intrinsic_height();
        for item in visible.iter().take(MAX_ROWS) {
            let action = item.action_kind(self.flatpak_available);
            let click_key = format!("{}{}", ACTION_CLICK_KEY_PREFIX, item.id);
            let select_key = format!("store.detail_select.{}", item.id);
            let pin_key = format!("{}{}", PIN_CLICK_KEY_PREFIX, item.id);
            let remove_key = format!("{}{}", REMOVE_CLICK_KEY_PREFIX, item.id);
            column = column.push(crate::widgets::store_card(
                item,
                action,
                &select_key,
                &click_key,
                &pin_key,
                &remove_key,
            ));
        }
        Box::new(
            Container::new()
                .filling()
                .with_child(Box::new(column)),
        )
    }

    fn build_status_bar(&self) -> Box<dyn Widget> {
        let line = if self.status_line.is_empty() {
            format!(
                "{} apps · {} shown",
                self.items.len(),
                self.visible_items().len()
            )
        } else {
            self.status_line.clone()
        };
        Box::new(
            Container::new()
                .with_background()
                .with_child(Box::new(
                    Text::new(&line)
                        .with_size(11.0)
                        .with_color(Theme::dark().text_dim),
                )),
        )
    }

    // ---- CRDT sync helpers ----

    /// Read the one-shot click keys the sub-widgets write and turn
    /// them into filter / category / action mutations. Returns
    /// `true` if anything changed (used by callers as a signal to
    /// re-render).
    fn drain_click_keys(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        // Source-filter chips are wired through the `filter_click`
        // cell (see `sync`), not a CRDT key, so there is no filter
        // loop here.
        // Category chips.
        for label in self.category_labels() {
            let key = format!("{}.{}", CATEGORY_SELECT_KEY, label);
            if take_bool_edge(state, path, &key) {
                self.active_category = if label == "All" { None } else { Some(label.clone()) };
                self.structure_dirty = true;
                any = true;
            }
        }
        // Primary action clicks (the big button on each card).
        if let Some((id, action)) = self.take_action_click(state, path) {
            self.status_line = match action {
                ActionKind::Open => format!("Opening {id}…"),
                ActionKind::Update => format!("Updating {id}…"),
                ActionKind::Remove => format!("Removing {id}…"),
                ActionKind::MarkInstalled => format!("Marking {id} installed…"),
                ActionKind::Unmark => format!("Unmarking {id}…"),
                ActionKind::FlatpakInstall => format!("Installing Flatpak {id}…"),
                ActionKind::FlatpakRemove => format!("Removing Flatpak {id}…"),
                ActionKind::NeedsFlatpakHost => {
                    "Install flatpak on the host to use this action.".to_string()
                }
            };
            if action != ActionKind::NeedsFlatpakHost {
                self.pending_actions.push((id, action));
            }
            any = true;
        }
        // Secondary clicks: the pin ("mark installed") toggle and the
        // Remove affordance on cached qOS cards. Collect first so the
        // item borrow ends before we mutate `self`.
        let mut secondary: Vec<(String, ActionKind)> = Vec::new();
        for item in &self.items {
            let pin_key = format!("{}{}", PIN_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &pin_key) {
                if let Some(action) = item.pin_action() {
                    secondary.push((item.id.clone(), action));
                }
            }
            let rm_key = format!("{}{}", REMOVE_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &rm_key) && item.offers_remove() {
                secondary.push((item.id.clone(), ActionKind::Remove));
            }
        }
        for (id, action) in secondary {
            self.status_line = format!("{} {}…", action.label(), id);
            self.pending_actions.push((id, action));
            any = true;
        }
        // Refresh.
        if take_bool_edge(state, path, REFRESH_KEY) {
            self.status_line = "Refreshing…".to_string();
            any = true;
        }
        any
    }

    /// Look for a `store.action_click.<id> = true` key. Returns the
    /// id + the action the item's action-kind resolved to.
    fn take_action_click(
        &self,
        state: &mut AutoCommit,
        path: &ObjId,
    ) -> Option<(String, ActionKind)> {
        for item in &self.items {
            let key = format!("{}{}", ACTION_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &key) {
                return Some((item.id.clone(), item.action_kind(self.flatpak_available)));
            }
        }
        None
    }
}

/// Resolve (or lazily create) the `Text` object at `path`.`key`. The
/// toolkit's `TextInput` binds to an existing `Text` but never creates
/// one, so the host must seed it before the input syncs.
fn ensure_text_obj(state: &mut AutoCommit, root: &ObjId, key: &str) -> ObjId {
    if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, key) {
        return id;
    }
    state
        .put_object(root, key, ObjType::Text)
        .expect("put_object Text never fails")
}

/// Read a `path`.`key` boolean and clear it in one shot. `true`
/// return means the key was true; the CRDT is left with the key
/// false so the next frame sees a fresh edge.
fn take_bool_edge(state: &mut AutoCommit, path: &ObjId, key: &str) -> bool {
    let hit = matches!(
        state.get(path, key),
        Ok(Some((Value::Scalar(s), _)))
            if matches!(s.as_ref(), ScalarValue::Boolean(true))
    );
    if hit {
        let _ = state.put(path, key, ScalarValue::Boolean(false));
    }
    hit
}

impl Widget for StoreApp {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        // A source-filter chip parked its segment index in the shared
        // cell during the last `handle_event`; adopt it now.
        if let Some(idx) = self.filter_click.take() {
            if let Some(f) = SourceFilter::ALL.get(idx).copied() {
                self.set_active_filter(f);
            }
        }
        // Seed the search box's backing `Text` object *before* the
        // children sync so the `TextInput` resolves a real object to
        // splice keystrokes into (it binds to an existing `Text` but
        // never creates one).
        let search_obj = ensure_text_obj(state, path, SEARCH_QUERY_KEY);
        let _ = self.drain_click_keys(state, path);
        if let Some(root) = self.root.as_mut() {
            root.sync(state, path);
        }
        // Pull the typed query back out and re-filter if it changed.
        if let Ok(text) = state.text(&search_obj) {
            self.set_query(text);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.structure_dirty = false;
        let mut root = self.compose();
        let child = root.build_layout(taffy, theme, node_map);
        self.root = Some(root);
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
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
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node)) {
            if let Some(&child_node) = children.first() {
                root.apply_layout(
                    taffy,
                    child_node,
                    Point::new(self.rect.x0, self.rect.y0),
                );
            }
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(root) = self.root.as_mut() {
            root.draw(scene, transform, text_renderer, theme, presence);
        }
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &PermissionContext,
    ) {
        if let Some(root) = self.root.as_mut() {
            root.draw_with_presence(scene, transform, text_renderer, theme, presence, permissions);
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node_id)) {
            if let Some(&child_node) = children.first() {
                root.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    child_node,
                    taffy,
                );
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.root
            .as_ref()
            .map(|r| r.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.root
            .as_mut()
            .map(|r| r.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "StoreApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .root
            .as_mut()
            .map(|r| r.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|r| r.needs_animation_frame())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{InstalledState, StoreItem, StoreItemVersion, StoreSource};

    fn item(name: &str, source: StoreSource, cat: &str, installed: InstalledState) -> StoreItem {
        StoreItem {
            id: format!("id.{}", name.to_ascii_lowercase()),
            name: name.into(),
            description: format!("{name} description"),
            categories: vec![cat.into()],
            icon_url: None,
            homepage_url: None,
            source_url: None,
            source,
            versions: vec![StoreItemVersion {
                version: "1".into(),
                released_at_unix: 0,
                download_url: None,
                root_hash_hex: None,
                size_bytes: 0,
            }],
            publisher_pubkey_hex: None,
            installed,
        }
    }

    fn seeded_app() -> StoreApp {
        let mut app = StoreApp::new();
        app.set_flatpak_available(true);
        app.set_items(vec![
            item(
                "Notes",
                StoreSource::Qos { registry_name: "qOS".into() },
                "Productivity",
                InstalledState::Not,
            ),
            item(
                "Terminal",
                StoreSource::Qos { registry_name: "qOS".into() },
                "System",
                InstalledState::Installed {
                    version: "1".into(),
                    available: "1".into(),
                    pinned: false,
                },
            ),
            item(
                "Chess",
                StoreSource::Flathub,
                "Games",
                InstalledState::Not,
            ),
        ]);
        app
    }

    #[test]
    fn visible_items_show_everything_when_filter_is_all() {
        let app = seeded_app();
        assert_eq!(app.visible_items().len(), 3);
    }

    #[test]
    fn filter_qos_hides_flatpak() {
        let mut app = seeded_app();
        app.set_active_filter(SourceFilter::Qos);
        let names: Vec<&str> = app.visible_items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Notes"));
        assert!(names.contains(&"Terminal"));
        assert!(!names.contains(&"Chess"));
    }

    #[test]
    fn filter_installed_shows_only_installed() {
        let mut app = seeded_app();
        app.set_active_filter(SourceFilter::Installed);
        let names: Vec<&str> = app.visible_items().iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["Terminal"]);
    }

    #[test]
    fn category_filter_gates_the_list() {
        let mut app = seeded_app();
        app.set_active_category(Some("Games".into()));
        let names: Vec<&str> = app.visible_items().iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["Chess"]);
    }

    #[test]
    fn search_narrows_and_ranks_by_score() {
        let mut app = seeded_app();
        // "Term" only matches Terminal — Notes contains "te" but not
        // "term", so the substring gate drops it.
        app.set_query("Term".into());
        let visible = app.visible_items();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].name, "Terminal");
    }

    #[test]
    fn search_ranks_name_prefix_hits_first() {
        let mut app = seeded_app();
        // Both "Notes" (name contains "te" mid-string) and
        // "Terminal" (name starts with "Te") match "te".
        app.set_query("te".into());
        let visible = app.visible_items();
        assert!(visible.len() >= 2);
        assert_eq!(visible[0].name, "Terminal", "prefix hit ranks first");
    }

    #[test]
    fn categories_include_all_plus_distinct() {
        let app = seeded_app();
        let cats = app.category_labels();
        assert_eq!(cats[0], "All");
        assert!(cats.iter().any(|c| c == "Productivity"));
        assert!(cats.iter().any(|c| c == "Games"));
        assert!(cats.iter().any(|c| c == "System"));
    }

    #[test]
    fn live_constructor_starts_empty() {
        // The live app must show a real (empty) catalog until the
        // refresh worker streams one — never a fabricated one.
        let app = StoreApp::new();
        assert!(
            app.items().is_empty(),
            "live StoreApp must not fabricate a catalog"
        );
        assert_eq!(app.active_filter(), SourceFilter::All);
        assert!(app.query().is_empty());
    }

    #[test]
    fn typed_search_query_filters_through_sync() {
        use automerge::transaction::Transactable;
        // Prove the wired search box: a query spliced into the bound
        // CRDT `Text` object is read back by `sync` and narrows the
        // visible list.
        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        // First sync seeds the Text object.
        app.sync(&mut doc, &root);
        let obj = ensure_text_obj(&mut doc, &root, SEARCH_QUERY_KEY);
        doc.splice_text(&obj, 0, 0, "Term").unwrap();
        app.sync(&mut doc, &root);
        assert_eq!(app.query(), "Term");
        let names: Vec<&str> = app.visible_items().iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["Terminal"]);
    }

    #[test]
    fn filter_chip_click_flips_active_filter_through_sync() {
        // Prove the wired filter chips: the `on_change` closure parks
        // a segment index in `filter_click`; `sync` adopts it and the
        // catalog re-filters.
        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        app.filter_click.set(Some(1)); // segment 1 == SourceFilter::Qos
        app.sync(&mut doc, &root);
        assert_eq!(app.active_filter(), SourceFilter::Qos);
        let names: Vec<&str> = app.visible_items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Notes"));
        assert!(!names.contains(&"Chess"), "qOS filter hides Flatpak items");
    }
}
