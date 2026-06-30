//! Rendering for the conferencing layers: the docked call "huddle" PiP,
//! the expanded 2x2 call overlay, and the two floating popovers (emoji
//! picker + edit history). The host tile is a real [`ui::widget::VideoPlayer`]
//! laid out by the app; everything here is the chrome around it.
//!
//! Screenshare is intentionally absent: peers "follow the host" through the
//! shared workspace rather than watching a video of a screen, so the
//! controls expose a Follow-host toggle instead.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Brush, Color, Fill, Gradient};

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::app::{frac_rect, ChatApp, Hit, HUDDLE_VIDEO, OVERLAY_VIDEO};
use crate::emoji;
use crate::model::{self, Presence};
use crate::paint::*;
use crate::render_chat::draw_avatar;

#[derive(Clone, Copy)]
enum TileKind {
    VideoHost,
    VideoTile,
    Muted,
    AudioOnly,
}

impl ChatApp {
    fn participants(&self) -> Vec<(&'static str, TileKind, bool)> {
        // (user id, tile kind, talking)
        vec![
            ("u_me", TileKind::VideoHost, false),
            ("u_aria", TileKind::VideoTile, true),
            ("u_milo", TileKind::Muted, false),
            ("u_sora", TileKind::AudioOnly, false),
        ]
    }

    fn huddle_panel(&self) -> Rect {
        let v = frac_rect(self.rect, HUDDLE_VIDEO);
        Rect::new(v.x0 - 12.0, v.y0 - 40.0, v.x1 + 12.0, v.y1 + 132.0)
    }

    // --------------------------------------------------------- huddle (PiP)
    pub(crate) fn draw_huddle_bg(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let panel = self.huddle_panel();
        // drop shadow
        fill_round(scene, tf, panel.inflate(6.0, 6.0), 20.0, Color::from_rgba8(0, 0, 0, 70));
        fill_round(scene, tf, panel, 16.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 16.0, c.border_white_10, 1.0);

        // title bar
        label(scene, tr, tf, "Design Huddle", 13.0, c.text_primary, panel.x0 + 14.0, panel.y0 + 10.0);
        fill_circle(scene, tf, Point::new(panel.x1 - 58.0, panel.y0 + 17.0), 3.5, c.semantic.success);
        label(scene, tr, tf, "12:04", 11.5, c.text_dim, panel.x1 - 48.0, panel.y0 + 10.0);
        let _ = hits;
    }

    pub(crate) fn draw_huddle_fg(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let panel = self.huddle_panel();
        let v = frac_rect(self.rect, HUDDLE_VIDEO);
        stroke_round(scene, tf, v, 10.0, c.border_white_20, 1.0);

        // host name plate over the video
        self.name_plate(scene, tf, tr, theme, v, "You", false, self.follow_host);

        // participant chips under the video
        let chips_y = v.y1 + 14.0;
        let mut x = panel.x0 + 14.0;
        for (id, kind, talking) in self.participants().into_iter().skip(1) {
            if let Some(u) = self.user(id) {
                let r = 16.0;
                let cc = Point::new(x + r, chips_y + r);
                if talking {
                    stroke_circle(scene, tf, cc, r + 3.0, c.accent[5], 2.0);
                }
                draw_avatar(scene, tf, tr, theme, cc, r, &u.initials, u.color_idx, None);
                let muted = matches!(kind, TileKind::Muted);
                if muted {
                    fill_circle(scene, tf, Point::new(cc.x + r * 0.7, cc.y + r * 0.7), 7.0, c.semantic.danger);
                    let (gw, gh) = tr.measure("🔇", 9.0, None);
                    text(scene, tr, tf, "🔇", 9.0, Color::WHITE, cc.x + r * 0.7 - gw as f64 / 2.0, cc.y + r * 0.7 - gh as f64 / 2.0, TextWeight::Regular, None);
                }
                x += 2.0 * r + 12.0;
            }
        }

        // controls row: mute, expand, leave
        let cy = panel.y1 - 34.0;
        let mute = Rect::new(panel.x0 + 14.0, cy, panel.x0 + 84.0, cy + 28.0);
        self.ctl(scene, tf, tr, theme, mute, if self.muted { "Unmute" } else { "Mute" }, self.muted, false);
        ChatApp::push_hit(hits, mute, Hit::CallMute);

        let exp = Rect::new(mute.x1 + 8.0, cy, mute.x1 + 86.0, cy + 28.0);
        self.ctl(scene, tf, tr, theme, exp, "Expand", false, false);
        ChatApp::push_hit(hits, exp, Hit::CallExpand);

        let leave = Rect::new(panel.x1 - 78.0, cy, panel.x1 - 14.0, cy + 28.0);
        self.ctl(scene, tf, tr, theme, leave, "Leave", false, true);
        ChatApp::push_hit(hits, leave, Hit::CallLeave);
    }

    // ----------------------------------------------------- expanded overlay
    pub(crate) fn draw_call_overlay_bg(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, self.rect, c.surfaces.overlay);

        // title
        let title = self.channel(&self.call_channel).map(|c| c.name.clone()).unwrap_or_else(|| "Call".into());
        let (tw, _) = tr.measure(&title, 20.0, None);
        text(scene, tr, tf, &title, 20.0, c.text_primary, self.rect.center().x - tw as f64 / 2.0, self.rect.y0 + 28.0, TextWeight::Bold, None);
        let sub = "4 in call · end-to-end encrypted over iroh";
        let (sw, _) = tr.measure(sub, 12.0, None);
        label(scene, tr, tf, sub, 12.0, c.text_dim, self.rect.center().x - sw as f64 / 2.0, self.rect.y0 + 56.0);

        // non-host tiles (host is the VideoPlayer drawn by the app)
        let host = frac_rect(self.rect, OVERLAY_VIDEO);
        stroke_round(scene, tf, host, 14.0, c.border_white_20, 1.5);
        let gap = host.width() * 0.04;
        let positions = [
            Rect::new(host.x1 + gap, host.y0, host.x1 + gap + host.width(), host.y1),
            Rect::new(host.x0, host.y1 + gap, host.x1, host.y1 + gap + host.height()),
            Rect::new(host.x1 + gap, host.y1 + gap, host.x1 + gap + host.width(), host.y1 + gap + host.height()),
        ];
        for ((id, kind, talking), rect) in self.participants().into_iter().skip(1).zip(positions) {
            self.draw_tile(scene, tf, tr, theme, rect, id, kind, talking);
        }
        let _ = hits;
    }

    pub(crate) fn draw_call_overlay_fg(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let host = frac_rect(self.rect, OVERLAY_VIDEO);
        self.name_plate(scene, tf, tr, theme, host, "You (host)", false, self.follow_host);

        // controls bar
        let bar_w = 560.0;
        let bar = Rect::new(self.rect.center().x - bar_w / 2.0, self.rect.y1 - 84.0, self.rect.center().x + bar_w / 2.0, self.rect.y1 - 28.0);
        fill_round(scene, tf, bar, 16.0, c.neutral[1]);
        stroke_round(scene, tf, bar, 16.0, c.border_white_10, 1.0);
        let mut x = bar.x0 + 16.0;
        let cy = bar.center().y - 16.0;
        for (label_txt, on, danger, hit) in [
            (if self.muted { "Unmute" } else { "Mute" }, self.muted, false, Hit::CallMute),
            ("Audio only", self.audio_only, false, Hit::CallAudioOnly),
            ("Follow host", self.follow_host, false, Hit::CallFollow),
            ("Collapse", false, false, Hit::CallExpand),
            ("Leave", false, true, Hit::CallLeave),
        ] {
            let w = tr.measure(label_txt, 13.0, None).0 as f64 + 28.0;
            let r = Rect::new(x, cy, x + w, cy + 32.0);
            self.ctl(scene, tf, tr, theme, r, label_txt, on, danger);
            ChatApp::push_hit(hits, r, hit);
            x = r.x1 + 10.0;
        }

        // permission row
        let perm = "🔒 Camera & mic shared with this cluster · revoke in Settings";
        let (pw, _) = tr.measure(perm, 11.5, None);
        label(scene, tr, tf, perm, 11.5, c.text_dim, self.rect.center().x - pw as f64 / 2.0, self.rect.y1 - 104.0);
    }

    fn draw_tile(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        id: &str,
        kind: TileKind,
        talking: bool,
    ) {
        let c = &theme.tokens.colors;
        let u = self.user(id);
        let cidx = u.map(|u| u.color_idx).unwrap_or(0);
        match kind {
            TileKind::VideoTile => {
                let g = Gradient::new_linear(Point::new(rect.x0, rect.y0), Point::new(rect.x1, rect.y1))
                    .with_stops([(0.0_f32, palette_color(theme, cidx)), (1.0_f32, palette_color(theme, cidx + 3))]);
                let shape = vello::kurbo::RoundedRect::from_rect(rect, 14.0);
                scene.fill(Fill::NonZero, tf, &Brush::Gradient(g), None, &shape);
                fill_round(scene, tf, Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + rect.height() * 0.5), 14.0, Color::from_rgba8(255, 255, 255, 16));
            }
            _ => {
                fill_round(scene, tf, rect, 14.0, c.neutral[1]);
            }
        }
        if let Some(u) = u {
            let r = (rect.height() * 0.22).min(46.0);
            let show = matches!(kind, TileKind::Muted | TileKind::AudioOnly);
            if show {
                draw_avatar(scene, tf, tr, theme, rect.center(), r, &u.initials, u.color_idx, None);
            }
        }
        if talking {
            stroke_round(scene, tf, rect, 14.0, c.accent[5], 2.5);
        }
        if matches!(kind, TileKind::AudioOnly) {
            let badge = Rect::new(rect.x1 - 96.0, rect.y0 + 10.0, rect.x1 - 10.0, rect.y0 + 34.0);
            fill_round(scene, tf, badge, 10.0, Color::from_rgba8(0, 0, 0, 120));
            label(scene, tr, tf, "Audio only", 11.0, c.text_primary, badge.x0 + 9.0, badge.center().y - 7.0);
        }
        let muted = matches!(kind, TileKind::Muted);
        let name = u.map(|u| u.name.clone()).unwrap_or_default();
        self.name_plate(scene, tf, tr, theme, rect, &name, muted, false);
        stroke_round(scene, tf, rect, 14.0, c.border_white_10, 1.0);
    }

    /// Bottom-left name plate over a tile: name + mic state (+ optional
    /// "following" badge for the host).
    #[allow(clippy::too_many_arguments)]
    fn name_plate(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tile: Rect,
        name: &str,
        muted: bool,
        following: bool,
    ) {
        let c = &theme.tokens.colors;
        let mic = if muted { "🔇" } else { "🎤" };
        let label_txt = format!("{mic}  {name}");
        let (lw, _) = tr.measure(&label_txt, 12.0, None);
        let plate = Rect::new(tile.x0 + 8.0, tile.y1 - 30.0, tile.x0 + 8.0 + lw as f64 + 20.0, tile.y1 - 8.0);
        fill_round(scene, tf, plate, 8.0, Color::from_rgba8(0, 0, 0, 150));
        let col = if muted { c.semantic.danger } else { Color::WHITE };
        label(scene, tr, tf, &label_txt, 12.0, col, plate.x0 + 10.0, plate.center().y - 8.0);
        if following {
            let fb = Rect::new(tile.x1 - 96.0, tile.y1 - 30.0, tile.x1 - 8.0, tile.y1 - 8.0);
            fill_round(scene, tf, fb, 8.0, c.accent[5]);
            label(scene, tr, tf, "Following", 11.0, Color::WHITE, fb.x0 + 9.0, fb.center().y - 7.0);
        }
    }

    /// A pill control button. `on` = active/highlighted, `danger` = red.
    #[allow(clippy::too_many_arguments)]
    fn ctl(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label_txt: &str,
        on: bool,
        danger: bool,
    ) {
        let c = &theme.tokens.colors;
        let (bg, fg) = if danger {
            (c.semantic.danger, Color::WHITE)
        } else if on {
            (c.accent[5], Color::WHITE)
        } else {
            (c.surface_white_10, c.text_primary)
        };
        fill_round(scene, tf, rect, 9.0, bg);
        let (lw, _) = tr.measure(label_txt, 13.0, None);
        label(scene, tr, tf, label_txt, 13.0, fg, rect.center().x - lw as f64 / 2.0, rect.center().y - 8.0);
    }

    // -------------------------------------------------------- emoji picker
    pub(crate) fn draw_emoji_picker(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        // full-window dismiss scrim (lowest priority)
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);

        let w = 320.0;
        let cols = 7usize;
        let cell = (w - 24.0) / cols as f64;
        // height: quick row + categories
        let mut rows_total = 1; // quick row
        for cat in emoji::CATEGORIES {
            rows_total += 1 + (cat.emoji.len() + cols - 1) / cols; // label + grid rows
        }
        let h = 56.0 + rows_total as f64 * cell + 12.0;
        let mut x0 = self.emoji_anchor.x;
        let mut y0 = self.emoji_anchor.y;
        x0 = x0.clamp(self.rect.x0 + 8.0, self.rect.x1 - w - 8.0);
        y0 = y0.clamp(self.rect.y0 + 8.0, (self.rect.y1 - h - 8.0).max(self.rect.y0 + 8.0));
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        fill_round(scene, tf, panel.inflate(5.0, 5.0), 18.0, Color::from_rgba8(0, 0, 0, 70));
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 14.0, c.border_white_10, 1.0);

        let title = if self.emoji_target.is_some() { "Add reaction" } else { "Emoji" };
        label(scene, tr, tf, title, 13.0, c.text_primary, panel.x0 + 14.0, panel.y0 + 12.0);

        let mut y = panel.y0 + 36.0;
        // quick row
        label(scene, tr, tf, "Frequent", 10.5, c.text_dim, panel.x0 + 14.0, y);
        y += 16.0;
        self.emoji_grid(scene, tf, tr, theme, &emoji::QUICK, panel.x0 + 12.0, &mut y, cols, cell, hits);
        for cat in emoji::CATEGORIES {
            label(scene, tr, tf, cat.name, 10.5, c.text_dim, panel.x0 + 14.0, y);
            y += 16.0;
            self.emoji_grid(scene, tf, tr, theme, cat.emoji, panel.x0 + 12.0, &mut y, cols, cell, hits);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emoji_grid(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        set: &[&str],
        x0: f64,
        y: &mut f64,
        cols: usize,
        cell: f64,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        for (i, e) in set.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx = x0 + col as f64 * cell;
            let cyy = *y + row as f64 * cell;
            let cellr = Rect::new(cx, cyy, cx + cell - 4.0, cyy + cell - 4.0);
            let (ew, eh) = tr.measure(e, 19.0, None);
            text(scene, tr, tf, e, 19.0, c.text_primary, cellr.center().x - ew as f64 / 2.0, cellr.center().y - eh as f64 / 2.0, TextWeight::Regular, None);
            ChatApp::push_hit(hits, cellr, Hit::EmojiChoice((*e).to_string()));
        }
        let rows = (set.len() + cols - 1) / cols;
        *y += rows as f64 * cell + 6.0;
    }

    // ------------------------------------------------------ history popover
    pub(crate) fn draw_history_popover(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let Some(mid) = self.history_open.clone() else { return };
        let Some(m) = self.message(&mid).cloned() else { return };

        let w = 380.0;
        let tw = (w - 28.0) as f32;
        // measure
        let mut entries: Vec<(String, String)> = vec![(format!("Now · {}", model::clock(m.ts)), m.text.clone())];
        for rec in m.history.iter().rev() {
            entries.push((format!("Edited · {}", model::clock(rec.ts)), rec.text.clone()));
        }
        let mut total = 44.0;
        let mut heights = Vec::new();
        for (_, body) in &entries {
            let bh = tr.measure(body, 13.0, Some(tw)).1 as f64 + 22.0 + 14.0;
            heights.push(bh);
            total += bh;
        }
        let mut x0 = self.history_anchor.x;
        let mut y0 = self.history_anchor.y;
        x0 = x0.clamp(self.rect.x0 + 8.0, self.rect.x1 - w - 8.0);
        y0 = y0.clamp(self.rect.y0 + 8.0, (self.rect.y1 - total - 8.0).max(self.rect.y0 + 8.0));
        let panel = Rect::new(x0, y0, x0 + w, y0 + total);
        fill_round(scene, tf, panel.inflate(5.0, 5.0), 18.0, Color::from_rgba8(0, 0, 0, 70));
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(scene, tf, panel, 14.0, c.border_white_10, 1.0);
        label(scene, tr, tf, "Edit history", 13.0, c.text_primary, panel.x0 + 14.0, panel.y0 + 12.0);

        let mut y = panel.y0 + 38.0;
        for (i, (head, body)) in entries.iter().enumerate() {
            let accent = i == 0;
            label(scene, tr, tf, head, 11.0, if accent { c.accent[6] } else { c.text_dim }, panel.x0 + 14.0, y);
            y += 18.0;
            text(scene, tr, tf, body, 13.0, if accent { c.text_primary } else { c.text_dim }, panel.x0 + 14.0, y, TextWeight::Regular, Some(tw));
            y += heights[i] - 22.0 - 14.0 + 8.0;
            if i + 1 < entries.len() {
                hline(scene, tf, panel.x0 + 12.0, panel.x1 - 12.0, y, c.border_standard);
            }
            y += 6.0;
        }
        let _ = Presence::Online;
    }
}
