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

use crate::account::AccountStore;
use crate::draw::{
    draw_avatar, elide, fill_rect, fill_round, stroke_round, ACCOUNT_HEADER_H, BACK_CLEARANCE,
    CHIP_CLEARANCE, FOLDER_ROW_H, HEADER_H, LIST_ROW_H,
};
use crate::model::{Addr, Folder, Message, MessageId};
use crate::state::{CtxAction, Intent, Shared};
use crate::util::{format_absolute, format_relative};

/// Distance (px) a horizontal drag must pass to fire a swipe action.
const SWIPE_THRESHOLD: f64 = 72.0;
/// Reader width (px) below which the action bar switches to icon-only buttons.
const READER_COMPACT_W: f64 = 480.0;

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
    Settings,
    Account(usize),
    AccountToggle(usize),
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
                    // settings gear (header, right)
                    let gear = Rect::new(sb.x1 - 44.0, sb.y0 + 12.0, sb.x1 - 12.0, sb.y0 + 44.0);
                    hits.push((gear, Hit::Settings));
                    // compose button under the header
                    let pad = 12.0;
                    let mut y = sb.y0 + HEADER_H + 6.0;
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

        // Header: app label (past the collapse chip) + settings gear.
        tr.draw_text_weighted(
            scene,
            "Mail",
            15.0,
            c.text_primary,
            t * Affine::translate((sb.x0 + CHIP_CLEARANCE, sb.y0 + 18.0)),
            None,
            TextWeight::Bold,
        );
        for (rect, hit) in &self.hits {
            if matches!(hit, Hit::Settings) {
                let hov = rect.contains(st.last_mouse);
                if hov {
                    fill_round(scene, t, *rect, 8.0, c.surface_white_5);
                }
                Icon::Settings.draw(scene, t, rect.inset(-8.0), c.neutral[7]);
            }
        }

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
                    let label = elide(tr, account.label(), 13.0, TextWeight::Semibold, sb.width() - 80.0);
                    let email = elide(tr, &account.config.email, 10.5, TextWeight::Regular, sb.width() - 80.0);
                    tr.draw_text_weighted(scene, &label, 13.0, c.text_primary, t * Affine::translate((rect.x0 + 54.0, rect.y0 + 6.0)), None, TextWeight::Semibold);
                    tr.draw_text(scene, &email, 10.5, c.text_dim, t * Affine::translate((rect.x0 + 54.0, rect.y0 + 24.0)), None);
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

        // Header: folder title (+ count), search box.
        let sr = self.search_box_rect();
        let title_x = r.x0 + self.header_left_inset();
        let title_budget = sr.x0 - title_x - 12.0;
        let title_full = st.folder.title();
        let (twd, _) = tr.measure_weighted(title_full, 18.0, None, TextWeight::Bold);
        let title = crate::draw::elide(tr, title_full, 18.0, TextWeight::Bold, title_budget);
        tr.draw_text_weighted(scene, &title, 18.0, c.text_primary, t * Affine::translate((title_x, r.y0 + 14.0)), None, TextWeight::Bold);
        // Count sits after the full title only when there's room for both.
        let count = st.mailbox.count(st.folder);
        let count_x = title_x + twd as f64 + 10.0;
        if (twd as f64) <= title_budget && count_x + 26.0 < sr.x0 {
            tr.draw_text(scene, &format!("{count}"), 12.0, c.text_dim, t * Affine::translate((count_x, r.y0 + 19.0)), None);
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
                "No account yet — open the gear to add one"
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

            let star_col = if msg.starred { c.semantic.warning } else { c.neutral[4] };
            Icon::Star.draw(scene, t, Rect::new(right - 24.0, row.y0 + 48.0, right - 8.0, row.y0 + 64.0), star_col);
            if msg.has_attachments() {
                Icon::Paperclip.draw(scene, t, Rect::new(right - 44.0, row.y0 + 48.0, right - 30.0, row.y0 + 62.0), c.text_dim);
            }
        }
    }

    fn draw_reader(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
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
            let col = if danger { c.semantic.danger } else if matches!(hit, Hit::Star) && starred { c.semantic.warning } else { c.neutral[8] };
            if compact {
                icon.draw(scene, t, Rect::new(rect.center().x - 8.0, rect.y0 + 8.0, rect.center().x + 8.0, rect.y0 + 24.0), col);
            } else {
                icon.draw(scene, t, Rect::new(rect.x0 + 10.0, rect.y0 + 8.0, rect.x0 + 26.0, rect.y0 + 24.0), col);
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

        for (i, m) in msgs.iter().enumerate() {
            if y > rr.y1 - 40.0 {
                break;
            }
            y = self.draw_message_card(scene, t, tr, theme, m, content_x, y, content_w, i == msgs.len() - 1, &mut chip_hits);
            y += 14.0;
        }
        self.reader_attachment_hits = chip_hits;
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_message_card(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, m: &Message, x: f64, y: f64, w: f32, expanded: bool, chip_hits: &mut Vec<(Rect, MessageId, usize)>) -> f64 {
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

        let mut yy = y + 48.0;
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
                        Hit::Settings => {
                            let active = self.shared.borrow().accounts.active;
                            let has = !self.shared.borrow().accounts.is_empty();
                            self.shared.borrow_mut().push_intent(Intent::Settings(has.then_some(active)));
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
                            let (account_id, folder) = {
                                let st = self.shared.borrow();
                                (st.accounts.active_config().email, st.folder)
                            };
                            let mut st = self.shared.borrow_mut();
                            st.push_intent(Intent::Engine(crate::engine::EngineRequest::Fetch { account_id, folder }));
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
                            st.mailbox.toggle_star(id);
                            st.persist_flags(doc, id);
                            st.dirty = true;
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
                                st.mailbox.mark_read(id, false);
                                st.persist_flags(doc, id);
                                st.dirty = true;
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
            Role::Reader => self.draw_reader(scene, t, tr, theme),
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
        MailState::shared(MailState::new_sample(crate::app::avatar_store()))
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
}
