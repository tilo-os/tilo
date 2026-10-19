//! Rendering for the conferencing layers: the docked call "huddle" `PiP`, the
//! expanded call overlay (a responsive participant grid, or — while someone is
//! screen-sharing — a featured screen tile above a participant filmstrip), and
//! the two floating popovers (emoji picker + edit history). The host tile is a
//! real [`ui::widget::VideoPlayer`] laid out by the app; everything here is the
//! chrome around it.
//!
//! Tiles are media: an empty viewfinder is the media letterbox, and chrome
//! over a tile (a name, a status) stands on the floating material. Controls
//! are the toolkit's buttons and chips.
//!
//! Screen-share honesty: the featured tile shows a peer's real decoded screen
//! frame when one has arrived (`remote_screens`) and an honest "connecting"
//! placeholder — never a fabricated picture — until then.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};

use ui::floating_surface::{Material, paint_material};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Button, ButtonFace, ButtonRole, ChipFace, ChipLead, ChipStyle, ControlSize, SegmentedFace,
    SegmentedStyle, ToggleGroup, Widget,
};

use crate::app::{ChatApp, Hit};
use crate::model::{self, VoiceMember};
use crate::paint::{elide, line_in, line_of, push_clip, text, width_of};

/// One person to render in the call, derived from the shared CRDT voice
/// membership (not hardcoded). The host (this peer) is listed first.
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about one participant (talking, muted, audio only, sharing), each read on its own"
)]
struct Participant {
    user: String,
    /// The call hears them now: their face rings.
    talking: bool,
    muted: bool,
    audio_only: bool,
    sharing: bool,
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
type OverlayButton = (String, bool, bool, bool, Hit, Rect);

/// Catalogue keys of every label the controls bar can show, both halves of
/// each toggle pair included, so the measurement covers a label before the
/// toggle that reveals it.
const CALL_CONTROL_KEYS: [&str; 11] = [
    "chat-call-mute",
    "chat-call-unmute",
    "chat-call-deafen",
    "chat-call-undeafen",
    "chat-call-audio-only",
    "chat-call-camera-on",
    "chat-call-camera-off",
    "chat-call-share-screen",
    "chat-call-stop-sharing",
    "chat-call-collapse",
    "chat-call-leave",
];

/// Measured sizes the expanded call overlay is laid out with.
///
/// The overlay's geometry (`overlay_controls_layout`, `overlay_content_region`)
/// is read from `&self` contexts that have no renderer — `build_layout` sizes
/// the host `VideoPlayer` tile from it and the pointer path hit-tests against
/// it — so the measurement happens once per paint, in the `&mut self` pass
/// that precedes drawing, and is cached here. Before the first paint the
/// theme's unmeasured values stand in (`Default`), which is also what the
/// geometry tests exercise.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CallOverlayMetrics {
    /// `(label, width)` for every control the bar may show: the button's own
    /// measured width with its icon.
    pub button_w: Vec<(String, f64)>,
    /// Height of a control button.
    pub button_h: f64,
    /// Gap between buttons and between wrapped rows (`s2`).
    pub gap: f64,
    /// Inner padding of the controls bar (`s2`).
    pub inner_pad: f64,
    /// Side inset the wrapped rows must fit inside (`s6`).
    pub side_pad: f64,
    /// Gap between the bar and the window's bottom edge (`s4`).
    pub bottom_pad: f64,
    /// Measured line box of the header's title.
    pub title_h: f64,
    /// Space above (`s6`) and below (`s4`) the title.
    pub header_pad_top: f64,
    pub header_pad_bottom: f64,
    /// What an unmeasured label's button adds to the label: its icon, the
    /// gap after it and the side padding.
    pub button_chrome_w: f64,
    /// The size of a control label, for estimating an unmeasured one.
    pub label_size: f64,
    /// Gap between participant tiles (`s3`).
    pub tile_gap: f64,
    /// The shortest the screen-share filmstrip gets: one tile's disc,
    /// caption and name plate with the air between them.
    pub strip_min_h: f64,
}

impl CallOverlayMetrics {
    /// The overlay's sizes in `theme` before anything is measured: the
    /// spacing steps, the control height and the title's line box.
    pub(crate) fn of(theme: &Theme) -> Self {
        let sp = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        Self {
            button_w: Vec::new(),
            button_h: ButtonFace::height(ControlSize::Regular, theme),
            gap: f64::from(sp.s2),
            inner_pad: f64::from(sp.s2),
            side_pad: f64::from(sp.s6),
            bottom_pad: f64::from(sp.s4),
            title_h: f64::from(theme.text(TextRole::Title).line_height_px()),
            header_pad_top: f64::from(sp.s6),
            header_pad_bottom: f64::from(sp.s4),
            button_chrome_w: f64::from(metrics.icon_md)
                + f64::from(metrics.icon_label_gap)
                + 2.0 * f64::from(sp.s3),
            label_size: f64::from(theme.text(TextRole::Label).size),
            tile_gap: f64::from(sp.s3),
            strip_min_h: ui::widget::AvatarSize::Regular.side(theme)
                + f64::from(theme.text(TextRole::Caption).line_height_px())
                + plate_height(theme)
                + 4.0 * f64::from(sp.s2),
        }
    }

    /// Width of the control labelled `label`: its measured width when this
    /// frame measured it, else an estimate from the label's length (so the
    /// geometry tests, which never paint, still resolve).
    fn button_width(&self, label: &str) -> f64 {
        self.button_w
            .iter()
            .find(|(l, _)| l.as_str() == label)
            .map_or_else(|| self.estimated_button_width(label), |(_, w)| *w)
    }

    /// A measurement-free width for a control: about half an em per
    /// character of the label, plus the button's icon and padding.
    fn estimated_button_width(&self, label: &str) -> f64 {
        label.chars().count() as f64 * self.label_size / 2.0 + self.button_chrome_w
    }

    /// Height of the title block above the tile band.
    pub(crate) fn header_h(&self) -> f64 {
        self.header_pad_top + self.title_h + self.header_pad_bottom
    }
}

impl Default for CallOverlayMetrics {
    /// The dark theme's sizes at the reference text size, before the first
    /// paint.
    fn default() -> Self {
        Self::of(&Theme::dark())
    }
}

/// The icon a call control shows beside its label.
fn control_icon(hit: &Hit, muted: bool) -> Icon {
    match hit {
        Hit::CallMute if muted => Icon::MicOff,
        Hit::CallMute => Icon::Mic,
        Hit::CallCamera => Icon::Video,
        Hit::CallScreen => Icon::ScreenShare,
        Hit::CallExpand => Icon::Minimize2,
        Hit::CallLeave => Icon::PhoneOff,
        _ => Icon::MoreHorizontal,
    }
}

impl ChatApp {
    /// Measure the expanded overlay's controls and header with the faces
    /// they paint with. Called from the `&mut self` paint pass right before
    /// the overlay is drawn, so this frame's layout and hit rects use this
    /// frame's text scale.
    pub(crate) fn measure_call_overlay(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        // The labels and line boxes only change with the text scale or the
        // catalogue; skip the lookups and measures otherwise.
        let key = (
            tr.text_scale().to_bits(),
            theme.text_scale.to_bits(),
            ui::i18n::I18nManager::revision(),
        );
        if self.call_overlay_metrics_key == Some(key) {
            return;
        }
        self.call_overlay_metrics_key = Some(key);
        // Every label the bar can show — both halves of each toggle pair —
        // is measured, so a Mute → Unmute flip between paints resolves to a
        // measured width and the hit rects never fall back to the estimate.
        let button_w = CALL_CONTROL_KEYS
            .iter()
            .map(|key| {
                let label = crate::i18n::t(key);
                let w = ButtonFace::new(Rect::ZERO, &label)
                    .icon(Icon::Mic)
                    .intrinsic_width(tr, theme);
                (label, w)
            })
            .collect();
        let measured = CallOverlayMetrics {
            button_w,
            title_h: line_of(tr, &theme.text(TextRole::Title)),
            ..CallOverlayMetrics::of(theme)
        };
        if self.call_overlay_metrics != measured {
            self.call_overlay_metrics = measured;
            // `build_layout` has no renderer, so the host tile's surface node
            // was sized from the previous metrics; a relayout on the next
            // frame places it on the geometry this frame paints.
            self.request_relayout();
        }
    }

    /// The local listening panel followed by authenticated remote audio streams.
    /// This is not a complete participant or listener roster.
    fn call_participants(&self) -> Vec<Participant> {
        let members: &[VoiceMember] = self.voice_in(&self.call.channel);
        let mut out: Vec<Participant> = Vec::new();
        // Host = this peer.
        out.push(Participant {
            user: self.me.id.clone(),
            talking: self
                .native_voice
                .snapshot
                .speaking
                .iter()
                .any(|person| self.is_me(person)),
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
                talking: m.talking,
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

    /// Follow who the call hears with each face's speaking ring, once per
    /// frame before anything paints a face.
    pub(crate) fn reconcile_speaking(&mut self, theme: &Theme) {
        if !self.call.active {
            self.speaking.clear();
            return;
        }
        let heard = self
            .call_participants()
            .into_iter()
            .filter(|p| p.talking && !p.muted)
            .map(|p| p.user)
            .collect();
        self.speaking
            .reconcile(heard, theme.motion(), ui::time::now());
    }

    /// A person's face in the call: their colour as a ring, and their
    /// speaking ring while the call hears them.
    pub(crate) fn paint_call_face(&self, ctx: &mut PaintCtx<'_>, rect: Rect, user: &str) {
        let name = self.user_name(user);
        ui::widget::AvatarFace::new(&name, user)
            .presence(self.identity_colour(user))
            .presence_ring()
            .speaking(self.speaking.level(user, ui::motion::frame_now()))
            .paint(ctx, rect);
    }

    /// Sample the participant reveal once per frame. Every geometry consumer
    /// subsequently reads the cached value, avoiding tiny wall-clock skew
    /// between the soft body, its controls, hit regions and native bounds.
    pub(crate) fn reconcile_huddle_motion(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        if !self.call.active {
            self.call_chrome_motion.reset();
            self.huddle_chip_rows = 0;
            return;
        }
        // The participant strip opens only when there is someone beside you
        // to show in it.
        let parts = self.call_participants();
        let open = parts.len() > 1 && self.participants_should_show(parts.len());
        self.call_chrome_motion.reconcile(open, theme);
        if !self.call.expanded {
            // The chips wrap inside the video's width; the window grows by
            // as many rows as they take.
            let widths: Vec<f64> = parts
                .iter()
                .skip(1)
                .map(|p| self.huddle_face_width(p, tr, theme))
                .collect();
            self.huddle_chip_rows = if widths.is_empty() {
                0
            } else {
                let room = self.huddle_video().width();
                pack_rows(&widths, self.chrome.call_gap, room).len()
            };
            self.call
                .placement
                .set_attached_chrome(self.huddle_attached_chrome());
        }
    }

    /// The call window's frame around its video, `[left, top, right,
    /// bottom]`: the title bar above, the inset beside, and the connection
    /// line, the participant chips (as they reveal) and the controls below.
    fn huddle_frame(&self) -> [f64; 4] {
        let c = &self.chrome;
        let reveal = f64::from(self.call_chrome_motion.sampled.clamp(0.0, 1.0));
        let chips = self.huddle_chip_rows as f64 * (c.call_chip_h + c.call_gap);
        let bottom = c.call_gap
            + c.call_status_h
            + reveal * chips
            + c.call_gap
            + c.call_controls_h
            + c.call_inset;
        [c.call_inset, c.call_title_h, c.call_inset, bottom]
    }

    /// What the attached call window keeps clear around its video: its own
    /// frame, and the window's header row above it, so the window never
    /// rests over the header's controls.
    pub(crate) fn huddle_attached_chrome(&self) -> [f64; 4] {
        let [left, top, right, bottom] = self.huddle_frame();
        [left, self.chrome.header_h + top, right, bottom]
    }

    /// The widest a participant's name stands beside their face.
    fn huddle_name_max(theme: &Theme) -> f64 {
        8.0 * f64::from(theme.tokens.metrics.icon_lg)
    }

    /// One participant beside the host in the call window: their face, their
    /// name and, while muted, a muted microphone.
    fn huddle_face_width(&self, p: &Participant, tr: &mut TextRenderer, theme: &Theme) -> f64 {
        let sp = &theme.tokens.spacing;
        let face = ui::widget::AvatarSize::Small.side(theme);
        let name = self.user_name(&p.user);
        let name_w =
            width_of(tr, &name, &theme.text(TextRole::Label)).min(Self::huddle_name_max(theme));
        let muted = if p.muted {
            f64::from(sp.s1) + f64::from(theme.tokens.metrics.icon_sm)
        } else {
            0.0
        };
        face + f64::from(sp.s2) + name_w + muted + f64::from(sp.s2)
    }

    /// The floating overlay's home rect (video) for the current drag offset.
    fn huddle_video(&self) -> Rect {
        self.call_video_rect()
    }

    /// The tile radius: a window-tile corner on the full stage, a control
    /// corner inside the small huddle panel.
    fn tile_radius(&self, theme: &Theme) -> f64 {
        let radii = &theme.tokens.radii;
        if self.call.expanded {
            f64::from(radii.tile)
        } else {
            f64::from(radii.control)
        }
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
        let rect = self.call_video_rect();
        let me = self.me.id.clone();
        if !self.call.audio_only {
            // This person's own camera, from this device.
            self.note_picture_tile(&me, ui::native_media::PictureSource::Camera, tf, rect);
            if let Some(img) = self.remote_frames.get(&me) {
                let radius = self.tile_radius(theme);
                ui::image_store::paint_image_cover(scene, tf, rect, img, radius);
                return;
            }
        }
        self.paint_viewfinder(
            PaintCtx::new(scene, tf, tr, theme),
            rect,
            &me,
            if self.call.audio_only {
                "Camera off"
            } else {
                "Your camera is starting"
            },
        );
    }

    /// Note that this frame shows `person`'s `source` in `rect`: the call is
    /// asked for that picture at the layer that fits the tile.
    fn note_picture_tile(
        &self,
        person: &str,
        source: ui::native_media::PictureSource,
        tf: Affine,
        rect: Rect,
    ) {
        let device = tf.transform_rect_bbox(rect);
        self.picture_tiles.borrow_mut().push((
            person.to_owned(),
            source,
            (device.width(), device.height()),
        ));
    }

    /// An empty viewfinder: the letterbox, the person's avatar and what the
    /// tile honestly shows ("Camera off", "No video") under it, one block
    /// centred in the band above the name plate.
    fn paint_viewfinder(&self, paint: PaintCtx<'_>, rect: Rect, user: &str, caption: &str) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let media = &theme.tokens.colors.media;
        let radius = self.tile_radius(theme);
        scene.fill(
            Fill::NonZero,
            tf,
            media.letterbox,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        let style = theme.text(TextRole::Caption);
        let caption_h = line_of(tr, &style);
        let s2 = f64::from(theme.tokens.spacing.s2);
        // The disc follows the tile, from a regular avatar up to three.
        let regular = ui::widget::AvatarSize::Regular.side(theme);
        let sides = AvatarSides {
            wanted: (rect.height() * 0.4).clamp(regular, 3.0 * regular),
            least: ui::widget::AvatarSize::Small.side(theme),
        };
        let Some(block) = viewfinder_block(rect, sides, caption_h, plate_height(theme), s2) else {
            // Too short for even a small disc: the name plate names them.
            return;
        };
        let cx = rect.center().x;
        let mut ctx = PaintCtx::new(scene, tf, tr, theme);
        self.paint_call_face(
            &mut ctx,
            Rect::from_center_size(Point::new(cx, block.disc_cy), (block.side, block.side)),
            user,
        );
        if let Some(caption_y) = block.caption_y {
            let w = width_of(ctx.text, caption, &style);
            text(
                &mut ctx,
                caption,
                &style,
                media.ink_dim,
                Point::new(cx - w / 2.0, caption_y),
                None,
            );
        }
    }

    /// The call window: its video inside its frame. It grows continuously
    /// with the participant reveal rather than stepping between two heights.
    pub(crate) fn huddle_panel(&self) -> Rect {
        let v = self.huddle_video();
        let [left, top, right, bottom] = self.huddle_frame();
        Rect::new(v.x0 - left, v.y0 - top, v.x1 + right, v.y1 + bottom)
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
        let panel = self.huddle_panel();
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        // A floating Panel: its shadow, material and edge.
        ui::widget::floating_surface::paint(scene, tf, panel, theme);

        // The title bar doubles as the drag handle.
        let title_bar = Rect::new(
            panel.x0,
            panel.y0,
            panel.x1,
            panel.y0 + self.chrome.call_title_h,
        );
        let name = self
            .channel(&self.call.channel)
            .map_or_else(|| "Call".into(), |c| c.name.clone());
        ui::surface::floating_drag_grip(scene, tf, panel, local.text_dim);
        let sp = &theme.tokens.spacing;
        let n = self.call_participants().len();
        let count = format!("{n} in call");
        let count_style = local.text(TextRole::Caption).numeric();
        let count_w = width_of(tr, &count, &count_style);
        let inset = f64::from(sp.s4);
        let mut ctx = PaintCtx::new(scene, tf, tr, &local);
        line_in(
            &mut ctx,
            &count,
            &count_style,
            local.text_dim,
            title_bar.x1 - inset - count_w,
            title_bar,
            count_w + 1.0,
        );
        line_in(
            &mut ctx,
            &name,
            &local.text(TextRole::Label).strong(),
            local.text_primary,
            title_bar.x0 + inset,
            title_bar,
            title_bar.width() - 3.0 * inset - count_w,
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
        let panel = self.huddle_panel();
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        let v = self.huddle_video();
        let you = self.user_name(&self.me.id);
        Self::name_plate(scene, tf, tr, theme, v, &you, self.call.muted);
        if let Some(error) = &self.media_error {
            let style = theme.text(TextRole::Caption);
            let band = Rect::new(v.x0, v.y0, v.x1, v.y0 + 2.0 * line_of(tr, &style));
            let inset = f64::from(theme.tokens.spacing.s2);
            line_in(
                &mut PaintCtx::new(scene, tf, tr, theme),
                error,
                &style,
                theme.tokens.colors.media.ink,
                v.x0 + inset,
                band,
                v.width() - 2.0 * inset,
            );
        }

        let parts = self.call_participants();
        let others = parts.len().saturating_sub(1);
        let c = &self.chrome;
        let status = Rect::new(
            panel.x0 + c.call_inset,
            v.y1 + c.call_gap,
            panel.x1 - c.call_inset,
            v.y1 + c.call_gap + c.call_status_h,
        );
        self.draw_huddle_strip_header(PaintCtx::new(scene, tf, tr, &local), status, &parts, hits);

        // Participant chips, shown when expanded and wrapping to the panel
        // width so the strip adapts to the headcount.
        let reveal = self.call_chrome_motion.sampled.clamp(0.0, 1.0);
        if reveal > 0.0 && others > 0 {
            // Fade and progressively reveal the chip strip inside the same
            // animated panel bounds. Chips never pop out before the surface
            // has made room for them, and collapse cannot paint through the
            // rising controls row.
            let controls_top = panel.y1 - c.call_inset - c.call_controls_h;
            let clip = Rect::new(
                panel.x0,
                status.y1,
                panel.x1,
                (controls_top - c.call_gap).max(status.y1),
            );
            let alpha = reveal * reveal * (3.0 - 2.0 * reveal);
            scene.push_layer(Fill::NonZero, BlendMode::default(), alpha, tf, &clip);
            self.draw_huddle_chips(PaintCtx::new(scene, tf, tr, &local), status, &parts);
            scene.pop_layer();
        }

        self.draw_huddle_controls(scene, tf, tr, &local, panel, hits);
    }

    /// The connection line under the video: how the call is connected and,
    /// once someone else is in it, the toggle that shows or hides their
    /// chips.
    fn draw_huddle_strip_header(
        &self,
        paint: PaintCtx<'_>,
        band: Rect,
        parts: &[Participant],
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let mut x = band.x0;
        let others = parts.len() > 1;
        if others {
            let icon_side = f64::from(theme.tokens.metrics.icon_sm);
            let chevron = if self.participants_should_show(parts.len()) {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            chevron.draw(
                scene,
                tf,
                Rect::new(
                    x,
                    band.center().y - icon_side / 2.0,
                    x + icon_side,
                    band.center().y + icon_side / 2.0,
                ),
                theme.text_dim,
            );
            x += icon_side + f64::from(theme.tokens.metrics.icon_label_gap);
        }
        let status = crate::i18n::t(match self.native_voice.snapshot.state {
            ui::native_media::State::Live => "chat-call-connected",
            ui::native_media::State::Reconnecting => "chat-call-reconnecting",
            ui::native_media::State::Preparing | ui::native_media::State::Joining => {
                "chat-call-joining"
            }
            _ => "chat-call-voice-unavailable",
        });
        line_in(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &status,
            &theme.text(TextRole::Caption),
            theme.text_dim,
            x,
            band,
            band.x1 - x,
        );
        if others {
            ChatApp::push_hit(hits, band, Hit::CallParticipantsToggle);
        }
    }

    /// One face per person beside the host, ringed in their colour and
    /// ringing while they talk, with their name and a muted microphone while
    /// they are muted; wrapping to the panel width in rows under the
    /// connection line.
    fn draw_huddle_chips(&self, paint: PaintCtx<'_>, status: Rect, parts: &[Participant]) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let sp = &theme.tokens.spacing;
        let gap = self.chrome.call_gap;
        let h = self.chrome.call_chip_h;
        let face = ui::widget::AvatarSize::Small.side(theme);
        let icon = f64::from(theme.tokens.metrics.icon_sm);
        let style = theme.text(TextRole::Label);
        let (x0, x1) = (status.x0, status.x1);
        let mut x = x0;
        let mut y = status.y1 + gap;
        for p in parts.iter().skip(1) {
            let w = self.huddle_face_width(p, tr, theme).min(x1 - x0);
            if x + w > x1 && x > x0 {
                x = x0;
                y += h + gap;
            }
            let cy = y + h / 2.0;
            let mut ctx = PaintCtx::new(scene, tf, tr, theme);
            self.paint_call_face(
                &mut ctx,
                Rect::from_center_size(Point::new(x + face / 2.0, cy), (face, face)),
                &p.user,
            );
            let name_x = x + face + f64::from(sp.s2);
            let muted_w = if p.muted {
                f64::from(sp.s1) + icon
            } else {
                0.0
            };
            let room = (x + w - f64::from(sp.s2) - muted_w - name_x).max(0.0);
            let name = elide(ctx.text, &self.user_name(&p.user), &style, room);
            let name_w = width_of(ctx.text, &name, &style);
            line_in(
                &mut ctx,
                &name,
                &style,
                theme.text_primary,
                name_x,
                Rect::new(name_x, y, name_x + room, y + h),
                room + 1.0,
            );
            if p.muted {
                let ix = name_x + name_w + f64::from(sp.s1);
                Icon::MicOff.draw(
                    scene,
                    tf,
                    Rect::new(ix, cy - icon / 2.0, ix + icon, cy + icon / 2.0),
                    theme.text_dim,
                );
            }
            x += w + gap;
        }
    }

    /// Controls row, evenly distributed across the panel so it never
    /// overflows a small (narrow-window) huddle. On a cramped panel the
    /// buttons drop to icons and Expand is trimmed (the header's call button
    /// also opens the call), so the row always fits.
    fn draw_huddle_controls(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        panel: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let bh = self.chrome.call_controls_h;
        let pad = self.chrome.call_inset;
        let cy = panel.y1 - pad - bh;
        // Sibling controls sit an `s2` apart, like the expanded bar's.
        let gap = f64::from(theme.tokens.spacing.s2);
        let muted = self.call.muted;
        let mut ctrls: Vec<(&str, Icon, ButtonRole, Hit)> = vec![
            (
                if muted { "Unmute" } else { "Mute" },
                if muted { Icon::MicOff } else { Icon::Mic },
                ButtonRole::Secondary,
                Hit::CallMute,
            ),
            (
                if self.call.audio_only {
                    "Camera"
                } else {
                    "Stop camera"
                },
                Icon::Video,
                ButtonRole::Secondary,
                Hit::CallCamera,
            ),
            (
                "Expand",
                Icon::Maximize2,
                ButtonRole::Secondary,
                Hit::CallExpand,
            ),
            (
                "Leave",
                Icon::PhoneOff,
                ButtonRole::Destructive,
                Hit::CallLeave,
            ),
        ];
        let share = |n: usize| {
            let n = n as f64;
            ((panel.width() - 2.0 * pad - (n - 1.0) * gap) / n).max(0.0)
        };
        // Too cramped for every icon: trim the camera, then Expand (the
        // header's call button also opens the call; the expanded call has
        // the camera).
        for trimmed in [Hit::CallCamera, Hit::CallExpand] {
            if share(ctrls.len()) < bh {
                ctrls.retain(|(.., hit)| {
                    std::mem::discriminant(hit) != std::mem::discriminant(&trimmed)
                });
            }
        }
        let cw = share(ctrls.len());
        let fits = ctrls.iter().all(|(label, icon, ..)| {
            ButtonFace::new(Rect::ZERO, label)
                .icon(*icon)
                .control_size(ControlSize::Small)
                .intrinsic_width(tr, theme)
                <= cw
        });
        let mut x = panel.x0 + pad;
        for (label, icon, role, hit) in ctrls {
            let r = Rect::new(x, cy, x + cw, cy + bh);
            let hovered = self.call_control_is_hovered(&hit);
            let face = if fits {
                ButtonFace::new(r, label).icon(icon)
            } else {
                ButtonFace::icon_only(r, icon, label)
            };
            Button::paint(
                scene,
                tf,
                tr,
                theme,
                &face
                    .role(role)
                    .control_size(ControlSize::Small)
                    .hovered(hovered),
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
        // The expanded call is its own page over the whole window, so the
        // conversation does not show through between the tiles.
        ui::surface::region(scene, tf, self.rect, ui::surface::Region::Content, theme);

        let m = &self.call_overlay_metrics;
        let title_y = self.rect.y0 + m.header_pad_top;
        let title = self
            .channel(&self.call.channel)
            .map_or_else(|| "Call".into(), |c| c.name.clone());
        let style = theme.text(TextRole::Title);
        let max_w = self.rect.width() - 2.0 * m.side_pad;
        let shown = elide(tr, &title, &style, max_w);
        let tw = width_of(tr, &shown, &style);
        text(
            &mut PaintCtx::new(scene, tf, tr, theme),
            &shown,
            &style,
            theme.text_primary,
            Point::new(self.rect.center().x - tw / 2.0, title_y),
            None,
        );

        let parts = self.call_participants();
        let sharer = self.screen_sharer();
        match self.overlay_stage() {
            // No share: host + remotes share one responsive grid that fills the
            // whole band between the header and the controls bar — the host is
            // simply the first cell. The VideoPlayer surface is drawn by the app
            // on top of the host cell.
            CallStage::Grid(tiles) => {
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
                for (p, rect) in parts.iter().skip(1).zip(strip.iter().skip(1)) {
                    self.draw_member_tile(scene, tf, tr, theme, *rect, p);
                }
            }
        }
    }

    /// The featured presentation tile: a peer's real decoded screen frame when
    /// one has arrived, and an honest placeholder — the screen icon with the
    /// sharer's name, never a fabricated picture — until then. A live chip
    /// marks whose screen is on the stage.
    fn draw_screen_tile(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        sharer: &str,
    ) {
        let media = &theme.tokens.colors.media;
        let name = self.user_name(sharer);
        let is_me = sharer == self.me.id;
        let radius = self.tile_radius(theme);
        scene.fill(
            Fill::NonZero,
            tf,
            media.letterbox,
            None,
            &RoundedRect::from_rect(rect, radius),
        );
        // The sharer's decoded screen takes over the tile once it arrives.
        if !is_me {
            self.note_picture_tile(sharer, ui::native_media::PictureSource::Screen, tf, rect);
        }
        if let Some(img) = self.remote_screens.get(sharer) {
            // All of a shared screen shows, letterboxed: nothing is cut off.
            ui::image_store::paint_image_contain(scene, tf, rect, img, radius);
        } else {
            let icon_side = 3.0 * f64::from(theme.tokens.metrics.icon_lg);
            let head_style = theme.text(TextRole::Heading);
            let sub_style = theme.text(TextRole::Body);
            let gap = f64::from(theme.tokens.spacing.s3);
            let block = icon_side + gap + line_of(tr, &head_style) + line_of(tr, &sub_style);
            let top = rect.center().y - block / 2.0;
            Icon::Monitor.draw(
                scene,
                tf,
                Rect::new(
                    rect.center().x - icon_side / 2.0,
                    top,
                    rect.center().x + icon_side / 2.0,
                    top + icon_side,
                ),
                media.ink_dim,
            );
            let head = if is_me {
                "You're sharing your screen".to_string()
            } else {
                format!("{name} is sharing their screen")
            };
            // Honest status: the local sharer is the source; a remote tile
            // waits on the first decoded frame (this branch only runs with no
            // frame, so it never claims to show video it lacks).
            let sub = if is_me {
                "Everyone in the call sees your screen"
            } else {
                "Waiting for the first frame"
            };
            let mut ctx = PaintCtx::new(scene, tf, tr, theme);
            let mut y = top + icon_side + gap;
            for (s, style, ink) in [
                (head.as_str(), &head_style, media.ink),
                (sub, &sub_style, media.ink_dim),
            ] {
                let shown = elide(ctx.text, s, style, rect.width() - 2.0 * gap);
                let w = width_of(ctx.text, &shown, style);
                let (_, h) = text(
                    &mut ctx,
                    &shown,
                    style,
                    ink,
                    Point::new(rect.center().x - w / 2.0, y),
                    None,
                );
                y += f64::from(h);
            }
        }
        let who = if is_me {
            "Your screen".to_string()
        } else {
            format!("{name}'s screen")
        };
        let h = ChipFace::height(theme);
        let w = ChipFace::intrinsic_width(&who, ChipLead::None, false, tr, theme);
        let inset = f64::from(theme.tokens.spacing.s3);
        ChipFace::new(
            Rect::new(
                rect.x0 + inset,
                rect.y0 + inset,
                rect.x0 + inset + w,
                rect.y0 + inset + h,
            ),
            &who,
        )
        .style(ChipStyle::Live)
        .paint(scene, tf, tr, theme);
    }

    pub(crate) fn draw_call_overlay_fg(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let host = self.overlay_host_rect();
        let you = self.user_name(&self.me.id);
        Self::name_plate(scene, tf, tr, theme, host, &you, self.call.muted);

        // Controls bar: auto-sized to its buttons and wrapped onto extra rows
        // when the window is too narrow to hold them all in one line, so it
        // never runs off either edge. It floats over the stage.
        let (bar, buttons) = self.overlay_controls_layout();
        let radius = f64::from(theme.tokens.radii.panel).min(bar.height() / 2.0);
        ui::surface::shadow(scene, tf, bar, radius, &theme.tokens.elevation.floating);
        ui::surface::panel(scene, tf, bar, radius, theme);
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        for (label_txt, _on, danger, enabled, hit, r) in buttons {
            let hovered = self.call_control_is_hovered(&hit);
            let face = ButtonFace::new(r, &label_txt)
                .icon(control_icon(&hit, self.call.muted))
                .role(if danger {
                    ButtonRole::Destructive
                } else {
                    ButtonRole::Secondary
                })
                .hovered(hovered)
                .disabled(!enabled);
            Button::paint(scene, tf, tr, &local, &face);
            if enabled {
                ChatApp::push_hit(hits, r, hit);
            }
        }
    }

    /// The band the expanded call fills with participant tiles: everything
    /// between the title and the controls bar, inset from the window edges.
    /// Deterministic (no text measurement) so the tile grid, the host
    /// [`VideoPlayer`](ui::widget::VideoPlayer) node built in `build_layout`,
    /// and `call_video_rect` all resolve to exactly the same geometry on a
    /// single frame.
    pub(crate) fn overlay_content_region(&self) -> Rect {
        let r = self.rect;
        let m = &self.call_overlay_metrics;
        let inset = m.side_pad;
        // Below the measured title (the 1× rule before the first paint), so a
        // scaled title pushes the tiles down instead of painting over them.
        let top = r.y0 + m.header_h();
        let (bar, _) = self.overlay_controls_layout();
        let bottom = (bar.y0 - m.bottom_pad).max(top + 1.0);
        Rect::new(r.x0 + inset, top, r.x1 - inset, bottom)
    }

    /// Host-first participant tiles for the expanded call, packed to fill the
    /// whole content region at every width — the host is simply the first
    /// cell, so nothing is left empty beneath or beside it. A solo call yields
    /// a single tile spanning the full region.
    pub(crate) fn overlay_tiles(&self) -> Vec<Rect> {
        let n = self.call_participants().len().max(1);
        grid_rects(
            self.overlay_content_region(),
            n,
            self.call_overlay_metrics.tile_gap,
        )
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
            let m = &self.call_overlay_metrics;
            let gap = m.tile_gap;
            // The screen dominates; participants ride a strip beneath it that
            // stays a minority of the surface. Clamped so a short window still
            // leaves a strip that holds a participant's disc, caption and name
            // plate, and a tall one doesn't over-grow it.
            let strip_h = (region.height() * 0.24)
                .clamp(m.strip_min_h, 2.0 * m.strip_min_h)
                .min(region.height() * 0.4);
            let screen_bottom = (region.y1 - strip_h - gap).max(region.y0 + 1.0);
            let screen = Rect::new(region.x0, region.y0, region.x1, screen_bottom);
            let strip_region = Rect::new(region.x0, screen.y1 + gap, region.x1, region.y1);
            CallStage::Share {
                screen,
                strip: filmstrip_rects(strip_region, n, gap),
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

    /// The expanded overlay's controls in bar order:
    /// `(label, active, danger, enabled, hit)`. One source for the layout
    /// (which sizes buttons from these labels) and the pre-paint measurement
    /// (which measures exactly these labels), so a toggled label — Mute →
    /// Unmute — is measured the frame it appears.
    fn overlay_control_specs(&self) -> [(String, bool, bool, bool, Hit); 5] {
        let t = crate::i18n::t;
        let speak = self.call_allows(crate::call::CallAction::Speak);
        [
            (
                t(if self.call.muted {
                    "chat-call-unmute"
                } else {
                    "chat-call-mute"
                }),
                self.call.muted,
                false,
                speak,
                Hit::CallMute,
            ),
            (
                t(if self.call.audio_only {
                    "chat-call-camera-on"
                } else {
                    "chat-call-camera-off"
                }),
                !self.call.audio_only,
                false,
                speak,
                Hit::CallCamera,
            ),
            (
                t(if self.call.sharing {
                    "chat-call-stop-sharing"
                } else {
                    "chat-call-share-screen"
                }),
                self.call.sharing,
                false,
                speak,
                Hit::CallScreen,
            ),
            (t("chat-call-collapse"), false, false, true, Hit::CallExpand),
            (t("chat-call-leave"), false, true, true, Hit::CallLeave),
        ]
    }

    /// Lay out the expanded-overlay control buttons, wrapping onto extra rows
    /// to fit the current window width. Returns the bar rect and each button's
    /// `(label, active, danger, enabled, hit, rect)`. Shared by the background
    /// pass (which bounds the tile region above the bar), the foreground pass
    /// (which draws it), and the layout pass (which sizes the tile band) — so
    /// all three agree without a `TextRenderer`: the button widths and height
    /// come from [`CallOverlayMetrics`], measured by the paint pass that
    /// precedes them (the 1× rules before the first paint).
    pub(crate) fn overlay_controls_layout(&self) -> (Rect, Vec<OverlayButton>) {
        let specs = self.overlay_control_specs();
        let m = &self.call_overlay_metrics;
        let gap = m.gap;
        let bh = m.button_h;
        let inner_pad = m.inner_pad;
        let max_row_w = (self.rect.width() - 2.0 * m.side_pad).max(bh);
        let widths: Vec<f64> = specs.iter().map(|s| m.button_width(&s.0)).collect();
        let rows = pack_rows(&widths, gap, max_row_w);

        let n_rows = rows.len() as f64;
        let bar_h = n_rows * bh + (n_rows - 1.0) * gap + 2.0 * inner_pad;
        let row_width = |row: &Vec<usize>| {
            row.iter().filter_map(|&i| widths.get(i)).sum::<f64>()
                + row.len().saturating_sub(1) as f64 * gap
        };
        let widest = rows.iter().map(row_width).fold(0.0_f64, f64::max);

        let cx = self.rect.center().x;
        let bar_bottom = self.rect.y1 - m.bottom_pad;
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
            let mut x = cx - row_width(row) / 2.0;
            for &i in row {
                let (Some(&w), Some(spec)) = (widths.get(i), specs.get(i)) else {
                    continue;
                };
                let r = Rect::new(x, ry, x + w, ry + bh);
                let (label_txt, on, danger, enabled, hit) = spec;
                out.push((label_txt.clone(), *on, *danger, *enabled, hit.clone(), r));
                x += w + gap;
            }
            ry += bh + gap;
        }
        (bar, out)
    }

    /// Draw one remote participant's tile in the expanded grid. There is no
    /// camera capture wired to remote peers in this environment, so a tile
    /// never fabricates a video frame: it shows the participant's avatar in
    /// an empty viewfinder — the same honest treatment the host tile gets —
    /// plus their real camera state.
    fn draw_member_tile(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        p: &Participant,
    ) {
        let name = self.user_name(&p.user);
        let is_stream = self
            .native_voice
            .snapshot
            .publishers
            .iter()
            .any(|namespace| namespace == &p.user);
        // Their decoded camera picture takes over the tile once it arrives;
        // until then the tile shows them.
        if !is_stream && !p.audio_only {
            self.note_picture_tile(&p.user, ui::native_media::PictureSource::Camera, tf, rect);
        }
        if !is_stream && let Some(img) = self.remote_frames.get(&p.user) {
            let radius = self.tile_radius(theme);
            ui::image_store::paint_image_cover(scene, tf, rect, img, radius);
            // Over a picture, the tile's edge rings in their colour while
            // they talk, as their face does without one.
            let speaking = self.speaking.level(&p.user, ui::motion::frame_now());
            ui::presence::paint_speaking_ring(
                scene,
                tf,
                RoundedRect::from_rect(rect, radius),
                theme
                    .tokens
                    .colors
                    .presence
                    .of(self.identity_colour(&p.user)),
                speaking,
                theme,
            );
        } else {
            let caption = if is_stream {
                "Audio stream"
            } else {
                member_tile_caption(p)
            };
            self.paint_viewfinder(PaintCtx::new(scene, tf, tr, theme), rect, &p.user, caption);
        }
        Self::name_plate(scene, tf, tr, theme, rect, &name, p.muted);
        if p.sharing {
            let side = ChipFace::height(theme);
            let inset = f64::from(theme.tokens.spacing.s2);
            ChipFace::new(
                Rect::new(
                    rect.x1 - inset - side,
                    rect.y0 + inset,
                    rect.x1 - inset,
                    rect.y0 + inset + side,
                ),
                "",
            )
            .lead(ChipLead::Glyph(Icon::Monitor))
            .style(ChipStyle::Live)
            .paint(scene, tf, tr, theme);
        }
    }

    /// The name plate at a tile's bottom-leading corner: the person's name
    /// and, while muted, a muted microphone, on the floating material over
    /// the media.
    fn name_plate(
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        tile: Rect,
        name: &str,
        muted: bool,
    ) {
        let sp = &theme.tokens.spacing;
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        let style = local.text(TextRole::Label);
        let plate_h = plate_height(theme);
        let inset = f64::from(sp.s2);
        let pad_x = f64::from(sp.s2);
        let icon_side = f64::from(theme.tokens.metrics.icon_sm);
        let icon_w = if muted {
            icon_side + f64::from(sp.s1)
        } else {
            0.0
        };
        let max_name = (tile.width() - 2.0 * inset - 2.0 * pad_x - icon_w).max(0.0);
        let shown = elide(tr, name, &style, max_name);
        let name_w = width_of(tr, &shown, &style);
        let plate = Rect::new(
            tile.x0 + inset,
            tile.y1 - inset - plate_h,
            tile.x0 + inset + 2.0 * pad_x + icon_w + name_w,
            tile.y1 - inset,
        );
        let shape = RoundedRect::from_rect(plate, plate_h / 2.0);
        paint_material(scene, tf, &shape, Material::Floating, theme);
        let mut x = plate.x0 + pad_x;
        if muted {
            Icon::MicOff.draw(
                scene,
                tf,
                Rect::new(
                    x,
                    plate.center().y - icon_side / 2.0,
                    x + icon_side,
                    plate.center().y + icon_side / 2.0,
                ),
                local.text_dim,
            );
            x += icon_w;
        }
        line_in(
            &mut PaintCtx::new(scene, tf, tr, &local),
            &shown,
            &style,
            local.text_primary,
            x,
            plate,
            name_w + 1.0,
        );
    }

    fn call_control_is_hovered(&self, hit: &Hit) -> bool {
        self.call_hovered
            .as_ref()
            .is_some_and(|hovered| std::mem::discriminant(hovered) == std::mem::discriminant(hit))
    }

    // -------------------------------------------------------- emoji picker
    /// The picker popover: a filter field, a tab per Unicode group, and a
    /// scrolling grid showing either the matches or recently used emoji
    /// followed by the selected group.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear layout-then-paint pass: panel, filter, tabs, clipped grid"
    )]
    pub(crate) fn draw_emoji_picker(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let catalogue = crate::i18n::emoji_catalogue();
        // Full-window dismiss target (lowest priority).
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let sp = &theme.tokens.spacing;
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        let pad = f64::from(sp.s3);
        let w = f64::from(theme.tokens.metrics.panel_width_utility);
        let cols = 8usize;
        let cell = (w - 2.0 * pad) / cols as f64;
        let grid_rows = 6.0;
        let title_h = ui::chrome::nav_row_height(theme);
        let field_h = ButtonFace::height(ControlSize::Regular, theme);
        let tabs_h = SegmentedFace::height(ControlSize::Regular, theme);
        let h = pad + title_h + field_h + pad + tabs_h + pad + grid_rows * cell + pad;
        let margin = f64::from(sp.s2);
        let x0 = self.emoji_anchor.x.clamp(
            self.rect.x0 + margin,
            (self.rect.x1 - w - margin).max(self.rect.x0 + margin),
        );
        let y0 = self.emoji_anchor.y.clamp(
            self.rect.y0 + margin,
            (self.rect.y1 - h - margin).max(self.rect.y0 + margin),
        );
        let panel = Rect::new(x0, y0, x0 + w, y0 + h);
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::shadow(scene, tf, panel, radius, &theme.tokens.elevation.floating);
        ui::surface::panel(scene, tf, panel, radius, theme);

        let title = if self.emoji_target.is_some() {
            crate::i18n::t("chat-emoji-add-reaction")
        } else {
            crate::i18n::t("chat-emoji-title")
        };
        let title_band = Rect::new(panel.x0, panel.y0 + pad, panel.x1, panel.y0 + pad + title_h);
        line_in(
            &mut PaintCtx::new(scene, tf, tr, &local),
            &title,
            &local.text(TextRole::Subheading),
            local.text_primary,
            panel.x0 + pad,
            title_band,
            w - 2.0 * pad,
        );

        // The filter: the shared field, placed by hand like the composers.
        let search = Rect::new(
            panel.x0 + pad,
            title_band.y1,
            panel.x1 - pad,
            title_band.y1 + field_h,
        );
        self.emoji_search.apply_layout_external(search);
        self.emoji_search.draw(scene, tf, tr, &local, presence);
        ChatApp::push_hit(hits, search, Hit::FocusEmojiSearch);
        // The rest of the panel keeps the query focused instead of dismissing;
        // the field's own rect above stays its accessible bounds.
        ChatApp::push_hit(hits, panel, Hit::FocusEmojiSearch);
        let query = self.emoji_search.value().trim().to_owned();

        // Group tabs.
        let groups = catalogue.groups();
        self.emoji_group = self.emoji_group.min(groups.len().saturating_sub(1));
        let tabs_y = search.y1 + pad;
        let icons: Vec<&str> = groups.iter().map(emoji::Group::icon).collect();
        let segments: Vec<ui::widget::Segment> = icons
            .iter()
            .map(|icon| ui::widget::Segment::new(*icon))
            .collect();
        let tab_rects = SegmentedFace::partition_segments(
            Rect::new(panel.x0 + pad, tabs_y, panel.x1 - pad, tabs_y + tabs_h),
            &segments,
            SegmentedStyle::Tabs,
            ControlSize::Regular,
            tr,
            &local,
        );
        let selected = if query.is_empty() {
            self.emoji_group
        } else {
            usize::MAX
        };
        ToggleGroup::paint(
            scene,
            tf,
            tr,
            &local,
            &SegmentedFace::new(&tab_rects, &icons, selected).style(SegmentedStyle::Tabs),
        );
        for (i, r) in tab_rects.iter().enumerate() {
            ChatApp::push_hit(hits, *r, Hit::EmojiGroup(i));
        }

        // The scrolling grid: matches, or recents + the group.
        let grid = Rect::new(
            panel.x0 + pad,
            tabs_y + tabs_h + pad,
            panel.x1 - pad,
            panel.y1 - pad,
        );
        self.emoji_grid = grid;
        let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
        if query.is_empty() {
            let recents: Vec<&str> = self
                .emoji_recents
                .items()
                .iter()
                .map(String::as_str)
                .collect();
            if !recents.is_empty() {
                sections.push((crate::i18n::t("chat-emoji-recent"), recents));
            }
            if let Some(group) = groups.get(self.emoji_group) {
                sections.push((
                    group.label().to_owned(),
                    catalogue
                        .group_emoji(group)
                        .iter()
                        .map(emoji::Emoji::text)
                        .collect(),
                ));
            }
        } else {
            let found: Vec<&str> = catalogue
                .search(&query, cols * 12)
                .into_iter()
                .map(emoji::Emoji::text)
                .collect();
            let heading = if found.is_empty() {
                crate::i18n::t("chat-emoji-no-results")
            } else {
                crate::i18n::t("chat-emoji-results")
            };
            sections.push((heading, found));
        }
        let heading_style = local.text(TextRole::Caption);
        let heading_h = line_of(tr, &heading_style) + f64::from(sp.s1);
        let content_h: f64 = sections
            .iter()
            .map(|(_, set)| heading_h + set.len().div_ceil(cols) as f64 * cell + f64::from(sp.s2))
            .sum();
        self.emoji_scroll = self.emoji_scroll.min((content_h - grid.height()).max(0.0));

        push_clip(scene, tf, grid);
        let mut y = grid.y0 - self.emoji_scroll;
        for (heading, set) in &sections {
            text(
                &mut PaintCtx::new(scene, tf, tr, &local),
                heading,
                &heading_style,
                local.text_dim,
                Point::new(grid.x0, y),
                None,
            );
            y += heading_h;
            self.emoji_grid(
                PaintCtx::new(scene, tf, tr, &local),
                set,
                grid,
                &mut y,
                (cols, cell),
                hits,
            );
            y += f64::from(sp.s2);
        }
        scene.pop_layer();
    }

    /// One section's cells from `*y` down, advancing `*y` past them. Only
    /// cells inside `clip` receive a hit target; the cell under the pointer
    /// lifts.
    fn emoji_grid(
        &self,
        paint: PaintCtx<'_>,
        set: &[&str],
        clip: Rect,
        y: &mut f64,
        (cols, cell): (usize, f64),
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        // Emoji are content: set at the size of a title line.
        let style = theme.text(TextRole::Title);
        let ground = theme.tokens.colors.surfaces.content;
        for (i, e) in set.iter().enumerate() {
            let col = i % cols;
            let row = i / cols;
            let cx = clip.x0 + col as f64 * cell;
            let cyy = *y + row as f64 * cell;
            let cellr = Rect::new(cx, cyy, cx + cell, cyy + cell);
            if cellr.y1 < clip.y0 || cellr.y0 > clip.y1 {
                continue;
            }
            ui::chrome::paint_row_selection(
                scene,
                tf,
                cellr,
                ui::chrome::SelectionState::new(false, cellr.contains(self.last_mouse), false),
                ground,
                theme,
            );
            let (ew, eh) = tr.measure_style(e, None, &style);
            text(
                &mut PaintCtx::new(scene, tf, tr, theme),
                e,
                &style,
                theme.text_primary,
                Point::new(
                    cellr.center().x - f64::from(ew) / 2.0,
                    cellr.center().y - f64::from(eh) / 2.0,
                ),
                None,
            );
            let target = cellr.intersect(clip);
            if target.area() > 0.0 {
                ChatApp::push_hit(hits, target, Hit::EmojiChoice((*e).to_string()));
            }
        }
        let rows = set.len().div_ceil(cols);
        *y += rows as f64 * cell;
    }

    // ------------------------------------------------------ history popover
    /// Every version of an edited message, newest first.
    pub(crate) fn draw_history_popover(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        ChatApp::push_hit(hits, self.rect, Hit::Dismiss);
        let Some(mid) = self.history_open.clone() else {
            return;
        };
        let Some(m) = self.message(&mid).cloned() else {
            return;
        };
        let sp = &theme.tokens.spacing;
        let local = theme.on_ground(ui::chrome::panel_ground(theme));
        let pad = f64::from(sp.s4);
        let w = f64::from(theme.tokens.metrics.panel_width_list);
        let tw = (w - 2.0 * pad) as f32;
        let head_style = local.text(TextRole::Caption).numeric();
        let body_style = local.text(TextRole::Body);
        let head_h = line_of(tr, &head_style);
        let mut entries: Vec<(String, String)> =
            vec![(format!("Current, {}", clock_label(m.ts)), m.text.clone())];
        for rec in m.history.iter().rev() {
            entries.push((
                format!("Earlier, {}", clock_label(rec.ts)),
                rec.text.clone(),
            ));
        }
        let title_h = ui::chrome::nav_row_height(theme);
        let gap = f64::from(sp.s3);
        let rows: Vec<(String, String, f64)> = entries
            .into_iter()
            .map(|(head, body)| {
                let bh = f64::from(tr.measure_style(&body, Some(tw), &body_style).1);
                (head, body, head_h + bh)
            })
            .collect();
        let total = pad + title_h + rows.iter().map(|(_, _, h)| h + gap).sum::<f64>() + pad - gap;
        let margin = f64::from(sp.s2);
        let x0 = self.history_anchor.x.clamp(
            self.rect.x0 + margin,
            (self.rect.x1 - w - margin).max(self.rect.x0 + margin),
        );
        let y0 = self.history_anchor.y.clamp(
            self.rect.y0 + margin,
            (self.rect.y1 - total - margin).max(self.rect.y0 + margin),
        );
        let panel = Rect::new(x0, y0, x0 + w, y0 + total);
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::shadow(scene, tf, panel, radius, &theme.tokens.elevation.floating);
        ui::surface::panel(scene, tf, panel, radius, theme);
        let mut ctx = PaintCtx::new(scene, tf, tr, &local);
        let title_band = Rect::new(panel.x0, panel.y0 + pad, panel.x1, panel.y0 + pad + title_h);
        line_in(
            &mut ctx,
            "Edit history",
            &local.text(TextRole::Subheading),
            local.text_primary,
            panel.x0 + pad,
            title_band,
            w - 2.0 * pad,
        );
        let mut y = title_band.y1;
        let ground = local.tokens.colors.surfaces.content;
        let row_count = rows.len();
        for (i, (head, body, h)) in rows.iter().enumerate() {
            text(
                &mut ctx,
                head,
                &head_style,
                local.text_dim,
                Point::new(panel.x0 + pad, y),
                None,
            );
            text(
                &mut ctx,
                body,
                &body_style,
                if i == 0 {
                    local.text_primary
                } else {
                    local.text_dim
                },
                Point::new(panel.x0 + pad, y + head_h),
                Some(tw),
            );
            y += h + gap;
            if i + 1 < row_count {
                ui::surface::separator(
                    ctx.scene,
                    tf,
                    panel.x0,
                    panel.x1,
                    y - gap / 2.0,
                    pad,
                    ground,
                );
            }
        }
    }
}

/// The local time of `ts` as `HH:MM`.
fn clock_label(ts: i64) -> String {
    model::clock(ts + i64::from(ui::clock::local_now().offset().local_minus_utc()))
}

/// Greedy-pack buttons `widths` wide into rows that each fit `max_row_w`.
/// Always at least one (possibly empty) row.
fn pack_rows(widths: &[f64], gap: f64, max_row_w: f64) -> Vec<Vec<usize>> {
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut cur = 0.0;
    for (i, w) in widths.iter().enumerate() {
        let full = rows.last().is_some_and(|last| !last.is_empty());
        let extra = if full { gap } else { 0.0 } + w;
        if full && cur + extra > max_row_w {
            rows.push(vec![i]);
            cur = *w;
        } else if let Some(last) = rows.last_mut() {
            last.push(i);
            cur += extra;
        }
    }
    rows
}

/// Height of a tile's name plate: one label line inside the selector
/// height, so the avatar block above it always clears it.
fn plate_height(theme: &Theme) -> f64 {
    ChipFace::height(theme)
}

/// How large a viewfinder's avatar disc may be: the side the tile asks for,
/// and the smallest it may shrink to before it is left out.
#[derive(Clone, Copy, Debug)]
struct AvatarSides {
    wanted: f64,
    least: f64,
}

/// Where a viewfinder's avatar disc and caption land.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ViewfinderBlock {
    disc_cy: f64,
    side: f64,
    /// The caption's top, or `None` when the tile has no room for it.
    caption_y: Option<f64>,
}

/// Lay out a viewfinder's disc and caption as one block, `gap` apart and
/// centred in the band between the tile's top inset and the name plate's
/// reserve (`plate_h` plus its `gap` inset plus `gap` of air above it). A
/// short tile shrinks the disc first, then drops the caption, then the disc,
/// so nothing ever touches the plate.
fn viewfinder_block(
    tile: Rect,
    sides: AvatarSides,
    caption_h: f64,
    plate_h: f64,
    gap: f64,
) -> Option<ViewfinderBlock> {
    let band_top = tile.y0 + gap;
    let band_bottom = tile.y1 - gap - plate_h - gap;
    let room = band_bottom - band_top;
    let with_caption = (room - gap - caption_h).min(sides.wanted);
    let (side, caption) = if with_caption >= sides.least {
        (with_caption, true)
    } else if room >= sides.least {
        (room.min(sides.wanted), false)
    } else {
        return None;
    };
    let block_h = side + if caption { gap + caption_h } else { 0.0 };
    let top = band_top + (room - block_h) / 2.0;
    Some(ViewfinderBlock {
        disc_cy: top + side / 2.0,
        side,
        caption_y: caption.then_some(top + side + gap),
    })
}

/// The honest one-line caption shown under a remote participant's avatar in
/// the expanded call grid. There is no remote camera capture wired up in this
/// environment, so a tile must never imply a live feed: an audio-only peer
/// reads "Camera off"; a camera-enabled peer whose frame doesn't reach us
/// reads "No video".
fn member_tile_caption(p: &Participant) -> &'static str {
    if p.audio_only {
        "Camera off"
    } else {
        "No video"
    }
}

/// Lay `n` participant tiles into a single centred row filling `region`'s
/// height — the presentation-stage filmstrip beneath the featured screen. Each
/// tile is landscape-ish (capped at an equal share of the row width) and every
/// returned rect stays inside `region`, so the strip never spills past the
/// window at any headcount.
fn filmstrip_rects(region: Rect, n: usize, gap: f64) -> Vec<Rect> {
    if n == 0 || region.width() <= 1.0 || region.height() <= 1.0 {
        return vec![];
    }
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
fn grid_rects(region: Rect, n: usize, gap: f64) -> Vec<Rect> {
    if n == 0 || region.width() <= 1.0 || region.height() <= 1.0 {
        return vec![];
    }

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
        AvatarSides, CallOverlayMetrics, Participant, filmstrip_rects, grid_rects,
        member_tile_caption, pack_rows, viewfinder_block,
    };
    use vello::kurbo::Rect;

    #[test]
    fn overlay_metrics_size_header_buttons_and_lower_chrome() {
        // Phase: overlay_metrics_fall_back_to_the_theme_before_the_first_paint.
        {
            let m = CallOverlayMetrics::default();
            // The header: `s6` + a Title line + `s4`.
            assert!(
                (m.header_h() - (m.header_pad_top + m.title_h + m.header_pad_bottom)).abs() < 1e-6
            );
            assert!(m.title_h > 0.0 && m.button_h > 0.0);
            // An unmeasured label is estimated from its length, so the
            // geometry tests (which never paint) still resolve, and a longer
            // label is estimated wider.
            assert!(m.button_width("Unmute") > m.button_width("Mute"));
            assert!(m.button_width("Mute") > m.button_chrome_w);
        }
        // Phase: measured_button_widths_replace_the_estimate_label_by_label.
        {
            let m = CallOverlayMetrics {
                button_w: vec![("Mute".to_string(), 71.0), ("Leave".to_string(), 80.5)],
                ..CallOverlayMetrics::default()
            };
            assert_eq!(m.button_width("Mute"), 71.0);
            assert_eq!(m.button_width("Leave"), 80.5);
            // A label that toggled since the measurement (Mute → Unmute) falls
            // back rather than borrowing another label's width.
            assert_eq!(
                m.button_width("Unmute"),
                CallOverlayMetrics::default().button_width("Unmute")
            );
        }
        // Phase: a_scaled_title_pushes_the_tile_band_down_by_its_extra_height.
        {
            let base = CallOverlayMetrics::default();
            let m = CallOverlayMetrics {
                title_h: base.title_h * 2.0,
                ..CallOverlayMetrics::default()
            };
            let grew = m.header_h() - base.header_h();
            assert!((grew - base.title_h).abs() < 1e-9, "grew {grew}");
        }
        // Phase: controls_pack_into_rows_that_fit.
        {
            assert_eq!(
                pack_rows(&[50.0, 50.0, 50.0], 10.0, 200.0),
                vec![vec![0, 1, 2]]
            );
            assert_eq!(
                pack_rows(&[80.0, 80.0, 80.0], 10.0, 200.0),
                vec![vec![0, 1], vec![2]]
            );
            assert_eq!(pack_rows(&[], 10.0, 200.0), vec![Vec::<usize>::new()]);
        }
    }

    #[test]
    fn member_tile_centres_an_honest_caption_clear_of_the_name_plate() {
        // Phase: the_block_centres_clear_of_the_name_plate_by_the_token_gap.
        {
            // A filmstrip tile, a disc that fits, a caption and a plate. The
            // caption's bottom stays a full `s2` above the plate's reserve.
            let tile = Rect::new(0.0, 0.0, 232.0, 152.0);
            let sides = AvatarSides {
                wanted: 60.0,
                least: 24.0,
            };
            let (caption_h, plate_h, gap) = (16.0, 28.0, 8.0);
            let block = viewfinder_block(tile, sides, caption_h, plate_h, gap).unwrap();
            assert_eq!(block.side, 60.0);
            let caption_y = block.caption_y.unwrap();
            let plate_top = tile.y1 - gap - plate_h;
            assert!(caption_y + caption_h <= plate_top - gap + 1e-9);
            assert!(block.disc_cy - block.side / 2.0 >= tile.y0 + gap);
            // Centred: equal air above the disc and below the caption.
            let above = (block.disc_cy - block.side / 2.0) - (tile.y0 + gap);
            let below = (plate_top - gap) - (caption_y + caption_h);
            assert!((above - below).abs() < 1e-9, "above {above} below {below}");
        }
        // Phase: a_short_tile_shrinks_the_disc_then_drops_the_caption_then_the_disc.
        {
            let sides = AvatarSides {
                wanted: 60.0,
                least: 24.0,
            };
            let (caption_h, plate_h, gap) = (16.0, 28.0, 8.0);
            let fits_all = |tile: Rect, block: super::ViewfinderBlock| {
                let plate_top = tile.y1 - gap - plate_h;
                let bottom = block
                    .caption_y
                    .map_or(block.disc_cy + block.side / 2.0, |y| y + caption_h);
                block.disc_cy - block.side / 2.0 >= tile.y0 + gap - 1e-9
                    && bottom <= plate_top - gap + 1e-9
            };
            // Room for a smaller disc and the caption.
            let tile = Rect::new(0.0, 0.0, 200.0, 100.0);
            let block = viewfinder_block(tile, sides, caption_h, plate_h, gap).unwrap();
            assert!(block.side < sides.wanted && block.side >= sides.least);
            assert!(block.caption_y.is_some());
            assert!(fits_all(tile, block));
            // Room for a disc only.
            let tile = Rect::new(0.0, 0.0, 200.0, 80.0);
            let block = viewfinder_block(tile, sides, caption_h, plate_h, gap).unwrap();
            assert!(block.caption_y.is_none());
            assert!(fits_all(tile, block));
            // No room for a disc: the plate alone names them.
            let tile = Rect::new(0.0, 0.0, 200.0, 60.0);
            assert!(viewfinder_block(tile, sides, caption_h, plate_h, gap).is_none());
        }
        // Phase: remote_tile_caption_is_honest_never_a_fabricated_feed.
        // Defect regression: remote tiles must be honest — never present a
        // fabricated video feed. Every caption is a truthful "camera state"
        // string (matching the host tile), never a claim of live video.
        {
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
    }

    #[test]
    fn filmstrip_is_a_single_centred_row_inside_its_region_or_empty() {
        // Phase: filmstrip_is_a_single_centred_row_inside_its_region.
        {
            for &(w, h) in &[(1200.0_f64, 180.0_f64), (700.0, 150.0), (500.0, 120.0)] {
                let region = Rect::new(40.0, 600.0, 40.0 + w, 600.0 + h);
                for n in 1..=8 {
                    let tiles = filmstrip_rects(region, n, 12.0);
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
        // Phase: filmstrip_is_empty_for_no_participants.
        {
            assert!(filmstrip_rects(Rect::new(0.0, 0.0, 400.0, 120.0), 0, 12.0).is_empty());
        }
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

    #[test]
    fn grid_packs_every_count_inside_the_region_and_covers_it() {
        // Phase: grid_tiles_stay_inside_region_at_every_count_and_width.
        // The packer must keep every tile inside the region it is handed, at any
        // headcount and width, with no degenerate tiles — the invariant the whole
        // expanded overlay leans on to stay within the window.
        {
            for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
                // A full-surface content band with symmetric side insets, matching
                // what the overlay now hands the packer (host + remotes together).
                let region = Rect::new(0.05 * w, 72.0, w - 0.05 * w, h - 128.0);
                for n in 1..=9 {
                    let rects = grid_rects(region, n, 12.0);
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
        // Phase: grid_is_empty_for_no_remotes.
        {
            assert!(grid_rects(Rect::new(0.0, 0.0, 400.0, 300.0), 0, 12.0).is_empty());
        }
        // Phase: grid_fills_the_region_leaving_no_void.
        // The narrow-width void regression: with the host as the first cell of a
        // unified grid, the packed tiles must *cover* the content region — their
        // bounding box spans it in both axes (within one inter-tile gap) — so no
        // large dead band is left beneath or beside the host at any width. This is
        // the geometry that replaced the old "host pinned left, remotes to the
        // right" layout which left the whole bottom-left corner empty on a
        // portrait window.
        {
            for &(w, h) in &[(1280.0_f64, 720.0_f64), (800.0, 600.0), (560.0, 720.0)] {
                let region = Rect::new(0.05 * w, 72.0, w - 0.05 * w, h - 128.0);
                // A host + three remotes: the demo huddle, the exact case that
                // exposed the void.
                let gap = 12.0;
                let tiles = grid_rects(region, 4, gap);
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
}
