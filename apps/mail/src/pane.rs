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
use ui::widget::TextInput;

use crate::html_mail::Block as HtmlBlock;

use crate::account::AccountStore;
use crate::draw::{
    draw_avatar, elide, fill_rect, fill_round, stroke_round, ACCOUNT_HEADER_H, BACK_CLEARANCE,
    CHIP_CLEARANCE, CHIP_INSET, CHIP_SIZE, FOLDER_ROW_H, HEADER_H, LIST_ROW_H,
};
use crate::model::{Addr, Folder, Message, MessageId};
use crate::state::{CtxAction, Intent, Shared};
use crate::util::{format_absolute, format_relative};

/// Distance (px) a horizontal drag must pass to fire a swipe action.
const SWIPE_THRESHOLD: f64 = 72.0;
/// Reader width (px) below which the action bar switches to icon-only buttons.
const READER_COMPACT_W: f64 = 480.0;
/// Height (px) of the "Add account" row at the foot of the sidebar account list.
const ADD_ACCOUNT_ROW_H: f64 = 38.0;

/// Header title placement for the list pane: the x where the folder title
/// starts, the y (top of its text box) that vertically centres it on the
/// NavShell collapse chip beside it, and the shared centre line (== the chip's
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
    Rect::new(header.x1 - s - 6.0, cy - s / 2.0, header.x1 - 6.0, cy + s / 2.0)
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
    /// Reader pane only: the parsed, sanitized HTML body of the open message,
    /// cached by message id so it is parsed once rather than every frame. `None`
    /// for plain-text mail (which never touches the HTML path).
    html_blocks: Option<(MessageId, Vec<HtmlBlock>)>,
    /// The message whose HTML body is shown — used to detect a genuine message
    /// change so the remote-content opt-in resets per mail.
    html_shown_id: Option<MessageId>,
    /// Whether the user has opted in to remote images for the open message.
    /// Reset whenever a different message is shown, so remote content is blocked
    /// again by default for each mail.
    html_load_remote: bool,
    /// The rect the HTML body occupies this frame (for wheel scrolling), the
    /// vertical scroll offset, the last body content height (to clamp scroll),
    /// and the link hit-rects resolved during the last draw.
    html_body_rect: Option<Rect>,
    html_scroll: f64,
    html_content_h: f64,
    html_link_hits: Vec<(Rect, String)>,
    /// An opted-in HTML-body image is still fetching/decoding, so the frame pump
    /// must stay alive until it settles and paints.
    html_loading: bool,
    /// The remote-images toggle chip rect (for click handling).
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
            html_blocks: None,
            html_shown_id: None,
            html_load_remote: false,
            html_body_rect: None,
            html_scroll: 0.0,
            html_content_h: 0.0,
            html_link_hits: Vec::new(),
            html_loading: false,
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
        Rect::new(r.x1 - 16.0 - 32.0, r.y0 + 12.0, r.x1 - 16.0, r.y0 + 12.0 + 32.0)
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
        let r = self.rect;
        let role = self.role;
        let mut search_rect: Option<Rect> = None;
        {
            let st = self.shared.borrow();
            match role {
                Role::Sidebar => {
                    let sb = r;
                    // Compose button under the shell's collapse chip (no
                    // app-name header band to clear anymore), so it aligns with
                    // the list column's first row.
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
                        // Per-account settings gear on the right of the row. Pushed
                        // after the row so it wins the hit test in its sub-rect;
                        // it edits THIS account, not the active one.
                        let gear = account_gear_rect(header);
                        hits.push((gear, Hit::AccountSettings(ai)));
                        y += ACCOUNT_HEADER_H;
                        if !account.collapsed {
                            // A gap after the account card so its highlight
                            // never abuts the first folder row's highlight.
                            y += 6.0;
                            for f in AccountStore::folders_for(account) {
                                let row = Rect::new(sb.x0 + 14.0, y, sb.x1 - 10.0, y + FOLDER_ROW_H);
                                hits.push((row, Hit::Folder(ai, f)));
                                y += FOLDER_ROW_H;
                            }
                        }
                        y += 16.0;
                    }
                    // A clear, separate "Add account" action at the foot of the
                    // list — adding is its own thing, distinct from per-account
                    // settings.
                    hits.push((
                        Rect::new(sb.x0 + 8.0, y, sb.x1 - 8.0, y + ADD_ACCOUNT_ROW_H),
                        Hit::AddAccount,
                    ));
                }
                Role::List => {
                    hits.push((self.refresh_box_rect(), Hit::Refresh));
                    let sr = self.search_box_rect();
                    search_rect = Some(Rect::new(sr.x0 + 30.0, sr.y0, sr.x1 - 26.0, sr.y1));
                    hits.push((sr, Hit::SearchFocus));
                    if !st.search.is_empty() {
                        hits.push((
                            Rect::new(sr.x1 - 26.0, sr.y0, sr.x1, sr.y1),
                            Hit::SearchClear,
                        ));
                    }
                    let top = r.y0 + HEADER_H;
                    let mut ly = top;
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
                Role::Reader => {
                    if st.selected.is_some() {
                        let inset = if st.narrow && st.detail_pushed { BACK_CLEARANCE } else { 20.0 };
                        let mut bx = r.x0 + inset;
                        let by = r.y0 + 12.0;
                        // Compact icon-only buttons when the reader column is
                        // tight (narrow layout, or a slim detail pane) so five
                        // actions always fit; labelled when there's room.
                        let compact = st.narrow || r.width() < READER_COMPACT_W;
                        let specs: [(f64, Hit); 5] = if compact {
                            [(38.0, Hit::Reply), (38.0, Hit::Archive), (38.0, Hit::Delete), (38.0, Hit::Star), (38.0, Hit::Unread)]
                        } else {
                            [(78.0, Hit::Reply), (88.0, Hit::Archive), (78.0, Hit::Delete), (64.0, Hit::Star), (104.0, Hit::Unread)]
                        };
                        for (w, hit) in specs {
                            let b = Rect::new(bx, by, bx + w, by + 32.0);
                            hits.push((b, hit));
                            bx += w + 8.0;
                        }
                    }
                }
            }
        }
        self.hits = hits;
        if let (Some(sr), Some(inp)) = (search_rect, self.search_input.as_mut()) {
            inp.apply_layout_external(sr);
        }
    }

    fn hit_at(&self, pt: Point) -> Option<Hit> {
        self.hits.iter().rev().find(|(rc, _)| rc.contains(pt)).map(|(_, h)| h.clone())
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
            let group_w = 18.0 + 8.0 + w as f64;
            let gx = (rect.x0 + rect.x1) / 2.0 - group_w / 2.0;
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
                    let Some(account) = st.accounts.accounts.get(*ai) else { continue };
                    let active = *ai == st.accounts.active;
                    let hov = rect.contains(st.last_mouse);
                    if active {
                        fill_round(scene, t, *rect, 9.0, c.accent[5].with_alpha_factor(0.12));
                    } else if hov {
                        fill_round(scene, t, *rect, 9.0, c.surface_white_5);
                    }
                    let chev = if account.collapsed { Icon::ChevronRight } else { Icon::ChevronDown };
                    chev.draw(scene, t, Rect::new(rect.x0 + 6.0, rect.y0 + ACCOUNT_HEADER_H / 2.0 - 8.0, rect.x0 + 22.0, rect.y0 + ACCOUNT_HEADER_H / 2.0 + 8.0), c.text_dim);
                    draw_avatar(
                        scene,
                        tr,
                        t,
                        theme,
                        &st.images,
                        Point::new(rect.x0 + 36.0, rect.y0 + ACCOUNT_HEADER_H / 2.0),
                        13.0,
                        &Addr::new(account.config.display_name.clone(), account.config.email.clone()),
                        account.tint,
                    );
                    // Reserve room on the right for the per-account gear.
                    let label = elide(tr, account.label(), 13.0, TextWeight::Semibold, sb.width() - 108.0);
                    let email = elide(tr, &account.config.email, 10.5, TextWeight::Regular, sb.width() - 108.0);
                    tr.draw_text_weighted(scene, &label, 13.0, c.text_primary, t * Affine::translate((rect.x0 + 54.0, rect.y0 + 6.0)), None, TextWeight::Semibold);
                    tr.draw_text(scene, &email, 10.5, c.text_dim, t * Affine::translate((rect.x0 + 54.0, rect.y0 + 24.0)), None);
                    // Per-account settings gear (right edge). Hover-lit; always
                    // drawn so it is visible headless / in shots.
                    let gear = account_gear_rect(*rect);
                    let ghov = gear.contains(st.last_mouse);
                    if ghov {
                        fill_round(scene, t, gear, 8.0, c.surface_white_10);
                    }
                    Icon::Settings.draw(scene, t, gear.inset(-7.0), if ghov { c.text_primary } else { c.text_dim });
                }
                Hit::AddAccount => {
                    let hov = rect.contains(st.last_mouse);
                    if hov {
                        fill_round(scene, t, *rect, 9.0, c.surface_white_5);
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
                Hit::Folder(ai, f) => {
                    let f = *f;
                    let active_account = *ai == st.accounts.active;
                    let selected = active_account && f == st.folder;
                    let hov = rect.contains(st.last_mouse);
                    if selected {
                        fill_round(scene, t, *rect, 8.0, c.accent[5].with_alpha_factor(0.20));
                        fill_rect(scene, t, Rect::new(rect.x0, rect.y0 + 6.0, rect.x0 + 3.0, rect.y1 - 6.0), c.accent[5]);
                    } else if hov {
                        fill_round(scene, t, *rect, 8.0, c.surface_white_5);
                    }
                    let ty = rect.y0 + (FOLDER_ROW_H - 14.0) / 2.0;
                    let label_col = if selected { c.text_primary } else { c.neutral[7] };
                    let icon_box = Rect::new(rect.x0 + 12.0, rect.y0 + FOLDER_ROW_H / 2.0 - 9.0, rect.x0 + 30.0, rect.y0 + FOLDER_ROW_H / 2.0 + 9.0);
                    let icon_col = if selected { c.accent[5] } else { c.neutral[6] };
                    f.icon().draw(scene, t, icon_box, icon_col);
                    tr.draw_text_weighted(
                        scene,
                        f.title(),
                        13.0,
                        label_col,
                        t * Affine::translate((rect.x0 + 40.0, ty)),
                        Some(120.0),
                        if selected { TextWeight::Semibold } else { TextWeight::Regular },
                    );
                    if active_account {
                        let unread = st.mailbox.unread_count(f);
                        if unread > 0 {
                            let txt = unread.to_string();
                            let (twd, _) = tr.measure_weighted(&txt, 11.0, None, TextWeight::Semibold);
                            let bw = (twd as f64 + 14.0).max(20.0);
                            let badge = Rect::new(rect.x1 - bw - 8.0, rect.y0 + 8.0, rect.x1 - 8.0, rect.y0 + 28.0);
                            let bg = if selected { c.accent[5] } else { c.neutral[3] };
                            fill_round(scene, t, badge, 10.0, bg);
                            tr.draw_text_weighted(
                                scene,
                                &txt,
                                11.0,
                                Color::WHITE,
                                t * Affine::translate(((badge.x0 + badge.x1) / 2.0 - twd as f64 / 2.0, badge.y0 + 4.0)),
                                None,
                                TextWeight::Semibold,
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn draw_list(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let st = self.shared.borrow();
        let r = self.rect;
        fill_rect(scene, t, r, c.surfaces.base);

        // Header: folder title (+ count), search box. The title is vertically
        // centred on the NavShell collapse/hamburger chip beside it and spaced a
        // consistent gap (a theme spacing token) past it — never crowding it.
        let sr = self.search_box_rect();
        // Whether the collapse chip sits over this pane's header (the list is
        // then the leftmost pane); matches `header_left_inset`'s List branch.
        let chip_here = st.narrow || !st.sidebar_open;
        let gap = theme.tokens.spacing.s3 as f64;
        let (title_x, title_y, header_cy) = list_header_layout(r, chip_here, gap);
        let title_budget = sr.x0 - title_x - 12.0;
        let title_full = st.folder.title();
        let (twd, _) = tr.measure_weighted(title_full, 18.0, None, TextWeight::Bold);
        let title = crate::draw::elide(tr, title_full, 18.0, TextWeight::Bold, title_budget);
        tr.draw_text_weighted(scene, &title, 18.0, c.text_primary, t * Affine::translate((title_x, title_y)), None, TextWeight::Bold);
        // Count sits after the full title only when there's room for both,
        // centred on the same line.
        let count = st.mailbox.count(st.folder);
        let count_x = title_x + twd as f64 + 10.0;
        if (twd as f64) <= title_budget && count_x + 26.0 < sr.x0 {
            let count_y = header_cy - 12.0 * 1.25 / 2.0;
            tr.draw_text(scene, &format!("{count}"), 12.0, c.text_dim, t * Affine::translate((count_x, count_y)), None);
        }
        fill_round(scene, t, sr, 9.0, c.surfaces.raised);
        stroke_round(scene, t, sr, 9.0, 1.0, if st.search_focused { c.border_focus } else { c.border_standard });
        Icon::Search.draw(scene, t, Rect::new(sr.x0 + 8.0, sr.y0 + 8.0, sr.x0 + 24.0, sr.y0 + 24.0), c.text_dim);
        if st.search.is_empty() && !st.search_focused {
            tr.draw_text(scene, "Search", 13.0, c.text_dim, t * Affine::translate((sr.x0 + 30.0, sr.y0 + 9.0)), None);
        }
        if !st.search.is_empty() {
            Icon::X.draw(scene, t, Rect::new(sr.x1 - 24.0, sr.y0 + 8.0, sr.x1 - 8.0, sr.y0 + 24.0), c.text_dim);
        }

        // Manual sync / refresh button.
        let rb = self.refresh_box_rect();
        let rb_hov = rb.contains(st.last_mouse);
        fill_round(scene, t, rb, 9.0, if rb_hov { c.surface_white_10 } else { c.surfaces.raised });
        stroke_round(scene, t, rb, 9.0, 1.0, c.border_standard);
        Icon::Refresh.draw(scene, t, rb.inset(-9.0), c.neutral[7]);

        // Rows.
        let rows = st.list_rows();
        if rows.is_empty() {
            let hint = if !st.mailbox.messages.is_empty() {
                if st.search.is_empty() { "No messages in this folder" } else { "No results" }
            } else if st.accounts.is_empty() {
                "No account yet — use Add account to set one up"
            } else {
                "No messages"
            };
            tr.draw_text(scene, hint, 13.0, c.text_dim, t * Affine::translate((r.x0 + self.header_left_inset(), r.y0 + HEADER_H + 12.0)), None);
            return;
        }

        for (rect, hit) in &self.hits {
            let Hit::Row(id) = hit else { continue };
            let id = *id;
            if rect.y1 > r.y1 + 1.0 {
                continue;
            }
            let Some(msg) = st.mailbox.get(id) else { continue };
            let len = rows.iter().find(|(rid, _)| *rid == id).map(|(_, l)| *l).unwrap_or(1);
            let selected = st.selected == Some(id) && !st.narrow;
            let hov = rect.contains(st.last_mouse);

            let swipe_dx = self.swipe.filter(|s| s.id == id).map(|s| s.dx).unwrap_or(0.0);
            if swipe_dx.abs() > 1.0 {
                let (col, icon, label, on_left) = if swipe_dx < 0.0 {
                    (c.semantic.warning, Icon::Archive, "Archive", false)
                } else {
                    (c.semantic.danger, Icon::Trash, "Delete", true)
                };
                fill_rect(scene, t, *rect, col.with_alpha_factor(0.9));
                let gx = if on_left { rect.x0 + 18.0 } else { rect.x1 - 90.0 };
                icon.draw(scene, t, Rect::new(gx, rect.y0 + 20.0, gx + 18.0, rect.y0 + 38.0), Color::WHITE);
                tr.draw_text_weighted(scene, label, 12.0, Color::WHITE, t * Affine::translate((gx + 26.0, rect.y0 + 22.0)), None, TextWeight::Semibold);
            }
            let row = Rect::new(rect.x0 + swipe_dx, rect.y0, rect.x1 + swipe_dx, rect.y1);

            if selected {
                fill_rect(scene, t, row, c.accent[5].with_alpha_factor(0.16));
                fill_rect(scene, t, Rect::new(row.x0, row.y0, row.x0 + 3.0, row.y1), c.accent[5]);
            } else if hov && swipe_dx.abs() < 1.0 {
                fill_rect(scene, t, row, c.surface_white_5);
            }
            fill_rect(scene, t, Rect::new(row.x0 + 12.0, row.y1 - 1.0, row.x1, row.y1), c.neutral[2]);

            let unread = !msg.read;
            if unread {
                scene.fill(Fill::NonZero, t, c.accent[5], None, &Circle::new(Point::new(row.x0 + 14.0, row.y0 + LIST_ROW_H / 2.0), 4.0));
            }
            draw_avatar(scene, tr, t, theme, &st.images, Point::new(row.x0 + 42.0, row.y0 + 26.0), 17.0, &msg.from, 0);

            let text_x = row.x0 + 70.0;
            let right = row.x1 - 14.0;
            let name_weight = if unread { TextWeight::Bold } else { TextWeight::Medium };
            let name = elide(tr, &msg.from.display(), 13.5, name_weight, right - text_x - 56.0);
            tr.draw_text_weighted(scene, &name, 13.5, c.text_primary, t * Affine::translate((text_x, row.y0 + 12.0)), None, name_weight);
            let date = format_relative(msg.timestamp_min, st.now_min);
            let (dw, _) = tr.measure(&date, 11.5, None);
            tr.draw_text(scene, &date, 11.5, c.text_dim, t * Affine::translate((right - dw as f64, row.y0 + 13.0)), None);

            let subj_y = row.y0 + 32.0;
            let subj_col = if unread { c.text_primary } else { c.neutral[7] };
            let subj_weight = if unread { TextWeight::Semibold } else { TextWeight::Regular };
            let subj_budget = right - text_x - if len > 1 { 48.0 } else { 16.0 };
            let subject = elide(tr, &msg.thread_subject(), 12.5, subj_weight, subj_budget);
            tr.draw_text_weighted(scene, &subject, 12.5, subj_col, t * Affine::translate((text_x, subj_y)), None, subj_weight);
            if len > 1 {
                let badge_txt = len.to_string();
                let (bw, _) = tr.measure_weighted(&badge_txt, 10.5, None, TextWeight::Semibold);
                let bx = right - bw as f64 - 12.0;
                let badge = Rect::new(bx - 6.0, subj_y - 1.0, right, subj_y + 15.0);
                fill_round(scene, t, badge, 8.0, c.neutral[3]);
                tr.draw_text_weighted(scene, &badge_txt, 10.5, c.neutral[8], t * Affine::translate((bx, subj_y + 1.0)), None, TextWeight::Semibold);
            }

            let icon_reserve = if msg.has_attachments() { 64.0 } else { 28.0 };
            let snippet = elide(tr, &msg.snippet(), 11.5, TextWeight::Regular, right - text_x - icon_reserve);
            tr.draw_text(scene, &snippet, 11.5, c.text_dim, t * Affine::translate((text_x, row.y0 + 52.0)), None);

            // A favourited mail shows a solid, filled star; an unstarred one a
            // faint outline — the state reads at a glance, not just as a colour.
            let star_box = Rect::new(right - 24.0, row.y0 + 48.0, right - 8.0, row.y0 + 64.0);
            if msg.starred {
                Icon::Star.draw_filled(scene, t, star_box, c.semantic.warning);
            } else {
                Icon::Star.draw(scene, t, star_box, c.neutral[4]);
            }
            if msg.has_attachments() {
                Icon::Paperclip.draw(scene, t, Rect::new(right - 44.0, row.y0 + 48.0, right - 30.0, row.y0 + 62.0), c.text_dim);
            }
        }
    }

    fn draw_reader(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let mut chip_hits: Vec<(Rect, MessageId, usize)> = Vec::new();
        let st = self.shared.borrow();
        let rr = self.rect;
        fill_rect(scene, t, rr, c.surfaces.raised);

        let msgs = st.selected_thread_messages();
        if msgs.is_empty() {
            self.reader_attachment_hits.clear();
            let cx = (rr.x0 + rr.x1) / 2.0;
            let cy = rr.y0 + rr.height() / 2.0;
            Icon::Mail.draw(scene, t, Rect::new(cx - 26.0, cy - 66.0, cx + 26.0, cy - 14.0), c.neutral[4]);
            let hint = "Select a conversation to read";
            let (w, _) = tr.measure(hint, 14.0, None);
            tr.draw_text(scene, hint, 14.0, c.text_dim, t * Affine::translate((cx - w as f64 / 2.0, cy)), None);
            return;
        }

        // Action bar (header strip).
        let compact = st.narrow || rr.width() < READER_COMPACT_W;
        let starred = st.selected.and_then(|id| st.mailbox.get(id)).map(|m| m.starred).unwrap_or(false);
        for (rect, hit) in &self.hits {
            let (label, danger, icon) = match hit {
                Hit::Reply => ("Reply", false, Icon::Reply),
                Hit::Archive => ("Archive", false, Icon::Archive),
                Hit::Delete => ("Delete", true, Icon::Trash),
                Hit::Star => (if starred { "Starred" } else { "Star" }, false, Icon::Star),
                Hit::Unread => ("Mark unread", false, Icon::Mail),
                _ => continue,
            };
            let hov = rect.contains(st.last_mouse);
            let bg = if hov { c.surface_white_10 } else { c.surface_white_5 };
            fill_round(scene, t, *rect, 8.0, bg);
            stroke_round(scene, t, *rect, 8.0, 1.0, c.border_white_10);
            // The Star action fills its glyph when the message is starred, so
            // the favourited state is a solid star, not merely a tinted outline.
            let is_star_on = matches!(hit, Hit::Star) && starred;
            let col = if danger { c.semantic.danger } else if is_star_on { c.semantic.warning } else { c.neutral[8] };
            let paint_icon = |scene: &mut Scene, ibox: Rect| {
                if is_star_on {
                    Icon::Star.draw_filled(scene, t, ibox, col);
                } else {
                    icon.draw(scene, t, ibox, col);
                }
            };
            if compact {
                paint_icon(scene, Rect::new(rect.center().x - 8.0, rect.y0 + 8.0, rect.center().x + 8.0, rect.y0 + 24.0));
            } else {
                paint_icon(scene, Rect::new(rect.x0 + 10.0, rect.y0 + 8.0, rect.x0 + 26.0, rect.y0 + 24.0));
                tr.draw_text_weighted(scene, label, 12.0, col, t * Affine::translate((rect.x0 + 30.0, rect.y0 + 9.0)), None, TextWeight::Medium);
            }
        }

        let head = &msgs[msgs.len() - 1];
        let content_x = rr.x0 + 24.0;
        let content_w = (rr.width() - 48.0) as f32;
        let mut y = rr.y0 + HEADER_H + 8.0;
        let subj = elide(tr, &head.thread_subject(), 20.0, TextWeight::Bold, content_w as f64);
        tr.draw_text_weighted(scene, &subj, 20.0, c.text_primary, t * Affine::translate((content_x, y)), None, TextWeight::Bold);
        y += 30.0;
        tr.draw_text(
            scene,
            &format!("{} message{} · {}", msgs.len(), if msgs.len() == 1 { "" } else { "s" }, st.folder.title()),
            12.0,
            c.text_dim,
            t * Affine::translate((content_x, y)),
            None,
        );
        y += 26.0;
        fill_rect(scene, t, Rect::new(content_x, y, rr.x1 - 24.0, y + 1.0), c.border_standard);
        y += 16.0;

        // Render the thread. When the head (newest, being-read) message carries
        // an HTML body, its body renders natively (sanitized rich text + images);
        // older messages and plain-text mail render as text cards.
        let head_id = head.id;
        let head_html = head.html_body.clone();
        let mut body_rect: Option<Rect> = None;
        for (i, m) in msgs.iter().enumerate() {
            if y > rr.y1 - 40.0 {
                break;
            }
            let is_head = i == msgs.len() - 1;
            if is_head && head_html.is_some() {
                // Head card: draw the header, then reserve the rest of the
                // reader for the native HTML body (painted once the shared
                // borrow is released, since it needs `&mut self`).
                y = self.draw_card_header(scene, t, tr, theme, m, content_x, y, content_w);
                y += 8.0;
                let bottom = rr.y1 - 46.0; // room for the remote-images bar
                if bottom > y + 48.0 {
                    body_rect = Some(Rect::new(content_x, y, content_x + content_w as f64, bottom));
                }
            } else {
                y = self.draw_message_card(scene, t, tr, theme, m, content_x, y, content_w, is_head, &mut chip_hits);
                y += 14.0;
            }
        }
        self.reader_attachment_hits = chip_hits;
        let images = st.images.clone();
        drop(st);

        // The native HTML body + its remote-images toggle, now that the shared
        // borrow is released (they need `&mut self`).
        self.html_body_rect = None;
        self.html_toggle_rect = None;
        self.html_link_hits.clear();
        match (body_rect, head_html) {
            (Some(rect), Some(html)) => {
                // Switching to a different message re-blocks remote content and
                // resets the scroll; parse the body once and cache it by id.
                if self.html_shown_id != Some(head_id) {
                    self.html_load_remote = false;
                    self.html_scroll = 0.0;
                    self.html_shown_id = Some(head_id);
                }
                if self.html_blocks.as_ref().map(|(id, _)| *id) != Some(head_id) {
                    self.html_blocks = Some((head_id, crate::html_mail::parse(&html)));
                }
                let blocks = self.html_blocks.as_ref().map(|(_, b)| b.as_slice()).unwrap_or(&[]);
                let block_remote = !self.html_load_remote;
                let rendered = crate::html_mail::draw_body(
                    scene, t, tr, theme, &images, blocks, rect, self.html_scroll, block_remote,
                );
                // Clamp the scroll against the freshly-measured content height.
                self.html_content_h = rendered.content_height;
                let max_scroll = (rendered.content_height - rect.height()).max(0.0);
                if self.html_scroll > max_scroll {
                    self.html_scroll = max_scroll;
                }
                self.html_link_hits = rendered.links;
                self.html_loading = rendered.loading;
                self.html_body_rect = Some(rect);
                self.draw_remote_toggle(scene, t, tr, theme, rect);
            }
            _ => {
                // Plain-text mail this frame — drop any cached body so returning
                // to an HTML mail parses fresh (and blocked again by default).
                self.html_blocks = None;
                self.html_shown_id = None;
                self.html_loading = false;
            }
        }
    }

    /// Draw the remote-images control under an HTML body and record its rect for
    /// hit-testing. Blocked by default (a clear warning colour); clicking loads
    /// remote images, and the loaded state offers "Block again".
    fn draw_remote_toggle(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, body: Rect) {
        let c = &theme.tokens.colors;
        let y = body.y1 + 8.0;
        // Two visually distinct states, both keyed on a clear semantic colour —
        // a warning (amber/gold) accent while blocked, neutral once loaded.
        let (icon, label, accent) = if self.html_load_remote {
            (Icon::Image, "Remote images loaded · Block again", c.text_dim)
        } else {
            (Icon::Ban, "Remote images blocked · Load", c.semantic.warning)
        };
        let (lw, _) = tr.measure(label, 11.5, None);
        let pill = Rect::new(body.x0, y, body.x0 + 30.0 + lw as f64 + 14.0, y + 28.0);
        let hov = pill.contains(self.shared.borrow().last_mouse);
        // Blocked: a warning-tinted pill so it reads unmistakably as a warning
        // (never a stray accent). Loaded: a plain neutral surface.
        let (bg, border) = if self.html_load_remote {
            (if hov { c.surface_white_10 } else { c.surface_white_5 }, c.border_white_10)
        } else {
            (
                c.semantic.warning.with_alpha_factor(if hov { 0.22 } else { 0.14 }),
                c.semantic.warning.with_alpha_factor(0.55),
            )
        };
        fill_round(scene, t, pill, 8.0, bg);
        stroke_round(scene, t, pill, 8.0, 1.0, border);
        icon.draw(scene, t, Rect::new(pill.x0 + 8.0, pill.y0 + 6.0, pill.x0 + 24.0, pill.y0 + 22.0), accent);
        tr.draw_text(scene, label, 11.5, c.text_primary, t * Affine::translate((pill.x0 + 30.0, pill.y0 + 7.0)), None);
        self.html_toggle_rect = Some(pill);
    }

    /// The colour the remote-images control paints its icon this frame — warning
    /// (amber) while blocked, neutral once loaded. Lets a test prove the colour
    /// regression (a stray non-warning tint) can't return.
    #[cfg(test)]
    pub(crate) fn remote_toggle_icon_color(&self, theme: &Theme) -> Color {
        if self.html_load_remote {
            theme.tokens.colors.text_dim
        } else {
            theme.tokens.colors.semantic.warning
        }
    }

    /// Whether the reader currently shows a native HTML body.
    #[cfg(test)]
    pub(crate) fn html_body_shown(&self) -> bool {
        self.html_body_rect.is_some()
    }

    /// The remote-images opt-in state (true = remote images loaded).
    #[cfg(test)]
    pub(crate) fn html_remote_loaded(&self) -> bool {
        self.html_load_remote
    }

    /// The link hit-rects resolved during the last HTML-body draw.
    #[cfg(test)]
    pub(crate) fn html_links(&self) -> &[(Rect, String)] {
        &self.html_link_hits
    }

    /// Draw the per-message card header (avatar · sender · date · recipients)
    /// and return the y where the body should start. Shared by the text card
    /// path and the native HTML-body path so both read identically.
    fn draw_card_header(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, m: &Message, x: f64, y: f64, w: f32) -> f64 {
        let c = &theme.tokens.colors;
        let card_x1 = x + w as f64;
        let st = self.shared.borrow();
        draw_avatar(scene, tr, t, theme, &st.images, Point::new(x + 18.0, y + 18.0), 17.0, &m.from, 0);
        let from_name = elide(tr, &m.from.display(), 13.5, TextWeight::Semibold, (w - 150.0) as f64);
        tr.draw_text_weighted(scene, &from_name, 13.5, c.text_primary, t * Affine::translate((x + 44.0, y + 6.0)), None, TextWeight::Semibold);
        let from_email = elide(tr, &m.from.email, 11.5, TextWeight::Regular, (w - 150.0) as f64);
        tr.draw_text(scene, &from_email, 11.5, c.text_dim, t * Affine::translate((x + 44.0, y + 24.0)), None);
        let date = format_absolute(m.timestamp_min);
        let (dw, _) = tr.measure(&date, 11.5, None);
        tr.draw_text(scene, &date, 11.5, c.text_dim, t * Affine::translate((card_x1 - dw as f64, y + 8.0)), None);
        let to_line = elide(tr, &format!("to {}", m.to.iter().map(|a| a.display()).collect::<Vec<_>>().join(", ")), 11.0, TextWeight::Regular, 150.0);
        tr.draw_text(scene, &to_line, 11.0, c.text_dim, t * Affine::translate((card_x1 - 150.0, y + 24.0)), None);
        y + 48.0
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_message_card(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, m: &Message, x: f64, y: f64, w: f32, expanded: bool, chip_hits: &mut Vec<(Rect, MessageId, usize)>) -> f64 {
        let c = &theme.tokens.colors;
        let card_x1 = x + w as f64;
        let mut yy = self.draw_card_header(scene, t, tr, theme, m, x, y, w);
        if expanded {
            for para in m.body.split('\n') {
                if para.trim().is_empty() {
                    yy += 8.0;
                    continue;
                }
                let quote = para.starts_with('>');
                let col = if quote { c.text_dim } else { c.neutral[8] };
                let (_pw, ph) = tr.measure(para, 13.0, Some(w - 8.0));
                tr.draw_text(scene, para, 13.0, col, t * Affine::translate((x, yy)), Some(w - 8.0));
                yy += ph.max(18.0) as f64 + 2.0;
            }
            if m.has_attachments() {
                yy += 8.0;
                tr.draw_text_weighted(scene, &format!("{} attachment{}", m.attachments.len(), if m.attachments.len() == 1 { "" } else { "s" }), 11.5, c.text_dim, t * Affine::translate((x, yy)), None, TextWeight::Medium);
                yy += 20.0;
                let mut ax = x;
                let hov_mouse = self.shared.borrow().last_mouse;
                for (ai, a) in m.attachments.iter().enumerate() {
                    let label = format!("{}  {}", a.filename, a.size_human());
                    let (lw, _) = tr.measure(&label, 11.5, None);
                    // Room for the trailing download/state glyph.
                    let chip_w = lw as f64 + 64.0;
                    if ax + chip_w > card_x1 {
                        ax = x;
                        yy += 36.0;
                    }
                    let chip = Rect::new(ax, yy, ax + chip_w, yy + 30.0);
                    let hov = chip.contains(hov_mouse);
                    fill_round(scene, t, chip, 8.0, if hov { c.surface_white_10 } else { c.surface_white_5 });
                    stroke_round(scene, t, chip, 8.0, 1.0, c.border_white_10);
                    a.icon().draw(scene, t, Rect::new(chip.x0 + 7.0, chip.y0 + 7.0, chip.x0 + 23.0, chip.y0 + 23.0), c.neutral[7]);
                    tr.draw_text(scene, &a.filename, 11.5, c.text_primary, t * Affine::translate((chip.x0 + 30.0, chip.y0 + 4.0)), Some(lw));
                    tr.draw_text(scene, &a.size_human(), 10.0, c.text_dim, t * Affine::translate((chip.x0 + 30.0, chip.y0 + 17.0)), None);
                    // Trailing affordance: a save/download icon when the bytes
                    // are present (clickable), a cloud when it must still be
                    // hydrated from the server.
                    let (icon, col) = if a.is_downloadable() {
                        (Icon::Download, c.accent[5])
                    } else {
                        (Icon::CloudDownload, c.neutral[5])
                    };
                    icon.draw(scene, t, Rect::new(chip.x1 - 24.0, chip.y0 + 7.0, chip.x1 - 8.0, chip.y0 + 23.0), col);
                    chip_hits.push((chip, m.id, ai));
                    ax += chip_w + 10.0;
                }
                yy += 30.0;
            }
        } else {
            // Collapsed preview: a single elided line so a narrow reader column
            // can't wrap it into the following card.
            let snip = elide(tr, &m.snippet(), 12.0, TextWeight::Regular, (w - 8.0) as f64);
            tr.draw_text(scene, &snip, 12.0, c.text_dim, t * Affine::translate((x, yy)), None);
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
            Role::Sidebar => {
                if let Some(hit) = self.hit_at(pt) {
                    match hit {
                        Hit::Compose => self.shared.borrow_mut().push_intent(Intent::Compose),
                        Hit::AccountSettings(ai) => {
                            // Per-account settings, scoped to the clicked account
                            // (not the active one).
                            self.shared.borrow_mut().push_intent(Intent::Settings(Some(ai)));
                        }
                        Hit::AddAccount => {
                            // Adding an account is its own action (a blank sheet).
                            self.shared.borrow_mut().push_intent(Intent::Settings(None));
                        }
                        Hit::AccountToggle(ai) => self.shared.borrow_mut().toggle_account_collapsed(ai),
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
            }
            Role::List => {
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
                                st.push_intent(Intent::Engine(crate::engine::EngineRequest::Fetch { account, folder }));
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
                                .map(|m| m.folder == Folder::Drafts)
                                .unwrap_or(false);
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
                // Record a potential swipe origin whenever the press lands on a
                // row. A later drag past the threshold turns it into a swipe
                // action (archive/delete) on release; a release without a drag
                // is a plain tap, which already selected on press.
                if let Some(id) = self.row_id_at(pt) {
                    self.mouse_down_row = Some((id, pt));
                }
            }
            Role::Reader => {
                // The remote-images toggle under an HTML body: opt this message
                // in to (or back out of) loading remote content.
                if self.html_toggle_rect.map(|r| r.contains(pt)).unwrap_or(false) {
                    self.html_load_remote = !self.html_load_remote;
                    self.dirty = true;
                    self.shared.borrow_mut().dirty = true;
                    *handled = true;
                    return;
                }
                // A link in the HTML body: surface its destination (the reader
                // has no browser wiring of its own — the sanitized href is shown
                // so the user can see where a link leads).
                if let Some((_, href)) = self.html_link_hits.iter().find(|(rc, _)| rc.contains(pt)).cloned() {
                    let mut st = self.shared.borrow_mut();
                    st.status = Some(format!("Link: {href}"));
                    st.dirty = true;
                    *handled = true;
                    return;
                }
                // An attachment chip saves the real bytes to disk.
                if let Some((_, mid, ai)) = self
                    .reader_attachment_hits
                    .iter()
                    .find(|(rc, _, _)| rc.contains(pt))
                    .cloned()
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
        }
    }

    /// Save a reader attachment's real bytes to the downloads directory, or
    /// report that it hasn't been downloaded (hydrated) yet.
    fn save_attachment_click(&mut self, mid: MessageId, ai: usize) {
        let mut st = self.shared.borrow_mut();
        let Some(att) = st.mailbox.get(mid).and_then(|m| m.attachments.get(ai)).cloned() else {
            return;
        };
        if !att.is_downloadable() {
            st.status = Some(format!("{} isn't downloaded yet", att.filename));
            st.dirty = true;
            return;
        }
        st.status = Some(match crate::util::save_download(&att.filename, &att.content) {
            Ok(path) => format!("Saved {} to {}", att.filename, path.display()),
            Err(e) => format!("Couldn't save {}: {e}", att.filename),
        });
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

impl Widget for Pane {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if let Some(i) = self.search_input.as_mut() {
            i.sync(state, path);
        }
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
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
        self.rebuild_hits();
    }

    fn draw(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
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

        // Reader: a wheel over a native HTML body scrolls it (long newsletters
        // exceed the reader height). Clamp to the measured content height.
        if self.role == Role::Reader {
            if let (WindowEvent::MouseWheel { delta, .. }, Some(rect)) = (event, self.html_body_rect) {
                let over = rect.contains(self.shared.borrow().last_mouse);
                if over {
                    let dy = match delta {
                        winit::event::MouseScrollDelta::LineDelta(_, y) => (*y as f64) * 48.0,
                        winit::event::MouseScrollDelta::PixelDelta(p) => p.y / scale_factor,
                    };
                    let max_scroll = (self.html_content_h - rect.height()).max(0.0);
                    let next = (self.html_scroll - dy).clamp(0.0, max_scroll);
                    if (next - self.html_scroll).abs() > f64::EPSILON {
                        self.html_scroll = next;
                        self.dirty = true;
                        self.shared.borrow_mut().dirty = true;
                    }
                    *handled = true;
                    return;
                }
            }
        }

        // Keyboard: route to the focused search field (list only).
        if matches!(event, WindowEvent::KeyboardInput { .. }) && self.role == Role::List {
            let focused = self.shared.borrow().search_focused;
            if focused {
                if let Some(inp) = self.search_input.as_mut() {
                    inp.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
                }
                // Reflect the field into the query + first-row selection.
                let value = self.search_input.as_ref().map(|i| i.value().to_string()).unwrap_or_default();
                let mut st = self.shared.borrow_mut();
                st.search = value;
                crate::doc::MailDoc::set_search(state, &st.search);
                st.selected = st.list_rows().first().map(|(id, _)| *id);
                st.dirty = true;
                *handled = true;
            }
            return;
        }

        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if self.role == Role::List {
                    let pt = self.shared.borrow().last_mouse;
                    if let Some(id) = self.row_id_at(pt) {
                        self.shared.borrow_mut().push_intent(Intent::ContextMenu(id, pt));
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let pt = self.shared.borrow().last_mouse;
                self.on_left_press(state, pt, handled);
            }
            WindowEvent::CursorMoved { .. } => {
                if self.role == Role::List {
                    if let Some((id, start)) = self.mouse_down_row {
                        let dx = self.shared.borrow().last_mouse.x - start.x;
                        if dx.abs() > 6.0 {
                            self.swipe = Some(SwipeState { id, dx });
                            self.dirty = true;
                            self.shared.borrow_mut().dirty = true;
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.role == Role::List {
                    if let Some(s) = self.swipe.take() {
                        if s.dx <= -SWIPE_THRESHOLD {
                            self.shared.borrow_mut().apply_message_action(state, s.id, CtxAction::Archive);
                        } else if s.dx >= SWIPE_THRESHOLD {
                            self.shared.borrow_mut().apply_message_action(state, s.id, CtxAction::Delete);
                        }
                        *handled = true;
                    }
                    self.mouse_down_row = None;
                }
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
        self.dirty
            || self.shared.borrow().dirty
            || self.search_input.as_ref().map(|i| i.needs_redraw()).unwrap_or(false)
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the frame pump alive while an opted-in HTML-body image is still
        // fetching/decoding, so it appears the moment it is ready.
        self.html_loading
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some(i) = self.search_input.as_mut() {
            i.mark_drawn();
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
        p.handle_event(ev, 1.0, &Modifiers::default(), &mut doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
        handled
    }

    fn press() -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Pressed, button: MouseButton::Left }
    }
    fn release() -> WindowEvent {
        WindowEvent::MouseInput { device_id: DeviceId::dummy(), state: ElementState::Released, button: MouseButton::Left }
    }
    fn moved(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved { device_id: DeviceId::dummy(), position: PhysicalPosition::new(p.x, p.y) }
    }
    fn named(k: NamedKey) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(k);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput { device_id: DeviceId::dummy(), event: ev, is_synthetic: false }
    }

    #[test]
    fn reader_action_bar_star_toggles_star() {
        let sh = shared();
        let id = { let st = sh.borrow(); st.mailbox.messages_in(Folder::Inbox)[0].id };
        let mut p = reader_for(&sh, id, 900.0, 800.0);
        let before = sh.borrow().mailbox.get(id).unwrap().starred;
        let star = p.hits.iter().find(|(_, h)| matches!(h, Hit::Star)).map(|(r, _)| *r).expect("star button present");
        sh.borrow_mut().last_mouse = star.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, star.center(), &mut handled);
        assert!(handled, "reader Star click consumed");
        assert_eq!(sh.borrow().mailbox.get(id).unwrap().starred, !before, "Star button toggled the flag");
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
        assert!((title_center - chip_cy).abs() < 0.001, "title centre {title_center} != chip centre {chip_cy}");
        assert!((header_cy - chip_cy).abs() < 0.001, "header centre line is the chip centre");
        // Spaced exactly one gap past the chip, and not crowding it.
        assert!((title_x - (chip_right + gap)).abs() < 0.001, "title starts one gap past the chip");
        assert!(title_x - chip_right >= 10.0, "title does not crowd the collapse button");
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
            st.mailbox.insert(crate::transport::parse_eml(raw, 200, Folder::Inbox).unwrap());
            st.mailbox.insert(crate::transport::parse_eml(raw, 300, Folder::Inbox).unwrap());
            st.selected = Some(id);
            id
        };
        assert_eq!(sh.borrow().mailbox.count(Folder::Inbox), 1, "one message, not three");
        assert_eq!(
            sh.borrow().selected_thread_messages().len(),
            1,
            "the reader draws exactly one card for a re-fetched mail"
        );
        let _ = id;
    }

    #[test]
    fn html_only_mail_renders_a_readable_body_not_a_broken_block() {
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
        let mut p = reader_for(&sh, id, 1000.0, 820.0);
        draw(&mut p);

        // The HTML body renders natively (not a webview colour block), remote
        // content blocked by default, with its toggle shown.
        assert!(p.html_body_shown(), "the HTML body is rendered");
        assert!(!p.html_remote_loaded(), "remote content is blocked by default");
        assert!(p.html_toggle_rect.is_some(), "the remote-images toggle is shown");

        // Real, readable text came through — headings + paragraphs, not an empty
        // or opaque block.
        let (_, blocks) = p.html_blocks.as_ref().expect("parsed body cached");
        let text: String = blocks
            .iter()
            .flat_map(|b| match b {
                HtmlBlock::Heading(_, s) | HtmlBlock::Paragraph(s) => s.clone(),
                _ => vec![],
            })
            .map(|s| s.text)
            .collect::<Vec<_>>()
            .join(" ");
        assert!(text.contains("This week in qOS"), "heading text rendered: {text}");
        assert!(text.contains("scrollable-strip"), "body text rendered: {text}");

        // The article link is hit-testable (rendered as a real link).
        assert!(
            p.html_links().iter().any(|(_, href)| href.contains("qos.example/weekly")),
            "the body link is resolved and clickable"
        );

        // The blocked-state toggle uses the warning (amber) colour — never a
        // stray accent/purple tint.
        let theme = Theme::dark();
        assert_eq!(
            p.remote_toggle_icon_color(&theme),
            theme.tokens.colors.semantic.warning,
            "the blocked toggle paints the warning colour"
        );
    }

    #[test]
    fn plain_text_mail_renders_without_an_html_body() {
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
        assert!(!p.html_body_shown(), "a plain-text mail renders no HTML body");
        assert!(p.html_toggle_rect.is_none(), "no remote-images toggle for plain text");
    }

    #[test]
    fn remote_images_toggle_flips_state_and_uses_the_correct_colour() {
        let sh = shared();
        let id = {
            let st = sh.borrow();
            st.mailbox.messages.values().find(|m| m.from.email == "news@weekly.qos.example").unwrap().id
        };
        let mut p = reader_for(&sh, id, 1000.0, 820.0);
        draw(&mut p);
        let theme = Theme::dark();
        assert!(!p.html_remote_loaded(), "blocked by default");
        assert_eq!(
            p.remote_toggle_icon_color(&theme),
            theme.tokens.colors.semantic.warning,
            "blocked state is the warning colour"
        );

        // Click the toggle → opt in; state flips and the colour goes neutral.
        let toggle = p.html_toggle_rect.expect("toggle shown");
        sh.borrow_mut().last_mouse = toggle.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, toggle.center(), &mut handled);
        assert!(handled, "toggle click consumed");
        draw(&mut p);
        assert!(p.html_remote_loaded(), "opting in loads remote images");
        assert_eq!(
            p.remote_toggle_icon_color(&theme),
            theme.tokens.colors.text_dim,
            "loaded state is neutral, not a warning"
        );

        // Click again → back to blocked + warning.
        let toggle = p.html_toggle_rect.expect("toggle shown");
        sh.borrow_mut().last_mouse = toggle.center();
        let mut handled = false;
        p.on_left_press(&mut doc, toggle.center(), &mut handled);
        assert!(handled, "second toggle click consumed");
        draw(&mut p);
        assert!(!p.html_remote_loaded(), "blocking again re-blocks remote images");
        assert_eq!(
            p.remote_toggle_icon_color(&theme),
            theme.tokens.colors.semantic.warning,
            "re-blocking returns to the warning colour (never purple)"
        );
    }

    #[test]
    fn reader_star_action_reflects_the_starred_state() {
        // Toggling the reader Star flips the flag that selects the filled vs
        // outline glyph (the draw path picks the fill purely from this state).
        let sh = shared();
        let id = { let st = sh.borrow(); st.mailbox.messages_in(Folder::Inbox)[0].id };
        // Force a known starting state.
        { sh.borrow_mut().mailbox.get_mut(id).unwrap().starred = false; }
        let mut p = reader_for(&sh, id, 900.0, 800.0);
        let star = p.hits.iter().find(|(_, h)| matches!(h, Hit::Star)).map(|(r, _)| *r).unwrap();
        sh.borrow_mut().last_mouse = star.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, star.center(), &mut handled);
        assert!(sh.borrow().mailbox.get(id).unwrap().starred, "starring sets the flag the filled glyph keys on");
    }

    #[test]
    fn reader_attachment_chip_click_saves_real_bytes() {
        let dir = std::env::temp_dir().join(format!("mail-dl-{}", uuid::Uuid::new_v4()));
        unsafe { std::env::set_var("QOS_MAIL_DOWNLOADS", &dir) };

        let sh = shared();
        // The Radia message is parsed from real MIME and carries a hydrated PDF.
        let id = {
            let st = sh.borrow();
            st.mailbox.messages.values().find(|m| m.from.email == "radia@spanning.example").unwrap().id
        };
        let mut p = reader_for(&sh, id, 760.0, 900.0);
        // The chip layout is only known after a draw.
        draw(&mut p);
        assert!(!p.reader_attachment_hits.is_empty(), "attachment chip has a hit region");
        let (rect, mid, _ai) = p.reader_attachment_hits[0];
        assert_eq!(mid, id);

        sh.borrow_mut().last_mouse = rect.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, rect.center(), &mut handled);
        assert!(handled, "chip click consumed");

        let saved = dir.join("spanning-tree.pdf");
        assert!(saved.exists(), "the real attachment bytes were written to disk");
        assert!(!std::fs::read(&saved).unwrap().is_empty());
        assert!(sh.borrow().status.as_ref().unwrap().contains("Saved"), "toast confirms the save");

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
            .find_map(|(r, h)| if let Hit::Row(i) = h { Some((*i, *r)) } else { None })
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

        assert_eq!(sh.borrow().mailbox.count(Folder::Archive), before + 1, "swipe-left archived the row");
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
        let sr = p.hits.iter().find(|(_, h)| matches!(h, Hit::SearchFocus)).map(|(r, _)| *r).unwrap();
        sh.borrow_mut().last_mouse = sr.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, sr.center(), &mut handled);
        assert!(sh.borrow().search_focused, "clicking the field focuses search");

        // Put a query in the field and drive the pane's reflect path.
        if let Some(i) = p.search_input.as_mut() {
            i.set_text("trajectory");
        }
        dispatch(&mut p, &named(NamedKey::ArrowRight));
        assert_eq!(sh.borrow().search, "trajectory", "field value reflected into the query");
        let filtered = sh.borrow().list_rows();
        assert!(!filtered.is_empty() && filtered.len() < full_rows, "results narrowed to matches");
        assert!(filtered.iter().all(|(mid, _)| {
            let st = sh.borrow();
            let m = st.mailbox.get(*mid).unwrap();
            m.subject.to_ascii_lowercase().contains("trajectory")
                || m.body.to_ascii_lowercase().contains("trajectory")
        }));

        // Clear (X) resets the query and rows.
        p.rebuild_hits();
        let clr = p.hits.iter().find(|(_, h)| matches!(h, Hit::SearchClear)).map(|(r, _)| *r).expect("clear button appears");
        let mut handled = false;
        p.on_left_press(&mut doc, clr.center(), &mut handled);
        assert!(sh.borrow().search.is_empty(), "clear reset the query");
        assert_eq!(sh.borrow().list_rows().len(), full_rows, "rows restored after clear");
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
        let row = p.hits.iter().rev().find(|(_, h)| matches!(h, Hit::Row(_))).map(|(r, _)| *r).unwrap();
        sh.borrow_mut().last_mouse = row.center();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        p.on_left_press(&mut doc, row.center(), &mut handled);
        let intents = sh.borrow_mut().take_intents();
        assert!(intents.iter().any(|i| matches!(i, Intent::EditDraft(mid) if *mid == id)), "a Drafts row raises EditDraft");
        assert!(!intents.iter().any(|i| matches!(i, Intent::ShowDetail)), "it does not open the reader");
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
        assert_eq!(sh.borrow().accounts.active, 0, "account #1 is not the active one");
        // The gear on account #1 (you@hooli.example) must scope settings to #1.
        let gear = p
            .hits
            .iter()
            .find_map(|(r, h)| matches!(h, Hit::AccountSettings(1)).then_some(*r))
            .expect("account #1 has a per-account settings gear");
        let intents = click_sidebar(&mut p, &sh, gear.center());
        assert!(
            intents.iter().any(|i| matches!(i, Intent::Settings(Some(1)))),
            "the gear opens settings scoped to account #1, got {intents:?}"
        );
        assert!(
            !intents.iter().any(|i| matches!(i, Intent::Settings(Some(0)) | Intent::Settings(None))),
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
            p.hit_at(Point::new(p.rect.x1 - 24.0, p.rect.y0 + 28.0)).is_none(),
            "no global settings control in the sidebar header"
        );
    }
}
