//! The three content/menu panes of the mail client, expressed as one widget
//! parameterised by [`Role`] so the [`ui::widget::NavShell`] can own three
//! distinct child `Element`s that all share one [`MailState`].
//!
//! * [`Role::Sidebar`] — the genuine menu (accounts + folders + compose).
//! * [`Role::List`] — the message overview (search + threaded rows).
//! * [`Role::Reader`] — the message detail (headers + body + actions).
//!
//! Each pane draws into the rect the shell assigns it, builds its own hit list
//! relative to that rect, mutates the shared state on interaction, and raises
//! [`Intent`]s for anything that needs a sibling pane or an overlay.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::Widget;
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::webview::{SharedEngine, WebViewCommand};
use ui::widget::{EmptyState, TextInput, WebView};

use crate::account::AccountStore;
use crate::draw::{
    ACCOUNT_HEADER_H, BACK_CLEARANCE, CHIP_CLEARANCE, CHIP_INSET, CHIP_SIZE, FOLDER_ROW_H,
    HEADER_H, LIST_ROW_H, draw_avatar, elide, fill_rect, fill_round, stroke_round,
};
use crate::model::{Addr, Folder, Message, MessageId};
use crate::state::{CtxAction, Intent, MailState, Shared};
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
fn list_header_layout(rect: Rect, chip_here: bool, gap: f64) -> (f64, f64, f64) {
    // The chip is a CHIP_SIZE square inset CHIP_INSET from the pane's top-left
    // (mirrored from `ui`'s NavShell), so its centre line is:
    let header_cy = rect.y0 + CHIP_INSET + CHIP_SIZE / 2.0;
    let title_x = if chip_here {
        rect.x0 + CHIP_INSET + CHIP_SIZE + gap
    } else {
        rect.x0 + 20.0
    };
    // Text is positioned by the top of its line box (`size * 1.25`), so drop the
    // title top by half its own box to centre the glyphs on `header_cy`.
    let title_y = header_cy - 18.0 * 1.25 / 2.0;
    (title_x, title_y, header_cy)
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

/// Everything [`Pane::draw_list_row`] needs about one row that
/// [`Pane::draw_list`] has already resolved: the message, its slot rect, its
/// thread size, and its selection/hover/swipe state.
#[derive(Clone, Copy)]
struct ListRowArgs<'a> {
    msg: &'a Message,
    rect: Rect,
    len: usize,
    selected: bool,
    hov: bool,
    swipe_dx: f64,
    now_min: i64,
}

pub struct Pane {
    role: Role,
    shared: Shared,
    rect: Rect,
    hits: Vec<(Rect, Hit)>,
    /// List pane only: the search field.
    search_input: Option<TextInput>,
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
            let mut i = TextInput::local();
            i.set_plain_chrome(true);
            Some(i)
        } else {
            None
        };
        Self {
            role,
            shared,
            rect: Rect::ZERO,
            hits: Vec::new(),
            search_input,
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

    fn header_left_inset(&self) -> f64 {
        let st = self.shared.borrow();
        match self.role {
            Role::Sidebar => CHIP_CLEARANCE,
            // The list is the leftmost content pane (so the shell's collapse /
            // hamburger toggle sits over its header) whenever the layout is
            // narrow OR the docked sidebar is collapsed. Reserve room so the
            // folder title never renders under the toggle chip.
            Role::List => {
                if st.narrow || !st.sidebar_open {
                    CHIP_CLEARANCE
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
        Rect::new(
            r.x1 - 16.0 - 32.0,
            r.y0 + 12.0,
            r.x1 - 16.0,
            r.y0 + 12.0 + 32.0,
        )
    }

    fn search_box_rect(&self) -> Rect {
        let r = self.rect;
        // Leave room for the refresh button on the far right.
        let x1 = self.refresh_box_rect().x0 - 10.0;
        // Adapt the search width to the (often narrow) list column, always
        // leaving room for the folder title on its left.
        let sw = (r.width() * 0.46).clamp(140.0, 230.0);
        let x0 = (x1 - sw).max(r.x0 + self.header_left_inset() + 70.0);
        Rect::new(x0, r.y0 + 12.0, x1, r.y0 + 12.0 + 32.0)
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
        let mut y = sb.y0 + CHIP_INSET + CHIP_SIZE + 8.0;
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
        hits.push((self.refresh_box_rect(), Hit::Refresh));
        let sr = self.search_box_rect();
        *search_rect = Some(Rect::new(sr.x0 + 30.0, sr.y0, sr.x1 - 26.0, sr.y1));
        hits.push((sr, Hit::SearchFocus));
        if !st.search.is_empty() {
            hits.push((
                Rect::new(sr.x1 - 26.0, sr.y0, sr.x1, sr.y1),
                Hit::SearchClear,
            ));
        }
        let mut ly = r.y0 + HEADER_H;
        for (id, _len) in st.list_rows() {
            if ly >= r.y1 {
                break;
            }
            let row = Rect::new(r.x0, ly, r.x1, ly + LIST_ROW_H);
            let star = Rect::new(row.x1 - 36.0, row.y0 + 8.0, row.x1 - 12.0, row.y0 + 32.0);
            hits.push((star, Hit::RowStar(id)));
            hits.push((row, Hit::Row(id)));
            ly += LIST_ROW_H;
        }
    }

    /// The reader's action-bar hit regions (Reply/Archive/Delete/Star/Mark
    /// unread), sized compact (icon-only) or full width depending on
    /// available room. Part of [`Self::rebuild_hits`].
    fn rebuild_reader_hits(&self, st: &MailState, hits: &mut Vec<(Rect, Hit)>) {
        let r = self.rect;
        if st.selected.is_none() {
            return;
        }
        let inset = if st.narrow && st.detail_pushed {
            BACK_CLEARANCE
        } else {
            20.0
        };
        let mut bx = r.x0 + inset;
        let by = r.y0 + 12.0;
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

        // No app-name header: the OS owns window chrome and shows "Mail" in the
        // titlebar. The shell's collapse chip sits at the sidebar's top-left;
        // the compose action and account list flow up beneath it. Account
        // settings are reached per-account (the gear on each account row).

        // Compose button.
        if let Some((rect, _)) = self.hits.iter().find(|(_, h)| matches!(h, Hit::Compose)) {
            let hov = rect.contains(st.last_mouse);
            let col = if hov { c.accent[4] } else { c.accent[5] };
            fill_round(scene, t, *rect, 10.0, col);
            let label = "Compose";
            let (w, _) = tr.measure_weighted(label, 14.0, None, TextWeight::Semibold);
            // Icon + label centred as a unit.
            let group_w = 18.0 + 8.0 + f64::from(w);
            let gx = f64::midpoint(rect.x0, rect.x1) - group_w / 2.0;
            let icon_box = Rect::new(gx, rect.y0 + 11.0, gx + 18.0, rect.y0 + 29.0);
            Icon::Pencil.draw(scene, t, icon_box, Color::WHITE);
            tr.draw_text_weighted(
                scene,
                label,
                14.0,
                Color::WHITE,
                t * Affine::translate((icon_box.x1 + 8.0, rect.y0 + 11.0)),
                None,
                TextWeight::Semibold,
            );
        }

        for (rect, hit) in &self.hits {
            match hit {
                Hit::Account(ai) => {
                    Self::draw_sidebar_account_row(scene, t, tr, theme, &st, *rect, *ai, sb);
                }
                Hit::AddAccount => {
                    self.draw_sidebar_add_account_row(scene, t, tr, theme, *rect);
                }
                Hit::Folder(ai, f) => {
                    Self::draw_sidebar_folder_row(scene, t, tr, theme, &st, *rect, *ai, *f);
                }
                _ => {}
            }
        }
    }

    /// One account header row: expand chevron, avatar, name/email, and the
    /// per-account settings gear. A no-op if `ai` is out of range. Part of
    /// [`Self::draw_sidebar`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the row layout draw_sidebar already computed; bundling them would just move the same count into a field list"
    )]
    fn draw_sidebar_account_row(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        st: &MailState,
        rect: Rect,
        ai: usize,
        sb: Rect,
    ) {
        let c = &theme.tokens.colors;
        let Some(account) = st.accounts.accounts.get(ai) else {
            return;
        };
        let active = ai == st.accounts.active;
        let hov = rect.contains(st.last_mouse);
        if active {
            fill_round(scene, t, rect, 9.0, c.accent[5].with_alpha_factor(0.12));
        } else if hov {
            fill_round(scene, t, rect, 9.0, c.lift(0.35));
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
            scene,
            tr,
            t,
            theme,
            &st.images,
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
            fill_round(scene, t, gear, 8.0, c.lift(0.72));
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
            fill_round(scene, t, rect, 9.0, c.lift(0.35));
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
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the row layout draw_sidebar already computed; bundling them would just move the same count into a field list"
    )]
    fn draw_sidebar_folder_row(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        st: &MailState,
        rect: Rect,
        ai: usize,
        f: Folder,
    ) {
        let c = &theme.tokens.colors;
        let active_account = ai == st.accounts.active;
        let selected = active_account && f == st.folder;
        let hov = rect.contains(st.last_mouse);
        if selected {
            fill_round(scene, t, rect, 8.0, c.accent[5].with_alpha_factor(0.20));
            fill_rect(
                scene,
                t,
                Rect::new(rect.x0, rect.y0 + 6.0, rect.x0 + 3.0, rect.y1 - 6.0),
                c.accent[5],
            );
        } else if hov {
            fill_round(scene, t, rect, 8.0, c.lift(0.35));
        }
        let ty = rect.y0 + (FOLDER_ROW_H - 14.0) / 2.0;
        let label_col = if selected {
            c.text_primary
        } else {
            c.neutral[7]
        };
        let icon_box = Rect::new(
            rect.x0 + 12.0,
            rect.y0 + FOLDER_ROW_H / 2.0 - 9.0,
            rect.x0 + 30.0,
            rect.y0 + FOLDER_ROW_H / 2.0 + 9.0,
        );
        let icon_col = if selected { c.accent[5] } else { c.neutral[6] };
        f.icon().draw(scene, t, icon_box, icon_col);
        tr.draw_text_weighted(
            scene,
            f.title(),
            13.0,
            label_col,
            t * Affine::translate((rect.x0 + 40.0, ty)),
            Some(120.0),
            if selected {
                TextWeight::Semibold
            } else {
                TextWeight::Regular
            },
        );
        if !active_account {
            return;
        }
        let unread = st.mailbox.unread_count(f);
        if unread == 0 {
            return;
        }
        let txt = unread.to_string();
        let (twd, _) = tr.measure_weighted(&txt, 11.0, None, TextWeight::Semibold);
        let bw = (f64::from(twd) + 14.0).max(20.0);
        let badge = Rect::new(
            rect.x1 - bw - 8.0,
            rect.y0 + 8.0,
            rect.x1 - 8.0,
            rect.y0 + 28.0,
        );
        let bg = if selected { c.accent[5] } else { c.neutral[3] };
        fill_round(scene, t, badge, 10.0, bg);
        tr.draw_text_weighted(
            scene,
            &txt,
            11.0,
            Color::WHITE,
            t * Affine::translate((
                f64::midpoint(badge.x0, badge.x1) - f64::from(twd) / 2.0,
                badge.y0 + 4.0,
            )),
            None,
            TextWeight::Semibold,
        );
    }

    /// The list pane's header: folder title (+ count) and the search box, plus
    /// the manual sync/refresh button beside it. Part of [`Self::draw_list`].
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
        let gap = f64::from(theme.tokens.spacing.s3);
        let (title_x, title_y, header_cy) = list_header_layout(r, chip_here, gap);
        let title_budget = sr.x0 - title_x - 12.0;
        let title_full = st.folder.title();
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
        let count = st.mailbox.count(st.folder);
        let count_x = title_x + f64::from(twd) + 10.0;
        if f64::from(twd) <= title_budget && count_x + 26.0 < sr.x0 {
            let count_y = header_cy - 12.0 * 1.25 / 2.0;
            tr.draw_text(
                scene,
                &format!("{count}"),
                12.0,
                c.text_dim,
                t * Affine::translate((count_x, count_y)),
                None,
            );
        }
        fill_round(scene, t, sr, 9.0, c.surfaces.raised);
        stroke_round(
            scene,
            t,
            sr,
            9.0,
            1.0,
            if st.search_focused {
                c.border_focus
            } else {
                c.border_standard
            },
        );
        Icon::Search.draw(
            scene,
            t,
            Rect::new(sr.x0 + 8.0, sr.y0 + 8.0, sr.x0 + 24.0, sr.y0 + 24.0),
            c.text_dim,
        );
        if st.search.is_empty() && !st.search_focused {
            tr.draw_text(
                scene,
                "Search",
                13.0,
                c.text_dim,
                t * Affine::translate((sr.x0 + 30.0, sr.y0 + 9.0)),
                None,
            );
        }
        if !st.search.is_empty() {
            Icon::X.draw(
                scene,
                t,
                Rect::new(sr.x1 - 24.0, sr.y0 + 8.0, sr.x1 - 8.0, sr.y0 + 24.0),
                c.text_dim,
            );
        }

        // Manual sync / refresh button.
        let rb = self.refresh_box_rect();
        let rb_hov = rb.contains(st.last_mouse);
        fill_round(
            scene,
            t,
            rb,
            9.0,
            if rb_hov {
                c.lift(0.72)
            } else {
                c.surfaces.raised
            },
        );
        stroke_round(scene, t, rb, 9.0, 1.0, c.border_standard);
        Icon::Refresh.draw(scene, t, rb.inset(-9.0), c.neutral[7]);
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
                scene,
                t,
                tr,
                theme,
                Rect::new(r.x0, r.y0 + HEADER_H, r.x1, r.y1),
                Some(icon),
                title,
                Some(message),
                None,
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
            let selected = st.selected == Some(id) && !st.narrow;
            let hov = rect.contains(st.last_mouse);
            let swipe_dx = self.swipe.filter(|s| s.id == id).map_or(0.0, |s| s.dx);
            self.draw_list_row(
                scene,
                t,
                tr,
                theme,
                &ListRowArgs {
                    msg,
                    rect: *rect,
                    len,
                    selected,
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
    fn draw_list_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        args: &ListRowArgs<'_>,
    ) {
        let ListRowArgs {
            msg,
            rect,
            len,
            selected,
            hov,
            swipe_dx,
            now_min,
        } = *args;
        let row = Self::draw_list_row_frame(scene, t, tr, theme, rect, swipe_dx, selected, hov);
        self.draw_list_row_content(scene, t, tr, theme, row, msg, len, now_min);
    }

    /// Draw the swipe-action overlay (if dragged past the threshold) and the
    /// row's selection/hover background + bottom divider, returning the
    /// (possibly swipe-shifted) row rect the rest of the row draws into.
    /// Part of [`Self::draw_list_row`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the row state draw_list_row already resolved; bundling them would just move the same count into a field list"
    )]
    fn draw_list_row_frame(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        swipe_dx: f64,
        selected: bool,
        hov: bool,
    ) -> Rect {
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
            icon.draw(
                scene,
                t,
                Rect::new(gx, rect.y0 + 20.0, gx + 18.0, rect.y0 + 38.0),
                Color::WHITE,
            );
            tr.draw_text_weighted(
                scene,
                label,
                12.0,
                Color::WHITE,
                t * Affine::translate((gx + 26.0, rect.y0 + 22.0)),
                None,
                TextWeight::Semibold,
            );
        }
        let row = Rect::new(rect.x0 + swipe_dx, rect.y0, rect.x1 + swipe_dx, rect.y1);

        if selected {
            fill_rect(scene, t, row, c.accent[5].with_alpha_factor(0.16));
            fill_rect(
                scene,
                t,
                Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1),
                c.accent[5],
            );
        } else if hov && swipe_dx.abs() < 1.0 {
            fill_rect(scene, t, row, c.lift(0.35));
        }
        fill_rect(
            scene,
            t,
            Rect::new(row.x0 + 12.0, row.y1 - 1.0, row.x1, row.y1),
            c.neutral[2],
        );
        row
    }

    /// Draw one row's content within its (already framed) `row` rect: unread
    /// dot, avatar, name/date, subject (+ thread-size badge), snippet, and
    /// star/attachment glyphs. Part of [`Self::draw_list_row`].
    #[allow(clippy::too_many_arguments)]
    fn draw_list_row_content(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        msg: &Message,
        len: usize,
        now_min: i64,
    ) {
        let unread = !msg.read;
        self.draw_list_row_identity(scene, t, tr, theme, row, msg, unread, now_min);
        Self::draw_list_row_subject(scene, t, tr, theme, row, msg, len, unread);
    }

    /// The unread dot, avatar, sender name, and relative date of a list row.
    /// Part of [`Self::draw_list_row_content`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the row state draw_list_row already resolved; bundling them would just move the same count into a field list"
    )]
    fn draw_list_row_identity(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        msg: &Message,
        unread: bool,
        now_min: i64,
    ) {
        let c = &theme.tokens.colors;
        if unread {
            scene.fill(
                Fill::NonZero,
                t,
                c.accent[5],
                None,
                &Circle::new(Point::new(row.x0 + 14.0, row.y0 + LIST_ROW_H / 2.0), 4.0),
            );
        }
        draw_avatar(
            scene,
            tr,
            t,
            theme,
            &self.shared.borrow().images,
            Point::new(row.x0 + 42.0, row.y0 + 26.0),
            17.0,
            &msg.from,
            0,
        );

        let text_x = row.x0 + 70.0;
        let right = row.x1 - 14.0;
        let name_weight = if unread {
            TextWeight::Bold
        } else {
            TextWeight::Medium
        };
        let name = elide(
            tr,
            &msg.from.display(),
            13.5,
            name_weight,
            right - text_x - 56.0,
        );
        tr.draw_text_weighted(
            scene,
            &name,
            13.5,
            c.text_primary,
            t * Affine::translate((text_x, row.y0 + 12.0)),
            None,
            name_weight,
        );
        let date = format_relative(msg.timestamp_min, now_min);
        let (dw, _) = tr.measure(&date, 11.5, None);
        tr.draw_text(
            scene,
            &date,
            11.5,
            c.text_dim,
            t * Affine::translate((right - f64::from(dw), row.y0 + 13.0)),
            None,
        );
    }

    /// The subject line (+ thread-size badge), snippet, and star/attachment
    /// glyphs of a list row. Part of [`Self::draw_list_row_content`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the row state draw_list_row already resolved; bundling them would just move the same count into a field list"
    )]
    fn draw_list_row_subject(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        row: Rect,
        msg: &Message,
        len: usize,
        unread: bool,
    ) {
        let c = &theme.tokens.colors;
        let text_x = row.x0 + 70.0;
        let right = row.x1 - 14.0;

        let subj_y = row.y0 + 32.0;
        let subj_col = if unread { c.text_primary } else { c.neutral[7] };
        let subj_weight = if unread {
            TextWeight::Semibold
        } else {
            TextWeight::Regular
        };
        let subj_budget = right - text_x - if len > 1 { 48.0 } else { 16.0 };
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
            let badge_txt = len.to_string();
            let (bw, _) = tr.measure_weighted(&badge_txt, 10.5, None, TextWeight::Semibold);
            let bx = right - f64::from(bw) - 12.0;
            let badge = Rect::new(bx - 6.0, subj_y - 1.0, right, subj_y + 15.0);
            fill_round(scene, t, badge, 8.0, c.neutral[3]);
            tr.draw_text_weighted(
                scene,
                &badge_txt,
                10.5,
                c.neutral[8],
                t * Affine::translate((bx, subj_y + 1.0)),
                None,
                TextWeight::Semibold,
            );
        }

        let icon_reserve = if msg.has_attachments() { 64.0 } else { 28.0 };
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
            t * Affine::translate((text_x, row.y0 + 52.0)),
            None,
        );

        // A favourited mail shows a solid, filled star; an unstarred one a
        // faint outline — the state reads at a glance, not just as a colour.
        let star_box = Rect::new(right - 24.0, row.y0 + 48.0, right - 8.0, row.y0 + 64.0);
        if msg.starred {
            Icon::Star.draw_filled(scene, t, star_box, c.semantic.warning);
        } else {
            Icon::Star.draw(scene, t, star_box, c.neutral[4]);
        }
        if msg.has_attachments() {
            Icon::Paperclip.draw(
                scene,
                t,
                Rect::new(right - 44.0, row.y0 + 48.0, right - 30.0, row.y0 + 62.0),
                c.text_dim,
            );
        }
    }

    /// The reader's action bar: Reply/Archive/Delete/Star/Mark-unread, icon
    /// only when `compact`, icon + label otherwise. Part of [`Self::draw_reader`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the reader state draw_reader already resolved; bundling them would just move the same count into a field list"
    )]
    fn draw_reader_action_bar(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        compact: bool,
        starred: bool,
        last_mouse: Point,
    ) {
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
            let bg = if hov { c.lift(0.72) } else { c.lift(0.35) };
            fill_round(scene, t, *rect, 8.0, bg);
            stroke_round(
                scene,
                t,
                *rect,
                8.0,
                1.0,
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            );
            // The Star action fills its glyph when the message is starred, so
            // the favourited state is a solid star, not merely a tinted outline.
            let is_star_on = matches!(hit, Hit::Star) && starred;
            let col = if danger {
                c.semantic.danger
            } else if is_star_on {
                c.semantic.warning
            } else {
                c.neutral[8]
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
            let cx = f64::midpoint(rr.x0, rr.x1);
            let cy = rr.y0 + rr.height() / 2.0;
            Icon::Mail.draw(
                scene,
                t,
                Rect::new(cx - 26.0, cy - 66.0, cx + 26.0, cy - 14.0),
                c.neutral[4],
            );
            let hint = "Select a conversation to read";
            let (w, _) = tr.measure(hint, 14.0, None);
            tr.draw_text(
                scene,
                hint,
                14.0,
                c.text_dim,
                t * Affine::translate((cx - f64::from(w) / 2.0, cy)),
                None,
            );
            return None;
        }

        // Action bar (header strip).
        let compact = st.narrow || rr.width() < READER_COMPACT_W;
        let starred = st
            .selected
            .and_then(|id| st.mailbox.get(id))
            .is_some_and(|m| m.starred);
        self.draw_reader_action_bar(scene, t, tr, theme, compact, starred, st.last_mouse);

        let Some(head) = msgs.last() else {
            // Unreachable: `msgs.is_empty()` returned above — but bail rather
            // than panic if that invariant is ever violated.
            return None;
        };
        let content_x = rr.x0 + 24.0;
        let content_w = (rr.width() - 48.0) as f32;
        let mut y = Self::draw_reader_thread_heading(
            scene,
            t,
            tr,
            theme,
            content_x,
            content_w,
            head,
            msgs.len(),
            st.folder,
            rr.y0 + HEADER_H + 8.0,
        );
        y = Self::draw_inspection_badge(
            scene,
            t,
            tr,
            theme,
            content_x,
            y,
            head.read,
            st.inspection == Some(head.id),
        );
        fill_rect(
            scene,
            t,
            Rect::new(content_x, y, rr.x1 - 24.0, y + 1.0),
            c.border_standard,
        );
        y += 16.0;

        // Render the thread. When the head (newest, being-read) message carries
        // an HTML body, its body renders in the sandboxed WebView; older
        // messages and plain-text mail render as text cards.
        let head_id = head.id;
        let head_html = head.html_body.clone();
        let webview_rect = self.draw_reader_thread_messages(
            scene,
            t,
            tr,
            theme,
            rr,
            &msgs,
            content_x,
            content_w,
            y,
            head_html.is_some(),
            &mut chip_hits,
        );
        self.reader_attachment_hits = chip_hits;
        drop(st);
        Some((head_id, head_html, webview_rect))
    }

    /// Draw the conversation title and its compact count/folder context,
    /// returning the baseline after that header.
    #[allow(clippy::too_many_arguments)]
    fn draw_reader_thread_heading(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content_x: f64,
        content_w: f32,
        head: &Message,
        message_count: usize,
        folder: Folder,
        y: f64,
    ) -> f64 {
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
    #[allow(clippy::too_many_arguments)]
    fn draw_inspection_badge(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        content_x: f64,
        y: f64,
        already_read: bool,
        inspecting: bool,
    ) -> f64 {
        if !inspecting {
            return y;
        }
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
        fill_round(scene, t, pill, 7.0, c.accent[5].with_alpha_factor(0.14));
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
    #[allow(
        clippy::too_many_arguments,
        reason = "internal draw-step split of an over-length function"
    )]
    fn draw_reader_thread_messages(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rr: Rect,
        msgs: &[&Message],
        content_x: f64,
        content_w: f32,
        mut y: f64,
        head_has_html: bool,
        chip_hits: &mut Vec<(Rect, MessageId, usize)>,
    ) -> Option<Rect> {
        let mut webview_rect: Option<Rect> = None;
        for (i, m) in msgs.iter().enumerate() {
            if y > rr.y1 - 40.0 {
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
                let bottom = rr.y1 - 46.0; // room for the remote-images bar
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
                    scene, t, tr, theme, m, content_x, y, content_w, is_head, chip_hits,
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
        fill_round(
            scene,
            t,
            pill,
            8.0,
            if hov { c.lift(0.72) } else { c.lift(0.35) },
        );
        stroke_round(
            scene,
            t,
            pill,
            8.0,
            1.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
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
        fill_round(scene, t, body, 6.0, c.surfaces.base);
        stroke_round(scene, t, body, 6.0, 1.0, c.border_standard);
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
        let card_x1 = cx + f64::from(width);
        let st = self.shared.borrow();
        draw_avatar(
            scene,
            tr,
            t,
            theme,
            &st.images,
            Point::new(cx + 18.0, cy + 18.0),
            17.0,
            &msg.from,
            0,
        );
        let from_name = elide(
            tr,
            &msg.from.display(),
            13.5,
            TextWeight::Semibold,
            f64::from(width - 150.0),
        );
        tr.draw_text_weighted(
            scene,
            &from_name,
            13.5,
            c.text_primary,
            t * Affine::translate((cx + 44.0, cy + 6.0)),
            None,
            TextWeight::Semibold,
        );
        let from_email = elide(
            tr,
            &msg.from.email,
            11.5,
            TextWeight::Regular,
            f64::from(width - 150.0),
        );
        tr.draw_text(
            scene,
            &from_email,
            11.5,
            c.text_dim,
            t * Affine::translate((cx + 44.0, cy + 24.0)),
            None,
        );
        let date = format_absolute(msg.timestamp_min);
        let (dw, _) = tr.measure(&date, 11.5, None);
        tr.draw_text(
            scene,
            &date,
            11.5,
            c.text_dim,
            t * Affine::translate((card_x1 - f64::from(dw), cy + 8.0)),
            None,
        );
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
            150.0,
        );
        tr.draw_text(
            scene,
            &to_line,
            11.0,
            c.text_dim,
            t * Affine::translate((card_x1 - 150.0, cy + 24.0)),
            None,
        );
        cy + 48.0
    }

    #[allow(clippy::too_many_arguments)]
    /// Draw the "N attachments" label and the wrapping row of attachment
    /// chips for an expanded message card, recording each chip's hit region.
    /// Returns the y past the chips. Part of [`Self::draw_message_card`].
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the card layout draw_message_card already computed; bundling them would just move the same count into a field list"
    )]
    fn draw_attachment_chips(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        msg: &Message,
        cx: f64,
        card_x1: f64,
        mut yy: f64,
        chip_hits: &mut Vec<(Rect, MessageId, usize)>,
    ) -> f64 {
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
            fill_round(
                scene,
                t,
                chip,
                8.0,
                if hov { c.lift(0.72) } else { c.lift(0.35) },
            );
            stroke_round(
                scene,
                t,
                chip,
                8.0,
                1.0,
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
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
            ax += chip_w + 10.0;
        }
        yy + 30.0
    }

    /// Draw one message card in the reader thread (header + body or a
    /// collapsed preview line) and return the y past it.
    #[allow(
        clippy::too_many_arguments,
        reason = "each parameter is a distinct piece of the thread layout draw_reader already resolved; bundling them would just move the same count into a field list"
    )]
    fn draw_message_card(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        msg: &Message,
        cx: f64,
        cy: f64,
        width: f32,
        expanded: bool,
        chip_hits: &mut Vec<(Rect, MessageId, usize)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let card_x1 = cx + f64::from(width);
        let mut yy = self.draw_card_header(
            scene,
            t,
            tr,
            theme,
            &CardHeaderArgs {
                msg,
                x: cx,
                y: cy,
                width,
            },
        );
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
                yy = self
                    .draw_attachment_chips(scene, t, tr, theme, msg, cx, card_x1, yy, chip_hits);
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
                st.set_active_account(doc, ai);
                st.select_folder(doc, Folder::Inbox);
                st.push_intent(Intent::CloseDrawer);
            }
            Hit::Folder(ai, f) => {
                let mut st = self.shared.borrow_mut();
                st.set_active_account(doc, ai);
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
                        st.push_intent(Intent::Engine(crate::engine::EngineRequest::Fetch {
                            account,
                            folder,
                        }));
                    }
                    st.status = Some(format!("Syncing {}…", folder.title()));
                    st.dirty = true;
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
                    crate::doc::MailDoc::set_search(doc, "");
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
        if let Some(id) = self.row_id_at(pt) {
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

    /// Save a reader attachment's real bytes to the downloads directory, or
    /// report that it hasn't been downloaded (hydrated) yet.
    fn save_attachment_click(&self, mid: MessageId, ai: usize) {
        let mut st = self.shared.borrow_mut();
        let Some(att) = st
            .mailbox
            .get(mid)
            .and_then(|m| m.attachments.get(ai))
            .cloned()
        else {
            return;
        };
        if !att.is_downloadable() {
            st.status = Some(format!("{} isn't downloaded yet", att.filename));
            st.dirty = true;
            return;
        }
        st.status = Some(
            match crate::util::save_download(&att.filename, &att.content) {
                Ok(path) => format!("Saved {} to {}", att.filename, path.display()),
                Err(e) => format!("Couldn't save {}: {e}", att.filename),
            },
        );
        st.dirty = true;
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
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event's own parameter list, which is fixed by the shared framework contract (see the trait doc)"
    )]
    fn forward_html_view_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
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
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            );
        }
    }

    /// Route a keyboard event to the focused search field (list role only),
    /// reflecting the field's value into the query + first-row selection.
    /// Returns whether [`Widget::handle_event`] should return immediately
    /// after this (true whenever the event was a keyboard event on the list
    /// role, matching the field regardless of whether search was focused).
    /// Part of [`Widget::handle_event`].
    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors Widget::handle_event's own parameter list, which is fixed by the shared framework contract (see the trait doc)"
    )]
    fn handle_search_keyboard(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !(matches!(event, WindowEvent::KeyboardInput { .. }) && self.role == Role::List) {
            return false;
        }
        let focused = self.shared.borrow().search_focused;
        if focused {
            if let Some(inp) = self.search_input.as_mut() {
                inp.handle_event(
                    event,
                    scale_factor,
                    &modifiers,
                    state,
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
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
            crate::doc::MailDoc::set_search(state, &st.search);
            st.selected = st.list_rows().first().map(|(id, _)| *id);
            st.dirty = true;
            *handled = true;
        }
        true
    }
}

impl Widget for Pane {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(i) = self.search_input.as_mut() {
            i.sync(state, path);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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
        }
        self.rebuild_hits();
        match self.role {
            Role::Sidebar => self.draw_sidebar(scene, t, tr, theme),
            Role::List => {
                self.draw_list(scene, t, tr, theme);
                if let Some(inp) = self.search_input.as_mut() {
                    inp.draw(scene, t, tr, theme, presence);
                }
            }
            Role::Reader => self.draw_reader(scene, t, tr, theme, presence),
        }
    }

    fn tooltip(&self) -> Option<&str> {
        if self.role != Role::Reader {
            return None;
        }
        let point = self.shared.borrow().last_mouse;
        self.hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            .and_then(|(_, hit)| match hit {
                Hit::Reply => Some("Reply (R)"),
                Hit::Archive => Some("Archive (E)"),
                Hit::Delete => Some("Delete (Delete)"),
                Hit::Star => Some("Toggle star (S)"),
                Hit::Unread => Some("Mark unread (U)"),
                _ => None,
            })
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
        if *handled {
            return;
        }
        self.rebuild_hits();

        self.forward_html_view_event(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
        );
        if *handled {
            return;
        }

        if self.handle_search_keyboard(
            event,
            scale_factor,
            *modifiers,
            state,
            path,
            handled,
            focus_target,
            node_id,
            taffy,
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
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let pt = self.shared.borrow().last_mouse;
                self.on_left_press(state, pt, handled);
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
                            state,
                            s.id,
                            CtxAction::Archive,
                        );
                    } else if s.dx >= SWIPE_THRESHOLD {
                        self.shared.borrow_mut().apply_message_action(
                            state,
                            s.id,
                            CtxAction::Delete,
                        );
                    }
                    *handled = true;
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
    fn empty_message_states_explain_the_next_useful_step() {
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
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
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
    fn reader_action_bar_star_toggles_star() {
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

    #[test]
    fn list_title_is_centered_on_and_spaced_from_the_collapse_chip() {
        // The folder title must line up vertically with the NavShell collapse
        // chip beside it and sit a consistent gap past it — not a few px off.
        let rect = Rect::new(0.0, 0.0, 420.0, 900.0);
        let gap = 12.0;
        let (title_x, title_y, header_cy) = list_header_layout(rect, true, gap);
        let chip_cy = rect.y0 + CHIP_INSET + CHIP_SIZE / 2.0;
        let chip_right = rect.x0 + CHIP_INSET + CHIP_SIZE;
        // Vertically centred on the chip's centre line (glyph box is size*1.25).
        let title_center = title_y + 18.0 * 1.25 / 2.0;
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
        let (nx, _, _) = list_header_layout(rect, false, gap);
        assert_eq!(nx, rect.x0 + 20.0);
    }

    #[test]
    fn reader_shows_one_card_for_a_refetched_message() {
        // The confirmed defect: re-fetching a message stacked N header copies in
        // the reader. With stable ids the thread the reader iterates holds one.
        let sh = MailState::shared(MailState::new_live(
            crate::account::AccountStore::new(),
            crate::app::avatar_store_offline(),
        ));
        let raw = b"From: Ada <ada@x.example>\r\nTo: you@qos.example\r\n\
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

    #[test]
    fn html_mail_renders_in_sandboxed_webview_with_remote_blocked() {
        use ui::webview::WebViewCommand;
        use ui::webview::test_engine::TestEngine;
        let sh = shared();
        let id = {
            let st = sh.borrow();
            st.mailbox
                .messages
                .values()
                .find(|m| m.from.email == "news@weekly.qos.example")
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

    #[test]
    fn plain_text_mail_does_not_use_the_webview() {
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

    #[test]
    fn loading_remote_images_reloads_the_body_unblocked() {
        use ui::webview::test_engine::TestEngine;
        let sh = shared();
        let id = {
            let st = sh.borrow();
            st.mailbox
                .messages
                .values()
                .find(|m| m.from.email == "news@weekly.qos.example")
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

    #[test]
    fn reader_star_action_reflects_the_starred_state() {
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

    #[test]
    fn reader_attachment_chip_click_saves_real_bytes() {
        let dir = std::env::temp_dir().join(format!("mail-dl-{}", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("QOS_MAIL_DOWNLOADS", &dir) };

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
        let (rect, mid, _ai) = p.reader_attachment_hits[0];
        assert_eq!(mid, id);

        sh.borrow_mut().last_mouse = rect.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, rect.center(), &mut handled);
        assert!(handled, "chip click consumed");

        let saved = dir.join("spanning-tree.pdf");
        assert!(
            saved.exists(),
            "the real attachment bytes were written to disk"
        );
        assert!(!std::fs::read(&saved).unwrap().is_empty());
        assert!(
            sh.borrow().status.as_ref().unwrap().contains("Saved"),
            "toast confirms the save"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_swipe_left_archives_the_row() {
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

    #[test]
    fn search_typing_filters_then_clear_resets() {
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

    #[test]
    fn drafts_row_click_opens_editor_not_reader() {
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

    #[test]
    fn account_row_gear_opens_that_accounts_settings_not_the_active() {
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

    #[test]
    fn sidebar_offers_a_separate_add_account_action() {
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

    #[test]
    fn sidebar_header_has_no_global_settings_gear() {
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
