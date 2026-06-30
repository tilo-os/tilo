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
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
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
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{FlapState, SplitLayout, Widget};

use crate::art;
use crate::audio::AudioEngine;
use crate::doc;
use crate::icon::{self, Icon};
use crate::lyrics::{self, LyricsLine};
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
    Liked,
    Search,
    Queue,
    Lyrics,
    Album(String),
    Artist(String),
    Playlist(String),
}

/// A click action captured during draw and dispatched on the next mouse
/// press. Owned data only, since the hit list is rebuilt every frame.
#[derive(Clone, Debug, PartialEq)]
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
    /// Append a track to the end of the play queue.
    AddToQueue(String),
    /// Insert a track right after the current track ("Play next").
    PlayNext(String),
    /// Remove the queue entry at this absolute index.
    RemoveFromQueue(usize),
    /// Move the queue entry at `from` to `to` (reorder).
    MoveQueueItem { from: usize, to: usize },
    /// Toggle a track's "Liked Songs" membership.
    ToggleLike(String),
    /// Toggle an album's saved-to-library state.
    ToggleSaveAlbum(String),
    /// Append a track to a named playlist (from the context menu).
    AddToPlaylist { playlist: String, track: String },
    /// Remove a track from a playlist (playlist detail row affordance).
    RemoveFromPlaylist { playlist: String, track: String },
    /// Reorder a track within a playlist.
    MovePlaylistItem { playlist: String, from: usize, to: usize },
    /// Create a new (empty) playlist with the pending name and open it.
    CreatePlaylist,
    /// Begin editing a new-playlist name (opens the name input overlay).
    BeginNewPlaylist,
    /// Begin renaming the playlist with this id.
    BeginRenamePlaylist(String),
    /// Commit the pending rename of the named playlist.
    CommitRename(String),
    /// Delete a playlist by id.
    DeletePlaylist(String),
    /// Open the track context menu at the cursor for this track, captured in
    /// the named originating list (so "remove from playlist" can target it).
    OpenTrackMenu { track: String, in_playlist: Option<String>, queue_index: Option<usize> },
    /// Open the "Add to playlist…" picker for the menu's target track.
    OpenAddToPlaylist(String),
    /// Dismiss the inline name-input overlay without committing.
    CloseNamePrompt,
    FocusSearch,
    /// Toggle the adaptive navigation sidebar (hamburger / collapse chevron).
    ToggleSidebar,
    /// Dismiss the overlay sidebar (scrim tap / Esc).
    CloseSidebar,
    /// A no-op hit used to swallow clicks inside a modal surface so they don't
    /// fall through to the scrim behind it.
    None,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Scrubber,
    Volume,
}

#[derive(Clone)]
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
    /// Per-row right-click targets, captured during draw and matched on a
    /// right mouse press (kept separate from `hits` so a left-click plays the
    /// row while a right-click opens its context menu).
    row_menu_hits: Vec<Hit>,
    drag: Option<Drag>,
    /// Interactive bar rects in the now-playing transport, captured during
    /// draw so the event handler can map drags to fractions.
    scrubber_rect: Rect,
    volume_rect: Rect,
    dirty: bool,
    toast: Option<(String, Instant)>,

    /// Self-routing right-click context menu (the `ui` primitive, driven
    /// imperatively: opened at the cursor, drawn last, polled via a sink).
    context: ContextMenu,
    /// The action each open menu row maps to. Parallel to `context.items()`;
    /// drained when a selection lands so the host performs the CRDT mutation
    /// the `Arc<Fn>` callback model can't express.
    menu_actions: Vec<Action>,
    /// Shared sink the menu callbacks write the chosen row index into; the
    /// host drains it after `click_item` and dispatches into the normal
    /// `Action` pipeline. `-1` means "no pending selection". `Arc<AtomicI64>`
    /// (not `Rc<Cell>`) because `ContextMenu`'s `MenuAction` callback is
    /// `Send + Sync`.
    pending: Arc<AtomicI64>,

    /// The track the open context menu targets (for go-to-album/artist etc.).
    ctx_track: Option<String>,
    /// The playlist the context-menu target row belongs to, if it was opened
    /// from a playlist detail (so "Remove from playlist" can target it).
    ctx_in_playlist: Option<String>,
    /// The absolute queue index of the context-menu target, if opened from the
    /// queue view (so "Remove from queue" can target it).
    ctx_queue_index: Option<usize>,

    /// An inline name-input overlay (create / rename playlist). When `Some`,
    /// keystrokes edit `name_input`; Enter commits the captured intent.
    name_prompt: Option<NamePrompt>,
    name_input: String,

    /// Cached lyrics for the current track + the id they were fetched for, so
    /// the lyrics view doesn't re-resolve every frame.
    lyrics: Vec<LyricsLine>,
    lyrics_for: Option<String>,
    /// Whether the current `lyrics` are synced (time-stamped).
    lyrics_synced: bool,

    /// A deferred `--shots` staging action, applied once on the first `sync`
    /// after the library has loaded (the catalog is empty at construction).
    shot_stage: Option<ShotStage>,
}

/// A deferred `--shots` staging action (see [`MusicApp::app_root_for_shot`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShotStage {
    TrackMenu,
    NewPlaylistPrompt,
}

/// What the inline name-input overlay is collecting.
#[derive(Clone, Debug, PartialEq, Eq)]
enum NamePrompt {
    /// Creating a brand-new playlist.
    NewPlaylist,
    /// Renaming an existing playlist (by id).
    Rename(String),
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
            row_menu_hits: Vec::new(),
            drag: None,
            scrubber_rect: Rect::ZERO,
            volume_rect: Rect::ZERO,
            dirty: true,
            toast: None,
            context: ContextMenu::new(),
            menu_actions: Vec::new(),
            pending: Arc::new(AtomicI64::new(-1)),
            ctx_track: None,
            ctx_in_playlist: None,
            ctx_queue_index: None,
            name_prompt: None,
            name_input: String::new(),
            lyrics: Vec::new(),
            lyrics_for: None,
            lyrics_synced: false,
            shot_stage: None,
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
            "liked" => View::Liked,
            "queue" => View::Queue,
            "lyrics" => View::Lyrics,
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
        // When the live GStreamer backend is actually decoding a stream, drive
        // the playhead off the *real* pipeline position rather than simulating
        // it. Offline / no-audio builds fall back to the local clock so the
        // transport still advances for the UI.
        if self.audio.has_stream() {
            if let Some(pos) = self.audio.position_ms() {
                self.playhead_ms = pos as f64;
            } else {
                self.playhead_ms += dt * 1000.0;
            }
        } else {
            self.playhead_ms += dt * 1000.0;
        }
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

    /// Insert a track immediately after the current one ("Play next"). If
    /// nothing is playing this seeds and starts the queue.
    fn queue_play_next(&mut self, id: String) {
        if self.pb.queue.is_empty() || self.pb.current.is_none() {
            self.pb.queue = vec![id];
            self.play_index(0);
            return;
        }
        let at = (self.pb.queue_index + 1).min(self.pb.queue.len());
        self.pb.queue.insert(at, id);
    }

    /// Remove the queue entry at an absolute index, keeping `queue_index`
    /// pointed at the same logical track (or the next one if the current was
    /// removed).
    fn queue_remove(&mut self, index: usize) {
        if index >= self.pb.queue.len() {
            return;
        }
        self.pb.queue.remove(index);
        if index < self.pb.queue_index {
            self.pb.queue_index = self.pb.queue_index.saturating_sub(1);
        } else if index == self.pb.queue_index {
            // The current track was dropped: clamp and re-bind to whatever now
            // sits at this slot (or stop if the queue emptied).
            if self.pb.queue.is_empty() {
                self.pb.current = None;
                self.pb.playing = false;
                self.pb.queue_index = 0;
            } else {
                self.pb.queue_index = self.pb.queue_index.min(self.pb.queue.len() - 1);
                self.pb.current = self.pb.queue.get(self.pb.queue_index).cloned();
                self.playhead_ms = 0.0;
            }
        }
        self.bound_track = self.pb.current.clone();
    }

    /// Reorder a queue entry, keeping `queue_index` on the same track.
    fn queue_move(&mut self, from: usize, to: usize) {
        let n = self.pb.queue.len();
        if from >= n || to >= n || from == to {
            return;
        }
        let current_id = self.pb.queue.get(self.pb.queue_index).cloned();
        let item = self.pb.queue.remove(from);
        self.pb.queue.insert(to, item);
        if let Some(cur) = current_id {
            if let Some(pos) = self.pb.queue.iter().position(|t| *t == cur) {
                self.pb.queue_index = pos;
            }
        }
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
            Action::PlayNext(id) => {
                self.queue_play_next(id.clone());
                let label = self.lib.track(&id).map(|t| t.title.clone()).unwrap_or_default();
                self.toast = Some((format!("Playing \u{201c}{label}\u{201d} next"), Instant::now()));
                if self.pb.current.as_deref() == Some(id.as_str()) {
                    self.start_stream(id);
                }
                self.persist_playback(state);
            }
            Action::RemoveFromQueue(index) => {
                self.queue_remove(index);
                self.persist_playback(state);
            }
            Action::MoveQueueItem { from, to } => {
                self.queue_move(from, to);
                self.persist_playback(state);
            }
            Action::ToggleLike(id) => {
                let now = doc::toggle_liked(state, &id);
                self.reload_library(state);
                let label = self.lib.track(&id).map(|t| t.title.clone()).unwrap_or_default();
                self.toast = Some((
                    if now {
                        format!("Liked \u{201c}{label}\u{201d}")
                    } else {
                        format!("Removed \u{201c}{label}\u{201d} from Liked Songs")
                    },
                    Instant::now(),
                ));
            }
            Action::ToggleSaveAlbum(id) => {
                let now = doc::toggle_saved_album(state, &id);
                self.reload_library(state);
                let label = self.lib.album_title(&id).to_string();
                self.toast = Some((
                    if now {
                        format!("Added \u{201c}{label}\u{201d} to your library")
                    } else {
                        format!("Removed \u{201c}{label}\u{201d} from your library")
                    },
                    Instant::now(),
                ));
            }
            Action::AddToPlaylist { playlist, track } => {
                doc::add_track_to_playlist(state, &playlist, &track);
                self.reload_library(state);
                let pl = self.lib.playlist(&playlist).map(|p| p.name.clone()).unwrap_or_default();
                self.toast = Some((format!("Added to \u{201c}{pl}\u{201d}"), Instant::now()));
            }
            Action::RemoveFromPlaylist { playlist, track } => {
                doc::remove_track_from_playlist(state, &playlist, &track);
                self.reload_library(state);
            }
            Action::MovePlaylistItem { playlist, from, to } => {
                doc::move_track_in_playlist(state, &playlist, from, to);
                self.reload_library(state);
            }
            Action::BeginNewPlaylist => {
                self.name_prompt = Some(NamePrompt::NewPlaylist);
                self.name_input = String::new();
                self.search_focused = false;
            }
            Action::CreatePlaylist => {
                let name = if self.name_input.trim().is_empty() {
                    "New Playlist".to_string()
                } else {
                    self.name_input.trim().to_string()
                };
                let id = doc::create_playlist(state, &name);
                self.reload_library(state);
                self.name_prompt = None;
                self.name_input.clear();
                self.nav_back.push(self.view.clone());
                self.view = View::Playlist(id);
                self.scroll = 0.0;
                self.toast = Some((format!("Created \u{201c}{name}\u{201d}"), Instant::now()));
            }
            Action::BeginRenamePlaylist(id) => {
                self.name_input =
                    self.lib.playlist(&id).map(|p| p.name.clone()).unwrap_or_default();
                self.name_prompt = Some(NamePrompt::Rename(id));
                self.search_focused = false;
            }
            Action::CommitRename(id) => {
                let name = self.name_input.trim().to_string();
                if !name.is_empty() {
                    doc::rename_playlist(state, &id, &name);
                    self.reload_library(state);
                }
                self.name_prompt = None;
                self.name_input.clear();
            }
            Action::DeletePlaylist(id) => {
                doc::delete_playlist(state, &id);
                self.reload_library(state);
                // Leave the now-deleted detail page.
                if self.view == View::Playlist(id.clone()) {
                    self.view = View::Playlists;
                    self.scroll = 0.0;
                }
                self.toast = Some(("Playlist deleted".into(), Instant::now()));
            }
            Action::OpenTrackMenu { track, in_playlist, queue_index } => {
                self.open_track_menu(track, in_playlist, queue_index);
            }
            Action::OpenAddToPlaylist(track) => {
                self.open_add_to_playlist_menu(track);
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
            Action::CloseNamePrompt => {
                self.name_prompt = None;
                self.name_input.clear();
            }
            Action::None => {}
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

    /// Register a right-click context target for a track row. Matched only on
    /// a right mouse press (left-clicks still play the row).
    fn push_row_menu(&mut self, rect: Rect, action: Action) {
        self.row_menu_hits.push(Hit { rect, action });
    }

    /// Topmost right-click target containing `p` (later-drawn = on top).
    fn row_menu_at(&self, p: Point) -> Option<Action> {
        self.row_menu_hits.iter().rev().find(|h| h.rect.contains(p)).map(|h| h.action.clone())
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
    ///
    /// The sidebar spans the **full window height** (down to `r.y1`). The
    /// now-playing transport is inset to start to the right of the docked
    /// sidebar (see `nowplaying_rect`), so the two never overlap — the sidebar
    /// is not truncated above the bar, and the bar does not claim the sidebar's
    /// bottom-left corner.
    fn sidebar_rect(&self) -> Rect {
        let r = self.rect;
        let bottom = r.y1;
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

    /// The now-playing transport's on-screen rect. It runs along the bottom of
    /// the content column — starting to the right of the docked sidebar (so the
    /// full-height sidebar is never occluded) and spanning to the window's right
    /// edge. In overlay mode `content_inset()` is 0, so the bar is full-width
    /// and the floating sidebar draws above it.
    fn nowplaying_rect(&self) -> Rect {
        let r = self.rect;
        Rect::new(r.x0 + self.content_inset(), r.y1 - NOWPLAYING_H, r.x1, r.y1)
    }

    /// Whether the hamburger toggle is currently shown — i.e. whenever the
    /// sidebar is not fully docked-open, so the user can summon it. True when
    /// the sidebar is logically closed (so a freshly-collapsed rail offers the
    /// re-expand affordance before its slide animation has finished) OR while
    /// it's mid-collapse animation.
    fn show_hamburger(&self) -> bool {
        let layout = self.sidebar_layout();
        layout == SplitLayout::Overlay || !self.flap.is_open(layout) || self.flap.reveal() < 0.999
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

    /// Stage a track context menu open for a `--shots` frame. Deferred to the
    /// first `sync` (the catalog is empty at construction); see [`apply_shot_stage`].
    pub fn snap_track_menu(&mut self) {
        self.shot_stage = Some(ShotStage::TrackMenu);
        // Default the shot to the Songs library so the menu has rows behind it.
        if matches!(self.view, View::Home) {
            self.view = View::Songs;
        }
    }

    /// Stage the inline create-playlist name prompt open for a `--shots` frame.
    pub fn snap_new_playlist_prompt(&mut self) {
        self.shot_stage = Some(ShotStage::NewPlaylistPrompt);
        if matches!(self.view, View::Home) {
            self.view = View::Playlists;
        }
    }

    /// Stage the header search pill focused with an empty query for a `--shots`
    /// frame — captures the caret at the field start (it must not sit at the end
    /// of the placeholder hint). `search_focused` is plain widget state, so no
    /// deferral is needed.
    pub fn snap_search_focus(&mut self) {
        self.search_focused = true;
        self.search_query.clear();
    }

    /// Apply a deferred `--shots` staging action now that the library is
    /// loaded. Runs once; clears itself.
    fn apply_shot_stage(&mut self) {
        let Some(stage) = self.shot_stage.take() else { return };
        match stage {
            ShotStage::TrackMenu => {
                // Target the 3rd sorted track and anchor the cursor on its row
                // (rows start at HEADER_H + ~9, 52px tall) so the highlighted
                // row and the menu's target track line up in the frame.
                let tracks = self.lib.tracks_sorted();
                let track = tracks.get(2).or_else(|| tracks.first()).map(|t| t.id.clone()).unwrap_or_default();
                if !track.is_empty() {
                    let row_y = self.rect.y0 + HEADER_H + 2.0 * 52.0 + 26.0;
                    self.mouse = Point::new(self.rect.x0 + 360.0, row_y);
                    self.open_track_menu(track, None, None);
                }
            }
            ShotStage::NewPlaylistPrompt => {
                self.name_prompt = Some(NamePrompt::NewPlaylist);
                self.name_input = "Late Night Coding".to_string();
            }
        }
    }

    // ---- context menu -----------------------------------------------------

    /// Push an action row onto the open menu, recording its parallel `Action`.
    fn push_menu(&mut self, label: impl Into<String>, action: Action) {
        let idx = self.menu_actions.len() as i64;
        let pending = self.pending.clone();
        self.context
            .push(MenuItem::action(label, move || pending.store(idx, Ordering::SeqCst)));
        self.menu_actions.push(action);
    }

    /// Build + open the per-track context menu at the cursor.
    fn open_track_menu(
        &mut self,
        track: String,
        in_playlist: Option<String>,
        queue_index: Option<usize>,
    ) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = Some(track.clone());
        self.ctx_in_playlist = in_playlist.clone();
        self.ctx_queue_index = queue_index;

        let liked = self.lib.is_liked(&track);
        let album_id = self.lib.track(&track).map(|t| t.album_id.clone());
        let artist_id = self.lib.track(&track).map(|t| t.artist_id.clone());

        self.push_menu("Play", Action::Play { queue: vec![track.clone()], index: 0 });
        self.push_menu("Play next", Action::PlayNext(track.clone()));
        self.push_menu("Add to queue", Action::AddToQueue(track.clone()));
        self.context.push(MenuItem::separator());
        self.menu_actions.push(Action::Back); // separator placeholder, never fired
        self.push_menu("Add to playlist\u{2026}", Action::OpenAddToPlaylist(track.clone()));
        if let Some(pl) = &in_playlist {
            self.push_menu(
                "Remove from this playlist",
                Action::RemoveFromPlaylist { playlist: pl.clone(), track: track.clone() },
            );
        }
        if let Some(qi) = queue_index {
            self.push_menu("Remove from queue", Action::RemoveFromQueue(qi));
        }
        self.push_menu("Add to library", Action::ToggleLike(track.clone()));
        self.context.push(MenuItem::separator());
        self.menu_actions.push(Action::Back);
        if let Some(al) = album_id {
            self.push_menu("Go to album", Action::Nav(View::Album(al)));
        }
        if let Some(ar) = artist_id {
            self.push_menu("Go to artist", Action::Nav(View::Artist(ar)));
        }
        self.push_menu(
            if liked { "Unlike" } else { "Like" },
            Action::ToggleLike(track.clone()),
        );

        let p = self.clamp_menu_origin(self.mouse);
        self.context.open_at([p.x as f32, p.y as f32]);
        self.layout_context_menu();
        self.dirty = true;
    }

    /// Build + open the "Add to playlist…" secondary picker.
    fn open_add_to_playlist_menu(&mut self, track: String) {
        self.context = ContextMenu::new();
        self.menu_actions.clear();
        self.ctx_track = Some(track.clone());

        self.push_menu("New playlist\u{2026}", Action::BeginNewPlaylist);
        if !self.lib.playlists.is_empty() {
            self.context.push(MenuItem::separator());
            self.menu_actions.push(Action::Back);
        }
        let playlists: Vec<(String, String)> = self
            .lib
            .playlists
            .iter()
            .map(|p| (p.id.clone(), p.name.clone()))
            .collect();
        for (id, name) in playlists {
            self.push_menu(
                name,
                Action::AddToPlaylist { playlist: id, track: track.clone() },
            );
        }

        let p = self.clamp_menu_origin(self.mouse);
        self.context.open_at([p.x as f32, p.y as f32]);
        self.layout_context_menu();
        self.dirty = true;
    }

    /// Nudge a menu anchor so the menu stays within the app rect. The menu is
    /// `MENU_WIDTH` (220) wide; height grows with the item count.
    fn clamp_menu_origin(&self, p: Point) -> Point {
        let menu_w = 220.0;
        let rows = self.menu_actions.len() as f64;
        let menu_h = rows * 28.0 + 24.0;
        let x = p.x.min(self.rect.x1 - menu_w - 8.0).max(self.rect.x0 + 8.0);
        let y = p.y.min(self.rect.y1 - menu_h - 8.0).max(self.rect.y0 + 8.0);
        Point::new(x, y)
    }

    /// Run the context menu's own layout pass against a throwaway taffy tree
    /// so its `rect` (and row geometry) are populated for hit-testing + draw.
    fn layout_context_menu(&mut self) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let theme = Theme::dark();
        let node = self.context.build_layout(&mut taffy, &theme, &mut node_map);
        let _ = taffy.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(240.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        );
        self.context.apply_layout(&taffy, node, Point::ZERO);
    }

    /// Drain a context-menu selection (set by a row callback) and dispatch the
    /// parallel [`Action`]. Returns whether something was handled.
    fn drain_menu_selection(&mut self, state: &mut AutoCommit) -> bool {
        let idx = self.pending.swap(-1, Ordering::SeqCst);
        if idx >= 0 {
            if let Some(action) = self.menu_actions.get(idx as usize).cloned() {
                self.dispatch(action, state);
            }
            return true;
        }
        false
    }

    // ---- lyrics -----------------------------------------------------------

    /// Resolve lyrics for the current track when the bound track changes, via
    /// the synchronous bundled/gated resolver. Cached by track id so the
    /// lyrics view doesn't re-resolve every frame. Live fetch runs on the
    /// async worker (gated behind the `lyrics` feature); the bundled path is
    /// always available for `--shots` + offline.
    fn ensure_lyrics(&mut self) {
        let cur = self.pb.current.clone();
        if cur == self.lyrics_for {
            return;
        }
        self.lyrics_for = cur.clone();
        self.lyrics.clear();
        self.lyrics_synced = false;
        let Some(id) = cur else { return };
        let Some(track) = self.lib.track(&id).cloned() else { return };
        let meta = lyrics::TrackMeta {
            track_id: id.clone(),
            title: track.title.clone(),
            artist: self.lib.artist_name(&track.artist_id).to_string(),
            album: self.lib.album_title(&track.album_id).to_string(),
            duration_s: track.duration_s,
        };
        if let Some(resolved) = lyrics::resolve(&meta) {
            self.lyrics = resolved.lines;
            self.lyrics_synced = resolved.synced;
        }
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

    // ---- synthetic winit events (drive `handle_event` like the real loop) --
    use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

    fn synth_device() -> DeviceId {
        unsafe { std::mem::zeroed() }
    }
    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: synth_device(),
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press(button: MouseButton) -> WindowEvent {
        WindowEvent::MouseInput { device_id: synth_device(), state: ElementState::Pressed, button }
    }
    /// Drive one event through the production `handle_event` path.
    fn send(app: &mut MusicApp, ev: &WindowEvent, doc: &mut AutoCommit) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus = None;
        let root = automerge::ROOT;
        app.handle_event(ev, 1.0, &winit::event::Modifiers::default(), doc, &root, &mut handled, &mut focus, node, &taffy);
        handled
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

    // ---- 1. sidebar collapse ----------------------------------------------

    #[test]
    fn docked_collapse_toggles_and_reexpands() {
        let (mut app, mut doc) = app_with_doc();
        sized(&mut app, 1280.0); // docked, open by default
        assert!(app.flap.is_open(SplitLayout::Docked));
        // Collapse via the (docked) toggle.
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(!app.flap.is_open(SplitLayout::Docked), "docked sidebar collapses");
        // Once collapsed, the hamburger reappears so it can be re-expanded.
        assert!(app.show_hamburger(), "collapsed docked rail offers a re-expand affordance");
        app.dispatch(Action::ToggleSidebar, &mut doc);
        assert!(app.flap.is_open(SplitLayout::Docked), "toggling again re-expands it");
    }

    // ---- 2. context menu --------------------------------------------------

    /// Open a track's context menu, returning the action index for a label.
    fn menu_index(app: &MusicApp, label: &str) -> Option<usize> {
        app.context.items().iter().enumerate().find_map(|(i, it)| {
            if it.label() == Some(label) { Some(i) } else { None }
        })
    }

    #[test]
    fn open_track_menu_lists_expected_actions() {
        let (mut app, _doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.open_track_menu("tk_g1".into(), None, None);
        assert!(app.context.is_open());
        for label in ["Play", "Play next", "Add to queue", "Add to playlist\u{2026}", "Add to library", "Go to album", "Go to artist"] {
            assert!(menu_index(&app, label).is_some(), "menu has {label}");
        }
    }

    #[test]
    fn menu_add_to_queue_grows_the_queue() {
        let (mut app, mut doc) = app_with_doc();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let before = app.pb.queue.len();
        app.open_track_menu("tk_s2".into(), None, None);
        let idx = menu_index(&app, "Add to queue").unwrap() as i64;
        // Simulate the menu callback firing for that row.
        app.pending.store(idx, Ordering::SeqCst);
        app.drain_menu_selection(&mut doc);
        assert_eq!(app.pb.queue.len(), before + 1, "Add to queue grows the queue");
        assert_eq!(app.pb.queue.last().map(|s| s.as_str()), Some("tk_s2"));
    }

    #[test]
    fn right_click_opens_menu_then_selection_dispatches() {
        // The full production path: draw to register row-menu hits, right-click
        // to open, then a left-click on the "Play next" row dispatches it.
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        // Render one frame so row-menu hits + geometry exist.
        draw_once(&mut app, &mut doc);
        // Find a track row's right-click target.
        let target = app.row_menu_hits.first().cloned();
        let target = target.expect("a track row registered a right-click target");
        let p = target.rect.center();
        send(&mut app, &cursor_at(p), &mut doc);
        let handled = send(&mut app, &press(MouseButton::Right), &mut doc);
        assert!(handled, "right-click on a row is consumed");
        assert!(app.context.is_open(), "right-click opens the context menu");
    }

    // ---- 3. add album to library ------------------------------------------

    #[test]
    fn toggle_save_album_round_trips() {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_album_saved("al_strata"));
        app.dispatch(Action::ToggleSaveAlbum("al_strata".into()), &mut doc);
        assert!(app.lib.is_album_saved("al_strata"));
        assert!(doc::load_library(&doc).is_album_saved("al_strata"));
        app.dispatch(Action::ToggleSaveAlbum("al_strata".into()), &mut doc);
        assert!(!app.lib.is_album_saved("al_strata"));
    }

    // ---- 4. like toggle ---------------------------------------------------

    #[test]
    fn toggle_like_round_trips_and_persists() {
        let (mut app, mut doc) = app_with_doc();
        assert!(!app.lib.is_liked("tk_g2"));
        app.dispatch(Action::ToggleLike("tk_g2".into()), &mut doc);
        assert!(app.lib.is_liked("tk_g2"), "like is reflected in the snapshot");
        assert!(doc::load_library(&doc).is_liked("tk_g2"), "like persists to the doc");
        // Liked view surfaces it.
        assert!(app.lib.liked_ids().contains(&"tk_g2".to_string()));
        app.dispatch(Action::ToggleLike("tk_g2".into()), &mut doc);
        assert!(!app.lib.is_liked("tk_g2"));
    }

    // ---- 5. queue ops -----------------------------------------------------

    #[test]
    fn play_next_inserts_after_current() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::Play { queue: vec!["a".into(), "b".into(), "c".into()], index: 0 }, &mut doc);
        assert_eq!(app.pb.queue_index, 0);
        app.dispatch(Action::PlayNext("x".into()), &mut doc);
        assert_eq!(app.pb.queue, vec!["a", "x", "b", "c"], "play-next sits right after current");
    }

    #[test]
    fn remove_from_queue_drops_entry() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::Play { queue: vec!["a".into(), "b".into(), "c".into()], index: 1 }, &mut doc);
        // Remove the entry before current → index shifts down, current stays "b".
        app.dispatch(Action::RemoveFromQueue(0), &mut doc);
        assert_eq!(app.pb.queue, vec!["b", "c"]);
        assert_eq!(app.pb.current.as_deref(), Some("b"));
        assert_eq!(app.pb.queue_index, 0);
    }

    #[test]
    fn move_queue_item_reorders_and_keeps_current() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::Play { queue: vec!["a".into(), "b".into(), "c".into()], index: 0 }, &mut doc);
        app.dispatch(Action::MoveQueueItem { from: 2, to: 1 }, &mut doc);
        assert_eq!(app.pb.queue, vec!["a", "c", "b"]);
        assert_eq!(app.pb.current.as_deref(), Some("a"), "current track unchanged by a reorder below it");
    }

    // ---- 6. playlist management -------------------------------------------

    #[test]
    fn create_playlist_navigates_to_it() {
        let (mut app, mut doc) = app_with_doc();
        app.name_input = "Roadtrip".into();
        let before = app.lib.playlists.len();
        app.dispatch(Action::CreatePlaylist, &mut doc);
        assert_eq!(app.lib.playlists.len(), before + 1);
        assert!(matches!(app.view, View::Playlist(_)), "creating opens the new playlist");
        let created = app.lib.playlists.iter().any(|p| p.name == "Roadtrip");
        assert!(created);
    }

    #[test]
    fn add_remove_reorder_playlist_tracks() {
        let (mut app, mut doc) = app_with_doc();
        // pl_morning seeds [tk_p1, tk_r1, tk_g2, tk_r3].
        app.dispatch(Action::AddToPlaylist { playlist: "pl_morning".into(), track: "tk_h2".into() }, &mut doc);
        let pl = app.lib.playlist("pl_morning").unwrap().clone();
        assert_eq!(pl.track_ids.last().map(|s| s.as_str()), Some("tk_h2"));
        app.dispatch(Action::RemoveFromPlaylist { playlist: "pl_morning".into(), track: "tk_g2".into() }, &mut doc);
        assert!(!app.lib.playlist("pl_morning").unwrap().track_ids.iter().any(|t| t == "tk_g2"));
        app.dispatch(Action::MovePlaylistItem { playlist: "pl_morning".into(), from: 0, to: 1 }, &mut doc);
        assert_eq!(app.lib.playlist("pl_morning").unwrap().track_ids[1], "tk_p1");
    }

    #[test]
    fn rename_and_delete_playlist_actions() {
        let (mut app, mut doc) = app_with_doc();
        app.name_input = "Concentration".into();
        app.dispatch(Action::CommitRename("pl_focus".into()), &mut doc);
        assert_eq!(app.lib.playlist("pl_focus").unwrap().name, "Concentration");
        app.dispatch(Action::DeletePlaylist("pl_focus".into()), &mut doc);
        assert!(app.lib.playlist("pl_focus").is_none());
    }

    // ---- 7. real audio path -----------------------------------------------

    #[test]
    fn play_drives_the_audio_pipeline_path() {
        let (mut app, mut doc) = app_with_doc();
        app.dispatch(Action::Play { queue: vec!["tk_g1".into()], index: 0 }, &mut doc);
        // The transport started the stream: the engine recorded a load (the
        // stub records the resolved/intended URI; the live backend set the
        // playbin uri). Offline the source resolver returns an error so no URI
        // is loaded — but with the `streaming`/`audio` features the path runs.
        // Either way `start_stream` was invoked and play state is set.
        assert!(app.pb.playing);
        // Volume changes hit the engine.
        app.dispatch(Action::VolumeFrac(0.5), &mut doc);
        assert!((app.audio.volume() - 0.5).abs() < 1e-6, "volume reaches the audio engine");
    }

    // ---- 9. lyrics --------------------------------------------------------

    #[test]
    fn ensure_lyrics_loads_bundled_for_current() {
        let (mut app, _doc) = app_with_doc();
        app.pb.current = Some("tk_g1".into());
        app.lyrics_for = None;
        app.ensure_lyrics();
        assert!(!app.lyrics.is_empty(), "bundled lyrics load for the current track");
        assert!(app.lyrics_synced, "tk_g1 has synced lyrics");
    }

    // ---- shared draw helper -----------------------------------------------

    /// Render one offscreen frame through the production draw path so hit
    /// lists + geometry are populated (no GPU; just the Vello scene encode).
    fn draw_once(app: &mut MusicApp, doc: &mut AutoCommit) {
        use ui::text_render::TextRenderer;
        use ui::theme::Theme;
        // Lay the root out to the test viewport first.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(doc, &automerge::ROOT);
        let mut tr = TextRenderer::new();
        let theme = Theme::dark();
        let mut scene = Scene::new();
        app.render_frame(&mut scene, Affine::IDENTITY, &mut tr, &theme);
    }

    #[test]
    fn draw_registers_row_menu_hits() {
        let (mut app, mut doc) = app_with_doc();
        app.view = View::Songs;
        draw_once(&mut app, &mut doc);
        assert!(!app.row_menu_hits.is_empty(), "the songs table registers per-row right-click targets");
    }
}
