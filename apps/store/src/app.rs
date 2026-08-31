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
//! │  ... more cards, spanning to the window bottom ...            │
//! │                                    ╭─────────────────────────╮ │
//! │                                    │ Installing dev.qos.notes…│ │  ← transient bubble
//! └────────────────────────────────────╰─────────────────────────╯─┘
//! ```
//!
//! There is no permanent footer strip: the catalog counts (`N apps · M
//! shown`) and the running action status surface in a floating
//! [`ui::StatusBubble`], revealed on a change or on hover of the
//! bottom-right band, then faded — so the card list reclaims the height.
//!
//! The widget is intentionally read-mostly: the source-of-truth for
//! the item list is refreshed by an outside "refresh" call
//! ([`StoreApp::set_items`]) rather than on every frame, so a shot
//! or a test can inject a deterministic catalog without spinning
//! up an HTTP server.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::widget::toggle_group::Segment;
use ui::widget::{Column, Container, Row, Spacer, Text, TextInput, ToggleGroup, Widget};
use ui::{Theme, permissions::PermissionContext, presence::PresenceCtx, text_render::TextRenderer};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::model::{ActionKind, SourceFilter, StoreItem};
use crate::sources::distinct_categories;

/// Build a taffy node from `children` already built in the same tree.
/// `new_with_children` only fails when a child id does not resolve in this
/// tree, which cannot happen for children this same call just built.
#[allow(
    clippy::unwrap_used,
    reason = "new_with_children cannot fail for children built in this same tree; see doc comment"
)]
fn parent_node(taffy: &mut TaffyTree<()>, style: Style, children: &[NodeId]) -> NodeId {
    taffy.new_with_children(style, children).unwrap()
}

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
const DETAIL_SELECT_KEY_PREFIX: &str = "store.detail_select.";
const DETAIL_BACK_KEY: &str = "store.detail_back";
const REMOTES_OPEN_KEY: &str = "store.remotes_open";
const REMOTES_BACK_KEY: &str = "store.remotes_back";
const REMOTES_REFRESH_KEY: &str = "store.remotes_refresh";
const REMOTES_ADD_KEY: &str = "store.remotes_add";
const REMOTES_REMOVE_KEY_PREFIX: &str = "store.remotes_remove.";
const REMOTE_NAME_KEY: &str = "store.remote_name";
const REMOTE_URL_KEY: &str = "store.remote_url";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteIntent {
    Refresh,
    Add { name: String, url: String },
    Remove(String),
}

/// Root widget.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent, unrelated flag (host flatpak \
              availability, a layout-rebuild gate, a pending worker action, \
              the remotes view's open/closed state, and a --shots-only seam); \
              grouping them into enums would not express any real invariant \
              between them and would touch sync/draw/handle_event call sites \
              across this file"
)]
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
    /// runs. Surfaced transiently in the floating status bubble (see
    /// [`draw_status_bubble`](Self::draw_status_bubble)); the catalog
    /// counts stand in while it is empty.
    status_line: String,
    /// Whether `flatpak` is available on the host — drives whether
    /// Flatpak items show "Install" or the "install flatpak first"
    /// helper.
    flatpak_available: bool,
    /// Which item the user has expanded to the detail sheet, if any.
    /// Keyed by [`StoreItem::id`]. Reserved for the follow-up
    /// slide-in-detail sheet; kept on the widget so the CRDT sync
    /// can restore the open state across window rebuilds.
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
    pending_refresh: bool,
    pending_search: Option<String>,
    pending_detail: Option<String>,
    remotes: Vec<crate::flathub_client::Remote>,
    remotes_open: bool,
    pending_remote_intents: Vec<RemoteIntent>,
    /// Segment index the source-filter `ToggleGroup` last committed.
    /// The `on_change` closure is `'static` and can't borrow `self`,
    /// so it parks the click here; `sync` reads it back and flips
    /// [`active_filter`]. Shared (`Rc`) because the closure is rebuilt
    /// into a fresh `ToggleGroup` every `build_layout`.
    filter_click: Rc<Cell<Option<usize>>>,
    /// Transient floating status chip. The catalog counts / action
    /// status surface here — revealed on a real change or while the
    /// pointer hovers the bottom-right band, then faded — in place of a
    /// permanent full-width footer strip, so the card list reclaims the
    /// height. See [`draw_status_bubble`](Self::draw_status_bubble).
    status_bubble: ui::StatusBubble,
    /// The label the bubble last reflected, so we only poke it (reveal
    /// + restart the linger) when the surfaced text actually changes.
    last_status_label: String,
    /// Last pointer position (window coords), tracked from `CursorMoved`
    /// so the bubble can reveal on hover of its zone.
    last_cursor: Point,
    /// `--shots` forces the bubble fully visible so a single headless
    /// frame captures the floating chip.
    shots_status: bool,
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
            pending_refresh: false,
            pending_search: None,
            pending_detail: None,
            remotes: Vec::new(),
            remotes_open: false,
            pending_remote_intents: Vec::new(),
            filter_click: Rc::new(Cell::new(None)),
            status_bubble: ui::StatusBubble::new(),
            last_status_label: String::new(),
            last_cursor: Point::ZERO,
            shots_status: false,
        }
    }

    /// Drain the actions clicked since the last frame. The host's
    /// worker executes each one (off the UI thread) and streams a
    /// fresh catalog + status back.
    pub fn take_pending_actions(&mut self) -> Vec<(String, ActionKind)> {
        std::mem::take(&mut self.pending_actions)
    }

    /// Consume the edge produced by the refresh affordance. The host sends
    /// it to the same serialized worker as mutations, so refreshes cannot
    /// race an install/remove or fan out into duplicate network work.
    pub fn take_refresh_requested(&mut self) -> bool {
        std::mem::take(&mut self.pending_refresh)
    }

    pub fn take_search_requested(&mut self) -> Option<String> {
        self.pending_search.take()
    }

    pub fn take_detail_requested(&mut self) -> Option<String> {
        self.pending_detail.take()
    }

    pub fn take_remote_intents(&mut self) -> Vec<RemoteIntent> {
        std::mem::take(&mut self.pending_remote_intents)
    }

    pub fn set_remotes(&mut self, remotes: Vec<crate::flathub_client::Remote>) {
        if self.remotes != remotes {
            self.remotes = remotes;
            self.structure_dirty = true;
        }
    }

    /// Merge an enriched `AppStream` record into the current list and keep the
    /// detail view focused on it.
    pub fn set_detail(&mut self, detail: StoreItem) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == detail.id) {
            *item = detail.clone();
        } else {
            self.items.push(detail.clone());
        }
        self.focused_detail = Some(detail.id);
        self.structure_dirty = true;
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
            self.pending_search = Some(self.query.clone());
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
            hits.sort_by_key(|i| {
                (
                    std::cmp::Reverse(i.install_rank()),
                    i.name.to_ascii_lowercase(),
                )
            });
        } else {
            let q = self.query.clone();
            hits.sort_by_key(|i| std::cmp::Reverse(i.fuzzy_score(&q)));
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
    fn compose(&self, theme: &Theme) -> Box<dyn Widget> {
        let header = Self::build_header();
        let filter_row = self.build_filter_row();
        let category_row = self.build_category_row();
        let list = if self.remotes_open {
            self.build_remotes(theme)
        } else {
            self.build_detail(theme)
                .unwrap_or_else(|| self.build_list(theme))
        };
        // No permanent footer strip: the catalog counts / action status
        // float as a transient bubble (see `draw_status_bubble`), so the
        // card list spans to the window bottom.
        let column = Column::new()
            .with_intrinsic_height()
            .push(header)
            .push(filter_row)
            .push(category_row)
            .push(list);
        Box::new(Container::new().filling().with_child(Box::new(column)))
    }

    fn build_detail(&self, theme: &Theme) -> Option<Box<dyn Widget>> {
        let id = self.focused_detail.as_ref()?;
        let item = self.items.iter().find(|item| &item.id == id)?;
        let source = match &item.source {
            crate::StoreSource::Qos { registry_name } => format!("qOS · {registry_name}"),
            crate::StoreSource::Flathub => "Flatpak · Flathub".to_string(),
        };
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                ui::widget::Button::new("store.detail.back", "Back to apps", Some(DETAIL_BACK_KEY))
                    .ghost(),
            ))
            .push(Box::new(Text::new(&item.name).with_size(22.0)))
            .push(Box::new(
                Text::new(&source)
                    .with_size(12.0)
                    .with_color(theme.text_dim),
            ))
            .push(Box::new(Text::new(&item.description).with_size(14.0)));
        if let Some(homepage) = item.homepage_url.as_ref() {
            column = column.push(Box::new(
                Text::new(homepage)
                    .with_size(12.0)
                    .with_color(theme.text_dim),
            ));
        }
        Some(Box::new(
            Container::new()
                .filling()
                .with_background()
                .with_child(Box::new(column)),
        ))
    }

    fn build_remotes(&self, theme: &Theme) -> Box<dyn Widget> {
        let mut column = Column::new()
            .with_intrinsic_height()
            .push(Box::new(
                ui::widget::Button::new("store.remotes.back", "Back to apps", Some(REMOTES_BACK_KEY))
                    .ghost(),
            ))
            .push(Box::new(Text::new("Flatpak remotes").with_size(22.0)))
            .push(Box::new(
                Text::new("Remote configuration applies to this machine and runs through the package worker.")
                    .with_size(12.0)
                    .with_color(theme.text_dim),
            ));
        for remote in &self.remotes {
            let label = format!(
                "{} · {}{}",
                remote.title,
                remote.url,
                if remote.disabled { " · disabled" } else { "" }
            );
            let key = format!("{}{}", REMOTES_REMOVE_KEY_PREFIX, remote.name);
            column = column.push(Box::new(
                Row::new()
                    .no_wrap()
                    .push(Box::new(Text::new(&label).with_size(13.0)))
                    .push(Box::new(Spacer::new()))
                    .push(Box::new(
                        ui::widget::Button::new(&key, "Remove", Some(&key)).ghost(),
                    )),
            ));
        }
        let form = Row::new()
            .no_wrap()
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("store.remote.name")
                    .with_on_change_path(REMOTE_NAME_KEY)
                    .with_placeholder("Remote name"),
            ))
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("store.remote.url")
                    .with_on_change_path(REMOTE_URL_KEY)
                    .with_placeholder("Repository URL"),
            ))
            .push(Box::new(
                ui::widget::Button::new("store.remote.add", "Add remote", Some(REMOTES_ADD_KEY))
                    .primary(),
            ))
            .push(Box::new(ui::widget::Button::new(
                "store.remote.refresh",
                "Refresh",
                Some(REMOTES_REFRESH_KEY),
            )));
        column = column.push(Box::new(form));
        Box::new(
            Container::new()
                .filling()
                .with_background()
                .with_child(Box::new(column)),
        )
    }

    fn build_header() -> Box<dyn Widget> {
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
        let refresh_btn = ui::widget::Button::new("store.refresh", "Refresh", Some(REFRESH_KEY));
        let remotes_btn =
            ui::widget::Button::new("store.remotes", "Remotes", Some(REMOTES_OPEN_KEY));
        let row = Row::new()
            .no_wrap()
            .push(Box::new(search))
            .push(Box::new(Spacer::new()))
            .push(Box::new(remotes_btn))
            .push(Box::new(refresh_btn));
        Box::new(Container::new().with_background().with_child(Box::new(row)))
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
        Box::new(Container::new().with_background().with_child(Box::new(row)))
    }

    fn build_list(&self, theme: &Theme) -> Box<dyn Widget> {
        // The card list is a plain column of cards. A `Scrollable`
        // wrapper is deliberately omitted because the retained
        // toolkit's scrollable + intrinsic-height card interaction
        // is unstable in headless render paths (see the settings
        // pane comment); we render a fixed head of items for the
        // initial cut and pick this up as a follow-up.
        const MAX_ROWS: usize = 30;

        let visible = self.visible_items();
        if visible.is_empty() {
            let text = if self.items.is_empty() {
                "Loading store…"
            } else {
                "No apps match this filter."
            };
            return Box::new(Container::new().with_background().with_child(Box::new(
                Text::new(text).with_size(13.0).with_color(theme.text_dim),
            )));
        }
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
                theme,
            ));
        }
        Box::new(Container::new().filling().with_child(Box::new(column)))
    }

    /// The label for the floating status bubble: a running action
    /// status while one is set, else the catalog counts (`N apps · M
    /// shown`) — the same genuinely-contextual info the old permanent
    /// footer strip carried. Never empty.
    fn footer_label(&self) -> String {
        if self.status_line.is_empty() {
            format!(
                "{} apps · {} shown",
                self.items.len(),
                self.visible_items().len()
            )
        } else {
            self.status_line.clone()
        }
    }

    /// `--shots`: force the status bubble fully visible so a single
    /// headless frame captures the floating chip.
    pub fn arm_status_bubble_for_shots(&mut self) {
        self.shots_status = true;
        self.status_bubble.poke(std::time::Instant::now());
    }

    /// Draw the transient floating status bubble at the window's
    /// bottom-right — the reclaimed home of the old footer strip. It
    /// surfaces on a real change (catalog refresh, filter, an action
    /// result) or while the pointer hovers the bottom-right band, then
    /// fades. Colours come from the theme.
    fn draw_status_bubble(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = std::time::Instant::now();
        let label = self.footer_label();
        if label != self.last_status_label {
            self.last_status_label.clone_from(&label);
            self.status_bubble.poke(now);
        }
        if label.is_empty() {
            return;
        }
        let text_size = 12.0f32;
        let (w, h) = ui::status_bubble::pill_size(&label, tr, text_size, 0.0);
        let margin = 14.0;
        let rect = Rect::new(
            self.rect.x1 - margin - w,
            self.rect.y1 - margin - h,
            self.rect.x1 - margin,
            self.rect.y1 - margin,
        );
        let zone = Rect::new(rect.x0 - 40.0, rect.y0 - 24.0, self.rect.x1, self.rect.y1);
        let over = self.shots_status || zone.contains(self.last_cursor);
        self.status_bubble.set_hover(over, now);
        let alpha = self.status_bubble.alpha(now);
        if alpha <= 0.0 {
            return;
        }
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            alpha,
            transform,
            &rect.inflate(12.0, 14.0),
        );
        ui::status_bubble::draw_pill(scene, transform, tr, theme, rect, &label, text_size);
        scene.pop_layer();
    }

    // ---- CRDT sync helpers ----

    /// Read the one-shot click keys the sub-widgets write and turn
    /// them into filter / category / action mutations. Returns
    /// `true` if anything changed (used by callers as a signal to
    /// re-render).
    fn drain_click_keys(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        // Source-filter chips are wired through the `filter_click`
        // cell (see `sync`), not a CRDT key, so there is no filter
        // loop here.
        let mut any = self.drain_category_clicks(state, path);
        any |= self.drain_primary_action_click(state, path);
        any |= self.drain_secondary_clicks(state, path);
        any |= self.drain_detail_back_click(state, path);
        any |= self.drain_remotes_clicks(state, path);
        any |= self.drain_refresh_click(state, path);
        any
    }

    /// Category chips.
    fn drain_category_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        for label in self.category_labels() {
            let key = format!("{CATEGORY_SELECT_KEY}.{label}");
            if take_bool_edge(state, path, &key) {
                self.active_category = if label == "All" {
                    None
                } else {
                    Some(label.clone())
                };
                self.structure_dirty = true;
                any = true;
            }
        }
        any
    }

    /// Primary action click (the big button on each card).
    fn drain_primary_action_click(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let Some((id, action)) = self.take_action_click(state, path) else {
            return false;
        };
        self.status_line = match action {
            ActionKind::Open => format!("Opening {id}…"),
            ActionKind::Update => format!("Updating {id}…"),
            ActionKind::Remove => format!("Removing {id}…"),
            ActionKind::MarkInstalled => format!("Marking {id} installed…"),
            ActionKind::Unmark => format!("Unmarking {id}…"),
            ActionKind::FlatpakInstall => format!("Installing Flatpak {id}…"),
            ActionKind::FlatpakUpdate => format!("Updating Flatpak {id}…"),
            // Named as unconfined in the status line, not only in
            // the launcher's log: this is the moment the user
            // chooses to start it.
            ActionKind::FlatpakOpen => format!("Opening {id} (unconfined host app)…"),
            ActionKind::FlatpakRemove => format!("Removing Flatpak {id}…"),
            ActionKind::NeedsFlatpakHost => {
                "Install flatpak on the host to use this action.".to_string()
            }
        };
        if action != ActionKind::NeedsFlatpakHost {
            self.pending_actions.push((id, action));
        }
        true
    }

    /// Secondary clicks: the pin ("mark installed") toggle, the Remove
    /// affordance on cached qOS cards, and the detail-select tap.
    /// Collect first so the item borrow ends before we mutate `self`.
    fn drain_secondary_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        let mut secondary: Vec<(String, ActionKind)> = Vec::new();
        for item in &self.items {
            let pin_key = format!("{}{}", PIN_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &pin_key)
                && let Some(action) = item.pin_action()
            {
                secondary.push((item.id.clone(), action));
            }
            let rm_key = format!("{}{}", REMOVE_CLICK_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &rm_key) {
                // The item decides *how* it is removed. A Flatpak
                // removed with `qos-install uninstall` would report
                // "no such package installed" and leave it there.
                if let Some(action) = item.remove_action() {
                    secondary.push((item.id.clone(), action));
                }
            }
            let detail_key = format!("{}{}", DETAIL_SELECT_KEY_PREFIX, item.id);
            if take_bool_edge(state, path, &detail_key) {
                self.focused_detail = Some(item.id.clone());
                self.pending_detail = Some(item.id.clone());
                self.status_line = format!("Loading details for {}…", item.name);
                self.structure_dirty = true;
                any = true;
            }
        }
        for (id, action) in secondary {
            self.status_line = format!("{} {}…", action.label(), id);
            self.pending_actions.push((id, action));
            any = true;
        }
        any
    }

    fn drain_detail_back_click(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        if take_bool_edge(state, path, DETAIL_BACK_KEY) {
            self.focused_detail = None;
            self.structure_dirty = true;
            true
        } else {
            false
        }
    }

    /// Remotes view: open/back navigation, a refresh tap, per-remote
    /// removal, and the add-remote form submit.
    fn drain_remotes_clicks(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        let mut any = false;
        if take_bool_edge(state, path, REMOTES_OPEN_KEY) {
            self.remotes_open = true;
            self.pending_remote_intents.push(RemoteIntent::Refresh);
            self.structure_dirty = true;
            any = true;
        }
        if take_bool_edge(state, path, REMOTES_BACK_KEY) {
            self.remotes_open = false;
            self.structure_dirty = true;
            any = true;
        }
        if take_bool_edge(state, path, REMOTES_REFRESH_KEY) {
            self.pending_remote_intents.push(RemoteIntent::Refresh);
            any = true;
        }
        let remote_names: Vec<String> = self
            .remotes
            .iter()
            .map(|remote| remote.name.clone())
            .collect();
        for name in remote_names {
            let key = format!("{REMOTES_REMOVE_KEY_PREFIX}{name}");
            if take_bool_edge(state, path, &key) {
                self.pending_remote_intents.push(RemoteIntent::Remove(name));
                any = true;
            }
        }
        if take_bool_edge(state, path, REMOTES_ADD_KEY) {
            let name = text_at(state, path, REMOTE_NAME_KEY);
            let url = text_at(state, path, REMOTE_URL_KEY);
            if !name.trim().is_empty() && !url.trim().is_empty() {
                self.pending_remote_intents
                    .push(RemoteIntent::Add { name, url });
            } else {
                self.status_line = "Remote name and repository URL are required.".into();
            }
            any = true;
        }
        any
    }

    fn drain_refresh_click(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
        if take_bool_edge(state, path, REFRESH_KEY) {
            self.status_line = "Refreshing…".to_string();
            self.pending_refresh = true;
            true
        } else {
            false
        }
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
///
/// # Panics
/// Never in practice: `root` is always a live Map (the widget's own CRDT
/// path), so `put_object` on it cannot fail short of a corrupted document.
#[allow(
    clippy::expect_used,
    reason = "root is always a live Map (the widget's own CRDT path), so put_object on it cannot fail short of a corrupted document — there is no meaningful ObjId to fall back to"
)]
fn ensure_text_obj(state: &mut AutoCommit, root: &ObjId, key: &str) -> ObjId {
    if let Ok(Some((Value::Object(ObjType::Text), id))) = state.get(root, key) {
        return id;
    }
    state
        .put_object(root, key, ObjType::Text)
        .expect("put_object Text never fails")
}

fn text_at(state: &AutoCommit, root: &ObjId, key: &str) -> String {
    state
        .get(root, key)
        .ok()
        .flatten()
        .and_then(|(value, id)| matches!(value, Value::Object(ObjType::Text)).then_some(id))
        .and_then(|id| state.text(&id).ok())
        .unwrap_or_default()
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
        if let Some(idx) = self.filter_click.take()
            && let Some(f) = SourceFilter::ALL.get(idx).copied()
        {
            self.set_active_filter(f);
        }
        // Seed the search box's backing `Text` object *before* the
        // children sync so the `TextInput` resolves a real object to
        // splice keystrokes into (it binds to an existing `Text` but
        // never creates one).
        let search_obj = ensure_text_obj(state, path, SEARCH_QUERY_KEY);
        let _ = ensure_text_obj(state, path, REMOTE_NAME_KEY);
        let _ = ensure_text_obj(state, path, REMOTE_URL_KEY);
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
        let mut root = self.compose(theme);
        let child = root.build_layout(taffy, theme, node_map);
        self.root = Some(root);
        let node = parent_node(
            taffy,
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            },
            &[child],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node))
            && let Some(&child_node) = children.first()
        {
            root.apply_layout(taffy, child_node, Point::new(self.rect.x0, self.rect.y0));
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
            root.draw_with_presence(
                scene,
                transform,
                text_renderer,
                theme,
                presence,
                permissions,
            );
        }
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Child overlays (any dropdown / popover) first, then the
        // floating status bubble on top of the content.
        if let Some(root) = self.root.as_mut() {
            root.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.draw_status_bubble(scene, transform, text_renderer, theme);
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
        // Track the pointer so the bubble can reveal on hover of its
        // bottom-right zone (coords are window-space, unscaled).
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        if let (Some(root), Ok(children)) = (self.root.as_mut(), taffy.children(node_id))
            && let Some(&child_node) = children.first()
        {
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

    fn name(&self) -> &'static str {
        "StoreApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self.root.as_mut().is_some_and(|r| r.take_structure_dirty());
        own || child
    }

    fn needs_animation_frame(&self) -> bool {
        self.root
            .as_ref()
            .is_some_and(|r| r.needs_animation_frame())
            || self.status_bubble.needs_frame(std::time::Instant::now())
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
            install_ref_path: None,
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
                StoreSource::Qos {
                    registry_name: "qOS".into(),
                },
                "Productivity",
                InstalledState::Not,
            ),
            item(
                "Terminal",
                StoreSource::Qos {
                    registry_name: "qOS".into(),
                },
                "System",
                InstalledState::Installed {
                    version: "1".into(),
                    available: "1".into(),
                    pinned: false,
                },
            ),
            item("Chess", StoreSource::Flathub, "Games", InstalledState::Not),
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
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert!(names.contains(&"Notes"));
        assert!(names.contains(&"Terminal"));
        assert!(!names.contains(&"Chess"));
    }

    #[test]
    fn filter_installed_shows_only_installed() {
        let mut app = seeded_app();
        app.set_active_filter(SourceFilter::Installed);
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert_eq!(names, vec!["Terminal"]);
    }

    #[test]
    fn category_filter_gates_the_list() {
        let mut app = seeded_app();
        app.set_active_category(Some("Games".into()));
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
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
        assert_eq!(app.take_search_requested().as_deref(), Some("Term"));
        assert!(app.take_search_requested().is_none());
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
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
        let names: Vec<&str> = app
            .visible_items()
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert!(names.contains(&"Notes"));
        assert!(!names.contains(&"Chess"), "qOS filter hides Flatpak items");
    }

    #[test]
    fn refresh_click_is_forwarded_once_to_the_backend_worker() {
        use automerge::transaction::Transactable;

        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, REFRESH_KEY, true).unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.take_refresh_requested());
        assert!(
            !app.take_refresh_requested(),
            "refresh is an edge, not a level"
        );
        assert!(matches!(
            doc.get(automerge::ROOT, REFRESH_KEY),
            Ok(Some((Value::Scalar(value), _)))
                if matches!(value.as_ref(), ScalarValue::Boolean(false))
        ));
    }

    #[test]
    fn detail_and_remote_controls_emit_serialized_worker_intents() {
        use automerge::transaction::Transactable;

        let mut app = seeded_app();
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "store.detail_select.id.chess", true)
            .unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.take_detail_requested().as_deref(), Some("id.chess"));
        assert!(app.take_detail_requested().is_none());

        doc.put(automerge::ROOT, REMOTES_OPEN_KEY, true).unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.take_remote_intents(), vec![RemoteIntent::Refresh]);
        assert!(app.take_remote_intents().is_empty());
    }
}
