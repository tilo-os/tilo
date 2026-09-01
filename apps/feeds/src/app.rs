//! Root widget for the Feeds reader — a three-pane RSS/Atom/Gemini client.
//!
//! Pane layout (left → right): a subscriptions sidebar (smart views +
//! folders + feeds with unread counts), an article list (title, source,
//! date, read/star state), and a reader rendering the selected article's
//! body. The widget is custom-drawn straight into the Vello scene rather
//! than composed from `ui` leaf widgets, so the list/reader can do native
//! block layout, scrolling, and hit-testing.
//!
//! State that *converges* (subscriptions, read + star flags, cached
//! articles) lives in the Automerge document via [`crate::doc`]. State that
//! is *local* to this peer (which article is selected, scroll offsets, the
//! active filter, the open modal) is kept in widget fields.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{FlapState, ModalDialog, SplitLayout, Widget};

use crate::actions::{self, FeedsAction, FeedsResourceCtx};
use crate::doc::FeedsDoc;
use crate::fetch::{FetchRequest, FetchResult, FetchWorker};
use crate::icons;
use crate::modal::{ModalBody, ModalIntent};
use crate::model::{
    Article, BlockKind, Feed, FeedKind, Filter, Id, ReadFont, ReadSettings, ReadTheme, Store,
    relative_time,
};
use crate::reader_theme::ReaderPalette;

fn opml_import_request() -> PickRequest {
    PickRequest::open("Import OPML", "Import")
        .thread_name("feeds-portal-import")
        .filter(
            FileFilter::new("OPML")
                .mimetype("text/x-opml")
                .mimetype("application/xml")
                .mimetype("text/xml")
                .glob("*.opml")
                .glob("*.xml"),
        )
        .all_files()
}
use crate::{opml, sample};

const SIDEBAR_W: f64 = 256.0;
const LIST_W: f64 = 400.0;
/// Max CRDT writes persisted per frame. A big refresh queues its writes and
/// they flush across several frames, so no single frame stalls persisting the
/// whole batch (which was a multi-second main-thread block). Kept small so even
/// an unoptimised (debug) build with a large document stays well under a 16 ms
/// frame budget; the backlog still drains within a second or two of wall-clock
/// while the UI stays fully interactive throughout.
const FLUSH_BUDGET: usize = 3;
const HEADER_H: f64 = 56.0;
const SIDE_ROW_H: f64 = 34.0;
const LIST_ROW_H: f64 = 88.0;
/// Height of the refresh-progress strip shown below the list header while a
/// "Refresh all" is in flight (and briefly after, as a summary).
const REFRESH_STRIP_H: f64 = 52.0;

/// At or above this width the three panes fit side by side (docked sidebar +
/// article list + reader). Sized so the sidebar (256) + a usable article list
/// (400) + a readable reader column (~344) all fit at once.
const WIDE_BREAKPOINT: f64 = 1000.0;
/// At or above this width (but below [`WIDE_BREAKPOINT`]) the sidebar stays
/// docked (fixed) but only one content pane — the article list OR the reader —
/// shares the remaining width, with a top-left Back returning reader→list.
/// Below it the sidebar itself collapses to an overlay drawer. Sized so the
/// docked sidebar (256) + a usable single content pane (~384) still fit.
const MIDDLE_BREAKPOINT: f64 = 640.0;

/// Which responsive tier the current width resolves to. Recomputed from the
/// allocated width every geometry pass (never laid out once), per HIG §1/§7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    /// Sidebar + list + reader, all docked side by side.
    Wide,
    /// Sidebar docked (fixed) + exactly one of {list, reader}; Back returns
    /// reader→list.
    Middle,
    /// A single view stack (list→reader push) with the sidebar as an overlay
    /// drawer.
    Narrow,
}

impl Tier {
    fn from_width(w: f64) -> Self {
        if w >= WIDE_BREAKPOINT {
            Tier::Wide
        } else if w >= MIDDLE_BREAKPOINT {
            Tier::Middle
        } else {
            Tier::Narrow
        }
    }
}

// ---------------------------------------------------------------------
// Local geometry / interaction model
// ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SmartKind {
    All,
    Unread,
    Starred,
}

#[derive(Clone, Debug)]
enum RowRole {
    Section,
    Smart(SmartKind),
    Folder { id: Id, collapsed: bool },
    Feed { id: Id, kind: FeedKind },
}

#[derive(Clone, Debug)]
struct SidebarRow {
    rect: Rect,
    indent: f64,
    label: String,
    badge: usize,
    selected: bool,
    role: RowRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolbarAction {
    AddSubscription,
    Refresh,
    MarkAllRead,
    ImportOpml,
    ExportOpml,
}

/// An entry in the per-feed right-click context menu. Aligned by index with
/// the [`ContextMenu`]'s action rows so a drained selection maps straight back
/// to the mutation to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FeedMenuAction {
    MarkAllRead,
    Rename,
    Unsubscribe,
}

#[derive(Clone, Debug)]
struct ToolbarButton {
    rect: Rect,
    action: ToolbarAction,
    /// Hover text for the button. Carried but never drawn — see the
    /// `tooltips are not drawn statically` note on the toolbar's draw pass.
    #[allow(dead_code)]
    tip: &'static str,
}

/// A control inside the reader's reading-settings popover.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SettingsAction {
    FontDec,
    FontInc,
    SetFont(ReadFont),
    SetTheme(ReadTheme),
}

#[derive(Clone, Debug)]
struct ListRow {
    rect: Rect,
    star_rect: Rect,
    id: Id,
}

/// A live dialog: the shared [`ModalDialog`] plus the intent that says which
/// real mutation its primary action runs. The dialog owns the backdrop, box,
/// title, footer buttons, Esc + backdrop dismissal, and the focus trap; the
/// [`ModalBody`] inside it hosts a real [`ui::widget::TextInput`] (or a
/// message). The `submit` / `cancel` cells are flipped by the dialog's action
/// callbacks and drained by the host each event.
struct FeedsModal {
    intent: ModalIntent,
    dialog: ModalDialog<ModalBody>,
    submit: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

impl FeedsModal {
    fn new(
        intent: ModalIntent,
        title: &str,
        body: ModalBody,
        primary_label: &str,
        max_w: f64,
        max_h: f64,
    ) -> Self {
        let submit = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(title, body)
            .open(true)
            .with_max_size(max_w, max_h)
            .primary_action(primary_label, {
                let s = submit.clone();
                move || s.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        Self {
            intent,
            dialog,
            submit,
            cancel,
        }
    }

    fn add_subscription() -> Self {
        let body = ModalBody::input(
            "Paste a feed or site URL — RSS, Atom, and gemini:// all work.",
            "Feed URL",
            "",
            "https://example.com/feed.xml",
        );
        Self::new(
            ModalIntent::AddSubscription,
            "Add subscription",
            body,
            "Add",
            520.0,
            248.0,
        )
    }

    fn rename(feed_id: Id, title: &str) -> Self {
        let body = ModalBody::input(
            "Give this feed a name that makes sense to you.",
            "Feed name",
            title,
            "Feed name",
        );
        Self::new(
            ModalIntent::RenameFeed { feed_id },
            "Rename feed",
            body,
            "Save",
            520.0,
            236.0,
        )
    }

    fn unsubscribe(feed_id: Id, title: &str) -> Self {
        let body = ModalBody::message(&format!(
            "Unsubscribe from \u{201c}{title}\u{201d}? Its articles will be removed from this device.",
        ));
        Self::new(
            ModalIntent::Unsubscribe { feed_id },
            "Unsubscribe",
            body,
            "Unsubscribe",
            460.0,
            210.0,
        )
    }
}

/// Live progress for an in-flight (or just-finished) "Refresh all". Shown as a
/// real progress strip in the list header — per-feed counts, a bar, and a
/// completion summary — rather than a one-line toast, because a refresh is a
/// meaningful, multi-second action.
#[derive(Clone, Debug, Default)]
struct RefreshState {
    /// Feeds queued in this refresh batch.
    total: usize,
    /// Feeds that have reported back (success or failure).
    done: usize,
    /// Feeds that returned at least one *new* article.
    updated_feeds: usize,
    /// Feeds that failed to fetch/parse.
    failed_feeds: usize,
    /// Total new articles across the batch.
    new_items: usize,
    /// True once `done == total`; drives the summary state + auto-expiry.
    finished_at: Option<Instant>,
}

impl RefreshState {
    fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }
    fn in_progress(&self) -> bool {
        self.total > 0 && self.finished_at.is_none()
    }
    /// Whether the strip should still be shown (in-progress, or a summary that
    /// hasn't aged out yet).
    fn visible(&self) -> bool {
        if self.in_progress() {
            return true;
        }
        match self.finished_at {
            Some(t) => t.elapsed().as_secs_f32() < 6.0,
            None => false,
        }
    }
}

/// A change the live (doc-less) `draw`-time drain produced that still needs to
/// be persisted into the Automerge document. Draining happens during `draw`
/// (which has no `&mut AutoCommit`), so the CRDT writes are queued here and
/// flushed on the next pass that *does* hold the doc (`sync` / `handle_event`).
/// This is what lets a refresh land live — updating the visible store on every
/// animation frame — without ever blocking the UI thread on the network or
/// waiting for an input event to drain.
#[derive(Clone, Debug)]
enum DocWrite {
    Article(Article),
    Feed(Feed),
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent render/interaction cache (popover open, staged \\
              --shots flags, animation settle state), not a state machine"
)]
pub struct FeedsApp {
    store: Store,
    filter: Filter,
    selected: Option<Id>,

    list_scroll: f64,
    reader_scroll: f64,
    reader_content_h: f64,
    list_content_h: f64,

    // Geometry, recomputed every `apply_layout`.
    rect: Rect,
    node: Option<NodeId>,
    sidebar_rect: Rect,
    list_rect: Rect,
    reader_rect: Rect,
    toolbar_rect: Rect,
    sidebar_rows: Vec<SidebarRow>,
    list_rows: Vec<ListRow>,
    toolbar_buttons: Vec<ToolbarButton>,
    reader_links: Vec<(Rect, String)>,
    reader_open_rect: Option<Rect>,

    last_mouse: Point,
    modal: Option<FeedsModal>,
    toast: Option<(String, Instant)>,

    // --- OPML import (XDG portal) ---------------------------------------
    /// In-flight import file-chooser: the portal worker's receiver, drained
    /// once per frame in `sync`. `None` when no picker is open.
    import_rx: Option<PortalChannel>,

    // --- per-feed context menu ------------------------------------------
    /// Right-click menu over a sidebar feed. Uses the shared `ui` primitive so
    /// the popover matches the rest of the OS; the app owns open/close + action
    /// routing (via `take_selection`) since actions mutate the CRDT store.
    feed_menu: ContextMenu,
    /// The feed the open context menu targets.
    feed_menu_target: Option<Id>,
    /// Menu actions aligned by index with `feed_menu`'s action rows.
    feed_menu_actions: Vec<FeedMenuAction>,
    /// `--shots` staging: open the context menu over the first feed row once
    /// geometry is known (the single-frame renderer can't reach it via input).
    stage_menu_shot: bool,

    // --- article reading settings (reader pane) -------------------------
    settings: ReadSettings,
    /// Whether the reader's reading-settings popover is open.
    show_reader_settings: bool,
    /// Gear button (toggles the popover) + popover hit rects, recomputed each
    /// draw. Empty when the reader has no article / the popover is closed.
    settings_btn_rect: Rect,
    settings_hits: Vec<(Rect, SettingsAction)>,

    // --- refresh progress + non-blocking drain --------------------------
    /// Live refresh progress shown in the list header.
    refresh: RefreshState,
    /// CRDT writes produced by the doc-less `draw`-time drain, flushed on the
    /// next pass that holds the document.
    pending_doc_writes: Vec<DocWrite>,

    // Responsive chrome. `sidebar` is the eased open/closed state of the
    // subscriptions flap (docked beside the content on a wide window; an
    // overlay over a scrim on a narrow one). `detail_pushed` drives the
    // narrow-only master→detail push: the article list is the master, the
    // reader is the detail pushed in on selection.
    sidebar: FlapState,
    detail_pushed: bool,
    /// Whether the flap's `reveal` has been settled to the resolved layout
    /// default once the real allocated width is first known. Without this a
    /// fresh narrow window would animate the sidebar out from the construction
    /// default (docked-open ⇒ reveal 1).
    settled_once: bool,
    // Hit rects for the responsive affordances, recomputed each layout. Empty
    // (`Rect::ZERO`) when the affordance isn't shown at the current width.
    hamburger_rect: Rect,
    scrim_rect: Rect,
    back_rect: Rect,
    last_frame: Instant,

    worker: Option<FetchWorker>,
    synced: bool,

    // --- action model ---------------------------------------------------
    /// Framework-owned action dispatch: every affordance routes its
    /// [`FeedsAction`] through here so it brackets exactly one undo boundary,
    /// encodes under [`FEEDS_ACTION_TYPE`](crate::actions::FEEDS_ACTION_TYPE),
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: ui::ActionHost,
    /// The document heads the mirrored [`Store`] was last reconciled against.
    /// When the document moves out-of-band (a Ctrl+K palette action mutates it
    /// directly, not through this widget's methods) and the fetch backlog is
    /// drained, `sync` reloads the store from the document so the change shows.
    store_heads: Vec<automerge::ChangeHash>,
    /// When true this is the hermetic `--shots`/demo build: it seeds the
    /// bundled sample subscriptions. The live app leaves this false and never
    /// fabricates feeds — it starts empty on the user's persisted document.
    demo: bool,
    now: i64,
}

impl Default for FeedsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedsApp {
    /// The live app: starts empty on the user's persisted subscriptions and
    /// never fabricates feeds.
    pub fn new() -> Self {
        Self::build(false)
    }

    /// Fully-populated build used only by the hermetic `--shots`/test path.
    /// This is the sole source of the bundled sample subscriptions — the live
    /// app never fabricates content.
    pub fn new_demo() -> Self {
        Self::build(true)
    }

    fn build(demo: bool) -> Self {
        let store = if demo { sample::store() } else { Store::new() };
        let mut app = Self {
            store,
            filter: Filter::All,
            selected: None,
            list_scroll: 0.0,
            reader_scroll: 0.0,
            reader_content_h: 0.0,
            list_content_h: 0.0,
            rect: Rect::ZERO,
            node: None,
            sidebar_rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            reader_rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            sidebar_rows: Vec::new(),
            list_rows: Vec::new(),
            toolbar_buttons: Vec::new(),
            reader_links: Vec::new(),
            reader_open_rect: None,
            last_mouse: Point::ZERO,
            modal: None,
            toast: None,
            import_rx: None,
            feed_menu: ContextMenu::new(),
            feed_menu_target: None,
            feed_menu_actions: Vec::new(),
            stage_menu_shot: false,
            settings: ReadSettings::default(),
            show_reader_settings: false,
            settings_btn_rect: Rect::ZERO,
            settings_hits: Vec::new(),
            refresh: RefreshState::default(),
            pending_doc_writes: Vec::new(),
            sidebar: FlapState::open_when_docked(),
            detail_pushed: false,
            settled_once: false,
            hamburger_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            back_rect: Rect::ZERO,
            last_frame: Instant::now(),
            worker: None,
            synced: false,
            actions: ui::ActionHost::new(),
            store_heads: Vec::new(),
            demo,
            now: chrono::Utc::now().timestamp(),
        };
        if demo {
            app.select_featured();
        }
        app
    }

    /// The responsive tier resolved from the current allocated width.
    fn tier(&self) -> Tier {
        Tier::from_width(self.rect.width())
    }

    /// Whether the content is a single pane showing one of {list, reader} at a
    /// time (master→detail push). True at the middle and narrow tiers.
    fn single_pane(&self) -> bool {
        matches!(self.tier(), Tier::Middle | Tier::Narrow)
    }

    /// How the subscriptions sidebar flap resolves: docked beside the content
    /// at the wide + middle tiers (fixed at middle), an overlay drawer at the
    /// narrow tier. The [`FlapState`] easing is driven off this.
    fn nav_layout(&self) -> SplitLayout {
        match self.tier() {
            Tier::Narrow => SplitLayout::Overlay,
            Tier::Wide | Tier::Middle => SplitLayout::Docked,
        }
    }

    /// Snap the subscriptions sidebar fully open/closed with no animation, so
    /// the single-frame `--shots` renderer captures a settled flap (the live
    /// app eases via [`FlapState::tick`]). Used to stage the narrow-window
    /// overlay screenshot.
    pub fn snap_sidebar_open(&mut self, open: bool) {
        self.sidebar.snap(open);
    }

    /// Stage the narrow master→detail flow as having pushed the reader in, so
    /// the single-frame `--shots` renderer can capture the detail pane with its
    /// Back affordance.
    pub fn force_detail_pushed(&mut self) {
        self.detail_pushed = true;
    }

    /// Open the reading-settings popover (for the `--shots` reader-settings
    /// scenario, which the single-frame renderer can't reach via live input).
    pub fn force_reader_settings_open(&mut self) {
        self.show_reader_settings = true;
    }

    /// Stage the per-feed context menu open (for the `--shots` context-menu
    /// scenario). The menu is anchored over the first feed row in `paint` once
    /// the sidebar geometry is known.
    pub fn force_feed_menu_open(&mut self) {
        self.stage_menu_shot = true;
    }

    /// Open the Add-subscription dialog with `url` pre-typed into the field, so
    /// a `--shots` capture shows the real modal + focused `TextInput` with a caret
    /// (the single-frame renderer can't reach it through live input).
    pub fn stage_add_subscription(&mut self, url: &str) {
        self.open_add_subscription();
        if let Some(m) = self.modal.as_mut() {
            m.dialog.content_mut().set_value(url);
        }
    }

    /// Merge the bundled sample OPML into the store so a `--shots` capture shows
    /// the sidebar right after an import — the freshly-created folder and its
    /// feeds folded in (the shot renders straight from the store).
    pub fn stage_post_import(&mut self) {
        let parsed = opml::parse(opml::sample_opml());
        opml::import_into(&mut self.store, &parsed);
    }

    /// Stage a refresh as mid-flight so the single-frame `--shots` renderer can
    /// capture the per-feed progress strip. Mirrors the live state a real
    /// "Refresh all" produces partway through: total queued, some done, a mix
    /// of updated and failed feeds, and a running new-item count.
    pub fn force_refresh_in_progress(&mut self) {
        self.refresh = RefreshState {
            total: 12,
            done: 4,
            updated_feeds: 3,
            failed_feeds: 1,
            new_items: 7,
            finished_at: None,
        };
    }

    /// Set the reading theme directly (for `--shots` of sepia/light pages).
    pub fn set_read_theme(&mut self, theme: ReadTheme) {
        self.settings.theme = theme;
    }
    /// Set the reading font directly (for `--shots` of the serif body).
    pub fn set_read_font(&mut self, font: ReadFont) {
        self.settings.font = font;
    }

    /// Give a couple of feeds deliberately long titles so a `--shots` capture
    /// exercises the sidebar's single-line elide + right-edge fade (proving a
    /// long entry no longer wraps into the row below).
    pub fn stage_long_sidebar_titles(&mut self) {
        let long = [
            "International Space Station Daily Operations & Mission Updates",
            "The Exhaustively Comprehensive Front-End Engineering Weekly Digest",
        ];
        for (f, title) in self.store.feeds.iter_mut().zip(long) {
            f.title = title.to_string();
        }
    }

    /// Choose an interesting article to open on launch (the richest unread
    /// one) so the offline screenshot lands on a fully-rendered body.
    fn select_featured(&mut self) {
        let best = self
            .store
            .articles_for(Filter::All)
            .into_iter()
            .max_by_key(|a| a.blocks.len() + if a.read { 0 } else { 100 })
            .map(|a| a.id);
        self.selected = best;
    }

    /// The first-run onboarding prompt to show while there are no
    /// subscriptions, or `None` once the store holds feeds. Returned as
    /// `(headline, hint)` so the draw path and tests gate on the same real
    /// state (`self.store.feeds`).
    pub fn empty_onboarding_prompt(&self) -> Option<(&'static str, &'static str)> {
        if self.store.feeds.is_empty() {
            Some((EMPTY_ONBOARDING_TITLE, EMPTY_ONBOARDING_HINT))
        } else {
            None
        }
    }

    fn ensure_selection(&mut self) {
        let in_list = self.selected.is_some_and(|id| {
            self.store
                .articles_for(self.filter)
                .iter()
                .any(|a| a.id == id)
        });
        if !in_list {
            self.selected = self.store.articles_for(self.filter).first().map(|a| a.id);
        }
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn worker(&mut self) -> &mut FetchWorker {
        self.worker.get_or_insert_with(FetchWorker::spawn)
    }

    /// Install a thread-less test worker and return the sender half of its
    /// result channel, so a test can inject `FetchResult`s exactly as the real
    /// worker thread would — proving the non-blocking drain path without any
    /// network I/O.
    #[cfg(test)]
    fn install_test_worker(&mut self) -> std::sync::mpsc::Sender<FetchResult> {
        let (w, tx) = FetchWorker::for_test();
        self.worker = Some(w);
        tx
    }

    /// Mirrored-store accessors used by the action tests to pick real ids /
    /// resolve the same batches the affordances do (so the covenant asserts on
    /// production state, never a fabricated fixture).
    #[cfg(test)]
    pub(crate) fn store_feed_ids(&self) -> Vec<Id> {
        self.store.feeds.iter().map(|f| f.id).collect()
    }
    #[cfg(test)]
    pub(crate) fn store_folder_ids(&self) -> Vec<Id> {
        self.store.folders.iter().map(|f| f.id).collect()
    }
    #[cfg(test)]
    pub(crate) fn store_article_ids(&self) -> Vec<Id> {
        self.store.articles.iter().map(|a| a.id).collect()
    }
    #[cfg(test)]
    pub(crate) fn store_unread_ids(&self, filter: Filter) -> Vec<Id> {
        self.store.unread_ids_for(filter)
    }

    // --- filter / selection helpers -------------------------------------

    fn filter_title(&self) -> String {
        match self.filter {
            Filter::All => "All Articles".to_string(),
            Filter::Unread => "Unread".to_string(),
            Filter::Starred => "Starred".to_string(),
            Filter::Folder(id) => self
                .store
                .folder(id)
                .map_or_else(|| "Folder".into(), |f| f.name.clone()),
            Filter::Feed(id) => self
                .store
                .feed(id)
                .map_or_else(|| "Feed".into(), |f| f.title.clone()),
        }
    }

    /// The article currently open, if any. Lets a test observe where a Ctrl+K
    /// activation navigated.
    pub fn selected_article(&self) -> Option<Id> {
        self.selected
    }

    /// The subset the list pane is currently showing. Lets a test observe that
    /// an activation focused the right source.
    pub fn active_filter(&self) -> Filter {
        self.filter
    }

    fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.list_scroll = 0.0;
        self.ensure_selection();
        self.reader_scroll = 0.0;
        // In a single-pane tier, changing the source returns to the list so the
        // new selection is visible (the reader is pushed back on the next select).
        if self.single_pane() {
            self.detail_pushed = false;
        }
    }

    fn select_article(&mut self, id: Id, doc: &mut AutoCommit) {
        self.selected = Some(id);
        self.reader_scroll = 0.0;
        if self.store.mark_read(id, true) {
            self.perform_action(doc, &actions::mark_read_action(id, true));
        }
        // Single-pane master→detail: selecting from the list pushes the reader in.
        if self.single_pane() {
            self.detail_pushed = true;
        }
    }

    /// Return from the pushed reader (detail) to the article list (master) in
    /// single-pane master→detail navigation.
    fn show_master(&mut self) {
        self.detail_pushed = false;
    }

    fn move_selection(&mut self, delta: i64, doc: &mut AutoCommit) {
        let ids: Vec<Id> = self
            .store
            .articles_for(self.filter)
            .iter()
            .map(|a| a.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        let cur = self.selected.and_then(|s| ids.iter().position(|&x| x == s));
        let next = match cur {
            Some(i) => {
                let n = i as i64 + delta;
                n.clamp(0, ids.len() as i64 - 1) as usize
            }
            None => 0,
        };
        let Some(&id) = ids.get(next) else {
            return;
        };
        self.select_article(id, doc);
        self.scroll_selection_into_view();
    }

    fn scroll_selection_into_view(&mut self) {
        if let Some(sel) = self.selected
            && let Some(pos) = self
                .store
                .articles_for(self.filter)
                .iter()
                .position(|a| a.id == sel)
        {
            let row_top = pos as f64 * LIST_ROW_H;
            let row_bot = row_top + LIST_ROW_H;
            let view_h = (self.list_rect.height() - HEADER_H).max(0.0);
            if row_top < self.list_scroll {
                self.list_scroll = row_top;
            } else if row_bot > self.list_scroll + view_h {
                self.list_scroll = row_bot - view_h;
            }
        }
    }

    fn toggle_star(&mut self, id: Id, doc: &mut AutoCommit) {
        if self.store.toggle_star(id) {
            let starred = self.store.article(id).is_some_and(|a| a.starred);
            self.perform_action(doc, &actions::set_starred_action(id, starred));
            self.set_toast(if starred { "Starred" } else { "Unstarred" });
        }
    }

    fn toggle_read(&mut self, doc: &mut AutoCommit) {
        if let Some(id) = self.selected {
            let now_read = self.store.article(id).is_none_or(|a| !a.read);
            if self.store.mark_read(id, now_read) {
                self.perform_action(doc, &actions::mark_read_action(id, now_read));
            }
        }
    }

    fn mark_all_read(&mut self, doc: &mut AutoCommit) {
        let ids = self.store.mark_all_read(self.filter);
        if !ids.is_empty() {
            self.perform_action(doc, &actions::mark_all_read_action(&ids));
        }
        self.set_toast(format!("Marked {} read", ids.len()));
    }

    // --- action model ---------------------------------------------------

    /// Dispatch one Feeds [`FeedsAction`] — the single mutation path shared with
    /// the Ctrl+K palette. The action is applied through the qOS action executor
    /// (a pinned actor + committed change, so it replays byte-identically on
    /// peers), exactly as the palette does. The document heads captured just
    /// before the first action of the event are stashed so the host records
    /// **one** undo boundary bracketing it ([`Widget::take_committed_action`]).
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &FeedsAction) {
        self.actions.dispatch_with_context(
            doc,
            actions::FEEDS_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Feeds", "Feeds", action),
        );
        // The action just mutated the document; keep the reload watermark ahead
        // of it so `sync`'s out-of-band reload doesn't redundantly rebuild the
        // store for a change this widget already mirrored.
        self.store_heads = doc.get_heads();
    }

    /// The presentation context Ctrl+K needs to build the reader/folder/feed
    /// actions, derived from the mirrored store (which converges with the doc).
    pub(crate) fn feeds_resource_ctx(&self) -> FeedsResourceCtx {
        let folders = self
            .store
            .folders_sorted()
            .iter()
            .map(|f| actions::FolderRef {
                id: f.id,
                name: f.name.clone(),
            })
            .collect();
        let mut feeds: Vec<actions::FeedRef> = self
            .store
            .feeds
            .iter()
            .map(|f| actions::FeedRef {
                id: f.id,
                title: f.title.clone(),
                unread: self.store.unread_ids_for(Filter::Feed(f.id)),
            })
            .collect();
        feeds.sort_by_key(|f| f.title.to_lowercase());
        let next_folder_order = self
            .store
            .folders
            .iter()
            .map(|f| f.order)
            .max()
            .map_or(0, |m| m + 1);
        FeedsResourceCtx {
            folders,
            feeds,
            next_folder_order,
        }
    }

    fn toggle_folder(&mut self, id: Id, doc: &mut AutoCommit) {
        if let Some(f) = self.store.folders.iter_mut().find(|f| f.id == id) {
            f.collapsed = !f.collapsed;
            let collapsed = f.collapsed;
            FeedsDoc::set_folder_collapsed(doc, id, collapsed);
        }
    }

    // --- reading settings -----------------------------------------------

    /// Mutate the reading settings and persist them to the CRDT (so the change
    /// survives restarts and converges with collaborators). All reader-settings
    /// changes funnel through here.
    fn bump_setting(&mut self, doc: &mut AutoCommit, f: impl FnOnce(&mut ReadSettings)) {
        let mut s = self.settings;
        f(&mut s);
        s = s.clamped();
        self.settings = s;
        FeedsDoc::write_settings(doc, &s);
    }

    fn apply_settings_action(&mut self, action: SettingsAction, doc: &mut AutoCommit) {
        match action {
            SettingsAction::FontDec => {
                self.bump_setting(doc, |s| s.font_scale -= 0.1);
            }
            SettingsAction::FontInc => {
                self.bump_setting(doc, |s| s.font_scale += 0.1);
            }
            SettingsAction::SetFont(f) => {
                self.bump_setting(doc, |s| s.font = f);
            }
            SettingsAction::SetTheme(th) => {
                self.bump_setting(doc, |s| s.theme = th);
            }
        }
    }

    /// The font family the reader body uses, derived from the setting.
    fn reader_family(&self) -> FontFamily {
        match self.settings.font {
            ReadFont::Sans => FontFamily::Sans,
            ReadFont::Serif => FontFamily::Serif,
            ReadFont::Mono => FontFamily::Mono,
        }
    }

    // --- fetch integration ----------------------------------------------

    /// Kick off a refresh of every subscription. This ONLY enqueues requests on
    /// the background worker's channel and returns immediately — it performs no
    /// network I/O and never blocks the UI thread. Results arrive asynchronously
    /// on the worker's result channel and are drained, non-blocking, every
    /// animation frame (see [`Self::drain_fetches_live`]).
    fn refresh_all(&mut self) {
        let reqs: Vec<FetchRequest> = self
            .store
            .feeds
            .iter()
            .map(|f| FetchRequest::Refresh {
                feed_id: f.id,
                url: f.url.clone(),
                kind: f.kind,
            })
            .collect();
        if reqs.is_empty() {
            return;
        }
        let n = reqs.len();
        let w = self.worker();
        for r in reqs {
            w.send(r);
        }
        // Start fresh progress for this batch (replacing any lingering summary).
        self.refresh = RefreshState {
            total: n,
            ..Default::default()
        };
        self.toast = None;
    }

    /// Add a subscription. The subscription *record* is a deterministic action
    /// (mint the id here, create the feed row with a placeholder title + derived
    /// site-url in the store and document); populating its real title + articles
    /// is an external fetch, not an action.
    fn subscribe(&mut self, url: &str, doc: &mut AutoCommit) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let folder = match self.filter {
            Filter::Folder(id) => Some(id),
            Filter::Feed(fid) => self.store.feed(fid).and_then(|f| f.folder),
            _ => None,
        };
        let id = Uuid::new_v4();
        let order = self.store.next_feed_order(folder);
        let feed = Feed {
            id,
            // Placeholder until the fetch fills the real title in.
            title: url.clone(),
            url: url.clone(),
            site_url: crate::fetch::derive_site_url(&url),
            folder,
            order,
            kind: actions::guess_kind(&url),
        };
        self.store.upsert_feed(feed);
        self.perform_action(
            doc,
            &actions::add_subscription_action(id, &url, folder, order),
        );
        // Populate the new feed's title + articles off-thread (external).
        self.refresh_feeds(&[id]);
        self.set_filter(Filter::Feed(id));
        self.set_toast(format!("Subscribing to {url}…"));
    }

    fn export_opml(&mut self) {
        let xml = opml::export(&self.store);
        let path = std::env::temp_dir().join("feeds-subscriptions.opml");
        match std::fs::write(&path, xml) {
            Ok(()) => self.set_toast(format!("Exported OPML → {}", path.display())),
            Err(e) => self.set_toast(format!("Export failed: {e}")),
        }
    }

    // --- OPML import (XDG portal) ----------------------------------------

    /// Ask the host desktop portal to pick an `.opml` file. Non-blocking — the
    /// D-Bus round-trip + host dialog run on a worker thread; the chosen path
    /// arrives over a channel drained by [`Self::drain_import`]. An in-flight
    /// request is not re-opened.
    fn request_import(&mut self) {
        if self.import_rx.is_some() {
            return;
        }
        self.import_rx = Some(opml_import_request().send());
        self.set_toast("Choose an OPML file…");
    }

    /// Drain an in-flight import portal request. Called once per frame from
    /// `sync` (which holds the document). When the user picked a file we read
    /// it, parse the OPML, subscribe to every feed, and kick a background fetch
    /// so the new feeds populate. Returns true if anything changed.
    fn drain_import(&mut self, doc: &mut AutoCommit) -> bool {
        let Some(rx) = &self.import_rx else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.import_rx = None;
                return false;
            }
        };
        self.import_rx = None;
        match outcome {
            PortalOutcome::Opened(path) => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let new_ids = self.apply_opml_import(&text, doc);
                    if new_ids.is_empty() {
                        self.set_toast("No new feeds in that OPML");
                    } else {
                        let plural = if new_ids.len() == 1 { "" } else { "s" };
                        self.set_toast(format!("Imported {} feed{plural}", new_ids.len()));
                        // Fetch the freshly-subscribed feeds so articles appear.
                        self.refresh_feeds(&new_ids);
                    }
                    true
                }
                Err(e) => {
                    self.set_toast(format!("Import failed: {e}"));
                    true
                }
            },
            PortalOutcome::Cancelled => false,
            PortalOutcome::Failed(reason) => {
                self.set_toast(format!("Import failed: {reason}"));
                true
            }
        }
    }

    /// Parse OPML `text`, merge it into the store (creating folders as needed,
    /// de-duplicating by URL), and persist the folders + feeds to the document.
    /// Hermetic — performs no network I/O. Returns the ids of the feeds that
    /// were newly subscribed (so the caller can fetch just those).
    fn apply_opml_import(&mut self, text: &str, doc: &mut AutoCommit) -> Vec<Id> {
        let parsed = opml::parse(text);
        let before: std::collections::HashSet<Id> = self.store.feeds.iter().map(|f| f.id).collect();
        opml::import_into(&mut self.store, &parsed);
        // Persist all folders + feeds. The upserts are change-guarded, so
        // re-writing entries that already existed is a no-op on the document.
        let folders = self.store.folders.clone();
        for f in &folders {
            FeedsDoc::upsert_folder(doc, f);
        }
        let feeds = self.store.feeds.clone();
        for f in &feeds {
            FeedsDoc::upsert_feed(doc, f);
        }
        doc.commit();
        self.store
            .feeds
            .iter()
            .filter(|f| !before.contains(&f.id))
            .map(|f| f.id)
            .collect()
    }

    /// Enqueue a background refresh for a specific set of feeds (used right
    /// after an OPML import). Non-blocking — only queues on the worker channel.
    fn refresh_feeds(&mut self, ids: &[Id]) {
        let reqs: Vec<FetchRequest> = ids
            .iter()
            .filter_map(|id| {
                self.store.feed(*id).map(|f| FetchRequest::Refresh {
                    feed_id: f.id,
                    url: f.url.clone(),
                    kind: f.kind,
                })
            })
            .collect();
        if reqs.is_empty() {
            return;
        }
        let n = reqs.len();
        let w = self.worker();
        for r in reqs {
            w.send(r);
        }
        self.refresh = RefreshState {
            total: n,
            ..Default::default()
        };
    }

    // --- per-feed context menu -------------------------------------------

    /// The feed whose sidebar row is under `p`, if any (folders/smart rows and
    /// section headers have no per-feed menu).
    fn feed_at(&self, p: Point) -> Option<Id> {
        self.sidebar_rows.iter().find_map(|row| {
            if row.rect.contains(p)
                && let RowRole::Feed { id, .. } = row.role
            {
                return Some(id);
            }
            None
        })
    }

    /// Open the per-feed context menu targeting `feed_id`, anchored near `at`
    /// (clamped so the popover stays within the widget).
    fn open_feed_menu(&mut self, feed_id: Id, at: Point) {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Mark all as read", || {}));
        menu.push(MenuItem::action("Rename…", || {}));
        menu.push(MenuItem::action("Unsubscribe", || {}));
        // Keep the menu inside the widget (rough size: 220 wide, ~96 tall).
        let x = at.x.min((self.rect.x1 - 224.0).max(self.rect.x0));
        let y = at.y.min((self.rect.y1 - 108.0).max(self.rect.y0));
        menu.open_at([x as f32, y as f32]);
        self.feed_menu = menu;
        self.feed_menu_target = Some(feed_id);
        self.feed_menu_actions = vec![
            FeedMenuAction::MarkAllRead,
            FeedMenuAction::Rename,
            FeedMenuAction::Unsubscribe,
        ];
    }

    fn close_feed_menu(&mut self) {
        self.feed_menu.close();
        self.feed_menu_target = None;
    }

    fn apply_feed_menu_action(
        &mut self,
        action: FeedMenuAction,
        feed_id: Id,
        doc: &mut AutoCommit,
    ) {
        match action {
            FeedMenuAction::MarkAllRead => {
                let ids = self.store.mark_all_read(Filter::Feed(feed_id));
                if !ids.is_empty() {
                    self.perform_action(doc, &actions::mark_all_read_action(&ids));
                }
                self.set_toast(format!("Marked {} read", ids.len()));
            }
            FeedMenuAction::Rename => {
                let title = self
                    .store
                    .feed(feed_id)
                    .map(|f| f.title.clone())
                    .unwrap_or_default();
                self.modal = Some(FeedsModal::rename(feed_id, &title));
            }
            // Unsubscribe is destructive, so confirm it through the shared modal
            // rather than removing the feed on the spot.
            FeedMenuAction::Unsubscribe => self.open_unsubscribe_confirm(feed_id),
        }
    }

    /// Open the "Add subscription" dialog (real modal + real URL field).
    fn open_add_subscription(&mut self) {
        self.modal = Some(FeedsModal::add_subscription());
    }

    /// Open the unsubscribe confirmation for `feed_id`.
    fn open_unsubscribe_confirm(&mut self, feed_id: Id) {
        let title = self
            .store
            .feed(feed_id)
            .map(|f| f.title.clone())
            .unwrap_or_default();
        self.modal = Some(FeedsModal::unsubscribe(feed_id, &title));
    }

    // --- modal lifecycle (drive the shared ModalDialog) ------------------

    /// Lay the open dialog out against the widget's own rect so its box,
    /// footer buttons, and embedded field geometry are current. Runs each
    /// frame from `apply_layout`. No-op when no modal is open.
    fn layout_modal(&mut self) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let Some(m) = self.modal.as_mut() else { return };
        let theme = Theme::dark();
        let mut text_renderer = TextRenderer::new();
        m.dialog.layout_in(self.rect, &theme, &mut text_renderer);
    }

    /// Route an event to the open dialog and act on the flags its action
    /// callbacks set. The dialog handles Esc, backdrop click, button clicks,
    /// typing, selection, and clipboard itself; Enter here is a submit
    /// convenience (the Add/Save button remains the real affordance).
    fn handle_modal_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state == ElementState::Pressed
            && matches!(ke.logical_key, Key::Named(NamedKey::Enter))
        {
            self.submit_modal(state);
            *handled = true;
            return;
        }
        let (submit, cancel) = {
            let Some(m) = self.modal.as_mut() else { return };
            *handled = m
                .dialog
                .handle_detached(event, scale_factor, modifiers, state, path);
            (m.submit.replace(false), m.cancel.replace(false))
        };
        if submit {
            self.submit_modal(state);
        } else if cancel {
            self.modal = None;
        }
        *handled = true;
    }

    /// Apply the open dialog's primary action against the real store +
    /// document, then close it. An input dialog with an empty field stays open
    /// (nothing to submit yet).
    fn submit_modal(&mut self, doc: &mut AutoCommit) {
        let (intent, value) = match self.modal.as_ref() {
            Some(m) => (m.intent, m.dialog.content().value().trim().to_string()),
            None => return,
        };
        if intent.requires_input() && value.is_empty() {
            return;
        }
        self.modal = None;
        match intent {
            ModalIntent::AddSubscription => self.subscribe(&value, doc),
            ModalIntent::RenameFeed { feed_id } => self.rename_feed(feed_id, &value, doc),
            ModalIntent::Unsubscribe { feed_id } => self.unsubscribe_feed(feed_id, doc),
        }
    }

    /// Delete a feed and its articles from the store **and** the document, and
    /// recover the selection / filter if they pointed at the removed feed.
    fn unsubscribe_feed(&mut self, feed_id: Id, doc: &mut AutoCommit) {
        let title = self
            .store
            .feed(feed_id)
            .map(|f| f.title.clone())
            .unwrap_or_default();
        let removed = self.store.remove_feed(feed_id);
        self.perform_action(doc, &actions::remove_subscription_action(feed_id));
        // If we were viewing the removed feed, fall back to All.
        if matches!(self.filter, Filter::Feed(id) if id == feed_id) {
            self.set_filter(Filter::All);
        }
        // Clear a selection that pointed at one of the removed articles.
        if self.selected.is_some_and(|s| removed.contains(&s)) {
            self.selected = None;
        }
        self.ensure_selection();
        self.set_toast(format!("Unsubscribed from {title}"));
    }

    /// Rename a feed in the store and persist the new title to the document.
    fn rename_feed(&mut self, feed_id: Id, name: &str, doc: &mut AutoCommit) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if self.store.feeds.iter().any(|f| f.id == feed_id) {
            if let Some(f) = self.store.feeds.iter_mut().find(|f| f.id == feed_id) {
                name.clone_into(&mut f.title);
            }
            self.perform_action(doc, &actions::rename_feed_action(feed_id, &name));
            self.set_toast("Renamed feed");
        }
    }

    /// Drain results the fetch worker produced, folding them into the in-memory
    /// store **and** updating live refresh progress — WITHOUT touching the
    /// Automerge document. This is safe to call from `draw` (which has no doc
    /// handle), so refreshed articles appear the instant they arrive even while
    /// the user is idle. CRDT persistence is deferred: each applied change is
    /// queued in `pending_doc_writes` and flushed by [`Self::flush_doc_writes`]
    /// on the next pass that holds the document. Returns true if anything
    /// changed (so the caller can request a redraw).
    fn drain_fetches_live(&mut self) -> bool {
        let results = match &mut self.worker {
            Some(w) => w.poll(),
            None => return false,
        };
        if results.is_empty() {
            return false;
        }
        for r in results {
            match r {
                FetchResult::Articles {
                    feed_id,
                    title,
                    articles,
                } => {
                    let mut feed_changed = false;
                    if let Some(t) = title
                        && !t.is_empty()
                        && let Some(f) = self.store.feeds.iter_mut().find(|f| f.id == feed_id)
                        && f.title != t
                    {
                        f.title = t;
                        feed_changed = true;
                    }
                    let mut added = 0;
                    for a in articles {
                        if self.store.insert_article_dedup(a.clone()) {
                            self.pending_doc_writes.push(DocWrite::Article(a));
                            added += 1;
                        }
                    }
                    if feed_changed && let Some(f) = self.store.feed(feed_id) {
                        self.pending_doc_writes.push(DocWrite::Feed(f.clone()));
                    }
                    // Progress accounting for this feed.
                    self.refresh.done += 1;
                    if added > 0 {
                        self.refresh.updated_feeds += 1;
                        self.refresh.new_items += added;
                    }
                    self.maybe_finish_refresh();
                }
                FetchResult::Subscribed { feed, articles } => {
                    let mut feed = feed;
                    feed.order = self.store.next_feed_order(feed.folder);
                    self.store.upsert_feed(feed.clone());
                    self.pending_doc_writes.push(DocWrite::Feed(feed.clone()));
                    for a in articles {
                        if self.store.insert_article_dedup(a.clone()) {
                            self.pending_doc_writes.push(DocWrite::Article(a));
                        }
                    }
                    self.set_toast(format!("Subscribed to {}", feed.title));
                    self.set_filter(Filter::Feed(feed.id));
                }
                FetchResult::Failed { feed_id, message } => {
                    if feed_id.is_some() {
                        // A refresh failure for a known feed counts toward the batch.
                        self.refresh.done += 1;
                        self.refresh.failed_feeds += 1;
                        self.maybe_finish_refresh();
                    } else {
                        self.set_toast(format!("Fetch failed: {message}"));
                    }
                }
            }
        }
        true
    }

    /// Mark the refresh finished once every queued feed has reported back.
    fn maybe_finish_refresh(&mut self) {
        if self.refresh.total > 0
            && self.refresh.done >= self.refresh.total
            && self.refresh.finished_at.is_none()
        {
            self.refresh.finished_at = Some(Instant::now());
        }
    }

    /// Persist changes the live drain queued, **bounded to a small budget per
    /// call** so a big refresh (hundreds of new articles at once) can never
    /// stall a frame. Automerge map writes + block JSON serialisation are not
    /// free; persisting an entire backlog in one pass was a multi-second
    /// main-thread block (the "OS asks to kill it" hang). Instead we drain up to
    /// [`FLUSH_BUDGET`] writes here and leave the rest queued; `needs_animation_frame`
    /// keeps frames flowing until the queue empties, so the backlog persists
    /// across a handful of frames while the UI stays responsive throughout.
    /// Idempotent: dedup already happened in the live drain, so these are
    /// straight upserts.
    fn flush_doc_writes(&mut self, doc: &mut AutoCommit) {
        if self.pending_doc_writes.is_empty() {
            return;
        }
        let n = self.pending_doc_writes.len().min(FLUSH_BUDGET);
        for w in self.pending_doc_writes.drain(..n) {
            match w {
                DocWrite::Article(a) => FeedsDoc::upsert_article(doc, &a),
                DocWrite::Feed(f) => FeedsDoc::upsert_feed(doc, &f),
            }
        }
        // Commit this batch so the pending-op buffer doesn't accumulate across
        // frames. An `AutoCommit` that never commits makes each successive `put`
        // re-scan the growing uncommitted transaction — O(n²) over a big
        // refresh, which is what turned a large batch into a multi-second stall.
        // Committing per flush keeps each write O(1)-ish.
        doc.commit();
    }

    // --- geometry --------------------------------------------------------

    fn recompute_geometry(&mut self) {
        let r = self.rect;
        let reveal = self.sidebar.reveal();
        // Sidebar can't be wider than the window (very narrow phones).
        let sw = SIDEBAR_W.min(r.width());

        match self.tier() {
            Tier::Wide => {
                // The subscriptions sidebar is docked beside the content, which
                // is inset by the eased width so it reveals/collapses smoothly.
                // The list + reader sit side by side in the content.
                let inset = sw * reveal;
                self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + inset, r.y1);
                let content_x0 = r.x0 + inset;
                self.toolbar_rect = Rect::new(content_x0, r.y0, r.x1, r.y0 + HEADER_H);
                self.list_rect = Rect::new(content_x0, r.y0, content_x0 + LIST_W, r.y1);
                self.reader_rect = Rect::new(content_x0 + LIST_W, r.y0, r.x1, r.y1);
                self.scrim_rect = Rect::ZERO;
                // The hamburger only appears while the docked rail is collapsing
                // / collapsed, so a hidden rail can always be summoned back.
                self.hamburger_rect = if reveal < 0.999 {
                    Self::toggle_button_rect(self.toolbar_rect)
                } else {
                    Rect::ZERO
                };
            }
            Tier::Middle => {
                // The sidebar stays docked and FIXED (always open, not
                // collapsible), sharing the width with exactly one content pane.
                // The list + reader are a master→detail push in that pane; a
                // top-left Back returns reader→list.
                self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + sw, r.y1);
                let content = Rect::new(r.x0 + sw, r.y0, r.x1, r.y1);
                self.place_single_content(content);
                self.scrim_rect = Rect::ZERO;
                // No hamburger — the sidebar is fixed at this tier.
                self.hamburger_rect = Rect::ZERO;
            }
            Tier::Narrow => {
                // The content spans the full width as a single master→detail
                // pane; the subscriptions sidebar floats in over a scrim.
                let content = r;
                self.place_single_content(content);
                // Sidebar slides in from the left edge by the eased reveal.
                let slide = self.sidebar.slide();
                let x0 = r.x0 - sw * (1.0 - slide);
                self.sidebar_rect = Rect::new(x0, r.y0, x0 + sw, r.y1);
                self.scrim_rect = if reveal > 0.001 { r } else { Rect::ZERO };
                // The top-left affordance is the Back chevron while a detail is
                // pushed (the sidebar is summoned from the master list); the
                // hamburger otherwise. They share the corner, so only one shows.
                self.hamburger_rect = if self.detail_pushed {
                    Rect::ZERO
                } else {
                    Self::toggle_button_rect(self.toolbar_rect)
                };
            }
        }

        self.build_sidebar_rows();
        self.build_list_rows();
        self.build_toolbar();
        self.build_back_affordance();
    }

    /// Lay out the single content pane used at the middle + narrow tiers: the
    /// toolbar spans the pane, and the list + reader are a master→detail push —
    /// one fills the pane while the other is parked off to the side (so a
    /// selection can slide the reader in and Back can slide it back out).
    fn place_single_content(&mut self, content: Rect) {
        self.toolbar_rect = Rect::new(content.x0, content.y0, content.x1, content.y0 + HEADER_H);
        if self.detail_pushed {
            // Detail (reader) fills the content; list is parked off to the left.
            self.list_rect = Rect::new(
                content.x0 - content.width(),
                content.y0,
                content.x0,
                content.y1,
            );
            self.reader_rect = content;
        } else {
            // Master (list) fills the content; reader is parked off to the right.
            self.list_rect = content;
            self.reader_rect = Rect::new(
                content.x1,
                content.y0,
                content.x1 + content.width(),
                content.y1,
            );
        }
    }

    /// Extra left margin for the sidebar's "Feeds" wordmark so it never sits
    /// under the expand/collapse toggle. The toggle is pinned to the content
    /// toolbar's top-left and is drawn over the panes; when the sidebar is open
    /// as an overlay it slides flush to the left edge, so the toggle lands over
    /// the sidebar header. In that case (and only then) push the wordmark right
    /// past the toggle's right edge. Returns 0 when the toggle can't reach the
    /// sidebar header (docked-open, or sidebar slid off-screen).
    /// The hamburger / close toggle rect, pinned to the top-left of the
    /// content's toolbar so it never collides with the right-aligned action
    /// buttons.
    fn toggle_button_rect(toolbar: Rect) -> Rect {
        let size = 36.0;
        let x = toolbar.x0 + 10.0;
        let cy = toolbar.y0 + HEADER_H / 2.0;
        Rect::new(x, cy - size / 2.0, x + size, cy + size / 2.0)
    }

    /// In a single-pane tier (middle + narrow), a Back chevron in the reader's
    /// header returns to the list. Empty otherwise.
    fn build_back_affordance(&mut self) {
        self.back_rect = if self.single_pane() && self.detail_pushed {
            let size = 36.0;
            let x = self.reader_rect.x0 + 10.0;
            let cy = self.reader_rect.y0 + HEADER_H / 2.0;
            Rect::new(x, cy - size / 2.0, x + size, cy + size / 2.0)
        } else {
            Rect::ZERO
        };
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one push per sidebar section (smart filters, folders, feeds); the length \
                  is the number of row kinds, not accidental complexity in any one section"
    )]
    fn build_sidebar_rows(&mut self) {
        let mut rows = Vec::new();
        let x0 = self.sidebar_rect.x0;
        let x1 = self.sidebar_rect.x1;
        let mut y = self.sidebar_rect.y0 + HEADER_H + 8.0;

        let push = |rows: &mut Vec<SidebarRow>,
                    y: &mut f64,
                    h: f64,
                    indent: f64,
                    label: String,
                    badge: usize,
                    selected: bool,
                    role: RowRole| {
            rows.push(SidebarRow {
                rect: Rect::new(x0, *y, x1, *y + h),
                indent,
                label,
                badge,
                selected,
                role,
            });
            *y += h;
        };

        push(
            &mut rows,
            &mut y,
            24.0,
            14.0,
            "LIBRARY".into(),
            0,
            false,
            RowRole::Section,
        );
        push(
            &mut rows,
            &mut y,
            SIDE_ROW_H,
            14.0,
            "All Articles".into(),
            self.store.total_unread(),
            matches!(self.filter, Filter::All),
            RowRole::Smart(SmartKind::All),
        );
        push(
            &mut rows,
            &mut y,
            SIDE_ROW_H,
            14.0,
            "Unread".into(),
            self.store.total_unread(),
            matches!(self.filter, Filter::Unread),
            RowRole::Smart(SmartKind::Unread),
        );
        push(
            &mut rows,
            &mut y,
            SIDE_ROW_H,
            14.0,
            "Starred".into(),
            self.store.total_starred(),
            matches!(self.filter, Filter::Starred),
            RowRole::Smart(SmartKind::Starred),
        );

        y += 10.0;
        push(
            &mut rows,
            &mut y,
            24.0,
            14.0,
            "FEEDS".into(),
            0,
            false,
            RowRole::Section,
        );

        let folders: Vec<(Id, String, bool)> = self
            .store
            .folders_sorted()
            .into_iter()
            .map(|f| (f.id, f.name.clone(), f.collapsed))
            .collect();
        for (fid, name, collapsed) in folders {
            let selected = matches!(self.filter, Filter::Folder(x) if x == fid);
            push(
                &mut rows,
                &mut y,
                SIDE_ROW_H,
                14.0,
                name,
                self.store.unread_for_folder(fid),
                selected,
                RowRole::Folder { id: fid, collapsed },
            );
            if !collapsed {
                let feeds: Vec<(Id, String, FeedKind)> = self
                    .store
                    .feeds_in_folder(Some(fid))
                    .into_iter()
                    .map(|f| (f.id, f.title.clone(), f.kind))
                    .collect();
                for (id, title, kind) in feeds {
                    let selected = matches!(self.filter, Filter::Feed(x) if x == id);
                    push(
                        &mut rows,
                        &mut y,
                        SIDE_ROW_H - 2.0,
                        34.0,
                        title,
                        self.store.unread_for_feed(id),
                        selected,
                        RowRole::Feed { id, kind },
                    );
                }
            }
        }

        // Uncategorised feeds.
        let loose: Vec<(Id, String, FeedKind)> = self
            .store
            .feeds_in_folder(None)
            .into_iter()
            .map(|f| (f.id, f.title.clone(), f.kind))
            .collect();
        if !loose.is_empty() {
            for (id, title, kind) in loose {
                let selected = matches!(self.filter, Filter::Feed(x) if x == id);
                push(
                    &mut rows,
                    &mut y,
                    SIDE_ROW_H - 2.0,
                    18.0,
                    title,
                    self.store.unread_for_feed(id),
                    selected,
                    RowRole::Feed { id, kind },
                );
            }
        }

        self.sidebar_rows = rows;
    }

    /// Height of the refresh-progress strip below the list header, or 0 when no
    /// refresh is in progress / recently finished.
    fn refresh_strip_h(&self) -> f64 {
        if self.refresh.visible() {
            REFRESH_STRIP_H
        } else {
            0.0
        }
    }

    fn build_list_rows(&mut self) {
        let mut rows = Vec::new();
        let content_top = self.list_rect.y0 + HEADER_H + self.refresh_strip_h();
        let x0 = self.list_rect.x0;
        let x1 = self.list_rect.x1;
        let articles: Vec<Id> = self
            .store
            .articles_for(self.filter)
            .iter()
            .map(|a| a.id)
            .collect();
        self.list_content_h = articles.len() as f64 * LIST_ROW_H;
        for (i, id) in articles.iter().enumerate() {
            let top = content_top + i as f64 * LIST_ROW_H - self.list_scroll;
            let rect = Rect::new(x0, top, x1, top + LIST_ROW_H);
            let star_rect = Rect::new(
                x1 - 40.0,
                top + LIST_ROW_H - 34.0,
                x1 - 8.0,
                top + LIST_ROW_H - 2.0,
            );
            rows.push(ListRow {
                rect,
                star_rect,
                id: *id,
            });
        }
        self.list_rows = rows;
    }

    fn build_toolbar(&mut self) {
        let mut buttons = Vec::new();
        let cy = self.toolbar_rect.y0 + HEADER_H / 2.0;
        let mut x = self.toolbar_rect.x1 - 28.0;
        let specs = [
            (ToolbarAction::AddSubscription, "Add subscription"),
            (ToolbarAction::Refresh, "Refresh all"),
            (ToolbarAction::MarkAllRead, "Mark all read"),
            (ToolbarAction::ImportOpml, "Import OPML"),
            (ToolbarAction::ExportOpml, "Export OPML"),
        ];
        for (action, tip) in specs {
            let rect = Rect::new(x - 18.0, cy - 18.0, x + 18.0, cy + 18.0);
            buttons.push(ToolbarButton { rect, action, tip });
            x -= 44.0;
        }
        self.toolbar_buttons = buttons;
    }

    // --- drawing ---------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over the app's fixed chrome regions; splitting it \
                  would only relocate the line count, not the amount a reader must hold in mind"
    )]
    fn paint(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        // Drain background-fetch results LIVE, every frame. `draw` has no
        // document handle, so this updates the in-memory store + progress and
        // queues the CRDT writes for the next doc-holding pass. This is the
        // crux of the non-blocking refresh: because `needs_animation_frame`
        // keeps frames flowing while a fetch is in flight, results land here
        // without the user having to move the mouse — and nothing on this path
        // ever calls the blocking network fetch.
        self.drain_fetches_live();

        // Advance the sidebar flap transition with real elapsed time, then
        // recompute the eased geometry so the slide is reflected this frame.
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f64().min(0.1);
        self.last_frame = now;
        self.sidebar.reconcile(self.nav_layout());
        self.sidebar.tick(self.nav_layout(), dt);
        self.recompute_geometry();

        // `--shots` staging: with the sidebar geometry now known, open the
        // per-feed context menu over the first feed row so the single-frame
        // renderer captures it.
        if self.stage_menu_shot
            && !self.feed_menu.is_open()
            && let Some((id, rect)) = self.sidebar_rows.iter().find_map(|r| match r.role {
                RowRole::Feed { id, .. } => Some((id, r.rect)),
                _ => None,
            })
        {
            self.open_feed_menu(id, Point::new(rect.x0 + 40.0, rect.y0 + 6.0));
        }

        let c = &theme.tokens.colors;
        let base = c.surfaces.base;
        let sidebar_bg = c.neutral[0];
        let tier = self.tier();
        // The subscriptions sidebar is an overlay (scrim) drawer only at the
        // narrow tier; docked (wide + middle) otherwise.
        let overlay = tier == Tier::Narrow;

        // Base background covers the whole widget so an off-pane child never
        // shows through during a push slide.
        scene.fill(Fill::NonZero, t, base, None, &self.rect);

        // --- content (list + reader) ------------------------------------
        // In narrow master→detail, only the title shifts right to make room
        // for the toggle / Back affordance; the list and reader still own the
        // full pane width so their content isn't cropped.
        let list_inset = if overlay && !self.detail_pushed {
            46.0
        } else {
            0.0
        };
        draw_list(
            scene,
            t,
            tr,
            theme,
            &self.list_rect,
            &self.list_rows,
            &self.store,
            self.selected,
            self.now,
            &self.filter_title(),
            self.store.count_for(self.filter),
            list_inset,
            &self.refresh,
        );
        // The reader shifts right to clear the top-left Back button whenever it
        // is the pushed single pane (middle + narrow).
        let reader_inset = if self.single_pane() && self.detail_pushed {
            46.0
        } else {
            0.0
        };
        let palette = ReaderPalette::resolve(self.settings.theme, theme);
        let has_article = self
            .selected
            .and_then(|id| self.store.article(id))
            .is_some();
        let (links, open_rect, content_h) = draw_reader(
            scene,
            t,
            tr,
            &self.reader_rect,
            self.selected.and_then(|id| self.store.article(id)),
            self.selected
                .and_then(|id| self.store.article(id))
                .and_then(|a| self.store.feed(a.feed_id)),
            self.reader_scroll,
            self.now,
            reader_inset,
            self.settings,
            palette,
            self.reader_family(),
        );
        self.reader_links = links;
        self.reader_open_rect = open_rect;
        self.reader_content_h = content_h;

        // The list/reader divider only exists in the wide three-pane layout;
        // the middle + narrow tiers show a single content pane at a time.
        let divider = alpha(c.border_standard, 150);
        if tier == Tier::Wide {
            vline(
                scene,
                t,
                self.list_rect.x1,
                self.rect.y0,
                self.rect.y1,
                divider,
            );
        }

        draw_toolbar(
            scene,
            t,
            tr,
            theme,
            &self.toolbar_rect,
            &self.toolbar_buttons,
        );

        // Reading-settings gear + popover (only when an article is showing).
        self.draw_reader_settings(scene, t, tr, theme, &palette, has_article);

        // Narrow Back affordance (return from the pushed reader to the list).
        if self.back_rect.area() > 0.0 {
            draw_chrome_button(
                scene,
                t,
                theme,
                &self.back_rect,
                ChromeGlyph::Back,
                self.last_mouse,
            );
        }

        // --- subscriptions sidebar (over a scrim when it's an overlay) ----
        // The scrim dims the content behind the floating flap and eats clicks
        // to dismiss it.
        if overlay && self.scrim_rect.area() > 0.0 {
            // Pure-black dimming scrim faded by the eased reveal (matches the
            // shared NavSplit flap), so the content clearly recedes behind the
            // floating sidebar.
            let scrim = Color::from_rgba8(0, 0, 0, 255)
                .with_alpha_factor((self.sidebar.reveal() * 0.5) as f32);
            scene.fill(Fill::NonZero, t, scrim, None, &self.scrim_rect);
        }
        if self.sidebar_rect.area() > 0.0 {
            scene.fill(Fill::NonZero, t, sidebar_bg, None, &self.sidebar_rect);
            vline(
                scene,
                t,
                self.sidebar_rect.x1,
                self.rect.y0,
                self.rect.y1,
                divider,
            );
            // The "Feeds" wordmark and the expand/collapse toggle share the
            // sidebar's top-left corner whenever the toggle can sit over the
            // open sidebar (overlay mode). Inset the wordmark past the toggle
            // footprint so the title and the toggle never overlap — keeping the
            // praised sidebar look otherwise untouched.
            draw_sidebar(scene, t, tr, theme, &self.sidebar_rect, &self.sidebar_rows);
        }

        // The hamburger / close toggle sits over the content's top-left, drawn
        // last so it stays clickable above the panes.
        if self.hamburger_rect.area() > 0.0 {
            let glyph = if self.sidebar.reveal() > 0.5 {
                ChromeGlyph::Close
            } else {
                ChromeGlyph::Menu
            };
            draw_chrome_button(
                scene,
                t,
                theme,
                &self.hamburger_rect,
                glyph,
                self.last_mouse,
            );
        }

        // Per-feed context menu (popover over the sidebar). Drawn through the
        // shared `ui` primitive so it matches the rest of the OS.
        if self.feed_menu.is_open() {
            let presence = PresenceCtx::empty();
            self.feed_menu.draw(scene, t, tr, theme, &presence);
        }

        // Toast (auto-expires after 4s).
        if let Some((msg, since)) = &self.toast
            && since.elapsed().as_secs_f32() < 4.0
        {
            paint_transient(
                scene,
                t,
                tr,
                theme,
                self.rect,
                ToastAnchor::BottomRight,
                &ToastCard::new(msg, ToastKind::Info),
            );
        }

        // Dialog on top of everything — the shared ModalDialog paints its own
        // backdrop, box, title, footer buttons, and the embedded field.
        if let Some(m) = self.modal.as_mut() {
            let presence = PresenceCtx::empty();
            m.dialog.draw(scene, t, tr, theme, &presence);
        }
    }

    /// Draw the reading-settings gear in the reader header and, when open, the
    /// settings popover (font size, font family incl. serif, reading theme).
    /// Recomputes `settings_btn_rect` + `settings_hits` for hit-testing. Chrome
    /// reads from the OS theme; only the reader page uses the reading palette.
    #[allow(
        clippy::too_many_lines,
        reason = "a single linear draw pass over the settings popover's fixed rows; splitting \
                  it would only relocate the line count, not the amount a reader must hold in mind"
    )]
    fn draw_reader_settings(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _palette: &ReaderPalette,
        has_article: bool,
    ) {
        self.settings_hits.clear();
        if !has_article {
            self.settings_btn_rect = Rect::ZERO;
            self.show_reader_settings = false;
            return;
        }
        let c = &theme.tokens.colors;

        // Gear button: top-right of the reader pane header, left of nothing
        // (the shared toolbar actions live over the list/content toolbar, so
        // the reader header's right edge is free).
        let size = 36.0;
        let bx1 = self.reader_rect.x1 - 12.0;
        let cy = self.reader_rect.y0 + HEADER_H / 2.0;
        let btn = Rect::new(bx1 - size, cy - size / 2.0, bx1, cy + size / 2.0);
        self.settings_btn_rect = btn;

        let shape = RoundedRect::from_rect(btn, 9.0);
        let hovered = btn.contains(self.last_mouse);
        let bg = if self.show_reader_settings {
            c.accent[5]
        } else if hovered {
            c.neutral[3]
        } else {
            c.surfaces.raised
        };
        scene.fill(Fill::NonZero, t, bg.with_alpha_factor(0.96), None, &shape);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            alpha(c.border_standard, 150),
            None,
            &shape,
        );
        let glyph_col = if self.show_reader_settings {
            Color::WHITE
        } else {
            c.text_primary
        };
        icons::gear(scene, t, btn.center(), 6.5, glyph_col);

        if !self.show_reader_settings {
            return;
        }

        // --- popover ----------------------------------------------------
        // Right-align under the gear, but clamp inside the reader pane so the
        // controls never spill off the edge on a narrow reader column.
        let ph = 196.0;
        let pw = 268.0_f64.min(self.reader_rect.width() - 16.0).max(180.0);
        let px1 = btn.x1.min(self.reader_rect.x1 - 8.0);
        let px0 = (px1 - pw).max(self.reader_rect.x0 + 8.0);
        let py0 = btn.y1 + 8.0;
        let card = Rect::new(px0, py0, px0 + pw, py0 + ph);
        scene.fill(
            Fill::NonZero,
            t,
            c.surfaces.raised,
            None,
            &RoundedRect::from_rect(card, 12.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            alpha(c.border_standard, 180),
            None,
            &RoundedRect::from_rect(card, 12.0),
        );

        let lx = card.x0 + 16.0;
        let rx = card.x1 - 16.0;
        let mut y = card.y0 + 16.0;

        tr.draw_text_weighted(
            scene,
            "Reading",
            13.0,
            c.text_primary,
            t * Affine::translate((lx, y)),
            None,
            TextWeight::Bold,
        );
        y += 26.0;

        // Font size row: −  Aa NNN%  +
        tr.draw_text(
            scene,
            "Text size",
            12.5,
            c.text_dim,
            t * Affine::translate((lx, y + 6.0)),
            None,
        );
        let bsz = 26.0;
        let plus = Rect::new(rx - bsz, y, rx, y + bsz);
        let pct = format!("{}%", (self.settings.font_scale * 100.0).round() as i32);
        let pctw = measure_w(tr, &pct, 12.5);
        let minus = Rect::new(
            plus.x0 - 8.0 - pctw - 8.0 - bsz,
            y,
            plus.x0 - 8.0 - pctw - 8.0,
            y + bsz,
        );
        self.step_button(scene, t, tr, theme, &minus, "−", SettingsAction::FontDec);
        tr.draw_text(
            scene,
            &pct,
            12.5,
            c.text_primary,
            t * Affine::translate((minus.x1 + 8.0, y + 5.0)),
            None,
        );
        self.step_button(scene, t, tr, theme, &plus, "+", SettingsAction::FontInc);
        y += bsz + 14.0;

        // Font family segmented control (Sans / Serif / Mono).
        tr.draw_text(
            scene,
            "Font",
            12.5,
            c.text_dim,
            t * Affine::translate((lx, y + 6.0)),
            None,
        );
        let seg_x0 = lx + 52.0;
        let seg_w = rx - seg_x0;
        let n = ReadFont::ALL.len();
        let cellw = seg_w / n as f64;
        for (i, f) in ReadFont::ALL.iter().enumerate() {
            let cell = Rect::new(
                seg_x0 + i as f64 * cellw,
                y,
                seg_x0 + (i + 1) as f64 * cellw,
                y + bsz,
            );
            let active = self.settings.font == *f;
            self.seg_cell(
                scene,
                t,
                tr,
                theme,
                &cell,
                f.label(),
                active,
                SettingsAction::SetFont(*f),
            );
        }
        y += bsz + 14.0;

        // Reading theme segmented control (Light / Sepia / Dark).
        tr.draw_text(
            scene,
            "Theme",
            12.5,
            c.text_dim,
            t * Affine::translate((lx, y + 6.0)),
            None,
        );
        let n = ReadTheme::ALL.len();
        let cellw = seg_w / n as f64;
        for (i, th) in ReadTheme::ALL.iter().enumerate() {
            let cell = Rect::new(
                seg_x0 + i as f64 * cellw,
                y,
                seg_x0 + (i + 1) as f64 * cellw,
                y + bsz,
            );
            let active = self.settings.theme == *th;
            self.seg_cell(
                scene,
                t,
                tr,
                theme,
                &cell,
                th.label(),
                active,
                SettingsAction::SetTheme(*th),
            );
        }
    }

    /// A small square stepper button ("−"/"+") for the settings popover.
    #[allow(clippy::too_many_arguments)]
    fn step_button(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: &Rect,
        glyph: &str,
        action: SettingsAction,
    ) {
        let c = &theme.tokens.colors;
        let hovered = rect.contains(self.last_mouse);
        let bg = if hovered { c.neutral[3] } else { c.neutral[1] };
        scene.fill(
            Fill::NonZero,
            t,
            bg,
            None,
            &RoundedRect::from_rect(*rect, 7.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            alpha(c.border_standard, 140),
            None,
            &RoundedRect::from_rect(*rect, 7.0),
        );
        let gw = measure_w(tr, glyph, 16.0);
        tr.draw_text_weighted(
            scene,
            glyph,
            16.0,
            c.text_primary,
            t * Affine::translate((rect.center().x - gw / 2.0, rect.y0 + 3.0)),
            None,
            TextWeight::Semibold,
        );
        self.settings_hits.push((*rect, action));
    }

    /// One cell of a segmented control in the settings popover.
    #[allow(clippy::too_many_arguments)]
    fn seg_cell(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: &Rect,
        label: &str,
        active: bool,
        action: SettingsAction,
    ) {
        let c = &theme.tokens.colors;
        let inner = Rect::new(rect.x0 + 2.0, rect.y0, rect.x1 - 2.0, rect.y1);
        let hovered = rect.contains(self.last_mouse);
        let bg = if active {
            c.accent[5]
        } else if hovered {
            c.neutral[3]
        } else {
            c.neutral[1]
        };
        scene.fill(
            Fill::NonZero,
            t,
            bg,
            None,
            &RoundedRect::from_rect(inner, 7.0),
        );
        if !active {
            scene.stroke(
                &Stroke::new(1.0),
                t,
                alpha(c.border_standard, 120),
                None,
                &RoundedRect::from_rect(inner, 7.0),
            );
        }
        let col = if active { Color::WHITE } else { c.text_primary };
        let lw = measure_w(tr, label, 12.0);
        tr.draw_text(
            scene,
            label,
            12.0,
            col,
            t * Affine::translate((inner.center().x - lw / 2.0, inner.y0 + 6.0)),
            None,
        );
        self.settings_hits.push((*rect, action));
    }

    // --- hit testing -----------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "one guard per interactive region in hit-test priority order (modal, context \
                  menu, toolbar, sidebar, list, reader); the length is the number of regions"
    )]
    fn click(&mut self, p: Point, doc: &mut AutoCommit) -> bool {
        // Modal eats all clicks (outside = cancel).
        if self.modal.is_some() {
            return true;
        }

        // Open feed context menu: a click on a row runs its action, a click
        // off the menu dismisses it. Either way the click is consumed so it
        // never falls through to the panes behind the popover.
        if self.feed_menu.is_open() {
            if let Some(idx) = self.feed_menu.item_at([p.x, p.y]) {
                self.feed_menu.click_item(idx);
                if let Some(sel) = self.feed_menu.take_selection() {
                    let action = self.feed_menu_actions.get(sel).copied();
                    let target = self.feed_menu_target;
                    if let (Some(action), Some(fid)) = (action, target) {
                        self.apply_feed_menu_action(action, fid, doc);
                    }
                }
                self.feed_menu_target = None;
            } else {
                self.close_feed_menu();
            }
            return true;
        }

        // --- responsive chrome (handled before the panes) ----------------
        // Back: return from the pushed reader to the list (narrow only).
        if self.back_rect.area() > 0.0 && self.back_rect.contains(p) {
            self.show_master();
            return true;
        }
        // Hamburger: summon / dismiss the subscriptions sidebar.
        if self.hamburger_rect.area() > 0.0 && self.hamburger_rect.contains(p) {
            self.sidebar.toggle(self.nav_layout());
            return true;
        }
        // Scrim: a click on the dimmed content (outside the floating sidebar)
        // dismisses an open overlay sidebar before reaching any pane.
        if self.nav_layout() == SplitLayout::Overlay
            && self.sidebar.is_open(SplitLayout::Overlay)
            && self.scrim_rect.area() > 0.0
            && self.scrim_rect.contains(p)
            && !self.sidebar_rect.contains(p)
        {
            self.sidebar.close();
            return true;
        }

        // Reading-settings gear toggles the popover.
        if self.settings_btn_rect.area() > 0.0 && self.settings_btn_rect.contains(p) {
            self.show_reader_settings = !self.show_reader_settings;
            return true;
        }
        // Open popover: apply a control, or dismiss on an outside click.
        if self.show_reader_settings {
            for (rect, action) in self.settings_hits.clone() {
                if rect.contains(p) {
                    self.apply_settings_action(action, doc);
                    return true;
                }
            }
            // Click outside the controls closes the popover (without falling
            // through to select an article behind it).
            self.show_reader_settings = false;
            return true;
        }

        // Toolbar.
        for b in self.toolbar_buttons.clone() {
            if b.rect.contains(p) {
                match b.action {
                    ToolbarAction::AddSubscription => self.open_add_subscription(),
                    ToolbarAction::Refresh => self.refresh_all(),
                    ToolbarAction::MarkAllRead => self.mark_all_read(doc),
                    ToolbarAction::ImportOpml => self.request_import(),
                    ToolbarAction::ExportOpml => self.export_opml(),
                }
                return true;
            }
        }
        // Sidebar rows.
        for row in self.sidebar_rows.clone() {
            if !row.rect.contains(p) {
                continue;
            }
            // Whether this row navigates (vs. just toggling a folder's
            // disclosure) — a navigation auto-dismisses the overlay sidebar.
            let mut navigated = true;
            match row.role {
                RowRole::Smart(SmartKind::All) => self.set_filter(Filter::All),
                RowRole::Smart(SmartKind::Unread) => self.set_filter(Filter::Unread),
                RowRole::Smart(SmartKind::Starred) => self.set_filter(Filter::Starred),
                RowRole::Folder { id, .. } => {
                    // Chevron hit (left 28px) toggles; else filter to folder.
                    if p.x < row.rect.x0 + 28.0 {
                        self.toggle_folder(id, doc);
                        navigated = false;
                    } else {
                        self.set_filter(Filter::Folder(id));
                    }
                }
                RowRole::Feed { id, .. } => self.set_filter(Filter::Feed(id)),
                RowRole::Section => navigated = false,
            }
            // On a narrow window, navigating from the overlay sidebar closes it
            // so the chosen list is revealed.
            if navigated && self.nav_layout() == SplitLayout::Overlay {
                self.sidebar.close();
            }
            return true;
        }
        // Article list — star toggles, body selects.
        for lr in self.list_rows.clone() {
            if lr.star_rect.contains(p) {
                self.toggle_star(lr.id, doc);
                return true;
            }
            if lr.rect.contains(p) && self.list_rect_content().contains(p) {
                self.select_article(lr.id, doc);
                return true;
            }
        }
        // Reader: "open link" affordance + inline links.
        if let Some(r) = self.reader_open_rect
            && r.contains(p)
        {
            self.open_selected_link();
            return true;
        }
        for (rect, href) in self.reader_links.clone() {
            if rect.contains(p) {
                self.open_url(&href);
                return true;
            }
        }
        false
    }

    fn list_rect_content(&self) -> Rect {
        Rect::new(
            self.list_rect.x0,
            self.list_rect.y0 + HEADER_H,
            self.list_rect.x1,
            self.list_rect.y1,
        )
    }

    fn open_selected_link(&mut self) {
        if let Some(a) = self.selected.and_then(|id| self.store.article(id)) {
            let url = a.url.clone();
            if !url.is_empty() {
                self.open_url(&url);
            }
        }
    }

    fn open_url(&mut self, url: &str) {
        // Best-effort hand-off to the desktop; never block or panic.
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
        self.set_toast(format!("Opening {url}"));
    }
}

// ---------------------------------------------------------------------
// Drawing free functions (explicit borrows keep the borrow checker happy)
// ---------------------------------------------------------------------

fn alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

fn vline(scene: &mut Scene, t: Affine, x: f64, y0: f64, y1: f64, color: Color) {
    scene.stroke(
        &Stroke::new(1.0),
        t,
        color,
        None,
        &Line::new(Point::new(x, y0), Point::new(x, y1)),
    );
}

fn hline(scene: &mut Scene, t: Affine, x0: f64, x1: f64, y: f64, color: Color) {
    scene.stroke(
        &Stroke::new(1.0),
        t,
        color,
        None,
        &Line::new(Point::new(x0, y), Point::new(x1, y)),
    );
}

/// Draw a pill badge with a number; returns its width.
#[allow(
    clippy::too_many_arguments,
    reason = "shares the standard (scene, t, tr, theme/palette) drawing-context params with \
              every other draw helper in this file; bundling them into a context struct is a \
              file-wide refactor out of scope here"
)]
fn badge(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    right_x: f64,
    cy: f64,
    n: usize,
    bg: Color,
    fg: Color,
) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let label = if n > 999 {
        "999+".to_string()
    } else {
        n.to_string()
    };
    let (tw, _) = tr.measure(&label, 11.0, None);
    let w = (f64::from(tw) + 14.0).max(20.0);
    let h = 18.0;
    let rect = Rect::new(right_x - w, cy - h / 2.0, right_x, cy + h / 2.0);
    scene.fill(
        Fill::NonZero,
        t,
        bg,
        None,
        &RoundedRect::from_rect(rect, 9.0),
    );
    tr.draw_text(
        scene,
        &label,
        11.0,
        fg,
        t * Affine::translate((rect.x0 + (w - f64::from(tw)) / 2.0, cy - 7.0)),
        None,
    );
    w
}

#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
fn draw_sidebar(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    rows: &[SidebarRow],
) {
    let c = &theme.tokens.colors;
    // The sidebar's own fill (see `paint`), used for the label fade-out band.
    let sidebar_bg = c.neutral[0];
    // No app-name wordmark: the OS titlebar shows "Feeds". The header band is
    // kept only to align the sidebar's first section with the content toolbar
    // (and to clear the overlay collapse toggle) — its divider stays.
    let header = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + HEADER_H);
    hline(
        scene,
        t,
        rect.x0,
        rect.x1,
        header.y1,
        alpha(c.border_standard, 120),
    );

    for row in rows {
        let cy = row.rect.y0 + row.rect.height() / 2.0;
        if let RowRole::Section = &row.role {
            tr.draw_text_weighted(
                scene,
                &row.label,
                10.5,
                c.text_dim,
                t * Affine::translate((row.rect.x0 + row.indent, cy - 6.0)),
                None,
                TextWeight::Semibold,
            );
            continue;
        }

        // Selection / hover background.
        if row.selected {
            let pill = Rect::new(
                row.rect.x0 + 6.0,
                row.rect.y0 + 2.0,
                row.rect.x1 - 6.0,
                row.rect.y1 - 2.0,
            );
            scene.fill(
                Fill::NonZero,
                t,
                alpha(c.accent[5], 46),
                None,
                &RoundedRect::from_rect(pill, 8.0),
            );
        }

        let label_color = if row.selected {
            c.text_primary
        } else {
            alpha(c.text_primary, 220)
        };
        let mut text_x = row.rect.x0 + row.indent;

        match &row.role {
            RowRole::Folder { collapsed, .. } => {
                icons::chevron(
                    scene,
                    t,
                    Point::new(row.rect.x0 + 16.0, cy),
                    4.0,
                    !collapsed,
                    c.text_dim,
                );
                text_x = row.rect.x0 + 30.0;
            }
            RowRole::Smart(kind) => {
                let center = Point::new(row.rect.x0 + 18.0, cy);
                match kind {
                    SmartKind::All => icons::ring(scene, t, center, 5.0, c.text_dim),
                    SmartKind::Unread => icons::dot(scene, t, center, 4.0, c.accent[5]),
                    SmartKind::Starred => {
                        icons::star(scene, t, center, 6.0, true, c.semantic.warning);
                    }
                }
                text_x = row.rect.x0 + 32.0;
            }
            RowRole::Feed { kind, .. } => {
                // Small kind tag dot.
                let dot_color = match kind {
                    FeedKind::Gemini => c.semantic.success,
                    FeedKind::Atom => c.accent[6],
                    _ => c.text_dim,
                };
                icons::dot(
                    scene,
                    t,
                    Point::new(row.rect.x0 + row.indent - 10.0, cy),
                    2.5,
                    dot_color,
                );
            }
            RowRole::Section => {}
        }

        // Badge (right-aligned), then label clipped to the remaining width.
        let badge_w = if row.badge > 0 {
            let (bg, fg) = if row.selected {
                (c.accent[5], Color::WHITE)
            } else {
                (alpha(c.neutral[3], 180), c.text_dim)
            };
            badge(scene, t, tr, row.rect.x1 - 12.0, cy, row.badge, bg, fg)
        } else {
            0.0
        };
        let max_w = (row.rect.x1 - 12.0 - badge_w - 6.0 - text_x).max(20.0) as f32;
        let weight = if row.badge > 0 {
            TextWeight::Semibold
        } else {
            TextWeight::Regular
        };
        let size = if matches!(row.role, RowRole::Feed { .. }) {
            13.0
        } else {
            14.0
        };
        // Single-line label with a soft right-edge fade. The label is measured
        // with the SAME weight it draws with (heavier weights advance wider, so
        // a Regular-measured elide could still overflow when drawn Semibold) and
        // drawn with `max_width: None` so cosmic-text never wraps it onto a
        // second line that would collide with the row below. When the full text
        // doesn't fit, a short gradient fades the last few px into the sidebar
        // background instead of hard-cutting — the requested fade-out.
        let full_w = tr.measure_weighted(&row.label, size, None, weight).0;
        let truncated = full_w > max_w;
        let baseline = cy - f64::from(size) * 0.62;
        if truncated {
            // Reserve room for the fade so a glyph never ends mid-fade.
            let fade_w = 22.0_f32.min(max_w * 0.4);
            let draw_w = (max_w - fade_w * 0.5).max(10.0);
            let label = elide_weighted(tr, &row.label, size, draw_w, weight);
            tr.draw_text_weighted(
                scene,
                &label,
                size,
                label_color,
                t * Affine::translate((text_x, baseline)),
                None,
                weight,
            );
            // Fade band: transparent → sidebar background, over the row's height.
            let fade_x1 = text_x + f64::from(max_w);
            let fade_x0 = fade_x1 - f64::from(fade_w);
            let grad = vello::peniko::Gradient::new_linear(
                Point::new(fade_x0, cy),
                Point::new(fade_x1, cy),
            )
            .with_stops([(0.0, alpha(sidebar_bg, 0)), (1.0, sidebar_bg)]);
            let band = Rect::new(fade_x0, row.rect.y0, fade_x1, row.rect.y1);
            scene.fill(Fill::NonZero, t, &grad, None, &band);
        } else {
            tr.draw_text_weighted(
                scene,
                &row.label,
                size,
                label_color,
                t * Affine::translate((text_x, baseline)),
                None,
                weight,
            );
        }
    }
}

/// First-run onboarding headline shown when there are no subscriptions yet.
const EMPTY_ONBOARDING_TITLE: &str = "No feeds yet";
/// Onboarding hint guiding the user to add their first subscription.
const EMPTY_ONBOARDING_HINT: &str = "Add one with +, or import an OPML file.";

/// Paint a centered headline + hint over an otherwise-blank content area.
/// Colours come from the theme so it matches the OS.
fn draw_centered_onboarding(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    area: Rect,
    title: &str,
    hint: &str,
) {
    let c = &theme.tokens.colors;
    let title_size = 20.0_f32;
    let hint_size = 14.0_f32;
    let gap = 10.0_f64;
    let title_w = measure_w(tr, title, title_size);
    let hint_w = measure_w(tr, hint, hint_size);
    let block_h = f64::from(title_size) + gap + f64::from(hint_size);
    let cx = area.center().x;
    let top = area.center().y - block_h / 2.0;
    tr.draw_text(
        scene,
        title,
        title_size,
        c.text_primary,
        t * Affine::translate((cx - title_w / 2.0, top)),
        None,
    );
    tr.draw_text(
        scene,
        hint,
        hint_size,
        c.text_dim,
        t * Affine::translate((cx - hint_w / 2.0, top + f64::from(title_size) + gap)),
        None,
    );
}

#[allow(clippy::too_many_arguments)]
#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
fn draw_list(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    rows: &[ListRow],
    store: &Store,
    selected: Option<Id>,
    now: i64,
    title: &str,
    count: usize,
    header_inset: f64,
    refresh: &RefreshState,
) {
    let c = &theme.tokens.colors;
    // Header. `header_inset` shifts the title right of the hamburger toggle
    // when the list is the narrow master pane.
    let title_x = rect.x0 + 18.0 + header_inset;
    tr.draw_text_weighted(
        scene,
        title,
        17.0,
        c.text_primary,
        t * Affine::translate((title_x, rect.y0 + 16.0)),
        None,
        TextWeight::Bold,
    );
    let count_label = format!("{count}");
    let title_w0 = measure_w(tr, title, 17.0);
    tr.draw_text(
        scene,
        &count_label,
        13.0,
        c.text_dim,
        t * Affine::translate((title_x + title_w0 + 10.0, rect.y0 + 19.0)),
        None,
    );
    let header_bottom = rect.y0 + HEADER_H;
    hline(
        scene,
        t,
        rect.x0,
        rect.x1,
        header_bottom,
        alpha(c.border_standard, 120),
    );

    // Refresh-progress strip (per-feed progress + a bar; a summary once done).
    let strip_h = if refresh.visible() {
        REFRESH_STRIP_H
    } else {
        0.0
    };
    if strip_h > 0.0 {
        draw_refresh_strip(scene, t, tr, theme, rect, header_bottom, refresh);
    }
    let content = Rect::new(rect.x0, header_bottom + strip_h, rect.x1, rect.y1);

    if rows.is_empty() {
        if store.feeds.is_empty() {
            // First-run: no subscriptions at all → onboarding prompt.
            draw_centered_onboarding(
                scene,
                t,
                tr,
                theme,
                content,
                EMPTY_ONBOARDING_TITLE,
                EMPTY_ONBOARDING_HINT,
            );
        } else {
            // Have feeds, but the current filter yields nothing.
            tr.draw_text(
                scene,
                "Nothing here — you're all caught up.",
                14.0,
                c.text_dim,
                t * Affine::translate((rect.x0 + 18.0, content.y0 + 24.0)),
                None,
            );
        }
        return;
    }

    // Clip the scrolling rows to the content area.
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        t,
        &content,
    );
    for lr in rows {
        if lr.rect.y1 < content.y0 || lr.rect.y0 > content.y1 {
            continue;
        }
        let Some(a) = store.article(lr.id) else {
            continue;
        };
        let is_sel = selected == Some(lr.id);
        if is_sel {
            scene.fill(Fill::NonZero, t, alpha(c.accent[5], 38), None, &lr.rect);
            let bar = Rect::new(lr.rect.x0, lr.rect.y0, lr.rect.x0 + 3.0, lr.rect.y1);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
        }
        hline(
            scene,
            t,
            lr.rect.x0 + 14.0,
            lr.rect.x1,
            lr.rect.y1,
            alpha(c.border_standard, 70),
        );

        // Unread / read marker.
        let mcy = lr.rect.y0 + 22.0;
        if a.read {
            icons::ring(
                scene,
                t,
                Point::new(lr.rect.x0 + 18.0, mcy),
                3.5,
                alpha(c.text_dim, 150),
            );
        } else {
            icons::dot(
                scene,
                t,
                Point::new(lr.rect.x0 + 18.0, mcy),
                4.0,
                c.accent[5],
            );
        }

        let text_x = lr.rect.x0 + 34.0;
        let right_pad = 16.0;
        // Time, top-right.
        let time = relative_time(a.published, now);
        let tw = measure_w(tr, &time, 12.0);
        tr.draw_text(
            scene,
            &time,
            12.0,
            c.text_dim,
            t * Affine::translate((lr.rect.x1 - right_pad - tw, lr.rect.y0 + 12.0)),
            None,
        );

        let title_w = (lr.rect.x1 - right_pad - tw - 12.0 - text_x).max(60.0) as f32;
        let title_color = if a.read {
            alpha(c.text_primary, 170)
        } else {
            c.text_primary
        };
        let title_weight = if a.read {
            TextWeight::Regular
        } else {
            TextWeight::Semibold
        };
        let title_text = two_line_elide(tr, &a.title, 15.0, title_w);
        tr.draw_text_weighted(
            scene,
            &title_text,
            15.0,
            title_color,
            t * Affine::translate((text_x, lr.rect.y0 + 12.0)),
            Some(title_w),
            title_weight,
        );

        // Source + summary line.
        let source = store.feed(a.feed_id).map_or("", |f| f.title.as_str());
        let meta = if a.summary.is_empty() {
            source.to_string()
        } else {
            format!("{source}  ·  {}", a.summary)
        };
        // Stop the meta line before the always-present star glyph (bottom-right)
        // so the truncated source·summary never collides with it.
        let meta_w = (lr.star_rect.x0 - 8.0 - text_x).max(60.0) as f32;
        let meta_text = elide(tr, &meta, 12.5, meta_w);
        tr.draw_text(
            scene,
            &meta_text,
            12.5,
            c.text_dim,
            t * Affine::translate((text_x, lr.rect.y1 - 26.0)),
            Some(meta_w),
        );

        // Star, bottom-right.
        icons::star(
            scene,
            t,
            Point::new(lr.star_rect.x1 - 14.0, lr.star_rect.y0 + 16.0),
            6.5,
            a.starred,
            if a.starred {
                c.semantic.warning
            } else {
                alpha(c.text_dim, 160)
            },
        );
    }
    scene.pop_layer();
}

/// Draw the refresh-progress strip: a status line ("Refreshing 3/12…" or a
/// completion summary), a determinate progress bar, and a tiny breakdown of
/// updated / failed feeds + new-item count. Replaces the previous one-line
/// toast for refreshes with real, legible progress.
fn draw_refresh_strip(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    top: f64,
    refresh: &RefreshState,
) {
    let c = &theme.tokens.colors;
    let band = Rect::new(rect.x0, top, rect.x1, top + REFRESH_STRIP_H);
    // Subtle raised band so the strip reads as chrome distinct from the rows.
    scene.fill(Fill::NonZero, t, alpha(c.accent[5], 18), None, &band);
    hline(
        scene,
        t,
        band.x0,
        band.x1,
        band.y1,
        alpha(c.border_standard, 120),
    );

    let pad = 18.0;
    let in_prog = refresh.in_progress();

    // Status line.
    let status = if in_prog {
        format!("Refreshing {}/{}…", refresh.done, refresh.total)
    } else {
        // Completion summary.
        let mut parts: Vec<String> = Vec::new();
        if refresh.updated_feeds > 0 {
            parts.push(format!(
                "{} feed{} updated",
                refresh.updated_feeds,
                if refresh.updated_feeds == 1 { "" } else { "s" }
            ));
        }
        if refresh.new_items > 0 {
            parts.push(format!(
                "{} new item{}",
                refresh.new_items,
                if refresh.new_items == 1 { "" } else { "s" }
            ));
        }
        if refresh.failed_feeds > 0 {
            parts.push(format!("{} failed", refresh.failed_feeds));
        }
        if parts.is_empty() {
            "Up to date — no new items".to_string()
        } else {
            format!("Refresh complete · {}", parts.join(" · "))
        }
    };
    tr.draw_text_weighted(
        scene,
        &status,
        13.5,
        c.text_primary,
        t * Affine::translate((band.x0 + pad, band.y0 + 8.0)),
        None,
        TextWeight::Semibold,
    );

    // Right-aligned live counts while in progress (updated / failed so far).
    if in_prog && (refresh.updated_feeds > 0 || refresh.failed_feeds > 0) {
        let mut chips: Vec<String> = Vec::new();
        if refresh.updated_feeds > 0 {
            chips.push(format!("✓ {}", refresh.updated_feeds));
        }
        if refresh.failed_feeds > 0 {
            chips.push(format!("✕ {}", refresh.failed_feeds));
        }
        let line = chips.join("   ");
        let lw = measure_w(tr, &line, 12.5);
        tr.draw_text(
            scene,
            &line,
            12.5,
            c.text_dim,
            t * Affine::translate((band.x1 - pad - lw, band.y0 + 9.0)),
            None,
        );
    }

    // Progress bar.
    let bar_y = band.y0 + 32.0;
    let bar_h = 6.0;
    let track = Rect::new(band.x0 + pad, bar_y, band.x1 - pad, bar_y + bar_h);
    scene.fill(
        Fill::NonZero,
        t,
        alpha(c.neutral[3], 200),
        None,
        &RoundedRect::from_rect(track, bar_h / 2.0),
    );
    let frac = if in_prog { refresh.fraction() } else { 1.0 };
    let fill_w = (track.width() * frac).max(if frac > 0.0 { bar_h } else { 0.0 });
    if fill_w > 0.0 {
        let fill_color = if refresh.failed_feeds > 0 && !in_prog {
            c.semantic.warning
        } else {
            c.accent[5]
        };
        let fill = Rect::new(track.x0, bar_y, track.x0 + fill_w, bar_y + bar_h);
        scene.fill(
            Fill::NonZero,
            t,
            fill_color,
            None,
            &RoundedRect::from_rect(fill, bar_h / 2.0),
        );
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
fn draw_reader(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    rect: &Rect,
    article: Option<&Article>,
    feed: Option<&Feed>,
    scroll: f64,
    now: i64,
    // Extra left margin so the body clears a narrow-mode Back affordance.
    header_inset: f64,
    settings: ReadSettings,
    palette: ReaderPalette,
    family: FontFamily,
) -> (Vec<(Rect, String)>, Option<Rect>, f64) {
    let mut links = Vec::new();
    let mut open_rect = None;

    // The reader PAGE uses the reading palette (light/sepia/dark paper), which
    // is deliberately independent of the OS theme. Chrome (toolbar/sidebar)
    // still reads from the OS theme.
    let page = Rect::new(rect.x0, rect.y0 + HEADER_H, rect.x1, rect.y1);
    scene.fill(Fill::NonZero, t, palette.page, None, &page);

    let Some(a) = article else {
        tr.draw_text(
            scene,
            "Select an article to read",
            16.0,
            palette.ink_dim,
            t * Affine::translate((rect.x0 + 48.0 + header_inset, rect.y0 + HEADER_H + 36.0)),
            None,
        );
        return (links, open_rect, 0.0);
    };

    let pad = (48.0_f64).max(header_inset);
    let content_x = rect.x0 + pad;
    let content_w = (rect.width() - pad * 2.0).max(120.0) as f32;
    // The shared toolbar bar occupies the top HEADER_H of this pane, so the
    // article body begins beneath it. Clip the scrolling content to the
    // area below the toolbar. Articles are infinite-scroll (no page modes).
    let top = rect.y0 + HEADER_H;
    let body = Rect::new(rect.x0, top, rect.x1, rect.y1);
    let content_top = top + 24.0;
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        t,
        &body,
    );
    let mut y = content_top - scroll;

    // Kicker: feed name (uppercase accent).
    if let Some(f) = feed {
        let kicker = f.title.to_uppercase();
        tr.draw_text_weighted(
            scene,
            &kicker,
            11.5,
            palette.accent,
            t * Affine::translate((content_x, y)),
            None,
            TextWeight::Semibold,
        );
        // Feed-kind tag on the right.
        let tag = f.kind.label();
        let tw = measure_w(tr, tag, 10.5);
        let tagx = rect.x1 - pad - tw - 14.0;
        let tag_rect = Rect::new(tagx, y - 2.0, tagx + tw + 14.0, y + 16.0);
        scene.fill(
            Fill::NonZero,
            t,
            palette.code_bg,
            None,
            &RoundedRect::from_rect(tag_rect, 6.0),
        );
        tr.draw_text(
            scene,
            tag,
            10.5,
            palette.ink_dim,
            t * Affine::translate((tagx + 7.0, y)),
            None,
        );
        y += 24.0;
    }

    // Title — sized relative to the reading font scale, in the chosen family.
    let title_size = settings.heading_size(1) + 6.0;
    let title_layout = RichTextLayout::shape(
        &mut tr.font_system,
        &[RichSpan::new(a.title.clone()).bold(true)],
        true,
        title_size,
        content_w,
        TextAlign::Left,
        family,
        vec![],
        vec![],
        1.0,
    );
    let th = title_layout.height();
    tr.draw_rich(
        scene,
        &title_layout,
        t * Affine::translate((content_x, y)),
        palette.ink,
    );
    y += f64::from(th) + 10.0;

    // Byline.
    let date = chrono_date(a.published, now);
    let mut byline = String::new();
    if !a.author.is_empty() {
        byline.push_str(&a.author);
    }
    if !date.is_empty() {
        if !byline.is_empty() {
            byline.push_str("  ·  ");
        }
        byline.push_str(&date);
    }
    if !byline.is_empty() {
        tr.draw_text(
            scene,
            &byline,
            13.0,
            palette.ink_dim,
            t * Affine::translate((content_x, y)),
            None,
        );
    }
    // "Open original" affordance on the right.
    if !a.url.is_empty() {
        let label = "Open original";
        let lw = measure_w(tr, label, 13.0);
        let ox = rect.x1 - pad - lw - 22.0;
        tr.draw_text(
            scene,
            label,
            13.0,
            palette.accent,
            t * Affine::translate((ox, y)),
            None,
        );
        icons::external_link(
            scene,
            t,
            Point::new(rect.x1 - pad - 7.0, y + 7.0),
            5.0,
            palette.accent,
        );
        open_rect = Some(Rect::new(ox - 4.0, y - 4.0, rect.x1 - pad + 4.0, y + 20.0));
    }
    y += 24.0;
    hline(scene, t, content_x, rect.x1 - pad, y, palette.rule);
    y += 22.0;

    // Body blocks — rendered with the reader's chosen family, size, and palette.
    for block in &a.blocks {
        y = draw_block(
            scene, t, tr, block, content_x, content_w, y, rect, &mut links, settings, &palette,
            family,
        );
    }
    y += 40.0;
    scene.pop_layer();

    let content_h = (y + scroll) - content_top;
    (links, open_rect, content_h)
}

/// Render one body block through `RichTextLayout` so the reader's chosen font
/// family (incl. a genuine serif) is honoured. Body ink/rules come from the
/// reading palette; sizes scale with the reading font setting.
#[allow(clippy::too_many_arguments)]
#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
#[allow(
    clippy::many_single_char_names,
    reason = "conventional short names for the shared drawing context (t: transform, w/h: \
              size, x/y: position) used consistently across every draw_* function in this file"
)]
fn draw_block(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    block: &crate::model::ContentBlock,
    x: f64,
    w: f32,
    y: f64,
    pane: &Rect,
    links: &mut Vec<(Rect, String)>,
    settings: ReadSettings,
    palette: &ReaderPalette,
    family: FontFamily,
) -> f64 {
    let body_size = settings.body_size();

    // Lay out `text` in `fam` at `size` within width `width`, returning height.
    let lay =
        |tr: &mut TextRenderer, text: &str, size: f32, width: f32, fam: FontFamily, bold: bool| {
            RichTextLayout::shape(
                &mut tr.font_system,
                &[RichSpan::new(text.to_string()).bold(bold)],
                bold,
                size,
                width,
                TextAlign::Left,
                fam,
                vec![],
                vec![],
                1.0,
            )
        };

    match block.kind {
        BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3 => {
            let level = match block.kind {
                BlockKind::Heading1 => 1,
                BlockKind::Heading2 => 2,
                _ => 3,
            };
            let size = settings.heading_size(level);
            let layout = lay(tr, &block.text, size, w, family, true);
            let h = layout.height();
            tr.draw_rich(
                scene,
                &layout,
                t * Affine::translate((x, y + 6.0)),
                palette.ink,
            );
            y + f64::from(h) + 12.0
        }
        BlockKind::Paragraph => {
            let layout = lay(tr, &block.text, body_size, w, family, false);
            let h = layout.height();
            tr.draw_rich(scene, &layout, t * Affine::translate((x, y)), palette.ink);
            y + f64::from(h) + 14.0
        }
        BlockKind::Bullet => {
            icons::dot(scene, t, Point::new(x + 6.0, y + 8.0), 2.5, palette.accent);
            let tw = w - 22.0;
            let layout = lay(tr, &block.text, body_size, tw, family, false);
            let h = layout.height();
            tr.draw_rich(
                scene,
                &layout,
                t * Affine::translate((x + 22.0, y)),
                palette.ink,
            );
            y + f64::from(h) + 8.0
        }
        BlockKind::Quote => {
            let tw = w - 24.0;
            // Quotes are italic — render via the styled (italic) span.
            let layout = RichTextLayout::shape(
                &mut tr.font_system,
                &[RichSpan::new(block.text.clone()).italic(true)],
                false,
                body_size,
                tw,
                TextAlign::Left,
                family,
                vec![],
                vec![],
                1.0,
            );
            let h = layout.height();
            let bar = Rect::new(x, y, x + 3.0, y + f64::from(h) + 4.0);
            scene.fill(Fill::NonZero, t, palette.accent, None, &bar);
            tr.draw_rich(
                scene,
                &layout,
                t * Affine::translate((x + 18.0, y)),
                palette.ink_dim,
            );
            y + f64::from(h) + 16.0
        }
        BlockKind::Code => {
            let inner = w - 28.0;
            // Code is always monospace regardless of the reading face.
            let layout = lay(
                tr,
                &block.text,
                body_size - 2.0,
                inner,
                FontFamily::Mono,
                false,
            );
            let h = layout.height();
            let box_rect = Rect::new(x, y, x + f64::from(w), y + f64::from(h) + 20.0);
            scene.fill(
                Fill::NonZero,
                t,
                palette.code_bg,
                None,
                &RoundedRect::from_rect(box_rect, 8.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                t,
                palette.rule,
                None,
                &RoundedRect::from_rect(box_rect, 8.0),
            );
            tr.draw_rich(
                scene,
                &layout,
                t * Affine::translate((x + 14.0, y + 10.0)),
                palette.ink,
            );
            y + f64::from(h) + 30.0
        }
        BlockKind::Link => {
            let layout = lay(tr, &block.text, body_size, w, family, false);
            let h = layout.height();
            tr.draw_rich(
                scene,
                &layout,
                t * Affine::translate((x, y)),
                palette.accent,
            );
            // Underline the link.
            let uw = layout
                .selection_rects(0, block.text.len())
                .iter()
                .map(|r| r.2)
                .fold(0.0f32, f32::max)
                .min(w);
            let uw = if uw > 0.0 {
                f64::from(uw)
            } else {
                f64::from(w)
            };
            hline(scene, t, x, x + uw, y + f64::from(h) - 1.0, palette.accent);
            if let Some(href) = &block.href {
                let rect = Rect::new(x, y, x + uw, y + f64::from(h));
                if rect.y1 > pane.y0 && rect.y0 < pane.y1 {
                    links.push((rect, href.clone()));
                }
            }
            y + f64::from(h) + 10.0
        }
        BlockKind::Divider => {
            hline(scene, t, x, x + f64::from(w), y + 8.0, palette.rule);
            y + 24.0
        }
    }
}

fn draw_toolbar(
    scene: &mut Scene,
    t: Affine,
    tr: &TextRenderer,
    theme: &Theme,
    rect: &Rect,
    buttons: &[ToolbarButton],
) {
    let c = &theme.tokens.colors;
    hline(
        scene,
        t,
        rect.x0,
        rect.x1,
        rect.y1,
        alpha(c.border_standard, 120),
    );
    for b in buttons {
        let center = b.rect.center();
        let bg = alpha(c.neutral[2], 160);
        scene.fill(
            Fill::NonZero,
            t,
            bg,
            None,
            &RoundedRect::from_rect(b.rect, 9.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            alpha(c.border_standard, 120),
            None,
            &RoundedRect::from_rect(b.rect, 9.0),
        );
        let col = c.text_primary;
        match b.action {
            ToolbarAction::AddSubscription => icons::plus(scene, t, center, 7.0, c.accent[6]),
            ToolbarAction::Refresh => icons::refresh(scene, t, center, 8.0, col),
            ToolbarAction::MarkAllRead => icons::check(scene, t, center, 7.0, col),
            ToolbarAction::ImportOpml => icons::import_arrow(scene, t, center, 6.0, col),
            ToolbarAction::ExportOpml => icons::external_link(scene, t, center, 6.0, col),
        }
        let _ = tr; // tooltips are not drawn statically
    }
}

/// Which glyph a responsive chrome button shows.
#[derive(Clone, Copy)]
enum ChromeGlyph {
    /// Hamburger — summon the collapsed sidebar.
    Menu,
    /// Close (X) — dismiss the open overlay sidebar.
    Close,
    /// Back chevron — return from the pushed detail to the list.
    Back,
}

/// A small raised, rounded button hosting a single chrome glyph (hamburger /
/// close / back). Used for the responsive sidebar toggle and the narrow
/// master→detail Back affordance.
fn draw_chrome_button(
    scene: &mut Scene,
    t: Affine,
    theme: &Theme,
    rect: &Rect,
    glyph: ChromeGlyph,
    mouse: Point,
) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(*rect, 9.0);
    let hovered = rect.contains(mouse);
    let bg = if hovered {
        c.neutral[3]
    } else {
        c.surfaces.raised
    };
    scene.fill(Fill::NonZero, t, bg.with_alpha_factor(0.94), None, &shape);
    scene.stroke(
        &Stroke::new(1.0),
        t,
        alpha(c.border_standard, 150),
        None,
        &shape,
    );
    let center = rect.center();
    match glyph {
        ChromeGlyph::Menu => icons::hamburger(scene, t, center, 8.0, c.text_primary),
        ChromeGlyph::Close => {
            let r = 6.0;
            let s = Stroke::new(1.8);
            scene.stroke(
                &s,
                t,
                c.text_primary,
                None,
                &Line::new(
                    Point::new(center.x - r, center.y - r),
                    Point::new(center.x + r, center.y + r),
                ),
            );
            scene.stroke(
                &s,
                t,
                c.text_primary,
                None,
                &Line::new(
                    Point::new(center.x + r, center.y - r),
                    Point::new(center.x - r, center.y + r),
                ),
            );
        }
        ChromeGlyph::Back => icons::back_arrow(scene, t, center, 6.0, c.text_primary),
    }
}

// --- text helpers -----------------------------------------------------

fn measure_w(tr: &mut TextRenderer, s: &str, size: f32) -> f64 {
    f64::from(tr.measure(s, size, None).0)
}

/// Truncate `s` with an ellipsis so it fits in `max_w` on a single line.
fn elide(tr: &mut TextRenderer, s: &str, size: f32, max_w: f32) -> String {
    if tr.measure(s, size, None).0 <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        probe.push('…');
        if tr.measure(&probe, size, None).0 > max_w {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

/// Weight-aware single-line truncation. Measures with the SAME `weight` the
/// text will be drawn with (heavier weights advance wider, so a Regular-measured
/// elide can still overflow when drawn Semibold — the sidebar-wrap bug). No
/// trailing ellipsis: the caller pairs this with a right-edge gradient fade, so
/// the cut reads as a fade rather than a hard "…".
fn elide_weighted(
    tr: &mut TextRenderer,
    s: &str,
    size: f32,
    max_w: f32,
    weight: TextWeight,
) -> String {
    if tr.measure_weighted(s, size, None, weight).0 <= max_w {
        return s.to_string();
    }
    let mut out = String::new();
    for ch in s.chars() {
        let mut probe = out.clone();
        probe.push(ch);
        if tr.measure_weighted(&probe, size, None, weight).0 > max_w {
            break;
        }
        out.push(ch);
    }
    out
}

/// Truncate to fit within two lines of width `max_w`.
fn two_line_elide(tr: &mut TextRenderer, s: &str, size: f32, max_w: f32) -> String {
    let (_w, h) = tr.measure(s, size, Some(max_w));
    let line_h = size * 1.25;
    if h <= line_h * 2.0 + 1.0 {
        return s.to_string();
    }
    // Binary-ish truncate by words until it fits two lines.
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut take = words.len();
    while take > 1 {
        take -= 1;
        let candidate = format!("{}…", words.get(..take).unwrap_or(&[]).join(" "));
        if tr.measure(&candidate, size, Some(max_w)).1 <= line_h * 2.0 + 1.0 {
            return candidate;
        }
    }
    s.to_string()
}

fn chrono_date(published: i64, now: i64) -> String {
    use chrono::{TimeZone, Utc};
    if published <= 0 {
        return String::new();
    }
    match Utc.timestamp_opt(published, 0).single() {
        Some(dt) => {
            let rel = relative_time(published, now);
            format!("{}  ({})", dt.format("%b %-d, %Y"), rel)
        }
        None => String::new(),
    }
}

// ---------------------------------------------------------------------
// Widget impl
// ---------------------------------------------------------------------

impl Widget for FeedsApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.now = chrono::Utc::now().timestamp();
        if !self.synced {
            if FeedsDoc::is_empty(state) {
                // The live app starts empty; only the hermetic demo build seeds
                // the bundled sample subscriptions into a fresh document.
                if self.demo {
                    FeedsDoc::seed(state, &self.store);
                }
            } else {
                self.store = FeedsDoc::load(state);
                self.ensure_selection();
                if self.selected.is_none() {
                    self.select_featured();
                }
            }
            // Seed reading settings on first run; otherwise adopt the persisted
            // (and possibly collaborator-updated) preferences.
            if FeedsDoc::has_settings(state) {
                self.settings = FeedsDoc::read_settings(state);
            } else {
                FeedsDoc::write_settings(state, &self.settings);
            }
            self.synced = true;
            self.store_heads = state.get_heads();
        }
        // Persist anything the live (doc-less) drain queued, then drain again
        // here so an input-driven sync also picks up fresh results promptly.
        self.flush_doc_writes(state);
        self.drain_fetches_live();
        self.flush_doc_writes(state);
        // Drain an in-flight OPML import portal request (holds the doc so the
        // parsed subscriptions persist the moment the user picks a file).
        self.drain_import(state);
        // A Ctrl+K palette action (or any other out-of-band write) mutates the
        // document directly, not this widget's mirrored store. Once the fetch
        // backlog is drained — so the store isn't leading the document with
        // not-yet-persisted articles — reload the store if the document has
        // moved since we last mirrored it, so those changes appear.
        if self.pending_doc_writes.is_empty() {
            let heads = state.get_heads();
            if heads != self.store_heads {
                self.store = FeedsDoc::load(state);
                // A palette delete can strand the selection on a now-gone
                // article; drop it rather than render a dangling reader.
                if let Some(sel) = self.selected
                    && self.store.article(sel).is_none()
                {
                    self.selected = None;
                }
                self.store_heads = heads;
            }
        }
    }

    /// Publish the reader + its folders and subscriptions so the host's Ctrl+K
    /// action palette scopes to them. Delegates to the [`ResourceProvider`]
    /// (`qos_actions::ResourceProvider`) impl in [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    /// Fold the reader's articles and subscriptions into the unified Ctrl+K
    /// palette: a snapshot
    /// [`FeedsSearchProvider`](crate::search_provider::FeedsSearchProvider)
    /// makes both findable there without touching the network, alongside the
    /// actions the app exports.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(crate::search_provider::FeedsSearchProvider::new(
            &self.store,
        ))]
    }

    /// Navigate to the article or feed a chosen palette row identifies. For an
    /// article, focus its feed (so the row is in the visible list) then open it,
    /// marking it read; for a feed, focus that subscription.
    fn activate_search_result(&mut self, action_id: &str, doc: &mut AutoCommit, _path: &ObjId) {
        match crate::search_provider::Activation::parse(action_id) {
            Some(crate::search_provider::Activation::Article(id)) => {
                if let Some(feed_id) = self.store.article(id).map(|a| a.feed_id) {
                    self.set_filter(Filter::Feed(feed_id));
                    self.select_article(id, doc);
                }
            }
            Some(crate::search_provider::Activation::Feed(id)) if self.store.feed(id).is_some() => {
                self.set_filter(Filter::Feed(id));
            }
            Some(crate::search_provider::Activation::Feed(_)) | None => {}
        }
    }

    #[allow(
        clippy::unwrap_used,
        reason = "a fresh leaf on a live taffy tree cannot fail short of OOM, and \
                  `Widget::build_layout` returns `NodeId` directly with no fallible path"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        // The width is now known — reconcile the flap so a breakpoint crossing
        // adopts the new default (wide ⇒ docked-open, narrow ⇒ overlay-closed)
        // before the next draw.
        let layout = self.nav_layout();
        self.sidebar.reconcile(layout);
        // On the very first real layout, settle the eased reveal to the
        // resolved default so a fresh narrow window opens closed (no slide-out
        // from the docked-open construction default) and a fresh wide window
        // opens with the rail already docked.
        if !self.settled_once {
            self.sidebar.snap(self.sidebar.is_open(layout));
            self.settled_once = true;
        }
        self.recompute_geometry();
        // With the widget rect known, lay the open dialog out over it so its
        // box + footer buttons + embedded field are current before draw/events.
        self.layout_modal();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Persist anything the live drain queued since the last doc-holding
        // pass (keeps the CRDT current without waiting for `sync`).
        self.flush_doc_writes(state);
        // Track the pointer everywhere so it's current when a modal closes.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }
        // An open dialog captures all input while it's up.
        if self.modal.is_some() {
            self.handle_modal_event(event, scale_factor, *modifiers, state, path, handled);
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                if self.click(p, state) {
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                // Right-click on a sidebar feed opens its context menu; a
                // right-click anywhere else dismisses any open menu.
                let p = self.last_mouse;
                if let Some(id) = self.feed_at(p) {
                    self.open_feed_menu(id, p);
                    *handled = true;
                } else if self.feed_menu.is_open() {
                    self.close_feed_menu();
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                if self.reader_rect.contains(self.last_mouse) {
                    let visible = (self.reader_rect.height() - HEADER_H).max(0.0);
                    let max = (self.reader_content_h - visible + 60.0).max(0.0);
                    self.reader_scroll = (self.reader_scroll - dy).clamp(0.0, max);
                } else if self.list_rect.contains(self.last_mouse) {
                    // The refresh strip (when visible) eats into the row viewport.
                    let view_h =
                        (self.list_rect.height() - HEADER_H - self.refresh_strip_h()).max(0.0);
                    let max = (self.list_content_h - view_h).max(0.0);
                    self.list_scroll = (self.list_scroll - dy).clamp(0.0, max);
                }
                *handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.handle_key(key_event, state, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "FeedsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames flowing while the sidebar flap is mid-slide, while
        // network fetches are outstanding (so the per-frame `draw` drain keeps
        // running until every result has landed), while the refresh progress
        // strip is visible, while CRDT writes await a flush, and briefly while
        // a toast is visible.
        self.sidebar.is_animating(self.nav_layout())
            || self.worker.as_ref().is_some_and(|w| w.pending() > 0)
            || self.refresh.visible()
            || !self.pending_doc_writes.is_empty()
            // Keep frames flowing while a file-chooser portal is open so its
            // outcome is drained (in `sync`) the moment the user picks a file.
            || self.import_rx.is_some()
            || self
                .toast
                .as_ref()
                .is_some_and(|(_, since)| since.elapsed().as_secs_f32() < 4.0)
    }
}

impl FeedsApp {
    fn handle_key(
        &mut self,
        key_event: &winit::event::KeyEvent,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // Esc closes an open feed context menu before anything else.
        if matches!(&key_event.logical_key, Key::Named(NamedKey::Escape))
            && self.feed_menu.is_open()
        {
            self.close_feed_menu();
            *handled = true;
            return;
        }

        // Esc dismisses the responsive overlay sidebar, then unwinds a pushed
        // detail back to the list (single-pane master→detail).
        if matches!(&key_event.logical_key, Key::Named(NamedKey::Escape)) {
            if self.nav_layout() == SplitLayout::Overlay
                && self.sidebar.is_open(SplitLayout::Overlay)
            {
                self.sidebar.close();
                *handled = true;
                return;
            }
            if self.single_pane() && self.detail_pushed {
                self.show_master();
                *handled = true;
                return;
            }
        }

        match &key_event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1, state);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1, state);
                *handled = true;
            }
            Key::Character(s) => {
                let ch = s.as_str();
                match ch {
                    "j" => {
                        self.move_selection(1, state);
                        *handled = true;
                    }
                    "k" => {
                        self.move_selection(-1, state);
                        *handled = true;
                    }
                    "m" => {
                        self.toggle_read(state);
                        *handled = true;
                    }
                    "s" => {
                        if let Some(id) = self.selected {
                            self.toggle_star(id, state);
                        }
                        *handled = true;
                    }
                    "r" => {
                        self.refresh_all();
                        *handled = true;
                    }
                    "a" => {
                        self.mark_all_read(state);
                        *handled = true;
                    }
                    "n" => {
                        self.open_add_subscription();
                        *handled = true;
                    }
                    "e" => {
                        self.export_opml();
                        *handled = true;
                    }
                    "i" => {
                        self.request_import();
                        *handled = true;
                    }
                    "1" => {
                        self.set_filter(Filter::All);
                        *handled = true;
                    }
                    "2" => {
                        self.set_filter(Filter::Unread);
                        *handled = true;
                    }
                    "3" => {
                        self.set_filter(Filter::Starred);
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Tests that exercise refresh/render over real subscriptions use the
    /// demo builder — the only place sample feeds come from. The live `new()`
    /// path starts empty (asserted by `live_new_starts_empty`).
    fn app() -> FeedsApp {
        FeedsApp::new_demo()
    }

    #[test]
    fn live_new_starts_empty_and_offers_onboarding() {
        // The live constructor must not fabricate any subscriptions or articles.
        let app = FeedsApp::new();
        assert!(
            app.store.feeds.is_empty(),
            "no fabricated feeds on the live path"
        );
        assert!(
            app.store.folders.is_empty(),
            "no fabricated folders on the live path"
        );
        assert!(
            app.store.articles.is_empty(),
            "no fabricated articles on the live path"
        );
        assert!(app.selected.is_none(), "nothing selected on an empty store");
        let (title, hint) = app
            .empty_onboarding_prompt()
            .expect("an empty store surfaces the first-run onboarding prompt");
        assert_eq!(title, EMPTY_ONBOARDING_TITLE);
        assert!(
            !hint.trim().is_empty(),
            "the hint guides the user to add a feed"
        );
    }

    #[test]
    fn live_sync_over_fresh_doc_stays_empty() {
        // Syncing the live app against a fresh (empty) document must NOT seed
        // sample content — only the demo build seeds.
        let mut app = FeedsApp::new();
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.store.feeds.is_empty(),
            "live sync must not fabricate feeds"
        );
        assert!(
            app.empty_onboarding_prompt().is_some(),
            "the empty store still shows onboarding after a live sync",
        );
    }

    #[test]
    fn demo_build_is_populated_and_hides_onboarding() {
        // The demo builder is the sole source of sample subscriptions.
        let app = FeedsApp::new_demo();
        assert!(!app.store.feeds.is_empty(), "demo build seeds sample feeds");
        assert!(
            app.empty_onboarding_prompt().is_none(),
            "a populated store hides the onboarding prompt",
        );
    }

    /// Run one full headless frame (sync → `build_layout` → compute → apply →
    /// draw), mirroring `ui::App`'s per-frame cycle, and return how long the
    /// **draw** pass took. No GPU — `TextRenderer`/`Scene` are CPU-side, so this
    /// exercises exactly the main-thread work a real frame does.
    fn headless_frame(
        app: &mut FeedsApp,
        doc: &mut AutoCommit,
        tr: &mut TextRenderer,
    ) -> std::time::Duration {
        use taffy::prelude::*;
        use vello::kurbo::Point;
        let theme = ui::theme::Theme::dark();
        let presence = ui::presence::PresenceCtx::empty();
        let permission = ui::permissions::PermissionContext::permissive();

        app.sync(doc, &automerge::ROOT);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let child = app.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(1280.0),
                        height: Dimension::length(800.0),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let trm = &mut *tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(1280.0),
                        height: AvailableSpace::Definite(800.0),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, trm) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        app.apply_layout(&taffy, child, Point::ORIGIN);

        let mut scene = vello::Scene::new();
        let t0 = std::time::Instant::now();
        app.draw_with_presence(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            tr,
            &theme,
            &presence,
            &permission,
        );
        t0.elapsed()
    }

    /// THE responsiveness regression test the coordinator demanded. Spawns the
    /// REAL worker (real thread + real Tokio runtime + real reqwest client),
    /// fires a refresh against unreachable hosts, then proves the MAIN THREAD
    /// stays responsive: `refresh_all`, every `draw`, and `handle_event` each
    /// return in well under a 16 ms frame budget while the refresh is in flight.
    /// If any network I/O, runtime construction, or blocking `recv` ran on the
    /// UI thread, one of these would blow the budget.
    #[test]
    fn real_worker_refresh_keeps_main_thread_responsive() {
        use std::time::{Duration, Instant};

        let mut a = app();
        let mut doc = AutoCommit::new();
        let mut tr = TextRenderer::new();
        // Warm frames so first-frame font loading / cache warm-up isn't
        // attributed to refresh, then capture a BASELINE for one idle draw +
        // one idle input event. The invariant we prove is build-independent:
        // an in-flight refresh must not make draw/handle_event meaningfully
        // slower than this baseline — i.e. no network / runtime / blocking recv
        // runs on the UI thread. (An absolute "<16ms" bound would just measure
        // how slow an unoptimised debug draw of the sample scene is.)
        let synth_event = || WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(200.0, 200.0),
        };
        let dispatch = |a: &mut FeedsApp, doc: &mut AutoCommit| {
            let mut handled = false;
            let mut focus: Option<NodeId> = None;
            let node = a.node.unwrap();
            let t = Instant::now();
            a.handle_event(
                &synth_event(),
                1.0,
                &winit::event::Modifiers::default(),
                doc,
                &automerge::ROOT,
                &mut handled,
                &mut focus,
                node,
                &TaffyTree::new(),
            );
            t.elapsed()
        };
        for _ in 0..3 {
            let _ = headless_frame(&mut a, &mut doc, &mut tr);
        }
        let mut base_draw = Duration::from_secs(9999);
        let mut base_event = Duration::from_secs(9999);
        for _ in 0..5 {
            base_draw = base_draw.min(headless_frame(&mut a, &mut doc, &mut tr));
            base_event = base_event.min(dispatch(&mut a, &mut doc));
        }

        // Point every feed at an unreachable host so the worker's fetches hang
        // on connect/timeout — the worst case for "does the UI block?".
        for f in &mut a.store.feeds {
            f.url = "https://10.255.255.1/feed.xml".to_string();
            f.kind = FeedKind::Rss;
        }

        // Cold start: the FIRST refresh must also spawn the worker + runtime
        // off the UI thread. The call itself must be trivial.
        let t_refresh = Instant::now();
        a.refresh_all();
        let refresh_cost = t_refresh.elapsed();
        assert!(
            refresh_cost < Duration::from_millis(16),
            "refresh_all (incl. cold worker/runtime spawn) must not block the UI thread; took {refresh_cost:?}"
        );
        assert!(a.worker.is_some(), "worker really spawned");
        assert!(a.refresh.in_progress());

        // Pump frames for a window while the fetches are genuinely outstanding
        // (the worker hammers unreachable hosts the whole time). Track the best
        // (fastest achievable) draw + event during the refresh — if any network
        // I/O or blocking recv leaked onto the UI thread, even the *best* frame
        // would balloon far past the idle baseline.
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut frames = 0u32;
        let mut best_draw = Duration::from_secs(9999);
        let mut best_event = Duration::from_secs(9999);
        while Instant::now() < deadline {
            best_draw = best_draw.min(headless_frame(&mut a, &mut doc, &mut tr));
            best_event = best_event.min(dispatch(&mut a, &mut doc));
            frames += 1;
        }

        assert!(
            frames > 5,
            "the loop kept pumping frames while refreshing (got {frames})"
        );
        // The refresh adds no blocking to the UI thread: draw + event during a
        // refresh are within a small factor of the idle baseline (generous 4x
        // slack absorbs scheduler jitter from the busy worker threads). A real
        // main-thread network block would be orders of magnitude over.
        let draw_bound = base_draw * 4 + Duration::from_millis(2);
        let event_bound = base_event * 4 + Duration::from_millis(2);
        assert!(
            best_draw <= draw_bound,
            "draw blocked during refresh: {best_draw:?} vs idle baseline {base_draw:?} (bound {draw_bound:?})"
        );
        assert!(
            best_event <= event_bound,
            "handle_event blocked during refresh: {best_event:?} vs idle baseline {base_event:?} (bound {event_bound:?})"
        );
        // And an event dispatch is genuinely cheap in absolute terms — a cursor
        // move can never touch the network, so this is a hard ceiling.
        assert!(
            best_event < Duration::from_millis(16),
            "a cursor move must be instant even during refresh; took {best_event:?}"
        );
        // While fetches are still outstanding, the widget keeps asking for
        // frames so the drain runs to completion without user input.
        assert!(
            a.needs_animation_frame(),
            "needs_animation_frame stays true while the refresh is in flight"
        );
    }

    // --- async refresh (non-blocking) ------------------------------------

    /// Proves the refresh path is fully async: `refresh_all` only enqueues (no
    /// network, returns instantly); results arrive via the worker's channel;
    /// the draw-time drain (`drain_fetches_live`) applies them to the store +
    /// progress WITHOUT a document handle and WITHOUT calling the blocking
    /// fetch; and the queued CRDT writes are flushed by a doc-holding pass. The
    /// UI passes that run every frame (`draw`/`handle_event` → drain) never
    /// touch the network.
    #[test]
    fn refresh_is_async_and_drains_without_blocking() {
        use std::time::Instant;

        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);

        // Swap in a thread-less worker so we control results deterministically
        // and never hit the network.
        let tx = a.install_test_worker();

        // Kick off a refresh. This must return immediately — it only enqueues.
        let start = Instant::now();
        a.refresh_all();
        assert!(
            start.elapsed().as_millis() < 50,
            "refresh_all must not block on the network"
        );
        assert!(a.refresh.in_progress(), "a refresh is now in progress");
        assert!(a.refresh.total > 0);
        // Nothing has been drained yet — no input, no draw has run.
        assert_eq!(a.refresh.done, 0);

        // A worker "completes" one feed: post a result onto the channel exactly
        // as the real background thread would.
        let feed_id = a.store.feeds[0].id;
        let new_article = Article {
            id: Uuid::new_v4(),
            feed_id,
            title: "Fresh from the worker".into(),
            author: String::new(),
            url: "https://example.com/brand-new".into(),
            published: a.now,
            summary: String::new(),
            blocks: vec![],
            read: false,
            starred: false,
        };
        let before = a.store.articles.len();
        tx.send(FetchResult::Articles {
            feed_id,
            title: None,
            articles: vec![new_article.clone()],
        })
        .unwrap();

        // The *draw-time* drain (no document handle) applies the result live.
        let changed = a.drain_fetches_live();
        assert!(changed, "live drain reports a change");
        assert_eq!(
            a.store.articles.len(),
            before + 1,
            "new article folded into the store"
        );
        assert_eq!(a.refresh.done, 1, "progress advanced");
        assert_eq!(a.refresh.new_items, 1, "new-item count tracked");
        assert_eq!(a.refresh.updated_feeds, 1, "feed counted as updated");
        // The CRDT write was queued (deferred), not yet persisted.
        assert!(
            !a.pending_doc_writes.is_empty(),
            "doc write queued for the next doc pass"
        );

        // A doc-holding pass flushes the queue into the Automerge document.
        a.flush_doc_writes(&mut doc);
        assert!(a.pending_doc_writes.is_empty(), "queue drained after flush");
        let persisted = FeedsDoc::load(&doc)
            .articles
            .iter()
            .any(|x| x.id == new_article.id);
        assert!(persisted, "the new article is now persisted in the CRDT");

        // A failure for another feed advances progress + failure count.
        tx.send(FetchResult::Failed {
            feed_id: Some(a.store.feeds[1].id),
            message: "boom".into(),
        })
        .unwrap();
        a.drain_fetches_live();
        assert_eq!(a.refresh.done, 2);
        assert_eq!(a.refresh.failed_feeds, 1);

        // While the batch is incomplete, the widget keeps requesting frames so
        // the drain keeps running without user input — this is what makes the
        // refresh land while idle instead of appearing to "block".
        assert!(
            a.needs_animation_frame(),
            "frames keep flowing until the batch finishes"
        );
    }

    /// A refresh that returns a LARGE volume of articles (the realistic case
    /// the unreachable-host test can't hit) must still keep the main thread
    /// responsive: the live drain + the per-frame CRDT flush must not blow the
    /// frame budget even when hundreds of articles land at once. Proves the
    /// flush is bounded per frame rather than persisting everything in one go.
    #[test]
    fn large_result_batch_does_not_block_the_frame() {
        use std::time::{Duration, Instant};

        let mut a = app();
        let mut doc = AutoCommit::new();
        let mut tr = TextRenderer::new();
        let _ = headless_frame(&mut a, &mut doc, &mut tr);

        let tx = a.install_test_worker();
        a.refresh_all();

        // Each feed returns a big batch — ~100 articles each, several feeds.
        let feed_ids: Vec<Id> = a.store.feeds.iter().map(|f| f.id).collect();
        let mut total_new = 0usize;
        for fid in &feed_ids {
            let mut arts = Vec::new();
            for i in 0..100 {
                arts.push(Article {
                    id: Uuid::new_v4(),
                    feed_id: *fid,
                    title: format!("Article {i} for feed"),
                    author: "a".into(),
                    url: format!("https://example.com/{fid}/{i}"),
                    published: a.now - i64::from(i) * 60,
                    summary: "summary text".into(),
                    blocks: vec![
                        crate::model::ContentBlock::new(
                            crate::model::BlockKind::Paragraph,
                            "Body paragraph one.",
                        ),
                        crate::model::ContentBlock::new(
                            crate::model::BlockKind::Paragraph,
                            "Body paragraph two.",
                        ),
                    ],
                    read: false,
                    starred: false,
                });
                total_new += 1;
            }
            tx.send(FetchResult::Articles {
                feed_id: *fid,
                title: None,
                articles: arts,
            })
            .unwrap();
        }

        // The live drain folds everything into the store in one pass. This is
        // pure in-memory work (no CRDT, no network) and must be cheap even for
        // a big batch — it's what runs on the draw thread every frame.
        let t_drain = Instant::now();
        a.drain_fetches_live();
        let drain_cost = t_drain.elapsed();
        assert!(
            drain_cost < Duration::from_millis(16),
            "live in-memory drain blocked the frame with a big batch: {drain_cost:?}"
        );
        // The whole batch is now visible in the store immediately...
        assert!(a.store.articles.len() >= total_new);
        // ...but the CRDT writes are queued, NOT persisted in one blocking pass.
        assert!(a.pending_doc_writes.len() >= total_new - 8);

        // The key invariant: each flush call persists at most FLUSH_BUDGET
        // writes, so the per-frame main-thread cost of persistence is BOUNDED
        // and independent of how big the refresh was. (Automerge write cost is
        // ~constant per op in release; the fix is that we never do the whole
        // backlog in a single frame — that was the multi-second hang.)
        let before = a.pending_doc_writes.len();
        a.flush_doc_writes(&mut doc);
        let persisted_this_frame = before - a.pending_doc_writes.len();
        assert!(
            persisted_this_frame <= FLUSH_BUDGET,
            "one flush must persist at most FLUSH_BUDGET ({FLUSH_BUDGET}) writes, did {persisted_this_frame}"
        );

        // Drain the rest across frames; the backlog empties in bounded steps.
        let mut frames = 1;
        while !a.pending_doc_writes.is_empty() && frames < 5000 {
            a.flush_doc_writes(&mut doc);
            frames += 1;
        }
        assert!(a.pending_doc_writes.is_empty(), "backlog fully drained");
        // Everything eventually persisted to the CRDT.
        let persisted = FeedsDoc::load(&doc).articles.len();
        assert!(
            persisted >= total_new,
            "all {total_new} articles persisted (got {persisted})"
        );
    }

    /// `refresh_all` enqueues exactly one request per feed and finishes only
    /// once every feed has reported back (success or failure).
    #[test]
    fn refresh_progress_finishes_when_all_feeds_report() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let tx = a.install_test_worker();
        a.refresh_all();
        let total = a.refresh.total;
        assert_eq!(total, a.store.feeds.len());

        // Every feed reports back (alternating success / failure).
        for (i, f) in a.store.feeds.clone().iter().enumerate() {
            if i % 2 == 0 {
                tx.send(FetchResult::Articles {
                    feed_id: f.id,
                    title: None,
                    articles: vec![],
                })
                .unwrap();
            } else {
                tx.send(FetchResult::Failed {
                    feed_id: Some(f.id),
                    message: "x".into(),
                })
                .unwrap();
            }
        }
        a.drain_fetches_live();
        assert_eq!(a.refresh.done, total);
        assert!(!a.refresh.in_progress(), "batch finished");
        assert!(a.refresh.finished_at.is_some(), "completion timestamped");
        // The completion summary lingers briefly, then frames can stop.
        assert!(a.refresh.visible());
    }

    /// Changing a reading setting applies it (and persists), proving the
    /// settings affect the reader. Drives the production action path.
    #[test]
    fn reading_settings_apply_and_persist() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);

        // Default is Sans; switch to Serif via the production action.
        assert_eq!(a.settings.font, ReadFont::Sans);
        a.apply_settings_action(SettingsAction::SetFont(ReadFont::Serif), &mut doc);
        assert_eq!(a.settings.font, ReadFont::Serif);
        assert_eq!(
            a.reader_family(),
            FontFamily::Serif,
            "reader renders in serif"
        );

        // Theme switch.
        a.apply_settings_action(SettingsAction::SetTheme(ReadTheme::Sepia), &mut doc);
        assert_eq!(a.settings.theme, ReadTheme::Sepia);

        // Font size step changes the body size and clamps.
        let base = a.settings.body_size();
        a.apply_settings_action(SettingsAction::FontInc, &mut doc);
        assert!(a.settings.body_size() > base, "text size increased");

        // All of it persisted to the CRDT.
        let loaded = FeedsDoc::read_settings(&doc);
        assert_eq!(loaded.font, ReadFont::Serif);
        assert_eq!(loaded.theme, ReadTheme::Sepia);
        assert!(loaded.font_scale > 1.0);
    }

    #[test]
    fn new_selects_a_featured_article() {
        let a = app();
        assert!(a.selected.is_some());
        let sel = a.selected.unwrap();
        let art = a.store.article(sel).unwrap();
        assert!(!art.blocks.is_empty());
    }

    #[test]
    fn seed_populates_doc_on_first_sync() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        assert!(FeedsDoc::is_empty(&doc));
        a.sync(&mut doc, &automerge::ROOT);
        assert!(!FeedsDoc::is_empty(&doc));
        let loaded = FeedsDoc::load(&doc);
        assert_eq!(loaded.feeds.len(), a.store.feeds.len());
    }

    #[test]
    fn selecting_marks_read_in_store_and_doc() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        // Pick an unread article in the All list.
        let unread = a
            .store
            .articles_for(Filter::All)
            .into_iter()
            .find(|x| !x.read)
            .map(|x| x.id)
            .unwrap();
        a.select_article(unread, &mut doc);
        assert!(a.store.article(unread).unwrap().read);
        assert!(
            FeedsDoc::load(&doc)
                .articles
                .iter()
                .find(|x| x.id == unread)
                .unwrap()
                .read
        );
    }

    #[test]
    fn star_toggles_through_doc() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let id = a.store.articles[0].id;
        let before = a.store.article(id).unwrap().starred;
        a.toggle_star(id, &mut doc);
        assert_ne!(a.store.article(id).unwrap().starred, before);
        let doc_star = FeedsDoc::load(&doc)
            .articles
            .iter()
            .find(|x| x.id == id)
            .unwrap()
            .starred;
        assert_eq!(doc_star, !before);
    }

    #[test]
    fn filter_changes_list_and_selection() {
        let mut a = app();
        a.set_filter(Filter::Starred);
        let starred = a.store.articles_for(Filter::Starred);
        assert!(!starred.is_empty());
        // Selection must be within the starred set.
        assert!(starred.iter().any(|x| Some(x.id) == a.selected));
    }

    #[test]
    fn mark_all_read_clears_unread_filter() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        a.set_filter(Filter::All);
        a.mark_all_read(&mut doc);
        assert_eq!(a.store.total_unread(), 0);
    }

    #[test]
    fn folder_toggle_collapses() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let fid = a.store.folders[0].id;
        let before = a.store.folder(fid).unwrap().collapsed;
        a.toggle_folder(fid, &mut doc);
        assert_ne!(a.store.folder(fid).unwrap().collapsed, before);
    }

    // --- responsive layout ------------------------------------------------

    /// Drive the same path `apply_layout` runs: set the allocated rect,
    /// reconcile + settle the flap, and recompute the cached pane geometry.
    fn lay_out(app: &mut FeedsApp, w: f64, h: f64) {
        app.rect = Rect::new(0.0, 0.0, w, h);
        let layout = app.nav_layout();
        app.sidebar.reconcile(layout);
        if !app.settled_once {
            app.sidebar.snap(app.sidebar.is_open(layout));
            app.settled_once = true;
        }
        app.recompute_geometry();
    }

    #[test]
    fn wide_keeps_three_panes_docked() {
        let mut a = app();
        lay_out(&mut a, 1280.0, 800.0);
        assert_eq!(a.nav_layout(), SplitLayout::Docked);
        // Sidebar is docked open beside the content (full reveal).
        assert!((a.sidebar_rect.width() - SIDEBAR_W).abs() < 0.5);
        // All three panes are present and ordered left→right.
        assert!(a.sidebar_rect.x1 <= a.list_rect.x0 + 0.5);
        assert!(a.list_rect.x1 <= a.reader_rect.x0 + 0.5);
        assert!(
            a.reader_rect.width() > 1.0,
            "reader pane has room when wide"
        );
        // No overlay chrome when docked-open.
        assert_eq!(a.scrim_rect.area(), 0.0);
        assert_eq!(a.hamburger_rect.area(), 0.0);
    }

    #[test]
    fn narrow_collapses_sidebar_to_closed_overlay() {
        let mut a = app();
        lay_out(&mut a, 560.0, 800.0);
        assert_eq!(a.nav_layout(), SplitLayout::Overlay);
        // Sidebar starts closed (slid off the left edge) — a fresh narrow
        // window opens straight to the list.
        assert!(!a.sidebar.is_open(SplitLayout::Overlay));
        assert!(
            a.sidebar_rect.x1 <= a.rect.x0 + 0.5,
            "closed flap is off-screen left"
        );
        // The hamburger is offered to summon it; no scrim while closed.
        assert!(a.hamburger_rect.area() > 0.0);
        assert_eq!(a.scrim_rect.area(), 0.0);
        // Master (list) fills the full content width.
        assert!((a.list_rect.width() - a.rect.width()).abs() < 0.5);
    }

    #[test]
    fn hamburger_click_opens_overlay_sidebar_with_scrim() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        lay_out(&mut a, 560.0, 800.0);
        let p = a.hamburger_rect.center();
        assert!(a.click(p, &mut doc), "hamburger click is consumed");
        assert!(a.sidebar.is_open(SplitLayout::Overlay), "sidebar opens");
        a.snap_sidebar_open(true);
        lay_out_keep(&mut a, 560.0, 800.0);
        assert!(
            a.scrim_rect.area() > 0.0,
            "open overlay flap dims the content"
        );
        assert!(
            (a.sidebar_rect.x0 - a.rect.x0).abs() < 0.5,
            "flap slid flush to the left"
        );
    }

    #[test]
    fn scrim_click_dismisses_overlay_sidebar() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        lay_out(&mut a, 560.0, 800.0);
        a.snap_sidebar_open(true);
        lay_out_keep(&mut a, 560.0, 800.0);
        // Click on the dimmed content to the right of the floating sidebar.
        let p = Point::new(a.rect.x1 - 20.0, 400.0);
        assert!(a.click(p, &mut doc), "scrim click is consumed");
        assert!(
            !a.sidebar.is_open(SplitLayout::Overlay),
            "scrim click closes the flap"
        );
    }

    #[test]
    fn selecting_an_article_pushes_detail_when_narrow() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 560.0, 800.0);
        assert!(!a.detail_pushed, "narrow window opens on the master list");
        let id = a.store.articles_for(Filter::All)[0].id;
        a.select_article(id, &mut doc);
        assert!(a.detail_pushed, "selecting pushes the reader (detail) in");
        // Recompute: the reader now fills the pane and a Back affordance shows.
        lay_out_keep(&mut a, 560.0, 800.0);
        assert!((a.reader_rect.width() - a.rect.width()).abs() < 0.5);
        assert!(
            a.back_rect.area() > 0.0,
            "detail pane offers a Back affordance"
        );
        // Hamburger is suppressed in detail (Back owns the corner).
        assert_eq!(a.hamburger_rect.area(), 0.0);
        // Back returns to the master list.
        let p = a.back_rect.center();
        assert!(a.click(p, &mut doc), "Back click is consumed");
        assert!(!a.detail_pushed, "Back returns to the master list");
    }

    #[test]
    fn selecting_an_article_does_not_push_when_wide() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 1280.0, 800.0);
        let id = a.store.articles_for(Filter::All)[0].id;
        a.select_article(id, &mut doc);
        assert!(!a.detail_pushed, "wide layout shows list + reader together");
        assert_eq!(
            a.back_rect.area(),
            0.0,
            "no Back affordance in the docked layout"
        );
    }

    /// The middle tier: the sidebar stays docked + fixed while exactly ONE
    /// content pane (the list here, since nothing is pushed) shares the rest of
    /// the width. No overlay chrome, no collapse toggle — the sidebar is fixed.
    #[test]
    fn middle_docks_sidebar_and_shows_a_single_content_pane() {
        let mut a = app();
        lay_out(&mut a, 820.0, 800.0);
        assert_eq!(a.tier(), Tier::Middle);
        assert_eq!(
            a.nav_layout(),
            SplitLayout::Docked,
            "the sidebar is docked, not an overlay"
        );
        // Sidebar docked at full width, fixed (no collapse toggle, no scrim).
        assert!(
            (a.sidebar_rect.width() - SIDEBAR_W).abs() < 0.5,
            "sidebar docked at full width"
        );
        assert_eq!(
            a.hamburger_rect.area(),
            0.0,
            "the middle-tier sidebar is fixed (no toggle)"
        );
        assert_eq!(a.scrim_rect.area(), 0.0, "a docked sidebar shows no scrim");
        // A single content pane fills the width right of the sidebar; with
        // nothing pushed it is the article list, and the reader is parked off.
        assert!(!a.detail_pushed, "the middle tier opens on the list");
        assert!(
            (a.list_rect.x0 - a.sidebar_rect.x1).abs() < 0.5,
            "list starts at the sidebar edge"
        );
        assert!(
            (a.list_rect.x1 - a.rect.x1).abs() < 0.5,
            "list fills to the right edge"
        );
        assert!(
            a.reader_rect.x0 >= a.rect.x1 - 0.5,
            "the reader is parked off-pane to the right"
        );
        assert_eq!(
            a.back_rect.area(),
            0.0,
            "no Back affordance while the list is showing"
        );
    }

    /// The middle tier's master→detail: selecting pushes the reader into the
    /// single content pane with a top-left Back button; Back returns to the
    /// list. Driven entirely through the real selection + click paths + state.
    #[test]
    fn middle_back_button_navigates_detail_to_list() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 820.0, 800.0);
        assert_eq!(a.tier(), Tier::Middle);
        assert!(!a.detail_pushed, "opens on the list");

        // Select an article: the reader is pushed into the single content pane.
        let id = a.store.articles_for(Filter::All)[0].id;
        a.select_article(id, &mut doc);
        assert!(
            a.detail_pushed,
            "selecting pushes the reader in at the middle tier"
        );
        lay_out_keep(&mut a, 820.0, 800.0);
        // Reader now fills the content pane; the list is parked off to the left.
        assert!(
            (a.reader_rect.x0 - a.sidebar_rect.x1).abs() < 0.5,
            "reader fills the content pane"
        );
        assert!((a.reader_rect.x1 - a.rect.x1).abs() < 0.5);
        assert!(
            a.list_rect.x1 <= a.sidebar_rect.x1 + 0.5,
            "the list is parked off-pane to the left"
        );
        // A top-left Back affordance is offered inside the reader pane.
        assert!(
            a.back_rect.area() > 0.0,
            "the pushed reader offers a Back button"
        );
        assert!(
            a.back_rect.x0 >= a.sidebar_rect.x1 - 0.5,
            "Back sits at the content pane's top-left"
        );

        // Clicking Back returns to the list on real state.
        let p = a.back_rect.center();
        assert!(a.click(p, &mut doc), "Back click is consumed");
        assert!(!a.detail_pushed, "Back returns the single pane to the list");
        lay_out_keep(&mut a, 820.0, 800.0);
        assert!(
            (a.list_rect.x1 - a.rect.x1).abs() < 0.5,
            "the list fills the content pane again"
        );
        assert_eq!(
            a.back_rect.area(),
            0.0,
            "no Back affordance once back on the list"
        );
        // The sidebar stayed docked + fixed throughout.
        assert!((a.sidebar_rect.width() - SIDEBAR_W).abs() < 0.5);
        assert_eq!(a.scrim_rect.area(), 0.0);
    }

    /// Crossing the tier boundaries flips the sidebar between docked (wide +
    /// middle) and an overlay drawer (narrow) — proven off the width alone.
    #[test]
    fn tier_resolves_from_width() {
        assert_eq!(Tier::from_width(1280.0), Tier::Wide);
        assert_eq!(
            Tier::from_width(1000.0),
            Tier::Wide,
            "wide boundary is inclusive"
        );
        assert_eq!(Tier::from_width(999.0), Tier::Middle);
        assert_eq!(
            Tier::from_width(640.0),
            Tier::Middle,
            "middle boundary is inclusive"
        );
        assert_eq!(Tier::from_width(639.0), Tier::Narrow);
        assert_eq!(Tier::from_width(400.0), Tier::Narrow);
    }

    /// Re-run geometry without re-settling the flap (so an explicit
    /// snap/toggle survives), mirroring a post-interaction draw frame.
    fn lay_out_keep(app: &mut FeedsApp, w: f64, h: f64) {
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.recompute_geometry();
    }

    // --- OPML import + per-feed context menu ------------------------------

    fn synth_device() -> winit::event::DeviceId {
        winit::event::DeviceId::dummy()
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn right_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        }
    }
    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: synth_device(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    /// A `Character` key press carrying `s` as both the logical key and text —
    /// the shape winit delivers for a typed character. Zeroed base then the
    /// inspected fields overwritten (winit's `KeyEventExtra` is `pub(crate)`),
    /// same pattern the toolkit's own widget tests use.
    fn char_key(s: &str) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(s.into());
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(s.into());
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    fn named_key(k: NamedKey) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: synth_device(),
            event: ev,
            is_synthetic: false,
        }
    }
    /// Dispatch an event with explicit modifiers held (for Ctrl+V etc).
    fn dispatch_mods(
        a: &mut FeedsApp,
        doc: &mut AutoCommit,
        event: &WindowEvent,
        mods: winit::event::Modifiers,
    ) -> bool {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        a.handle_event(
            event,
            1.0,
            &mods,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }
    /// Drive the app's real `handle_event` dispatch (same path the winit loop
    /// uses), returning whether the event was consumed.
    fn dispatch(a: &mut FeedsApp, doc: &mut AutoCommit, event: &WindowEvent) -> bool {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        a.handle_event(
            event,
            1.0,
            &winit::event::Modifiers::default(),
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            &taffy,
        );
        handled
    }

    /// Open the per-feed context menu for the first sidebar feed row via the
    /// real right-click path, returning that feed's id.
    fn open_first_feed_menu(a: &mut FeedsApp, doc: &mut AutoCommit) -> Id {
        let (id, center) = a
            .sidebar_rows
            .iter()
            .find_map(|r| match r.role {
                RowRole::Feed { id, .. } => Some((id, r.rect.center())),
                _ => None,
            })
            .expect("a feed row exists in the sidebar");
        dispatch(a, doc, &cursor_at(center));
        assert!(
            dispatch(a, doc, &right_press()),
            "right-click on a feed is consumed"
        );
        assert!(a.feed_menu.is_open(), "right-click opens the context menu");
        assert_eq!(
            a.feed_menu_target,
            Some(id),
            "the menu targets the clicked feed"
        );
        id
    }

    /// A logical point that the open menu's hit-test maps to action row `idx`.
    /// Probes the real `item_at` so it never couples to the popover's private
    /// row metrics.
    fn menu_point_for(a: &FeedsApp, idx: usize) -> Point {
        let r = a.feed_menu.get_rect();
        let x = r.x0 + 20.0;
        // Bounded by the menu's pixel height (+1 margin) rather than a float
        // `while`, so a pathological rect can't spin forever on rounding.
        let steps = (r.y1 - r.y0).max(0.0).ceil() as u32 + 1;
        for step in 0..steps {
            let y = r.y0 + f64::from(step);
            if y >= r.y1 {
                break;
            }
            if a.feed_menu.item_at([x, y]) == Some(idx) {
                return Point::new(x, y);
            }
        }
        panic!("no point maps to menu item {idx}");
    }

    #[test]
    fn opml_import_adds_all_feeds_and_persists() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);

        let feeds_before = a.store.feeds.len();
        let new_ids = a.apply_opml_import(opml::sample_opml(), &mut doc);

        // All three OPML feeds are new subscriptions (none deduped away).
        assert_eq!(new_ids.len(), 3, "every OPML feed was subscribed");
        assert_eq!(a.store.feeds.len(), feeds_before + 3);
        for url in [
            "https://lwn.net/headlines/rss",
            "https://simonwillison.net/atom/everything/",
            "https://jvns.ca/atom.xml",
        ] {
            assert!(
                a.store.feeds.iter().any(|f| f.url == url),
                "store gained a subscription for {url}"
            );
        }
        // The OPML folder was created.
        assert!(a.store.folders.iter().any(|f| f.name == "Reading"));

        // Persisted to the document (the real convergence path).
        let loaded = FeedsDoc::load(&doc);
        for url in [
            "https://lwn.net/headlines/rss",
            "https://simonwillison.net/atom/everything/",
            "https://jvns.ca/atom.xml",
        ] {
            assert!(
                loaded.feeds.iter().any(|f| f.url == url),
                "document persisted the subscription for {url}"
            );
        }
        assert!(loaded.folders.iter().any(|f| f.name == "Reading"));

        // Re-importing the same OPML is a no-op (dedup by URL).
        let again = a.apply_opml_import(opml::sample_opml(), &mut doc);
        assert!(again.is_empty(), "re-import adds nothing");
    }

    #[test]
    fn portal_import_drain_reads_file_and_subscribes() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        // A no-thread worker so the post-import background refresh only records
        // pending requests (no network, no spawned thread).
        let _tx = a.install_test_worker();

        // Write a real OPML file to a scratch path and hand the app the outcome
        // the portal worker would deliver, then run one `sync` — the exact
        // per-frame drain the live portal feeds.
        let dir = std::env::temp_dir().join(format!("feeds-opml-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("subs.opml");
        std::fs::write(&path, opml::sample_opml()).unwrap();

        let feeds_before = a.store.feeds.len();
        a.import_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(path)));
        assert!(
            a.import_rx.is_some(),
            "a request is in flight until drained"
        );

        a.sync(&mut doc, &automerge::ROOT);

        assert!(
            a.import_rx.is_none(),
            "draining the outcome clears the request"
        );
        assert_eq!(
            a.store.feeds.len(),
            feeds_before + 3,
            "all OPML feeds subscribed"
        );
        assert!(
            a.store
                .feeds
                .iter()
                .any(|f| f.url == "https://jvns.ca/atom.xml")
        );
        // Persisted on the real path.
        assert!(
            FeedsDoc::load(&doc)
                .feeds
                .iter()
                .any(|f| f.url == "https://lwn.net/headlines/rss")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cancelled_import_leaves_store_untouched() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let before = a.store.feeds.len();
        a.import_rx = Some(ui::portal::inject_for_test(PortalOutcome::Cancelled));
        a.sync(&mut doc, &automerge::ROOT);
        assert!(a.import_rx.is_none(), "cancel clears the request");
        assert_eq!(a.store.feeds.len(), before, "cancel adds no feeds");
    }

    #[test]
    fn right_click_feed_opens_context_menu_with_actions() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 1280.0, 800.0);

        let id = open_first_feed_menu(&mut a, &mut doc);
        assert_eq!(a.feed_menu_target, Some(id));
        // Three actions: mark-all-read, rename, unsubscribe.
        assert_eq!(
            a.feed_menu_actions,
            vec![
                FeedMenuAction::MarkAllRead,
                FeedMenuAction::Rename,
                FeedMenuAction::Unsubscribe
            ]
        );

        // A right-click on empty (non-feed) space dismisses the menu.
        let off = a.reader_rect.center();
        dispatch(&mut a, &mut doc, &cursor_at(off));
        assert!(dispatch(&mut a, &mut doc, &right_press()));
        assert!(
            !a.feed_menu.is_open(),
            "right-click off a feed closes the menu"
        );
    }

    #[test]
    fn context_menu_unsubscribe_removes_feed_and_persists() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 1280.0, 800.0);

        let id = open_first_feed_menu(&mut a, &mut doc);
        assert!(a.store.feed(id).is_some());
        let had_articles = a.store.unread_for_feed(id)
            + a.store.articles.iter().filter(|x| x.feed_id == id).count();
        assert!(had_articles > 0, "the target feed has articles to remove");

        // Click the Unsubscribe row (index 2) via the real click path — this
        // now opens a confirmation dialog rather than removing on the spot.
        let p = menu_point_for(&a, 2);
        assert!(a.click(p, &mut doc), "unsubscribe click is consumed");
        assert!(!a.feed_menu.is_open(), "the menu closes after the action");

        let m = a
            .modal
            .as_ref()
            .expect("unsubscribe opens a confirmation dialog");
        assert_eq!(m.intent, ModalIntent::Unsubscribe { feed_id: id });
        assert!(
            a.store.feed(id).is_some(),
            "the feed is NOT removed until the confirmation is accepted"
        );

        // Confirm via the dialog's real primary ("Unsubscribe") button.
        lay_out(&mut a, 1280.0, 800.0);
        a.layout_modal();
        let primary = a.modal.as_ref().unwrap().dialog.primary_rect().center();
        assert!(dispatch(&mut a, &mut doc, &cursor_at(primary)));
        assert!(
            dispatch(&mut a, &mut doc, &left_press()),
            "confirm click is consumed"
        );

        assert!(a.modal.is_none(), "confirming closes the dialog");
        assert!(
            a.store.feed(id).is_none(),
            "the feed is removed from the store"
        );
        assert!(
            !a.store.articles.iter().any(|x| x.feed_id == id),
            "the feed's articles are removed too"
        );
        // Persisted: the document no longer carries the feed or its articles.
        let loaded = FeedsDoc::load(&doc);
        assert!(!loaded.feeds.iter().any(|f| f.id == id));
        assert!(!loaded.articles.iter().any(|x| x.feed_id == id));
    }

    #[test]
    fn context_menu_mark_all_read_marks_the_feed_read() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 1280.0, 800.0);

        let id = open_first_feed_menu(&mut a, &mut doc);
        assert!(
            a.store.unread_for_feed(id) > 0,
            "the feed starts with unread items"
        );

        // Click the Mark-all-read row (index 0).
        let p = menu_point_for(&a, 0);
        assert!(a.click(p, &mut doc));

        assert_eq!(
            a.store.unread_for_feed(id),
            0,
            "every article in the feed is read"
        );
        // Persisted: the document's articles for this feed are all read.
        let loaded = FeedsDoc::load(&doc);
        assert!(
            loaded
                .articles
                .iter()
                .filter(|x| x.feed_id == id)
                .all(|x| x.read),
            "the read flags persisted to the document"
        );
    }

    #[test]
    fn context_menu_rename_opens_prefilled_modal_and_persists() {
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        lay_out(&mut a, 1280.0, 800.0);

        let id = open_first_feed_menu(&mut a, &mut doc);
        let old_title = a.store.feed(id).unwrap().title.clone();

        // Click the Rename row (index 1) — opens the rename modal prefilled with
        // the current title in a real TextInput.
        let p = menu_point_for(&a, 1);
        assert!(a.click(p, &mut doc));
        let m = a.modal.as_ref().expect("rename opens the rename modal");
        assert_eq!(m.intent, ModalIntent::RenameFeed { feed_id: id });
        assert_eq!(
            m.dialog.content().value(),
            old_title,
            "the field is prefilled with the current title"
        );

        // Commit a new name through the production mutation and assert it lands
        // in both the store and the document.
        a.rename_feed(id, "My Renamed Feed", &mut doc);
        assert_eq!(a.store.feed(id).unwrap().title, "My Renamed Feed");
        let loaded = FeedsDoc::load(&doc);
        assert_eq!(
            loaded.feeds.iter().find(|f| f.id == id).unwrap().title,
            "My Renamed Feed",
            "the rename persisted to the document"
        );
    }

    #[test]
    fn fetched_articles_render_through_the_store() {
        // A real fetch result (as the worker posts it) folds into the store and
        // renders — the article carries laid-out content blocks.
        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let tx = a.install_test_worker();

        let feed_id = a.store.feeds[0].id;
        let before = a.store.articles_for(Filter::Feed(feed_id)).len();
        let art = Article {
            id: Uuid::new_v4(),
            feed_id,
            title: "Fresh headline".into(),
            author: "Reporter".into(),
            url: "https://example.com/fresh".into(),
            published: a.now,
            summary: "A summary".into(),
            blocks: vec![crate::model::ContentBlock::new(
                BlockKind::Paragraph,
                "The body text renders as a block.",
            )],
            read: false,
            starred: false,
        };
        tx.send(FetchResult::Articles {
            feed_id,
            title: None,
            articles: vec![art.clone()],
        })
        .unwrap();

        // The live (doc-less) drain folds it into the store immediately.
        assert!(
            a.drain_fetches_live(),
            "the fetched article drains into the store"
        );
        a.flush_doc_writes(&mut doc);

        let after = a.store.articles_for(Filter::Feed(feed_id));
        assert_eq!(after.len(), before + 1, "the article joins the feed's list");
        let fresh = a
            .store
            .article(art.id)
            .expect("the article is in the store");
        assert!(
            !fresh.blocks.is_empty(),
            "the article body renders as blocks"
        );
        // Persisted to the document with its content.
        assert!(
            FeedsDoc::load(&doc)
                .articles
                .iter()
                .any(|x| x.id == art.id && !x.blocks.is_empty())
        );
    }

    // --- add-subscription dialog (real ModalDialog + real TextInput) -------

    /// The add-subscription dialog hosts a real [`ui::widget::TextInput`], so
    /// the shared clipboard core works: seed the clipboard, paste with Ctrl+V
    /// through the app's real event path, and the URL lands in the field.
    /// Submitting then runs the real `subscribe` path. This is the proof the
    /// hand-rolled fake field (no clipboard, "Press enter to add") is gone.
    #[test]
    fn add_subscription_modal_pastes_url_and_subscribes() {
        use winit::event::Modifiers;
        use winit::keyboard::ModifiersState;

        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        // A thread-less worker so `subscribe` enqueues without any network.
        let _tx = a.install_test_worker();

        // Open the dialog through the real toolbar action path, then lay it out.
        lay_out(&mut a, 1280.0, 800.0);
        a.open_add_subscription();
        a.layout_modal();
        assert_eq!(
            a.modal.as_ref().map(|m| m.intent),
            Some(ModalIntent::AddSubscription),
            "the Add-subscription dialog is open"
        );
        assert!(
            a.modal
                .as_ref()
                .unwrap()
                .dialog
                .content()
                .value()
                .is_empty(),
            "the URL field starts empty"
        );

        // Seed the clipboard, then paste with Ctrl+V through `handle_event`.
        let url = "https://blog.rust-lang.org/feed.xml";
        ui::text_selection::clipboard_copy(url);
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        assert!(
            dispatch_mods(&mut a, &mut doc, &char_key("v"), ctrl),
            "Ctrl+V is consumed by the dialog"
        );
        assert_eq!(
            a.modal.as_ref().unwrap().dialog.content().value(),
            url,
            "Ctrl+V pasted the clipboard URL into the real TextInput"
        );

        // Enter submits: the real `subscribe` runs and the dialog closes.
        assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::Enter)));
        assert!(a.modal.is_none(), "submitting closes the dialog");
        let toast = a.toast.as_ref().map(|(m, _)| m.clone()).unwrap_or_default();
        assert!(
            toast.contains(url),
            "submit ran the real subscribe path for the pasted URL (toast: {toast:?})"
        );
    }

    /// Esc cancels the dialog without subscribing, and typing then Ctrl+A +
    /// typing edits the field — exercising the shared caret/selection core
    /// rather than a per-app buffer.
    #[test]
    fn add_subscription_modal_esc_cancels_and_typing_edits() {
        use winit::event::Modifiers;
        use winit::keyboard::ModifiersState;

        let mut a = app();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let _tx = a.install_test_worker();
        lay_out(&mut a, 1280.0, 800.0);

        a.open_add_subscription();
        a.layout_modal();
        // Type two characters into the real field.
        let none = Modifiers::from(ModifiersState::empty());
        dispatch_mods(&mut a, &mut doc, &char_key("h"), none);
        dispatch_mods(&mut a, &mut doc, &char_key("i"), none);
        assert_eq!(a.modal.as_ref().unwrap().dialog.content().value(), "hi");
        // Select-all + replace, proving the selection core is live.
        let ctrl = Modifiers::from(ModifiersState::CONTROL);
        dispatch_mods(&mut a, &mut doc, &char_key("a"), ctrl);
        dispatch_mods(&mut a, &mut doc, &char_key("x"), none);
        assert_eq!(
            a.modal.as_ref().unwrap().dialog.content().value(),
            "x",
            "Ctrl+A then a keystroke replaces the whole selection"
        );

        // Esc cancels — no dialog, no subscription toast.
        assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::Escape)));
        assert!(a.modal.is_none(), "Esc closes the dialog");
        assert!(a.toast.is_none(), "cancelling must not run subscribe");
    }
}
