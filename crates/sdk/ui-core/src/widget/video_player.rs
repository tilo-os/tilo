//! `VideoPlayer` — a reusable video surface with a full playback-controls overlay.
//!
//! Two frame sources feed the same `latest_frame`: **engine mode** pumps an
//! attached [`media::MediaEngine`] each [`Widget::sync`] (used by the local
//! player, editor preview, and video app, so streaming + collaborative
//! playback land once). **Push mode** decodes streamed access units handed to
//! [`VideoPlayer::feed_frame`] (H.264) or [`VideoPlayer::feed_encoded`] (any
//! real-time codec) through a [`media::RealtimeDecoder`], which picks a
//! hardware decoder first. Push mode decodes only in builds where `media`'s
//! `realtime` feature is on (any app that streams video enables it); in other
//! builds the feed methods report that nothing was decoded.

use crate::num::f64_of_u64;
use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use std::sync::Arc;

use crate::widget::TaffyTreeExt;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{
    BlendMode, Blob, Color, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format,
};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::{FullscreenProvider, Widget};

const CONTROLS_AUTO_HIDE: Duration = Duration::from_secs(3);
/// Height of the controls Panel at 1x: a Panel row (`metrics.panel_row_height`).
const CONTROLS_BAR_HEIGHT: f64 = 44.0;
/// The transport glyphs are the standard glyph (`metrics.icon_md`), each in
/// a [`MIN_TARGET`] hit rect.
const PLAY_BUTTON_SIZE: f64 = 20.0;
/// Minimum pointer target for every control in the bar (HIG §9.6: a 20 px
/// glyph gets a 32 px hit rect). Glyphs keep their painted size; the hit
/// rect stored for input, tooltips and accessibility is at least this on
/// each axis, clamped to the bar's height.
const MIN_TARGET: f64 = 32.0;
const SCRUB_TRACK_HEIGHT: f64 = 5.0;
/// The unfilled scrub / volume track is the bar's own dim ink at this
/// alpha: derived from the scrim like every other mark on the bar, so it
/// reads on both themes' scrims where a declared `border_standard` did not.
const TRACK_INK_ALPHA: f32 = 0.35;
/// The selected menu option's lift: one translucent step off the panel
/// ground, polarity chosen the way `hairline_on` chooses it (HIG §9.3).
const MENU_SELECTED_ALPHA: u8 = 56;
const SCRUB_THUMB_RADIUS: f64 = 7.0;
const VOLUME_WIDTH: f64 = 78.0;
const KEY_SEEK_STEP: Duration = Duration::from_secs(5);

/// Push mode has no engine to report a real duration, so this stands in.
const PUSH_MODE_DEFAULT_DURATION_MS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

const MENU_ROW_H: f64 = 30.0;
const MENU_WIDTH: f64 = 224.0;
const MENU_WIDTH_F32: f32 = 224.0;
const MIN_SCRUB_WIDTH: f64 = 48.0;

/// The embedding app owns the *meaning*; the player owns the *rendering* and
/// reports interactions back via [`VideoPlayer::take_setting_events`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerSettingControl {
    Toggle {
        on: bool,
    },
    /// Activating opens a themed option list; picking one selects it.
    Choice {
        options: Vec<String>,
        selected: usize,
    },
    Button,
}

/// `priority` decides placement: `0` tucks inside the settings-gear menu;
/// `>= 1` promotes to its own icon button when there is room, highest first,
/// with overflow falling back into the gear.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSetting {
    /// Stable key the host uses to read back changes.
    pub id: String,
    pub icon: Icon,
    /// The tooltip and the gear-menu label.
    pub name: String,
    pub priority: i32,
    pub control: PlayerSettingControl,
}

impl PlayerSetting {
    pub fn toggle(
        id: impl Into<String>,
        icon: Icon,
        name: impl Into<String>,
        priority: i32,
        on: bool,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            name: name.into(),
            priority,
            control: PlayerSettingControl::Toggle { on },
        }
    }

    pub fn choice(
        id: impl Into<String>,
        icon: Icon,
        name: impl Into<String>,
        priority: i32,
        options: Vec<String>,
        selected: usize,
    ) -> Self {
        let selected = selected.min(options.len().saturating_sub(1));
        Self {
            id: id.into(),
            icon,
            name: name.into(),
            priority,
            control: PlayerSettingControl::Choice { options, selected },
        }
    }

    pub fn button(
        id: impl Into<String>,
        icon: Icon,
        name: impl Into<String>,
        priority: i32,
    ) -> Self {
        Self {
            id: id.into(),
            icon,
            name: name.into(),
            priority,
            control: PlayerSettingControl::Button,
        }
    }
}

/// Drained by the host each frame via [`VideoPlayer::take_setting_events`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerSettingEvent {
    Toggled { id: String, on: bool },
    Chose { id: String, index: usize },
    Activated { id: String },
}

fn setting_value(setting: &PlayerSetting) -> String {
    match &setting.control {
        PlayerSettingControl::Toggle { on } => {
            if *on {
                "On".into()
            } else {
                "Off".into()
            }
        }
        PlayerSettingControl::Choice { options, selected } => {
            options.get(*selected).cloned().unwrap_or_default()
        }
        PlayerSettingControl::Button => String::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpenMenu {
    /// Lists every priority-0 + overflowed setting.
    Gear,
    /// A single [`PlayerSettingControl::Choice`]'s option list, anchored on its
    /// own bar icon (`usize` = index into `settings`).
    Choice(usize),
}

/// A hit region inside an open menu, recorded each paint so `handle_event` can
/// route a click without re-deriving the layout.
#[derive(Clone, Copy, Debug)]
enum MenuHit {
    Setting(usize),
    /// `(setting index, option index)`.
    Option(usize, usize),
}

/// One row to be painted inside an open settings menu.
enum MenuRow {
    Setting(usize),
    /// `(setting index, option index)`.
    Option(usize, usize),
}

/// The rows an open menu should paint, plus where to anchor the panel and
/// its optional header (a [`OpenMenu::Choice`] popover names itself; the
/// gear list doesn't).
struct MenuPanel {
    rows: Vec<MenuRow>,
    anchor: Rect,
    title: Option<String>,
}

/// Supplied so the player can publish it to the desktop media controls
/// (MPRIS: media keys + the top-bar widget).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub art_url: String,
}

// Six independent lifecycle/interaction flags (mute, fullscreen intent vs.
// touched, drag state, focus, dirty), not a combinatorial state machine —
// grouping them into an enum would obscure rather than clarify.
#[allow(
    clippy::struct_excessive_bools,
    reason = "flags are independent widget state, not related toggles"
)]
pub struct VideoPlayer {
    /// `None` in push mode.
    engine: Option<media::MediaEngine>,
    /// Lazily constructed on the first non-empty push so engine-mode
    /// consumers never start a decode pipeline. Rebuilt when the pushed
    /// codec changes.
    decoder: Option<media::RealtimeDecoder>,
    decoder_codec: Option<media::StreamCodec>,
    /// Shared render cache for both the engine and push paths.
    latest_frame: Option<PenikoImage>,
    /// What shows before the first frame: a still of the video.
    poster: Option<crate::widget::MediaSource>,
    poster_image: Option<PenikoImage>,
    /// So engine-mode `sync` only rebuilds the peniko image when the
    /// picture actually changed.
    last_frame_pts: Option<u64>,
    state: PlaybackState,
    position_ms: u64,
    duration_ms: u64,
    /// Buffered-range indicator, 0..=100.
    buffering: u8,
    volume: f32,
    muted: bool,
    transport_enabled: bool,
    /// So a fresh player expresses no opinion until asked — see
    /// [`FullscreenProvider::fullscreen_intent`].
    fullscreen: bool,
    fullscreen_touched: bool,
    /// Push-mode consumers that own their own clock.
    on_seek: Option<Box<dyn FnMut(Duration)>>,
    /// Bounds + hit regions, recomputed each draw.
    rect: Rect,
    play_button_rect: Rect,
    scrub_track_rect: Rect,
    volume_track_rect: Rect,
    mute_button_rect: Rect,
    fullscreen_button_rect: Rect,
    /// Painted rect of the elapsed/duration label, `Rect::ZERO` when the bar
    /// had no room for even the position. Not a pointer target; recorded so
    /// the no-overlap contract can include the one non-control in the bar.
    time_label_rect: Rect,
    /// `None` means hidden.
    controls_visible_until: Option<Instant>,
    scrubbing: bool,
    last_cursor: Point,
    focused: bool,
    a11y_focus: Option<String>,
    motion_reduced: bool,
    dirty: bool,
    test_id: Option<&'static str>,
    /// Duration-fraction bands painted on the scrub track — e.g. Tube's
    /// `SponsorBlock`. Empty by default, so other consumers are unaffected.
    scrub_marks: Vec<(f32, f32, Color)>,
    /// Rendered as priority icons on the bar and/or under the settings gear.
    settings: Vec<PlayerSetting>,
    setting_events: Vec<PlayerSettingEvent>,
    /// Terminal engine events are owned by the widget's frame pump, but the
    /// embedding app must be able to report a truthful open failure or advance
    /// a real queue when decoding ends. Progress events stay in the widget's
    /// mirrored fields rather than accumulating when an embedder does not
    /// observe this channel. Drained with [`Self::take_media_events`] so a
    /// terminal status cannot be replayed accidentally across frames.
    media_events: Vec<media::MediaEvent>,
    open_menu: Option<OpenMenu>,
    gear_expanded: Option<usize>,
    /// ZERO when not shown.
    gear_button_rect: Rect,
    /// `(settings index, rect)`, recomputed each draw.
    setting_icon_rects: Vec<(usize, Rect)>,
    /// Priority-0 + overflow, in menu order. Recomputed each draw.
    gear_entries: Vec<usize>,
    menu_hit_rects: Vec<(MenuHit, Rect)>,
    now_playing: NowPlaying,
    /// Inert until [`Self::enable_mpris`].
    mpris: crate::mpris::MprisHandle,
    fullscreen_target: crate::fullscreen::FullscreenTarget,
}

/// Shared geometry threaded through `paint_controls`'s helpers, bundled so
/// none of them needs more than a handful of parameters. `cluster_x0` starts
/// as a placeholder and is filled in by `paint_settings_cluster`.
#[derive(Clone, Copy)]
struct ControlsLayout {
    bar: Rect,
    mid_y: f64,
    pad: f64,
    icon: f64,
    /// Side of the square hit rect every button gets: `MIN_TARGET` clamped
    /// to the bar. Painted glyph rects stay `icon`-sized inside it.
    hit: f64,
    play_btn: Rect,
    cluster_x0: f64,
    /// Ink that clears contrast over the controls scrim, picked by the
    /// theme — `text_primary` is near-black on light themes and vanishes
    /// on the dark scrim.
    ink: Color,
    ink_dim: Color,
}

impl ControlsLayout {
    /// Space between two neighbouring controls' glyphs: at least the pad,
    /// and enough that their pointer targets touch but never overlap.
    fn gap(&self) -> f64 {
        self.pad.max(self.hit - self.icon)
    }

    /// The hit rect for a control whose glyph is painted in `glyph`: at
    /// least `hit` on each axis, centred on the glyph.
    fn hit_rect(&self, glyph: Rect) -> Rect {
        Rect::from_center_size(
            glyph.center(),
            (glyph.width().max(self.hit), glyph.height().max(self.hit)),
        )
    }

    /// The hit rect for a horizontal track painted `SCRUB_TRACK_HEIGHT`
    /// tall: the same x-range (so a click maps to the same fraction the
    /// paint shows) but `hit` tall, centred on the bar's centre line.
    fn track_hit_rect(&self, track: Rect) -> Rect {
        Rect::new(
            track.x0,
            self.mid_y - self.hit / 2.0,
            track.x1,
            self.mid_y + self.hit / 2.0,
        )
    }
}

/// The elapsed/duration label, pre-measured once so every layout helper that
/// needs it shares the same measurement.
struct TimeLabel<'a> {
    text: &'a str,
    width: f32,
    /// Measured line-box height; the label is centred on this, not on
    /// `size`, so it holds the bar's centre line at every text scale.
    height: f32,
    style: TextStyle,
}

impl VideoPlayer {
    pub fn new() -> Self {
        Self {
            engine: None,
            decoder: None,
            decoder_codec: None,
            latest_frame: None,
            poster: None,
            poster_image: None,
            last_frame_pts: None,
            state: PlaybackState::Paused,
            position_ms: 0,
            duration_ms: PUSH_MODE_DEFAULT_DURATION_MS,
            buffering: 100,
            volume: 0.8,
            muted: false,
            transport_enabled: true,
            fullscreen: false,
            fullscreen_touched: false,
            on_seek: None,
            rect: Rect::ZERO,
            play_button_rect: Rect::ZERO,
            scrub_track_rect: Rect::ZERO,
            volume_track_rect: Rect::ZERO,
            mute_button_rect: Rect::ZERO,
            fullscreen_button_rect: Rect::ZERO,
            time_label_rect: Rect::ZERO,
            controls_visible_until: None,
            scrubbing: false,
            last_cursor: Point::ZERO,
            focused: false,
            a11y_focus: None,
            motion_reduced: false,
            dirty: true,
            test_id: None,
            scrub_marks: Vec::new(),
            settings: Vec::new(),
            setting_events: Vec::new(),
            media_events: Vec::new(),
            open_menu: None,
            gear_expanded: None,
            gear_button_rect: Rect::ZERO,
            setting_icon_rects: Vec::new(),
            gear_entries: Vec::new(),
            menu_hit_rects: Vec::new(),
            now_playing: NowPlaying::default(),
            mpris: crate::mpris::MprisHandle::disabled(),
            fullscreen_target: crate::fullscreen::FullscreenTarget::Largest,
        }
    }

    /// A still that shows, contain-fitted on the letterbox, until the first
    /// frame arrives. A [`MediaSource::Url`](crate::widget::MediaSource) loads
    /// in the background; a `Paint` source paints the host's own art on a
    /// 16:9 picture. The controls stand over either.
    #[must_use]
    pub fn with_poster(mut self, poster: crate::widget::MediaSource) -> Self {
        self.set_poster(Some(poster));
        self
    }

    /// Replace or clear the poster.
    pub fn set_poster(&mut self, poster: Option<crate::widget::MediaSource>) {
        self.poster_image = match &poster {
            Some(crate::widget::MediaSource::Image(image)) => Some(image.clone()),
            _ => None,
        };
        self.poster = poster;
        self.resolve_poster();
        self.dirty = true;
    }

    fn resolve_poster(&mut self) {
        if self.poster_image.is_none()
            && let Some(crate::widget::MediaSource::Url { store, url }) = &self.poster
            && let Some(image) = store.get(url)
        {
            self.poster_image = Some(image);
            self.dirty = true;
        }
    }

    /// Marks are fractions `0.0..=1.0` of the duration with an RGB colour.
    /// Only repaints when the set actually changes, so a host may call this
    /// every rebuild.
    pub fn set_sponsor_marks(&mut self, marks: Vec<(f32, f32, Color)>) {
        // Exact equality is the point here — this is change detection for a
        // dirty flag, not a numerical comparison that needs a tolerance.
        #[allow(
            clippy::float_cmp,
            reason = "exact-change detection for a dirty flag, not numerical comparison"
        )]
        let changed = marks.len() != self.scrub_marks.len()
            || marks
                .iter()
                .zip(&self.scrub_marks)
                .any(|(a, b)| a.0 != b.0 || a.1 != b.1);
        if changed {
            self.scrub_marks = marks;
            self.dirty = true;
        }
    }

    /// Replaces an existing setting with the same `id`, so a host may push its
    /// current settings every rebuild.
    pub fn set_setting(&mut self, setting: PlayerSetting) {
        if let Some(existing) = self.settings.iter_mut().find(|s| s.id == setting.id) {
            if *existing != setting {
                *existing = setting;
                self.dirty = true;
            }
        } else {
            self.settings.push(setting);
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn with_setting(mut self, setting: PlayerSetting) -> Self {
        self.set_setting(setting);
        self
    }

    pub fn remove_setting(&mut self, id: &str) {
        let before = self.settings.len();
        self.settings.retain(|s| s.id != id);
        if self.settings.len() != before {
            self.dirty = true;
        }
    }

    pub fn settings(&self) -> &[PlayerSetting] {
        &self.settings
    }

    pub fn setting(&self, id: &str) -> Option<&PlayerSetting> {
        self.settings.iter().find(|s| s.id == id)
    }

    /// Syncs a choice's selected index from the host (e.g. after the backend
    /// confirms a resolution switch) without emitting an event.
    pub fn set_choice_selection(&mut self, id: &str, index: usize) {
        if let Some(s) = self.settings.iter_mut().find(|s| s.id == id)
            && let PlayerSettingControl::Choice { options, selected } = &mut s.control
        {
            let idx = index.min(options.len().saturating_sub(1));
            if *selected != idx {
                *selected = idx;
                self.dirty = true;
            }
        }
    }

    pub fn take_setting_events(&mut self) -> Vec<PlayerSettingEvent> {
        std::mem::take(&mut self.setting_events)
    }

    /// Drain terminal decoder events observed during the widget's normal frame
    /// pump. Only fatal errors and end-of-stream are forwarded: position,
    /// duration, and buffering continuously mirror into the widget and never
    /// accumulate in an unattended embedding app. Playback, rendering, and
    /// accessibility state remain owned by `VideoPlayer`.
    pub fn take_media_events(&mut self) -> Vec<media::MediaEvent> {
        std::mem::take(&mut self.media_events)
    }

    /// Reveals the controls so the menu is visible; lays out on the next paint.
    /// `expand` optionally pre-expands a choice entry by id.
    pub fn open_settings_menu(&mut self, expand: Option<&str>) {
        self.open_menu = Some(OpenMenu::Gear);
        self.gear_expanded = expand.and_then(|id| self.settings.iter().position(|s| s.id == id));
        self.reveal_controls();
        self.dirty = true;
    }

    pub fn menu_open(&self) -> bool {
        self.open_menu.is_some()
    }

    /// `None` if the setting overflowed into the gear (or controls are hidden).
    pub fn setting_icon_rect(&self, id: &str) -> Option<Rect> {
        let idx = self.settings.iter().position(|s| s.id == id)?;
        self.setting_icon_rects
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, r)| *r)
    }

    pub fn setting_in_gear(&self, id: &str) -> bool {
        match self.settings.iter().position(|s| s.id == id) {
            Some(idx) => self.gear_entries.contains(&idx),
            None => false,
        }
    }

    /// Republished automatically whenever it or the transport state changes.
    pub fn set_now_playing(&mut self, np: NowPlaying) {
        if self.now_playing != np {
            self.now_playing = np;
            self.publish_mpris();
        }
    }

    pub fn now_playing(&self) -> &NowPlaying {
        &self.now_playing
    }

    /// `app_id` is the MPRIS bus suffix (`org.mpris.MediaPlayer2.<app_id>`),
    /// `identity` the human-readable player name. Without the `mpris` feature
    /// (or a session bus) this is an inert no-op, so the call is always safe.
    pub fn enable_mpris(&mut self, app_id: &str, identity: &str) {
        if let Some(engine) = self.engine.as_mut() {
            engine.set_handoff_controller(Some(format!("org.mpris.MediaPlayer2.{app_id}")));
        }
        self.mpris = crate::mpris::MprisHandle::spawn(
            crate::mpris::MprisConfig::new(app_id, identity),
            self.mpris_snapshot(),
        );
    }

    /// Call once per frame; inert without an active MPRIS service.
    pub fn pump_media_controls(&mut self) {
        for cmd in self.mpris.drain_commands() {
            self.apply_media_command(cmd);
        }
    }

    fn apply_media_command(&mut self, cmd: crate::mpris::MprisCommand) {
        use crate::mpris::MprisCommand as C;
        match cmd {
            C::Play => self.play(),
            C::Pause => self.pause(),
            C::PlayPause => self.toggle_play(),
            C::Stop => {
                self.pause();
                self.state = PlaybackState::Stopped;
            }
            C::Seek(us) => {
                let delta_ms = us / 1000;
                let position = i64::try_from(self.position_ms).unwrap_or(i64::MAX);
                let target = position.saturating_add(delta_ms).max(0);
                self.seek_to_ms(u64::try_from(target).unwrap_or(0));
            }
            C::SetPosition(us) => {
                self.seek_to_ms(u64::try_from(us.max(0)).unwrap_or(0) / 1000);
            }
            // Volume is a `0.0..=1.0` fraction (clamped again in
            // `set_volume`), so the `f64 -> f32` narrowing cannot lose
            // meaningful precision.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "volume fraction in 0.0..=1.0; f32 precision is sufficient"
            )]
            C::SetVolume(v) => self.set_volume(v as f32),
            // No single-clip meaning here; a host can observe these once it wires a playlist.
            C::Next | C::Previous | C::Raise | C::Quit => {}
        }
    }

    fn mpris_snapshot(&self) -> crate::mpris::PlayerSnapshot {
        use crate::mpris::{PlaybackStatus, PlayerSnapshot};
        let status = match self.state {
            PlaybackState::Playing => PlaybackStatus::Playing,
            PlaybackState::Paused => PlaybackStatus::Paused,
            PlaybackState::Stopped => PlaybackStatus::Stopped,
        };
        PlayerSnapshot {
            status,
            track_id: self.now_playing.track_id.clone(),
            title: self.now_playing.title.clone(),
            artist: self.now_playing.artist.clone(),
            album: String::new(),
            art_url: self.now_playing.art_url.clone(),
            length_us: i64::try_from(self.duration_ms)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
            position_us: i64::try_from(self.position_ms)
                .unwrap_or(i64::MAX)
                .saturating_mul(1000),
            volume: f64::from(self.effective_volume()),
            can_next: false,
            can_prev: false,
            can_play: true,
            can_pause: true,
            can_seek: true,
        }
    }

    fn publish_mpris(&mut self) {
        let snap = self.mpris_snapshot();
        self.mpris.publish(snap);
    }

    #[must_use]
    pub fn with_engine(mut self, engine: media::MediaEngine) -> Self {
        self.set_engine(engine);
        self
    }

    pub fn set_engine(&mut self, engine: media::MediaEngine) {
        self.volume = engine.volume();
        self.engine = Some(engine);
        self.dirty = true;
    }

    /// Attach the engine `build` makes, unless one is attached already, and
    /// answer the attached one. For an app that shows a player long before it
    /// plays anything, or never does: an engine's first construction starts
    /// the decode backend (`GStreamer`'s init, plugin registry and playback
    /// pipeline), which costs hundreds of milliseconds, so building it with
    /// the window delays the window. The volume and mute already set on the
    /// player carry over to the engine.
    pub fn ensure_engine(
        &mut self,
        build: impl FnOnce() -> media::MediaEngine,
    ) -> &mut media::MediaEngine {
        let volume = self.effective_volume();
        let engine = self.engine.get_or_insert_with(|| {
            let mut engine = build();
            engine.set_volume(volume);
            engine
        });
        self.dirty = true;
        engine
    }

    /// So the host can `open` a source or drive collaborative transport
    /// (`apply_transport` / `transport_state`).
    pub fn engine_mut(&mut self) -> Option<&mut media::MediaEngine> {
        self.engine.as_mut()
    }

    pub fn engine(&self) -> Option<&media::MediaEngine> {
        self.engine.as_ref()
    }

    pub fn open(&mut self, source: media::Source) {
        if let Some(engine) = &mut self.engine {
            engine.open(source);
            self.last_frame_pts = None;
            self.dirty = true;
        }
    }

    #[must_use]
    pub fn with_test_id(mut self, id: &'static str) -> Self {
        self.test_id = Some(id);
        self
    }

    #[must_use]
    pub fn on_seek<F: FnMut(Duration) + 'static>(mut self, f: F) -> Self {
        self.on_seek = Some(Box::new(f));
        self
    }

    /// Push one H.264 access unit (Annex-B). Returns `true` when a frame was
    /// decoded and `latest_frame` updated.
    pub fn feed_frame(&mut self, nalus: &[u8]) -> bool {
        self.feed_encoded(media::StreamCodec::H264, nalus)
    }

    /// Push one access unit of `codec`. Returns `true` when a frame was
    /// decoded and `latest_frame` updated. Units before the stream's first
    /// keyframe decode to nothing.
    pub fn feed_encoded(&mut self, codec: media::StreamCodec, unit: &[u8]) -> bool {
        if unit.is_empty() {
            return false;
        }
        let codec_changed = self.decoder_codec.is_some_and(|current| current != codec);
        if self.decoder.is_none() || codec_changed {
            let mut config = media::DecoderConfig::new(codec);
            config.output = media::DecodeOutput::Rgba;
            match media::RealtimeDecoder::new(config) {
                Ok(d) => {
                    self.decoder = Some(d);
                    self.decoder_codec = Some(codec);
                }
                Err(_) => return false,
            }
        }
        let Some(decoder) = self.decoder.as_mut() else {
            return false;
        };
        let Ok(Some(picture)) = decoder.decode(unit, None) else {
            return false;
        };
        let Some(rgba) = picture.frame.to_rgba() else {
            return false;
        };
        self.latest_frame = Some(rgba_image(picture.frame.width, picture.frame.height, rgba));
        self.dirty = true;
        true
    }

    pub fn state(&self) -> PlaybackState {
        self.state
    }

    pub fn latest_frame(&self) -> Option<&PenikoImage> {
        self.latest_frame.as_ref()
    }

    /// Whether a source is loaded, including the asynchronous interval before
    /// its first decoded frame arrives.
    pub fn has_media(&self) -> bool {
        self.latest_frame.is_some()
            || self
                .engine
                .as_ref()
                .is_some_and(|engine| engine.source().is_some())
    }

    pub fn is_playing(&self) -> bool {
        match &self.engine {
            Some(e) => e.is_playing(),
            None => self.state == PlaybackState::Playing,
        }
    }

    pub fn play(&mut self) {
        if let Some(e) = &mut self.engine {
            e.play();
        }
        self.state = PlaybackState::Playing;
        self.dirty = true;
        if self.mpris.is_active() {
            self.publish_mpris();
        }
    }

    pub fn pause(&mut self) {
        if let Some(e) = &mut self.engine {
            e.pause();
        }
        self.state = PlaybackState::Paused;
        self.dirty = true;
        if self.mpris.is_active() {
            self.publish_mpris();
        }
    }

    pub fn toggle_play(&mut self) {
        if !self.transport_enabled {
            return;
        }
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// `ms` is clamped to the duration.
    pub fn seek_to_ms(&mut self, ms: u64) {
        if !self.transport_enabled {
            return;
        }
        let target = ms.min(self.duration_ms);
        self.position_ms = target;
        if let Some(e) = &mut self.engine {
            e.seek_ms(target);
        }
        if let Some(cb) = self.on_seek.as_mut() {
            cb(Duration::from_millis(target));
        }
        self.dirty = true;
        if self.mpris.is_active() {
            self.publish_mpris();
        }
    }

    /// Also unmutes.
    pub fn set_volume(&mut self, v: f32) {
        self.volume = v.clamp(0.0, 1.0);
        self.muted = false;
        if let Some(e) = &mut self.engine {
            e.set_volume(self.volume);
        }
        self.dirty = true;
        if self.mpris.is_active() {
            self.publish_mpris();
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if let Some(e) = &mut self.engine {
            e.set_volume(if self.muted { 0.0 } else { self.volume });
        }
        self.dirty = true;
    }

    /// Disable local play/seek input while a collaborative follower is bound
    /// to an authoritative host. Audio volume and presentation stay local.
    pub fn set_transport_enabled(&mut self, enabled: bool) {
        if self.transport_enabled != enabled {
            self.transport_enabled = enabled;
            self.dirty = true;
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    pub fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        self.fullscreen_touched = true;
        self.dirty = true;
    }

    /// Reflected through [`FullscreenProvider::fullscreen_params`].
    pub fn set_fullscreen_target(&mut self, target: crate::fullscreen::FullscreenTarget) {
        self.fullscreen_target = target;
    }

    pub fn fullscreen_target(&self) -> crate::fullscreen::FullscreenTarget {
        self.fullscreen_target
    }

    fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// Used by the headless `--shots` path so a captured frame shows the full
    /// controls bar (which otherwise auto-hides when no cursor is moving).
    pub fn reveal_controls(&mut self) {
        // `checked_add` rather than `+`: `Instant` addition panics on
        // overflow, though `now + 1h` cannot realistically overflow.
        self.controls_visible_until = crate::time::now().checked_add(Duration::from_secs(3600));
        self.dirty = true;
    }

    fn controls_visible_now(&self) -> bool {
        match self.controls_visible_until {
            Some(deadline) => crate::time::now() < deadline,
            None => false,
        }
    }

    /// Opacity for the shared control/host-overlay fade. Reduced motion keeps
    /// the exact same visibility timing but snaps between endpoints.
    pub fn controls_opacity(&self) -> f32 {
        if self.open_menu.is_some() {
            return 1.0;
        }
        let Some(deadline) = self.controls_visible_until else {
            return 0.0;
        };
        let now = crate::time::now();
        if now >= deadline {
            return 0.0;
        }
        if self.motion_reduced {
            return 1.0;
        }
        // Woken at once by the pointer, gone over the standard exit by the
        // deadline (`motion::Transient`).
        crate::motion::Transient::opacity(
            crate::motion::Transient::CONFIRMATION,
            (now, deadline),
            now,
            crate::tokens::EffectiveMotion::default(),
        )
    }

    /// An app that floats its own affordance over the video (e.g. an Open
    /// button) reads this so its overlay hides in lockstep with the
    /// player's own controls.
    pub fn controls_visible(&self) -> bool {
        self.controls_visible_now() || self.open_menu.is_some()
    }

    fn show_controls(&mut self) {
        self.controls_visible_until = crate::time::now().checked_add(CONTROLS_AUTO_HIDE);
    }

    /// The controls Panel: floating over the bottom of the picture, one
    /// `spacing.s2` in from the sides and the bottom edge.
    fn controls_bar_rect(&self, theme: &Theme) -> Rect {
        let height = if theme.text_scale > 1.25 {
            (CONTROLS_BAR_HEIGHT * f64::from(theme.text_scale)).min(96.0)
        } else {
            CONTROLS_BAR_HEIGHT
        };
        let inset = f64::from(theme.tokens.spacing.s2);
        let y1 = (self.rect.y1 - inset).max(self.rect.y0);
        Rect::new(
            self.rect.x0 + inset,
            (y1 - height).max(self.rect.y0),
            (self.rect.x1 - inset).max(self.rect.x0 + inset),
            y1,
        )
    }

    fn playhead_fraction(&self) -> f64 {
        if self.duration_ms == 0 {
            0.0
        } else {
            (f64_of_u64(self.position_ms) / f64_of_u64(self.duration_ms)).clamp(0.0, 1.0)
        }
    }

    /// Split out of `sync` so the borrow of `self.engine` is fully released
    /// before the fields it mirrors into are written.
    fn pump_engine(&mut self) {
        // Read before borrowing the engine, so the clone-guard decision below
        // can be made inside the closure without a second borrow of `self`.
        let last_pts = self.last_frame_pts;
        let have_frame = self.latest_frame.is_some();
        let snapshot = self.engine.as_mut().map(|engine| {
            let events = engine.poll();
            // Clone the (multi-megabyte) RGBA buffer only on a real pts
            // change — the scroll path re-syncs every frame, so cloning
            // unconditionally was a per-frame memcpy this guard discards.
            let cur_pts = engine.latest_frame().map(|f| f.pts_ms);
            let frame = if cur_pts.is_some() && (last_pts != cur_pts || !have_frame) {
                engine
                    .latest_frame()
                    .map(|f| (f.width, f.height, f.rgba.clone(), f.pts_ms))
            } else {
                None
            };
            (
                engine.position_ms(),
                engine.duration_ms(),
                engine.buffering_percent(),
                engine.is_playing(),
                frame,
                events,
            )
        });
        let Some((pos, dur, buffering, playing, frame, events)) = snapshot else {
            return;
        };
        self.record_terminal_media_events(events);
        if self.position_ms != pos {
            self.position_ms = pos;
            self.dirty = true;
        }
        if let Some(d) = dur
            && d > 0
            && self.duration_ms != d
        {
            self.duration_ms = d;
            self.dirty = true;
        }
        if self.buffering != buffering {
            self.buffering = buffering;
            self.dirty = true;
        }
        let new_state = if playing {
            PlaybackState::Playing
        } else {
            PlaybackState::Paused
        };
        if self.state != new_state {
            self.state = new_state;
            self.dirty = true;
        }
        if let Some((w, h, rgba, pts)) = frame
            && (self.last_frame_pts != Some(pts) || self.latest_frame.is_none())
        {
            self.last_frame_pts = Some(pts);
            self.latest_frame = Some(rgba_image(w, h, rgba));
            self.dirty = true;
        }
        // `publish` dedups sub-second position jitter, so this is quiet on the bus.
        if self.mpris.is_active() {
            self.publish_mpris();
        }
    }

    /// Keep the embedding observation channel bounded under normal playback.
    /// Position/buffering/duration events have already been reflected into the
    /// widget's state by the caller, while the terminal cases (and the missing
    /// decoder that explains an error) require an app-level outcome.
    fn record_terminal_media_events(
        &mut self,
        events: impl IntoIterator<Item = media::MediaEvent>,
    ) {
        self.media_events.extend(events.into_iter().filter(|event| {
            matches!(
                event,
                media::MediaEvent::Ended
                    | media::MediaEvent::Error(_)
                    | media::MediaEvent::MissingDecoder { .. }
            )
        }));
    }

    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // The letterbox is the media's own near-black in every theme, never
        // a theme surface that turns mid-grey in light.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.media.letterbox,
            None,
            &self.rect,
        );

        // Latest frame, contain-fit; before it, the poster; else nothing.
        if let Some(img) = self
            .latest_frame
            .clone()
            .or_else(|| self.poster_image.clone())
        {
            let iw = f64::from(img.width);
            let ih = f64::from(img.height);
            let rw = self.rect.width();
            let rh = self.rect.height();
            if iw > 0.0 && ih > 0.0 && rw > 0.0 && rh > 0.0 {
                let scale = (rw / iw).min(rh / ih);
                let dw = iw * scale;
                let dh = ih * scale;
                let dx = self.rect.x0 + (rw - dw) * 0.5;
                let dy = self.rect.y0 + (rh - dh) * 0.5;
                // Affine composition on f64 fields; see `translated`'s
                // doc comment for why `arithmetic_side_effects` is a
                // false positive here.
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "Affine composition of f64 transforms; cannot meaningfully overflow"
                )]
                let brush_transform =
                    Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
                let dest = Rect::new(dx, dy, dx + dw, dy + dh);
                let image_brush = vello::peniko::ImageBrush::new(img);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    &image_brush,
                    Some(brush_transform),
                    &dest,
                );
            }
        } else if let Some(crate::widget::MediaSource::Paint(paint)) = &self.poster {
            // Art the host paints (a video with no still of its own), on a
            // 16:9 picture contain-fitted like a frame, under the controls.
            let (rw, rh) = (self.rect.width(), self.rect.height());
            let (dw, dh) = if rw * 9.0 > rh * 16.0 {
                (rh * 16.0 / 9.0, rh)
            } else {
                (rw, rw * 9.0 / 16.0)
            };
            let picture = Rect::from_center_size(self.rect.center(), (dw, dh));
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &picture,
            );
            let mut ctx = crate::paint::PaintCtx::new(scene, transform, text_renderer, theme);
            paint(&mut ctx, picture);
            scene.pop_layer();
        } else {
            self.paint_empty_placeholder(scene, transform, theme);
        }

        // Controls overlay, or whenever a settings menu is open (so it can't
        // vanish under the user's cursor).
        if !self.controls_visible_now() && self.open_menu.is_none() {
            self.play_button_rect = Rect::ZERO;
            self.scrub_track_rect = Rect::ZERO;
            self.volume_track_rect = Rect::ZERO;
            self.mute_button_rect = Rect::ZERO;
            self.fullscreen_button_rect = Rect::ZERO;
            self.gear_button_rect = Rect::ZERO;
            self.setting_icon_rects.clear();
            self.menu_hit_rects.clear();
            return;
        }
        let opacity = self.controls_opacity();
        if opacity <= 0.0 {
            return;
        }
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            opacity,
            transform,
            &self.rect,
        );
        self.paint_controls(scene, transform, text_renderer, theme);
        scene.pop_layer();
        self.paint_accessibility_focus(scene, transform, theme);
    }

    fn paint_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let bar = self.controls_bar_rect(theme);
        if bar.width() <= 0.0 || bar.height() <= 0.0 {
            return;
        }
        // Chrome over media is the floating material on the media: a Panel
        // shaped to the controls, blurring the picture behind it. Everything
        // on it is inked for the panel ground.
        crate::widget::floating_surface::paint(scene, transform, bar, theme);
        let panel_theme = theme.on_ground(crate::chrome::panel_ground(theme));
        let theme = &panel_theme;
        let on_panel = theme.on_surface(theme.tokens.colors.surfaces.content);

        if theme.text_scale > 1.25 {
            self.paint_large_text_controls(scene, transform, text_renderer, theme, bar);
            self.paint_menu(scene, transform, text_renderer, theme);
            return;
        }

        let pad = f64::from(theme.tokens.spacing.s2);
        let mid_y = bar.y0 + bar.height() * 0.5;

        let icon = PLAY_BUTTON_SIZE.min(bar.height() - 8.0).max(0.0);
        let hit = MIN_TARGET.min(bar.height()).max(icon);
        let play_btn = Rect::new(
            bar.x0 + pad,
            mid_y - icon * 0.5,
            bar.x0 + pad + icon,
            mid_y + icon * 0.5,
        );

        // Feeds the left reserve that bounds how many setting icons can fit
        // without crushing the scrubber.
        let time_text = format!(
            "{} / {}",
            format_ms(self.position_ms),
            format_ms(self.duration_ms)
        );
        let time_style = theme.text(TextRole::Caption);
        let (tw, th) = text_renderer.measure_style(&time_text, None, &time_style);
        // The cluster reserves room for the label's *shortest* useful form
        // (the position alone): controls outrank the full "m:ss / m:ss",
        // which yields to them, so folding a setting icon into the gear to
        // protect a label that is then not painted would hide a reachable
        // control for nothing.
        let (position_w, _) =
            text_renderer.measure_style(&format_ms(self.position_ms), None, &time_style);

        let mut layout = ControlsLayout {
            bar,
            mid_y,
            pad,
            icon,
            hit,
            play_btn,
            cluster_x0: 0.0,
            ink: on_panel.ink,
            ink_dim: on_panel.ink_dim,
        };
        self.play_button_rect = layout.hit_rect(play_btn);
        self.draw_play_glyph(scene, transform, play_btn, layout.ink);
        self.paint_settings_cluster(scene, transform, theme, &mut layout, position_w);

        let time = TimeLabel {
            text: &time_text,
            width: tw,
            height: th,
            style: time_style,
        };
        self.paint_volume_time_track(scene, transform, text_renderer, theme, &layout, &time);

        self.paint_menu(scene, transform, text_renderer, theme);
    }

    /// Large text uses two intentional rows: an uninterrupted timeline, then
    /// the complete transport/audio/settings cluster. This keeps every semantic
    /// control reachable without shrinking type or letting the time label eat
    /// the scrubber on phone-width surfaces.
    fn paint_large_text_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        bar: Rect,
    ) {
        let pad = f64::from(theme.tokens.spacing.s2).max(8.0);
        let icon = (PLAY_BUTTON_SIZE * 1.25).min(bar.height() * 0.42);
        let timeline_y = bar.y0 + pad + SCRUB_THUMB_RADIUS;
        let timeline_layout = ControlsLayout {
            bar,
            mid_y: timeline_y,
            pad,
            icon,
            play_btn: Rect::ZERO,
            cluster_x0: bar.x1,
            hit: MIN_TARGET.min(bar.height()).max(icon),
            ink: theme.text_primary,
            ink_dim: theme.text_dim,
        };
        self.paint_scrub_track(
            scene,
            transform,
            theme,
            &timeline_layout,
            bar.x0 + pad,
            bar.x1 - pad,
        );

        let (play, mid_y, cluster_x0) =
            self.paint_large_transport_ends(scene, transform, theme, bar, pad, icon);
        self.paint_large_time_volume(
            scene,
            transform,
            text_renderer,
            theme,
            (play, mid_y, cluster_x0, pad, icon),
        );
    }

    fn paint_large_transport_ends(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        bar: Rect,
        pad: f64,
        icon: f64,
    ) -> (Rect, f64, f64) {
        let colors = &theme.tokens.colors;
        let mid_y = bar.y1 - pad - icon * 0.5;
        let play = Rect::new(
            bar.x0 + pad,
            mid_y - icon * 0.5,
            bar.x0 + pad + icon,
            mid_y + icon * 0.5,
        );
        let target = |glyph: Rect| {
            let side = icon.max(MIN_TARGET);
            Rect::from_center_size(glyph.center(), (side, side))
        };
        self.play_button_rect = target(play);
        self.draw_play_glyph(scene, transform, play, colors.text_primary);
        let fs = Rect::new(
            bar.x1 - pad - icon,
            mid_y - icon * 0.5,
            bar.x1 - pad,
            mid_y + icon * 0.5,
        );
        self.fullscreen_button_rect = target(fs);
        draw_fullscreen_glyph(scene, transform, fs, colors.text_primary, self.fullscreen);
        self.setting_icon_rects.clear();
        self.gear_entries.clear();
        self.gear_entries.extend(0..self.settings.len());
        let gear = if self.settings.is_empty() {
            Rect::ZERO
        } else {
            let rect = Rect::new(
                fs.x0 - pad - icon,
                mid_y - icon * 0.5,
                fs.x0 - pad,
                mid_y + icon * 0.5,
            );
            let tint = if self.open_menu == Some(OpenMenu::Gear) {
                colors.intent.inverted
            } else {
                colors.text_primary
            };
            Icon::Settings2.draw(scene, transform, rect, tint);
            rect
        };
        self.gear_button_rect = if gear.width() > 0.0 {
            target(gear)
        } else {
            gear
        };
        (
            play,
            mid_y,
            if gear.width() > 0.0 { gear.x0 } else { fs.x0 },
        )
    }

    fn paint_large_time_volume(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: (Rect, f64, f64, f64, f64),
    ) {
        let (play, mid_y, cluster_x0, pad, icon) = layout;
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;
        let time_text = format!(
            "{} / {}",
            format_ms(self.position_ms),
            format_ms(self.duration_ms)
        );
        let time_size = 12.0;
        let (time_w, _) = text_renderer.measure(&time_text, time_size, None);
        let time_x = play.x1 + pad;
        text_renderer.draw_text(
            scene,
            &time_text,
            time_size,
            colors.text_dim,
            transform.pre_translate(
                (
                    time_x,
                    mid_y - f64::from(time_size) * f64::from(theme.text_scale) * 0.5,
                )
                    .into(),
            ),
            None,
        );

        let mute_x0 = time_x + f64::from(time_w) + pad;
        let mute_extent = (icon * 0.8).max(MIN_TARGET);
        let mute = Rect::new(
            mute_x0,
            mid_y - mute_extent * 0.5,
            (mute_x0 + mute_extent).min(cluster_x0 - pad),
            mid_y + mute_extent * 0.5,
        );
        self.mute_button_rect = if mute.width() >= MIN_TARGET {
            mute
        } else {
            Rect::ZERO
        };
        if self.mute_button_rect.width() > 0.0 {
            draw_speaker_glyph(
                scene,
                transform,
                self.mute_button_rect,
                colors.text_primary,
                self.muted,
            );
        }
        let volume_x0 = self.mute_button_rect.x1 + pad * 0.5;
        let volume_x1 = cluster_x0 - pad;
        if volume_x1 - volume_x0 >= MIN_TARGET {
            let track = Rect::new(
                volume_x0,
                mid_y - SCRUB_TRACK_HEIGHT * 0.5,
                volume_x1,
                mid_y + SCRUB_TRACK_HEIGHT * 0.5,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                colors.border_standard,
                None,
                &RoundedRect::from_rect(track, SCRUB_TRACK_HEIGHT * 0.5),
            );
            let filled = Rect::new(
                track.x0,
                track.y0,
                track.x0 + track.width() * f64::from(self.effective_volume()),
                track.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(filled, SCRUB_TRACK_HEIGHT * 0.5),
            );
            self.volume_track_rect = Rect::new(
                volume_x0,
                mid_y - MIN_TARGET * 0.5,
                volume_x1,
                mid_y + MIN_TARGET * 0.5,
            );
        } else {
            self.volume_track_rect = Rect::ZERO;
        }
    }

    /// Right cluster `[ setting icons ][ gear ][ fullscreen ]`, packed from
    /// the right so fullscreen stays pinned to the corner. Priority `>= 1`
    /// settings get their own icon, highest first; overflow (plus every
    /// priority-0 setting) collapses into the gear. Writes the leftmost
    /// edge the cluster reached into `layout.cluster_x0`, so the caller
    /// knows where the volume control has room to end.
    fn paint_settings_cluster(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        layout: &mut ControlsLayout,
        tw: f32,
    ) {
        let ControlsLayout {
            bar,
            mid_y,
            pad,
            icon,
            play_btn,
            ink,
            ..
        } = *layout;
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;

        let fs_size = icon;
        let btn = icon;
        let gap = layout.gap();

        let (prio, zero) = self.settings_priority_lists();

        let fs_btn = Rect::new(
            bar.x1 - pad - fs_size,
            mid_y - fs_size * 0.5,
            bar.x1 - pad,
            mid_y + fs_size * 0.5,
        );

        let left_reserve =
            play_btn.x1 + pad + MIN_SCRUB_WIDTH + pad + f64::from(tw) + pad + VOLUME_WIDTH + pad;
        let slot = btn + gap;
        let avail = (fs_btn.x0 - gap - left_reserve).max(0.0);
        // `avail` is clamped to >= 0.0 above and `slot > 0.0` is checked, so
        // the quotient is always non-negative and bounded by a realistic
        // count of setting icons — far under `usize::MAX`.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "avail >= 0.0 and slot > 0.0, so the quotient is a small non-negative icon count"
        )]
        let max_fit = if slot > 0.0 {
            (avail / slot).floor() as usize
        } else {
            0
        };
        let (gear_needed, shown) = if zero.is_empty() && prio.len() <= max_fit {
            (false, prio.len())
        } else {
            (true, prio.len().min(max_fit.saturating_sub(1)))
        };

        self.fullscreen_button_rect = layout.hit_rect(fs_btn);
        draw_fullscreen_glyph(scene, transform, fs_btn, ink, self.fullscreen);

        let mut cx = fs_btn.x0 - gap;
        let gear_btn = if gear_needed {
            let r = Rect::new(cx - btn, mid_y - btn * 0.5, cx, mid_y + btn * 0.5);
            cx = r.x0 - gap;
            let tint = if self.open_menu == Some(OpenMenu::Gear) {
                accent
            } else {
                ink
            };
            Icon::Settings2.draw(scene, transform, r, tint);
            r
        } else {
            Rect::ZERO
        };
        self.gear_button_rect = if gear_needed {
            layout.hit_rect(gear_btn)
        } else {
            Rect::ZERO
        };

        // `shown` is always `<= prio.len()` by construction above; `.get()`
        // plus the full-slice fallback keeps that provable to the reader
        // without a raw range-index.
        let shown_slice = prio.get(..shown).unwrap_or(&prio);
        let leftmost_glyph_x0 =
            self.paint_setting_icons(scene, transform, theme, layout, cx, shown_slice);

        self.gear_entries.clear();
        self.gear_entries.extend(prio.iter().skip(shown).copied());
        self.gear_entries.extend(zero.iter().copied());

        // Leftmost *painted* edge the right cluster reached — the volume
        // sits to its left. Glyph edges, not hit edges, so the widened hit
        // rects do not shift the layout.
        layout.cluster_x0 =
            leftmost_glyph_x0.unwrap_or(if gear_needed { gear_btn.x0 } else { fs_btn.x0 });
    }

    /// Priority `>= 1` settings (highest first, ties broken by original
    /// index) and priority-0 settings, both as indices into `self.settings`.
    fn settings_priority_lists(&self) -> (Vec<usize>, Vec<usize>) {
        // Indices only ever come from `0..self.settings.len()`, so they stay
        // valid for the lifetime of any caller — `.get()` still guards each
        // use since `self.settings` isn't provably immutable to the reader.
        let mut prio: Vec<usize> = self
            .settings
            .iter()
            .enumerate()
            .filter(|(_, s)| s.priority >= 1)
            .map(|(i, _)| i)
            .collect();
        prio.sort_by(|&a, &b| {
            let pb = self.settings.get(b).map_or(0, |s| s.priority);
            let pa = self.settings.get(a).map_or(0, |s| s.priority);
            pb.cmp(&pa).then(a.cmp(&b))
        });
        let zero: Vec<usize> = self
            .settings
            .iter()
            .enumerate()
            .filter(|(_, s)| s.priority == 0)
            .map(|(i, _)| i)
            .collect();
        (prio, zero)
    }

    /// Draws each of `shown`'s setting icons leftward from `start_cx`,
    /// pushing its hit rect into `setting_icon_rects`. Returns the leftmost
    /// painted glyph edge, so the caller can lay the volume control out
    /// against what is visible rather than against the wider hit rect.
    fn paint_setting_icons(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        layout: &ControlsLayout,
        start_cx: f64,
        shown: &[usize],
    ) -> Option<f64> {
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;
        let btn = layout.icon;
        let gap = layout.gap();
        let mid_y = layout.mid_y;

        self.setting_icon_rects.clear();
        let mut cx = start_cx;
        let mut leftmost = None;
        for &idx in shown {
            let Some(s) = self.settings.get(idx) else {
                continue;
            };
            let r = Rect::new(cx - btn, mid_y - btn * 0.5, cx, mid_y + btn * 0.5);
            cx = r.x0 - gap;
            leftmost = Some(r.x0);
            let on = matches!(s.control, PlayerSettingControl::Toggle { on: true });
            let menu_open = self.open_menu == Some(OpenMenu::Choice(idx));
            let tint = if on || menu_open { accent } else { layout.ink };
            s.icon.draw(scene, transform, r, tint);
            self.setting_icon_rects.push((idx, layout.hit_rect(r)));
        }
        leftmost
    }

    /// Volume slider, elapsed/duration text, and the scrub track (with
    /// buffering, sponsor marks and the playhead thumb).
    fn paint_volume_time_track(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        layout: &ControlsLayout,
        time: &TimeLabel<'_>,
    ) {
        let ControlsLayout {
            mid_y,
            pad,
            play_btn,
            ink_dim,
            ..
        } = *layout;

        let vol_x0 = self.paint_volume_slider(scene, transform, theme, layout);

        let track_x0 = play_btn.x1 + pad;
        let time_x1 = vol_x0 - pad;
        // The label yields before the scrubber does. Its right edge is fixed
        // by the volume control; when the full "m:ss / m:ss" would push the
        // track under `MIN_SCRUB_WIDTH` — a narrow bar, a long duration,
        // large type — it degrades to the position alone, and then to
        // nothing, rather than painting over the track or the controls
        // beside it (the old `max(play_btn.x1 + pad)` clamp moved the
        // label's left edge but let its right end run under the volume).
        let room = time_x1 - (track_x0 + MIN_SCRUB_WIDTH + pad);
        let shown = if f64::from(time.width) <= room {
            Some((time.text.to_string(), time.width))
        } else {
            let position = format_ms(self.position_ms);
            let (pw, _) = text_renderer.measure_style(&position, None, &time.style);
            (f64::from(pw) <= room).then_some((position, pw))
        };
        let track_x1 = if let Some((text, width)) = shown {
            let time_x0 = time_x1 - f64::from(width);
            let top = mid_y - f64::from(time.height) * 0.5;
            self.time_label_rect = Rect::new(time_x0, top, time_x1, top + f64::from(time.height));
            text_renderer.draw_style(
                scene,
                &text,
                ink_dim,
                transform.pre_translate((time_x0, top).into()),
                None,
                &time.style,
            );
            time_x0 - pad
        } else {
            self.time_label_rect = Rect::ZERO;
            time_x1
        };
        self.paint_scrub_track(scene, transform, theme, layout, track_x0, track_x1);
    }

    /// Draws the speaker glyph and volume level track. Returns its left
    /// edge, so the caller knows where the time label has room to end.
    fn paint_volume_slider(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        layout: &ControlsLayout,
    ) -> f64 {
        let ControlsLayout {
            mid_y,
            pad,
            icon,
            play_btn,
            cluster_x0,
            ink,
            ..
        } = *layout;
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;

        let vol_x1 = cluster_x0 - pad;
        let vol_x0 = (vol_x1 - VOLUME_WIDTH).max(play_btn.x1);
        let speaker = Rect::new(
            vol_x0,
            mid_y - icon * 0.5,
            vol_x0 + icon,
            mid_y + icon * 0.5,
        );
        draw_speaker_glyph(scene, transform, speaker, ink, self.muted);
        // The speaker keeps its whole pointer target; the volume track's own
        // target begins where it ends, so the two never claim one pixel.
        let mute_hit = layout.hit_rect(speaker);
        let vtrack_x0 = mute_hit.x1.max(speaker.x1 + pad * 0.5);
        let vtrack = Rect::new(
            vtrack_x0,
            mid_y - SCRUB_TRACK_HEIGHT * 0.5,
            vol_x1,
            mid_y + SCRUB_TRACK_HEIGHT * 0.5,
        );
        self.mute_button_rect = mute_hit;
        if vtrack.width() > 0.0 {
            self.volume_track_rect = layout.track_hit_rect(vtrack);
            let shape = RoundedRect::from_rect(vtrack, SCRUB_TRACK_HEIGHT * 0.5);
            scene.fill(
                Fill::NonZero,
                transform,
                layout.ink_dim.multiply_alpha(TRACK_INK_ALPHA),
                None,
                &shape,
            );
            let filled = Rect::new(
                vtrack.x0,
                vtrack.y0,
                vtrack.x0 + vtrack.width() * f64::from(self.effective_volume()),
                vtrack.y1,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(filled, SCRUB_TRACK_HEIGHT * 0.5),
            );
        } else {
            self.volume_track_rect = Rect::ZERO;
        }
        vol_x0
    }

    /// Draws the scrub track — background, buffered range, played range,
    /// sponsor-style marks and the playhead thumb — between `track_x0` and
    /// `track_x1`. A non-positive-width range just hides the track.
    fn paint_scrub_track(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        layout: &ControlsLayout,
        track_x0: f64,
        track_x1: f64,
    ) {
        if track_x1 <= track_x0 {
            self.scrub_track_rect = Rect::ZERO;
            return;
        }
        let mid_y = layout.mid_y;
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;

        let track = Rect::new(
            track_x0,
            mid_y - SCRUB_TRACK_HEIGHT * 0.5,
            track_x1,
            mid_y + SCRUB_TRACK_HEIGHT * 0.5,
        );
        // A 5 px track is not a pointer target; the hit rect keeps the
        // track's x-range (so the seek fraction is the one painted) at the
        // minimum target height.
        self.scrub_track_rect = layout.track_hit_rect(track);
        scene.fill(
            Fill::NonZero,
            transform,
            layout.ink_dim.multiply_alpha(TRACK_INK_ALPHA),
            None,
            &RoundedRect::from_rect(track, SCRUB_TRACK_HEIGHT * 0.5),
        );
        let buf_frac = (f64::from(self.buffering) / 100.0).clamp(0.0, 1.0);
        if buf_frac > 0.0 {
            let buffered = Rect::new(
                track.x0,
                track.y0,
                track.x0 + track.width() * buf_frac,
                track.y1,
            );
            // Buffered range in the bar's own dim ink: `text_dim` is chosen
            // against the app surface, not the scrim, and vanished on the
            // light theme's dark scrim.
            scene.fill(
                Fill::NonZero,
                transform,
                layout.ink_dim,
                None,
                &RoundedRect::from_rect(buffered, SCRUB_TRACK_HEIGHT * 0.5),
            );
        }
        let frac = self.playhead_fraction();
        let played = Rect::new(
            track.x0,
            track.y0,
            track.x0 + track.width() * frac,
            track.y1,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            accent,
            None,
            &RoundedRect::from_rect(played, SCRUB_TRACK_HEIGHT * 0.5),
        );
        // SponsorBlock-style marks as slightly-taller bands, visible on the
        // ONE timeline before playback rather than a separate bar.
        let mid_y = track.center().y;
        let band_h = SCRUB_TRACK_HEIGHT + 3.0;
        for (s, e, color) in &self.scrub_marks {
            let s = f64::from((*s).clamp(0.0, 1.0));
            let e = f64::from((*e).clamp(0.0, 1.0));
            if e <= s {
                continue;
            }
            let x0 = track.x0 + track.width() * s;
            let x1 = (track.x0 + track.width() * e).max(x0 + 2.0);
            let band = Rect::new(x0, mid_y - band_h * 0.5, x1, mid_y + band_h * 0.5);
            scene.fill(
                Fill::NonZero,
                transform,
                *color,
                None,
                &RoundedRect::from_rect(band, 1.5),
            );
        }
        let thumb_cx = track.x0 + track.width() * frac;
        scene.fill(
            Fill::NonZero,
            transform,
            accent,
            None,
            &Circle::new((thumb_cx, mid_y), SCRUB_THUMB_RADIUS),
        );
    }

    /// Walks `menu`'s entries into paintable [`MenuRow`]s. `None` only for a
    /// [`OpenMenu::Choice`] whose setting has since vanished (e.g. the host
    /// removed it while the popover was open).
    fn build_menu_rows(&self, menu: OpenMenu) -> Option<MenuPanel> {
        let mut rows = Vec::new();
        let (anchor, title) = match menu {
            OpenMenu::Gear => {
                for &idx in &self.gear_entries {
                    rows.push(MenuRow::Setting(idx));
                    // Options appear indented only while this is the expanded
                    // entry, so the gear reads as a tidy list until drilled in.
                    if self.gear_expanded == Some(idx)
                        && let Some(PlayerSettingControl::Choice { options, .. }) =
                            self.settings.get(idx).map(|s| &s.control)
                    {
                        for oi in 0..options.len() {
                            rows.push(MenuRow::Option(idx, oi));
                        }
                    }
                }
                (self.gear_button_rect, None)
            }
            OpenMenu::Choice(idx) => {
                let s = self.settings.get(idx)?;
                if let PlayerSettingControl::Choice { options, .. } = &s.control {
                    for oi in 0..options.len() {
                        rows.push(MenuRow::Option(idx, oi));
                    }
                }
                let anchor = self
                    .setting_icon_rects
                    .iter()
                    .find(|(i, _)| *i == idx)
                    .map_or(self.gear_button_rect, |(_, r)| *r);
                (anchor, Some(s.name.clone()))
            }
        };
        Some(MenuPanel {
            rows,
            anchor,
            title,
        })
    }

    /// Records `menu_hit_rects` for `handle_event`.
    fn paint_menu(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.menu_hit_rects.clear();
        let Some(menu) = self.open_menu else { return };
        let Some(panel) = self.build_menu_rows(menu) else {
            return;
        };
        if panel.rows.is_empty() && panel.title.is_none() {
            return;
        }
        let colors = &theme.tokens.colors;

        let text_scale = f64::from(theme.text_scale);
        let row_h = MENU_ROW_H * text_scale * if theme.text_scale > 1.25 { 1.35 } else { 1.0 };
        let pad = f64::from(theme.tokens.spacing.s2);
        let title_h = if panel.title.is_some() { row_h } else { 0.0 };
        // The row count is a small UI list (settings + their options), well
        // under any precision limit `f64` could impose.
        #[allow(
            clippy::cast_precision_loss,
            reason = "menu row count is a small UI list, well within f64 precision"
        )]
        let panel_h = title_h + panel.rows.len() as f64 * row_h + pad;
        let width = (MENU_WIDTH * text_scale.min(1.5))
            .min(self.rect.width() - 8.0)
            .max(80.0);
        // Right-aligned to the anchor, clamped inside the surface.
        let x1 = panel.anchor.x1.min(self.rect.x1 - 4.0);
        let x0 = (x1 - width).max(self.rect.x0 + 4.0);
        let y1 = (panel.anchor.y0 - pad * 0.5).max(self.rect.y0 + panel_h + 4.0);
        let y0 = y1 - panel_h;
        let panel_rect = Rect::new(x0, y0, x1, y1);

        // Panel role (HIG §9.1): a settings menu floats over the video, so it
        // takes the panel ground and the panel edge — the declared
        // `border_standard` this replaces vanished on the dark preset.
        crate::widget::floating_surface::paint(scene, transform, panel_rect, theme);

        let mut y = y0 + pad * 0.5;
        if let Some(t) = &panel.title {
            let title_style = theme.text(TextRole::Caption);
            let (_, th) = text_renderer.measure_style(t, None, &title_style);
            text_renderer.draw_style(
                scene,
                t,
                colors.text_dim,
                transform.pre_translate((x0 + pad, y + (row_h - f64::from(th)) * 0.5).into()),
                None,
                &title_style,
            );
            y += row_h;
        }

        for row in &panel.rows {
            let row_rect = Rect::new(x0 + 3.0, y, x1 - 3.0, y + row_h - 2.0);
            self.paint_menu_row(
                scene,
                transform,
                text_renderer,
                theme,
                menu,
                (row, row_rect),
            );
            y += row_h;
        }
    }

    /// Paints one row of an open settings menu and records its hit rect.
    /// `row_rect` alone determines the row's geometry, so the two row kinds
    /// only need it plus their own indices.
    fn paint_menu_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        menu: OpenMenu,
        entry: (&MenuRow, Rect),
    ) {
        let (row, row_rect) = entry;
        match row {
            MenuRow::Setting(idx) => {
                self.paint_setting_row(scene, transform, text_renderer, theme, *idx, row_rect);
            }
            MenuRow::Option(idx, oi) => {
                self.paint_option_row(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    menu,
                    ((*idx, *oi), row_rect),
                );
            }
        }
    }

    /// Paints a [`MenuRow::Setting`] row: icon, name, and its current value
    /// (On/Off, the selected choice, or nothing for a bare button).
    fn paint_setting_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        idx: usize,
        row_rect: Rect,
    ) {
        let Some(s) = self.settings.get(idx).cloned() else {
            return;
        };
        let x1 = row_rect.x1 + 3.0;
        let y = row_rect.y0;
        let row_h = row_rect.height() + 2.0;
        let pad = f64::from(theme.tokens.spacing.s2);
        let text_scale = f64::from(theme.text_scale);
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;

        if theme.text_scale > 1.25 {
            self.paint_large_setting_row(
                scene,
                transform,
                text_renderer,
                theme,
                (idx, row_rect, &s),
            );
            return;
        }

        let icon_r = Rect::new(
            row_rect.x0 + 4.0,
            y + 5.0 * text_scale,
            row_rect.x0 + 4.0 + 18.0 * text_scale.min(1.5),
            y + row_h - 7.0 * text_scale,
        );
        s.icon.draw(scene, transform, icon_r, colors.text_primary);
        // Control label and value are both `scale.sm` (HIG §9.6), centred on
        // the row from their measured line box.
        let label_style = theme.text(TextRole::Caption);
        let (_, lh) = text_renderer.measure_style(&s.name, None, &label_style);
        let label_top = y + (row_h - f64::from(lh)) * 0.5;
        text_renderer.draw_style(
            scene,
            &s.name,
            colors.text_primary,
            transform.pre_translate((icon_r.x1 + 8.0, label_top).into()),
            None,
            &label_style,
        );
        let value: String = match &s.control {
            PlayerSettingControl::Toggle { on } => {
                if *on {
                    "On".into()
                } else {
                    "Off".into()
                }
            }
            PlayerSettingControl::Choice { options, selected } => {
                options.get(*selected).cloned().unwrap_or_default()
            }
            PlayerSettingControl::Button => String::new(),
        };
        if !value.is_empty() {
            let (vw, vh) = text_renderer.measure_style(&value, None, &label_style);
            let tint = matches!(s.control, PlayerSettingControl::Toggle { on: true })
                .then_some(accent)
                .unwrap_or(colors.text_dim);
            text_renderer.draw_style(
                scene,
                &value,
                tint,
                transform.pre_translate(
                    (x1 - pad - f64::from(vw), y + (row_h - f64::from(vh)) * 0.5).into(),
                ),
                None,
                &label_style,
            );
        }
        self.menu_hit_rects.push((MenuHit::Setting(idx), row_rect));
    }

    fn paint_large_setting_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        entry: (usize, Rect, &PlayerSetting),
    ) {
        let (idx, row_rect, setting) = entry;
        let colors = &theme.tokens.colors;
        let row_h = row_rect.height() + 2.0;
        let text_scale = f64::from(theme.text_scale);
        let icon_size = 24.0 * text_scale.min(1.5);
        let icon_r = Rect::new(
            row_rect.x0 + 6.0,
            row_rect.y0 + (row_h - icon_size) * 0.5,
            row_rect.x0 + 6.0 + icon_size,
            row_rect.y0 + (row_h + icon_size) * 0.5,
        );
        setting
            .icon
            .draw(scene, transform, icon_r, colors.text_primary);
        let text_x = icon_r.x1 + 10.0;
        let text_budget = (MENU_WIDTH_F32 * theme.text_scale - 72.0).max(0.0);
        let name = text_renderer.elide_to_width(&setting.name, 13.0, text_budget);
        text_renderer.draw_text(
            scene,
            &name,
            13.0,
            colors.text_primary,
            transform.pre_translate((text_x, row_rect.y0 + 6.0).into()),
            None,
        );
        let value = setting_value(setting);
        if !value.is_empty() {
            let value = text_renderer.elide_to_width(&value, 12.0, text_budget);
            let tint = matches!(setting.control, PlayerSettingControl::Toggle { on: true })
                .then_some(colors.intent.inverted)
                .unwrap_or(colors.text_dim);
            text_renderer.draw_text(
                scene,
                &value,
                12.0,
                tint,
                transform.pre_translate((text_x, row_rect.y0 + row_h * 0.5).into()),
                None,
            );
        }
        self.menu_hit_rects.push((MenuHit::Setting(idx), row_rect));
    }

    /// Paints a [`MenuRow::Option`] row: a [`PlayerSettingControl::Choice`]
    /// option label, highlighted and checked when it's the selected one.
    fn paint_option_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        menu: OpenMenu,
        entry: ((usize, usize), Rect),
    ) {
        let ((idx, oi), row_rect) = entry;
        let Some(s) = self.settings.get(idx) else {
            return;
        };
        let PlayerSettingControl::Choice { options, selected } = &s.control else {
            return;
        };
        let x0 = row_rect.x0 - 3.0;
        let x1 = row_rect.x1 + 3.0;
        let y = row_rect.y0;
        let row_h = row_rect.height() + 2.0;
        let pad = f64::from(theme.tokens.spacing.s2);
        let colors = &theme.tokens.colors;
        let accent = colors.intent.inverted;

        let selected = *selected == oi;
        // The standalone popover has no header, so no indent.
        let indent = if matches!(menu, OpenMenu::Gear) {
            26.0
        } else {
            10.0
        };
        if selected {
            // Selection is a lift, not an accent wash (HIG §9.3): one step
            // off the panel it sits on under an edge derived from the panel.
            // `Colors::lift` is measured against the desktop and has no
            // purchase on a panel, so the step derives from the panel itself.
            let hl_rect = Rect::new(
                row_rect.x0 + 2.0,
                y + 1.0,
                row_rect.x1 - 2.0,
                y + row_h - 3.0,
            );
            let panel = crate::chrome::shell::panel_ground(theme);
            scene.fill(
                Fill::NonZero,
                transform,
                crate::tokens::colors::Colors::hairline_on(panel, MENU_SELECTED_ALPHA),
                None,
                &RoundedRect::from_rect(hl_rect, 6.0),
            );
            scene.stroke(
                &Stroke::new(1.0),
                transform,
                crate::surface::edge_on(panel),
                None,
                &crate::surface::edge_outline(hl_rect, 6.0),
            );
        }
        let label = options.get(oi).cloned().unwrap_or_default();
        let tint = if selected {
            accent
        } else {
            colors.text_primary
        };
        let label_style = theme.text(TextRole::Caption);
        let (_, lh) = text_renderer.measure_style(&label, None, &label_style);
        let label_top = y + (row_h - f64::from(lh)) * 0.5;
        text_renderer.draw_style(
            scene,
            &label,
            tint,
            transform.pre_translate((x0 + indent, label_top).into()),
            None,
            &label_style,
        );
        if selected {
            let (cw, ch) = text_renderer.measure_style("✓", None, &label_style);
            text_renderer.draw_style(
                scene,
                "✓",
                accent,
                transform.pre_translate(
                    (x1 - pad - f64::from(cw), y + (row_h - f64::from(ch)) * 0.5).into(),
                ),
                None,
                &label_style,
            );
        }
        self.menu_hit_rects
            .push((MenuHit::Option(idx, oi), row_rect));
    }

    /// A player with no frame shows only its letterbox: the embedding app
    /// says what is missing (an empty state, an error), and no glyph is
    /// ever painted under that text.
    fn paint_empty_placeholder(&self, _scene: &mut Scene, _transform: Affine, _theme: &Theme) {}

    /// Play and pause are solid marks, each inside the same square glyph box
    /// as every other transport glyph, inset alike on every side.
    fn draw_play_glyph(&self, scene: &mut Scene, transform: Affine, r: Rect, color: Color) {
        let side = r.width().min(r.height());
        if side <= 0.0 {
            return;
        }
        let glyph = Rect::from_center_size(r.center(), (side, side)).inset(-side * 0.18);
        if self.is_playing() {
            let bar = glyph.width() * 0.32;
            for x0 in [glyph.x0, glyph.x1 - bar] {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::new(x0, glyph.y0, x0 + bar, glyph.y1, bar * 0.25),
                );
            }
        } else {
            let mut path = vello::kurbo::BezPath::new();
            path.move_to((glyph.x0, glyph.y0));
            path.line_to((glyph.x1, glyph.center().y));
            path.line_to((glyph.x0, glyph.y1));
            path.close_path();
            scene.fill(Fill::NonZero, transform, color, None, &path);
        }
    }

    fn paint_accessibility_focus(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rect = self.a11y_focus.as_deref().and_then(|target| {
            self.semantic_controls()
                .into_iter()
                .find(|(candidate, _, _)| candidate == target)
                .map(|(_, _, rect)| rect)
        });
        if let Some(rect) = rect.filter(|rect| rect.width() > 0.0 && rect.height() > 0.0) {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.inverted,
                None,
                &RoundedRect::from_rect(rect.inflate(3.0, 3.0), 6.0),
            );
        }
    }
}

fn rgba_image(width: u32, height: u32, rgba: Arc<Vec<u8>>) -> PenikoImage {
    PenikoImage {
        data: Blob::new(rgba),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

/// Every transport glyph is a `ui::icons` icon in one square box, centred
/// in the control's rect so its insets are the same on every side.
fn transport_glyph(scene: &mut Scene, transform: Affine, r: Rect, icon: Icon, color: Color) {
    let side = r.width().min(r.height());
    if side <= 0.0 {
        return;
    }
    icon.draw(
        scene,
        transform,
        Rect::from_center_size(r.center(), (side, side)),
        color,
    );
}

fn draw_speaker_glyph(scene: &mut Scene, transform: Affine, r: Rect, color: Color, muted: bool) {
    let icon = if muted { Icon::VolumeX } else { Icon::Volume2 };
    transport_glyph(scene, transform, r, icon, color);
}

fn draw_fullscreen_glyph(
    scene: &mut Scene,
    transform: Affine,
    r: Rect,
    color: Color,
    active: bool,
) {
    let icon = if active {
        Icon::Minimize2
    } else {
        Icon::Maximize2
    };
    transport_glyph(scene, transform, r, icon, color);
}

fn format_ms(ms: u64) -> String {
    crate::format::timecode(ms / 1000)
}

impl Default for VideoPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl FullscreenProvider for VideoPlayer {
    fn fullscreen_intent(&self) -> Option<bool> {
        if self.fullscreen_touched {
            Some(self.fullscreen)
        } else {
            None
        }
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        crate::fullscreen::FullscreenRequest::mirror(self.fullscreen_target)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        // A compositor may grant or exit fullscreen independently; keep the
        // control and level-triggered intent aligned with the actual state.
        if self.fullscreen != active {
            self.fullscreen = active;
            self.fullscreen_touched = true;
            self.dirty = true;
        }
    }
}

impl Widget for VideoPlayer {
    crate::provides!(dyn crate::widget::FullscreenProvider);

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        self.resolve_poster();
        self.pump_engine();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Fill whatever the host gives us — the same `percent(1.0)` leaf as
        // `ImageViewer`. `auto` only fills when a flex parent stretches the
        // node; as the root of a tree (a gallery cell, a harness) it
        // resolved to 0×0 and the placeholder glyph landed on the origin.
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            flex_grow: 1.0,
            flex_shrink: 1.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_cursor = logical;
                if self.scrubbing {
                    self.seek_from_cursor();
                    self.show_controls();
                    self.dirty = true;
                    *ctx.handled = true;
                } else if self.rect.contains(logical) {
                    self.show_controls();
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if !self.rect.contains(self.last_cursor) {
                    return;
                }
                *ctx.focus_target = Some(ctx.node_id);
                // Control hit regions are ZERO while hidden, so this naturally
                // falls through to the surface play/pause toggle.
                if self.press_at_cursor() {
                    *ctx.handled = true;
                }
                self.show_controls();
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.scrubbing {
                    self.scrubbing = false;
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } if self.handle_key_input(&key_event.logical_key, key_event.state) => {
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "VideoPlayer"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        let on_menu_row = self.menu_hit_rects.iter().any(|(_, r)| r.contains(point));
        let on_setting_icon = self
            .setting_icon_rects
            .iter()
            .any(|(_, r)| r.contains(point));
        if on_menu_row
            || ((self.controls_visible_now() || self.open_menu.is_some())
                && (self.play_button_rect.contains(point)
                    || self.fullscreen_button_rect.contains(point)
                    || self.mute_button_rect.contains(point)
                    || self.scrub_track_rect.contains(point)
                    || self.volume_track_rect.contains(point)
                    || (self.gear_button_rect.width() > 0.0
                        && self.gear_button_rect.contains(point))
                    || on_setting_icon))
        {
            Some(winit::window::CursorIcon::Pointer)
        } else {
            None
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if focused {
            self.show_controls();
        } else {
            self.a11y_focus = None;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_pointer_capture(&self) -> bool {
        // Owns the pointer mid-drag so the seek tracks the cursor even after it
        // leaves the thin track band.
        self.scrubbing
    }

    fn needs_animation_frame(&self) -> bool {
        self.is_playing() || self.controls_visible_now()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let children = self
            .semantic_controls()
            .into_iter()
            .filter(|(_, _, rect)| rect.width() > 0.0 && rect.height() > 0.0)
            .map(|(_, node, rect)| node.with_bounds(rect))
            .collect();
        Some(
            AccessNode::new(Role::Group)
                .with_name("Video player")
                .with_description(if self.has_media() {
                    "Space plays or pauses; Left and Right Arrow seek; Up and Down Arrow change volume; M mutes; F toggles fullscreen"
                } else {
                    "No video loaded"
                })
                .with_state(AccessState {
                    busy: self.is_playing(),
                    focusable: true,
                    focused: self.focused,
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        self.semantic_controls()
            .into_iter()
            .filter(|(_, _, rect)| rect.width() > 0.0 && rect.height() > 0.0)
            .nth(*index)
            .map(|(_, _, rect)| rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else { return false };
        let target = self
            .semantic_controls()
            .into_iter()
            .filter(|(_, _, rect)| rect.width() > 0.0 && rect.height() > 0.0)
            .nth(*index)
            .map(|(target, _, _)| target);
        if let Some(target) = target {
            self.a11y_focus = Some(target);
            self.focused = true;
            self.reveal_controls();
            true
        } else {
            false
        }
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        self.activate_semantic_target(target, action)
    }

    fn tooltip(&self) -> Option<&str> {
        let point = self.last_cursor;
        if self.play_button_rect.contains(point) {
            Some(if self.is_playing() { "Pause" } else { "Play" })
        } else if self.scrub_track_rect.contains(point) {
            Some("Seek")
        } else if self.mute_button_rect.contains(point) {
            Some(if self.muted { "Unmute" } else { "Mute" })
        } else if self.volume_track_rect.contains(point) {
            Some("Volume")
        } else if self.fullscreen_button_rect.contains(point) {
            Some(if self.fullscreen {
                "Exit fullscreen"
            } else {
                "Enter fullscreen"
            })
        } else if self.gear_button_rect.contains(point) {
            Some("Playback settings")
        } else {
            self.setting_icon_rects
                .iter()
                .find(|(_, rect)| rect.contains(point))
                .and_then(|(index, _)| self.settings.get(*index))
                .map(|setting| setting.name.as_str())
        }
    }

    fn set_motion_reduced(&mut self, reduced: bool) {
        self.motion_reduced = reduced;
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl VideoPlayer {
    /// Route one logical keyboard input through the focused player surface.
    ///
    /// The application shell can use this same narrow input surface instead
    /// of manufacturing platform-owned `winit::KeyEvent` values. Returning
    /// `true` means the player consumed the key.
    pub fn handle_key_input(&mut self, logical_key: &Key, state: ElementState) -> bool {
        if !self.focused || state != ElementState::Pressed {
            return false;
        }
        match logical_key {
            Key::Named(NamedKey::Space) if self.has_media() => {
                self.toggle_play();
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowLeft) if self.has_media() => {
                let step = u64::try_from(KEY_SEEK_STEP.as_millis()).unwrap_or(u64::MAX);
                self.seek_to_ms(self.position_ms.saturating_sub(step));
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowRight) if self.has_media() => {
                let step = u64::try_from(KEY_SEEK_STEP.as_millis()).unwrap_or(u64::MAX);
                self.seek_to_ms(self.position_ms.saturating_add(step));
                self.show_controls();
            }
            Key::Character(c) if c.eq_ignore_ascii_case("f") => self.toggle_fullscreen(),
            Key::Character(c) if c.eq_ignore_ascii_case("m") && self.has_media() => {
                self.toggle_mute();
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowUp) if self.has_media() => {
                self.set_volume(self.volume + 0.05);
                self.show_controls();
            }
            Key::Named(NamedKey::ArrowDown) if self.has_media() => {
                self.set_volume(self.volume - 0.05);
                self.show_controls();
            }
            Key::Named(NamedKey::Home) if self.has_media() => {
                self.seek_to_ms(0);
                self.show_controls();
            }
            Key::Named(NamedKey::End) if self.has_media() => {
                self.seek_to_ms(self.duration_ms);
                self.show_controls();
            }
            Key::Named(NamedKey::Escape) if self.open_menu.is_some() => {
                self.open_menu = None;
                self.gear_expanded = None;
                self.dirty = true;
            }
            _ => return false,
        }
        true
    }

    /// Routes a left-press at `self.last_cursor` through the settings menu,
    /// the setting icons + gear, then the standard transport controls, in
    /// that priority order.
    fn press_at_cursor(&mut self) -> bool {
        let p = self.last_cursor;

        // An open menu owns the click: a row wins, a click off the menu (and
        // off the buttons that could re-open it) dismisses it.
        if self.open_menu.is_some() {
            if let Some(hit) = self
                .menu_hit_rects
                .iter()
                .find(|(_, r)| r.contains(p))
                .map(|(h, _)| *h)
            {
                self.activate_menu_hit(hit);
                return true;
            }
            let on_gear = self.gear_button_rect.width() > 0.0 && self.gear_button_rect.contains(p);
            let on_icon = self.setting_icon_rects.iter().any(|(_, r)| r.contains(p));
            if !on_gear && !on_icon {
                self.open_menu = None;
                self.gear_expanded = None;
                self.dirty = true;
                return true;
            }
        }

        if self.gear_button_rect.width() > 0.0 && self.gear_button_rect.contains(p) {
            self.open_menu = if self.open_menu == Some(OpenMenu::Gear) {
                None
            } else {
                Some(OpenMenu::Gear)
            };
            self.gear_expanded = None;
            self.dirty = true;
            return true;
        }

        if let Some((idx, _)) = self
            .setting_icon_rects
            .iter()
            .find(|(_, r)| r.contains(p))
            .copied()
        {
            self.activate_setting_icon(idx);
            return true;
        }

        if self.play_button_rect.contains(p) {
            if self.has_media() {
                self.toggle_play();
            }
            return true;
        }
        if self.fullscreen_button_rect.contains(p) {
            self.toggle_fullscreen();
            return true;
        }
        if self.volume_track_rect.width() > 0.0 && self.volume_track_rect.contains(p) {
            let frac = ((p.x - self.volume_track_rect.x0) / self.volume_track_rect.width())
                .clamp(0.0, 1.0);
            // `frac` is clamped to 0.0..=1.0; `set_volume` clamps again, so
            // the narrowing to `f32` cannot lose meaningful precision.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "frac is clamped to 0.0..=1.0; f32 precision is sufficient for a volume fraction"
            )]
            self.set_volume(frac as f32);
            return true;
        }
        if self.mute_button_rect.width() > 0.0 && self.mute_button_rect.contains(p) {
            if self.has_media() {
                self.toggle_mute();
            }
            return true;
        }
        if self.scrub_track_rect.width() > 0.0 && self.scrub_track_rect.contains(p) {
            self.scrubbing = true;
            self.seek_from_cursor();
            return true;
        }

        // A plain click on the video surface toggles play/pause.
        if self.has_media() {
            self.toggle_play();
        }
        true
    }

    fn activate_menu_hit(&mut self, hit: MenuHit) {
        match hit {
            MenuHit::Setting(idx) => {
                let Some(s) = self.settings.get(idx) else {
                    return;
                };
                match &s.control {
                    PlayerSettingControl::Toggle { .. } => self.toggle_setting(idx),
                    PlayerSettingControl::Button => {
                        let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
                            return;
                        };
                        self.setting_events
                            .push(PlayerSettingEvent::Activated { id });
                        self.open_menu = None;
                        self.dirty = true;
                    }
                    PlayerSettingControl::Choice { .. } => {
                        self.gear_expanded = if self.gear_expanded == Some(idx) {
                            None
                        } else {
                            Some(idx)
                        };
                        self.dirty = true;
                    }
                }
            }
            MenuHit::Option(idx, oi) => self.choose_option(idx, oi),
        }
    }

    fn activate_setting_icon(&mut self, idx: usize) {
        let Some(s) = self.settings.get(idx) else {
            return;
        };
        match &s.control {
            PlayerSettingControl::Toggle { .. } => self.toggle_setting(idx),
            PlayerSettingControl::Button => {
                let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
                    return;
                };
                self.setting_events
                    .push(PlayerSettingEvent::Activated { id });
                self.dirty = true;
            }
            PlayerSettingControl::Choice { .. } => {
                self.open_menu = if self.open_menu == Some(OpenMenu::Choice(idx)) {
                    None
                } else {
                    Some(OpenMenu::Choice(idx))
                };
                self.dirty = true;
            }
        }
    }

    fn toggle_setting(&mut self, idx: usize) {
        let Some(s) = self.settings.get_mut(idx) else {
            return;
        };
        let PlayerSettingControl::Toggle { on } = &mut s.control else {
            return;
        };
        *on = !*on;
        let new_on = *on;
        let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
            return;
        };
        self.setting_events
            .push(PlayerSettingEvent::Toggled { id, on: new_on });
        self.dirty = true;
    }

    fn choose_option(&mut self, idx: usize, oi: usize) {
        let Some(s) = self.settings.get_mut(idx) else {
            return;
        };
        let PlayerSettingControl::Choice { options, selected } = &mut s.control else {
            return;
        };
        if oi >= options.len() {
            return;
        }
        *selected = oi;
        let Some(id) = self.settings.get(idx).map(|s| s.id.clone()) else {
            return;
        };
        self.setting_events
            .push(PlayerSettingEvent::Chose { id, index: oi });
        self.open_menu = None;
        self.gear_expanded = None;
        self.dirty = true;
    }

    fn seek_from_cursor(&mut self) {
        if self.scrub_track_rect.width() <= 0.0 {
            return;
        }
        let frac = ((self.last_cursor.x - self.scrub_track_rect.x0)
            / self.scrub_track_rect.width())
        .clamp(0.0, 1.0);
        // `frac` is clamped to 0.0..=1.0, so the product stays within
        // `0..=duration_ms` and is always non-negative — the `u64` cast
        // cannot truncate or flip sign.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "frac is clamped to 0.0..=1.0, so frac * duration_ms stays within 0..=duration_ms"
        )]
        let target = (frac * f64_of_u64(self.duration_ms)) as u64;
        self.seek_to_ms(target);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the accessibility tree intentionally inventories the complete transport and settings surface in one order-matched function"
    )]
    fn semantic_controls(&self) -> Vec<(String, crate::a11y::AccessNode, Rect)> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let mut controls = Vec::new();
        let focused = |target: &str| self.a11y_focus.as_deref() == Some(target);
        let focusable = |target: &str| AccessState {
            focusable: true,
            focused: focused(target),
            ..Default::default()
        };

        if self.has_media() {
            let play_target = "transport.play-pause".to_string();
            controls.push((
                play_target.clone(),
                AccessNode::new(Role::Button)
                    .with_name(if self.is_playing() { "Pause" } else { "Play" })
                    .with_description("Space")
                    .with_action_target(play_target.clone())
                    .with_state(AccessState {
                        disabled: !self.transport_enabled,
                        ..focusable(&play_target)
                    }),
                self.play_button_rect,
            ));

            let seek_target = "transport.seek".to_string();
            controls.push((
                seek_target.clone(),
                AccessNode::new(Role::Slider)
                    .with_name("Playback position")
                    .with_value(format!(
                        "{} of {}",
                        format_ms(self.position_ms),
                        format_ms(self.duration_ms)
                    ))
                    .with_description("Left and Right Arrow seek five seconds")
                    .with_action_target(seek_target.clone())
                    .with_state(AccessState {
                        disabled: !self.transport_enabled,
                        ..focusable(&seek_target)
                    }),
                self.scrub_track_rect,
            ));

            let mute_target = "audio.mute".to_string();
            controls.push((
                mute_target.clone(),
                AccessNode::new(Role::ToggleButton)
                    .with_name(if self.muted { "Unmute" } else { "Mute" })
                    .with_description("M")
                    .with_action_target(mute_target.clone())
                    .with_state(AccessState {
                        checked: Some(self.muted),
                        ..focusable(&mute_target)
                    }),
                self.mute_button_rect,
            ));

            let volume_target = "audio.volume".to_string();
            controls.push((
                volume_target.clone(),
                AccessNode::new(Role::Slider)
                    .with_name("Volume")
                    .with_value(format!("{}%", (self.effective_volume() * 100.0).round()))
                    .with_action_target(volume_target.clone())
                    .with_state(focusable(&volume_target)),
                self.volume_track_rect,
            ));
        }

        for (idx, rect) in &self.setting_icon_rects {
            let Some(setting) = self.settings.get(*idx) else {
                continue;
            };
            let target = format!("setting:{}", setting.id);
            let mut state = focusable(&target);
            let (role, value) = match &setting.control {
                PlayerSettingControl::Toggle { on } => {
                    state.checked = Some(*on);
                    (Role::ToggleButton, None)
                }
                PlayerSettingControl::Choice { options, selected } => {
                    state.expanded = Some(self.open_menu == Some(OpenMenu::Choice(*idx)));
                    (Role::ComboBox, options.get(*selected).cloned())
                }
                PlayerSettingControl::Button => (Role::Button, None),
            };
            controls.push((
                target.clone(),
                AccessNode::new(role)
                    .with_name(setting.name.clone())
                    .with_optional_value(value)
                    .with_action_target(target.clone())
                    .with_state(state),
                *rect,
            ));
        }

        if self.gear_button_rect.width() > 0.0 {
            let target = "settings.menu".to_string();
            controls.push((
                target.clone(),
                AccessNode::new(Role::Button)
                    .with_name("Playback settings")
                    .with_action_target(target.clone())
                    .with_state(AccessState {
                        expanded: Some(self.open_menu == Some(OpenMenu::Gear)),
                        ..focusable(&target)
                    }),
                self.gear_button_rect,
            ));
        }

        for (hit, rect) in &self.menu_hit_rects {
            match hit {
                MenuHit::Setting(idx) => {
                    let Some(setting) = self.settings.get(*idx) else {
                        continue;
                    };
                    let target = format!("menu-setting:{}", setting.id);
                    let mut state = focusable(&target);
                    let value = match &setting.control {
                        PlayerSettingControl::Toggle { on } => {
                            state.checked = Some(*on);
                            Some(if *on { "On".into() } else { "Off".into() })
                        }
                        PlayerSettingControl::Choice { options, selected } => {
                            state.expanded = Some(self.gear_expanded == Some(*idx));
                            options.get(*selected).cloned()
                        }
                        PlayerSettingControl::Button => None,
                    };
                    controls.push((
                        target.clone(),
                        AccessNode::new(Role::MenuItem)
                            .with_name(setting.name.clone())
                            .with_optional_value(value)
                            .with_action_target(target.clone())
                            .with_state(state),
                        *rect,
                    ));
                }
                MenuHit::Option(idx, option) => {
                    let Some(setting) = self.settings.get(*idx) else {
                        continue;
                    };
                    let PlayerSettingControl::Choice { options, selected } = &setting.control
                    else {
                        continue;
                    };
                    let Some(label) = options.get(*option) else {
                        continue;
                    };
                    let target = format!("setting-option:{}:{option}", setting.id);
                    controls.push((
                        target.clone(),
                        AccessNode::new(Role::MenuItem)
                            .with_name(label.clone())
                            .with_action_target(target.clone())
                            .with_state(AccessState {
                                selected: *selected == *option,
                                ..focusable(&target)
                            }),
                        *rect,
                    ));
                }
            }
        }

        let fullscreen_target = "presentation.fullscreen".to_string();
        controls.push((
            fullscreen_target.clone(),
            AccessNode::new(Role::Button)
                .with_name(if self.fullscreen {
                    "Exit fullscreen"
                } else {
                    "Enter fullscreen"
                })
                .with_description("F")
                .with_action_target(fullscreen_target.clone())
                .with_state(focusable(&fullscreen_target)),
            self.fullscreen_button_rect,
        ));
        controls
    }

    fn setting_index(&self, id: &str) -> Option<usize> {
        self.settings.iter().position(|setting| setting.id == id)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "all semantic targets share one auditable dispatch table matching semantic_controls"
    )]
    fn activate_semantic_target(&mut self, target: &str, action: &crate::a11y::Action) -> bool {
        use crate::a11y::Action;
        match target {
            "transport.play-pause"
                if self.has_media()
                    && self.transport_enabled
                    && matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.toggle_play();
                self.show_controls();
                true
            }
            "transport.seek" if self.has_media() && self.transport_enabled => match action {
                Action::Increment => {
                    self.seek_to_ms(self.position_ms.saturating_add(5_000));
                    true
                }
                Action::Decrement => {
                    self.seek_to_ms(self.position_ms.saturating_sub(5_000));
                    true
                }
                Action::SetValue(value) => value
                    .parse::<f64>()
                    .ok()
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
                    .is_some_and(|seconds| {
                        let millis = crate::time::Duration::from_secs_f64(seconds).as_millis();
                        self.seek_to_ms(u64::try_from(millis).unwrap_or(u64::MAX));
                        true
                    }),
                _ => false,
            },
            "audio.mute"
                if self.has_media() && matches!(action, Action::Activate | Action::ToggleCheck) =>
            {
                self.toggle_mute();
                true
            }
            "audio.volume" if self.has_media() => match action {
                Action::Increment => {
                    self.set_volume(self.volume + 0.05);
                    true
                }
                Action::Decrement => {
                    self.set_volume(self.volume - 0.05);
                    true
                }
                Action::SetValue(value) => {
                    let normalized = value
                        .trim()
                        .strip_suffix('%')
                        .and_then(|raw| raw.parse::<f32>().ok().map(|percent| percent / 100.0));
                    value
                        .parse::<f32>()
                        .ok()
                        .or(normalized)
                        .is_some_and(|volume| {
                            self.set_volume(volume);
                            true
                        })
                }
                _ => false,
            },
            "presentation.fullscreen" if matches!(action, Action::Activate) => {
                self.toggle_fullscreen();
                true
            }
            "settings.menu" => match action {
                Action::Activate | Action::Expand => {
                    self.open_menu = Some(OpenMenu::Gear);
                    self.reveal_controls();
                    self.dirty = true;
                    true
                }
                Action::Collapse => {
                    self.open_menu = None;
                    self.gear_expanded = None;
                    self.dirty = true;
                    true
                }
                _ => false,
            },
            _ => {
                if let Some(id) = target.strip_prefix("setting:")
                    && let Some(index) = self.setting_index(id)
                    && matches!(
                        action,
                        Action::Activate | Action::ToggleCheck | Action::Expand
                    )
                {
                    self.activate_setting_icon(index);
                    return true;
                }
                if let Some(id) = target.strip_prefix("menu-setting:")
                    && let Some(index) = self.setting_index(id)
                    && matches!(
                        action,
                        Action::Activate | Action::ToggleCheck | Action::Expand
                    )
                {
                    self.activate_menu_hit(MenuHit::Setting(index));
                    return true;
                }
                if let Some(rest) = target.strip_prefix("setting-option:")
                    && let Some((id, option)) = rest.rsplit_once(':')
                    && let Some(index) = self.setting_index(id)
                    && let Ok(option) = option.parse::<usize>()
                    && matches!(action, Action::Activate | Action::ToggleCheck)
                {
                    self.choose_option(index, option);
                    return true;
                }
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;
    use automerge::AutoCommit;

    use crate::synthetic_input::cursor_moved;

    use crate::synthetic_input::mouse_press;
    use crate::testing::dispatch;

    #[test]
    fn an_engine_attached_when_needed_keeps_the_players_volume_and_is_built_once() {
        let mut vp = VideoPlayer::new();
        vp.set_volume(0.3);
        assert!(vp.engine().is_none(), "a new player starts no engine");
        let mut built = 0;
        vp.ensure_engine(|| {
            built += 1;
            media::MediaEngine::new_synthesized()
        });
        assert!((vp.engine().unwrap().volume() - 0.3).abs() < 1e-6);
        vp.ensure_engine(|| {
            built += 1;
            media::MediaEngine::new_synthesized()
        });
        assert_eq!(built, 1, "an attached engine is kept");

        let mut muted = VideoPlayer::new();
        muted.toggle_mute();
        let engine = muted.ensure_engine(media::MediaEngine::new_synthesized);
        assert!(
            engine.volume().abs() < 1e-6,
            "a muted player's engine is silent"
        );
    }

    #[test]
    fn transport_keys_and_surface_clicks_drive_the_engine() {
        // Case `video_player_starts_paused`.
        {
            let vp = VideoPlayer::new();
            assert_eq!(vp.state(), PlaybackState::Paused);
            assert!(!vp.is_playing());
        }

        // Case `engine_mode_sync_pulls_a_frame_and_metadata`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            let doc = AutoCommit::new();
            let root = automerge::ROOT;
            vp.sync(&doc, &root);
            assert!(
                vp.latest_frame().is_some(),
                "engine frame pulled into the surface"
            );
            assert!(vp.duration_ms() > 0, "duration mirrored from the engine");
        }

        // Case `space_toggles_play_on_focused_engine_player`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.set_focus(true);
            assert!(!vp.is_playing());
            assert!(vp.handle_key_input(&Key::Named(NamedKey::Space), ElementState::Pressed));
            assert!(vp.is_playing(), "Space started playback via the engine");
            assert!(vp.handle_key_input(&Key::Named(NamedKey::Space), ElementState::Pressed));
            assert!(!vp.is_playing(), "Space paused it again");
        }

        // Case `arrow_keys_seek_the_engine_playhead`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.set_focus(true);
            vp.seek_to_ms(20_000);
            assert!(vp.handle_key_input(&Key::Named(NamedKey::ArrowRight), ElementState::Pressed));
            assert_eq!(vp.position_ms(), 25_000, "→ seeks forward 5s");
            assert!(vp.handle_key_input(&Key::Named(NamedKey::ArrowLeft), ElementState::Pressed));
            assert_eq!(vp.position_ms(), 20_000, "← seeks back 5s");
            // The seek propagated to the engine, not just the widget cache.
            assert_eq!(vp.engine().unwrap().position_ms(), 20_000);
        }

        // Case `clicking_the_video_surface_toggles_play_pause`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
            vp.reveal_controls();
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            assert!(!vp.is_playing());
            // Top-centre: clear of the controls bar, the scrub track, every button.
            dispatch(&mut vp, &cursor_moved(320.0, 40.0));
            assert!(dispatch(&mut vp, &mouse_press()), "surface click handled");
            assert!(vp.is_playing(), "surface click started playback");
            dispatch(&mut vp, &mouse_press());
            assert!(!vp.is_playing(), "a second surface click paused it");
        }
    }

    #[test]
    fn scrub_track_seeks_where_it_is_painted_and_shows_sponsor_bands() {
        // Case `scrub_click_seeks_to_the_track_fraction`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
            vp.duration_ms = 100_000;
            vp.reveal_controls();
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            assert!(vp.scrub_track_rect.width() > 0.0, "scrub track laid out");
            let mid_x = vp.scrub_track_rect.x0 + vp.scrub_track_rect.width() * 0.5;
            let mid_y = vp.scrub_track_rect.center().y;
            dispatch(&mut vp, &cursor_moved(mid_x, mid_y));
            let handled = dispatch(&mut vp, &mouse_press());
            assert!(handled, "scrub press handled");
            let pos = vp.position_ms();
            assert!((45_000..=55_000).contains(&pos), "seeked to ~50%: {pos}ms");
        }

        // Case `dragging_the_scrub_track_seeks_continuously`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
            vp.duration_ms = 100_000;
            vp.reveal_controls();
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let track = vp.scrub_track_rect;
            assert!(track.width() > 0.0);
            let y = track.center().y;
            dispatch(&mut vp, &cursor_moved(track.x0 + track.width() * 0.1, y));
            assert!(
                dispatch(&mut vp, &mouse_press()),
                "press on track begins a scrub"
            );
            assert!(vp.scrubbing, "press started a drag");
            assert!(vp.wants_pointer_capture(), "a scrub owns the pointer");
            let near = vp.position_ms();
            dispatch(&mut vp, &cursor_moved(track.x0 + track.width() * 0.9, y));
            let far = vp.position_ms();
            assert!(
                far > near,
                "dragging right advanced the playhead ({near} → {far})"
            );
            assert!((85_000..=95_000).contains(&far), "drag to ~90%: {far}ms");
        }

        // Case `a_widened_scrub_target_still_seeks_where_the_track_is_painted`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.show_controls();
            draw_once(&mut vp);
            let bar = vp.controls_bar_rect(&Theme::dark());
            let pad = f64::from(Theme::dark().tokens.spacing.s2);
            let icon = PLAY_BUTTON_SIZE.min(bar.height() - 8.0);
            // The painted track begins one pad after the play glyph; the hit
            // rect must start at the same x so a click maps to the same fraction
            // the paint shows.
            let painted_x0 = bar.x0 + pad + icon + pad;
            assert!(
                (vp.scrub_track_rect.x0 - painted_x0).abs() < 1e-6,
                "scrub hit rect x0 {} drifted from the painted track {painted_x0}",
                vp.scrub_track_rect.x0
            );
            assert!(
                vp.scrub_track_rect.height() > SCRUB_TRACK_HEIGHT,
                "the hit rect must be taller than the 5px track it targets"
            );
        }

        // Case `sponsor_marks_emit_bands_on_the_single_timeline`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
            vp.duration_ms = 100_000;
            vp.reveal_controls();
            let baseline = {
                let theme = Theme::dark();
                let mut tr = TextRenderer::new();
                let presence = PresenceCtx::default();
                let mut scene = Scene::new();
                vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
                scene.encoding().draw_tags.len()
            };
            vp.set_sponsor_marks(vec![
                (0.2, 0.3, Color::from_rgb8(0, 0xd4, 0)),
                (0.6, 0.72, Color::from_rgb8(0, 0xff, 0xff)),
            ]);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            assert!(
                scene.encoding().draw_tags.len() > baseline,
                "two sponsor bands add fills to the timeline ({} vs {baseline})",
                scene.encoding().draw_tags.len(),
            );
        }
    }

    #[test]
    fn fullscreen_intent_toggles_and_requests_the_largest_output() {
        // Case `f_key_toggles_fullscreen_intent`.
        {
            let mut vp = VideoPlayer::new();
            vp.set_focus(true);
            assert_eq!(
                FullscreenProvider::fullscreen_intent(&vp),
                None,
                "no opinion until asked"
            );
            assert!(vp.handle_key_input(&Key::Character("f".into()), ElementState::Pressed));
            assert_eq!(FullscreenProvider::fullscreen_intent(&vp), Some(true));
            assert!(vp.handle_key_input(&Key::Character("F".into()), ElementState::Pressed));
            assert_eq!(FullscreenProvider::fullscreen_intent(&vp), Some(false));
        }

        // Case `fullscreen_toggle_drives_a_largest_output_mirror_request`.
        {
            use crate::fullscreen::{FullscreenTarget, FullscreenView};
            let mut vp = VideoPlayer::new();
            assert_eq!(FullscreenProvider::fullscreen_intent(&vp), None);
            assert_eq!(
                FullscreenProvider::fullscreen_params(&vp).target,
                FullscreenTarget::Largest
            );
            vp.toggle_fullscreen();
            assert_eq!(
                FullscreenProvider::fullscreen_intent(&vp),
                Some(true),
                "toggle asks the App to fullscreen"
            );
            let params = FullscreenProvider::fullscreen_params(&vp);
            assert_eq!(
                params.target,
                FullscreenTarget::Largest,
                "immersive video targets the big screen"
            );
            assert_eq!(
                params.view,
                FullscreenView::Mirror,
                "video mirrors the window, not a distinct view"
            );
            vp.set_fullscreen_target(FullscreenTarget::Primary);
            assert_eq!(
                FullscreenProvider::fullscreen_params(&vp).target,
                FullscreenTarget::Primary
            );
        }
    }

    #[test]
    fn media_session_and_engine_events_reach_the_right_side_once() {
        // Case `mpris_snapshot_reflects_metadata_and_controls_on_play`.
        {
            let mut vp = VideoPlayer::new();
            vp.duration_ms = 240_000;
            vp.set_now_playing(NowPlaying {
                track_id: "yt:abc123".into(),
                title: "Runaway".into(),
                artist: "Aurora".into(),
                art_url: "https://img/a.jpg".into(),
            });
            vp.play();
            let snap = vp.mpris_snapshot();
            assert_eq!(
                snap.status,
                crate::mpris::PlaybackStatus::Playing,
                "play → Playing on the bus"
            );
            assert_eq!(snap.title, "Runaway");
            assert_eq!(snap.artist, "Aurora");
            assert_eq!(snap.art_url, "https://img/a.jpg");
            assert_eq!(snap.length_us, 240_000_000, "duration published as µs");
            assert!(
                snap.can_play && snap.can_pause && snap.can_seek,
                "transport controls advertised"
            );
        }

        // Case `external_media_command_pause_reaches_the_player`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.play();
            assert!(vp.is_playing());
            vp.apply_media_command(crate::mpris::MprisCommand::Pause);
            assert!(!vp.is_playing(), "external Pause paused playback");
            vp.apply_media_command(crate::mpris::MprisCommand::PlayPause);
            assert!(vp.is_playing(), "external PlayPause resumed it");
        }

        // Case `engine_lifecycle_events_are_forwarded_once_to_the_embedding_app`.
        {
            let mut vp = VideoPlayer::new();
            vp.record_terminal_media_events([
                media::MediaEvent::Ended,
                media::MediaEvent::MissingDecoder {
                    codec: "HEVC".into(),
                },
                media::MediaEvent::Error("decoder stopped".into()),
            ]);
            let events = vp.take_media_events();
            assert!(
                matches!(events.as_slice(), [media::MediaEvent::Ended, media::MediaEvent::MissingDecoder { codec }, media::MediaEvent::Error(message)] if codec == "HEVC" && message == "decoder stopped")
            );
            assert!(
                vp.take_media_events().is_empty(),
                "taking events cannot replay an old terminal status"
            );
        }

        // Case `nonterminal_engine_events_never_accumulate_for_an_unattended_embedder`.
        {
            let mut vp = VideoPlayer::new();
            for position in 0..1_000 {
                vp.record_terminal_media_events([
                    media::MediaEvent::DurationKnown { ms: 90_000 },
                    media::MediaEvent::Buffering { percent: 50 },
                    media::MediaEvent::Position { ms: position },
                ]);
            }
            assert!(
                vp.media_events.is_empty(),
                "progress is mirrored by the widget, never buffered for the app"
            );
        }
    }

    fn draw_once(vp: &mut VideoPlayer) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn draw_once_with_theme(vp: &mut VideoPlayer, theme: &Theme) {
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    }

    #[test]
    fn over_media_chrome_is_legible_and_dark_in_every_theme() {
        use crate::a11y::{contrast_ratio, relative_luminance};
        for theme in [Theme::dark(), Theme::light()] {
            let name = theme.name.clone();
            // The letterbox stays the media's near-black in a light theme.
            let letterbox = theme.tokens.colors.media.letterbox;
            assert!(
                relative_luminance(letterbox) < 0.02,
                "{name}: the letterbox turned grey"
            );
            // Controls ink is measured against the Panel they sit on.
            let panel = crate::chrome::panel_ground(&theme);
            let on_panel = theme.on_ground(panel);
            assert!(
                contrast_ratio(on_panel.text_primary, panel) >= 4.5,
                "{name}: controls ink on the media Panel"
            );
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.show_controls();
            draw_once_with_theme(&mut vp, &theme);
            let bar = vp.controls_bar_rect(&theme);
            let inset = f64::from(theme.tokens.spacing.s2);
            assert!(
                (bar.x0 - inset).abs() < 1e-9 && (vp.rect.y1 - bar.y1 - inset).abs() < 1e-9,
                "{name}: the controls float as a Panel inside the picture"
            );
            assert!(
                vp.play_button_rect.width() >= f64::from(theme.tokens.metrics.min_pointer_target),
                "{name}: the play control is a full pointer target"
            );
        }
    }

    #[test]
    fn controls_bar_lays_out_reachable_aligned_targets_at_every_size() {
        // Case `large_text_controls_use_two_reachable_rows_while_one_x_is_unchanged`.
        {
            let mut regular = VideoPlayer::new();
            regular.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
            regular.reveal_controls();
            regular.set_setting(PlayerSetting::toggle(
                "captions",
                Icon::FileText,
                "Captions",
                2,
                false,
            ));
            let one_x = Theme::dark();
            draw_once_with_theme(&mut regular, &one_x);
            assert_eq!(
                regular.controls_bar_rect(&one_x).height(),
                CONTROLS_BAR_HEIGHT
            );
            assert_eq!(
                regular.scrub_track_rect.center().y,
                regular.play_button_rect.center().y
            );

            let mut accessible = VideoPlayer::new();
            accessible.rect = regular.rect;
            accessible.reveal_controls();
            accessible.set_setting(PlayerSetting::toggle(
                "captions",
                Icon::FileText,
                "Captions",
                2,
                false,
            ));
            let two_x = Theme::dark().with_text_scale(2.0);
            draw_once_with_theme(&mut accessible, &two_x);
            let bar = accessible.controls_bar_rect(&two_x);
            assert_eq!(bar.height(), CONTROLS_BAR_HEIGHT * 2.0);
            assert!(accessible.scrub_track_rect.width() > 350.0);
            assert!(accessible.scrub_track_rect.center().y < accessible.play_button_rect.y0);
            assert!(bar.contains(accessible.play_button_rect.center()));
            assert!(bar.contains(accessible.fullscreen_button_rect.center()));
            assert!(bar.contains(accessible.gear_button_rect.center()));
            assert!(accessible.setting_icon_rects.is_empty());
            assert!(accessible.setting_in_gear("captions"));
        }

        // Case `every_bar_control_meets_the_minimum_pointer_target`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.show_controls();
            vp.set_setting(PlayerSetting::toggle(
                "captions",
                Icon::FileText,
                "Captions",
                2,
                false,
            ));
            vp.set_setting(PlayerSetting::choice(
                "resolution",
                Icon::Monitor,
                "Resolution",
                0,
                vec!["1080p".into(), "720p".into()],
                0,
            ));
            draw_once(&mut vp);
            for (name, r) in bar_targets(&vp) {
                assert!(
                    r.width() >= MIN_TARGET - 1e-6 && r.height() >= MIN_TARGET - 1e-6,
                    "{name} hit rect {r:?} is under the {MIN_TARGET}px minimum target"
                );
            }
        }

        // Case `bar_controls_share_one_centre_line_and_never_overlap`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.show_controls();
            vp.set_setting(PlayerSetting::toggle(
                "captions",
                Icon::FileText,
                "Captions",
                2,
                false,
            ));
            draw_once(&mut vp);
            let bar = vp.controls_bar_rect(&Theme::dark());
            let mid_y = bar.center().y;
            let targets = bar_targets(&vp);
            for (name, r) in &targets {
                assert!(
                    (r.center().y - mid_y).abs() < 1e-6,
                    "{name} centre {} is off the bar's centre line {mid_y}",
                    r.center().y
                );
            }
            for (i, (a_name, a)) in targets.iter().enumerate() {
                for (b_name, b) in targets.iter().skip(i + 1) {
                    let overlap = a.intersect(*b);
                    assert!(
                        overlap.is_zero_area(),
                        "{a_name} {a:?} and {b_name} {b:?} claim the same pixels: {overlap:?}"
                    );
                }
            }
        }

        // Case `time_label_yields_to_the_scrubber_rather_than_overlapping_controls`.
        // The time label is the one thing in the bar that is not a pointer
        // target, and it used to be the one thing allowed to overlap them: its
        // left edge was clamped to the play button while its right end ran
        // under the volume control. Now it yields — position only, then
        // nothing — so at every width and text scale the scrubber keeps its
        // floor and no control is painted over.
        {
            for (width, scale) in [(300.0, 1.0), (300.0, 2.0), (960.0, 2.0), (420.0, 1.0)] {
                let mut vp = VideoPlayer::new();
                vp.rect = Rect::new(0.0, 0.0, width, 200.0);
                vp.show_controls();
                vp.duration_ms = 2 * 60 * 60 * 1000;
                vp.position_ms = 60 * 60 * 1000 + 8000;
                draw_at_text_scale(&mut vp, scale);
                assert!(
                    vp.scrub_track_rect.width() >= MIN_SCRUB_WIDTH - 1e-6,
                    "at {width}px / {scale}x the scrubber ({:?}) lost its floor",
                    vp.scrub_track_rect
                );
                let label = vp.time_label_rect;
                for (name, r) in bar_targets(&vp) {
                    let overlap = label.intersect(r);
                    assert!(
                        overlap.is_zero_area(),
                        "at {width}px / {scale}x the time label {label:?} paints over {name} {r:?}"
                    );
                }
            }
        }

        // Case `a_wide_bar_paints_the_full_time_label`.
        // The degrade is a last resort: on a bar with room the full
        // "m:ss / m:ss" is painted, exactly as wide as it measures.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.show_controls();
            vp.duration_ms = 120_000;
            vp.position_ms = 8_000;
            draw_once(&mut vp);
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let full = format!(
                "{} / {}",
                format_ms(vp.position_ms),
                format_ms(vp.duration_ms)
            );
            let (tw, _) = tr.measure_style(&full, None, &theme.text(TextRole::Caption));
            assert!(
                (vp.time_label_rect.width() - f64::from(tw)).abs() < 1e-6,
                "label rect {:?} is not the measured full label ({tw})",
                vp.time_label_rect
            );
            assert!(
                (vp.time_label_rect.center().y - vp.controls_bar_rect(&Theme::dark()).center().y)
                    .abs()
                    < 1e-6,
                "the label left the bar's centre line"
            );
        }

        // Case `player_embeds_flush_with_no_forced_internal_padding`.
        {
            let mut vp = VideoPlayer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map = std::collections::HashMap::new();
            let theme = Theme::dark();
            let node = vp.build_layout(&mut taffy, &theme, &mut node_map);
            let style = taffy.style(node).unwrap();
            let default: taffy::Style = taffy::Style::default();
            assert_eq!(
                format!("{:?}", style.padding),
                format!("{:?}", default.padding),
                "no forced internal padding — embeds edge-to-edge"
            );
            assert_eq!(
                format!("{:?}", style.margin),
                format!("{:?}", default.margin),
                "no forced margin"
            );
            assert_eq!(style.flex_grow, 1.0, "the player fills its slot");
        }
    }

    #[test]
    fn settings_surface_by_priority_and_round_trip_through_the_gear() {
        // Case `mixed_priority_settings_surface_as_icons_and_under_the_gear`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.reveal_controls();
            // A high-priority toggle (own icon) + a priority-0 choice (under the gear).
            vp.set_setting(PlayerSetting::toggle(
                "captions",
                Icon::FileText,
                "Captions",
                2,
                false,
            ));
            vp.set_setting(PlayerSetting::choice(
                "resolution",
                Icon::Monitor,
                "Resolution",
                0,
                vec!["1080p".into(), "720p".into(), "480p".into()],
                0,
            ));
            draw_once(&mut vp);
            assert!(
                vp.setting_icon_rect("captions").is_some(),
                "priority-2 setting surfaces as its own bar icon"
            );
            assert!(
                vp.setting_in_gear("resolution"),
                "priority-0 setting lives under the settings gear"
            );
            assert!(
                !vp.setting_in_gear("captions"),
                "the promoted icon is not also in the gear"
            );
        }

        // Case `higher_priority_wins_limited_space_and_overflow_falls_into_the_gear`.
        {
            let mut vp = VideoPlayer::new();
            // Narrow bar + many priority settings ⇒ only the top few fit as icons.
            vp.rect = Rect::new(0.0, 0.0, 420.0, 236.0);
            vp.reveal_controls();
            for p in 1..=8 {
                vp.set_setting(PlayerSetting::button(
                    format!("act{p}"),
                    Icon::Star,
                    format!("Action {p}"),
                    p,
                ));
            }
            draw_once(&mut vp);
            let icon_prios: Vec<i32> = vp
                .setting_icon_rects
                .iter()
                .map(|(i, _)| vp.settings[*i].priority)
                .collect();
            let overflow_prios: Vec<i32> = vp
                .gear_entries
                .iter()
                .map(|&i| vp.settings[i].priority)
                .collect();
            assert!(!icon_prios.is_empty(), "some settings fit as icons");
            assert!(
                !overflow_prios.is_empty(),
                "the rest overflow into the gear at this width"
            );
            let min_icon = *icon_prios.iter().min().unwrap();
            let max_overflow = *overflow_prios.iter().max().unwrap();
            assert!(
                min_icon >= max_overflow,
                "every shown icon outranks every overflowed setting ({min_icon} >= {max_overflow})"
            );
        }

        // Case `resolution_setting_round_trips_through_the_player`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.reveal_controls();
            vp.set_setting(PlayerSetting::choice(
                "resolution",
                Icon::Monitor,
                "Resolution",
                2, // its own bar icon
                vec!["1080p".into(), "720p".into(), "480p".into()],
                0,
            ));
            draw_once(&mut vp);
            let icon = vp
                .setting_icon_rect("resolution")
                .expect("resolution has a bar icon");
            dispatch(&mut vp, &cursor_moved(icon.center().x, icon.center().y));
            assert!(dispatch(&mut vp, &mouse_press()), "icon click handled");
            assert!(
                vp.menu_open(),
                "clicking the resolution icon opened its option list"
            );
            draw_once(&mut vp);
            let row = vp
                .menu_hit_rects
                .iter()
                .find(|(h, _)| matches!(h, MenuHit::Option(_, 1)))
                .map(|(_, r)| *r)
                .expect("720p option row laid out");
            dispatch(&mut vp, &cursor_moved(row.center().x, row.center().y));
            assert!(dispatch(&mut vp, &mouse_press()), "option click handled");
            let events = vp.take_setting_events();
            assert_eq!(
                events,
                vec![PlayerSettingEvent::Chose {
                    id: "resolution".into(),
                    index: 1
                }],
                "picking 720p emits a Chose event the host drains"
            );
            match &vp.setting("resolution").unwrap().control {
                PlayerSettingControl::Choice { selected, .. } => assert_eq!(*selected, 1),
                _ => panic!("resolution is a choice"),
            }
            assert!(!vp.menu_open(), "picking an option closes the popover");
        }

        // Case `gear_toggle_setting_flips_and_emits`.
        {
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
            vp.reveal_controls();
            // Priority 0 ⇒ under the gear.
            vp.set_setting(PlayerSetting::toggle(
                "autoplay",
                Icon::Play,
                "Autoplay",
                0,
                false,
            ));
            draw_once(&mut vp);
            let gear = vp.gear_button_rect;
            assert!(gear.width() > 0.0, "gear present for the tucked setting");
            dispatch(&mut vp, &cursor_moved(gear.center().x, gear.center().y));
            assert!(dispatch(&mut vp, &mouse_press()), "gear click handled");
            assert!(vp.menu_open(), "gear opened the menu");
            draw_once(&mut vp);
            let row = vp
                .menu_hit_rects
                .iter()
                .find(|(h, _)| matches!(h, MenuHit::Setting(_)))
                .map(|(_, r)| *r)
                .expect("autoplay row laid out under the gear");
            dispatch(&mut vp, &cursor_moved(row.center().x, row.center().y));
            assert!(dispatch(&mut vp, &mouse_press()), "row click handled");
            let events = vp.take_setting_events();
            assert_eq!(
                events,
                vec![PlayerSettingEvent::Toggled {
                    id: "autoplay".into(),
                    on: true
                }],
                "toggling the gear row flips it on and emits"
            );
        }
    }

    #[test]
    fn accessibility_exposes_transport_state_and_truthful_controls() {
        // Case `accessibility_reports_group_role_and_busy_while_playing`.
        {
            use crate::a11y::Role;
            let mut vp = VideoPlayer::new();
            let node = Widget::accessibility(&vp).expect("player is not presentational");
            assert_eq!(node.role, Role::Group);
            assert_eq!(node.name.as_deref(), Some("Video player"));
            assert!(!node.state.busy);
            vp.play();
            assert!(Widget::accessibility(&vp).unwrap().state.busy);
        }

        // Case `accessibility_exposes_every_painted_transport_control_and_drives_it`.
        {
            use crate::a11y::{Action, Role};
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.rect = Rect::new(0.0, 0.0, 760.0, 480.0);
            vp.reveal_controls();
            draw_once(&mut vp);

            let node = Widget::accessibility(&vp).unwrap();
            let names: Vec<_> = node
                .children
                .iter()
                .filter_map(|child| child.name.as_deref())
                .collect();
            assert!(names.contains(&"Play"), "{names:?}");
            assert!(names.contains(&"Playback position"), "{names:?}");
            assert!(names.contains(&"Mute"), "{names:?}");
            assert!(names.contains(&"Volume"), "{names:?}");
            assert!(names.contains(&"Enter fullscreen"), "{names:?}");
            assert!(node.children.iter().all(|child| child.bounds.is_some()));
            assert!(node.children.iter().any(|child| child.role == Role::Slider));

            assert!(Widget::perform_semantic_action(
                &mut vp,
                "transport.play-pause",
                &Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert!(vp.is_playing());
            assert!(Widget::perform_semantic_action(
                &mut vp,
                "audio.volume",
                &Action::SetValue("25%".into()),
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert!((vp.volume() - 0.25).abs() < 1e-6);
        }

        // Case `follower_transport_is_truthfully_disabled_but_local_audio_still_works`.
        {
            let mut engine = media::MediaEngine::new_synthesized();
            engine.open(media::Source::Remote {
                url: "https://x/v.mp4".into(),
            });
            let mut vp = VideoPlayer::new().with_engine(engine);
            vp.set_transport_enabled(false);
            vp.toggle_play();
            assert!(!vp.is_playing());
            vp.seek_to_ms(30_000);
            assert_eq!(vp.position_ms(), 0);
            vp.set_volume(0.3);
            assert!((vp.volume() - 0.3).abs() < 1e-6);
        }
    }

    /// Push mode through a real encoder and decoder. Needs the `realtime`
    /// feature of `media` (GStreamer); VP9 in software needs no GPU.
    #[cfg(feature = "realtime-video")]
    #[test]
    fn push_mode_feed_encoded_updates_latest_image() {
        let (w, h) = (64u32, 48u32);
        let mut config = media::EncoderConfig::new(media::StreamCodec::Vp9, w, h, 30);
        config.policy = media::realtime::RealtimePolicy::SoftwareOnly;
        let mut enc = media::RealtimeEncoder::new(&config).expect("VP9 software encoder");
        let rgba = vec![128u8; (w * h * 4) as usize];
        let unit = enc
            .encode(media::InputFrame::Packed(&rgba), std::time::Duration::ZERO)
            .expect("encode grey frame")
            .remove(0);

        let mut vp = VideoPlayer::new();
        assert!(
            vp.feed_encoded(media::StreamCodec::Vp9, &unit.data),
            "decoder consumed the frame"
        );
        let frame = vp.latest_frame().expect("a frame");
        assert_eq!((frame.width, frame.height), (w, h));
    }

    #[test]
    fn surface_paints_pushed_frames_or_a_placeholder() {
        // Case `push_mode_ignores_empty_units`.
        {
            let mut vp = VideoPlayer::new();
            assert!(!vp.feed_frame(&[]));
            assert!(vp.latest_frame().is_none());
        }

        // Case `empty_surface_paints_a_placeholder_and_no_image`.
        {
            use vello_encoding::DrawTag;
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 320.0, 240.0);
            assert!(vp.latest_frame().is_none());
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let image_count = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::IMAGE)
                .count();
            assert_eq!(
                image_count, 0,
                "no frame → no image draw in the empty state"
            );
            // Only the letterbox: the app's empty state or error says what is
            // missing, and no glyph is painted under its text.
            assert_eq!(
                scene.encoding().draw_tags.len(),
                1,
                "an empty player paints its letterbox and nothing else"
            );
        }

        // Case `paint_with_image_uses_image_draw_command`.
        {
            use vello_encoding::DrawTag;
            let mut vp = VideoPlayer::new();
            vp.rect = Rect::new(0.0, 0.0, 64.0, 48.0);
            vp.latest_frame = Some(rgba_image(16, 16, Arc::new(vec![255u8; 4 * 16 * 16])));
            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let image_count = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::IMAGE)
                .count();
            assert!(
                image_count >= 1,
                "expected an IMAGE draw, got {image_count}"
            );
        }
    }

    /// Every pointer target the bar exposes, named, in one place.
    fn bar_targets(vp: &VideoPlayer) -> Vec<(String, Rect)> {
        let mut out = vec![
            ("play".to_string(), vp.play_button_rect),
            ("scrub".to_string(), vp.scrub_track_rect),
            ("volume".to_string(), vp.volume_track_rect),
            ("mute".to_string(), vp.mute_button_rect),
            ("fullscreen".to_string(), vp.fullscreen_button_rect),
        ];
        if vp.gear_button_rect.width() > 0.0 {
            out.push(("gear".to_string(), vp.gear_button_rect));
        }
        for (idx, r) in &vp.setting_icon_rects {
            out.push((format!("setting-{idx}"), *r));
        }
        out
    }

    /// Paint once with the renderer at `scale` (the a11y text-scale
    /// convention the `Host` palette source uses), so the label is measured
    /// and drawn larger than the theme's type size says.
    fn draw_at_text_scale(vp: &mut VideoPlayer, scale: f32) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        tr.set_text_scale(scale);
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    #[test]
    fn controls_ink_and_edges_read_on_both_presets() {
        // Case `controls_ink_reads_over_the_scrim_in_the_light_theme`.
        {
            use crate::offscreen::OffscreenRenderer;
            let theme = Theme::light();
            let mut vp = VideoPlayer::new();
            let (w, h) = (320.0, 120.0);
            vp.rect = Rect::new(0.0, 0.0, w, h);
            vp.show_controls();
            let mut tr = TextRenderer::new();
            let presence = PresenceCtx::default();
            let mut scene = Scene::new();
            vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a 320x120 test surface"
            )]
            let rgba = OffscreenRenderer::new(w as u32, h as u32)
                .render(&scene, theme.tokens.colors.surfaces.content);
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "pixel coordinates inside a 320x120 surface"
            )]
            let sample = |p: Point| {
                let idx = ((p.y as usize) * (w as usize) + p.x as usize) * 4;
                Color::from_rgb8(rgba[idx], rgba[idx + 1], rgba[idx + 2])
            };
            // Inside the paused play triangle: for a glyph box of side `s` the
            // triangle spans x ∈ [cx − 0.5s·½ … cx + 0.7s·½]; (cx − 4, cy) is
            // well inside it at the 30px glyph.
            let c = vp.play_button_rect.center();
            let glyph = sample(Point::new(c.x - 4.0, c.y));
            // A bare patch of the painted scrim: just right of the play target,
            // above the scrub track, inside the bar.
            let bar = vp.controls_bar_rect(&Theme::dark());
            let scrim = sample(Point::new(vp.play_button_rect.x1 + 2.0, bar.y0 + 3.0));
            let ratio = crate::a11y::contrast_ratio(glyph, scrim);
            assert!(
                ratio >= 4.5,
                "play glyph contrast {ratio:.2}:1 against the painted scrim on the light theme \
             (WCAG AA is 4.5:1): near-black text_primary on the dark scrim; ink must be \
             chosen against the scrim"
            );
        }

        // Case `ux1x_controls_bar_and_menu_edges_register_on_both_presets`.
        // The controls bar's top hairline, its unfilled track and the open
        // settings menu keep a **visible derived edge** at 1× on both presets
        // (HIG §9.5). Before, all three wore `border_standard` — one fixed
        // colour per theme, measured against neither theme's scrim, three
        // shades from the dark raised menu — and the light theme's track was
        // `text_dim` (near-black) on a near-black scrim.
        //
        // Writes `$UX1X_F4_OUT/<theme>/video_player.png` plus the harness
        // inspection when that variable is set; asserts regardless.
        {
            use crate::offscreen::OffscreenRenderer;
            const EDGE_MIN: f64 = 0.008;
            let (w, h) = (640.0, 360.0);
            // Evidence first, for both presets, so a failing assertion on one
            // preset still leaves the other's PNG and inspection on disk.
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                if let Some(png) = ux1x_out(&theme, "video_player.png") {
                    let harness = crate::headless::FrameHarness::new_with_theme(
                        crate::State::new(),
                        Box::new(ux1x_fixture()),
                        (640, 360),
                        theme.clone(),
                    );
                    harness.render_png(&png);
                    if let Ok(json) = harness.inspection_json() {
                        std::fs::write(png.with_extension("inspection.json"), json)
                            .expect("write inspection");
                    }
                }
            }
            for theme in [Theme::dark(), Theme::light()] {
                let theme = theme.with_reduce_motion(true);
                let mut vp = ux1x_fixture();
                vp.rect = Rect::new(0.0, 0.0, w, h);
                let mut tr = TextRenderer::new();
                let presence = PresenceCtx::default();
                let mut scene = Scene::new();
                vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a 640x360 test surface"
                )]
                let rgba = OffscreenRenderer::new(w as u32, h as u32)
                    .render(&scene, theme.tokens.colors.surfaces.content);
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "a 640x360 test surface"
                )]
                let lum = |x: f64, y: f64| lum_at(&rgba, w as u32, x, y);

                // Controls Panel: its edge against its ground just inside it.
                let bar = vp.controls_bar_rect(&theme);
                let edge = lum(bar.x0, bar.center().y);
                let ground = lum(bar.x0 + 4.0, bar.center().y);
                assert!(
                    (edge - ground).abs() >= EDGE_MIN,
                    "{}: controls Panel edge {edge:.4} vs ground {ground:.4} did not register",
                    theme.name
                );

                // Track: the unfilled end (past the 40 % buffered range) against
                // the bare scrim beside it.
                let hit = vp.scrub_track_rect;
                let track = lum(hit.x1 - 3.0, hit.center().y);
                let beside = lum(hit.x1 - 3.0, hit.center().y - 7.0);
                assert!(
                    (track - beside).abs() >= EDGE_MIN,
                    "{}: unfilled track {track:.4} vs scrim {beside:.4} did not register",
                    theme.name
                );

                // Menu: the panel edge (the row hit rects sit 3 px inside the
                // panel) against the panel ground just inside it.
                let first_row = vp
                    .menu_hit_rects
                    .first()
                    .map(|(_, rect)| *rect)
                    .expect("the open menu painted rows");
                let panel_x0 = first_row.x0 - 3.0;
                let edge = lum(panel_x0, first_row.center().y);
                let ground = lum(panel_x0 + 2.0, first_row.center().y);
                assert!(
                    (edge - ground).abs() >= EDGE_MIN,
                    "{}: menu edge {edge:.4} vs panel {ground:.4} did not register",
                    theme.name
                );
            }
        }
    }

    /// Relative luminance of the composited pixel at (`x`, `y`).
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "probe coordinates are non-negative pixel positions inside a small test surface"
    )]
    fn lum_at(rgba: &[u8], w: u32, x: f64, y: f64) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(
            rgba,
            w,
            x.floor() as u32,
            y.floor() as u32,
        );
        crate::a11y::relative_luminance(Color::from_rgb8(r, g, b))
    }

    /// `$UX1X_F4_OUT/<theme>/<name>` when the 1× review asked for evidence.
    fn ux1x_out(theme: &Theme, name: &str) -> Option<std::path::PathBuf> {
        let dir = std::path::PathBuf::from(std::env::var_os("UX1X_F4_OUT")?)
            .join(theme.name.to_lowercase());
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir.join(name))
    }

    /// A player with the controls shown, a partly buffered track and the
    /// settings menu open on an expanded choice — every surface the bar
    /// paints, in one frame.
    fn ux1x_fixture() -> VideoPlayer {
        let mut vp = VideoPlayer::new();
        vp.buffering = 40;
        vp.set_setting(PlayerSetting::toggle(
            "captions",
            Icon::FileText,
            "Captions",
            0,
            true,
        ));
        vp.set_setting(PlayerSetting::choice(
            "speed",
            Icon::Settings2,
            "Speed",
            0,
            vec!["0.5×".into(), "1×".into(), "1.5×".into()],
            1,
        ));
        vp.show_controls();
        vp.open_settings_menu(Some("speed"));
        vp
    }
}
