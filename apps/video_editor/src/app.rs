//! The editor's root widget: media bin, preview, transport, timeline and
//! inspector, all drawn into one Vello scene with hand-rolled hit-testing.
//!
//! Why one big custom widget instead of composed toolkit widgets? A
//! timeline is fundamentally a canvas: clips are positioned proportional
//! to time, trim handles live on sub-pixel edges, the playhead crosses
//! every lane. That density is far cleaner to express as direct drawing +
//! geometric hit-tests than as a Taffy tree. The structural state still
//! lives in the CRDT (see [`crate::doc`]) so the edit is collaborative and
//! undoable; only ephemeral *view* state (playhead, zoom, selection) is
//! held in widget fields, matching the toolkit's per-peer-view convention.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use uuid::Uuid;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{FlapState, SplitLayout, Widget};

use crate::actions::{
    delete_clip_action, ripple_delete_action, split_clip_action, upsert_clip_action,
    upsert_media_action, upsert_track_action, VideoResourceCtx, VIDEO_EDITOR_ACTION_TYPE,
};
use crate::doc::VideoDoc;
use crate::export::{
    begin_export, Container, ExportSettings, ExportStatus, FrameRate, Resolution,
};
use crate::model::{Clip, ClipId, Filters, MediaId, MediaItem, MediaKind, Project, TrackId, TrackKind, Transition};
use crate::portal::{self, ImportChannel, ImportOutcome};
use crate::preview::{self, CompositedFrame, Rgb};
use crate::probe;
use crate::timecode::{clock, frame_step, snap_to_frame, timecode};

// ---- layout constants ----------------------------------------------------
const TOPBAR_H: f64 = 48.0;
const TIMELINE_H: f64 = 304.0;
const BIN_W: f64 = 268.0;
const INSPECTOR_W: f64 = 300.0;
const GUTTER_W: f64 = 128.0;
const RULER_H: f64 = 30.0;
const TRANSPORT_H: f64 = 58.0;
const LANE_GAP: f64 = 4.0;
const VIDEO_LANE_H: f64 = 62.0;
const AUDIO_LANE_H: f64 = 46.0;
const TRIM_HIT: f64 = 8.0;
const PAD: f64 = 14.0;
/// Below this allocated width the media bin stops being a docked pane and
/// collapses into an overlay flap summoned by the topbar hamburger. Above it
/// the bin is docked beside the editor (the wide desktop layout).
const BIN_BREAKPOINT: f64 = 900.0;

/// Computed region rectangles for one frame, derived purely from the root
/// rect. Recomputed in both `draw` and `handle_event` so the two never
/// disagree about where anything is.
#[derive(Clone, Copy, Default)]
struct Regions {
    topbar: Rect,
    /// Hamburger toggle in the topbar. Zero-sized (and skipped) while the bin
    /// is docked; present only in the narrow overlay layout.
    menu_btn: Rect,
    export_btn: Rect,
    split_btn: Rect,
    import_btn: Rect,
    bin: Rect,
    preview: Rect,
    canvas: Rect,
    transport: Rect,
    inspector: Rect,
    timeline: Rect,
    ruler: Rect,
    gutter: Rect,
    lanes: Rect,
    /// Full-bounds dimming scrim drawn under the open overlay bin. Zero-sized
    /// while docked or while the flap is fully closed.
    scrim: Rect,
    /// `true` when the bin is collapsed to an overlay flap (narrow window).
    bin_overlay: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum TransportAction {
    ToStart,
    StepBack,
    PlayPause,
    StepFwd,
    ToEnd,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Playhead,
    Scrubber,
    ClipMove { id: ClipId, grab_dt: f64 },
    TrimIn { id: ClipId },
    TrimOut { id: ClipId },
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    FadeIn,
    FadeOut,
}

/// Which inspector slider a drag is editing. Brightness/contrast/saturation
/// share one code path; the variant disambiguates the field.
#[derive(Clone, Copy)]
enum FilterField {
    Brightness,
    Contrast,
    Saturation,
}

/// One-shot capture scenarios selected by the `--shots` entry points. Each
/// deterministically arranges editor state so a single offscreen frame shows
/// a specific feature (filters, a fresh split). `None` is the live editor.
#[derive(Clone, Copy, PartialEq)]
pub enum ShotScenario {
    /// Default: the live editor with its auto-selected clip.
    Live,
    /// The graded drone clip selected, filters + fades visible in the
    /// inspector, playhead inside the clip.
    SelectedFilters,
    /// A split performed at the playhead, leaving two contiguous clips.
    JustSplit,
}

pub struct VideoEditorApp {
    project: Project,
    /// Deterministic capture scenario applied once during the first `sync`.
    shot_scenario: ShotScenario,
    /// Guards the one-time scenario setup so it doesn't re-run every sync.
    shot_applied: bool,
    // --- view state (per-peer, never in the CRDT) ---
    playhead: f64,
    zoom: f64,  // pixels per second
    scroll: f64, // seconds at the left edge of the lane area
    selected: Option<ClipId>,
    selected_media: Option<MediaId>,
    playing: bool,
    // --- ui state ---
    show_export: bool,
    export: ExportSettings,
    export_status: ExportStatus,
    /// A media-import portal request in flight, drained each `sync`.
    import_rx: Option<ImportChannel>,
    /// A short, honest status line shown after an import attempt (how many
    /// clips landed, or why the picker was unavailable). Never a fake success.
    import_notice: Option<String>,
    /// A file queued to import on the next `sync` (deterministic `--shots` /
    /// tests seed a real clip here instead of opening the portal).
    pending_imports: Vec<PathBuf>,
    /// When set, a queued import is also dropped onto the timeline (creating a
    /// track if the blank project has none) and the playhead parked inside it —
    /// so a `--shots` capture / preload shows the clip in the preview, not just
    /// the bin. Off for a plain portal import (which lands in the bin only).
    auto_place_imports: bool,
    /// Real preview decoder. Only driven under `--features decode`, where it
    /// decodes the true frame of the clip under the playhead through GStreamer.
    #[cfg(feature = "decode")]
    preview_decoder: preview::PreviewDecoder,
    drag: Option<Drag>,
    last_mouse: Point,
    hover_clip: Option<ClipId>,
    hover_media: Option<MediaId>,
    last_click_media: Option<(MediaId, Instant)>,
    seeded: bool,
    /// When true this is the hermetic `--shots`/demo build: the first sync
    /// seeds the bundled sample project into an empty document. The live app
    /// leaves this false and never fabricates a project — it opens blank.
    seed_demo: bool,
    initialised_selection: bool,
    last_tick: Instant,
    rect: Rect,
    /// Framework-owned action dispatch. Every document-mutating affordance
    /// dispatches its [`VideoEditorAction`](crate::actions::VideoEditorAction)
    /// through this: the host brackets the undo boundary, applies the action via
    /// the executor (so it replays byte-identically on peers), and remembers it
    /// for the affordance == `ActionSpec` covenant test. Ctrl+Z / Ctrl+Y are
    /// handled OS-wide by the toolkit off the recorded boundary.
    actions: ui::ActionHost,
    /// Copied clip, ready to paste at the playhead (Ctrl+C / Ctrl+V). Holds
    /// a full clip snapshot — trim window, filters and fades come along.
    clipboard: Option<Clip>,
    /// JKL shuttle rate: -1 reverse, 0 stop, +1..N forward speed multiplier.
    /// Repeated L speeds up, J reverses, K stops — the standard NLE shuttle.
    shuttle: f64,
    /// `false` until the first laid-out frame resolves the bin flap to the
    /// width's natural state — so a cold launch into a narrow window starts
    /// with the bin already collapsed instead of flashing it open.
    flap_settled: bool,
    // --- responsive media-bin flap (shared toolkit kernel) ---
    /// Drives the media bin's docked-vs-overlay reveal. The bin is the
    /// primary sidebar: docked beside the editor on a wide window, an
    /// eased overlay flap behind a scrim on a narrow one. `open_when_docked`
    /// keeps the wide layout pixel-identical to before.
    bin_flap: FlapState,
    /// Real-time clock for the flap's frame-rate-independent eased slide.
    flap_tick: Instant,
}

impl Default for VideoEditorApp {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoEditorApp {
    pub fn new() -> Self {
        Self {
            project: Project::new("Untitled"),
            shot_scenario: ShotScenario::Live,
            shot_applied: false,
            playhead: 10.0,
            zoom: 34.0,
            scroll: 0.0,
            selected: None,
            selected_media: None,
            playing: false,
            // Dev aid: `QOS_VE_EXPORT=1` opens straight into the export
            // dialog so its layout can be screenshotted headlessly.
            show_export: std::env::var_os("QOS_VE_EXPORT").is_some(),
            export: ExportSettings::default(),
            export_status: ExportStatus::Idle,
            import_rx: None,
            import_notice: None,
            pending_imports: Vec::new(),
            auto_place_imports: false,
            #[cfg(feature = "decode")]
            preview_decoder: preview::PreviewDecoder::new(),
            drag: None,
            last_mouse: Point::ZERO,
            hover_clip: None,
            hover_media: None,
            last_click_media: None,
            seeded: false,
            seed_demo: false,
            initialised_selection: false,
            last_tick: Instant::now(),
            rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
            clipboard: None,
            shuttle: 0.0,
            flap_settled: false,
            bin_flap: FlapState::open_when_docked(),
            flap_tick: Instant::now(),
        }
    }

    /// The hermetic `--shots`/demo build: seeds the bundled sample project on
    /// the first sync. This is the sole source of sample media/clips — the
    /// live app never fabricates a project.
    pub fn new_demo() -> Self {
        let mut app = Self::new();
        app.seed_demo = true;
        app
    }

    /// Construct the editor pre-arranged for a deterministic `--shots`
    /// capture scenario (see [`ShotScenario`]). The scenario is applied once
    /// during the first `sync`, after the sample project loads.
    pub fn new_for_shot(scenario: ShotScenario) -> Self {
        let mut app = Self::new_demo();
        app.shot_scenario = scenario;
        app
    }

    /// Build a blank live app that imports `paths` on its first `sync` (as
    /// real clips, probed for real metadata). Used by `--shots`/tests to seed a
    /// real imported clip deterministically without opening the portal — the
    /// same code path a portal pick drives.
    pub fn new_import(paths: Vec<PathBuf>) -> Self {
        let mut app = Self::new();
        app.pending_imports = paths;
        app
    }

    /// Like [`Self::new_import`] but also drops the first imported clip onto the
    /// timeline (creating a track if needed) and parks the playhead inside it,
    /// so the preview has real content to decode/composite for a `--shots`
    /// capture or a `--import` preload.
    pub fn new_import_placed(paths: Vec<PathBuf>) -> Self {
        let mut app = Self::new_import(paths);
        app.auto_place_imports = true;
        app
    }

    /// Whether the real preview decoder currently has a decoded frame on the
    /// canvas. The `--shots` decode path pumps `sync` until this is true so an
    /// asynchronously-prerolled frame is captured, not a blank one.
    pub fn has_preview_frame(&self) -> bool {
        #[cfg(feature = "decode")]
        {
            self.preview_decoder.frame().is_some()
        }
        #[cfg(not(feature = "decode"))]
        {
            false
        }
    }

    /// Like [`Self::new`] but with the media bin flap snapped fully open, for
    /// a deterministic narrow-window `--shots` capture that shows the overlay
    /// sidebar revealed in a single frame.
    pub fn new_with_bin_open() -> Self {
        let mut app = Self::new_demo();
        app.bin_flap.snap(true);
        // Pre-settled: the explicit open snap is authoritative, so the first
        // laid-out frame must not re-snap it to the width's default.
        app.flap_settled = true;
        app
    }

    /// Resolve the bin's docked-vs-overlay layout purely from the allocated
    /// width against the breakpoint. The shared `SplitLayout` is the single
    /// source of truth so the kernel's reconcile/animation agree with our
    /// geometry.
    fn bin_layout(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), BIN_BREAKPOINT)
    }

    // ---- region geometry -------------------------------------------------
    fn regions(&self) -> Regions {
        let r = self.rect;
        let layout = self.bin_layout();
        let overlay = layout == SplitLayout::Overlay;
        let topbar = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOPBAR_H);
        let export_btn = Rect::new(r.x1 - 124.0, r.y0 + 9.0, r.x1 - 14.0, r.y0 + 39.0);
        let split_btn = Rect::new(r.x1 - 244.0, r.y0 + 9.0, r.x1 - 134.0, r.y0 + 39.0);
        let import_btn = Rect::new(r.x1 - 366.0, r.y0 + 9.0, r.x1 - 254.0, r.y0 + 39.0);
        // The hamburger replaces the static app glyph slot when the bin is an
        // overlay, so the user can summon the collapsed bin.
        let menu_btn = if overlay {
            Rect::new(r.x0 + 12.0, r.y0 + 10.0, r.x0 + 40.0, r.y0 + 38.0)
        } else {
            Rect::ZERO
        };
        let timeline_top = (r.y1 - TIMELINE_H).max(topbar.y1 + 80.0);
        let timeline = Rect::new(r.x0, timeline_top, r.x1, r.y1);
        let mid_top = topbar.y1;
        let mid_bot = timeline.y0;
        let inspector = Rect::new(r.x1 - INSPECTOR_W, mid_top, r.x1, mid_bot);
        // Docked: the bin occupies its column and the preview starts after it.
        // Overlay: the preview reclaims the bin's column (full editor width)
        // and the bin slides in from the left over a scrim by the eased
        // reveal — exactly the flap kernel's overlay geometry.
        let (bin, preview_x0, scrim) = if overlay {
            let slide = self.bin_flap.slide();
            let bin_x0 = r.x0 - BIN_W * (1.0 - slide);
            let bin = Rect::new(bin_x0, mid_top, bin_x0 + BIN_W, r.y1);
            let scrim = if self.bin_flap.reveal() > 0.001 {
                Rect::new(r.x0, mid_top, r.x1, r.y1)
            } else {
                Rect::ZERO
            };
            (bin, r.x0, scrim)
        } else {
            let bin = Rect::new(r.x0, mid_top, r.x0 + BIN_W, mid_bot);
            (bin, bin.x1, Rect::ZERO)
        };
        let preview = Rect::new(preview_x0, mid_top, inspector.x0, mid_bot);
        let transport = Rect::new(preview.x0, preview.y1 - TRANSPORT_H, preview.x1, preview.y1);
        let canvas_area = Rect::new(preview.x0 + 10.0, preview.y0 + 10.0, preview.x1 - 10.0, transport.y0 - 8.0);
        let canvas = fit_aspect(canvas_area, self.project.width.max(16) as f64, self.project.height.max(9) as f64);
        let ruler = Rect::new(timeline.x0 + GUTTER_W, timeline.y0, timeline.x1, timeline.y0 + RULER_H);
        let gutter = Rect::new(timeline.x0, timeline.y0 + RULER_H, timeline.x0 + GUTTER_W, timeline.y1);
        let lanes = Rect::new(timeline.x0 + GUTTER_W, timeline.y0 + RULER_H, timeline.x1, timeline.y1);
        Regions {
            topbar, menu_btn, export_btn, split_btn, import_btn, bin, preview, canvas, transport,
            inspector, timeline, ruler, gutter, lanes, scrim, bin_overlay: overlay,
        }
    }

    fn time_to_x(&self, t: f64, lanes: &Rect) -> f64 {
        lanes.x0 + (t - self.scroll) * self.zoom
    }
    fn x_to_time(&self, x: f64, lanes: &Rect) -> f64 {
        (self.scroll + (x - lanes.x0) / self.zoom).max(0.0)
    }

    /// (track id, kind, lane rect) in display order. Lane rect spans the
    /// full lane width.
    fn lane_rects(&self, r: &Regions) -> Vec<(TrackId, TrackKind, Rect)> {
        let mut out = Vec::new();
        let mut y = r.lanes.y0 + LANE_GAP;
        for t in self.project.display_tracks() {
            let h = if t.kind == TrackKind::Video { VIDEO_LANE_H } else { AUDIO_LANE_H };
            let lane = Rect::new(r.lanes.x0, y, r.lanes.x1, y + h);
            out.push((t.id, t.kind, lane));
            y += h + LANE_GAP;
        }
        out
    }

    fn clip_rect(&self, clip: &Clip, lane: &Rect, r: &Regions) -> Rect {
        let x0 = self.time_to_x(clip.start, &r.lanes);
        let x1 = self.time_to_x(clip.end(), &r.lanes);
        Rect::new(x0, lane.y0 + 2.0, x1.max(x0 + 2.0), lane.y1 - 2.0)
    }

    fn transport_buttons(&self, r: &Regions) -> [(TransportAction, Rect); 5] {
        let cy = r.transport.y0 + 20.0;
        let cx0 = r.transport.x0 + 30.0;
        let gap = 40.0;
        let mk = |i: f64, a: TransportAction| {
            let cx = cx0 + i * gap;
            (a, Rect::new(cx - 15.0, cy - 15.0, cx + 15.0, cy + 15.0))
        };
        [
            mk(0.0, TransportAction::ToStart),
            mk(1.0, TransportAction::StepBack),
            mk(2.0, TransportAction::PlayPause),
            mk(3.0, TransportAction::StepFwd),
            mk(4.0, TransportAction::ToEnd),
        ]
    }

    fn scrubber_rect(&self, r: &Regions) -> Rect {
        Rect::new(r.transport.x0 + 220.0, r.transport.y0 + 38.0, r.transport.x1 - 16.0, r.transport.y0 + 46.0)
    }

    /// Media for the bin in a stable, meaningful order: visual media
    /// first, then audio, each alphabetised. (The CRDT key iteration order
    /// is otherwise arbitrary.)
    fn bin_order(&self) -> Vec<MediaId> {
        let mut v: Vec<&crate::model::MediaItem> = self.project.media.iter().collect();
        v.sort_by(|a, b| {
            b.kind.is_visual().cmp(&a.kind.is_visual()).then(a.name.cmp(&b.name))
        });
        v.into_iter().map(|m| m.id).collect()
    }

    fn bin_item_rects(&self, r: &Regions) -> Vec<(MediaId, Rect)> {
        let mut out = Vec::new();
        let mut y = r.bin.y0 + 40.0;
        let row_h = 56.0;
        for mid in self.bin_order() {
            let rect = Rect::new(r.bin.x0 + 8.0, y, r.bin.x1 - 8.0, y + row_h - 6.0);
            out.push((mid, rect));
            y += row_h;
        }
        out
    }

    // ---- playback --------------------------------------------------------
    fn advance_playback(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        let dur = self.project.duration();
        if self.playing {
            self.playhead += dt;
            if self.playhead >= dur {
                self.playhead = dur;
                self.playing = false;
            }
        } else if self.shuttle != 0.0 {
            // Shuttle scrubs at `shuttle`× real time; clamps at the ends.
            self.playhead = (self.playhead + dt * self.shuttle).clamp(0.0, dur);
            if self.playhead <= 0.0 || self.playhead >= dur {
                self.shuttle = 0.0;
            }
        }
    }

    /// Reconcile the bin flap with the current width and advance its eased
    /// reveal using real elapsed time. Called once at the top of `draw`.
    fn advance_flap(&mut self) {
        let layout = self.bin_layout();
        self.bin_flap.reconcile(layout);
        // First settled frame: snap to the width's natural state so a cold
        // narrow launch starts collapsed (no open→closed flash) and a cold
        // wide launch starts docked-open.
        if !self.flap_settled {
            self.bin_flap.snap(layout == SplitLayout::Docked);
            self.flap_settled = true;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.flap_tick).as_secs_f64();
        self.flap_tick = now;
        self.bin_flap.tick(layout, dt);
    }

    /// Toggle the media bin flap (the topbar hamburger / `B` shortcut).
    fn toggle_bin(&mut self) {
        self.bin_flap.toggle(self.bin_layout());
    }

    /// Whether the bin flap is animating and the host should keep painting.
    fn flap_animating(&self) -> bool {
        self.bin_flap.is_animating(self.bin_layout())
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        self.last_tick = Instant::now();
        if self.playing && self.playhead >= self.project.duration() {
            self.playhead = 0.0;
        }
    }

    fn step_frame(&mut self, dir: f64) {
        let step = frame_step(self.project.fps);
        self.playhead = (self.playhead + dir * step).clamp(0.0, self.project.duration());
        self.playing = false;
        self.shuttle = 0.0;
    }

    /// J/K/L shuttle. Pressing in the *same* direction doubles the rate
    /// (1→2→4…); pressing the opposite direction resets to ±1×; the rate is
    /// advanced each frame in [`Self::advance_playback`].
    fn shuttle_key(&mut self, dir: f64) {
        // Same direction speeds up; reversal restarts at 1×.
        if self.shuttle.signum() == dir.signum() && self.shuttle != 0.0 {
            self.shuttle = (self.shuttle * 2.0).clamp(-16.0, 16.0);
        } else {
            self.shuttle = dir;
        }
        self.playing = false; // shuttle and play are mutually exclusive
        self.last_tick = Instant::now();
    }

    // ---- mutations (dispatched as replayable actions) --------------------
    // Every document-mutating edit dispatches a `VideoEditorAction` through the
    // framework's `ActionHost`: it brackets the undo boundary, applies the
    // action through the executor (so it replays byte-identically on peers), and
    // remembers it for the covenant test. There is no per-edit `needs_commit`
    // flag any more — the host records exactly one undo boundary per event.

    /// Dispatch a whole-clip write for `id` — the single path add / move / trim
    /// / grade all funnel through. Carries the clip's final shape, so it replays
    /// deterministically with no per-frame churn.
    fn dispatch_clip(&mut self, doc: &mut AutoCommit, id: ClipId) {
        if let Some(c) = self.project.clip(id).cloned() {
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_clip_action(&c));
        }
    }

    fn split_selected(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected else { return };
        if let Some(rid) = self.project.split_clip(id, self.playhead) {
            let left = self.project.clip(id).cloned();
            let right = self.project.clip(rid).cloned();
            if let (Some(left), Some(right)) = (left, right) {
                self.actions.dispatch(
                    doc,
                    VIDEO_EDITOR_ACTION_TYPE,
                    &split_clip_action(&left, &right),
                );
            }
            self.selected = Some(rid);
        }
    }

    fn delete_selected(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected else { return };
        self.project.delete_clip(id);
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &delete_clip_action(&id));
        self.selected = None;
    }

    /// Delete the selected clip and close the gap, shifting later clips on
    /// the same track left. The whole ripple — the delete plus every shifted
    /// clip — is carried in one action, so it lands as a single undo boundary
    /// and replays atomically on a peer.
    fn ripple_delete_selected(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected else { return };
        let moved_ids = self.project.ripple_delete(id);
        let moved: Vec<Clip> = moved_ids
            .iter()
            .filter_map(|m| self.project.clip(*m).cloned())
            .collect();
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &ripple_delete_action(&id, &moved));
        self.selected = None;
    }

    /// Copy the selected clip to the clipboard (no CRDT change).
    fn copy_selected(&mut self) {
        if let Some(id) = self.selected {
            self.clipboard = self.project.clip(id).cloned();
        }
    }

    /// Paste the clipboard clip at the playhead, on its original track.
    /// Falls back to the selected clip's track. New clip becomes selected.
    fn paste_clip(&mut self, doc: &mut AutoCommit) {
        let Some(src) = self.clipboard.clone() else { return };
        // Keep the copied clip's track if it still exists, else the
        // currently-selected clip's track, else the first track.
        let track = self
            .project
            .track(src.track)
            .map(|t| t.id)
            .or_else(|| self.selected.and_then(|s| self.project.clip(s)).map(|c| c.track))
            .or_else(|| self.project.tracks.first().map(|t| t.id));
        let Some(track) = track else { return };
        let id = self.project.paste_clip(src, track, self.playhead);
        self.dispatch_clip(doc, id);
        self.selected = Some(id);
    }

    /// Set the in-point of the selected clip to the playhead (`I`): trims
    /// the head so the clip starts at the current frame.
    fn set_in_at_playhead(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected else { return };
        let delta = self.project.clip(id).map(|c| self.playhead - c.start).unwrap_or(0.0);
        if delta.abs() < 1e-6 {
            return;
        }
        self.project.trim_in(id, delta);
        self.dispatch_clip(doc, id);
    }

    /// Set the out-point of the selected clip to the playhead (`O`): trims
    /// the tail so the clip ends at the current frame.
    fn set_out_at_playhead(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected else { return };
        let delta = self.project.clip(id).map(|c| self.playhead - c.end()).unwrap_or(0.0);
        if delta.abs() < 1e-6 {
            return;
        }
        self.project.trim_out(id, delta);
        self.dispatch_clip(doc, id);
    }

    /// Nudge the selected clip by `dt` seconds (arrow keys when a clip is
    /// selected). Snaps the result to the frame grid.
    fn nudge_selected(&mut self, doc: &mut AutoCommit, dt: f64) {
        let Some(id) = self.selected else { return };
        let new_start = self
            .project
            .clip(id)
            .map(|c| snap_to_frame((c.start + dt).max(0.0), self.project.fps))
            .unwrap_or(0.0);
        self.project.move_clip(id, new_start);
        self.dispatch_clip(doc, id);
    }

    /// Apply `edit` to the selected clip and dispatch a whole-clip write. The
    /// single write-through path for a *discrete* per-clip field edit (a
    /// transition / palette click, or a keyboard-driven grade) — each is one
    /// undo step. Continuous slider drags use [`Self::preview_edit`] +
    /// [`Self::commit_drag`] instead, so a drag is one action, not one a frame.
    fn edit_selected(&mut self, doc: &mut AutoCommit, edit: impl FnOnce(&mut Clip)) {
        let Some(id) = self.selected else { return };
        if let Some(c) = self.project.clip_mut(id) {
            edit(c);
        }
        self.dispatch_clip(doc, id);
    }

    /// Apply `edit` to the selected clip **in memory only** — the live drag
    /// preview. No action is dispatched; the final value is committed once when
    /// the drag ends (see [`Self::commit_drag`]).
    fn preview_edit(&mut self, edit: impl FnOnce(&mut Clip)) {
        let Some(id) = self.selected else { return };
        if let Some(c) = self.project.clip_mut(id) {
            edit(c);
        }
    }

    fn set_filter(&mut self, doc: &mut AutoCommit, field: FilterField, v: f64) {
        let v = v.clamp(0.0, 1.0);
        self.edit_selected(doc, move |c| match field {
            FilterField::Brightness => c.filters.brightness = v,
            FilterField::Contrast => c.filters.contrast = v,
            FilterField::Saturation => c.filters.saturation = v,
        });
    }
    fn set_transition(&mut self, doc: &mut AutoCommit, t: Transition) {
        self.edit_selected(doc, move |c| c.transition = t);
    }
    fn set_palette(&mut self, doc: &mut AutoCommit, p: u8) {
        self.edit_selected(doc, move |c| c.palette = p);
    }

    /// Append the bin-selected (or given) media as a clip on a matching
    /// track, at the end of that track's content.
    fn add_media_to_timeline(&mut self, doc: &mut AutoCommit, media: MediaId) {
        let Some(m) = self.project.media(media).cloned() else { return };
        let kind = if m.kind.is_visual() { TrackKind::Video } else { TrackKind::Audio };
        // Lowest-index track of the right kind, else any.
        let track = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .min_by_key(|t| t.index)
            .map(|t| t.id);
        let Some(track) = track else { return };
        let start = self
            .project
            .clips_on(track)
            .last()
            .map(|c| c.end())
            .unwrap_or(0.0);
        if let Some(id) = self.project.add_clip(track, media, start) {
            self.dispatch_clip(doc, id);
            self.selected = Some(id);
        }
    }

    // ---- media import ----------------------------------------------------
    /// Open the host file picker to import media (a real XDG portal on
    /// `--features portal`, otherwise an honest "unavailable" notice — never a
    /// fake success).
    fn trigger_import(&mut self) {
        self.import_notice = Some("Choose media to import…".to_string());
        self.import_rx = Some(portal::request_import());
    }

    /// Drain any pending imports (queued deterministic files first, then a
    /// portal outcome), bringing each chosen file into the bin as a real clip.
    fn drain_import(&mut self, doc: &mut AutoCommit) {
        if !self.pending_imports.is_empty() {
            let paths = std::mem::take(&mut self.pending_imports);
            let n = self.import_files(doc, &paths);
            self.import_notice = Some(import_summary(n, paths.len()));
            if self.auto_place_imports {
                if let Some(mid) = self.selected_media {
                    self.ensure_track_and_place(doc, mid);
                }
            }
        }
        let mut clear = false;
        let mut outcome = None;
        if let Some(rx) = self.import_rx.as_ref() {
            match rx.try_recv() {
                Ok(o) => {
                    outcome = Some(o);
                    clear = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(std::sync::mpsc::TryRecvError::Disconnected) => clear = true,
            }
        }
        if clear {
            self.import_rx = None;
        }
        match outcome {
            Some(ImportOutcome::Chosen(paths)) => {
                let n = self.import_files(doc, &paths);
                self.import_notice = Some(import_summary(n, paths.len()));
            }
            Some(ImportOutcome::Cancelled) => self.import_notice = None,
            Some(ImportOutcome::Failed(reason)) => self.import_notice = Some(reason),
            None => {}
        }
    }

    /// Import a batch of files, returning how many became real clips.
    fn import_files(&mut self, doc: &mut AutoCommit, paths: &[PathBuf]) -> usize {
        paths.iter().filter(|p| self.import_one(doc, p)).count()
    }

    /// Import one file: probe **real** metadata from the container and add it to
    /// the media bin as a real [`MediaItem`], replicated through the CRDT. The
    /// stored path is workspace-relative for a file under the FUSE mount (so it
    /// replicates to peers) or absolute for an imported local file.
    fn import_one(&mut self, doc: &mut AutoCommit, path: &Path) -> bool {
        let probed = match probe::probe_file(path) {
            Ok(p) => p,
            Err(_) => return false,
        };
        let resolved = crate::media::resolve(&path.to_string_lossy());
        let stored = crate::media::stored_path_for_import(&resolved.abs_path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| stored.clone());
        let palette = (self.project.media.len() as u8) % 7;
        let item = MediaItem {
            id: Uuid::new_v4(),
            name,
            kind: probed.kind,
            duration: probed.duration,
            width: probed.width,
            height: probed.height,
            fps: if probed.fps > 0.0 { probed.fps } else { 30.0 },
            palette,
            path: stored,
            hydration: resolved.hydration,
        };
        let id = item.id;
        self.project.media.push(item.clone());
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&item));
        self.selected_media = Some(id);
        true
    }

    /// Drop `media_id` onto the timeline (creating a track of the right kind if
    /// the blank project has none) and park the playhead inside the new clip.
    /// Used by the `--shots`/preload placed-import path so the preview shows the
    /// clip; a normal portal import lands in the bin only.
    fn ensure_track_and_place(&mut self, doc: &mut AutoCommit, media_id: MediaId) {
        let Some(m) = self.project.media(media_id).cloned() else { return };
        let kind = if m.kind.is_visual() { TrackKind::Video } else { TrackKind::Audio };
        if !self.project.tracks.iter().any(|t| t.kind == kind) {
            let track = crate::model::Track {
                id: Uuid::new_v4(),
                name: if kind == TrackKind::Video { "V1" } else { "A1" }.into(),
                kind,
                index: 1,
                muted: false,
                locked: false,
            };
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&track));
            self.project.tracks.push(track);
        }
        self.add_media_to_timeline(doc, media_id);
        // Park the playhead inside the placed clip so the preview has content.
        if let Some(cid) = self.selected {
            if let Some(c) = self.project.clip(cid) {
                let inside = c.start + (c.duration() * 0.4);
                self.playhead = inside.clamp(c.start, (c.end() - 0.05).max(c.start));
            }
        }
    }

    /// The clickable "Import media" button drawn in the empty-bin state, if the
    /// bin is on-screen and has no media yet.
    fn empty_bin_import_rect(&self, r: &Regions) -> Option<Rect> {
        if !self.project.media.is_empty() {
            return None;
        }
        let cx = r.bin.center().x;
        let cy = r.bin.y0 + 150.0;
        Some(Rect::new(cx - 92.0, cy, cx + 92.0, cy + 34.0))
    }

    /// Pump the real preview decoder for the clip under the playhead. No-op
    /// build carries no decoder; the decode build seeks + decodes the frame.
    #[cfg(feature = "decode")]
    fn pump_preview_decode(&mut self) {
        self.preview_decoder.update(&self.project, self.playhead);
    }

    fn zoom_by(&mut self, factor: f64) {
        self.zoom = (self.zoom * factor).clamp(6.0, 220.0);
    }

    fn inspector_layout(&self, r: &Regions) -> InspectorLayout {
        let mut l = InspectorLayout::default();
        let Some(id) = self.selected else { return l };
        let Some(clip) = self.project.clip(id) else { return l };
        let is_video = self
            .project
            .media(clip.media)
            .map(|m| m.kind.is_visual())
            .unwrap_or(true);
        let x0 = r.inspector.x0 + PAD;
        let x1 = r.inspector.x1 - PAD;
        // Static header + four property rows consume a fixed block; the
        // editable controls start below it. A compact 36px pitch fits a
        // grade stack + fades + transition + label + actions in the pane.
        let mut y = r.inspector.y0 + 146.0;
        let row = 34.0; // label sits at y, the 9px track at y+19
        let track_at = |y: f64| Rect::new(x0, y + 19.0, x1, y + 28.0);

        if is_video {
            l.opacity = Some(track_at(y)); y += row;
            l.brightness = Some(track_at(y)); y += row;
            l.contrast = Some(track_at(y)); y += row;
            l.saturation = Some(track_at(y)); y += row;
        } else {
            l.volume = Some(track_at(y)); y += row;
        }
        // Fade in / out share a row (two half-width tracks).
        let half = (x1 - x0) / 2.0 - 6.0;
        l.fade_in = Some(Rect::new(x0, y + 19.0, x0 + half, y + 28.0));
        l.fade_out = Some(Rect::new(x1 - half, y + 19.0, x1, y + 28.0));
        y += row + 2.0;

        // Transition segmented control.
        let seg_w = (x1 - x0) / Transition::ALL.len() as f64;
        l.transition_y = y + 15.0;
        for (i, t) in Transition::ALL.iter().enumerate() {
            let sx0 = x0 + i as f64 * seg_w;
            l.transitions.push((*t, Rect::new(sx0, y + 15.0, sx0 + seg_w - 3.0, y + 37.0)));
        }
        y += 48.0;
        // Label color swatches.
        l.colors_y = y + 15.0;
        let sw = 24.0;
        for i in 0..7u8 {
            let sx0 = x0 + i as f64 * (sw + 4.0);
            if sx0 + sw > x1 { break; }
            l.colors.push((i, Rect::new(sx0, y + 15.0, sx0 + sw, y + 15.0 + sw)));
        }
        y += 44.0;
        l.split = Rect::new(x0, y, (x0 + x1) / 2.0 - 4.0, y + 28.0);
        l.delete = Rect::new((x0 + x1) / 2.0 + 4.0, y, x1, y + 28.0);
        l.valid = true;
        l
    }

    fn export_layout(&self, r: &Regions) -> ExportLayout {
        let card_w = 460.0;
        let card_h = 360.0;
        let cx = (r.timeline.x0 + r.timeline.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let card = Rect::new(cx - card_w / 2.0, cy - card_h / 2.0, cx + card_w / 2.0, cy + card_h / 2.0);
        let x0 = card.x0 + 24.0;
        let x1 = card.x1 - 24.0;
        let mut l = ExportLayout { card, ..Default::default() };
        let mut y = card.y0 + 70.0;
        let seg = |x0: f64, x1: f64, n: usize, y: f64| -> Vec<Rect> {
            let w = (x1 - x0) / n as f64;
            (0..n).map(|i| Rect::new(x0 + i as f64 * w, y, x0 + (i as f64 + 1.0) * w - 4.0, y + 32.0)).collect()
        };
        for (i, rect) in seg(x0, x1, Resolution::ALL.len(), y).into_iter().enumerate() {
            l.resolution.push((Resolution::ALL[i], rect));
        }
        y += 64.0;
        for (i, rect) in seg(x0, x1, Container::ALL.len(), y).into_iter().enumerate() {
            l.container.push((Container::ALL[i], rect));
        }
        y += 64.0;
        for (i, rect) in seg(x0, x1, FrameRate::ALL.len(), y).into_iter().enumerate() {
            l.fps.push((FrameRate::ALL[i], rect));
        }
        l.cancel = Rect::new(x0, card.y1 - 50.0, (x0 + x1) / 2.0 - 6.0, card.y1 - 18.0);
        l.confirm = Rect::new((x0 + x1) / 2.0 + 6.0, card.y1 - 50.0, x1, card.y1 - 18.0);
        l
    }

    /// Deterministically arrange editor state for a `--shots` scenario. Real
    /// production edit paths are used (split/grade through the CRDT) so the
    /// captured frame reflects genuine behaviour, not a mock.
    fn apply_shot_scenario(&mut self, state: &mut AutoCommit) {
        match self.shot_scenario {
            ShotScenario::Live => {}
            ShotScenario::SelectedFilters => {
                // Select the graded drone clip on V1 and park the playhead a
                // few seconds in (past its fade-in) so the inspector shows a
                // graded clip with fades and the preview shows the grade.
                if let Some(c) = self.project.top_video_clip_at(4.0) {
                    self.selected = Some(c.id);
                    self.selected_media = Some(c.media);
                }
                self.playhead = 4.0;
                // Push the saturation up a touch through the real edit path so
                // the grade is unmistakable in the capture.
                self.set_filter(state, FilterField::Saturation, 0.72);
                self.zoom = 30.0;
            }
            ShotScenario::JustSplit => {
                // Select the interview body clip and split it at the playhead,
                // then select the right half — exactly what pressing S does.
                if let Some(c) = self.project.top_video_clip_at(12.0) {
                    self.selected = Some(c.id);
                }
                self.playhead = 14.0;
                self.split_selected(state);
                self.zoom = 40.0;
            }
        }
    }

    fn ensure_initial_selection(&mut self) {
        if self.initialised_selection || self.project.clips.is_empty() {
            return;
        }
        self.selected = self
            .project
            .top_video_clip_at(self.playhead)
            .map(|c| c.id)
            .or_else(|| self.project.clips.first().map(|c| c.id));
        self.selected_media = self.project.media.first().map(|m| m.id);
        self.initialised_selection = true;
    }

    /// The presentation context Ctrl+K's resource tree is built from: a snapshot
    /// of the open project plus the live playhead. Returns `None` when nothing
    /// is open (a blank project), so the palette shows an empty tree — a steady,
    /// level-triggered value recomputed each frame.
    pub(crate) fn video_resource_ctx(&self) -> Option<VideoResourceCtx> {
        if self.project.tracks.is_empty() && self.project.clips.is_empty() {
            return None;
        }
        Some(VideoResourceCtx {
            project: self.project.clone(),
            playhead: self.playhead,
        })
    }
}

#[derive(Default)]
struct InspectorLayout {
    valid: bool,
    opacity: Option<Rect>,
    volume: Option<Rect>,
    brightness: Option<Rect>,
    contrast: Option<Rect>,
    saturation: Option<Rect>,
    fade_in: Option<Rect>,
    fade_out: Option<Rect>,
    transition_y: f64,
    transitions: Vec<(Transition, Rect)>,
    colors_y: f64,
    colors: Vec<(u8, Rect)>,
    split: Rect,
    delete: Rect,
}

#[derive(Default)]
struct ExportLayout {
    card: Rect,
    resolution: Vec<(Resolution, Rect)>,
    container: Vec<(Container, Rect)>,
    fps: Vec<(FrameRate, Rect)>,
    cancel: Rect,
    confirm: Rect,
}

// ==========================================================================
// Widget impl
// ==========================================================================
impl Widget for VideoEditorApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            // The live app opens on a blank project; only the hermetic demo
            // build seeds the bundled sample project into a fresh document.
            if self.seed_demo && VideoDoc::is_empty(state) {
                VideoDoc::seed_sample(state);
            }
            self.seeded = true;
        }
        // While a clip-editing drag is in flight, the live preview lives only in
        // the in-memory project — the CRDT isn't written until the drag commits
        // — so don't clobber it by reloading a stale document mid-drag.
        if !self.drag_is_editing() {
            self.project = VideoDoc::load_project(state);
        }
        // Bring in any pending / portal-picked media as real clips (this pushes
        // straight into `self.project.media` and the CRDT).
        self.drain_import(state);
        // Drop a stale selection if the clip vanished (e.g. remote delete).
        if let Some(id) = self.selected {
            if self.project.clip(id).is_none() {
                self.selected = None;
            }
        }
        self.ensure_initial_selection();
        // Apply a one-shot capture scenario once the project is loaded.
        if !self.shot_applied && self.shot_scenario != ShotScenario::Live
            && !self.project.clips.is_empty()
        {
            self.apply_shot_scenario(state);
            self.shot_applied = true;
        }
        // Keep the real preview decoder tracking the clip under the playhead so
        // the `--shots` warm-up (which pumps `sync`) captures a decoded frame.
        #[cfg(feature = "decode")]
        self.pump_preview_decode();
    }

    fn needs_idle_sync(&self) -> bool {
        // Keep syncing until the demo/shared project is loaded so a
        // freshly-spawned, input-free instance still seeds + paints. The live
        // app opens blank, so once seeded it must not idle-sync forever. Also
        // keep syncing while an import is pending so its outcome is drained.
        !self.seeded
            || (self.seed_demo && self.project.clips.is_empty())
            || self.import_rx.is_some()
            || !self.pending_imports.is_empty()
    }

    fn needs_animation_frame(&self) -> bool {
        self.playing || self.shuttle != 0.0 || self.flap_animating() || self.import_rx.is_some()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
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
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.advance_playback();
        self.advance_flap();
        // Track the decoded frame to the (possibly advancing) playhead.
        #[cfg(feature = "decode")]
        self.pump_preview_decode();
        let r = self.regions();
        self.draw_chrome(scene, transform, tr, theme, &r);
        // Editor body. When the bin is docked it draws in its column; when
        // it's an overlay flap it is drawn LAST so it floats over the body
        // behind a scrim.
        if !r.bin_overlay {
            self.draw_bin(scene, transform, tr, theme, &r);
        }
        self.draw_preview(scene, transform, tr, theme, &r);
        self.draw_transport(scene, transform, tr, theme, &r);
        self.draw_inspector(scene, transform, tr, theme, &r);
        self.draw_timeline(scene, transform, tr, theme, &r);
        if r.bin_overlay {
            // Dimming scrim, then the bin sliding in on top of it.
            if r.scrim.width() > 0.0 {
                let a = self.bin_flap.reveal() * 0.5;
                fill_rect(scene, transform, with_alpha(Color::BLACK, a), r.scrim);
            }
            if self.bin_flap.reveal() > 0.001 {
                self.draw_bin(scene, transform, tr, theme, &r);
            }
        }
        if self.show_export {
            self.draw_export_modal(scene, transform, tr, theme, &r);
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        let r = self.regions();
        match event {
            WindowEvent::KeyboardInput { event: key_event, .. }
                if key_event.state == ElementState::Pressed =>
            {
                if self.handle_key(&key_event.logical_key, modifiers, state, &r) {
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.handle_press(state, &r);
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.drag.is_some() {
                    // Commit the drag's final position / trim / grade as one
                    // action (one undo boundary), then end the drag.
                    self.commit_drag(state);
                    self.drag = None;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                self.update_hover(&r);
                self.apply_drag(&r);
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => *y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                if dy.abs() > 0.0 {
                    self.zoom_by(if dy > 0.0 { 1.12 } else { 1.0 / 1.12 });
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "VideoEditorApp"
    }
    fn test_id(&self) -> Option<&str> {
        Some("video_editor_root")
    }

    /// Publish the open project + its tracks and clips so the host's Ctrl+K
    /// action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the document heads captured before an on-screen affordance
    /// dispatched its timeline action, so it records exactly one undo boundary
    /// bracketing it (Ctrl+Z reverts it OS-wide). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, transform, tr, theme, presence);
    }
}

// ==========================================================================
// Event handling helpers
// ==========================================================================
impl VideoEditorApp {
    fn handle_key(
        &mut self,
        key: &Key,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        r: &Regions,
    ) -> bool {
        let ctrl = modifiers.state().control_key() || modifiers.state().super_key();
        let shift = modifiers.state().shift_key();
        if self.show_export {
            if matches!(key, Key::Named(NamedKey::Escape)) {
                self.show_export = false;
                return true;
            }
            if matches!(key, Key::Named(NamedKey::Enter)) {
                self.export_status = begin_export(&self.project, &self.export);
                return true;
            }
            return false;
        }
        match key {
            Key::Named(NamedKey::Space) => {
                self.toggle_play();
                true
            }
            // Arrows nudge the selected clip a frame (Shift = one second); with
            // nothing selected they step the playhead instead.
            Key::Named(NamedKey::ArrowLeft) => {
                let step = if shift { 1.0 } else { frame_step(self.project.fps) };
                if self.selected.is_some() {
                    self.nudge_selected(state, -step);
                } else {
                    self.step_frame(if shift { -self.project.fps } else { -1.0 });
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let step = if shift { 1.0 } else { frame_step(self.project.fps) };
                if self.selected.is_some() {
                    self.nudge_selected(state, step);
                } else {
                    self.step_frame(if shift { self.project.fps } else { 1.0 });
                }
                true
            }
            Key::Named(NamedKey::Home) => {
                self.playhead = 0.0;
                self.playing = false;
                self.shuttle = 0.0;
                true
            }
            Key::Named(NamedKey::End) => {
                self.playhead = self.project.duration();
                self.playing = false;
                self.shuttle = 0.0;
                true
            }
            // Shift+Delete ripples (closes the gap); plain Delete lifts.
            Key::Named(NamedKey::Delete) | Key::Named(NamedKey::Backspace) => {
                if shift {
                    self.ripple_delete_selected(state);
                } else {
                    self.delete_selected(state);
                }
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("e") => {
                self.show_export = true;
                let _ = r;
                true
            }
            // Ctrl+O opens the host file picker to import real media.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("o") => {
                self.trigger_import();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("c") => {
                self.copy_selected();
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                self.paste_clip(state);
                true
            }
            // J / K / L shuttle: J reverse, K stop, L forward. Repeated J/L
            // doubles the rate, the standard NLE shuttle.
            Key::Character(c) if c.eq_ignore_ascii_case("j") && !ctrl => {
                self.shuttle_key(-1.0);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("k") && !ctrl => {
                self.shuttle = 0.0;
                self.playing = false;
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("l") && !ctrl => {
                self.shuttle_key(1.0);
                true
            }
            // I / O set the selected clip's in / out point to the playhead.
            Key::Character(c) if c.eq_ignore_ascii_case("i") && !ctrl => {
                self.set_in_at_playhead(state);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("o") && !ctrl => {
                self.set_out_at_playhead(state);
                true
            }
            Key::Character(c) if c.eq_ignore_ascii_case("s") && !ctrl => {
                self.split_selected(state);
                true
            }
            // Toggle the media bin flap. Useful on narrow windows where the
            // bin is an overlay; harmless (collapses the docked bin) on wide.
            Key::Character(c) if c.eq_ignore_ascii_case("b") && !ctrl => {
                self.toggle_bin();
                true
            }
            Key::Character(c) if c.as_str() == "+" || c.as_str() == "=" => {
                self.zoom_by(1.25);
                true
            }
            Key::Character(c) if c.as_str() == "-" || c.as_str() == "_" => {
                self.zoom_by(1.0 / 1.25);
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Esc dismisses an open overlay bin first, then clears the
                // selection — matching the flap kernel's Esc-to-close.
                if self.bin_layout() == SplitLayout::Overlay
                    && self.bin_flap.is_open(SplitLayout::Overlay)
                {
                    self.bin_flap.close();
                } else {
                    self.selected = None;
                }
                true
            }
            _ => false,
        }
    }

    fn handle_press(&mut self, state: &mut AutoCommit, r: &Regions) {
        let p = self.last_mouse;

        // Modal owns all clicks.
        if self.show_export {
            self.press_export(state, r, p);
            return;
        }
        // Hamburger toggle (overlay layout only).
        if r.bin_overlay && r.menu_btn.contains(p) {
            self.toggle_bin();
            return;
        }
        // Top bar buttons.
        if r.export_btn.contains(p) {
            self.show_export = true;
            return;
        }
        if r.import_btn.contains(p) {
            self.trigger_import();
            return;
        }
        if r.split_btn.contains(p) {
            self.split_selected(state);
            return;
        }
        // Empty-bin "Import media" affordance.
        if let Some(btn) = self.empty_bin_import_rect(r) {
            if btn.contains(p) {
                self.trigger_import();
                return;
            }
        }
        // While the bin is an open overlay, a click on the scrim (anywhere
        // outside the flap and the topbar toggle) dismisses it — matching the
        // flap kernel's scrim/Esc behaviour.
        let bin_visible = !r.bin_overlay || self.bin_flap.is_open(self.bin_layout());
        if r.bin_overlay && bin_visible && !r.bin.contains(p) && !r.menu_btn.contains(p) {
            self.bin_flap.close();
            return;
        }
        // Media bin (only when it's on-screen: docked, or overlay open).
        if bin_visible {
            for (mid, rect) in self.bin_item_rects(r) {
                if rect.contains(p) {
                    self.selected_media = Some(mid);
                    let dbl = matches!(self.last_click_media, Some((m, t)) if m == mid && t.elapsed().as_millis() < 400);
                    if dbl {
                        self.add_media_to_timeline(state, mid);
                        self.last_click_media = None;
                    } else {
                        self.last_click_media = Some((mid, Instant::now()));
                    }
                    return;
                }
            }
        }
        // Inspector controls.
        if r.inspector.contains(p) {
            self.press_inspector(state, r, p);
            return;
        }
        // Transport controls.
        for (action, rect) in self.transport_buttons(r) {
            if rect.contains(p) {
                self.transport_action(action);
                return;
            }
        }
        let scrub = self.scrubber_rect(r);
        if scrub.contains(p) || (p.x >= scrub.x0 && p.x <= scrub.x1 && (p.y - scrub.center().y).abs() < 14.0) {
            self.drag = Some(Drag::Scrubber);
            self.apply_scrubber(p, r);
            return;
        }
        // Ruler scrub.
        if r.ruler.contains(p) {
            self.drag = Some(Drag::Playhead);
            self.playhead = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps)
                .clamp(0.0, self.project.duration());
            return;
        }
        // Timeline lanes — clips, trim handles, empty-space scrub.
        if r.lanes.contains(p) {
            self.press_timeline(r, p);
        }
    }

    fn press_timeline(&mut self, r: &Regions, p: Point) {
        for (tid, _kind, lane) in self.lane_rects(r) {
            if p.y < lane.y0 || p.y > lane.y1 {
                continue;
            }
            // Search clips on this lane.
            let clip_ids: Vec<ClipId> = self.project.clips_on(tid).iter().map(|c| c.id).collect();
            for cid in clip_ids {
                let Some(clip) = self.project.clip(cid).cloned() else { continue };
                let cr = self.clip_rect(&clip, &lane, r);
                if p.x < cr.x0 - 2.0 || p.x > cr.x1 + 2.0 {
                    continue;
                }
                self.selected = Some(cid);
                if (p.x - cr.x0).abs() <= TRIM_HIT {
                    self.drag = Some(Drag::TrimIn { id: cid });
                } else if (p.x - cr.x1).abs() <= TRIM_HIT {
                    self.drag = Some(Drag::TrimOut { id: cid });
                } else {
                    self.drag = Some(Drag::ClipMove { id: cid, grab_dt: self.x_to_time(p.x, &r.lanes) - clip.start });
                }
                return;
            }
        }
        // Empty lane area: scrub the playhead.
        self.drag = Some(Drag::Playhead);
        self.playhead = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps)
            .clamp(0.0, self.project.duration());
    }

    fn press_inspector(&mut self, state: &mut AutoCommit, r: &Regions, p: Point) {
        let l = self.inspector_layout(r);
        if !l.valid {
            return;
        }
        // The slider controls start a drag and preview the click position in
        // memory; the final value is dispatched as one action on mouse-up
        // (see `commit_drag`), so a slider interaction is one undo step.
        if let Some(track) = l.opacity {
            if hit_slider(track, p) {
                self.drag = Some(Drag::Opacity);
                let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.preview_edit(|c| c.opacity = v);
                return;
            }
        }
        if let Some(track) = l.volume {
            if hit_slider(track, p) {
                self.drag = Some(Drag::Volume);
                let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.preview_edit(|c| c.volume = v);
                return;
            }
        }
        // Colour-grade sliders.
        for (track, field, drag) in [
            (l.brightness, FilterField::Brightness, Drag::Brightness),
            (l.contrast, FilterField::Contrast, Drag::Contrast),
            (l.saturation, FilterField::Saturation, Drag::Saturation),
        ] {
            if let Some(track) = track {
                if hit_slider(track, p) {
                    self.drag = Some(drag);
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_edit(move |c| match field {
                        FilterField::Brightness => c.filters.brightness = v,
                        FilterField::Contrast => c.filters.contrast = v,
                        FilterField::Saturation => c.filters.saturation = v,
                    });
                    return;
                }
            }
        }
        // Fade in / out sliders (value in seconds, 0..clip-duration).
        let max = self.selected.and_then(|id| self.project.clip(id)).map(|c| c.duration()).unwrap_or(0.0);
        if let Some(track) = l.fade_in {
            if hit_slider(track, p) {
                self.drag = Some(Drag::FadeIn);
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.preview_edit(move |c| c.fade_in = (frac * max).clamp(0.0, c.duration()));
                return;
            }
        }
        if let Some(track) = l.fade_out {
            if hit_slider(track, p) {
                self.drag = Some(Drag::FadeOut);
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.preview_edit(move |c| c.fade_out = (frac * max).clamp(0.0, c.duration()));
                return;
            }
        }
        for (t, rect) in &l.transitions {
            if rect.contains(p) {
                self.set_transition(state, *t);
                return;
            }
        }
        for (pal, rect) in &l.colors {
            if rect.contains(p) {
                self.set_palette(state, *pal);
                return;
            }
        }
        if l.split.contains(p) {
            self.split_selected(state);
            return;
        }
        if l.delete.contains(p) {
            self.delete_selected(state);
        }
    }

    fn press_export(&mut self, state: &mut AutoCommit, r: &Regions, p: Point) {
        let l = self.export_layout(r);
        if !l.card.contains(p) {
            // Click outside the card dismisses.
            self.show_export = false;
            return;
        }
        for (res, rect) in &l.resolution {
            if rect.contains(p) {
                self.export.resolution = *res;
                return;
            }
        }
        for (c, rect) in &l.container {
            if rect.contains(p) {
                self.export.container = *c;
                return;
            }
        }
        for (f, rect) in &l.fps {
            if rect.contains(p) {
                self.export.frame_rate = *f;
                return;
            }
        }
        if l.cancel.contains(p) {
            self.show_export = false;
            return;
        }
        if l.confirm.contains(p) {
            self.export_status = begin_export(&self.project, &self.export);
            let _ = state;
        }
    }

    fn transport_action(&mut self, action: TransportAction) {
        match action {
            TransportAction::ToStart => {
                self.playhead = 0.0;
                self.playing = false;
            }
            TransportAction::StepBack => self.step_frame(-1.0),
            TransportAction::PlayPause => self.toggle_play(),
            TransportAction::StepFwd => self.step_frame(1.0),
            TransportAction::ToEnd => {
                self.playhead = self.project.duration();
                self.playing = false;
            }
        }
    }

    /// Track the live drag, updating **only the in-memory project** for a
    /// smooth preview. Nothing is written to the CRDT here — the clip's final
    /// position / trim / grade is dispatched as one action when the drag ends
    /// (see [`Self::commit_drag`]), so a drag is one undo step, not sixty a
    /// second of doc churn. The playhead / scrubber drags are pure presentation.
    fn apply_drag(&mut self, r: &Regions) {
        let Some(drag) = self.drag else { return };
        let p = self.last_mouse;
        match drag {
            Drag::Playhead => {
                self.playhead = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps)
                    .clamp(0.0, self.project.duration());
            }
            Drag::Scrubber => self.apply_scrubber(p, r),
            Drag::ClipMove { id, grab_dt } => {
                let raw = (self.x_to_time(p.x, &r.lanes) - grab_dt).max(0.0);
                // Snap the clip's left edge to a neighbouring edge / playhead
                // when within a few pixels, then to the frame grid.
                let snapped = self.snap_time(raw, id);
                let new_start = snap_to_frame(snapped, self.project.fps);
                self.project.move_clip(id, new_start);
            }
            Drag::TrimIn { id } => {
                let raw = self.x_to_time(p.x, &r.lanes);
                let target = snap_to_frame(self.snap_time(raw, id), self.project.fps);
                let delta = self.project.clip(id).map(|c| target - c.start).unwrap_or(0.0);
                self.project.trim_in(id, delta);
            }
            Drag::TrimOut { id } => {
                let raw = self.x_to_time(p.x, &r.lanes);
                let target = snap_to_frame(self.snap_time(raw, id), self.project.fps);
                let delta = self.project.clip(id).map(|c| target - c.end()).unwrap_or(0.0);
                self.project.trim_out(id, delta);
            }
            Drag::Opacity => {
                if let Some(track) = self.inspector_layout(r).opacity {
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_edit(|c| c.opacity = v);
                }
            }
            Drag::Volume => {
                if let Some(track) = self.inspector_layout(r).volume {
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_edit(|c| c.volume = v);
                }
            }
            Drag::Brightness | Drag::Contrast | Drag::Saturation => {
                let (track, field) = match drag {
                    Drag::Brightness => (self.inspector_layout(r).brightness, FilterField::Brightness),
                    Drag::Contrast => (self.inspector_layout(r).contrast, FilterField::Contrast),
                    _ => (self.inspector_layout(r).saturation, FilterField::Saturation),
                };
                if let Some(track) = track {
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_edit(move |c| match field {
                        FilterField::Brightness => c.filters.brightness = v,
                        FilterField::Contrast => c.filters.contrast = v,
                        FilterField::Saturation => c.filters.saturation = v,
                    });
                }
            }
            Drag::FadeIn => {
                if let Some(track) = self.inspector_layout(r).fade_in {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self.selected.and_then(|id| self.project.clip(id)).map(|c| c.duration()).unwrap_or(0.0);
                    self.preview_edit(move |c| c.fade_in = (frac * max).clamp(0.0, c.duration()));
                }
            }
            Drag::FadeOut => {
                if let Some(track) = self.inspector_layout(r).fade_out {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self.selected.and_then(|id| self.project.clip(id)).map(|c| c.duration()).unwrap_or(0.0);
                    self.preview_edit(move |c| c.fade_out = (frac * max).clamp(0.0, c.duration()));
                }
            }
        }
    }

    /// Commit the edit a drag just made: dispatch one action carrying the final
    /// shape of the clip the drag touched. Called on mouse-up, before the drag
    /// is cleared. Playhead / scrubber drags are presentation and commit
    /// nothing.
    fn commit_drag(&mut self, doc: &mut AutoCommit) {
        let Some(drag) = self.drag else { return };
        match drag {
            Drag::Playhead | Drag::Scrubber => {}
            Drag::ClipMove { id, .. } | Drag::TrimIn { id } | Drag::TrimOut { id } => {
                self.dispatch_clip(doc, id);
            }
            Drag::Opacity
            | Drag::Volume
            | Drag::Brightness
            | Drag::Contrast
            | Drag::Saturation
            | Drag::FadeIn
            | Drag::FadeOut => {
                if let Some(id) = self.selected {
                    self.dispatch_clip(doc, id);
                }
            }
        }
    }

    /// True while a drag that edits a clip is in flight, so [`Widget::sync`]
    /// leaves the live in-memory preview alone instead of reloading it from the
    /// (not-yet-written) document mid-drag.
    fn drag_is_editing(&self) -> bool {
        matches!(
            self.drag,
            Some(
                Drag::ClipMove { .. }
                    | Drag::TrimIn { .. }
                    | Drag::TrimOut { .. }
                    | Drag::Opacity
                    | Drag::Volume
                    | Drag::Brightness
                    | Drag::Contrast
                    | Drag::Saturation
                    | Drag::FadeIn
                    | Drag::FadeOut
            )
        )
    }

    /// Magnetic snap for a dragged clip edge at raw time `t`: snaps to the
    /// nearest neighbouring clip edge / playhead / origin within a small
    /// pixel tolerance (converted to seconds via the current zoom), so clips
    /// butt up cleanly. Excludes the dragged clip's own edges.
    fn snap_time(&self, t: f64, dragged: ClipId) -> f64 {
        let tol = 8.0 / self.zoom.max(0.001); // 8px in seconds
        let cands = self.project.snap_candidates(dragged, self.playhead);
        Project::snap_to_candidates(t, &cands, tol)
    }

    fn apply_scrubber(&mut self, p: Point, r: &Regions) {
        let scrub = self.scrubber_rect(r);
        let frac = ((p.x - scrub.x0) / scrub.width()).clamp(0.0, 1.0);
        self.playhead = (frac * self.project.duration()).clamp(0.0, self.project.duration());
        self.playing = false;
    }

    fn update_hover(&mut self, r: &Regions) {
        let p = self.last_mouse;
        let bin_visible = !r.bin_overlay || self.bin_flap.is_open(self.bin_layout());
        self.hover_media = if bin_visible {
            self.bin_item_rects(r).into_iter().find(|(_, rect)| rect.contains(p)).map(|(m, _)| m)
        } else {
            None
        };
        self.hover_clip = None;
        if r.lanes.contains(p) {
            for (tid, _k, lane) in self.lane_rects(r) {
                if p.y < lane.y0 || p.y > lane.y1 {
                    continue;
                }
                for c in self.project.clips_on(tid) {
                    let cr = self.clip_rect(c, &lane, r);
                    if p.x >= cr.x0 && p.x <= cr.x1 {
                        self.hover_clip = Some(c.id);
                    }
                }
            }
        }
    }
}

// ==========================================================================
// Drawing helpers
// ==========================================================================
impl VideoEditorApp {
    fn draw_chrome(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, self.rect);
        // Top bar.
        fill_rect(scene, tf, c.surfaces.raised, r.topbar);
        stroke_line(scene, tf, c.border_standard, r.topbar.x0, r.topbar.y1, r.topbar.x1, r.topbar.y1, 1.0);
        // Leading slot: a hamburger toggle when the bin is collapsed to an
        // overlay flap. The OS owns the window title/app-name/logo, so the app
        // draws no glyph or wordmark here.
        if r.bin_overlay {
            self.draw_hamburger(scene, tf, theme, r.menu_btn);
        }
        // Project meta, centered-ish.
        let meta = format!(
            "{}   ·   {}×{}   ·   {:.0} fps   ·   {}",
            self.project.name,
            self.project.width,
            self.project.height,
            self.project.fps,
            timecode(self.project.duration(), self.project.fps),
        );
        let (mw, _) = tr.measure(&meta, 13.0, None);
        text(scene, tr, tf, &meta, 13.0, c.text_dim, (self.rect.x0 + self.rect.x1) / 2.0 - mw as f64 / 2.0, r.topbar.y0 + 17.0);
        // Buttons.
        draw_button(scene, tf, tr, theme, r.import_btn, "Import…", false);
        draw_button(scene, tf, tr, theme, r.split_btn, "Split  (S)", false);
        draw_button(scene, tf, tr, theme, r.export_btn, "Export…", true);
    }

    /// Three-bar hamburger that summons the collapsed media bin. Highlights
    /// while the flap is open so it reads as a pressed toggle.
    fn draw_hamburger(&self, scene: &mut Scene, tf: Affine, theme: &Theme, rect: Rect) {
        if rect.width() <= 0.0 {
            return;
        }
        let c = &theme.tokens.colors;
        let open = self.bin_flap.is_open(self.bin_layout());
        let hovered = rect.contains(self.last_mouse);
        let bg = if open {
            c.accent[5]
        } else if hovered {
            c.surface_white_10
        } else {
            c.neutral[3]
        };
        fill_rrect(scene, tf, bg, rect, 7.0);
        let bar = if open { Color::WHITE } else { c.text_primary };
        let bx0 = rect.x0 + 7.0;
        let bx1 = rect.x1 - 7.0;
        for i in 0..3 {
            let by = rect.center().y - 5.0 + i as f64 * 5.0;
            fill_rrect(scene, tf, bar, Rect::new(bx0, by - 1.0, bx1, by + 1.0), 1.0);
        }
    }

    fn draw_bin(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.raised, r.bin);
        stroke_line(scene, tf, c.border_standard, r.bin.x1, r.bin.y0, r.bin.x1, r.bin.y1, 1.0);
        text_w(scene, tr, tf, "MEDIA BIN", 11.0, c.text_dim, r.bin.x0 + 14.0, r.bin.y0 + 14.0, TextWeight::Semibold);
        let count = format!("{} clips", self.project.media.len());
        let (cw, _) = tr.measure(&count, 11.0, None);
        text(scene, tr, tf, &count, 11.0, c.text_dim, r.bin.x1 - 14.0 - cw as f64, r.bin.y0 + 14.0);

        // Clip bin content so a long list can't bleed over the timeline.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, tf, &Rect::new(r.bin.x0, r.bin.y0 + 30.0, r.bin.x1, r.bin.y1));
        if self.project.media.is_empty() {
            // Empty state: a real "Import media" affordance (the button opens the
            // host file picker) plus an honest status line.
            let cx = r.bin.center().x;
            let cy = r.bin.y0 + 120.0;
            let line1 = "No media yet";
            let (w1, _) = tr.measure(line1, 14.0, None);
            text(scene, tr, tf, line1, 14.0, c.text_primary, cx - w1 as f64 / 2.0, cy);
            if let Some(btn) = self.empty_bin_import_rect(r) {
                draw_button(scene, tf, tr, theme, btn, "Import media", true);
            }
            // Show the last import status (why the picker is unavailable on a
            // hermetic build, or how many clips landed) — never a fake success.
            if let Some(notice) = &self.import_notice {
                let (nw, _) = tr.measure(notice, 10.5, None);
                text(scene, tr, tf, notice, 10.5, c.text_dim, cx - nw as f64 / 2.0, r.bin.y0 + 200.0);
            }
        }
        for (mid, rect) in self.bin_item_rects(r) {
            let Some(m) = self.project.media(mid) else { continue };
            let selected = self.selected_media == Some(mid);
            let hover = self.hover_media == Some(mid);
            if selected {
                fill_rrect(scene, tf, with_alpha(c.accent[5], 0.22), rect, 8.0);
                stroke_rrect(scene, tf, c.accent[5], rect, 8.0, 1.5);
            } else if hover {
                fill_rrect(scene, tf, c.surface_white_5, rect, 8.0);
            }
            // Thumbnail swatch.
            let thumb = Rect::new(rect.x0 + 8.0, rect.y0 + 8.0, rect.x0 + 8.0 + 64.0, rect.y1 - 8.0);
            let base = clip_color(theme, m.palette);
            fill_rrect(scene, tf, darken(base, 0.25), thumb, 5.0);
            // mini content: filmstrip for visual, bars for audio.
            if m.kind.is_visual() {
                for i in 0..3 {
                    let bx = thumb.x0 + 6.0 + i as f64 * 18.0;
                    let block = Rect::new(bx, thumb.y0 + 6.0, bx + 14.0, thumb.y1 - 6.0);
                    fill_rrect(scene, tf, lighten(base, 0.18), block, 2.0);
                }
            } else {
                let midy = thumb.center().y;
                for i in 0..9 {
                    let bx = thumb.x0 + 6.0 + i as f64 * 6.4;
                    let amp = waveform_amp(i as f64) * (thumb.height() / 2.0 - 6.0);
                    let bar = Rect::new(bx, midy - amp, bx + 3.0, midy + amp);
                    fill_rect(scene, tf, lighten(base, 0.25), bar);
                }
            }
            // kind dot.
            let kc = match m.kind {
                MediaKind::Video => c.semantic.info,
                MediaKind::Audio => c.semantic.success,
                MediaKind::Image => c.semantic.warning,
            };
            scene.fill(Fill::NonZero, tf, kc, None, &Circle::new(Point::new(thumb.x1 - 8.0, thumb.y0 + 8.0), 4.0));
            // text.
            let tx = thumb.x1 + 12.0;
            text_w(scene, tr, tf, &elide(&m.name, 22), 13.0, c.text_primary, tx, rect.y0 + 10.0, TextWeight::Medium);
            let sub = match m.kind {
                MediaKind::Audio => format!("Audio · {}", clock(m.duration)),
                _ => format!("{}p · {}", m.height, clock(m.duration)),
            };
            text(scene, tr, tf, &sub, 11.0, c.text_dim, tx, rect.y0 + 30.0);
        }
        scene.pop_layer();
    }

    fn draw_preview(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, r.preview);
        // Canvas backdrop (letterbox black).
        let canvas = r.canvas;
        fill_rect(scene, tf, Color::BLACK, canvas);
        // The composited frame is computed once, here, by the preview module —
        // the same colour-grade + fade math the (gated) decode path and export
        // use. The representative frame is a graded gradient; with --features
        // decode and hydrated bytes the canvas instead blits a real frame.
        let frame = preview::composite(&self.project, self.playhead);
        // Did the real decoder land a frame for the clip under the playhead?
        // Only true under `--features decode` with hydrated bytes.
        #[allow(unused_mut)]
        let mut showed_real = false;
        match frame {
            CompositedFrame::Clip { media, base, envelope, flash, decoded } => {
                let _ = decoded;
                // Blit the real decoded frame when the decode pipeline has one
                // for this clip; otherwise the representative two-band gradient.
                #[cfg(feature = "decode")]
                {
                    if self.preview_decoder.frame_media() == Some(media) {
                        if let Some(f) = self.preview_decoder.frame() {
                            blit_frame(scene, tf, f, canvas);
                            showed_real = true;
                        }
                    }
                }
                if !showed_real {
                    // `base` is already graded + faded; build a representative
                    // two-band gradient from it so grading is visibly reflected.
                    let col = rgb_to_color(base);
                    fill_rect(scene, tf, darken(col, 0.18), canvas);
                    let upper = Rect::new(canvas.x0, canvas.y0, canvas.x1, canvas.center().y);
                    fill_rect(scene, tf, with_alpha(lighten(col, 0.18), 0.55), upper);
                    let band = Rect::new(canvas.x0, canvas.y0 + canvas.height() * 0.62, canvas.x1, canvas.y0 + canvas.height() * 0.72);
                    fill_rect(scene, tf, with_alpha(darken(col, 0.15), 0.6), band);
                    stroke_rrect(scene, tf, with_alpha(Color::BLACK, 0.5), canvas.inset(2.0), 2.0, 18.0);
                    if let Some(m) = self.project.media(media) {
                        let (nw, _) = tr.measure_weighted(&m.name, 22.0, None, TextWeight::Semibold);
                        text_w(scene, tr, tf, &m.name, 22.0, with_alpha(Color::WHITE, 0.92), canvas.center().x - nw as f64 / 2.0, canvas.center().y - 16.0, TextWeight::Semibold);
                    }
                }
                // The fade/opacity envelope dims the whole frame toward black.
                if envelope < 0.999 {
                    fill_rect(scene, tf, with_alpha(Color::BLACK, 1.0 - envelope), canvas);
                }
                // Transition flash on the clip head.
                if let Some(f) = flash {
                    let col = if f.white { Color::WHITE } else { Color::BLACK };
                    fill_rect(scene, tf, with_alpha(col, f.alpha), canvas);
                }
            }
            CompositedFrame::Empty => {
                let (tw, _) = tr.measure("No clip at playhead", 14.0, None);
                text(scene, tr, tf, "No clip at playhead", 14.0, c.text_dim, canvas.center().x - tw as f64 / 2.0, canvas.center().y - 8.0);
            }
        }
        // HUD overlays.
        text_w(scene, tr, tf, &timecode(self.playhead, self.project.fps), 13.0, with_alpha(Color::WHITE, 0.85), canvas.x0 + 10.0, canvas.y0 + 8.0, TextWeight::Bold);
        let res = format!("{}×{}", self.project.width, self.project.height);
        let (rw, _) = tr.measure(&res, 12.0, None);
        text(scene, tr, tf, &res, 12.0, with_alpha(Color::WHITE, 0.7), canvas.x1 - 10.0 - rw as f64, canvas.y0 + 8.0);
        // Decode-mode badge so the gated-vs-real state is legible in a shot:
        // LIVE when a real decoded frame is on the canvas, DECODE when the
        // pipeline is compiled but has no hydrated frame, PREVIEW otherwise.
        let badge = if showed_real {
            "LIVE"
        } else if cfg!(feature = "decode") {
            "DECODE"
        } else {
            "PREVIEW"
        };
        let badge_col = if showed_real { c.semantic.success } else { with_alpha(Color::WHITE, 0.45) };
        text(scene, tr, tf, badge, 9.5, badge_col, canvas.x0 + 10.0, canvas.y1 - 18.0);
        // Safe-area guide.
        stroke_rrect(scene, tf, with_alpha(Color::WHITE, 0.12), canvas.inset(-canvas.width() * 0.05), 0.0, 1.0);
    }

    fn draw_transport(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.raised, r.transport);
        stroke_line(scene, tf, c.border_standard, r.transport.x0, r.transport.y0, r.transport.x1, r.transport.y0, 1.0);
        for (action, rect) in self.transport_buttons(r) {
            let is_play = action == TransportAction::PlayPause;
            let hovered = rect.contains(self.last_mouse);
            let bg = if is_play { c.accent[5] } else if hovered { c.surface_white_10 } else { c.neutral[3] };
            scene.fill(Fill::NonZero, tf, bg, None, &Circle::new(rect.center(), rect.width() / 2.0));
            let col = if is_play { Color::WHITE } else { c.text_primary };
            self.draw_transport_icon(scene, tf, action, rect.center(), col);
        }
        // Timecode readout.
        let big = timecode(self.playhead, self.project.fps);
        text_w(scene, tr, tf, &big, 18.0, c.text_primary, r.transport.x0 + 220.0, r.transport.y0 + 8.0, TextWeight::Bold);
        let total = format!("/  {}", timecode(self.project.duration(), self.project.fps));
        let (bw, _) = tr.measure_weighted(&big, 18.0, None, TextWeight::Bold);
        text(scene, tr, tf, &total, 13.0, c.text_dim, r.transport.x0 + 224.0 + bw as f64, r.transport.y0 + 13.0);
        // Scrubber.
        let scrub = self.scrubber_rect(r);
        fill_rrect(scene, tf, c.neutral[3], scrub, 4.0);
        let frac = (self.playhead / self.project.duration()).clamp(0.0, 1.0);
        let filled = Rect::new(scrub.x0, scrub.y0, scrub.x0 + scrub.width() * frac, scrub.y1);
        fill_rrect(scene, tf, c.accent[5], filled, 4.0);
        scene.fill(Fill::NonZero, tf, Color::WHITE, None, &Circle::new(Point::new(filled.x1, scrub.center().y), 6.0));
        if self.playing {
            text(scene, tr, tf, "PLAYING", 10.0, c.semantic.success, scrub.x0, scrub.y0 - 16.0);
        }
    }

    fn draw_transport_icon(&self, scene: &mut Scene, tf: Affine, action: TransportAction, ctr: Point, col: Color) {
        let s = 6.0;
        match action {
            TransportAction::PlayPause => {
                if self.playing {
                    fill_rect(scene, tf, col, Rect::new(ctr.x - 5.0, ctr.y - 6.0, ctr.x - 1.0, ctr.y + 6.0));
                    fill_rect(scene, tf, col, Rect::new(ctr.x + 1.0, ctr.y - 6.0, ctr.x + 5.0, ctr.y + 6.0));
                } else {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x - 5.0, ctr.y - 7.0));
                    p.line_to((ctr.x + 7.0, ctr.y));
                    p.line_to((ctr.x - 5.0, ctr.y + 7.0));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::StepBack => {
                let mut p = BezPath::new();
                p.move_to((ctr.x + s, ctr.y - s));
                p.line_to((ctr.x - 2.0, ctr.y));
                p.line_to((ctr.x + s, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
                fill_rect(scene, tf, col, Rect::new(ctr.x - 6.0, ctr.y - s, ctr.x - 4.0, ctr.y + s));
            }
            TransportAction::StepFwd => {
                let mut p = BezPath::new();
                p.move_to((ctr.x - s, ctr.y - s));
                p.line_to((ctr.x + 2.0, ctr.y));
                p.line_to((ctr.x - s, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
                fill_rect(scene, tf, col, Rect::new(ctr.x + 4.0, ctr.y - s, ctr.x + 6.0, ctr.y + s));
            }
            TransportAction::ToStart => {
                fill_rect(scene, tf, col, Rect::new(ctr.x - 7.0, ctr.y - s, ctr.x - 5.0, ctr.y + s));
                let mut p = BezPath::new();
                p.move_to((ctr.x + 6.0, ctr.y - s));
                p.line_to((ctr.x - 3.0, ctr.y));
                p.line_to((ctr.x + 6.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
            TransportAction::ToEnd => {
                fill_rect(scene, tf, col, Rect::new(ctr.x + 5.0, ctr.y - s, ctr.x + 7.0, ctr.y + s));
                let mut p = BezPath::new();
                p.move_to((ctr.x - 6.0, ctr.y - s));
                p.line_to((ctr.x + 3.0, ctr.y));
                p.line_to((ctr.x - 6.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
        }
    }

    fn draw_inspector(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.raised, r.inspector);
        stroke_line(scene, tf, c.border_standard, r.inspector.x0, r.inspector.y0, r.inspector.x0, r.inspector.y1, 1.0);
        // Clip everything to the inspector rect so controls never bleed into
        // the timeline if the selected clip's control stack runs tall.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, tf, &r.inspector);
        self.draw_inspector_body(scene, tf, tr, theme, r);
        scene.pop_layer();
    }

    fn draw_inspector_body(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        let x0 = r.inspector.x0 + PAD;
        let x1 = r.inspector.x1 - PAD;
        text_w(scene, tr, tf, "INSPECTOR", 11.0, c.text_dim, x0, r.inspector.y0 + 14.0, TextWeight::Semibold);

        let Some(id) = self.selected else {
            text(scene, tr, tf, "No clip selected", 13.0, c.text_dim, x0, r.inspector.y0 + 60.0);
            text(scene, tr, tf, "Click a clip in the timeline", 11.5, c.text_dim, x0, r.inspector.y0 + 82.0);
            return;
        };
        let Some(clip) = self.project.clip(id).cloned() else { return };
        let media = self.project.media(clip.media).cloned();
        let is_video = media.as_ref().map(|m| m.kind.is_visual()).unwrap_or(true);
        let track_name = self.project.track(clip.track).map(|t| t.name.clone()).unwrap_or_default();

        // Header card with color tag + name.
        let tag = Rect::new(x0, r.inspector.y0 + 36.0, x0 + 6.0, r.inspector.y0 + 64.0);
        fill_rrect(scene, tf, clip_color(theme, clip.palette), tag, 3.0);
        let name = media.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "clip".into());
        text_w(scene, tr, tf, &elide(&name, 26), 15.0, c.text_primary, x0 + 14.0, r.inspector.y0 + 38.0, TextWeight::Semibold);
        text(scene, tr, tf, &format!("on {track_name}"), 11.5, c.text_dim, x0 + 14.0, r.inspector.y0 + 58.0);

        // Property rows.
        let mut y = r.inspector.y0 + 78.0;
        let fps = self.project.fps;
        let prop = |scene: &mut Scene, tr: &mut TextRenderer, label: &str, value: &str, y: f64| {
            text(scene, tr, tf, label, 12.0, c.text_dim, x0, y);
            let (vw, _) = tr.measure_weighted(value, 12.0, None, TextWeight::Medium);
            text_w(scene, tr, tf, value, 12.0, c.text_primary, x1 - vw as f64, y, TextWeight::Medium);
        };
        prop(scene, tr, "Position", &timecode(clip.start, fps), y); y += 18.0;
        prop(scene, tr, "Duration", &timecode(clip.duration(), fps), y); y += 18.0;
        prop(scene, tr, "Source In", &timecode(clip.in_point, fps), y); y += 18.0;
        prop(scene, tr, "Source Out", &timecode(clip.out_point, fps), y);

        let l = self.inspector_layout(r);
        // Opacity / Volume slider.
        if let Some(track) = l.opacity {
            text(scene, tr, tf, "Opacity", 12.0, c.text_dim, x0, track.y0 - 20.0);
            let pct = format!("{:.0}%", clip.opacity * 100.0);
            let (pw, _) = tr.measure(&pct, 12.0, None);
            text(scene, tr, tf, &pct, 12.0, c.text_primary, x1 - pw as f64, track.y0 - 20.0);
            draw_track(scene, tf, theme, track, clip.opacity);
        }
        if let Some(track) = l.volume {
            text(scene, tr, tf, "Volume", 12.0, c.text_dim, x0, track.y0 - 20.0);
            let pct = format!("{:.0}%", clip.volume * 100.0);
            let (pw, _) = tr.measure(&pct, 12.0, None);
            text(scene, tr, tf, &pct, 12.0, c.text_primary, x1 - pw as f64, track.y0 - 20.0);
            draw_track(scene, tf, theme, track, clip.volume);
        }
        // Colour-grade sliders (video only). The right-hand readout shows
        // the multiplier (0.5 slider == 1.0×, the identity).
        let grade = |scene: &mut Scene, tr: &mut TextRenderer, track: Option<Rect>, label: &str, v: f64| {
            if let Some(track) = track {
                text(scene, tr, tf, label, 12.0, c.text_dim, x0, track.y0 - 20.0);
                let mult = format!("{:.2}×", Filters::gain(v));
                let (pw, _) = tr.measure(&mult, 12.0, None);
                text(scene, tr, tf, &mult, 12.0, c.text_primary, x1 - pw as f64, track.y0 - 20.0);
                draw_track(scene, tf, theme, track, v);
            }
        };
        grade(scene, tr, l.brightness, "Brightness", clip.filters.brightness);
        grade(scene, tr, l.contrast, "Contrast", clip.filters.contrast);
        grade(scene, tr, l.saturation, "Saturation", clip.filters.saturation);
        // Fade in / out (seconds). Two half-width tracks share a row.
        let cd = clip.duration().max(1e-6);
        if let Some(track) = l.fade_in {
            text(scene, tr, tf, "Fade In", 11.0, c.text_dim, track.x0, track.y0 - 20.0);
            let v = format!("{:.1}s", clip.fade_in);
            text(scene, tr, tf, &v, 11.0, c.text_primary, track.x1 - 30.0, track.y0 - 20.0);
            draw_track(scene, tf, theme, track, (clip.fade_in / cd).clamp(0.0, 1.0));
        }
        if let Some(track) = l.fade_out {
            text(scene, tr, tf, "Fade Out", 11.0, c.text_dim, track.x0, track.y0 - 20.0);
            let v = format!("{:.1}s", clip.fade_out);
            text(scene, tr, tf, &v, 11.0, c.text_primary, track.x1 - 30.0, track.y0 - 20.0);
            draw_track(scene, tf, theme, track, (clip.fade_out / cd).clamp(0.0, 1.0));
        }
        // Transition segmented control.
        if !l.transitions.is_empty() {
            text(scene, tr, tf, "Transition", 12.0, c.text_dim, x0, l.transition_y - 20.0);
            for (t, rect) in &l.transitions {
                let on = clip.transition == *t;
                let bg = if on { c.accent[5] } else { c.neutral[3] };
                fill_rrect(scene, tf, bg, *rect, 5.0);
                let col = if on { Color::WHITE } else { c.text_dim };
                let lab = t.label();
                let (lw, _) = tr.measure(lab, 10.0, None);
                text(scene, tr, tf, lab, 10.0, col, rect.center().x - lw as f64 / 2.0, rect.center().y - 6.0);
            }
        }
        // Color swatches.
        if !l.colors.is_empty() {
            text(scene, tr, tf, "Label", 12.0, c.text_dim, x0, l.colors_y - 20.0);
            for (pal, rect) in &l.colors {
                fill_rrect(scene, tf, clip_color(theme, *pal), *rect, 6.0);
                if clip.palette == *pal {
                    stroke_rrect(scene, tf, Color::WHITE, *rect, 6.0, 2.0);
                }
            }
        }
        if l.valid {
            draw_button(scene, tf, tr, theme, l.split, "Split", false);
            draw_button(scene, tf, tr, theme, l.delete, "Delete", false);
        }
        if !is_video {
            // a small note for audio clips
            text(scene, tr, tf, "Audio clip", 10.5, c.text_dim, x0, r.inspector.y1 - 22.0);
        }
    }

    fn draw_timeline(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, r.timeline);
        stroke_line(scene, tf, c.border_standard, r.timeline.x0, r.timeline.y0, r.timeline.x1, r.timeline.y0, 1.0);

        // --- ruler ticks ---
        fill_rect(scene, tf, c.surfaces.raised, r.ruler);
        let tick_step = nice_tick(self.zoom);
        let start_t = self.scroll.max(0.0);
        let end_t = self.x_to_time(r.lanes.x1, &r.lanes);
        let mut t = (start_t / tick_step).floor() * tick_step;
        while t <= end_t {
            let x = self.time_to_x(t, &r.lanes);
            if x >= r.ruler.x0 - 1.0 && x <= r.ruler.x1 {
                stroke_line(scene, tf, c.border_standard, x, r.ruler.y1 - 8.0, x, r.ruler.y1, 1.0);
                let label = timecode(t, self.project.fps);
                text(scene, tr, tf, &label, 10.0, c.text_dim, x + 4.0, r.ruler.y0 + 6.0);
                // faint grid line down the lanes
                stroke_line(scene, tf, with_alpha(c.border_standard, 0.4), x, r.lanes.y0, x, r.timeline.y1, 1.0);
            }
            t += tick_step;
        }

        // --- lanes + headers + clips ---
        let lanes = self.lane_rects(r);
        for (tid, kind, lane) in &lanes {
            // lane background stripe
            let stripe = if *kind == TrackKind::Video { c.surface_white_5 } else { with_alpha(c.semantic.success, 0.05) };
            fill_rect(scene, tf, stripe, Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1));
            // header
            let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
            fill_rect(scene, tf, c.surfaces.raised, header);
            let track = self.project.track(*tid);
            let name = track.map(|t| t.name.as_str()).unwrap_or("?");
            text_w(scene, tr, tf, name, 13.0, c.text_primary, header.x0 + 12.0, header.center().y - 8.0, TextWeight::Semibold);
            let knd = if *kind == TrackKind::Video { "VIDEO" } else { "AUDIO" };
            text(scene, tr, tf, knd, 9.0, c.text_dim, header.x0 + 12.0, header.center().y + 6.0);
            // mute pill
            let muted = track.map(|t| t.muted).unwrap_or(false);
            let pill = Rect::new(header.x1 - 26.0, header.center().y - 9.0, header.x1 - 8.0, header.center().y + 9.0);
            fill_rrect(scene, tf, if muted { c.semantic.danger } else { c.neutral[3] }, pill, 4.0);
            let ml = if *kind == TrackKind::Video { "H" } else { "M" };
            text(scene, tr, tf, ml, 10.0, c.text_primary, pill.center().x - 4.0, pill.center().y - 6.0);
        }

        // Clip bodies clipped to the lane area so they don't bleed over the gutter/ruler.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, tf, &Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1));
        for (tid, kind, lane) in &lanes {
            for clip in self.project.clips_on(*tid) {
                self.draw_clip(scene, tf, tr, theme, clip, lane, *kind, r);
            }
        }
        scene.pop_layer();

        // --- playhead ---
        let px = self.time_to_x(self.playhead, &r.lanes);
        if px >= r.lanes.x0 - 0.5 && px <= r.lanes.x1 + 0.5 {
            stroke_line(scene, tf, c.accent[5], px, r.ruler.y0 + 4.0, px, r.timeline.y1, 1.5);
            let head = triangle_down(px, r.ruler.y0 + 4.0, 10.0);
            scene.fill(Fill::NonZero, tf, c.accent[5], None, &head);
        }
        // playhead timecode tag in ruler
        let tag = timecode(self.playhead, self.project.fps);
        let (tw, _) = tr.measure_weighted(&tag, 10.0, None, TextWeight::Bold);
        let tagx = (px + 6.0).min(r.ruler.x1 - tw as f64 - 8.0).max(r.ruler.x0);
        let tagrect = Rect::new(tagx - 3.0, r.ruler.y0 + 2.0, tagx + tw as f64 + 5.0, r.ruler.y0 + 16.0);
        fill_rrect(scene, tf, c.accent[5], tagrect, 3.0);
        text_w(scene, tr, tf, &tag, 10.0, Color::WHITE, tagx, r.ruler.y0 + 3.0, TextWeight::Bold);
    }

    fn draw_clip(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, clip: &Clip, lane: &Rect, kind: TrackKind, r: &Regions) {
        let c = &theme.tokens.colors;
        let cr = self.clip_rect(clip, lane, r);
        if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
            return;
        }
        let base = clip_color(theme, clip.palette);
        let selected = self.selected == Some(clip.id);
        let hovered = self.hover_clip == Some(clip.id);
        let body = if kind == TrackKind::Video { base } else { darken(base, 0.05) };
        let body = if hovered { lighten(body, 0.08) } else { body };
        fill_rrect(scene, tf, with_alpha(body, if kind == TrackKind::Video { clip.opacity * 0.85 + 0.15 } else { 0.92 }), cr, 6.0);
        // header strip
        let strip = Rect::new(cr.x0, cr.y0, cr.x1, (cr.y0 + 18.0).min(cr.y1));
        fill_rrect(scene, tf, darken(base, 0.3), strip, 6.0);

        // content: filmstrip or waveform
        let inner = Rect::new(cr.x0 + 2.0, strip.y1, cr.x1 - 2.0, cr.y1 - 2.0);
        if inner.height() > 4.0 && inner.width() > 4.0 {
            if kind == TrackKind::Video {
                let n = (inner.width() / 28.0).floor().max(1.0) as i64;
                for i in 0..n {
                    let bx = inner.x0 + 4.0 + i as f64 * 28.0;
                    if bx + 22.0 > inner.x1 { break; }
                    fill_rrect(scene, tf, lighten(base, 0.16), Rect::new(bx, inner.y0 + 3.0, bx + 22.0, inner.y1 - 3.0), 2.0);
                }
            } else {
                let midy = inner.center().y;
                let n = (inner.width() / 4.0).floor() as i64;
                for i in 0..n {
                    let bx = inner.x0 + 2.0 + i as f64 * 4.0;
                    let amp = waveform_amp(i as f64 + clip.start) * (inner.height() / 2.0 - 2.0) * (0.35 + clip.volume * 0.65);
                    fill_rect(scene, tf, lighten(base, 0.3), Rect::new(bx, midy - amp, bx + 2.0, midy + amp));
                }
            }
        }
        // name label
        let label = self.project.media(clip.media).map(|m| m.name.clone()).unwrap_or_default();
        let avail = (cr.width() - 10.0).max(4.0) as f32;
        if cr.width() > 30.0 {
            let shown = elide_w(&label, avail, tr);
            text_w(scene, tr, tf, &shown, 11.0, with_alpha(Color::WHITE, 0.95), cr.x0 + 5.0, cr.y0 + 3.0, TextWeight::Medium);
        }
        // transition marker at head
        if clip.transition != Transition::None && cr.width() > 16.0 {
            let mk = triangle_corner(cr.x0, cr.y0, 12.0);
            scene.fill(Fill::NonZero, tf, with_alpha(Color::WHITE, 0.85), None, &mk);
        }
        // selection / trim handles
        if selected {
            stroke_rrect(scene, tf, Color::WHITE, cr, 6.0, 2.0);
            for hx in [cr.x0, cr.x1] {
                let hrect = Rect::new(hx - 3.0, cr.y0 + 2.0, hx + 3.0, cr.y1 - 2.0);
                fill_rrect(scene, tf, c.accent[5], hrect, 2.0);
            }
        } else {
            stroke_rrect(scene, tf, with_alpha(Color::BLACK, 0.35), cr, 6.0, 1.0);
        }
    }

    fn draw_export_modal(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        // Scrim.
        fill_rect(scene, tf, c.surfaces.overlay, self.rect);
        let l = self.export_layout(r);
        let card = l.card;
        fill_rrect(scene, tf, c.surfaces.raised, card, 14.0);
        stroke_rrect(scene, tf, c.border_standard, card, 14.0, 1.0);
        let x0 = card.x0 + 24.0;
        text_w(scene, tr, tf, "Export Project", 18.0, c.text_primary, x0, card.y0 + 20.0, TextWeight::Bold);
        text(scene, tr, tf, &self.export.suggested_filename(&self.project.name), 12.0, c.text_dim, x0, card.y0 + 44.0);

        let seg_group = |scene: &mut Scene, tr: &mut TextRenderer, label: &str, items: &[(String, Rect, bool)]| {
            if let Some((_, first, _)) = items.first() {
                text(scene, tr, tf, label, 11.0, c.text_dim, first.x0, first.y0 - 18.0);
            }
            for (name, rect, on) in items {
                let bg = if *on { c.accent[5] } else { c.neutral[3] };
                fill_rrect(scene, tf, bg, *rect, 6.0);
                let col = if *on { Color::WHITE } else { c.text_dim };
                let (lw, _) = tr.measure(name, 12.0, None);
                text(scene, tr, tf, name, 12.0, col, rect.center().x - lw as f64 / 2.0, rect.center().y - 7.0);
            }
        };
        let res: Vec<_> = l.resolution.iter().map(|(v, r)| (v.label().to_string(), *r, *v == self.export.resolution)).collect();
        seg_group(scene, tr, "RESOLUTION", &res);
        let con: Vec<_> = l.container.iter().map(|(v, r)| (v.label().to_string(), *r, *v == self.export.container)).collect();
        seg_group(scene, tr, "FORMAT", &con);
        let fps: Vec<_> = l.fps.iter().map(|(v, r)| (format!("{} fps", v.label()), *r, *v == self.export.frame_rate)).collect();
        seg_group(scene, tr, "FRAME RATE", &fps);

        // Estimate + status.
        let est = self.export.estimated_mb(self.project.duration());
        let (w, h) = self.export.resolution.dims();
        let info = format!("≈ {:.0} MB   ·   {}×{}   ·   {}", est, w, h, timecode(self.project.duration(), self.project.fps));
        text(scene, tr, tf, &info, 12.0, c.text_dim, x0, card.y1 - 96.0);
        match &self.export_status {
            ExportStatus::Rendering { progress } => {
                let bar = Rect::new(x0, card.y1 - 74.0, card.x1 - 24.0, card.y1 - 66.0);
                fill_rrect(scene, tf, c.neutral[3], bar, 4.0);
                let f = Rect::new(bar.x0, bar.y0, bar.x0 + bar.width() * progress.max(0.04), bar.y1);
                fill_rrect(scene, tf, c.accent[5], f, 4.0);
                text(scene, tr, tf, "Rendering…", 10.5, c.text_dim, x0, card.y1 - 62.0);
            }
            ExportStatus::Done { path } => {
                text(scene, tr, tf, &format!("Done: {path}"), 11.0, c.semantic.success, x0, card.y1 - 68.0);
            }
            ExportStatus::Failed { reason } => {
                text(scene, tr, tf, reason, 10.5, c.semantic.warning, x0, card.y1 - 68.0);
            }
            ExportStatus::Idle => {
                text(scene, tr, tf, "Export isn't available yet — video encoding is coming soon.", 10.5, c.text_dim, x0, card.y1 - 68.0);
            }
        }
        draw_button(scene, tf, tr, theme, l.cancel, "Close", false);
        draw_button(scene, tf, tr, theme, l.confirm, "Export", true);
    }
}

// ==========================================================================
// Free drawing utilities
// ==========================================================================
/// Honest, human-readable summary of an import batch. Never claims a success
/// that did not happen.
fn import_summary(added: usize, total: usize) -> String {
    if added == 0 {
        "No media could be imported".to_string()
    } else if added == total {
        format!("Imported {added} clip{}", if added == 1 { "" } else { "s" })
    } else {
        format!("Imported {added} of {total} files")
    }
}

/// Blit a decoded RGBA frame into `canvas`, scaled to fit (letterboxed),
/// centred. Only used under `--features decode` where the preview shows the
/// clip's real picture.
#[cfg(feature = "decode")]
fn blit_frame(scene: &mut Scene, tf: Affine, frame: &::media::VideoFrame, canvas: Rect) {
    use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
    let iw = frame.width as f64;
    let ih = frame.height as f64;
    if iw <= 0.0 || ih <= 0.0 || canvas.width() <= 0.0 || canvas.height() <= 0.0 {
        return;
    }
    let img = ImageData {
        data: Blob::new(frame.rgba.clone()),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: frame.width,
        height: frame.height,
    };
    let scale = (canvas.width() / iw).min(canvas.height() / ih);
    let dw = iw * scale;
    let dh = ih * scale;
    let dx = canvas.x0 + (canvas.width() - dw) * 0.5;
    let dy = canvas.y0 + (canvas.height() - dh) * 0.5;
    let dest = Rect::new(dx, dy, dx + dw, dy + dh);
    let brush_tf = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
    let brush = ImageBrush::new(img);
    scene.fill(Fill::NonZero, tf, &brush, Some(brush_tf), &dest);
}

fn fill_rect(scene: &mut Scene, tf: Affine, color: Color, rect: Rect) {
    scene.fill(Fill::NonZero, tf, color, None, &rect);
}
fn fill_rrect(scene: &mut Scene, tf: Affine, color: Color, rect: Rect, radius: f64) {
    scene.fill(Fill::NonZero, tf, color, None, &RoundedRect::from_rect(rect, radius));
}
fn stroke_rrect(scene: &mut Scene, tf: Affine, color: Color, rect: Rect, radius: f64, w: f64) {
    scene.stroke(&Stroke::new(w), tf, color, None, &RoundedRect::from_rect(rect, radius));
}
fn stroke_line(scene: &mut Scene, tf: Affine, color: Color, x0: f64, y0: f64, x1: f64, y1: f64, w: f64) {
    scene.stroke(&Stroke::new(w), tf, color, None, &Line::new(Point::new(x0, y0), Point::new(x1, y1)));
}
fn text(scene: &mut Scene, tr: &mut TextRenderer, tf: Affine, s: &str, size: f32, color: Color, x: f64, y: f64) {
    tr.draw_text(scene, s, size, color, tf * Affine::translate((x, y)), None);
}
#[allow(clippy::too_many_arguments)]
fn text_w(scene: &mut Scene, tr: &mut TextRenderer, tf: Affine, s: &str, size: f32, color: Color, x: f64, y: f64, weight: TextWeight) {
    tr.draw_text_weighted(scene, s, size, color, tf * Affine::translate((x, y)), None, weight);
}

fn draw_button(scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, primary: bool) {
    let c = &theme.tokens.colors;
    let bg = if primary { c.accent[5] } else { c.neutral[3] };
    fill_rrect(scene, tf, bg, rect, 7.0);
    if !primary {
        stroke_rrect(scene, tf, c.border_standard, rect, 7.0, 1.0);
    }
    let col = if primary { Color::WHITE } else { c.text_primary };
    let (lw, _) = tr.measure_weighted(label, 13.0, None, TextWeight::Medium);
    text_w(scene, tr, tf, label, 13.0, col, rect.center().x - lw as f64 / 2.0, rect.center().y - 8.0, TextWeight::Medium);
}

/// Draw an inspector slider track + fill + knob for a 0..1 value.
fn draw_track(scene: &mut Scene, tf: Affine, theme: &Theme, track: Rect, value: f64) {
    let c = &theme.tokens.colors;
    fill_rrect(scene, tf, c.neutral[3], track, 4.0);
    let v = value.clamp(0.0, 1.0);
    let filled = Rect::new(track.x0, track.y0, track.x0 + track.width() * v, track.y1);
    fill_rrect(scene, tf, c.accent[5], filled, 4.0);
    scene.fill(Fill::NonZero, tf, Color::WHITE, None, &Circle::new(Point::new(filled.x1, track.center().y), 7.0));
    scene.stroke(&Stroke::new(1.0), tf, c.border_standard, None, &Circle::new(Point::new(filled.x1, track.center().y), 7.0));
}

fn hit_slider(track: Rect, p: Point) -> bool {
    p.x >= track.x0 - 8.0 && p.x <= track.x1 + 8.0 && (p.y - track.center().y).abs() < 14.0
}

fn triangle(x: f64, y: f64, s: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((x, y));
    p.line_to((x + s, y + s / 2.0));
    p.line_to((x, y + s));
    p.close_path();
    p
}
fn triangle_down(cx: f64, top: f64, s: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((cx - s / 2.0, top));
    p.line_to((cx + s / 2.0, top));
    p.line_to((cx, top + s));
    p.close_path();
    p
}
fn triangle_corner(x: f64, y: f64, s: f64) -> BezPath {
    let mut p = BezPath::new();
    p.move_to((x, y));
    p.line_to((x + s, y));
    p.line_to((x, y + s));
    p.close_path();
    p
}

fn clip_color(theme: &Theme, palette: u8) -> Color {
    let c = &theme.tokens.colors;
    match palette % 7 {
        0 => c.accent[5],
        1 => c.semantic.info,
        2 => c.semantic.success,
        3 => c.semantic.warning,
        4 => c.accent[6],
        5 => c.semantic.danger,
        _ => c.neutral[5],
    }
}

/// Convert a normalised preview [`Rgb`] (the graded representative colour)
/// to a Vello `Color`.
fn rgb_to_color(c: Rgb) -> Color {
    Color::from_rgb8(
        (c.r * 255.0).round() as u8,
        (c.g * 255.0).round() as u8,
        (c.b * 255.0).round() as u8,
    )
}

fn with_alpha(c: Color, a: f64) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), (a.clamp(0.0, 1.0) * 255.0).round() as u8)
}
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t.clamp(0.0, 1.0)).round() as u8
}
fn lighten(c: Color, t: f64) -> Color {
    Color::from_rgb8(lerp_u8(c.r(), 255, t), lerp_u8(c.g(), 255, t), lerp_u8(c.b(), 255, t))
}
fn darken(c: Color, t: f64) -> Color {
    Color::from_rgb8(lerp_u8(c.r(), 0, t), lerp_u8(c.g(), 0, t), lerp_u8(c.b(), 0, t))
}

/// Deterministic pseudo-waveform amplitude in 0..1 for column `i`.
fn waveform_amp(i: f64) -> f64 {
    let v = (i * 0.7).sin() * 0.5 + (i * 0.31 + 1.3).sin() * 0.3 + (i * 1.93).sin() * 0.2;
    (v.abs() * 0.85 + 0.12).clamp(0.05, 1.0)
}

/// Fit a w:h aspect rect centered inside `area`.
fn fit_aspect(area: Rect, w: f64, h: f64) -> Rect {
    if area.width() <= 0.0 || area.height() <= 0.0 {
        return area;
    }
    let target = w / h;
    let avail = area.width() / area.height();
    let (rw, rh) = if avail > target {
        (area.height() * target, area.height())
    } else {
        (area.width(), area.width() / target)
    };
    let cx = area.center().x;
    let cy = area.center().y;
    Rect::new(cx - rw / 2.0, cy - rh / 2.0, cx + rw / 2.0, cy + rh / 2.0)
}

fn elide(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn elide_w(s: &str, max_px: f32, tr: &mut TextRenderer) -> String {
    let (w, _) = tr.measure(s, 11.0, None);
    if w <= max_px {
        return s.to_string();
    }
    let mut chars: Vec<char> = s.chars().collect();
    while chars.len() > 1 {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "…";
        let (cw, _) = tr.measure(&candidate, 11.0, None);
        if cw <= max_px {
            return candidate;
        }
    }
    "…".to_string()
}

/// Choose a ruler tick spacing (seconds) so labels stay ~70px apart.
fn nice_tick(px_per_sec: f64) -> f64 {
    let target_px = 76.0;
    let raw = target_px / px_per_sec.max(0.001);
    for step in [0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0] {
        if step >= raw {
            return step;
        }
    }
    600.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_sample() -> VideoEditorApp {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        app
    }

    #[test]
    fn sync_seeds_and_selects() {
        let app = app_with_sample();
        assert_eq!(app.project.clips.len(), 7);
        assert!(app.selected.is_some(), "a clip should be auto-selected");
    }

    #[test]
    fn live_new_over_fresh_doc_stays_empty() {
        // The live constructor must not fabricate a project: synced against a
        // fresh (empty) document it opens blank — no media, no clips.
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.project.clips.is_empty(), "no fabricated clips on the live path");
        assert!(app.project.media.is_empty(), "no fabricated media on the live path");
        assert!(app.selected.is_none(), "nothing selected in a blank project");
    }

    #[test]
    fn demo_build_seeds_a_project() {
        // The demo builder is the sole source of the sample project.
        let mut app = VideoEditorApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.project.clips.is_empty(), "demo build seeds sample clips");
        assert!(!app.project.media.is_empty(), "demo build seeds sample media");
    }

    /// Write a real PNG (crafted header, known dimensions) to a temp file so a
    /// probe/import test operates on real bytes, not a mock.
    fn write_real_png(w: u32, h: u32) -> std::path::PathBuf {
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&w.to_be_bytes());
        png.extend_from_slice(&h.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0]);
        let f = std::env::temp_dir()
            .join(format!("qos-ve-import-app-{}-{}.png", std::process::id(), w));
        std::fs::write(&f, &png).unwrap();
        f
    }

    #[test]
    fn importing_a_picked_file_adds_a_real_clip_to_the_bin() {
        // A real file on disk (not a mock) becomes a real MediaItem with real,
        // probed metadata, replicated into the CRDT.
        let f = write_real_png(320, 240);
        let mut app = VideoEditorApp::new_import(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        assert_eq!(app.project.media.len(), 1, "the picked file is now in the bin");
        let m = &app.project.media[0];
        assert_eq!(m.name, f.file_name().unwrap().to_string_lossy());
        assert_eq!(m.kind, MediaKind::Image);
        assert_eq!((m.width, m.height), (320, 240), "real probed dimensions");
        assert!(
            (m.duration - crate::probe::DEFAULT_IMAGE_DURATION).abs() < 1e-9,
            "a still gets the default timeline duration",
        );
        assert!(m.hydration.is_local(), "the imported bytes are local/decodable");
        // It replicated through the document (a peer would load it).
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.media.len(), 1);
        assert_eq!(reloaded.media[0].width, 320);
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn placed_import_puts_the_clip_on_the_timeline_under_the_playhead() {
        // The `--shots`/preload placed import also lands the clip on a freshly
        // created track with the playhead inside it, so the preview has content.
        let f = write_real_png(640, 360);
        let mut app = VideoEditorApp::new_import_placed(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);

        assert_eq!(app.project.media.len(), 1);
        assert_eq!(app.project.tracks.len(), 1, "a track was created for the clip");
        assert_eq!(app.project.clips.len(), 1, "the clip is on the timeline");
        let clip = &app.project.clips[0];
        assert!(
            app.playhead > clip.start && app.playhead < clip.end(),
            "playhead {} parked inside the clip [{}, {}]",
            app.playhead, clip.start, clip.end(),
        );
        // The preview composites the placed clip (not Empty) at the playhead.
        assert!(
            matches!(preview::composite(&app.project, app.playhead), CompositedFrame::Clip { .. }),
            "the preview has a clip to render",
        );
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn time_x_round_trips() {
        let app = app_with_sample();
        let r = app.regions();
        let x = app.time_to_x(12.0, &r.lanes);
        let t = app.x_to_time(x, &r.lanes);
        assert!((t - 12.0).abs() < 1e-6);
    }

    #[test]
    fn split_then_delete_changes_clip_count() {
        let mut app = app_with_sample();
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        app.sync(&mut doc, &automerge::ROOT);
        // Select a clip we know spans the playhead and split there.
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selected = Some(target);
        app.playhead = 12.0;
        let before = app.project.clips.len();
        app.split_selected(&mut doc);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.project.clips.len(), before + 1, "split adds one clip");
        // Delete the new right half (now selected).
        app.delete_selected(&mut doc);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.project.clips.len(), before);
    }

    #[test]
    fn lane_rects_cover_all_tracks() {
        let app = app_with_sample();
        let r = app.regions();
        assert_eq!(app.lane_rects(&r).len(), 4);
    }

    #[test]
    fn regions_letterbox_canvas_is_16x9() {
        let app = app_with_sample();
        let r = app.regions();
        let aspect = r.canvas.width() / r.canvas.height();
        assert!((aspect - 16.0 / 9.0).abs() < 0.05, "canvas aspect {aspect}");
    }

    #[test]
    fn wide_window_docks_the_bin_beside_the_preview() {
        let app = app_with_sample(); // 1280 wide ≥ breakpoint
        assert_eq!(app.bin_layout(), SplitLayout::Docked);
        let r = app.regions();
        assert!(!r.bin_overlay, "wide window keeps the bin docked");
        // The preview starts after the docked bin, never overlapping it.
        assert!(
            r.preview.x0 >= r.bin.x1 - 0.5,
            "docked preview begins at the bin's right edge"
        );
        assert!(r.menu_btn.width() == 0.0, "no hamburger while docked");
        assert!(r.scrim.width() == 0.0, "no scrim while docked");
    }

    #[test]
    fn narrow_window_collapses_bin_to_overlay_with_hamburger() {
        let mut app = app_with_sample();
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0); // < breakpoint
        app.advance_flap(); // first frame settles the bin to the width default
        assert_eq!(app.bin_layout(), SplitLayout::Overlay);
        let r = app.regions();
        assert!(r.bin_overlay, "narrow window collapses the bin to an overlay");
        assert!(r.menu_btn.width() > 0.0, "narrow window shows the hamburger");
        // Closed by default: the bin sits fully off the left edge and the
        // preview reclaims the full editor width.
        assert!(r.bin.x1 <= app.rect.x0 + 0.5, "closed overlay bin is off-screen left");
        assert!(
            (r.preview.x0 - app.rect.x0).abs() < 0.5,
            "narrow preview reclaims the bin's column"
        );
        assert!(r.scrim.width() == 0.0, "closed overlay paints no scrim");
    }

    #[test]
    fn opening_narrow_bin_reveals_it_over_a_scrim() {
        let mut app = app_with_sample();
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0);
        app.toggle_bin(); // user summons the bin
        app.bin_flap.snap(true); // settle the slide deterministically
        let r = app.regions();
        assert!(app.bin_flap.is_open(SplitLayout::Overlay));
        assert!(r.scrim.width() > 0.0, "open overlay bin dims the editor with a scrim");
        // Fully revealed: the bin sits flush at the left edge.
        assert!((r.bin.x0 - app.rect.x0).abs() < 0.5, "revealed bin is flush left");
    }

    #[test]
    fn crossing_breakpoint_returns_to_docked_default() {
        let mut app = app_with_sample();
        // Narrow + summon the overlay bin.
        app.rect = Rect::new(0.0, 0.0, 720.0, 820.0);
        app.advance_flap();
        app.toggle_bin();
        assert!(app.bin_flap.is_open(SplitLayout::Overlay));
        // Widen past the breakpoint: reconcile drops the stale preference and
        // the bin returns to its docked-open default.
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.advance_flap();
        assert_eq!(app.bin_layout(), SplitLayout::Docked);
        assert!(app.bin_flap.is_open(SplitLayout::Docked), "widening re-docks the open bin");
    }

    // ---- shortcut / edit-path tests --------------------------------------
    // These drive the *production* key handler and assert on the reloaded
    // CRDT, proving the shortcut → mutation → document path end to end.

    use winit::keyboard::{Key as WKey, ModifiersState};

    /// Build an app + its backing doc seeded from the same sample, so the
    /// app's in-memory project and `doc` agree (mirrors the live wiring).
    fn app_and_doc() -> (VideoEditorApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }

    fn key_press(app: &mut VideoEditorApp, doc: &mut AutoCommit, ch: &str, ctrl: bool) -> bool {
        let r = app.regions();
        let mods = if ctrl {
            winit::event::Modifiers::from(ModifiersState::CONTROL)
        } else {
            winit::event::Modifiers::default()
        };
        app.handle_key(&WKey::Character(ch.into()), &mods, doc, &r)
    }

    #[test]
    fn split_shortcut_mutates_the_crdt_timeline() {
        let (mut app, mut doc) = app_and_doc();
        let target = app.project.top_video_clip_at(10.0).unwrap().id;
        app.selected = Some(target);
        app.playhead = 12.0;
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(key_press(&mut app, &mut doc, "s", false), "S handled");
        // The new clip is in the *document*, not just the in-memory mirror.
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.clips.len(), before + 1, "split wrote a clip to the CRDT");
        assert!(
            app.actions.take_committed().is_some(),
            "split dispatched an action, arming one undo boundary",
        );
    }

    #[test]
    fn brightness_filter_writes_through_to_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        // Select a video clip so the brightness control exists.
        let id = app.project.top_video_clip_at(5.0).unwrap().id;
        app.selected = Some(id);
        app.set_filter(&mut doc, FilterField::Brightness, 0.9);
        let reloaded = VideoDoc::load_project(&doc);
        assert!((reloaded.clip(id).unwrap().filters.brightness - 0.9).abs() < 1e-9);
        assert!(
            app.actions.take_committed().is_some(),
            "the grade dispatched an action, arming one undo boundary",
        );
    }

    #[test]
    fn copy_paste_shortcuts_add_an_independent_clip() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(5.0).unwrap().id;
        app.selected = Some(id);
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(key_press(&mut app, &mut doc, "c", true), "Ctrl+C handled");
        app.playhead = 30.0;
        assert!(key_press(&mut app, &mut doc, "v", true), "Ctrl+V handled");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.clips.len(), before + 1, "paste added a clip to the CRDT");
        // The pasted clip is the new selection and sits at the playhead.
        let pasted = app.selected.unwrap();
        assert_ne!(pasted, id);
        assert!((reloaded.clip(pasted).unwrap().start - 30.0).abs() < 1e-9);
    }

    #[test]
    fn set_in_out_shortcuts_trim_the_selected_clip() {
        let (mut app, mut doc) = app_and_doc();
        // Interview clip spans roughly [8, 20] on V1.
        let id = app.project.top_video_clip_at(12.0).unwrap().id;
        app.selected = Some(id);
        // Move playhead into the clip and set the out-point there.
        app.playhead = 15.0;
        assert!(key_press(&mut app, &mut doc, "o", false), "O handled");
        let reloaded = VideoDoc::load_project(&doc);
        assert!((reloaded.clip(id).unwrap().end() - 15.0).abs() < 1e-6, "O trims the tail to the playhead");
        assert!(
            app.actions.take_committed().is_some(),
            "setting the out-point dispatched an action, arming one undo boundary",
        );
    }

    #[test]
    fn jkl_shuttle_moves_and_stops_the_playhead() {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 5.0;
        assert!(key_press(&mut app, &mut doc, "l", false), "L handled");
        assert!(app.shuttle > 0.0, "L shuttles forward");
        assert!(key_press(&mut app, &mut doc, "l", false));
        assert!(app.shuttle >= 2.0, "repeated L doubles the rate");
        assert!(key_press(&mut app, &mut doc, "k", false), "K handled");
        assert_eq!(app.shuttle, 0.0, "K stops the shuttle");
        assert!(key_press(&mut app, &mut doc, "j", false));
        assert!(app.shuttle < 0.0, "J shuttles in reverse");
    }

    #[test]
    fn ripple_delete_shortcut_closes_the_gap_in_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        // Market cutaway is the last V1 clip at start=20.
        let v1_first = app.project.top_video_clip_at(3.0).unwrap();
        let track = v1_first.track;
        let first = v1_first.id;
        // Record the next clip on the same track and its start.
        let next = app.project.clips_on(track).into_iter().find(|c| c.id != first).map(|c| (c.id, c.start));
        app.selected = Some(first);
        let mods = winit::event::Modifiers::from(ModifiersState::SHIFT);
        let r = app.regions();
        // Shift+Delete ripples.
        app.handle_key(&WKey::Named(NamedKey::Delete), &mods, &mut doc, &r);
        let reloaded = VideoDoc::load_project(&doc);
        assert!(reloaded.clip(first).is_none(), "rippled clip removed from CRDT");
        if let Some((nid, old_start)) = next {
            assert!(reloaded.clip(nid).unwrap().start < old_start, "later clip shifted left in the CRDT");
        }
    }

    #[test]
    fn nudge_arrow_moves_selected_clip_in_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(12.0).unwrap().id;
        app.selected = Some(id);
        let before = VideoDoc::load_project(&doc).clip(id).unwrap().start;
        let r = app.regions();
        // Shift = one-second nudge right.
        let shift = winit::event::Modifiers::from(ModifiersState::SHIFT);
        app.handle_key(&WKey::Named(NamedKey::ArrowRight), &shift, &mut doc, &r);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().start;
        assert!((after - before - 1.0).abs() < 1e-6, "Shift+Right nudges +1s in the CRDT");
    }

    // ---- click-driven tests: drive the REAL handle_event pipeline --------
    // These synthesise winit events at rects the app itself computes, so they
    // exercise the exact production path (regions → hit-test → drag/mutation →
    // CRDT) a live click takes — not shortcut helpers.

    /// Winit `DeviceId` isn't publicly constructible; zero-init it (the app
    /// never inspects it), matching the terminal/date-picker test pattern.
    fn fake_device_id() -> winit::event::DeviceId {
        unsafe { std::mem::zeroed() }
    }

    fn dispatch(app: &mut VideoEditorApp, doc: &mut AutoCommit, ev: WindowEvent) {
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::<()>::new();
        let node = NodeId::new(0);
        app.handle_event(&ev, 1.0, &winit::event::Modifiers::default(), doc, &automerge::ROOT, &mut handled, &mut focus, node, &taffy);
    }

    fn move_to(app: &mut VideoEditorApp, doc: &mut AutoCommit, x: f64, y: f64) {
        dispatch(app, doc, WindowEvent::CursorMoved {
            device_id: fake_device_id(),
            position: winit::dpi::PhysicalPosition::new(x, y),
        });
    }
    fn press(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
        dispatch(app, doc, WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        });
    }
    fn release(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
        dispatch(app, doc, WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Released,
            button: MouseButton::Left,
        });
    }
    /// A full click at (x,y): move (sets last_mouse, as the live loop does),
    /// then press.
    fn click(app: &mut VideoEditorApp, doc: &mut AutoCommit, x: f64, y: f64) {
        move_to(app, doc, x, y);
        press(app, doc);
    }

    #[test]
    fn click_scrubs_playhead_on_the_ruler() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Click a point partway along the ruler and assert the playhead
        // landed near the corresponding time (via the app's own mapping).
        let x = r.ruler.x0 + 200.0;
        let y = r.ruler.center().y;
        let expected = app.x_to_time(x, &r.lanes);
        click(&mut app, &mut doc, x, y);
        assert!((app.playhead - expected).abs() < 0.1, "ruler click scrubs playhead to {expected}, got {}", app.playhead);
    }

    #[test]
    fn double_click_bin_item_adds_clip_to_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Target the first bin row's rect (computed by the app).
        let (_mid, rect) = app.bin_item_rects(&r)[0];
        let before = VideoDoc::load_project(&doc).clips.len();
        // Two presses inside the double-click window at the same spot.
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let after = VideoDoc::load_project(&doc).clips.len();
        assert_eq!(after, before + 1, "double-click on a bin item appends a clip to the CRDT");
    }

    #[test]
    fn drag_moves_a_clip_and_writes_the_new_start_to_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Find the drone clip's on-screen rect on V1 (the first video lane row).
        let clip = app.project.top_video_clip_at(3.0).unwrap().clone();
        let (_tid, _k, lane) = app.lane_rects(&r).into_iter()
            .find(|(tid, _, _)| *tid == clip.track).unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        let start_before = VideoDoc::load_project(&doc).clip(clip.id).unwrap().start;
        // Grab the body (avoid the ±8px trim edges), drag right ~2 seconds.
        let grab_x = cr.center().x;
        let grab_y = cr.center().y;
        let drop_x = grab_x + 2.0 * app.zoom; // +2s at current px/sec
        move_to(&mut app, &mut doc, grab_x, grab_y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, drop_x, grab_y);
        release(&mut app, &mut doc);
        let start_after = VideoDoc::load_project(&doc).clip(clip.id).unwrap().start;
        assert!(start_after > start_before + 1.0, "dragging moved the clip right in the CRDT ({start_before} -> {start_after})");
    }

    #[test]
    fn drag_right_edge_trims_the_clip_in_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let clip = app.project.top_video_clip_at(3.0).unwrap().clone();
        let (_tid, _k, lane) = app.lane_rects(&r).into_iter()
            .find(|(tid, _, _)| *tid == clip.track).unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        let out_before = VideoDoc::load_project(&doc).clip(clip.id).unwrap().out_point;
        // Grab within TRIM_HIT of the right edge and drag left ~1.5s.
        let grab_x = cr.x1 - 1.0;
        let grab_y = cr.center().y;
        let drop_x = grab_x - 1.5 * app.zoom;
        move_to(&mut app, &mut doc, grab_x, grab_y);
        press(&mut app, &mut doc);
        assert!(matches!(app.drag, Some(Drag::TrimOut { .. })), "grabbing the right edge starts a trim-out drag");
        move_to(&mut app, &mut doc, drop_x, grab_y);
        release(&mut app, &mut doc);
        let out_after = VideoDoc::load_project(&doc).clip(clip.id).unwrap().out_point;
        assert!(out_after < out_before, "trimming the right edge shortened out_point in the CRDT ({out_before} -> {out_after})");
    }

    #[test]
    fn click_selects_a_clip_via_the_real_hit_test() {
        let (mut app, mut doc) = app_and_doc();
        app.selected = None;
        let r = app.regions();
        // The market cutaway on V1 sits around t=22.
        let clip = app.project.top_video_clip_at(22.0).unwrap().clone();
        let (_tid, _k, lane) = app.lane_rects(&r).into_iter()
            .find(|(tid, _, _)| *tid == clip.track).unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        assert_eq!(app.selected, Some(clip.id), "clicking a clip selects it");
    }

    #[test]
    fn editing_brightness_brightens_the_preview_composite() {
        // Drive the real inspector edit path, then assert the preview module's
        // composited frame for that clip is brighter — proving inspector →
        // CRDT → preview is one connected pipeline.
        let (mut app, mut doc) = app_and_doc();
        // Pick the market cutaway on V1 (neutral grade, fully steady at t=23).
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selected = Some(id);
        let before = match preview::composite(&app.project, 23.0) {
            CompositedFrame::Clip { base, .. } => base.luma(),
            _ => panic!("expected clip"),
        };
        app.set_filter(&mut doc, FilterField::Brightness, 0.95);
        // Reload from the CRDT so we're reading the persisted grade, not the
        // in-memory mirror.
        app.sync(&mut doc, &automerge::ROOT);
        let after = match preview::composite(&app.project, 23.0) {
            CompositedFrame::Clip { base, .. } => base.luma(),
            _ => panic!("expected clip"),
        };
        assert!(after > before, "raising brightness brightens the composite ({before} -> {after})");
    }
}
