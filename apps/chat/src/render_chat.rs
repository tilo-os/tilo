//! Rendering for the persistent chat surface: workspace rail, channel
//! sidebar, channel header, the grouped message timeline, the composer,
//! and the thread side-panel. All geometry is computed from `self.rect`;
//! every clickable area is recorded into `hits`. Colours come only from
//! the [`Theme`].

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::ColorCompat;

use crate::app::{ChatApp, Hit, MenuTarget, AVATAR};
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
        // The single user avatar lives in the sidebar identity bar (drawn in
        // `draw_sidebar`), not here — the rail foot intentionally has none.
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

        // "+ category" affordance below the workspace title.
        let add_cat = Rect::new(side.x0 + 12.0, header.y1 + 6.0, side.x0 + 118.0, header.y1 + 28.0);
        fill_round(scene, tf, add_cat, 7.0, c.surface_white_5);
        label(scene, tr, tf, "＋ Category", 11.0, c.text_dim, add_cat.x0 + 9.0, add_cat.center().y - 7.0);
        ChatApp::push_hit(hits, add_cat, Hit::AddCategory(self.active_ws.clone()));

        // The channel tree: an uncategorised group first, then each category
        // with a collapse caret, a "+" to add a channel, and its channels.
        let mut y = header.y1 + 38.0;
        let body_bottom = side.y1 - 60.0;

        // Uncategorised channels (category id "").
        y = self.draw_channel_group(scene, tf, tr, theme, side, None, y, hits);

        for cat in self.ws_categories() {
            if y > body_bottom {
                break;
            }
            // Category header row: caret + name + "+".
            let hrow = Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + 24.0);
            let caret = if cat.collapsed { "▸" } else { "▾" };
            label(scene, tr, tf, caret, 10.0, c.text_dim, hrow.x0 + 4.0, y + 5.0);
            // Inline rename for a category, else its name.
            let renaming_cat = matches!(&self.rename, Some((MenuTarget::Category(id), _)) if id == &cat.id);
            if renaming_cat {
                if let Some((_, txt)) = &self.rename {
                    self.draw_inline_rename(scene, tf, tr, theme, hrow.x0 + 18.0, y + 4.0, side.x1 - 60.0, txt, hits);
                }
            } else {
                let name = truncate_to(tr, &cat.name.to_uppercase(), 10.5, hrow.width() - 60.0);
                text(scene, tr, tf, &name, 10.5, c.text_dim, hrow.x0 + 18.0, y + 5.0, TextWeight::Semibold, None);
            }
            // "+" add-channel button.
            let plus = Rect::new(hrow.x1 - 22.0, y + 2.0, hrow.x1 - 4.0, y + 20.0);
            label(scene, tr, tf, "+", 16.0, c.text_dim, plus.x0 + 4.0, y + 1.0);
            ChatApp::push_hit(hits, plus, Hit::AddChannel { workspace: self.active_ws.clone(), category: cat.id.clone() });
            // Caret + name toggles collapse; right-click opens the menu.
            let toggle = Rect::new(hrow.x0, y, plus.x0, y + 24.0);
            ChatApp::push_hit(hits, toggle, Hit::CategoryToggle(cat.id.clone()));
            ChatApp::push_hit(hits, hrow, Hit::ContextMenu { target: MenuTarget::Category(cat.id.clone()), anchor: Point::new(hrow.x0 + 40.0, y + 22.0) });
            y += 26.0;

            if !cat.collapsed {
                y = self.draw_channel_group(scene, tf, tr, theme, side, Some(&cat.id), y, hits);
            }
            y += 6.0;
        }

        // self identity bar pinned to the sidebar bottom — the SINGLE user
        // avatar lives here (mute + deafen toggles to its right).
        let bar = Rect::new(side.x0, side.y1 - 56.0, side.x1, side.y1);
        fill_rect(scene, tf, bar, c.neutral[0]);
        if let Some(me) = self.user(model::ME) {
            draw_avatar(scene, tf, tr, theme, Point::new(bar.x0 + 28.0, bar.center().y), 16.0, &me.initials, me.color_idx, Some(me.status));
            label(scene, tr, tf, &me.name, 13.0, c.text_primary, bar.x0 + 50.0, bar.center().y - 14.0);
            let sub = if self.call.active { "In voice" } else { "Available" };
            label(scene, tr, tf, sub, 11.0, c.text_dim, bar.x0 + 50.0, bar.center().y + 2.0);
        }
        // deafen + mute toggles.
        let deaf = Rect::new(bar.x1 - 80.0, bar.center().y - 16.0, bar.x1 - 48.0, bar.center().y + 16.0);
        fill_round(scene, tf, deaf, 8.0, if self.call.deafened { c.semantic.danger } else { c.surface_white_10 });
        let dglyph = if self.call.deafened { "🔕" } else { "🎧" };
        let (dw, dh) = tr.measure(dglyph, 14.0, None);
        text(scene, tr, tf, dglyph, 14.0, c.text_primary, deaf.center().x - dw as f64 / 2.0, deaf.center().y - dh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, deaf, Hit::CallDeafen);

        let mute = Rect::new(bar.x1 - 44.0, bar.center().y - 16.0, bar.x1 - 12.0, bar.center().y + 16.0);
        fill_round(scene, tf, mute, 8.0, if self.call.muted { c.semantic.danger } else { c.surface_white_10 });
        let mglyph = if self.call.muted { "🔇" } else { "🎤" };
        let (mw, mh) = tr.measure(mglyph, 14.0, None);
        text(scene, tr, tf, mglyph, 14.0, c.text_primary, mute.center().x - mw as f64 / 2.0, mute.center().y - mh as f64 / 2.0, TextWeight::Regular, None);
        ChatApp::push_hit(hits, mute, Hit::CallMute);
    }

    /// Draw the channels of one group (a category id, or `None` for the
    /// uncategorised group). Returns the new cursor `y`. Records per-row
    /// click + right-click context-menu hits, applies the active row swipe
    /// offset, and renders voice-channel presence (members + speaking) under
    /// each voice channel — Discord-style.
    #[allow(clippy::too_many_arguments)]
    fn draw_channel_group(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        side: Rect,
        category: Option<&str>,
        mut y: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) -> f64 {
        let c = &theme.tokens.colors;
        let cat_key = category.unwrap_or("");
        let channels = self.category_channels(cat_key);
        // Index for swipe parity: position among all ws_channels.
        let ws_channels = self.ws_channels();
        for ch in channels {
            // Swipe offset: shift this row left while it's being swiped, and
            // reveal a red delete affordance behind it.
            let swipe_dx = self
                .swipe
                .filter(|s| ws_channels.get(s.channel_index).map(|cc| cc.id.as_str()) == Some(ch.id.as_str()))
                .map(|s| s.dx())
                .unwrap_or(0.0);
            let row = Rect::new(side.x0 + 8.0 - swipe_dx, y, side.x1 - 8.0 - swipe_dx, y + 32.0);
            if swipe_dx > 1.0 {
                let trash = Rect::new(side.x1 - 8.0 - swipe_dx + 6.0, y, side.x1 - 8.0, y + 32.0);
                fill_round(scene, tf, Rect::new(side.x0 + 8.0, y, side.x1 - 8.0, y + 32.0), 8.0, c.semantic.danger.with_alpha_factor(0.9));
                let _ = trash;
                label(scene, tr, tf, "🗑", 14.0, Color::WHITE, side.x1 - 30.0, y + 8.0);
            }
            let is_active = ch.id == self.active_channel && ch.kind == ChannelKind::Text;
            let is_live = self.call.active && self.call.channel == ch.id;
            if is_active {
                fill_round(scene, tf, row, 8.0, c.surface_white_10);
                let bar = Rect::new(side.x0, y + 6.0, side.x0 + 4.0, y + 26.0);
                fill_round(scene, tf, bar, 2.0, c.accent[5]);
            }
            let glyph = ch.kind.glyph();
            let gcol = if is_active { c.accent[6] } else { c.text_dim };
            label(scene, tr, tf, glyph, 14.0, gcol, row.x0 + 10.0, y + 8.0);

            // Inline rename for a channel, else its name.
            let renaming = matches!(&self.rename, Some((MenuTarget::Channel(id), _)) if id == &ch.id);
            if renaming {
                if let Some((_, txt)) = &self.rename {
                    self.draw_inline_rename(scene, tf, tr, theme, row.x0 + 32.0, y + 6.0, row.width() - 80.0, txt, hits);
                }
            } else {
                let name_col = if is_active || ch.unread > 0 { c.text_primary } else { c.text_dim };
                let weight = if ch.unread > 0 { TextWeight::Semibold } else { TextWeight::Regular };
                let name = truncate_to(tr, &ch.name, 14.0, row.width() - 80.0);
                text(scene, tr, tf, &name, 14.0, name_col, row.x0 + 32.0, y + 8.0, weight, None);
            }

            if is_live {
                fill_circle(scene, tf, Point::new(row.x1 - 16.0, row.center().y), 4.0, c.semantic.success);
            } else if ch.unread > 0 {
                let txt = ch.unread.to_string();
                let (bw, _) = tr.measure(&txt, 11.0, None);
                let badge = Rect::new(row.x1 - bw as f64 - 22.0, y + 7.0, row.x1 - 8.0, y + 25.0);
                fill_round(scene, tf, badge, 9.0, c.accent[5]);
                label(scene, tr, tf, &txt, 11.0, Color::WHITE, badge.x0 + 7.0, badge.center().y - 7.0);
            }
            // Click joins/opens; right-click opens the row context menu.
            let click_hit = if ch.kind == ChannelKind::Voice { Hit::VoiceChannel(ch.id.clone()) } else { Hit::Channel(ch.id.clone()) };
            ChatApp::push_hit(hits, row, click_hit);
            ChatApp::push_hit(hits, row, Hit::ContextMenu { target: MenuTarget::Channel(ch.id.clone()), anchor: Point::new(row.x0 + 40.0, y + 28.0) });
            y += 34.0;

            // Voice presence: list connected members under a voice channel.
            if ch.kind == ChannelKind::Voice {
                for m in self.voice_in(&ch.id) {
                    if let Some(u) = self.user(&m.user) {
                        let mrow = Rect::new(side.x0 + 36.0, y, side.x1 - 8.0, y + 24.0);
                        let r = 9.0;
                        let cc = Point::new(mrow.x0 + r + 2.0, mrow.center().y);
                        if m.talking && !m.muted {
                            stroke_circle(scene, tf, cc, r + 2.5, c.semantic.success, 2.0);
                        }
                        draw_avatar(scene, tf, tr, theme, cc, r, &u.initials, u.color_idx, None);
                        let nm = truncate_to(tr, &u.name, 12.0, mrow.width() - 60.0);
                        let ncol = if m.talking && !m.muted { c.text_primary } else { c.text_dim };
                        label(scene, tr, tf, &nm, 12.0, ncol, mrow.x0 + 2.0 * r + 8.0, mrow.center().y - 7.0);
                        // mic / audio-only state glyphs on the right.
                        if m.muted {
                            label(scene, tr, tf, "🔇", 11.0, c.semantic.danger, mrow.x1 - 18.0, mrow.center().y - 7.0);
                        } else if m.audio_only {
                            label(scene, tr, tf, "🎧", 11.0, c.text_dim, mrow.x1 - 18.0, mrow.center().y - 7.0);
                        }
                        y += 26.0;
                    }
                }
            }
        }
        y
    }

    /// Draw the inline rename editor field (a bordered box + the working
    /// text + a caret), recording a confirm hit on it.
    #[allow(clippy::too_many_arguments)]
    fn draw_inline_rename(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        x: f64,
        y: f64,
        w: f64,
        value: &str,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let box_r = Rect::new(x - 4.0, y - 3.0, x + w, y + 19.0);
        fill_round(scene, tf, box_r, 5.0, c.neutral[2]);
        stroke_round(scene, tf, box_r, 5.0, c.border_focus, 1.4);
        let (tw, _) = text(scene, tr, tf, value, 13.0, c.text_primary, x, y, TextWeight::Regular, Some((w - 8.0) as f32));
        let caret = Rect::new(x + tw as f64 + 1.0, y, x + tw as f64 + 2.4, y + 16.0);
        fill_rect(scene, tf, caret, c.border_focus);
        ChatApp::push_hit(hits, box_r, Hit::RenameConfirm);
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

        // When the left flap is hidden/overlay, leave room for the floating
        // hamburger that `draw_flap_toggles` paints at the header's left edge.
        let left_inset = if self.left_flap_open() && self.left_mode == crate::responsive::FlapMode::Docked {
            18.0
        } else {
            54.0
        };

        let ch = self.channel(&self.active_channel).cloned();
        let (name, topic, glyph) = match &ch {
            Some(ch) => (ch.name.clone(), ch.topic.clone(), ch.kind.glyph()),
            None => (String::new(), String::new(), "#"),
        };

        // Right-aligned action buttons are placed first so the title + topic
        // know exactly where they must stop — they truncate before the button
        // cluster and can never overlap it, even on a very narrow header.
        let members = self.users.len();
        let call_w = 84.0;
        let mem_w = 52.0;
        let gap = 8.0;
        let cy = h.center().y;
        // call button (accent when a call is live).
        let call_x0 = h.x1 - 16.0 - call_w;
        let call_btn = Rect::new(call_x0, cy - 16.0, call_x0 + call_w, cy + 16.0);
        let (cbg, ctx) = if self.call.active {
            (c.accent[5], Color::WHITE)
        } else {
            (c.surface_white_10, c.text_primary)
        };
        fill_round(scene, tf, call_btn, 8.0, cbg);
        let call_label = if self.call.active { "● In call" } else { "📹 Call" };
        label(scene, tr, tf, call_label, 12.5, ctx, call_btn.x0 + 12.0, call_btn.center().y - 8.0);
        ChatApp::push_hit(hits, call_btn, Hit::StartCall);

        // members pill — toggles the right flap (members / thread).
        let mem_x0 = call_x0 - gap - mem_w;
        let mem = Rect::new(mem_x0, cy - 16.0, mem_x0 + mem_w, cy + 16.0);
        let mem_on = self.right_flap_open();
        fill_round(scene, tf, mem, 8.0, if mem_on { c.accent[5].with_alpha_factor(0.22) } else { c.surface_white_5 });
        label(scene, tr, tf, &format!("👥 {members}"), 12.5, if mem_on { c.accent[7] } else { c.text_dim }, mem.x0 + 9.0, mem.center().y - 8.0);
        ChatApp::push_hit(hits, mem, Hit::ToggleRightFlap);

        // Title + topic occupy the space left of the button cluster; both
        // truncate to that width so nothing runs under the buttons.
        let cluster_left = mem_x0;
        label(scene, tr, tf, glyph, 18.0, c.text_dim, h.x0 + left_inset, cy - 11.0);
        let nx = h.x0 + left_inset + 22.0;
        let name_max = (cluster_left - 12.0 - nx).max(0.0);
        let name_t = truncate_to(tr, &name, 17.0, name_max);
        let (nw, _) = text(scene, tr, tf, &name_t, 17.0, c.text_primary, nx, cy - 11.0, TextWeight::Bold, None);
        if !topic.is_empty() {
            let tx = nx + nw as f64 + 18.0;
            let avail = cluster_left - 12.0 - tx;
            if avail >= 48.0 {
                let topic_t = truncate_to(tr, &topic, 13.0, avail);
                label(scene, tr, tf, &topic_t, 13.0, c.text_dim, tx, cy - 7.0);
            }
        }
    }

    /// Floating flap toggles: a hamburger at the header's left edge when the
    /// left flap is hidden/overlay, summoning the channel sidebar; the
    /// members pill in `draw_header` toggles the right flap.
    pub(crate) fn draw_flap_toggles(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let docked_open = self.left_flap_open() && self.left_mode == crate::responsive::FlapMode::Docked;
        if !docked_open {
            let h = self.header_rect();
            let btn = Rect::new(h.x0 + 12.0, h.center().y - 16.0, h.x0 + 44.0, h.center().y + 16.0);
            fill_round(scene, tf, btn, 8.0, c.surface_white_10);
            label(scene, tr, tf, "☰", 16.0, c.text_primary, btn.x0 + 8.0, btn.center().y - 9.0);
            ChatApp::push_hit(hits, btn, Hit::ToggleLeftFlap);
        }
    }

    /// A right-click context menu over a channel / category / workspace.
    /// Lays its items below the anchor and records a per-item action hit
    /// plus a full-window scrim hit to dismiss.
    pub(crate) fn draw_context_menu(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        menu: &crate::app::ContextMenu,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        // Dismiss scrim (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);

        let w = 188.0;
        let item_h = 32.0;
        let h = 10.0 + menu.items.len() as f64 * item_h + 6.0;
        let x0 = menu.anchor.x.clamp(self.rect.x0 + 6.0, self.rect.x1 - w - 6.0);
        let y0 = menu.anchor.y.clamp(self.rect.y0 + 6.0, (self.rect.y1 - h - 6.0).max(self.rect.y0 + 6.0));
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        fill_round(scene, tf, panel.inflate(5.0, 5.0), 16.0, Color::from_rgba8(0, 0, 0, 80));
        fill_round(scene, tf, panel, 12.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 12.0, c.border_white_10, 1.0);

        let mut y = panel.y0 + 6.0;
        for item in &menu.items {
            let r = Rect::new(panel.x0 + 5.0, y, panel.x1 - 5.0, y + item_h - 3.0);
            let hot = r.contains(self.last_mouse);
            if hot {
                fill_round(scene, tf, r, 8.0, c.surface_white_10);
            }
            let col = if item.is_destructive() { c.semantic.danger } else { c.text_primary };
            label(scene, tr, tf, item.label(), 13.0, col, r.x0 + 12.0, r.center().y - 8.0);
            ChatApp::push_hit(hits, r, Hit::MenuAction { target: menu.target.clone(), action: *item });
            y += item_h;
        }
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

        // Empty channel: a quiet start-of-conversation marker rather than a
        // blank void, so a fresh channel reads as ready, not broken.
        if self.messages.is_empty() {
            let ch_name = self.channel(&self.active_channel).map(|c| c.name.clone()).unwrap_or_default();
            let cx = area.center().x;
            let cy = area.y0 + (area.height() * 0.5).min(220.0);
            fill_circle(scene, tf, Point::new(cx, cy), 30.0, c.surface_white_5);
            let (gw, gh) = tr.measure_weighted("#", 30.0, None, TextWeight::Bold);
            text(scene, tr, tf, "#", 30.0, c.text_dim, cx - gw as f64 / 2.0, cy - gh as f64 / 2.0, TextWeight::Bold, None);
            let title = format!("#{ch_name}");
            let (tw, _) = tr.measure_weighted(&title, 20.0, None, TextWeight::Bold);
            text(scene, tr, tf, &title, 20.0, c.text_primary, cx - tw as f64 / 2.0, cy + 46.0, TextWeight::Bold, None);
            let sub = format!("This is the start of the {title} channel.");
            let (sw, _) = tr.measure(&sub, 13.0, None);
            label(scene, tr, tf, &sub, 13.0, c.text_dim, cx - sw as f64 / 2.0, cy + 76.0);
            return;
        }

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
            Attachment::Image { name, path, w, h } => {
                let (tw, th) = image_thumb_size(*w, *h, text_w as f64);
                let rect = Rect::new(x, y, x + tw, y + th);
                // Real decoded bytes from the shared background image cache
                // (projected from the FUSE workspace). `get` schedules the
                // load the first time a path is seen and returns the decoded
                // image on a later frame; until then we show a neutral tile.
                if let Some(img) = self.images.get(path) {
                    ui::image_store::paint_image_cover(scene, tf, rect, &img, 12.0);
                } else {
                    fill_round(scene, tf, rect, 12.0, c.surfaces.raised);
                    let dot = Point::new(rect.center().x, rect.center().y - 4.0);
                    fill_circle(scene, tf, dot, 12.0, c.surface_white_10);
                    let (gw, gh) = tr.measure("🖼", 18.0, None);
                    text(scene, tr, tf, "🖼", 18.0, c.text_dim, dot.x - gw as f64 / 2.0, dot.y - gh as f64 / 2.0, TextWeight::Regular, None);
                }
                // Caption gradient scrim + filename, then a framing stroke.
                fill_round(scene, tf, Rect::new(rect.x0, rect.y1 - 30.0, rect.x1, rect.y1), 12.0, Color::from_rgba8(0, 0, 0, 120));
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
                let ext = std::path::Path::new(name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_uppercase())
                    .filter(|e| !e.is_empty());
                let meta = match ext {
                    Some(e) => format!("{e} · {}", human_size(*size)),
                    None => human_size(*size),
                };
                label(scene, tr, tf, &meta, 11.0, c.text_dim, chip.x0 + 56.0, chip.center().y + 2.0);
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

    // ------------------------------------------------------- attach picker
    /// The workspace file picker, anchored above the composer's "+". Lists the
    /// real image files found in the shared workspace, each with a decoded
    /// thumbnail (through the same [`ui::ImageStore`] the timeline uses), so a
    /// share is always an actual file the user chose. An empty workspace says
    /// so plainly rather than offering anything fabricated.
    pub(crate) fn draw_attach_picker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        // Full-window dismiss scrim (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);

        let comp = self.composer_rect(tr);
        let w = 360.0_f64.min(self.rect.width() - 32.0).max(220.0);
        let x0 = comp
            .x0
            .min(self.rect.x1 - w - 12.0)
            .max(self.rect.x0 + 12.0);

        let head_h = 40.0;
        let row_h = 52.0;
        let max_rows = 6usize;
        let shown = self.attach_entries.len().min(max_rows);
        let body_h = if self.attach_entries.is_empty() {
            46.0
        } else {
            shown as f64 * row_h + 8.0
        };
        let more_h = if self.attach_entries.len() > max_rows { 20.0 } else { 0.0 };
        let panel_h = head_h + body_h + more_h + 8.0;
        let y1 = comp.y0 - 10.0;
        let y0 = (y1 - panel_h).max(self.rect.y0 + 12.0);
        let panel = Rect::new(x0, y0, x0 + w, y1);

        fill_round(scene, tf, panel.inflate(5.0, 5.0), 18.0, Color::from_rgba8(0, 0, 0, 70));
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 14.0, c.border_white_10, 1.0);

        label(scene, tr, tf, "Share a file", 13.5, c.text_primary, panel.x0 + 14.0, panel.y0 + 12.0);
        let count = format!("{}", self.attach_entries.len());
        let (cw, _) = tr.measure(&count, 11.5, None);
        label(scene, tr, tf, &count, 11.5, c.text_dim, panel.x1 - cw as f64 - 14.0, panel.y0 + 14.0);
        hline(scene, tf, panel.x0 + 12.0, panel.x1 - 12.0, panel.y0 + head_h - 4.0, c.border_standard);

        if self.attach_entries.is_empty() {
            let dir = crate::picker::attach_dir();
            let s = format!("No images in {}", dir.display());
            let s = truncate_to(tr, &s, 12.5, panel.width() - 28.0);
            label(scene, tr, tf, &s, 12.5, c.text_dim, panel.x0 + 14.0, panel.y0 + head_h + 10.0);
            return;
        }

        let mut y = panel.y0 + head_h + 4.0;
        for e in self.attach_entries.iter().take(max_rows) {
            let row = Rect::new(panel.x0 + 6.0, y, panel.x1 - 6.0, y + row_h - 6.0);
            if row.contains(self.last_mouse) {
                fill_round(scene, tf, row, 10.0, c.surface_white_5);
            }
            let thumb = Rect::new(row.x0 + 6.0, row.y0 + 5.0, row.x0 + 62.0, row.y1 - 5.0);
            let key = e.key();
            if let Some(img) = self.images.get(&key) {
                ui::image_store::paint_image_cover(scene, tf, thumb, &img, 8.0);
            } else {
                fill_round(scene, tf, thumb, 8.0, c.surfaces.raised);
                let (gw, gh) = tr.measure("🖼", 16.0, None);
                text(scene, tr, tf, "🖼", 16.0, c.text_dim, thumb.center().x - gw as f64 / 2.0, thumb.center().y - gh as f64 / 2.0, TextWeight::Regular, None);
            }
            stroke_round(scene, tf, thumb, 8.0, c.border_white_10, 1.0);
            let name = truncate_to(tr, &e.name, 13.5, row.width() - 96.0);
            label(scene, tr, tf, &name, 13.5, c.text_primary, thumb.x1 + 12.0, row.center().y - 13.0);
            label(scene, tr, tf, &human_size(e.size), 11.0, c.text_dim, thumb.x1 + 12.0, row.center().y + 3.0);
            ChatApp::push_hit(hits, row, Hit::AttachPick(key));
            y += row_h;
        }
        if self.attach_entries.len() > max_rows {
            let more = format!("+{} more in the workspace", self.attach_entries.len() - max_rows);
            label(scene, tr, tf, &more, 11.0, c.text_dim, panel.x0 + 14.0, panel.y1 - 16.0);
        }
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
