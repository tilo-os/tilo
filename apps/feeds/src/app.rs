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
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{
    FileFilter, LaunchChannel, LaunchOutcome, PickRequest, PortalChannel, PortalOutcome,
};
use ui::presence::PresenceCtx;
use ui::richtext::{FontFamily, RichSpan, RichTextLayout, TextAlign};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::toast::{ToastAnchor, ToastCard, ToastKind, paint_transient};
use ui::widget::{
    Button, ButtonFace, FlapState, ModalDialog, SegmentedFace, SplitLayout, ToggleGroup, Widget,
};

use crate::actions::{self, FeedsAction, FeedsResourceCtx};
use crate::doc::FeedsDoc;
use crate::fetch::{FetchRequest, FetchResult, FetchWorker};
use crate::icons;
use crate::list_layout::{ArticleRowMetrics, article_row_layout};
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

fn opml_export_request() -> PickRequest {
    PickRequest::save("Export OPML", "Export", "feeds-subscriptions.opml")
        .thread_name("feeds-portal-export")
        .filter(FileFilter::new("OPML").glob("*.opml"))
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
/// Authored 1× height of a sidebar section header ("LIBRARY", "FEEDS").
const SIDE_SECTION_H: f64 = 24.0;
/// How much shorter a feed row nested under a folder is than the base
/// sidebar row, so the nesting reads as a tighter group.
const SIDE_FEED_ROW_INSET: f64 = 2.0;
const LIST_ROW_H: f64 = 88.0;
/// Height of the refresh-progress strip shown below the list header while a
/// "Refresh all" is in flight (and briefly after, as a summary).
const REFRESH_STRIP_H: f64 = 52.0;

/// At or above this width the three panes fit side by side (docked sidebar +
/// article list + reader). Sized so the sidebar (256) + a usable article list
/// (400) + a reader column (~464) that keeps its byline and external-link
/// affordance separate all fit at once.
const WIDE_BREAKPOINT: f64 = 1120.0;
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
    Tag { key: crate::model::TagKey },
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
    /// Hover text served by [`Widget::tooltip`], never painted statically.
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

/// Hit regions produced alongside the custom reader paint pass.
type ReaderPaint = (
    Vec<(Rect, String)>,
    Vec<(Rect, crate::model::TagKey)>,
    Option<Rect>,
    f64,
);

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

    fn edit_tags(article_id: Id, tags: &[String]) -> Self {
        let body = ModalBody::input(
            "Add a few useful labels, separated by commas.",
            "Article tags",
            &tags.join(", "),
            "design, research, later…",
        );
        Self::new(
            ModalIntent::EditArticleTags { article_id },
            "Edit article tags",
            body,
            "Save",
            520.0,
            236.0,
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

    sidebar_scroll: f64,
    sidebar_content_h: f64,
    sidebar_row_h: f64,
    sidebar_section_h: f64,
    list_row_h: f64,
    /// Measured sizes of a row's trailing column (time label over the star),
    /// taken on paint with the styles the column paints with; the authored
    /// 1× column before the first paint. `build_list_rows` places the star's
    /// hit rect from these so pointer, focus and paint share one centre.
    list_trailing: crate::list_layout::TrailingColumnMetrics,
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
    /// Visible tag chips in the reader. Tags are a discovery affordance: a
    /// click narrows the local list to that canonical tag resource.
    reader_tags: Vec<(Rect, crate::model::TagKey)>,
    reader_open_rect: Option<Rect>,
    /// The sidebar-header affordance for creating an organizing folder.
    new_folder_rect: Rect,

    last_mouse: Point,
    modal: Option<FeedsModal>,
    modal_structure_dirty: bool,
    toast: Option<(String, Instant)>,

    // --- OPML import (XDG portal) ---------------------------------------
    /// In-flight import file-chooser: the portal worker's receiver, drained
    /// once per frame in `sync`. `None` when no picker is open.
    import_rx: Option<PortalChannel>,
    /// Save chooser and the subscription snapshot captured when export began.
    export_rx: Option<(PortalChannel, String)>,
    /// In-flight external link hand-off, drained once per frame.
    open_link_rx: Option<LaunchChannel>,
    background_poll_deadline: Cell<Option<Instant>>,

    // --- per-feed context menu ------------------------------------------
    /// Right-click menu over a sidebar feed. Uses the shared `ui` primitive so
    /// the popover matches the rest of the OS; the app owns open/close + action
    /// routing (via `take_selection`) since actions mutate the CRDT store.
    feed_menu: ContextMenu,
    /// The feed the open context menu targets.
    feed_menu_target: Option<Id>,
    /// Menu actions aligned by index with `feed_menu`'s action rows.
    feed_menu_actions: Vec<FeedMenuAction>,
    /// Pointer help for the topmost icon-only control. The application is
    /// custom-painted, so it publishes the same labels used by its semantic
    /// tree through the toolkit's shared tooltip host.
    hover_tooltip: Option<String>,
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
    tags_btn_rect: Rect,
    settings_hits: Vec<(Rect, SettingsAction)>,

    // --- refresh progress + non-blocking drain --------------------------
    /// Live refresh progress shown in the list header.
    refresh: RefreshState,
    resource_refresh_invocation: Option<tilo_actions::InvocationId>,
    resource_refresh_result: Option<tilo_actions::InvocationUpdate>,
    /// CRDT writes produced by the doc-less `draw`-time drain, flushed on the
    /// next pass that holds the document.
    pending_doc_writes: Vec<DocWrite>,
    /// Canonical mutations implied by a brokered View operation. Resource
    /// handlers receive an immutable document, so opening an unread article
    /// queues the same `MarkRead` action used by a direct list selection for the
    /// next widget sync instead of writing around the action host.
    pending_resource_actions: Vec<actions::FeedsAction>,

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
    actions: crate::resource::ActionSink,
    /// The document heads the mirrored [`Store`] was last reconciled against.
    /// When the document moves out-of-band (a Ctrl+K palette action mutates it
    /// directly, not through this widget's methods) and the fetch backlog is
    /// drained, `sync` reloads the store from the document so the change shows.
    store_heads: Vec<automerge::ChangeHash>,
    /// When true this is the hermetic `--shots`/demo build: it seeds the
    /// bundled sample subscriptions. The live app leaves this false and never
    /// fabricates feeds — it starts empty on the user's persisted document.
    demo: bool,
    /// OS accessibility text scale. Geometry follows this as well as glyph
    /// painting: treating it as paint-only made 200% text overlap fixed-height
    /// list rows and headers.
    text_scale: f64,
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
            sidebar_scroll: 0.0,
            sidebar_content_h: 0.0,
            sidebar_row_h: SIDE_ROW_H,
            sidebar_section_h: 24.0,
            list_row_h: LIST_ROW_H,
            list_trailing: crate::list_layout::TrailingColumnMetrics::ONE_X,
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
            reader_tags: Vec::new(),
            reader_open_rect: None,
            new_folder_rect: Rect::ZERO,
            last_mouse: Point::ZERO,
            modal: None,
            modal_structure_dirty: false,
            toast: None,
            import_rx: None,
            export_rx: None,
            open_link_rx: None,
            background_poll_deadline: Cell::new(None),
            feed_menu: ContextMenu::new(),
            feed_menu_target: None,
            feed_menu_actions: Vec::new(),
            hover_tooltip: None,
            stage_menu_shot: false,
            settings: ReadSettings::default(),
            show_reader_settings: false,
            settings_btn_rect: Rect::ZERO,
            tags_btn_rect: Rect::ZERO,
            settings_hits: Vec::new(),
            refresh: RefreshState::default(),
            resource_refresh_invocation: None,
            resource_refresh_result: None,
            pending_doc_writes: Vec::new(),
            pending_resource_actions: Vec::new(),
            sidebar: FlapState::open_when_docked(),
            detail_pushed: false,
            settled_once: false,
            hamburger_rect: Rect::ZERO,
            scrim_rect: Rect::ZERO,
            back_rect: Rect::ZERO,
            last_frame: ui::time::now(),
            worker: None,
            synced: false,
            actions: crate::resource::ActionSink::default(),
            store_heads: Vec::new(),
            demo,
            text_scale: 1.0,
            now: chrono::Utc::now().timestamp(),
        };
        if demo {
            app.select_featured();
        }
        app
    }

    /// The responsive tier resolved from the current allocated width.
    fn tier(&self) -> Tier {
        // A 1600px window at 200% text has the usable information density of
        // an 800px window. Resolve pane breakpoints in those logical pixels so
        // columns never become narrow text traps.
        Tier::from_width(self.rect.width() / self.text_scale.max(1.0))
    }

    fn header_h(&self) -> f64 {
        HEADER_H * self.text_scale
    }

    #[cfg(test)]
    fn list_row_h(&self) -> f64 {
        LIST_ROW_H * self.text_scale
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

    /// Stage the selected article's real tag editor for screenshot review.
    pub fn stage_tag_editor(&mut self) {
        self.open_tag_editor();
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
        self.toast = Some((msg.into(), ui::time::now()));
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
            Filter::Tag(key) => self
                .store
                .tags()
                .into_iter()
                .find(|tag| crate::model::tag_key(tag) == key)
                .map_or_else(|| "Tag".into(), |tag| format!("#{tag}")),
        }
    }

    /// Add a folder through the same replayable action exported to Ctrl+K.
    /// The dialog itself is local; only the accepted name and pre-minted id
    /// become collaborative document state.
    fn add_folder(&mut self, name: &str, doc: &mut AutoCommit) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let order = self
            .store
            .folders
            .iter()
            .map(|folder| folder.order)
            .max()
            .map_or(0, |last| last.saturating_add(1));
        let folder = crate::model::Folder {
            id: Uuid::new_v4(),
            name: name.to_string(),
            order,
            collapsed: false,
        };
        self.store.upsert_folder(folder.clone());
        self.perform_action(
            doc,
            &actions::add_folder_action(folder.id, &folder.name, folder.order),
        );
        self.set_filter(Filter::Folder(folder.id));
        self.set_toast(format!("Created folder {}", folder.name));
    }

    fn open_add_folder(&mut self) {
        let body = ModalBody::input(
            "Group subscriptions for a calmer reading list.",
            "Folder name",
            "",
            "Work, Newsletters, Research…",
        );
        self.modal = Some(FeedsModal::new(
            ModalIntent::AddFolder,
            "New folder",
            body,
            "Create",
            520.0,
            236.0,
        ));
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
            let row_top = pos as f64 * self.list_row_h;
            let row_bot = row_top + self.list_row_h;
            let view_h = (self.list_rect.height() - self.header_h()).max(0.0);
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

    fn open_tag_editor(&mut self) {
        let Some(article) = self.selected.and_then(|id| self.store.article(id)) else {
            return;
        };
        self.show_reader_settings = false;
        self.modal = Some(FeedsModal::edit_tags(article.id, &article.tags));
        self.modal_structure_dirty = true;
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
    /// the Ctrl+K palette. The action is applied through the Tilo action executor
    /// (a pinned actor + committed change, so it replays byte-identically on
    /// peers), exactly as the palette does. The document heads captured just
    /// before the first action of the event are stashed so the host records
    /// **one** undo boundary bracketing it ([`CommittedActions::take_committed_action`](ui::undo::CommittedActions::take_committed_action)).
    fn perform_action(&mut self, doc: &mut AutoCommit, action: &FeedsAction) {
        self.actions.dispatch_with_context(
            doc,
            actions::FEEDS_ACTION_TYPE,
            action,
            tilo_actions::ActionContext::for_action("so.tilo.feeds", "Feeds", action),
        );
        // The action just mutated the document; keep the reload watermark ahead
        // of it so `sync`'s out-of-band reload doesn't redundantly rebuild the
        // store for a change this widget already mirrored.
        self.store_heads = doc.get_heads();
    }

    /// The presentation context Ctrl+K needs to build the reader/folder/feed
    /// actions, derived from the mirrored store (which converges with the doc).
    pub(crate) fn feeds_resource_ctx(&self) -> FeedsResourceCtx {
        self.feeds_resource_ctx_with_limits(
            actions::MAX_CONTEXT_FOLDER_RESOURCES,
            actions::MAX_CONTEXT_FEED_RESOURCES,
            actions::MAX_CONTEXT_ARTICLE_RESOURCES,
            actions::MAX_CONTEXT_TAG_RESOURCES,
        )
    }

    /// Build only the resource context needed for one exact lazy address.
    /// Lookup may scan the store's compact vectors, but allocations are bound
    /// to one folder, tag, feed and/or article rather than the archive size.
    pub(crate) fn feeds_resource_ctx_for(&self, id: &str) -> Option<FeedsResourceCtx> {
        if id == "feeds.reader" || id == "feeds.reader-settings" {
            return Some(self.feeds_resource_ctx());
        }
        let mut ctx = FeedsResourceCtx {
            next_feed_order: self.store.next_feed_order(None),
            next_folder_order: self
                .store
                .folders
                .iter()
                .map(|folder| folder.order)
                .max()
                .map_or(0, |order| order.saturating_add(1)),
            settings: self.settings,
            ..FeedsResourceCtx::default()
        };
        if let Some(raw) = id.strip_prefix("feeds.folder:") {
            let folder_id = raw.parse::<Id>().ok()?;
            let folder = self.store.folder(folder_id)?;
            ctx.folders.push(actions::FolderRef {
                id: folder.id,
                name: folder.name.clone(),
                collapsed: folder.collapsed,
            });
        } else if let Some(raw) = id.strip_prefix("feeds.subscription:") {
            let feed_id = raw.parse::<Id>().ok()?;
            let feed = self.store.feed(feed_id)?;
            ctx.feeds.push(actions::FeedRef {
                id: feed.id,
                title: feed.title.clone(),
                folder: feed.folder,
                unread: self.store.unread_ids_for(Filter::Feed(feed.id)),
                articles: Vec::new(),
            });
        } else if id.starts_with("feeds.tag:") {
            let name = actions::tag_from_resource_id(id)?;
            if !self.store.has_tag(&name) {
                return None;
            }
            ctx.tags.push(actions::TagRef { name });
        } else {
            let article_id = id.strip_prefix("feeds.article:")?.parse::<Id>().ok()?;
            let article = self.store.article(article_id)?;
            let feed = self.store.feed(article.feed_id)?;
            ctx.feeds.push(actions::FeedRef {
                id: feed.id,
                title: feed.title.clone(),
                folder: feed.folder,
                unread: Vec::new(),
                articles: vec![actions::ArticleRef {
                    id: article.id,
                    title: article.title.clone(),
                    read: article.read,
                    starred: article.starred,
                    tags: article.tags.clone(),
                }],
            });
        }
        Some(ctx)
    }

    fn feeds_resource_ctx_with_limits(
        &self,
        folder_limit: usize,
        feed_limit: usize,
        article_limit: usize,
        tag_limit: usize,
    ) -> FeedsResourceCtx {
        actions::resource_ctx_from_library_with_limits(
            &self.store,
            self.settings,
            folder_limit,
            feed_limit,
            article_limit,
            tag_limit,
        )
    }

    fn toggle_folder(&mut self, id: Id, doc: &mut AutoCommit) {
        if let Some(f) = self.store.folders.iter_mut().find(|f| f.id == id) {
            f.collapsed = !f.collapsed;
            let collapsed = f.collapsed;
            self.perform_action(doc, &actions::set_folder_collapsed_action(id, collapsed));
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
        self.perform_action(doc, &actions::set_read_settings_action(s));
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

    fn refresh_hover_tooltip(&mut self) {
        let p = self.last_mouse;
        self.hover_tooltip = if self.modal.is_some() || self.feed_menu.is_open() {
            None
        } else if self.hamburger_rect.area() > 0.0 && self.hamburger_rect.contains(p) {
            Some(
                if self.sidebar.is_open(self.nav_layout()) {
                    "Hide subscriptions (Esc)"
                } else {
                    "Show subscriptions"
                }
                .into(),
            )
        } else if self.back_rect.area() > 0.0 && self.back_rect.contains(p) {
            Some("Back to articles (Esc)".into())
        } else if self.settings_btn_rect.area() > 0.0 && self.settings_btn_rect.contains(p) {
            Some("Reading settings".into())
        } else if self.tags_btn_rect.area() > 0.0 && self.tags_btn_rect.contains(p) {
            Some("Edit article tags (T)".into())
        } else if self.new_folder_rect.area() > 0.0 && self.new_folder_rect.contains(p) {
            Some("New folder".into())
        } else if let Some((_, key)) = self.reader_tags.iter().find(|(rect, _)| rect.contains(p)) {
            self.store
                .tags()
                .into_iter()
                .find(|tag| crate::model::tag_key(tag) == *key)
                .map(|tag| format!("Show articles tagged #{tag}"))
        } else if let Some(button) = self
            .toolbar_buttons
            .iter()
            .find(|button| button.rect.contains(p))
        {
            Some(button.tip.into())
        } else if let Some(row) = self.list_rows.iter().find(|row| row.star_rect.contains(p)) {
            self.store.article(row.id).map(|article| {
                if article.starred {
                    "Unstar article"
                } else {
                    "Star article"
                }
                .into()
            })
        } else if self.reader_open_rect.is_some_and(|rect| rect.contains(p)) {
            Some("Open original article".into())
        } else if self.reader_links.iter().any(|(rect, _)| rect.contains(p)) {
            Some("Open link".into())
        } else {
            None
        };
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
        if self.import_rx.is_some() || self.export_rx.is_some() {
            return;
        }
        let xml = opml::export(&self.store);
        self.export_rx = Some((opml_export_request().send(), xml));
        self.set_toast("Choose where to save subscriptions…");
    }

    fn drain_export(&mut self) {
        let Some((rx, _)) = &self.export_rx else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                PortalOutcome::Failed("file chooser closed without a result".into())
            }
        };
        let Some((_, xml)) = self.export_rx.take() else {
            return;
        };
        match outcome {
            PortalOutcome::Opened(path) => {
                match tilo_files::file_at(&path).and_then(|(dir, name)| {
                    tilo_files::blocking::save_document(&dir, &name, xml.as_bytes())
                }) {
                    Ok(()) => self.set_toast(format!("Exported OPML → {}", path.display())),
                    Err(error) => self.set_toast(format!("Export failed: {error}")),
                }
            }
            PortalOutcome::Cancelled => self.set_toast("Export cancelled"),
            PortalOutcome::Failed(reason) => self.set_toast(format!("Export failed: {reason}")),
        }
    }

    // --- OPML import (XDG portal) ----------------------------------------

    /// Ask the host desktop portal to pick an `.opml` file. Non-blocking — the
    /// D-Bus round-trip + host dialog run on a worker thread; the chosen path
    /// arrives over a channel drained by [`Self::drain_import`]. An in-flight
    /// request is not re-opened.
    fn request_import(&mut self) {
        if self.import_rx.is_some() || self.export_rx.is_some() {
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
            PortalOutcome::Opened(path) => match tilo_files::file_at(&path)
                .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
                .and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| tilo_files::FsError::Io("the file is not UTF-8 text".into()))
                }) {
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
            if self.sidebar_rect.contains(p)
                && p.y >= self.sidebar_rect.y0 + HEADER_H
                && row.rect.contains(p)
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
        menu.open_anchored(Rect::new(at.x, at.y, at.x, at.y), self.rect);
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
                self.modal_structure_dirty = true;
            }
            // Unsubscribe is destructive, so confirm it through the shared modal
            // rather than removing the feed on the spot.
            FeedMenuAction::Unsubscribe => self.open_unsubscribe_confirm(feed_id),
        }
    }

    /// Open the "Add subscription" dialog (real modal + real URL field).
    fn open_add_subscription(&mut self) {
        self.modal = Some(FeedsModal::add_subscription());
        self.modal_structure_dirty = true;
    }

    /// Open the unsubscribe confirmation for `feed_id`.
    fn open_unsubscribe_confirm(&mut self, feed_id: Id) {
        let title = self
            .store
            .feed(feed_id)
            .map(|f| f.title.clone())
            .unwrap_or_default();
        self.modal = Some(FeedsModal::unsubscribe(feed_id, &title));
        self.modal_structure_dirty = true;
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
        {
            let Some(m) = self.modal.as_mut() else { return };
            *handled = m
                .dialog
                .handle_detached(event, scale_factor, modifiers, state, path);
        }
        self.drain_modal_actions(state);
        *handled = true;
    }

    fn drain_modal_actions(&mut self, state: &mut AutoCommit) {
        let Some(modal) = self.modal.as_ref() else {
            return;
        };
        let (submit, cancel) = (modal.submit.replace(false), modal.cancel.replace(false));
        if submit {
            self.submit_modal(state);
        } else if cancel {
            self.modal = None;
            self.modal_structure_dirty = true;
        }
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
        self.modal_structure_dirty = true;
        match intent {
            ModalIntent::AddSubscription => self.subscribe(&value, doc),
            ModalIntent::AddFolder => self.add_folder(&value, doc),
            ModalIntent::RenameFeed { feed_id } => self.rename_feed(feed_id, &value, doc),
            ModalIntent::Unsubscribe { feed_id } => self.unsubscribe_feed(feed_id, doc),
            ModalIntent::EditArticleTags { article_id } => {
                let tags = actions::normalize_tags(value.split(','));
                if self.store.set_tags(article_id, tags.clone()) {
                    self.perform_action(doc, &actions::set_article_tags_action(article_id, &tags));
                    self.set_toast(if tags.is_empty() {
                        "Removed article tags".to_string()
                    } else {
                        format!(
                            "Saved {} tag{}",
                            tags.len(),
                            if tags.len() == 1 { "" } else { "s" }
                        )
                    });
                }
            }
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
            self.refresh.finished_at = Some(ui::time::now());
            if let Some(invocation) = self.resource_refresh_invocation.take() {
                self.resource_refresh_result = Some(if self.refresh.failed_feeds > 0 {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "feeds.refresh_incomplete".into(),
                            message: format!(
                                "{} of {} subscriptions failed to refresh",
                                self.refresh.failed_feeds, self.refresh.total
                            ),
                            details: None,
                            retryable: true,
                        },
                    }
                } else {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    }
                });
            }
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
        let mut articles = Vec::new();
        let mut feeds = Vec::new();
        for write in self.pending_doc_writes.drain(..n) {
            match write {
                DocWrite::Article(article) => articles.push(article),
                DocWrite::Feed(feed) => feeds.push(feed),
            }
        }
        // One document action for this frame's budget. The dispatcher commits
        // it, so a large refresh still persists in bounded steps.
        self.perform_action(doc, &FeedsAction::UpsertFetched { articles, feeds });
    }

    /// Show the library the framework resource already holds.
    /// An empty projection does not wipe feeds that have not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::resource::FeedsState) {
        if state.store.is_empty() {
            return;
        }
        self.store.clone_from(&state.store);
        self.settings = state.settings;
        self.ensure_selection();
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Feeds>,
    ) {
        self.actions.attach(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    pub(crate) fn published_reader(&self) -> Vec<tilo_actions::Resource> {
        crate::resource::provide_resources(&crate::resource::FeedsState {
            store: self.store.clone(),
            settings: self.settings,
        })
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    // --- geometry --------------------------------------------------------

    fn recompute_geometry(&mut self) {
        let r = self.rect;
        if self.feed_menu.is_open() {
            self.feed_menu.constrain_to(r);
        }
        let reveal = self.sidebar.reveal();
        // Sidebar can't be wider than the window (very narrow phones).
        let sw = (SIDEBAR_W * self.text_scale).min(r.width());

        match self.tier() {
            Tier::Wide => {
                // The subscriptions sidebar is docked beside the content, which
                // is inset by the eased width so it reveals/collapses smoothly.
                // The list + reader sit side by side in the content.
                let inset = sw * reveal;
                self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + inset, r.y1);
                let content_x0 = r.x0 + inset;
                let list_w = (LIST_W * self.text_scale).min((r.x1 - content_x0).max(0.0));
                self.list_rect = Rect::new(content_x0, r.y0, content_x0 + list_w, r.y1);
                self.reader_rect = Rect::new(content_x0 + list_w, r.y0, r.x1, r.y1);
                self.toolbar_rect = Rect::new(
                    self.list_rect.x0,
                    self.list_rect.y0,
                    self.list_rect.x1,
                    self.list_rect.y0 + self.header_h(),
                );
                self.scrim_rect = Rect::ZERO;
                // The hamburger only appears while the docked rail is collapsing
                // / collapsed, so a hidden rail can always be summoned back.
                self.hamburger_rect = if reveal < 0.999 {
                    self.toggle_button_rect(self.toolbar_rect)
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
                    self.toggle_button_rect(self.toolbar_rect)
                };
            }
        }

        self.build_sidebar_rows();
        self.build_list_rows();
        self.build_toolbar();
        self.build_back_affordance();
        self.new_folder_rect = if self.sidebar_rect.width() >= 64.0 * self.text_scale {
            Rect::new(
                self.sidebar_rect.x1 - 12.0 - 36.0 * self.text_scale,
                self.sidebar_rect.y0 + 10.0 * self.text_scale,
                self.sidebar_rect.x1 - 12.0 * self.text_scale,
                self.sidebar_rect.y0 + 46.0 * self.text_scale,
            )
        } else {
            Rect::ZERO
        };
    }

    /// Lay out the single content pane used at the middle + narrow tiers: the
    /// toolbar spans the pane, and the list + reader are a master→detail push —
    /// one fills the pane while the other is parked off to the side (so a
    /// selection can slide the reader in and Back can slide it back out).
    fn place_single_content(&mut self, content: Rect) {
        self.toolbar_rect = Rect::new(
            content.x0,
            content.y0,
            content.x1,
            content.y0 + self.header_h(),
        );
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
    fn toggle_button_rect(&self, toolbar: Rect) -> Rect {
        let size = 36.0;
        let x = toolbar.x0 + 10.0;
        let cy = toolbar.y0 + self.header_h() / 2.0;
        Rect::new(x, cy - size / 2.0, x + size, cy + size / 2.0)
    }

    /// In a single-pane tier (middle + narrow), a Back chevron in the reader's
    /// header returns to the list. Empty otherwise.
    fn build_back_affordance(&mut self) {
        self.back_rect = if self.single_pane() && self.detail_pushed {
            let size = 36.0;
            let x = self.reader_rect.x0 + 10.0;
            let cy = self.reader_rect.y0 + self.header_h() / 2.0;
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
        let mut y = self.sidebar_rect.y0 + self.header_h() + 8.0 * self.text_scale;

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
            self.sidebar_section_h,
            14.0,
            "LIBRARY".into(),
            0,
            false,
            RowRole::Section,
        );
        push(
            &mut rows,
            &mut y,
            self.sidebar_row_h,
            14.0,
            "All Articles".into(),
            self.store.total_unread(),
            matches!(self.filter, Filter::All),
            RowRole::Smart(SmartKind::All),
        );
        push(
            &mut rows,
            &mut y,
            self.sidebar_row_h,
            14.0,
            "Unread".into(),
            self.store.total_unread(),
            matches!(self.filter, Filter::Unread),
            RowRole::Smart(SmartKind::Unread),
        );
        push(
            &mut rows,
            &mut y,
            self.sidebar_row_h,
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
            self.sidebar_section_h,
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
                self.sidebar_row_h,
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
                        self.sidebar_row_h - SIDE_FEED_ROW_INSET,
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
                    self.sidebar_row_h - SIDE_FEED_ROW_INSET,
                    18.0,
                    title,
                    self.store.unread_for_feed(id),
                    selected,
                    RowRole::Feed { id, kind },
                );
            }
        }

        let tags = self.store.tags_limited(actions::MAX_CONTEXT_TAG_RESOURCES);
        if !tags.is_empty() {
            y += 10.0;
            push(
                &mut rows,
                &mut y,
                self.sidebar_section_h,
                14.0,
                "TAGS".into(),
                0,
                false,
                RowRole::Section,
            );
            for tag in tags {
                let key = crate::model::tag_key(&tag);
                push(
                    &mut rows,
                    &mut y,
                    self.sidebar_row_h - 2.0,
                    18.0,
                    format!("#{tag}"),
                    self.store.count_for(Filter::Tag(key)),
                    matches!(self.filter, Filter::Tag(active) if active == key),
                    RowRole::Tag { key },
                );
            }
        }

        self.sidebar_content_h = y - self.sidebar_rect.y0 - HEADER_H;
        let visible = (self.sidebar_rect.height() - HEADER_H).max(0.0);
        self.sidebar_scroll = self
            .sidebar_scroll
            .clamp(0.0, (self.sidebar_content_h - visible).max(0.0));
        for row in &mut rows {
            row.rect = row.rect - vello::kurbo::Vec2::new(0.0, self.sidebar_scroll);
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
        let content_top = self.list_rect.y0 + self.header_h() + self.refresh_strip_h();
        let x0 = self.list_rect.x0;
        let x1 = self.list_rect.x1;
        let articles: Vec<Id> = self
            .store
            .articles_for(self.filter)
            .iter()
            .map(|a| a.id)
            .collect();
        self.list_content_h = articles.len() as f64 * self.list_row_h;
        let viewport_h = (self.list_rect.y1 - content_top).max(0.0);
        self.list_scroll = self
            .list_scroll
            .clamp(0.0, (self.list_content_h - viewport_h).max(0.0));
        for (i, id) in articles.iter().enumerate() {
            let top = content_top + i as f64 * self.list_row_h - self.list_scroll;
            let rect = Rect::new(x0, top, x1, top + self.list_row_h);
            // One rect for the star's paint, pointer, and accessibility bounds:
            // centred on the glyph's place in the trailing column (time label
            // over star; see `list_layout`).
            let (_, star_cy) = crate::list_layout::trailing_column(rect, &self.list_trailing);
            let star_rect = crate::list_layout::star_rect(
                rect,
                crate::list_layout::STAR_HIT_SIZE,
                crate::list_layout::STAR_RIGHT_PAD,
                star_cy,
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
        if self.single_pane() && self.detail_pushed {
            self.toolbar_buttons.clear();
            return;
        }
        let mut buttons = Vec::new();
        let cy = self.toolbar_rect.y0 + self.header_h() / 2.0;
        let mut x = self.toolbar_rect.x1 - 28.0;
        let specs = [
            (ToolbarAction::AddSubscription, "Add subscription (N)"),
            (ToolbarAction::Refresh, "Refresh all (R)"),
            (ToolbarAction::MarkAllRead, "Mark all read (A)"),
            (ToolbarAction::ImportOpml, "Import OPML (I)"),
            (ToolbarAction::ExportOpml, "Export OPML (E)"),
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
        // Use the renderer's actual scaled line boxes for surrounding geometry:
        // every row is its measured line plus a token pad above and below,
        // never shorter than the authored 1× row, so 200% text grows the
        // lists instead of stacking rows over each other.
        let sp = &theme.tokens.spacing;
        // Feed rows nested under a folder are `SIDE_FEED_ROW_INSET` shorter
        // than the base row, so the base row reserves that inset on top of
        // the `s1` pad each side — every row, nested or not, then holds its
        // measured line plus at least `s2` of air.
        let row_pad = 2.0 * f64::from(sp.s1) + SIDE_FEED_ROW_INSET;
        self.sidebar_row_h = SIDE_ROW_H.max(
            f64::from(
                tr.measure_weighted("Ag", 14.0, None, TextWeight::Semibold)
                    .1,
            ) + row_pad,
        );
        self.sidebar_section_h = SIDE_SECTION_H.max(
            f64::from(
                tr.measure_weighted("Ag", 10.5, None, TextWeight::Semibold)
                    .1,
            ) + 2.0 * f64::from(sp.s1),
        );
        let title_line = f64::from(
            tr.measure_weighted("Ag", 15.0, None, TextWeight::Semibold)
                .1,
        );
        let meta_line = f64::from(tr.measure("Ag", 12.5, None).1);
        // The row reserves `s3` above and below and an `s2` gap between the
        // two title lines and the meta line; `draw_list` then groups the
        // block with the tighter `s1` gap inside that room (HIG §7a), so the
        // block never touches the row's edges at any text scale.
        self.list_row_h = crate::list_layout::row_height(
            title_line,
            meta_line,
            f64::from(sp.s2),
            f64::from(sp.s3),
            LIST_ROW_H,
        );
        // The trailing column (time label over the star): the 12 px label's
        // line box (`text_metrics::line_height_px`, what `measure` reports
        // for one line) and the glyph's box, so the star's hit rect lands
        // where the glyph does.
        self.list_trailing = crate::list_layout::TrailingColumnMetrics {
            time_h: ui::text_metrics::line_height_px(12.0, theme.text_scale),
            star_box: f64::from(theme.tokens.metrics.icon_sm),
            gap: f64::from(sp.s1),
        };
        self.text_scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        // A staged/live settings popover means an article is already in focus;
        // preserve that intent when larger text changes wide columns into a
        // single-pane layout.
        if self.show_reader_settings && self.single_pane() && self.selected.is_some() {
            self.detail_pushed = true;
        }

        // Advance the sidebar flap transition with real elapsed time, then
        // recompute the eased geometry so the slide is reflected this frame.
        let now = ui::time::now();
        let dt = (now - self.last_frame).as_secs_f64().min(0.1);
        self.last_frame = now;
        let nav_layout = self.nav_layout();
        self.sidebar.reconcile(nav_layout);
        if theme.reduce_motion {
            self.sidebar.snap(self.sidebar.is_open(nav_layout));
        } else {
            self.sidebar.tick(nav_layout, dt);
        }
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
        let header_h = self.header_h();

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
        let list_title = self.filter_title();
        draw_list(
            PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            &ListView {
                rect: &self.list_rect,
                rows: &self.list_rows,
                store: &self.store,
                selected: self.selected,
                now: self.now,
                title: &list_title,
                count: self.store.count_for(self.filter),
                header_inset: list_inset,
                header_text_end: self
                    .toolbar_buttons
                    .iter()
                    .filter(|button| button.rect.intersect(self.list_rect).area() > 0.0)
                    .map(|button| button.rect.x0 - 10.0)
                    .fold(self.list_rect.x1 - 18.0, f64::min),
                refresh: &self.refresh,
                header_h,
            },
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
        let article = self.selected.and_then(|id| self.store.article(id));
        let feed = article.and_then(|article| self.store.feed(article.feed_id));
        let (links, tags, open_rect, content_h) = draw_reader(
            PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            &ReaderView {
                rect: &self.reader_rect,
                article,
                feed,
                scroll: self.reader_scroll,
                now: self.now,
                header_inset: reader_inset,
                settings: self.settings,
                palette,
                family: self.reader_family(),
                header_h,
            },
        );
        self.reader_links = links;
        self.reader_tags = tags;
        self.reader_open_rect = open_rect;
        self.reader_content_h = content_h;

        // The list/reader divider only exists in the wide three-pane layout;
        // the middle + narrow tiers show a single content pane at a time. It
        // is derived against the base both panes sit on (HIG §9.5).
        let divider = ui::surface::edge_on(c.surfaces.base);
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
            ui::chrome::fill_scrim(
                scene,
                t,
                self.scrim_rect,
                (self.sidebar.reveal() * 0.5) as f32,
                theme,
            );
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
            draw_sidebar(
                scene,
                t,
                tr,
                theme,
                &self.sidebar_rect,
                &self.sidebar_rows,
                header_h,
            );
            draw_new_folder_button(scene, t, theme, &self.new_folder_rect, self.last_mouse);
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
        self.refresh_hover_tooltip();
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
            self.tags_btn_rect = Rect::ZERO;
            self.show_reader_settings = false;
            return;
        }
        let c = &theme.tokens.colors;
        let scale = self.text_scale;

        // Gear button: top-right of the reader pane header, left of nothing
        // (the shared toolbar actions live over the list/content toolbar, so
        // the reader header's right edge is free).
        let size = 36.0 * scale;
        let bx1 = self.reader_rect.x1 - 12.0 * scale;
        let cy = self.reader_rect.y0 + self.header_h() / 2.0;
        let btn = Rect::new(bx1 - size, cy - size / 2.0, bx1, cy + size / 2.0);
        self.settings_btn_rect = btn;

        // Siblings in the header: `s2` apart, like the toolbar's buttons.
        let sp = &theme.tokens.spacing;
        let radius = f64::from(theme.tokens.radii.md);
        let sibling_gap = f64::from(sp.s2);
        let tag_btn = Rect::new(
            btn.x0 - size - sibling_gap,
            btn.y0,
            btn.x0 - sibling_gap,
            btn.y1,
        );
        self.tags_btn_rect = tag_btn;
        // Both chrome buttons are the shared button face (HIG §8): the tags
        // button carries its `#` as the label; the gear paints the face with
        // no label and draws its own glyph in the face's ink, Primary while
        // the popover it opens is showing.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(tag_btn, "#")
                .text_size(16.0)
                .weight(TextWeight::Semibold)
                .radius(radius)
                .hovered(tag_btn.contains(self.last_mouse)),
        );

        let hovered = btn.contains(self.last_mouse);
        let gear_face = ButtonFace::new(btn, "").radius(radius).hovered(hovered);
        let gear_face = if self.show_reader_settings {
            gear_face.primary()
        } else {
            gear_face
        };
        Button::paint(scene, t, tr, theme, &gear_face);
        icons::gear(scene, t, btn.center(), 6.5, gear_face.colors(theme).2);

        if !self.show_reader_settings {
            return;
        }

        // --- popover ----------------------------------------------------
        // Right-align under the gear, but clamp inside the reader pane so the
        // controls never spill off the edge on a narrow reader column.
        let ph = 196.0 * scale;
        let pw = (268.0 * scale)
            .min(self.reader_rect.width() - 16.0 * scale)
            .max(180.0 * scale);
        let px1 = btn.x1.min(self.reader_rect.x1 - 8.0 * scale);
        let px0 = (px1 - pw).max(self.reader_rect.x0 + 8.0 * scale);
        let py0 = btn.y1 + 8.0 * scale;
        let card = Rect::new(px0, py0, px0 + pw, py0 + ph);
        // A floating Panel: derived panel border, crisp on the outermost
        // pixel row (HIG §9.5).
        let card_radius = f64::from(theme.tokens.radii.lg);
        scene.fill(
            Fill::NonZero,
            t,
            c.surfaces.raised,
            None,
            &RoundedRect::from_rect(card, card_radius),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(card, card_radius),
        );

        let lx = card.x0 + 16.0 * scale;
        let rx = card.x1 - 16.0 * scale;
        let mut y = card.y0 + 16.0 * scale;

        tr.draw_text_weighted(
            scene,
            "Reading",
            13.0,
            c.text_primary,
            t * Affine::translate((lx, y)),
            None,
            TextWeight::Bold,
        );
        y += 26.0 * scale;

        // Font size row: −  Aa NNN%  +
        tr.draw_text(
            scene,
            "Text size",
            12.5,
            c.text_dim,
            t * Affine::translate((lx, y + 6.0 * scale)),
            None,
        );
        let bsz = 26.0 * scale;
        let plus = Rect::new(rx - bsz, y, rx, y + bsz);
        let pct = format!("{}%", (self.settings.font_scale * 100.0).round() as i32);
        let pctw = measure_w(tr, &pct, 12.5);
        let minus = Rect::new(
            plus.x0 - 8.0 * scale - pctw - 8.0 * scale - bsz,
            y,
            plus.x0 - 8.0 * scale - pctw - 8.0 * scale,
            y + bsz,
        );
        self.step_button(
            PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            &minus,
            "−",
            SettingsAction::FontDec,
        );
        tr.draw_text(
            scene,
            &pct,
            12.5,
            c.text_primary,
            t * Affine::translate((minus.x1 + 8.0 * scale, y + 5.0 * scale)),
            None,
        );
        self.step_button(
            PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            &plus,
            "+",
            SettingsAction::FontInc,
        );
        y += bsz + 14.0 * scale;

        // Font family segmented control (Sans / Serif / Mono).
        tr.draw_text(
            scene,
            "Font",
            12.5,
            c.text_dim,
            t * Affine::translate((lx, y + 6.0 * scale)),
            None,
        );
        let seg_x0 = lx + 52.0 * scale;
        let seg_w = rx - seg_x0;
        let font_labels: Vec<&str> = ReadFont::ALL.iter().map(|f| f.label()).collect();
        let font_actions: Vec<SettingsAction> = ReadFont::ALL
            .iter()
            .map(|f| SettingsAction::SetFont(*f))
            .collect();
        let font_selected = ReadFont::ALL
            .iter()
            .position(|f| *f == self.settings.font)
            .unwrap_or(0);
        self.seg_row(
            PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            Rect::new(seg_x0, y, seg_x0 + seg_w, y + bsz),
            &font_labels,
            font_selected,
            &font_actions,
        );
        y += bsz + 14.0;

        // Reading theme segmented control (Light / Sepia / Dark).
        tr.draw_text(
            scene,
            "Theme",
            12.5,
            c.text_dim,
            t * Affine::translate((lx, y + 6.0 * scale)),
            None,
        );
        let theme_labels: Vec<&str> = ReadTheme::ALL.iter().map(|th| th.label()).collect();
        let theme_actions: Vec<SettingsAction> = ReadTheme::ALL
            .iter()
            .map(|th| SettingsAction::SetTheme(*th))
            .collect();
        let theme_selected = ReadTheme::ALL
            .iter()
            .position(|th| *th == self.settings.theme)
            .unwrap_or(0);
        self.seg_row(
            PaintCtx {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            Rect::new(seg_x0, y, seg_x0 + seg_w, y + bsz),
            &theme_labels,
            theme_selected,
            &theme_actions,
        );
    }

    /// One segmented row of the settings popover, painted by the shared
    /// `ToggleGroup` face (HIG §8: never hand-roll a control) over rects this
    /// popover owns: equal-width segments an `s2` apart across `band`, each
    /// registered as a settings hit.
    fn seg_row(
        &mut self,
        paint: PaintCtx<'_>,
        band: Rect,
        labels: &[&str],
        selected: usize,
        actions: &[SettingsAction],
    ) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = paint;
        let n = labels.len().max(1) as f64;
        let gap = f64::from(theme.tokens.spacing.s2);
        let cellw = ((band.width() - (n - 1.0) * gap) / n).max(0.0);
        let rects: Vec<Rect> = (0..labels.len())
            .map(|i| {
                let x0 = band.x0 + i as f64 * (cellw + gap);
                Rect::new(x0, band.y0, x0 + cellw, band.y1)
            })
            .collect();
        let hovered = rects.iter().position(|r| r.contains(self.last_mouse));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&rects, labels, selected)
                .pill()
                .hovered(hovered)
                .text_size(12.0),
        );
        for (rect, action) in rects.iter().zip(actions) {
            self.settings_hits.push((*rect, *action));
        }
    }

    /// A small square stepper button ("−"/"+") for the settings popover.
    fn step_button(
        &mut self,
        paint: PaintCtx<'_>,
        rect: &Rect,
        glyph: &str,
        action: SettingsAction,
    ) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = paint;
        let hovered = rect.contains(self.last_mouse);
        // The shared Neutral button face (HIG §8) with the glyph as its
        // label, centred on its measured box by the face painter.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(*rect, glyph)
                .text_size(16.0)
                .weight(TextWeight::Semibold)
                .radius(f64::from(theme.tokens.radii.sm))
                .hovered(hovered),
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
        if self.tags_btn_rect.area() > 0.0 && self.tags_btn_rect.contains(p) {
            self.open_tag_editor();
            return true;
        }
        if self.new_folder_rect.area() > 0.0 && self.new_folder_rect.contains(p) {
            self.open_add_folder();
            return true;
        }
        if let Some((_, key)) = self.reader_tags.iter().find(|(rect, _)| rect.contains(p)) {
            self.set_filter(Filter::Tag(*key));
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
            if !row.rect.contains(p)
                || !self.sidebar_rect.contains(p)
                || p.y < self.sidebar_rect.y0 + HEADER_H
            {
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
                RowRole::Tag { key } => self.set_filter(Filter::Tag(key)),
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
            self.list_rect.y0 + self.header_h(),
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
        let Some(url) = tilo_net::Url::parse(url.trim())
            .ok()
            .filter(|url| match url.scheme() {
                "http" | "https" | "gemini" => url.host_str().is_some(),
                "mailto" => !url.path().is_empty(),
                _ => false,
            })
        else {
            self.set_toast(ui::i18n!("feeds-open-link-invalid"));
            return;
        };
        self.open_link_rx = Some(ui::portal::send_open_uri(url.as_str()));
        self.set_toast(ui::i18n!("feeds-open-link-started"));
    }

    fn drain_open_link(&mut self) {
        let Some(rx) = &self.open_link_rx else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                LaunchOutcome::Failed("the opener stopped without a result".into())
            }
        };
        self.open_link_rx = None;
        if let LaunchOutcome::Failed(error) = outcome {
            self.set_toast(format!("{}: {error}", ui::i18n!("feeds-open-link-failed")));
        }
    }
}

// ---------------------------------------------------------------------
// Drawing free functions (explicit borrows keep the borrow checker happy)
// ---------------------------------------------------------------------

#[derive(Clone, Copy)]
struct ListView<'a> {
    rect: &'a Rect,
    rows: &'a [ListRow],
    store: &'a Store,
    selected: Option<Id>,
    now: i64,
    title: &'a str,
    count: usize,
    header_inset: f64,
    header_text_end: f64,
    refresh: &'a RefreshState,
    header_h: f64,
}

#[derive(Clone, Copy)]
struct ReaderView<'a> {
    rect: &'a Rect,
    article: Option<&'a Article>,
    feed: Option<&'a Feed>,
    scroll: f64,
    now: i64,
    header_inset: f64,
    settings: ReadSettings,
    palette: ReaderPalette,
    family: FontFamily,
    header_h: f64,
}

struct BlockPaintContext<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    pane: &'a Rect,
    links: &'a mut Vec<(Rect, String)>,
    settings: ReadSettings,
    palette: &'a ReaderPalette,
    family: FontFamily,
}

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
fn badge(paint: PaintCtx<'_>, right_x: f64, cy: f64, n: usize, bg: Color, fg: Color) -> f64 {
    let PaintCtx {
        scene,
        transform: t,
        text: tr,
        theme: _,
    } = paint;
    if n == 0 {
        return 0.0;
    }
    let label = if n > 999 {
        "999+".to_string()
    } else {
        n.to_string()
    };
    let (tw, th) = tr.measure(&label, 11.0, None);
    let w = (f64::from(tw) + 14.0).max(20.0);
    let h = 18.0_f64.max(f64::from(th) + 4.0);
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
        t * Affine::translate((
            rect.x0 + (w - f64::from(tw)) / 2.0,
            cy - f64::from(th) / 2.0,
        )),
        None,
    );
    w
}

/// The pill a sidebar row paints its selection as, and the bounds the row
/// reports to assistive technology: inset from the row band so two stacked
/// rows read as neighbours an `s1` apart (HIG §7) rather than one shape. The
/// pointer hit rect stays the full band.
fn sidebar_pill(row: Rect) -> Rect {
    Rect::new(row.x0 + 6.0, row.y0 + 2.0, row.x1 - 6.0, row.y1 - 2.0)
}

#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
/// Compact sidebar-header control for creating a subscription folder. It is
/// intentionally local chrome rather than another custom widget: the durable
/// folder record still travels through `FeedsAction::AddFolder` on submit.
fn draw_new_folder_button(scene: &mut Scene, t: Affine, theme: &Theme, rect: &Rect, mouse: Point) {
    if rect.area() <= 0.0 {
        return;
    }
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(*rect, 8.0);
    scene.fill(
        Fill::NonZero,
        t,
        if rect.contains(mouse) {
            c.neutral[3]
        } else {
            c.neutral[2]
        },
        None,
        &shape,
    );
    scene.stroke(
        &Stroke::new(1.0),
        t,
        alpha(c.border_standard, 130),
        None,
        &shape,
    );
    // Folder tab + body, with a small plus: recognizable at the same density
    // as the rest of the sidebar controls without spending a row on chrome.
    let center = rect.center();
    let mut folder = vello::kurbo::BezPath::new();
    folder.move_to(Point::new(center.x - 9.0, center.y - 5.0));
    folder.line_to(Point::new(center.x - 2.0, center.y - 5.0));
    folder.line_to(Point::new(center.x, center.y - 2.5));
    folder.line_to(Point::new(center.x + 9.0, center.y - 2.5));
    folder.line_to(Point::new(center.x + 9.0, center.y + 6.5));
    folder.line_to(Point::new(center.x - 9.0, center.y + 6.5));
    folder.close_path();
    scene.stroke(&Stroke::new(1.45), t, c.text_primary, None, &folder);
    icons::plus(
        scene,
        t,
        Point::new(center.x + 7.0, center.y + 5.5),
        3.0,
        c.accent[6],
    );
}

#[allow(
    clippy::too_many_lines,
    reason = "the sidebar has one linear rendering pass for its fixed row kinds"
)]
fn draw_sidebar(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    rows: &[SidebarRow],
    header_h: f64,
) {
    let c = &theme.tokens.colors;
    // The sidebar's own fill (see `paint`), used for the label fade-out band.
    let sidebar_bg = c.neutral[0];
    // No app-name wordmark: the OS titlebar shows "Feeds". The header band is
    // kept only to align the sidebar's first section with the content toolbar
    // (and to clear the overlay collapse toggle) — its divider stays.
    let header = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + header_h);
    hline(
        scene,
        t,
        rect.x0,
        rect.x1,
        header.y1,
        ui::surface::edge_on(sidebar_bg),
    );

    let content = Rect::new(rect.x0, rect.y0 + HEADER_H, rect.x1, rect.y1);
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        t,
        &content,
    );
    for row in rows {
        if row.rect.y1 <= content.y0 || row.rect.y0 >= content.y1 {
            continue;
        }
        let cy = row.rect.y0 + row.rect.height() / 2.0;
        if let RowRole::Section = &row.role {
            let line_h = f64::from(
                tr.measure_weighted(&row.label, 10.5, None, TextWeight::Semibold)
                    .1,
            );
            tr.draw_text_weighted(
                scene,
                &row.label,
                10.5,
                c.text_dim,
                t * Affine::translate((row.rect.x0 + row.indent, cy - line_h / 2.0)),
                None,
                TextWeight::Semibold,
            );
            continue;
        }

        // Selection / hover background.
        if row.selected {
            scene.fill(
                Fill::NonZero,
                t,
                alpha(c.accent[5], 46),
                None,
                &RoundedRect::from_rect(sidebar_pill(row.rect), 8.0),
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
            RowRole::Tag { .. } | RowRole::Section => {}
        }

        // Badge (right-aligned), then label clipped to the remaining width.
        let badge_w = if row.badge > 0 {
            let (bg, fg) = if row.selected {
                (c.accent[5], theme.on_surface(c.accent[5]).ink)
            } else {
                (alpha(c.neutral[3], 180), c.text_dim)
            };
            badge(
                PaintCtx {
                    scene,
                    transform: t,
                    text: tr,
                    theme,
                },
                row.rect.x1 - 12.0,
                cy,
                row.badge,
                bg,
                fg,
            )
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
        let baseline = cy - f64::from(tr.measure_weighted(&row.label, size, None, weight).1) / 2.0;
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
    scene.pop_layer();
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

#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
fn draw_list(paint: PaintCtx<'_>, view: &ListView<'_>) {
    let PaintCtx {
        scene,
        transform: t,
        text: tr,
        theme,
    } = paint;
    let ListView {
        rect,
        rows,
        store,
        selected,
        now,
        title,
        count,
        header_inset,
        header_text_end,
        refresh,
        header_h,
    } = *view;
    let c = &theme.tokens.colors;
    // Header. `header_inset` shifts the title right of the hamburger toggle
    // when the list is the narrow master pane.
    let title_x = rect.x0 + 18.0 + header_inset;
    let count_label = format!("{count}");
    let (count_w, count_h) = tr.measure(&count_label, 13.0, None);
    let available = (header_text_end - title_x).max(0.0) as f32;
    let show_count = available >= count_w + 18.0 + 70.0;
    let title_limit = if show_count {
        available - count_w - 18.0
    } else {
        available
    };
    let (full_w, title_h) = tr.measure_weighted(title, 17.0, None, TextWeight::Bold);
    let title_label = if full_w <= title_limit {
        title.to_string()
    } else {
        let ellipsis_w = tr.measure_weighted("…", 17.0, None, TextWeight::Bold).0;
        if title_limit < ellipsis_w {
            String::new()
        } else {
            format!(
                "{}…",
                elide_weighted(tr, title, 17.0, title_limit - ellipsis_w, TextWeight::Bold)
            )
        }
    };
    let title_w = tr
        .measure_weighted(&title_label, 17.0, None, TextWeight::Bold)
        .0;
    let header_center = rect.y0 + header_h / 2.0;
    tr.draw_text_weighted(
        scene,
        &title_label,
        17.0,
        c.text_primary,
        t * Affine::translate((title_x, header_center - f64::from(title_h) / 2.0)),
        None,
        TextWeight::Bold,
    );
    if show_count {
        tr.draw_text(
            scene,
            &count_label,
            13.0,
            c.text_dim,
            t * Affine::translate((
                title_x + f64::from(title_w) + 18.0,
                header_center - f64::from(count_h) / 2.0,
            )),
            None,
        );
    }
    let header_bottom = rect.y0 + header_h;
    hline(
        scene,
        t,
        rect.x0,
        rect.x1,
        header_bottom,
        ui::surface::edge_on(c.surfaces.base),
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
        // The rule between rows is derived against the base and stops an
        // `s3` short of both ends (HIG §9.5): the washed `border_standard`
        // it replaced was a 27 %-alpha ghost on the dark preset.
        ui::surface::separator(
            scene,
            t,
            lr.rect.x0,
            lr.rect.x1,
            lr.rect.y1 - 0.5,
            f64::from(theme.tokens.spacing.s3),
            c.surfaces.base,
        );

        // Title + source line are one related group, centred in the row (HIG
        // §7a); the unread marker sits on the title's first line and the
        // trailing column (time over star) is centred on the row. Everything
        // below is measured with the style it is painted with and placed by
        // `list_layout`.
        let text_x = lr.rect.x0 + 34.0;
        let right_pad = 16.0;
        let time = relative_time(a.published, now);
        let (tw, th) = tr.measure(&time, 12.0, None);
        let tw = f64::from(tw);

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
        let title_text = two_line_elide(tr, &a.title, 15.0, title_w, title_weight);
        let (_, title_h) = tr.measure_weighted(&title_text, 15.0, Some(title_w), title_weight);
        let (_, title_line_h) = tr.measure_weighted("Ag", 15.0, None, title_weight);

        // Source + summary line, stopped before the star so the truncated
        // source·summary never collides with it.
        let source = store.feed(a.feed_id).map_or("", |f| f.title.as_str());
        let meta = if a.summary.is_empty() {
            source.to_string()
        } else {
            format!("{source}  ·  {}", a.summary)
        };
        let meta_w = (lr.star_rect.x0 - 8.0 - text_x).max(60.0) as f32;
        let meta_text = tr.elide_to_width(&meta, 12.5, meta_w);
        let (_, meta_h) = tr.measure(&meta_text, 12.5, None);

        let sp = &theme.tokens.spacing;
        let row = article_row_layout(
            lr.rect,
            &ArticleRowMetrics {
                title_h: f64::from(title_h),
                title_line_h: f64::from(title_line_h),
                meta_h: f64::from(meta_h),
                gap: f64::from(sp.s1),
                pad_min: f64::from(sp.s3),
                trailing: crate::list_layout::TrailingColumnMetrics {
                    time_h: f64::from(th),
                    star_box: f64::from(theme.tokens.metrics.icon_sm),
                    gap: f64::from(sp.s1),
                },
            },
        );

        // Unread / read marker, on the title's first line.
        if a.read {
            icons::ring(
                scene,
                t,
                Point::new(lr.rect.x0 + 18.0, row.marker_cy),
                3.5,
                alpha(c.text_dim, 150),
            );
        } else {
            icons::dot(
                scene,
                t,
                Point::new(lr.rect.x0 + 18.0, row.marker_cy),
                4.0,
                c.accent[5],
            );
        }
        // Time, right-aligned on the title's first line.
        tr.draw_text(
            scene,
            &time,
            12.0,
            c.text_dim,
            t * Affine::translate((lr.rect.x1 - right_pad - tw, row.time_y)),
            None,
        );
        tr.draw_text_weighted(
            scene,
            &title_text,
            15.0,
            title_color,
            t * Affine::translate((text_x, row.title_y)),
            Some(title_w),
            title_weight,
        );
        tr.draw_text(
            scene,
            &meta_text,
            12.5,
            c.text_dim,
            t * Affine::translate((text_x, row.meta_y)),
            Some(meta_w),
        );

        // Star, centred in its (vertically centred) hit rect.
        icons::star(
            scene,
            t,
            Point::new(lr.star_rect.center().x, row.star_cy),
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
        ui::surface::edge_on(c.surfaces.base),
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

#[allow(
    clippy::too_many_lines,
    reason = "a single linear draw pass over this panel's fixed regions; splitting it would \
              only relocate the line count, not the amount a reader must hold in mind"
)]
fn draw_reader(paint: PaintCtx<'_>, view: &ReaderView<'_>) -> ReaderPaint {
    let PaintCtx {
        scene,
        transform: t,
        text: tr,
        theme: _,
    } = paint;
    let ReaderView {
        rect,
        article,
        feed,
        scroll,
        now,
        header_inset,
        settings,
        palette,
        family,
        header_h,
    } = *view;
    let mut links = Vec::new();
    let mut tag_hits = Vec::new();
    let mut open_rect = None;

    // The reader PAGE uses the reading palette (light/sepia/dark paper), which
    // is deliberately independent of the OS theme. Chrome (toolbar/sidebar)
    // still reads from the OS theme.
    let page = Rect::new(rect.x0, rect.y0 + header_h, rect.x1, rect.y1);
    scene.fill(Fill::NonZero, t, palette.page, None, &page);

    let Some(a) = article else {
        tr.draw_text(
            scene,
            "Select an article to read",
            16.0,
            palette.ink_dim,
            t * Affine::translate((rect.x0 + 48.0 + header_inset, rect.y0 + header_h + 36.0)),
            None,
        );
        return (links, tag_hits, open_rect, 0.0);
    };

    let pad = (48.0_f64).max(header_inset);
    let content_x = rect.x0 + pad;
    let content_w = (rect.width() - pad * 2.0).max(120.0) as f32;
    // The shared toolbar bar occupies the top HEADER_H of this pane, so the
    // article body begins beneath it. Clip the scrolling content to the
    // area below the toolbar. Articles are infinite-scroll (no page modes).
    let top = rect.y0 + header_h;
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
        // A chip on the page: its fill plus the edge derived from that fill
        // (HIG §7), so it reads as a chip on every reading palette.
        ui::surface::card(scene, t, tag_rect, 6.0, palette.code_bg);
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
    let title_layout = RichTextLayout::shape_styled(
        &mut tr.font_system,
        &[RichSpan::new(a.title.clone()).bold(true)],
        &RichTextLayout::block_style(true, title_size, family, 1.0),
        Some(content_w),
        TextAlign::Left,
        vec![],
        vec![],
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
    let byline_w = measure_w(tr, &byline, 13.0);
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
        // A compact reader cannot fit author/date and the trailing action on
        // one line. Give the action its own line instead of painting the two
        // strings through each other; the full-width reader keeps the tighter
        // single-line treatment.
        if !byline.is_empty() && byline_w + lw + 42.0 > f64::from(content_w) {
            y += 22.0;
        }
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
    if !a.tags.is_empty() {
        let right = rect.x1 - pad;
        let mut chip_x = content_x;
        let mut chip_y = y;
        for tag in a.tags.iter().take(8) {
            let label = format!("#{tag}");
            let chip_w = measure_w(tr, &label, 11.5) + 18.0;
            if chip_x > content_x && chip_x + chip_w > right {
                chip_x = content_x;
                chip_y += 28.0;
            }
            let chip = Rect::new(chip_x, chip_y - 3.0, chip_x + chip_w, chip_y + 20.0);
            ui::surface::card(scene, t, chip, 7.0, palette.code_bg);
            tr.draw_text_weighted(
                scene,
                &label,
                11.5,
                palette.accent,
                t * Affine::translate((chip_x + 9.0, chip_y + 2.0)),
                None,
                TextWeight::Semibold,
            );
            tag_hits.push((chip, crate::model::tag_key(tag)));
            chip_x += chip_w + 7.0;
        }
        y = chip_y + 29.0;
    }
    hline(scene, t, content_x, rect.x1 - pad, y, palette.rule);
    y += 22.0;

    // Body blocks — rendered with the reader's chosen family, size, and palette.
    for block in &a.blocks {
        y = draw_block(
            BlockPaintContext {
                scene,
                transform: t,
                text: tr,
                pane: rect,
                links: &mut links,
                settings,
                palette: &palette,
                family,
            },
            block,
            content_x,
            content_w,
            y,
        );
    }
    y += 40.0;
    scene.pop_layer();

    let content_h = (y + scroll) - content_top;
    (links, tag_hits, open_rect, content_h)
}

/// Render one body block through `RichTextLayout` so the reader's chosen font
/// family (incl. a genuine serif) is honoured. Body ink/rules come from the
/// reading palette; sizes scale with the reading font setting.
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
    paint: BlockPaintContext<'_>,
    block: &crate::model::ContentBlock,
    x: f64,
    w: f32,
    y: f64,
) -> f64 {
    let BlockPaintContext {
        scene,
        transform: t,
        text: tr,
        pane,
        links,
        settings,
        palette,
        family,
    } = paint;
    let body_size = settings.body_size();

    // Lay out `text` in `fam` at `size` within width `width`, returning height.
    let lay =
        |tr: &mut TextRenderer, text: &str, size: f32, width: f32, fam: FontFamily, bold: bool| {
            RichTextLayout::shape_styled(
                &mut tr.font_system,
                &[RichSpan::new(text.to_string()).bold(bold)],
                &RichTextLayout::block_style(bold, size, fam, 1.0),
                Some(width),
                TextAlign::Left,
                vec![],
                vec![],
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
            let layout = RichTextLayout::shape_styled(
                &mut tr.font_system,
                &[RichSpan::new(block.text.clone()).italic(true)],
                &RichTextLayout::block_style(false, body_size, family, 1.0),
                Some(tw),
                TextAlign::Left,
                vec![],
                vec![],
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
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    buttons: &[ToolbarButton],
) {
    let c = &theme.tokens.colors;
    // The toolbar's bottom rule and each button's rest edge are derived
    // against the surface they sit on (HIG §9.5): a washed `border_standard`
    // over the dark base vanished, leaving five glyphs floating with no
    // control shape around them.
    hline(
        scene,
        t,
        rect.x0,
        rect.x1,
        rect.y1,
        ui::surface::edge_on(c.surfaces.base),
    );
    for b in buttons {
        let center = b.rect.center();
        // The shared Neutral button face (HIG §8) with no label; the app's
        // own glyph is drawn in the face's ink.
        let face = ButtonFace::new(b.rect, "").radius(f64::from(theme.tokens.radii.md));
        Button::paint(scene, t, tr, theme, &face);
        let col = face.colors(theme).2;
        match b.action {
            ToolbarAction::AddSubscription => icons::plus(scene, t, center, 7.0, c.accent[6]),
            ToolbarAction::Refresh => icons::refresh(scene, t, center, 8.0, col),
            ToolbarAction::MarkAllRead => icons::check(scene, t, center, 7.0, col),
            ToolbarAction::ImportOpml => icons::import_arrow(scene, t, center, 6.0, col),
            ToolbarAction::ExportOpml => icons::external_link(scene, t, center, 6.0, col),
        }
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
    let hovered = rect.contains(mouse);
    // The shared Neutral button face (HIG §8), resolved through
    // `face_colors` because this painter runs without a text renderer; the
    // glyph is drawn in the face's ink.
    let (bg, border, ink) = ui::widget::face_colors(
        theme,
        ui::widget::ButtonStyle::Neutral,
        ui::widget::ButtonInteract {
            hovered,
            ..Default::default()
        },
    );
    let shape = RoundedRect::from_rect(*rect, f64::from(theme.tokens.radii.md));
    scene.fill(Fill::NonZero, t, bg, None, &shape);
    if let Some(border) = border {
        scene.stroke(&Stroke::new(1.0), t, border, None, &shape);
    }
    let center = rect.center();
    match glyph {
        ChromeGlyph::Menu => icons::hamburger(scene, t, center, 8.0, ink),
        ChromeGlyph::Close => {
            let r = 6.0;
            let s = Stroke::new(1.8);
            scene.stroke(
                &s,
                t,
                ink,
                None,
                &Line::new(
                    Point::new(center.x - r, center.y - r),
                    Point::new(center.x + r, center.y + r),
                ),
            );
            scene.stroke(
                &s,
                t,
                ink,
                None,
                &Line::new(
                    Point::new(center.x + r, center.y - r),
                    Point::new(center.x - r, center.y + r),
                ),
            );
        }
        ChromeGlyph::Back => icons::back_arrow(scene, t, center, 6.0, ink),
    }
}

// --- text helpers -----------------------------------------------------

fn measure_w(tr: &mut TextRenderer, s: &str, size: f32) -> f64 {
    f64::from(tr.measure(s, size, None).0)
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
fn two_line_elide(
    tr: &mut TextRenderer,
    s: &str,
    size: f32,
    max_w: f32,
    weight: TextWeight,
) -> String {
    let (_w, h) = tr.measure_weighted(s, size, Some(max_w), weight);
    let line_h = tr.measure_weighted("Ag", size, None, weight).1;
    if h <= line_h * 2.0 + 1.0 {
        return s.to_string();
    }
    // Binary-ish truncate by words until it fits two lines.
    let words: Vec<&str> = s.split_whitespace().collect();
    let mut take = words.len();
    while take > 1 {
        take -= 1;
        let candidate = format!("{}…", words.get(..take).unwrap_or(&[]).join(" "));
        if tr.measure_weighted(&candidate, size, Some(max_w), weight).1 <= line_h * 2.0 + 1.0 {
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
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::undo::LocalUndo,
        dyn ui::resources::PublishesResources,
        dyn ui::search::capability::SearchSource
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::FEEDS_OPERATION_TYPE]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        self.background_poll_deadline.set(None);
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
                self.perform_action(
                    state,
                    &FeedsAction::SetReadSettings {
                        settings: self.settings,
                    },
                );
            }
            self.synced = true;
            self.store_heads = state.get_heads();
        }
        self.drain_modal_actions(state);
        // Persist anything the live (doc-less) drain queued, then drain again
        // here so an input-driven sync also picks up fresh results promptly.
        self.flush_doc_writes(state);
        self.drain_fetches_live();
        self.flush_doc_writes(state);
        for action in std::mem::take(&mut self.pending_resource_actions) {
            self.perform_action(state, &action);
        }
        // Drain an in-flight OPML import portal request (holds the doc so the
        // parsed subscriptions persist the moment the user picks a file).
        self.drain_import(state);
        self.drain_export();
        self.drain_open_link();
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

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.drain_fetches_live();
        if self
            .resource_refresh_result
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            self.resource_refresh_result.take()
        } else {
            None
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
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.text_scale = f64::from(theme.text_scale).clamp(1.0, 2.0);
        let children: Vec<_> = self
            .modal
            .as_mut()
            .map(|modal| modal.dialog.build_layout(taffy, theme, node_map))
            .into_iter()
            .collect();
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &children,
            )
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // Persist anything the live drain queued since the last doc-holding
        // pass (keeps the CRDT current without waiting for `sync`).
        self.flush_doc_writes(ctx.state);
        // Track the pointer everywhere so it's current when a modal closes.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        // An open dialog captures all input while it's up.
        if self.modal.is_some() {
            self.handle_modal_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.handled,
            );
            return;
        }
        // The popover owns input while open. Right-click still reaches the
        // sidebar routing below so another feed can replace the current menu.
        if self.feed_menu.is_open()
            && !matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Right,
                    ..
                }
            )
        {
            if matches!(event, WindowEvent::Focused(false)) {
                self.close_feed_menu();
                *ctx.handled = true;
                return;
            }
            self.feed_menu.handle_event(event, ctx);
            if let Some(selection) = self.feed_menu.take_selection()
                && let Some(action) = self.feed_menu_actions.get(selection).copied()
                && let Some(feed_id) = self.feed_menu_target
            {
                self.apply_feed_menu_action(action, feed_id, ctx.state);
            }
            if !self.feed_menu.is_open() {
                self.feed_menu_target = None;
            }
            if matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
            ) {
                *ctx.handled = true;
            }
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.last_mouse;
                if self.click(p, ctx.state) {
                    *ctx.handled = true;
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
                    *ctx.handled = true;
                } else if self.feed_menu.is_open() {
                    self.close_feed_menu();
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.scroll_pane_under_pointer(dy);
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if key_event.state == ElementState::Pressed => {
                self.handle_key(key_event, *ctx.modifiers, ctx.state, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "FeedsApp"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.modal
            .as_ref()
            .map(|modal| &modal.dialog as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.modal_structure_dirty)
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.modal
            .as_mut()
            .map(|modal| &mut modal.dialog as &mut dyn Widget)
            .into_iter()
            .collect()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip(&self) -> Option<&str> {
        self.hover_tooltip.as_deref()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let interactive = (self.hamburger_rect.area() > 0.0 && self.hamburger_rect.contains(point))
            || (self.back_rect.area() > 0.0 && self.back_rect.contains(point))
            || (self.settings_btn_rect.area() > 0.0 && self.settings_btn_rect.contains(point))
            || (self.tags_btn_rect.area() > 0.0 && self.tags_btn_rect.contains(point))
            || (self.new_folder_rect.area() > 0.0 && self.new_folder_rect.contains(point))
            || self
                .toolbar_buttons
                .iter()
                .any(|button| button.rect.contains(point))
            || self
                .list_rows
                .iter()
                .any(|row| row.star_rect.contains(point))
            || self
                .reader_open_rect
                .is_some_and(|rect| rect.contains(point))
            || self
                .reader_links
                .iter()
                .any(|(rect, _)| rect.contains(point));
        let interactive = interactive
            || self
                .reader_tags
                .iter()
                .any(|(rect, _)| rect.contains(point));
        interactive.then_some(winit::window::CursorIcon::Pointer)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one semantic mirror covers the custom-painted toolbar, subscription tree, article list and reader document"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        if self.modal.is_some() {
            return None;
        }
        if self.feed_menu.is_open() {
            return self.feed_menu.accessibility();
        }
        let mut toolbar: Vec<_> = self
            .toolbar_buttons
            .iter()
            .map(|button| {
                let (target, name) = match button.action {
                    ToolbarAction::AddSubscription => ("feeds.toolbar:add", "Add subscription"),
                    ToolbarAction::Refresh => ("feeds.toolbar:refresh", "Refresh all feeds"),
                    ToolbarAction::MarkAllRead => ("feeds.toolbar:read", "Mark all read"),
                    ToolbarAction::ImportOpml => ("feeds.toolbar:import", "Import OPML"),
                    ToolbarAction::ExportOpml => ("feeds.toolbar:export", "Export OPML"),
                };
                AccessNode::new(Role::Button)
                    .with_name(name)
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(button.rect)
                    .with_action_target(target)
            })
            .collect();
        // Compact navigation belongs to the same action group as the regular
        // feed controls. Keeping it in the toolbar also gives every synthetic
        // toolbar child a stable action target without mixing keyed controls
        // with position-addressed root regions.
        if self.hamburger_rect.area() > 0.0 {
            toolbar.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name("Show subscriptions")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.sidebar.is_open(self.nav_layout())),
                    )
                    .with_bounds(self.hamburger_rect)
                    .with_action_target("feeds.sidebar"),
            );
        }
        if self.back_rect.area() > 0.0 {
            toolbar.push(
                AccessNode::new(Role::Button)
                    .with_name("Back to articles")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.back_rect)
                    .with_action_target("feeds.back"),
            );
        }
        if self.settings_btn_rect.area() > 0.0 {
            toolbar.push(
                AccessNode::new(Role::ToggleButton)
                    .with_name("Reading settings")
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.show_reader_settings),
                    )
                    .with_bounds(self.settings_btn_rect)
                    .with_action_target("feeds.reader-settings"),
            );
        }
        if self.tags_btn_rect.area() > 0.0 {
            toolbar.push(
                AccessNode::new(Role::Button)
                    .with_name("Edit article tags")
                    .with_description("Add or remove comma-separated labels. Shortcut T")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.tags_btn_rect)
                    .with_action_target("feeds.edit-tags"),
            );
        }
        if self.new_folder_rect.area() > 0.0 {
            toolbar.push(
                AccessNode::new(Role::Button)
                    .with_name("New folder")
                    .with_description("Create a subscription folder")
                    .with_state(AccessState::new().focusable(true))
                    .with_bounds(self.new_folder_rect)
                    .with_action_target("feeds.new-folder"),
            );
        }
        if self.show_reader_settings {
            toolbar.extend(
                self.settings_hits
                    .iter()
                    .enumerate()
                    .map(|(index, (rect, action))| {
                        let (name, checked) = match action {
                            SettingsAction::FontDec => {
                                ("Decrease reader text size".to_string(), None)
                            }
                            SettingsAction::FontInc => {
                                ("Increase reader text size".to_string(), None)
                            }
                            SettingsAction::SetFont(font) => (
                                format!("Use {} reader font", font.label()),
                                Some(self.settings.font == *font),
                            ),
                            SettingsAction::SetTheme(read_theme) => (
                                format!("Use {} reader paper", read_theme.label()),
                                Some(self.settings.theme == *read_theme),
                            ),
                        };
                        let role = if checked.is_some() {
                            Role::RadioButton
                        } else {
                            Role::Button
                        };
                        let mut state = AccessState::new().focusable(true);
                        if let Some(checked) = checked {
                            state = state.checked(checked);
                        }
                        AccessNode::new(role)
                            .with_name(name)
                            .with_state(state)
                            .with_bounds(*rect)
                            .with_action_target(format!("feeds.setting:{index}"))
                    }),
            );
        }
        let sidebar = self
            .sidebar_rows
            .iter()
            .filter(|row| row.rect.intersect(self.sidebar_rect).area() > 0.0)
            .map(|row| match row.role {
                RowRole::Section => AccessNode::new(Role::Heading { level: 2 })
                    .with_name(row.label.clone())
                    .with_hashed_key(format!("feeds.section:{}", row.label))
                    .with_bounds(row.rect),
                RowRole::Smart(kind) => {
                    let target = match kind {
                        SmartKind::All => "all",
                        SmartKind::Unread => "unread",
                        SmartKind::Starred => "starred",
                    };
                    AccessNode::new(Role::RadioButton)
                        .with_name(row.label.clone())
                        .with_description(format!("{} unread", row.badge))
                        .with_state(AccessState::new().focusable(true).checked(row.selected))
                        .with_bounds(sidebar_pill(row.rect))
                        .with_action_target(format!("feeds.filter:{target}"))
                }
                RowRole::Folder { id, collapsed } => AccessNode::new(Role::TreeItem)
                    .with_name(row.label.clone())
                    .with_description(format!("{} unread", row.badge))
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .selected(row.selected)
                            .expanded(!collapsed),
                    )
                    .with_bounds(sidebar_pill(row.rect))
                    .with_action_target(format!("feeds.folder:{id}")),
                RowRole::Feed { id, .. } => AccessNode::new(Role::TreeItem)
                    .with_name(row.label.clone())
                    .with_description(format!("{} unread", row.badge))
                    .with_state(AccessState::new().focusable(true).selected(row.selected))
                    .with_bounds(sidebar_pill(row.rect))
                    .with_action_target(format!("feeds.feed:{id}")),
                RowRole::Tag { key } => AccessNode::new(Role::TreeItem)
                    .with_name(row.label.clone())
                    .with_description(format!("{} articles", row.badge))
                    .with_state(AccessState::new().focusable(true).selected(row.selected))
                    .with_bounds(sidebar_pill(row.rect))
                    .with_action_target(format!("feeds.tag-filter:{key}")),
            })
            .collect();
        let articles = self
            .list_rows
            .iter()
            .filter(|row| row.rect.intersect(self.list_rect_content()).area() > 0.0)
            .filter_map(|row| {
                let article = self.store.article(row.id)?;
                Some(
                    AccessNode::new(Role::ListItem)
                        .with_name(article.title.clone())
                        .with_description(article.summary.clone())
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .selected(self.selected == Some(row.id)),
                        )
                        .with_bounds(crate::list_layout::row_semantic_bounds(row.rect))
                        .with_action_target(format!("feeds.article:{}", row.id))
                        .with_children(vec![
                            AccessNode::new(Role::ToggleButton)
                                .with_name(format!("Star {}", article.title))
                                .with_state(
                                    AccessState::new().focusable(true).checked(article.starred),
                                )
                                .with_bounds(row.star_rect)
                                .with_action_target(format!("feeds.star:{}", row.id)),
                        ]),
                )
            })
            .collect();
        let mut children = vec![
            AccessNode::new(Role::Toolbar)
                .with_name("Feed actions")
                .with_bounds(self.toolbar_rect)
                .with_children(toolbar),
            AccessNode::new(Role::Tree)
                .with_name("Subscriptions")
                .with_bounds(self.sidebar_rect)
                .with_children(sidebar),
            AccessNode::new(Role::List)
                .with_name("Articles")
                .with_bounds(self.list_rect)
                .with_children(articles),
        ];
        if let Some(article) = self.selected.and_then(|id| self.store.article(id)) {
            let mut reader_children = Vec::new();
            reader_children
                .push(AccessNode::new(Role::Heading { level: 1 }).with_name(article.title.clone()));
            if !article.author.is_empty() {
                reader_children
                    .push(AccessNode::new(Role::StaticText).with_name(article.author.clone()));
            }
            reader_children.extend(article.tags.iter().filter_map(|tag| {
                let key = crate::model::tag_key(tag);
                let rect = self
                    .reader_tags
                    .iter()
                    .find_map(|(rect, found)| (*found == key).then_some(*rect))?;
                Some(
                    AccessNode::new(Role::Link)
                        .with_name(format!("Tag #{tag}"))
                        .with_description("Show articles with this tag")
                        .with_state(AccessState::new().focusable(true))
                        .with_bounds(rect)
                        .with_action_target(format!("feeds.reader-tag:{key}")),
                )
            }));
            reader_children.extend(article.blocks.iter().enumerate().map(|(index, block)| {
                let role = match block.kind {
                    BlockKind::Heading1 => Role::Heading { level: 1 },
                    BlockKind::Heading2 => Role::Heading { level: 2 },
                    BlockKind::Heading3 => Role::Heading { level: 3 },
                    BlockKind::Link => Role::Link,
                    _ => Role::Paragraph,
                };
                let mut node = AccessNode::new(role)
                    .with_name(block.text.clone())
                    .with_hashed_key((article.id, index));
                if role == Role::Link && block.href.is_some() {
                    node = node.with_action_target(format!("feeds.reader-link:{index}"));
                }
                node
            }));
            if self.reader_open_rect.is_some() && !article.url.is_empty() {
                reader_children.push(
                    AccessNode::new(Role::Link)
                        .with_name("Open original article")
                        .with_action_target("feeds.open-original"),
                );
            }
            children.push(
                AccessNode::new(Role::Document)
                    .with_name(article.title.clone())
                    .with_description("Selected feed article")
                    .with_state(AccessState::new().read_only(true))
                    .with_bounds(self.reader_rect)
                    .with_children(reader_children),
            );
        }
        if self.refresh.visible() {
            children.push(
                AccessNode::new(Role::ProgressBar)
                    .with_name(format!(
                        "Refreshing feeds: {} of {} complete",
                        self.refresh.done, self.refresh.total
                    ))
                    .with_live(Politeness::Polite),
            );
        }
        if let Some((message, since)) = &self.toast
            && since.elapsed().as_secs_f32() < 4.0
        {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(message.clone())
                    .with_live(Politeness::Polite),
            );
        }
        Some(
            AccessNode::new(Role::Window)
                .with_name("Feeds")
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one routing table keeps every synthetic Feeds node on the same existing interaction and action paths"
    )]
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        state: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (state, _) = ui::doc::document(state, path);
        if matches!(action, A11yAction::Focus | A11yAction::ScrollIntoView) {
            return true;
        }
        if !matches!(
            action,
            A11yAction::Activate
                | A11yAction::ToggleCheck
                | A11yAction::Expand
                | A11yAction::Collapse
        ) {
            return false;
        }
        match target {
            "feeds.toolbar:add" => self.open_add_subscription(),
            "feeds.toolbar:refresh" => self.refresh_all(),
            "feeds.toolbar:read" => self.mark_all_read(state),
            "feeds.toolbar:import" => self.request_import(),
            "feeds.toolbar:export" => self.export_opml(),
            "feeds.sidebar" => self.sidebar.toggle(self.nav_layout()),
            "feeds.back" => self.show_master(),
            "feeds.reader-settings" => self.show_reader_settings = !self.show_reader_settings,
            "feeds.edit-tags" => self.open_tag_editor(),
            "feeds.new-folder" => self.open_add_folder(),
            "feeds.filter:all" => self.set_filter(Filter::All),
            "feeds.filter:unread" => self.set_filter(Filter::Unread),
            "feeds.filter:starred" => self.set_filter(Filter::Starred),
            "feeds.open-original" => self.open_selected_link(),
            _ if target.starts_with("feeds.setting:") => {
                let Some(index) = target
                    .strip_prefix("feeds.setting:")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    return false;
                };
                let Some((_, setting)) = self.settings_hits.get(index).copied() else {
                    return false;
                };
                self.apply_settings_action(setting, state);
            }
            _ if target.starts_with("feeds.folder:") => {
                let Some(id) = target
                    .strip_prefix("feeds.folder:")
                    .and_then(|value| value.parse::<Id>().ok())
                else {
                    return false;
                };
                if self.store.folder(id).is_none() {
                    return false;
                }
                match action {
                    A11yAction::Expand => {
                        if self.store.folder(id).is_some_and(|folder| folder.collapsed) {
                            self.toggle_folder(id, state);
                        }
                    }
                    A11yAction::Collapse => {
                        if self
                            .store
                            .folder(id)
                            .is_some_and(|folder| !folder.collapsed)
                        {
                            self.toggle_folder(id, state);
                        }
                    }
                    _ => self.set_filter(Filter::Folder(id)),
                }
            }
            _ if target.starts_with("feeds.feed:") => {
                let Some(id) = target
                    .strip_prefix("feeds.feed:")
                    .and_then(|value| value.parse::<Id>().ok())
                else {
                    return false;
                };
                if self.store.feed(id).is_none() {
                    return false;
                }
                self.set_filter(Filter::Feed(id));
            }
            _ if target.starts_with("feeds.tag-filter:") => {
                let Some(key) = target
                    .strip_prefix("feeds.tag-filter:")
                    .and_then(|value| value.parse::<crate::model::TagKey>().ok())
                else {
                    return false;
                };
                if !self.store.articles.iter().any(|article| {
                    article
                        .tags
                        .iter()
                        .any(|tag| crate::model::tag_key(tag) == key)
                }) {
                    return false;
                }
                self.set_filter(Filter::Tag(key));
            }
            _ if target.starts_with("feeds.reader-tag:") => {
                let Some(key) = target
                    .strip_prefix("feeds.reader-tag:")
                    .and_then(|value| value.parse::<crate::model::TagKey>().ok())
                else {
                    return false;
                };
                if !self.store.articles.iter().any(|article| {
                    article
                        .tags
                        .iter()
                        .any(|tag| crate::model::tag_key(tag) == key)
                }) {
                    return false;
                }
                self.set_filter(Filter::Tag(key));
            }
            _ if target.starts_with("feeds.reader-link:") => {
                let Some(index) = target
                    .strip_prefix("feeds.reader-link:")
                    .and_then(|value| value.parse::<usize>().ok())
                else {
                    return false;
                };
                let Some(href) = self
                    .selected
                    .and_then(|id| self.store.article(id))
                    .and_then(|article| article.blocks.get(index))
                    .and_then(|block| block.href.clone())
                else {
                    return false;
                };
                self.open_url(&href);
            }
            _ if target.starts_with("feeds.article:") => {
                let Some(raw_id) = target.strip_prefix("feeds.article:") else {
                    return false;
                };
                let Ok(id) = raw_id.parse() else {
                    return false;
                };
                if self.store.article(id).is_none() {
                    return false;
                }
                self.select_article(id, state);
            }
            _ if target.starts_with("feeds.star:") => {
                let Some(raw_id) = target.strip_prefix("feeds.star:") else {
                    return false;
                };
                let Ok(id) = raw_id.parse() else {
                    return false;
                };
                if self.store.article(id).is_none() {
                    return false;
                }
                self.toggle_star(id, state);
            }
            _ => return false,
        }
        true
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.import_rx.is_none()
            && self.export_rx.is_none()
            && self
                .worker
                .as_ref()
                .is_none_or(|worker| worker.pending() == 0)
            && self.pending_doc_writes.is_empty()
        {
            return None;
        }
        let deadline = self.background_poll_deadline.get().unwrap_or_else(|| {
            ui::time::now()
                .checked_add(std::time::Duration::from_millis(16))
                .unwrap_or_else(ui::time::now)
        });
        self.background_poll_deadline.set(Some(deadline));
        Some(deadline)
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
            || self.export_rx.is_some()
            || self
                .toast
                .as_ref()
                .is_some_and(|(_, since)| since.elapsed().as_secs_f32() < 4.0)
    }
}

impl ui::search::capability::SearchSource for FeedsApp {
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
}

impl ui::resources::PublishesResources for FeedsApp {
    /// Publish the reader + its folders and subscriptions so the host's Ctrl+K
    /// action palette scopes to them. When the framework hosts the surface,
    /// [`tilo_resource::publish`] already owns the reader door; hosted returns
    /// nothing rather than a second hand-built tree.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.is_hosted() {
            return Vec::new();
        }
        self.publish_resources(doc)
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.is_hosted() && id == crate::resource::READER_RESOURCE_ID {
            return None;
        }
        self.publish_resource(doc, id)
    }
}

impl ui::undo::LocalUndo for FeedsApp {
    fn handle_local_undo_request(&mut self, _request: ui::undo::UndoRequest) -> bool {
        self.feed_menu.is_open()
    }
}

impl ui::undo::CommittedActions for FeedsApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records one undo boundary for it (see
    /// `Self::perform_action`). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl FeedsApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        match (kind, operation) {
            (tilo_actions::OperationKind::View, crate::actions::FeedsOperation::ImportOpml) => {
                self.request_import();
            }
            (tilo_actions::OperationKind::View, crate::actions::FeedsOperation::ExportOpml) => {
                self.export_opml();
            }
            (tilo_actions::OperationKind::Effect, crate::actions::FeedsOperation::RefreshAll) => {
                if self.store.feeds.is_empty() {
                    return Err("there are no subscriptions to refresh".into());
                }
                self.refresh_all();
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::FeedsOperation::RevealFeed { id },
            ) => {
                let id = id.parse::<Id>().map_err(|_| "invalid feed id")?;
                if self.store.feed(id).is_none() {
                    return Err("feed no longer exists".into());
                }
                self.set_filter(Filter::Feed(id));
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::FeedsOperation::RevealArticle { id },
            ) => {
                let id = id.parse::<Id>().map_err(|_| "invalid article id")?;
                let Some(feed_id) = self.store.article(id).map(|article| article.feed_id) else {
                    return Err("article no longer exists".into());
                };
                self.set_filter(Filter::Feed(feed_id));
                self.selected = Some(id);
                self.reader_scroll = 0.0;
                if self.store.mark_read(id, true) {
                    self.pending_resource_actions
                        .push(actions::mark_read_action(id, true));
                }
                if self.single_pane() {
                    self.detail_pushed = true;
                }
            }
            (
                tilo_actions::OperationKind::View,
                crate::actions::FeedsOperation::RevealTag { tag },
            ) => {
                let tag = actions::normalize_tags([tag.as_str()])
                    .into_iter()
                    .next()
                    .ok_or_else(|| "invalid empty tag".to_string())?;
                if !self.store.has_tag(&tag) {
                    return Err("tag no longer exists".into());
                }
                self.set_filter(Filter::Tag(crate::model::tag_key(&tag)));
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect
            || type_id != crate::actions::FEEDS_OPERATION_TYPE
        {
            return Ok(None);
        }
        if self.resource_refresh_invocation.is_some() || self.refresh.in_progress() {
            return Err("a feed refresh is already running".into());
        }
        if !self.answer_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        self.resource_refresh_invocation = Some(invocation);
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl FeedsApp {
    /// Scrolls whichever pane the pointer is over by `dy` logical pixels.
    fn scroll_pane_under_pointer(&mut self, dy: f64) {
        if self.sidebar_rect.contains(self.last_mouse) {
            let visible = (self.sidebar_rect.height() - HEADER_H).max(0.0);
            let max = (self.sidebar_content_h - visible).max(0.0);
            self.sidebar_scroll = (self.sidebar_scroll - dy).clamp(0.0, max);
            self.build_sidebar_rows();
        } else if self.reader_rect.contains(self.last_mouse) {
            let visible = (self.reader_rect.height() - HEADER_H).max(0.0);
            let max = (self.reader_content_h - visible + 60.0).max(0.0);
            self.reader_scroll = (self.reader_scroll - dy).clamp(0.0, max);
        } else if self.list_rect.contains(self.last_mouse) {
            // The refresh strip (when visible) eats into the row viewport.
            let view_h =
                (self.list_rect.height() - self.header_h() - self.refresh_strip_h()).max(0.0);
            let max = (self.list_content_h - view_h).max(0.0);
            self.list_scroll = (self.list_scroll - dy).clamp(0.0, max);
        }
    }

    fn handle_key(
        &mut self,
        key_event: &winit::event::KeyEvent,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
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

        let modified = {
            let modifiers = modifiers.state();
            modifiers.control_key() || modifiers.super_key() || modifiers.alt_key()
        };
        match &key_event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.move_selection(1, state);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_selection(-1, state);
                *handled = true;
            }
            Key::Character(s) if !modified => {
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
                    "t" => {
                        self.open_tag_editor();
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
    use ui::headless::Framed;
    use uuid::Uuid;

    /// Tests that exercise refresh/render over real subscriptions use the
    /// demo builder — the only place sample feeds come from. The live `new()`
    /// path starts empty (asserted by `live_new_starts_empty`).
    fn app() -> FeedsApp {
        FeedsApp::new_demo()
    }

    /// Start an OPML export under a recording host whose save chooser
    /// answers `outcome`, and check the app asked for the export dialog.
    fn export_with(a: &mut FeedsApp, outcome: PortalOutcome) {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || a.export_opml());
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1, "one save chooser was requested");
        assert_eq!(picks[0].title(), "Export OPML");
    }

    /// Start an OPML import under a recording host whose open chooser
    /// answers `outcome`, and check the app asked for the import dialog.
    fn import_with(a: &mut FeedsApp, outcome: PortalOutcome) {
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(outcome);
        ui::host::scoped(&services, || a.request_import());
        let picks = recorder.pick_requests();
        assert_eq!(picks.len(), 1, "one open chooser was requested");
        assert_eq!(picks[0].title(), "Import OPML");
    }

    /// A live app starts empty with onboarding; only the demo build seeds and selects sample content.
    #[test]
    fn startup_is_empty_live_and_populated_only_in_demo() {
        // --- phase: live_new_starts_empty_and_offers_onboarding ---
        {
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

        // --- phase: live_sync_over_fresh_doc_stays_empty ---
        {
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
            assert!(FeedsDoc::has_settings(&doc));
            let recorded = app
                .actions
                .last_dispatched()
                .expect("reading settings are an action");
            assert!(String::from_utf8_lossy(&recorded.payload).contains("SetReadSettings"));
        }

        // --- phase: demo_build_is_populated_and_hides_onboarding ---
        {
            // The demo builder is the sole source of sample subscriptions.
            let app = FeedsApp::new_demo();
            assert!(!app.store.feeds.is_empty(), "demo build seeds sample feeds");
            assert!(
                app.empty_onboarding_prompt().is_none(),
                "a populated store hides the onboarding prompt",
            );
        }

        // --- phase: new_selects_a_featured_article ---
        {
            let a = app();
            assert!(a.selected.is_some());
            let sel = a.selected.unwrap();
            let art = a.store.article(sel).unwrap();
            assert!(!art.blocks.is_empty());
        }

        // --- phase: seed_populates_doc_on_first_sync ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            assert!(FeedsDoc::is_empty(&doc));
            a.sync(&mut doc, &automerge::ROOT);
            assert!(!FeedsDoc::is_empty(&doc));
            let loaded = FeedsDoc::load(&doc);
            assert_eq!(loaded.feeds.len(), a.store.feeds.len());
        }
    }

    /// Mount `app` over an empty document in the production frame host at
    /// 1280x800 and run its first frame, with per-pass timing on.
    fn framed(app: FeedsApp) -> Framed<FeedsApp> {
        let mut app = Framed::new(ui::State::new(), app, (1280, 800), ui::theme::Theme::dark());
        Framed::harness_mut(&mut app).begin_profiling();
        app
    }

    /// Run one full headless frame (update → sync → layout → draw), the
    /// harness's copy of `ui::App`'s per-frame cycle, and return how long the
    /// **draw** pass took. No GPU — `TextRenderer`/`Scene` are CPU-side, so this
    /// exercises exactly the main-thread work a real frame does.
    fn headless_frame(app: &mut Framed<FeedsApp>) -> std::time::Duration {
        let harness = Framed::harness_mut(app);
        harness.take_profile_samples();
        harness.relayout();
        let frame = harness
            .take_profile_samples()
            .pop()
            .expect("the frame was profiled");
        std::time::Duration::from_micros(frame.draw_us)
    }

    /// Feed refresh runs on a worker, drains without blocking the frame, reports completion and renders fetched articles through the store.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn refresh_runs_off_thread_and_renders_through_the_store() {
        // --- phase: real_worker_refresh_keeps_main_thread_responsive ---
        // A real refresh worker must not block the UI thread.
        {
            use ui::time::Duration;

            let mut a = framed(app());
            // Compare active-refresh work with warmed idle frames to avoid tying
            // the assertion to a machine-specific rendering budget.
            let synth_event = || WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(200.0, 200.0),
            };
            let dispatch = |a: &mut Framed<FeedsApp>| {
                let event = synth_event();
                let harness = Framed::harness_mut(a);
                let t = Instant::now();
                harness.dispatch_to_root(&event);
                t.elapsed()
            };
            for _ in 0..3 {
                let _ = headless_frame(&mut a);
            }
            let mut base_draw = Duration::from_secs(9999);
            let mut base_event = Duration::from_secs(9999);
            for _ in 0..5 {
                base_draw = base_draw.min(headless_frame(&mut a));
                base_event = base_event.min(dispatch(&mut a));
            }

            // Point every feed at an unreachable host so the worker's fetches hang
            // on connect/timeout — the worst case for "does the UI block?".
            for f in &mut a.store.feeds {
                f.url = "https://10.255.255.1/feed.xml".to_string();
                f.kind = FeedKind::Rss;
            }

            // Worker and runtime startup must stay off the UI thread.
            let t_refresh = ui::time::now();
            a.refresh_all();
            let refresh_cost = t_refresh.elapsed();
            assert!(
                refresh_cost < Duration::from_millis(16),
                "refresh_all (incl. cold worker/runtime spawn) must not block the UI thread; took {refresh_cost:?}"
            );
            assert!(a.worker.is_some(), "worker really spawned");
            assert!(a.refresh.in_progress());

            // Pump frames while requests remain outstanding.
            let deadline = ui::time::now() + Duration::from_millis(500);
            let mut frames = 0u32;
            let mut best_draw = Duration::from_secs(9999);
            let mut best_event = Duration::from_secs(9999);
            while Instant::now() < deadline {
                best_draw = best_draw.min(headless_frame(&mut a));
                best_event = best_event.min(dispatch(&mut a));
                frames += 1;
            }

            assert!(
                frames > 5,
                "the loop kept pumping frames while refreshing (got {frames})"
            );
            // Allow scheduler jitter while rejecting main-thread I/O or blocking
            // receives.
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

        // --- phase: refresh_is_async_and_drains_without_blocking ---
        // Proves the refresh path is fully async: `refresh_all` only enqueues (no
        // network, returns instantly); results arrive via the worker's channel;
        // the draw-time drain (`drain_fetches_live`) applies them to the store +
        // progress WITHOUT a document handle and WITHOUT calling the blocking
        // fetch; and the queued CRDT writes are flushed by a doc-holding pass. The
        // UI passes that run every frame (`draw`/`handle_event` → drain) never
        // touch the network.
        {
            fn refresh_is_async_and_drains_without_blocking() {
                let mut a = app();
                let mut doc = AutoCommit::new();
                a.sync(&mut doc, &automerge::ROOT);

                // Swap in a thread-less worker so we control results deterministically
                // and never hit the network.
                let tx = a.install_test_worker();

                // Kick off a refresh. This must return immediately — it only enqueues.
                let start = ui::time::now();
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
                    tags: Vec::new(),
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
            refresh_is_async_and_drains_without_blocking();
        }

        // --- phase: large_result_batch_does_not_block_the_frame ---
        // A refresh that returns a LARGE volume of articles (the realistic case
        // the unreachable-host test can't hit) must still keep the main thread
        // responsive: the live drain + the per-frame CRDT flush must not blow the
        // frame budget even when hundreds of articles land at once. Proves the
        // flush is bounded per frame rather than persisting everything in one go.
        {
            let mut mounted = framed(app());
            let (a, st) = Framed::split(&mut mounted);
            let doc = &mut st.doc;

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
                        tags: Vec::new(),
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

            // The live drain folds everything into the store in one pass. It is
            // pure in-memory work: it persists nothing to the CRDT, which is what
            // keeps it cheap on the draw thread even for a big batch. (A wall-clock
            // bound here measures the shared test host, not the drain.)
            let heads_before = doc.get_heads();
            a.drain_fetches_live();
            assert_eq!(
                doc.get_heads(),
                heads_before,
                "the live drain must not persist the batch in the frame"
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
            a.flush_doc_writes(doc);
            let persisted_this_frame = before - a.pending_doc_writes.len();
            assert!(
                persisted_this_frame <= FLUSH_BUDGET,
                "one flush must persist at most FLUSH_BUDGET ({FLUSH_BUDGET}) writes, did {persisted_this_frame}"
            );

            // Drain the rest across frames; the backlog empties in bounded steps.
            let mut frames = 1;
            while !a.pending_doc_writes.is_empty() && frames < 5000 {
                a.flush_doc_writes(doc);
                frames += 1;
            }
            assert!(a.pending_doc_writes.is_empty(), "backlog fully drained");
            // Everything eventually persisted to the CRDT.
            let persisted = FeedsDoc::load(doc).articles.len();
            assert!(
                persisted >= total_new,
                "all {total_new} articles persisted (got {persisted})"
            );
        }

        // --- phase: refresh_progress_finishes_when_all_feeds_report ---
        // `refresh_all` enqueues exactly one request per feed and finishes only
        // once every feed has reported back (success or failure).
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            let tx = a.install_test_worker();
            a.refresh_all();
            let total = a.refresh.total;
            assert_eq!(total, a.store.feeds.len());
            let deadline = a
                .next_wake_deadline()
                .expect("network results must wake without animation");
            assert_eq!(a.next_wake_deadline(), Some(deadline));

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
            a.sync(&mut doc, &automerge::ROOT);
            assert!(
                a.next_wake_deadline().is_none(),
                "completed work stops polling"
            );
        }

        // --- phase: fetched_articles_render_through_the_store ---
        {
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
                tags: Vec::new(),
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
    }

    // --- async refresh (non-blocking) ------------------------------------

    /// Reading settings, read/star state, filters and folder collapse persist through the document.
    #[test]
    fn reader_state_edits_persist_through_the_document() {
        // --- phase: reading_settings_apply_and_persist ---
        // Changing a reading setting applies it (and persists), proving the
        // settings affect the reader. Drives the production action path.
        {
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

        // --- phase: selecting_marks_read_in_store_and_doc ---
        {
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

        // --- phase: star_toggles_through_doc ---
        {
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

        // --- phase: filter_changes_list_and_selection ---
        {
            let mut a = app();
            a.set_filter(Filter::Starred);
            let starred = a.store.articles_for(Filter::Starred);
            assert!(!starred.is_empty());
            // Selection must be within the starred set.
            assert!(starred.iter().any(|x| Some(x.id) == a.selected));
        }

        // --- phase: mark_all_read_clears_unread_filter ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            a.set_filter(Filter::All);
            a.mark_all_read(&mut doc);
            assert_eq!(a.store.total_unread(), 0);
        }

        // --- phase: folder_toggle_collapses ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            let fid = a.store.folders[0].id;
            let before = a.store.folder(fid).unwrap().collapsed;
            a.toggle_folder(fid, &mut doc);
            assert_ne!(a.store.folder(fid).unwrap().collapsed, before);
        }
    }

    /// Accessibility mirrors navigation, reader, controls and modal fields, and operating them dispatches the existing actions.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn accessibility_mirrors_and_operates_the_reader() {
        // --- phase: accessibility_exposes_article_and_star_through_existing_actions ---
        {
            let mut mounted = framed(app());
            let (a, st) = Framed::split(&mut mounted);
            let id = a.list_rows[0].id;
            let before = a.store.article(id).expect("sample article").starred;
            let tree = a.accessibility().expect("feeds exports a semantic window");
            assert_eq!(tree.role, Role::Window);
            assert!(tree.children.iter().any(|node| node.role == Role::Toolbar));
            assert!(a.perform_semantic_action(
                &format!("feeds.star:{id}"),
                &A11yAction::ToggleCheck,
                &mut st.doc,
                &st.root,
            ));
            assert_ne!(a.store.article(id).expect("sample article").starred, before);
            assert!(a.perform_semantic_action(
                &format!("feeds.article:{id}"),
                &A11yAction::Activate,
                &mut st.doc,
                &st.root,
            ));
            assert_eq!(a.selected_article(), Some(id));
        }

        // --- phase: accessibility_mirrors_navigation_reader_and_reading_controls ---
        {
            fn has_target(node: &AccessNode, target: &str) -> bool {
                node.action_target.as_deref() == Some(target)
                    || node.children.iter().any(|child| has_target(child, target))
            }

            let mut a = app();
            a.force_reader_settings_open();
            let a = framed(a);
            let tree = a.accessibility().expect("feeds exports a semantic window");
            assert!(tree.children.iter().any(|node| node.role == Role::Tree));
            assert!(
                tree.children.iter().any(|node| node.role == Role::Document),
                "the custom-painted article body is exposed as a document"
            );
            assert!(has_target(&tree, "feeds.filter:unread"));
            assert!(has_target(&tree, "feeds.reader-settings"));
            assert!(
                tree.children
                    .iter()
                    .flat_map(|node| &node.children)
                    .any(|node| node
                        .action_target
                        .as_deref()
                        .is_some_and(|target| { target.starts_with("feeds.setting:") })),
                "the open settings popover exposes its real controls"
            );
        }

        // --- phase: icon_only_controls_publish_teaching_tooltips ---
        {
            let mut a = framed(app());
            let refresh = a
                .toolbar_buttons
                .iter()
                .find(|button| button.action == ToolbarAction::Refresh)
                .expect("refresh button")
                .rect;
            a.last_mouse = refresh.center();
            a.refresh_hover_tooltip();
            assert_eq!(Widget::tooltip(&*a), Some("Refresh all (R)"));
            assert_eq!(
                Widget::cursor_shape(&*a, refresh.center()),
                Some(winit::window::CursorIcon::Pointer)
            );
        }

        // --- phase: native_accessibility_dispatch_edits_the_modal_field ---
        {
            let mut reader = FeedsApp::new();
            reader.open_add_subscription();
            lay_out(&mut reader, 800.0, 600.0);
            let mut app = ui::App::new(ui::State::new(), Box::new(reader));
            app.init_layout();
            let field = app
                .build_a11y_tree_update(1.0)
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Feed URL"))
                .map(|(id, _)| *id)
                .expect("native field must be exported");
            assert!(
                field.0 < (1_u64 << 48),
                "field has no retained layout id: {field:?}"
            );
            app.dispatch_a11y_action(
                field,
                &A11yAction::SetValue("https://example.invalid/rss.xml".into()),
            );
            let after = app.build_a11y_tree_update(1.0);
            let field = after
                .nodes
                .iter()
                .find(|(_, node)| node.label() == Some("Feed URL"))
                .unwrap();
            assert_eq!(field.1.value(), Some("https://example.invalid/rss.xml"));
        }

        // --- phase: accessibility_modal_field_and_footer_submit_a_real_subscription ---
        {
            let mut reader = FeedsApp::new();
            let mut doc = AutoCommit::new();
            reader.sync(&mut doc, &automerge::ROOT);
            let _sender = reader.install_test_worker();
            reader.open_add_subscription();
            assert!(
                reader.take_structure_dirty(),
                "opening must rebuild the native input tree"
            );
            assert!(
                !reader.take_structure_dirty(),
                "the unchanged tree must remain stable"
            );
            lay_out(&mut reader, 800.0, 600.0);
            let url = "https://example.invalid/feeds.xml";
            {
                let mut dialogs = reader.children_mut();
                let dialog = dialogs
                    .first_mut()
                    .expect("open dialog must be in the widget tree");
                {
                    let mut bodies = dialog.children_mut();
                    let body = bodies.first_mut().unwrap();
                    let mut fields = body.children_mut();
                    let field = fields
                        .first_mut()
                        .expect("real TextInput must be reachable");
                    assert_eq!(
                        field.accessibility().unwrap().name.as_deref(),
                        Some("Feed URL")
                    );
                    assert!(field.perform_action(
                        &A11yAction::SetValue(url.into()),
                        &mut doc,
                        &automerge::ROOT
                    ));
                }
                assert!(dialog.perform_semantic_action(
                    "modal.primary",
                    &A11yAction::Activate,
                    &mut doc,
                    &automerge::ROOT
                ));
            }
            reader.sync(&mut doc, &automerge::ROOT);
            assert!(reader.modal.is_none());
            assert!(
                reader.take_structure_dirty(),
                "closing must remove stale input targets"
            );
            assert!(
                FeedsDoc::load(&doc)
                    .feeds
                    .iter()
                    .any(|feed| feed.url == url)
            );
        }
    }

    /// Tag editing, new folders, tag chips, toolbar navigation and lazy context all route through the canonical actions.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn ui_edits_route_through_canonical_actions() {
        // --- phase: article_tag_editor_uses_the_canonical_action_and_allows_clearing ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            let id = a.selected.expect("demo selects an article");

            a.open_tag_editor();
            assert_eq!(
                a.modal.as_ref().map(|modal| modal.intent),
                Some(ModalIntent::EditArticleTags { article_id: id })
            );
            if let Some(modal) = a.modal.as_mut() {
                modal
                    .dialog
                    .content_mut()
                    .set_value("Design, Later, design");
            }
            a.submit_modal(&mut doc);
            assert_eq!(
                a.store.article(id).expect("article").tags,
                ["design", "later"]
            );
            assert_eq!(
                FeedsDoc::load(&doc)
                    .article(id)
                    .expect("persisted article")
                    .tags,
                ["design", "later"]
            );

            a.open_tag_editor();
            if let Some(modal) = a.modal.as_mut() {
                modal.dialog.content_mut().set_value("");
            }
            a.submit_modal(&mut doc);
            assert!(a.store.article(id).expect("article").tags.is_empty());
            assert!(
                FeedsDoc::load(&doc)
                    .article(id)
                    .expect("persisted article")
                    .tags
                    .is_empty()
            );
        }

        // --- phase: sidebar_new_folder_uses_the_canonical_action_and_persists ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut a, 1280.0, 800.0);

            assert!(
                a.new_folder_rect.area() > 0.0,
                "wide sidebar exposes folder creation"
            );
            assert!(a.click(a.new_folder_rect.center(), &mut doc));
            assert_eq!(
                a.modal.as_ref().map(|modal| modal.intent),
                Some(ModalIntent::AddFolder),
                "the sidebar affordance opens the shared real-input dialog"
            );
            a.modal
                .as_mut()
                .unwrap()
                .dialog
                .content_mut()
                .set_value("Later");
            a.submit_modal(&mut doc);

            let folder_id = a
                .store
                .folders
                .iter()
                .find(|folder| folder.name == "Later")
                .expect("created folder joins the local store")
                .id;
            assert_eq!(a.active_filter(), Filter::Folder(folder_id));
            assert!(
                FeedsDoc::load(&doc)
                    .folders
                    .iter()
                    .any(|persisted| persisted.id == folder_id && persisted.name == "Later"),
                "the exact folder is persisted through FeedsAction::AddFolder"
            );
            assert!(
                ui::undo::take_committed_action(&mut a).is_some(),
                "folder creation creates one undo boundary"
            );
        }

        // --- phase: reader_tag_chips_filter_articles_and_are_semantically_actionable ---
        {
            let mut mounted = framed(app());

            let (a, st) = Framed::split(&mut mounted);
            let (chip, key) = a
                .reader_tags
                .first()
                .copied()
                .expect("featured fixture article exposes a visible tag chip");
            assert!(a.click(chip.center(), &mut st.doc));
            assert_eq!(a.active_filter(), Filter::Tag(key));

            // Re-render after the filter transition and drive the same tagged
            // discovery affordance through the semantic tree's stable target.
            a.select_featured();
            headless_frame(&mut mounted);
            let (a, st) = Framed::split(&mut mounted);
            assert!(a.perform_semantic_action(
                &format!("feeds.reader-tag:{key}"),
                &A11yAction::Activate,
                &mut st.doc,
                &st.root,
            ));
            assert_eq!(a.active_filter(), Filter::Tag(key));
        }

        // --- phase: compact_navigation_is_grouped_with_keyed_toolbar_actions ---
        {
            let mut a = app();
            lay_out(&mut a, 390.0, 844.0);
            let tree = a.accessibility().expect("feeds exports a semantic window");
            assert!(
                tree.children.iter().all(|node| node.semantic_key.is_none()),
                "root regions remain position-addressed"
            );
            let toolbar = tree
                .children
                .iter()
                .find(|node| node.role == Role::Toolbar)
                .expect("feed actions toolbar");
            assert!(
                toolbar
                    .children
                    .iter()
                    .all(|node| node.semantic_key.is_some()),
                "all compact toolbar actions have stable identities"
            );
            assert!(
                toolbar
                    .children
                    .iter()
                    .any(|node| { node.action_target.as_deref() == Some("feeds.sidebar") })
            );
        }

        // --- phase: exact_lazy_context_contains_only_the_addressed_archive_item ---
        {
            let mut a = app();
            let feed_template = a.store.feeds[0].clone();
            let article_template = a.store.articles[0].clone();
            let mut target = article_template.id;
            for index in 0..200_u128 {
                let mut feed = feed_template.clone();
                feed.id = Uuid::from_u128(50_000 + index);
                feed.title = format!("Archive feed {index}");
                let mut article = article_template.clone();
                article.id = Uuid::from_u128(60_000 + index);
                article.feed_id = feed.id;
                article.tags = vec![format!("archive-{index}")];
                target = article.id;
                a.store.feeds.push(feed);
                a.store.articles.push(article);
            }

            let article_ctx = a
                .feeds_resource_ctx_for(&actions::article_resource_id(target))
                .expect("late article resolves");
            assert_eq!(article_ctx.feeds.len(), 1);
            assert_eq!(article_ctx.feeds[0].articles.len(), 1);
            assert_eq!(article_ctx.feeds[0].articles[0].id, target);
            assert!(article_ctx.folders.is_empty());
            assert!(article_ctx.tags.is_empty());
            assert!(article_ctx.unread.is_empty());

            let tag_id = actions::tag_resource_id("archive-199");
            let tag_ctx = a
                .feeds_resource_ctx_for(&tag_id)
                .expect("late tag resolves");
            assert_eq!(tag_ctx.tags.len(), 1);
            assert!(tag_ctx.feeds.is_empty());
            assert!(tag_ctx.folders.is_empty());
            assert!(
                a.feeds_resource_ctx_for(&format!("feeds.tag:{}", "aa".repeat(193)))
                    .is_none(),
                "oversized hostile ids are rejected before materialization"
            );
        }
    }

    /// The sidebar docks when wide and becomes a hamburger/scrim overlay when narrow, per the resolved width tier and reduced motion.
    #[test]
    fn sidebar_overlay_follows_the_width_tier() {
        // --- phase: reduced_motion_settles_the_sidebar_in_one_frame ---
        {
            let mut a = app();
            lay_out(&mut a, 390.0, 844.0);
            a.sidebar.snap(false);
            a.sidebar.toggle(SplitLayout::Overlay);
            assert!(a.sidebar.is_animating(SplitLayout::Overlay));

            let mut scene = Scene::new();
            let mut text = TextRenderer::new();
            a.paint(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &Theme::dark().with_reduce_motion(true),
            );
            assert!(!a.sidebar.is_animating(SplitLayout::Overlay));
            assert!(a.sidebar.is_open(SplitLayout::Overlay));
        }

        // --- phase: wide_keeps_three_panes_docked ---
        {
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

        // --- phase: narrow_collapses_sidebar_to_closed_overlay ---
        {
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

        // --- phase: hamburger_click_opens_overlay_sidebar_with_scrim ---
        {
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

        // --- phase: scrim_click_dismisses_overlay_sidebar ---
        {
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

        // --- phase: tier_resolves_from_width ---
        // Crossing the tier boundaries flips the sidebar between docked (wide +
        // middle) and an overlay drawer (narrow) — proven off the width alone.
        {
            assert_eq!(Tier::from_width(1280.0), Tier::Wide);
            assert_eq!(
                Tier::from_width(WIDE_BREAKPOINT),
                Tier::Wide,
                "wide boundary is inclusive"
            );
            assert_eq!(
                Tier::from_width(1024.0),
                Tier::Middle,
                "a high-contrast 1024px reader must not squeeze three panes"
            );
            assert_eq!(Tier::from_width(WIDE_BREAKPOINT - 1.0), Tier::Middle);
            assert_eq!(
                Tier::from_width(640.0),
                Tier::Middle,
                "middle boundary is inclusive"
            );
            assert_eq!(Tier::from_width(639.0), Tier::Narrow);
            assert_eq!(Tier::from_width(400.0), Tier::Narrow);
        }
    }

    /// Selecting an article pushes the detail only when narrow; middle and large-text layouts show one content pane with back navigation.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn detail_navigation_adapts_to_width_and_text_size() {
        // --- phase: large_text_reflows_to_a_single_pane_with_generous_vertical_metrics ---
        {
            let mut a = app();
            a.text_scale = 2.0;
            lay_out(&mut a, 1600.0, 900.0);

            assert_eq!(
                a.tier(),
                Tier::Middle,
                "200% text reduces usable column density"
            );
            assert!(a.single_pane());
            assert!((a.sidebar_rect.width() - SIDEBAR_W * 2.0).abs() < 0.5);
            assert!((a.header_h() - HEADER_H * 2.0).abs() < f64::EPSILON);
            assert!((a.list_row_h() - LIST_ROW_H * 2.0).abs() < f64::EPSILON);
            assert!(
                a.new_folder_rect.height() >= 70.0,
                "the sidebar-header folder control retains a spacious touch target"
            );
        }

        // --- phase: selecting_an_article_pushes_detail_when_narrow ---
        {
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

        // --- phase: selecting_an_article_does_not_push_when_wide ---
        {
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

        // --- phase: middle_docks_sidebar_and_shows_a_single_content_pane ---
        // The middle tier: the sidebar stays docked + fixed while exactly ONE
        // content pane (the list here, since nothing is pushed) shares the rest of
        // the width. No overlay chrome, no collapse toggle — the sidebar is fixed.
        {
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

        // --- phase: middle_back_button_navigates_detail_to_list ---
        // The middle tier's master→detail: selecting pushes the reader into the
        // single content pane with a top-left Back button; Back returns to the
        // list. Driven entirely through the real selection + click paths + state.
        {
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
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

    /// OPML import and portal export/import persist their results and never report success on cancel or failure.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn opml_portal_import_and_export_report_honestly() {
        // --- phase: opml_import_adds_all_feeds_and_persists ---
        {
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

        // --- phase: portal_export_writes_the_snapshot_to_the_selected_unicode_path ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            let directory = std::env::temp_dir().join(format!("feeds-export-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&directory).unwrap();
            let path = directory.join("Subscriptions Grün 🌍.opml");
            let xml = opml::export(&a.store);
            export_with(&mut a, PortalOutcome::Opened(path.clone()));
            let deadline = a
                .next_wake_deadline()
                .expect("export must wake without animation");
            assert_eq!(a.next_wake_deadline(), Some(deadline));
            assert!(!path.exists());
            a.sync(&mut doc, &automerge::ROOT);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), xml);
            assert_eq!(opml::parse(&xml).len(), a.store.feeds.len());
            assert!(a.export_rx.is_none());
            assert!(a.next_wake_deadline().is_none());
            assert!(a.toast.as_ref().unwrap().0.starts_with("Exported OPML"));
            std::fs::remove_dir_all(directory).unwrap();
        }

        // --- phase: portal_export_cancel_and_failure_never_report_success ---
        {
            for outcome in [
                PortalOutcome::Cancelled,
                PortalOutcome::Failed("test denial".into()),
            ] {
                let mut a = app();
                let mut doc = AutoCommit::new();
                a.sync(&mut doc, &automerge::ROOT);
                export_with(&mut a, outcome.clone());
                a.sync(&mut doc, &automerge::ROOT);
                assert!(a.export_rx.is_none());
                let message = &a.toast.as_ref().unwrap().0;
                assert!(
                    a.accessibility().unwrap().children.iter().any(|node| {
                        node.name.as_deref() == Some(message.as_str())
                            && node.live == Some(Politeness::Polite)
                    }),
                    "native accessibility must expose the transfer outcome"
                );
                match outcome {
                    PortalOutcome::Cancelled => assert_eq!(message, "Export cancelled"),
                    PortalOutcome::Failed(reason) => {
                        assert_eq!(message, &format!("Export failed: {reason}"));
                    }
                    PortalOutcome::Opened(_) => unreachable!(),
                }
            }
        }

        // --- phase: portal_export_write_failure_is_visible ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            // A directory cannot be replaced by the export's file write on either host.
            export_with(&mut a, PortalOutcome::Opened(std::env::temp_dir()));
            a.sync(&mut doc, &automerge::ROOT);
            assert!(a.export_rx.is_none());
            assert!(a.toast.as_ref().unwrap().0.starts_with("Export failed:"));
        }

        // --- phase: portal_import_completion_has_a_fixed_non_animation_deadline ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            assert!(a.next_wake_deadline().is_none());
            import_with(&mut a, PortalOutcome::Cancelled);
            let deadline = a
                .next_wake_deadline()
                .expect("file completion must wake even with reduced motion");
            assert_eq!(a.next_wake_deadline(), Some(deadline));
            a.sync(&mut doc, &automerge::ROOT);
            assert!(a.import_rx.is_none());
            assert!(a.next_wake_deadline().is_none());
        }

        // --- phase: portal_import_drain_reads_file_and_subscribes ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            // A no-thread worker so the post-import background refresh only records
            // pending requests (no network, no spawned thread).
            let _tx = a.install_test_worker();

            // Write a real OPML file to a scratch path, have the recording host's
            // chooser pick it, then run one `sync` — the exact per-frame drain the
            // live portal feeds.
            let dir = std::env::temp_dir().join(format!("feeds-opml-test-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("subs.opml");
            std::fs::write(&path, opml::sample_opml()).unwrap();

            let feeds_before = a.store.feeds.len();
            import_with(&mut a, PortalOutcome::Opened(path));
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

        // --- phase: cancelled_import_leaves_store_untouched ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            let before = a.store.feeds.len();
            import_with(&mut a, PortalOutcome::Cancelled);
            a.sync(&mut doc, &automerge::ROOT);
            assert!(a.import_rx.is_none(), "cancel clears the request");
            assert_eq!(a.store.feeds.len(), before, "cancel adds no feeds");
        }
    }

    /// The feed context menu opens on right click, owns keyboard/hover/wheel input, captures host undo and stays inside the viewport.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn feed_context_menu_input_and_geometry() {
        // --- phase: right_click_feed_opens_context_menu_with_actions ---
        {
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

        // --- phase: feed_menu_keyboard_selects_actions_without_changing_articles ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut a, 1280.0, 800.0);
            let id = open_first_feed_menu(&mut a, &mut doc);
            let selected = a.selected;
            assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::ArrowDown)));
            assert_eq!(
                a.selected, selected,
                "menu navigation must not select articles"
            );
            assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::ArrowDown)));
            assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::Enter)));
            assert!(!a.feed_menu.is_open());
            assert_eq!(a.feed_menu_target, None);
            assert_eq!(
                a.modal.as_ref().unwrap().intent,
                ModalIntent::RenameFeed { feed_id: id }
            );
        }

        // --- phase: feed_menu_hover_focuses_action_and_captures_article_shortcuts ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut a, 1280.0, 800.0);
            open_first_feed_menu(&mut a, &mut doc);
            let p = menu_point_for(&a, 1);
            assert!(
                dispatch(&mut a, &mut doc, &cursor_at(p)),
                "menu hover must reach the host redraw path"
            );
            assert!(a.needs_redraw(), "menu hover must repaint");
            assert!(
                a.feed_menu.accessibility().unwrap().children[1]
                    .state
                    .focused
            );
            let before: Vec<_> = a
                .store
                .articles
                .iter()
                .map(|a| (a.id, a.read, a.starred))
                .collect();
            for key in ["m", "s", "j", "k", "a"] {
                assert!(dispatch(&mut a, &mut doc, &char_key(key)));
            }
            let after: Vec<_> = a
                .store
                .articles
                .iter()
                .map(|a| (a.id, a.read, a.starred))
                .collect();
            assert_eq!(before, after, "menu captures article mutations");
            assert!(a.modal.is_none(), "shortcuts cannot open other overlays");
            assert!(dispatch(&mut a, &mut doc, &named_key(NamedKey::Escape)));
            assert!(!a.feed_menu.is_open());
            assert_eq!(a.feed_menu_target, None);
        }

        // --- phase: feed_menu_wheel_does_not_scroll_panes_and_focus_loss_dismisses ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            a.sync(&mut doc, &automerge::ROOT);
            lay_out(&mut a, 1280.0, 800.0);
            open_first_feed_menu(&mut a, &mut doc);
            let p = a.list_rect.center();
            dispatch(&mut a, &mut doc, &cursor_at(p));
            a.list_content_h = 3000.0;
            let wheel = WindowEvent::MouseWheel {
                device_id: synth_device(),
                delta: MouseScrollDelta::LineDelta(0.0, -2.0),
                phase: winit::event::TouchPhase::Moved,
            };
            assert!(dispatch(&mut a, &mut doc, &wheel));
            assert_eq!(a.list_scroll, 0.0);
            assert!(
                dispatch(&mut a, &mut doc, &WindowEvent::Focused(false)),
                "dismissal must reach the host redraw path"
            );
            assert!(!a.feed_menu.is_open());
            assert_eq!(a.feed_menu_target, None);
        }

        // --- phase: feed_menu_repositions_after_viewport_shrinks ---
        {
            let mut a = app();
            lay_out(&mut a, 1280.0, 800.0);
            a.open_feed_menu(a.store.feeds[0].id, Point::new(1100.0, 750.0));
            lay_out_keep(&mut a, 320.0, 240.0);
            let r = a.feed_menu.get_rect();
            assert!(
                r.x1 <= 320.0 && r.y1 <= 240.0,
                "resized menu {r:?} is offscreen"
            );
        }

        // --- phase: feed_menu_captures_host_undo_before_document_history_changes ---
        {
            let mut a = app();
            lay_out(&mut a, 640.0, 480.0);
            a.open_feed_menu(a.store.feeds[0].id, Point::new(100.0, 200.0));
            assert!(ui::undo::handle_local_undo_request(
                &mut a,
                ui::undo::UndoRequest::Undo
            ));
            assert!(ui::undo::handle_local_undo_request(
                &mut a,
                ui::undo::UndoRequest::Redo
            ));
        }

        // --- phase: feed_menu_stays_within_tiny_viewport ---
        {
            let mut a = app();
            lay_out(&mut a, 160.0, 80.0);
            a.open_feed_menu(a.store.feeds[0].id, Point::new(150.0, 75.0));
            let r = a.feed_menu.get_rect();
            assert!(r.x0 >= a.rect.x0 && r.y0 >= a.rect.y0);
            assert!(
                r.x1 <= a.rect.x1 && r.y1 <= a.rect.y1,
                "{r:?} exceeds {:?}",
                a.rect
            );
        }
    }

    /// Sidebar and article lists scroll to their content and rows contain their measured, scaled text.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn list_scrolling_and_text_scaling_keep_rows_visible() {
        // --- phase: sidebar_wheel_reaches_subscriptions_below_the_viewport ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            lay_out(&mut a, 640.0, 480.0);
            let before = a.sidebar_rows.last().unwrap().rect.y1;
            dispatch(&mut a, &mut doc, &cursor_at(Point::new(100.0, 300.0)));
            dispatch(
                &mut a,
                &mut doc,
                &WindowEvent::MouseWheel {
                    device_id: synth_device(),
                    delta: MouseScrollDelta::LineDelta(0.0, -100.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            a.recompute_geometry();
            let after = a.sidebar_rows.last().unwrap().rect.y1;
            assert!(
                after < before && after <= 480.0,
                "last sidebar row remains inaccessible at {after}"
            );
        }

        // --- phase: smaller_text_keeps_scrolled_article_rows_visible ---
        {
            let mut a = app();
            let mut doc = AutoCommit::new();
            lay_out(&mut a, 640.0, 480.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            a.paint(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &Theme::dark().with_text_scale(2.0),
            );
            let p = a.list_rect.center();
            dispatch(&mut a, &mut doc, &cursor_at(p));
            dispatch(
                &mut a,
                &mut doc,
                &WindowEvent::MouseWheel {
                    device_id: synth_device(),
                    delta: MouseScrollDelta::LineDelta(0.0, -1000.0),
                    phase: winit::event::TouchPhase::Moved,
                },
            );
            text.set_text_scale(1.0);
            a.paint(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &Theme::dark(),
            );
            assert!(
                a.list_rows
                    .iter()
                    .any(|row| row.rect.y1 > a.list_rect.y0 + HEADER_H
                        && row.rect.y0 < a.list_rect.y1),
                "reducing text scale must not leave the article pane blank"
            );
            let view_h = (a.list_rect.height() - a.header_h() - a.refresh_strip_h()).max(0.0);
            let max_scroll = (a.list_content_h - view_h).max(0.0);
            assert!(
                a.list_scroll <= max_scroll + 0.01,
                "scroll offset must remain inside the resized content"
            );
        }

        // --- phase: large_text_rows_contain_their_measured_content ---
        {
            let mut a = app();
            lay_out(&mut a, 640.0, 480.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            a.paint(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &Theme::dark().with_text_scale(2.0),
            );
            let sidebar_line = f64::from(
                text.measure_weighted("Ag", 14.0, None, TextWeight::Semibold)
                    .1,
            );
            for row in &a.sidebar_rows {
                if !matches!(row.role, RowRole::Section) {
                    assert!(
                        row.rect.height() >= sidebar_line + 8.0,
                        "sidebar row {:?} clips scaled text height {sidebar_line}",
                        row.rect
                    );
                }
            }
            let title_line = f64::from(
                text.measure_weighted("Ag", 15.0, None, TextWeight::Semibold)
                    .1,
            );
            let meta_line = f64::from(text.measure("Ag", 12.5, None).1);
            for row in &a.list_rows {
                assert!(
                    row.rect.height() >= 12.0 + title_line * 2.0 + 8.0 + meta_line + 10.0,
                    "article row {:?} overlaps title and metadata",
                    row.rect
                );
            }
        }

        // --- phase: article_title_elision_uses_both_scaled_lines ---
        {
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            let title = "The quick brown fox jumps over the lazy dog";
            let rendered = two_line_elide(&mut text, title, 15.0, 200.0, TextWeight::Semibold);
            assert!(
                rendered.contains("brown"),
                "scaled two-line title was shortened to {rendered:?}"
            );
            let line_height = text
                .measure_weighted("Ag", 15.0, None, TextWeight::Semibold)
                .1;
            let height = text
                .measure_weighted(&rendered, 15.0, Some(200.0), TextWeight::Semibold)
                .1;
            assert!(
                height <= line_height * 2.0 + 1.0,
                "title uses {height}px, exceeding two lines"
            );
        }
    }

    /// Unsubscribe, mark-all-read and rename from the feed context menu persist.
    #[test]
    fn feed_context_menu_actions_persist() {
        // --- phase: context_menu_unsubscribe_removes_feed_and_persists ---
        {
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

        // --- phase: context_menu_mark_all_read_marks_the_feed_read ---
        {
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

        // --- phase: context_menu_rename_opens_prefilled_modal_and_persists ---
        {
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
    }

    /// The add-subscription modal pastes, edits and cancels correctly, and external article links reject local files.
    #[test]
    fn subscription_modal_and_links_accept_only_safe_input() {
        // --- phase: external_article_links_reject_local_files ---
        {
            let mut reader = FeedsApp::new();
            reader.open_url("file:///does-not-exist-tilo-link-proof");
            assert_eq!(
                reader.toast.as_ref().map(|(text, _)| text.as_str()),
                Some(ui::i18n!("feeds-open-link-invalid").as_str())
            );
        }

        // --- phase: add_subscription_modal_pastes_url_and_subscribes ---
        // Verifies Ctrl+V reaches the add-subscription field and submission uses
        // the normal `subscribe` path.
        {
            use winit::event::Modifiers;
            use winit::keyboard::ModifiersState;

            // Exercise the host clipboard boundary and the real TextInput paste
            // handler without touching the user's clipboard or requiring a desktop
            // clipboard grant. Successful OS transport requires separate native verification.
            let (services, _recorder) = ui::host::HostServices::recording();
            let _host = ui::host::enter(&services);

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

        // --- phase: add_subscription_modal_esc_cancels_and_typing_edits ---
        // Esc cancels the dialog without subscribing, and typing then Ctrl+A +
        // typing edits the field — exercising the shared caret/selection core
        // rather than a per-app buffer.
        {
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

    // --- add-subscription dialog (real ModalDialog + real TextInput) -------
}
