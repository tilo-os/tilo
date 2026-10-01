//! The three content/menu panes of the mail client, expressed as one widget
//! parameterised by [`Role`] so the [`ui::widget::NavShell`] can own three
//! distinct child `Element`s that all share one [`MailState`].
//!
//! * [`Role::Sidebar`] — the menu (accounts + folders + compose).
//! * [`Role::List`] — the message overview (search + threaded rows).
//! * [`Role::Reader`] — the message detail (headers + body + actions).
//!
//! Each pane draws into the rect the shell assigns it, builds its own hit list
//! relative to that rect, mutates the shared state on interaction, and raises
//! [`Intent`]s for anything that needs a sibling pane or an overlay.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::{Button, ButtonFace, EmptyState, EmptyStateContent, TextInput, WebView};

use crate::account::AccountStore;
use ui::paint::PaintCtx;
use ui::surface::{card, edge_on, pill, separator};

use crate::draw::{
    ACCOUNT_HEADER_H, AvatarPaintContext, BACK_CLEARANCE, CHIP_CLEARANCE, CHIP_INSET, CHIP_SIZE,
    FOLDER_ROW_H, HEADER_H, LIST_ROW_H, draw_avatar, elide, fill_rect, fill_round, stroke_round,
};
use crate::model::{Addr, Folder, Message, MessageId};
use crate::state::{BulkAction, CtxAction, Intent, MailState, Shared};
use crate::util::{format_absolute, format_relative};

/// Distance (px) a horizontal drag must pass to fire a swipe action.
const SWIPE_THRESHOLD: f64 = 72.0;
/// Reader width (px) below which the action bar switches to icon-only buttons.
const READER_COMPACT_W: f64 = 480.0;
/// Height (px) of the "Add account" row at the foot of the sidebar account list.
const ADD_ACCOUNT_ROW_H: f64 = 38.0;

/// Honest, actionable copy for each way the message list can be empty. Kept
/// outside `draw_list` so search, folder and first-run states cannot collapse
/// back into one vague "No messages" line.
fn list_zero_state(
    has_messages: bool,
    has_accounts: bool,
    search: &str,
) -> (Icon, &'static str, &'static str) {
    if has_messages && search.is_empty() {
        (
            Icon::Inbox,
            "No messages in this folder",
            "Messages moved here will appear in this list.",
        )
    } else if has_messages {
        (
            Icon::Search,
            "No matching messages",
            "Try another sender, subject, or keyword.",
        )
    } else if !has_accounts {
        (
            Icon::Mail,
            "Connect your email",
            "Use Add account in the sidebar to start sending and receiving messages.",
        )
    } else {
        (
            Icon::Inbox,
            "Your inbox is ready",
            "New messages will appear here after the next sync.",
        )
    }
}

/// Header title placement for the list pane: the x where the folder title
/// starts, the y (top of its text box) that vertically centres it on the
/// `NavShell` collapse chip beside it, and the shared centre line (== the chip's
/// vertical centre). `chip_here` is whether the chip overlays this pane's
/// header. Pure so the "title lines up with, and is spaced from, the collapse
/// button" invariant is unit-testable.
fn list_header_layout(
    rect: Rect,
    chip: Rect,
    chip_here: bool,
    gap: f64,
    title_h: f64,
) -> (f64, f64, f64) {
    // `chip` is the shell's collapse chip as laid out (or the 1x mirror,
    // `mirrored_chip`, when the shell shows none); its centre line is:
    let header_cy = chip.center().y;
    let title_x = if chip_here {
        chip.x1 + gap
    } else {
        rect.x0 + 20.0
    };
    // Text is positioned by the top of its line box, so drop the title top by
    // half its *measured* box (`title_h`) to centre the glyphs on `header_cy`
    // at every text scale.
    let title_y = header_cy - title_h / 2.0;
    (title_x, title_y, header_cy)
}

/// The 1x mirror of the shell's collapse chip: a `CHIP_SIZE` square inset
/// `CHIP_INSET` from the pane's top-left. Used only while the shell has not
/// published its real chip (see `State::shell_chip`).
fn mirrored_chip(rect: Rect) -> Rect {
    Rect::new(
        rect.x0 + CHIP_INSET,
        rect.y0 + CHIP_INSET,
        rect.x0 + CHIP_INSET + CHIP_SIZE,
        rect.y0 + CHIP_INSET + CHIP_SIZE,
    )
}

/// The per-account settings gear rect on the right edge of an account row.
fn account_gear_rect(header: Rect) -> Rect {
    let s = 30.0;
    let cy = header.y0 + ACCOUNT_HEADER_H / 2.0;
    Rect::new(
        header.x1 - s - 6.0,
        cy - s / 2.0,
        header.x1 - 6.0,
        cy + s / 2.0,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Sidebar,
    List,
    Reader,
}

#[derive(Clone)]
enum Hit {
    // sidebar
    Compose,
    Account(usize),
    AccountToggle(usize),
    /// The per-account settings affordance (gear on the account row). Scoped to
    /// that account, not the active one.
    AccountSettings(usize),
    /// The "Add account" action at the foot of the account list.
    AddAccount,
    Folder(usize, Folder),
    // list
    Row(MessageId),
    RowStar(MessageId),
    SearchFocus,
    SearchClear,
    Refresh,
    ToggleBulkSelection,
    SelectAllVisible,
    Bulk(BulkAction),
    // reader
    Reply,
    Archive,
    Delete,
    Star,
    Unread,
}

#[derive(Clone, Copy)]
struct SwipeState {
    id: MessageId,
    dx: f64,
}

/// Where and for which message to draw [`Pane::draw_card_header`]: the
/// message and its card's top-left origin + width.
#[derive(Clone, Copy)]
struct CardHeaderArgs<'a> {
    msg: &'a Message,
    x: f64,
    y: f64,
    width: f32,
}

/// Measured inputs for [`card_header_layout`]: the card's origin and width,
/// the avatar radius, and the line boxes / widths measured with the styles
/// the header paints with.
#[derive(Clone, Copy, Debug)]
struct CardHeaderMetrics {
    x: f64,
    y: f64,
    width: f64,
    avatar_r: f64,
    name_h: f64,
    email_h: f64,
    date_w: f64,
    to_w: f64,
    gap: f64,
}

/// Where a message card's header pieces land.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CardHeaderLayout {
    avatar_c: Point,
    /// Left edge of the name/email column.
    text_x: f64,
    name_y: f64,
    name_h: f64,
    email_y: f64,
    email_h: f64,
    /// Width the name and email may occupy before eliding: up to the start
    /// of the right column minus a gap.
    from_budget: f64,
    /// Left edges of the right-aligned date and recipients lines.
    date_x: f64,
    to_x: f64,
    /// Bottom of the header (where the body starts).
    bottom: f64,
}

/// Lay out a message card header. The name+email block is centred on the
/// avatar; the date and the "to …" line are both right-aligned to the card's
/// right edge (one shared edge, not a fixed 150px column), and the name/email
/// budget stops one gap before whichever of them reaches further left.
fn card_header_layout(m: &CardHeaderMetrics) -> CardHeaderLayout {
    let avatar_c = Point::new(m.x + m.avatar_r + 1.0, m.y + m.avatar_r + 1.0);
    let text_x = m.x + 2.0 * m.avatar_r + 10.0;
    let block_h = m.name_h + m.email_h;
    let name_y = avatar_c.y - block_h / 2.0;
    let email_y = name_y + m.name_h;
    let right = m.x + m.width;
    let date_x = right - m.date_w;
    let to_x = right - m.to_w;
    let from_budget = (date_x.min(to_x) - m.gap - text_x).max(0.0);
    let block_bottom = (email_y + m.email_h).max(avatar_c.y + m.avatar_r);
    // The 1× header keeps its authored 48px rhythm; taller text pushes the
    // body down instead of painting the first paragraph over the email line.
    let bottom = (block_bottom + m.gap).max(m.y + 48.0);
    CardHeaderLayout {
        avatar_c,
        text_x,
        name_y,
        name_h: m.name_h,
        email_y,
        email_h: m.email_h,
        from_budget,
        date_x,
        to_x,
        bottom,
    }
}

/// Vertical bands of one message-list row, measured from the styles the row
/// paints with and shared by the hit list and the painter so the star the
/// pointer hits is the star that was drawn, at every text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ListRowMetrics {
    row_h: f64,
    /// Top of the sender line and its line box.
    name_y: f64,
    name_h: f64,
    /// Top of the subject line and its line box.
    subj_y: f64,
    subj_h: f64,
    /// Top of the snippet line and its line box.
    snippet_y: f64,
    snippet_h: f64,
    /// Avatar centre: on the sender + subject block.
    avatar_cy: f64,
    /// Centre line of the star / paperclip glyphs: the snippet line.
    icon_cy: f64,
    /// Radius of the sender avatar. The shared `Avatar::draw_at` sizes the
    /// initials at 0.4× the disc through the renderer, which applies the text
    /// scale, so the disc follows the same scale to keep them inside.
    avatar_r: f64,
    /// Height of the header's search box and refresh button: the search
    /// text's measured line box plus a pad each side, never shorter than the
    /// 1× control.
    ctl_h: f64,
    /// Height of the list header band the rows start under: the controls
    /// plus their inset above and below, never shorter than the 1× header.
    header_h: f64,
}

/// 1× radius of the list-row sender avatar.
const LIST_AVATAR_R: f64 = 17.0;
/// 1× height of the header's search box and refresh button.
const HEADER_CTL_H: f64 = 32.0;
/// Inset of the header controls from the pane's top edge (`s3`).
const HEADER_CTL_INSET: f64 = 12.0;
/// Renderer-relative size of the search box's text and placeholder.
const SEARCH_TEXT: f32 = 13.0;

impl ListRowMetrics {
    /// The 1× row before anything has been measured (and for the geometry
    /// tests, which never paint): the same rule applied to the 1× line boxes.
    fn fallback() -> Self {
        Self::from_line_boxes(16.875, 15.625, 14.375, 4.0, 12.0)
    }

    /// Measure the three text lines with their paint styles, and the
    /// header control / avatar sizes for the current text scale.
    fn measure(tr: &mut TextRenderer, theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let (_, name_h) = tr.measure_weighted("Ag", 13.5, None, TextWeight::Bold);
        let (_, subj_h) = tr.measure_weighted("Ag", 12.5, None, TextWeight::Semibold);
        let (_, snippet_h) = tr.measure("Ag", 11.5, None);
        let (_, search_h) = tr.measure("Ag", SEARCH_TEXT, None);
        let pad = f64::from(sp.s3);
        let mut m = Self::from_line_boxes(
            f64::from(name_h),
            f64::from(subj_h),
            f64::from(snippet_h),
            f64::from(sp.s1),
            pad,
        );
        m.avatar_r = LIST_AVATAR_R * f64::from(theme.text_scale).max(1.0);
        m.row_h = m.row_h.max((2.0 * m.avatar_r + 2.0 * pad).ceil());
        m.ctl_h = (f64::from(search_h) + 2.0 * f64::from(sp.s2)).max(HEADER_CTL_H);
        m.header_h = (2.0 * HEADER_CTL_INSET + m.ctl_h).max(HEADER_H);
        m
    }

    /// Stack sender, subject and snippet with the small related gap between
    /// them and the row padding above and below; the row is never shorter
    /// than the authored 1× row and is rounded up to whole pixels so its
    /// hairline stays crisp.
    fn from_line_boxes(name_h: f64, subj_h: f64, snippet_h: f64, gap: f64, pad: f64) -> Self {
        let name_y = pad;
        let subj_y = name_y + name_h + gap;
        let snippet_y = subj_y + subj_h + gap;
        let row_h = (snippet_y + snippet_h + pad).ceil().max(LIST_ROW_H);
        Self {
            row_h,
            name_y,
            name_h,
            subj_y,
            subj_h,
            snippet_y,
            snippet_h,
            avatar_cy: (name_y + subj_y + subj_h) / 2.0,
            icon_cy: snippet_y + snippet_h / 2.0,
            avatar_r: LIST_AVATAR_R,
            ctl_h: HEADER_CTL_H,
            header_h: HEADER_H,
        }
    }

    /// Centre x of the sender avatar in a row starting at `row_x0`.
    fn avatar_cx(&self, row_x0: f64) -> f64 {
        row_x0 + 25.0 + self.avatar_r
    }

    /// Left edge of the sender / subject / snippet column: one `s3` past the
    /// disc, so all three lines start together whatever the disc's radius.
    fn text_x(&self, row_x0: f64, s3: f64) -> f64 {
        self.avatar_cx(row_x0) + self.avatar_r + s3
    }

    /// The star's hit rect for a row: a 24px target centred on the painted
    /// glyph (`right - 24 .. right - 8` on the snippet line).
    fn star_hit(&self, row: Rect) -> Rect {
        let right = row.x1 - 14.0;
        let cy = row.y0 + self.icon_cy;
        Rect::new(right - 28.0, cy - 12.0, right - 4.0, cy + 12.0)
    }
}

/// Everything [`Pane::draw_list_row`] needs about one row that
/// [`Pane::draw_list`] has already resolved: the message, its slot rect, its
/// thread size, and its selection/hover/swipe state.
#[derive(Clone, Copy)]
struct ListRowArgs<'a> {
    msg: &'a Message,
    rect: Rect,
    len: usize,
    selected: bool,
    bulk_selected: bool,
    hov: bool,
    swipe_dx: f64,
    now_min: i64,
}

/// Shared immediate-mode paint resources for focused pane helpers.
struct DrawContext<'a> {
    scene: &'a mut Scene,
    transform: Affine,
    text: &'a mut TextRenderer,
    theme: &'a Theme,
}

struct ReaderThreadLayout {
    reader: Rect,
    content_x: f64,
    content_w: f32,
    y: f64,
    head_has_html: bool,
}

struct LargeTextFlow {
    bounds: Rect,
    x: f64,
    width: f32,
    y: f64,
}

struct EventContext<'a> {
    scale_factor: f64,
    modifiers: winit::event::Modifiers,
    state: &'a mut AutoCommit,
    path: &'a ObjId,
    handled: &'a mut bool,
    focus_target: &'a mut Option<NodeId>,
    node_id: NodeId,
    taffy: &'a TaffyTree<()>,
}

pub struct Pane {
    role: Role,
    shared: Shared,
    rect: Rect,
    /// Captured from the host theme during the inner `NavShell` layout pass.
    /// Canvas text is scaled by the shared renderer; this companion value
    /// expands Mail's custom row/control geometry so 200% text has room.
    layout_scale: f64,
    hits: Vec<(Rect, Hit)>,
    /// List pane only: the search field.
    search_input: Option<TextInput>,
    /// List pane only: the row bands measured on the last paint, read by the
    /// hit list. `None` until the first paint, when the 1× rule stands in.
    list_metrics: Option<ListRowMetrics>,
    // list swipe state
    swipe: Option<SwipeState>,
    mouse_down_row: Option<(MessageId, Point)>,
    /// Reader pane only: attachment chip hit regions `(rect, message, index)`,
    /// resolved during draw (the card layout is only known then). Clicking one
    /// saves the real attachment bytes to disk.
    reader_attachment_hits: Vec<(Rect, MessageId, usize)>,
    /// Reader pane only: the sandboxed `WebView` that renders an HTML mail body,
    /// its engine, and the (message, `block_remote`) key currently loaded into it.
    /// Created lazily on the first HTML mail so text-only usage stays webview-free.
    html_engine: Option<SharedEngine>,
    /// True only when the engine is a real isolated browser renderer. The
    /// deterministic test engine is useful for plumbing tests but must never be
    /// presented as message content.
    html_engine_live: bool,
    html_view: Option<WebView>,
    html_key: Option<(MessageId, bool)>,
    /// The message the HTML view currently shows — distinct from `html_key`
    /// (which the remote-images toggle clears to force a reload). Used to detect
    /// a genuine message change so the remote-content opt-in resets per mail.
    html_shown_id: Option<MessageId>,
    /// Whether the user has opted in to remote images for the open message.
    /// Reset whenever a different message is shown, so remote content is blocked
    /// again by default for each mail.
    html_load_remote: bool,
    /// The rect the HTML body view occupies this frame (for scroll forwarding),
    /// and the "Load remote images" toggle chip rect (for click handling).
    html_view_rect: Option<Rect>,
    html_toggle_rect: Option<Rect>,
    dirty: bool,
}

impl Pane {
    pub fn new(role: Role, shared: Shared) -> Self {
        let search_input = if role == Role::List {
            // The placeholder belongs to the shared field so the hint and the
            // typed query share one font, gutter and centre line (the header
            // used to paint its own "Search" over the box at a fixed offset).
            // The shared field resolves the catalogue key itself, so the key
            // is handed over rather than a resolved string (which it would
            // translate a second time).
            let mut i = TextInput::local().with_placeholder("mail-search-placeholder");
            i.set_plain_chrome(true);
            Some(i)
        } else {
            None
        };
        Self {
            role,
            shared,
            rect: Rect::ZERO,
            layout_scale: 1.0,
            hits: Vec::new(),
            search_input,
            list_metrics: None,
            swipe: None,
            mouse_down_row: None,
            reader_attachment_hits: Vec::new(),
            html_engine: None,
            html_engine_live: false,
            html_view: None,
            html_key: None,
            html_shown_id: None,
            html_load_remote: false,
            html_view_rect: None,
            html_toggle_rect: None,
            dirty: true,
        }
    }

    pub fn boxed(role: Role, shared: Shared) -> Box<dyn Widget> {
        Box::new(Self::new(role, shared))
    }

    // ---- geometry / hit-list, recomputed from the current rect + state ----

    /// The list row bands from the last paint, or the 1× rule before it.
    fn list_metrics(&self) -> ListRowMetrics {
        self.list_metrics.unwrap_or_else(ListRowMetrics::fallback)
    }

    /// The shell's collapse chip in app coordinates, or the 1x mirror at
    /// this pane's top-left while the shell shows none.
    fn shell_chip(&self, rect: Rect) -> Rect {
        self.shared
            .borrow()
            .shell_chip
            .unwrap_or_else(|| mirrored_chip(rect))
    }

    /// Leading room the shell's chip needs in this pane's header: past the
    /// chip's right edge by the same breath the 1x constant carried.
    fn chip_clearance(&self) -> f64 {
        let chip = self.shell_chip(self.rect);
        (chip.x1 - self.rect.x0) + (CHIP_CLEARANCE - CHIP_INSET - CHIP_SIZE)
    }

    fn scale(&self) -> f64 {
        self.layout_scale.clamp(0.5, 4.0)
    }

    fn header_h(&self) -> f64 {
        self.list_metrics().header_h
    }

    /// A local selection is a second, compact toolbar below the ordinary
    /// folder/search header.  It never changes the navigation shell's pane
    /// semantics; it only reserves enough space that list rows cannot paint
    /// under its actions at large text sizes.
    fn list_content_y(&self) -> f64 {
        let selecting = self.role == Role::List && self.shared.borrow().bulk_selecting;
        self.rect.y0 + self.header_h() + if selecting { 140.0 * self.scale() } else { 0.0 }
    }

    fn list_row_h(&self) -> f64 {
        self.list_metrics().row_h
    }

    fn header_left_inset(&self) -> f64 {
        let st = self.shared.borrow();
        match self.role {
            Role::Sidebar => self.chip_clearance(),
            // The list is the leftmost content pane (so the shell's collapse /
            // hamburger toggle sits over its header) whenever the layout is
            // narrow OR the docked sidebar is collapsed. Reserve room so the
            // folder title never renders under the toggle chip.
            Role::List => {
                if st.narrow || !st.sidebar_open {
                    self.chip_clearance()
                } else {
                    20.0
                }
            }
            Role::Reader => {
                if st.narrow && st.detail_pushed {
                    BACK_CLEARANCE
                } else {
                    20.0
                }
            }
        }
    }

    /// The manual sync / refresh button in the list header (right edge).
    fn refresh_box_rect(&self) -> Rect {
        let r = self.rect;
        // A square the height of the header controls (measured, so it grows
        // with the search box at a larger text scale).
        let h = self.list_metrics().ctl_h;
        Rect::new(
            r.x1 - 16.0 - h,
            r.y0 + HEADER_CTL_INSET,
            r.x1 - 16.0,
            r.y0 + HEADER_CTL_INSET + h,
        )
    }

    /// The always-visible entry to bulk triage. An outlined square is compact
    /// enough for a narrow list header; its tooltip and accessible name make
    /// the mode discoverable without introducing a second title row.
    fn bulk_mode_rect(&self) -> Rect {
        let r = self.rect;
        let s = self.scale();
        let x0 = r.x0 + self.header_left_inset() * s;
        Rect::new(x0, r.y0 + 12.0 * s, x0 + 32.0 * s, r.y0 + 44.0 * s)
    }

    /// The five labelled actions presented when local selection mode is active.
    /// Permanent deletion replaces (never sits beside) Move to Trash only for
    /// an all-Trash selection; virtual mixed views therefore cannot suggest
    /// that one click might mean two destructive things. Keeping
    /// their order and hit geometry in one helper ensures painting and event
    /// routing cannot drift.
    fn bulk_action_rects(&self) -> Vec<(Rect, Hit, &'static str)> {
        let r = self.rect;
        let s = self.scale();
        let h = 38.0 * s;
        let gap = 8.0 * s;
        let left = r.x0 + 14.0 * s;
        let right = r.x1 - 14.0 * s;
        let pair_w = (right - left - gap) / 2.0;
        let y0 = r.y0 + self.header_h() + 6.0 * s;
        let y1 = y0 + h + 7.0 * s;
        let y2 = y1 + h + 7.0 * s;
        let all_trash = {
            let state = self.shared.borrow();
            let ids = state.selected_bulk_ids();
            !ids.is_empty()
                && ids.iter().all(|id| {
                    state
                        .mailbox
                        .get(*id)
                        .is_some_and(|message| message.folder == Folder::Trash)
                })
        };
        let destructive = if all_trash {
            (Hit::Bulk(BulkAction::Purge), "Delete permanently")
        } else {
            (Hit::Bulk(BulkAction::Delete), "Move to Trash")
        };
        vec![
            (
                Rect::new(left, y0, left + pair_w, y0 + h),
                Hit::SelectAllVisible,
                "Select all",
            ),
            (
                Rect::new(left + pair_w + gap, y0, right, y0 + h),
                Hit::Bulk(BulkAction::Archive),
                "Archive",
            ),
            (
                Rect::new(left, y1, left + pair_w, y1 + h),
                Hit::Bulk(BulkAction::MarkRead),
                "Mark read",
            ),
            (
                Rect::new(left + pair_w + gap, y1, right, y1 + h),
                Hit::Bulk(BulkAction::MarkUnread),
                "Mark unread",
            ),
            (
                Rect::new(left, y2, right, y2 + h),
                destructive.0,
                destructive.1,
            ),
        ]
    }

    fn search_box_rect(&self) -> Rect {
        let r = self.rect;
        // Leave room for the refresh button on the far right.
        let x1 = self.refresh_box_rect().x0 - 10.0;
        // Adapt the search width to the (often narrow) list column, always
        // leaving room for the folder title on its left.
        let sw = (r.width() * 0.46).clamp(140.0, 230.0);
        let x0 = (x1 - sw).max(r.x0 + self.header_left_inset() + 70.0);
        let h = self.list_metrics().ctl_h;
        Rect::new(x0, r.y0 + HEADER_CTL_INSET, x1, r.y0 + HEADER_CTL_INSET + h)
    }

    fn rebuild_hits(&mut self) {
        let mut hits: Vec<(Rect, Hit)> = Vec::new();
        let mut search_rect: Option<Rect> = None;
        {
            let st = self.shared.borrow();
            match self.role {
                Role::Sidebar => self.rebuild_sidebar_hits(&st, &mut hits),
                Role::List => self.rebuild_list_hits(&st, &mut hits, &mut search_rect),
                Role::Reader => self.rebuild_reader_hits(&st, &mut hits),
            }
        }
        self.hits = hits;
        if let (Some(sr), Some(inp)) = (search_rect, self.search_input.as_mut()) {
            inp.apply_layout_external(sr);
        }
    }

    /// The sidebar's hit regions: compose, each account's header/chevron/
    /// settings-gear, its folders when expanded, and the add-account row.
    /// Part of [`Self::rebuild_hits`].
    fn rebuild_sidebar_hits(&self, st: &MailState, hits: &mut Vec<(Rect, Hit)>) {
        let sb = self.rect;
        // Compose button under the shell's collapse chip (no app-name header
        // band to clear anymore), so it aligns with the list column's first
        // row.
        let pad = 12.0;
        let mut y = self.shell_chip(sb).y1 + 8.0;
        hits.push((
            Rect::new(sb.x0 + pad, y, sb.x1 - pad, y + 40.0),
            Hit::Compose,
        ));
        y += 40.0 + 14.0;
        for (ai, account) in st.accounts.accounts.iter().enumerate() {
            let header = Rect::new(sb.x0 + 8.0, y, sb.x1 - 8.0, y + ACCOUNT_HEADER_H);
            hits.push((header, Hit::Account(ai)));
            let chev = Rect::new(header.x0, header.y0, header.x0 + 28.0, header.y1);
            hits.push((chev, Hit::AccountToggle(ai)));
            // Per-account settings gear on the right of the row. Pushed after
            // the row so it wins the hit test in its sub-rect; it edits THIS
            // account, not the active one.
            let gear = account_gear_rect(header);
            hits.push((gear, Hit::AccountSettings(ai)));
            y += ACCOUNT_HEADER_H;
            if !account.collapsed {
                // A gap after the account card so its highlight never abuts
                // the first folder row's highlight.
                y += 6.0;
                for f in AccountStore::folders_for(account) {
                    let row = Rect::new(sb.x0 + 14.0, y, sb.x1 - 10.0, y + FOLDER_ROW_H);
                    hits.push((row, Hit::Folder(ai, f)));
                    y += FOLDER_ROW_H;
                }
            }
            y += 16.0;
        }
        // A clear, separate "Add account" action at the foot of the list —
        // adding is its own thing, distinct from per-account settings.
        hits.push((
            Rect::new(sb.x0 + 8.0, y, sb.x1 - 8.0, y + ADD_ACCOUNT_ROW_H),
            Hit::AddAccount,
        ));
    }

    /// The list pane's hit regions: refresh, the search box (+ its clear
    /// button and focus-target sub-rect), and each visible row + its star.
    /// Part of [`Self::rebuild_hits`].
    fn rebuild_list_hits(
        &self,
        st: &MailState,
        hits: &mut Vec<(Rect, Hit)>,
        search_rect: &mut Option<Rect>,
    ) {
        let r = self.rect;
        let m = self.list_metrics();
        hits.push((self.refresh_box_rect(), Hit::Refresh));
        hits.push((self.bulk_mode_rect(), Hit::ToggleBulkSelection));
        if !st.bulk_selecting {
            let sr = self.search_box_rect();
            *search_rect = Some(Rect::new(sr.x0 + 30.0, sr.y0, sr.x1 - 26.0, sr.y1));
            hits.push((sr, Hit::SearchFocus));
            if !st.search.is_empty() {
                hits.push((
                    Rect::new(sr.x1 - 26.0, sr.y0, sr.x1, sr.y1),
                    Hit::SearchClear,
                ));
            }
        }
        if st.bulk_selecting {
            hits.extend(
                self.bulk_action_rects()
                    .into_iter()
                    .map(|(rect, hit, _)| (rect, hit)),
            );
        }
        let mut ly = self.list_content_y();
        for (id, _len) in st.list_rows() {
            if ly >= r.y1 {
                break;
            }
            let row = Rect::new(r.x0, ly, r.x1, ly + m.row_h);
            // The star target sits on the painted glyph (snippet line, right
            // edge) — not on the date at the row's top.
            hits.push((m.star_hit(row), Hit::RowStar(id)));
            hits.push((row, Hit::Row(id)));
            ly += m.row_h;
        }
    }

    /// The reader's action-bar hit regions (Reply/Archive/Delete/Star/Mark
    /// unread), sized compact (icon-only) or full width depending on
    /// available room. Part of [`Self::rebuild_hits`].
    fn rebuild_reader_hits(&self, st: &MailState, hits: &mut Vec<(Rect, Hit)>) {
        let r = self.rect;
        if st.selected.is_none() || st.bulk_selecting {
            return;
        }
        let inset = if st.narrow && st.detail_pushed {
            BACK_CLEARANCE
        } else {
            20.0
        };
        let mut bx = r.x0 + inset;
        // In the accessibility reading flow the NavShell back affordance owns
        // the first header band. Keep message actions in the second band so
        // they remain visible and reachable rather than being translated
        // above the narrow reader viewport.
        let by = r.y0 + if self.scale() > 1.25 { HEADER_H } else { 0.0 } + 12.0;
        // Compact icon-only buttons when the reader column is tight (narrow
        // layout, or a slim detail pane) so five actions always fit;
        // labelled when there's room.
        let compact = st.narrow || r.width() < READER_COMPACT_W;
        let specs: [(f64, Hit); 5] = if compact {
            [
                (38.0, Hit::Reply),
                (38.0, Hit::Archive),
                (38.0, Hit::Delete),
                (38.0, Hit::Star),
                (38.0, Hit::Unread),
            ]
        } else {
            [
                (78.0, Hit::Reply),
                (88.0, Hit::Archive),
                (78.0, Hit::Delete),
                (64.0, Hit::Star),
                (104.0, Hit::Unread),
            ]
        };
        for (w, hit) in specs {
            let b = Rect::new(bx, by, bx + w, by + 32.0);
            hits.push((b, hit));
            bx += w + 8.0;
        }
    }

    fn hit_at(&self, pt: Point) -> Option<Hit> {
        self.hits
            .iter()
            .rev()
            .find(|(rc, _)| rc.contains(pt))
            .map(|(_, h)| h.clone())
    }

    fn row_id_at(&self, pt: Point) -> Option<MessageId> {
        self.hits.iter().rev().find_map(|(rc, h)| match h {
            Hit::Row(id) if rc.contains(pt) => Some(*id),
            _ => None,
        })
    }

    // ---- drawing ----------------------------------------------------------

    fn draw_sidebar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let st = self.shared.borrow();
        let sb = self.rect;
        fill_rect(scene, t, sb, c.neutral[0]);

        // The shell's collapse chip occupies the left edge of this compact
        // header. Keeping a quiet in-app title beside it makes that band read
        // as intentional on desktop instead of as a stranded chevron above a
        // blank gap; account settings remain scoped to each account row.
        tr.draw_text_weighted(
            scene,
            "Mail",
            16.0,
            c.text_primary,
            t * Affine::translate((sb.x0 + CHIP_INSET + CHIP_SIZE + 12.0, sb.y0 + 19.0)),
            None,
            TextWeight::Semibold,
        );

        // Compose button.
        if let Some((rect, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::Compose)) {
            let hov = rect.contains(st.last_mouse);
            let col = if hov { c.accent[4] } else { c.accent[5] };
            pill(scene, t, *rect, col, None, Some(10.0));
            let label = "Compose";
            let (w, _) = tr.measure_weighted(label, 14.0, None, TextWeight::Semibold);
            // Icon + label centred as a unit. Ink is the accent's own, so a
            // bright accent does not leave white type on a light fill.
            let ink = c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST);
            let group_w = 18.0 + 8.0 + f64::from(w);
            let gx = f64::midpoint(rect.x0, rect.x1) - group_w / 2.0;
            let icon_box = Rect::new(gx, rect.y0 + 11.0, gx + 18.0, rect.y0 + 29.0);
            Icon::Pencil.draw(scene, t, icon_box, ink);
            tr.draw_text_weighted(
                scene,
                label,
                14.0,
                ink,
                t * Affine::translate((icon_box.x1 + 8.0, rect.y0 + 11.0)),
                None,
                TextWeight::Semibold,
            );
        }

        for (rect, hit) in &self.hits {
            match hit {
                Hit::Account(ai) => {
                    Self::draw_sidebar_account_row(
                        &mut DrawContext {
                            scene,
                            transform: t,
                            text: tr,
                            theme,
                        },
                        &st,
                        *rect,
                        *ai,
                        sb,
                    );
                }
                Hit::AddAccount => {
                    self.draw_sidebar_add_account_row(scene, t, tr, theme, *rect);
                }
                Hit::Folder(ai, f) => {
                    Self::draw_sidebar_folder_row(
                        &mut DrawContext {
                            scene,
                            transform: t,
                            text: tr,
                            theme,
                        },
                        &st,
                        *rect,
                        *ai,
                        *f,
                    );
                }
                _ => {}
            }
        }
    }

    /// One account header row: expand chevron, avatar, name/email, and the
    /// per-account settings gear. A no-op if `ai` is out of range. Part of
    /// [`Self::draw_sidebar`].
    fn draw_sidebar_account_row(
        ctx: &mut DrawContext<'_>,
        st: &MailState,
        rect: Rect,
        ai: usize,
        sb: Rect,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        let Some(account) = st.accounts.accounts.get(ai) else {
            return;
        };
        let active = ai == st.accounts.active;
        let hov = rect.contains(st.last_mouse);
        // The active account is a lifted card with its derived edge (HIG
        // §9.3 / §9.5): selection is a lift, not an accent wash, and a bare
        // wash on the sidebar had no edge at all on the dark preset.
        if active {
            card(scene, t, rect, 9.0, theme.bg_active);
        } else if hov {
            card(scene, t, rect, 9.0, theme.bg_hover);
        }
        let chev = if account.collapsed {
            Icon::ChevronRight
        } else {
            Icon::ChevronDown
        };
        chev.draw(
            scene,
            t,
            Rect::new(
                rect.x0 + 6.0,
                rect.y0 + ACCOUNT_HEADER_H / 2.0 - 8.0,
                rect.x0 + 22.0,
                rect.y0 + ACCOUNT_HEADER_H / 2.0 + 8.0,
            ),
            c.text_dim,
        );
        draw_avatar(
            &mut AvatarPaintContext {
                scene,
                text: tr,
                transform: t,
                theme,
                images: &st.images,
            },
            Point::new(rect.x0 + 36.0, rect.y0 + ACCOUNT_HEADER_H / 2.0),
            13.0,
            &Addr::new(
                account.config.display_name.clone(),
                account.config.email.clone(),
            ),
            account.tint,
        );
        // Reserve room on the right for the per-account gear.
        let label = elide(
            tr,
            account.label(),
            13.0,
            TextWeight::Semibold,
            sb.width() - 108.0,
        );
        let email = elide(
            tr,
            &account.config.email,
            10.5,
            TextWeight::Regular,
            sb.width() - 108.0,
        );
        tr.draw_text_weighted(
            scene,
            &label,
            13.0,
            c.text_primary,
            t * Affine::translate((rect.x0 + 54.0, rect.y0 + 6.0)),
            None,
            TextWeight::Semibold,
        );
        tr.draw_text(
            scene,
            &email,
            10.5,
            c.text_dim,
            t * Affine::translate((rect.x0 + 54.0, rect.y0 + 24.0)),
            None,
        );
        // Per-account settings gear (right edge). Hover-lit; always drawn so
        // it is visible headless / in shots.
        let gear = account_gear_rect(rect);
        let ghov = gear.contains(st.last_mouse);
        if ghov {
            pill(scene, t, gear, theme.bg_hover, None, Some(8.0));
        }
        Icon::Settings.draw(
            scene,
            t,
            gear.inset(-7.0),
            if ghov { c.text_primary } else { c.text_dim },
        );
    }

    /// The "Add account" row at the foot of the account list. Part of
    /// [`Self::draw_sidebar`].
    fn draw_sidebar_add_account_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
    ) {
        let c = &theme.tokens.colors;
        let hov = rect.contains(self.shared.borrow().last_mouse);
        if hov {
            pill(scene, t, rect, theme.bg_hover, None, Some(9.0));
        }
        let cy = rect.y0 + ADD_ACCOUNT_ROW_H / 2.0;
        let icon = Rect::new(rect.x0 + 12.0, cy - 8.0, rect.x0 + 28.0, cy + 8.0);
        let col = if hov { c.accent[4] } else { c.accent[5] };
        Icon::Plus.draw(scene, t, icon, col);
        tr.draw_text_weighted(
            scene,
            "Add account",
            13.0,
            col,
            t * Affine::translate((rect.x0 + 38.0, cy - 8.0)),
            None,
            TextWeight::Semibold,
        );
    }

    /// One folder row within an expanded account: icon, title, unread badge
    /// (active account only), and the selected-state highlight. Part of
    /// [`Self::draw_sidebar`].
    fn draw_sidebar_folder_row(
        ctx: &mut DrawContext<'_>,
        st: &MailState,
        rect: Rect,
        ai: usize,
        f: Folder,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        let active_account = ai == st.accounts.active;
        let selected = active_account && f == st.folder;
        let hov = rect.contains(st.last_mouse);
        // Selection is a lift with the accent kept to the leading bar (HIG
        // §9.3 / §9.4); hover is the shared hover tier.
        if selected {
            pill(scene, t, rect, theme.bg_active, None, Some(8.0));
            fill_rect(
                scene,
                t,
                Rect::new(rect.x0, rect.y0 + 6.0, rect.x0 + 3.0, rect.y1 - 6.0),
                c.accent[5],
            );
        } else if hov {
            pill(scene, t, rect, theme.bg_hover, None, Some(8.0));
        }
        let sp = &theme.tokens.spacing;
        let cy = rect.y0 + FOLDER_ROW_H / 2.0;
        let label_col = if selected {
            c.text_primary
        } else {
            c.neutral[7]
        };
        let icon_box = Rect::new(rect.x0 + 12.0, cy - 9.0, rect.x0 + 30.0, cy + 9.0);
        let icon_col = if selected { c.accent[5] } else { c.neutral[6] };
        f.icon().draw(scene, t, icon_box, icon_col);

        // The unread badge is measured first so the title can be elided into
        // the width that is actually left, instead of wrapping at a fixed
        // budget onto the next row.
        let unread = if active_account {
            st.mailbox.unread_count(f)
        } else {
            0
        };
        let badge = (unread > 0).then(|| {
            let txt = unread.to_string();
            let (twd, th) = tr.measure_weighted(&txt, 11.0, None, TextWeight::Semibold);
            let bw = (f64::from(twd) + 14.0).max(20.0);
            let rect = Rect::new(
                rect.x1 - bw - f64::from(sp.s2),
                rect.y0 + 8.0,
                rect.x1 - f64::from(sp.s2),
                rect.y0 + 28.0,
            );
            (txt, f64::from(twd), f64::from(th), rect)
        });

        let text_x = rect.x0 + 40.0;
        let label_right = badge.as_ref().map_or_else(
            || rect.x1 - f64::from(sp.s2),
            |(_, _, _, b)| b.x0 - f64::from(sp.s2),
        );
        let weight = if selected {
            TextWeight::Semibold
        } else {
            TextWeight::Regular
        };
        let title = elide(tr, f.title(), 13.0, weight, label_right - text_x);
        let (_, th) = tr.measure_weighted(&title, 13.0, None, weight);
        tr.draw_text_weighted(
            scene,
            &title,
            13.0,
            label_col,
            t * Affine::translate((text_x, cy - f64::from(th) / 2.0)),
            None,
            weight,
        );

        let Some((txt, twd, th, badge)) = badge else {
            return;
        };
        let bg = if selected { c.accent[5] } else { c.neutral[3] };
        pill(scene, t, badge, bg, None, Some(10.0));
        tr.draw_text_weighted(
            scene,
            &txt,
            11.0,
            c.on_surface(bg).ink,
            t * Affine::translate((badge.center().x - twd / 2.0, badge.center().y - th / 2.0)),
            None,
            TextWeight::Semibold,
        );
    }

    /// The list pane's header: folder title (+ count) and the search box, plus
    /// the manual sync/refresh button beside it. Part of [`Self::draw_list`].
    #[allow(
        clippy::too_many_lines,
        reason = "the header and its contextual selection replacement share measured geometry and must remain visually synchronized"
    )]
    fn draw_list_header(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let st = self.shared.borrow();
        let r = self.rect;
        // Header: folder title (+ count), search box. The title is vertically
        // centred on the NavShell collapse/hamburger chip beside it and spaced a
        // consistent gap (a theme spacing token) past it — never crowding it.
        let sr = self.search_box_rect();
        // Whether the collapse chip sits over this pane's header (the list is
        // then the leftmost pane); matches `header_left_inset`'s List branch.
        let chip_here = st.narrow || !st.sidebar_open;
        let s = self.scale();
        let gap = f64::from(theme.tokens.spacing.s3);
        let (layout_title_x, title_y, header_cy) = list_header_layout(
            r,
            self.shell_chip(r),
            chip_here,
            gap,
            f64::from(tr.measure_weighted("Ag", 18.0, None, TextWeight::Bold).1),
        );
        let mode_rect = self.bulk_mode_rect();
        let title_x = (mode_rect.x1 + 10.0 * s).max(layout_title_x);
        // Selection mode is an action state, not a second search state. Hide
        // the search input while it is active so the explicit count has a
        // proper, non-ellipsized title instead of competing with two compact
        // controls in a 340px list column.
        let title_right = if st.bulk_selecting {
            self.refresh_box_rect().x0 - 12.0 * s
        } else {
            sr.x0 - 12.0 * s
        };
        let title_budget = title_right - title_x;
        let title_full = if st.bulk_selecting {
            "Selected"
        } else {
            st.folder.title()
        };
        let (twd, _) = tr.measure_weighted(title_full, 18.0, None, TextWeight::Bold);
        let title = crate::draw::elide(tr, title_full, 18.0, TextWeight::Bold, title_budget);
        tr.draw_text_weighted(
            scene,
            &title,
            18.0,
            c.text_primary,
            t * Affine::translate((title_x, title_y)),
            None,
            TextWeight::Bold,
        );
        // Count sits after the full title only when there's room for both,
        // centred on the same line.
        let count = if st.bulk_selecting {
            st.selected_bulk_ids().len()
        } else {
            st.mailbox.count(st.folder)
        };
        let count_x = title_x + f64::from(twd) + 10.0 * s;
        if f64::from(twd) <= title_budget && count_x + 26.0 * s < title_right {
            let count_label = format!("{count}");
            let count_y = header_cy - f64::from(tr.measure(&count_label, 12.0, None).1) / 2.0;
            tr.draw_text(
                scene,
                &count_label,
                12.0,
                c.text_dim,
                t * Affine::translate((count_x, count_y)),
                None,
            );
        }

        // Explicit mode affordance: unchecked begins local multi-select;
        // checked returns to ordinary reading and clears that local selection.
        let mode_hover = mode_rect.contains(st.last_mouse);
        fill_round(
            scene,
            t,
            mode_rect,
            8.0 * s,
            if mode_hover {
                c.lift(0.72)
            } else {
                c.lift(0.35)
            },
        );
        stroke_round(scene, t, mode_rect, 8.0 * s, c.border_standard, 1.0);
        if st.bulk_selecting {
            Icon::Check.draw(scene, t, mode_rect.inset(-8.0 * s), c.accent[5]);
        } else {
            Icon::Square.draw(scene, t, mode_rect.inset(-8.0 * s), c.neutral[7]);
        }
        if !st.bulk_selecting {
            fill_round(scene, t, sr, 9.0 * s, c.surfaces.raised);
            stroke_round(
                scene,
                t,
                sr,
                9.0 * s,
                if st.search_focused {
                    c.border_focus
                } else {
                    c.border_standard
                },
                1.0,
            );
            Icon::Search.draw(
                scene,
                t,
                Rect::new(
                    sr.x0 + 8.0 * s,
                    sr.y0 + 8.0 * s,
                    sr.x0 + 24.0 * s,
                    sr.y0 + 24.0 * s,
                ),
                c.text_dim,
            );
            if st.search.is_empty() && !st.search_focused {
                tr.draw_text(
                    scene,
                    "Search",
                    13.0,
                    c.text_dim,
                    t * Affine::translate((sr.x0 + 30.0 * s, sr.y0 + 9.0 * s)),
                    None,
                );
            }
            if !st.search.is_empty() {
                Icon::X.draw(
                    scene,
                    t,
                    Rect::new(
                        sr.x1 - 24.0 * s,
                        sr.y0 + 8.0 * s,
                        sr.x1 - 8.0 * s,
                        sr.y0 + 24.0 * s,
                    ),
                    c.text_dim,
                );
            }
        }

        // Manual sync / refresh button.
        let rb = self.refresh_box_rect();
        let rb_hov = rb.contains(st.last_mouse);
        // The shared Neutral button face (HIG §8) with no label; the glyph
        // is drawn in the face's ink.
        let face = ButtonFace::new(rb, "").radius(9.0).hovered(rb_hov);
        Button::paint(scene, t, tr, theme, &face);
        let icon = f64::from(theme.tokens.metrics.icon_sm) * f64::from(theme.text_scale);
        Icon::Refresh.draw(
            scene,
            t,
            rb.inset(-(rb.height() - icon) / 2.0),
            face.colors(theme).2,
        );

        if st.bulk_selecting {
            self.draw_bulk_toolbar(scene, t, tr, theme, &st);
        }
    }

    /// Paint the compact, explicit selection toolbar. The icon order matches
    /// [`Self::bulk_action_rects`], which is also the one event routing uses.
    fn draw_bulk_toolbar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        state: &MailState,
    ) {
        let c = &theme.tokens.colors;
        let s = self.scale();
        let actions = self.bulk_action_rects();
        for (rect, hit, label) in actions {
            let hover = rect.contains(state.last_mouse);
            fill_round(
                scene,
                t,
                rect,
                8.0 * s,
                if hover { c.lift(0.72) } else { c.lift(0.35) },
            );
            stroke_round(scene, t, rect, 8.0 * s, c.border_standard, 1.0);
            let (icon, color) = match hit {
                Hit::SelectAllVisible => (Icon::Square, c.neutral[8]),
                Hit::Bulk(BulkAction::Archive) => (Icon::Archive, c.neutral[8]),
                Hit::Bulk(BulkAction::Delete) => (Icon::Trash, c.semantic.danger),
                Hit::Bulk(BulkAction::Purge) => (Icon::Trash2, c.semantic.danger),
                Hit::Bulk(BulkAction::MarkRead) => (Icon::Mail, c.neutral[8]),
                Hit::Bulk(BulkAction::MarkUnread) => (Icon::Undo2, c.neutral[8]),
                _ => continue,
            };
            icon.draw(
                scene,
                t,
                Rect::new(
                    rect.x0 + 10.0 * s,
                    rect.y0 + 10.0 * s,
                    rect.x0 + 26.0 * s,
                    rect.y0 + 26.0 * s,
                ),
                color,
            );
            tr.draw_text_weighted(
                scene,
                label,
                11.5,
                if matches!(hit, Hit::Bulk(BulkAction::Delete | BulkAction::Purge)) {
                    color
                } else {
                    c.text_primary
                },
                t * Affine::translate((rect.x0 + 32.0 * s, rect.y0 + 12.0 * s)),
                None,
                TextWeight::Semibold,
            );
        }
    }

    fn draw_list(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let st = self.shared.borrow();
        let r = self.rect;
        fill_rect(scene, t, r, c.surfaces.base);
        self.draw_list_header(scene, t, tr, theme);

        // Rows.
        let rows = st.list_rows();
        if rows.is_empty() {
            let (icon, title, message) = list_zero_state(
                !st.mailbox.messages.is_empty(),
                !st.accounts.is_empty(),
                &st.search,
            );
            EmptyState::paint(
                &mut PaintCtx::new(scene, t, tr, theme),
                Rect::new(r.x0, self.list_content_y(), r.x1, r.y1),
                &EmptyStateContent {
                    icon: Some(icon),
                    title,
                    message: Some(message),
                    action_label: None,
                },
                Point::new(-1.0, -1.0),
            );
            return;
        }

        for (rect, hit) in &self.hits {
            let Hit::Row(id) = hit else { continue };
            let id = *id;
            if rect.y1 > r.y1 + 1.0 {
                continue;
            }
            let Some(msg) = st.mailbox.get(id) else {
                continue;
            };
            let len = rows
                .iter()
                .find(|(rid, _)| *rid == id)
                .map_or(1, |(_, l)| *l);
            let selected = st.selected == Some(id) && !st.narrow && !st.bulk_selecting;
            let bulk_selected = st.bulk_selection.contains(&id);
            let hov = rect.contains(st.last_mouse);
            let swipe_dx = self.swipe.filter(|s| s.id == id).map_or(0.0, |s| s.dx);
            self.draw_list_row(
                &mut DrawContext {
                    scene,
                    transform: t,
                    text: tr,
                    theme,
                },
                &ListRowArgs {
                    msg,
                    rect: *rect,
                    len,
                    selected,
                    bulk_selected,
                    hov,
                    swipe_dx,
                    now_min: st.now_min,
                },
            );
        }
    }

    /// One row of the message list: the swipe-action overlay (if dragged),
    /// selection/hover background, unread dot, avatar, name/date, subject
    /// (+ thread-size badge), snippet, and star/attachment glyphs. Part of
    /// [`Self::draw_list`].
    fn draw_list_row(&self, ctx: &mut DrawContext<'_>, args: &ListRowArgs<'_>) {
        let ListRowArgs {
            msg,
            rect,
            len,
            selected,
            bulk_selected,
            hov,
            swipe_dx,
            now_min,
        } = *args;
        let row = Self::draw_list_row_frame(ctx, rect, swipe_dx, selected, bulk_selected, hov);
        self.draw_list_row_content(ctx, row, msg, len, now_min, bulk_selected);
    }

    /// Draw the swipe-action overlay (if dragged past the threshold) and the
    /// row's selection/hover background + bottom divider, returning the
    /// (possibly swipe-shifted) row rect the rest of the row draws into.
    /// Part of [`Self::draw_list_row`].
    fn draw_list_row_frame(
        ctx: &mut DrawContext<'_>,
        rect: Rect,
        swipe_dx: f64,
        selected: bool,
        bulk_selected: bool,
        hov: bool,
    ) -> Rect {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        if swipe_dx.abs() > 1.0 {
            let (col, icon, label, on_left) = if swipe_dx < 0.0 {
                (c.semantic.warning, Icon::Archive, "Archive", false)
            } else {
                (c.semantic.danger, Icon::Trash, "Delete", true)
            };
            fill_rect(scene, t, rect, col.with_alpha_factor(0.9));
            let gx = if on_left {
                rect.x0 + 18.0
            } else {
                rect.x1 - 90.0
            };
            let ink = c.on_surface(col).ink;
            icon.draw(
                scene,
                t,
                Rect::new(gx, rect.y0 + 20.0, gx + 18.0, rect.y0 + 38.0),
                ink,
            );
            tr.draw_text_weighted(
                scene,
                label,
                12.0,
                ink,
                t * Affine::translate((gx + 26.0, rect.y0 + 22.0)),
                None,
                TextWeight::Semibold,
            );
        }
        let row = Rect::new(rect.x0 + swipe_dx, rect.y0, rect.x1 + swipe_dx, rect.y1);

        if bulk_selected {
            fill_rect(scene, t, row, c.accent[5].with_alpha_factor(0.24));
            fill_rect(
                scene,
                t,
                Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1),
                c.accent[5],
            );
        } else if selected {
            fill_rect(scene, t, row, theme.bg_active);
            fill_rect(
                scene,
                t,
                Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1),
                c.accent[5],
            );
        } else if hov && swipe_dx.abs() < 1.0 {
            fill_rect(scene, t, row, c.lift(0.35));
        }
        // The rule between rows is derived against the list's base and stops
        // an `s3` short of both ends (HIG §9.5).
        separator(
            scene,
            t,
            row.x0,
            row.x1,
            row.y1 - 0.5,
            f64::from(theme.tokens.spacing.s3),
            c.surfaces.base,
        );
        row
    }

    /// Draw one row's content within its (already framed) `row` rect: unread
    /// dot, avatar, name/date, subject (+ thread-size badge), snippet, and
    /// star/attachment glyphs. Part of [`Self::draw_list_row`].
    fn draw_list_row_content(
        &self,
        ctx: &mut DrawContext<'_>,
        row: Rect,
        msg: &Message,
        len: usize,
        now_min: i64,
        bulk_selected: bool,
    ) {
        let unread = !msg.read;
        self.draw_list_row_identity(ctx, row, msg, unread, now_min, bulk_selected);
        self.draw_list_row_subject(ctx, row, msg, len, unread);
    }

    /// The unread dot, avatar, sender name, and relative date of a list row.
    /// Part of [`Self::draw_list_row_content`].
    fn draw_list_row_identity(
        &self,
        ctx: &mut DrawContext<'_>,
        row: Rect,
        msg: &Message,
        unread: bool,
        now_min: i64,
        bulk_selected: bool,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let m = self.list_metrics();
        let c = &theme.tokens.colors;
        if bulk_selected {
            scene.fill(
                Fill::NonZero,
                t,
                c.accent[5],
                None,
                &Circle::new(
                    Point::new(
                        row.x0 + 14.0 * self.scale(),
                        row.y0 + self.list_row_h() / 2.0,
                    ),
                    7.0 * self.scale(),
                ),
            );
            Icon::Check.draw(
                scene,
                t,
                Rect::new(
                    row.x0 + 8.0 * self.scale(),
                    row.y0 + self.list_row_h() / 2.0 - 6.0 * self.scale(),
                    row.x0 + 20.0 * self.scale(),
                    row.y0 + self.list_row_h() / 2.0 + 6.0 * self.scale(),
                ),
                c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST),
            );
        } else if unread {
            scene.fill(
                Fill::NonZero,
                t,
                c.accent[5],
                None,
                &Circle::new(
                    Point::new(
                        row.x0 + 14.0 * self.scale(),
                        row.y0 + self.list_row_h() / 2.0,
                    ),
                    4.0 * self.scale(),
                ),
            );
        }
        // The disc's radius is the row's measured `avatar_r` (text-scaled, so
        // the initials stay inside it); the text column starts one `s3` past
        // the disc instead of at a fixed 70 px — the same `text_x` the
        // subject and snippet lines use.
        let avatar_cx = m.avatar_cx(row.x0);
        draw_avatar(
            &mut AvatarPaintContext {
                scene,
                text: tr,
                transform: t,
                theme,
                images: &self.shared.borrow().images,
            },
            Point::new(avatar_cx, row.y0 + m.avatar_cy),
            m.avatar_r,
            &msg.from,
            0,
        );

        let text_x = m.text_x(row.x0, f64::from(theme.tokens.spacing.s3));
        let right = row.x1 - 14.0;
        let name_weight = if unread {
            TextWeight::Bold
        } else {
            TextWeight::Medium
        };
        // The date is measured first so the name is elided into the width
        // that is actually left of it.
        let date = format_relative(msg.timestamp_min, now_min);
        let (dw, dh) = tr.measure(&date, 11.5, None);
        let name = elide(
            tr,
            &msg.from.display(),
            13.5,
            name_weight,
            right - f64::from(dw) - f64::from(theme.tokens.spacing.s3) - text_x,
        );
        tr.draw_text_weighted(
            scene,
            &name,
            13.5,
            c.text_primary,
            t * Affine::translate((text_x, row.y0 + m.name_y)),
            None,
            name_weight,
        );
        // Bottom-align the date's line box to the name's so the baselines
        // meet at every text scale.
        tr.draw_text(
            scene,
            &date,
            11.5,
            c.text_dim,
            t * Affine::translate((
                right - f64::from(dw),
                row.y0 + m.name_y + m.name_h - f64::from(dh),
            )),
            None,
        );
    }

    /// The subject line (+ thread-size badge), snippet, and star/attachment
    /// glyphs of a list row. Part of [`Self::draw_list_row_content`].
    fn draw_list_row_subject(
        &self,
        ctx: &mut DrawContext<'_>,
        row: Rect,
        msg: &Message,
        len: usize,
        unread: bool,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let m = self.list_metrics();
        let c = &theme.tokens.colors;
        // Same column as the sender line: one `s3` past the measured disc.
        let text_x = m.text_x(row.x0, f64::from(theme.tokens.spacing.s3));
        let right = row.x1 - 14.0;

        let subj_y = row.y0 + m.subj_y;
        let subj_col = if unread { c.text_primary } else { c.neutral[7] };
        let subj_weight = if unread {
            TextWeight::Semibold
        } else {
            TextWeight::Regular
        };
        let s = self.scale();
        let subj_budget = right - text_x - if len > 1 { 48.0 * s } else { 16.0 * s };
        let subject = elide(tr, &msg.thread_subject(), 12.5, subj_weight, subj_budget);
        tr.draw_text_weighted(
            scene,
            &subject,
            12.5,
            subj_col,
            t * Affine::translate((text_x, subj_y)),
            None,
            subj_weight,
        );
        if len > 1 {
            // Thread-size badge: a capsule centred on the subject line.
            let badge_txt = len.to_string();
            let (bw, bh) = tr.measure_weighted(&badge_txt, 10.5, None, TextWeight::Semibold);
            let subject_mid = subj_y + m.subj_h / 2.0;
            let badge_h = (f64::from(bh) + 2.0).max(16.0);
            let bx = right - f64::from(bw) - 12.0;
            let badge = Rect::new(
                bx - 6.0,
                subject_mid - badge_h / 2.0,
                right,
                subject_mid + badge_h / 2.0,
            );
            pill(scene, t, badge, c.neutral[3], None, None);
            tr.draw_text_weighted(
                scene,
                &badge_txt,
                10.5,
                c.neutral[8],
                t * Affine::translate((bx, subject_mid - f64::from(bh) / 2.0)),
                None,
                TextWeight::Semibold,
            );
        }

        let icon_reserve = if msg.has_attachments() {
            64.0 * s
        } else {
            28.0 * s
        };
        let snippet = elide(
            tr,
            &msg.snippet(),
            11.5,
            TextWeight::Regular,
            right - text_x - icon_reserve,
        );
        tr.draw_text(
            scene,
            &snippet,
            11.5,
            c.text_dim,
            t * Affine::translate((text_x, row.y0 + m.snippet_y)),
            None,
        );

        // Star and paperclip sit on the snippet line's centre; the star's hit
        // rect (`ListRowMetrics::star_hit`) is centred on this same glyph.
        let icon_cy = row.y0 + m.icon_cy;
        // A favourited mail shows a solid, filled star; an unstarred one a
        // faint outline — the state reads at a glance, not just as a colour.
        let star_box = Rect::new(right - 24.0, icon_cy - 8.0, right - 8.0, icon_cy + 8.0);
        if msg.starred {
            Icon::Star.draw_filled(scene, t, star_box, c.semantic.warning);
        } else {
            Icon::Star.draw(scene, t, star_box, c.neutral[4]);
        }
        if msg.has_attachments() {
            Icon::Paperclip.draw(
                scene,
                t,
                Rect::new(right - 44.0, icon_cy - 7.0, right - 30.0, icon_cy + 7.0),
                c.text_dim,
            );
        }
    }

    /// The reader's action bar: Reply/Archive/Delete/Star/Mark-unread, icon
    /// only when `compact`, icon + label otherwise. Part of [`Self::draw_reader`].
    fn draw_reader_action_bar(
        &self,
        ctx: &mut DrawContext<'_>,
        compact: bool,
        starred: bool,
        last_mouse: Point,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        for (rect, hit) in &self.hits {
            let (label, danger, icon) = match hit {
                Hit::Reply => ("Reply", false, Icon::Reply),
                Hit::Archive => ("Archive", false, Icon::Archive),
                Hit::Delete => ("Delete", true, Icon::Trash),
                Hit::Star => (if starred { "Starred" } else { "Star" }, false, Icon::Star),
                Hit::Unread => ("Mark unread", false, Icon::Mail),
                _ => continue,
            };
            let hov = rect.contains(last_mouse);
            // The shared button face (HIG §8): Neutral, or DangerOutline for
            // the destructive action; glyph and label are drawn in the face's
            // ink (the starred star keeps its warning fill).
            let face = ButtonFace::new(*rect, "").radius(8.0).hovered(hov);
            let face = if danger { face.danger_outline() } else { face };
            Button::paint(scene, t, tr, theme, &face);
            // The Star action fills its glyph when the message is starred, so
            // the favourited state is a solid star, not merely a tinted outline.
            let is_star_on = matches!(hit, Hit::Star) && starred;
            let col = if is_star_on {
                c.semantic.warning
            } else {
                face.colors(theme).2
            };
            let paint_icon = |scene: &mut Scene, ibox: Rect| {
                if is_star_on {
                    Icon::Star.draw_filled(scene, t, ibox, col);
                } else {
                    icon.draw(scene, t, ibox, col);
                }
            };
            if compact {
                paint_icon(
                    scene,
                    Rect::new(
                        rect.center().x - 8.0,
                        rect.y0 + 8.0,
                        rect.center().x + 8.0,
                        rect.y0 + 24.0,
                    ),
                );
            } else {
                paint_icon(
                    scene,
                    Rect::new(
                        rect.x0 + 10.0,
                        rect.y0 + 8.0,
                        rect.x0 + 26.0,
                        rect.y0 + 24.0,
                    ),
                );
                tr.draw_text_weighted(
                    scene,
                    label,
                    12.0,
                    col,
                    t * Affine::translate((rect.x0 + 30.0, rect.y0 + 9.0)),
                    None,
                    TextWeight::Medium,
                );
            }
        }
    }

    fn draw_reader(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let c = &theme.tokens.colors;
        let rr = self.rect;
        fill_rect(scene, t, rr, c.surfaces.raised);

        if self.shared.borrow().bulk_selecting {
            let count = self.shared.borrow().selected_bulk_ids().len();
            let title = format!(
                "{count} message{} selected",
                if count == 1 { "" } else { "s" }
            );
            tr.draw_text_weighted(
                scene,
                &title,
                20.0,
                c.text_primary,
                t * Affine::translate((rr.x0 + 28.0, rr.y0 + 38.0)),
                None,
                TextWeight::Bold,
            );
            tr.draw_text(
                scene,
                "Choose a batch action in the message list. Individual message actions are paused while selecting.",
                13.0,
                c.text_dim,
                t * Affine::translate((rr.x0 + 28.0, rr.y0 + 68.0)),
                None,
            );
            self.html_view_rect = None;
            self.html_toggle_rect = None;
            return;
        }

        // Thread cards deliberately pack several independent lines into a
        // compact desktop rhythm. At accessibility scales that density stops
        // being readable, so use a single-column reading flow measured by the
        // same TextRenderer instead of allowing the original baselines to
        // overlap. This is a presentation reflow only; messages, actions and
        // the selected resource remain the same.
        if theme.text_scale > 1.25 {
            self.draw_large_text_reader(scene, t, tr, theme, rr);
            return;
        }

        let Some((head_id, head_html, webview_rect)) =
            self.draw_reader_thread(scene, t, tr, theme, rr)
        else {
            return;
        };

        // The HTML body view + its remote-images toggle, now that the shared
        // borrow is released (they need `&mut self`).
        self.html_view_rect = None;
        self.html_toggle_rect = None;
        if let (Some(rect), Some(html)) = (webview_rect, head_html) {
            // Switching to a different message re-blocks remote content
            // (keyed on the shown id, not `html_key`, which the toggle
            // clears to force a reload of the *same* message).
            if self.html_shown_id != Some(head_id) {
                self.html_load_remote = false;
                self.html_shown_id = Some(head_id);
            }
            self.load_html_body(head_id, &html);
            if self.html_engine_live {
                let view = self.ensure_html_view();
                view.set_viewport(rect, 1.0);
                view.draw(scene, t, tr, theme, presence);
                self.html_view_rect = Some(rect);
                self.draw_remote_toggle(scene, t, tr, theme, rect, true);
            } else {
                // WPE is optional and can also fail to initialise on a given
                // GPU. The test engine paints a diagnostic colour, not HTML;
                // render the already-decoded MIME text alternative instead.
                let plain = self
                    .shared
                    .borrow()
                    .mailbox
                    .get(head_id)
                    .map_or_else(String::new, |message| message.body.clone());
                self.draw_native_html_fallback(scene, t, tr, theme, rect, &plain);
                self.draw_remote_toggle(scene, t, tr, theme, rect, false);
            }
        } else {
            // Plain-text mail this frame — drop any stale load so returning
            // to an HTML mail reloads fresh (and blocked again by default).
            self.html_key = None;
            self.html_shown_id = None;
        }
    }

    /// Accessibility-scale reader presentation: a measured, single-column
    /// thread flow.  It intentionally favours message text over dense sender /
    /// recipient metadata, which is still available in the ordinary 1× cards.
    fn draw_large_text_reader(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rr: Rect,
    ) {
        let c = &theme.tokens.colors;
        self.reader_attachment_hits.clear();
        self.html_view_rect = None;
        self.html_toggle_rect = None;
        let st = self.shared.borrow();
        let msgs = st.selected_thread_messages();
        if msgs.is_empty() {
            let hint = "Select a conversation to read";
            let (w, _) = tr.measure(hint, 14.0, None);
            tr.draw_text(
                scene,
                hint,
                14.0,
                c.text_dim,
                t * Affine::translate((
                    f64::midpoint(rr.x0, rr.x1) - f64::from(w) / 2.0,
                    rr.y0 + rr.height() / 2.0,
                )),
                None,
            );
            return;
        }

        let compact = true;
        let starred = st
            .selected
            .and_then(|id| st.mailbox.get(id))
            .is_some_and(|message| message.starred);
        self.draw_reader_action_bar(
            &mut DrawContext {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            compact,
            starred,
            st.last_mouse,
        );

        let width = (rr.width() - 48.0).max(120.0) as f32;
        let x = rr.x0 + 24.0;
        let mut y = rr.y0 + HEADER_H + 68.0;
        let Some(head) = msgs.last().copied() else {
            return;
        };
        let subject = elide(
            tr,
            &head.thread_subject(),
            20.0,
            TextWeight::Bold,
            f64::from(width),
        );
        let (_, title_h) = tr.measure_weighted(&subject, 20.0, Some(width), TextWeight::Bold);
        tr.draw_text_weighted(
            scene,
            &subject,
            20.0,
            c.text_primary,
            t * Affine::translate((x, y)),
            Some(width),
            TextWeight::Bold,
        );
        y += f64::from(title_h) + 10.0;
        let context = format!(
            "{} message{} · {}",
            msgs.len(),
            if msgs.len() == 1 { "" } else { "s" },
            st.folder.title()
        );
        tr.draw_text(
            scene,
            &context,
            12.0,
            c.text_dim,
            t * Affine::translate((x, y)),
            Some(width),
        );
        y += 38.0;

        Self::draw_large_text_messages(
            &mut DrawContext {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            &msgs,
            &LargeTextFlow {
                bounds: rr,
                x,
                width,
                y,
            },
        );
    }

    fn draw_large_text_messages(
        ctx: &mut DrawContext<'_>,
        messages: &[&Message],
        flow: &LargeTextFlow,
    ) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let colors = &theme.tokens.colors;
        let LargeTextFlow {
            bounds,
            x,
            width,
            mut y,
        } = *flow;
        for message in messages {
            if y >= bounds.y1 - 30.0 {
                break;
            }
            let sender = message.from.display();
            tr.draw_text_weighted(
                scene,
                &sender,
                14.0,
                colors.text_primary,
                t * Affine::translate((x, y)),
                Some(width),
                TextWeight::Semibold,
            );
            y += 28.0;
            let when = format_absolute(message.timestamp_min);
            tr.draw_text(
                scene,
                &when,
                11.5,
                colors.text_dim,
                t * Affine::translate((x, y)),
                Some(width),
            );
            y += 28.0;
            let (_, body_h) = tr.measure(&message.body, 13.5, Some(width));
            tr.draw_text(
                scene,
                &message.body,
                13.5,
                colors.text_primary,
                t * Affine::translate((x, y)),
                Some(width),
            );
            y += f64::from(body_h) + 24.0;
            fill_rect(
                scene,
                t,
                Rect::new(x, y - 12.0, bounds.x1 - 24.0, y - 11.0),
                colors.border_standard,
            );
        }
    }

    fn draw_empty_reader(ctx: &mut DrawContext<'_>, bounds: Rect) {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let colors = &ctx.theme.tokens.colors;
        let center = bounds.center();
        Icon::Mail.draw(
            scene,
            ctx.transform,
            Rect::new(
                center.x - 26.0,
                center.y - 66.0,
                center.x + 26.0,
                center.y - 14.0,
            ),
            colors.neutral[4],
        );
        let hint = "Select a conversation to read";
        let (width, _) = tr.measure(hint, 14.0, None);
        tr.draw_text(
            scene,
            hint,
            14.0,
            colors.text_dim,
            ctx.transform * Affine::translate((center.x - f64::from(width) / 2.0, center.y)),
            None,
        );
    }

    /// The message-thread portion of [`Self::draw_reader`]: the empty-state
    /// placeholder, the action bar, the subject/meta header, and each message
    /// card. Returns the head message's id, its HTML body (if any), and the
    /// rect reserved for the sandboxed HTML view, or `None` if there is no
    /// selected thread to draw. Part of [`Self::draw_reader`].
    fn draw_reader_thread(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rr: Rect,
    ) -> Option<(MessageId, Option<String>, Option<Rect>)> {
        let c = &theme.tokens.colors;
        let mut chip_hits: Vec<(Rect, MessageId, usize)> = Vec::new();
        let st = self.shared.borrow();

        let msgs = st.selected_thread_messages();
        if msgs.is_empty() {
            self.reader_attachment_hits.clear();
            Self::draw_empty_reader(
                &mut DrawContext {
                    scene,
                    transform: t,
                    text: tr,
                    theme,
                },
                rr,
            );
            return None;
        }

        // Action bar (header strip).
        let compact = st.narrow || rr.width() < READER_COMPACT_W;
        let starred = st
            .selected
            .and_then(|id| st.mailbox.get(id))
            .is_some_and(|m| m.starred);
        self.draw_reader_action_bar(
            &mut DrawContext {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            compact,
            starred,
            st.last_mouse,
        );

        let Some(head) = msgs.last() else {
            // Unreachable: `msgs.is_empty()` returned above — but bail rather
            // than panic if that invariant is ever violated.
            return None;
        };
        let content_x = rr.x0 + 24.0;
        let content_w = (rr.width() - 48.0) as f32;
        let mut y = Self::draw_reader_thread_heading(
            &mut DrawContext {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            content_x,
            content_w,
            head,
            msgs.len(),
            st.folder,
            rr.y0 + HEADER_H + 8.0,
        );
        y = Self::draw_inspection_badge(
            &mut DrawContext {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            content_x,
            y,
            head.read,
            st.inspection == Some(head.id),
        );
        // The rule under the thread heading is derived against the reader's
        // raised ground (HIG §9.5).
        fill_rect(
            scene,
            t,
            Rect::new(content_x, y, rr.x1 - 24.0, y + 1.0),
            edge_on(c.surfaces.raised),
        );
        y += 16.0;

        // Render the thread. When the head (newest, being-read) message carries
        // an HTML body, its body renders in the sandboxed WebView; older
        // messages and plain-text mail render as text cards.
        let head_id = head.id;
        let head_html = head.html_body.clone();
        let webview_rect = self.draw_reader_thread_messages(
            &mut DrawContext {
                scene,
                transform: t,
                text: tr,
                theme,
            },
            &msgs,
            &ReaderThreadLayout {
                reader: rr,
                content_x,
                content_w,
                y,
                head_has_html: head_html.is_some(),
            },
            &mut chip_hits,
        );
        self.reader_attachment_hits = chip_hits;
        drop(st);
        Some((head_id, head_html, webview_rect))
    }

    /// Draw the conversation title and its compact count/folder context,
    /// returning the baseline after that header.
    fn draw_reader_thread_heading(
        ctx: &mut DrawContext<'_>,
        content_x: f64,
        content_w: f32,
        head: &Message,
        message_count: usize,
        folder: Folder,
        y: f64,
    ) -> f64 {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        let subject = elide(
            tr,
            &head.thread_subject(),
            20.0,
            TextWeight::Bold,
            f64::from(content_w),
        );
        tr.draw_text_weighted(
            scene,
            &subject,
            20.0,
            c.text_primary,
            t * Affine::translate((content_x, y)),
            None,
            TextWeight::Bold,
        );
        let meta_y = y + 30.0;
        tr.draw_text(
            scene,
            &format!(
                "{message_count} message{} · {}",
                if message_count == 1 { "" } else { "s" },
                folder.title()
            ),
            12.0,
            c.text_dim,
            t * Affine::translate((content_x, meta_y)),
            None,
        );
        meta_y + 26.0
    }

    /// Draw the persistent visual counterpart of an explicit no-side-effect
    /// inspection. Returns the next baseline unchanged when the reader is
    /// showing a normal message open.
    fn draw_inspection_badge(
        ctx: &mut DrawContext<'_>,
        content_x: f64,
        y: f64,
        already_read: bool,
        inspecting: bool,
    ) -> f64 {
        if !inspecting {
            return y;
        }
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        let label = if already_read {
            "Preview · read state unchanged"
        } else {
            "Preview · remains unread"
        };
        let (label_w, _) = tr.measure(label, 11.0, None);
        let pill = Rect::new(
            content_x,
            y - 5.0,
            content_x + f64::from(label_w) + 34.0,
            y + 17.0,
        );
        ui::surface::pill(
            scene,
            t,
            pill,
            c.accent[5].with_alpha_factor(0.14),
            None,
            Some(7.0),
        );
        Icon::Eye.draw(
            scene,
            t,
            Rect::new(pill.x0 + 7.0, pill.y0 + 4.0, pill.x0 + 21.0, pill.y0 + 18.0),
            c.accent[5],
        );
        tr.draw_text_weighted(
            scene,
            label,
            11.0,
            c.accent[5],
            t * Affine::translate((pill.x0 + 25.0, pill.y0 + 4.0)),
            None,
            TextWeight::Medium,
        );
        y + 28.0
    }

    /// Draw each message card in the thread, starting at `y`: the head
    /// message reserves the rest of the reader for the sandboxed HTML view
    /// when it carries an HTML body, everything else draws as a text card.
    /// Returns the reserved HTML webview rect, if any. Part of
    /// [`Self::draw_reader_thread`].
    fn draw_reader_thread_messages(
        &self,
        ctx: &mut DrawContext<'_>,
        msgs: &[&Message],
        layout: &ReaderThreadLayout,
        chip_hits: &mut Vec<(Rect, MessageId, usize)>,
    ) -> Option<Rect> {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let ReaderThreadLayout {
            reader,
            content_x,
            content_w,
            mut y,
            head_has_html,
        } = *layout;
        let mut webview_rect: Option<Rect> = None;
        for (i, m) in msgs.iter().enumerate() {
            if y > reader.y1 - 40.0 {
                break;
            }
            let is_head = i == msgs.len() - 1;
            if is_head && head_has_html {
                // Head card: draw the header, then reserve the rest of the
                // reader for the sandboxed HTML body (painted once the shared
                // borrow is released, since the view needs `&mut self`).
                y = self.draw_card_header(
                    scene,
                    t,
                    tr,
                    theme,
                    &CardHeaderArgs {
                        msg: m,
                        x: content_x,
                        y,
                        width: content_w,
                    },
                );
                y += 8.0;
                let bottom = reader.y1 - 46.0; // room for the remote-images bar
                if bottom > y + 48.0 {
                    webview_rect = Some(Rect::new(
                        content_x,
                        y,
                        content_x + f64::from(content_w),
                        bottom,
                    ));
                }
            } else {
                y = self.draw_message_card(
                    &mut DrawContext {
                        scene,
                        transform: t,
                        text: tr,
                        theme,
                    },
                    &CardHeaderArgs {
                        msg: m,
                        x: content_x,
                        y,
                        width: content_w,
                    },
                    is_head,
                    chip_hits,
                );
                y += 14.0;
            }
        }
        webview_rect
    }

    /// Draw the "remote content blocked / loaded" pill under an HTML body and
    /// record its rect for hit-testing. Off (blocked) by default; clicking it
    /// opts the current message in to remote images.
    fn draw_remote_toggle(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        interactive: bool,
    ) {
        let c = &theme.tokens.colors;
        let y = body.y1 + 8.0;
        let (icon, label, col) = if !interactive {
            (
                Icon::Shield,
                "Safe text version · Rich HTML unavailable",
                c.text_dim,
            )
        } else if self.html_load_remote {
            (Icon::Image, "Remote images loaded", c.text_dim)
        } else {
            (
                Icon::Ban,
                "Remote content blocked · Load images",
                c.semantic.warning,
            )
        };
        let (lw, _) = tr.measure(label, 11.5, None);
        let pill = Rect::new(body.x0, y, body.x0 + 30.0 + f64::from(lw) + 14.0, y + 28.0);
        let hov = pill.contains(self.shared.borrow().last_mouse);
        card(
            scene,
            t,
            pill,
            8.0,
            if hov { theme.bg_hover } else { c.lift(0.35) },
        );
        icon.draw(
            scene,
            t,
            Rect::new(pill.x0 + 8.0, pill.y0 + 6.0, pill.x0 + 24.0, pill.y0 + 22.0),
            col,
        );
        tr.draw_text(
            scene,
            label,
            11.5,
            c.text_primary,
            t * Affine::translate((pill.x0 + 30.0, pill.y0 + 7.0)),
            None,
        );
        self.html_toggle_rect = interactive.then_some(pill);
    }

    #[allow(
        clippy::many_single_char_names,
        clippy::unused_self,
        reason = "drawing code follows the renderer's established transform and coordinate notation"
    )]
    fn draw_native_html_fallback(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        body: Rect,
        plain: &str,
    ) {
        let c = &theme.tokens.colors;
        // A base-filled box on the raised reader: fill + derived edge.
        card(scene, t, body, 6.0, c.surfaces.base);
        let x = body.x0 + 18.0;
        let width = (body.width() - 36.0).max(80.0) as f32;
        let mut y = body.y0 + 18.0;
        for paragraph in plain.lines() {
            if y >= body.y1 - 24.0 {
                break;
            }
            if paragraph.trim().is_empty() {
                y += 9.0;
                continue;
            }
            let (_w, h) = tr.measure(paragraph, 13.5, Some(width));
            tr.draw_text(
                scene,
                paragraph,
                13.5,
                c.text_primary,
                t * Affine::translate((x, y)),
                Some(width),
            );
            y += f64::from(h.max(19.0)) + 5.0;
        }
    }

    /// Lazily create the reader's HTML-body `WebView` (and its engine) on first
    /// use. Text-only mail never touches this, so it stays webview-free.
    fn ensure_html_view(&mut self) -> &mut WebView {
        if self.html_engine.is_none() {
            let (engine, live) = new_html_engine();
            self.html_engine = Some(engine);
            self.html_engine_live = live;
        }
        let engine = self
            .html_engine
            .clone()
            .unwrap_or_else(|| new_html_engine().0);
        self.html_view
            .get_or_insert_with(|| WebView::new(engine).with_test_id("mail_html_body"))
    }

    /// Load `html` for message `id` into the sandboxed view — building the
    /// remote-blocked `data:` document and installing the matching content
    /// filter — unless that exact (message, block state) is already loaded.
    fn load_html_body(&mut self, id: MessageId, html: &str) {
        let block_remote = !self.html_load_remote;
        let key = (id, block_remote);
        if self.html_key == Some(key) {
            return;
        }
        let url = crate::html_mail::data_url(html, block_remote);
        let filter = std::sync::Arc::new(crate::html_mail::content_filter_json(block_remote));
        let engine = self.ensure_html_view().engine().clone();
        engine.send(WebViewCommand::SetContentFilter(Some(filter)));
        engine.send(WebViewCommand::Navigate(url));
        self.html_key = Some(key);
    }

    /// Inject the HTML-body engine (tests use this to observe the commands the
    /// reader sends). Must be called before the first HTML mail is drawn.
    #[cfg(test)]
    pub(crate) fn set_html_engine(&mut self, engine: SharedEngine) {
        self.html_engine = Some(engine);
        self.html_engine_live = true;
    }

    /// The (message, `block_remote`) currently loaded into the HTML view, if any.
    #[cfg(test)]
    pub(crate) fn html_loaded_key(&self) -> Option<(MessageId, bool)> {
        self.html_key
    }

    /// Draw the per-message card header (avatar · sender · date · recipients)
    /// and return the y where the body should start. Shared by the text card
    /// path and the HTML-body (`WebView`) path so both read identically.
    #[expect(
        clippy::too_many_lines,
        reason = "the card header paints avatar, sender, date and recipients in one pass sharing the running y cursor both body paths start from"
    )]
    fn draw_card_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        args: &CardHeaderArgs<'_>,
    ) -> f64 {
        let CardHeaderArgs {
            msg,
            x: cx,
            y: cy,
            width,
        } = *args;
        let c = &theme.tokens.colors;
        let st = self.shared.borrow();
        let width = f64::from(width);

        // Measure every piece with the style it is painted with, then let one
        // layout place them: the name/email block is centred on the avatar,
        // and the right column (date over recipients) shares one right edge.
        let date = format_absolute(msg.timestamp_min);
        let (dw, dh) = tr.measure(&date, 11.5, None);
        let to_budget = (width * 0.4).max(150.0).min(width - 60.0);
        let to_line = elide(
            tr,
            &format!(
                "to {}",
                msg.to
                    .iter()
                    .map(super::model::Addr::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            11.0,
            TextWeight::Regular,
            to_budget,
        );
        let (tow, toh) = tr.measure(&to_line, 11.0, None);
        let (_, name_h) = tr.measure_weighted("Ag", 13.5, None, TextWeight::Semibold);
        let (_, email_h) = tr.measure("Ag", 11.5, None);
        // The disc follows the text scale so the shared avatar's initials
        // (0.4× the disc, through the renderer) stay inside it.
        let avatar_r = LIST_AVATAR_R * f64::from(theme.text_scale).max(1.0);
        let layout = card_header_layout(&CardHeaderMetrics {
            x: cx,
            y: cy,
            width,
            avatar_r,
            name_h: f64::from(name_h),
            email_h: f64::from(email_h),
            date_w: f64::from(dw),
            to_w: f64::from(tow),
            gap: f64::from(theme.tokens.spacing.s3),
        });

        draw_avatar(
            &mut AvatarPaintContext {
                scene,
                text: tr,
                transform: t,
                theme,
                images: &st.images,
            },
            layout.avatar_c,
            avatar_r,
            &msg.from,
            0,
        );
        let from_name = elide(
            tr,
            &msg.from.display(),
            13.5,
            TextWeight::Semibold,
            layout.from_budget,
        );
        tr.draw_text_weighted(
            scene,
            &from_name,
            13.5,
            c.text_primary,
            t * Affine::translate((layout.text_x, layout.name_y)),
            None,
            TextWeight::Semibold,
        );
        let from_email = elide(
            tr,
            &msg.from.email,
            11.5,
            TextWeight::Regular,
            layout.from_budget,
        );
        tr.draw_text(
            scene,
            &from_email,
            11.5,
            c.text_dim,
            t * Affine::translate((layout.text_x, layout.email_y)),
            None,
        );
        // Date and recipients bottom-align their line boxes to the name and
        // email lines, so the two columns read as two rows.
        tr.draw_text(
            scene,
            &date,
            11.5,
            c.text_dim,
            t * Affine::translate((layout.date_x, layout.name_y + layout.name_h - f64::from(dh))),
            None,
        );
        tr.draw_text(
            scene,
            &to_line,
            11.0,
            c.text_dim,
            t * Affine::translate((
                layout.to_x,
                layout.email_y + layout.email_h - f64::from(toh),
            )),
            None,
        );
        layout.bottom
    }

    /// Draw the "N attachments" label and the wrapping row of attachment
    /// chips for an expanded message card, recording each chip's hit region.
    /// Returns the y past the chips. Part of [`Self::draw_message_card`].
    fn draw_attachment_chips(
        &self,
        ctx: &mut DrawContext<'_>,
        msg: &Message,
        cx: f64,
        card_x1: f64,
        mut yy: f64,
        chip_hits: &mut Vec<(Rect, MessageId, usize)>,
    ) -> f64 {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        yy += 8.0;
        tr.draw_text_weighted(
            scene,
            &format!(
                "{} attachment{}",
                msg.attachments.len(),
                if msg.attachments.len() == 1 { "" } else { "s" }
            ),
            11.5,
            c.text_dim,
            t * Affine::translate((cx, yy)),
            None,
            TextWeight::Medium,
        );
        yy += 20.0;
        let mut ax = cx;
        let hov_mouse = self.shared.borrow().last_mouse;
        for (ai, a) in msg.attachments.iter().enumerate() {
            let label = format!("{}  {}", a.filename, a.size_human());
            let (lw, _) = tr.measure(&label, 11.5, None);
            // Room for the trailing download/state glyph.
            let chip_w = f64::from(lw) + 64.0;
            if ax + chip_w > card_x1 {
                ax = cx;
                yy += 36.0;
            }
            let chip = Rect::new(ax, yy, ax + chip_w, yy + 30.0);
            let hov = chip.contains(hov_mouse);
            card(
                scene,
                t,
                chip,
                8.0,
                if hov { theme.bg_hover } else { c.lift(0.35) },
            );
            a.icon().draw(
                scene,
                t,
                Rect::new(chip.x0 + 7.0, chip.y0 + 7.0, chip.x0 + 23.0, chip.y0 + 23.0),
                c.neutral[7],
            );
            tr.draw_text(
                scene,
                &a.filename,
                11.5,
                c.text_primary,
                t * Affine::translate((chip.x0 + 30.0, chip.y0 + 4.0)),
                Some(lw),
            );
            tr.draw_text(
                scene,
                &a.size_human(),
                10.0,
                c.text_dim,
                t * Affine::translate((chip.x0 + 30.0, chip.y0 + 17.0)),
                None,
            );
            // Trailing affordance: a save/download icon when the bytes are
            // present (clickable), a cloud when it must still be hydrated
            // from the server.
            let (icon, col) = if a.is_downloadable() {
                (Icon::Download, c.accent[5])
            } else {
                (Icon::CloudDownload, c.neutral[5])
            };
            icon.draw(
                scene,
                t,
                Rect::new(chip.x1 - 24.0, chip.y0 + 7.0, chip.x1 - 8.0, chip.y0 + 23.0),
                col,
            );
            chip_hits.push((chip, msg.id, ai));
            // Sibling chips sit an `s3` apart (HIG §7b).
            ax += chip_w + f64::from(theme.tokens.spacing.s3);
        }
        yy + 30.0
    }

    /// Draw one message card in the reader thread (header + body or a
    /// collapsed preview line) and return the y past it.
    fn draw_message_card(
        &self,
        ctx: &mut DrawContext<'_>,
        args: &CardHeaderArgs<'_>,
        expanded: bool,
        chip_hits: &mut Vec<(Rect, MessageId, usize)>,
    ) -> f64 {
        let scene = &mut *ctx.scene;
        let tr = &mut *ctx.text;
        let theme = ctx.theme;
        let t = ctx.transform;
        let c = &theme.tokens.colors;
        let CardHeaderArgs {
            msg,
            x,
            y: _,
            width,
        } = *args;
        let cx = x;
        let card_x1 = cx + f64::from(width);
        let mut yy = self.draw_card_header(scene, t, tr, theme, args);
        if expanded {
            for para in msg.body.split('\n') {
                if para.trim().is_empty() {
                    yy += 8.0;
                    continue;
                }
                let quote = para.starts_with('>');
                let col = if quote { c.text_dim } else { c.neutral[8] };
                let (_pw, ph) = tr.measure(para, 13.0, Some(width - 8.0));
                tr.draw_text(
                    scene,
                    para,
                    13.0,
                    col,
                    t * Affine::translate((cx, yy)),
                    Some(width - 8.0),
                );
                yy += f64::from(ph.max(18.0)) + 2.0;
            }
            if msg.has_attachments() {
                yy = self.draw_attachment_chips(
                    &mut DrawContext {
                        scene,
                        transform: t,
                        text: tr,
                        theme,
                    },
                    msg,
                    cx,
                    card_x1,
                    yy,
                    chip_hits,
                );
            }
        } else {
            // Collapsed preview: a single elided line so a narrow reader column
            // can't wrap it into the following card.
            let snip = elide(
                tr,
                &msg.snippet(),
                12.0,
                TextWeight::Regular,
                f64::from(width - 8.0),
            );
            tr.draw_text(
                scene,
                &snip,
                12.0,
                c.text_dim,
                t * Affine::translate((cx, yy)),
                None,
            );
            yy += 18.0;
        }
        yy
    }

    // ---- event handling ---------------------------------------------------

    fn on_left_press(&mut self, doc: &mut AutoCommit, pt: Point, handled: &mut bool) {
        if !self.rect.contains(pt) {
            return;
        }
        match self.role {
            Role::Sidebar => self.on_left_press_sidebar(doc, pt, handled),
            Role::List => self.on_left_press_list(doc, pt, handled),
            Role::Reader => self.on_left_press_reader(doc, pt, handled),
        }
    }

    /// The sidebar's left-click routing: compose, per-account settings/
    /// toggle/select, and folder select. Part of [`Self::on_left_press`].
    fn on_left_press_sidebar(&self, doc: &mut AutoCommit, pt: Point, handled: &mut bool) {
        let Some(hit) = self.hit_at(pt) else {
            return;
        };
        match hit {
            Hit::Compose => self.shared.borrow_mut().push_intent(Intent::Compose),
            Hit::AccountSettings(ai) => {
                // Per-account settings, scoped to the clicked account (not
                // the active one).
                self.shared
                    .borrow_mut()
                    .push_intent(Intent::Settings(Some(ai)));
            }
            Hit::AddAccount => {
                // Adding an account is its own action (a blank sheet).
                self.shared.borrow_mut().push_intent(Intent::Settings(None));
            }
            Hit::AccountToggle(ai) => {
                self.shared.borrow_mut().toggle_account_collapsed(ai);
            }
            Hit::Account(ai) => {
                let mut st = self.shared.borrow_mut();
                st.set_active_account(ai);
                st.select_folder(doc, Folder::Inbox);
                st.push_intent(Intent::CloseDrawer);
            }
            Hit::Folder(ai, f) => {
                let mut st = self.shared.borrow_mut();
                st.set_active_account(ai);
                st.select_folder(doc, f);
                st.push_intent(Intent::CloseDrawer);
            }
            _ => {}
        }
        *handled = true;
    }

    /// The list pane's left-click routing: refresh, search focus/clear, star
    /// toggle, row select/open, and arming a potential row swipe. Part of
    /// [`Self::on_left_press`].
    fn on_left_press_list(&mut self, doc: &mut AutoCommit, pt: Point, handled: &mut bool) {
        if let Some(hit) = self.hit_at(pt) {
            match hit {
                Hit::Refresh => {
                    // Manually re-fetch the current folder from the server.
                    let (account, folder) = {
                        let st = self.shared.borrow();
                        (st.accounts.active_config(), st.folder)
                    };
                    let mut st = self.shared.borrow_mut();
                    if !account.email.is_empty() {
                        st.push_intent(Intent::Engine(crate::engine::EngineRequest::fetch(
                            account, folder,
                        )));
                    }
                    st.status = Some(format!("Syncing {}…", folder.title()));
                    st.dirty = true;
                    *handled = true;
                }
                Hit::ToggleBulkSelection => {
                    let selecting = self.shared.borrow().bulk_selecting;
                    self.shared.borrow_mut().set_bulk_selecting(!selecting);
                    *handled = true;
                }
                Hit::SelectAllVisible => {
                    self.shared.borrow_mut().select_all_visible();
                    *handled = true;
                }
                Hit::Bulk(action) => {
                    if action == BulkAction::Purge {
                        self.shared
                            .borrow_mut()
                            .push_intent(Intent::ConfirmBulkPurge);
                        *handled = true;
                        return;
                    }
                    let affected = self.shared.borrow_mut().apply_bulk_action(doc, action);
                    if affected > 0 {
                        self.shared.borrow_mut().status = Some(format!(
                            "{} {} message{}",
                            action.label(),
                            affected,
                            if affected == 1 { "" } else { "s" }
                        ));
                    }
                    *handled = true;
                }
                Hit::SearchFocus => {
                    self.set_search_focus(true);
                    *handled = true;
                }
                Hit::SearchClear => {
                    self.shared.borrow_mut().search.clear();
                    if let Some(i) = self.search_input.as_mut() {
                        i.set_text("");
                    }
                    self.set_search_focus(false);
                    *handled = true;
                }
                Hit::RowStar(id) => {
                    let mut st = self.shared.borrow_mut();
                    st.toggle_star(doc, id);
                    *handled = true;
                }
                Hit::Row(id) => {
                    self.set_search_focus(false);
                    if self.shared.borrow().bulk_selecting {
                        self.shared.borrow_mut().toggle_bulk_selection(id);
                        *handled = true;
                        return;
                    }
                    let is_draft = self
                        .shared
                        .borrow()
                        .mailbox
                        .get(id)
                        .is_some_and(|m| m.folder == Folder::Drafts);
                    if is_draft {
                        // A Drafts row reopens the composer, not the reader.
                        self.shared.borrow_mut().push_intent(Intent::EditDraft(id));
                    } else {
                        self.shared.borrow_mut().select_message(doc, id);
                    }
                    *handled = true;
                }
                _ => {}
            }
        }
        // Record a potential swipe origin whenever the press lands on a row.
        // A later drag past the threshold turns it into a swipe action
        // (archive/delete) on release; a release without a drag is a plain
        // tap, which already selected on press.
        if !self.shared.borrow().bulk_selecting
            && let Some(id) = self.row_id_at(pt)
        {
            self.mouse_down_row = Some((id, pt));
        }
    }

    /// The reader's left-click routing: the remote-images toggle, an
    /// attachment-chip save, and the action bar (Reply/Archive/Delete/Star/
    /// Mark unread). Part of [`Self::on_left_press`].
    fn on_left_press_reader(&mut self, doc: &mut AutoCommit, pt: Point, handled: &mut bool) {
        // The remote-images toggle under an HTML body: opt this message in to
        // (or back out of) loading remote content.
        if self.html_toggle_rect.is_some_and(|r| r.contains(pt)) {
            self.html_load_remote = !self.html_load_remote;
            // Force a reload with the new policy on the next draw.
            self.html_key = None;
            self.dirty = true;
            self.shared.borrow_mut().dirty = true;
            *handled = true;
            return;
        }
        // An attachment chip saves the real bytes to disk.
        if let Some((_, mid, ai)) = self
            .reader_attachment_hits
            .iter()
            .find(|(rc, _, _)| rc.contains(pt))
            .copied()
        {
            self.save_attachment_click(mid, ai);
            *handled = true;
            return;
        }
        if let Some(hit) = self.hit_at(pt) {
            let sel = self.shared.borrow().selected;
            if let Some(id) = sel {
                let mut st = self.shared.borrow_mut();
                match hit {
                    Hit::Reply => st.push_intent(Intent::Reply(id)),
                    Hit::Archive => st.apply_message_action(doc, id, CtxAction::Archive),
                    Hit::Delete => st.apply_message_action(doc, id, CtxAction::Delete),
                    Hit::Star => st.apply_message_action(doc, id, CtxAction::ToggleStar),
                    Hit::Unread => {
                        st.set_read(doc, id, false);
                    }
                    _ => {}
                }
                *handled = true;
            }
        }
    }

    /// Request saving a reader attachment through the root's host-portal
    /// boundary. A pane never invents a Downloads path or writes the host
    /// filesystem itself.
    fn save_attachment_click(&self, mid: MessageId, ai: usize) {
        self.shared
            .borrow_mut()
            .push_intent(Intent::SaveAttachment(mid, ai));
    }

    fn set_search_focus(&mut self, on: bool) {
        if let Some(i) = self.search_input.as_mut() {
            i.set_focus(on);
        }
        let mut st = self.shared.borrow_mut();
        st.search_focused = on;
        st.dirty = true;
    }
}

/// The web engine backing HTML-mail rendering. Under the `html-webview`
/// feature this is the real WPE `WebKit` engine (the same one the browser uses),
/// falling back to the hermetic test engine if it can't start; otherwise it is
/// the deterministic test engine — which still exercises the full
/// sandbox/remote-block plumbing in the default build and `--shots`.
fn new_html_engine() -> (SharedEngine, bool) {
    #[cfg(feature = "html-webview")]
    {
        match ui::webview::wpe::WpeEngine::new() {
            Ok(e) => return (e, true),
            Err(err) => tracing::warn!(%err, "mail: WPE engine unavailable; using test engine"),
        }
    }
    let e: SharedEngine = ui::webview::test_engine::TestEngine::new();
    (e, false)
}

impl Pane {
    /// Forward pointer-move/wheel into the sandboxed HTML body view while one
    /// is actually shown in the reader, so an HTML mail scrolls; the view
    /// consumes a wheel that lands over it. Part of [`Widget::handle_event`].
    fn forward_html_view_event(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) {
        let over_html = self.role == Role::Reader
            && self.html_view_rect.is_some()
            && matches!(
                event,
                WindowEvent::MouseWheel { .. } | WindowEvent::CursorMoved { .. }
            );
        if !over_html {
            return;
        }
        if let Some(view) = self.html_view.as_mut() {
            view.handle_event(
                event,
                &mut ui::widget::EventCtx {
                    scale_factor: ctx.scale_factor,
                    modifiers: &ctx.modifiers,
                    state: ctx.state,
                    path: ctx.path,
                    handled: ctx.handled,
                    focus_target: ctx.focus_target,
                    node_id: ctx.node_id,
                    taffy: ctx.taffy,
                },
            );
        }
    }

    /// Route a keyboard event to the focused search field (list role only),
    /// reflecting the field's value into the query + first-row selection.
    /// Returns whether [`Widget::handle_event`] should return immediately
    /// after this (true whenever the event was a keyboard event on the list
    /// role, matching the field regardless of whether search was focused).
    /// Part of [`Widget::handle_event`].
    fn handle_search_keyboard(&mut self, event: &WindowEvent, ctx: &mut EventContext<'_>) -> bool {
        if !(matches!(event, WindowEvent::KeyboardInput { .. }) && self.role == Role::List) {
            return false;
        }
        let focused = self.shared.borrow().search_focused;
        if focused {
            if let Some(inp) = self.search_input.as_mut() {
                inp.handle_event(
                    event,
                    &mut ui::widget::EventCtx {
                        scale_factor: ctx.scale_factor,
                        modifiers: &ctx.modifiers,
                        state: ctx.state,
                        path: ctx.path,
                        handled: ctx.handled,
                        focus_target: ctx.focus_target,
                        node_id: ctx.node_id,
                        taffy: ctx.taffy,
                    },
                );
            }
            // Reflect the field into the query + first-row selection.
            let value = self
                .search_input
                .as_ref()
                .map(|i| i.value().to_string())
                .unwrap_or_default();
            let mut st = self.shared.borrow_mut();
            st.search = value;
            st.selected = st.list_rows().first().map(|(id, _)| *id);
            st.dirty = true;
            *ctx.handled = true;
        }
        true
    }
}

impl Widget for Pane {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        if let Some(i) = self.search_input.as_mut() {
            i.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.layout_scale = f64::from(theme.text_scale);
        // `Widget::build_layout` returns a bare `NodeId`, not a `Result` — a
        // framework-wide contract this one widget can't change — so a
        // `new_leaf` failure (which a valid, freshly-created `Style` cannot
        // produce) has no propagation path here.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf on a fresh tree with a valid Style cannot fail; Widget::build_layout's Result-free signature is shared by every implementor and out of this crate's scope to change"
        )]
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
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // `node` is the id this same widget inserted into this exact tree in
        // `build_layout` earlier in the same frame, so `layout` cannot fail
        // here in practice; `apply_layout`'s `()` return leaves no error
        // path, and that signature is shared framework-wide.
        #[allow(
            clippy::unwrap_used,
            reason = "node was inserted into this tree by this widget's own build_layout earlier in the same frame; Widget::apply_layout's Result-free signature is shared by every implementor and out of this crate's scope to change"
        )]
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.rebuild_hits();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Consume a pending `/`-shortcut focus request (list only).
        if self.role == Role::List {
            let req = {
                let mut st = self.shared.borrow_mut();
                std::mem::take(&mut st.focus_search_request)
            };
            if req {
                self.set_search_focus(true);
            }
            // Measure the row bands for this text scale before the hit list
            // is rebuilt from them, so hits and paint describe the same rows.
            self.list_metrics = Some(ListRowMetrics::measure(tr, theme));
        }
        self.rebuild_hits();
        match self.role {
            Role::Sidebar => self.draw_sidebar(scene, t, tr, theme),
            Role::List => {
                // The query may also change through restored collaborative
                // state or a resource operation, not only this input's event
                // handler. Keep the retained editor projection in lockstep so
                // a focused search never paints an empty field over active
                // results (and so assistive technology gets the real value).
                let query = self.shared.borrow().search.clone();
                if let Some(inp) = self.search_input.as_mut()
                    && inp.value() != query
                {
                    inp.set_text(&query);
                }
                self.draw_list(scene, t, tr, theme);
                if let Some(inp) = self.search_input.as_mut() {
                    inp.draw(scene, t, tr, theme, presence);
                }
            }
            Role::Reader => self.draw_reader(scene, t, tr, theme, presence),
        }
    }

    fn tooltip(&self) -> Option<&str> {
        let point = self.shared.borrow().last_mouse;

        // A compact reader action bar deliberately becomes icon-only on a
        // phone.  Keep those actions discoverable, and name the two less
        // obvious reader controls before consulting the ordinary hit list.
        // The same descriptions are also useful on a wide pointer UI, where
        // attachment and remote-content policy otherwise have no text label.
        if self.role == Role::Reader {
            if self
                .html_toggle_rect
                .is_some_and(|rect| rect.contains(point))
            {
                return Some("Load or block remote images");
            }
            if self
                .reader_attachment_hits
                .iter()
                .any(|(rect, _, _)| rect.contains(point))
            {
                return Some("Save attachment");
            }
        }

        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .map(|(_, hit)| match hit {
                Hit::Compose => "Compose (C)",
                Hit::Account(_) => "Open account",
                Hit::AccountToggle(_) => "Expand or collapse account",
                Hit::AccountSettings(_) => "Account settings",
                Hit::AddAccount => "Add account",
                Hit::Folder(_, _) => "Open folder",
                Hit::Row(id) => {
                    if self
                        .shared
                        .borrow()
                        .mailbox
                        .get(*id)
                        .is_some_and(|message| message.folder == Folder::Drafts)
                    {
                        "Edit draft"
                    } else {
                        "Open message"
                    }
                }
                Hit::RowStar(id) => {
                    if self
                        .shared
                        .borrow()
                        .mailbox
                        .get(*id)
                        .is_some_and(|message| message.starred)
                    {
                        "Unstar (S)"
                    } else {
                        "Star (S)"
                    }
                }
                Hit::SearchFocus => "Search mail (/ or Ctrl+F)",
                Hit::SearchClear => "Clear search",
                Hit::Refresh => "Refresh (F5)",
                Hit::ToggleBulkSelection => {
                    if self.shared.borrow().bulk_selecting {
                        "Done selecting messages (Esc)"
                    } else {
                        "Select messages for bulk actions (X)"
                    }
                }
                Hit::SelectAllVisible => "Select all visible messages",
                Hit::Bulk(BulkAction::Archive) => "Archive selected messages (E)",
                Hit::Bulk(BulkAction::Delete) => "Move selected messages to Trash (Delete)",
                Hit::Bulk(BulkAction::Purge) => {
                    "Delete selected trashed messages permanently (Shift+Delete)"
                }
                Hit::Bulk(BulkAction::MarkRead) => "Mark selected messages read",
                Hit::Bulk(BulkAction::MarkUnread) => "Mark selected messages unread",
                Hit::Reply => "Reply (R)",
                Hit::Archive => "Archive (E)",
                Hit::Delete => "Delete (Delete)",
                Hit::Star => {
                    let starred = {
                        let state = self.shared.borrow();
                        state
                            .selected
                            .and_then(|id| state.mailbox.get(id))
                            .is_some_and(|message| message.starred)
                    };
                    if starred { "Unstar (S)" } else { "Star (S)" }
                }
                Hit::Unread => "Mark unread (U)",
            })
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        self.rebuild_hits();

        self.forward_html_view_event(
            event,
            &mut EventContext {
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state,
                path: ctx.path,
                handled: ctx.handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        );
        if *ctx.handled {
            return;
        }

        if self.handle_search_keyboard(
            event,
            &mut EventContext {
                scale_factor: ctx.scale_factor,
                modifiers: *ctx.modifiers,
                state: ctx.state,
                path: ctx.path,
                handled: ctx.handled,
                focus_target: ctx.focus_target,
                node_id: ctx.node_id,
                taffy: ctx.taffy,
            },
        ) {
            return;
        }

        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if self.role == Role::List {
                    let pt = self.shared.borrow().last_mouse;
                    if let Some(id) = self.row_id_at(pt) {
                        self.shared
                            .borrow_mut()
                            .push_intent(Intent::ContextMenu(id, pt));
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.shared.borrow().last_mouse;
                self.on_left_press(ctx.state, pt, ctx.handled);
            }
            WindowEvent::CursorMoved { .. } => {
                if self.role == Role::List
                    && let Some((id, start)) = self.mouse_down_row
                {
                    let dx = self.shared.borrow().last_mouse.x - start.x;
                    if dx.abs() > 6.0 {
                        self.swipe = Some(SwipeState { id, dx });
                        self.dirty = true;
                        self.shared.borrow_mut().dirty = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.role == Role::List => {
                if let Some(s) = self.swipe.take() {
                    if s.dx <= -SWIPE_THRESHOLD {
                        self.shared.borrow_mut().apply_message_action(
                            ctx.state,
                            s.id,
                            CtxAction::Archive,
                        );
                    } else if s.dx >= SWIPE_THRESHOLD {
                        self.shared.borrow_mut().apply_message_action(
                            ctx.state,
                            s.id,
                            CtxAction::Delete,
                        );
                    }
                    *ctx.handled = true;
                }
                self.mouse_down_row = None;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        match self.role {
            Role::Sidebar => "MailSidebar",
            Role::List => "MailList",
            Role::Reader => "MailReader",
        }
    }

    fn test_id(&self) -> Option<&str> {
        Some(match self.role {
            Role::Sidebar => "mail_sidebar",
            Role::List => "mail_list",
            Role::Reader => "mail_reader",
        })
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        // NB: a leaf WebView's default `needs_redraw` is always true, so it is
        // deliberately NOT folded in here (it would spin the compositor). A new
        // engine frame instead surfaces through `needs_animation_frame`.
        self.dirty
            || self.shared.borrow().dirty
            || self
                .search_input
                .as_ref()
                .is_some_and(ui::Widget::needs_redraw)
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the frame pump alive while the HTML body view has a freshly
        // rendered engine frame the scene hasn't painted yet.
        self.html_view
            .as_ref()
            .is_some_and(ui::Widget::needs_animation_frame)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some(i) = self.search_input.as_mut() {
            i.mark_drawn();
        }
        if let Some(v) = self.html_view.as_mut() {
            v.mark_drawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MailState;
    use ui::presence::PresenceCtx;
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, Modifiers};
    use winit::keyboard::{Key, NamedKey};

    fn shared() -> Shared {
        MailState::shared(MailState::new_sample(crate::app::avatar_store_offline()))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn empty_states_and_tooltips_explain_compact_controls() {
        // Phase `empty_message_states_explain_the_next_useful_step`.
        {
            let (_, title, body) = list_zero_state(false, false, "");
            assert_eq!(title, "Connect your email");
            assert!(body.contains("Add account"));

            let (_, title, body) = list_zero_state(false, true, "");
            assert_eq!(title, "Your inbox is ready");
            assert!(body.contains("next sync"));

            let (_, title, body) = list_zero_state(true, true, "roadmap");
            assert_eq!(title, "No matching messages");
            assert!(body.contains("keyword"));

            let (_, title, _) = list_zero_state(true, true, "");
            assert_eq!(title, "No messages in this folder");
        }

        // Phase `tooltips_keep_compact_and_icon_only_mail_controls_discoverable`.
        {
            let sh = shared();
            let target = Rect::new(10.0, 10.0, 40.0, 40.0);
            sh.borrow_mut().last_mouse = target.center();

            let mut sidebar = Pane::new(Role::Sidebar, sh.clone());
            sidebar.hits = vec![(target, Hit::Compose)];
            assert_eq!(ui::Widget::tooltip(&sidebar), Some("Compose (C)"));

            let mut list = Pane::new(Role::List, sh.clone());
            list.hits = vec![(target, Hit::Refresh)];
            assert_eq!(ui::Widget::tooltip(&list), Some("Refresh (F5)"));

            let mut reader = Pane::new(Role::Reader, sh);
            // On a narrow reader the button itself is an icon, so its tooltip is
            // the only textual explanation of this privacy-sensitive control.
            reader.html_toggle_rect = Some(target);
            reader.hits = vec![(target, Hit::Unread)];
            assert_eq!(
                ui::Widget::tooltip(&reader),
                Some("Load or block remote images")
            );
        }
    }

    fn reader_for(sh: &Shared, id: MessageId, w: f64, h: f64) -> Pane {
        {
            let mut doc = AutoCommit::new();
            sh.borrow_mut().select_message(&mut doc, id);
            sh.borrow_mut().take_intents();
        }
        let mut p = Pane::new(Role::Reader, sh.clone());
        p.rect = Rect::new(0.0, 0.0, w, h);
        p.rebuild_hits();
        p
    }

    fn draw(p: &mut Pane) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        p.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn dispatch(p: &mut Pane, ev: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        p.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }
    fn moved(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(p.x, p.y),
        }
    }
    fn named(k: NamedKey) -> WindowEvent {
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
            device_id: DeviceId::dummy(),
            event: ev,
            is_synthetic: false,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn reader_renders_bodies_safely_and_routes_star_and_attachments() {
        // Phase `reader_action_bar_star_toggles_star`.
        {
            let sh = shared();
            let id = {
                let st = sh.borrow();
                st.mailbox.messages_in(Folder::Inbox)[0].id
            };
            let mut p = reader_for(&sh, id, 900.0, 800.0);
            let before = sh.borrow().mailbox.get(id).unwrap().starred;
            let star = p
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::Star))
                .map(|(r, _)| *r)
                .expect("star button present");
            sh.borrow_mut().last_mouse = star.center();
            let mut doc = AutoCommit::new();
            let mut handled = false;
            p.on_left_press(&mut doc, star.center(), &mut handled);
            assert!(handled, "reader Star click consumed");
            assert_eq!(
                sh.borrow().mailbox.get(id).unwrap().starred,
                !before,
                "Star button toggled the flag"
            );
        }

        // Phase `reader_shows_one_card_for_a_refetched_message`.
        {
            // The confirmed defect: re-fetching a message stacked N header copies in
            // the reader. With stable ids the thread the reader iterates holds one.
            let sh = MailState::shared(MailState::new_live(
                crate::account::AccountStore::new(),
                crate::app::avatar_store_offline(),
            ));
            let raw = b"From: Ada <ada@x.example>\r\nTo: you@example.test\r\n\
    Subject: Launch\r\nMessage-ID: <dup-1@x.example>\r\n\r\nHello there.\r\n";
            let id = {
                let mut st = sh.borrow_mut();
                let m = crate::transport::parse_eml(raw, 100, Folder::Inbox).unwrap();
                let id = m.id;
                st.mailbox.insert(m);
                // A second and third fetch of the very same server message.
                st.mailbox
                    .insert(crate::transport::parse_eml(raw, 200, Folder::Inbox).unwrap());
                st.mailbox
                    .insert(crate::transport::parse_eml(raw, 300, Folder::Inbox).unwrap());
                st.selected = Some(id);
                id
            };
            assert_eq!(
                sh.borrow().mailbox.count(Folder::Inbox),
                1,
                "one message, not three"
            );
            assert_eq!(
                sh.borrow().selected_thread_messages().len(),
                1,
                "the reader draws exactly one card for a re-fetched mail"
            );
            let _ = id;
        }

        // Phase `html_mail_renders_in_sandboxed_webview_with_remote_blocked`.
        {
            use ui::webview::WebViewCommand;
            use ui::webview::test_engine::TestEngine;
            let sh = shared();
            let id = {
                let st = sh.borrow();
                st.mailbox
                    .messages
                    .values()
                    .find(|m| m.from.email == "news@weekly.example.test")
                    .expect("the sample HTML newsletter is present")
                    .id
            };
            {
                let mut doc = AutoCommit::new();
                sh.borrow_mut().select_message(&mut doc, id);
                sh.borrow_mut().take_intents();
            }
            let mut p = Pane::new(Role::Reader, sh);
            // Inject a concrete engine we can inspect for the commands the reader sends.
            let te = TestEngine::new();
            p.set_html_engine(te.clone());
            p.rect = Rect::new(0.0, 0.0, 960.0, 820.0);
            p.rebuild_hits();
            draw(&mut p);

            // The HTML body was loaded, remote content blocked by default.
            assert_eq!(
                p.html_loaded_key(),
                Some((id, true)),
                "HTML body loaded with remote blocked"
            );
            let cmds = te.commands();
            let nav = cmds
                .iter()
                .find_map(|c| {
                    if let WebViewCommand::Navigate(u) = c {
                        Some(u.clone())
                    } else {
                        None
                    }
                })
                .expect("the reader navigates the HTML body");
            assert!(
                nav.starts_with("data:text/html"),
                "renders a sandboxed data: document, not a remote URL: {nav}"
            );
            assert!(
                cmds.iter()
                    .any(|c| matches!(c, WebViewCommand::SetContentFilter(Some(_)))),
                "a remote-blocking content filter is installed"
            );
            // The remote-images affordance is shown (blocked by default).
            assert!(
                p.html_toggle_rect.is_some(),
                "the remote-images toggle is shown"
            );
        }

        // Phase `plain_text_mail_does_not_use_the_webview`.
        {
            let sh = shared();
            // The Apollo message is plain text/plain (no HTML part).
            let id = {
                let st = sh.borrow();
                st.mailbox
                    .messages
                    .values()
                    .find(|m| m.from.email == "margaret@apollo.example")
                    .expect("plain-text sample present")
                    .id
            };
            let mut p = reader_for(&sh, id, 900.0, 800.0);
            draw(&mut p);
            assert!(
                p.html_loaded_key().is_none(),
                "a plain-text mail never loads the webview"
            );
            assert!(
                p.html_toggle_rect.is_none(),
                "no remote-images toggle for plain text"
            );
        }

        // Phase `loading_remote_images_reloads_the_body_unblocked`.
        {
            use ui::webview::test_engine::TestEngine;
            let sh = shared();
            let id = {
                let st = sh.borrow();
                st.mailbox
                    .messages
                    .values()
                    .find(|m| m.from.email == "news@weekly.example.test")
                    .unwrap()
                    .id
            };
            {
                let mut doc = AutoCommit::new();
                sh.borrow_mut().select_message(&mut doc, id);
                sh.borrow_mut().take_intents();
            }
            let mut p = Pane::new(Role::Reader, sh.clone());
            p.set_html_engine(TestEngine::new());
            p.rect = Rect::new(0.0, 0.0, 960.0, 820.0);
            p.rebuild_hits();
            draw(&mut p);
            assert_eq!(p.html_loaded_key(), Some((id, true)), "blocked by default");

            // Click the remote-images toggle → opt in, reload unblocked.
            let toggle = p.html_toggle_rect.expect("toggle shown");
            sh.borrow_mut().last_mouse = toggle.center();
            let mut doc = AutoCommit::new();
            let mut handled = false;
            p.on_left_press(&mut doc, toggle.center(), &mut handled);
            assert!(handled, "toggle click consumed");
            draw(&mut p);
            assert_eq!(
                p.html_loaded_key(),
                Some((id, false)),
                "opting in reloads the body with remote content allowed"
            );
        }

        // Phase `reader_star_action_reflects_the_starred_state`.
        {
            // Toggling the reader Star flips the flag that selects the filled vs
            // outline glyph (the draw path picks the fill purely from this state).
            let sh = shared();
            let id = {
                let st = sh.borrow();
                st.mailbox.messages_in(Folder::Inbox)[0].id
            };
            // Force a known starting state.
            {
                sh.borrow_mut().mailbox.get_mut(id).unwrap().starred = false;
            }
            let mut p = reader_for(&sh, id, 900.0, 800.0);
            let star = p
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::Star))
                .map(|(r, _)| *r)
                .unwrap();
            sh.borrow_mut().last_mouse = star.center();
            let mut doc = AutoCommit::new();
            let mut handled = false;
            p.on_left_press(&mut doc, star.center(), &mut handled);
            assert!(
                sh.borrow().mailbox.get(id).unwrap().starred,
                "starring sets the flag the filled glyph keys on"
            );
        }

        // Phase `reader_attachment_chip_click_requests_the_root_portal_save`.
        {
            let sh = shared();
            // The Radia message is parsed from real MIME and carries a hydrated PDF.
            let id = {
                let st = sh.borrow();
                st.mailbox
                    .messages
                    .values()
                    .find(|m| m.from.email == "radia@spanning.example")
                    .unwrap()
                    .id
            };
            let mut p = reader_for(&sh, id, 760.0, 900.0);
            // The chip layout is only known after a draw.
            draw(&mut p);
            assert!(
                !p.reader_attachment_hits.is_empty(),
                "attachment chip has a hit region"
            );
            let (rect, mid, ai) = p.reader_attachment_hits[0];
            assert_eq!(mid, id);

            sh.borrow_mut().last_mouse = rect.center();
            let mut doc = AutoCommit::new();
            let mut handled = false;
            p.on_left_press(&mut doc, rect.center(), &mut handled);
            assert!(handled, "chip click consumed");

            assert!(
                sh.borrow_mut()
                    .take_intents()
                    .iter()
                    .any(|intent| matches!(intent, Intent::SaveAttachment(found, found_index) if *found == id && *found_index == ai)),
                "a pane hands the stable attachment address to MailApp; only the root opens the host portal and writes bytes"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn list_and_card_geometry_hold_their_authored_sizes() {
        // Phase `list_header_and_avatar_keep_their_one_x_sizes_before_the_first_paint`.
        {
            // The pre-paint rules are the authored 1× header band, control
            // height and avatar radius, so nothing moves at 1×.
            let m = ListRowMetrics::fallback();
            assert_eq!(m.header_h, HEADER_H);
            assert_eq!(m.ctl_h, HEADER_CTL_H);
            assert_eq!(m.avatar_r, LIST_AVATAR_R);
            // A scaled control pushes the rows down instead of under the search
            // box: the header is the controls plus their inset above and below.
            let mut big = m;
            big.ctl_h = 49.0;
            big.header_h = (2.0 * HEADER_CTL_INSET + big.ctl_h).max(HEADER_H);
            assert_eq!(big.header_h, 73.0);
            assert!(big.header_h > HEADER_H);
        }

        // Phase `list_row_bands_stack_without_overlap_and_the_star_hit_covers_its_glyph`.
        {
            let m = ListRowMetrics::fallback();
            // 1× keeps the authored row height (rounded up to a whole pixel).
            assert_eq!(m.row_h, 79.0);
            // Sender, subject and snippet are separate bands, top to bottom.
            assert!(m.subj_y >= m.name_y + m.name_h);
            assert!(m.snippet_y >= m.subj_y + m.subj_h);
            assert!(m.snippet_y + m.snippet_h <= m.row_h);
            // The star target contains the painted 16px glyph on the snippet line.
            let row = Rect::new(0.0, 100.0, 340.0, 100.0 + m.row_h);
            let hit = m.star_hit(row);
            let right = row.x1 - 14.0;
            let glyph = Rect::new(
                right - 24.0,
                row.y0 + m.icon_cy - 8.0,
                right - 8.0,
                row.y0 + m.icon_cy + 8.0,
            );
            assert_eq!(hit.union(glyph), hit, "star hit rect contains the glyph");
            assert!(
                hit.y0 > row.y0 + m.name_y,
                "the target is not over the date line"
            );

            // Doubled line boxes (200% text) grow the row instead of overlapping.
            let big = ListRowMetrics::from_line_boxes(33.75, 31.25, 28.75, 4.0, 12.0);
            assert!(big.row_h > m.row_h);
            assert!(big.subj_y >= big.name_y + big.name_h);
            assert!(big.snippet_y >= big.subj_y + big.subj_h);
            assert!(big.snippet_y + big.snippet_h + 12.0 <= big.row_h);
        }

        // Phase `card_header_right_column_shares_one_edge_and_centres_on_the_avatar`.
        {
            let m = CardHeaderMetrics {
                x: 100.0,
                y: 200.0,
                width: 500.0,
                avatar_r: 17.0,
                name_h: 16.875,
                email_h: 14.375,
                date_w: 110.0,
                to_w: 40.0,
                gap: 12.0,
            };
            let l = card_header_layout(&m);
            // Date and recipients end on the same right edge — the card's.
            assert!((l.date_x + m.date_w - 600.0).abs() < 1e-6);
            assert!((l.to_x + m.to_w - 600.0).abs() < 1e-6);
            // Name + email block is centred on the avatar's centre line.
            let block_mid = (l.name_y + l.email_y + l.email_h) / 2.0;
            assert!((block_mid - l.avatar_c.y).abs() < 1e-6);
            // The sender column stops one gap before the wider right-column line.
            assert!((l.text_x + l.from_budget + m.gap - l.date_x).abs() < 1e-6);
            // 1× keeps the authored 48px header rhythm.
            assert!((l.bottom - (m.y + 48.0)).abs() < 1e-6);
            // Larger text grows the header instead of overlapping the body.
            let tall = CardHeaderMetrics {
                name_h: 33.75,
                email_h: 28.75,
                ..m
            };
            let lt = card_header_layout(&tall);
            assert!(lt.bottom >= lt.email_y + tall.email_h + tall.gap);
        }

        // Phase `list_title_is_centered_on_and_spaced_from_the_collapse_chip`.
        {
            // The folder title must line up vertically with the NavShell collapse
            // chip beside it and sit a consistent gap past it — not a few px off.
            let rect = Rect::new(0.0, 0.0, 420.0, 900.0);
            let gap = 12.0;
            // The 1× line box of the 18 px bold title; the layout centres whatever
            // box it is handed, so a 200% box (45) lands on the same centre.
            let title_h = 22.5;
            let (title_x, title_y, header_cy) =
                list_header_layout(rect, mirrored_chip(rect), true, gap, title_h);
            let chip_cy = rect.y0 + CHIP_INSET + CHIP_SIZE / 2.0;
            let chip_right = rect.x0 + CHIP_INSET + CHIP_SIZE;
            // Vertically centred on the chip's centre line from the measured box.
            let title_center = title_y + title_h / 2.0;
            let (_, big_y, _) = list_header_layout(rect, mirrored_chip(rect), true, gap, 45.0);
            assert!(
                (big_y + 22.5 - chip_cy).abs() < 0.001,
                "a scaled title box still centres on the chip"
            );
            assert!(
                (title_center - chip_cy).abs() < 0.001,
                "title centre {title_center} != chip centre {chip_cy}"
            );
            assert!(
                (header_cy - chip_cy).abs() < 0.001,
                "header centre line is the chip centre"
            );
            // Spaced exactly one gap past the chip, and not crowding it.
            assert!(
                (title_x - (chip_right + gap)).abs() < 0.001,
                "title starts one gap past the chip"
            );
            assert!(
                title_x - chip_right >= 10.0,
                "title does not crowd the collapse button"
            );
            // With no chip over the header, the title returns to the plain inset.
            let (nx, _, _) = list_header_layout(rect, mirrored_chip(rect), false, gap, title_h);
            // The shell's real chip at 2x is larger than the 1x mirror; the title
            // follows it, centred on its centre line and spaced past its edge.
            let big_chip = Rect::new(12.0, 12.0, 12.0 + 53.5, 12.0 + 53.5);
            let (bx, by, bcy) = list_header_layout(rect, big_chip, true, gap, 45.0);
            assert!(
                (bcy - big_chip.center().y).abs() < 1e-9,
                "centre line follows the real chip"
            );
            assert!(
                (bx - (big_chip.x1 + gap)).abs() < 1e-9,
                "title clears the real chip"
            );
            assert!((by + 22.5 - bcy).abs() < 1e-9);
            assert_eq!(nx, rect.x0 + 20.0);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn list_and_sidebar_pointer_paths_route_to_their_own_targets() {
        // Phase `list_swipe_left_archives_the_row`.
        {
            let sh = shared();
            let mut p = Pane::new(Role::List, sh.clone());
            p.rect = Rect::new(0.0, 0.0, 420.0, 900.0);
            p.rebuild_hits();
            let (id, row) = p
                .hits
                .iter()
                .rev()
                .find_map(|(r, h)| {
                    if let Hit::Row(i) = h {
                        Some((*i, *r))
                    } else {
                        None
                    }
                })
                .expect("a list row");
            let before = sh.borrow().mailbox.count(Folder::Archive);
            let start = row.center();

            sh.borrow_mut().last_mouse = start;
            dispatch(&mut p, &press());
            // Drag left past the threshold.
            let far = Point::new(start.x - (SWIPE_THRESHOLD + 24.0), start.y);
            sh.borrow_mut().last_mouse = far;
            dispatch(&mut p, &moved(far));
            dispatch(&mut p, &release());

            assert_eq!(
                sh.borrow().mailbox.count(Folder::Archive),
                before + 1,
                "swipe-left archived the row"
            );
            assert_eq!(sh.borrow().mailbox.get(id).unwrap().folder, Folder::Archive);
        }

        // Phase `search_typing_filters_then_clear_resets`.
        {
            let sh = shared();
            let mut p = Pane::new(Role::List, sh.clone());
            p.rect = Rect::new(0.0, 0.0, 420.0, 900.0);
            p.rebuild_hits();
            let full_rows = sh.borrow().list_rows().len();

            // Focus the search field via its real hit.
            let sr = p
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::SearchFocus))
                .map(|(r, _)| *r)
                .unwrap();
            sh.borrow_mut().last_mouse = sr.center();
            let mut doc = AutoCommit::new();
            let mut handled = false;
            p.on_left_press(&mut doc, sr.center(), &mut handled);
            assert!(
                sh.borrow().search_focused,
                "clicking the field focuses search"
            );

            // Put a query in the field and drive the pane's reflect path.
            if let Some(i) = p.search_input.as_mut() {
                i.set_text("trajectory");
            }
            dispatch(&mut p, &named(NamedKey::ArrowRight));
            assert_eq!(
                sh.borrow().search,
                "trajectory",
                "field value reflected into the query"
            );
            let filtered = sh.borrow().list_rows();
            assert!(
                !filtered.is_empty() && filtered.len() < full_rows,
                "results narrowed to matches"
            );
            assert!(filtered.iter().all(|(mid, _)| {
                let st = sh.borrow();
                let m = st.mailbox.get(*mid).unwrap();
                m.subject.to_ascii_lowercase().contains("trajectory")
                    || m.body.to_ascii_lowercase().contains("trajectory")
            }));

            // Clear (X) resets the query and rows.
            p.rebuild_hits();
            let clr = p
                .hits
                .iter()
                .find(|(_, h)| matches!(h, Hit::SearchClear))
                .map(|(r, _)| *r)
                .expect("clear button appears");
            let mut handled = false;
            p.on_left_press(&mut doc, clr.center(), &mut handled);
            assert!(sh.borrow().search.is_empty(), "clear reset the query");
            assert_eq!(
                sh.borrow().list_rows().len(),
                full_rows,
                "rows restored after clear"
            );
        }

        // Phase `drafts_row_click_opens_editor_not_reader`.
        {
            let sh = shared();
            // Put a message into the Drafts folder and switch to it.
            let id = {
                let mut st = sh.borrow_mut();
                let some = *st.mailbox.messages.keys().next().unwrap();
                st.mailbox.move_to(some, Folder::Drafts);
                st.folder = Folder::Drafts;
                some
            };
            let mut p = Pane::new(Role::List, sh.clone());
            p.rect = Rect::new(0.0, 0.0, 420.0, 900.0);
            p.rebuild_hits();
            let row = p
                .hits
                .iter()
                .rev()
                .find(|(_, h)| matches!(h, Hit::Row(_)))
                .map(|(r, _)| *r)
                .unwrap();
            sh.borrow_mut().last_mouse = row.center();
            let mut doc = AutoCommit::new();
            let mut handled = false;
            p.on_left_press(&mut doc, row.center(), &mut handled);
            let intents = sh.borrow_mut().take_intents();
            assert!(
                intents
                    .iter()
                    .any(|i| matches!(i, Intent::EditDraft(mid) if *mid == id)),
                "a Drafts row raises EditDraft"
            );
            assert!(
                !intents.iter().any(|i| matches!(i, Intent::ShowDetail)),
                "it does not open the reader"
            );
        }

        // Phase `account_row_gear_opens_that_accounts_settings_not_the_active`.
        {
            let (mut p, sh) = sidebar();
            assert_eq!(
                sh.borrow().accounts.active,
                0,
                "account #1 is not the active one"
            );
            // The gear on account #1 (you@hooli.example) must scope settings to #1.
            let gear = p
                .hits
                .iter()
                .find_map(|(r, h)| matches!(h, Hit::AccountSettings(1)).then_some(*r))
                .expect("account #1 has a per-account settings gear");
            let intents = click_sidebar(&mut p, &sh, gear.center());
            assert!(
                intents
                    .iter()
                    .any(|i| matches!(i, Intent::Settings(Some(1)))),
                "the gear opens settings scoped to account #1, got {intents:?}"
            );
            assert!(
                !intents
                    .iter()
                    .any(|i| matches!(i, Intent::Settings(Some(0) | None))),
                "it must not fall back to the active account or the add-account flow"
            );
        }

        // Phase `sidebar_offers_a_separate_add_account_action`.
        {
            let (mut p, sh) = sidebar();
            let add = p
                .hits
                .iter()
                .find_map(|(r, h)| matches!(h, Hit::AddAccount).then_some(*r))
                .expect("a distinct Add-account action");
            let intents = click_sidebar(&mut p, &sh, add.center());
            assert!(
                intents.iter().any(|i| matches!(i, Intent::Settings(None))),
                "Add account opens the blank add-account sheet, got {intents:?}"
            );
        }

        // Phase `sidebar_header_has_no_global_settings_gear`.
        {
            let (p, _sh) = sidebar();
            // The old global-looking gear sat at the top-right of the header. Account
            // settings are per-account now, so there must be no hit there.
            assert!(
                p.hit_at(Point::new(p.rect.x1 - 24.0, p.rect.y0 + 28.0))
                    .is_none(),
                "no global settings control in the sidebar header"
            );
        }
    }

    fn sidebar() -> (Pane, Shared) {
        let sh = shared(); // sample store: 2 accounts, active = 0
        let mut p = Pane::new(Role::Sidebar, sh.clone());
        p.rect = Rect::new(0.0, 0.0, 260.0, 800.0);
        p.rebuild_hits();
        (p, sh)
    }

    fn click_sidebar(p: &mut Pane, sh: &Shared, pt: Point) -> Vec<Intent> {
        sh.borrow_mut().last_mouse = pt;
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, pt, &mut handled);
        assert!(handled, "sidebar click at {pt:?} consumed");
        sh.borrow_mut().take_intents()
    }
}
