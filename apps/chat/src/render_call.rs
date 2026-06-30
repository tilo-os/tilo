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

use crate::app::{frac_rect, ChatApp, Hit, OVERLAY_VIDEO};
use crate::emoji;
use crate::model::{self, Presence, VoiceMember};
use crate::paint::*;
use crate::render_chat::draw_avatar;

/// One person to render in the call, derived from the shared CRDT voice
/// membership (not hardcoded). The host (this peer) is listed first.
struct Participant {
    user: String,
    talking: bool,
    muted: bool,
    audio_only: bool,
}

impl ChatApp {
    /// The call roster, host-first, from the shared voice membership. Falls
    /// back to "just me" if the channel has no recorded members yet.
    fn call_participants(&self) -> Vec<Participant> {
        let members: &[VoiceMember] = self.voice_in(&self.call.channel);
        let mut out: Vec<Participant> = Vec::new();
        // Host = this peer.
        out.push(Participant {
            user: model::ME.to_string(),
            talking: !self.call.muted,
            muted: self.call.muted,
            audio_only: self.call.audio_only,
        });
        for m in members {
            if m.user == model::ME {
                continue; // already the host tile
            }
            out.push(Participant {
                user: m.user.clone(),
                talking: m.talking && !m.muted,
                muted: m.muted,
                audio_only: m.audio_only,
            });
        }
        out
    }

    /// Whether to show the participant list expanded — the user's toggle,
    /// but auto-collapsing very large calls until summoned.
    fn participants_shown(&self, n: usize) -> bool {
        self.call.participants_expanded && crate::call::CallState::auto_participants_expanded(n)
            || self.call.participants_expanded && n <= 12
    }

    /// The floating overlay's home rect (video) for the current drag offset.
    fn huddle_video(&self) -> Rect {
        self.call_video_rect()
    }

    fn huddle_panel(&self) -> Rect {
        let v = self.huddle_video();
        // Panel grows with the participant roster so the chip strip fits.
        let n = self.call_participants().len();
        let extra = if self.participants_shown(n) { 60.0 } else { 20.0 };
        Rect::new(v.x0 - 12.0, v.y0 - 40.0, v.x1 + 12.0, v.y1 + 96.0 + extra)
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

        // Title bar doubles as the DRAG HANDLE — grab it to move the overlay.
        let title_bar = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 30.0);
        let name = self.channel(&self.call.channel).map(|c| c.name.clone()).unwrap_or_else(|| "Call".into());
        label(scene, tr, tf, "⠿", 12.0, c.text_dim, panel.x0 + 12.0, panel.y0 + 9.0);
        label(scene, tr, tf, &name, 13.0, c.text_primary, panel.x0 + 28.0, panel.y0 + 10.0);
        fill_circle(scene, tf, Point::new(panel.x1 - 58.0, panel.y0 + 17.0), 3.5, c.semantic.success);
        let n = self.voice_in(&self.call.channel).len().max(1);
        label(scene, tr, tf, &format!("{n}"), 11.5, c.text_dim, panel.x1 - 48.0, panel.y0 + 10.0);
        ChatApp::push_hit(hits, title_bar, Hit::CallDragHandle);
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
        let v = self.huddle_video();
        // Active-speaker ring around the host tile when this peer is talking.
        let host_talking = !self.call.muted;
        if host_talking {
            stroke_round(scene, tf, v, 10.0, c.semantic.success, 2.5);
        } else {
            stroke_round(scene, tf, v, 10.0, c.border_white_20, 1.0);
        }
        self.name_plate(scene, tf, tr, theme, v, "You", self.call.muted, self.call.follow_host);

        // Participant strip header with an expand/collapse toggle.
        let parts = self.call_participants();
        let others = parts.len().saturating_sub(1);
        let strip_y = v.y1 + 10.0;
        let toggle = Rect::new(panel.x1 - 84.0, strip_y, panel.x1 - 14.0, strip_y + 20.0);
        let caret = if self.call.participants_expanded { "▾" } else { "▸" };
        label(scene, tr, tf, &format!("{caret} {} in call", parts.len()), 11.0, c.text_dim, panel.x0 + 14.0, strip_y + 3.0);
        let _ = toggle;
        let toggle_hit = Rect::new(panel.x0 + 12.0, strip_y - 2.0, panel.x1 - 12.0, strip_y + 20.0);
        ChatApp::push_hit(hits, toggle_hit, Hit::CallParticipantsToggle);

        // Adaptive participant chips: shown when expanded, wrapping to the
        // panel width so the strip auto-adapts to the headcount.
        if self.participants_shown(parts.len()) && others > 0 {
            let chips_y = strip_y + 26.0;
            let r = 15.0;
            let step = 2.0 * r + 12.0;
            let per_row = ((panel.width() - 28.0) / step).floor().max(1.0) as usize;
            for (i, p) in parts.iter().skip(1).enumerate() {
                if let Some(u) = self.user(&p.user) {
                    let col = i % per_row;
                    let row = i / per_row;
                    let cc = Point::new(panel.x0 + 14.0 + r + col as f64 * step, chips_y + r + row as f64 * step);
                    // Speaking ring on whoever is talking.
                    if p.talking {
                        stroke_circle(scene, tf, cc, r + 3.0, c.semantic.success, 2.0);
                    }
                    draw_avatar(scene, tf, tr, theme, cc, r, &u.initials, u.color_idx, None);
                    if p.muted {
                        fill_circle(scene, tf, Point::new(cc.x + r * 0.72, cc.y + r * 0.72), 6.5, c.semantic.danger);
                    } else if p.audio_only {
                        fill_circle(scene, tf, Point::new(cc.x + r * 0.72, cc.y + r * 0.72), 6.5, c.neutral[3]);
                    }
                }
            }
        }

        // controls row: mute, deafen, expand, leave
        let cy = panel.y1 - 34.0;
        let mute = Rect::new(panel.x0 + 12.0, cy, panel.x0 + 70.0, cy + 28.0);
        self.ctl(scene, tf, tr, theme, mute, if self.call.muted { "Unmute" } else { "Mute" }, self.call.muted, false);
        ChatApp::push_hit(hits, mute, Hit::CallMute);

        let deaf = Rect::new(mute.x1 + 6.0, cy, mute.x1 + 64.0, cy + 28.0);
        self.ctl(scene, tf, tr, theme, deaf, if self.call.deafened { "Undeafen" } else { "Deafen" }, self.call.deafened, false);
        ChatApp::push_hit(hits, deaf, Hit::CallDeafen);

        let exp = Rect::new(deaf.x1 + 6.0, cy, deaf.x1 + 64.0, cy + 28.0);
        self.ctl(scene, tf, tr, theme, exp, "Expand", false, false);
        ChatApp::push_hit(hits, exp, Hit::CallExpand);

        let leave = Rect::new(panel.x1 - 70.0, cy, panel.x1 - 12.0, cy + 28.0);
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

        // title + connection line (real vs simulated transport, honestly).
        let title = self.channel(&self.call.channel).map(|c| c.name.clone()).unwrap_or_else(|| "Call".into());
        let (tw, _) = tr.measure(&title, 20.0, None);
        text(scene, tr, tf, &title, 20.0, c.text_primary, self.rect.center().x - tw as f64 / 2.0, self.rect.y0 + 28.0, TextWeight::Bold, None);
        let parts = self.call_participants();
        let sub = format!("{} in call · {} transport", parts.len(), self.media.description());
        let (sw, _) = tr.measure(&sub, 12.0, None);
        label(scene, tr, tf, &sub, 12.0, c.text_dim, self.rect.center().x - sw as f64 / 2.0, self.rect.y0 + 56.0);

        // Host tile (the VideoPlayer is drawn by the app on top); ring it if
        // this peer is the active speaker.
        let host = frac_rect(self.rect, OVERLAY_VIDEO);
        if !self.call.muted {
            stroke_round(scene, tf, host, 14.0, c.semantic.success, 2.5);
        } else {
            stroke_round(scene, tf, host, 14.0, c.border_white_20, 1.5);
        }

        // Remote tiles auto-arranged in a grid that adapts to the headcount.
        let others: Vec<&Participant> = parts.iter().skip(1).collect();
        let rects = grid_rects(host, others.len());
        for (p, rect) in others.iter().zip(rects) {
            self.draw_member_tile(scene, tf, tr, theme, rect, p);
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
        self.name_plate(scene, tf, tr, theme, host, "You (host)", self.call.muted, self.call.follow_host);

        // controls bar
        let bar_w = 640.0;
        let bar = Rect::new(self.rect.center().x - bar_w / 2.0, self.rect.y1 - 84.0, self.rect.center().x + bar_w / 2.0, self.rect.y1 - 28.0);
        fill_round(scene, tf, bar, 16.0, c.neutral[1]);
        stroke_round(scene, tf, bar, 16.0, c.border_white_10, 1.0);
        let mut x = bar.x0 + 14.0;
        let cy = bar.center().y - 16.0;
        for (label_txt, on, danger, hit) in [
            (if self.call.muted { "Unmute" } else { "Mute" }, self.call.muted, false, Hit::CallMute),
            (if self.call.deafened { "Undeafen" } else { "Deafen" }, self.call.deafened, false, Hit::CallDeafen),
            ("Audio only", self.call.audio_only, false, Hit::CallAudioOnly),
            ("Follow host", self.call.follow_host, false, Hit::CallFollow),
            ("Collapse", false, false, Hit::CallExpand),
            ("Leave", false, true, Hit::CallLeave),
        ] {
            let w = tr.measure(label_txt, 13.0, None).0 as f64 + 24.0;
            let r = Rect::new(x, cy, x + w, cy + 32.0);
            self.ctl(scene, tf, tr, theme, r, label_txt, on, danger);
            ChatApp::push_hit(hits, r, hit);
            x = r.x1 + 8.0;
        }

        // permission line — honest about the seam (default-allow today).
        let perm = "🔒 Camera & mic shared with this cluster · call permissions: allow (OS policy pending)";
        let (pw, _) = tr.measure(perm, 11.5, None);
        label(scene, tr, tf, perm, 11.5, c.text_dim, self.rect.center().x - pw as f64 / 2.0, self.rect.y1 - 104.0);
    }

    /// Draw one remote participant's tile in the expanded grid. Camera-on
    /// participants get a gradient video stand-in; muted / audio-only ones
    /// show a centred avatar. A speaking ring highlights the active speaker.
    fn draw_member_tile(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        p: &Participant,
    ) {
        let c = &theme.tokens.colors;
        let u = self.user(&p.user);
        let cidx = u.map(|u| u.color_idx).unwrap_or(0);
        let camera_on = !p.audio_only && !p.muted;
        if camera_on {
            let g = Gradient::new_linear(Point::new(rect.x0, rect.y0), Point::new(rect.x1, rect.y1))
                .with_stops([(0.0_f32, palette_color(theme, cidx)), (1.0_f32, palette_color(theme, cidx + 3))]);
            let shape = vello::kurbo::RoundedRect::from_rect(rect, 14.0);
            scene.fill(Fill::NonZero, tf, &Brush::Gradient(g), None, &shape);
            fill_round(scene, tf, Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + rect.height() * 0.5), 14.0, Color::from_rgba8(255, 255, 255, 16));
        } else {
            fill_round(scene, tf, rect, 14.0, c.neutral[1]);
            if let Some(u) = u {
                let r = (rect.height() * 0.22).min(46.0);
                draw_avatar(scene, tf, tr, theme, rect.center(), r, &u.initials, u.color_idx, None);
            }
        }
        if p.talking {
            stroke_round(scene, tf, rect, 14.0, c.semantic.success, 2.5);
        }
        if p.audio_only {
            let badge = Rect::new(rect.x1 - 96.0, rect.y0 + 10.0, rect.x1 - 10.0, rect.y0 + 34.0);
            fill_round(scene, tf, badge, 10.0, Color::from_rgba8(0, 0, 0, 120));
            label(scene, tr, tf, "Audio only", 11.0, c.text_primary, badge.x0 + 9.0, badge.center().y - 7.0);
        }
        let name = u.map(|u| u.name.clone()).unwrap_or_default();
        self.name_plate(scene, tf, tr, theme, rect, &name, p.muted, false);
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

/// Arrange `n` remote tiles in a grid to the right of / below the host tile,
/// auto-adapting the column count to the headcount so a 1-person call is a
/// single big tile and a crowd packs into a tidy grid. Each tile matches the
/// host's aspect.
fn grid_rects(host: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return vec![];
    }
    let gap = host.width() * 0.045;
    // Columns scale with the count: 1→1, 2-3→2, 4-9→3, 10+→4 (plus the host
    // column on the left), packed beside and below the host.
    let cols = match n {
        1 => 1,
        2..=3 => 2,
        4..=8 => 3,
        _ => 4,
    };
    let tw = host.width();
    let th = host.height();
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let col = (i % cols) as f64;
        let row = (i / cols) as f64;
        // First row sits beside the host; the host occupies the leftmost
        // slot of row 0, so remote tiles start one column to its right.
        let x0 = host.x1 + gap + col * (tw + gap);
        let y0 = host.y0 + row * (th + gap);
        out.push(Rect::new(x0, y0, x0 + tw, y0 + th));
    }
    out
}
