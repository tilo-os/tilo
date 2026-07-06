//! Rendering for the conferencing layers: the docked call "huddle" PiP,
//! the expanded 2x2 call overlay, and the two floating popovers (emoji
//! picker + edit history). The host tile is a real [`ui::widget::VideoPlayer`]
//! laid out by the app; everything here is the chrome around it.
//!
//! Screenshare is intentionally absent: peers share the workspace spatially
//! rather than watching a video of a screen, so there is no screen-feed tile.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::app::{ChatApp, Hit};
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
            user: self.me.id.clone(),
            talking: !self.call.muted,
            muted: self.call.muted,
            audio_only: self.call.audio_only,
        });
        for m in members {
            if m.user == self.me.id {
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

    /// Paint an honest "camera off" placeholder over the host tile when the
    /// real [`VideoPlayer`](ui::widget::VideoPlayer) surface has no decoded
    /// frame. There is no camera in this environment, so instead of faking a
    /// video we show the local avatar and a plain label — the same treatment a
    /// camera-off remote participant gets. Once a live capture feeds a frame,
    /// this yields to it automatically.
    pub(crate) fn draw_host_camera_off(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.video.latest_frame().is_some() {
            return; // a real capture is feeding the surface
        }
        let c = &theme.tokens.colors;
        let rect = self.call_video_rect();
        let radius = if self.call.expanded { 14.0 } else { 10.0 };
        fill_round(scene, tf, rect, radius, c.neutral[2]);
        let (initials, cidx) = self
            .user(&self.me.id)
            .map(|u| (u.initials.clone(), u.color_idx))
            .unwrap_or_else(|| ("Y".into(), 0));
        let r = (rect.height() * 0.20).clamp(22.0, 52.0);
        let cx = rect.center().x;
        let cy = rect.center().y - r * 0.35;
        draw_avatar(scene, tf, tr, theme, Point::new(cx, cy), r, &initials, cidx, None);
        let s = "Camera off";
        let (sw, _) = tr.measure(s, 12.0, None);
        label(scene, tr, tf, s, 12.0, c.text_dim, cx - sw as f64 / 2.0, cy + r + 8.0);
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
        self.name_plate(scene, tf, tr, theme, v, "You", self.call.muted);

        // Participant strip header with an expand/collapse toggle.
        let parts = self.call_participants();
        let others = parts.len().saturating_sub(1);
        let strip_y = v.y1 + 10.0;
        let toggle = Rect::new(panel.x1 - 84.0, strip_y, panel.x1 - 14.0, strip_y + 20.0);
        let caret = if self.call.participants_expanded { "▾" } else { "▸" };
        // Surface the real media routing decision (P2P vs host relay), chosen
        // from the live headcount — the call promotes as people join.
        label(scene, tr, tf, &format!("{caret} {} in call · {}", parts.len(), self.call_session().label()), 11.0, c.text_dim, panel.x0 + 14.0, strip_y + 3.0);
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

        // Controls row, evenly distributed across the panel so it never
        // overflows a small (narrow-window) huddle. On a cramped panel the
        // buttons drop to icons and Expand is trimmed (the header "In call"
        // button also expands the call), so the row always fits.
        let cy = panel.y1 - 34.0;
        let pad = 12.0;
        let gap = 6.0;
        let muted = self.call.muted;
        let deaf = self.call.deafened;
        let mut ctrls: Vec<(&str, &str, bool, bool, Hit)> = vec![
            (if muted { "Unmute" } else { "Mute" }, if muted { "🔇" } else { "🎤" }, muted, false, Hit::CallMute),
            (if deaf { "Undeafen" } else { "Deafen" }, if deaf { "🔕" } else { "🎧" }, deaf, false, Hit::CallDeafen),
            ("Expand", "⤢", false, false, Hit::CallExpand),
            ("Leave", "✕", false, true, Hit::CallLeave),
        ];
        if panel.width() < 300.0 {
            ctrls.remove(2); // trim Expand on a cramped panel
        }
        let n = ctrls.len() as f64;
        let cw = ((panel.width() - 2.0 * pad - (n - 1.0) * gap) / n).max(24.0);
        let use_text = cw >= 58.0;
        let mut x = panel.x0 + pad;
        for (txt, icon, on, danger, hit) in ctrls {
            let r = Rect::new(x, cy, x + cw, cy + 28.0);
            self.ctl(scene, tf, tr, theme, r, if use_text { txt } else { icon }, on, danger);
            ChatApp::push_hit(hits, r, hit);
            x = r.x1 + gap;
        }
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
        // Opaque backdrop so the expanded call reads as its own focused
        // surface (the app base, darkened by the overlay tint) rather than a
        // thin scrim with the conversation showing through behind the tiles.
        fill_rect(scene, tf, self.rect, c.surfaces.base);
        fill_rect(scene, tf, self.rect, c.surfaces.overlay);

        // title + participant count
        let title = self.channel(&self.call.channel).map(|c| c.name.clone()).unwrap_or_else(|| "Call".into());
        let (tw, _) = tr.measure(&title, 20.0, None);
        text(scene, tr, tf, &title, 20.0, c.text_primary, self.rect.center().x - tw as f64 / 2.0, self.rect.y0 + 28.0, TextWeight::Bold, None);
        let parts = self.call_participants();
        let sub = format!("{} in the call", parts.len());
        let (sw, _) = tr.measure(&sub, 12.0, None);
        label(scene, tr, tf, &sub, 12.0, c.text_dim, self.rect.center().x - sw as f64 / 2.0, self.rect.y0 + 56.0);

        // Host + remotes share one responsive grid that fills the whole band
        // between the header and the controls bar, so there is never dead
        // space beneath or beside the host at any width — the host is simply
        // the first cell. The VideoPlayer surface is drawn by the app on top
        // of the host cell.
        let tiles = self.overlay_tiles();
        if let Some(&host) = tiles.first() {
            if !self.call.muted {
                stroke_round(scene, tf, host, 14.0, c.semantic.success, 2.5);
            } else {
                stroke_round(scene, tf, host, 14.0, c.border_white_20, 1.5);
            }
        }
        for (p, rect) in parts.iter().skip(1).zip(tiles.iter().skip(1)) {
            self.draw_member_tile(scene, tf, tr, theme, *rect, p);
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
        let host = self.overlay_host_rect();
        self.name_plate(scene, tf, tr, theme, host, "You (host)", self.call.muted);

        // Controls bar: auto-sized to its buttons and wrapped onto extra rows
        // when the window is too narrow to hold them all in one line, so it
        // never runs off either edge.
        let (bar, buttons) = self.overlay_controls_layout();
        fill_round(scene, tf, bar, 16.0, c.neutral[1]);
        stroke_round(scene, tf, bar, 16.0, c.border_white_10, 1.0);
        for (label_txt, on, danger, hit, r) in buttons {
            self.ctl(scene, tf, tr, theme, r, label_txt, on, danger);
            ChatApp::push_hit(hits, r, hit);
        }
    }

    /// The band the expanded call fills with participant tiles: everything
    /// between the title/subtitle header and the controls bar, inset from the
    /// window edges. Deterministic (no text measurement) so the tile grid, the
    /// host [`VideoPlayer`](ui::widget::VideoPlayer) node built in
    /// `build_layout`, and `call_video_rect` all resolve to exactly the same
    /// geometry on a single frame.
    pub(crate) fn overlay_content_region(&self) -> Rect {
        let r = self.rect;
        let inset = (r.width() * 0.05).clamp(16.0, 40.0);
        let top = r.y0 + 72.0; // below the title (y0+28) + subtitle (y0+56)
        let (bar, _) = self.overlay_controls_layout();
        let bottom = (bar.y0 - 14.0).max(top + 1.0);
        Rect::new(r.x0 + inset, top, r.x1 - inset, bottom)
    }

    /// Host-first participant tiles for the expanded call, packed to fill the
    /// whole content region at every width — the host is simply the first
    /// cell, so nothing is left empty beneath or beside it. A solo call yields
    /// a single tile spanning the full region.
    pub(crate) fn overlay_tiles(&self) -> Vec<Rect> {
        let n = self.call_participants().len().max(1);
        grid_rects(self.overlay_content_region(), n)
    }

    /// The host tile rect in the expanded overlay (the first grid cell).
    pub(crate) fn overlay_host_rect(&self) -> Rect {
        self.overlay_tiles()
            .into_iter()
            .next()
            .unwrap_or_else(|| self.overlay_content_region())
    }

    /// Lay out the expanded-overlay control buttons, wrapping onto extra rows
    /// to fit the current window width. Returns the bar background rect and
    /// each button's `(label, active, danger, hit, rect)`. Shared by the
    /// background pass (which bounds the tile region above the bar), the
    /// foreground pass (which draws it), and the layout pass (which sizes the
    /// tile band) — so all three agree without a `TextRenderer`. Button widths
    /// are derived from the fixed label lengths rather than measured, which
    /// keeps the whole call layout resolvable on a single headless frame.
    pub(crate) fn overlay_controls_layout(&self) -> (Rect, Vec<(&'static str, bool, bool, Hit, Rect)>) {
        let specs: [(&'static str, bool, bool, Hit); 5] = [
            (if self.call.muted { "Unmute" } else { "Mute" }, self.call.muted, false, Hit::CallMute),
            (if self.call.deafened { "Undeafen" } else { "Deafen" }, self.call.deafened, false, Hit::CallDeafen),
            ("Audio only", self.call.audio_only, false, Hit::CallAudioOnly),
            ("Collapse", false, false, Hit::CallExpand),
            ("Leave", false, true, Hit::CallLeave),
        ];
        let gap = 8.0;
        let row_gap = 8.0;
        let bh = 32.0;
        let side_pad = 24.0;
        let inner_pad = 12.0;
        let max_row_w = (self.rect.width() - 2.0 * side_pad).max(120.0);
        let widths: Vec<f64> = specs.iter().map(|s| overlay_button_width(s.0)).collect();

        // Greedy pack into rows that each fit `max_row_w`.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut cur = 0.0;
        for (i, w) in widths.iter().enumerate() {
            let extra = if rows.last().unwrap().is_empty() { 0.0 } else { gap } + w;
            if !rows.last().unwrap().is_empty() && cur + extra > max_row_w {
                rows.push(Vec::new());
                cur = 0.0;
            }
            let extra = if rows.last().unwrap().is_empty() { 0.0 } else { gap } + w;
            rows.last_mut().unwrap().push(i);
            cur += extra;
        }

        let n_rows = rows.len() as f64;
        let bar_h = n_rows * bh + (n_rows - 1.0) * row_gap + 2.0 * inner_pad;
        let widest = rows
            .iter()
            .map(|row| {
                row.iter().map(|&i| widths[i]).sum::<f64>()
                    + row.len().saturating_sub(1) as f64 * gap
            })
            .fold(0.0_f64, f64::max);

        let cx = self.rect.center().x;
        let bar_bottom = self.rect.y1 - 18.0;
        let bar_top = bar_bottom - bar_h;
        let bar = Rect::new(cx - widest / 2.0 - inner_pad, bar_top, cx + widest / 2.0 + inner_pad, bar_bottom);

        let mut out: Vec<(&'static str, bool, bool, Hit, Rect)> = Vec::with_capacity(specs.len());
        let mut ry = bar_top + inner_pad;
        for row in &rows {
            let rw = row.iter().map(|&i| widths[i]).sum::<f64>()
                + row.len().saturating_sub(1) as f64 * gap;
            let mut x = cx - rw / 2.0;
            for &i in row {
                let r = Rect::new(x, ry, x + widths[i], ry + bh);
                let (label_txt, on, danger, hit) = specs[i].clone();
                out.push((label_txt, on, danger, hit, r));
                x += widths[i] + gap;
            }
            ry += bh + row_gap;
        }
        (bar, out)
    }

    /// Draw one remote participant's tile in the expanded grid. There is no
    /// camera capture wired to remote peers in this environment, so a tile
    /// never fabricates a video frame: it shows the participant's avatar on a
    /// neutral surface — the same honest treatment the host tile gets — plus
    /// their real camera state. A speaking ring highlights the active speaker.
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
        fill_round(scene, tf, rect, 14.0, c.neutral[2]);
        if let Some(u) = u {
            let r = (rect.height() * 0.22).clamp(16.0, 46.0);
            let cx = rect.center().x;
            let cy = rect.center().y - r * 0.3;
            draw_avatar(scene, tf, tr, theme, Point::new(cx, cy), r, &u.initials, u.color_idx, None);
            // Honest state under the avatar — the same treatment the host tile
            // gets. There is no remote camera capture in this environment, so
            // the tile never fabricates a picture: an audio-only peer reads
            // "Camera off"; a camera-enabled peer whose frame doesn't reach
            // this peer reads "No video". It never claims to show video it
            // doesn't have.
            let s = member_tile_caption(p);
            let (sw, _) = tr.measure(s, 11.5, None);
            label(scene, tr, tf, s, 11.5, c.text_dim, cx - sw as f64 / 2.0, cy + r + 8.0);
        }
        if p.talking {
            stroke_round(scene, tf, rect, 14.0, c.semantic.success, 2.5);
        }
        let name = u.map(|u| u.name.clone()).unwrap_or_default();
        self.name_plate(scene, tf, tr, theme, rect, &name, p.muted);
        stroke_round(scene, tf, rect, 14.0, c.border_white_10, 1.0);
    }

    /// Bottom-left name plate over a tile: name + mic state.
    fn name_plate(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tile: Rect,
        name: &str,
        muted: bool,
    ) {
        let c = &theme.tokens.colors;
        let mic = if muted { "🔇" } else { "🎤" };
        let label_txt = format!("{mic}  {name}");
        let (lw, _) = tr.measure(&label_txt, 12.0, None);
        let plate = Rect::new(tile.x0 + 8.0, tile.y1 - 30.0, tile.x0 + 8.0 + lw as f64 + 20.0, tile.y1 - 8.0);
        fill_round(scene, tf, plate, 8.0, Color::from_rgba8(0, 0, 0, 150));
        let col = if muted { c.semantic.danger } else { Color::WHITE };
        label(scene, tr, tf, &label_txt, 12.0, col, plate.x0 + 10.0, plate.center().y - 8.0);
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

/// The honest one-line caption shown under a remote participant's avatar in
/// the expanded call grid. There is no remote camera capture wired up in this
/// environment, so a tile must never imply a live feed: an audio-only peer
/// reads "Camera off"; a camera-enabled peer whose frame doesn't reach us
/// reads "No video". Both are literally true, and both mirror the host tile's
/// honest placeholder — the tile shows an avatar, never a fabricated picture.
fn member_tile_caption(p: &Participant) -> &'static str {
    if p.audio_only {
        "Camera off"
    } else {
        "No video"
    }
}

/// A measurement-free width for an expanded-overlay control button: the fixed
/// label length at the 13px control font plus horizontal padding. Deterministic
/// so the controls bar (and the tile band bounded above it) lays out
/// identically without a [`TextRenderer`], while the label still renders
/// centred within the button via the real measured width at draw time.
fn overlay_button_width(label: &str) -> f64 {
    label.chars().count() as f64 * 8.0 + 26.0
}

/// Pack `n` tiles into `region`, choosing the column count that yields
/// the largest, most camera-shaped uniform tiles. Every returned rect is
/// contained within `region`, so the grid can never spill past the window at
/// any width or headcount. A partly-filled last row is centred.
fn grid_rects(region: Rect, n: usize) -> Vec<Rect> {
    if n == 0 || region.width() <= 1.0 || region.height() <= 1.0 {
        return vec![];
    }
    let gap = (region.width().min(region.height()) * 0.05).clamp(6.0, 16.0);

    // Try every column count and keep the one that maximises usable tile size,
    // lightly penalising extreme aspect ratios so tiles stay landscape-ish.
    let mut best_cols = 1usize;
    let mut best_score = f64::MIN;
    for cols in 1..=n {
        let rows = n.div_ceil(cols);
        let tw = (region.width() - (cols as f64 - 1.0) * gap) / cols as f64;
        let th = (region.height() - (rows as f64 - 1.0) * gap) / rows as f64;
        if tw <= 0.0 || th <= 0.0 {
            continue;
        }
        let aspect = tw / th;
        let aspect_pen = if aspect > 2.4 {
            2.4 / aspect
        } else if aspect < 0.85 {
            aspect / 0.85
        } else {
            1.0
        };
        let score = tw.min(th * 1.6) * aspect_pen;
        if score > best_score {
            best_score = score;
            best_cols = cols;
        }
    }

    let cols = best_cols;
    let rows = n.div_ceil(cols);
    let tw = (region.width() - (cols as f64 - 1.0) * gap) / cols as f64;
    let th = (region.height() - (rows as f64 - 1.0) * gap) / rows as f64;

    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let row = i / cols;
        let col = i % cols;
        // Centre the final row when it holds fewer than `cols` tiles.
        let in_row = if row == rows - 1 { n - row * cols } else { cols };
        let row_w = in_row as f64 * tw + (in_row as f64 - 1.0) * gap;
        let row_x0 = region.x0 + (region.width() - row_w) / 2.0;
        let x0 = row_x0 + col as f64 * (tw + gap);
        let y0 = region.y0 + row as f64 * (th + gap);
        out.push(Rect::new(x0, y0, x0 + tw, y0 + th));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{grid_rects, member_tile_caption, Participant};
    use vello::kurbo::Rect;

    fn participant(audio_only: bool) -> Participant {
        Participant { user: "u_x".into(), talking: false, muted: false, audio_only }
    }

    /// Defect regression: remote tiles must be honest — never present a
    /// fabricated video feed. Every caption is a truthful "camera state"
    /// string (matching the host tile), never a claim of live video.
    #[test]
    fn remote_tile_caption_is_honest_never_a_fabricated_feed() {
        assert_eq!(member_tile_caption(&participant(true)), "Camera off");
        assert_eq!(member_tile_caption(&participant(false)), "No video");
        for audio_only in [true, false] {
            let cap = member_tile_caption(&participant(audio_only)).to_lowercase();
            assert!(
                !cap.contains("live") && !cap.contains("streaming") && !cap.contains("feed"),
                "caption must not imply a live feed: {cap:?}"
            );
        }
    }

    /// The packer must keep every tile inside the region it is handed, at any
    /// headcount and width, with no degenerate tiles — the invariant the whole
    /// expanded overlay leans on to stay within the window.
    #[test]
    fn grid_tiles_stay_inside_region_at_every_count_and_width() {
        for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
            // A full-surface content band with symmetric side insets, matching
            // what the overlay now hands the packer (host + remotes together).
            let region = Rect::new(0.05 * w, 72.0, w - 0.05 * w, h - 128.0);
            for n in 1..=9 {
                let rects = grid_rects(region, n);
                assert_eq!(rects.len(), n, "n={n} produced the wrong tile count");
                for r in &rects {
                    assert!(
                        r.x0 >= region.x0 - 0.5 && r.x1 <= region.x1 + 0.5,
                        "n={n} w={w}: tile x [{:.1},{:.1}] escaped region x [{:.1},{:.1}]",
                        r.x0, r.x1, region.x0, region.x1
                    );
                    assert!(
                        r.y0 >= region.y0 - 0.5 && r.y1 <= region.y1 + 0.5,
                        "n={n} w={w}: tile y [{:.1},{:.1}] escaped region y [{:.1},{:.1}]",
                        r.y0, r.y1, region.y0, region.y1
                    );
                    assert!(r.width() > 0.0 && r.height() > 0.0, "n={n}: degenerate tile");
                }
            }
        }
    }

    #[test]
    fn grid_is_empty_for_no_remotes() {
        assert!(grid_rects(Rect::new(0.0, 0.0, 400.0, 300.0), 0).is_empty());
    }

    /// The narrow-width void regression: with the host as the first cell of a
    /// unified grid, the packed tiles must *cover* the content region — their
    /// bounding box spans it in both axes (within one inter-tile gap) — so no
    /// large dead band is left beneath or beside the host at any width. This is
    /// the geometry that replaced the old "host pinned left, remotes to the
    /// right" layout which left the whole bottom-left corner empty on a
    /// portrait window.
    #[test]
    fn grid_fills_the_region_leaving_no_void() {
        for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
            let region = Rect::new(0.05 * w, 72.0, w - 0.05 * w, h - 128.0);
            // A host + three remotes: the demo huddle, the exact case that
            // exposed the void.
            let tiles = grid_rects(region, 4);
            let bbox = tiles.iter().fold(
                Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN),
                |b, r| Rect::new(b.x0.min(r.x0), b.y0.min(r.y0), b.x1.max(r.x1), b.y1.max(r.y1)),
            );
            // One inter-tile gap is the largest legitimate slack (a centred row
            // or column can sit a gap in from an edge).
            let gap = (region.width().min(region.height()) * 0.05).clamp(6.0, 16.0);
            let slack = gap + 1.0;
            assert!(
                bbox.x0 - region.x0 <= slack && region.x1 - bbox.x1 <= slack,
                "w={w}: tiles leave a horizontal void (bbox x [{:.1},{:.1}] vs region [{:.1},{:.1}])",
                bbox.x0, bbox.x1, region.x0, region.x1
            );
            assert!(
                bbox.y0 - region.y0 <= slack && region.y1 - bbox.y1 <= slack,
                "w={w}: tiles leave a vertical void (bbox y [{:.1},{:.1}] vs region [{:.1},{:.1}])",
                bbox.y0, bbox.y1, region.y0, region.y1
            );
            // And a tile actually occupies the lower-left quadrant — the exact
            // corner the old layout left black.
            let midx = region.x0 + region.width() * 0.5;
            let midy = region.y0 + region.height() * 0.5;
            assert!(
                tiles.iter().any(|r| r.center().x < midx && r.center().y > midy),
                "w={w}: nothing fills the lower-left of the call surface"
            );
        }
    }
}
