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

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::doc::FeedsDoc;
use crate::fetch::{FetchRequest, FetchResult, FetchWorker};
use crate::icons;
use crate::model::{
    relative_time, Article, BlockKind, Feed, FeedKind, Filter, Id, Store,
};
use crate::{opml, sample};

const SIDEBAR_W: f64 = 256.0;
const LIST_W: f64 = 400.0;
const HEADER_H: f64 = 56.0;
const SIDE_ROW_H: f64 = 34.0;
const LIST_ROW_H: f64 = 88.0;

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
    ExportOpml,
}

#[derive(Clone, Debug)]
struct ToolbarButton {
    rect: Rect,
    action: ToolbarAction,
    tip: &'static str,
}

#[derive(Clone, Debug)]
struct ListRow {
    rect: Rect,
    star_rect: Rect,
    id: Id,
}

/// A transient modal overlay.
enum Modal {
    AddSubscription { input: String },
}

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
    modal: Option<Modal>,
    toast: Option<(String, Instant)>,

    worker: Option<FetchWorker>,
    synced: bool,
    now: i64,
}

impl Default for FeedsApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedsApp {
    pub fn new() -> Self {
        let store = sample::store();
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
            worker: None,
            synced: false,
            now: chrono::Utc::now().timestamp(),
        };
        app.select_featured();
        app
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

    fn ensure_selection(&mut self) {
        let in_list = self
            .selected
            .map(|id| self.store.articles_for(self.filter).iter().any(|a| a.id == id))
            .unwrap_or(false);
        if !in_list {
            self.selected = self.store.articles_for(self.filter).first().map(|a| a.id);
        }
    }

    fn set_toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), Instant::now()));
    }

    fn worker(&mut self) -> &mut FetchWorker {
        if self.worker.is_none() {
            self.worker = Some(FetchWorker::spawn());
        }
        self.worker.as_mut().unwrap()
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
                .map(|f| f.name.clone())
                .unwrap_or_else(|| "Folder".into()),
            Filter::Feed(id) => self
                .store
                .feed(id)
                .map(|f| f.title.clone())
                .unwrap_or_else(|| "Feed".into()),
        }
    }

    fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.list_scroll = 0.0;
        self.ensure_selection();
        self.reader_scroll = 0.0;
    }

    fn select_article(&mut self, id: Id, doc: &mut AutoCommit) {
        self.selected = Some(id);
        self.reader_scroll = 0.0;
        if self.store.mark_read(id, true) {
            FeedsDoc::set_read(doc, id, true);
        }
    }

    fn move_selection(&mut self, delta: i64, doc: &mut AutoCommit) {
        let ids: Vec<Id> = self.store.articles_for(self.filter).iter().map(|a| a.id).collect();
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
        let id = ids[next];
        self.select_article(id, doc);
        self.scroll_selection_into_view();
    }

    fn scroll_selection_into_view(&mut self) {
        if let Some(sel) = self.selected {
            if let Some(pos) = self
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
    }

    fn toggle_star(&mut self, id: Id, doc: &mut AutoCommit) {
        if self.store.toggle_star(id) {
            let starred = self.store.article(id).map(|a| a.starred).unwrap_or(false);
            FeedsDoc::set_starred(doc, id, starred);
            self.set_toast(if starred { "Starred" } else { "Unstarred" });
        }
    }

    fn toggle_read(&mut self, doc: &mut AutoCommit) {
        if let Some(id) = self.selected {
            let now_read = self.store.article(id).map(|a| !a.read).unwrap_or(true);
            if self.store.mark_read(id, now_read) {
                FeedsDoc::set_read(doc, id, now_read);
            }
        }
    }

    fn mark_all_read(&mut self, doc: &mut AutoCommit) {
        let ids = self.store.mark_all_read(self.filter);
        for id in &ids {
            FeedsDoc::set_read(doc, *id, true);
        }
        self.set_toast(format!("Marked {} read", ids.len()));
    }

    fn toggle_folder(&mut self, id: Id, doc: &mut AutoCommit) {
        if let Some(f) = self.store.folders.iter_mut().find(|f| f.id == id) {
            f.collapsed = !f.collapsed;
            let collapsed = f.collapsed;
            FeedsDoc::set_folder_collapsed(doc, id, collapsed);
        }
    }

    // --- fetch integration ----------------------------------------------

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
        self.set_toast(format!("Refreshing {n} feeds…"));
    }

    fn subscribe(&mut self, url: String) {
        let url = url.trim().to_string();
        if url.is_empty() {
            return;
        }
        let folder = match self.filter {
            Filter::Folder(id) => Some(id),
            Filter::Feed(fid) => self.store.feed(fid).and_then(|f| f.folder),
            _ => None,
        };
        self.worker().send(FetchRequest::Subscribe { url: url.clone(), folder });
        self.set_toast(format!("Subscribing to {url}…"));
    }

    fn export_opml(&mut self) {
        let xml = opml::export(&self.store);
        let path = std::env::temp_dir().join("feeds-subscriptions.opml");
        match std::fs::write(&path, xml) {
            Ok(_) => self.set_toast(format!("Exported OPML → {}", path.display())),
            Err(e) => self.set_toast(format!("Export failed: {e}")),
        }
    }

    /// Drain any results the fetch worker produced and fold them into the
    /// store + document.
    fn drain_fetches(&mut self, doc: &mut AutoCommit) {
        let results = match &mut self.worker {
            Some(w) => w.poll(),
            None => return,
        };
        for r in results {
            match r {
                FetchResult::Articles { feed_id, title, articles } => {
                    if let (Some(t), Some(f)) =
                        (title, self.store.feeds.iter_mut().find(|f| f.id == feed_id))
                    {
                        if !t.is_empty() {
                            f.title = t;
                        }
                    }
                    let mut added = 0;
                    for a in articles {
                        if self.store.insert_article_dedup(a.clone()) {
                            FeedsDoc::upsert_article(doc, &a);
                            added += 1;
                        }
                    }
                    if let Some(f) = self.store.feed(feed_id) {
                        FeedsDoc::upsert_feed(doc, &f.clone());
                    }
                    if added > 0 {
                        self.set_toast(format!("{added} new articles"));
                    }
                }
                FetchResult::Subscribed { feed, articles } => {
                    let mut feed = feed;
                    feed.order = self.store.next_feed_order(feed.folder);
                    self.store.upsert_feed(feed.clone());
                    FeedsDoc::upsert_feed(doc, &feed);
                    for a in articles {
                        if self.store.insert_article_dedup(a.clone()) {
                            FeedsDoc::upsert_article(doc, &a);
                        }
                    }
                    self.set_toast(format!("Subscribed to {}", feed.title));
                    self.set_filter(Filter::Feed(feed.id));
                }
                FetchResult::Failed { message, .. } => {
                    self.set_toast(format!("Fetch failed: {message}"));
                }
            }
        }
    }

    // --- geometry --------------------------------------------------------

    fn recompute_geometry(&mut self) {
        let r = self.rect;
        self.sidebar_rect = Rect::new(r.x0, r.y0, r.x0 + SIDEBAR_W, r.y1);
        let right_x0 = r.x0 + SIDEBAR_W;
        self.toolbar_rect = Rect::new(right_x0, r.y0, r.x1, r.y0 + HEADER_H);
        self.list_rect = Rect::new(right_x0, r.y0, right_x0 + LIST_W, r.y1);
        self.reader_rect = Rect::new(right_x0 + LIST_W, r.y0, r.x1, r.y1);
        self.build_sidebar_rows();
        self.build_list_rows();
        self.build_toolbar();
    }

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

        push(&mut rows, &mut y, 24.0, 14.0, "LIBRARY".into(), 0, false, RowRole::Section);
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
        push(&mut rows, &mut y, 24.0, 14.0, "FEEDS".into(), 0, false, RowRole::Section);

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

    fn build_list_rows(&mut self) {
        let mut rows = Vec::new();
        let content_top = self.list_rect.y0 + HEADER_H;
        let x0 = self.list_rect.x0;
        let x1 = self.list_rect.x1;
        let articles: Vec<Id> =
            self.store.articles_for(self.filter).iter().map(|a| a.id).collect();
        self.list_content_h = articles.len() as f64 * LIST_ROW_H;
        for (i, id) in articles.iter().enumerate() {
            let top = content_top + i as f64 * LIST_ROW_H - self.list_scroll;
            let rect = Rect::new(x0, top, x1, top + LIST_ROW_H);
            let star_rect = Rect::new(x1 - 40.0, top + LIST_ROW_H - 34.0, x1 - 8.0, top + LIST_ROW_H - 2.0);
            rows.push(ListRow { rect, star_rect, id: *id });
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

    fn paint(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let c = &theme.tokens.colors;
        let base = c.surfaces.base;
        let sidebar_bg = c.neutral[0];

        // Pane backgrounds.
        scene.fill(Fill::NonZero, t, base, None, &self.rect);
        scene.fill(Fill::NonZero, t, sidebar_bg, None, &self.sidebar_rect);

        // Pane dividers.
        let divider = alpha(c.border_standard, 150);
        vline(scene, t, self.sidebar_rect.x1, self.rect.y0, self.rect.y1, divider);
        vline(scene, t, self.list_rect.x1, self.rect.y0, self.rect.y1, divider);

        draw_sidebar(scene, t, tr, theme, &self.sidebar_rect, &self.sidebar_rows);
        draw_list(
            scene, t, tr, theme, &self.list_rect, &self.list_rows, &self.store, self.selected,
            self.now, &self.filter_title(), self.store.count_for(self.filter),
        );
        let (links, open_rect, content_h) = draw_reader(
            scene,
            t,
            tr,
            theme,
            &self.reader_rect,
            self.selected.and_then(|id| self.store.article(id)),
            self.selected.and_then(|id| self.store.article(id)).and_then(|a| self.store.feed(a.feed_id)),
            self.reader_scroll,
            self.now,
        );
        self.reader_links = links;
        self.reader_open_rect = open_rect;
        self.reader_content_h = content_h;

        draw_toolbar(scene, t, tr, theme, &self.toolbar_rect, &self.toolbar_buttons);

        // Toast (auto-expires after 4s).
        if let Some((msg, since)) = &self.toast {
            if since.elapsed().as_secs_f32() < 4.0 {
                draw_toast(scene, t, tr, theme, &self.rect, msg);
            }
        }

        if let Some(Modal::AddSubscription { input }) = &self.modal {
            draw_add_modal(scene, t, tr, theme, &self.rect, input);
        }
    }

    // --- hit testing -----------------------------------------------------

    fn click(&mut self, p: Point, doc: &mut AutoCommit) -> bool {
        // Modal eats all clicks (outside = cancel).
        if self.modal.is_some() {
            return true;
        }
        // Toolbar.
        for b in self.toolbar_buttons.clone() {
            if b.rect.contains(p) {
                match b.action {
                    ToolbarAction::AddSubscription => {
                        self.modal = Some(Modal::AddSubscription { input: String::new() });
                    }
                    ToolbarAction::Refresh => self.refresh_all(),
                    ToolbarAction::MarkAllRead => self.mark_all_read(doc),
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
            match row.role {
                RowRole::Smart(SmartKind::All) => self.set_filter(Filter::All),
                RowRole::Smart(SmartKind::Unread) => self.set_filter(Filter::Unread),
                RowRole::Smart(SmartKind::Starred) => self.set_filter(Filter::Starred),
                RowRole::Folder { id, .. } => {
                    // Chevron hit (left 28px) toggles; else filter to folder.
                    if p.x < row.rect.x0 + 28.0 {
                        self.toggle_folder(id, doc);
                    } else {
                        self.set_filter(Filter::Folder(id));
                    }
                }
                RowRole::Feed { id, .. } => self.set_filter(Filter::Feed(id)),
                RowRole::Section => {}
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
        if let Some(r) = self.reader_open_rect {
            if r.contains(p) {
                self.open_selected_link();
                return true;
            }
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
    scene.stroke(&Stroke::new(1.0), t, color, None, &Line::new(Point::new(x, y0), Point::new(x, y1)));
}

fn hline(scene: &mut Scene, t: Affine, x0: f64, x1: f64, y: f64, color: Color) {
    scene.stroke(&Stroke::new(1.0), t, color, None, &Line::new(Point::new(x0, y), Point::new(x1, y)));
}

/// Draw a pill badge with a number; returns its width.
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
    let label = if n > 999 { "999+".to_string() } else { n.to_string() };
    let (tw, _) = tr.measure(&label, 11.0, None);
    let w = (tw as f64 + 14.0).max(20.0);
    let h = 18.0;
    let rect = Rect::new(right_x - w, cy - h / 2.0, right_x, cy + h / 2.0);
    scene.fill(Fill::NonZero, t, bg, None, &RoundedRect::from_rect(rect, 9.0));
    tr.draw_text(
        scene,
        &label,
        11.0,
        fg,
        t * Affine::translate((rect.x0 + (w - tw as f64) / 2.0, cy - 7.0)),
        None,
    );
    w
}

fn draw_sidebar(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    rows: &[SidebarRow],
) {
    let c = &theme.tokens.colors;
    // Header / wordmark.
    let header = Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + HEADER_H);
    tr.draw_text_weighted(
        scene,
        "Feeds",
        20.0,
        c.text_primary,
        t * Affine::translate((rect.x0 + 18.0, rect.y0 + 18.0)),
        None,
        TextWeight::Bold,
    );
    hline(scene, t, rect.x0, rect.x1, header.y1, alpha(c.border_standard, 120));

    for row in rows {
        let cy = row.rect.y0 + row.rect.height() / 2.0;
        match &row.role {
            RowRole::Section => {
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
            _ => {}
        }

        // Selection / hover background.
        if row.selected {
            let pill = Rect::new(row.rect.x0 + 6.0, row.rect.y0 + 2.0, row.rect.x1 - 6.0, row.rect.y1 - 2.0);
            scene.fill(Fill::NonZero, t, alpha(c.accent[5], 46), None, &RoundedRect::from_rect(pill, 8.0));
        }

        let label_color = if row.selected { c.text_primary } else { alpha(c.text_primary, 220) };
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
                    SmartKind::Starred => icons::star(scene, t, center, 6.0, true, c.semantic.warning),
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
                icons::dot(scene, t, Point::new(row.rect.x0 + row.indent - 10.0, cy), 2.5, dot_color);
            }
            _ => {}
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
        let weight = if row.badge > 0 { TextWeight::Semibold } else { TextWeight::Regular };
        let size = if matches!(row.role, RowRole::Feed { .. }) { 13.0 } else { 14.0 };
        let label = elide(tr, &row.label, size, max_w);
        tr.draw_text_weighted(
            scene,
            &label,
            size,
            label_color,
            t * Affine::translate((text_x, cy - (size as f64) * 0.62)),
            Some(max_w),
            weight,
        );
    }
}

#[allow(clippy::too_many_arguments)]
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
) {
    let c = &theme.tokens.colors;
    // Header.
    tr.draw_text_weighted(
        scene,
        title,
        17.0,
        c.text_primary,
        t * Affine::translate((rect.x0 + 18.0, rect.y0 + 16.0)),
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
        t * Affine::translate((rect.x0 + 18.0 + title_w0 + 10.0, rect.y0 + 19.0)),
        None,
    );
    let content = Rect::new(rect.x0, rect.y0 + HEADER_H, rect.x1, rect.y1);
    hline(scene, t, rect.x0, rect.x1, content.y0, alpha(c.border_standard, 120));

    if rows.is_empty() {
        tr.draw_text(
            scene,
            "Nothing here — you're all caught up.",
            14.0,
            c.text_dim,
            t * Affine::translate((rect.x0 + 18.0, content.y0 + 24.0)),
            None,
        );
        return;
    }

    // Clip the scrolling rows to the content area.
    scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &content);
    for lr in rows {
        if lr.rect.y1 < content.y0 || lr.rect.y0 > content.y1 {
            continue;
        }
        let Some(a) = store.article(lr.id) else { continue };
        let is_sel = selected == Some(lr.id);
        if is_sel {
            scene.fill(Fill::NonZero, t, alpha(c.accent[5], 38), None, &lr.rect);
            let bar = Rect::new(lr.rect.x0, lr.rect.y0, lr.rect.x0 + 3.0, lr.rect.y1);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
        }
        hline(scene, t, lr.rect.x0 + 14.0, lr.rect.x1, lr.rect.y1, alpha(c.border_standard, 70));

        // Unread / read marker.
        let mcy = lr.rect.y0 + 22.0;
        if a.read {
            icons::ring(scene, t, Point::new(lr.rect.x0 + 18.0, mcy), 3.5, alpha(c.text_dim, 150));
        } else {
            icons::dot(scene, t, Point::new(lr.rect.x0 + 18.0, mcy), 4.0, c.accent[5]);
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
        let title_color = if a.read { alpha(c.text_primary, 170) } else { c.text_primary };
        let title_weight = if a.read { TextWeight::Regular } else { TextWeight::Semibold };
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
        let source = store.feed(a.feed_id).map(|f| f.title.as_str()).unwrap_or("");
        let meta = if a.summary.is_empty() {
            source.to_string()
        } else {
            format!("{source}  ·  {}", a.summary)
        };
        let meta_w = (lr.rect.x1 - right_pad - text_x).max(60.0) as f32;
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
            if a.starred { c.semantic.warning } else { alpha(c.text_dim, 160) },
        );
    }
    scene.pop_layer();
}

#[allow(clippy::too_many_arguments)]
fn draw_reader(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    article: Option<&Article>,
    feed: Option<&Feed>,
    scroll: f64,
    now: i64,
) -> (Vec<(Rect, String)>, Option<Rect>, f64) {
    let c = &theme.tokens.colors;
    let mut links = Vec::new();
    let mut open_rect = None;

    let Some(a) = article else {
        tr.draw_text(
            scene,
            "Select an article to read",
            16.0,
            c.text_dim,
            t * Affine::translate((rect.x0 + 48.0, rect.y0 + 64.0)),
            None,
        );
        return (links, open_rect, 0.0);
    };

    let pad = 48.0;
    let content_x = rect.x0 + pad;
    let content_w = (rect.width() - pad * 2.0).max(120.0) as f32;
    // The shared toolbar bar occupies the top HEADER_H of this pane, so the
    // article body begins beneath it. Clip the scrolling content to the
    // area below the toolbar.
    let top = rect.y0 + HEADER_H;
    let body = Rect::new(rect.x0, top, rect.x1, rect.y1);
    let content_top = top + 24.0;
    scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, t, &body);
    let mut y = content_top - scroll;

    // Kicker: feed name (uppercase accent).
    if let Some(f) = feed {
        let kicker = f.title.to_uppercase();
        tr.draw_text_weighted(
            scene,
            &kicker,
            11.5,
            c.accent[6],
            t * Affine::translate((content_x, y)),
            None,
            TextWeight::Semibold,
        );
        // Feed-kind tag on the right.
        let tag = f.kind.label();
        let tw = measure_w(tr, tag, 10.5);
        let tagx = rect.x1 - pad - tw - 14.0;
        let tag_rect = Rect::new(tagx, y - 2.0, tagx + tw + 14.0, y + 16.0);
        scene.fill(Fill::NonZero, t, alpha(c.neutral[3], 150), None, &RoundedRect::from_rect(tag_rect, 6.0));
        tr.draw_text(scene, tag, 10.5, c.text_dim, t * Affine::translate((tagx + 7.0, y)), None);
        y += 24.0;
    }

    // Title.
    let (_tw, th) = tr.measure_weighted(&a.title, 26.0, Some(content_w), TextWeight::Bold);
    tr.draw_text_weighted(
        scene,
        &a.title,
        26.0,
        c.text_primary,
        t * Affine::translate((content_x, y)),
        Some(content_w),
        TextWeight::Bold,
    );
    y += th as f64 + 10.0;

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
        tr.draw_text(scene, &byline, 13.0, c.text_dim, t * Affine::translate((content_x, y)), None);
    }
    // "Open original" affordance on the right.
    if !a.url.is_empty() {
        let label = "Open original";
        let lw = measure_w(tr, label, 13.0);
        let ox = rect.x1 - pad - lw - 22.0;
        tr.draw_text(scene, label, 13.0, c.accent[6], t * Affine::translate((ox, y)), None);
        icons::external_link(scene, t, Point::new(rect.x1 - pad - 7.0, y + 7.0), 5.0, c.accent[6]);
        open_rect = Some(Rect::new(ox - 4.0, y - 4.0, rect.x1 - pad + 4.0, y + 20.0));
    }
    y += 24.0;
    hline(scene, t, content_x, rect.x1 - pad, y, alpha(c.border_standard, 140));
    y += 22.0;

    // Body blocks.
    for block in &a.blocks {
        y = draw_block(scene, t, tr, theme, block, content_x, content_w, y, rect, &mut links);
    }
    y += 40.0;
    scene.pop_layer();

    let content_h = (y + scroll) - content_top;
    (links, open_rect, content_h)
}

#[allow(clippy::too_many_arguments)]
fn draw_block(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    block: &crate::model::ContentBlock,
    x: f64,
    w: f32,
    y: f64,
    pane: &Rect,
    links: &mut Vec<(Rect, String)>,
) -> f64 {
    let c = &theme.tokens.colors;
    match block.kind {
        BlockKind::Heading1 | BlockKind::Heading2 | BlockKind::Heading3 => {
            let size = match block.kind {
                BlockKind::Heading1 => 22.0,
                BlockKind::Heading2 => 19.0,
                _ => 16.0,
            };
            let (_w, h) = tr.measure_weighted(&block.text, size, Some(w), TextWeight::Semibold);
            tr.draw_text_weighted(
                scene,
                &block.text,
                size,
                c.text_primary,
                t * Affine::translate((x, y + 6.0)),
                Some(w),
                TextWeight::Semibold,
            );
            y + h as f64 + 12.0
        }
        BlockKind::Paragraph => {
            let (_w, h) = tr.measure(&block.text, 15.0, Some(w));
            tr.draw_text(
                scene,
                &block.text,
                15.0,
                alpha(c.text_primary, 224),
                t * Affine::translate((x, y)),
                Some(w),
            );
            y + h as f64 + 14.0
        }
        BlockKind::Bullet => {
            icons::dot(scene, t, Point::new(x + 6.0, y + 8.0), 2.5, c.accent[6]);
            let tw = w - 22.0;
            let (_w, h) = tr.measure(&block.text, 15.0, Some(tw));
            tr.draw_text(
                scene,
                &block.text,
                15.0,
                alpha(c.text_primary, 224),
                t * Affine::translate((x + 22.0, y)),
                Some(tw),
            );
            y + h as f64 + 8.0
        }
        BlockKind::Quote => {
            let tw = w - 24.0;
            let (_w, h) = tr.measure_styled(&block.text, 15.0, Some(tw), TextWeight::Regular, true, false);
            let bar = Rect::new(x, y, x + 3.0, y + h as f64 + 4.0);
            scene.fill(Fill::NonZero, t, c.accent[5], None, &bar);
            tr.draw_text_styled(
                scene,
                &block.text,
                15.0,
                c.text_dim,
                t * Affine::translate((x + 18.0, y)),
                Some(tw),
                TextWeight::Regular,
                true,
                false,
            );
            y + h as f64 + 16.0
        }
        BlockKind::Code => {
            let inner = w - 28.0;
            let (_w, h) = tr.measure_styled(&block.text, 13.0, Some(inner), TextWeight::Regular, false, true);
            let box_rect = Rect::new(x, y, x + w as f64, y + h as f64 + 20.0);
            scene.fill(Fill::NonZero, t, c.neutral[0], None, &RoundedRect::from_rect(box_rect, 8.0));
            scene.stroke(&Stroke::new(1.0), t, alpha(c.border_standard, 120), None, &RoundedRect::from_rect(box_rect, 8.0));
            tr.draw_text_styled(
                scene,
                &block.text,
                13.0,
                alpha(c.text_primary, 230),
                t * Affine::translate((x + 14.0, y + 10.0)),
                Some(inner),
                TextWeight::Regular,
                false,
                true,
            );
            y + h as f64 + 30.0
        }
        BlockKind::Link => {
            let label = &block.text;
            let (lw, h) = tr.measure(label, 15.0, Some(w));
            tr.draw_text(
                scene,
                label,
                15.0,
                theme.tokens.colors.accent[6],
                t * Affine::translate((x, y)),
                Some(w),
            );
            // Underline.
            let uw = (lw as f64).min(w as f64);
            hline(scene, t, x, x + uw, y + h as f64 - 1.0, theme.tokens.colors.accent[6]);
            if let Some(href) = &block.href {
                let rect = Rect::new(x, y, x + uw, y + h as f64);
                // Only register if visible within the pane.
                if rect.y1 > pane.y0 && rect.y0 < pane.y1 {
                    links.push((rect, href.clone()));
                }
            }
            y + h as f64 + 10.0
        }
        BlockKind::Divider => {
            hline(scene, t, x, x + w as f64, y + 8.0, alpha(c.border_standard, 160));
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
    hline(scene, t, rect.x0, rect.x1, rect.y1, alpha(c.border_standard, 120));
    for b in buttons {
        let center = b.rect.center();
        let bg = alpha(c.neutral[2], 160);
        scene.fill(Fill::NonZero, t, bg, None, &RoundedRect::from_rect(b.rect, 9.0));
        scene.stroke(&Stroke::new(1.0), t, alpha(c.border_standard, 120), None, &RoundedRect::from_rect(b.rect, 9.0));
        let col = c.text_primary;
        match b.action {
            ToolbarAction::AddSubscription => icons::plus(scene, t, center, 7.0, c.accent[6]),
            ToolbarAction::Refresh => icons::refresh(scene, t, center, 8.0, col),
            ToolbarAction::MarkAllRead => icons::check(scene, t, center, 7.0, col),
            ToolbarAction::ExportOpml => icons::external_link(scene, t, center, 6.0, col),
        }
        let _ = tr; // tooltips are not drawn statically
    }
}

fn draw_toast(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    msg: &str,
) {
    let c = &theme.tokens.colors;
    let (tw, _) = tr.measure(msg, 13.0, None);
    let w = tw as f64 + 36.0;
    let h = 38.0;
    let x = rect.x1 - w - 24.0;
    let y = rect.y1 - h - 24.0;
    let r = Rect::new(x, y, x + w, y + h);
    scene.fill(Fill::NonZero, t, c.neutral[2], None, &RoundedRect::from_rect(r, 10.0));
    scene.stroke(&Stroke::new(1.0), t, alpha(c.border_focus, 160), None, &RoundedRect::from_rect(r, 10.0));
    tr.draw_text(scene, msg, 13.0, c.text_primary, t * Affine::translate((x + 18.0, y + 11.0)), None);
}

fn draw_add_modal(
    scene: &mut Scene,
    t: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: &Rect,
    input: &str,
) {
    let c = &theme.tokens.colors;
    // Scrim.
    scene.fill(Fill::NonZero, t, c.surfaces.overlay, None, rect);

    let w = 520.0;
    let h = 190.0;
    let cx = (rect.x0 + rect.x1) / 2.0;
    let x = cx - w / 2.0;
    let y = rect.y0 + 150.0;
    let card = Rect::new(x, y, x + w, y + h);
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &RoundedRect::from_rect(card, 14.0));
    scene.stroke(&Stroke::new(1.0), t, alpha(c.border_standard, 180), None, &RoundedRect::from_rect(card, 14.0));

    tr.draw_text_weighted(
        scene,
        "Add subscription",
        18.0,
        c.text_primary,
        t * Affine::translate((x + 24.0, y + 20.0)),
        None,
        TextWeight::Bold,
    );
    tr.draw_text(
        scene,
        "Paste a feed or site URL (RSS, Atom, or gemini://).",
        13.0,
        c.text_dim,
        t * Affine::translate((x + 24.0, y + 48.0)),
        None,
    );

    // Input field.
    let field = Rect::new(x + 24.0, y + 78.0, x + w - 24.0, y + 78.0 + 40.0);
    scene.fill(Fill::NonZero, t, c.surfaces.base, None, &RoundedRect::from_rect(field, 8.0));
    scene.stroke(&Stroke::new(1.5), t, c.border_focus, None, &RoundedRect::from_rect(field, 8.0));
    let shown = if input.is_empty() {
        "https://".to_string()
    } else {
        input.to_string()
    };
    let color = if input.is_empty() { alpha(c.text_dim, 160) } else { c.text_primary };
    tr.draw_text(scene, &shown, 14.0, color, t * Affine::translate((field.x0 + 12.0, field.y0 + 11.0)), None);
    // Caret.
    let cw = measure_w(tr, &shown, 14.0);
    if !input.is_empty() {
        vline(scene, t, field.x0 + 12.0 + cw + 1.0, field.y0 + 9.0, field.y1 - 9.0, c.accent[5]);
    }

    // Hint.
    tr.draw_text(
        scene,
        "Enter to subscribe · Esc to cancel",
        12.0,
        c.text_dim,
        t * Affine::translate((x + 24.0, y + h - 26.0)),
        None,
    );
}

// --- text helpers -----------------------------------------------------

fn measure_w(tr: &mut TextRenderer, s: &str, size: f32) -> f64 {
    tr.measure(s, size, None).0 as f64
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
        let candidate = format!("{}…", words[..take].join(" "));
        if tr.measure(&candidate, size, Some(max_w)).1 <= line_h * 2.0 + 1.0 {
            return candidate;
        }
    }
    s.to_string()
}

fn chrono_date(published: i64, now: i64) -> String {
    if published <= 0 {
        return String::new();
    }
    use chrono::{TimeZone, Utc};
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
                FeedsDoc::seed(state, &self.store);
            } else {
                self.store = FeedsDoc::load(state);
                self.ensure_selection();
                if self.selected.is_none() {
                    self.select_featured();
                }
            }
            self.synced = true;
        }
        self.drain_fetches(state);
    }

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
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
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
        self.recompute_geometry();
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
        _modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
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
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (*y as f64) * 48.0,
                    MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                };
                if self.reader_rect.contains(self.last_mouse) {
                    let visible = (self.reader_rect.height() - HEADER_H).max(0.0);
                    let max = (self.reader_content_h - visible + 60.0).max(0.0);
                    self.reader_scroll = (self.reader_scroll - dy).clamp(0.0, max);
                } else if self.list_rect.contains(self.last_mouse) {
                    let max = (self.list_content_h - (self.list_rect.height() - HEADER_H)).max(0.0);
                    self.list_scroll = (self.list_scroll - dy).clamp(0.0, max);
                }
                *handled = true;
            }
            WindowEvent::KeyboardInput { event: key_event, .. }
                if key_event.state == ElementState::Pressed =>
            {
                self.handle_key(key_event, state, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "FeedsApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep frames flowing while network fetches are outstanding so the
        // `sync` drain runs, and briefly while a toast is visible.
        self.worker.as_ref().map(|w| w.pending() > 0).unwrap_or(false)
            || self
                .toast
                .as_ref()
                .map(|(_, since)| since.elapsed().as_secs_f32() < 4.0)
                .unwrap_or(false)
    }
}

impl FeedsApp {
    fn handle_key(
        &mut self,
        key_event: &winit::event::KeyEvent,
        state: &mut AutoCommit,
        handled: &mut bool,
    ) {
        // Modal: capture text entry.
        if let Some(Modal::AddSubscription { input }) = &mut self.modal {
            match &key_event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.modal = None;
                }
                Key::Named(NamedKey::Enter) => {
                    let url = input.clone();
                    self.modal = None;
                    self.subscribe(url);
                }
                Key::Named(NamedKey::Backspace) => {
                    input.pop();
                }
                Key::Named(NamedKey::Space) => {
                    input.push(' ');
                }
                Key::Character(s) => {
                    input.push_str(s.as_str());
                }
                _ => {}
            }
            *handled = true;
            return;
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
                        self.modal = Some(Modal::AddSubscription { input: String::new() });
                        *handled = true;
                    }
                    "e" => {
                        self.export_opml();
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
mod tests {
    use super::*;

    fn app() -> FeedsApp {
        FeedsApp::new()
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
        assert!(FeedsDoc::load(&doc).articles.iter().find(|x| x.id == unread).unwrap().read);
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
        let doc_star = FeedsDoc::load(&doc).articles.iter().find(|x| x.id == id).unwrap().starred;
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
}
