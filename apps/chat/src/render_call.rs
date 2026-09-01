//! Rendering for the conferencing layers: the docked call "huddle" `PiP`, the
//! expanded call overlay (a responsive participant grid, or — while someone is
//! screen-sharing — a featured screen tile above a participant filmstrip), and
//! the two floating popovers (emoji picker + edit history). The host tile is a
//! real [`ui::widget::VideoPlayer`] laid out by the app; everything here is the
//! chrome around it.
//!
//! Screen-share honesty: the featured tile shows a peer's real decoded screen
//! frame when one has arrived (`remote_screens`) and an honest "connecting"
//! placeholder — a drawn monitor, never a fabricated picture — until then.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};

use ui::ColorCompat;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;

use crate::app::{ChatApp, Hit};
use crate::emoji;
use crate::model::{self, Presence, VoiceMember};
use crate::paint::{
    fill_circle, fill_rect, fill_round, hline, label, stroke_circle, stroke_round, text,
};
use crate::render_chat::draw_avatar;

/// One person to render in the call, derived from the shared CRDT voice
/// membership (not hardcoded). The host (this peer) is listed first.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool mirrors a distinct CRDT voice-membership field (talking/muted/audio_only/sharing) read straight off the shared state — grouping them would just relocate the boolean soup, not remove it"
)]
struct Participant {
    user: String,
    talking: bool,
    muted: bool,
    audio_only: bool,
    sharing: bool,
}

fn huddle_lower_chrome_for_reveal(reveal: f32) -> f64 {
    116.0 + 40.0 * f64::from(reveal.clamp(0.0, 1.0))
}

/// How the expanded call overlay tiles the surface. A plain call packs every
/// participant into a uniform [`CallStage::Grid`]; once someone shares their
/// screen it promotes to a [`CallStage::Share`] presentation stage — one large
/// featured screen tile above a single-row participant filmstrip.
pub(crate) enum CallStage {
    Grid(Vec<Rect>),
    Share { screen: Rect, strip: Vec<Rect> },
}

/// One expanded-overlay control button, as laid out by
/// [`ChatApp::overlay_controls_layout`]:
/// `(label, active, danger, enabled, hit, rect)`.
type OverlayButton = (&'static str, bool, bool, bool, Hit, Rect);

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
            sharing: self.call.sharing,
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
                sharing: m.sharing,
            });
        }
        out
    }

    /// Whether to show the participant list expanded — the user's toggle,
    /// but auto-collapsing very large calls until summoned.
    fn participants_should_show(&self, n: usize) -> bool {
        self.call.participants_expanded
            && (crate::call::CallState::auto_participants_expanded(n) || n <= 12)
    }

    /// Sample the participant reveal once per frame. Every geometry consumer
    /// subsequently reads the cached value, avoiding tiny wall-clock skew
    /// between the soft body, its controls, hit regions and native bounds.
    pub(crate) fn reconcile_huddle_motion(&mut self, theme: &Theme) {
        if !self.call.active {
            self.call_chrome_motion.reset();
            return;
        }
        let participant_count = self.call_participants().len();
        let open = self.participants_should_show(participant_count);
        self.call_chrome_motion.reconcile(open, theme);
        if !self.call.expanded {
            let lower_chrome = self.huddle_lower_chrome();
            self.call
                .placement
                .set_attached_chrome([12.0, 40.0, 12.0, lower_chrome]);
        }
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
            .map_or_else(|| ("Y".into(), 0), |u| (u.initials.clone(), u.color_idx));
        let r = (rect.height() * 0.20).clamp(22.0, 52.0);
        let cx = rect.center().x;
        let cy = rect.center().y - r * 0.35;
        draw_avatar(
            scene,
            tf,
            tr,
            theme,
            Point::new(cx, cy),
            r,
            &initials,
            cidx,
            None,
        );
        let s = "Camera off";
        let (sw, _) = tr.measure(s, 12.0, None);
        label(
            scene,
            tr,
            tf,
            s,
            12.0,
            c.text_dim,
            cx - f64::from(sw) / 2.0,
            cy + r + 8.0,
        );
    }

    pub(crate) fn huddle_panel(&self) -> Rect {
        let v = self.huddle_video();
        // Panel grows continuously with the participant reveal rather than
        // stepping between two native-window heights.
        let extra = self.huddle_lower_chrome() - 96.0;
        Rect::new(v.x0 - 12.0, v.y0 - 40.0, v.x1 + 12.0, v.y1 + 96.0 + extra)
    }

    pub(crate) fn huddle_attached_lower_chrome(&self) -> f64 {
        self.huddle_lower_chrome()
    }

    fn huddle_lower_chrome(&self) -> f64 {
        huddle_lower_chrome_for_reveal(self.call_chrome_motion.sampled)
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
        // One shared organic silhouette for the shadow, body and border. The
        // 8px transparent native-window gutter encloses the 6px shadow shape
        // exactly, so desktop adoption never clips a corner or widens it into
        // a second rectangular decoration.
        let shadow = ui::surface::soft_outline(panel.inflate(6.0, 6.0));
        scene.fill(
            Fill::NonZero,
            tf,
            Color::from_rgba8(0, 0, 0, 70),
            None,
            &shadow,
        );
        let body = ui::surface::soft_outline(panel);
        scene.fill(Fill::NonZero, tf, c.neutral[1], None, &body);
        scene.stroke(
            &Stroke::new(1.0),
            tf,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            None,
            &body,
        );

        // Title bar doubles as the DRAG HANDLE — grab it to move the overlay.
        let title_bar = Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 30.0);
        let name = self
            .channel(&self.call.channel)
            .map_or_else(|| "Call".into(), |c| c.name.clone());
        ui::surface::floating_drag_grip(scene, tf, panel, c.text_dim);
        label(
            scene,
            tr,
            tf,
            &name,
            13.0,
            c.text_primary,
            panel.x0 + 16.0,
            panel.y0 + 10.0,
        );
        fill_circle(
            scene,
            tf,
            Point::new(panel.x1 - 58.0, panel.y0 + 17.0),
            3.5,
            c.semantic.success,
        );
        let n = self.voice_in(&self.call.channel).len().max(1);
        label(
            scene,
            tr,
            tf,
            &format!("{n}"),
            11.5,
            c.text_dim,
            panel.x1 - 48.0,
            panel.y0 + 10.0,
        );
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
            stroke_round(
                scene,
                tf,
                v,
                10.0,
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 51),
                1.0,
            );
        }
        Self::name_plate(scene, tf, tr, theme, v, "You", self.call.muted);

        let parts = self.call_participants();
        let others = parts.len().saturating_sub(1);
        let strip_y = v.y1 + 10.0;
        self.draw_huddle_strip_header(scene, tf, tr, theme, panel, strip_y, &parts, hits);

        // Adaptive participant chips: shown when expanded, wrapping to the
        // panel width so the strip auto-adapts to the headcount.
        let reveal = self.call_chrome_motion.sampled.clamp(0.0, 1.0);
        if reveal > 0.001 && others > 0 {
            // Fade and progressively reveal the chip strip inside the same
            // animated panel bounds. Chips never pop out before the surface
            // has made room for them, and collapse cannot paint through the
            // rising controls row.
            let clip = Rect::new(
                panel.x0 + 8.0,
                strip_y + 20.0,
                panel.x1 - 8.0,
                (panel.y1 - 48.0).max(strip_y + 20.0),
            );
            let alpha = reveal * reveal * (3.0 - 2.0 * reveal);
            scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, tf, &clip);
            self.draw_huddle_chips(scene, tf, tr, theme, panel, strip_y, &parts);
            scene.pop_layer();
        }

        self.draw_huddle_controls(scene, tf, tr, theme, panel, hits);
    }

    /// Participant strip header: headcount + media-routing label, and the
    /// expand/collapse toggle's hit region.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_huddle_strip_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        strip_y: f64,
        parts: &[Participant],
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let c = &theme.tokens.colors;
        let toggle = Rect::new(panel.x1 - 84.0, strip_y, panel.x1 - 14.0, strip_y + 20.0);
        let caret = if self.participants_should_show(parts.len()) {
            "▾"
        } else {
            "▸"
        };
        // Surface the real media routing decision (P2P vs host relay), chosen
        // from the live headcount — the call promotes as people join.
        label(
            scene,
            tr,
            tf,
            &format!(
                "{caret} {} in call · {}",
                parts.len(),
                self.call_session().label()
            ),
            11.0,
            c.text_dim,
            panel.x0 + 14.0,
            strip_y + 3.0,
        );
        let _ = toggle;
        let toggle_hit = Rect::new(
            panel.x0 + 12.0,
            strip_y - 2.0,
            panel.x1 - 12.0,
            strip_y + 20.0,
        );
        ChatApp::push_hit(hits, toggle_hit, Hit::CallParticipantsToggle);
    }

    /// Adaptive participant chips beneath the strip header — one per
    /// non-host member, wrapping to the panel width.
    #[allow(
        clippy::too_many_arguments,
        reason = "draw-context bundle, not real parameter coupling"
    )]
    fn draw_huddle_chips(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        strip_y: f64,
        parts: &[Participant],
    ) {
        let c = &theme.tokens.colors;
        let chips_y = strip_y + 26.0;
        let radius = 15.0;
        let step = 2.0 * radius + 12.0;
        let per_row = ((panel.width() - 28.0) / step).floor().max(1.0) as usize;
        for (idx, p) in parts.iter().skip(1).enumerate() {
            if let Some(u) = self.user(&p.user) {
                let col = idx % per_row;
                let row = idx / per_row;
                let cc = Point::new(
                    panel.x0 + 14.0 + radius + col as f64 * step,
                    chips_y + radius + row as f64 * step,
                );
                // Speaking ring on whoever is talking.
                if p.talking {
                    stroke_circle(scene, tf, cc, radius + 3.0, c.semantic.success, 2.0);
                }
                draw_avatar(
                    scene,
                    tf,
                    tr,
                    theme,
                    cc,
                    radius,
                    &u.initials,
                    u.color_idx,
                    None,
                );
                if p.muted {
                    fill_circle(
                        scene,
                        tf,
                        Point::new(cc.x + radius * 0.72, cc.y + radius * 0.72),
                        6.5,
                        c.semantic.danger,
                    );
                } else if p.audio_only {
                    fill_circle(
                        scene,
                        tf,
                        Point::new(cc.x + radius * 0.72, cc.y + radius * 0.72),
                        6.5,
                        c.neutral[3],
                    );
                }
            }
        }
    }

    /// Controls row, evenly distributed across the panel so it never
    /// overflows a small (narrow-window) huddle. On a cramped panel the
    /// buttons drop to icons and Expand is trimmed (the header "In call"
    /// button also expands the call), so the row always fits.
    fn draw_huddle_controls(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let cy = panel.y1 - 34.0;
        let pad = 12.0;
        let gap = 6.0;
        let muted = self.call.muted;
        let deaf = self.call.deafened;
        let mut ctrls: Vec<(&str, &str, bool, bool, Hit)> = vec![
            (
                if muted { "Unmute" } else { "Mute" },
                if muted { "🔇" } else { "🎤" },
                muted,
                false,
                Hit::CallMute,
            ),
            (
                if deaf { "Undeafen" } else { "Deafen" },
                if deaf { "🔕" } else { "🎧" },
                deaf,
                false,
                Hit::CallDeafen,
            ),
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
            let hovered = self.call_control_is_hovered(&hit);
            Self::ctl(
                scene,
                tf,
                tr,
                theme,
                r,
                if use_text { txt } else { icon },
                on,
                danger,
                hovered,
                true,
            );
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
    ) {
        let c = &theme.tokens.colors;
        // Opaque backdrop so the expanded call reads as its own focused
        // surface (the app base, darkened by the overlay tint) rather than a
        // thin scrim with the conversation showing through behind the tiles.
        fill_rect(scene, tf, self.rect, c.surfaces.base);
        fill_rect(scene, tf, self.rect, c.surfaces.overlay);

        // title + participant count
        let title = self
            .channel(&self.call.channel)
            .map_or_else(|| "Call".into(), |c| c.name.clone());
        let (tw, _) = tr.measure(&title, 20.0, None);
        text(
            scene,
            tr,
            tf,
            &title,
            20.0,
            c.text_primary,
            self.rect.center().x - f64::from(tw) / 2.0,
            self.rect.y0 + 28.0,
            TextWeight::Bold,
            None,
        );
        let parts = self.call_participants();
        let sharer = self.screen_sharer();
        // Subtitle carries the headcount and, when someone is presenting, whose
        // screen is on the stage — surfaced the instant the shared voice
        // membership records a share, before any frame arrives.
        let sub = match &sharer {
            Some(id) => {
                let who = if *id == self.me.id {
                    "You are".to_string()
                } else {
                    format!("{} is", self.user_name(id))
                };
                format!("{} in the call · {who} sharing a screen", parts.len())
            }
            None => format!("{} in the call", parts.len()),
        };
        let (sw, _) = tr.measure(&sub, 12.0, None);
        label(
            scene,
            tr,
            tf,
            &sub,
            12.0,
            c.text_dim,
            self.rect.center().x - f64::from(sw) / 2.0,
            self.rect.y0 + 56.0,
        );

        // The host active-speaker treatment is shared by both layouts: a bright
        // ring while transmitting, a hairline otherwise.
        let (ring_col, ring_w) = if self.call.muted {
            (
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 51),
                1.5,
            )
        } else {
            (c.semantic.success, 2.5)
        };
        match self.overlay_stage() {
            // No share: host + remotes share one responsive grid that fills the
            // whole band between the header and the controls bar — the host is
            // simply the first cell. The VideoPlayer surface is drawn by the app
            // on top of the host cell.
            CallStage::Grid(tiles) => {
                if let Some(&host) = tiles.first() {
                    stroke_round(scene, tf, host, 14.0, ring_col, ring_w);
                }
                for (p, rect) in parts.iter().skip(1).zip(tiles.iter().skip(1)) {
                    self.draw_member_tile(scene, tf, tr, theme, *rect, p);
                }
            }
            // Someone is presenting: a large featured screen tile fills the band
            // above a single-row participant filmstrip. The host keeps its own
            // (camera) cell as the first strip tile; remotes follow.
            CallStage::Share { screen, strip } => {
                if let Some(sharer) = &sharer {
                    self.draw_screen_tile(scene, tf, tr, theme, screen, sharer);
                }
                if let Some(&host) = strip.first() {
                    stroke_round(scene, tf, host, 12.0, ring_col, ring_w);
                }
                for (p, rect) in parts.iter().skip(1).zip(strip.iter().skip(1)) {
                    self.draw_member_tile(scene, tf, tr, theme, *rect, p);
                }
            }
        }
    }

    /// The featured presentation tile: a peer's real decoded screen frame when
    /// one has arrived, and an honest "connecting" placeholder — a drawn monitor
    /// with the sharer's name, never a fabricated picture — until then. A live
    /// pill marks the tile as the active share.
    fn draw_screen_tile(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        sharer: &str,
    ) {
        let c = &theme.tokens.colors;
        let name = self
            .user(sharer)
            .map_or_else(|| sharer.to_string(), |u| u.name.clone());
        let is_me = sharer == self.me.id;
        let radius = 16.0;

        // A real decoded screen frame from the sharer takes over the tile. This
        // slot is only ever filled by the live media pump with a genuinely
        // decoded frame, so when present it is an honest live view of the shared
        // screen; otherwise we fall through to the drawn placeholder.
        if let Some(img) = self.remote_screens.get(sharer) {
            fill_round(scene, tf, rect, radius, Color::from_rgba8(0, 0, 0, 255));
            ui::image_store::paint_image_cover(scene, tf, rect, img, radius);
            Self::screen_live_pill(scene, tf, tr, theme, rect, &name, is_me);
            stroke_round(scene, tf, rect, radius, c.accent[5], 2.0);
            return;
        }

        // Honest placeholder — no fabricated screen pixels. A deep neutral tile
        // with a soft accent-tinted inner panel for depth, a crisply drawn
        // monitor glyph, and truthful status text.
        fill_round(scene, tf, rect, radius, c.neutral[1]);
        let margin = (rect.width().min(rect.height()) * 0.14).min(90.0);
        let inner = rect.inflate(-margin, -margin);
        fill_round(scene, tf, inner, radius, c.lift(0.35));
        stroke_round(
            scene,
            tf,
            inner,
            radius,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );

        // Drawn monitor icon, centred a touch above the middle.
        let gw = (rect.height() * 0.20).clamp(44.0, 120.0);
        let gh = gw * 0.62;
        let gcx = rect.center().x;
        let gcy = rect.center().y - gh * 0.5;
        let screen_r = Rect::new(
            gcx - gw / 2.0,
            gcy - gh / 2.0,
            gcx + gw / 2.0,
            gcy + gh / 2.0,
        );
        fill_round(scene, tf, screen_r, 8.0, c.neutral[3]);
        stroke_round(scene, tf, screen_r, 8.0, c.accent[5], 2.0);
        // Stand: a small neck + base bar beneath the screen.
        let neck = Rect::new(
            gcx - gw * 0.06,
            screen_r.y1,
            gcx + gw * 0.06,
            screen_r.y1 + gh * 0.16,
        );
        fill_round(scene, tf, neck, 2.0, c.neutral[3]);
        let base = Rect::new(
            gcx - gw * 0.22,
            neck.y1,
            gcx + gw * 0.22,
            neck.y1 + gh * 0.09,
        );
        fill_round(scene, tf, base, 3.0, c.neutral[3]);

        // Status text below the glyph.
        let head = if is_me {
            "You're sharing your screen".to_string()
        } else {
            format!("{name} is sharing a screen")
        };
        let (hw, _) = tr.measure(&head, 15.0, None);
        text(
            scene,
            tr,
            tf,
            &head,
            15.0,
            c.text_primary,
            gcx - f64::from(hw) / 2.0,
            base.y1 + 18.0,
            TextWeight::Bold,
            None,
        );
        // Honest status: the local sharer is the source; a remote tile is
        // waiting on the first decoded frame (this branch only runs with no
        // frame present, so it never claims to be showing video it lacks).
        let sub = if is_me {
            "Everyone in the call sees your screen"
        } else {
            "Connecting to the shared screen…"
        };
        let (subw, _) = tr.measure(sub, 12.0, None);
        label(
            scene,
            tr,
            tf,
            sub,
            12.0,
            c.text_dim,
            gcx - f64::from(subw) / 2.0,
            base.y1 + 40.0,
        );

        Self::screen_live_pill(scene, tf, tr, theme, rect, &name, is_me);
        stroke_round(scene, tf, rect, radius, c.accent[5], 2.0);
    }

    /// A small "● Live · <name>" pill in the top-left of the featured screen
    /// tile, marking which peer's screen is on the stage.
    fn screen_live_pill(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        name: &str,
        is_me: bool,
    ) {
        let c = &theme.tokens.colors;
        let who = if is_me {
            "Your screen".to_string()
        } else {
            format!("{name}'s screen")
        };
        let txt = format!("Live · {who}");
        let (lw, _) = tr.measure(&txt, 11.5, None);
        let pill = Rect::new(
            rect.x0 + 12.0,
            rect.y0 + 12.0,
            rect.x0 + 12.0 + f64::from(lw) + 34.0,
            rect.y0 + 12.0 + 24.0,
        );
        fill_round(scene, tf, pill, 12.0, Color::from_rgba8(0, 0, 0, 150));
        fill_circle(
            scene,
            tf,
            Point::new(pill.x0 + 14.0, pill.center().y),
            4.0,
            c.semantic.danger,
        );
        label(
            scene,
            tr,
            tf,
            &txt,
            11.5,
            Color::WHITE,
            pill.x0 + 24.0,
            pill.center().y - 8.0,
        );
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
        Self::name_plate(scene, tf, tr, theme, host, "You (host)", self.call.muted);

        // Controls bar: auto-sized to its buttons and wrapped onto extra rows
        // when the window is too narrow to hold them all in one line, so it
        // never runs off either edge.
        let (bar, buttons) = self.overlay_controls_layout();
        fill_round(scene, tf, bar, 16.0, c.neutral[1]);
        stroke_round(
            scene,
            tf,
            bar,
            16.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        for (label_txt, on, danger, enabled, hit, r) in buttons {
            let hovered = self.call_control_is_hovered(&hit);
            Self::ctl(
                scene, tf, tr, theme, r, label_txt, on, danger, hovered, enabled,
            );
            if enabled {
                ChatApp::push_hit(hits, r, hit);
            }
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

    /// The user id of whoever is sharing their screen into the current call, if
    /// anyone. This peer's own share wins the featured tile; otherwise the first
    /// remote member the shared voice membership marks as sharing. This is the
    /// switch between the uniform grid and the featured presentation stage.
    pub(crate) fn screen_sharer(&self) -> Option<String> {
        if self.call.sharing {
            return Some(self.me.id.clone());
        }
        self.voice_in(&self.call.channel)
            .iter()
            .find(|m| m.sharing && m.user != self.me.id)
            .map(|m| m.user.clone())
    }

    /// The expanded overlay's layout for the current call: a uniform participant
    /// grid, or — while someone is screen-sharing — a large featured screen tile
    /// above a single-row participant filmstrip. Deterministic (no text
    /// measurement) so the tile geometry, the host [`VideoPlayer`] node built in
    /// `build_layout`, and every draw pass resolve identically on one frame.
    pub(crate) fn overlay_stage(&self) -> CallStage {
        let region = self.overlay_content_region();
        let n = self.call_participants().len().max(1);
        if self.screen_sharer().is_some() {
            let gap = 14.0;
            // The screen dominates; participants ride a strip beneath it whose
            // height eases up a little with the headcount but stays a minority
            // of the surface. Clamped so a short window still leaves a usable
            // strip and a tall one doesn't over-grow it.
            let strip_h = (region.height() * 0.24)
                .clamp(88.0, 190.0)
                .min(region.height() * 0.4);
            let screen_bottom = (region.y1 - strip_h - gap).max(region.y0 + 1.0);
            let screen = Rect::new(region.x0, region.y0, region.x1, screen_bottom);
            let strip_region = Rect::new(region.x0, screen.y1 + gap, region.x1, region.y1);
            CallStage::Share {
                screen,
                strip: filmstrip_rects(strip_region, n),
            }
        } else {
            CallStage::Grid(self.overlay_tiles())
        }
    }

    /// The host tile rect in the expanded overlay: the first grid cell, or — in
    /// the presentation stage — the host's first filmstrip cell. The single
    /// source of truth the host [`VideoPlayer`] surface tracks in both modes.
    pub(crate) fn overlay_host_rect(&self) -> Rect {
        match self.overlay_stage() {
            CallStage::Grid(tiles) => tiles
                .into_iter()
                .next()
                .unwrap_or_else(|| self.overlay_content_region()),
            CallStage::Share { strip, screen } => strip.into_iter().next().unwrap_or(screen),
        }
    }

    /// Lay out the expanded-overlay control buttons, wrapping onto extra rows
    /// to fit the current window width. Returns the bar background rect and
    /// each button's `(label, active, danger, enabled, hit, rect)`. Shared by the
    /// background pass (which bounds the tile region above the bar), the
    /// foreground pass (which draws it), and the layout pass (which sizes the
    /// tile band) — so all three agree without a `TextRenderer`. Button widths
    /// are derived from the fixed label lengths rather than measured, which
    /// keeps the whole call layout resolvable on a single headless frame.
    #[allow(
        clippy::too_many_lines,
        reason = "one deterministic layout pass must keep wrapped call controls, their hit targets and the returned bar bounds identical"
    )]
    pub(crate) fn overlay_controls_layout(&self) -> (Rect, Vec<OverlayButton>) {
        let specs: [(&'static str, bool, bool, bool, Hit); 5] = [
            (
                if self.call.muted { "Unmute" } else { "Mute" },
                self.call.muted,
                false,
                self.call_allows(crate::call::CallAction::Speak),
                Hit::CallMute,
            ),
            (
                if self.call.deafened {
                    "Undeafen"
                } else {
                    "Deafen"
                },
                self.call.deafened,
                false,
                self.call_allows(crate::call::CallAction::Speak),
                Hit::CallDeafen,
            ),
            (
                "Audio only",
                self.call.audio_only,
                false,
                self.call_allows(crate::call::CallAction::Camera),
                Hit::CallAudioOnly,
            ),
            ("Collapse", false, false, true, Hit::CallExpand),
            ("Leave", false, true, true, Hit::CallLeave),
        ];
        let gap = 8.0;
        let row_gap = 8.0;
        let bh = 32.0;
        let side_pad = 24.0;
        let inner_pad = 12.0;
        let max_row_w = (self.rect.width() - 2.0 * side_pad).max(120.0);
        let widths: Vec<f64> = specs.iter().map(|s| overlay_button_width(s.0)).collect();

        // Greedy pack into rows that each fit `max_row_w`. `rows` always holds
        // at least one (possibly empty) row, so `last`/`last_mut` never miss —
        // still routed through `let...else` rather than `unwrap` in case that
        // invariant is ever broken by a future edit.
        let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
        let mut cur = 0.0;
        for (i, w) in widths.iter().enumerate() {
            let Some(last) = rows.last() else { break };
            let extra = if last.is_empty() { 0.0 } else { gap } + w;
            if !last.is_empty() && cur + extra > max_row_w {
                rows.push(Vec::new());
                cur = 0.0;
            }
            let Some(last) = rows.last() else { break };
            let extra = if last.is_empty() { 0.0 } else { gap } + w;
            let Some(last_mut) = rows.last_mut() else {
                break;
            };
            last_mut.push(i);
            cur += extra;
        }

        let n_rows = rows.len() as f64;
        let bar_h = n_rows * bh + (n_rows - 1.0) * row_gap + 2.0 * inner_pad;
        let widest = rows
            .iter()
            .map(|row| {
                row.iter().filter_map(|&i| widths.get(i)).sum::<f64>()
                    + row.len().saturating_sub(1) as f64 * gap
            })
            .fold(0.0_f64, f64::max);

        let cx = self.rect.center().x;
        let bar_bottom = self.rect.y1 - 18.0;
        let bar_top = bar_bottom - bar_h;
        let bar = Rect::new(
            cx - widest / 2.0 - inner_pad,
            bar_top,
            cx + widest / 2.0 + inner_pad,
            bar_bottom,
        );

        let mut out: Vec<OverlayButton> = Vec::with_capacity(specs.len());
        let mut ry = bar_top + inner_pad;
        for row in &rows {
            let rw = row.iter().filter_map(|&i| widths.get(i)).sum::<f64>()
                + row.len().saturating_sub(1) as f64 * gap;
            let mut x = cx - rw / 2.0;
            for &i in row {
                let (Some(&w), Some(spec)) = (widths.get(i), specs.get(i)) else {
                    continue;
                };
                let r = Rect::new(x, ry, x + w, ry + bh);
                let (label_txt, on, danger, enabled, hit) = spec;
                out.push((*label_txt, *on, *danger, *enabled, hit.clone(), r));
                x += w + gap;
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
        // A real decoded camera/screen frame from this peer takes over the tile.
        // This is only ever populated by the live media pump with a genuinely
        // decoded frame — never a fabricated picture — so when present it is an
        // honest live feed; otherwise we fall through to the avatar placeholder.
        if let Some(img) = self.remote_frames.get(&p.user) {
            ui::image_store::paint_image_cover(scene, tf, rect, img, 14.0);
            if p.talking {
                stroke_round(scene, tf, rect, 14.0, c.semantic.success, 2.5);
            }
            let name = u.map(|u| u.name.clone()).unwrap_or_default();
            Self::name_plate(scene, tf, tr, theme, rect, &name, p.muted);
            if p.sharing {
                Self::presenter_badge(scene, tf, theme, rect);
            }
            stroke_round(
                scene,
                tf,
                rect,
                14.0,
                ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
                1.0,
            );
            return;
        }
        if let Some(u) = u {
            let radius = (rect.height() * 0.22).clamp(16.0, 46.0);
            let cx = rect.center().x;
            let cy = rect.center().y - radius * 0.3;
            draw_avatar(
                scene,
                tf,
                tr,
                theme,
                Point::new(cx, cy),
                radius,
                &u.initials,
                u.color_idx,
                None,
            );
            // Honest state under the avatar — the same treatment the host tile
            // gets. There is no remote camera capture in this environment, so
            // the tile never fabricates a picture: an audio-only peer reads
            // "Camera off"; a camera-enabled peer whose frame doesn't reach
            // this peer reads "No video". It never claims to show video it
            // doesn't have.
            let s = member_tile_caption(p);
            let (sw, _) = tr.measure(s, 11.5, None);
            label(
                scene,
                tr,
                tf,
                s,
                11.5,
                c.text_dim,
                cx - f64::from(sw) / 2.0,
                cy + radius + 8.0,
            );
        }
        if p.talking {
            stroke_round(scene, tf, rect, 14.0, c.semantic.success, 2.5);
        }
        let name = u.map(|u| u.name.clone()).unwrap_or_default();
        Self::name_plate(scene, tf, tr, theme, rect, &name, p.muted);
        if p.sharing {
            Self::presenter_badge(scene, tf, theme, rect);
        }
        stroke_round(
            scene,
            tf,
            rect,
            14.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
    }

    /// A small monitor badge in a participant tile's top-right marking them as
    /// the one presenting, so the filmstrip communicates who is on the stage.
    fn presenter_badge(scene: &mut Scene, tf: Affine, theme: &Theme, rect: Rect) {
        let c = &theme.tokens.colors;
        let s = 12.0;
        let cx = rect.x1 - 8.0 - s / 2.0;
        let cy = rect.y0 + 8.0 + s / 2.0;
        let chip = Rect::new(
            cx - s / 2.0 - 4.0,
            cy - s / 2.0 - 4.0,
            cx + s / 2.0 + 4.0,
            cy + s / 2.0 + 4.0,
        );
        fill_round(scene, tf, chip, 6.0, c.accent[5]);
        // A tiny drawn monitor: screen rect + base bar.
        let screen = Rect::new(cx - s / 2.0, cy - s / 2.0, cx + s / 2.0, cy + s / 2.0 - 3.0);
        stroke_round(scene, tf, screen, 2.0, Color::WHITE, 1.4);
        let base = Rect::new(cx - 3.0, screen.y1 + 0.5, cx + 3.0, screen.y1 + 2.0);
        fill_round(scene, tf, base, 1.0, Color::WHITE);
    }

    /// Bottom-left name plate over a tile: name + mic state.
    fn name_plate(
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
        let plate = Rect::new(
            tile.x0 + 8.0,
            tile.y1 - 30.0,
            tile.x0 + 8.0 + f64::from(lw) + 20.0,
            tile.y1 - 8.0,
        );
        fill_round(scene, tf, plate, 8.0, Color::from_rgba8(0, 0, 0, 150));
        let col = if muted {
            c.semantic.danger
        } else {
            Color::WHITE
        };
        label(
            scene,
            tr,
            tf,
            &label_txt,
            12.0,
            col,
            plate.x0 + 10.0,
            plate.center().y - 8.0,
        );
    }

    /// A pill control button. `on` = active/highlighted, `danger` = red.
    #[allow(clippy::too_many_arguments)]
    fn ctl(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label_txt: &str,
        on: bool,
        danger: bool,
        hovered: bool,
        enabled: bool,
    ) {
        let c = &theme.tokens.colors;
        let (bg, fg) = if !enabled {
            (
                c.lift(0.72).with_alpha_factor(0.45),
                c.text_dim.with_alpha_factor(0.45),
            )
        } else if danger && hovered {
            (c.semantic.danger, Color::WHITE)
        } else if danger {
            (c.semantic.danger.with_alpha_factor(0.16), c.semantic.danger)
        } else if on {
            (c.accent[5], Color::WHITE)
        } else if hovered {
            (c.lift(1.0), c.text_primary)
        } else {
            (c.lift(0.72), c.text_primary)
        };
        fill_round(scene, tf, rect, 9.0, bg);
        if hovered {
            stroke_round(
                scene,
                tf,
                rect,
                9.0,
                if danger {
                    c.semantic.danger
                } else {
                    ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 51)
                },
                1.0,
            );
        }
        let (lw, _) = tr.measure(label_txt, 13.0, None);
        label(
            scene,
            tr,
            tf,
            label_txt,
            13.0,
            fg,
            rect.center().x - f64::from(lw) / 2.0,
            rect.center().y - 8.0,
        );
    }

    fn call_control_is_hovered(&self, hit: &Hit) -> bool {
        self.call_hovered
            .as_ref()
            .is_some_and(|hovered| std::mem::discriminant(hovered) == std::mem::discriminant(hit))
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
            rows_total += 1 + cat.emoji.len().div_ceil(cols); // label + grid rows
        }
        let h = 56.0 + rows_total as f64 * cell + 12.0;
        let mut x0 = self.emoji_anchor.x;
        let mut y0 = self.emoji_anchor.y;
        x0 = x0.clamp(self.rect.x0 + 8.0, self.rect.x1 - w - 8.0);
        y0 = y0.clamp(
            self.rect.y0 + 8.0,
            (self.rect.y1 - h - 8.0).max(self.rect.y0 + 8.0),
        );
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        fill_round(
            scene,
            tf,
            panel.inflate(5.0, 5.0),
            18.0,
            Color::from_rgba8(0, 0, 0, 70),
        );
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(
            scene,
            tf,
            panel,
            14.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );

        let title = if self.emoji_target.is_some() {
            "Add reaction"
        } else {
            "Emoji"
        };
        label(
            scene,
            tr,
            tf,
            title,
            13.0,
            c.text_primary,
            panel.x0 + 14.0,
            panel.y0 + 12.0,
        );

        let mut y = panel.y0 + 36.0;
        // quick row
        label(
            scene,
            tr,
            tf,
            "Frequent",
            10.5,
            c.text_dim,
            panel.x0 + 14.0,
            y,
        );
        y += 16.0;
        Self::emoji_grid(
            scene,
            tf,
            tr,
            theme,
            &emoji::QUICK,
            panel.x0 + 12.0,
            &mut y,
            cols,
            cell,
            hits,
        );
        for cat in emoji::CATEGORIES {
            label(
                scene,
                tr,
                tf,
                cat.name,
                10.5,
                c.text_dim,
                panel.x0 + 14.0,
                y,
            );
            y += 16.0;
            Self::emoji_grid(
                scene,
                tf,
                tr,
                theme,
                cat.emoji,
                panel.x0 + 12.0,
                &mut y,
                cols,
                cell,
                hits,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emoji_grid(
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
            text(
                scene,
                tr,
                tf,
                e,
                19.0,
                c.text_primary,
                cellr.center().x - f64::from(ew) / 2.0,
                cellr.center().y - f64::from(eh) / 2.0,
                TextWeight::Regular,
                None,
            );
            ChatApp::push_hit(hits, cellr, Hit::EmojiChoice((*e).to_string()));
        }
        let rows = set.len().div_ceil(cols);
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
        let Some(mid) = self.history_open.clone() else {
            return;
        };
        let Some(m) = self.message(&mid).cloned() else {
            return;
        };

        let w = 380.0;
        let tw = (w - 28.0) as f32;
        // measure — each row's rendered body height is carried alongside its
        // text so the draw loop never has to re-index back into a parallel
        // heights vector.
        let mut entries: Vec<(String, String)> =
            vec![(format!("Now · {}", model::clock(m.ts)), m.text.clone())];
        for rec in m.history.iter().rev() {
            entries.push((
                format!("Edited · {}", model::clock(rec.ts)),
                rec.text.clone(),
            ));
        }
        let mut total = 44.0;
        let rows: Vec<(String, String, f64)> = entries
            .into_iter()
            .map(|(head, body)| {
                let bh = f64::from(tr.measure(&body, 13.0, Some(tw)).1) + 22.0 + 14.0;
                total += bh;
                (head, body, bh)
            })
            .collect();
        let mut x0 = self.history_anchor.x;
        let mut y0 = self.history_anchor.y;
        x0 = x0.clamp(self.rect.x0 + 8.0, self.rect.x1 - w - 8.0);
        y0 = y0.clamp(
            self.rect.y0 + 8.0,
            (self.rect.y1 - total - 8.0).max(self.rect.y0 + 8.0),
        );
        let panel = Rect::new(x0, y0, x0 + w, y0 + total);
        fill_round(
            scene,
            tf,
            panel.inflate(5.0, 5.0),
            18.0,
            Color::from_rgba8(0, 0, 0, 70),
        );
        fill_round(scene, tf, panel, 14.0, c.neutral[1]);
        stroke_round(
            scene,
            tf,
            panel,
            14.0,
            ui::tokens::colors::Colors::hairline_on(c.surfaces.raised, 26),
            1.0,
        );
        label(
            scene,
            tr,
            tf,
            "Edit history",
            13.0,
            c.text_primary,
            panel.x0 + 14.0,
            panel.y0 + 12.0,
        );

        let mut y = panel.y0 + 38.0;
        let row_count = rows.len();
        for (i, (head, body, bh)) in rows.iter().enumerate() {
            let accent = i == 0;
            label(
                scene,
                tr,
                tf,
                head,
                11.0,
                if accent { c.accent[6] } else { c.text_dim },
                panel.x0 + 14.0,
                y,
            );
            y += 18.0;
            text(
                scene,
                tr,
                tf,
                body,
                13.0,
                if accent { c.text_primary } else { c.text_dim },
                panel.x0 + 14.0,
                y,
                TextWeight::Regular,
                Some(tw),
            );
            y += bh - 22.0 - 14.0 + 8.0;
            if i + 1 < row_count {
                hline(
                    scene,
                    tf,
                    panel.x0 + 12.0,
                    panel.x1 - 12.0,
                    y,
                    c.border_standard,
                );
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

/// Lay `n` participant tiles into a single centred row filling `region`'s
/// height — the presentation-stage filmstrip beneath the featured screen. Each
/// tile is landscape-ish (capped at an equal share of the row width) and every
/// returned rect stays inside `region`, so the strip never spills past the
/// window at any headcount.
fn filmstrip_rects(region: Rect, n: usize) -> Vec<Rect> {
    if n == 0 || region.width() <= 1.0 || region.height() <= 1.0 {
        return vec![];
    }
    let gap = 12.0;
    let h = region.height();
    // An equal share of the row is the ceiling; prefer a landscape tile but
    // never wider than that share, so the row always fits.
    let share = ((region.width() - (n as f64 - 1.0) * gap) / n as f64).max(1.0);
    let w = share.min(h * 1.5).max(1.0);
    let row_w = n as f64 * w + (n as f64 - 1.0) * gap;
    let x0 = region.x0 + (region.width() - row_w) / 2.0;
    (0..n)
        .map(|i| {
            let x = x0 + i as f64 * (w + gap);
            Rect::new(x, region.y0, x + w, region.y0 + h)
        })
        .collect()
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
        let in_row = if row == rows - 1 {
            n - row * cols
        } else {
            cols
        };
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
    use super::{
        Participant, filmstrip_rects, grid_rects, huddle_lower_chrome_for_reveal,
        member_tile_caption,
    };
    use vello::kurbo::Rect;

    #[test]
    fn participant_reveal_morphs_the_complete_lower_chrome_monotonically() {
        let samples = [0.0, 0.25, 0.5, 0.75, 1.0].map(huddle_lower_chrome_for_reveal);
        assert_eq!(samples[0], 116.0);
        assert_eq!(samples[2], 136.0);
        assert_eq!(samples[4], 156.0);
        assert!(samples.windows(2).all(|pair| pair[0] < pair[1]));
    }

    /// The presentation-stage filmstrip must lay every participant in a single
    /// centred row that stays inside its region, at any headcount — the
    /// invariant the screen-share stage leans on so the strip never spills.
    #[test]
    fn filmstrip_is_a_single_centred_row_inside_its_region() {
        for &(w, h) in &[(1200.0_f64, 180.0_f64), (700.0, 150.0), (500.0, 120.0)] {
            let region = Rect::new(40.0, 600.0, 40.0 + w, 600.0 + h);
            for n in 1..=8 {
                let tiles = filmstrip_rects(region, n);
                assert_eq!(tiles.len(), n, "n={n}: wrong tile count");
                for t in &tiles {
                    assert!(
                        t.x0 >= region.x0 - 0.5 && t.x1 <= region.x1 + 0.5,
                        "n={n} w={w}: tile escaped the strip horizontally"
                    );
                    // One row: every tile spans the full strip height.
                    assert!(
                        (t.y0 - region.y0).abs() < 0.5 && (t.y1 - region.y1).abs() < 0.5,
                        "n={n}: strip tiles are not a single full-height row"
                    );
                    assert!(t.width() > 0.0, "n={n}: degenerate strip tile");
                }
                // Centred: equal slack on both sides of the row.
                let left = tiles.first().unwrap().x0 - region.x0;
                let right = region.x1 - tiles.last().unwrap().x1;
                assert!(
                    (left - right).abs() < 1.0,
                    "n={n} w={w}: strip is not centred"
                );
            }
        }
    }

    #[test]
    fn filmstrip_is_empty_for_no_participants() {
        assert!(filmstrip_rects(Rect::new(0.0, 0.0, 400.0, 120.0), 0).is_empty());
    }

    fn participant(audio_only: bool) -> Participant {
        Participant {
            user: "u_x".into(),
            talking: false,
            muted: false,
            audio_only,
            sharing: false,
        }
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
                        r.x0,
                        r.x1,
                        region.x0,
                        region.x1
                    );
                    assert!(
                        r.y0 >= region.y0 - 0.5 && r.y1 <= region.y1 + 0.5,
                        "n={n} w={w}: tile y [{:.1},{:.1}] escaped region y [{:.1},{:.1}]",
                        r.y0,
                        r.y1,
                        region.y0,
                        region.y1
                    );
                    assert!(
                        r.width() > 0.0 && r.height() > 0.0,
                        "n={n}: degenerate tile"
                    );
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
            let bbox =
                tiles
                    .iter()
                    .fold(Rect::new(f64::MAX, f64::MAX, f64::MIN, f64::MIN), |b, r| {
                        Rect::new(
                            b.x0.min(r.x0),
                            b.y0.min(r.y0),
                            b.x1.max(r.x1),
                            b.y1.max(r.y1),
                        )
                    });
            // One inter-tile gap is the largest legitimate slack (a centred row
            // or column can sit a gap in from an edge).
            let gap = (region.width().min(region.height()) * 0.05).clamp(6.0, 16.0);
            let slack = gap + 1.0;
            assert!(
                bbox.x0 - region.x0 <= slack && region.x1 - bbox.x1 <= slack,
                "w={w}: tiles leave a horizontal void (bbox x [{:.1},{:.1}] vs region [{:.1},{:.1}])",
                bbox.x0,
                bbox.x1,
                region.x0,
                region.x1
            );
            assert!(
                bbox.y0 - region.y0 <= slack && region.y1 - bbox.y1 <= slack,
                "w={w}: tiles leave a vertical void (bbox y [{:.1},{:.1}] vs region [{:.1},{:.1}])",
                bbox.y0,
                bbox.y1,
                region.y0,
                region.y1
            );
            // And a tile actually occupies the lower-left quadrant — the exact
            // corner the old layout left black.
            let mid = (
                region.x0 + region.width() * 0.5,
                region.y0 + region.height() * 0.5,
            );
            assert!(
                tiles
                    .iter()
                    .any(|r| r.center().x < mid.0 && r.center().y > mid.1),
                "w={w}: nothing fills the lower-left of the call surface"
            );
        }
    }
}
