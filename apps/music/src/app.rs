//! Root widget for the Music app.
//!
//! The whole UI is drawn directly into the Vello scene (nav rail, library
//! grids, track lists, detail pages, search and the now-playing transport
//! bar), so the app owns its layout math and hit-testing rather than
//! composing stock taffy widgets. Authoritative state lives in the Automerge
//! document via [`crate::doc`]; `sync` refreshes the in-memory render
//! snapshot each frame and every mutation writes back to the document so the
//! library + play-state replicate across devices.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};

use crate::art;
use crate::audio::AudioEngine;
use crate::doc;
use crate::icon::{self, Icon};
use crate::model::{fmt_duration, CacheState, Library, Playback, RepeatMode};
use crate::seed;
use crate::source::Source;

const SIDEBAR_W: f64 = 236.0;
const NOWPLAYING_H: f64 = 92.0;
const HEADER_H: f64 = 72.0;

/// Below this allocated width (logical px) the navigation sidebar stops
/// sitting permanently beside the content and becomes an overlay flap that
/// slides in over it behind a dimming scrim. Chosen so the sidebar plus a
/// readable two-column content area (album/track grids) stop fitting side by
/// side.
const SIDEBAR_BREAKPOINT: f64 = 760.0;

/// Which screen is showing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum View {
    Home,
    Songs,
    Albums,
    Artists,
    Playlists,
    Downloads,
    Search,
    Queue,
    Album(String),
    Artist(String),
    Playlist(String),
}

/// A click action captured during draw and dispatched on the next mouse
/// press. Owned data only, since the hit list is rebuilt every frame.
#[derive(Clone, Debug)]
enum Action {
    Nav(View),
    Back,
    /// Play `queue[index]` after replacing the queue.
    Play { queue: Vec<String>, index: usize },
    TogglePlay,
    Next,
    Prev,
    ToggleShuffle,
    CycleRepeat,
    SeekFrac(f64),
    VolumeFrac(f64),
    ToggleDownload(String),
    /// Append a track to the play queue. Wired through `dispatch`; reserved
    /// for the per-row "add to queue" affordance / context menu.
    #[allow(dead_code)]
    AddToQueue(String),
    FocusSearch,
    /// Toggle the adaptive navigation sidebar (hamburger).
    ToggleSidebar,
    /// Dismiss the overlay sidebar (scrim tap / Esc).
    CloseSidebar,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Scrubber,
    Volume,
}

struct Hit {
    rect: Rect,
    action: Action,
}

pub struct MusicApp {
    lib: Library,
    pb: Playback,
    view: View,
    nav_back: Vec<View>,
    search_query: String,
    search_focused: bool,
    source: Source,
    #[allow(dead_code)]
    audio: AudioEngine,

    seeded: bool,
    /// Whether the initial playback snapshot has been adopted from the
    /// document. After the first load, playback is authoritative in-memory
    /// (persisted out to the doc on each event) so the smoothly-advancing
    /// playhead isn't clobbered by re-reading the doc each frame.
    playback_loaded: bool,
    /// Track id the playhead is currently bound to (detects track changes
    /// across a doc reload so the live playhead isn't reset spuriously).
    bound_track: Option<String>,
    playhead_ms: f64,
    last_tick: Instant,

    rect: Rect,
    body_rect: Rect,
    scroll: f64,
    content_height: f64,

    /// Adaptive navigation sidebar: docked beside the content on a wide
    /// window, an overlay flap (with a dimming scrim, closed by default,
    /// summoned by the hamburger) on a narrow one. The eased reveal kernel is
    /// the shared [`ui::widget::FlapState`].
    flap: FlapState,
    /// Wall-clock of the last flap animation tick.
    flap_tick: Instant,
    /// `false` until the first frame has resolved the layout, so a cold start
    /// directly into a narrow window snaps the sidebar to its closed default
    /// instead of animating in from the wide (open) initial reveal.
    flap_primed: bool,
    /// Hit rect of the hamburger toggle, captured during draw so the event
    /// handler can route a press to [`FlapState::toggle`].
    hamburger_rect: Rect,

    mouse: Point,
    hits: Vec<Hit>,
    drag: Option<Drag>,
    /// Interactive bar rects in the now-playing transport, captured during
    /// draw so the event handler can map drags to fractions.
    scrubber_rect: Rect,
    volume_rect: Rect,
    dirty: bool,
    toast: Option<(String, Instant)>,
}

impl MusicApp {
    pub fn new() -> Self {
        // Optional initial-view override, used by `--shots` to capture
        // screens other than Home (e.g. `QOS_MUSIC_VIEW=album:al_glasslight`).
        let (view, query) = Self::parse_initial_view();
        Self {
            lib: Library::default(),
            pb: Playback::default(),
            view,
            nav_back: Vec::new(),
            search_query: query,
            search_focused: false,
            source: Source::default(),
            audio: AudioEngine::new(),
            seeded: false,
            playback_loaded: false,
            bound_track: None,
            playhead_ms: 0.0,
            last_tick: Instant::now(),
            rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            scroll: 0.0,
            content_height: 0.0,
            flap: FlapState::open_when_docked(),
            flap_tick: Instant::now(),
            flap_primed: false,
            hamburger_rect: Rect::ZERO,
            mouse: Point::ZERO,
            hits: Vec::new(),
            drag: None,
            scrubber_rect: Rect::ZERO,
            volume_rect: Rect::ZERO,
            dirty: true,
            toast: None,
        }
    }

    fn parse_initial_view() -> (View, String) {
        let raw = match std::env::var("QOS_MUSIC_VIEW") {
            Ok(v) => v,
            Err(_) => return (View::Home, String::new()),
        };
        let v = match raw.as_str() {
            "home" => View::Home,
            "songs" => View::Songs,
            "albums" => View::Albums,
            "artists" => View::Artists,
            "playlists" => View::Playlists,
            "downloads" => View::Downloads,
            "queue" => View::Queue,
            "search" => View::Search,
            other => {
                if let Some(id) = other.strip_prefix("album:") {
                    View::Album(id.to_string())
                } else if let Some(id) = other.strip_prefix("artist:") {
                    View::Artist(id.to_string())
                } else if let Some(id) = other.strip_prefix("playlist:") {
                    View::Playlist(id.to_string())
                } else if let Some(q) = other.strip_prefix("search:") {
                    return (View::Search, q.to_string());
                } else {
                    View::Home
                }
            }
        };
        (v, String::new())
    }

    // ---- state plumbing ---------------------------------------------------

    fn ensure_seeded(&mut self, state: &mut AutoCommit) {
        if self.seeded {
            return;
        }
        if !doc::is_seeded(state) {
            let lib = seed::sample_library();
            let pb = seed::sample_playback(&lib);
            doc::seed(state, &lib, &pb);
        }
        self.seeded = true;
    }

    /// Refresh just the catalog/playlists/cache from the document. Runs
    /// every `sync` so remote library edits (a playlist gaining a track on
    /// another device) appear without disturbing local playback.
    fn reload_library(&mut self, state: &AutoCommit) {
        self.lib = doc::load_library(state);
    }

    /// Adopt the full snapshot (library + playback) from the document. Used
    /// for the initial load and by tests.
    fn reload(&mut self, state: &AutoCommit) {
        self.lib = doc::load_library(state);
        self.pb = doc::load_playback(state);
        self.bound_track = self.pb.current.clone();
        self.playhead_ms = self.pb.position_ms as f64;
    }

    fn persist_playback(&mut self, state: &mut AutoCommit) {
        self.pb.position_ms = self.playhead_ms as u64;
        doc::write_playback(state, &self.pb);
        self.bound_track = self.pb.current.clone();
    }

    fn current_duration_ms(&self) -> u64 {
        self.pb
            .current
            .as_ref()
            .and_then(|id| self.lib.track(id))
            .map(|t| t.duration_ms())
            .unwrap_or(0)
    }

    /// Advance the playhead while playing and auto-advance at end of track.
    /// In-memory only (called from `draw`, which has no document handle); the
    /// updated position persists to the document on the next user event.
    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        if !self.pb.playing || self.pb.current.is_none() {
            return;
        }
        self.playhead_ms += dt * 1000.0;
        let dur = self.current_duration_ms();
        if dur > 0 && self.playhead_ms >= dur as f64 {
            self.advance(true);
        }
    }

    // ---- transport --------------------------------------------------------

    fn play_index(&mut self, index: usize) {
        if let Some(id) = self.pb.queue.get(index).cloned() {
            self.pb.queue_index = index;
            self.pb.current = Some(id);
            self.pb.playing = true;
            self.playhead_ms = 0.0;
            self.bound_track = self.pb.current.clone();
        }
    }

    fn advance(&mut self, auto: bool) {
        if self.pb.queue.is_empty() {
            return;
        }
        if self.pb.repeat == RepeatMode::One && auto {
            self.playhead_ms = 0.0;
            return;
        }
        let last = self.pb.queue.len() - 1;
        if self.pb.queue_index >= last {
            if self.pb.repeat == RepeatMode::All {
                self.play_index(0);
            } else {
                // Stop at the end of the queue.
                self.playhead_ms = self.current_duration_ms() as f64;
                self.pb.playing = false;
            }
        } else {
            let next = self.pb.queue_index + 1;
            self.play_index(next);
        }
    }

    fn previous(&mut self) {
        if self.playhead_ms > 3000.0 || self.pb.queue_index == 0 {
            self.playhead_ms = 0.0;
            return;
        }
        let prev = self.pb.queue_index - 1;
        self.play_index(prev);
    }

    fn dispatch(&mut self, action: Action, state: &mut AutoCommit) {
        self.dirty = true;
        match action {
            Action::Nav(v) => {
                if v != self.view {
                    self.nav_back.push(self.view.clone());
                    self.view = v;
                    self.scroll = 0.0;
                }
                if matches!(self.view, View::Search) {
                    self.search_focused = true;
                }
                // Picking a destination dismisses the overlay sidebar so the
                // chosen screen takes the full width (Adwaita flap behaviour).
                if self.sidebar_layout() == SplitLayout::Overlay {
                    self.flap.close();
                }
            }
            Action::Back => {
                if let Some(v) = self.nav_back.pop() {
                    self.view = v;
                    self.scroll = 0.0;
                }
            }
            Action::Play { queue, index } => {
                self.pb.queue = queue;
                self.play_index(index);
                if let Some(id) = &self.pb.current {
                    self.start_stream(id.clone());
                }
                self.persist_playback(state);
            }
            Action::TogglePlay => {
                self.pb.playing = !self.pb.playing;
                if self.pb.playing {
                    self.audio.play();
                } else {
                    self.audio.pause();
                }
                self.persist_playback(state);
            }
            Action::Next => {
                self.advance(false);
                if let Some(id) = self.pb.current.clone() {
                    self.start_stream(id);
                }
                self.persist_playback(state);
            }
            Action::Prev => {
                self.previous();
                if let Some(id) = self.pb.current.clone() {
                    self.start_stream(id);
                }
                self.persist_playback(state);
            }
            Action::ToggleShuffle => {
                self.pb.shuffle = !self.pb.shuffle;
                self.persist_playback(state);
            }
            Action::CycleRepeat => {
                self.pb.repeat = self.pb.repeat.next();
                self.persist_playback(state);
            }
            Action::SeekFrac(f) => {
                let dur = self.current_duration_ms();
                self.playhead_ms = (f.clamp(0.0, 1.0) * dur as f64).max(0.0);
                self.audio.seek_ms(self.playhead_ms as u64);
                self.persist_playback(state);
            }
            Action::VolumeFrac(f) => {
                self.pb.volume = (f.clamp(0.0, 1.0) * 100.0).round() as u8;
                self.audio.set_volume(self.pb.volume as f32 / 100.0);
                self.persist_playback(state);
            }
            Action::ToggleDownload(id) => {
                let new = match self.lib.cache_state(&id) {
                    CacheState::Cached => CacheState::Streamed,
                    _ => CacheState::Cached,
                };
                doc::set_cache(state, &id, new);
                self.lib = doc::load_library(state);
                let label = self.lib.track(&id).map(|t| t.title.clone()).unwrap_or_default();
                self.toast = Some((
                    match new {
                        CacheState::Cached => format!("Downloaded \u{201c}{label}\u{201d}"),
                        _ => format!("Removed download \u{201c}{label}\u{201d}"),
                    },
                    Instant::now(),
                ));
            }
            Action::AddToQueue(id) => {
                self.pb.queue.push(id.clone());
                let label = self.lib.track(&id).map(|t| t.title.clone()).unwrap_or_default();
                self.toast = Some((format!("Added \u{201c}{label}\u{201d} to queue"), Instant::now()));
                self.persist_playback(state);
            }
            Action::FocusSearch => {
                if self.view != View::Search {
                    self.nav_back.push(self.view.clone());
                }
                self.view = View::Search;
                self.search_focused = true;
            }
            Action::ToggleSidebar => {
                self.toggle_sidebar();
            }
            Action::CloseSidebar => {
                self.flap.close();
            }
        }
    }

    /// Resolve a track's stream URL (offline: stub) and hand it to the audio
    /// engine. Real audio only sounds with the `audio` + `streaming`
    /// features; offline this records intent and stays silent.
    fn start_stream(&mut self, track_id: String) {
        if let Some(t) = self.lib.track(&track_id) {
            match self.source.resolve_best(&t.yt_id) {
                Ok(stream) => {
                    self.audio.load(&stream.url);
                    if self.pb.playing {
                        self.audio.play();
                    }
                }
                Err(_offline) => {
                    // Offline / no streaming feature: keep the transport
                    // state moving via the local playhead; no audio output.
                }
            }
        }
    }

    fn push_hit(&mut self, rect: Rect, action: Action) -> bool {
        let hovered = rect.contains(self.mouse);
        self.hits.push(Hit { rect, action });
        hovered
    }

    // ---- adaptive sidebar geometry ---------------------------------------

    /// Resolve the sidebar layout (docked vs overlay) from the app's allocated
    /// width against the breakpoint.
    fn sidebar_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), SIDEBAR_BREAKPOINT)
    }

    /// The width the content area is inset by on the left for the sidebar.
    /// Docked: the revealed sidebar width (full width when open, 0 collapsed).
    /// Overlay: always 0 — the flap floats over the content.
    fn content_inset(&self) -> f64 {
        match self.sidebar_layout() {
            SplitLayout::Docked => SIDEBAR_W * self.flap.reveal(),
            SplitLayout::Overlay => 0.0,
        }
    }

    /// The sidebar's on-screen rect. Docked: pinned to the left, its width
    /// scaled by the reveal so collapsing slides the content over it. Overlay:
    /// slides in from the left edge, fully off-screen at reveal 0.
    fn sidebar_rect(&self) -> Rect {
        let r = self.rect;
        let bottom = r.y1 - NOWPLAYING_H;
        match self.sidebar_layout() {
            SplitLayout::Docked => {
                let w = SIDEBAR_W * self.flap.reveal();
                Rect::new(r.x0, r.y0, r.x0 + w, bottom)
            }
            SplitLayout::Overlay => {
                let slide = self.flap.slide();
                let x0 = r.x0 - SIDEBAR_W * (1.0 - slide);
                Rect::new(x0, r.y0, x0 + SIDEBAR_W, bottom)
            }
        }
    }

    /// Whether the hamburger toggle is currently shown — i.e. whenever the
    /// sidebar is not fully docked-open, so the user can summon it.
    fn show_hamburger(&self) -> bool {
        self.sidebar_layout() == SplitLayout::Overlay || self.flap.reveal() < 0.999
    }

    /// Flip the sidebar open/closed for the current layout.
    fn toggle_sidebar(&mut self) {
        self.flap.toggle(self.sidebar_layout());
        self.dirty = true;
    }

    /// Stage the sidebar for a deterministic `--shots` frame (no event loop to
    /// drive the reveal animation). `open` snaps it fully revealed.
    pub fn snap_sidebar(&mut self, open: bool) {
        self.flap.snap(open);
    }
}

impl Default for MusicApp {
    fn default() -> Self {
        Self::new()
    }
}

// ---- small drawing helpers ----------------------------------------------

fn rrect(r: Rect, radius: f64) -> RoundedRect {
    RoundedRect::from_rect(r, radius)
}

fn fill_rrect(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &rrect(r, radius));
}

fn draw_text(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    text: &str,
    x: f64,
    y: f64,
    size: f32,
    color: Color,
    weight: TextWeight,
    max_w: Option<f32>,
) {
    tr.draw_text_weighted(scene, text, size, color, t * Affine::translate((x, y)), max_w, weight);
}

/// Truncate `text` to fit `max_w` px at `size`, appending an ellipsis.
fn elide(tr: &mut TextRenderer, text: &str, size: f32, weight: TextWeight, max_w: f64) -> String {
    if tr.measure_weighted(text, size, None, weight).0 as f64 <= max_w {
        return text.to_string();
    }
    let mut s = String::new();
    for ch in text.chars() {
        let cand = format!("{s}{ch}\u{2026}");
        if tr.measure_weighted(&cand, size, None, weight).0 as f64 > max_w {
            break;
        }
        s.push(ch);
    }
    if s.is_empty() {
        "\u{2026}".to_string()
    } else {
        format!("{s}\u{2026}")
    }
}

mod render;

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_doc() -> (MusicApp, AutoCommit) {
        let mut app = MusicApp::new();
        let mut doc = AutoCommit::new();
        app.ensure_seeded(&mut doc);
        app.reload(&doc);
        (app, doc)
    }

    #[test]
    fn seeds_and_loads_library() {
        let (app, _doc) = app_with_doc();
        assert!(app.lib.total_tracks() >= 20);
        assert!(app.pb.current.is_some());
    }

    #[test]
    fn toggle_play_round_trips_through_doc() {
        let (mut app, mut doc) = app_with_doc();
        let was = app.pb.playing;
        app.dispatch(Action::TogglePlay, &mut doc);
        assert_eq!(app.pb.playing, !was);
        // Persisted to the document.
        assert_eq!(doc::load_playback(&doc).playing, !was);
    }

    #[test]
    fn play_action_sets_queue_and_current() {
        let (mut app, mut doc) = app_with_doc();
        let queue: Vec<String> = vec!["tk_g1".into(), "tk_g2".into(), "tk_g3".into()];
        app.dispatch(Action::Play { queue: queue.clone(), index: 1 }, &mut doc);
        assert_eq!(app.pb.current.as_deref(), Some("tk_g2"));
        assert_eq!(app.pb.queue, queue);
        assert!(app.pb.playing);
    }

    #[test]
    fn next_advances_and_wraps_with_repeat_all() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(
            Action::Play { queue: vec!["a".into(), "b".into()], index: 1 },
            &mut doc,
        );
        app.pb.repeat = RepeatMode::All;
        app.dispatch(Action::Next, &mut doc);
        assert_eq!(app.pb.queue_index, 0);
    }

    #[test]
    fn repeat_one_restarts_on_auto_advance() {
        let (mut app, _doc) = app_with_doc();
        app.pb.queue = vec!["tk_g1".into(), "tk_g2".into()];
        app.pb.queue_index = 0;
        app.pb.current = Some("tk_g1".into());
        app.pb.repeat = RepeatMode::One;
        app.playhead_ms = 5000.0;
        app.advance(true);
        assert_eq!(app.pb.queue_index, 0);
        assert_eq!(app.playhead_ms, 0.0);
    }

    #[test]
    fn seek_sets_playhead() {
        let (mut app, mut doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into()); // 215s
        app.dispatch(Action::SeekFrac(0.5), &mut doc);
        assert!((app.playhead_ms - 215_000.0 * 0.5).abs() < 1.0);
    }

    #[test]
    fn download_toggle_persists() {
        let (mut app, mut doc) = app_with_doc();
        // tk_g2 starts streamed.
        assert_eq!(app.lib.cache_state("tk_g2"), CacheState::Streamed);
        app.dispatch(Action::ToggleDownload("tk_g2".into()), &mut doc);
        assert_eq!(app.lib.cache_state("tk_g2"), CacheState::Cached);
        assert_eq!(doc::load_library(&doc).cache_state("tk_g2"), CacheState::Cached);
    }

    #[test]
    fn nav_and_back() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::Nav(View::Albums), &mut doc);
        assert_eq!(app.view, View::Albums);
        app.dispatch(Action::Nav(View::Album("al_glasslight".into())), &mut doc);
        assert_eq!(app.view, View::Album("al_glasslight".into()));
        app.dispatch(Action::Back, &mut doc);
        assert_eq!(app.view, View::Albums);
    }

    // ---- responsive sidebar -----------------------------------------------

    /// Set the app's allocated width directly (normally done by `apply_layout`).
    fn sized(app: &mut MusicApp, w: f64) {
        app.rect = Rect::new(0.0, 0.0, w, 800.0);
    }

    #[test]
    fn layout_docks_when_wide_overlays_when_narrow() {
        let mut app = MusicApp::new();
        sized(&mut app, 1280.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        sized(&mut app, 480.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
        // Boundary is inclusive at the breakpoint → docked.
        sized(&mut app, SIDEBAR_BREAKPOINT);
        assert_eq!(app.sidebar_layout(), SplitLayout::Docked);
        sized(&mut app, SIDEBAR_BREAKPOINT - 1.0);
        assert_eq!(app.sidebar_layout(), SplitLayout::Overlay);
    }

    #[test]
    fn sidebar_open_docked_closed_overlay_by_default() {
        let mut app = MusicApp::new();
        sized(&mut app, 1280.0);
        assert!(app.flap.is_open(app.sidebar_layout()), "wide: docked + open");
        assert!(app.content_inset() > 0.0, "wide content is inset by the sidebar");

        sized(&mut app, 480.0);
        assert!(!app.flap.is_open(app.sidebar_layout()), "narrow: overlay + closed");
        assert_eq!(app.content_inset(), 0.0, "narrow content is full-width");
    }

    #[test]
    fn hamburger_toggles_overlay_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0); // overlay
        assert!(!app.flap.is_open(SplitLayout::Overlay));
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(app.flap.is_open(SplitLayout::Overlay), "hamburger opens the flap");
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(!app.flap.is_open(SplitLayout::Overlay), "toggling again closes it");
    }

    #[test]
    fn navigating_dismisses_overlay_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0); // overlay
        app.flap.open();
        assert!(app.flap.is_open(SplitLayout::Overlay));
        // Selecting a destination from the open flap closes it.
        app.dispatch(Action::Nav(View::Albums), &mut doc);
        assert_eq!(app.view, View::Albums);
        assert!(!app.flap.is_open(SplitLayout::Overlay), "selection dismisses the flap");
    }

    #[test]
    fn navigating_keeps_docked_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0); // docked
        app.dispatch(Action::Nav(View::Albums), &mut doc);
        assert!(app.flap.is_open(SplitLayout::Docked), "docked sidebar persists across nav");
    }

    #[test]
    fn close_action_dismisses_sidebar() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 480.0);
        app.flap.open();
        app.dispatch(Action::CloseSidebar, &mut doc);
        assert!(!app.flap.is_open(SplitLayout::Overlay));
    }
}
