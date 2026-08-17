//! VideoPlayer — a reusable video surface with a full playback-controls overlay.
//!
//! The widget paints the latest decoded picture scaled-to-fit and overlays a
//! controls bar: play/pause, a scrubbable seek bar with a buffered range,
//! current / total time, a volume control and a fullscreen toggle. Controls
//! auto-hide a few seconds after the last cursor movement; keyboard shortcuts
//! (Space play/pause, ←/→ seek, F fullscreen, M mute) work while the surface is
//! focused. Colours come from the [`Theme`], never hard-coded.
//!
//! ## Two frame sources, one surface
//!
//! * **Engine mode** (the shared path) — the widget owns a
//!   [`media::MediaEngine`]. Each [`Widget::sync`] pumps the engine, pulls the
//!   freshest frame, and reflects its playhead / duration / buffering. This is
//!   what the local video player, the editor preview and the video app all use,
//!   so streaming + collaborative playback land once. Attach one with
//!   [`VideoPlayer::with_engine`] / [`VideoPlayer::set_engine`].
//! * **Push mode** (legacy) — call sites feed raw H.264 NAL units via
//!   [`VideoPlayer::feed_frame`], decoded in-process by openh264. Preserved so
//!   existing consumers keep working while they migrate onto the engine.
//!
//! Both paths land the picture in the same `latest_frame`, so `draw` is shared.

use std::sync::Arc;
use std::time::{Duration, Instant};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Blob, Color, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::widget::{EmptyState, Widget};

/// How long the controls bar stays visible after the last cursor movement.
const CONTROLS_AUTO_HIDE: Duration = Duration::from_secs(3);

/// Height of the controls overlay band at the bottom of the rect.
const CONTROLS_BAR_HEIGHT: f64 = 44.0;

/// Side length of the square play/pause icon button at the left of the bar.
const PLAY_BUTTON_SIZE: f64 = 30.0;

/// Vertical thickness of the scrub track.
const SCRUB_TRACK_HEIGHT: f64 = 5.0;

/// Radius of the scrub thumb.
const SCRUB_THUMB_RADIUS: f64 = 7.0;

/// Width reserved for the volume control (speaker + level track).
const VOLUME_WIDTH: f64 = 78.0;

/// Seek step for the ←/→ keyboard shortcuts.
const KEY_SEEK_STEP: Duration = Duration::from_secs(5);

/// Fallback stream length (ms) used only in push mode, where the widget has no
/// engine to report a real duration.
const PUSH_MODE_DEFAULT_DURATION_MS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlaybackState {
    Playing,
    Paused,
    Stopped,
}

/// Height of one row in the settings-gear menu overlay.
const MENU_ROW_H: f64 = 30.0;
/// Width of the settings menu / choice popover panel.
const MENU_WIDTH: f64 = 224.0;
/// Minimum scrub-track width preserved when packing setting icons onto the bar.
const MIN_SCRUB_WIDTH: f64 = 48.0;

/// The value + control kind of an app-registered [`PlayerSetting`].
///
/// The embedding app owns the *meaning*; the player owns the *rendering* and
/// reports interactions back via [`VideoPlayer::take_setting_events`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerSettingControl {
    /// An on/off switch (captions, autoplay, loop, …). Activating flips it.
    Toggle { on: bool },
    /// A one-of-N choice (resolution, playback speed, caption track, …).
    /// Activating opens a themed option list; picking one selects it.
    Choice { options: Vec<String>, selected: usize },
    /// A plain action button (Cast, Download, Open externally, …).
    Button,
}

/// A control the embedding app registers on the player's control bar.
///
/// `priority` decides placement:
/// * `0` → tucked inside the settings-gear menu.
/// * `>= 1` → promoted to its own icon button on the control bar when there is
///   room; higher priority wins the limited space and any overflow falls back
///   into the gear.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSetting {
    /// Stable key the host uses to read back changes.
    pub id: String,
    /// Vector glyph shown on the bar / in the menu row.
    pub icon: Icon,
    /// Human-readable name — the tooltip and the gear-menu label.
    pub name: String,
    /// Placement priority (see the struct docs).
    pub priority: i32,
    /// The value + control kind.
    pub control: PlayerSettingControl,
}

impl PlayerSetting {
    /// A toggle setting. `priority >= 1` promotes it to its own icon button.
    pub fn toggle(id: impl Into<String>, icon: Icon, name: impl Into<String>, priority: i32, on: bool) -> Self {
        Self { id: id.into(), icon, name: name.into(), priority, control: PlayerSettingControl::Toggle { on } }
    }

    /// A one-of-N choice setting (e.g. resolution).
    pub fn choice(
        id: impl Into<String>,
        icon: Icon,
        name: impl Into<String>,
        priority: i32,
        options: Vec<String>,
        selected: usize,
    ) -> Self {
        let selected = selected.min(options.len().saturating_sub(1));
        Self { id: id.into(), icon, name: name.into(), priority, control: PlayerSettingControl::Choice { options, selected } }
    }

    /// A plain action button.
    pub fn button(id: impl Into<String>, icon: Icon, name: impl Into<String>, priority: i32) -> Self {
        Self { id: id.into(), icon, name: name.into(), priority, control: PlayerSettingControl::Button }
    }
}

/// A user interaction with a registered [`PlayerSetting`], drained by the host
/// each frame via [`VideoPlayer::take_setting_events`].
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerSettingEvent {
    /// A toggle flipped to `on`.
    Toggled { id: String, on: bool },
    /// A choice selected option `index`.
    Chose { id: String, index: usize },
    /// A button was activated.
    Activated { id: String },
}

/// Which overlay menu is currently open, and what it is anchored to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum OpenMenu {
    /// The gear menu: lists every priority-0 + overflowed setting.
    Gear,
    /// A single [`PlayerSettingControl::Choice`] setting's option list, anchored
    /// on its own bar icon (`usize` = index into `settings`).
    Choice(usize),
}

/// A hit region inside an open menu, recorded each paint so `handle_event` can
/// route a click without re-deriving the layout.
#[derive(Clone, Copy, Debug)]
enum MenuHit {
    /// A top-level setting row in the gear menu (`usize` = index into `settings`).
    Setting(usize),
    /// An option row of a choice (`setting index`, `option index`).
    Option(usize, usize),
}

/// Now-playing metadata the embedding app supplies so the player can publish it
/// to the desktop media controls (MPRIS: media keys + the top-bar widget).
/// `title`/`artist` show in the system media widget; `art_url` is an http(s)
/// cover; `track_id` keys the MPRIS track identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NowPlaying {
    pub track_id: String,
    pub title: String,
    /// Channel / uploader / artist line.
    pub artist: String,
    pub art_url: String,
}

/// The centered call-to-action a player shows while **no media is loaded** — a
/// hero icon, a title, an optional subtitle and an optional action pill,
/// rendered through the shared [`EmptyState`]. It replaces the bare placeholder
/// ring so an idle player reads as an intentional, inviting zero-state ("Open a
/// video") rather than a fake loaded clip. Configure it with
/// [`VideoPlayer::with_empty_state`]; the host drains action-pill clicks via
/// [`VideoPlayer::take_empty_action`] (e.g. to open the file portal).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EmptyMedia {
    /// Hero icon shown in a soft tinted disc above the title.
    pub icon: Option<Icon>,
    /// The headline (e.g. "Open a video").
    pub title: String,
    /// Optional dim one-liner under the title (e.g. "or drop a file here").
    pub message: Option<String>,
    /// Optional accent action-pill label (e.g. "Open"). `None` ⇒ no pill.
    pub action: Option<String>,
}

/// Video surface with an overlaid playback-controls bar.
pub struct VideoPlayer {
    /// Shared streaming engine (engine mode). `None` in push mode.
    engine: Option<media::MediaEngine>,
    /// Legacy in-process H.264 decoder for [`Self::feed_frame`]. Lazily
    /// constructed on the first non-empty push so engine-mode consumers never
    /// touch the openh264 runtime.
    decoder: Option<openh264::decoder::Decoder>,
    /// Most recently produced picture, RGBA. Shared render cache for both the
    /// engine and push paths.
    latest_frame: Option<PenikoImage>,
    /// PTS of the frame currently cached in `latest_frame`, so engine-mode
    /// `sync` only rebuilds the peniko image when the picture actually changes.
    last_frame_pts: Option<u64>,
    /// Reflected playback state (engine-derived in engine mode; set by
    /// `play`/`pause` in push mode).
    state: PlaybackState,
    /// Playhead + stream metadata mirrored from the engine (or defaults in push
    /// mode).
    position_ms: u64,
    duration_ms: u64,
    /// Buffering / prefetch progress 0..=100 (the buffered-range indicator).
    buffering: u8,
    /// Output volume 0..=1 and mute flag.
    volume: f32,
    muted: bool,
    /// Fullscreen intent, and whether the user has ever toggled it (so a fresh
    /// player expresses no fullscreen opinion until asked — see
    /// [`Widget::fullscreen_intent`]).
    fullscreen: bool,
    fullscreen_touched: bool,
    /// Optional seek callback (push-mode consumers that own their own clock).
    on_seek: Option<Box<dyn FnMut(Duration)>>,
    /// Bounds + hit regions, recomputed each draw.
    rect: Rect,
    play_button_rect: Rect,
    scrub_track_rect: Rect,
    volume_track_rect: Rect,
    fullscreen_button_rect: Rect,
    /// Controls fade-out deadline. `None` means hidden.
    controls_visible_until: Option<Instant>,
    /// Whether the user is mid-drag on the scrub track.
    scrubbing: bool,
    last_cursor: Point,
    focused: bool,
    /// Repaint dirty flag (incremental redraw).
    dirty: bool,
    /// Stable e2e/test identifier, when set by the host.
    test_id: Option<&'static str>,
    /// Optional coloured marks painted onto the scrub track as fractions of the
    /// duration — used by clients (e.g. Tube's SponsorBlock) to show community
    /// segments inline on the ONE timeline. Empty by default, so every other
    /// consumer (camera, music, editor preview) is unaffected.
    scrub_marks: Vec<(f32, f32, Color)>,
    /// App-registered settings (resolution, captions, speed, …). Rendered as
    /// priority icons on the bar and/or under the settings gear.
    settings: Vec<PlayerSetting>,
    /// Interactions with `settings` the host drains each frame.
    setting_events: Vec<PlayerSettingEvent>,
    /// The currently open settings overlay, if any.
    open_menu: Option<OpenMenu>,
    /// Which choice-setting is currently expanded inside the gear menu.
    gear_expanded: Option<usize>,
    /// Hit region for the settings gear button (ZERO when not shown).
    gear_button_rect: Rect,
    /// Bar icon-button hit regions: `(settings index, rect)`, recomputed each draw.
    setting_icon_rects: Vec<(usize, Rect)>,
    /// Which settings indices are tucked under the gear this frame (priority 0 +
    /// overflow), in menu order. Recomputed each draw.
    gear_entries: Vec<usize>,
    /// Open-menu row hit regions, recomputed each draw.
    menu_hit_rects: Vec<(MenuHit, Rect)>,
    /// Now-playing metadata the host supplies for the MPRIS top-bar publish.
    now_playing: NowPlaying,
    /// The desktop-media (MPRIS) publisher. Inert until [`Self::enable_mpris`].
    mpris: crate::mpris::MprisHandle,
    /// Fullscreen output target expressed when the user toggles fullscreen.
    fullscreen_target: crate::fullscreen::FullscreenTarget,
    /// Zero-state CTA shown while no media is loaded (see [`EmptyMedia`]).
    /// `None` keeps the bare placeholder ring (unchanged for push-mode consumers
    /// that never configure one).
    empty: Option<EmptyMedia>,
    /// Hit rect of the empty-state action pill, recomputed each draw. ZERO when
    /// no CTA is shown this frame (media loaded, or no pill configured).
    empty_action_rect: Rect,
    /// Drained flag: the empty-state action pill was clicked this frame.
    empty_action_fired: bool,
}

impl VideoPlayer {
    /// Construct a paused player with no engine, no decoder and no frame.
    pub fn new() -> Self {
        Self {
            engine: None,
            decoder: None,
            latest_frame: None,
            last_frame_pts: None,
            state: PlaybackState::Paused,
            position_ms: 0,
            duration_ms: PUSH_MODE_DEFAULT_DURATION_MS,
            buffering: 100,
            volume: 0.8,
            muted: false,
            fullscreen: false,
            fullscreen_touched: false,
            on_seek: None,
            rect: Rect::ZERO,
            play_button_rect: Rect::ZERO,
            scrub_track_rect: Rect::ZERO,
            volume_track_rect: Rect::ZERO,
            fullscreen_button_rect: Rect::ZERO,
            controls_visible_until: None,
            scrubbing: false,
            last_cursor: Point::ZERO,
            focused: false,
            dirty: true,
            test_id: None,
            scrub_marks: Vec::new(),
            settings: Vec::new(),
            setting_events: Vec::new(),
            open_menu: None,
            gear_expanded: None,
            gear_button_rect: Rect::ZERO,
            setting_icon_rects: Vec::new(),
            gear_entries: Vec::new(),
            menu_hit_rects: Vec::new(),
            now_playing: NowPlaying::default(),
            mpris: crate::mpris::MprisHandle::disabled(),
            fullscreen_target: crate::fullscreen::FullscreenTarget::Largest,
            empty: None,
            empty_action_rect: Rect::ZERO,
            empty_action_fired: false,
        }
    }

    /// Replace the coloured scrub-track marks (fractions `0.0..=1.0` of the
    /// duration, each with an RGB colour). Painted on the single timeline so a
    /// client can show community segments (SponsorBlock) inline. A no-op-looking
    /// empty vec clears them. Only repaints when the set actually changes, so
    /// setting it every rebuild is cheap.
    pub fn set_sponsor_marks(&mut self, marks: Vec<(f32, f32, Color)>) {
        if marks.len() != self.scrub_marks.len()
            || marks.iter().zip(&self.scrub_marks).any(|(a, b)| a.0 != b.0 || a.1 != b.1)
        {
            self.scrub_marks = marks;
            self.dirty = true;
        }
    }

    // --- App-registered player settings (resolution, captions, speed, …) ---

    /// Register a setting, or replace an existing one with the same `id`. The
    /// player renders it as a priority icon and/or a gear-menu row and reports
    /// interactions via [`Self::take_setting_events`]. Registering is idempotent
    /// on `id`, so a host may push its current settings every rebuild.
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

    /// Builder form of [`Self::set_setting`].
    pub fn with_setting(mut self, setting: PlayerSetting) -> Self {
        self.set_setting(setting);
        self
    }

    /// Remove a registered setting by `id` (no-op if absent).
    pub fn remove_setting(&mut self, id: &str) {
        let before = self.settings.len();
        self.settings.retain(|s| s.id != id);
        if self.settings.len() != before {
            self.dirty = true;
        }
    }

    /// The registered settings, in registration order.
    pub fn settings(&self) -> &[PlayerSetting] {
        &self.settings
    }

    /// Borrow a registered setting by `id`.
    pub fn setting(&self, id: &str) -> Option<&PlayerSetting> {
        self.settings.iter().find(|s| s.id == id)
    }

    /// Keep a choice setting's selected index in sync from the host (e.g. after
    /// the backend confirms a resolution switch) without emitting an event.
    pub fn set_choice_selection(&mut self, id: &str, index: usize) {
        if let Some(s) = self.settings.iter_mut().find(|s| s.id == id) {
            if let PlayerSettingControl::Choice { options, selected } = &mut s.control {
                let idx = index.min(options.len().saturating_sub(1));
                if *selected != idx {
                    *selected = idx;
                    self.dirty = true;
                }
            }
        }
    }

    /// Drain the interactions with registered settings since the last call. The
    /// host reacts to each (e.g. re-opens the stream at a new resolution).
    pub fn take_setting_events(&mut self) -> Vec<PlayerSettingEvent> {
        std::mem::take(&mut self.setting_events)
    }

    /// Programmatically open the settings-gear menu (e.g. an onboarding hint, or
    /// a headless capture). Reveals the controls so the menu is visible; it lays
    /// out on the next paint. `expand` optionally pre-expands a choice entry by id.
    pub fn open_settings_menu(&mut self, expand: Option<&str>) {
        self.open_menu = Some(OpenMenu::Gear);
        self.gear_expanded = expand.and_then(|id| self.settings.iter().position(|s| s.id == id));
        self.reveal_controls();
        self.dirty = true;
    }

    /// Whether the settings gear (or a choice popover) is open. Exposed so hosts
    /// / tests can assert the menu surfaced.
    pub fn menu_open(&self) -> bool {
        self.open_menu.is_some()
    }

    /// The bar rect a priority-`>=1` setting was laid out into this frame, or
    /// `None` if it overflowed into the gear (or the controls are hidden). Lets
    /// hosts / tests confirm a high-priority setting surfaced as its own icon.
    pub fn setting_icon_rect(&self, id: &str) -> Option<Rect> {
        let idx = self.settings.iter().position(|s| s.id == id)?;
        self.setting_icon_rects
            .iter()
            .find(|(i, _)| *i == idx)
            .map(|(_, r)| *r)
    }

    /// Whether a setting is currently tucked under the settings gear this frame.
    pub fn setting_in_gear(&self, id: &str) -> bool {
        match self.settings.iter().position(|s| s.id == id) {
            Some(idx) => self.gear_entries.contains(&idx),
            None => false,
        }
    }

    // --- Now-playing / desktop media (MPRIS) ---

    /// Supply the now-playing metadata surfaced to the desktop media controls
    /// (system top-bar widget + media keys). Republished automatically whenever
    /// it or the transport state changes.
    pub fn set_now_playing(&mut self, np: NowPlaying) {
        if self.now_playing != np {
            self.now_playing = np;
            self.publish_mpris();
        }
    }

    /// The current now-playing metadata.
    pub fn now_playing(&self) -> &NowPlaying {
        &self.now_playing
    }

    /// Bring up the desktop-media (MPRIS) service so media keys + the system
    /// media widget can drive this player and show its now-playing. `app_id` is
    /// the MPRIS bus suffix (`org.mpris.MediaPlayer2.<app_id>`) and `identity`
    /// the human-readable player name. Without the `mpris` feature (or a session
    /// bus) this is an inert no-op, so the call is always safe.
    pub fn enable_mpris(&mut self, app_id: &str, identity: &str) {
        self.mpris = crate::mpris::MprisHandle::spawn(
            crate::mpris::MprisConfig::new(app_id, identity),
            self.mpris_snapshot(),
        );
    }

    /// Drain external media-control commands (media keys, the system media
    /// widget, `playerctl`) and apply them to the engine. Call once per frame;
    /// inert without an active MPRIS service.
    pub fn pump_media_controls(&mut self) {
        for cmd in self.mpris.drain_commands() {
            self.apply_media_command(cmd);
        }
    }

    /// Apply one external MPRIS command to the player/engine.
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
                let target = (self.position_ms as i64 + delta_ms).max(0) as u64;
                self.seek_to_ms(target);
            }
            C::SetPosition(us) => self.seek_to_ms((us.max(0) as u64) / 1000),
            C::SetVolume(v) => self.set_volume(v as f32),
            // Next / Previous / Raise / Quit have no single-clip meaning here; the
            // host can observe them if it later wires a playlist.
            C::Next | C::Previous | C::Raise | C::Quit => {}
        }
    }

    /// Build the MPRIS snapshot from the current transport + metadata.
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
            length_us: self.duration_ms as i64 * 1000,
            position_us: self.position_ms as i64 * 1000,
            volume: self.effective_volume() as f64,
            can_next: false,
            can_prev: false,
            can_play: true,
            can_pause: true,
            can_seek: true,
        }
    }

    /// Push the current snapshot to the MPRIS worker (deduped there).
    fn publish_mpris(&mut self) {
        let snap = self.mpris_snapshot();
        self.mpris.publish(snap);
    }

    /// Attach a shared [`media::MediaEngine`] (builder form). The widget then
    /// pulls frames + playhead from it every `sync`.
    pub fn with_engine(mut self, engine: media::MediaEngine) -> Self {
        self.set_engine(engine);
        self
    }

    /// Attach (or replace) the engine after construction.
    pub fn set_engine(&mut self, engine: media::MediaEngine) {
        self.volume = engine.volume();
        self.engine = Some(engine);
        self.dirty = true;
    }

    /// Mutable access to the engine, so the host can `open` a source or drive
    /// collaborative transport (`apply_transport` / `transport_state`).
    pub fn engine_mut(&mut self) -> Option<&mut media::MediaEngine> {
        self.engine.as_mut()
    }

    /// Shared read access to the engine.
    pub fn engine(&self) -> Option<&media::MediaEngine> {
        self.engine.as_ref()
    }

    /// Open a [`media::Source`] on the attached engine (no-op without one).
    pub fn open(&mut self, source: media::Source) {
        if let Some(engine) = &mut self.engine {
            engine.open(source);
            self.last_frame_pts = None;
            self.dirty = true;
        }
    }

    /// Give this player a stable identifier for the e2e test harness.
    pub fn with_test_id(mut self, id: &'static str) -> Self {
        self.test_id = Some(id);
        self
    }

    /// Register a seek callback (push-mode consumers). Receives the seek target
    /// as a `Duration` from the start.
    pub fn on_seek<F: FnMut(Duration) + 'static>(mut self, f: F) -> Self {
        self.on_seek = Some(Box::new(f));
        self
    }

    /// Push H.264 NALU bytes to the in-process decoder (push mode). Returns
    /// `true` when a frame was decoded and `latest_frame` updated.
    pub fn feed_frame(&mut self, nalus: &[u8]) -> bool {
        if nalus.is_empty() {
            return false;
        }
        if self.decoder.is_none() {
            match openh264::decoder::Decoder::new() {
                Ok(d) => self.decoder = Some(d),
                Err(_) => return false,
            }
        }
        let decoder = self.decoder.as_mut().expect("decoder just initialised");
        let yuv = match decoder.decode(nalus) {
            Ok(Some(yuv)) => yuv,
            Ok(None) | Err(_) => return false,
        };
        use openh264::formats::YUVSource;
        let (width, height) = yuv.dimensions();
        let mut rgba = vec![0u8; width * height * 4];
        yuv.write_rgba8(&mut rgba);
        self.latest_frame = Some(rgba_image(width as u32, height as u32, Arc::new(rgba)));
        self.dirty = true;
        true
    }

    /// Current playback state.
    pub fn state(&self) -> PlaybackState {
        self.state
    }

    /// Borrow the most recently produced frame, if any.
    pub fn latest_frame(&self) -> Option<&PenikoImage> {
        self.latest_frame.as_ref()
    }

    /// Whether any media is actually loaded: a picture has been decoded, or the
    /// engine has a source open (a clip that is prerolling but hasn't produced a
    /// frame yet). This gates the transport bar — no scrubber/timeline is shown
    /// over an empty player, since a "0:00 / duration" bar with nothing loaded
    /// is a fabricated clip — and selects the empty-state CTA vs. the surface.
    pub fn has_media(&self) -> bool {
        self.latest_frame.is_some()
            || self.engine.as_ref().is_some_and(|e| e.source().is_some())
    }

    /// Configure the centered zero-state CTA shown while no media is loaded (a
    /// hero icon + title + subtitle + action pill, via the shared
    /// [`EmptyState`]). Replaces the bare placeholder ring. Action-pill clicks
    /// are drained with [`Self::take_empty_action`] so the host can open a file.
    pub fn set_empty_state(&mut self, empty: EmptyMedia) {
        self.empty = Some(empty);
        self.dirty = true;
    }

    /// Builder form of [`Self::set_empty_state`].
    pub fn with_empty_state(mut self, empty: EmptyMedia) -> Self {
        self.set_empty_state(empty);
        self
    }

    /// Drain the "empty-state action pill was clicked" signal (fires once per
    /// click). The host reacts by opening a file (e.g. through the portal).
    pub fn take_empty_action(&mut self) -> bool {
        std::mem::take(&mut self.empty_action_fired)
    }

    /// The resolved bounds of the empty-state action pill (ZERO until the
    /// zero-state has been drawn, or when media is loaded). Exposed so a host
    /// or test can hit-test the CTA without rendering to pixels.
    pub fn empty_action_rect(&self) -> Rect {
        self.empty_action_rect
    }

    /// Whether playback is currently running (engine-aware).
    pub fn is_playing(&self) -> bool {
        match &self.engine {
            Some(e) => e.is_playing(),
            None => self.state == PlaybackState::Playing,
        }
    }

    /// Start (or resume) playback.
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

    /// Pause playback.
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

    /// Toggle play/pause.
    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// The current playhead in milliseconds.
    pub fn position_ms(&self) -> u64 {
        self.position_ms
    }

    /// The total duration in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// Seek to an absolute playhead position in milliseconds (clamped).
    pub fn seek_to_ms(&mut self, ms: u64) {
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

    /// Set output volume 0..=1 (also unmutes).
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

    /// Toggle mute (engine volume drops to 0 while muted).
    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        if let Some(e) = &mut self.engine {
            e.set_volume(if self.muted { 0.0 } else { self.volume });
        }
        self.dirty = true;
    }

    /// Whether fullscreen is currently requested.
    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    /// Toggle fullscreen intent.
    pub fn toggle_fullscreen(&mut self) {
        self.fullscreen = !self.fullscreen;
        self.fullscreen_touched = true;
        self.dirty = true;
    }

    /// Choose which output the player fullscreens onto (default:
    /// [`crate::fullscreen::FullscreenTarget::Largest`], so immersive video
    /// lands on the big screen). Reflected through [`Widget::fullscreen_params`].
    pub fn set_fullscreen_target(&mut self, target: crate::fullscreen::FullscreenTarget) {
        self.fullscreen_target = target;
    }

    /// The output the player will fullscreen onto.
    pub fn fullscreen_target(&self) -> crate::fullscreen::FullscreenTarget {
        self.fullscreen_target
    }

    /// The effective output volume for the meter (0 while muted).
    fn effective_volume(&self) -> f32 {
        if self.muted { 0.0 } else { self.volume }
    }

    /// Force the controls overlay visible for a long while — used by the
    /// headless `--shots` path so a single captured frame shows the full
    /// controls bar (which otherwise auto-hides when no cursor is moving).
    pub fn reveal_controls(&mut self) {
        self.controls_visible_until = Some(Instant::now() + Duration::from_secs(3600));
        self.dirty = true;
    }

    fn controls_visible_now(&self) -> bool {
        match self.controls_visible_until {
            Some(deadline) => Instant::now() < deadline,
            None => false,
        }
    }

    /// Whether the auto-hiding controls overlay is currently on screen (the
    /// reveal timeout is live, or a settings menu is open). An app that floats
    /// its own affordance over the video (e.g. an Open button) reads this so its
    /// overlay hides in lockstep with the player's controls — keeping the video
    /// truly edge-to-edge during immersive playback.
    pub fn controls_visible(&self) -> bool {
        self.controls_visible_now() || self.open_menu.is_some()
    }

    fn show_controls(&mut self) {
        self.controls_visible_until = Some(Instant::now() + CONTROLS_AUTO_HIDE);
    }

    fn controls_bar_rect(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            (self.rect.y1 - CONTROLS_BAR_HEIGHT).max(self.rect.y0),
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn playhead_fraction(&self) -> f64 {
        if self.duration_ms == 0 {
            0.0
        } else {
            (self.position_ms as f64 / self.duration_ms as f64).clamp(0.0, 1.0)
        }
    }

    /// Pull the freshest engine state into the widget's render caches. Split
    /// out of `sync` so the borrow of `self.engine` is fully released before
    /// the fields it mirrors into are written.
    fn pump_engine(&mut self) {
        // Read the caches the clone-guard needs *before* borrowing the engine,
        // so the decision can be made inside the closure without a second
        // borrow of `self`.
        let last_pts = self.last_frame_pts;
        let have_frame = self.latest_frame.is_some();
        let snapshot = self.engine.as_mut().map(|engine| {
            engine.poll();
            // Only clone the (multi-megabyte) frame buffer when it is actually a
            // NEW frame. A paused-but-loaded video keeps handing back the same
            // frame every poll; cloning its RGBA on every sync (the scroll path
            // re-syncs each frame) was a per-frame memcpy of the whole picture —
            // pure waste, since the pts-guard below would then discard it. Peek
            // the pts first (cheap) and clone only on a real change.
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
            )
        });
        let Some((pos, dur, buffering, playing, frame)) = snapshot else {
            return;
        };
        if self.position_ms != pos {
            self.position_ms = pos;
            self.dirty = true;
        }
        if let Some(d) = dur {
            if d > 0 && self.duration_ms != d {
                self.duration_ms = d;
                self.dirty = true;
            }
        }
        if self.buffering != buffering {
            self.buffering = buffering;
            self.dirty = true;
        }
        let new_state = if playing { PlaybackState::Playing } else { PlaybackState::Paused };
        if self.state != new_state {
            self.state = new_state;
            self.dirty = true;
        }
        if let Some((w, h, rgba, pts)) = frame {
            if self.last_frame_pts != Some(pts) || self.latest_frame.is_none() {
                self.last_frame_pts = Some(pts);
                self.latest_frame = Some(rgba_image(w, h, rgba));
                self.dirty = true;
            }
        }
        // Keep the desktop media widget's playhead + status fresh. `publish`
        // dedups sub-second position jitter, so this is quiet on the bus.
        if self.mpris.is_active() {
            self.publish_mpris();
        }
    }

    /// Paint the frame + (conditionally) the controls overlay.
    fn paint(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        // 1) Letterbox background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );

        // 2) Latest frame, contain-fit — the empty-state CTA when nothing is
        //    loaded, or a brief placeholder while a source prerolls its picture.
        if let Some(img) = self.latest_frame.clone() {
            self.empty_action_rect = Rect::ZERO;
            let iw = img.width as f64;
            let ih = img.height as f64;
            let rw = self.rect.width();
            let rh = self.rect.height();
            if iw > 0.0 && ih > 0.0 && rw > 0.0 && rh > 0.0 {
                let scale = (rw / iw).min(rh / ih);
                let dw = iw * scale;
                let dh = ih * scale;
                let dx = self.rect.x0 + (rw - dw) * 0.5;
                let dy = self.rect.y0 + (rh - dh) * 0.5;
                let brush_transform =
                    Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
                let dest = Rect::new(dx, dy, dx + dw, dy + dh);
                let image_brush = vello::peniko::ImageBrush::new(img);
                scene.fill(Fill::NonZero, transform, &image_brush, Some(brush_transform), &dest);
            }
        } else if self.has_media() {
            // A source is opening but no frame has decoded yet — a brief preroll.
            self.empty_action_rect = Rect::ZERO;
            self.paint_empty_placeholder(scene, transform, theme);
        } else {
            // Nothing loaded: an inviting zero-state that offers to open a file.
            self.paint_empty(scene, transform, text_renderer, theme);
        }

        // 3) Controls overlay — only once real media is loaded (an empty player
        //    shows no transport bar / scrubber, which would be a fake clip), and
        //    within the auto-hide window or whenever a settings menu is open (so
        //    it can't vanish under the user's cursor).
        let show_controls =
            self.has_media() && (self.controls_visible_now() || self.open_menu.is_some());
        if !show_controls {
            self.play_button_rect = Rect::ZERO;
            self.scrub_track_rect = Rect::ZERO;
            self.volume_track_rect = Rect::ZERO;
            self.fullscreen_button_rect = Rect::ZERO;
            self.gear_button_rect = Rect::ZERO;
            self.setting_icon_rects.clear();
            self.menu_hit_rects.clear();
            return;
        }
        self.paint_controls(scene, transform, text_renderer, theme);
    }

    fn paint_controls(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let bar = self.controls_bar_rect();
        if bar.width() <= 0.0 || bar.height() <= 0.0 {
            return;
        }
        let colors = &theme.tokens.colors;
        let accent = colors.accent[ACCENT_PRIMARY_IDX];

        // Scrim so controls read over bright frames.
        scene.fill(Fill::NonZero, transform, Color::from_rgba8(0, 0, 0, 170), None, &bar);

        let pad = theme.tokens.spacing.s2 as f64;
        let mid_y = bar.y0 + bar.height() * 0.5;

        // --- Play / pause button (left) ---
        let icon = PLAY_BUTTON_SIZE.min(bar.height() - 8.0).max(0.0);
        let play_btn = Rect::new(bar.x0 + pad, mid_y - icon * 0.5, bar.x0 + pad + icon, mid_y + icon * 0.5);
        self.play_button_rect = play_btn;
        self.draw_play_glyph(scene, transform, play_btn, colors.text_primary);

        // --- Right cluster: [ setting icons ][ gear ][ fullscreen ] ---
        //
        // Priority-`>=1` settings become their own icon buttons here; higher
        // priority wins the limited space and any overflow (plus every
        // priority-0 setting) collapses into the settings gear. The whole
        // cluster is packed from the right so the fullscreen button stays pinned
        // to the corner.
        let fs_size = icon;
        let btn = icon;
        let gap = pad;

        // Time label measured up-front: it feeds the left reserve that bounds
        // how many setting icons can fit without crushing the scrubber.
        let time_text = format!(
            "{} / {}",
            format_ms(self.position_ms),
            format_ms(self.duration_ms)
        );
        let time_size = 12.0f32;
        let (tw, _th) = text_renderer.measure(&time_text, time_size, None);

        // Classify registered settings: priority icons (highest first), and the
        // priority-0 set that always lives under the gear.
        let mut prio: Vec<usize> =
            (0..self.settings.len()).filter(|&i| self.settings[i].priority >= 1).collect();
        prio.sort_by(|&a, &b| {
            self.settings[b].priority.cmp(&self.settings[a].priority).then(a.cmp(&b))
        });
        let zero: Vec<usize> =
            (0..self.settings.len()).filter(|&i| self.settings[i].priority == 0).collect();

        // Fullscreen pinned far right.
        let fs_btn = Rect::new(
            bar.x1 - pad - fs_size,
            mid_y - fs_size * 0.5,
            bar.x1 - pad,
            mid_y + fs_size * 0.5,
        );

        // How many icon slots fit between the fullscreen button and the reserved
        // play + scrub + time + volume region on the left.
        let left_reserve =
            play_btn.x1 + pad + MIN_SCRUB_WIDTH + pad + tw as f64 + pad + VOLUME_WIDTH + pad;
        let slot = btn + gap;
        let avail = (fs_btn.x0 - gap - left_reserve).max(0.0);
        let max_fit = if slot > 0.0 { (avail / slot).floor() as usize } else { 0 };
        let (gear_needed, shown) = if zero.is_empty() && prio.len() <= max_fit {
            (false, prio.len())
        } else {
            (true, prio.len().min(max_fit.saturating_sub(1)))
        };

        self.fullscreen_button_rect = fs_btn;
        draw_fullscreen_glyph(scene, transform, fs_btn, colors.text_primary, self.fullscreen);

        // Gear (right of the icons, left of fullscreen) when anything hides in it.
        let mut cx = fs_btn.x0 - gap;
        let gear_btn = if gear_needed {
            let r = Rect::new(cx - btn, mid_y - btn * 0.5, cx, mid_y + btn * 0.5);
            cx = r.x0 - gap;
            let tint = if self.open_menu == Some(OpenMenu::Gear) { accent } else { colors.text_primary };
            Icon::Settings2.draw(scene, transform, inset_rect(r, 4.0), tint);
            r
        } else {
            Rect::ZERO
        };
        self.gear_button_rect = gear_btn;

        // Priority icon buttons, packed leftward from the gear.
        self.setting_icon_rects.clear();
        for &idx in prio.iter().take(shown) {
            let r = Rect::new(cx - btn, mid_y - btn * 0.5, cx, mid_y + btn * 0.5);
            cx = r.x0 - gap;
            let s = &self.settings[idx];
            let on = matches!(s.control, PlayerSettingControl::Toggle { on: true });
            let menu_open = self.open_menu == Some(OpenMenu::Choice(idx));
            let tint = if on || menu_open { accent } else { colors.text_primary };
            s.icon.draw(scene, transform, inset_rect(r, 4.0), tint);
            self.setting_icon_rects.push((idx, r));
        }

        // Record which settings ended up under the gear (overflow first, then
        // the always-tucked priority-0 set) for the menu + hit-testing.
        self.gear_entries.clear();
        self.gear_entries.extend(prio.iter().skip(shown).copied());
        self.gear_entries.extend(zero.iter().copied());

        // Leftmost edge the right cluster reached — the volume sits to its left.
        let cluster_x0 = self
            .setting_icon_rects
            .last()
            .map(|(_, r)| r.x0)
            .unwrap_or(if gear_needed { gear_btn.x0 } else { fs_btn.x0 });

        // --- Volume control (left of the right cluster) ---
        let vol_x1 = cluster_x0 - pad;
        let vol_x0 = (vol_x1 - VOLUME_WIDTH).max(play_btn.x1);
        let speaker = Rect::new(vol_x0, mid_y - icon * 0.4, vol_x0 + icon * 0.8, mid_y + icon * 0.4);
        draw_speaker_glyph(scene, transform, speaker, colors.text_primary, self.muted);
        let vtrack_x0 = speaker.x1 + pad * 0.5;
        let vtrack = Rect::new(
            vtrack_x0,
            mid_y - SCRUB_TRACK_HEIGHT * 0.5,
            vol_x1,
            mid_y + SCRUB_TRACK_HEIGHT * 0.5,
        );
        if vtrack.width() > 0.0 {
            self.volume_track_rect = vtrack;
            let shape = RoundedRect::from_rect(vtrack, SCRUB_TRACK_HEIGHT * 0.5);
            scene.fill(Fill::NonZero, transform, colors.border_standard, None, &shape);
            let filled = Rect::new(
                vtrack.x0,
                vtrack.y0,
                vtrack.x0 + vtrack.width() * self.effective_volume() as f64,
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

        // --- Time label + scrub track (centre) ---
        let time_x1 = vol_x0 - pad;
        let time_x0 = (time_x1 - tw as f64).max(play_btn.x1 + pad);
        text_renderer.draw_text(
            scene,
            &time_text,
            time_size,
            colors.text_dim,
            transform * Affine::translate((time_x0, mid_y - time_size as f64 * 0.5)),
            None,
        );

        let track_x0 = play_btn.x1 + pad;
        let track_x1 = time_x0 - pad;
        if track_x1 > track_x0 {
            let track = Rect::new(
                track_x0,
                mid_y - SCRUB_TRACK_HEIGHT * 0.5,
                track_x1,
                mid_y + SCRUB_TRACK_HEIGHT * 0.5,
            );
            self.scrub_track_rect = track;
            // Base track.
            scene.fill(
                Fill::NonZero,
                transform,
                colors.border_standard,
                None,
                &RoundedRect::from_rect(track, SCRUB_TRACK_HEIGHT * 0.5),
            );
            // Buffered range (lighter, up to buffering %).
            let buf_frac = (self.buffering as f64 / 100.0).clamp(0.0, 1.0);
            if buf_frac > 0.0 {
                let buffered = Rect::new(track.x0, track.y0, track.x0 + track.width() * buf_frac, track.y1);
                scene.fill(
                    Fill::NonZero,
                    transform,
                    colors.text_dim,
                    None,
                    &RoundedRect::from_rect(buffered, SCRUB_TRACK_HEIGHT * 0.5),
                );
            }
            // Played range (accent).
            let frac = self.playhead_fraction();
            let played = Rect::new(track.x0, track.y0, track.x0 + track.width() * frac, track.y1);
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &RoundedRect::from_rect(played, SCRUB_TRACK_HEIGHT * 0.5),
            );
            // Community (SponsorBlock) marks, painted as slightly-taller bands in
            // their category colours over the whole track so the sponsor / intro
            // / outro spans are visible on the ONE timeline before playback.
            let mid_y = track.center().y;
            let band_h = SCRUB_TRACK_HEIGHT + 3.0;
            for (s, e, color) in &self.scrub_marks {
                let s = (*s).clamp(0.0, 1.0) as f64;
                let e = (*e).clamp(0.0, 1.0) as f64;
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
            // Thumb.
            let thumb_cx = track.x0 + track.width() * frac;
            scene.fill(
                Fill::NonZero,
                transform,
                accent,
                None,
                &Circle::new((thumb_cx, mid_y), SCRUB_THUMB_RADIUS),
            );
        } else {
            self.scrub_track_rect = Rect::ZERO;
        }

        // Outline so the bar reads as a distinct surface.
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &bar);

        // Settings menu (gear list or a choice popover) floats above the bar.
        self.paint_menu(scene, transform, text_renderer, theme);
    }

    /// Paint the open settings overlay — the gear menu (every tucked setting) or
    /// a single choice's option list — as a themed rounded panel above its
    /// anchor button. Records `menu_hit_rects` for `handle_event`.
    fn paint_menu(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        self.menu_hit_rects.clear();
        let Some(menu) = self.open_menu else { return };
        let colors = &theme.tokens.colors;
        let accent = colors.accent[ACCENT_PRIMARY_IDX];

        // Build the flat row list: each entry is either a top-level setting row
        // (with, for an expanded choice, its options indented beneath) or, for a
        // choice popover, just that choice's option rows.
        enum Row {
            Setting(usize),
            Option(usize, usize),
        }
        let mut rows: Vec<Row> = Vec::new();
        let (anchor, title): (Rect, Option<String>) = match menu {
            OpenMenu::Gear => {
                for &idx in &self.gear_entries {
                    rows.push(Row::Setting(idx));
                    // A choice expands inline (its options appear indented
                    // beneath it) only while it is the expanded entry, so the
                    // gear reads as a tidy list until the user drills in.
                    if self.gear_expanded == Some(idx) {
                        if let PlayerSettingControl::Choice { options, .. } = &self.settings[idx].control {
                            for oi in 0..options.len() {
                                rows.push(Row::Option(idx, oi));
                            }
                        }
                    }
                }
                (self.gear_button_rect, None)
            }
            OpenMenu::Choice(idx) => {
                if let Some(s) = self.settings.get(idx) {
                    if let PlayerSettingControl::Choice { options, .. } = &s.control {
                        for oi in 0..options.len() {
                            rows.push(Row::Option(idx, oi));
                        }
                    }
                    let anchor = self
                        .setting_icon_rects
                        .iter()
                        .find(|(i, _)| *i == idx)
                        .map(|(_, r)| *r)
                        .unwrap_or(self.gear_button_rect);
                    (anchor, Some(s.name.clone()))
                } else {
                    return;
                }
            }
        };
        if rows.is_empty() && title.is_none() {
            return;
        }

        let row_h = MENU_ROW_H;
        let pad = theme.tokens.spacing.s2 as f64;
        let title_h = if title.is_some() { row_h } else { 0.0 };
        let panel_h = title_h + rows.len() as f64 * row_h + pad;
        let width = MENU_WIDTH.min(self.rect.width() - 8.0).max(80.0);
        // Right-align the panel to the anchor, clamped inside the surface.
        let x1 = anchor.x1.min(self.rect.x1 - 4.0);
        let x0 = (x1 - width).max(self.rect.x0 + 4.0);
        let y1 = (anchor.y0 - pad * 0.5).max(self.rect.y0 + panel_h + 4.0);
        let y0 = y1 - panel_h;
        let panel = Rect::new(x0, y0, x1, y1);

        // Panel background + border, themed like a raised surface so it reads
        // as a coherent menu in both light and dark themes.
        let bg = RoundedRect::from_rect(panel, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, transform, colors.surfaces.raised, None, &bg);
        scene.stroke(&Stroke::new(1.0), transform, colors.border_standard, None, &bg);

        let mut y = y0 + pad * 0.5;
        if let Some(t) = &title {
            text_renderer.draw_text(
                scene,
                t,
                12.0,
                colors.text_dim,
                transform * Affine::translate((x0 + pad, y + row_h * 0.5 - 7.0)),
                None,
            );
            y += row_h;
        }

        for row in &rows {
            let row_rect = Rect::new(x0 + 3.0, y, x1 - 3.0, y + row_h - 2.0);
            match row {
                Row::Setting(idx) => {
                    let s = &self.settings[*idx];
                    // Icon.
                    let icon_r = Rect::new(row_rect.x0 + 4.0, y + 5.0, row_rect.x0 + 22.0, y + row_h - 7.0);
                    s.icon.draw(scene, transform, icon_r, colors.text_primary);
                    // Label.
                    text_renderer.draw_text(
                        scene,
                        &s.name,
                        13.0,
                        colors.text_primary,
                        transform * Affine::translate((icon_r.x1 + 8.0, y + row_h * 0.5 - 8.0)),
                        None,
                    );
                    // Right-aligned value affordance.
                    let value: String = match &s.control {
                        PlayerSettingControl::Toggle { on } => {
                            if *on { "On".into() } else { "Off".into() }
                        }
                        PlayerSettingControl::Choice { options, selected } => {
                            options.get(*selected).cloned().unwrap_or_default()
                        }
                        PlayerSettingControl::Button => String::new(),
                    };
                    if !value.is_empty() {
                        let (vw, _) = text_renderer.measure(&value, 12.0, None);
                        let tint = matches!(s.control, PlayerSettingControl::Toggle { on: true })
                            .then_some(accent)
                            .unwrap_or(colors.text_dim);
                        text_renderer.draw_text(
                            scene,
                            &value,
                            12.0,
                            tint,
                            transform * Affine::translate((x1 - pad - vw as f64, y + row_h * 0.5 - 7.0)),
                            None,
                        );
                    }
                    self.menu_hit_rects.push((MenuHit::Setting(*idx), row_rect));
                }
                Row::Option(idx, oi) => {
                    if let PlayerSettingControl::Choice { options, selected } = &self.settings[*idx].control {
                        let selected = *selected == *oi;
                        // Indent options under their choice header when in the
                        // gear; the standalone popover has no header so no indent.
                        let indent = if matches!(menu, OpenMenu::Gear) { 26.0 } else { 10.0 };
                        if selected {
                            let hl = RoundedRect::from_rect(
                                Rect::new(row_rect.x0 + 2.0, y + 1.0, row_rect.x1 - 2.0, y + row_h - 3.0),
                                6.0,
                            );
                            scene.fill(Fill::NonZero, transform, accent.multiply_alpha(0.22), None, &hl);
                        }
                        let label = options.get(*oi).cloned().unwrap_or_default();
                        let tint = if selected { accent } else { colors.text_primary };
                        text_renderer.draw_text(
                            scene,
                            &label,
                            13.0,
                            tint,
                            transform * Affine::translate((x0 + indent, y + row_h * 0.5 - 8.0)),
                            None,
                        );
                        if selected {
                            let (cw, _) = text_renderer.measure("✓", 13.0, None);
                            text_renderer.draw_text(
                                scene,
                                "✓",
                                13.0,
                                accent,
                                transform * Affine::translate((x1 - pad - cw as f64, y + row_h * 0.5 - 8.0)),
                                None,
                            );
                        }
                        self.menu_hit_rects.push((MenuHit::Option(*idx, *oi), row_rect));
                    }
                }
            }
            y += row_h;
        }
    }

    /// Paint the zero-state while no media is loaded. With an [`EmptyMedia`]
    /// configured this is the shared [`EmptyState`] CTA (hero icon + title +
    /// subtitle + action pill), and the pill's hit rect is recorded so a click
    /// routes through [`Self::press_at_cursor`]. Without one it falls back to the
    /// bare placeholder ring (unchanged for consumers that configure nothing).
    fn paint_empty(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(empty) = self.empty.clone() else {
            self.empty_action_rect = Rect::ZERO;
            self.paint_empty_placeholder(scene, transform, theme);
            return;
        };
        self.empty_action_rect = EmptyState::paint(
            scene,
            transform,
            text_renderer,
            theme,
            self.rect,
            empty.icon,
            &empty.title,
            empty.message.as_deref(),
            empty.action.as_deref(),
            self.last_cursor,
        )
        .unwrap_or(Rect::ZERO);
    }

    /// Paint the empty-surface placeholder: a dim ringed play glyph centred in
    /// the letterbox, so a player with no media reads as intentionally empty
    /// ("nothing loaded — Open a video") rather than a blank/broken black rect.
    fn paint_empty_placeholder(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let dim = theme.tokens.colors.text_dim;
        let cx = self.rect.center().x;
        let cy = self.rect.center().y;
        let r = (self.rect.width().min(self.rect.height()) * 0.08).clamp(16.0, 44.0);
        if r <= 0.0 {
            return;
        }
        // Faint ring.
        scene.stroke(&Stroke::new(2.0), transform, dim, None, &Circle::new((cx, cy), r));
        // Play triangle inside it, nudged right so it reads centred.
        let s = r * 0.62;
        let mut p = BezPath::new();
        p.move_to((cx - s * 0.35, cy - s * 0.62));
        p.line_to((cx + s * 0.7, cy));
        p.line_to((cx - s * 0.35, cy + s * 0.62));
        p.close_path();
        scene.fill(Fill::NonZero, transform, dim, None, &p);
    }

    fn draw_play_glyph(&self, scene: &mut Scene, transform: Affine, r: Rect, color: Color) {
        let cx = r.center().x;
        let cy = r.center().y;
        let s = r.height().min(r.width()) * 0.5;
        if self.is_playing() {
            // Two vertical bars.
            let bw = s * 0.34;
            let gap = s * 0.30;
            let bar1 = Rect::new(cx - gap - bw, cy - s * 0.6, cx - gap, cy + s * 0.6);
            let bar2 = Rect::new(cx + gap, cy - s * 0.6, cx + gap + bw, cy + s * 0.6);
            scene.fill(Fill::NonZero, transform, color, None, &bar1);
            scene.fill(Fill::NonZero, transform, color, None, &bar2);
        } else {
            // Right-pointing triangle.
            let mut p = BezPath::new();
            p.move_to((cx - s * 0.5, cy - s * 0.7));
            p.line_to((cx + s * 0.7, cy));
            p.line_to((cx - s * 0.5, cy + s * 0.7));
            p.close_path();
            scene.fill(Fill::NonZero, transform, color, None, &p);
        }
    }
}

/// Shrink `r` by `d` on every side (used to breathe a glyph inside its button).
fn inset_rect(r: Rect, d: f64) -> Rect {
    Rect::new(r.x0 + d, r.y0 + d, r.x1 - d, r.y1 - d)
}

/// Build a peniko RGBA image from tightly-packed bytes.
fn rgba_image(width: u32, height: u32, rgba: Arc<Vec<u8>>) -> PenikoImage {
    PenikoImage {
        data: Blob::new(rgba),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width,
        height,
    }
}

/// Vector speaker glyph; draws a mute cross when `muted`.
fn draw_speaker_glyph(scene: &mut Scene, transform: Affine, r: Rect, color: Color, muted: bool) {
    let s = r.height();
    let x0 = r.x0;
    let cy = r.center().y;
    // Body: a small rectangle + a triangular cone.
    let mut body = BezPath::new();
    body.move_to((x0, cy - s * 0.18));
    body.line_to((x0 + s * 0.22, cy - s * 0.18));
    body.line_to((x0 + s * 0.5, cy - s * 0.42));
    body.line_to((x0 + s * 0.5, cy + s * 0.42));
    body.line_to((x0 + s * 0.22, cy + s * 0.18));
    body.line_to((x0, cy + s * 0.18));
    body.close_path();
    scene.fill(Fill::NonZero, transform, color, None, &body);
    if muted {
        let x1 = x0 + s * 0.62;
        let x2 = x0 + s * 0.9;
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((x1, cy - s * 0.2), (x2, cy + s * 0.2)));
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((x1, cy + s * 0.2), (x2, cy - s * 0.2)));
    } else {
        // Two sound-wave arcs approximated by short strokes.
        let wx = x0 + s * 0.6;
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((wx, cy - s * 0.22), (wx + s * 0.12, cy)));
        scene.stroke(&Stroke::new(1.6), transform, color, None, &Line::new((wx + s * 0.12, cy), (wx, cy + s * 0.22)));
    }
}

/// Vector fullscreen glyph: four corner brackets (enter) or inward brackets
/// (exit). Kept vector so it never depends on a glyph being in the font.
fn draw_fullscreen_glyph(scene: &mut Scene, transform: Affine, r: Rect, color: Color, active: bool) {
    let stroke = Stroke::new(1.8);
    let m = r.height() * 0.28;
    let corners = [
        (r.x0, r.y0, 1.0, 1.0),
        (r.x1, r.y0, -1.0, 1.0),
        (r.x0, r.y1, 1.0, -1.0),
        (r.x1, r.y1, -1.0, -1.0),
    ];
    for (x, y, sx, sy) in corners {
        // When active (currently fullscreen) draw inward-pointing brackets.
        let (ax, ay) = if active { (x + sx * m, y + sy * m) } else { (x, y) };
        let dir = if active { -1.0 } else { 1.0 };
        scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Line::new((ax, ay), (ax + sx * m * dir, ay)),
        );
        scene.stroke(
            &stroke,
            transform,
            color,
            None,
            &Line::new((ax, ay), (ax, ay + sy * m * dir)),
        );
    }
}

/// Format milliseconds as `M:SS` (or `H:MM:SS` past an hour).
fn format_ms(ms: u64) -> String {
    let total = ms / 1000;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

impl Default for VideoPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for VideoPlayer {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {
        self.pump_engine();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::auto(), height: Dimension::auto() },
                flex_grow: 1.0,
                flex_shrink: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let logical = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = logical;
                if self.scrubbing {
                    self.seek_from_cursor();
                    self.show_controls();
                    self.dirty = true;
                    *handled = true;
                } else if self.rect.contains(logical) && self.has_media() {
                    // Only wake the auto-hiding controls when there is media to
                    // control; an empty player stays at rest (no idle repaint).
                    self.show_controls();
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if !self.rect.contains(self.last_cursor) {
                    return;
                }
                // Clicking the surface focuses it for keyboard shortcuts.
                *focus_target = Some(node_id);
                // Route the press through the controls + settings-menu hit map.
                // Control hit regions are ZERO while the controls are hidden, so
                // these checks naturally fall through to the surface play/pause
                // toggle when nothing is on screen.
                if self.press_at_cursor() {
                    *handled = true;
                }
                if self.has_media() {
                    self.show_controls();
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.scrubbing {
                    self.scrubbing = false;
                    *handled = true;
                }
            }
            WindowEvent::KeyboardInput { event: key_event, .. }
                if self.focused && key_event.state == ElementState::Pressed =>
            {
                match &key_event.logical_key {
                    Key::Named(NamedKey::Space) => {
                        self.toggle_play();
                        self.show_controls();
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        let step = KEY_SEEK_STEP.as_millis() as u64;
                        self.seek_to_ms(self.position_ms.saturating_sub(step));
                        self.show_controls();
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowRight) => {
                        let step = KEY_SEEK_STEP.as_millis() as u64;
                        self.seek_to_ms(self.position_ms.saturating_add(step));
                        self.show_controls();
                        *handled = true;
                    }
                    Key::Character(c) if c.eq_ignore_ascii_case("f") => {
                        self.toggle_fullscreen();
                        *handled = true;
                    }
                    Key::Character(c) if c.eq_ignore_ascii_case("m") => {
                        self.toggle_mute();
                        self.show_controls();
                        *handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "VideoPlayer"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        // The empty-state action pill is clickable while nothing is loaded.
        if !self.has_media()
            && self.empty.is_some()
            && self.empty_action_rect.width() > 0.0
            && self.empty_action_rect.contains(point)
        {
            return Some(winit::window::CursorIcon::Pointer);
        }
        let on_menu_row = self.menu_hit_rects.iter().any(|(_, r)| r.contains(point));
        let on_setting_icon = self.setting_icon_rects.iter().any(|(_, r)| r.contains(point));
        if on_menu_row
            || ((self.controls_visible_now() || self.open_menu.is_some())
                && (self.play_button_rect.contains(point)
                    || self.fullscreen_button_rect.contains(point)
                    || self.scrub_track_rect.contains(point)
                    || self.volume_track_rect.contains(point)
                    || (self.gear_button_rect.width() > 0.0 && self.gear_button_rect.contains(point))
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
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn wants_pointer_capture(&self) -> bool {
        // While mid-drag on the scrub track, own the pointer so the seek keeps
        // tracking the cursor even after it leaves the thin track band.
        self.scrubbing
    }

    fn fullscreen_intent(&self) -> Option<bool> {
        // No opinion until the user first toggles fullscreen, so a freshly
        // launched player doesn't force the compositor's window state.
        if self.fullscreen_touched {
            Some(self.fullscreen)
        } else {
            None
        }
    }

    fn fullscreen_params(&self) -> crate::fullscreen::FullscreenRequest {
        // Immersive video mirrors the app window onto the chosen output (the
        // largest monitor by default — the big screen).
        crate::fullscreen::FullscreenRequest::mirror(self.fullscreen_target)
    }

    fn on_fullscreen_changed(&mut self, active: bool) {
        // The compositor granted or (e.g. via a global keybind) exited
        // fullscreen — reflect it so our button + intent stay truthful.
        if self.fullscreen != active {
            self.fullscreen = active;
            self.fullscreen_touched = true;
            self.dirty = true;
        }
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep pumping while playing (to pull frames + advance the scrubber) and
        // while the controls are fading so the auto-hide actually repaints.
        self.is_playing() || self.controls_visible_now()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    /// Accessibility: a media player surface. `Role::Group` named "Video
    /// player"; `busy` while playing.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Group)
                .with_name("Video player")
                .with_state(AccessState {
                    busy: self.is_playing(),
                    ..Default::default()
                }),
        )
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

impl VideoPlayer {
    /// Route a left-press at `self.last_cursor` (logical coords) through the
    /// settings menu, the setting icons + gear, and then the standard transport
    /// controls. Returns whether the press was consumed.
    fn press_at_cursor(&mut self) -> bool {
        let p = self.last_cursor;

        // 0) Empty state: the only actionable target is the zero-state action
        //    pill (e.g. "Open"). A press anywhere else must not fabricate
        //    playback on a player that has nothing loaded.
        if !self.has_media() {
            if self.empty.is_some() && self.empty_action_rect.contains(p) {
                self.empty_action_fired = true;
                self.dirty = true;
                return true;
            }
            return false;
        }

        // 1) An open menu owns the click. A row wins; a click anywhere off the
        //    menu (and off the buttons that could re-open it) dismisses it.
        if self.open_menu.is_some() {
            if let Some(hit) =
                self.menu_hit_rects.iter().find(|(_, r)| r.contains(p)).map(|(h, _)| *h)
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
            // Otherwise fall through to gear / icon handling below.
        }

        // 2) Settings gear toggles its menu.
        if self.gear_button_rect.width() > 0.0 && self.gear_button_rect.contains(p) {
            self.open_menu = if self.open_menu == Some(OpenMenu::Gear) { None } else { Some(OpenMenu::Gear) };
            self.gear_expanded = None;
            self.dirty = true;
            return true;
        }

        // 3) A priority setting's own icon button.
        if let Some((idx, _)) = self.setting_icon_rects.iter().find(|(_, r)| r.contains(p)).copied() {
            self.activate_setting_icon(idx);
            return true;
        }

        // 4) Standard transport controls.
        if self.play_button_rect.contains(p) {
            self.toggle_play();
            return true;
        }
        if self.fullscreen_button_rect.contains(p) {
            self.toggle_fullscreen();
            return true;
        }
        if self.volume_track_rect.width() > 0.0 && self.volume_track_rect.contains(p) {
            let frac = ((p.x - self.volume_track_rect.x0) / self.volume_track_rect.width()).clamp(0.0, 1.0);
            self.set_volume(frac as f32);
            return true;
        }
        if self.scrub_track_rect.width() > 0.0 && self.scrub_track_rect.contains(p) {
            self.scrubbing = true;
            self.seek_from_cursor();
            return true;
        }

        // 5) A plain click on the video surface toggles play/pause.
        self.toggle_play();
        true
    }

    /// Handle a click on a row inside the open settings menu.
    fn activate_menu_hit(&mut self, hit: MenuHit) {
        match hit {
            MenuHit::Setting(idx) => match &self.settings[idx].control {
                PlayerSettingControl::Toggle { .. } => self.toggle_setting(idx),
                PlayerSettingControl::Button => {
                    let id = self.settings[idx].id.clone();
                    self.setting_events.push(PlayerSettingEvent::Activated { id });
                    self.open_menu = None;
                    self.dirty = true;
                }
                PlayerSettingControl::Choice { .. } => {
                    // Expand / collapse this choice's options inline.
                    self.gear_expanded =
                        if self.gear_expanded == Some(idx) { None } else { Some(idx) };
                    self.dirty = true;
                }
            },
            MenuHit::Option(idx, oi) => self.choose_option(idx, oi),
        }
    }

    /// Handle a click on a priority setting's own bar icon.
    fn activate_setting_icon(&mut self, idx: usize) {
        match &self.settings[idx].control {
            PlayerSettingControl::Toggle { .. } => self.toggle_setting(idx),
            PlayerSettingControl::Button => {
                let id = self.settings[idx].id.clone();
                self.setting_events.push(PlayerSettingEvent::Activated { id });
                self.dirty = true;
            }
            PlayerSettingControl::Choice { .. } => {
                // Open (or close) this choice's option popover, anchored on the icon.
                self.open_menu =
                    if self.open_menu == Some(OpenMenu::Choice(idx)) { None } else { Some(OpenMenu::Choice(idx)) };
                self.dirty = true;
            }
        }
    }

    /// Flip a toggle setting and record the event.
    fn toggle_setting(&mut self, idx: usize) {
        let new_on = if let PlayerSettingControl::Toggle { on } = &mut self.settings[idx].control {
            *on = !*on;
            *on
        } else {
            return;
        };
        let id = self.settings[idx].id.clone();
        self.setting_events.push(PlayerSettingEvent::Toggled { id, on: new_on });
        self.dirty = true;
    }

    /// Select a choice option, record the event, and close the menu.
    fn choose_option(&mut self, idx: usize, oi: usize) {
        if let PlayerSettingControl::Choice { options, selected } = &mut self.settings[idx].control {
            if oi >= options.len() {
                return;
            }
            *selected = oi;
        } else {
            return;
        }
        let id = self.settings[idx].id.clone();
        self.setting_events.push(PlayerSettingEvent::Chose { id, index: oi });
        self.open_menu = None;
        self.gear_expanded = None;
        self.dirty = true;
    }

    /// Seek to the playhead the cursor's X maps to on the scrub track.
    fn seek_from_cursor(&mut self) {
        if self.scrub_track_rect.width() <= 0.0 {
            return;
        }
        let frac = ((self.last_cursor.x - self.scrub_track_rect.x0)
            / self.scrub_track_rect.width())
        .clamp(0.0, 1.0);
        let target = (frac * self.duration_ms as f64) as u64;
        self.seek_to_ms(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presence::PresenceCtx;
    use crate::text_render::TextRenderer;
    use crate::theme::Theme;

    fn press(key: Key) -> WindowEvent {
        // Same construction idiom as the block_editor tests: zero a KeyEvent
        // (its `platform_specific` field has no public constructor) then set the
        // fields we assert on.
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn dispatch(vp: &mut VideoPlayer, ev: &WindowEvent) -> bool {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = winit::event::Modifiers::default();
        let mut handled = false;
        let mut focus_target = None;
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = NodeId::new(0);
        vp.handle_event(ev, 1.0, &mods, &mut doc, &root, &mut handled, &mut focus_target, node, &taffy);
        handled
    }

    #[test]
    fn video_player_starts_paused() {
        let vp = VideoPlayer::new();
        assert_eq!(vp.state(), PlaybackState::Paused);
        assert!(!vp.is_playing());
    }

    #[test]
    fn engine_mode_sync_pulls_a_frame_and_metadata() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        vp.sync(&mut doc, &root);
        assert!(vp.latest_frame().is_some(), "engine frame pulled into the surface");
        assert!(vp.duration_ms() > 0, "duration mirrored from the engine");
    }

    #[test]
    fn space_toggles_play_on_focused_engine_player() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_focus(true);
        assert!(!vp.is_playing());
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::Space))));
        assert!(vp.is_playing(), "Space started playback via the engine");
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::Space))));
        assert!(!vp.is_playing(), "Space paused it again");
    }

    #[test]
    fn arrow_keys_seek_the_engine_playhead() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.set_focus(true);
        vp.seek_to_ms(20_000);
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::ArrowRight))));
        assert_eq!(vp.position_ms(), 25_000, "→ seeks forward 5s");
        assert!(dispatch(&mut vp, &press(Key::Named(NamedKey::ArrowLeft))));
        assert_eq!(vp.position_ms(), 20_000, "← seeks back 5s");
        // The seek propagated to the engine, not just the widget cache.
        assert_eq!(vp.engine().unwrap().position_ms(), 20_000);
    }

    #[test]
    fn scrub_click_seeks_to_the_track_fraction() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        // Reveal + lay out the controls by painting a frame.
        vp.show_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(vp.scrub_track_rect.width() > 0.0, "scrub track laid out");
        // Click at the horizontal midpoint of the track → ~50% of duration.
        let mid_x = vp.scrub_track_rect.x0 + vp.scrub_track_rect.width() * 0.5;
        let mid_y = vp.scrub_track_rect.center().y;
        dispatch(&mut vp, &cursor_moved(mid_x, mid_y));
        let handled = dispatch(&mut vp, &mouse_press());
        assert!(handled, "scrub press handled");
        let pos = vp.position_ms();
        assert!((45_000..=55_000).contains(&pos), "seeked to ~50%: {pos}ms");
    }

    #[test]
    fn clicking_the_video_surface_toggles_play_pause() {
        // A plain click on the surface (not on any control) must toggle
        // play/pause — the basic affordance, exercised through the real
        // handle_event path.
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.show_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        assert!(!vp.is_playing());
        // Click near the top-centre of the surface — clear of the bottom controls
        // bar, the scrub track and every button.
        dispatch(&mut vp, &cursor_moved(320.0, 40.0));
        assert!(dispatch(&mut vp, &mouse_press()), "surface click handled");
        assert!(vp.is_playing(), "surface click started playback");
        dispatch(&mut vp, &mouse_press());
        assert!(!vp.is_playing(), "a second surface click paused it");
    }

    #[test]
    fn sponsor_marks_emit_bands_on_the_single_timeline() {
        // Marks set via `set_sponsor_marks` must paint onto the one scrub track,
        // proving the SponsorBlock segments render inline on the shared timeline
        // (not a second bar).
        let mut vp = player_with_media();
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        vp.show_controls();
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

    #[test]
    fn dragging_the_scrub_track_seeks_continuously() {
        // Press on the track then drag: the playhead must follow the cursor the
        // whole way (drag-to-seek on the ONE timeline), through real events.
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.rect = Rect::new(0.0, 0.0, 640.0, 360.0);
        vp.duration_ms = 100_000;
        vp.show_controls();
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
        let track = vp.scrub_track_rect;
        assert!(track.width() > 0.0);
        let y = track.center().y;
        // Press near the start of the track.
        dispatch(&mut vp, &cursor_moved(track.x0 + track.width() * 0.1, y));
        assert!(dispatch(&mut vp, &mouse_press()), "press on track begins a scrub");
        assert!(vp.scrubbing, "press started a drag");
        assert!(vp.wants_pointer_capture(), "a scrub owns the pointer");
        let near = vp.position_ms();
        // Drag toward the far end — the playhead tracks the cursor.
        dispatch(&mut vp, &cursor_moved(track.x0 + track.width() * 0.9, y));
        let far = vp.position_ms();
        assert!(far > near, "dragging right advanced the playhead ({near} → {far})");
        assert!((85_000..=95_000).contains(&far), "drag to ~90%: {far}ms");
    }

    #[test]
    fn f_key_toggles_fullscreen_intent() {
        let mut vp = VideoPlayer::new();
        vp.set_focus(true);
        assert_eq!(Widget::fullscreen_intent(&vp), None, "no opinion until asked");
        assert!(dispatch(&mut vp, &press(Key::Character("f".into()))));
        assert_eq!(Widget::fullscreen_intent(&vp), Some(true));
        assert!(dispatch(&mut vp, &press(Key::Character("F".into()))));
        assert_eq!(Widget::fullscreen_intent(&vp), Some(false));
    }

    // --- Feature 1: shared fullscreen mechanism (player side) ---

    #[test]
    fn fullscreen_toggle_drives_a_largest_output_mirror_request() {
        use crate::fullscreen::{FullscreenTarget, FullscreenView};
        let mut vp = VideoPlayer::new();
        // No opinion, but the default params already name the largest output.
        assert_eq!(Widget::fullscreen_intent(&vp), None);
        assert_eq!(Widget::fullscreen_params(&vp).target, FullscreenTarget::Largest);
        // The button/key toggle is what the App reconcile consumes.
        vp.toggle_fullscreen();
        assert_eq!(Widget::fullscreen_intent(&vp), Some(true), "toggle asks the App to fullscreen");
        let params = Widget::fullscreen_params(&vp);
        assert_eq!(params.target, FullscreenTarget::Largest, "immersive video targets the big screen");
        assert_eq!(params.view, FullscreenView::Mirror, "video mirrors the window, not a distinct view");
        // The host can retarget (e.g. onto the primary panel).
        vp.set_fullscreen_target(FullscreenTarget::Primary);
        assert_eq!(Widget::fullscreen_params(&vp).target, FullscreenTarget::Primary);
    }

    // --- Feature 2: MPRIS now-playing published from the widget ---

    #[test]
    fn mpris_snapshot_reflects_metadata_and_controls_on_play() {
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
        assert_eq!(snap.status, crate::mpris::PlaybackStatus::Playing, "play → Playing on the bus");
        assert_eq!(snap.title, "Runaway");
        assert_eq!(snap.artist, "Aurora");
        assert_eq!(snap.art_url, "https://img/a.jpg");
        assert_eq!(snap.length_us, 240_000_000, "duration published as µs");
        assert!(snap.can_play && snap.can_pause && snap.can_seek, "transport controls advertised");
    }

    #[test]
    fn external_media_command_pause_reaches_the_player() {
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let mut vp = VideoPlayer::new().with_engine(engine);
        vp.play();
        assert!(vp.is_playing());
        // A media-key / system-widget Pause command lands as an MprisCommand and
        // drives the engine through the same path the on-screen button takes.
        vp.apply_media_command(crate::mpris::MprisCommand::Pause);
        assert!(!vp.is_playing(), "external Pause paused playback");
        vp.apply_media_command(crate::mpris::MprisCommand::PlayPause);
        assert!(vp.is_playing(), "external PlayPause resumed it");
    }

    // --- Feature 3: app-registered player settings ---

    fn draw_once(vp: &mut VideoPlayer) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        vp.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    /// A player with media on the surface, so the transport bar actually
    /// renders. The controls (scrubber, time, settings icons, gear) only appear
    /// once real media is loaded — an empty player shows the zero-state instead
    /// — so any test that lays out or clicks a control must put a frame up first.
    fn player_with_media() -> VideoPlayer {
        let mut vp = VideoPlayer::new();
        vp.latest_frame = Some(rgba_image(16, 16, Arc::new(vec![0u8; 4 * 16 * 16])));
        vp
    }

    #[test]
    fn mixed_priority_settings_surface_as_icons_and_under_the_gear() {
        let mut vp = player_with_media();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        // A high-priority toggle (own icon) + a priority-0 choice (under the gear).
        vp.set_setting(PlayerSetting::toggle("captions", Icon::FileText, "Captions", 2, false));
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
        assert!(!vp.setting_in_gear("captions"), "the promoted icon is not also in the gear");
    }

    #[test]
    fn higher_priority_wins_limited_space_and_overflow_falls_into_the_gear() {
        let mut vp = player_with_media();
        // Narrow bar + many priority settings ⇒ only the top few fit as icons.
        vp.rect = Rect::new(0.0, 0.0, 560.0, 315.0);
        vp.show_controls();
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
        assert!(!overflow_prios.is_empty(), "the rest overflow into the gear at this width");
        let min_icon = *icon_prios.iter().min().unwrap();
        let max_overflow = *overflow_prios.iter().max().unwrap();
        assert!(
            min_icon >= max_overflow,
            "every shown icon outranks every overflowed setting ({min_icon} >= {max_overflow})"
        );
    }

    #[test]
    fn resolution_setting_round_trips_through_the_player() {
        let mut vp = player_with_media();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        vp.set_setting(PlayerSetting::choice(
            "resolution",
            Icon::Monitor,
            "Resolution",
            2, // its own bar icon
            vec!["1080p".into(), "720p".into(), "480p".into()],
            0,
        ));
        draw_once(&mut vp);
        let icon = vp.setting_icon_rect("resolution").expect("resolution has a bar icon");
        // Click the icon → its option popover opens.
        dispatch(&mut vp, &cursor_moved(icon.center().x, icon.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "icon click handled");
        assert!(vp.menu_open(), "clicking the resolution icon opened its option list");
        // Lay the popover out, then click the "720p" option row.
        draw_once(&mut vp);
        let row = vp
            .menu_hit_rects
            .iter()
            .find(|(h, _)| matches!(h, MenuHit::Option(_, 1)))
            .map(|(_, r)| *r)
            .expect("720p option row laid out");
        dispatch(&mut vp, &cursor_moved(row.center().x, row.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "option click handled");
        // The selection round-trips: an event for the host + the widget's own state.
        let events = vp.take_setting_events();
        assert_eq!(
            events,
            vec![PlayerSettingEvent::Chose { id: "resolution".into(), index: 1 }],
            "picking 720p emits a Chose event the host drains"
        );
        match &vp.setting("resolution").unwrap().control {
            PlayerSettingControl::Choice { selected, .. } => assert_eq!(*selected, 1),
            _ => panic!("resolution is a choice"),
        }
        assert!(!vp.menu_open(), "picking an option closes the popover");
    }

    #[test]
    fn gear_toggle_setting_flips_and_emits() {
        let mut vp = player_with_media();
        vp.rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        vp.show_controls();
        // Priority 0 ⇒ under the gear.
        vp.set_setting(PlayerSetting::toggle("autoplay", Icon::Play, "Autoplay", 0, false));
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
            vec![PlayerSettingEvent::Toggled { id: "autoplay".into(), on: true }],
            "toggling the gear row flips it on and emits"
        );
    }

    #[test]
    fn player_embeds_flush_with_no_forced_internal_padding() {
        // The widget lays itself out as a grow leaf with no margin/padding, so
        // an embedding app can place it edge-to-edge. Prove the taffy style
        // carries zero padding + zero margin.
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

    #[test]
    fn accessibility_reports_group_role_and_busy_while_playing() {
        use crate::a11y::Role;
        let mut vp = VideoPlayer::new();
        let node = Widget::accessibility(&vp).expect("player is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name.as_deref(), Some("Video player"));
        assert!(!node.state.busy);
        vp.play();
        assert!(Widget::accessibility(&vp).unwrap().state.busy);
    }

    /// Push mode still works: a round-tripped openh264 frame lands in
    /// `latest_frame` without any engine attached.
    #[test]
    fn push_mode_feed_frame_updates_latest_image() {
        use openh264::encoder::Encoder;
        use openh264::formats::YUVBuffer;

        let w = 16usize;
        let h = 16usize;
        let total = w * h + 2 * ((w * h) / 4);
        let plane = vec![128u8; total];
        let yuv = YUVBuffer::from_vec(plane, w, h);
        let mut enc = Encoder::new().expect("openh264 encoder");
        let encoded = enc.encode(&yuv).expect("encode grey frame").to_vec();
        assert!(!encoded.is_empty());

        let mut vp = VideoPlayer::new();
        assert!(vp.feed_frame(&encoded), "decoder consumed the frame");
        assert!(vp.latest_frame().is_some());
    }

    #[test]
    fn empty_surface_paints_a_placeholder_and_no_image() {
        use vello_encoding::DrawTag;
        let mut vp = VideoPlayer::new();
        vp.rect = Rect::new(0.0, 0.0, 320.0, 240.0);
        // No frame loaded: the empty state must draw a placeholder, never an image.
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
        assert_eq!(image_count, 0, "no frame → no image draw in the empty state");
        // The letterbox fill + the ring + the play triangle are all painted.
        assert!(
            scene.encoding().draw_tags.len() >= 2,
            "empty state still paints a visible placeholder"
        );
    }

    #[test]
    fn paint_with_image_uses_image_draw_command() {
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
        assert!(image_count >= 1, "expected an IMAGE draw, got {image_count}");
    }

    #[test]
    fn has_media_is_false_until_a_source_or_frame() {
        assert!(!VideoPlayer::new().has_media(), "no engine, no frame ⇒ empty");
        let mut engine = media::MediaEngine::new_synthesized();
        engine.open(media::Source::Remote { url: "https://x/v.mp4".into() });
        let vp = VideoPlayer::new().with_engine(engine);
        assert!(vp.has_media(), "an opened source counts as media (it is prerolling)");
    }

    #[test]
    fn empty_player_shows_the_cta_and_no_transport_bar() {
        let mut vp = VideoPlayer::new().with_empty_state(EmptyMedia {
            icon: Some(Icon::Film),
            title: "Open a video".into(),
            message: Some("or drop a file here".into()),
            action: Some("Open".into()),
        });
        vp.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        assert!(!vp.has_media(), "a fresh player has nothing loaded");

        // Even with the controls explicitly revealed, an empty player paints no
        // transport bar — no fake 0:00 / duration scrubber over a blank surface.
        vp.reveal_controls();
        draw_once(&mut vp);
        assert_eq!(vp.scrub_track_rect, Rect::ZERO, "no scrubber over the empty state");
        assert_eq!(vp.play_button_rect, Rect::ZERO, "no transport bar over the empty state");

        // The zero-state resolved an "Open" hit target.
        let cta = vp.empty_action_rect();
        assert!(cta.width() > 0.0 && cta.height() > 0.0, "the Open CTA has a hit target");

        // Clicking it fires the drain the host reads to open a file.
        dispatch(&mut vp, &cursor_moved(cta.center().x, cta.center().y));
        assert!(dispatch(&mut vp, &mouse_press()), "the CTA click is consumed");
        assert!(vp.take_empty_action(), "the open-action signal fires");
        assert!(!vp.take_empty_action(), "and drains after one read");
    }

    #[test]
    fn a_press_off_the_cta_does_not_fabricate_playback() {
        // On an empty player a click anywhere but the CTA must do nothing — in
        // particular it must not start "playing" a clip that does not exist.
        let mut vp = VideoPlayer::new().with_empty_state(EmptyMedia {
            icon: Some(Icon::Film),
            title: "Open a video".into(),
            message: None,
            action: Some("Open".into()),
        });
        vp.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        draw_once(&mut vp);
        // Top-left corner is well clear of the centered CTA.
        dispatch(&mut vp, &cursor_moved(8.0, 8.0));
        assert!(!dispatch(&mut vp, &mouse_press()), "an off-CTA press is not consumed");
        assert!(!vp.take_empty_action(), "no open-action fired");
        assert!(!vp.is_playing(), "an empty player never starts playing");
    }

    #[test]
    fn transport_bar_appears_once_media_is_loaded() {
        let mut vp = player_with_media();
        vp.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        vp.show_controls();
        draw_once(&mut vp);
        assert!(vp.has_media(), "a frame on the surface counts as media");
        assert!(vp.scrub_track_rect.width() > 0.0, "the scrubber appears once media is loaded");
        assert!(vp.play_button_rect.width() > 0.0, "the transport bar appears once media is loaded");
        assert_eq!(vp.empty_action_rect(), Rect::ZERO, "no empty CTA while media is loaded");
    }
}
