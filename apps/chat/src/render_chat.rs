//! Rendering for the persistent chat surface: workspace rail, channel
//! sidebar, channel header, the grouped message timeline, the composer,
//! and the thread side-panel. All geometry is computed from `self.rect`;
//! every clickable area is recorded into `hits`. Colours come only from
//! the [`Theme`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Brush, Color, Fill, Gradient};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::ColorCompat;

use crate::app::{ChatApp, Hit, AVATAR};
use crate::model::{self, Attachment, ChannelKind, Message, Presence};
use crate::paint::*;

/// Draw a circular initials avatar with an optional presence dot.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_avatar(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    center: Point,
    r: f64,
    initials: &str,
    color_idx: usize,
    status: Option<Presence>,
) {
    let bg = palette_color(theme, color_idx);
    fill_circle(scene, tf, center, r, bg);
    let size = (r * 0.8) as f32;
    let (tw, th) = tr.measure_weighted(initials, size, None, TextWeight::Semibold);
    text(
        scene,
        tr,
        tf,
        initials,
        size,
        Color::WHITE,
        center.x - tw as f64 / 2.0,
        center.y - th as f64 / 2.0,
        TextWeight::Semibold,
        None,
    );
    if let Some(st) = status {
        let c = &theme.tokens.colors;
        let dot = match st {
            Presence::Online => c.semantic.success,
            Presence::Idle => c.semantic.warning,
            Presence::Dnd => c.semantic.danger,
            Presence::Offline => c.neutral[5],
        };
        let dc = Point::new(center.x + r * 0.72, center.y + r * 0.72);
        fill_circle(scene, tf, dc, r * 0.32 + 2.0, c.surfaces.base);
        fill_circle(scene, tf, dc, r * 0.32, dot);
    }
}

fn char_to_byte(s: &str, ci: usize) -> usize {
    s.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(s.len())
}

impl ChatApp {
    // ---------------------------------------------------------------- rail
    pub(crate) fn draw_rail(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let rail = self.rail_rect();
        fill_rect(scene, tf, rail, c.neutral[0]);

        let btn = 48.0;
        let cx = rail.x0 + RAIL_W_CENTER;
        let mut y = rail.y0 + 16.0;
        for ws in &self.workspaces {
            let active = ws.id == self.active_ws;
            let b = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
            // selection pill on the rail's left edge
            if active {
                let pill = Rect::new(rail.x0 + 3.0, y + 8.0, rail.x0 + 7.0, y + btn - 8.0);
                fill_round(scene, tf, pill, 2.0, c.text_primary);
            }
            let radius = if active { 16.0 } else { btn / 2.0 };
            let fill = if active { palette_color(theme, ws.color_idx) } else { c.neutral[2] };
            fill_round(scene, tf, b, radius, fill);
            let (tw, th) = tr.measure_weighted(&ws.glyph, 17.0, None, TextWeight::Bold);
            let col = if active { Color::WHITE } else { c.text_dim };
            text(
                scene, tr, tf, &ws.glyph, 17.0, col,
                b.center().x - tw as f64 / 2.0,
                b.center().y - th as f64 / 2.0,
                TextWeight::Bold, None,
            );
            ChatApp::push_hit(hits, b, Hit::Workspace(ws.id.clone()));
            y += btn + 12.0;
        }

        // add-workspace affordance
        let add = Rect::new(cx - btn / 2.0, y, cx + btn / 2.0, y + btn);
        fill_round(scene, tf, add, btn / 2.0, c.neutral[2]);
        let (aw, ah) = tr.measure_weighted("+", 24.0, None, TextWeight::Regular);
        text(
            scene, tr, tf, "+", 24.0, c.semantic.success,
            add.center().x - aw as f64 / 2.0, add.center().y - ah as f64 / 2.0,
            TextWeight::Regular, None,
        );

        // self avatar pinned to the bottom of the rail
        if let Some(me) = self.user(model::ME) {
            let mc = Point::new(cx, rail.y1 - 34.0);
            draw_avatar(scene, tf, tr, theme, mc, 18.0, &me.initials, me.color_idx, Some(me.status));
        }
    }

    // ------------------------------------------------------------- sidebar
    pub(crate) fn draw_sidebar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let side = self.sidebar_rect();
        fill_rect(scene, tf, side, c.surfaces.raised);

        // workspace title header
        let ws_name = self
            .workspaces
            .iter()
            .find(|w| w.id == self.active_ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let header = Rect::new(side.x0, side.y0, side.x1, side.y0 + 52.0);
        let ws_title = truncate_to(tr, &ws_name, 16.0, side.width() - 60.0);
        label(scene, tr, tf, &ws_title, 16.0, c.text_primary, side.x0 + 16.0, side.y0 + 17.0);
        // a tiny "presence count" pill
        let online = self.users.iter().filter(|u| u.status == Presence::Online).count();
        let pill_txt = format!("{online} online");
        let (pw, _) = tr.measure(&pill_txt, 10.5, None);
        let pill = Rect::new(side.x1 - pw as f64 - 28.0, side.y0 + 16.0, side.x1 - 12.0, side.y0 + 34.0);
        fill_round(scene, tf, pill, 9.0, c.surface_white_10);
        fill_circle(scene, tf, Point::new(pill.x0 + 10.0, pill.center().y), 3.0, c.semantic.success);
        label(scene, tr, tf, &pill_txt, 10.5, c.text_dim, pill.x0 + 17.0, pill.center().y - 7.0);
        hline(scene, tf, side.x0 + 12.0, side.x1 - 12.0, header.y1, c.border_standard);

        let mut y = header.y1 + 10.0;
        for (label_txt, kind) in [("TEXT CHANNELS", ChannelKind::Text), ("VOICE", ChannelKind::Voice)] {
            label(scene, tr, tf, label_txt, 10.5, c.text_dim, side.x0 + 16.0, y);
            y += 22.0;
            for ch in self.ws_channels().into_iter().filter(|c| c.kind == kind) {
                let row = Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + 34.0);
                let is_active = ch.id == self.active_channel && kind == ChannelKind::Text;
                let is_live = self.call_active && self.call_channel == ch.id;
                if is_active {
                    fill_round(scene, tf, row, 8.0, c.surface_white_10);
                    let bar = Rect::new(side.x0, y + 7.0, side.x0 + 4.0, y + 27.0);
                    fill_round(scene, tf, bar, 2.0, c.accent[5]);
                }
                let glyph = ch.kind.glyph();
                let gcol = if is_active { c.accent[6] } else { c.text_dim };
                label(scene, tr, tf, glyph, 14.0, gcol, row.x0 + 10.0, y + 9.0);
                let name_col = if is_active || ch.unread > 0 { c.text_primary } else { c.text_dim };
                let weight = if ch.unread > 0 { TextWeight::Semibold } else { TextWeight::Regular };
                let name = truncate_to(tr, &ch.name, 14.0, row.width() - 80.0);
                text(scene, tr, tf, &name, 14.0, name_col, row.x0 + 32.0, y + 9.0, weight, None);

                if is_live {
                    // live-call dot + "in call"
                    fill_circle(scene, tf, Point::new(row.x1 - 16.0, row.center().y), 4.0, c.semantic.success);
                } else if ch.unread > 0 {
                    let txt = ch.unread.to_string();
                    let (bw, _) = tr.measure(&txt, 11.0, None);
                    let badge = Rect::new(row.x1 - bw as f64 - 22.0, y + 8.0, row.x1 - 8.0, y + 26.0);
                    fill_round(scene, tf, badge, 9.0, c.accent[5]);
                    label(scene, tr, tf, &txt, 11.0, Color::WHITE, badge.x0 + 7.0, badge.center().y - 7.0);
                }
                ChatApp::push_hit(hits, row, Hit::Channel(ch.id.clone()));
                y += 36.0;
            }
            y += 8.0;
        }

        // self identity bar pinned to the sidebar bottom (mute toggle lives here)
        let bar = Rect::new(side.x0, side.y1 - 56.0, side.x1, side.y1);
        fill_rect(scene, tf, bar, c.neutral[0]);
        if let Some(me) = self.user(model::ME) {
            draw_avatar(scene, tf, tr, theme, Point::new(bar.x0 + 28.0, bar.center().y), 16.0, &me.initials, me.color_idx, Some(me.status));
            label(scene, tr, tf, &me.name, 13.0, c.text_primary, bar.x0 + 50.0, bar.center().y - 14.0);
            label(scene, tr, tf, "Available", 11.0, c.text_dim, bar.x0 + 50.0, bar.center().y + 2.0);
        }
        let mute = Rect::new(bar.x1 - 44.0, bar.center().y - 16.0, bar.x1 - 12.0, bar.center().y + 16.0);
        fill_round(scene, tf, mute, 8.0, if self.muted { c.semantic.danger } else { c.surface_white_10 });
        let mglyph = if self.muted { "🔇" } else { "🎤" };
        let (mw, mh) = tr.measure(mglyph, 14.0, None);
        text(scene, tr, tf, mglyph, 14.0, c.text_primary, mute.center().x - mw as f64 / 2.0, mute.center().y - mh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, mute, Hit::CallMute);
    }

    // -------------------------------------------------------------- header
    pub(crate) fn draw_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let h = self.header_rect();
        fill_rect(scene, tf, h, c.surfaces.base);
        hline(scene, tf, h.x0, h.x1, h.y1, c.border_standard);

        let ch = self.channel(&self.active_channel).cloned();
        let (name, topic, glyph) = match &ch {
            Some(ch) => (ch.name.clone(), ch.topic.clone(), ch.kind.glyph()),
            None => (String::new(), String::new(), "#"),
        };
        label(scene, tr, tf, glyph, 18.0, c.text_dim, h.x0 + 18.0, h.center().y - 11.0);
        let nx = h.x0 + 40.0;
        let (nw, _) = text(scene, tr, tf, &name, 17.0, c.text_primary, nx, h.center().y - 11.0, TextWeight::Bold, None);
        // topic after the name, dim, with a separator
        if !topic.is_empty() {
            let sep_x = nx + nw as f64 + 14.0;
            hline(scene, tf, sep_x, sep_x, h.center().y - 9.0, c.border_standard); // tiny tick (no-op visually)
            let tx = sep_x + 4.0;
            let avail = h.x1 - tx - 220.0;
            let topic_t = truncate_to(tr, &topic, 13.0, avail.max(40.0));
            label(scene, tr, tf, &topic_t, 13.0, c.text_dim, tx, h.center().y - 7.0);
        }

        // right-aligned action buttons
        let mut x = h.x1 - 16.0;
        let members = self.users.len();
        // call button (accent when a call is live)
        x -= 92.0;
        let call_btn = Rect::new(x, h.center().y - 16.0, x + 84.0, h.center().y + 16.0);
        let (cbg, ctx) = if self.call_active {
            (c.accent[5], Color::WHITE)
        } else {
            (c.surface_white_10, c.text_primary)
        };
        fill_round(scene, tf, call_btn, 8.0, cbg);
        let call_label = if self.call_active { "● In call" } else { "📹 Call" };
        label(scene, tr, tf, call_label, 12.5, ctx, call_btn.x0 + 12.0, call_btn.center().y - 8.0);
        ChatApp::push_hit(hits, call_btn, Hit::StartCall);

        // members pill
        x -= 60.0;
        let mem = Rect::new(x, h.center().y - 16.0, x + 52.0, h.center().y + 16.0);
        fill_round(scene, tf, mem, 8.0, c.surface_white_5);
        label(scene, tr, tf, &format!("👥 {members}"), 12.5, c.text_dim, mem.x0 + 9.0, mem.center().y - 8.0);
    }

    // ------------------------------------------------------------ messages
    pub(crate) fn draw_messages(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let text_x = area.x0 + 16.0 + AVATAR + 14.0;
        let text_w = (area.x1 - text_x - 18.0).max(80.0) as f32;

        let groups = model::group_messages(&self.messages);

        // Pass 1: measure total height (divider + rows).
        let divider_h = 36.0;
        let mut total = divider_h;
        let mut rows: Vec<RowM> = Vec::new();
        for g in &groups {
            for (gi, &idx) in g.message_indices.iter().enumerate() {
                let m = &self.messages[idx];
                let show_header = gi == 0;
                let rm = self.measure_row(tr, m, show_header, text_w);
                total += rm.height();
                rows.push(rm);
            }
        }

        let start_y = if total > area.height() { area.y1 - total } else { area.y0 };
        let max_start = area.y0;
        let min_start = (area.y1 - total).min(area.y0);
        let mut y = (start_y + self.scroll).clamp(min_start, max_start);

        // date divider
        let dl = self
            .messages
            .first()
            .map(|m| model::day_label(m.ts, crate::doc::NOW))
            .unwrap_or_else(|| "Today".into());
        let (dw, _) = tr.measure(&dl, 11.5, None);
        let dcy = y + divider_h / 2.0;
        hline(scene, tf, area.x0 + 24.0, area.center().x - dw as f64 / 2.0 - 14.0, dcy, c.border_standard);
        hline(scene, tf, area.center().x + dw as f64 / 2.0 + 14.0, area.x1 - 24.0, dcy, c.border_standard);
        label(scene, tr, tf, &dl, 11.5, c.text_dim, area.center().x - dw as f64 / 2.0, dcy - 7.0);
        y += divider_h;

        for rm in &rows {
            self.draw_row(scene, tf, tr, theme, rm, area, text_x, text_w, &mut y, hits);
        }
    }

    fn measure_row(&self, tr: &mut TextRenderer, m: &Message, show_header: bool, text_w: f32) -> RowM {
        let body_h = if m.text.is_empty() {
            0.0
        } else {
            tr.measure(&m.text, 15.0, Some(text_w)).1 as f64
        };
        let att_h = match &m.attachment {
            None => 0.0,
            Some(Attachment::File { .. }) => 70.0,
            Some(Attachment::Image { w, h, .. }) => image_thumb_size(*w, *h, text_w as f64).1 + 24.0,
        };
        let react_h = if m.reactions.is_empty() { 0.0 } else { 32.0 };
        let edited_h = if m.edited { 18.0 } else { 0.0 };
        let thread_h = if model::reply_count(&self.messages, &m.id) > 0 { 46.0 } else { 0.0 };
        RowM { idx_id: m.id.clone(), show_header, body_h, att_h, react_h, edited_h, thread_h }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_row(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rm: &RowM,
        area: Rect,
        text_x: f64,
        text_w: f32,
        y: &mut f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let Some(m) = self.message(&rm.idx_id) else { return };
        let top_gap = if rm.show_header { 14.0 } else { 3.0 };
        *y += top_gap;

        if rm.show_header {
            let u = self.user(&m.author).cloned();
            let (initials, cidx, status) = u
                .as_ref()
                .map(|u| (u.initials.clone(), u.color_idx, Some(u.status)))
                .unwrap_or(("?".into(), 0, None));
            draw_avatar(
                scene, tf, tr, theme,
                Point::new(area.x0 + 16.0 + AVATAR / 2.0, *y + AVATAR / 2.0),
                AVATAR / 2.0, &initials, cidx, status,
            );
            let name = self.user_name(&m.author);
            let (nw, _) = text(scene, tr, tf, &name, 14.5, palette_color(theme, cidx), text_x, *y, TextWeight::Semibold, None);
            label(scene, tr, tf, &model::clock(m.ts), 11.5, c.text_dim, text_x + nw as f64 + 10.0, *y + 3.0);
            *y += 22.0;
        }

        if !m.text.is_empty() {
            text(scene, tr, tf, &m.text, 15.0, c.text_primary, text_x, *y, TextWeight::Regular, Some(text_w));
            *y += rm.body_h + 2.0;
        }

        if m.edited {
            let s = "(edited) · view history";
            let r = Rect::new(text_x, *y, text_x + tr.measure(s, 11.5, None).0 as f64 + 4.0, *y + 16.0);
            label(scene, tr, tf, s, 11.5, c.text_dim, text_x, *y, );
            ChatApp::push_hit(hits, r, Hit::EditHistory { msg: m.id.clone(), anchor: Point::new(text_x, *y + 18.0) });
            *y += rm.edited_h;
        }

        if let Some(att) = &m.attachment {
            self.draw_attachment(scene, tf, tr, theme, att, text_x, *y, text_w);
            *y += rm.att_h;
        }

        if !m.reactions.is_empty() {
            self.draw_reactions(scene, tf, tr, theme, m, text_x, *y, hits);
            *y += rm.react_h;
        } else {
            // empty reaction strip still gets an add affordance on header rows
        }

        if rm.thread_h > 0.0 {
            self.draw_thread_affordance(scene, tf, tr, theme, m, text_x, *y, text_w, hits);
            *y += rm.thread_h;
        }
        *y += 4.0;
    }

    fn draw_reactions(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        text_x: f64,
        y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let mut x = text_x;
        let h = 26.0;
        for r in &m.reactions {
            let mine = r.reacted_by(model::ME);
            let lbl = format!("{}  {}", r.emoji, r.count());
            let (lw, _) = tr.measure(&lbl, 12.5, None);
            let chip = Rect::new(x, y, x + lw as f64 + 20.0, y + h);
            let bg = if mine { c.accent[5].with_alpha_factor(0.22) } else { c.surface_white_5 };
            fill_round(scene, tf, chip, 13.0, bg);
            if mine {
                stroke_round(scene, tf, chip, 13.0, c.accent[5], 1.0);
            }
            label(scene, tr, tf, &lbl, 12.5, if mine { c.accent[7] } else { c.text_primary }, chip.x0 + 10.0, chip.center().y - 8.0);
            ChatApp::push_hit(hits, chip, Hit::Reaction { msg: m.id.clone(), emoji: r.emoji.clone() });
            x = chip.x1 + 6.0;
        }
        // add-reaction affordance
        let add = Rect::new(x, y, x + 34.0, y + h);
        fill_round(scene, tf, add, 13.0, c.surface_white_5);
        let (aw, ah) = tr.measure("☺", 14.0, None);
        text(scene, tr, tf, "☺", 14.0, c.text_dim, add.center().x - aw as f64 / 2.0, add.center().y - ah as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, add, Hit::AddReaction { msg: m.id.clone(), anchor: Point::new(add.x0, y + h + 4.0) });
    }

    fn draw_thread_affordance(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        text_x: f64,
        y: f64,
        text_w: f32,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let n = model::reply_count(&self.messages, &m.id);
        let parts = model::thread_participants(&self.messages, &m.id);
        let bar = Rect::new(text_x, y, text_x + (text_w as f64).min(360.0), y + 38.0);
        let open = self.thread_open.as_deref() == Some(m.id.as_str());
        fill_round(scene, tf, bar, 10.0, if open { c.accent[5].with_alpha_factor(0.14) } else { c.surface_white_5 });
        if open {
            stroke_round(scene, tf, bar, 10.0, c.accent[5].with_alpha_factor(0.5), 1.0);
        }
        let mut ax = bar.x0 + 14.0;
        for pid in parts.iter().take(3) {
            if let Some(u) = self.user(pid) {
                fill_circle(scene, tf, Point::new(ax + 11.0, bar.center().y), 13.0, c.surfaces.raised);
                draw_avatar(scene, tf, tr, theme, Point::new(ax + 11.0, bar.center().y), 11.0, &u.initials, u.color_idx, None);
            }
            ax += 17.0;
        }
        let txt = format!("{n} {}", if n == 1 { "reply" } else { "replies" });
        label(scene, tr, tf, &txt, 13.0, c.accent[6], ax + 8.0, bar.center().y - 8.0);
        let last_ts = self
            .messages
            .iter()
            .filter(|r| r.parent.as_deref() == Some(m.id.as_str()))
            .map(|r| r.ts)
            .max();
        if let Some(ts) = last_ts {
            let s = format!("last reply {}", model::clock(ts));
            let sx = bar.x1 - tr.measure(&s, 11.0, None).0 as f64 - 14.0;
            label(scene, tr, tf, &s, 11.0, c.text_dim, sx, bar.center().y - 6.0);
        }
        ChatApp::push_hit(hits, bar, Hit::OpenThread(m.id.clone()));
    }

    pub(crate) fn draw_attachment(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        att: &Attachment,
        x: f64,
        y: f64,
        text_w: f32,
    ) {
        let c = &theme.tokens.colors;
        match att {
            Attachment::Image { name, w, h, tint, .. } => {
                let (tw, th) = image_thumb_size(*w, *h, text_w as f64);
                let rect = Rect::new(x, y, x + tw, y + th);
                // gradient stand-in for the FUSE-hydrated image
                let g = Gradient::new_linear(Point::new(rect.x0, rect.y0), Point::new(rect.x1, rect.y1))
                    .with_stops([
                        (0.0_f32, palette_color(theme, *tint)),
                        (1.0_f32, palette_color(theme, tint + 2)),
                    ]);
                let shape = vello::kurbo::RoundedRect::from_rect(rect, 12.0);
                scene.fill(Fill::NonZero, tf, &Brush::Gradient(g), None, &shape);
                // soft inner vignette + a framing stroke
                fill_round(scene, tf, Rect::new(rect.x0, rect.y1 - 30.0, rect.x1, rect.y1), 12.0, Color::from_rgba8(0, 0, 0, 90));
                stroke_round(scene, tf, rect, 12.0, c.border_white_10, 1.0);
                label(scene, tr, tf, &format!("🖼  {name}"), 12.0, Color::WHITE, rect.x0 + 12.0, rect.y1 - 22.0);
                let dim = format!("{w}×{h}");
                label(scene, tr, tf, &dim, 11.0, c.text_dim, rect.x0, rect.y1 + 4.0);
            }
            Attachment::File { name, size, .. } => {
                let chip = Rect::new(x, y, x + (text_w as f64).min(360.0), y + 58.0);
                fill_round(scene, tf, chip, 10.0, c.surfaces.raised);
                stroke_round(scene, tf, chip, 10.0, c.border_standard, 1.0);
                let icon = Rect::new(chip.x0 + 10.0, chip.y0 + 11.0, chip.x0 + 46.0, chip.y1 - 11.0);
                fill_round(scene, tf, icon, 8.0, c.accent[5]);
                let (iw, ih) = tr.measure("📄", 16.0, None);
                text(scene, tr, tf, "📄", 16.0, Color::WHITE, icon.center().x - iw as f64 / 2.0, icon.center().y - ih as f64 / 2.0, TextWeight::Regular, None);
                let fname = truncate_to(tr, name, 13.5, chip.width() - 70.0);
                label(scene, tr, tf, &fname, 13.5, c.text_primary, chip.x0 + 56.0, chip.center().y - 14.0);
                label(scene, tr, tf, &format!("{} · shared via /var/workspace", human_size(*size)), 11.0, c.text_dim, chip.x0 + 56.0, chip.center().y + 2.0);
            }
        }
    }

    // ------------------------------------------------------------ composer
    pub(crate) fn draw_composer(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let rect = self.composer_rect(tr);
        let ch_name = self.channel(&self.active_channel).map(|c| c.name.clone()).unwrap_or_default();
        let placeholder = format!("Message #{ch_name}");
        let focused = matches!(self.focus, crate::app::Focus::Main);
        let caret = if focused { Some(self.caret) } else { None };
        self.draw_input_box(
            scene, tf, tr, theme, rect, &self.draft_main, &placeholder, caret, focused,
            Hit::FocusMain, Hit::Send, Hit::ComposerEmoji(Point::new(rect.x1 - 220.0, rect.y0 - 8.0)), true, hits,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw_input_box(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        value: &str,
        placeholder: &str,
        caret: Option<usize>,
        focused: bool,
        focus_hit: Hit,
        send_hit: Hit,
        emoji_hit: Hit,
        attach: bool,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        fill_round(scene, tf, rect, 14.0, c.neutral[2]);
        stroke_round(scene, tf, rect, 14.0, if focused { c.border_focus } else { c.border_standard }, if focused { 1.6 } else { 1.0 });
        ChatApp::push_hit(hits, rect, focus_hit);

        let mut tx = rect.x0 + 16.0;
        if attach {
            let plus = Rect::new(rect.x0 + 10.0, rect.center().y - 14.0, rect.x0 + 38.0, rect.center().y + 14.0);
            fill_round(scene, tf, plus, 8.0, c.surface_white_10);
            let (pw, ph) = tr.measure("+", 20.0, None);
            text(scene, tr, tf, "+", 20.0, c.text_dim, plus.center().x - pw as f64 / 2.0, plus.center().y - ph as f64 / 2.0, TextWeight::Regular, None);
            ChatApp::push_hit(hits, plus, Hit::Attach);
            tx = plus.x1 + 12.0;
        }

        // right cluster: emoji + send
        let send = Rect::new(rect.x1 - 56.0, rect.center().y - 16.0, rect.x1 - 12.0, rect.center().y + 16.0);
        let send_on = !value.trim().is_empty();
        fill_round(scene, tf, send, 9.0, if send_on { c.accent[5] } else { c.surface_white_5 });
        let (sw, sh) = tr.measure("➤", 16.0, None);
        text(scene, tr, tf, "➤", 16.0, if send_on { Color::WHITE } else { c.text_dim }, send.center().x - sw as f64 / 2.0, send.center().y - sh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, send, Hit::Send);

        let emoji = Rect::new(send.x0 - 40.0, rect.center().y - 16.0, send.x0 - 8.0, rect.center().y + 16.0);
        let (ew, eh) = tr.measure("🙂", 16.0, None);
        text(scene, tr, tf, "🙂", 16.0, c.text_dim, emoji.center().x - ew as f64 / 2.0, emoji.center().y - eh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, emoji, Hit::ComposerEmoji(Point::new(emoji.x0 - 140.0, rect.y0 - 8.0)));
        let _ = emoji_hit; // anchor encoded above

        // text / placeholder
        let text_top = rect.y0 + 14.0;
        let avail = (emoji.x0 - tx - 12.0).max(40.0) as f32;
        if value.is_empty() {
            label(scene, tr, tf, placeholder, 15.0, c.text_dim, tx, text_top);
        } else {
            text(scene, tr, tf, value, 15.0, c.text_primary, tx, text_top, TextWeight::Regular, Some(avail));
        }
        if let Some(ci) = caret {
            let prefix = &value[..char_to_byte(value, ci)];
            let cx = tx + tr.measure(prefix, 15.0, None).0 as f64;
            let cx = cx.min(tx + avail as f64);
            let car = Rect::new(cx, text_top, cx + 1.6, text_top + 19.0);
            fill_rect(scene, tf, car, c.border_focus);
        }
        let _ = send_hit;
    }

    // ---------------------------------------------------------------- thread
    pub(crate) fn draw_thread(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let Some(parent_id) = self.thread_open.clone() else { return };
        let panel = self.thread_rect();
        fill_rect(scene, tf, panel, c.surfaces.raised);
        hline(scene, tf, panel.x0, panel.x0, panel.y0, c.border_standard);
        scene.stroke(&vello::kurbo::Stroke::new(1.0), tf, c.border_standard, None, &vello::kurbo::Line::new(Point::new(panel.x0, panel.y0), Point::new(panel.x0, panel.y1)));

        // header
        let head = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 52.0);
        label(scene, tr, tf, "Thread", 15.0, c.text_primary, panel.x0 + 16.0, panel.y0 + 12.0);
        let chn = self.channel(&self.active_channel).map(|c| format!("#{}", c.name)).unwrap_or_default();
        label(scene, tr, tf, &chn, 11.5, c.text_dim, panel.x0 + 16.0, panel.y0 + 31.0);
        let close = Rect::new(panel.x1 - 40.0, panel.y0 + 12.0, panel.x1 - 12.0, panel.y0 + 40.0);
        fill_round(scene, tf, close, 8.0, c.surface_white_10);
        let (xw, xh) = tr.measure("✕", 14.0, None);
        text(scene, tr, tf, "✕", 14.0, c.text_primary, close.center().x - xw as f64 / 2.0, close.center().y - xh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, close, Hit::CloseThread);
        hline(scene, tf, panel.x0 + 12.0, panel.x1 - 12.0, head.y1, c.border_standard);

        let body = Rect::new(panel.x0, head.y1, panel.x1, panel.y1 - 70.0);
        push_clip(scene, tf, body);
        let tx = panel.x0 + 16.0 + 32.0 + 10.0;
        let tw = (panel.x1 - tx - 14.0).max(60.0) as f32;
        let mut y = body.y0 + 12.0;

        // parent message
        if let Some(parent) = self.message(&parent_id).cloned() {
            y = self.draw_thread_message(scene, tf, tr, theme, &parent, panel.x0 + 16.0, tx, tw, y, hits);
            // replies divider
            let n = model::reply_count(&self.messages, &parent_id);
            let s = format!("{n} {}", if n == 1 { "reply" } else { "replies" });
            let (sw, _) = tr.measure(&s, 11.5, None);
            let dcy = y + 14.0;
            hline(scene, tf, panel.x0 + 16.0, panel.center().x - sw as f64 / 2.0 - 10.0, dcy, c.border_standard);
            hline(scene, tf, panel.center().x + sw as f64 / 2.0 + 10.0, panel.x1 - 16.0, dcy, c.border_standard);
            label(scene, tr, tf, &s, 11.5, c.text_dim, panel.center().x - sw as f64 / 2.0, dcy - 7.0);
            y += 30.0;
        }
        let replies: Vec<Message> = self
            .messages
            .iter()
            .filter(|m| m.parent.as_deref() == Some(parent_id.as_str()))
            .cloned()
            .collect();
        for r in &replies {
            y = self.draw_thread_message(scene, tf, tr, theme, r, panel.x0 + 16.0, tx, tw, y, hits);
        }
        scene.pop_layer();

        // thread composer
        let comp = Rect::new(panel.x0 + 12.0, panel.y1 - 60.0, panel.x1 - 12.0, panel.y1 - 12.0);
        let focused = matches!(self.focus, crate::app::Focus::Thread);
        let caret = if focused { Some(self.caret) } else { None };
        self.draw_input_box(
            scene, tf, tr, theme, comp, &self.draft_thread, "Reply…", caret, focused,
            Hit::FocusThread, Hit::Send, Hit::ComposerEmoji(Point::new(comp.x0, comp.y0 - 200.0)), false, hits,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_thread_message(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        m: &Message,
        avx: f64,
        tx: f64,
        tw: f32,
        y: f64,
        _hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let mut yy = y;
        if let Some(u) = self.user(&m.author) {
            draw_avatar(scene, tf, tr, theme, Point::new(avx + 16.0, yy + 14.0), 14.0, &u.initials, u.color_idx, Some(u.status));
        }
        let (nw, _) = text(scene, tr, tf, &self.user_name(&m.author), 13.5, palette_color(theme, self.user(&m.author).map(|u| u.color_idx).unwrap_or(0)), tx, yy, TextWeight::Semibold, None);
        label(scene, tr, tf, &model::clock(m.ts), 11.0, c.text_dim, tx + nw as f64 + 8.0, yy + 2.0);
        yy += 19.0;
        if !m.text.is_empty() {
            let h = tr.measure(&m.text, 14.0, Some(tw)).1 as f64;
            text(scene, tr, tf, &m.text, 14.0, c.text_primary, tx, yy, TextWeight::Regular, Some(tw));
            yy += h + 4.0;
        }
        if let Some(att) = &m.attachment {
            self.draw_attachment(scene, tf, tr, theme, att, tx, yy, tw);
            yy += match att {
                Attachment::File { .. } => 66.0,
                Attachment::Image { w, h, .. } => image_thumb_size(*w, *h, tw as f64).1 + 24.0,
            };
        }
        if !m.reactions.is_empty() {
            let mut x = tx;
            for r in &m.reactions {
                let lbl = format!("{} {}", r.emoji, r.count());
                let (lw, _) = tr.measure(&lbl, 11.5, None);
                let chip = Rect::new(x, yy, x + lw as f64 + 16.0, yy + 22.0);
                fill_round(scene, tf, chip, 11.0, c.surface_white_5);
                label(scene, tr, tf, &lbl, 11.5, c.text_primary, chip.x0 + 8.0, chip.center().y - 7.0);
                x = chip.x1 + 5.0;
            }
            yy += 28.0;
        }
        yy + 10.0
    }
}

/// Per-row measured heights for the timeline layout.
struct RowM {
    idx_id: String,
    show_header: bool,
    body_h: f64,
    att_h: f64,
    react_h: f64,
    edited_h: f64,
    thread_h: f64,
}
impl RowM {
    fn height(&self) -> f64 {
        let header = if self.show_header { 22.0 } else { 0.0 };
        let top_gap = if self.show_header { 14.0 } else { 3.0 };
        let body = if self.body_h > 0.0 { self.body_h + 2.0 } else { 0.0 };
        top_gap + header + body + self.edited_h + self.att_h + self.react_h + self.thread_h + 4.0
    }
}

/// Contain-fit an image into the available text width, capped at a tidy
/// preview size.
fn image_thumb_size(w: u32, h: u32, max_w: f64) -> (f64, f64) {
    let maxw = max_w.min(380.0);
    let maxh = 210.0;
    let (w, h) = (w.max(1) as f64, h.max(1) as f64);
    let mut tw = maxw;
    let mut th = tw * h / w;
    if th > maxh {
        th = maxh;
        tw = th * w / h;
    }
    (tw, th)
}

fn human_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} B")
    } else if b < K * K {
        format!("{:.1} KB", b / K)
    } else {
        format!("{:.1} MB", b / (K * K))
    }
}

/// Horizontal centre of the workspace rail (column of square buttons).
const RAIL_W_CENTER: f64 = crate::app::RAIL_W / 2.0;
