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

use std::collections::{HashMap, HashSet};
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
use ui::widget::{
    ContextMenu, EditOutcome, EditableLabel, FlapState, MenuItem, SplitLayout, Widget,
};

use crate::actions::{
    delete_clip_action, remove_media_action, ripple_delete_action, set_project_meta_action,
    split_clip_action,
    upsert_clip_action, upsert_media_action, upsert_track_action, VideoResourceCtx,
    VIDEO_EDITOR_ACTION_TYPE,
};
use crate::doc::VideoDoc;
use crate::export::{
    Container, ExportSettings, ExportStatus, FrameRate, Resolution,
};
use crate::model::{
    Clip, ClipId, EffectKind, Filters, MediaId, MediaItem, MediaKind, Project, TextAlign,
    TextSource, TrackId, TrackKind, Transition, MAX_SPEED, MIN_SPEED,
};
use crate::param::{Interp, Param};
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
/// Overlay-scrollbar geometry, matching [`ui::widget::DocumentViewport`] so
/// the timeline's bar is the same object the rest of the fleet shows.
const SCROLLBAR_THICKNESS: f64 = 8.0;
const SCROLLBAR_INSET: f64 = 3.0;
/// Below this (logical px, either axis) the letterboxed preview canvas has no
/// room to show a frame and the pane says so instead.
const MIN_CANVAS: f64 = 48.0;
/// Below this the transport has no room for a scrubber and draws none.
const MIN_SCRUBBER_W: f64 = 90.0;
/// Above this a longer bar buys no precision and just looks stretched.
const MAX_SCRUBBER_W: f64 = 560.0;
/// How dark the overlay-bin scrim gets at full reveal. The editor behind it
/// must stay readable — the scrim says "the bin has the input now", it is not
/// a curtain. Deliberately a number here rather than the alpha of
/// `surfaces.scrim`: that token is opaque in every theme.
const SCRIM_ALPHA: f64 = 0.55;
/// How dark the export modal's backdrop gets. Heavier than the bin flap's
/// veil — a modal really does take the whole app — but still a veil: the user
/// keeps their place in the timeline behind it.
const MODAL_SCRIM_ALPHA: f64 = 0.72;
/// Vertical space the export card's header (title + filename) needs before
/// the first section label can start.
const EXPORT_HEADER_H: f64 = 76.0;
/// Space the card's footer needs: size estimate, status line, buttons.
const EXPORT_FOOTER_H: f64 = 104.0;
/// A section label's own height, above its row of segments.
const EXPORT_LABEL_H: f64 = 18.0;
/// The least a label-plus-segments group can be squeezed to.
const EXPORT_GROUP_H: f64 = 50.0;

/// What the editor says, in plain words, when a source's bytes are not on this
/// device — the reference replicated but the content has not been hydrated
/// from the FUSE (or the file is gone). One string so the bin row, the clip
/// and the preview never disagree about what they are disclosing.
const MEDIA_NOT_LOCAL: &str = "Not on this device";
/// The same claim for a pane too narrow to hold the full phrase (the media
/// bin is a fixed 268px column). Shorter, never vaguer.
const MEDIA_NOT_LOCAL_SHORT: &str = "Not on device";

/// What the editor says when a source's bytes ARE here but are not media — a
/// web page saved with a `.mp4` extension, a truncated download, a format
/// nothing on this box can decode. Distinct from [`MEDIA_NOT_LOCAL`]: fetching
/// it again will not help.
const MEDIA_UNREADABLE: &str = "Unreadable media";
/// The compact form, for the media bin's fixed-width column.
const MEDIA_UNREADABLE_SHORT: &str = "Unreadable";

/// Below this allocated width the media bin stops being a docked pane and
/// collapses into an overlay flap summoned by the topbar hamburger. Above it
/// the bin is docked beside the editor (the wide desktop layout).
/// Height of a collapsible inspector section header.
const SECTION_H: f64 = 24.0;
/// The inset a picture-in-picture preset places a clip at: a third of the
/// frame, a small margin off the corner. Big enough to see, small enough to
/// leave the frame underneath readable.
const PIP_SCALE: f64 = 0.32;
const PIP_INSET: f64 = 0.04;
/// Width of the key control at the head of an animatable inspector row.
const KEY_BOX: f64 = 20.0;

const BIN_BREAKPOINT: f64 = 900.0;

/// Below this window width the inspector collapses to an overlay flap from
/// the right, exactly as the bin does from the left.
///
/// A fixed 300px panel is a third of a 900px window and nearly two thirds of
/// a 500px one — at which point the preview, the transport buttons and the
/// timecode are all squeezed out by a panel that edits a clip the user can no
/// longer see. Below the breakpoint the inspector gets out of the way and is
/// summoned by the topbar toggle.
///
/// Unlike the bin, an open inspector paints **no scrim**: its whole purpose
/// is to change the picture, so dimming the picture while you drag a slider
/// would hide the thing you are adjusting.
const INSPECTOR_BREAKPOINT: f64 = 760.0;

/// Computed region rectangles for one frame, derived purely from the root
/// rect. Recomputed in both `draw` and `handle_event` so the two never
/// disagree about where anything is.
#[derive(Clone, Copy, Default)]
struct Regions {
    topbar: Rect,
    /// Hamburger toggle in the topbar. Zero-sized (and skipped) while the bin
    /// is docked; present only in the narrow overlay layout.
    menu_btn: Rect,
    /// Inspector toggle in the topbar. Zero-sized while the inspector is
    /// docked; present only in the narrow overlay layout.
    inspector_btn: Rect,
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
    /// `true` when the inspector is collapsed to an overlay flap. It slides
    /// from the right and paints no scrim — see [`INSPECTOR_BREAKPOINT`].
    inspector_overlay: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum TransportAction {
    ToStart,
    StepBack,
    PlayPause,
    StepFwd,
    ToEnd,
}

/// A collapsible block of inspector controls.
///
/// Sections exist because the inspector grew past what a 300px column shows
/// at once. Which are folded is **presentation** — it is a property of this
/// reader's panel, not of the clip — so it lives in a widget field and never
/// in the CRDT, and it persists across selections: a user who folds Transform
/// away is telling the panel what they are working on, not this clip.
///
/// A folded header still states what is inside it (see `section_summary`), so
/// folding hides controls but never hides state.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Section {
    Transform,
    Speed,
    Title,
    Effects,
    Markers,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Section::Transform => "TRANSFORM",
            Section::Speed => "SPEED",
            Section::Title => "TITLE",
            Section::Effects => "EFFECTS",
            Section::Markers => "MARKERS",
        }
    }
}

/// A numeric inspector control that is not one of the original `0..1`
/// sliders: it has its own range, its own readout, and in two cases it edits
/// the *media item* rather than the clip.
///
/// One enum rather than a `Drag` variant each keeps the press / drag / commit
/// arms from growing a near-identical copy per knob.
#[derive(Clone, Copy, PartialEq, Debug)]
enum NumField {
    ScaleX,
    ScaleY,
    PosX,
    PosY,
    Rotation,
    CropLeft,
    CropRight,
    CropTop,
    CropBottom,
    Speed,
    TitleSize,
    TitleBackdrop,
}

impl NumField {
    fn label(self) -> &'static str {
        match self {
            NumField::ScaleX => "Scale X",
            NumField::ScaleY => "Scale Y",
            NumField::PosX => "Position X",
            NumField::PosY => "Position Y",
            NumField::Rotation => "Rotation",
            NumField::CropLeft => "Left",
            NumField::CropRight => "Right",
            NumField::CropTop => "Top",
            NumField::CropBottom => "Bottom",
            NumField::Speed => "Rate",
            NumField::TitleSize => "Size",
            NumField::TitleBackdrop => "Backdrop",
        }
    }

    /// The animatable parameter this control drives, if any. Crop and anchor
    /// are deliberately not animatable in the model — they frame the shot
    /// rather than move it — and neither title knob is a clip parameter at
    /// all, so those have no key control.
    fn param(self) -> Option<Param> {
        Some(match self {
            NumField::ScaleX => Param::ScaleX,
            NumField::ScaleY => Param::ScaleY,
            NumField::PosX => Param::PosX,
            NumField::PosY => Param::PosY,
            NumField::Rotation => Param::Rotation,
            _ => return None,
        })
    }

    /// Whether this knob writes the selected clip's **media item** instead of
    /// the clip — the two title properties do, and they commit a different
    /// action because of it.
    fn is_media(self) -> bool {
        matches!(self, NumField::TitleSize | NumField::TitleBackdrop)
    }

    /// The slider's linear span. Cropping stops short of the far edge so a
    /// clip can never be dragged out of existence, and scale stops short of
    /// zero for the same reason.
    fn range(self) -> (f64, f64) {
        match self {
            NumField::ScaleX | NumField::ScaleY => (0.05, 4.0),
            NumField::PosX | NumField::PosY => (-1.0, 1.0),
            NumField::Rotation => (-180.0, 180.0),
            NumField::CropLeft | NumField::CropRight | NumField::CropTop | NumField::CropBottom => {
                (0.0, 0.9)
            }
            NumField::Speed => (MIN_SPEED, MAX_SPEED),
            NumField::TitleSize => (0.02, 0.4),
            NumField::TitleBackdrop => (0.0, 1.0),
        }
    }

    /// Rate is the one knob whose span is multiplicative: 0.05x to 20x is
    /// eight octaves, and on a linear track normal speed would sit at 5% of
    /// the way along with every useful slow-motion rate crushed against the
    /// left stop. On a log track 1x lands dead centre and half speed and
    /// double speed are the same distance from it, which is how a rate reads.
    fn logarithmic(self) -> bool {
        matches!(self, NumField::Speed)
    }

    fn to_frac(self, v: f64) -> f64 {
        let (lo, hi) = self.range();
        if self.logarithmic() {
            let v = v.clamp(lo, hi).max(1e-6);
            return ((v.ln() - lo.ln()) / (hi.ln() - lo.ln())).clamp(0.0, 1.0);
        }
        ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
    }

    fn from_frac(self, f: f64) -> f64 {
        let (lo, hi) = self.range();
        let f = f.clamp(0.0, 1.0);
        if self.logarithmic() {
            return (lo.ln() + (hi.ln() - lo.ln()) * f).exp();
        }
        lo + (hi - lo) * f
    }

    fn readout(self, v: f64) -> String {
        match self {
            NumField::ScaleX | NumField::ScaleY => format!("{v:.2}×"),
            NumField::PosX | NumField::PosY => format!("{v:+.2}"),
            NumField::Rotation => format!("{v:.0}°"),
            NumField::CropLeft
            | NumField::CropRight
            | NumField::CropTop
            | NumField::CropBottom => format!("{:.0}%", v * 100.0),
            NumField::Speed => format!("{v:.2}×"),
            NumField::TitleSize => format!("{:.0}%", v * 100.0),
            NumField::TitleBackdrop => format!("{:.0}%", v * 100.0),
        }
    }
}

/// Which marker an inspector row addresses. A clip marker and a project
/// marker are edited through different actions (a whole-clip write versus a
/// project-meta write), so the row has to carry which it is.
#[derive(Clone, Copy, PartialEq, Debug)]
struct MarkerRef {
    id: Uuid,
    /// `Some` for a marker pinned to a clip, `None` for a timeline one.
    clip: Option<ClipId>,
    /// Absolute timeline seconds, already rebased for a clip marker.
    at: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Playhead,
    Scrubber,
    /// Moving the selection. `id` is the clip under the pointer (the one whose
    /// left edge snaps); `grab_dt` is where inside it the grab landed; `track`
    /// is the lane it should land on, re-resolved from the cursor on every
    /// move so a vertical drag crosses tracks. Every other selected clip
    /// follows by the same time delta.
    ClipMove { id: ClipId, grab_dt: f64, track: TrackId },
    TrimIn { id: ClipId },
    TrimOut { id: ClipId },
    /// Rubber-band selection from `origin` on empty lane space. `additive`
    /// keeps the previous selection (shift held). A press that never leaves
    /// [`MARQUEE_THRESHOLD`] falls back to scrubbing on release.
    Marquee { origin: Point, additive: bool },
    /// Dragging a scrollbar thumb. The variant names which pane's bar, so
    /// one drag arm serves the timeline, the inspector and the bin.
    Scrollbar(ScrollPane),
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    FadeIn,
    FadeOut,
    /// Dragging the incoming transition's length.
    TransitionLen,
    /// Dragging the intensity slider of the effect at `idx` on the selected clip.
    EffectAmount { idx: usize },
    /// Dragging one of the ranged controls — geometry, rate, title size. See
    /// [`NumField`], which carries the range, the readout and where the value
    /// is written.
    Num(NumField),
}

/// The timeline's clip selection: an ordered set with a *primary* (anchor)
/// clip. The primary is what the inspector edits and what a fresh click
/// re-anchors to; every clip in the set takes part in delete / ripple-delete /
/// copy / cut / duplicate / nudge / split, each of which lands as **one** undo
/// boundary regardless of how many clips it touched.
#[derive(Clone, Default, PartialEq)]
struct Selection {
    ids: Vec<ClipId>,
    primary: Option<ClipId>,
}

impl Selection {
    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    fn len(&self) -> usize {
        self.ids.len()
    }
    /// The anchor clip — the one the inspector shows and single-clip edits act
    /// on. `None` exactly when the selection is empty.
    fn primary(&self) -> Option<ClipId> {
        self.primary
    }
    fn contains(&self, id: ClipId) -> bool {
        self.ids.contains(&id)
    }
    fn ids(&self) -> &[ClipId] {
        &self.ids
    }
    fn clear(&mut self) {
        self.ids.clear();
        self.primary = None;
    }
    /// Collapse to just `id` — a plain click.
    fn select_only(&mut self, id: ClipId) {
        self.ids.clear();
        self.ids.push(id);
        self.primary = Some(id);
    }
    /// Add or remove `id` (shift-click). Removing the primary promotes
    /// whatever is left so the inspector never points at nothing while clips
    /// are still selected.
    fn toggle(&mut self, id: ClipId) {
        if let Some(pos) = self.ids.iter().position(|&c| c == id) {
            self.ids.remove(pos);
            if self.primary == Some(id) {
                self.primary = self.ids.last().copied();
            }
        } else {
            self.ids.push(id);
            self.primary = Some(id);
        }
    }
    /// Re-anchor on an already-selected clip without disturbing the set — a
    /// plain click inside a multi-selection.
    fn set_primary(&mut self, id: ClipId) {
        if self.ids.contains(&id) {
            self.primary = Some(id);
        }
    }
    /// Drop every id in `gone` (clips a track / media deletion is about to
    /// take with it), promoting the primary if it was one of them.
    fn remove_all(&mut self, gone: &[ClipId]) {
        self.ids.retain(|id| !gone.contains(id));
        if self.primary.map(|id| gone.contains(&id)).unwrap_or(false) {
            self.primary = self.ids.last().copied();
        }
    }
    /// Replace the whole set (a marquee release), anchoring on the last id.
    fn set(&mut self, ids: Vec<ClipId>) {
        self.primary = ids.last().copied();
        self.ids = ids;
    }
    /// Drop ids whose clips are gone (a remote delete, or a track deletion),
    /// promoting the primary if it was one of them.
    fn retain_existing(&mut self, p: &Project) {
        self.ids.retain(|id| p.clip(*id).is_some());
        if self.primary.map(|id| p.clip(id).is_none()).unwrap_or(false) {
            self.primary = self.ids.last().copied();
        }
    }
}

/// Pointer travel (logical px) before a press on empty lane space becomes a
/// rubber-band selection rather than a playhead scrub.
const MARQUEE_THRESHOLD: f64 = 4.0;

/// How long after a bin click a second click on the same item still counts as
/// a double-click ("add to timeline") rather than a rename intent.
const BIN_DOUBLE_CLICK_MS: u128 = 400;
/// The upper edge of the deliberate slow-second-click rename window. Past it
/// the previous click is stale and a press is just a fresh press — without
/// this bound a bin item, once clicked, could never be dragged again.
const BIN_RENAME_WINDOW_MS: u128 = 2_000;

/// A scrollable pane of the editor. Each owns an offset, a shared
/// [`ui::widget::Scrollbar`] interaction model and a track rect; naming them
/// lets one press / drag / wheel arm serve all three.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScrollPane {
    /// The timeline's horizontal axis (seconds).
    Timeline,
    /// The inspector body's vertical axis (px).
    Inspector,
    /// The media bin's vertical axis (px).
    Bin,
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
    /// A bin item's context menu open (Add to timeline / Rename / Delete).
    BinMenu,
    /// A bin item being renamed in place through the shared editable label.
    BinRename,
    /// A bin clip mid-drag over a timeline lane, with the ghost + drop marker.
    DragToTimeline,
    /// The graded drone selected with its effects stack scrolled into view.
    Effects,
    /// Several clips selected at once, showing the anchor's full outline
    /// against the lighter outlines of the rest and the inspector's count.
    MultiSelect,
    /// A locked video track: the header's lock pill lit, the lane hatched
    /// over its clips, and the inspector naming the lock.
    LockedTrack,
    /// Zoomed in and scrolled into the middle of the timeline, so the
    /// horizontal scrollbar is showing part-way along its track.
    Scrolled,
    /// A timeline clip's context menu open (cut / copy / duplicate / split /
    /// delete).
    ClipMenu,
    /// A rubber-band selection mid-sweep across a lane.
    Marquee,
    /// A composed frame: a clip inset into a corner, a title in the lower
    /// third, named markers on the ruler.
    Composed,
    /// The inspector scrolled to the geometry and rate sections, on a clip
    /// that is inset, re-timed and keyframed.
    Geometry,
    /// A title clip selected, with its words, size, alignment and colour
    /// showing, and the marker list under them.
    TitleClip,
    /// A keyframed clip with the playhead sitting on a key: the key control
    /// filled, the curve's diamonds on the clip body.
    Keyframes,
    /// The marker list, holding timeline markers and a clip's own, with the
    /// ruler flags they correspond to.
    Markers,
}

impl ShotScenario {
    /// Scenarios arranged from a rect-dependent interaction (menu / rename /
    /// drag / effects), applied in `draw` once the layout rect is known rather
    /// than during `sync` (where the rect is still zero).
    fn is_interaction(self) -> bool {
        matches!(
            self,
            ShotScenario::BinMenu
                | ShotScenario::BinRename
                | ShotScenario::DragToTimeline
                | ShotScenario::Effects
                | ShotScenario::MultiSelect
                | ShotScenario::LockedTrack
                | ShotScenario::Scrolled
                | ShotScenario::ClipMenu
                | ShotScenario::Marquee
                | ShotScenario::Geometry
                | ShotScenario::TitleClip
                | ShotScenario::Keyframes
                | ShotScenario::Markers
        )
    }

    /// Parse a `--scenario <name>` selector. Returns `None` for an unknown
    /// name so the caller can fall back to the plain editor rather than
    /// silently capturing the wrong thing.
    pub fn from_arg(name: &str) -> Option<Self> {
        Some(match name {
            "filters" => ShotScenario::SelectedFilters,
            "split" => ShotScenario::JustSplit,
            "menu" => ShotScenario::BinMenu,
            "rename" => ShotScenario::BinRename,
            "drag" => ShotScenario::DragToTimeline,
            "effects" => ShotScenario::Effects,
            "multiselect" => ShotScenario::MultiSelect,
            "locked" => ShotScenario::LockedTrack,
            "scrolled" => ShotScenario::Scrolled,
            "clipmenu" => ShotScenario::ClipMenu,
            "marquee" => ShotScenario::Marquee,
            "composed" => ShotScenario::Composed,
            "geometry" => ShotScenario::Geometry,
            "title" => ShotScenario::TitleClip,
            "keyframes" => ShotScenario::Keyframes,
            "markers" => ShotScenario::Markers,
            _ => return None,
        })
    }
}

pub struct VideoEditorApp {
    project: Project,
    /// Deterministic capture scenario applied once during the first `sync`.
    shot_scenario: ShotScenario,
    /// Guards the one-time scenario setup so it doesn't re-run every sync.
    shot_applied: bool,
    /// Guards the one-time rect-dependent interaction scenario setup (applied in
    /// `draw` once the layout rect is known).
    shot_interaction_applied: bool,
    // --- view state (per-peer, never in the CRDT) ---
    playhead: f64,
    zoom: f64,  // pixels per second
    scroll: f64, // seconds at the left edge of the lane area
    /// Shared scrollbar interaction models (thumb geometry, thumb drag, track
    /// paging) — the same ones [`ui::widget::Scrollable`] and
    /// [`ui::widget::DocumentViewport`] use, so every bar here behaves and
    /// looks identical to every other scroll surface in the fleet. One per
    /// scrollable axis: the timeline scrolls horizontally, the inspector and
    /// the media bin vertically.
    hbar: ui::widget::Scrollbar,
    inspector_bar: ui::widget::Scrollbar,
    bin_bar: ui::widget::Scrollbar,
    selection: Selection,
    selected_media: Option<MediaId>,
    playing: bool,
    // --- ui state ---
    show_export: bool,
    export: ExportSettings,
    export_status: ExportStatus,
    /// A media-import portal request in flight, drained each `sync`.
    import_rx: Option<ImportChannel>,
    /// A short, honest status line shown after an import attempt (how many
    /// clips landed, or why the portal round-trip failed). Never a fake success.
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
    /// The preview's frame source, held for the app's lifetime.
    ///
    /// It owns the decode sessions and one YUV converter per layer slot, so
    /// rebuilding it per frame would re-open a pipeline every frame. It is
    /// re-indexed (not rebuilt) when the media bin changes.
    #[cfg(feature = "decode")]
    frame_source: Option<crate::gpu::MediaFrameSource>,
    /// Signature of the media the frame source was last indexed against.
    #[cfg(feature = "decode")]
    frame_source_sig: u64,
    /// Frames since the frame source last dropped its idle sessions.
    #[cfg(feature = "decode")]
    prune_countdown: u32,
    /// Audio monitoring: the mixed timeline, played while the transport
    /// runs. `None` until the first frame builds it, and it holds a
    /// [`audio::NullSink`] on a box with no output — silent playback rather
    /// than no playback, so `Clip::volume` and the track mutes still have
    /// something on the other end of them.
    #[cfg(any(feature = "gst", feature = "decode"))]
    monitor: Option<crate::audio::AudioMonitor>,
    #[cfg(any(feature = "gst", feature = "decode"))]
    monitor_tried: bool,
    /// Set when the document changes, so the monitor is handed the new
    /// timeline once per edit instead of once per frame.
    #[cfg(any(feature = "gst", feature = "decode"))]
    monitor_stale: bool,
    /// The document heads the model was last loaded at — how "the document
    /// changed" is told from "the same document was reloaded".
    doc_heads: Vec<automerge::ChangeHash>,
    /// The GPU compositor that produces the preview picture — the *same*
    /// renderer the export runs, so what you see is byte-for-byte what you
    /// get. Built once and reused; `None` on a box with no usable adapter,
    /// where the preview falls back to the representative fill rather than
    /// pretending it composited anything.
    compositor: Option<crate::gpu::HeadlessCompositor>,
    /// Set once we have tried to build the compositor, so a box that cannot
    /// have one does not retry every frame.
    compositor_tried: bool,
    /// Scratch RGBA readback from the compositor, reused between frames.
    composited: Option<(u32, u32, std::sync::Arc<Vec<u8>>)>,
    drag: Option<Drag>,
    /// Live rubber-band rect while a [`Drag::Marquee`] is past its threshold,
    /// so `draw` can paint it. `None` until the press actually becomes a drag.
    marquee: Option<Rect>,
    /// Set whenever the playhead moves on its own (playback, shuttle, a
    /// transport button, a keyboard jump) so the next frame scrolls it back
    /// into view. Deliberately *not* set by wheel panning: the view follows
    /// the playhead, never fights the user's own scroll.
    chase_playhead: bool,
    last_mouse: Point,
    hover_clip: Option<ClipId>,
    hover_media: Option<MediaId>,
    last_click_media: Option<(MediaId, Instant)>,
    /// A bin press inside the slow-second-click rename window. The rename only
    /// fires if the press is *released* without ever passing
    /// [`BIN_DRAG_THRESHOLD`] — a drag always wins over a rename.
    pending_rename: Option<MediaId>,
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
    /// Copied clips, ready to paste at the playhead (Ctrl+C / Ctrl+X / Ctrl+V).
    /// Holds full clip snapshots — trim window, filters and fades come along —
    /// and pasting preserves their relative offsets and lanes, so copying a
    /// multi-clip selection round-trips as a block rather than a pile.
    clipboard: Vec<Clip>,
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
    /// The same kernel driving the inspector's docked-vs-overlay reveal from
    /// the right edge. See [`INSPECTOR_BREAKPOINT`].
    inspector_flap: FlapState,
    /// Real-time clock for the flap's frame-rate-independent eased slide.
    flap_tick: Instant,
    // --- media-bin clip management ---
    /// Right-click context menu for a bin item (Add to timeline / Rename /
    /// Delete). Owned as a field and driven directly (open_at / item_at /
    /// take_selection) because the editor is a canvas, not a taffy tree.
    bin_menu: ContextMenu,
    /// What the open context menu (or a pending menu action) targets — a bin
    /// media item or a timeline track header.
    menu_target: Option<MenuTarget>,
    /// Inspector sections the reader has folded away. Presentation, never in
    /// the document — see [`Section`]. Empty means everything is open.
    collapsed: HashSet<Section>,
    /// In-flight inline edit of the selected title's words, through the same
    /// shared `EditableLabel` the bin rename uses.
    title_edit: Option<(MediaId, EditableLabel)>,
    /// In-flight inline rename of a marker in the inspector's list.
    marker_rename: Option<(MarkerRef, EditableLabel)>,
    /// Open keyframe menu: which parameter it belongs to, so choosing an item
    /// knows what to key. The menu itself is the shared `ContextMenu`.
    key_menu_param: Option<Param>,
    /// In-flight inline rename of a bin item: the shared `EditableLabel` gives
    /// the edit real caret / selection / clipboard / a11y for free.
    bin_rename: Option<(MediaId, EditableLabel)>,
    /// A bin item being dragged toward the timeline. Armed on a bin press and
    /// promoted to a live drag once the pointer passes the threshold; on release
    /// over a lane it places a clip at the drop position.
    bin_drag: Option<BinDrag>,
    /// `true` while the OS is hovering a file over the window (an external
    /// drag-and-drop), so the bin can invite the drop. Dropping imports it.
    file_hovering: bool,
    /// Vertical scroll offset of the inspector body (px), so the effects stack
    /// can grow past the pane without clipping the actions.
    inspector_scroll: f64,
    /// Vertical scroll offset of the media bin (px). Without it a project with
    /// more sources than fit the pane drew the rest outside it, unreachable by
    /// any means.
    bin_scroll: f64,
    /// Representative (or, under decode, real) thumbnail frames per source.
    thumbnails: crate::thumbnail::ThumbnailCache,
    /// Real audio waveform peaks per source, computed off-thread and cached.
    waveforms: crate::waveform::WaveformCache,
    /// Compute waveforms synchronously (so a single-frame `--shots`/demo render
    /// captures them). The live app leaves this off and computes off-thread.
    warm_waveforms_sync: bool,
    /// Sources whose bytes are **not** on this device — the media reference
    /// replicated fine but [`Hydration::Projected`](crate::media::Hydration)
    /// says the content has not been fetched from the FUSE (or the file is
    /// gone). Refreshed in `sync` from the same `source()` resolution the
    /// thumbnail and waveform passes already do, so the disclosure costs no
    /// extra stat and never runs in `draw`.
    projected_media: HashSet<MediaId>,
    /// The render in flight, if any. The worker encodes off the UI thread and
    /// publishes progress; this polls it each frame. Dropped once its status
    /// is terminal and the dialog has shown the outcome.
    export_job: Option<crate::export::ExportJob>,
    /// A destination-picker round-trip in flight, drained each frame exactly
    /// like the import one. The user chooses where the file lands (and, under
    /// a strict sandbox, that grant is what earns the write).
    export_dest_rx: Option<portal::ExportDestChannel>,
    /// A short, honest line about the export flow itself — choosing a
    /// destination, or why the round-trip failed. Distinct from
    /// [`ExportStatus`], which reports the *render*.
    export_notice: Option<String>,
}

/// A drag that started on a media-bin item and is heading for the timeline.
#[derive(Clone, Copy)]
struct BinDrag {
    media: MediaId,
    /// Pointer position where the press began, to measure the drag threshold.
    origin: Point,
    /// `true` once the pointer has moved past the threshold — only then does a
    /// ghost follow the cursor and a drop place a clip.
    active: bool,
}

/// Pointer travel (logical px) before a bin press becomes a drag toward the
/// timeline, so a plain click never jitters into a placement.
const BIN_DRAG_THRESHOLD: f64 = 6.0;

/// What an open context menu acts on.
#[derive(Clone, Copy, PartialEq)]
enum MenuTarget {
    /// A media-bin item: Add to timeline / Rename / Delete.
    Media(MediaId),
    /// A timeline track header: mute / lock / add / delete track.
    Track(TrackId),
    /// A timeline clip: cut / copy / duplicate / split / delete.
    Clip(ClipId),
    /// The inspector's "Add effect" picker for the selected clip.
    AddEffect,
    /// A key control's menu: what to do with `param`'s curve at the playhead.
    Keyframe(Param),
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
            shot_interaction_applied: false,
            playhead: 10.0,
            zoom: 34.0,
            scroll: 0.0,
            hbar: ui::widget::Scrollbar::horizontal(),
            inspector_bar: ui::widget::Scrollbar::vertical(),
            bin_bar: ui::widget::Scrollbar::vertical(),
            selection: Selection::default(),
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
            #[cfg(feature = "decode")]
            frame_source: None,
            #[cfg(feature = "decode")]
            frame_source_sig: 0,
            #[cfg(feature = "decode")]
            prune_countdown: 0,
            #[cfg(any(feature = "gst", feature = "decode"))]
            monitor: None,
            #[cfg(any(feature = "gst", feature = "decode"))]
            monitor_tried: false,
            #[cfg(any(feature = "gst", feature = "decode"))]
            monitor_stale: true,
            doc_heads: Vec::new(),
            compositor: None,
            compositor_tried: false,
            composited: None,
            drag: None,
            marquee: None,
            chase_playhead: false,
            last_mouse: Point::ZERO,
            hover_clip: None,
            hover_media: None,
            last_click_media: None,
            pending_rename: None,
            seeded: false,
            seed_demo: false,
            initialised_selection: false,
            last_tick: Instant::now(),
            rect: Rect::ZERO,
            actions: ui::ActionHost::new(),
            clipboard: Vec::new(),
            shuttle: 0.0,
            flap_settled: false,
            bin_flap: FlapState::open_when_docked(),
            inspector_flap: FlapState::open_when_docked(),
            flap_tick: Instant::now(),
            bin_menu: ContextMenu::new(),
            menu_target: None,
            collapsed: HashSet::new(),
            title_edit: None,
            marker_rename: None,
            key_menu_param: None,
            bin_rename: None,
            bin_drag: None,
            file_hovering: false,
            inspector_scroll: 0.0,
            bin_scroll: 0.0,
            thumbnails: crate::thumbnail::ThumbnailCache::new(),
            waveforms: crate::waveform::WaveformCache::new(),
            warm_waveforms_sync: false,
            projected_media: HashSet::new(),
            export_job: None,
            export_dest_rx: None,
            export_notice: None,
        }
    }

    /// The hermetic `--shots`/demo build: seeds the bundled sample project on
    /// the first sync. This is the sole source of sample media/clips — the
    /// live app never fabricates a project.
    pub fn new_demo() -> Self {
        let mut app = Self::new();
        app.seed_demo = true;
        // A single-frame demo/`--shots` render can't wait on a background
        // waveform compute, so warm them synchronously for a deterministic shot.
        app.warm_waveforms_sync = true;
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

    /// Docked or overlay for the inspector, from the same shared kernel the
    /// bin uses — one rule for "this window is too narrow for a fixed side
    /// panel", applied at each panel's own breakpoint.
    fn inspector_split(&self) -> SplitLayout {
        SplitLayout::from_width(self.rect.width(), INSPECTOR_BREAKPOINT)
    }

    /// Whether the inspector is on screen right now: always when docked,
    /// only while the flap is open when it is an overlay.
    fn inspector_is_visible(&self, r: &Regions) -> bool {
        !r.inspector_overlay || self.inspector_flap.is_open(self.inspector_split())
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
        // Summons the collapsed inspector. It sits left of Import rather than
        // on the right with the panel it opens, because the right of the
        // topbar is already full at the widths where the panel collapses —
        // and a toggle that does not fit is worse than one on the wrong side.
        let inspector_btn = if self.inspector_split() == SplitLayout::Overlay {
            Rect::new(import_btn.x0 - 42.0, r.y0 + 10.0, import_btn.x0 - 14.0, r.y0 + 38.0)
        } else {
            Rect::ZERO
        };
        let timeline_top = (r.y1 - TIMELINE_H).max(topbar.y1 + 80.0);
        let timeline = Rect::new(r.x0, timeline_top, r.x1, r.y1);
        let mid_top = topbar.y1;
        let mid_bot = timeline.y0;
        // Docked: the inspector holds its column and the preview ends at it.
        // Overlay: the preview reclaims the column and the inspector slides in
        // over it from the right by the eased reveal. `preview_x1` is where
        // the preview (and so the transport) actually ends, which is the whole
        // window's edge in the overlay case — that is what gives the transport
        // room for its five buttons and its timecode on a narrow window.
        let inspector_overlay = self.inspector_split() == SplitLayout::Overlay;
        let (inspector, preview_x1) = if inspector_overlay {
            let slide = self.inspector_flap.slide();
            let x1 = r.x1 + INSPECTOR_W * (1.0 - slide);
            (Rect::new(x1 - INSPECTOR_W, mid_top, x1, r.y1), r.x1)
        } else {
            let i = Rect::new(r.x1 - INSPECTOR_W, mid_top, r.x1, mid_bot);
            (i, i.x0)
        };
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
        let preview = Rect::new(preview_x0, mid_top, preview_x1, mid_bot);
        let transport = Rect::new(preview.x0, preview.y1 - TRANSPORT_H, preview.x1, preview.y1);
        let canvas_area = Rect::new(preview.x0 + 10.0, preview.y0 + 10.0, preview.x1 - 10.0, transport.y0 - 8.0);
        let canvas = fit_aspect(canvas_area, self.project.width.max(16) as f64, self.project.height.max(9) as f64);
        let ruler = Rect::new(timeline.x0 + GUTTER_W, timeline.y0, timeline.x1, timeline.y0 + RULER_H);
        let gutter = Rect::new(timeline.x0, timeline.y0 + RULER_H, timeline.x0 + GUTTER_W, timeline.y1);
        let lanes = Rect::new(timeline.x0 + GUTTER_W, timeline.y0 + RULER_H, timeline.x1, timeline.y1);
        Regions {
            topbar, menu_btn, inspector_btn, export_btn, split_btn, import_btn, bin, preview,
            canvas, transport, inspector, timeline, ruler, gutter, lanes, scrim,
            bin_overlay: overlay, inspector_overlay,
        }
    }

    fn time_to_x(&self, t: f64, lanes: &Rect) -> f64 {
        lanes.x0 + (t - self.scroll) * self.zoom
    }
    fn x_to_time(&self, x: f64, lanes: &Rect) -> f64 {
        (self.scroll + (x - lanes.x0) / self.zoom).max(0.0)
    }

    // ---- horizontal timeline scroll --------------------------------------

    /// How many seconds fit across the lane area at the current zoom.
    fn visible_seconds(&self, r: &Regions) -> f64 {
        (r.lanes.width() / self.zoom.max(0.001)).max(0.0)
    }

    /// The scrollable extent in seconds: the project's content plus a screenful
    /// of headroom past the end, so a clip can be dragged out beyond the last
    /// one instead of hitting a wall at the final frame.
    fn content_seconds(&self, r: &Regions) -> f64 {
        self.project.duration() + self.visible_seconds(r) * 0.5
    }

    /// Furthest left-edge time reachable by scrolling. Zero when everything
    /// already fits.
    fn max_scroll(&self, r: &Regions) -> f64 {
        (self.content_seconds(r) - self.visible_seconds(r)).max(0.0)
    }

    /// The shared scroll geometry for the timeline's horizontal axis, in
    /// **pixels** (the units [`ui::widget::ScrollMetrics`] speaks), so the
    /// toolkit's scrollbar model computes the same thumb every other scroll
    /// surface gets.
    fn h_metrics(&self, r: &Regions) -> ui::widget::ScrollMetrics {
        ui::widget::ScrollMetrics::new(
            self.content_seconds(r) * self.zoom,
            r.lanes.width(),
            self.scroll * self.zoom,
        )
    }

    /// Keep `scroll` inside the content bounds. Called after every zoom,
    /// scroll and project change, so the view can never strand itself past
    /// the end of the timeline.
    fn clamp_scroll(&mut self, r: &Regions) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(r));
    }

    /// Pan by `dt` seconds (positive = later), clamped to the content.
    fn scroll_by(&mut self, dt: f64, r: &Regions) {
        self.scroll += dt;
        self.clamp_scroll(r);
    }

    /// Bring `t` into view, keeping a margin at whichever edge it crossed so
    /// the playhead is never pinned against the frame. Used to follow playback
    /// and to chase a keyboard-moved playhead.
    fn scroll_to_time(&mut self, t: f64, r: &Regions) {
        let visible = self.visible_seconds(r);
        if visible <= 0.0 {
            return;
        }
        let margin = (visible * 0.1).min(1.0);
        if t < self.scroll + margin {
            self.scroll = t - margin;
        } else if t > self.scroll + visible - margin {
            // Jump a page ahead rather than creeping, so playback scrolls in
            // readable steps instead of smearing every frame.
            self.scroll = t - visible * 0.25;
        }
        self.clamp_scroll(r);
    }

    /// Ask the thumbnail cache for the frames every **visible** video clip's
    /// filmstrip will draw this frame, at the source times its cells sit on.
    ///
    /// Lives here rather than in `draw_clip` because requesting needs `&mut
    /// self` and drawing does not. `ensure_strip` is idempotent, so calling it
    /// every frame costs a lookup once the frames are in; off-screen clips are
    /// skipped so a long scrolled timeline does not queue decodes nobody can
    /// see.
    fn request_filmstrips(&mut self, r: &Regions) {
        let mut want: Vec<(MediaId, u8, crate::media::MediaSource, f64, f64, usize)> = Vec::new();
        for (tid, kind, lane) in self.lane_rects(r) {
            if kind != TrackKind::Video {
                continue;
            }
            for clip in self.project.clips_on(tid) {
                let cr = self.clip_rect(clip, &lane, r);
                if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
                    continue;
                }
                let inner = clip_inner_rect(cr);
                if inner.height() <= 4.0 || inner.width() <= 4.0 {
                    continue;
                }
                let (_, n) = filmstrip_cells(inner);
                let Some(m) = self.project.media(clip.media) else { continue };
                want.push((
                    clip.media,
                    m.palette,
                    m.source(),
                    clip.in_point,
                    clip.out_point,
                    n as usize,
                ));
            }
        }
        for (id, palette, src, in_point, out_point, cells) in want {
            self.thumbnails
                .ensure_strip(id, palette, &src, in_point, out_point, cells);
        }
    }

    /// The horizontal scrollbar's track rect, or `None` when the timeline
    /// content already fits. Overlaid on the bottom of the lane area, inset
    /// like every other toolkit scroll surface.
    fn hscroll_track(&self, r: &Regions) -> Option<Rect> {
        if !self.h_metrics(r).overflows() {
            return None;
        }
        let y1 = r.timeline.y1 - SCROLLBAR_INSET;
        Some(Rect::new(
            r.lanes.x0 + SCROLLBAR_INSET,
            y1 - SCROLLBAR_THICKNESS,
            r.lanes.x1 - SCROLLBAR_INSET,
            y1,
        ))
    }

    /// Set the scroll offset from a scrollbar interaction, which works in
    /// pixels; convert back to the seconds `scroll` is measured in.
    fn set_scroll_px(&mut self, px: f64, r: &Regions) {
        self.scroll = px / self.zoom.max(0.001);
        self.clamp_scroll(r);
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

    /// The mute pill rect on a track header (shared by draw + hit-test).
    fn mute_pill_rect(&self, header: Rect) -> Rect {
        Rect::new(header.x1 - 26.0, header.center().y - 9.0, header.x1 - 8.0, header.center().y + 9.0)
    }

    /// The lock pill rect, immediately left of the mute pill.
    fn lock_pill_rect(&self, header: Rect) -> Rect {
        let m = self.mute_pill_rect(header);
        Rect::new(m.x0 - 22.0, m.y0, m.x1 - 22.0, m.y1)
    }

    /// The solo pill rect, immediately left of the lock pill.
    fn solo_pill_rect(&self, header: Rect) -> Rect {
        let l = self.lock_pill_rect(header);
        Rect::new(l.x0 - 22.0, l.y0, l.x1 - 22.0, l.y1)
    }

    /// The two "add track" buttons in the gutter footer, if there is room below
    /// the last lane: `(kind, rect)` for a Video and an Audio add.
    fn add_track_buttons(&self, r: &Regions) -> Vec<(TrackKind, Rect)> {
        let lanes = self.lane_rects(r);
        let y = lanes
            .last()
            .map(|(_, _, l)| l.y1 + LANE_GAP + 4.0)
            .unwrap_or(r.gutter.y0 + 6.0);
        if y + 24.0 > r.timeline.y1 {
            return Vec::new();
        }
        let pad = 8.0;
        let w = (r.gutter.width() - pad * 2.0 - 6.0) / 2.0;
        let vbtn = Rect::new(r.gutter.x0 + pad, y, r.gutter.x0 + pad + w, y + 24.0);
        let abtn = Rect::new(vbtn.x1 + 6.0, y, vbtn.x1 + 6.0 + w, y + 24.0);
        vec![(TrackKind::Video, vbtn), (TrackKind::Audio, abtn)]
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

    /// The transport scrubber's track. `Rect::ZERO` when the transport is too
    /// narrow to hold one — below roughly a 500px window the old unclamped
    /// arithmetic produced an *inverted* rect (x0 > x1) and drew the knob
    /// outside its own track. Capped at [`MAX_SCRUBBER_W`] so a very wide
    /// window does not stretch a 6px knob across three thousand pixels of bar.
    fn scrubber_rect(&self, r: &Regions) -> Rect {
        let left = r.transport.x0 + 220.0;
        let right = r.transport.x1 - 16.0;
        if right - left < MIN_SCRUBBER_W {
            return Rect::ZERO;
        }
        let w = (right - left).min(MAX_SCRUBBER_W);
        let y0 = r.transport.y0 + 38.0;
        Rect::new(left, y0, left + w, y0 + 8.0)
    }

    /// Whether the transport currently has room for a scrubber.
    fn has_scrubber(&self, r: &Regions) -> bool {
        self.scrubber_rect(r).width() > 1.0
    }

    /// Whether this source's bytes are absent from this device. Read from the
    /// set `sync` resolves, so a `draw` pass never touches the filesystem.
    fn media_is_projected(&self, mid: MediaId) -> bool {
        self.projected_media.contains(&mid)
    }

    /// Whether this source's bytes are present but are not usable media.
    ///
    /// Import refuses such a file now, but a document written before that
    /// refusal existed still carries the entry, and it renders from whatever
    /// was probed at the time — which for a web page saved as `.mp4` is
    /// nothing at all. Derived from what the item actually carries (no
    /// duration, no dimensions) or from a decode that has since been proven
    /// impossible, so it cannot drift from what the rest of the app knows.
    fn media_is_unreadable(&self, mid: MediaId) -> bool {
        if self.media_is_projected(mid) {
            return false; // absent bytes are a different, recoverable state
        }
        let Some(m) = self.project.media(mid) else { return false };
        if self.thumbnails.is_undecodable(mid) {
            return true;
        }
        m.duration <= 0.0 && m.width == 0 && m.height == 0
    }

    /// The bin row's metadata line. Unknown is written as unknown: a source
    /// that carries no dimensions or no length must not be rendered as
    /// "0p · 0:00", which reads as a probed fact and is not one.
    fn bin_subtitle(&self, m: &MediaItem) -> String {
        let dur = if m.duration > 0.0 {
            clock(m.duration)
        } else {
            "length unknown".to_string()
        };
        match m.kind {
            MediaKind::Audio => format!("Audio · {dur}"),
            // A title is drawn at whatever the project frame is, so it has
            // no size of its own. That is not the same as "we could not
            // read the size", which is what an unreadable file gets.
            MediaKind::Text => format!("Title · {dur}"),
            _ if m.height == 0 => format!("size unknown · {dur}"),
            _ => format!("{}p · {}", m.height, dur),
        }
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

    /// Row pitch of the media bin list, and where the first row starts.
    const BIN_ROW_H: f64 = 56.0;
    const BIN_LIST_TOP: f64 = 40.0;

    /// The bin header's "+ Title" button, which makes a title source, places
    /// it at the playhead and opens its words for typing.
    fn add_title_rect(&self, r: &Regions) -> Rect {
        let x1 = r.bin.x0 + 152.0;
        Rect::new(x1 - 62.0, r.bin.y0 + 6.0, x1, r.bin.y0 + 26.0)
    }

    fn bin_item_rects(&self, r: &Regions) -> Vec<(MediaId, Rect)> {
        let mut out = Vec::new();
        let mut y = r.bin.y0 + Self::BIN_LIST_TOP - self.bin_scroll;
        for mid in self.bin_order() {
            let rect = Rect::new(r.bin.x0 + 8.0, y, r.bin.x1 - 8.0, y + Self::BIN_ROW_H - 6.0);
            out.push((mid, rect));
            y += Self::BIN_ROW_H;
        }
        out
    }

    /// Whether the media bin is on screen: docked, or an open overlay flap.
    fn bin_is_visible(&self, r: &Regions) -> bool {
        !r.bin_overlay || self.bin_flap.is_open(self.bin_layout())
    }

    /// The bin's visible list viewport — everything below the header band.
    fn bin_viewport(&self, r: &Regions) -> Rect {
        Rect::new(r.bin.x0, r.bin.y0 + 30.0, r.bin.x1, r.bin.y1)
    }

    /// Total height the bin list wants, in px.
    fn bin_content_height(&self) -> f64 {
        self.project.media.len() as f64 * Self::BIN_ROW_H + (Self::BIN_LIST_TOP - 30.0) + 8.0
    }

    fn bin_metrics(&self, r: &Regions) -> ui::widget::ScrollMetrics {
        ui::widget::ScrollMetrics::new(
            self.bin_content_height(),
            self.bin_viewport(r).height(),
            self.bin_scroll,
        )
    }

    fn bin_max_scroll(&self, r: &Regions) -> f64 {
        self.bin_metrics(r).max_scroll()
    }

    /// The bin's vertical scrollbar track, or `None` when the list fits.
    fn bin_scroll_track(&self, r: &Regions) -> Option<Rect> {
        if !self.bin_metrics(r).overflows() {
            return None;
        }
        let v = self.bin_viewport(r);
        let x1 = v.x1 - SCROLLBAR_INSET;
        Some(Rect::new(
            x1 - SCROLLBAR_THICKNESS,
            v.y0 + SCROLLBAR_INSET,
            x1,
            v.y1 - SCROLLBAR_INSET,
        ))
    }

    fn inspector_metrics(&self, r: &Regions) -> ui::widget::ScrollMetrics {
        ui::widget::ScrollMetrics::new(
            r.inspector.height() + self.inspector_max_scroll(r),
            r.inspector.height(),
            self.inspector_scroll,
        )
    }

    /// The inspector's vertical scrollbar track, or `None` when it all fits.
    /// Replaces a painted-only thumb that had no hit-test at all — the pane's
    /// EFFECTS section was clipped off the bottom and unreachable.
    fn inspector_scroll_track(&self, r: &Regions) -> Option<Rect> {
        if !self.inspector_metrics(r).overflows() {
            return None;
        }
        let x1 = r.inspector.x1 - SCROLLBAR_INSET;
        Some(Rect::new(
            x1 - SCROLLBAR_THICKNESS,
            r.inspector.y0 + SCROLLBAR_INSET,
            x1,
            r.inspector.y1 - SCROLLBAR_INSET,
        ))
    }

    /// Track rect + metrics + current offset for one scrollable pane, so the
    /// press / drag / hover paths do not each re-derive them.
    fn scroll_pane(&self, pane: ScrollPane, r: &Regions) -> Option<(Rect, ui::widget::ScrollMetrics)> {
        match pane {
            ScrollPane::Timeline => self.hscroll_track(r).map(|t| (t, self.h_metrics(r))),
            ScrollPane::Inspector => {
                self.inspector_scroll_track(r).map(|t| (t, self.inspector_metrics(r)))
            }
            ScrollPane::Bin => self.bin_scroll_track(r).map(|t| (t, self.bin_metrics(r))),
        }
    }

    fn scroll_bar_mut(&mut self, pane: ScrollPane) -> &mut ui::widget::Scrollbar {
        match pane {
            ScrollPane::Timeline => &mut self.hbar,
            ScrollPane::Inspector => &mut self.inspector_bar,
            ScrollPane::Bin => &mut self.bin_bar,
        }
    }

    fn scroll_bar(&self, pane: ScrollPane) -> &ui::widget::Scrollbar {
        match pane {
            ScrollPane::Timeline => &self.hbar,
            ScrollPane::Inspector => &self.inspector_bar,
            ScrollPane::Bin => &self.bin_bar,
        }
    }

    /// Apply an absolute offset (in the bar's own px units) to `pane`.
    fn set_scroll_offset(&mut self, pane: ScrollPane, offset: f64, r: &Regions) {
        match pane {
            ScrollPane::Timeline => self.set_scroll_px(offset, r),
            ScrollPane::Inspector => {
                let max = self.inspector_max_scroll(r);
                self.inspector_scroll = offset.clamp(0.0, max);
            }
            ScrollPane::Bin => {
                let max = self.bin_max_scroll(r);
                self.bin_scroll = offset.clamp(0.0, max);
            }
        }
    }

    /// Every pane that currently shows a bar, for the hover / press sweeps.
    const SCROLL_PANES: [ScrollPane; 3] =
        [ScrollPane::Inspector, ScrollPane::Bin, ScrollPane::Timeline];

    /// Keep the audio monitor in step with the transport.
    ///
    /// One reconcile per frame rather than a call at every site that moves
    /// the playhead: the transport is set from a dozen places (a key, a
    /// scrub, a click on the ruler, reaching the end), and a monitor that
    /// only heard about some of them would drift out of step with the
    /// picture in exactly the cases the user notices.
    ///
    /// The monitor is *not* re-seeked for small differences while playing.
    /// Its clock is the sample count and the playhead's is wall time, so
    /// they wander apart by a few milliseconds; correcting that every frame
    /// would restart the mix continuously. Only a real jump moves it.
    #[cfg(any(feature = "gst", feature = "decode"))]
    fn pump_monitor(&mut self) {
        if !self.monitor_tried {
            self.monitor_tried = true;
            // The demo/shots build must make no sound and open no pipeline:
            // a capture has to stay hermetic.
            if !self.seed_demo {
                self.monitor = Some(crate::audio::AudioMonitor::new(
                    &self.project,
                    crate::audio::open_output(),
                    Box::new(crate::audio::DecodedPcm::new(&self.project)),
                ));
                self.monitor_stale = false;
            }
        }
        let Some(m) = self.monitor.as_ref() else { return };
        if self.monitor_stale {
            m.set_project(&self.project);
            self.monitor_stale = false;
        }
        // A jump this big is a scrub or a cut, not clock wander.
        const RESYNC: f64 = 0.35;
        if self.playing {
            if !m.is_playing() {
                m.play(self.playhead);
            } else if (m.position() - self.playhead).abs() > RESYNC {
                m.seek(self.playhead);
            }
        } else {
            if m.is_playing() {
                m.pause();
            }
            if (m.position() - self.playhead).abs() > 1e-6 {
                m.seek(self.playhead);
            }
        }
    }

    // ---- playback --------------------------------------------------------
    fn advance_playback(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64();
        self.last_tick = now;
        let dur = self.project.duration();
        if self.playing || self.shuttle != 0.0 {
            // A moving playhead pulls the view along with it.
            self.chase_playhead = true;
        }
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

    /// Reconcile both side flaps with the current width and advance their
    /// eased reveal using real elapsed time. Called once at the top of `draw`.
    fn advance_flap(&mut self) {
        let layout = self.bin_layout();
        let insp = self.inspector_split();
        self.bin_flap.reconcile(layout);
        self.inspector_flap.reconcile(insp);
        // First settled frame: snap to the width's natural state so a cold
        // narrow launch starts collapsed (no open→closed flash) and a cold
        // wide launch starts docked-open.
        if !self.flap_settled {
            self.bin_flap.snap(layout == SplitLayout::Docked);
            self.inspector_flap.snap(insp == SplitLayout::Docked);
            self.flap_settled = true;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.flap_tick).as_secs_f64();
        self.flap_tick = now;
        self.bin_flap.tick(layout, dt);
        self.inspector_flap.tick(insp, dt);
    }

    /// Toggle the media bin flap (the topbar hamburger / `B` shortcut).
    fn toggle_bin(&mut self) {
        self.bin_flap.toggle(self.bin_layout());
    }

    /// Toggle the inspector flap (the topbar toggle / `Tab`). On a wide
    /// window the inspector is docked and this collapses it away, which is
    /// the same affordance the bin's hamburger offers there.
    fn toggle_inspector(&mut self) {
        self.inspector_flap.toggle(self.inspector_split());
    }

    /// Bring the inspector on screen if it is a closed overlay. Called when
    /// something selects a clip, so "click a clip, edit it" does not need a
    /// second click on a toggle the user has to find first.
    fn reveal_inspector(&mut self) {
        if self.inspector_split() == SplitLayout::Overlay {
            self.inspector_flap.open();
        }
    }

    /// Whether either flap is animating and the host should keep painting.
    fn flap_animating(&self) -> bool {
        self.bin_flap.is_animating(self.bin_layout())
            || self.inspector_flap.is_animating(self.inspector_split())
    }

    fn toggle_play(&mut self) {
        self.playing = !self.playing;
        self.last_tick = Instant::now();
        if self.playing && self.playhead >= self.project.duration() {
            self.playhead = 0.0;
        }
        self.chase_playhead = true;
    }

    fn step_frame(&mut self, dir: f64) {
        let step = frame_step(self.project.fps);
        self.playhead = (self.playhead + dir * step).clamp(0.0, self.project.duration());
        self.playing = false;
        self.shuttle = 0.0;
        self.chase_playhead = true;
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

    // ---- selection ------------------------------------------------------

    /// The primary (anchor) selected clip — what the inspector edits.
    fn selected(&self) -> Option<ClipId> {
        self.selection.primary()
    }

    /// The selected clips that may actually be edited, in a deterministic
    /// order (by start time, then id) so a multi-clip edit dispatches the same
    /// sequence of actions on every peer. Clips on locked tracks are filtered
    /// out here — that is the single choke point every timeline edit passes
    /// through, so a lock cannot be routed around by a shortcut.
    fn editable_selection(&self) -> Vec<ClipId> {
        let mut v: Vec<(f64, ClipId)> = self
            .selection
            .ids()
            .iter()
            .filter(|id| self.project.clip_editable(**id))
            .filter_map(|id| self.project.clip(*id).map(|c| (c.start, c.id)))
            .collect();
        v.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        v.into_iter().map(|(_, id)| id).collect()
    }

    /// True when the selection holds at least one clip that a lock forbids
    /// editing — the cue the timeline shows instead of silently doing nothing.
    fn selection_has_locked(&self) -> bool {
        self.selection
            .ids()
            .iter()
            .any(|id| !self.project.clip_editable(*id))
    }

    /// Select every clip on the timeline (Ctrl+A), in display order.
    fn select_all(&mut self) {
        let ids: Vec<ClipId> = self
            .project
            .display_tracks()
            .iter()
            .flat_map(|t| self.project.clips_on(t.id))
            .map(|c| c.id)
            .collect();
        self.selection.set(ids);
    }

    /// Split every editable selected clip at the playhead. Only clips whose
    /// span actually covers the playhead split; the rest are left alone. All
    /// the splits dispatch inside one event, so they collapse into a single
    /// undo boundary (see [`ui::ActionHost::dispatch`]).
    fn split_selected(&mut self, doc: &mut AutoCommit) {
        let mut right_halves = Vec::new();
        for id in self.editable_selection() {
            let Some(rid) = self.project.split_clip(id, self.playhead) else { continue };
            let left = self.project.clip(id).cloned();
            let right = self.project.clip(rid).cloned();
            if let (Some(left), Some(right)) = (left, right) {
                self.actions.dispatch(
                    doc,
                    VIDEO_EDITOR_ACTION_TYPE,
                    &split_clip_action(&left, &right),
                );
            }
            right_halves.push(rid);
        }
        // The right-hand halves become the selection, matching the
        // single-clip behaviour (S leaves you on the tail).
        if !right_halves.is_empty() {
            self.selection.set(right_halves);
        }
    }

    /// Lift every editable selected clip, leaving the gaps where they were.
    fn delete_selected(&mut self, doc: &mut AutoCommit) {
        let ids = self.editable_selection();
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.project.delete_clip(id);
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &delete_clip_action(&id));
        }
        self.selection.clear();
    }

    /// Delete the selected clips and close the gaps, shifting later clips on
    /// the same track left. Each ripple — the delete plus every shifted clip —
    /// is carried in one action, and the whole selection's worth of ripples
    /// dispatches inside one event, so it lands as a single undo boundary and
    /// replays atomically on a peer. Deleting right-to-left keeps each
    /// remaining clip's start valid while the earlier ones are still there.
    fn ripple_delete_selected(&mut self, doc: &mut AutoCommit) {
        let mut ids = self.editable_selection();
        if ids.is_empty() {
            return;
        }
        ids.reverse();
        for id in ids {
            let moved_ids = self.project.ripple_delete(id);
            let moved: Vec<Clip> = moved_ids
                .iter()
                .filter_map(|m| self.project.clip(*m).cloned())
                .collect();
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &ripple_delete_action(&id, &moved));
        }
        self.selection.clear();
    }

    /// Copy the whole selection to the clipboard (no CRDT change). A locked
    /// track only forbids *edits*, so its clips can still be copied.
    fn copy_selected(&mut self) {
        let ids: Vec<ClipId> = self.selection.ids().to_vec();
        let mut clips: Vec<Clip> = ids
            .iter()
            .filter_map(|id| self.project.clip(*id).cloned())
            .collect();
        clips.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        if !clips.is_empty() {
            self.clipboard = clips;
        }
    }

    /// Ctrl+X: copy the selection, then lift it. Both halves happen in one
    /// event, so the removal is one undo boundary.
    fn cut_selected(&mut self, doc: &mut AutoCommit) {
        self.copy_selected();
        self.delete_selected(doc);
    }

    /// Paste the clipboard at the playhead. The earliest copied clip lands on
    /// the playhead and the rest keep their relative offsets, so a copied
    /// block pastes as a block. Each clip goes to a *sensible* track: the one
    /// it came from when that still exists and is unlocked, otherwise the
    /// selected clip's track, otherwise any unlocked track of the right kind
    /// (created if the project has none). The pasted clips become the
    /// selection.
    fn paste_clip(&mut self, doc: &mut AutoCommit) {
        let src = self.clipboard.clone();
        if src.is_empty() {
            return;
        }
        let base = src
            .iter()
            .map(|c| c.start)
            .fold(f64::INFINITY, f64::min);
        let near = self.selected().and_then(|s| self.project.clip(s)).map(|c| c.track);
        let mut pasted = Vec::new();
        for clip in src {
            let kind = self
                .project
                .media(clip.media)
                .map(|m| if m.kind.is_visual() { TrackKind::Video } else { TrackKind::Audio })
                .unwrap_or(TrackKind::Video);
            // The clip's own track first (that is where the user copied it
            // from), then the current selection's track, then any unlocked
            // track of the right kind — creating one only as a last resort.
            let track = self
                .project
                .placement_track(kind, Some(clip.track))
                .or_else(|| self.project.placement_track(kind, near))
                .unwrap_or_else(|| self.ensure_track_of_kind(doc, kind));
            let start = snap_to_frame((self.playhead + (clip.start - base)).max(0.0), self.project.fps);
            let id = self.project.paste_clip(clip, track, start);
            self.dispatch_clip(doc, id);
            pasted.push(id);
        }
        if !pasted.is_empty() {
            self.selection.set(pasted);
        }
    }

    /// Ctrl+D: drop a copy of the selection immediately after it, on the same
    /// tracks, and select the copies. One event → one undo boundary.
    fn duplicate_selected(&mut self, doc: &mut AutoCommit) {
        let ids = self.editable_selection();
        if ids.is_empty() {
            return;
        }
        let clips: Vec<Clip> = ids
            .iter()
            .filter_map(|id| self.project.clip(*id).cloned())
            .collect();
        // Offset the whole block by its own span so the copies butt up
        // against the originals instead of overlapping them.
        let lo = clips.iter().map(|c| c.start).fold(f64::INFINITY, f64::min);
        let hi = clips.iter().map(|c| c.end()).fold(0.0_f64, f64::max);
        let offset = (hi - lo).max(crate::model::MIN_CLIP);
        let mut copies = Vec::new();
        for clip in clips {
            let track = clip.track;
            let start = snap_to_frame(clip.start + offset, self.project.fps);
            let id = self.project.paste_clip(clip, track, start);
            self.dispatch_clip(doc, id);
            copies.push(id);
        }
        self.selection.set(copies);
    }

    /// Set the in-point of the primary selected clip to the playhead (`I`):
    /// trims the head so the clip starts at the current frame. A single-clip
    /// trim by nature — it acts on the anchor, and only when its track allows
    /// edits.
    fn set_in_at_playhead(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let delta = self.project.clip(id).map(|c| self.playhead - c.start).unwrap_or(0.0);
        if delta.abs() < 1e-6 {
            return;
        }
        self.project.trim_in(id, delta);
        self.dispatch_clip(doc, id);
    }

    /// Set the out-point of the primary selected clip to the playhead (`O`):
    /// trims the tail so the clip ends at the current frame.
    fn set_out_at_playhead(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let delta = self.project.clip(id).map(|c| self.playhead - c.end()).unwrap_or(0.0);
        if delta.abs() < 1e-6 {
            return;
        }
        self.project.trim_out(id, delta);
        self.dispatch_clip(doc, id);
    }

    /// Nudge every editable selected clip by `dt` seconds (arrow keys).
    /// Snaps each result to the frame grid. One event → one undo boundary,
    /// so a multi-clip nudge is a single Ctrl+Z.
    fn nudge_selected(&mut self, doc: &mut AutoCommit, dt: f64) {
        for id in self.editable_selection() {
            let new_start = self
                .project
                .clip(id)
                .map(|c| snap_to_frame((c.start + dt).max(0.0), self.project.fps))
                .unwrap_or(0.0);
            self.project.move_clip(id, new_start);
            self.dispatch_clip(doc, id);
        }
    }

    /// Apply `edit` to the selected clip and dispatch a whole-clip write. The
    /// single write-through path for a *discrete* per-clip field edit (a
    /// transition / palette click, or a keyboard-driven grade) — each is one
    /// undo step. Continuous slider drags use [`Self::preview_edit`] +
    /// [`Self::commit_drag`] instead, so a drag is one action, not one a frame.
    fn edit_selected(&mut self, doc: &mut AutoCommit, edit: impl FnOnce(&mut Clip)) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        if let Some(c) = self.project.clip_mut(id) {
            edit(c);
        }
        self.dispatch_clip(doc, id);
    }

    /// Apply `edit` to the selected clip **in memory only** — the live drag
    /// preview. No action is dispatched; the final value is committed once when
    /// the drag ends (see [`Self::commit_drag`]).
    fn preview_edit(&mut self, edit: impl FnOnce(&mut Clip)) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        if let Some(c) = self.project.clip_mut(id) {
            edit(c);
        }
    }

    /// The media item behind the selected clip, if any.
    fn selected_media_item(&self) -> Option<&MediaItem> {
        let id = self.selected()?;
        let clip = self.project.clip(id)?;
        self.project.media(clip.media)
    }

    /// Apply `edit` to the media item behind the selected clip, in memory
    /// only — the live preview half of a title-property drag. The two title
    /// knobs edit the *source*, not the clip, so they cannot go through
    /// [`Self::preview_edit`] and commit a different action on release.
    fn preview_media_edit(&mut self, edit: impl FnOnce(&mut MediaItem)) {
        let Some(id) = self.selected() else { return };
        if !self.project.clip_editable(id) {
            return;
        }
        let Some(mid) = self.project.clip(id).map(|c| c.media) else { return };
        if let Some(m) = self.project.media.iter_mut().find(|m| m.id == mid) {
            edit(m);
        }
    }

    /// Dispatch a whole-media write for the selected clip's source.
    fn dispatch_selected_media(&mut self, doc: &mut AutoCommit) {
        if let Some(m) = self.selected_media_item().cloned() {
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&m));
        }
    }

    /// Apply `edit` to the selected clip's title styling and dispatch it as
    /// one media write. A no-op on a clip whose source is not a title.
    fn edit_title(&mut self, doc: &mut AutoCommit, edit: impl FnOnce(&mut TextSource)) {
        let has_text = self.selected_media_item().map(|m| m.text.is_some()).unwrap_or(false);
        if !has_text {
            return;
        }
        self.preview_media_edit(|m| {
            let mut t = m.text.clone().unwrap_or_default();
            edit(&mut t);
            // The bin row names a title by its first line, so keep the two in
            // step rather than leaving a renamed title labelled by old words.
            m.name = t.content.lines().next().unwrap_or("Title").trim().to_string();
            if m.name.is_empty() {
                m.name = "Title".into();
            }
            m.text = Some(t);
        });
        self.dispatch_selected_media(doc);
    }

    /// What a ranged control currently shows. `None` when the control has
    /// nothing behind it — a title knob with no title selected.
    ///
    /// Animatable channels read through [`Clip::param_at`], the same door the
    /// compositor uses, so a keyframed slider tracks the playhead instead of
    /// showing a constant the picture is ignoring.
    fn num_value(&self, f: NumField) -> Option<f64> {
        let clip = self.selected().and_then(|id| self.project.clip(id))?;
        if let Some(p) = f.param() {
            return Some(clip.param_at(p, self.playhead));
        }
        Some(match f {
            NumField::CropLeft => clip.transform.crop_left,
            NumField::CropRight => clip.transform.crop_right,
            NumField::CropTop => clip.transform.crop_top,
            NumField::CropBottom => clip.transform.crop_bottom,
            NumField::Speed => clip.speed.rate(),
            NumField::TitleSize => self.selected_media_item()?.text.as_ref()?.size,
            NumField::TitleBackdrop => self.selected_media_item()?.text.as_ref()?.backdrop,
            _ => return None,
        })
    }

    /// Write a ranged control's value in memory (the live drag preview).
    ///
    /// An animated parameter is **keyed** at the playhead rather than having
    /// its dead constant written: once a parameter has a curve the constant
    /// is never read again, so writing it would look like the slider had
    /// stopped working. This is also what makes a second keyframe cost one
    /// drag — key it, move the playhead, drag.
    /// Write an animatable parameter in memory: a key at the playhead when it
    /// is animated, its constant when it is not.
    ///
    /// Every parameter-backed slider goes through here, the original grade and
    /// level rows included. They used to write their constant field directly,
    /// which meant that once a parameter was keyed its slider silently stopped
    /// having any effect — the curve overrides the constant everywhere.
    fn preview_param(&mut self, param: Param, v: f64) {
        let at = self.playhead;
        self.preview_edit(move |c| c.set_param_at(param, at, v));
    }

    fn set_num(&mut self, f: NumField, v: f64) {
        if let Some(p) = f.param() {
            self.preview_param(p, v);
            return;
        }
        let at = self.playhead;
        let _ = at;
        match f {
            NumField::CropLeft => self.preview_edit(move |c| c.transform.crop_left = v),
            NumField::CropRight => self.preview_edit(move |c| c.transform.crop_right = v),
            NumField::CropTop => self.preview_edit(move |c| c.transform.crop_top = v),
            NumField::CropBottom => self.preview_edit(move |c| c.transform.crop_bottom = v),
            NumField::Speed => {
                self.preview_edit(move |c| {
                    let rev = c.speed.reverse;
                    c.set_speed(v, rev);
                });
            }
            NumField::TitleSize => self.preview_media_edit(move |m| {
                if let Some(t) = m.text.as_mut() {
                    t.size = v;
                }
            }),
            NumField::TitleBackdrop => self.preview_media_edit(move |m| {
                if let Some(t) = m.text.as_mut() {
                    t.backdrop = v;
                }
            }),
            _ => {}
        }
    }

    /// The three-state reading a key control shows for `param` on the
    /// selection right now.
    fn key_state(&self, param: Param) -> KeyState {
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else {
            return KeyState::Off;
        };
        if !clip.anim.is_animated(param) {
            KeyState::Off
        } else if clip.has_key_at(param, self.playhead) {
            KeyState::OnKey
        } else {
            KeyState::Between
        }
    }

    /// Plant a key for `param` at the playhead, holding whatever the
    /// parameter reads there. The first key never moves the picture — it is
    /// the *second* one that makes anything animate — so keying is safe to
    /// try, which is what makes the control learnable.
    fn key_here(&mut self, doc: &mut AutoCommit, param: Param) {
        let at = self.playhead;
        self.edit_selected(doc, move |c| {
            let now = c.param_at(param, at);
            let interp = c.key_interp_at(param, at).unwrap_or_default();
            c.set_key_at(param, at, now, interp);
        });
    }

    /// Open the keyframe menu for `param`. What it offers depends on whether
    /// a key sits on the playhead: with one there the menu shapes or removes
    /// it, without one it plants one. Both ways it can step to the
    /// neighbouring keys and clear the whole curve.
    fn open_key_menu(&mut self, param: Param, at: Point) {
        let clip = self.selected().and_then(|id| self.project.clip(id)).cloned();
        let Some(clip) = clip else { return };
        let on_key = clip.has_key_at(param, self.playhead);
        let interp = clip.key_interp_at(param, self.playhead).unwrap_or_default();
        let mut menu = ContextMenu::new();
        if on_key {
            menu.push(MenuItem::action("Remove key here", || {}));
            menu.push(MenuItem::separator());
            for i in Interp::ALL {
                // The shape in force is ticked, so the menu reports the key's
                // state as well as offering to change it.
                let mark = if i == interp { "✓ " } else { "    " };
                menu.push(MenuItem::action(format!("{mark}{}", i.label()), || {}));
            }
        } else {
            menu.push(MenuItem::action("Add key here", || {}));
        }
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Previous key", || {}));
        menu.push(MenuItem::action("Next key", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Remove all keys", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.key_menu_param = Some(param);
        self.menu_target = Some(MenuTarget::Keyframe(param));
    }

    /// Act on a keyframe-menu choice. The index scheme mirrors what
    /// [`Self::open_key_menu`] built for this playhead position.
    /// Indices count separators, because that is what `ContextMenu::item_at`
    /// reports. The two shapes the menu takes are laid out here beside the
    /// builder that produces them so they cannot drift apart.
    fn key_menu_choose(&mut self, doc: &mut AutoCommit, param: Param, idx: usize) {
        self.key_menu_param = None;
        let at = self.playhead;
        let on_key = self
            .selected()
            .and_then(|id| self.project.clip(id))
            .map(|c| c.has_key_at(param, at))
            .unwrap_or(false);
        if on_key {
            // 0 remove · 1 sep · 2..4 shapes · 5 sep · 6 prev · 7 next · 8 sep · 9 clear
            match idx {
                0 => self.edit_selected(doc, move |c| {
                    c.remove_key_at(param, at);
                }),
                2..=4 => {
                    let shape = Interp::ALL[idx - 2];
                    self.edit_selected(doc, move |c| {
                        c.set_interp_at(param, at, shape);
                    })
                }
                6 => self.jump_to_key(false),
                7 => self.jump_to_key(true),
                9 => self.edit_selected(doc, move |c| c.anim.clear(param)),
                _ => {}
            }
        } else {
            // 0 add · 1 sep · 2 prev · 3 next · 4 sep · 5 clear
            match idx {
                0 => self.key_here(doc, param),
                2 => self.jump_to_key(false),
                3 => self.jump_to_key(true),
                5 => self.edit_selected(doc, move |c| c.anim.clear(param)),
                _ => {}
            }
        }
    }

    /// Move the playhead to the selected clip's next / previous keyframe.
    /// Presentation, so no action: the playhead is this reader's, not the
    /// document's.
    fn jump_to_key(&mut self, forward: bool) {
        let Some(clip) = self.selected().and_then(|id| self.project.clip(id)) else { return };
        let to = if forward {
            clip.next_key_after(self.playhead)
        } else {
            clip.prev_key_before(self.playhead)
        };
        if let Some(t) = to {
            self.playhead = t.clamp(0.0, self.project.duration());
            self.playing = false;
            self.shuttle = 0.0;
            self.chase_playhead = true;
        }
    }

    /// Move the playhead to the next / previous marked instant, across the
    /// project's markers and every clip's alike.
    fn jump_to_marker(&mut self, forward: bool) {
        let to = if forward {
            self.project.next_marker_at(self.playhead)
        } else {
            self.project.prev_marker_at(self.playhead)
        };
        if let Some(t) = to {
            self.playhead = t.clamp(0.0, self.project.duration());
            self.playing = false;
            self.shuttle = 0.0;
            self.chase_playhead = true;
        }
    }

    /// Add a timeline marker at the playhead and open it for naming, so the
    /// marker is never left as an anonymous tick the user has to hunt for.
    fn add_marker_here(&mut self, doc: &mut AutoCommit) {
        let mut next = self.project.clone();
        let palette = (next.markers.len() as u8 % 6) + 1;
        let id = next.add_marker(self.playhead, "Marker", palette);
        self.project = next.clone();
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &set_project_meta_action(&next));
        self.collapsed.remove(&Section::Markers);
        self.begin_marker_rename(MarkerRef { id, clip: None, at: self.playhead });
    }

    fn begin_marker_rename(&mut self, m: MarkerRef) {
        let name = self
            .project
            .all_markers()
            .into_iter()
            .find(|(_, mk, _)| mk.id == m.id)
            .map(|(_, mk, _)| mk.name.clone())
            .unwrap_or_default();
        let mut label = EditableLabel::new(&name).with_font_size(12.0).with_id("marker_rename");
        label.begin_edit();
        self.marker_rename = Some((m, label));
    }

    /// Commit a marker rename through whichever action owns that marker: a
    /// project-meta write for a timeline marker, a whole-clip write for one
    /// pinned to a clip.
    fn commit_marker_rename(&mut self, doc: &mut AutoCommit, m: MarkerRef, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        match m.clip {
            None => {
                let mut next = self.project.clone();
                let Some(mk) = next.marker_mut(m.id) else { return };
                if mk.name == name {
                    return;
                }
                mk.name = name;
                self.project = next.clone();
                self.actions
                    .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &set_project_meta_action(&next));
            }
            Some(cid) => {
                if let Some(c) = self.project.clip_mut(cid) {
                    let Some(mk) = c.markers.iter_mut().find(|mk| mk.id == m.id) else { return };
                    if mk.name == name {
                        return;
                    }
                    mk.name = name;
                }
                self.dispatch_clip(doc, cid);
            }
        }
    }

    fn delete_marker(&mut self, doc: &mut AutoCommit, m: MarkerRef) {
        match m.clip {
            None => {
                let mut next = self.project.clone();
                if !next.remove_marker(m.id) {
                    return;
                }
                self.project = next.clone();
                self.actions
                    .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &set_project_meta_action(&next));
            }
            Some(cid) => {
                let gone = self
                    .project
                    .clip_mut(cid)
                    .map(|c| c.remove_marker(m.id))
                    .unwrap_or(false);
                if gone {
                    self.dispatch_clip(doc, cid);
                }
            }
        }
        if self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id) {
            self.marker_rename = None;
        }
    }

    /// Create a title, place it at the playhead, select it and open its words
    /// for typing.
    ///
    /// A title is the one source with no bytes: nothing to import, nothing to
    /// hydrate, and it renders on every peer immediately — so "add a title"
    /// can go all the way to an editable clip on the timeline in one step
    /// rather than leaving an unnamed item in the bin to be dragged out.
    fn add_title(&mut self, doc: &mut AutoCommit) {
        let palette = (self.project.media.len() as u8) % 7;
        let item = MediaItem {
            id: Uuid::new_v4(),
            name: "Title".into(),
            kind: MediaKind::Text,
            duration: 5.0,
            width: self.project.width,
            height: self.project.height,
            fps: self.project.fps,
            palette,
            path: String::new(),
            hydration: crate::media::Hydration::Local,
            text: Some(TextSource::default()),
            ..Default::default()
        };
        let mid = item.id;
        self.project.media.push(item.clone());
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&item));
        self.selected_media = Some(mid);
        if let Some(cid) = self.place_title_clip(doc, mid) {
            self.selection.select_only(cid);
            self.reveal_inspector();
        }
        self.collapsed.remove(&Section::Title);
        self.begin_title_edit(mid);
    }

    /// Place a fresh title clip at the playhead on the topmost video track
    /// that will take it, so a title lands *over* the picture rather than
    /// under it.
    fn place_title_clip(&mut self, doc: &mut AutoCommit, mid: MediaId) -> Option<ClipId> {
        let track = self
            .project
            .display_tracks()
            .into_iter()
            .find(|t| t.kind == TrackKind::Video && !t.locked)
            .map(|t| t.id)
            .unwrap_or_else(|| self.add_track(doc, TrackKind::Video));
        let at = snap_to_frame(self.playhead, self.project.fps);
        let cid = self.project.add_clip(track, mid, at)?;
        self.dispatch_clip(doc, cid);
        Some(cid)
    }

    /// Commit new words for a title: one media write, so the picture, the bin
    /// row and every clip that placed this title all follow from it.
    fn commit_title_text(&mut self, doc: &mut AutoCommit, mid: MediaId, words: &str) {
        let Some(m) = self.project.media(mid) else { return };
        let mut next = m.clone();
        let mut t = next.text.clone().unwrap_or_default();
        if t.content == words {
            return;
        }
        t.content = words.to_string();
        next.name = words.lines().next().unwrap_or("Title").trim().to_string();
        if next.name.is_empty() {
            next.name = "Title".into();
        }
        next.text = Some(t);
        if let Some(slot) = self.project.media.iter_mut().find(|m| m.id == mid) {
            *slot = next.clone();
        }
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&next));
    }

    fn begin_title_edit(&mut self, mid: MediaId) {
        let content = self
            .project
            .media(mid)
            .and_then(|m| m.text.as_ref())
            .map(|t| t.content.clone())
            .unwrap_or_default();
        let mut label = EditableLabel::new(&content).with_font_size(13.0).with_id("title_text");
        label.begin_edit();
        self.title_edit = Some((mid, label));
    }

    /// Longest transition the selected clip can carry: its own length, since
    /// a ramp cannot outlast the footage it ramps into.
    fn transition_len_max(&self) -> f64 {
        self.selected()
            .and_then(|id| self.project.clip(id))
            .map(|c| c.duration())
            .unwrap_or(0.0)
            .max(0.01)
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
    fn add_effect_to_selected(&mut self, doc: &mut AutoCommit, kind: EffectKind) {
        self.edit_selected(doc, move |c| c.add_effect(kind));
    }
    fn remove_effect_from_selected(&mut self, doc: &mut AutoCommit, idx: usize) {
        self.edit_selected(doc, move |c| c.remove_effect(idx));
    }
    fn move_effect_selected(&mut self, doc: &mut AutoCommit, idx: usize, up: bool) {
        self.edit_selected(doc, move |c| {
            if up {
                c.move_effect_up(idx)
            } else {
                c.move_effect_down(idx)
            }
        });
    }
    /// Open the "Add effect" picker (the shared context menu) at `at`.
    fn open_effect_menu(&mut self, at: Point) {
        let mut menu = ContextMenu::new();
        for k in EffectKind::ALL {
            menu.push(MenuItem::action(k.label(), || {}));
        }
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::AddEffect);
    }
    fn set_palette(&mut self, doc: &mut AutoCommit, p: u8) {
        self.edit_selected(doc, move |c| c.palette = p);
    }

    /// Return the lowest-index *unlocked* track of `kind`, creating one
    /// (dispatched as an action, in the same undo boundary as the caller's
    /// edit) when the project has none — so placing a clip from the bin works
    /// even on a blank project, and a locked track never silently swallows a
    /// drop.
    fn ensure_track_of_kind(&mut self, doc: &mut AutoCommit, kind: TrackKind) -> TrackId {
        if let Some(t) = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind && !t.locked)
            .min_by_key(|t| t.index)
        {
            return t.id;
        }
        let index = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.index)
            .max()
            .unwrap_or(0)
            + 1;
        let name = format!("{}{index}", if kind == TrackKind::Video { "V" } else { "A" });
        let track = crate::model::Track {
            id: Uuid::new_v4(),
            name,
            kind,
            index,
            muted: false,
            locked: false, solo: false,
        };
        let id = track.id;
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&track));
        self.project.tracks.push(track);
        id
    }

    /// Append the given media as a clip on a matching track (creating that track
    /// if the project has none), at the end of that track's content. Returns the
    /// new clip id.
    fn add_media_to_timeline(&mut self, doc: &mut AutoCommit, media: MediaId) -> Option<ClipId> {
        let m = self.project.media(media).cloned()?;
        let kind = if m.kind.is_visual() { TrackKind::Video } else { TrackKind::Audio };
        let track = self.ensure_track_of_kind(doc, kind);
        let start = self
            .project
            .clips_on(track)
            .last()
            .map(|c| c.end())
            .unwrap_or(0.0);
        let id = self.project.add_clip(track, media, start)?;
        self.dispatch_clip(doc, id);
        self.selection.select_only(id);
        Some(id)
    }

    /// Place `media` on `track` at timeline time `start` (a drag-drop from the
    /// bin), creating and selecting the clip. Returns the new clip id.
    fn place_media_at(
        &mut self,
        doc: &mut AutoCommit,
        media: MediaId,
        track: TrackId,
        start: f64,
    ) -> Option<ClipId> {
        let start = snap_to_frame(start.max(0.0), self.project.fps);
        let id = self.project.add_clip(track, media, start)?;
        self.dispatch_clip(doc, id);
        self.selection.select_only(id);
        Some(id)
    }

    // ---- bin → timeline drag ---------------------------------------------

    /// Promote an armed bin drag to a live one once the pointer passes the
    /// threshold. Called on every pointer move.
    fn update_bin_drag(&mut self) {
        let mouse = self.last_mouse;
        if let Some(bd) = self.bin_drag.as_mut() {
            if !bd.active && bd.origin.distance(mouse) > BIN_DRAG_THRESHOLD {
                bd.active = true;
                // A press that becomes a drag is never a rename.
                self.pending_rename = None;
            }
        }
    }

    /// The lane under the current cursor while a bin drag is live, as
    /// `(track, kind, lane rect)` — the drop target.
    ///
    /// Only the pointer's **y** picks the lane. Requiring `x >= lanes.x0` used
    /// to mean a straight-down drag from a bin row — the natural gesture,
    /// since the bin sits above the gutter — landed in the 128px track-header
    /// gutter, missed every lane, and silently appended the clip at the end of
    /// the timeline instead of placing it. A drop anywhere across the
    /// timeline's width now hits the lane it is level with; the gutter simply
    /// resolves to the leftmost visible time (see [`Self::drop_time`]).
    fn bin_drop_lane(&self, r: &Regions) -> Option<(TrackId, TrackKind, Rect)> {
        let p = self.last_mouse;
        if p.x < r.timeline.x0 || p.x > r.timeline.x1 {
            return None;
        }
        self.lane_rects(r)
            .into_iter()
            .find(|(_, _, lane)| p.y >= lane.y0 && p.y <= lane.y1)
    }

    /// The timeline time a drop at `x` means. Inside the lanes it is the time
    /// under the pointer; over the gutter it clamps to the lane area's left
    /// edge, i.e. the earliest visible time (0 on an unscrolled timeline).
    fn drop_time(&self, x: f64, r: &Regions) -> f64 {
        self.x_to_time(x.max(r.lanes.x0), &r.lanes)
    }

    /// Finish a bin drag: place the dragged media as a clip at the drop
    /// position. A drop on a matching-kind unlocked lane lands at the drop time
    /// on that track; a kind-mismatched or locked lane lands at the drop time
    /// on a matching-kind track (created if needed); a drop elsewhere on the
    /// timeline appends to a matching track. A drop off the timeline cancels.
    ///
    /// A press that never became a drag ends here too: if it armed a rename
    /// intent, this is where the rename actually begins — release-time, so a
    /// drag that started from the same press has already cancelled it.
    fn release_bin_drag(&mut self, doc: &mut AutoCommit, r: &Regions) {
        let Some(bd) = self.bin_drag.take() else { return };
        if !bd.active {
            // A plain click that never moved. If it landed on the name of an
            // item clicked a moment ago, that is the Finder-style slow second
            // click — begin the inline rename now.
            if self.pending_rename.take() == Some(bd.media) {
                self.begin_rename(bd.media);
                self.last_click_media = None;
            }
            return;
        }
        self.pending_rename = None;
        let p = self.last_mouse;
        let want_kind = if self.project.media(bd.media).map(|m| m.kind.is_visual()).unwrap_or(true) {
            TrackKind::Video
        } else {
            TrackKind::Audio
        };
        if let Some((tid, kind, _)) = self.bin_drop_lane(r) {
            let t = self.drop_time(p.x, r);
            if kind == want_kind && !self.project.track_locked(tid) {
                self.place_media_at(doc, bd.media, tid, t);
            } else {
                // Wrong kind, or the lane refuses edits: fall to a track that
                // can take it, at the same drop time.
                let track = self
                    .project
                    .placement_track(want_kind, None)
                    .unwrap_or_else(|| self.ensure_track_of_kind(doc, want_kind));
                self.place_media_at(doc, bd.media, track, t);
            }
        } else if r.timeline.contains(p) {
            // On the timeline but not level with any lane (below the last one)
            // → append to a matching track.
            self.add_media_to_timeline(doc, bd.media);
        }
    }

    // ---- media-bin clip management ---------------------------------------

    /// The sub-rect of a bin row where the media *name* is drawn — clicking it
    /// (on an already-selected item) begins an inline rename, and it's where the
    /// rename field is placed.
    fn bin_name_rect(&self, row: Rect) -> Rect {
        // Mirrors draw_bin: a 64px thumbnail at the left, then the name text.
        let x0 = row.x0 + 8.0 + 64.0 + 12.0;
        Rect::new(x0, row.y0 + 4.0, row.x1 - 8.0, row.y0 + 30.0)
    }

    /// Open the bin context menu for `mid` at logical point `at`.
    fn open_bin_menu(&mut self, mid: MediaId, at: Point) {
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Add to timeline", || {}));
        menu.push(MenuItem::action("Rename", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Media(mid));
        self.selected_media = Some(mid);
    }

    /// Open the track-header context menu for `tid` at logical point `at`.
    /// Carries the two per-track state toggles as well as the structural
    /// actions, so mute and lock are reachable without hunting for the pills.
    fn open_track_menu(&mut self, tid: TrackId, at: Point) {
        let t = self.project.track(tid);
        let muted = t.map(|t| t.muted).unwrap_or(false);
        let locked = t.map(|t| t.locked).unwrap_or(false);
        let solo = t.map(|t| t.solo).unwrap_or(false);
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action(if muted { "Unmute track" } else { "Mute track" }, || {}));
        menu.push(MenuItem::action(if solo { "Unsolo track" } else { "Solo track" }, || {}));
        menu.push(MenuItem::action(if locked { "Unlock track" } else { "Lock track" }, || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Add video track", || {}));
        menu.push(MenuItem::action("Add audio track", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete track", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Track(tid));
    }

    /// Open the timeline clip context menu for `cid` at logical point `at`.
    /// Mirrors the bin and track-header menus, and offers exactly the
    /// selection-wide edits the keyboard has: cut, copy, duplicate, split,
    /// delete and ripple delete.
    fn open_clip_menu(&mut self, cid: ClipId, at: Point) {
        // Right-clicking outside the current selection re-targets it, so the
        // menu always acts on what the user just pointed at.
        if !self.selection.contains(cid) {
            self.selection.select_only(cid);
        } else {
            self.selection.set_primary(cid);
        }
        let mut menu = ContextMenu::new();
        menu.push(MenuItem::action("Cut", || {}));
        menu.push(MenuItem::action("Copy", || {}));
        menu.push(MenuItem::action("Duplicate", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Split at playhead", || {}));
        menu.push(MenuItem::separator());
        menu.push(MenuItem::action("Delete", || {}));
        menu.push(MenuItem::action("Ripple delete", || {}));
        menu.open_at([at.x as f32, at.y as f32]);
        self.bin_menu = menu;
        self.menu_target = Some(MenuTarget::Clip(cid));
    }

    /// If the cursor is over a timeline clip, open its context menu there.
    fn open_clip_menu_at_cursor(&mut self, r: &Regions) -> bool {
        let p = self.last_mouse;
        if !r.lanes.contains(p) {
            return false;
        }
        let Some((cid, _lane)) = self.clip_at(r, p) else { return false };
        self.open_clip_menu(cid, p);
        true
    }

    /// If the cursor is over a visible bin item, open its context menu there.
    fn open_bin_menu_at_cursor(&mut self, r: &Regions) -> bool {
        let p = self.last_mouse;
        let bin_visible = self.bin_is_visible(r);
        if !bin_visible {
            return false;
        }
        for (mid, rect) in self.bin_item_rects(r) {
            if rect.contains(p) {
                self.open_bin_menu(mid, p);
                return true;
            }
        }
        false
    }

    /// If the cursor is over a timeline track header (the gutter), open its
    /// track context menu there.
    fn open_track_menu_at_cursor(&mut self, r: &Regions) -> bool {
        let p = self.last_mouse;
        if !r.gutter.contains(p) {
            return false;
        }
        for (tid, _kind, lane) in self.lane_rects(r) {
            let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
            if header.contains(p) {
                self.open_track_menu(tid, p);
                return true;
            }
        }
        false
    }

    /// Act on a chosen context-menu row (indices match the `open_*_menu`
    /// builders for the current [`MenuTarget`]).
    fn bin_menu_choose(&mut self, doc: &mut AutoCommit, idx: usize) {
        match self.menu_target {
            Some(MenuTarget::Media(mid)) => match idx {
                0 => {
                    self.add_media_to_timeline(doc, mid);
                }
                1 => self.begin_rename(mid),
                3 => self.delete_media(doc, mid),
                _ => {}
            },
            Some(MenuTarget::Track(tid)) => match idx {
                0 => self.toggle_track_mute(doc, tid),
                1 => self.toggle_track_solo(doc, tid),
                2 => self.toggle_track_lock(doc, tid),
                4 => {
                    self.add_track(doc, TrackKind::Video);
                }
                5 => {
                    self.add_track(doc, TrackKind::Audio);
                }
                7 => self.delete_track(doc, tid),
                _ => {}
            },
            Some(MenuTarget::Clip(_)) => match idx {
                0 => self.cut_selected(doc),
                1 => self.copy_selected(),
                2 => self.duplicate_selected(doc),
                4 => self.split_selected(doc),
                6 => self.delete_selected(doc),
                7 => self.ripple_delete_selected(doc),
                _ => {}
            },
            Some(MenuTarget::AddEffect) => {
                if let Some(&kind) = EffectKind::ALL.get(idx) {
                    self.add_effect_to_selected(doc, kind);
                }
            }
            Some(MenuTarget::Keyframe(param)) => self.key_menu_choose(doc, param, idx),
            None => {}
        }
    }

    /// Add a fresh empty track of `kind` above the others of its kind
    /// (index = current max of that kind + 1), dispatched as an action.
    fn add_track(&mut self, doc: &mut AutoCommit, kind: TrackKind) -> TrackId {
        let index = self
            .project
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.index)
            .max()
            .unwrap_or(0)
            + 1;
        let name = format!("{}{index}", if kind == TrackKind::Video { "V" } else { "A" });
        let track = crate::model::Track {
            id: Uuid::new_v4(),
            name,
            kind,
            index,
            muted: false,
            locked: false, solo: false,
        };
        let id = track.id;
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&track));
        self.project.tracks.push(track);
        id
    }

    /// Delete a track and every clip on it, as one action.
    fn delete_track(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        let clip_ids: Vec<ClipId> =
            self.project.clips.iter().filter(|c| c.track == tid).map(|c| c.id).collect();
        self.selection.remove_all(&clip_ids);
        self.actions.dispatch(
            doc,
            VIDEO_EDITOR_ACTION_TYPE,
            &crate::actions::delete_track_action(&tid, &clip_ids),
        );
    }

    /// Toggle a track's mute flag (the header pill), dispatched as an action.
    fn toggle_track_mute(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        if let Some(t) = self.project.track(tid) {
            let mut next = t.clone();
            next.muted = !next.muted;
            let muted = next.muted;
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&next));
            if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                t.muted = muted;
            }
        }
    }

    /// Toggle a track's solo, dispatched as an action. Wired exactly like
    /// mute: the flag lives on the track, replicates, and undoes the same way.
    /// [`Project::track_audible`] is where the two combine.
    fn toggle_track_solo(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        if let Some(t) = self.project.track(tid) {
            let mut next = t.clone();
            next.solo = !next.solo;
            let solo = next.solo;
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&next));
            if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                t.solo = solo;
            }
        }
    }

    /// Toggle a track's lock (the header pill next to mute), dispatched as an
    /// action so it replicates and undoes like every other edit. Locking is
    /// what [`Project::clip_editable`] reads, so the flag really does hold
    /// every move / trim / delete / split off the track's clips.
    fn toggle_track_lock(&mut self, doc: &mut AutoCommit, tid: TrackId) {
        if let Some(t) = self.project.track(tid) {
            let mut next = t.clone();
            next.locked = !next.locked;
            // Reflect it in memory too so the very next press is already
            // guarded, before the document round-trips back through `sync`.
            let locked = next.locked;
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_track_action(&next));
            if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                t.locked = locked;
            }
        }
    }

    /// Begin an inline rename of bin media `mid` using the shared editable
    /// label (real caret / selection / clipboard). No-op if the media is gone.
    fn begin_rename(&mut self, mid: MediaId) {
        let Some(m) = self.project.media(mid) else { return };
        let mut label = EditableLabel::new(&m.name)
            .with_font_size(13.0)
            .with_id("bin_rename");
        label.begin_edit();
        self.bin_rename = Some((mid, label));
        self.selected_media = Some(mid);
    }

    /// Commit an inline rename: dispatch a whole-media write with the new name.
    fn commit_rename(&mut self, doc: &mut AutoCommit, mid: MediaId, name: &str) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(m) = self.project.media(mid) {
            if m.name == trimmed {
                return;
            }
            let mut next = m.clone();
            next.name = trimmed.to_string();
            self.actions
                .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &upsert_media_action(&next));
        }
    }

    /// Delete a bin media and every clip that placed it, as one action.
    fn delete_media(&mut self, doc: &mut AutoCommit, mid: MediaId) {
        let clip_ids: Vec<ClipId> =
            self.project.clips.iter().filter(|c| c.media == mid).map(|c| c.id).collect();
        // Keep the in-memory selection consistent before the reload.
        self.selection.remove_all(&clip_ids);
        self.actions
            .dispatch(doc, VIDEO_EDITOR_ACTION_TYPE, &remove_media_action(&mid, &clip_ids));
        // Mirror the action in memory, the way the import path does. The
        // document is the truth and `sync` will reload it, but the model
        // must not disagree with it in between — a caller that deletes and
        // then reads back its own project would otherwise still be handed
        // the source it just removed.
        self.project.media.retain(|m| m.id != mid);
        self.project.clips.retain(|c| c.media != mid);
        if self.selected_media == Some(mid) {
            self.selected_media = None;
        }
        if self.hover_media == Some(mid) {
            self.hover_media = None;
        }
    }

    // ---- media import ----------------------------------------------------
    /// Open the host file picker to import media through the shared XDG portal
    /// (`ui::portal`). The picker is a pure-Rust round-trip compiled into every
    /// build; the chosen files are drained each frame in [`Self::drain_import`].
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
            if n > 0 || !self.has_import_reason() {
                self.import_notice = Some(import_summary(n, paths.len()));
            }
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
            Some(ImportOutcome::Selected(paths)) => {
                let n = self.import_files(doc, &paths);
                if n > 0 || !self.has_import_reason() {
                    self.import_notice = Some(import_summary(n, paths.len()));
                }
            }
            Some(ImportOutcome::Cancelled) => self.import_notice = None,
            Some(ImportOutcome::Failed(reason)) => self.import_notice = Some(reason),
            None => {}
        }
    }

    /// Import a batch of files, returning how many became real clips. Each
    /// refusal leaves the probe's own reason in `import_notice`; a batch that
    /// imported nothing keeps the last one, because "No media could be
    /// imported" on its own tells the user nothing they can act on.
    fn import_files(&mut self, doc: &mut AutoCommit, paths: &[PathBuf]) -> usize {
        self.import_notice = None;
        paths.iter().filter(|p| self.import_one(doc, p)).count()
    }

    /// Whether the last import batch left a specific per-file reason worth
    /// keeping instead of overwriting with a batch summary.
    fn has_import_reason(&self) -> bool {
        self.import_notice.is_some()
    }

    /// Import one file: probe **real** metadata from the container and add it to
    /// the media bin as a real [`MediaItem`], replicated through the CRDT. The
    /// stored path is workspace-relative for a file under the FUSE mount (so it
    /// replicates to peers) or absolute for an imported local file.
    fn import_one(&mut self, doc: &mut AutoCommit, path: &Path) -> bool {
        let probed = match probe::probe_file(path) {
            Ok(p) => p,
            Err(e) => {
                // Say *why* it was refused. The probe knows ("is not a media
                // file - it is an HTML page"); a bare "couldn't be imported"
                // throws that away and leaves the user guessing.
                self.import_notice = Some(format!("{}: {e}", display_name(path)));
                return false;
            }
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
            // 0.0 means the container did not say. Carry that through rather
            // than inventing 30 — the bin renders unknown as unknown.
            fps: probed.fps,
            palette,
            path: stored,
            hydration: resolved.hydration,
            // An imported file is decoded, never generated: only a title
            // carries text.
            text: None,
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
        // `add_media_to_timeline` now creates a matching track when the project
        // has none, so this just places and parks the playhead in the new clip.
        let Some(cid) = self.add_media_to_timeline(doc, media_id) else { return };
        if let Some(c) = self.project.clip(cid) {
            let inside = c.start + (c.duration() * 0.4);
            self.playhead = inside.clamp(c.start, (c.end() - 0.05).max(c.start));
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
        // The bundled demo/shots project references placeholder sample-media,
        // not real bytes, so decoding it would spin a real GStreamer pipeline
        // for no usable frame and make a headless render depend on the gst
        // runtime. Keep the demo preview on its deterministic representative
        // composite. `decode` stays compiled in: the live app and the
        // `--import` real-decode capture (which loads genuine files) still
        // decode. This is the runtime half of "features compile the code in;
        // the runtime mode decides live vs sample".
        if self.seed_demo {
            return;
        }
        self.preview_decoder.update(&self.project, self.playhead);
    }

    /// Composite the preview picture for the current playhead, into
    /// `self.composited`.
    ///
    /// Runs the shared GPU compositor at the canvas's own pixel size (capped,
    /// so a huge window does not read back a huge buffer every frame) and
    /// keeps the RGBA for `draw_preview` to blit. Under `--features decode`
    /// the compositor's layers are filled with really-decoded pictures; with
    /// no decoder — the hermetic `--shots` path — it gets `NoSources` and each
    /// layer draws its representative fill, which keeps a capture
    /// deterministic and free of any media pipeline.
    fn pump_compositor(&mut self, r: &Regions) {
        if !self.compositor_tried {
            self.compositor_tried = true;
            match crate::gpu::HeadlessCompositor::new() {
                Ok(c) => self.compositor = Some(c),
                Err(e) => tracing::warn!(error = %e, "no GPU compositor for the preview"),
            }
        }
        let Some(compositor) = self.compositor.as_mut() else { return };
        if self.project.clips.is_empty() || r.canvas.width() < 1.0 {
            self.composited = None;
            return;
        }
        // Match the canvas's aspect at a sane readback size.
        const MAX_W: f64 = 960.0;
        let scale = (MAX_W / r.canvas.width()).min(1.0);
        let w = (r.canvas.width() * scale).round().max(2.0) as u32;
        let h = (r.canvas.height() * scale).round().max(2.0) as u32;

        #[cfg(feature = "decode")]
        let rgba = {
            // The preview resolves its layers straight off the decode
            // sessions, which hand back a GPU-resident planar picture. The
            // CPU path this replaced converted every frame to RGBA on the
            // host first — a full-frame colour convert per layer per frame,
            // for a picture that was already on the GPU.
            //
            // `Responsive` never blocks: it serves the cached frame if there
            // is one and otherwise nudges the pipeline and holds the last
            // picture, so a scrub cannot stall the UI thread.
            let sig = crate::gpu::MediaFrameSource::project_signature(&self.project);
            if self.frame_source.is_none() {
                self.frame_source =
                    Some(crate::gpu::MediaFrameSource::new(&self.project, crate::gpu::Fetch::Responsive));
                self.frame_source_sig = sig;
            } else if sig != self.frame_source_sig {
                if let Some(src) = self.frame_source.as_mut() {
                    src.set_project(&self.project);
                }
                self.frame_source_sig = sig;
            }
            let sources = self.frame_source.as_mut().expect("just built");
            // An open pipeline holds a decoder and a slice of the GPU. Drop
            // the idle ones periodically rather than every frame, which
            // would churn sessions the playhead is about to want again.
            self.prune_countdown = self.prune_countdown.wrapping_add(1);
            if self.prune_countdown.is_multiple_of(120) {
                sources.prune();
            }
            compositor.frame(&self.project, self.playhead, w, h, sources)
        };
        #[cfg(not(feature = "decode"))]
        let rgba = {
            let mut sources = crate::gpu::NoSources;
            compositor.frame(&self.project, self.playhead, w, h, &mut sources)
        };

        if rgba.len() == (w as usize) * (h as usize) * 4 {
            self.composited = Some((w, h, std::sync::Arc::new(rgba)));
        } else {
            self.composited = None;
        }
    }

    /// Zoom so the whole project fits the lane area, and park the view at the
    /// start. The answer to "I zoomed in and now I am lost", which the editor
    /// previously had no button for at all.
    fn zoom_to_fit(&mut self, r: &Regions) {
        let dur = self.project.duration().max(0.01);
        let w = r.lanes.width().max(1.0);
        self.zoom = (w / dur).clamp(6.0, 220.0);
        self.scroll = 0.0;
        self.clamp_scroll(r);
    }

    /// The zoom-readout chip in the timeline's gutter footer — also the
    /// fit-to-window button.
    fn zoom_chip_rect(&self, r: &Regions) -> Rect {
        let y = r.ruler.y0 + 4.0;
        Rect::new(r.gutter.x0 + 8.0, y, r.gutter.x0 + 8.0 + 84.0, y + 22.0)
    }

    fn zoom_by(&mut self, factor: f64) {
        self.zoom = (self.zoom * factor).clamp(6.0, 220.0);
        let r = self.regions();
        self.clamp_scroll(&r);
    }

    /// Zoom keeping the time currently under `anchor_x` pinned to that pixel —
    /// the wheel-over-the-timeline gesture. Outside the lane area it falls
    /// back to anchoring on the playhead, which is what the keyboard's +/- do.
    fn zoom_at(&mut self, factor: f64, anchor_x: f64, r: &Regions) {
        let anchor_t = if anchor_x >= r.lanes.x0 && anchor_x <= r.lanes.x1 {
            self.x_to_time(anchor_x, &r.lanes)
        } else {
            self.playhead
        };
        let anchor_px = (anchor_t - self.scroll) * self.zoom;
        self.zoom = (self.zoom * factor).clamp(6.0, 220.0);
        self.scroll = anchor_t - anchor_px / self.zoom;
        self.clamp_scroll(r);
    }

    fn inspector_layout(&self, r: &Regions) -> InspectorLayout {
        let mut l = InspectorLayout::default();
        let Some(id) = self.selected() else { return l };
        let Some(clip) = self.project.clip(id) else { return l };
        let media = self.project.media(clip.media);
        let is_video = media.map(|m| m.kind.is_visual()).unwrap_or(true);
        let is_title = media.map(|m| m.text.is_some()).unwrap_or(false);
        let x0 = r.inspector.x0 + PAD;
        let x1 = r.inspector.x1 - PAD;
        // Static header + four property rows consume a fixed block; the
        // editable controls start below it. Everything scrolls together by
        // `inspector_scroll` (the pane clips + is wheel-scrollable) so the
        // effects stack can grow without clipping the actions.
        let top = r.inspector.y0 - self.inspector_scroll;
        // The four property rows end on a baseline at `top + 132`; a 12px
        // line reaches ~15px past it, so the first control label cannot start
        // before `top + 147` without sitting on top of "Source Out".
        let mut y = top + 162.0;
        let row = 34.0; // label sits at y, the 9px track at y+19
        let track_at = |y: f64| Rect::new(x0, y + 19.0, x1, y + 28.0);
        // An animatable row indents its label past a key control at the head.
        // The indent is unconditional so the row does not shuffle sideways the
        // moment a parameter is keyed.
        let key_at = |y: f64| Rect::new(x0, y - 3.0, x0 + KEY_BOX, y + 15.0);

        if is_video {
            l.opacity = Some(track_at(y)); l.keys.push((Param::Opacity, key_at(y))); y += row;
            l.brightness = Some(track_at(y)); l.keys.push((Param::Brightness, key_at(y))); y += row;
            l.contrast = Some(track_at(y)); l.keys.push((Param::Contrast, key_at(y))); y += row;
            l.saturation = Some(track_at(y)); l.keys.push((Param::Saturation, key_at(y))); y += row;
        } else {
            l.volume = Some(track_at(y)); l.keys.push((Param::Volume, key_at(y))); y += row;
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
        // Transition length, only when there is a transition to size.
        if clip.transition != Transition::None {
            l.transition_len = Some(Rect::new(x0, y + 19.0, x1, y + 28.0));
            y += row;
        }
        // Label color swatches.
        l.colors_y = y + 15.0;
        let sw = 24.0;
        for i in 0..7u8 {
            let sx0 = x0 + i as f64 * (sw + 4.0);
            if sx0 + sw > x1 { break; }
            l.colors.push((i, Rect::new(sx0, y + 15.0, sx0 + sw, y + 15.0 + sw)));
        }
        y += 44.0;

        // ---- collapsible sections ------------------------------------
        // Each opens with a header row that folds it; a folded section
        // contributes only its header (and still states what is inside it).
        let mut header = |l: &mut InspectorLayout, y: &mut f64, s: Section| -> bool {
            l.sections.push((s, Rect::new(x0, *y, x1, *y + SECTION_H)));
            *y += SECTION_H + 4.0;
            !self.collapsed.contains(&s)
        };
        // Geometry, video only — an audio clip has no picture to place.
        if is_video && header(&mut l, &mut y, Section::Transform) {
            for f in [
                NumField::ScaleX,
                NumField::ScaleY,
                NumField::PosX,
                NumField::PosY,
                NumField::Rotation,
            ] {
                l.nums.push((f, track_at(y)));
                if let Some(p) = f.param() {
                    l.keys.push((p, key_at(y)));
                }
                y += row;
            }
            // Crop: two half-width pairs under their own caption. No key
            // control — crop frames the shot rather than moving it, and the
            // model does not animate it.
            y += 12.0;
            l.crop_label_y = y;
            y += 20.0;
            let half = (x1 - x0) / 2.0 - 6.0;
            for pair in [
                [NumField::CropLeft, NumField::CropRight],
                [NumField::CropTop, NumField::CropBottom],
            ] {
                l.nums.push((pair[0], Rect::new(x0, y + 19.0, x0 + half, y + 28.0)));
                l.nums.push((pair[1], Rect::new(x1 - half, y + 19.0, x1, y + 28.0)));
                y += row;
            }
            // Picture-in-picture, as the four corners it is actually used for.
            y += 16.0;
            let bw = (x1 - x0 - 18.0) / 4.0;
            for (i, (label, corner)) in [
                ("↖", (-1.0, -1.0)),
                ("↗", (1.0, -1.0)),
                ("↙", (-1.0, 1.0)),
                ("↘", (1.0, 1.0)),
            ]
            .into_iter()
            .enumerate()
            {
                let bx = x0 + i as f64 * (bw + 6.0);
                l.pip.push((label, corner, Rect::new(bx, y, bx + bw, y + 26.0)));
            }
            y += 32.0;
            l.reset_transform = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // Rate and direction.
        if header(&mut l, &mut y, Section::Speed) {
            l.nums.push((NumField::Speed, track_at(y)));
            y += row;
            let pw = (x1 - x0) / SPEED_PRESETS.len() as f64;
            for (i, rate) in SPEED_PRESETS.iter().enumerate() {
                let px0 = x0 + i as f64 * pw;
                l.speed_presets.push((*rate, Rect::new(px0, y, px0 + pw - 3.0, y + 24.0)));
            }
            y += 32.0;
            l.reverse = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // A title's words and setting — only for a clip whose source is one.
        if is_title && header(&mut l, &mut y, Section::Title) {
            // Captioned controls need the caption's line above them; without
            // it the caption lands on the section header it sits under.
            y += 16.0;
            l.title_text = Some(Rect::new(x0, y, x1, y + 30.0));
            y += 40.0;
            l.nums.push((NumField::TitleSize, track_at(y)));
            y += row;
            let aw = (x1 - x0) / TextAlign::ALL.len() as f64;
            for (i, a) in TextAlign::ALL.iter().enumerate() {
                let ax = x0 + i as f64 * aw;
                l.title_align.push((*a, Rect::new(ax, y, ax + aw - 3.0, y + 24.0)));
            }
            y += 32.0 + 16.0;
            let sw = 24.0;
            for (i, col) in TITLE_COLORS.iter().enumerate() {
                let sx = x0 + i as f64 * (sw + 4.0);
                if sx + sw > x1 {
                    break;
                }
                l.title_colors.push((*col, Rect::new(sx, y, sx + sw, y + sw)));
            }
            y += sw + 12.0;
            let hw = (x1 - x0) / 2.0 - 4.0;
            l.title_bold = Some(Rect::new(x0, y, x0 + hw, y + 26.0));
            l.title_italic = Some(Rect::new(x1 - hw, y, x1, y + 26.0));
            y += 34.0;
            l.nums.push((NumField::TitleBackdrop, track_at(y)));
            y += row + 4.0;
        }
        // Effects stack (video only): one row per effect, then an "Add
        // effect" button.
        if is_video {
            let open = header(&mut l, &mut y, Section::Effects);
            l.effects_label_y = y;
            if open {
                for _ in 0..clip.effects.len() {
                    l.effect_rows.push(Rect::new(x0, y, x1, y + 24.0));
                    y += 28.0;
                }
                l.add_effect = Rect::new(x0, y, x0 + 132.0, y + 24.0);
                y += 34.0;
            }
        }
        // Markers: every marked instant on the timeline, the project's own and
        // each clip's, so one list is the whole answer to "what is marked".
        if header(&mut l, &mut y, Section::Markers) {
            for (at, m, on_clip) in self.project.all_markers() {
                l.marker_rows.push((
                    MarkerRef { id: m.id, clip: on_clip, at },
                    Rect::new(x0, y, x1, y + 26.0),
                ));
                y += 30.0;
            }
            l.add_marker = Some(Rect::new(x0, y, x1, y + 26.0));
            y += 34.0;
        }
        // Actions.
        y += 6.0;
        l.split = Rect::new(x0, y, (x0 + x1) / 2.0 - 4.0, y + 28.0);
        l.delete = Rect::new((x0 + x1) / 2.0 + 4.0, y, x1, y + 28.0);
        y += 34.0;
        l.content_height = y - top;
        l.valid = true;
        l
    }

    /// Sub-rects of one effect row: `(reorder-up, reorder-down, slider, remove)`.
    fn effect_row_parts(row: Rect) -> (Rect, Rect, Rect, Rect) {
        let cy = row.center().y;
        // 22px tall targets — the row is 24px, so this is as close to the
        // toolkit's minimum as the row allows, and a real improvement on the
        // 16x18 boxes these were.
        let h = 11.0;
        let up = Rect::new(row.x0, cy - h, row.x0 + 22.0, cy + h);
        let down = Rect::new(row.x0 + 23.0, cy - h, row.x0 + 45.0, cy + h);
        let remove = Rect::new(row.x1 - 22.0, cy - h, row.x1, cy + h);
        let slider = Rect::new(row.x1 - 96.0, cy - 4.0, row.x1 - 30.0, cy + 4.0);
        (up, down, slider, remove)
    }

    /// The maximum inspector scroll offset for the current selection.
    /// `content_height` is scroll-independent (the scroll is a pure translation),
    /// so the current layout's extent is the intrinsic content height.
    fn inspector_max_scroll(&self, r: &Regions) -> f64 {
        (self.inspector_layout(r).content_height - r.inspector.height()).max(0.0)
    }

    fn export_layout(&self, r: &Regions) -> ExportLayout {
        // Clamp to the window: a fixed 460x360 hung off both edges of a small
        // one, putting the Cancel and Export buttons outside the surface.
        let card_w = 460.0_f64.min(self.rect.width() - 32.0).max(240.0);
        let card_h = 360.0_f64.min(self.rect.height() - 32.0).max(200.0);
        let cx = (r.timeline.x0 + r.timeline.x1) / 2.0;
        let cy = (self.rect.y0 + self.rect.y1) / 2.0;
        let card = Rect::new(cx - card_w / 2.0, cy - card_h / 2.0, cx + card_w / 2.0, cy + card_h / 2.0);
        let x0 = card.x0 + 24.0;
        let x1 = card.x1 - 24.0;
        let mut l = ExportLayout { card, ..Default::default() };
        // The three groups are spaced within the band between the header and
        // the footer rather than from fixed offsets off the card top. The old
        // constants assumed a 360px card AND that the header was one line: the
        // filename subtitle and the RESOLUTION label were both computed to land
        // at the same place, and drew through each other.
        let content_top = card.y0 + EXPORT_HEADER_H;
        let footer_top = (card.y1 - EXPORT_FOOTER_H).max(content_top + EXPORT_GROUP_H);
        let step = ((footer_top - content_top) / 3.0).clamp(EXPORT_GROUP_H, 64.0);
        let seg = |x0: f64, x1: f64, n: usize, y: f64| -> Vec<Rect> {
            let w = (x1 - x0) / n as f64;
            (0..n).map(|i| Rect::new(x0 + i as f64 * w, y, x0 + (i as f64 + 1.0) * w - 4.0, y + 32.0)).collect()
        };
        // Each group's label is drawn 18px above its segments, so the segment
        // row starts one label-height into the group's slot.
        let mut y = content_top + EXPORT_LABEL_H;
        for (i, rect) in seg(x0, x1, Resolution::ALL.len(), y).into_iter().enumerate() {
            l.resolution.push((Resolution::ALL[i], rect));
        }
        y += step;
        for (i, rect) in seg(x0, x1, Container::ALL.len(), y).into_iter().enumerate() {
            l.container.push((Container::ALL[i], rect));
        }
        y += step;
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
                    self.selection.select_only(c.id);
                    self.selected_media = Some(c.media);
                }
                self.playhead = 4.0;
                // Push the saturation up a touch through the real edit path so
                // the grade is unmistakable in the capture.
                self.set_filter(state, FilterField::Saturation, 0.72);
                self.zoom = 30.0;
            }
            ShotScenario::Composed => {
                // Everything geometry made possible, arranged through the
                // production model: an inset over the spine, a title in the
                // lower third, and markers on the ruler.
                self.playhead = 23.0;
                self.zoom = 26.0;
                if let Some(c) = self.project.top_video_clip_at(23.0) {
                    let cid = c.id;
                    if let Some(c) = self.project.clip_mut(cid) {
                        c.transform = crate::model::Transform::pip(0.34, (1.0, -1.0), 0.04);
                    }
                    self.selection.select_only(cid);
                }
                let title = MediaItem {
                    id: Uuid::new_v4(),
                    name: "Coastline".into(),
                    kind: MediaKind::Text,
                    duration: 8.0,
                    palette: 4,
                    path: String::new(),
                    hydration: crate::media::Hydration::Local,
                    text: Some(crate::model::TextSource {
                        content: "COASTLINE\nepisode two".into(),
                        size: 0.14,
                        align: crate::model::TextAlign::Left,
                        ..crate::model::TextSource::default()
                    }),
                    ..Default::default()
                };
                let tid = title.id;
                self.project.media.push(title);
                if let Some(v2) = self
                    .project
                    .tracks
                    .iter()
                    .find(|t| t.kind == TrackKind::Video && t.index == 2)
                    .map(|t| t.id)
                {
                    if let Some(cid) = self.project.add_clip(v2, tid, 34.0) {
                        if let Some(c) = self.project.clip_mut(cid) {
                            c.transform = crate::model::Transform {
                                scale_x: 0.6,
                                scale_y: 0.6,
                                pos_x: -0.1,
                                pos_y: 0.27,
                                ..Default::default()
                            };
                        }
                    }
                }
                self.project.add_marker(9.0, "act two", 3);
                self.project.add_marker(21.0, "sunset", 5);
                VideoDoc::write_project(state, &self.project);
            }
            ShotScenario::JustSplit => {
                // Select the interview body clip and split it at the playhead,
                // then select the right half — exactly what pressing S does.
                if let Some(c) = self.project.top_video_clip_at(12.0) {
                    self.selection.select_only(c.id);
                }
                self.playhead = 14.0;
                self.split_selected(state);
                self.zoom = 40.0;
            }
            // Rect-dependent interactions are arranged in `draw` (the layout
            // rect is still zero here in `sync`).
            s if s.is_interaction() => {}
            _ => {}
        }
    }

    /// Arrange a rect-dependent interaction scenario for a `--shots` capture,
    /// once, from `draw` where the layout rect is known. Uses the same
    /// production affordances a live interaction drives.
    fn apply_shot_interaction(&mut self, r: &Regions) {
        match self.shot_scenario {
            ShotScenario::BinMenu => {
                if let Some((mid, row)) = self.bin_item_rects(r).into_iter().nth(2) {
                    let at = Point::new(row.center().x, row.center().y);
                    self.open_bin_menu(mid, at);
                }
            }
            ShotScenario::BinRename => {
                if let Some((mid, _row)) = self.bin_item_rects(r).into_iter().nth(2) {
                    self.begin_rename(mid);
                }
            }
            ShotScenario::DragToTimeline => {
                // A video bin item mid-drag over the V1 lane.
                let item = self
                    .bin_item_rects(r)
                    .into_iter()
                    .find(|(m, _)| self.project.media(*m).map(|mm| mm.kind.is_visual()).unwrap_or(false));
                let lane = self
                    .lane_rects(r)
                    .into_iter()
                    .find(|(_, k, _)| *k == TrackKind::Video);
                if let (Some((mid, row)), Some((_, _, lane))) = (item, lane) {
                    self.selected_media = Some(mid);
                    self.bin_drag = Some(BinDrag {
                        media: mid,
                        origin: row.center(),
                        active: true,
                    });
                    self.last_mouse = Point::new(lane.x0 + 470.0, lane.center().y);
                }
            }
            ShotScenario::Effects => {
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    self.selection.select_only(c.id);
                    self.selected_media = Some(c.media);
                }
                self.playhead = 6.0;
                // Scroll the inspector so the effects stack + actions are in view.
                self.inspector_scroll = self.inspector_max_scroll(r);
            }
            // A clip that has actually been placed, re-timed and keyframed,
            // with the inspector scrolled to the controls that did it. Every
            // value goes through the production model, so the capture is what
            // the compositor is drawing rather than a posed panel.
            ShotScenario::Geometry => {
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        c.transform = crate::model::Transform::pip(PIP_SCALE, (1.0, -1.0), PIP_INSET);
                        c.transform.rotation = -6.0;
                        c.transform.crop_left = 0.08;
                        c.transform.crop_right = 0.08;
                        c.set_speed(2.0, false);
                        // Two keys, so the row reads as animated and the clip
                        // carries diamonds.
                        let start = c.start;
                        c.set_key_at(Param::PosX, start + 0.5, 0.30, Interp::Ease);
                        c.set_key_at(Param::PosX, start + 3.0, -0.30, Interp::Ease);
                    }
                    // 2x halves the clip's length, so a playhead left where it
                    // was would now sit past its end and the preview would go
                    // black. Land it inside what the clip now occupies.
                    if let Some(c) = self.project.clip(cid) {
                        self.playhead = c.start + c.duration() * 0.45;
                    }
                }
                // Past the levels, onto the geometry and rate sections.
                self.inspector_scroll = (self.inspector_max_scroll(r) * 0.62).max(0.0);
            }
            ShotScenario::TitleClip => {
                // Build the title through the model the app itself uses, then
                // select it — so what the capture shows is a real title clip,
                // not an inspector posed to look like one.
                let item = MediaItem {
                    id: Uuid::new_v4(),
                    name: "COASTLINE".into(),
                    kind: MediaKind::Text,
                    duration: 6.0,
                    width: self.project.width,
                    height: self.project.height,
                    fps: self.project.fps,
                    palette: 4,
                    path: String::new(),
                    hydration: crate::media::Hydration::Local,
                    text: Some(TextSource {
                        content: "COASTLINE".into(),
                        size: 0.16,
                        color: TITLE_COLORS[2],
                        align: TextAlign::Left,
                        backdrop: 0.25,
                        ..TextSource::default()
                    }),
                    ..Default::default()
                };
                let mid = item.id;
                self.project.media.push(item);
                let track = self
                    .project
                    .display_tracks()
                    .into_iter()
                    .find(|t| t.kind == TrackKind::Video)
                    .map(|t| t.id);
                if let Some(track) = track {
                    if let Some(cid) = self.project.add_clip(track, mid, 4.0) {
                        self.selection.select_only(cid);
                        self.selected_media = Some(mid);
                        self.playhead = 6.0;
                    }
                }
                self.project.add_marker(9.0, "act two", 1);
                self.project.add_marker(18.5, "sunset", 3);
                self.collapsed.insert(Section::Transform);
                self.collapsed.insert(Section::Speed);
                self.inspector_scroll = self.inspector_max_scroll(r) * 0.5;
            }
            ShotScenario::Keyframes => {
                self.playhead = 6.0;
                if let Some(c) = self.project.top_video_clip_at(6.0) {
                    let cid = c.id;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        let start = c.start;
                        // Opacity ramps across the clip, with a key exactly
                        // under the playhead so the control shows its
                        // on-a-key state; brightness is animated but has no
                        // key here, so it shows the between-keys state.
                        c.set_key_at(Param::Opacity, start + 1.0, 0.15, Interp::Linear);
                        c.set_key_at(Param::Opacity, 6.0, 1.0, Interp::Ease);
                        c.set_key_at(Param::Opacity, start + 8.0, 0.4, Interp::Hold);
                        c.set_key_at(Param::Brightness, start + 2.0, 0.5, Interp::Linear);
                        c.set_key_at(Param::Brightness, start + 7.0, 0.8, Interp::Linear);
                    }
                }
                self.zoom = 42.0;
            }
            ShotScenario::Markers => {
                self.playhead = 14.0;
                self.project.add_marker(4.0, "cold open", 1);
                self.project.add_marker(9.5, "act two", 3);
                self.project.add_marker(21.0, "sunset", 5);
                if let Some(c) = self.project.top_video_clip_at(23.0) {
                    let cid = c.id;
                    let start = c.start;
                    self.selection.select_only(cid);
                    if let Some(c) = self.project.clip_mut(cid) {
                        c.add_marker(start + 1.5, "cutaway", 2);
                        // Re-timed, so the folded SPEED header has something
                        // to report — a fold must never hide state.
                        c.set_speed(2.0, true);
                    }
                }
                self.collapsed.insert(Section::Transform);
                self.collapsed.insert(Section::Speed);
                self.collapsed.insert(Section::Effects);
                self.inspector_scroll = self.inspector_max_scroll(r);
            }
            ShotScenario::MultiSelect => {
                // Everything on the top video lane, plus an audio clip, so the
                // capture shows a selection spanning tracks.
                let mut ids: Vec<ClipId> = self
                    .lane_rects(r)
                    .into_iter()
                    .take(2)
                    .flat_map(|(tid, _, _)| self.project.clips_on(tid))
                    .map(|c| c.id)
                    .collect();
                ids.truncate(3);
                self.selection.set(ids);
                self.playhead = 8.0;
            }
            ShotScenario::LockedTrack => {
                // Lock the first video track and select a clip on it, so both
                // the lane hatch and the inspector's note are visible.
                if let Some((tid, _, _)) = self.lane_rects(r).into_iter().next() {
                    if let Some(t) = self.project.tracks.iter_mut().find(|t| t.id == tid) {
                        t.locked = true;
                    }
                    if let Some(c) = self.project.clips_on(tid).first() {
                        let (id, media) = (c.id, c.media);
                        self.selection.select_only(id);
                        self.selected_media = Some(media);
                    }
                }
                self.playhead = 8.0;
            }
            ShotScenario::Scrolled => {
                // Zoom well in, then park the view in the middle of the
                // timeline so the scrollbar thumb sits part-way along.
                self.zoom = 120.0;
                self.playhead = self.project.duration() * 0.5;
                self.scroll_to_time(self.playhead, r);
            }
            ShotScenario::ClipMenu => {
                if let Some((tid, _, lane)) = self.lane_rects(r).into_iter().next() {
                    if let Some(clip) = self.project.clips_on(tid).first().cloned() {
                        let cr = self.clip_rect(&clip, &lane, r);
                        self.last_mouse = cr.center();
                        self.open_clip_menu(clip.id, Point::new(cr.center().x, cr.y0 + 10.0));
                    }
                }
            }
            ShotScenario::Marquee => {
                if let Some((_, _, lane)) = self.lane_rects(r).into_iter().next() {
                    let band = Rect::new(
                        r.lanes.x0 + 60.0,
                        lane.y0 - 6.0,
                        r.lanes.x0 + 430.0,
                        lane.y1 + 34.0,
                    );
                    self.drag = Some(Drag::Marquee {
                        origin: Point::new(band.x0, band.y0),
                        additive: false,
                    });
                    self.marquee = Some(band);
                    self.last_mouse = Point::new(band.x1, band.y1);
                    self.release_marquee(Point::new(band.x0, band.y0), false, r);
                    // Keep the band drawn: release_marquee only computes the
                    // selection, and the capture wants both on screen.
                    self.marquee = Some(band);
                }
            }
            _ => {}
        }
    }

    fn ensure_initial_selection(&mut self) {
        if self.initialised_selection || self.project.clips.is_empty() {
            return;
        }
        if let Some(id) = self
            .project
            .top_video_clip_at(self.playhead)
            .map(|c| c.id)
            .or_else(|| self.project.clips.first().map(|c| c.id))
        {
            self.selection.select_only(id);
        }
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
    /// Length slider for the incoming transition. `None` when the clip's
    /// transition is `None` — there is no ramp to size.
    transition_len: Option<Rect>,
    colors_y: f64,
    colors: Vec<(u8, Rect)>,
    /// y of the "EFFECTS" label; each row rect is a full-width effect row.
    effects_label_y: f64,
    effect_rows: Vec<Rect>,
    add_effect: Rect,
    split: Rect,
    delete: Rect,
    // ---- collapsible sections -------------------------------------------
    /// Section header rows, in draw order. Clicking one folds or unfolds it.
    sections: Vec<(Section, Rect)>,
    /// Every ranged control currently laid out, with the track to drag.
    nums: Vec<(NumField, Rect)>,
    /// Picture-in-picture presets: label, corner in `-1..1`, and the button.
    pip: Vec<(&'static str, (f64, f64), Rect)>,
    /// "Fill frame" — clears the transform back to the plain aspect-fit.
    reset_transform: Option<Rect>,
    /// Baseline y of the "Crop" caption above the two crop rows.
    crop_label_y: f64,
    /// Rate presets (`0.5×`, `1×`, …) as a segmented row.
    speed_presets: Vec<(f64, Rect)>,
    /// The reverse toggle.
    reverse: Option<Rect>,
    // ---- title ------------------------------------------------------------
    /// The words, edited inline through the shared editable label.
    title_text: Option<Rect>,
    title_align: Vec<(TextAlign, Rect)>,
    /// Preset colours for the title's own (authored, non-palette) colour.
    title_colors: Vec<([f64; 4], Rect)>,
    title_bold: Option<Rect>,
    title_italic: Option<Rect>,
    // ---- keyframes --------------------------------------------------------
    /// The key control at the head of each animatable row.
    keys: Vec<(Param, Rect)>,
    // ---- markers ----------------------------------------------------------
    marker_rows: Vec<(MarkerRef, Rect)>,
    /// Sub-rects of the row above: the name (click to rename) and the delete.
    add_marker: Option<Rect>,
    /// Total content extent (px) from the inspector top, for scroll clamping.
    content_height: f64,
}

impl InspectorLayout {
    /// The track laid out for `field`, if it is on screen right now.
    fn num(&self, field: NumField) -> Option<Rect> {
        self.nums.iter().find(|(f, _)| *f == field).map(|(_, r)| *r)
    }
}

/// The three states a key control shows, which have to stay distinguishable:
/// a parameter that is not animated at all, one that is animated with no key
/// on the playhead, and one with a key right here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum KeyState {
    Off,
    Between,
    OnKey,
}

/// Sub-rects of a marker row: `(name, rename, delete)`.
///
/// Renaming gets its own target rather than living on the name text, because
/// the row's main job is navigation: a marker exists to be jumped to, and
/// that has to be what clicking it does. `name` is where the label is drawn
/// and where the inline editor is placed, not a hit target.
fn marker_row_parts(row: Rect) -> (Rect, Rect, Rect) {
    let cy = row.center().y;
    let del = Rect::new(row.x1 - 22.0, cy - 11.0, row.x1, cy + 11.0);
    let rename = Rect::new(del.x0 - 24.0, cy - 11.0, del.x0 - 2.0, cy + 11.0);
    let name = Rect::new(row.x0 + 16.0, row.y0, rename.x0 - 60.0, row.y1);
    (name, rename, del)
}

/// The colours a title can be set to from the inspector.
///
/// A title's colour is authored content, not chrome: the user picked it and it
/// must survive a theme change, so unlike a clip's label these are stored as
/// real values rather than palette slots. The set is deliberately small and
/// legible against video — whites and blacks for titling, then a few
/// saturated hues — with a full picker left to Ctrl+K.
const TITLE_COLORS: [[f64; 4]; 8] = [
    [1.0, 1.0, 1.0, 1.0],
    [0.08, 0.08, 0.09, 1.0],
    [0.98, 0.83, 0.25, 1.0],
    [0.95, 0.42, 0.31, 1.0],
    [0.40, 0.78, 0.98, 1.0],
    [0.44, 0.85, 0.55, 1.0],
    [0.79, 0.55, 0.96, 1.0],
    [1.0, 1.0, 1.0, 0.55],
];

/// Rate presets offered as a segmented row beside the rate slider.
const SPEED_PRESETS: [f64; 5] = [0.25, 0.5, 1.0, 2.0, 4.0];

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
        // The document's heads are the cheap "something actually changed"
        // signal. Reloading the model every frame says nothing; a new head
        // means a real edit, local or from a peer.
        let heads = state.get_heads();
        if heads != self.doc_heads {
            self.doc_heads = heads;
            #[cfg(any(feature = "gst", feature = "decode"))]
            {
                self.monitor_stale = true;
            }
        }
        // Bring in any pending / portal-picked media as real clips (this pushes
        // straight into `self.project.media` and the CRDT).
        self.drain_import(state);
        // Drop selected clips that vanished (e.g. a remote delete), keeping
        // whatever is left selected rather than clearing the lot.
        self.selection.retain_existing(&self.project);
        // Drop a rename / menu whose target media vanished (e.g. remote delete).
        if let Some((mid, _)) = &self.bin_rename {
            if self.project.media(*mid).is_none() {
                self.bin_rename = None;
            }
        }
        match self.menu_target {
            Some(MenuTarget::Media(mid)) if self.project.media(mid).is_none() => {
                self.bin_menu.close();
                self.menu_target = None;
            }
            Some(MenuTarget::Track(tid)) if self.project.track(tid).is_none() => {
                self.bin_menu.close();
                self.menu_target = None;
            }
            Some(MenuTarget::Clip(cid)) if self.project.clip(cid).is_none() => {
                self.bin_menu.close();
                self.menu_target = None;
            }
            _ => {}
        }
        if let Some(bd) = self.bin_drag {
            if self.project.media(bd.media).is_none() {
                self.bin_drag = None;
            }
        }
        self.ensure_initial_selection();
        // Resolve which sources actually have bytes here. The reference always
        // replicates; the content is fetched lazily, so a project opened on a
        // second peer legitimately holds media this device has never seen. The
        // editor must say that in words rather than drawing a stand-in picture
        // and leaving the user to guess.
        self.projected_media = self
            .project
            .media
            .iter()
            .filter(|m| !m.source().hydration.is_local())
            .map(|m| m.id)
            .collect();
        // Keep audio waveforms current: drain completed background computes, then
        // request (off-thread) or warm (synchronous, for --shots) each source.
        self.waveforms.drain();
        let audio: Vec<(MediaId, crate::media::MediaSource)> = self
            .project
            .media
            .iter()
            .filter(|m| m.kind == MediaKind::Audio)
            .map(|m| (m.id, m.source()))
            .collect();
        for (id, src) in audio {
            if self.warm_waveforms_sync {
                self.waveforms.warm_blocking(id, &src);
            } else {
                self.waveforms.request(id, &src);
            }
        }
        // Thumbnails for visual sources: a representative frame now (cached),
        // and a real one off-thread under the decode feature.
        self.thumbnails.drain();
        let visual: Vec<(MediaId, u8, crate::media::MediaSource, f64)> = self
            .project
            .media
            .iter()
            .filter(|m| m.kind.is_visual())
            .map(|m| (m.id, m.palette, m.source(), (m.duration * 0.3).clamp(0.2, 3.0)))
            .collect();
        for (id, palette, src, at) in visual {
            self.thumbnails.ensure(id, palette, &src, at);
        }
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
            || self.waveforms.has_pending() // keep syncing until peaks land
            || self.thumbnails.has_pending() // …and until real frames land
    }

    fn needs_animation_frame(&self) -> bool {
        self.playing
            || self.shuttle != 0.0
            || self.flap_animating()
            || self.import_rx.is_some()
            || self.bin_rename.is_some() // a live rename blinks its caret
            || self.export_job.is_some() // a running render moves its progress bar
            || self.export_dest_rx.is_some() // …and a picker is still an action in flight
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
        presence: &PresenceCtx,
    ) {
        self.advance_playback();
        #[cfg(any(feature = "gst", feature = "decode"))]
        self.pump_monitor();
        self.advance_flap();
        // Destination round-trip + render progress, every frame.
        self.poll_export();
        // Track the decoded frame to the (possibly advancing) playhead.
        #[cfg(feature = "decode")]
        self.pump_preview_decode();
        let r = self.regions();
        // Arrange a rect-dependent interaction scenario once (for --shots).
        if !self.shot_interaction_applied
            && self.shot_scenario.is_interaction()
            && self.rect.width() > 1.0
        {
            self.apply_shot_interaction(&r);
            self.shot_interaction_applied = true;
        }
        // Keep the inspector scroll within the current selection's content.
        let max_scroll = self.inspector_max_scroll(&r);
        self.inspector_scroll = self.inspector_scroll.clamp(0.0, max_scroll);
        // Same for the bin: deleting sources shortens the list under us.
        let bin_max = self.bin_max_scroll(&r);
        self.bin_scroll = self.bin_scroll.clamp(0.0, bin_max);
        // Keep the playhead on screen while it is moving under its own steam,
        // and the horizontal scroll inside a timeline whose length may have
        // just changed under us (a delete, a remote edit).
        if std::mem::take(&mut self.chase_playhead) {
            self.scroll_to_time(self.playhead, &r);
        }
        self.clamp_scroll(&r);
        // Request the filmstrip frames the timeline is about to draw, now that
        // the scroll is settled and we know which clips are on screen.
        self.request_filmstrips(&r);
        // Composite this frame's preview picture before anything paints, so
        // `draw_preview` only has to blit it.
        self.pump_compositor(&r);
        self.draw_chrome(scene, transform, tr, theme, &r);
        // Editor body. When the bin is docked it draws in its column; when
        // it's an overlay flap it is drawn LAST so it floats over the body
        // behind a scrim.
        if !r.bin_overlay {
            self.draw_bin(scene, transform, tr, theme, &r);
        }
        self.draw_preview(scene, transform, tr, theme, &r);
        self.draw_transport(scene, transform, tr, theme, &r);
        // Docked, the inspector draws in its column. As an overlay it floats
        // over the preview it is editing — no scrim, so the picture the
        // sliders are changing stays visible while they change it.
        if !r.inspector_overlay {
            self.draw_inspector(scene, transform, tr, theme, &r);
        }
        self.draw_timeline(scene, transform, tr, theme, &r);
        if r.inspector_overlay && self.inspector_flap.reveal() > 0.001 {
            self.draw_inspector(scene, transform, tr, theme, &r);
        }
        if r.bin_overlay {
            // Dimming scrim, then the bin sliding in on top of it.
            if r.scrim.width() > 0.0 {
                // The themed scrim token gives the hue (raw black over a light
                // theme reads as a hole); the *strength* is ours. The token is
                // built with `from_rgb8`/`mix8`, so its own alpha is 1.0 in
                // every theme — multiplying by it dimmed nothing and painted an
                // opaque slab over the preview, transport, inspector and
                // timeline, leaving a narrow window looking like a black void.
                let a = self.bin_flap.reveal() * SCRIM_ALPHA;
                let scrim = theme.tokens.colors.surfaces.scrim;
                fill_rect(scene, transform, with_alpha(scrim, a), r.scrim);
            }
            if self.bin_flap.reveal() > 0.001 {
                self.draw_bin(scene, transform, tr, theme, &r);
            }
        }
        // Live bin→timeline drag: drop-lane highlight, insertion marker + ghost.
        self.draw_bin_drag(scene, transform, tr, theme, &r);
        // Inline rename field, drawn over the bin item's name band.
        self.draw_bin_rename(scene, transform, tr, theme, presence, &r);
        // Inline editors inside the inspector: the title's words and a
        // marker's name. Drawn after the panel so the caret is on top of it.
        if self.inspector_is_visible(&r) {
            let l = self.inspector_layout(&r);
            if let (Some((_, label)), Some(rect)) = (self.title_edit.as_mut(), l.title_text) {
                label.apply_layout_external(rect.inset(-4.0));
                label.draw(scene, transform, tr, theme, presence);
            }
            let row = self
                .marker_rename
                .as_ref()
                .and_then(|(m, _)| l.marker_rows.iter().find(|(r, _)| r.id == m.id))
                .map(|(_, row)| marker_row_parts(*row).0);
            if let (Some((_, label)), Some(rect)) = (self.marker_rename.as_mut(), row) {
                label.apply_layout_external(rect);
                label.draw(scene, transform, tr, theme, presence);
            }
        }
        if self.show_export {
            self.draw_export_modal(scene, transform, tr, theme, &r);
        }
        // The bin context menu floats above everything else.
        self.bin_menu.draw(scene, transform, tr, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let r = self.regions();
        // An in-flight inline rename owns input: forward every event to the
        // shared editable label, then reconcile its outcome. A blur-commit
        // (click outside) is intentionally not consumed, so the same click also
        // reaches the host — native inline-rename behaviour.
        if self.bin_rename.is_some() {
            let name_rect = self
                .bin_item_rects(&r)
                .into_iter()
                .find(|(m, _)| Some(*m) == self.bin_rename.as_ref().map(|(id, _)| *id))
                .map(|(_, row)| self.bin_name_rect(row));
            let (mid, outcome) = {
                let (mid, label) = self.bin_rename.as_mut().unwrap();
                if let Some(rect) = name_rect {
                    label.apply_layout_external(rect);
                }
                label.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, node_id,
                    taffy,
                );
                (*mid, label.take_outcome())
            };
            match outcome {
                EditOutcome::Committed(name) => {
                    self.commit_rename(state, mid, &name);
                    self.bin_rename = None;
                }
                EditOutcome::Cancelled => self.bin_rename = None,
                EditOutcome::None => {}
            }
            if *handled {
                return;
            }
        }
        // The title's words, edited in place in the inspector through the
        // same shared label. Committing writes the media item, so the title's
        // picture, its bin row and every clip that placed it all update from
        // one action.
        if self.title_edit.is_some() {
            let l = self.inspector_layout(&r);
            let (mid, outcome) = {
                let (mid, label) = self.title_edit.as_mut().unwrap();
                if let Some(rect) = l.title_text {
                    label.apply_layout_external(rect.inset(-4.0));
                }
                label.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, node_id,
                    taffy,
                );
                (*mid, label.take_outcome())
            };
            match outcome {
                EditOutcome::Committed(words) => {
                    self.commit_title_text(state, mid, &words);
                    self.title_edit = None;
                }
                EditOutcome::Cancelled => self.title_edit = None,
                EditOutcome::None => {}
            }
            if *handled {
                return;
            }
        }
        // A marker's name, edited in place in the inspector's list.
        if self.marker_rename.is_some() {
            let l = self.inspector_layout(&r);
            let row = self
                .marker_rename
                .as_ref()
                .and_then(|(m, _)| l.marker_rows.iter().find(|(r, _)| r.id == m.id))
                .map(|(_, row)| marker_row_parts(*row).0);
            let (target, outcome) = {
                let (target, label) = self.marker_rename.as_mut().unwrap();
                if let Some(rect) = row {
                    label.apply_layout_external(rect);
                }
                label.handle_event(
                    event, scale_factor, modifiers, state, path, handled, focus_target, node_id,
                    taffy,
                );
                (*target, label.take_outcome())
            };
            match outcome {
                EditOutcome::Committed(name) => {
                    self.commit_marker_rename(state, target, &name);
                    self.marker_rename = None;
                }
                EditOutcome::Cancelled => self.marker_rename = None,
                EditOutcome::None => {}
            }
            if *handled {
                return;
            }
        }
        // An open bin context menu is a transient overlay that owns clicks/keys.
        if self.bin_menu.is_open() {
            self.bin_menu.handle_event(
                event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy,
            );
            if let Some(idx) = self.bin_menu.take_selection() {
                self.bin_menu_choose(state, idx);
            }
            if *handled {
                return;
            }
        }
        // Right-click on a bin item or a track header opens its context menu.
        if let WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button: MouseButton::Right,
            ..
        } = event
        {
            if self.open_bin_menu_at_cursor(&r)
                || self.open_track_menu_at_cursor(&r)
                || self.open_clip_menu_at_cursor(&r)
            {
                *handled = true;
                return;
            }
        }
        match event {
            WindowEvent::KeyboardInput { event: key_event, .. }
                if key_event.state == ElementState::Pressed =>
            {
                if self.handle_key(&key_event.logical_key, modifiers, state, &r) {
                    *handled = true;
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.handle_press(state, &r, modifiers.state().shift_key());
                *handled = true;
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if self.bin_drag.is_some() {
                    // A bin→timeline drag drops a clip at the release position;
                    // a bin press that never moved may instead begin a rename.
                    self.release_bin_drag(state, &r);
                    *handled = true;
                } else if self.drag.is_some() {
                    // A rubber band resolves to a selection; a press that never
                    // travelled falls back to scrubbing where it landed.
                    if let Some(Drag::Marquee { origin, additive }) = self.drag {
                        self.release_marquee(origin, additive, &r);
                    }
                    for pane in Self::SCROLL_PANES {
                        self.scroll_bar_mut(pane).release();
                    }
                    // Commit the drag's final position / trim / grade as one
                    // action (one undo boundary), then end the drag.
                    self.commit_drag(state);
                    self.drag = None;
                    self.marquee = None;
                    *handled = true;
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                self.update_hover(&r);
                self.apply_drag(&r);
                self.update_bin_drag();
                *handled = true;
            }
            // External file drop (an OS drag-and-drop onto the window) imports
            // media, exactly like the portal pick — the whole editor accepts it.
            WindowEvent::DroppedFile(path) => {
                self.file_hovering = false;
                if self.import_one(state, path) {
                    self.import_notice = Some(import_summary(1, 1));
                } else if self.import_notice.is_none() {
                    // `import_one` normally leaves the probe's own reason here;
                    // only fall back when it somehow did not.
                    self.import_notice = Some("That file couldn't be imported".to_string());
                }
                *handled = true;
            }
            WindowEvent::HoveredFile(_) => {
                self.file_hovering = true;
                *handled = true;
            }
            WindowEvent::HoveredFileCancelled => {
                self.file_hovering = false;
                *handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (*x as f64, *y as f64),
                    MouseScrollDelta::PixelDelta(p) => (p.x / 40.0, p.y / 40.0),
                };
                let shift = modifiers.state().shift_key();
                let over_bin = self.bin_is_visible(&r) && r.bin.contains(self.last_mouse);
                let over_timeline = r.timeline.contains(self.last_mouse);
                if self.inspector_is_visible(&r) && r.inspector.contains(self.last_mouse) && !over_bin {
                    // Scroll the inspector body (its content can exceed the pane).
                    if dy.abs() > 0.0 {
                        let max = self.inspector_max_scroll(&r);
                        self.inspector_scroll = (self.inspector_scroll - dy * 28.0).clamp(0.0, max);
                        *handled = true;
                    }
                } else if over_bin {
                    // Scroll the media bin list.
                    if dy.abs() > 0.0 {
                        let max = self.bin_max_scroll(&r);
                        self.bin_scroll = (self.bin_scroll - dy * 40.0).clamp(0.0, max);
                        *handled = true;
                    }
                } else if !over_timeline {
                    // The preview and transport have nothing to scroll. A wheel
                    // there used to zoom the timeline, which is a control the
                    // pointer is nowhere near; leave the event unhandled so it
                    // reaches the host instead of moving something off-screen.
                } else if dx.abs() > 0.0 || (shift && dy.abs() > 0.0) {
                    // A horizontal wheel, or Shift + a vertical one, pans the
                    // timeline. One notch moves a fixed slice of the visible
                    // span, so panning feels the same at every zoom.
                    let notches = if dx.abs() > 0.0 { -dx } else { -dy };
                    let step = self.visible_seconds(&r) * 0.15;
                    self.scroll_by(notches * step, &r);
                    *handled = true;
                } else if dy.abs() > 0.0 {
                    // Plain wheel zooms, anchored on the time under the pointer
                    // so the frame you are looking at stays put.
                    self.zoom_at(if dy > 0.0 { 1.12 } else { 1.0 / 1.12 }, self.last_mouse.x, &r);
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    /// What the pointer is over. The editor is a canvas, so without this every
    /// region — a draggable bin row, a trim handle, a scrubber — looked
    /// identical to dead space.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        let r = self.regions();
        if self.show_export {
            let l = self.export_layout(&r);
            let over_btn = l.cancel.contains(point)
                || l.confirm.contains(point)
                || l.resolution.iter().any(|(_, rc)| rc.contains(point))
                || l.container.iter().any(|(_, rc)| rc.contains(point))
                || l.fps.iter().any(|(_, rc)| rc.contains(point));
            return Some(if over_btn { CursorIcon::Pointer } else { CursorIcon::Default });
        }
        // A live drag owns the cursor for its whole duration.
        match self.drag {
            Some(Drag::ClipMove { .. }) => return Some(CursorIcon::Grabbing),
            Some(Drag::TrimIn { .. }) | Some(Drag::TrimOut { .. }) => {
                return Some(CursorIcon::EwResize)
            }
            Some(Drag::Playhead) | Some(Drag::Scrubber) => return Some(CursorIcon::ColResize),
            Some(Drag::Marquee { .. }) => return Some(CursorIcon::Crosshair),
            _ => {}
        }
        if self.bin_drag.map(|b| b.active).unwrap_or(false) {
            return Some(CursorIcon::Grabbing);
        }
        // Scrollbars first — they float over their panes.
        for pane in Self::SCROLL_PANES {
            if let Some((track, m)) = self.scroll_pane(pane, &r) {
                if self.scroll_bar(pane).thumb_rect(track, m).is_some_and(|t| t.contains(point)) {
                    return Some(CursorIcon::Pointer);
                }
            }
        }
        // Topbar buttons.
        if [r.import_btn, r.split_btn, r.export_btn, r.menu_btn].iter().any(|b| b.contains(point)) {
            return Some(CursorIcon::Pointer);
        }
        // Bin rows can be dragged onto the timeline.
        if self.bin_is_visible(&r) && r.bin.contains(point) {
            if self.bin_item_rects(&r).iter().any(|(_, rect)| rect.contains(point)) {
                return Some(CursorIcon::Grab);
            }
            return Some(CursorIcon::Default);
        }
        // Transport.
        if self.transport_buttons(&r).iter().any(|(_, rect)| rect.contains(point)) {
            return Some(CursorIcon::Pointer);
        }
        if self.has_scrubber(&r) {
            let sc = self.scrubber_rect(&r);
            if point.x >= sc.x0 && point.x <= sc.x1 && (point.y - sc.center().y).abs() < 14.0 {
                return Some(CursorIcon::ColResize);
            }
        }
        if r.ruler.contains(point) {
            return Some(CursorIcon::ColResize);
        }
        // Track-header pills and the add-track buttons.
        if r.gutter.contains(point) {
            for (_, _, lane) in self.lane_rects(&r) {
                let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
                if header.contains(point)
                    && [
                        self.mute_pill_rect(header),
                        self.lock_pill_rect(header),
                        self.solo_pill_rect(header),
                    ]
                    .iter()
                    .any(|pill| pill.contains(point))
                {
                    return Some(CursorIcon::Pointer);
                }
            }
            if self.add_track_buttons(&r).iter().any(|(_, rect)| rect.contains(point)) {
                return Some(CursorIcon::Pointer);
            }
            return Some(CursorIcon::Default);
        }
        // Timeline clips: trim handles at the edges, grab in the body.
        if r.lanes.contains(point) {
            if let Some((cid, lane)) = self.clip_at(&r, point) {
                if !self.project.clip_editable(cid) {
                    return Some(CursorIcon::NotAllowed);
                }
                if let Some(clip) = self.project.clip(cid) {
                    let cr = self.clip_rect(clip, &lane, &r);
                    if (point.x - cr.x0).abs() <= TRIM_HIT || (point.x - cr.x1).abs() <= TRIM_HIT {
                        return Some(CursorIcon::EwResize);
                    }
                }
                return Some(CursorIcon::Grab);
            }
            return Some(CursorIcon::Crosshair);
        }
        // Inspector controls.
        if self.inspector_is_visible(&r) && r.inspector.contains(point) {
            let l = self.inspector_layout(&r);
            let sliders = [
                l.opacity, l.volume, l.brightness, l.contrast, l.saturation, l.fade_in,
                l.fade_out, l.transition_len,
            ];
            if sliders.iter().flatten().any(|t| hit_slider(*t, point)) {
                return Some(CursorIcon::Pointer);
            }
            if l.valid
                && (l.split.contains(point)
                    || l.delete.contains(point)
                    || l.add_effect.contains(point)
                    || l.transitions.iter().any(|(_, rc)| rc.contains(point))
                    || l.colors.iter().any(|(_, rc)| rc.contains(point)))
            {
                return Some(CursorIcon::Pointer);
            }
        }
        Some(CursorIcon::Default)
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
                if self.export_is_offered() {
                    self.request_export();
                }
                return true;
            }
            return false;
        }
        match key {
            Key::Named(NamedKey::Space) => {
                self.toggle_play();
                true
            }
            // Arrows nudge every selected clip a frame (Shift = one second);
            // with nothing selected they step the playhead instead.
            Key::Named(NamedKey::ArrowLeft) => {
                let step = if shift { 1.0 } else { frame_step(self.project.fps) };
                if !self.selection.is_empty() {
                    self.nudge_selected(state, -step);
                } else {
                    self.step_frame(if shift { -self.project.fps } else { -1.0 });
                }
                true
            }
            Key::Named(NamedKey::ArrowRight) => {
                let step = if shift { 1.0 } else { frame_step(self.project.fps) };
                if !self.selection.is_empty() {
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
                self.chase_playhead = true;
                true
            }
            Key::Named(NamedKey::End) => {
                self.playhead = self.project.duration();
                self.playing = false;
                self.shuttle = 0.0;
                self.chase_playhead = true;
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
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("x") => {
                self.cut_selected(state);
                true
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                self.paste_clip(state);
                true
            }
            // Ctrl+D drops a copy of the selection immediately after it.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("d") => {
                self.duplicate_selected(state);
                true
            }
            // Ctrl+A selects every clip on the timeline.
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => {
                self.select_all();
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
            // M drops a marker at the playhead and opens it for naming — an
            // unnamed tick is a marker you cannot find again.
            Key::Character(c) if c.eq_ignore_ascii_case("m") && !ctrl => {
                self.add_marker_here(state);
                true
            }
            // T makes a title, places it and puts the caret in its words.
            Key::Character(c) if c.eq_ignore_ascii_case("t") && !ctrl => {
                self.add_title(state);
                true
            }
            // , and . step between marked instants, the project's own and
            // every clip's alike.
            Key::Character(c) if (c.as_str() == "," || c.as_str() == "<") && !ctrl => {
                self.jump_to_marker(false);
                true
            }
            Key::Character(c) if (c.as_str() == "." || c.as_str() == ">") && !ctrl => {
                self.jump_to_marker(true);
                true
            }
            // Shift+Z fits the whole project to the window, the NLE standard.
            Key::Character(c) if c.eq_ignore_ascii_case("z") && !ctrl => {
                self.zoom_to_fit(r);
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
            // Tab shows and hides the inspector. On a narrow window that is
            // the only way back to a panel dismissed by Esc without hunting
            // for the topbar toggle; on a wide one it reclaims the column.
            Key::Named(NamedKey::Tab) => {
                self.toggle_inspector();
                true
            }
            Key::Named(NamedKey::Escape) => {
                // Esc dismisses an open overlay bin first, then an open
                // overlay inspector, then clears the selection — matching the
                // flap kernel's Esc-to-close.
                if self.bin_layout() == SplitLayout::Overlay
                    && self.bin_flap.is_open(SplitLayout::Overlay)
                {
                    self.bin_flap.close();
                } else if self.inspector_split() == SplitLayout::Overlay
                    && self.inspector_flap.is_open(SplitLayout::Overlay)
                {
                    self.inspector_flap.close();
                } else {
                    self.selection.clear();
                }
                true
            }
            _ => false,
        }
    }

    fn handle_press(&mut self, state: &mut AutoCommit, r: &Regions, shift: bool) {
        let p = self.last_mouse;

        // Modal owns all clicks.
        if self.show_export {
            self.press_export(state, r, p);
            return;
        }
        // Scrollbars float over their panes, so they are hit-tested before any
        // content underneath them.
        if self.press_scrollbars(r, p) {
            return;
        }
        // Hamburger toggle (overlay layout only).
        if r.bin_overlay && r.menu_btn.contains(p) {
            self.toggle_bin();
            return;
        }
        // Inspector toggle (overlay layout only).
        if r.inspector_overlay && r.inspector_btn.contains(p) {
            self.toggle_inspector();
            return;
        }
        // An open inspector overlay owns its own rect, before the preview and
        // timeline underneath it get a look.
        if r.inspector_overlay && self.inspector_is_visible(r) && r.inspector.contains(p) {
            self.press_inspector(state, r, p);
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
        // "+ Title" in the bin header.
        if self.bin_is_visible(r) && self.add_title_rect(r).contains(p) {
            self.add_title(state);
            return;
        }
        // While the bin is an open overlay, a click on the scrim (anywhere
        // outside the flap and the topbar toggle) dismisses it — matching the
        // flap kernel's scrim/Esc behaviour.
        let bin_visible = self.bin_is_visible(r);
        if r.bin_overlay && bin_visible && !r.bin.contains(p) && !r.menu_btn.contains(p) {
            self.bin_flap.close();
            return;
        }
        // Media bin (only when it's on-screen: docked, or overlay open).
        if bin_visible {
            for (mid, rect) in self.bin_item_rects(r) {
                if rect.contains(p) {
                    self.selected_media = Some(mid);
                    // How long ago this same item was last *clicked* (not
                    // programmatically selected) — `None` for a different item
                    // or a fresh one. Both the double-click and the rename are
                    // gated on it, so a freshly-imported, auto-selected item is
                    // never renamed on the first click.
                    let since = self.last_click_media.and_then(|(m, t)| {
                        (m == mid).then(|| t.elapsed().as_millis())
                    });
                    let dbl = matches!(since, Some(ms) if ms < BIN_DOUBLE_CLICK_MS);
                    // The rename window is *bounded at both ends*: fast enough
                    // and it is a double-click, slow enough and the earlier
                    // click is stale and this is simply a new press. Without
                    // the upper bound an item, once clicked, could never be
                    // dragged to the timeline again — the name band covers
                    // most of the row, which is exactly where you grab it.
                    let rename_window = matches!(
                        since,
                        Some(ms) if (BIN_DOUBLE_CLICK_MS..BIN_RENAME_WINDOW_MS).contains(&ms)
                    );
                    if dbl {
                        self.add_media_to_timeline(state, mid);
                        self.last_click_media = None;
                        self.pending_rename = None;
                    } else {
                        // Always arm a potential bin→timeline drag. When the
                        // press also falls in the rename window, the rename is
                        // only an *intent*: it fires on release if the pointer
                        // never moved, and is cancelled the moment the press
                        // becomes a drag (see `update_bin_drag`). A drag
                        // therefore always wins over a rename.
                        self.bin_drag = Some(BinDrag { media: mid, origin: p, active: false });
                        self.pending_rename = (rename_window
                            && self.bin_name_rect(rect).contains(p))
                        .then_some(mid);
                        self.last_click_media = Some((mid, Instant::now()));
                    }
                    return;
                }
            }
        }
        // Inspector controls.
        if self.inspector_is_visible(r) && r.inspector.contains(p) {
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
        if self.has_scrubber(r)
            && (scrub.contains(p)
                || (p.x >= scrub.x0 && p.x <= scrub.x1 && (p.y - scrub.center().y).abs() < 14.0))
        {
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
        // Zoom readout / fit-to-window, in the gutter's ruler corner.
        if self.zoom_chip_rect(r).contains(p) {
            self.zoom_to_fit(r);
            return;
        }
        // Timeline gutter: add-track buttons and per-track mute pills.
        if r.gutter.contains(p) {
            for (kind, rect) in self.add_track_buttons(r) {
                if rect.contains(p) {
                    self.add_track(state, kind);
                    return;
                }
            }
            for (tid, _kind, lane) in self.lane_rects(r) {
                let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
                if header.contains(p) {
                    if self.mute_pill_rect(header).contains(p) {
                        self.toggle_track_mute(state, tid);
                    } else if self.lock_pill_rect(header).contains(p) {
                        self.toggle_track_lock(state, tid);
                    } else if self.solo_pill_rect(header).contains(p) {
                        self.toggle_track_solo(state, tid);
                    }
                    return;
                }
            }
            return;
        }
        // Timeline lanes — clips, trim handles, rubber-band / scrub.
        if r.lanes.contains(p) {
            self.press_timeline(r, p, shift);
        }
    }

    /// Give every visible scrollbar first refusal on a press. Bars float over
    /// their pane's content, so they must be hit-tested before it. Returns
    /// `true` when one took the press.
    fn press_scrollbars(&mut self, r: &Regions, p: Point) -> bool {
        for pane in Self::SCROLL_PANES {
            let Some((track, m)) = self.scroll_pane(pane, r) else { continue };
            match self.scroll_bar_mut(pane).press(track, m, p) {
                ui::widget::PressOutcome::Thumb => {
                    self.drag = Some(Drag::Scrollbar(pane));
                    return true;
                }
                ui::widget::PressOutcome::Page(target) => {
                    self.set_scroll_offset(pane, target, r);
                    return true;
                }
                ui::widget::PressOutcome::Miss => {}
            }
        }
        false
    }

    /// Finish a rubber-band drag. If the band ever opened, select every clip
    /// it touches (extending the previous selection when `additive`).
    /// Otherwise the press was a plain click on empty space: clear the
    /// selection and scrub the playhead there, the behaviour a bare click on
    /// the lanes has always had.
    fn release_marquee(&mut self, origin: Point, additive: bool, r: &Regions) {
        let Some(band) = self.marquee else {
            if !additive {
                self.selection.clear();
            }
            self.playhead = snap_to_frame(self.x_to_time(origin.x, &r.lanes), self.project.fps)
                .clamp(0.0, self.project.duration());
            return;
        };
        let mut hits: Vec<ClipId> = if additive {
            self.selection.ids().to_vec()
        } else {
            Vec::new()
        };
        for (tid, _kind, lane) in self.lane_rects(r) {
            if lane.y1 < band.y0 || lane.y0 > band.y1 {
                continue;
            }
            for c in self.project.clips_on(tid) {
                let cr = self.clip_rect(c, &lane, r);
                // Any horizontal overlap with the band counts — the lane test
                // above already established the vertical one.
                if cr.x1 >= band.x0 && cr.x0 <= band.x1 && !hits.contains(&c.id) {
                    hits.push(c.id);
                }
            }
        }
        self.selection.set(hits);
    }

    /// The clip under `p`, as `(clip id, its lane rect)`. `None` over empty
    /// lane space.
    fn clip_at(&self, r: &Regions, p: Point) -> Option<(ClipId, Rect)> {
        for (tid, _kind, lane) in self.lane_rects(r) {
            if p.y < lane.y0 || p.y > lane.y1 {
                continue;
            }
            for c in self.project.clips_on(tid) {
                let cr = self.clip_rect(c, &lane, r);
                if p.x >= cr.x0 - 2.0 && p.x <= cr.x1 + 2.0 {
                    return Some((c.id, lane));
                }
            }
        }
        None
    }

    /// Press inside the lane area. On a clip: select (shift extends), then arm
    /// a trim or a move — unless its track is locked, in which case the clip
    /// still selects (so the inspector can show it) but no edit drag begins.
    /// On empty space: arm a rubber-band selection, which falls back to a
    /// playhead scrub if the press never moves.
    fn press_timeline(&mut self, r: &Regions, p: Point, shift: bool) {
        if let Some((cid, lane)) = self.clip_at(r, p) {
            if shift {
                self.selection.toggle(cid);
            } else if !self.selection.contains(cid) {
                // Clicking a clip that is already part of a multi-selection
                // keeps that selection, so the whole block can be dragged.
                self.selection.select_only(cid);
            } else {
                self.selection.set_primary(cid);
            }
            // A narrow window keeps the inspector out of the way until there
            // is something to inspect; picking a clip is exactly that moment.
            self.reveal_inspector();
            let Some(clip) = self.project.clip(cid).cloned() else { return };
            let cr = self.clip_rect(&clip, &lane, r);
            // A key diamond on the clip's keyframe lane takes the playhead to
            // that key. Checked before the trim handles and the move drag,
            // because it is the smaller, more specific target inside the same
            // body.
            if !shift && p.y >= cr.y1 - 13.0 {
                if let Some(t) = clip
                    .key_times()
                    .into_iter()
                    .find(|t| (self.time_to_x(*t, &r.lanes) - p.x).abs() <= 5.0)
                {
                    self.playhead = t.clamp(0.0, self.project.duration());
                    self.playing = false;
                    self.shuttle = 0.0;
                    return;
                }
            }
            if !self.project.clip_editable(cid) || shift {
                // A locked clip is inspectable but not draggable; a shift-click
                // is a selection gesture, not the start of a move.
                return;
            }
            if (p.x - cr.x0).abs() <= TRIM_HIT {
                self.drag = Some(Drag::TrimIn { id: cid });
            } else if (p.x - cr.x1).abs() <= TRIM_HIT {
                self.drag = Some(Drag::TrimOut { id: cid });
            } else {
                self.drag = Some(Drag::ClipMove {
                    id: cid,
                    grab_dt: self.x_to_time(p.x, &r.lanes) - clip.start,
                    track: clip.track,
                });
            }
            return;
        }
        // Empty lane area: a rubber band, resolved on release. A press that
        // never travels past MARQUEE_THRESHOLD scrubs the playhead instead,
        // so click-to-scrub still works exactly as it did.
        self.drag = Some(Drag::Marquee { origin: p, additive: shift });
        self.marquee = None;
    }

    fn press_inspector(&mut self, state: &mut AutoCommit, r: &Regions, p: Point) {
        let l = self.inspector_layout(r);
        if !l.valid {
            return;
        }
        // The slider controls start a drag and preview the click position in
        // memory; the final value is dispatched as one action on mouse-up
        // (see `commit_drag`), so a slider interaction is one undo step.
        for (track, param, drag) in [
            (l.opacity, Param::Opacity, Drag::Opacity),
            (l.volume, Param::Volume, Drag::Volume),
            (l.brightness, Param::Brightness, Drag::Brightness),
            (l.contrast, Param::Contrast, Drag::Contrast),
            (l.saturation, Param::Saturation, Drag::Saturation),
        ] {
            if let Some(track) = track {
                if hit_slider(track, p) {
                    self.drag = Some(drag);
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_param(param, v);
                    return;
                }
            }
        }
        // Fade in / out sliders (value in seconds, 0..clip-duration).
        let max = self.selected().and_then(|id| self.project.clip(id)).map(|c| c.duration()).unwrap_or(0.0);
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
        if let Some(track) = l.transition_len {
            if hit_slider(track, p) {
                self.drag = Some(Drag::TransitionLen);
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                let max = self.transition_len_max();
                self.preview_edit(move |c| c.transition_duration = (frac * max).max(0.01));
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
        // Effect rows: reorder / intensity slider / remove.
        for (i, row) in l.effect_rows.iter().enumerate() {
            let (up, down, slider, remove) = Self::effect_row_parts(*row);
            if up.contains(p) {
                self.move_effect_selected(state, i, true);
                return;
            }
            if down.contains(p) {
                self.move_effect_selected(state, i, false);
                return;
            }
            if remove.contains(p) {
                self.remove_effect_from_selected(state, i);
                return;
            }
            if hit_slider(slider, p) {
                self.drag = Some(Drag::EffectAmount { idx: i });
                let v = ((p.x - slider.x0) / slider.width()).clamp(0.0, 1.0);
                self.preview_edit(move |c| {
                    if let Some(e) = c.effects.get_mut(i) {
                        e.amount = v;
                    }
                });
                return;
            }
        }
        if l.add_effect.width() > 0.0 && l.add_effect.contains(p) {
            self.open_effect_menu(p);
            return;
        }
        // Section headers fold and unfold.
        for (s, rect) in &l.sections {
            if rect.contains(p) {
                if !self.collapsed.remove(s) {
                    self.collapsed.insert(*s);
                }
                return;
            }
        }
        // Key controls. An unkeyed parameter takes one click to start
        // animating; a keyed one opens the menu that shapes, removes and
        // steps between its keys, because "what do I do with this key" has
        // more than one answer and a second click cannot express them all.
        for (param, rect) in &l.keys {
            if rect.contains(p) {
                if self.key_state(*param) == KeyState::Off {
                    self.key_here(state, *param);
                } else {
                    self.open_key_menu(*param, p);
                }
                return;
            }
        }
        // Ranged controls — geometry, rate, title size.
        for (f, track) in l.nums.clone() {
            if hit_slider(track, p) {
                self.drag = Some(Drag::Num(f));
                let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                self.set_num(f, f.from_frac(frac));
                return;
            }
        }
        // Picture-in-picture presets, and the way back to filling the frame.
        for (_, corner, rect) in l.pip.clone() {
            if rect.contains(p) {
                self.edit_selected(state, move |c| {
                    c.transform = crate::model::Transform::pip(PIP_SCALE, corner, PIP_INSET);
                });
                return;
            }
        }
        if let Some(rect) = l.reset_transform {
            if rect.contains(p) {
                self.edit_selected(state, |c| c.transform = crate::model::Transform::default());
                return;
            }
        }
        for (rate, rect) in l.speed_presets.clone() {
            if rect.contains(p) {
                self.edit_selected(state, move |c| {
                    let rev = c.speed.reverse;
                    c.set_speed(rate, rev);
                });
                return;
            }
        }
        if let Some(rect) = l.reverse {
            if rect.contains(p) {
                self.edit_selected(state, |c| {
                    let (rate, rev) = (c.speed.rate(), c.speed.reverse);
                    c.set_speed(rate, !rev);
                });
                return;
            }
        }
        // Title properties.
        if let Some(rect) = l.title_text {
            if rect.contains(p) {
                if let Some(m) = self.selected_media_item().map(|m| m.id) {
                    self.begin_title_edit(m);
                }
                return;
            }
        }
        for (align, rect) in l.title_align.clone() {
            if rect.contains(p) {
                self.edit_title(state, move |t| t.align = align);
                return;
            }
        }
        for (col, rect) in l.title_colors.clone() {
            if rect.contains(p) {
                self.edit_title(state, move |t| t.color = col);
                return;
            }
        }
        if let Some(rect) = l.title_bold {
            if rect.contains(p) {
                self.edit_title(state, |t| t.bold = !t.bold);
                return;
            }
        }
        if let Some(rect) = l.title_italic {
            if rect.contains(p) {
                self.edit_title(state, |t| t.italic = !t.italic);
                return;
            }
        }
        // Marker rows: the name renames, the cross deletes, the rest of the
        // row takes the playhead there — which is what a marker is for.
        for (m, row) in l.marker_rows.clone() {
            if !row.contains(p) {
                continue;
            }
            let (_, rename, del) = marker_row_parts(row);
            if del.contains(p) {
                self.delete_marker(state, m);
            } else if rename.contains(p) {
                self.begin_marker_rename(m);
            } else {
                self.playhead = m.at.clamp(0.0, self.project.duration());
                self.playing = false;
                self.shuttle = 0.0;
                self.chase_playhead = true;
            }
            return;
        }
        if let Some(rect) = l.add_marker {
            if rect.contains(p) {
                self.add_marker_here(state);
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
            if self.export_running() {
                // A render in flight: this is Cancel, not Close. Stop it and
                // leave the dialog up so the outcome is visible.
                if let Some(job) = self.export_job.as_ref() {
                    job.cancel();
                }
            } else {
                self.show_export = false;
            }
            return;
        }
        if l.confirm.contains(p) {
            // Matches what the button paints: a disabled Export refuses rather
            // than starting something that can only fail.
            if self.export_is_offered() {
                self.request_export();
            }
            let _ = state;
        }
    }

    // ---- export ----------------------------------------------------------
    /// Ask the user where the render should land, then encode there. The
    /// picker is the grant: under a strict sandbox it is what earns the write
    /// to somewhere like `~/Videos`, and without it the file lands in a temp
    /// directory the user was never told about and cannot find.
    fn request_export(&mut self) {
        if self.export_job.is_some() {
            return; // one render at a time
        }
        if let Some(reason) = crate::export::unavailable_reason() {
            self.export_status = ExportStatus::Failed { reason: reason.to_string() };
            return;
        }
        let name = self.export.suggested_filename(&self.project.name);
        let ext = self.export.container.extension();
        self.export_notice = Some("Choose where to save…".to_string());
        self.export_dest_rx = Some(portal::request_export_destination(&name, ext));
    }

    /// Begin the render at `destination` on a worker thread.
    fn begin_export_to(&mut self, destination: PathBuf) {
        self.export_notice = None;
        self.export_status = ExportStatus::Rendering { progress: 0.0 };
        self.export_job = Some(crate::export::start_export(&self.project, &self.export, destination));
    }

    /// Drain the destination picker and poll the running render. Called every
    /// frame; `needs_animation_frame` keeps frames coming while a job runs, so
    /// the progress bar actually moves.
    fn poll_export(&mut self) {
        // Destination round-trip.
        let mut outcome = None;
        let mut clear = false;
        if let Some(rx) = self.export_dest_rx.as_ref() {
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
            self.export_dest_rx = None;
        }
        match outcome {
            Some(portal::ExportDestOutcome::Opened(path)) => self.begin_export_to(path),
            Some(portal::ExportDestOutcome::Cancelled) => self.export_notice = None,
            Some(portal::ExportDestOutcome::Failed(reason)) => {
                // No portal on this box. Say where the file will go rather than
                // silently dropping it somewhere the user will never look.
                let fallback = crate::export::output_path(&self.export, &self.project);
                self.export_notice = Some(format!(
                    "{reason} — saving to {}",
                    fallback.display()
                ));
                self.begin_export_to(fallback);
            }
            None => {}
        }
        // Running render.
        if let Some(job) = self.export_job.as_ref() {
            let status = job.status();
            let finished = !matches!(status, ExportStatus::Rendering { .. });
            self.export_status = status;
            if finished {
                self.export_job = None;
            }
        }
    }

    /// Whether a render is in flight (so Cancel means "stop it", not "close").
    fn export_running(&self) -> bool {
        self.export_job.is_some()
    }

    /// Whether the Export action is actually available. The dialog paints the
    /// button from this and the press path gates on it, so the offer and the
    /// behaviour cannot disagree — which they did: the dialog said "Export
    /// needs the GStreamer build" and put an enabled Export button under it.
    fn export_is_offered(&self) -> bool {
        crate::export::unavailable_reason().is_none() && !self.export_running()
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
        self.chase_playhead = true;
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
                self.chase_playhead = true;
            }
            Drag::Scrubber => self.apply_scrubber(p, r),
            Drag::Scrollbar(pane) => {
                if let Some((track, m)) = self.scroll_pane(pane, r) {
                    if let Some(px) = self.scroll_bar(pane).drag_to(track, m, p) {
                        self.set_scroll_offset(pane, px, r);
                    }
                }
            }
            Drag::Marquee { origin, .. } => {
                if origin.distance(p) > MARQUEE_THRESHOLD {
                    self.marquee = Some(Rect::new(
                        origin.x.min(p.x),
                        origin.y.min(p.y),
                        origin.x.max(p.x),
                        origin.y.max(p.y),
                    ));
                }
            }
            Drag::ClipMove { id, grab_dt, track } => {
                let raw = (self.x_to_time(p.x, &r.lanes) - grab_dt).max(0.0);
                // Snap the grabbed clip's left edge to a neighbouring edge /
                // playhead when within a few pixels, then to the frame grid.
                let snapped = self.snap_time(raw, id);
                let new_start = snap_to_frame(snapped, self.project.fps);
                let old_start = self.project.clip(id).map(|c| c.start).unwrap_or(new_start);
                let dt = new_start - old_start;
                // Resolve the lane under the cursor: a vertical drag moves the
                // clip between tracks. Only a lane that will actually accept it
                // (right kind, unlocked) becomes the target — over anything
                // else the clip keeps the track it already had, so the drag
                // degrades to a pure horizontal move instead of vanishing.
                let target = self
                    .lane_rects(r)
                    .into_iter()
                    .find(|(_, _, lane)| p.y >= lane.y0 && p.y <= lane.y1)
                    .map(|(tid, _, _)| tid)
                    .filter(|tid| self.project.can_place(id, *tid))
                    .unwrap_or(track);
                // Move the grabbed clip, then carry the rest of the selection
                // by the same time delta so a multi-clip drag stays rigid.
                // Only the grabbed clip changes lane; the others would need a
                // parallel lane each, which the timeline cannot promise.
                self.project.move_clip_to(id, target, new_start);
                if dt != 0.0 {
                    for other in self.editable_selection() {
                        if other == id {
                            continue;
                        }
                        let s = self.project.clip(other).map(|c| c.start).unwrap_or(0.0);
                        self.project.move_clip(other, (s + dt).max(0.0));
                    }
                }
                if let Some(d) = self.drag.as_mut() {
                    *d = Drag::ClipMove { id, grab_dt, track: target };
                }
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
            Drag::Opacity
            | Drag::Volume
            | Drag::Brightness
            | Drag::Contrast
            | Drag::Saturation => {
                let l = self.inspector_layout(r);
                let (track, param) = match drag {
                    Drag::Opacity => (l.opacity, Param::Opacity),
                    Drag::Volume => (l.volume, Param::Volume),
                    Drag::Brightness => (l.brightness, Param::Brightness),
                    Drag::Contrast => (l.contrast, Param::Contrast),
                    _ => (l.saturation, Param::Saturation),
                };
                if let Some(track) = track {
                    let v = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.preview_param(param, v);
                }
            }
            Drag::FadeIn => {
                if let Some(track) = self.inspector_layout(r).fade_in {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self.selected().and_then(|id| self.project.clip(id)).map(|c| c.duration()).unwrap_or(0.0);
                    self.preview_edit(move |c| c.fade_in = (frac * max).clamp(0.0, c.duration()));
                }
            }
            Drag::TransitionLen => {
                if let Some(track) = self.inspector_layout(r).transition_len {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self.transition_len_max();
                    self.preview_edit(move |c| c.transition_duration = (frac * max).max(0.01));
                }
            }
            Drag::FadeOut => {
                if let Some(track) = self.inspector_layout(r).fade_out {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    let max = self.selected().and_then(|id| self.project.clip(id)).map(|c| c.duration()).unwrap_or(0.0);
                    self.preview_edit(move |c| c.fade_out = (frac * max).clamp(0.0, c.duration()));
                }
            }
            Drag::EffectAmount { idx } => {
                if let Some(slider) = self.inspector_layout(r).effect_rows.get(idx).map(|row| Self::effect_row_parts(*row).2) {
                    let v = ((p.x - slider.x0) / slider.width()).clamp(0.0, 1.0);
                    self.preview_edit(move |c| {
                        if let Some(e) = c.effects.get_mut(idx) {
                            e.amount = v;
                        }
                    });
                }
            }
            Drag::Num(f) => {
                if let Some(track) = self.inspector_layout(r).num(f) {
                    let frac = ((p.x - track.x0) / track.width()).clamp(0.0, 1.0);
                    self.set_num(f, f.from_frac(frac));
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
            Drag::Playhead | Drag::Scrubber | Drag::Scrollbar(_) | Drag::Marquee { .. } => {}
            Drag::ClipMove { id, .. } => {
                // The whole moved selection lands in one event, so the move —
                // including the lane change and every clip carried along —
                // is a single undo boundary.
                self.dispatch_clip(doc, id);
                for other in self.editable_selection() {
                    if other != id {
                        self.dispatch_clip(doc, other);
                    }
                }
            }
            Drag::TrimIn { id } | Drag::TrimOut { id } => {
                self.dispatch_clip(doc, id);
            }
            Drag::Opacity
            | Drag::Volume
            | Drag::Brightness
            | Drag::Contrast
            | Drag::Saturation
            | Drag::FadeIn
            | Drag::FadeOut
            | Drag::TransitionLen
            | Drag::EffectAmount { .. } => {
                if let Some(id) = self.selected() {
                    self.dispatch_clip(doc, id);
                }
            }
            // A title knob writes the *source*, so it commits a media write;
            // everything else on this path is a clip field.
            Drag::Num(f) if f.is_media() => self.dispatch_selected_media(doc),
            Drag::Num(_) => {
                if let Some(id) = self.selected() {
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
                    | Drag::TransitionLen
                    | Drag::EffectAmount { .. }
                    | Drag::Num(_)
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
        if scrub.width() <= 1.0 {
            return;
        }
        let frac = ((p.x - scrub.x0) / scrub.width()).clamp(0.0, 1.0);
        self.playhead = (frac * self.project.duration()).clamp(0.0, self.project.duration());
        self.playing = false;
        // The transport scrubber spans the whole project, so it can land the
        // playhead anywhere — bring the timeline along.
        self.chase_playhead = true;
    }

    fn update_hover(&mut self, r: &Regions) {
        let p = self.last_mouse;
        // Brighten whichever scrollbar thumb the pointer is on.
        for pane in Self::SCROLL_PANES {
            match self.scroll_pane(pane, r) {
                Some((track, m)) => {
                    self.scroll_bar_mut(pane).update_hover(track, m, p);
                }
                None => {
                    self.scroll_bar_mut(pane).set_hover(false);
                }
            }
        }
        let bin_visible = self.bin_is_visible(r);
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
        // Inspector toggle, present only while the inspector is collapsed.
        if r.inspector_overlay && r.inspector_btn.width() > 0.0 {
            let rect = r.inspector_btn;
            let open = self.inspector_flap.is_open(self.inspector_split());
            let hovered = rect.contains(self.last_mouse);
            let bg = if open {
                c.accent[5]
            } else if hovered {
                c.surface_white_10
            } else {
                c.neutral[3]
            };
            fill_rrect(scene, tf, bg, rect, 7.0);
            let fg = if open { on_fill(theme, bg).1 } else { c.text_primary };
            // A panel glyph: the pane outline with its right column filled,
            // which is where the inspector lives and what the toggle reveals.
            let g = rect.inset(-8.0);
            stroke_rrect(scene, tf, fg, g, 2.0, 1.4);
            fill_rect(scene, tf, fg, Rect::new(g.x0 + g.width() * 0.55, g.y0, g.x1, g.y1));
        }
        // No project meta here. It used to be centred across the topbar, where
        // it collided with these buttons at 720px — the `--narrow` capture
        // width itself — and it carried the project *name*, which belongs to
        // the window title the OS draws. The technical meta now lives in the
        // transport, beside the timecode it belongs with.
        draw_button(scene, tf, tr, theme, r.import_btn, "Import…", false, r.import_btn.contains(self.last_mouse));
        draw_button(scene, tf, tr, theme, r.split_btn, "Split (S)", false, r.split_btn.contains(self.last_mouse));
        draw_button(scene, tf, tr, theme, r.export_btn, "Export…", true, r.export_btn.contains(self.last_mouse));
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
        let bar = if open { on_fill(theme, bg).1 } else { c.text_primary };
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
        // Invite an external file drop while the OS is hovering one over us.
        if self.file_hovering {
            fill_rect(scene, tf, with_alpha(c.accent[5], 0.10), r.bin);
            stroke_rrect(scene, tf, c.accent[5], r.bin.inset(-3.0), 8.0, 2.0);
            let hint = "Drop to import";
            let (hw, _) = tr.measure_weighted(hint, 13.0, None, TextWeight::Semibold);
            text_w(scene, tr, tf, hint, 13.0, c.accent[5], r.bin.center().x - hw as f64 / 2.0, r.bin.center().y, TextWeight::Semibold);
        }
        text_w(scene, tr, tf, "MEDIA BIN", 11.0, c.text_dim, r.bin.x0 + 14.0, r.bin.y0 + 14.0, TextWeight::Semibold);
        // A title is the one source you make rather than import, so the bin
        // offers it beside the count instead of leaving `MediaKind::Text`
        // reachable only from the palette.
        let add = self.add_title_rect(r);
        let add_hover = add.contains(self.last_mouse);
        fill_rrect(scene, tf, if add_hover { c.surface_white_10 } else { c.neutral[3] }, add, 5.0);
        stroke_rrect(scene, tf, c.border_standard, add, 5.0, 1.0);
        let at = text_centre(add, "+ Title", 10.5, TextWeight::Medium, tr);
        text_w(scene, tr, tf, "+ Title", 10.5, c.text_primary, at.x, at.y, TextWeight::Medium);
        let count = format!("{} clips", self.project.media.len());
        let (cw, _) = tr.measure(&count, 11.0, None);
        let count_x = r.bin.x1 - 14.0 - cw as f64;
        if count_x > add.x1 + 8.0 {
            text(scene, tr, tf, &count, 11.0, c.text_dim, count_x, r.bin.y0 + 14.0);
        }

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
                draw_button(scene, tf, tr, theme, btn, "Import media", true, btn.contains(self.last_mouse));
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
            // Visual media: the cached thumbnail frame (representative, or real
            // under decode). Audio: a real waveform.
            if m.kind.is_visual() {
                if let Some(t) = self.thumbnails.get(mid) {
                    blit_thumb(scene, tf, t, thumb, 5.0);
                } else {
                    for i in 0..3 {
                        let bx = thumb.x0 + 6.0 + i as f64 * 18.0;
                        let block = Rect::new(bx, thumb.y0 + 6.0, bx + 14.0, thumb.y1 - 6.0);
                        fill_rrect(scene, tf, lighten(base, 0.18), block, 2.0);
                    }
                }
            } else {
                let color = lighten(base, 0.25);
                let inner = Rect::new(thumb.x0 + 4.0, thumb.y0 + 6.0, thumb.x1 - 4.0, thumb.y1 - 6.0);
                match (self.waveforms.state(m.id), self.waveforms.peaks(m.id)) {
                    (crate::waveform::WaveformState::Ready, Some(peaks)) => {
                        draw_waveform(scene, tf, inner, peaks, 0.0, m.duration, m.duration, 1.0, color)
                    }
                    _ => draw_flat_signal(scene, tf, inner, color),
                }
            }
            // A source whose picture or waveform has not landed yet says so,
            // rather than looking identical to one that is simply blank.
            let pending = if m.kind.is_visual() {
                self.thumbnails.get(mid).is_none()
            } else {
                matches!(
                    self.waveforms.state(m.id),
                    crate::waveform::WaveformState::Pending
                        | crate::waveform::WaveformState::Unrequested
                )
            };
            if pending {
                let dot_r = 2.5;
                let cy = thumb.y1 - 8.0;
                for i in 0..3 {
                    let cx = thumb.x0 + 10.0 + i as f64 * 8.0;
                    scene.fill(
                        Fill::NonZero,
                        tf,
                        with_alpha(Color::WHITE, 0.35 + 0.2 * i as f64),
                        None,
                        &Circle::new(Point::new(cx, cy), dot_r),
                    );
                }
            }
            // kind dot.
            let kc = match m.kind {
                MediaKind::Video => c.semantic.info,
                MediaKind::Audio => c.semantic.success,
                MediaKind::Image => c.semantic.warning,
                MediaKind::Text => c.accent[5],
            };
            scene.fill(Fill::NonZero, tf, kc, None, &Circle::new(Point::new(thumb.x1 - 8.0, thumb.y0 + 8.0), 4.0));
            // text (the name is drawn by the inline rename field while editing).
            let tx = thumb.x1 + 12.0;
            let renaming = self.bin_rename.as_ref().map(|(id, _)| *id) == Some(mid);
            if !renaming {
                // Pixel-accurate elision: a character budget cuts "IMG_0001.MOV"
                // and "Wwwwwwwwwwww.mov" at the same place though one is half
                // again as wide.
                let avail = (rect.x1 - 8.0 - tx) as f32;
                let shown = elide_w(&m.name, avail, tr);
                text_w(scene, tr, tf, &shown, 13.0, c.text_primary, tx, rect.y0 + 10.0, TextWeight::Medium);
            }
            let sub = self.bin_subtitle(m);
            // A source the editor cannot use says which kind of "cannot": the
            // bytes are not here (fetchable), or they are here and are not
            // media (not fetchable — the file itself is the problem). The row
            // still shows whatever metadata the CRDT genuinely carries.
            //
            // The chip shares the sub-line: where both do not fit, the state
            // wins and the metadata is dropped, because a user who cannot play
            // the clip does not need to know its height first.
            let (long, short) = if self.media_is_unreadable(mid) {
                (Some(MEDIA_UNREADABLE), Some(MEDIA_UNREADABLE_SHORT))
            } else if self.media_is_projected(mid) {
                (Some(MEDIA_NOT_LOCAL), Some(MEDIA_NOT_LOCAL_SHORT))
            } else {
                (None, None)
            };
            let (sw, _) = tr.measure(&sub, 11.0, None);
            let right = rect.x1 - 8.0;
            // Longest wording that fits, rather than one that fits nowhere and
            // elides to "Not on this devi…". Beside the metadata when there is
            // room; in its place when there is not.
            let chip = long.zip(short).and_then(|(long, short)| {
                [
                    (tx + sw as f64 + 8.0, long),
                    (tx + sw as f64 + 8.0, short),
                    (tx, long),
                    (tx, short),
                ]
                .into_iter()
                .find(|(x0, label)| {
                    x0 + tr.measure(label, 10.5, None).0 as f64 + 14.0 <= right
                })
            });
            // The metadata line survives unless the chip had to take its place.
            if chip.map(|(x0, _)| x0 > tx).unwrap_or(true) {
                text(scene, tr, tf, &sub, 11.0, c.text_dim, tx, rect.y0 + 30.0);
            }
            if let Some((cx0, label)) = chip {
                let w = tr.measure(label, 10.5, None).0 as f64 + 14.0;
                let chip = Rect::new(cx0, rect.y0 + 28.0, cx0 + w, rect.y0 + 45.0);
                fill_rrect(scene, tf, with_alpha(c.semantic.warning, 0.20), chip, 4.0);
                stroke_rrect(scene, tf, with_alpha(c.semantic.warning, 0.65), chip, 4.0, 1.0);
                let baseline = text_baseline(chip, 10.5, tr);
                text(scene, tr, tf, label, 10.5, c.semantic.warning, chip.x0 + 7.0, baseline);
            }
        }
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Bin, r);
        scene.pop_layer();
        // Last import outcome, pinned to the foot of the pane. It used to be
        // drawn only inside the empty-bin branch, so once the bin held
        // anything at all every import error was silent — six sites wrote it
        // and one read it. Errors tint danger; a plain count stays quiet.
        if let Some(notice) = &self.import_notice {
            let failed = self.import_notice_is_error();
            let strip = Rect::new(r.bin.x0, r.bin.y1 - 34.0, r.bin.x1, r.bin.y1);
            let tint = if failed { c.semantic.danger } else { c.accent[5] };
            fill_rect(scene, tf, with_alpha(tint, 0.16), strip);
            stroke_line(scene, tf, with_alpha(tint, 0.5), strip.x0, strip.y0, strip.x1, strip.y0, 1.0);
            let col = if failed { c.semantic.danger } else { c.text_primary };
            let shown = elide_w(notice, (strip.width() - 24.0) as f32, tr);
            let baseline = text_baseline(strip, 11.0, tr);
            text(scene, tr, tf, &shown, 11.0, col, strip.x0 + 12.0, baseline);
        }
    }

    /// Whether the standing import notice reports a failure (so it is tinted
    /// as one). A refusal always names the file and carries the probe's
    /// reason; the success and progress lines never contain "not" / "n't".
    fn import_notice_is_error(&self) -> bool {
        self.import_notice
            .as_deref()
            .map(|n| {
                n.contains("could not")
                    || n.contains("couldn't")
                    || n.contains("is not")
                    || n.contains("No media")
                    || n.contains("is empty")
            })
            .unwrap_or(false)
    }

    /// Draw feedback for a live bin→timeline drag: highlight the drop lane, mark
    /// the insertion point at the drop time, and float a ghost chip at the cursor.
    fn draw_bin_drag(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let Some(bd) = self.bin_drag else { return };
        if !bd.active {
            return;
        }
        let c = &theme.tokens.colors;
        let p = self.last_mouse;
        // Drop-lane highlight + insertion marker at the snapped drop time.
        if let Some((_, _, lane)) = self.bin_drop_lane(r) {
            fill_rect(
                scene,
                tf,
                with_alpha(c.accent[5], 0.16),
                Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1),
            );
            let t = snap_to_frame(self.x_to_time(p.x, &r.lanes), self.project.fps);
            let x = self.time_to_x(t, &r.lanes).clamp(r.lanes.x0, r.lanes.x1);
            stroke_line(scene, tf, c.accent[5], x, lane.y0, x, lane.y1, 2.0);
        }
        // Ghost chip following the cursor.
        if let Some(m) = self.project.media(bd.media) {
            let (w, h) = (154.0, 30.0);
            let gr = Rect::new(p.x + 12.0, p.y - h / 2.0, p.x + 12.0 + w, p.y + h / 2.0);
            let ghost_fill = clip_color(theme, m.palette);
            fill_rrect(scene, tf, with_alpha(ghost_fill, 0.92), gr, 6.0);
            stroke_rrect(scene, tf, with_alpha(c.border_focus, 0.9), gr, 6.0, 1.0);
            let ghost_label = elide_w(&m.name, (gr.width() - 20.0) as f32, tr);
            let ghost_y = text_baseline(gr, 12.0, tr);
            text_w(
                scene, tr, tf, &ghost_label, 12.0, on_fill(theme, ghost_fill).1,
                gr.x0 + 10.0, ghost_y, TextWeight::Medium,
            );
        }
    }

    /// Draw the in-flight inline rename field over its bin row's name band.
    fn draw_bin_rename(
        &mut self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        r: &Regions,
    ) {
        let mid = match &self.bin_rename {
            Some((m, _)) => *m,
            None => return,
        };
        let bin_visible = self.bin_is_visible(r);
        if !bin_visible {
            return;
        }
        let Some(row) = self
            .bin_item_rects(r)
            .into_iter()
            .find(|(m, _)| *m == mid)
            .map(|(_, row)| row)
        else {
            return;
        };
        let rect = self.bin_name_rect(row);
        if let Some((_, label)) = self.bin_rename.as_mut() {
            label.apply_layout_external(rect);
            label.draw(scene, tf, tr, theme, presence);
        }
    }

    fn draw_preview(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.base, r.preview);
        let canvas = r.canvas;
        // A short window crushes the letterboxed canvas to a few pixels. There
        // is nothing to show at that size, and the HUD and safe-area guide
        // drawn at fixed offsets would land outside it; say so instead.
        if canvas.width() < MIN_CANVAS || canvas.height() < MIN_CANVAS {
            let msg = "Not enough room for the preview";
            let (w, _) = tr.measure(msg, 11.5, None);
            let area = Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0);
            if area.height() > 16.0 {
                text(
                    scene, tr, tf, msg, 11.5, c.text_dim,
                    area.center().x - w as f64 / 2.0,
                    area.center().y - 7.0,
                );
            }
            return;
        }
        // Everything below is clipped to the preview pane, so an overlay drawn
        // at a fixed offset from the canvas can never bleed into the bin, the
        // inspector or the transport.
        scene.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            tf,
            &Rect::new(r.preview.x0, r.preview.y0, r.preview.x1, r.transport.y0),
        );
        // Canvas backdrop (letterbox black).
        fill_rect(scene, tf, Color::BLACK, canvas);
        // One picture, produced by the GPU compositor in `crate::gpu` — the
        // *same* renderer the export runs, so the preview is byte-for-byte
        // what the export writes. It resolves every layer, grade, fade,
        // transition and effect itself; nothing here re-implements any of it.
        // The HUD below still draws on top, which is why this reads the frame
        // back and blits it rather than hooking the render target.
        let showed_real = match &self.composited {
            Some((w, h, rgba)) if *w > 0 && *h > 0 => {
                let frame = ::media::VideoFrame {
                    width: *w,
                    height: *h,
                    rgba: rgba.clone(),
                    pts_ms: 0,
                };
                blit_frame(scene, tf, &frame, canvas);
                true
            }
            _ => false,
        };
        if !showed_real {
            // No compositor on this box (no usable adapter). Say so rather
            // than inventing a picture.
            if self.project.top_video_clip_at(self.playhead).is_none() {
                let (tw, _) = tr.measure("No clip at playhead", 14.0, None);
                text(scene, tr, tf, "No clip at playhead", 14.0, c.text_dim, canvas.center().x - tw as f64 / 2.0, canvas.center().y - 8.0);
            } else {
                let msg = "Preview needs a GPU adapter";
                let (tw, _) = tr.measure(msg, 13.0, None);
                text(scene, tr, tf, msg, 13.0, c.text_dim, canvas.center().x - tw as f64 / 2.0, canvas.center().y - 7.0);
            }
        }
        // HUD overlays: the playhead's time, and nothing else.
        //
        // The frame size used to be painted in the canvas's top-right, which
        // is exactly where a corner picture-in-picture sits — and now that
        // `Transform` is real and `Transform::pip()` puts a layer there by
        // design, that collision is systematic rather than unlucky. Frame size
        // is static project metadata, so it belongs beside the timecode in the
        // transport (which states it) and in the export dialog, not painted
        // over the user's composition for the life of the project.
        //
        // What stays is per-frame information, and it takes a chip so it reads
        // over a bright picture as well as a dark one.
        let tc = timecode(self.playhead, self.project.fps);
        let (tcw, _) = tr.measure_weighted(&tc, 13.0, None, TextWeight::Bold);
        let tc_chip = Rect::new(
            canvas.x0 + 8.0,
            canvas.y0 + 6.0,
            canvas.x0 + 8.0 + tcw as f64 + 14.0,
            canvas.y0 + 28.0,
        );
        if tc_chip.x1 < canvas.x1 {
            fill_rrect(scene, tf, with_alpha(Color::BLACK, 0.55), tc_chip, 5.0);
            let baseline = text_baseline(tc_chip, 13.0, tr);
            text_w(scene, tr, tf, &tc, 13.0, Color::WHITE, tc_chip.x0 + 7.0, baseline, TextWeight::Bold);
        }
        // The clip under the playhead has no bytes on this device: say so on the
        // canvas, because the picture behind this line is a stand-in generated
        // from metadata, not the user's footage.
        if let Some(clip) = self.project.top_video_clip_at(self.playhead) {
            let unusable = if self.media_is_unreadable(clip.media) {
                Some(MEDIA_UNREADABLE)
            } else if self.media_is_projected(clip.media) {
                Some(MEDIA_NOT_LOCAL)
            } else {
                None
            };
            if let Some(msg) = unusable {
                let (mw, _) = tr.measure_weighted(msg, 12.0, None, TextWeight::Semibold);
                let pad = 8.0;
                let chip = Rect::new(
                    canvas.x0 + 10.0,
                    canvas.y1 - 32.0,
                    canvas.x0 + 10.0 + mw as f64 + pad * 2.0,
                    canvas.y1 - 10.0,
                );
                fill_rrect(scene, tf, with_alpha(Color::BLACK, 0.62), chip, 5.0);
                stroke_rrect(scene, tf, with_alpha(Color::WHITE, 0.35), chip, 5.0, 1.0);
                let baseline = text_baseline(chip, 12.0, tr);
                text_w(
                    scene, tr, tf, msg, 12.0, Color::WHITE,
                    chip.x0 + pad, baseline,
                    TextWeight::Semibold,
                );
            }
        }
        // Safe-area guide.
        stroke_rrect(scene, tf, with_alpha(Color::WHITE, 0.12), canvas.inset(-canvas.width() * 0.05), 0.0, 1.0);
        scene.pop_layer();
    }

    fn draw_transport(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        fill_rect(scene, tf, c.surfaces.raised, r.transport);
        stroke_line(scene, tf, c.border_standard, r.transport.x0, r.transport.y0, r.transport.x1, r.transport.y0, 1.0);
        // Everything below is clipped to the transport, so no control drawn
        // at a fixed offset can reach into the pane beside it.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, tf, &r.transport);
        for (action, rect) in self.transport_buttons(r) {
            let is_play = action == TransportAction::PlayPause;
            let hovered = rect.contains(self.last_mouse);
            let bg = if is_play { c.accent[5] } else if hovered { c.surface_white_10 } else { c.neutral[3] };
            scene.fill(Fill::NonZero, tf, bg, None, &Circle::new(rect.center(), rect.width() / 2.0));
            let col = if is_play { on_fill(theme, bg).1 } else { c.text_primary };
            self.draw_transport_icon(scene, tf, action, rect.center(), col);
        }
        // Timecode readout. Each part is drawn only when it fits *whole*
        // inside the transport: a number the pane edge cuts mid-digit does not
        // read as a clipped number, it reads as a shorter one — "00:42:00"
        // arriving as "00:4" states a project length that is simply false.
        // Dropping the part is the honest narrow-window answer, and the parts
        // go in order of what the user cannot get elsewhere.
        let big = timecode(self.playhead, self.project.fps);
        let total = format!("/  {}", timecode(self.project.duration(), self.project.fps));
        // Project format — the app's own content area, rather than the OS's
        // title slot. This is the only place the frame size is stated now that
        // the canvas HUD no longer paints it over the picture.
        let meta = format!(
            "{}×{} · {:.0} fps",
            self.project.width, self.project.height, self.project.fps
        );
        let (bw, _) = tr.measure_weighted(&big, 18.0, None, TextWeight::Bold);
        let (tw, _) = tr.measure(&total, 13.0, None);
        let (mw, _) = tr.measure(&meta, 11.5, None);
        let fit = readout_fit(r.transport, bw as f64, tw as f64, mw as f64);
        if let Some(x) = fit.big_x {
            text_w(scene, tr, tf, &big, 18.0, c.text_primary, x, r.transport.y0 + 8.0, TextWeight::Bold);
        }
        if let Some(x) = fit.total_x {
            text(scene, tr, tf, &total, 13.0, c.text_dim, x, r.transport.y0 + 13.0);
        }
        if let Some(x) = fit.meta_x {
            text(scene, tr, tf, &meta, 11.5, c.text_dim, x, r.transport.y0 + 14.0);
        }
        // Scrubber.
        let scrub = self.scrubber_rect(r);
        if scrub.width() <= 1.0 {
            scene.pop_layer();
            return;
        }
        fill_rrect(scene, tf, c.neutral[3], scrub, 4.0);
        let frac = (self.playhead / self.project.duration()).clamp(0.0, 1.0);
        let filled = Rect::new(scrub.x0, scrub.y0, scrub.x0 + scrub.width() * frac, scrub.y1);
        fill_rrect(scene, tf, c.accent[5], filled, 4.0);
        let knob = Circle::new(Point::new(filled.x1, scrub.center().y), 6.0);
        scene.fill(Fill::NonZero, tf, c.surfaces.raised, None, &knob);
        scene.stroke(&Stroke::new(1.5), tf, c.border_focus, None, &knob);
        if self.playing {
            text(scene, tr, tf, "PLAYING", 10.0, c.semantic.success, scrub.x0, scrub.y0 - 16.0);
        }
        scene.pop_layer();
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
            // Step-by-frame is a *double* chevron with no bar. Jump-to-end is
            // a single chevron against a bar. They used to differ by one pixel
            // of bar position and read as the same button twice.
            TransportAction::StepBack => {
                for dx in [1.0, 7.0] {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x - dx + s, ctr.y - s));
                    p.line_to((ctr.x - dx, ctr.y));
                    p.line_to((ctr.x - dx + s, ctr.y + s));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::StepFwd => {
                for dx in [1.0, 7.0] {
                    let mut p = BezPath::new();
                    p.move_to((ctr.x + dx - s, ctr.y - s));
                    p.line_to((ctr.x + dx, ctr.y));
                    p.line_to((ctr.x + dx - s, ctr.y + s));
                    p.close_path();
                    scene.fill(Fill::NonZero, tf, col, None, &p);
                }
            }
            TransportAction::ToStart => {
                fill_rect(scene, tf, col, Rect::new(ctr.x - 7.0, ctr.y - s, ctr.x - 4.5, ctr.y + s));
                let mut p = BezPath::new();
                p.move_to((ctr.x + 7.0, ctr.y - s));
                p.line_to((ctr.x - 3.0, ctr.y));
                p.line_to((ctr.x + 7.0, ctr.y + s));
                p.close_path();
                scene.fill(Fill::NonZero, tf, col, None, &p);
            }
            TransportAction::ToEnd => {
                fill_rect(scene, tf, col, Rect::new(ctr.x + 4.5, ctr.y - s, ctr.x + 7.0, ctr.y + s));
                let mut p = BezPath::new();
                p.move_to((ctr.x - 7.0, ctr.y - s));
                p.line_to((ctr.x + 3.0, ctr.y));
                p.line_to((ctr.x - 7.0, ctr.y + s));
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
        // A clip on a locked track has inert controls — every edit path checks
        // the lock — so they are veiled rather than left looking live. The
        // header above stays crisp, because that is where the reason is
        // written ("on V2 · locked").
        let locked = self.selected().map(|id| !self.project.clip_editable(id)).unwrap_or(false);
        if locked {
            let l = self.inspector_layout(r);
            let top = l
                .opacity
                .or(l.volume)
                .map(|t| t.y0 - 24.0)
                .unwrap_or(r.inspector.y0 + 170.0);
            let veil = Rect::new(r.inspector.x0, top.max(r.inspector.y0), r.inspector.x1, r.inspector.y1);
            fill_rect(scene, tf, with_alpha(c.surfaces.raised, 0.6), veil);
        }
        // Soft edges where the pane cuts its content. A row sliced in half by
        // a hard boundary reads as a broken control; the same row fading out
        // reads as "this scrolls", which is what it is. Only drawn on the edge
        // that actually has content beyond it.
        let max = self.inspector_max_scroll(r);
        let fade = |scene: &mut Scene, from_y: f64, dir: f64| {
            const STEPS: i32 = 14;
            const H: f64 = 22.0;
            for i in 0..STEPS {
                let f = i as f64 / STEPS as f64;
                let y0 = from_y + dir * H * f;
                let y1 = y0 + dir * (H / STEPS as f64);
                let band = Rect::new(
                    r.inspector.x0,
                    y0.min(y1),
                    r.inspector.x1,
                    y0.max(y1),
                );
                fill_rect(scene, tf, with_alpha(c.surfaces.raised, 0.10 + 0.9 * f), band);
            }
        };
        if self.inspector_scroll > 0.5 {
            fade(scene, r.inspector.y0, 1.0);
        }
        if self.inspector_scroll < max - 0.5 {
            fade(scene, r.inspector.y1, -1.0);
        }
        scene.pop_layer();
        // Real, grabbable scrollbar. What stood here was a painted indicator
        // with no hit-test, so the EFFECTS section below the fold could be
        // seen to exist and never reached.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Inspector, r);
    }

    fn draw_inspector_body(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        let x0 = r.inspector.x0 + PAD;
        let x1 = r.inspector.x1 - PAD;
        // The whole body scrolls together by `inspector_scroll` (clamped to 0
        // when nothing is selected, so the empty state stays put).
        let top = r.inspector.y0 - self.inspector_scroll;
        text_w(scene, tr, tf, "INSPECTOR", 11.0, c.text_dim, x0, top + 14.0, TextWeight::Semibold);
        // With more than one clip selected, say so where the count matters —
        // every edit below applies to all of them.
        if self.selection.len() > 1 {
            let label = format!("{} clips selected", self.selection.len());
            let (lw, _) = tr.measure_weighted(&label, 10.5, None, TextWeight::Medium);
            let pillr = Rect::new(x1 - lw as f64 - 14.0, top + 4.0, x1, top + 22.0);
            fill_rrect(scene, tf, with_alpha(c.accent[5], 0.22), pillr, 9.0);
            text_w(scene, tr, tf, &label, 10.5, c.accent[5], pillr.x0 + 7.0, top + 8.0, TextWeight::Medium);
        }

        let Some(id) = self.selected() else {
            text(scene, tr, tf, "No clip selected", 13.0, c.text_dim, x0, top + 60.0);
            text(scene, tr, tf, "Click a clip in the timeline", 11.5, c.text_dim, x0, top + 82.0);
            return;
        };
        let Some(clip) = self.project.clip(id).cloned() else { return };
        let media = self.project.media(clip.media).cloned();
        let is_video = media.as_ref().map(|m| m.kind.is_visual()).unwrap_or(true);
        let track_name = self.project.track(clip.track).map(|t| t.name.clone()).unwrap_or_default();

        // Header card with color tag + name (scrolls with the body).
        let tag = Rect::new(x0, top + 36.0, x0 + 6.0, top + 64.0);
        fill_rrect(scene, tf, clip_color(theme, clip.palette), tag, 3.0);
        let name = media.as_ref().map(|m| m.name.clone()).unwrap_or_else(|| "clip".into());
        let shown_name = elide_w(&name, (x1 - x0 - 14.0) as f32, tr);
        text_w(scene, tr, tf, &shown_name, 15.0, c.text_primary, x0 + 14.0, top + 38.0, TextWeight::Semibold);
        // Name the lane, and say plainly when it refuses edits — the same
        // reason the timeline hatches it.
        let sub = if self.selection_has_locked() {
            format!("on {track_name} · locked")
        } else {
            format!("on {track_name}")
        };
        let sub_col = if self.selection_has_locked() { c.semantic.warning } else { c.text_dim };
        text(scene, tr, tf, &sub, 11.5, sub_col, x0 + 14.0, top + 58.0);

        // Property rows.
        let mut y = top + 78.0;
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
        // Every slider row: a label, a right-aligned readout measured like all
        // the others, and the shared slider face carrying hover / drag state.
        let row = |scene: &mut Scene,
                       tr: &mut TextRenderer,
                       track: Option<Rect>,
                       label: &str,
                       readout: String,
                       v: f64,
                       size: f32,
                       drag: bool,
                       // `Some` when this row carries a key control, whose box
                       // the label starts after. The indent is unconditional
                       // so keying a parameter does not shuffle its row
                       // sideways; an animated one takes the accent instead.
                       keyed: Option<bool>| {
            let Some(track) = track else { return };
            let lx = if keyed.is_some() { track.x0 + KEY_BOX + 6.0 } else { track.x0 };
            let col = if keyed == Some(true) { c.accent[5] } else { c.text_dim };
            text(scene, tr, tf, label, size, col, lx, track.y0 - 20.0);
            let (pw, _) = tr.measure(&readout, size, None);
            text(scene, tr, tf, &readout, size, c.text_primary, track.x1 - pw as f64, track.y0 - 20.0);
            draw_track(scene, tf, theme, track, v, hit_slider(track, self.last_mouse), drag);
        };
        let dragging = |d: Option<Drag>, want: Drag| d == Some(want);
        // Animatable rows read through `param_at`, the same door the
        // compositor uses, so a keyframed slider follows the playhead rather
        // than showing a constant the picture is already ignoring.
        let animated = |p: Param| self.key_state(p) != KeyState::Off;
        let at = self.playhead;
        let opacity = clip.param_at(Param::Opacity, at);
        row(scene, tr, l.opacity, "Opacity", format!("{:.0}%", opacity * 100.0),
            opacity, 12.0, dragging(self.drag, Drag::Opacity), Some(animated(Param::Opacity)));
        let volume = clip.param_at(Param::Volume, at);
        row(scene, tr, l.volume, "Volume", format!("{:.0}%", volume * 100.0),
            volume, 12.0, dragging(self.drag, Drag::Volume), Some(animated(Param::Volume)));
        // Colour-grade rows. The readout shows the multiplier (0.5 == 1.00×,
        // the identity).
        for (track, label, p, d) in [
            (l.brightness, "Brightness", Param::Brightness, Drag::Brightness),
            (l.contrast, "Contrast", Param::Contrast, Drag::Contrast),
            (l.saturation, "Saturation", Param::Saturation, Drag::Saturation),
        ] {
            let v = clip.param_at(p, at);
            row(scene, tr, track, label, format!("{:.2}×", Filters::gain(v)), v, 12.0,
                dragging(self.drag, d), Some(animated(p)));
        }
        // Fade in / out (seconds). Two half-width tracks share a row; their
        // readouts right-align by measurement like every other row, rather
        // than by a magic inset that only fitted one string.
        let cd = clip.duration().max(1e-6);
        row(scene, tr, l.fade_in, "Fade In", format!("{:.1}s", clip.fade_in),
            (clip.fade_in / cd).clamp(0.0, 1.0), 11.0, dragging(self.drag, Drag::FadeIn), None);
        row(scene, tr, l.fade_out, "Fade Out", format!("{:.1}s", clip.fade_out),
            (clip.fade_out / cd).clamp(0.0, 1.0), 11.0, dragging(self.drag, Drag::FadeOut), None);
        // Transition segmented control, through the shared painter.
        if !l.transitions.is_empty() {
            text(scene, tr, tf, "Transition", 12.0, c.text_dim, x0, l.transition_y - 20.0);
            let rects: Vec<Rect> = l.transitions.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.transitions.iter().map(|(t, _)| t.label()).collect();
            let sel = l
                .transitions
                .iter()
                .position(|(t, _)| *t == clip.transition)
                .unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        // Transition length.
        if let Some(track) = l.transition_len {
            let max = clip.duration().max(0.01);
            row(
                scene, tr, Some(track), "Transition length",
                format!("{:.2}s", clip.transition_len()),
                (clip.transition_len() / max).clamp(0.0, 1.0), 11.0,
                self.drag == Some(Drag::TransitionLen), None,
            );
        }
        // Colour swatches.
        if !l.colors.is_empty() {
            text(scene, tr, tf, "Label", 12.0, c.text_dim, x0, l.colors_y - 20.0);
            for (pal, rect) in &l.colors {
                let sw = clip_color(theme, *pal);
                fill_rrect(scene, tf, sw, *rect, 6.0);
                if rect.contains(self.last_mouse) && clip.palette != *pal {
                    stroke_rrect(scene, tf, with_alpha(c.text_primary, 0.5), *rect, 6.0, 1.5);
                }
                if clip.palette == *pal {
                    // The selection ring has to read against whichever colour
                    // it rings, so it comes from the focus token rather than
                    // a hardcoded white that vanishes on the pale swatches.
                    stroke_rrect(scene, tf, c.border_focus, rect.inset(1.5), 7.0, 2.0);
                }
            }
        }
        // ---- collapsible sections ------------------------------------
        // Headers first, so a folded section still says what is inside it.
        for (s, rect) in &l.sections {
            self.draw_section_header(scene, tf, tr, theme, *rect, *s, &clip, media.as_ref());
        }
        // Every ranged control laid out this frame: geometry, rate, title
        // size. Each reads its value back through `num_value`, so a keyframed
        // one tracks the playhead rather than showing a dead constant.
        for (f, track) in &l.nums {
            let Some(v) = self.num_value(*f) else { continue };
            let keyed = f
                .param()
                .map(|p| self.key_state(p) != KeyState::Off)
                .unwrap_or(false);
            let lx = if f.param().is_some() { track.x0 + KEY_BOX + 6.0 } else { track.x0 };
            let label_col = if keyed { c.accent[5] } else { c.text_dim };
            text(scene, tr, tf, f.label(), 11.5, label_col, lx, track.y0 - 20.0);
            let readout = f.readout(v);
            let (pw, _) = tr.measure(&readout, 11.5, None);
            text(scene, tr, tf, &readout, 11.5, c.text_primary, track.x1 - pw as f64, track.y0 - 20.0);
            draw_track(
                scene, tf, theme, *track, f.to_frac(v),
                hit_slider(*track, self.last_mouse),
                self.drag == Some(Drag::Num(*f)),
            );
        }
        // Picture-in-picture presets. The glyph is the corner it puts the
        // picture in, which is the whole of what the button does.
        if !l.pip.is_empty() {
            let top = l.pip[0].2;
            text(scene, tr, tf, "Picture in picture", 11.5, c.text_dim, x0, top.y0 - 16.0);
            let inset = crate::model::Transform::pip(PIP_SCALE, (1.0, -1.0), PIP_INSET);
            let is_pip = (clip.transform.scale_x - inset.scale_x).abs() < 1e-6;
            for (_, corner, rect) in &l.pip {
                let on = is_pip
                    && (clip.transform.pos_x.signum() - corner.0).abs() < 1e-6
                    && (clip.transform.pos_y.signum() - corner.1).abs() < 1e-6;
                let hovered = rect.contains(self.last_mouse);
                let bg = if on {
                    c.accent[5]
                } else if hovered {
                    c.surface_white_10
                } else {
                    c.neutral[3]
                };
                fill_rrect(scene, tf, bg, *rect, 6.0);
                stroke_rrect(scene, tf, c.border_standard, *rect, 6.0, 1.0);
                // A miniature frame with the inset drawn in the named corner.
                let fg = if on { on_fill(theme, bg).1 } else { c.text_primary };
                let g = Rect::new(
                    rect.center().x - 11.0,
                    rect.center().y - 7.0,
                    rect.center().x + 11.0,
                    rect.center().y + 7.0,
                );
                stroke_rrect(scene, tf, with_alpha(fg, 0.5), g, 2.0, 1.0);
                let iw = g.width() * 0.42;
                let ih = g.height() * 0.42;
                let ix = if corner.0 < 0.0 { g.x0 + 2.0 } else { g.x1 - 2.0 - iw };
                let iy = if corner.1 < 0.0 { g.y0 + 2.0 } else { g.y1 - 2.0 - ih };
                fill_rrect(scene, tf, fg, Rect::new(ix, iy, ix + iw, iy + ih), 1.5);
            }
        }
        if l.crop_label_y > 0.0 {
            text(scene, tr, tf, "Crop", 11.5, c.text_dim, x0, l.crop_label_y);
        }
        if let Some(rect) = l.reset_transform {
            let plain = clip.transform.is_identity();
            draw_button_enabled(
                scene, tf, tr, theme, rect, "Fill frame", false,
                rect.contains(self.last_mouse), !plain,
            );
        }
        // Rate presets + the reverse toggle.
        if !l.speed_presets.is_empty() {
            let rects: Vec<Rect> = l.speed_presets.iter().map(|(_, r)| *r).collect();
            let labels: Vec<String> = l
                .speed_presets
                .iter()
                .map(|(rate, _)| {
                    if (rate - rate.round()).abs() < 1e-6 {
                        format!("{}×", rate.round() as i64)
                    } else {
                        format!("{rate}×")
                    }
                })
                .collect();
            let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
            // No preset is "selected" unless the rate is exactly on one —
            // a segmented control that always highlights something would
            // claim 1.37× was 1×.
            let sel = l
                .speed_presets
                .iter()
                .position(|(rate, _)| (rate - clip.speed.rate()).abs() < 1e-6)
                .unwrap_or(usize::MAX);
            draw_segmented(scene, tf, tr, theme, &rects, &refs, sel, self.last_mouse);
        }
        if let Some(rect) = l.reverse {
            let on = clip.speed.reverse;
            draw_button(
                scene, tf, tr, theme, rect,
                if on { "Playing backwards" } else { "Play backwards" },
                on, rect.contains(self.last_mouse),
            );
        }
        // ---- title ----------------------------------------------------
        if let Some(rect) = l.title_text {
            let text_src = media.as_ref().and_then(|m| m.text.clone()).unwrap_or_default();
            text(scene, tr, tf, "Words", 11.5, c.text_dim, rect.x0, rect.y0 - 16.0);
            let editing = self.title_edit.as_ref().map(|(m, _)| Some(*m) == media.as_ref().map(|m| m.id));
            fill_rrect(scene, tf, c.surface_white_5, rect, 6.0);
            stroke_rrect(
                scene, tf,
                if editing == Some(true) { c.border_focus } else { c.border_standard },
                rect, 6.0, 1.0,
            );
            // While the inline editor owns this box it draws the words itself
            // (with a caret); otherwise show them, or say plainly that there
            // are none rather than leaving an empty box that looks broken.
            if editing != Some(true) {
                let shown = text_src.content.replace('\n', " ⏎ ");
                let (shown, col) = if shown.trim().is_empty() {
                    ("Click to type a title".to_string(), c.text_dim)
                } else {
                    (elide_w(&shown, (rect.width() - 20.0) as f32, tr), c.text_primary)
                };
                let baseline = text_baseline(rect, 13.0, tr);
                text(scene, tr, tf, &shown, 13.0, col, rect.x0 + 10.0, baseline);
            }
        }
        if !l.title_align.is_empty() {
            let cur = media.as_ref().and_then(|m| m.text.as_ref()).map(|t| t.align).unwrap_or_default();
            let rects: Vec<Rect> = l.title_align.iter().map(|(_, r)| *r).collect();
            let labels: Vec<&str> = l.title_align.iter().map(|(a, _)| a.label()).collect();
            let sel = l.title_align.iter().position(|(a, _)| *a == cur).unwrap_or(0);
            draw_segmented(scene, tf, tr, theme, &rects, &labels, sel, self.last_mouse);
        }
        if !l.title_colors.is_empty() {
            let cur = media.as_ref().and_then(|m| m.text.as_ref()).map(|t| t.color).unwrap_or([1.0; 4]);
            text(scene, tr, tf, "Colour", 11.5, c.text_dim, x0, l.title_colors[0].1.y0 - 16.0);
            for (col, rect) in &l.title_colors {
                let sw = title_color(*col);
                // A swatch can be white, near-black or translucent, so it is
                // drawn on a chequer-free plate with a border: without one the
                // white swatch vanishes on a light theme and the dark one on a
                // dark theme.
                fill_rrect(scene, tf, c.neutral[3], *rect, 6.0);
                fill_rrect(scene, tf, sw, *rect, 6.0);
                stroke_rrect(scene, tf, with_alpha(c.text_primary, 0.25), *rect, 6.0, 1.0);
                if col.iter().zip(cur.iter()).all(|(a, b)| (a - b).abs() < 1e-6) {
                    stroke_rrect(scene, tf, c.border_focus, rect.inset(1.5), 7.0, 2.0);
                } else if rect.contains(self.last_mouse) {
                    stroke_rrect(scene, tf, with_alpha(c.text_primary, 0.6), *rect, 6.0, 1.5);
                }
            }
        }
        for (rect, on, label) in [
            (l.title_bold, media.as_ref().and_then(|m| m.text.as_ref()).map(|t| t.bold).unwrap_or(false), "Bold"),
            (l.title_italic, media.as_ref().and_then(|m| m.text.as_ref()).map(|t| t.italic).unwrap_or(false), "Italic"),
        ] {
            let Some(rect) = rect else { continue };
            draw_button(scene, tf, tr, theme, rect, label, on, rect.contains(self.last_mouse));
        }
        // ---- markers --------------------------------------------------
        for (m, row) in &l.marker_rows {
            let (name_rect, rename, del) = marker_row_parts(*row);
            let hovered = row.contains(self.last_mouse);
            if hovered {
                fill_rrect(scene, tf, c.surface_white_5, *row, 6.0);
            }
            let Some((_, mk, _)) =
                self.project.all_markers().into_iter().find(|(_, k, _)| k.id == m.id)
            else {
                continue;
            };
            let col = clip_color(theme, mk.palette);
            scene.fill(
                Fill::NonZero, tf, col, None,
                &Circle::new(Point::new(row.x0 + 7.0, row.center().y), 4.0),
            );
            let renaming = self.marker_rename.as_ref().map(|(r, _)| r.id) == Some(m.id);
            if !renaming {
                let name = if mk.name.trim().is_empty() { "Marker" } else { mk.name.as_str() };
                let shown = elide_w(name, name_rect.width() as f32, tr);
                let baseline = text_baseline(*row, 12.0, tr);
                text(scene, tr, tf, &shown, 12.0, c.text_primary, name_rect.x0, baseline);
                // A clip marker says which clip it travels with, because
                // moving that clip moves the marker and nothing else would
                // explain why the time changed.
                let stamp = timecode(m.at, self.project.fps);
                let (sw2, _) = tr.measure(&stamp, 11.0, None);
                let stamp_col = if m.clip.is_some() { c.text_tertiary } else { c.text_dim };
                text(scene, tr, tf, &stamp, 11.0, stamp_col, rename.x0 - 8.0 - sw2 as f64, baseline);
            }
            // Rename and delete only appear under the pointer, so a resting
            // list reads as names and times rather than as a wall of icons.
            if hovered {
                for (rect, icon, tint) in [
                    (rename, ui::icons::Icon::Pencil, c.text_primary),
                    (del, ui::icons::Icon::X, c.semantic.danger),
                ] {
                    if rect.contains(self.last_mouse) {
                        fill_rrect(scene, tf, with_alpha(tint, 0.18), rect, 4.0);
                    }
                    let g = 11.0;
                    let box_ = Rect::new(
                        rect.center().x - g / 2.0, rect.center().y - g / 2.0,
                        rect.center().x + g / 2.0, rect.center().y + g / 2.0,
                    );
                    ui::icons::draw_icon(
                        scene, tf, box_, icon, tint,
                        ui::icons::LUCIDE_DEFAULT_STROKE_PX,
                    );
                }
            }
        }
        if let Some(rect) = l.add_marker {
            if l.marker_rows.is_empty() {
                text(scene, tr, tf, "Nothing marked yet", 11.5, c.text_dim, x0, rect.y0 - 16.0);
            }
            draw_button(
                scene, tf, tr, theme, rect, "+ Marker at playhead (M)", false,
                rect.contains(self.last_mouse),
            );
        }
        // ---- key controls ---------------------------------------------
        // Drawn last so a diamond sits over its row rather than under it.
        for (param, rect) in &l.keys {
            draw_key_control(scene, tf, theme, *rect, self.key_state(*param), rect.contains(self.last_mouse));
        }
        // Effects stack (video only).
        if is_video {
        for (i, row) in l.effect_rows.iter().enumerate() {
            let Some(e) = clip.effects.get(i) else { continue };
            let (up, down, slider, remove) = Self::effect_row_parts(*row);
            fill_rrect(scene, tf, c.surface_white_5, *row, 6.0);
            // Reorder / remove: real icons on hover-lit hit targets, sized to
            // the pointer rather than to the glyph. These were raw Unicode
            // arrows in 16x18px boxes with no hover state at all.
            let last = i + 1 == clip.effects.len();
            for (rect, icon, enabled) in [
                (up, ui::icons::Icon::ChevronUp, i > 0),
                (down, ui::icons::Icon::ChevronDown, !last),
            ] {
                if enabled && rect.contains(self.last_mouse) {
                    fill_rrect(scene, tf, c.overlay_medium, rect, 4.0);
                }
                let col = if enabled { c.text_primary } else { c.text_tertiary };
                let g = 12.0;
                let box_ = Rect::new(
                    rect.center().x - g / 2.0,
                    rect.center().y - g / 2.0,
                    rect.center().x + g / 2.0,
                    rect.center().y + g / 2.0,
                );
                ui::icons::draw_icon(scene, tf, box_, icon, col, ui::icons::LUCIDE_DEFAULT_STROKE_PX);
            }
            // Name.
            let name_y = text_baseline(*row, 12.0, tr);
            text_w(scene, tr, tf, e.kind.label(), 12.0, c.text_primary, down.x1 + 8.0, name_y, TextWeight::Medium);
            // Intensity slider.
            draw_track(
                scene, tf, theme, slider, e.amount,
                hit_slider(slider, self.last_mouse),
                self.drag == Some(Drag::EffectAmount { idx: i }),
            );
            // Remove.
            if remove.contains(self.last_mouse) {
                fill_rrect(scene, tf, with_alpha(c.semantic.danger, 0.18), remove, 4.0);
            }
            let g = 12.0;
            let xbox = Rect::new(
                remove.center().x - g / 2.0,
                remove.center().y - g / 2.0,
                remove.center().x + g / 2.0,
                remove.center().y + g / 2.0,
            );
            ui::icons::draw_icon(
                scene, tf, xbox, ui::icons::Icon::X, c.semantic.danger,
                ui::icons::LUCIDE_DEFAULT_STROKE_PX,
            );
        }
        // Add-effect button.
        let add_hover = l.add_effect.contains(self.last_mouse);
        fill_rrect(scene, tf, if add_hover { c.surface_white_10 } else { c.neutral[3] }, l.add_effect, 6.0);
        stroke_rrect(scene, tf, c.border_standard, l.add_effect, 6.0, 1.0);
        text(scene, tr, tf, "+ Add effect", 12.0, c.text_primary, l.add_effect.x0 + 12.0, l.add_effect.center().y - 8.0);
        }

        if l.valid {
            draw_button(scene, tf, tr, theme, l.split, "Split", false, l.split.contains(self.last_mouse));
            draw_danger_button(scene, tf, tr, theme, l.delete, "Delete", l.delete.contains(self.last_mouse));
        }
    }

    /// One collapsible section header: a fold arrow, the name, and — when it
    /// is folded — a summary of what is inside it.
    ///
    /// The summary is the point. Folding a section away is a view choice, but
    /// a fold that hid "this clip plays at 4x in reverse" would turn a view
    /// choice into missing state, and the user would have no way to know the
    /// clip was re-timed without unfolding every section on every clip.
    fn draw_section_header(
        &self,
        scene: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        section: Section,
        clip: &Clip,
        media: Option<&MediaItem>,
    ) {
        let c = &theme.tokens.colors;
        let open = !self.collapsed.contains(&section);
        if rect.contains(self.last_mouse) {
            fill_rrect(scene, tf, c.surface_white_5, rect, 5.0);
        }
        let g = 11.0;
        let arrow = Rect::new(
            rect.x0 + 1.0,
            rect.center().y - g / 2.0,
            rect.x0 + 1.0 + g,
            rect.center().y + g / 2.0,
        );
        ui::icons::draw_icon(
            scene, tf, arrow,
            if open { ui::icons::Icon::ChevronDown } else { ui::icons::Icon::ChevronRight },
            c.text_dim,
            ui::icons::LUCIDE_DEFAULT_STROKE_PX,
        );
        let baseline = text_baseline(rect, 11.0, tr);
        text_w(
            scene, tr, tf, section.label(), 11.0, c.text_dim,
            rect.x0 + 18.0, baseline, TextWeight::Semibold,
        );
        if open {
            return;
        }
        let Some(summary) = self.section_summary(section, clip, media) else { return };
        let (w, _) = tr.measure(&summary, 11.0, None);
        // A summary that reports something other than the default is worth
        // reading, so it takes the accent rather than the dim tone.
        text(scene, tr, tf, &summary, 11.0, c.accent[5], rect.x1 - w as f64, baseline);
    }

    /// What a folded section reports about itself, or `None` when everything
    /// inside it is at its default and there is nothing to report.
    fn section_summary(
        &self,
        section: Section,
        clip: &Clip,
        media: Option<&MediaItem>,
    ) -> Option<String> {
        match section {
            Section::Transform => {
                if clip.transform.is_identity() {
                    return None;
                }
                let t = &clip.transform;
                let mut parts = Vec::new();
                if (t.scale_x - 1.0).abs() > 1e-6 || (t.scale_y - 1.0).abs() > 1e-6 {
                    parts.push(format!("{:.2}×", t.scale_x));
                }
                if t.rotation.abs() > 1e-6 {
                    parts.push(format!("{:.0}°", t.rotation));
                }
                if t.is_cropped() {
                    parts.push("cropped".into());
                }
                if parts.is_empty() {
                    parts.push("moved".into());
                }
                Some(parts.join(" · "))
            }
            Section::Speed => (!clip.speed.is_normal()).then(|| clip.speed.label()),
            Section::Title => {
                let t = media?.text.as_ref()?;
                let first = t.content.lines().next().unwrap_or("").trim();
                Some(if first.is_empty() { "no words yet".into() } else { first.to_string() })
            }
            Section::Effects => (!clip.effects.is_empty()).then(|| {
                if clip.effects.len() == 1 {
                    clip.effects[0].kind.label().to_string()
                } else {
                    format!("{} effects", clip.effects.len())
                }
            }),
            Section::Markers => {
                let n = self.project.all_markers().len();
                (n > 0).then(|| if n == 1 { "1 marker".into() } else { format!("{n} markers") })
            }
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

        // --- markers ---
        // Every marker on one absolute timeline: the project's own, plus
        // each clip's rebased onto where that clip sits. A flag on the
        // ruler with a hairline down the lanes, so a marked instant can be
        // lined up against every track at once.
        for (at, marker, on_clip) in self.project.all_markers() {
            let x = self.time_to_x(at, &r.lanes);
            if x < r.ruler.x0 - 1.0 || x > r.ruler.x1 {
                continue;
            }
            let col = clip_color(theme, marker.palette);
            stroke_line(scene, tf, with_alpha(col, 0.55), x, r.lanes.y0, x, r.timeline.y1, 1.0);
            // A clip's marker is drawn as a smaller tick than a project
            // one: it belongs to the footage, not to the timeline.
            let h = if on_clip.is_some() { 6.0 } else { 10.0 };
            let flag = Rect::new(x, r.ruler.y1 - h, x + 3.0, r.ruler.y1);
            fill_rect(scene, tf, col, flag);
            if on_clip.is_none() && !marker.name.is_empty() {
                text(scene, tr, tf, &marker.name, 9.0, col, x + 6.0, r.ruler.y1 - 11.0);
            }
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
            // mute + lock pills (clickable toggles)
            let muted = track.map(|t| t.muted).unwrap_or(false);
            let locked = track.map(|t| t.locked).unwrap_or(false);
            let solo = track.map(|t| t.solo).unwrap_or(false);
            for (pill, on, label, on_bg) in [
                (self.mute_pill_rect(header), muted, "M", c.semantic.danger),
                (self.lock_pill_rect(header), locked, "L", c.semantic.warning),
                (self.solo_pill_rect(header), solo, "S", c.semantic.info),
            ] {
                let pill_hover = pill.contains(self.last_mouse);
                let pill_bg = if on {
                    on_bg
                } else if pill_hover {
                    c.surface_white_10
                } else {
                    c.neutral[3]
                };
                let (pill_bg, on_fg) = on_fill(theme, pill_bg);
                fill_rrect(scene, tf, pill_bg, pill, 4.0);
                let fg = if on { on_fg } else { c.text_dim };
                let at = text_centre(pill, label, 10.0, TextWeight::Regular, tr);
                text(scene, tr, tf, label, 10.0, fg, at.x, at.y);
            }
            let _ = locked;
        }

        // Clip bodies clipped to the lane area so they don't bleed over the gutter/ruler.
        scene.push_layer(Fill::NonZero, vello::peniko::BlendMode::default(), 1.0, tf, &Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1));
        for (tid, kind, lane) in &lanes {
            for clip in self.project.clips_on(*tid) {
                self.draw_clip(scene, tf, tr, theme, clip, lane, *kind, r);
            }
        }
        // A lane that will not be heard or seen — muted, or not soloed while
        // something else of its kind is — greys back, so the pill state is
        // legible from the clips themselves and not only from the header.
        for (tid, _kind, lane) in &lanes {
            if self.project.track_audible(*tid) {
                continue;
            }
            let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, with_alpha(c.surfaces.base, 0.45), full);
        }
        // A locked lane says so across its whole width, drawn *over* its clips:
        // they grey back under a diagonal hatch, so "my drag did nothing"
        // always has a visible reason rather than reading as a dead app.
        for (tid, _kind, lane) in &lanes {
            if !self.project.track(*tid).map(|t| t.locked).unwrap_or(false) {
                continue;
            }
            let full = Rect::new(r.lanes.x0, lane.y0, r.lanes.x1, lane.y1);
            fill_rect(scene, tf, with_alpha(c.surfaces.base, 0.5), full);
            let h = full.height();
            let mut x = full.x0 - h;
            while x < full.x1 {
                stroke_line(scene, tf, with_alpha(c.text_dim, 0.16), x, full.y1, x + h, full.y0, 1.0);
                x += 12.0;
            }
        }
        // Live rubber band, over the clips it is sweeping.
        if let Some(band) = self.marquee {
            fill_rect(scene, tf, with_alpha(c.accent[5], 0.24), band);
            stroke_rrect(scene, tf, c.border_focus, band, 2.0, 1.5);
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
        text_w(scene, tr, tf, &tag, 10.0, on_fill(theme, c.accent[5]).1, tagx, r.ruler.y0 + 3.0, TextWeight::Bold);

        // Zoom readout, doubling as fit-to-window. Without it there was no way
        // to tell how far in you were, and no way back out but wheeling.
        let chip = self.zoom_chip_rect(r);
        let chip_hover = chip.contains(self.last_mouse);
        fill_rrect(scene, tf, if chip_hover { c.overlay_medium } else { c.neutral[3] }, chip, 5.0);
        stroke_rrect(scene, tf, c.border_standard, chip, 5.0, 1.0);
        let zl = if self.zoom >= 100.0 {
            format!("{:.0} px/s", self.zoom)
        } else {
            format!("{:.1} px/s", self.zoom)
        };
        let at = text_centre(chip, &zl, 10.0, TextWeight::Regular, tr);
        text(scene, tr, tf, &zl, 10.0, c.text_dim, at.x, at.y);

        // Empty timeline: say what to do, rather than showing bare lanes.
        if self.project.clips.is_empty() {
            let area = Rect::new(r.lanes.x0, r.lanes.y0, r.lanes.x1, r.timeline.y1);
            let line = if self.project.media.is_empty() {
                "Import media, then drag it here"
            } else {
                "Drag a clip from the media bin onto a track"
            };
            let (lw, _) = tr.measure(line, 12.5, None);
            text(
                scene, tr, tf, line, 12.5, c.text_dim,
                area.center().x - lw as f64 / 2.0, area.center().y - 8.0,
            );
        }

        // Add-track buttons in the gutter footer.
        for (kind, rect) in self.add_track_buttons(r) {
            let hover = rect.contains(self.last_mouse);
            fill_rrect(scene, tf, if hover { c.surface_white_10 } else { c.neutral[3] }, rect, 6.0);
            stroke_rrect(scene, tf, c.border_standard, rect, 6.0, 1.0);
            let label = if kind == TrackKind::Video { "+ Video" } else { "+ Audio" };
            let at = text_centre(rect, label, 11.0, TextWeight::Regular, tr);
            text(scene, tr, tf, label, 11.0, c.text_primary, at.x, at.y);
        }

        // Horizontal scrollbar, floated over the bottom of the lane area.
        self.draw_scrollbar(scene, tf, theme, ScrollPane::Timeline, r);
    }

    /// Paint one pane's overlay scrollbar. Geometry and thumb come from the
    /// toolkit's shared scrollbar model and the colours from the same theme
    /// tokens `DocumentViewport` reads, so all three bars here are the fleet's
    /// overlay scrollbar rather than three lookalikes.
    fn draw_scrollbar(
        &self,
        scene: &mut Scene,
        tf: Affine,
        theme: &Theme,
        pane: ScrollPane,
        r: &Regions,
    ) {
        let c = &theme.tokens.colors;
        let Some((track, m)) = self.scroll_pane(pane, r) else { return };
        let bar = self.scroll_bar(pane);
        let Some(thumb) = bar.thumb_rect(track, m) else { return };
        let radius = SCROLLBAR_THICKNESS / 2.0;
        fill_rrect(scene, tf, with_alpha(c.surfaces.base, 0.55), track, radius);
        let col = if bar.is_active() {
            with_alpha(c.text_dim, 0.85)
        } else {
            with_alpha(c.text_dim, 0.55)
        };
        fill_rrect(scene, tf, col, thumb, radius);
    }

    fn draw_clip(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, clip: &Clip, lane: &Rect, kind: TrackKind, r: &Regions) {
        let c = &theme.tokens.colors;
        let cr = self.clip_rect(clip, lane, r);
        if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
            return;
        }
        let base = clip_color(theme, clip.palette);
        let selected = self.selection.contains(clip.id);
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
                // A filmstrip: one frame **per cell**, walking the clip's
                // trimmed source window, so the strip shows the shot changing.
                // Tiling one thumbnail across every cell is not a filmstrip;
                // it is one picture repeated. The frames themselves are
                // requested in `request_filmstrips` (which needs `&mut self`).
                let (cell_w, n) = filmstrip_cells(inner);
                let span = clip.out_point - clip.in_point;
                for i in 0..n {
                    let bx = inner.x0 + 4.0 + i as f64 * (cell_w + 2.0);
                    if bx + cell_w > inner.x1 { break; }
                    let cell = Rect::new(bx, inner.y0 + 3.0, bx + cell_w, inner.y1 - 3.0);
                    let t = clip.in_point + ((i as f64 + 0.5) / n as f64) * span;
                    // `frame_at` already degrades exact → nearby-decoded →
                    // the generated representative, so a cell only comes back
                    // empty when the source has no picture at all.
                    match self.thumbnails.frame_at(clip.media, t) {
                        Some(th) => blit_thumb(scene, tf, th, cell, 2.0),
                        None => fill_rrect(scene, tf, lighten(base, 0.16), cell, 2.0),
                    }
                }
                // A source that could not be decoded says so once, over the
                // strip, rather than leaving a row of blanks to be read as
                // "this clip has no picture".
                if self.thumbnails.is_undecodable(clip.media) && cr.width() > 90.0 {
                    let msg = "No preview";
                    let (mw, _) = tr.measure(msg, 10.0, None);
                    text(
                        scene,
                        tr,
                        tf,
                        msg,
                        10.0,
                        with_alpha(Color::WHITE, 0.7),
                        inner.center().x - mw as f64 / 2.0,
                        inner.center().y - 6.0,
                    );
                }
            } else {
                let color = lighten(base, 0.3);
                let media_dur = self.project.media(clip.media).map(|m| m.duration).unwrap_or(0.0);
                // Only real peaks are drawn as a waveform. Every other state —
                // still computing, never asked, or unreadable — draws the flat
                // line, because none of them know what the audio looks like.
                match (self.waveforms.state(clip.media), self.waveforms.peaks(clip.media)) {
                    (crate::waveform::WaveformState::Ready, Some(peaks)) => draw_waveform(
                        scene, tf, inner, peaks, clip.in_point, clip.out_point, media_dur,
                        clip.volume, color,
                    ),
                    _ => draw_flat_signal(scene, tf, inner, color),
                }
            }
        }
        // A source the clip cannot draw from: hatch the body so it cannot be
        // mistaken for one whose footage is here, and name which kind of
        // problem it is when the clip is wide enough to hold the words.
        let unusable = if self.media_is_unreadable(clip.media) {
            Some(MEDIA_UNREADABLE)
        } else if self.media_is_projected(clip.media) {
            Some(MEDIA_NOT_LOCAL)
        } else {
            None
        };
        if let Some(msg) = unusable {
            hatch_rect(scene, tf, with_alpha(c.semantic.warning, 0.30), cr, 7.0);
            if cr.width() > 118.0 && inner.height() > 14.0 {
                let (mw, mh) = tr.measure(msg, 10.0, None);
                let pill = Rect::new(
                    inner.center().x - mw as f64 / 2.0 - 6.0,
                    inner.center().y - mh as f64 / 2.0 - 2.0,
                    inner.center().x + mw as f64 / 2.0 + 6.0,
                    inner.center().y + mh as f64 / 2.0 + 2.0,
                );
                fill_rrect(scene, tf, with_alpha(Color::BLACK, 0.55), pill, 4.0);
                text(
                    scene, tr, tf, msg, 10.0,
                    with_alpha(Color::WHITE, 0.95),
                    pill.x0 + 6.0,
                    pill.y0 + 2.0,
                );
            }
        }
        // name label
        let label = self.project.media(clip.media).map(|m| m.name.clone()).unwrap_or_default();
        // A re-timed clip carries a rate badge in the header's right end, so
        // the name has to elide before it rather than running underneath it.
        let badge_w = if clip.speed.is_normal() {
            0.0
        } else {
            tr.measure_weighted(&clip.speed.label(), 9.5, None, TextWeight::Bold).0 as f64 + 18.0
        };
        let avail = (cr.width() - 10.0 - badge_w).max(4.0) as f32;
        if cr.width() > 30.0 {
            let shown = elide_w(&label, avail, tr);
            // The name sits on the clip's header strip, whose colour comes
            // from the user's chosen label — so the text colour has to be
            // resolved against it rather than assumed white.
            let (_, name_col) = on_fill(theme, darken(base, 0.3));
            text_w(scene, tr, tf, &shown, 11.0, name_col, cr.x0 + 5.0, cr.y0 + 3.0, TextWeight::Medium);
        }
        // transition marker at head
        if clip.transition != Transition::None && cr.width() > 16.0 {
            let mk = triangle_corner(cr.x0, cr.y0, 12.0);
            scene.fill(Fill::NonZero, tf, with_alpha(on_fill(theme, base).1, 0.85), None, &mk);
        }
        // A re-timed clip says its rate on its own body. Speed changes the
        // clip's *length*, so without a badge a 2x clip is indistinguishable
        // from one that was simply trimmed shorter.
        if !clip.speed.is_normal() && cr.width() > 52.0 {
            let label = clip.speed.label();
            let (lw, _) = tr.measure_weighted(&label, 9.5, None, TextWeight::Bold);
            let pill = Rect::new(cr.x1 - 8.0 - lw as f64 - 10.0, cr.y0 + 2.0, cr.x1 - 4.0, cr.y0 + 16.0);
            if pill.x0 > cr.x0 + 4.0 {
                let bg = darken(base, 0.45);
                fill_rrect(scene, tf, with_alpha(bg, 0.9), pill, 4.0);
                let baseline = text_baseline(pill, 9.5, tr);
                text_w(
                    scene, tr, tf, &label, 9.5, on_fill(theme, bg).1,
                    pill.x0 + 5.0, baseline, TextWeight::Bold,
                );
            }
        }
        // Keyframes, as diamonds on a lane along the foot of the clip. This is
        // where animation becomes visible without opening the inspector — and
        // clicking one takes the playhead there, which makes stepping through
        // a curve direct rather than a menu round-trip.
        //
        // The foot, not the header: the header carries the clip's name, and
        // marks laid over it turned "Drone_Coastline.mov" into
        // "Drone_Coastli◆e.mov".
        let key_times = clip.key_times();
        if cr.width() > 12.0 && !key_times.is_empty() && cr.height() > 24.0 {
            let mark = on_fill(theme, base).1;
            let y = cr.y1 - 7.0;
            stroke_line(
                scene, tf, with_alpha(darken(base, 0.45), 0.75),
                (cr.x0 + 3.0).max(r.lanes.x0), y, (cr.x1 - 3.0).min(r.lanes.x1), y, 1.0,
            );
            for t in key_times {
                let x = self.time_to_x(t, &r.lanes);
                if x < cr.x0 - 1.0 || x > cr.x1 + 1.0 || x < r.lanes.x0 || x > r.lanes.x1 {
                    continue;
                }
                let s = 3.5;
                let mut d = BezPath::new();
                d.move_to((x, y - s));
                d.line_to((x + s, y));
                d.line_to((x, y + s));
                d.line_to((x - s, y));
                d.close_path();
                scene.fill(Fill::NonZero, tf, with_alpha(mark, 0.95), None, &d);
                scene.stroke(&Stroke::new(1.0), tf, with_alpha(darken(base, 0.5), 0.9), None, &d);
            }
        }
        // Selection. The primary (anchor) clip — the one the inspector edits —
        // keeps the full white outline and trim handles; the rest of a
        // multi-selection gets a lighter outline and no handles, so it is
        // always clear which clip a trim or a grade will land on.
        let primary = self.selection.primary() == Some(clip.id);
        if primary {
            stroke_rrect(scene, tf, c.border_focus, cr, 6.0, 2.0);
            for hx in [cr.x0, cr.x1] {
                let hrect = Rect::new(hx - 3.0, cr.y0 + 2.0, hx + 3.0, cr.y1 - 2.0);
                fill_rrect(scene, tf, c.accent[5], hrect, 2.0);
            }
        } else if selected {
            stroke_rrect(scene, tf, with_alpha(c.border_focus, 0.6), cr, 6.0, 2.0);
        } else {
            stroke_rrect(scene, tf, with_alpha(c.surfaces.base, 0.55), cr, 6.0, 1.0);
        }
    }

    fn draw_export_modal(&self, scene: &mut Scene, tf: Affine, tr: &mut TextRenderer, theme: &Theme, r: &Regions) {
        let c = &theme.tokens.colors;
        // Backdrop. `surfaces.overlay` is the opaque deep-menu surface — using
        // it here painted solid black and deleted the editor behind the dialog.
        // The scrim token carries the theme's hue; the strength is ours.
        fill_rect(scene, tf, with_alpha(c.surfaces.scrim, MODAL_SCRIM_ALPHA), self.rect);
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
                let col = if *on { on_fill(theme, c.accent[5]).1 } else { c.text_dim };
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
        // One source of truth for the size line: the export module knows which
        // encoder will run here and whether a size can honestly be predicted
        // from the rate it is given, which this dialog does not.
        let est = self.export.estimate_label(&self.project);
        let (w, h) = self.export.resolution.dims();
        let info = format!("{est}   ·   {}×{}   ·   {}", w, h, timecode(self.project.duration(), self.project.fps));
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
                // Danger, not warning: the export did not happen.
                text(scene, tr, tf, reason, 10.5, c.semantic.danger, x0, card.y1 - 68.0);
            }
            ExportStatus::Idle => {
                // One source of truth for the availability copy — the export
                // module knows what it can actually do, this dialog does not.
                if let Some(reason) = crate::export::unavailable_reason() {
                    text(scene, tr, tf, reason, 10.5, c.text_dim, x0, card.y1 - 68.0);
                }
            }
        }
        // The flow's own status (choosing a destination, a portal refusal)
        // sits under the render status, so an action is never silent.
        if let Some(notice) = &self.export_notice {
            let shown = elide_w(notice, (card.width() - 48.0) as f32, tr);
            text(scene, tr, tf, &shown, 10.5, c.text_dim, x0, card.y1 - 82.0);
        }
        // A render in flight turns Close into Cancel — the only way to stop it.
        let running = self.export_running();
        draw_button(
            scene, tf, tr, theme, l.cancel,
            if running { "Cancel" } else { "Close" },
            false, l.cancel.contains(self.last_mouse),
        );
        // Export is offered only when it can actually happen. The dialog used
        // to say "Export needs the GStreamer build" and put a bright, enabled
        // Export button directly underneath it.
        let can_export = self.export_is_offered();
        draw_button_enabled(
            scene, tf, tr, theme, l.confirm,
            if running { "Rendering…" } else { "Export" },
            true, l.confirm.contains(self.last_mouse), can_export,
        );
    }
}

// ==========================================================================
// Free drawing utilities
// ==========================================================================
/// A label colour guaranteed legible on `fill`, and the (possibly deepened)
/// fill to paint under it — the toolkit's own contrast rule, so a clip name on
/// an amber body reads in light and dark alike. Replaces the bare
/// `Color::WHITE` this file used on every coloured surface, which was chosen
/// for one palette and was already weak on the pale ones.
fn on_fill(theme: &Theme, fill: Color) -> (Color, Color) {
    theme.tokens.colors.legible_fill(fill, Color::WHITE)
}

/// The `y` to pass to [`text`] so one line of `size` sits vertically centred
/// in `rect`, measured from the font's real line height.
///
/// Replaces the hand-tuned `center().y - 8.0` / `- 6.0` / `- 7.0` constants
/// this file used to carry at every call site, which were each correct for
/// exactly one font size and drifted the moment one changed.
fn text_baseline(rect: Rect, size: f32, tr: &mut TextRenderer) -> f64 {
    let (_, h) = tr.measure("Hg", size, None);
    rect.center().y - h as f64 / 2.0
}

/// Top-left corner for `s` drawn centred both ways inside `rect`.
fn text_centre(rect: Rect, s: &str, size: f32, weight: TextWeight, tr: &mut TextRenderer) -> Point {
    let (w, h) = tr.measure_weighted(s, size, None, weight);
    Point::new(rect.center().x - w as f64 / 2.0, rect.center().y - h as f64 / 2.0)
}

/// Cell width and count for a filmstrip drawn across `inner`. Shared by the
/// request pass and the draw pass so the two never disagree about how many
/// frames a strip needs — a mismatch would request frames at times the strip
/// never asks for, and leave the cells it does ask for undecoded.
fn filmstrip_cells(inner: Rect) -> (f64, i64) {
    let cell_w = (inner.height() * 16.0 / 9.0).clamp(24.0, 56.0);
    let n = (inner.width() / (cell_w + 2.0)).floor().max(1.0) as i64;
    (cell_w, n)
}

/// The content rect inside a clip body, below its header strip. Shared for the
/// same reason as [`filmstrip_cells`].
fn clip_inner_rect(cr: Rect) -> Rect {
    let strip_y1 = (cr.y0 + 18.0).min(cr.y1);
    Rect::new(cr.x0 + 2.0, strip_y1, cr.x1 - 2.0, cr.y1 - 2.0)
}

/// The file's own name, for a message about it — never the whole path, which
/// would run off the notice line.
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

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

/// Blit an RGBA frame into `canvas`, scaled to fit (letterboxed), centred.
/// Carries the preview's composited picture in every build — the compositor
/// runs with or without a decoder, drawing real footage when it has any and
/// representative fills when it does not.
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

/// Blit a thumbnail into `dest`, cover-fitting (fill + crop) with rounded
/// corners `radius`. Used for the bin swatch and timeline filmstrip cells; the
/// same path shows the generated representative frame or (under decode) the real
/// decoded frame.
fn blit_thumb(scene: &mut Scene, tf: Affine, thumb: &crate::thumbnail::Thumb, dest: Rect, radius: f64) {
    use vello::peniko::ImageBrush;
    let iw = thumb.width as f64;
    let ih = thumb.height as f64;
    if iw <= 0.0 || ih <= 0.0 || dest.width() <= 0.0 || dest.height() <= 0.0 {
        return;
    }
    // The thumbnail cache builds the render handle once and hands it out by
    // reference, so painting a frame costs a clone of the handle rather than a
    // copy of the pixels.
    let img = thumb.image().clone();
    // Cover: scale so the frame fills the dest, cropping the overflow.
    let scale = (dest.width() / iw).max(dest.height() / ih);
    let dw = iw * scale;
    let dh = ih * scale;
    let dx = dest.x0 + (dest.width() - dw) * 0.5;
    let dy = dest.y0 + (dest.height() - dh) * 0.5;
    let brush_tf = Affine::translate((dx, dy)) * Affine::scale_non_uniform(scale, scale);
    let brush = ImageBrush::new(img);
    scene.fill(Fill::NonZero, tf, &brush, Some(brush_tf), &RoundedRect::from_rect(dest, radius));
}

fn fill_rect(scene: &mut Scene, tf: Affine, color: Color, rect: Rect) {
    scene.fill(Fill::NonZero, tf, color, None, &rect);
}
/// Diagonal hatching clipped to `rect`, for a surface whose content is a
/// stand-in rather than the real thing. Hatching (not a flat tint) so the
/// state survives a colour-blind reading and a greyscale screenshot.
fn hatch_rect(scene: &mut Scene, tf: Affine, color: Color, rect: Rect, radius: f64) {
    if rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }
    scene.push_layer(
        Fill::NonZero,
        vello::peniko::BlendMode::default(),
        1.0,
        tf,
        &RoundedRect::from_rect(rect, radius),
    );
    let step = 9.0;
    let stroke = Stroke::new(2.0);
    let mut x = rect.x0 - rect.height();
    while x < rect.x1 {
        scene.stroke(
            &stroke,
            tf,
            color,
            None,
            &Line::new(
                Point::new(x, rect.y1),
                Point::new(x + rect.height(), rect.y0),
            ),
        );
        x += step;
    }
    scene.pop_layer();
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

/// A themed button face. Delegates to the toolkit's shared
/// [`ui::widget::Button::paint`], which is provided exactly for canvas apps
/// that own their own hit-testing — so these buttons get the fleet's fill,
/// border, hover, press and disabled treatment instead of a local
/// approximation that had no hover state at all.
fn draw_button(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    primary: bool,
    hovered: bool,
) {
    draw_button_enabled(scene, tf, tr, theme, rect, label, primary, hovered, true);
}

/// [`draw_button`] plus an explicit enabled flag, so a control the app cannot
/// actually perform is painted with the toolkit's own disabled treatment
/// instead of an inviting primary fill it will only refuse.
fn draw_button_enabled(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    primary: bool,
    hovered: bool,
    enabled: bool,
) {
    use ui::widget::{Button, ButtonFace, ButtonStyle};
    let face = ButtonFace::new(rect, label)
        .style(if primary { ButtonStyle::Primary } else { ButtonStyle::Neutral })
        .radius(7.0)
        .text_size(13.0)
        .hovered(hovered && enabled)
        .disabled(!enabled);
    Button::paint(scene, tf, tr, theme, &face);
}

/// A danger-styled button face (the inspector's Delete).
fn draw_danger_button(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    rect: Rect,
    label: &str,
    hovered: bool,
) {
    use ui::widget::{Button, ButtonFace, ButtonStyle};
    let face = ButtonFace::new(rect, label)
        .style(ButtonStyle::Danger)
        .radius(7.0)
        .text_size(13.0)
        .hovered(hovered);
    Button::paint(scene, tf, tr, theme, &face);
}

/// Draw an inspector slider for a 0..1 value, through the toolkit's shared
/// [`ui::widget::Slider::paint`]. The knob was a bare white circle here, which
/// is invisible on a light theme; the shared face carries the fleet's track,
/// fill, knob, hover, press and focus treatment instead.
///
/// `track` is the 9px band the inspector lays out; the face wants a taller box
/// to centre its knob in, so it is grown symmetrically about the same centre —
/// the hit-test in `hit_slider` already allows that much slop.
fn draw_track(
    scene: &mut Scene,
    tf: Affine,
    theme: &Theme,
    track: Rect,
    value: f64,
    hovered: bool,
    dragging: bool,
) {
    let cy = track.center().y;
    let box_rect = Rect::new(track.x0 - 5.0, cy - 11.0, track.x1 + 5.0, cy + 11.0);
    let face = ui::widget::SliderFace::new(box_rect, value)
        .hovered(hovered)
        .dragging(dragging);
    ui::widget::Slider::paint(scene, tf, theme, &face);
}

/// Draw a segmented control over caller-owned rects through the toolkit's
/// shared [`ui::widget::ToggleGroup::paint`], so the transition row and the
/// export dialog's option groups look like every other segmented control in
/// the fleet — including a hover state, which the local painter had none of.
fn draw_segmented(
    scene: &mut Scene,
    tf: Affine,
    tr: &mut TextRenderer,
    theme: &Theme,
    segments: &[Rect],
    labels: &[&str],
    selected: usize,
    cursor: Point,
) {
    let hovered = segments.iter().position(|r| r.contains(cursor));
    let face = ui::widget::SegmentedFace::new(segments, labels, selected)
        .hovered(hovered)
        .radius(6.0)
        .text_size(11.5);
    ui::widget::ToggleGroup::paint(scene, tf, tr, theme, &face);
}

/// Where each part of the transport's readout goes, or `None` for a part
/// that does not fit **whole**.
///
/// The rule this encodes: a number the pane edge cuts mid-digit does not read
/// as a clipped number, it reads as a shorter one — a 42-second project
/// arriving as "00:4" states a length that is simply false. So a part that
/// cannot be drawn in full is not drawn at all, and the parts are dropped in
/// reverse order of what the user cannot get anywhere else: the playhead's
/// own time is the last to go, the total next, the static frame size first.
#[derive(Debug, Default, PartialEq)]
struct ReadoutFit {
    big_x: Option<f64>,
    total_x: Option<f64>,
    meta_x: Option<f64>,
}

fn readout_fit(transport: Rect, big_w: f64, total_w: f64, meta_w: f64) -> ReadoutFit {
    let right_edge = transport.x1 - 12.0;
    let big_x = transport.x0 + 220.0;
    let mut fit = ReadoutFit::default();
    // Nothing at all when even the playhead time would overrun.
    if big_x + big_w > right_edge {
        return fit;
    }
    fit.big_x = Some(big_x);
    let mut used_to = big_x + big_w;
    let total_x = used_to + 4.0;
    if total_x + total_w <= right_edge {
        fit.total_x = Some(total_x);
        used_to = total_x + total_w;
    }
    let meta_x = transport.x1 - 16.0 - meta_w;
    if meta_x > used_to + 16.0 {
        fit.meta_x = Some(meta_x);
    }
    fit
}

/// The key control at the head of an animatable inspector row: a diamond,
/// the standard mark for a keyframe.
///
/// Its three states have to be told apart at a glance, so they differ in
/// **two** ways each rather than only in colour: not animated is a small dim
/// outline, animated-but-between-keys is a larger accent outline, and a key
/// on the playhead is a filled accent diamond with a ring. A control that
/// only changed hue would be unreadable to anyone who cannot separate those
/// hues, and indistinguishable in a screenshot.
fn draw_key_control(scene: &mut Scene, tf: Affine, theme: &Theme, rect: Rect, state: KeyState, hovered: bool) {
    let c = &theme.tokens.colors;
    let ctr = rect.center();
    if hovered {
        fill_rrect(scene, tf, c.surface_white_10, rect, 5.0);
    }
    let (r, col, filled) = match state {
        KeyState::Off => (4.0, c.text_tertiary, false),
        KeyState::Between => (5.5, c.accent[5], false),
        KeyState::OnKey => (5.5, c.accent[5], true),
    };
    let mut d = BezPath::new();
    d.move_to((ctr.x, ctr.y - r));
    d.line_to((ctr.x + r, ctr.y));
    d.line_to((ctr.x, ctr.y + r));
    d.line_to((ctr.x - r, ctr.y));
    d.close_path();
    if filled {
        scene.fill(Fill::NonZero, tf, col, None, &d);
        // The ring says "the playhead is exactly here", which is what makes
        // this the state that edits a key rather than making one.
        scene.stroke(&Stroke::new(1.0), tf, with_alpha(col, 0.5), None, &Circle::new(ctr, r + 3.0));
    } else {
        scene.stroke(&Stroke::new(1.4), tf, col, None, &d);
    }
}

/// A title's authored colour as a Vello colour. Unlike a clip's palette slot
/// this is a real stored value — the user picked it and it must survive a
/// theme change — so it is converted, not resolved.
fn title_color(c: [f64; 4]) -> Color {
    Color::new([c[0] as f32, c[1] as f32, c[2] as f32, c[3] as f32])
}

fn hit_slider(track: Rect, p: Point) -> bool {
    p.x >= track.x0 - 8.0 && p.x <= track.x1 + 8.0 && (p.y - track.center().y).abs() < 14.0
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

/// Draw a real audio waveform from normalized `peaks` into `area`, mapping the
/// source window `[in_p, out_p]` (over `media_dur`) into the peaks array so a
/// trimmed clip shows its own portion. `volume` scales the bar height.
#[allow(clippy::too_many_arguments)]
fn draw_waveform(
    scene: &mut Scene,
    tf: Affine,
    area: Rect,
    peaks: &[f32],
    in_p: f64,
    out_p: f64,
    media_dur: f64,
    volume: f64,
    color: Color,
) {
    if area.width() < 3.0 || area.height() < 2.0 || peaks.is_empty() {
        return;
    }
    let bars = (area.width() / 3.0).floor().max(1.0) as i64;
    let midy = area.center().y;
    let half = (area.height() / 2.0 - 2.0).max(1.0);
    let dur = media_dur.max(1e-6);
    let vscale = 0.25 + volume.clamp(0.0, 1.0) * 0.75;
    for i in 0..bars {
        let fx = (i as f64 + 0.5) / bars as f64;
        let x = area.x0 + 2.0 + fx * (area.width() - 4.0);
        let t = in_p + fx * (out_p - in_p);
        let frac = (t / dur).clamp(0.0, 1.0);
        let idx = ((frac * peaks.len() as f64) as usize).min(peaks.len() - 1);
        let amp = (peaks[idx] as f64 * half * vscale).max(0.5);
        fill_rect(scene, tf, color, Rect::new(x, midy - amp, x + 2.0, midy + amp));
    }
}

/// The placeholder an audio body shows when there is no real signal to draw:
/// a flat line down the middle.
///
/// It is deliberately flat. What stood here before was a sum of three sines
/// dressed up as an envelope — invented data drawn in the same ink as measured
/// data, which is the one thing a waveform must never do. A flat line says
/// "nothing measured yet"; a wiggle says "this is your audio", and would be a
/// lie whether the peaks were still computing or could never be read at all.
fn draw_flat_signal(scene: &mut Scene, tf: Affine, area: Rect, color: Color) {
    if area.width() <= 2.0 || area.height() <= 2.0 {
        return;
    }
    let midy = area.center().y;
    fill_rect(
        scene,
        tf,
        with_alpha(color, 0.55),
        Rect::new(area.x0 + 2.0, midy - 0.75, area.x1 - 2.0, midy + 0.75),
    );
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
        assert!(app.selected().is_some(), "a clip should be auto-selected");
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
        assert!(app.selected().is_none(), "nothing selected in a blank project");
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

    /// A source whose bytes are not on this device must SAY so — the editor
    /// still draws a picture for it (composited from metadata), and without a
    /// disclosure that stand-in is indistinguishable from the real footage.
    #[test]
    fn media_whose_bytes_are_absent_is_disclosed_in_the_bin() {
        // Its own file: `write_real_png` keys its temp name on the width, and
        // this test deletes the file, which would pull it out from under any
        // sibling test that happened to pick the same size.
        let src = write_real_png(640, 360);
        let f = std::env::temp_dir()
            .join(format!("qos-ve-absent-{}.png", std::process::id()));
        std::fs::copy(&src, &f).unwrap();
        let mut app = VideoEditorApp::new_import(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let mid = app.project.media.first().expect("import landed").id;
        assert!(
            !app.media_is_projected(mid),
            "a file that is on disk must not be reported as absent",
        );
        let present = render_pixels(&mut app, 1280, 800, &Theme::dark());

        // Take the bytes away — the CRDT reference survives, the content does
        // not. This is the ordinary state of a project opened on a second peer.
        std::fs::remove_file(&f).unwrap();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.media_is_projected(mid),
            "a reference with no local bytes must be reported as absent",
        );
        let absent = render_pixels(&mut app, 1280, 800, &Theme::dark());

        // …and that state has to reach the pixels, not just a field.
        let r = app.regions();
        let row = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| *m == mid)
            .expect("the source has a bin row")
            .1;
        let changed = (row.y0 as u32..row.y1 as u32)
            .flat_map(|y| (row.x0 as u32..row.x1 as u32).map(move |x| (x, y)))
            .filter(|(x, y)| {
                let i = ((y * 1280 + x) * 4) as usize;
                present.get(i..i + 3) != absent.get(i..i + 3)
            })
            .count();
        assert!(
            changed > 200,
            "the bin row must visibly disclose absent media; only {changed} pixels changed",
        );
    }

    /// A document written before import learned to refuse non-media still
    /// carries the entry, and opening that document is not an import — so the
    /// probe's refusal never runs. The user's own saved project holds an HTML
    /// page saved as `.mp4`: duration 0, 0x0, bytes present. It rendered as
    /// `0p · 0:00`, which reads as two probed facts and is neither.
    #[test]
    fn a_loaded_document_says_unknown_rather_than_inventing_zeroes() {
        // Real bytes on disk that are not media — the actual failure mode.
        let f = std::env::temp_dir()
            .join(format!("qos-ve-notmedia-{}.mp4", std::process::id()));
        std::fs::write(&f, b"<!doctype html><html><body>not a video</body></html>").unwrap();

        // Seed the document directly: this is a LOAD, not an import.
        let mut doc = AutoCommit::new();
        // Built off a real sample item so the shape stays valid as `MediaItem`
        // grows; only the fields this defect is about are overridden.
        let base = crate::doc::sample_project().media.remove(0);
        let poisoned = MediaItem {
            id: Uuid::new_v4(),
            name: "everything_wrong_CMAF_480.mp4".to_string(),
            kind: MediaKind::Video,
            duration: 0.0,
            width: 0,
            height: 0,
            fps: 0.0,
            path: f.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..base
        };
        VideoDoc::write_project_meta(&mut doc, &Project::new("Loaded"));
        VideoDoc::upsert_media(&mut doc, &poisoned);

        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.sync(&mut doc, &automerge::ROOT);
        let m = app
            .project
            .media
            .iter()
            .find(|m| m.id == poisoned.id)
            .expect("the loaded document's entry survives the load")
            .clone();

        // The bytes are here, so this is NOT the "not on this device" state.
        assert!(
            !app.media_is_projected(m.id),
            "the file is on disk; calling it absent would send the user fetching",
        );
        assert!(
            app.media_is_unreadable(m.id),
            "a source with no duration and no dimensions is not usable media",
        );

        let sub = app.bin_subtitle(&m);
        assert!(
            !sub.contains("0p") && !sub.contains("0:00"),
            "unknown metadata must not be printed as a fact; got {sub:?}",
        );
        assert!(
            sub.contains("unknown"),
            "the row must say what it does not know; got {sub:?}",
        );

        // …and the user must be able to get rid of it. The bin's own menu is
        // the way out, driven here exactly as a right-click drives it.
        let r = app.regions();
        let row = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(id, _)| *id == m.id)
            .expect("the entry has a bin row")
            .1;
        app.open_bin_menu(m.id, row.center());
        assert!(app.bin_menu.is_open(), "the row offers a menu");
        let delete = app
            .bin_menu
            .items()
            .iter()
            .position(|i| i.label() == Some("Delete"))
            .expect("the menu offers Delete");
        app.bin_menu_choose(&mut doc, delete);
        assert!(
            !app.project.media.iter().any(|x| x.id == m.id),
            "Delete must remove an unusable source from the document",
        );

        let _ = std::fs::remove_file(&f);
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
    fn portal_import_action_adds_the_clip_through_the_document() {
        // Drive the exact per-frame path a real portal pick feeds — an injected
        // multi-select outcome carrying one real file, installed on `import_rx`
        // exactly as `portal::request_import()` would. This proves the import
        // code path is compiled into the default build (no feature wall) and
        // that draining it routes the clip through the `UpsertMedia` action on
        // `ui::ActionHost`, so it lands in the shared document a peer replicates.
        let f = write_real_png(480, 270);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        app.import_rx = Some(portal::inject_for_test(ImportOutcome::Selected(vec![f.clone()])));

        let mut doc = AutoCommit::new();
        assert!(app.project.media.is_empty(), "blank project starts with no media");
        app.sync(&mut doc, &automerge::ROOT);

        // The in-memory projection gained the clip with real, probed metadata…
        assert_eq!(app.project.media.len(), 1, "the picked file landed in the bin");
        let m = &app.project.media[0];
        assert_eq!(m.name, f.file_name().unwrap().to_string_lossy());
        assert_eq!((m.width, m.height), (480, 270), "real probed dimensions");
        assert!(m.hydration.is_local(), "the imported bytes are local/decodable");

        // …and, crucially, so did the CRDT the action wrote to: the import is a
        // document mutation dispatched through the action model, not a local-only
        // edit, so a second replica loading these bytes sees the same clip.
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.media.len(), 1, "import routed through the doc action");
        assert_eq!(reloaded.media[0].width, 480, "real probed metadata replicated");
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
        app.selection.select_only(target);
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
        app.selection.select_only(target);
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
        app.selection.select_only(id);
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
        app.selection.select_only(id);
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(key_press(&mut app, &mut doc, "c", true), "Ctrl+C handled");
        app.playhead = 30.0;
        assert!(key_press(&mut app, &mut doc, "v", true), "Ctrl+V handled");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.clips.len(), before + 1, "paste added a clip to the CRDT");
        // The pasted clip is the new selection and sits at the playhead.
        let pasted = app.selected().unwrap();
        assert_ne!(pasted, id);
        assert!((reloaded.clip(pasted).unwrap().start - 30.0).abs() < 1e-9);
    }

    #[test]
    fn set_in_out_shortcuts_trim_the_selected_clip() {
        let (mut app, mut doc) = app_and_doc();
        // Interview clip spans roughly [8, 20] on V1.
        let id = app.project.top_video_clip_at(12.0).unwrap().id;
        app.selection.select_only(id);
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
        app.selection.select_only(first);
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
        app.selection.select_only(id);
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
    fn right_press(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
        dispatch(app, doc, WindowEvent::MouseInput {
            device_id: fake_device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Right,
        });
    }
    fn char_ev(s: &str) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = WKey::Character(s.into());
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(s.into());
        WindowEvent::KeyboardInput { device_id: fake_device_id(), event: ev, is_synthetic: false }
    }
    fn named_ev(k: NamedKey) -> WindowEvent {
        let mut ev: winit::event::KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = WKey::Named(k);
        ev.physical_key =
            winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput { device_id: fake_device_id(), event: ev, is_synthetic: false }
    }

    /// Click the open context-menu row whose label is `label`, through the
    /// menu's real geometry. Addressing rows by label rather than by index
    /// means adding an entry cannot silently retarget a test at its neighbour.
    fn click_menu_item(app: &mut VideoEditorApp, doc: &mut AutoCommit, label: &str) {
        let idx = app
            .bin_menu
            .items()
            .iter()
            .position(|i| i.label() == Some(label))
            .unwrap_or_else(|| {
                let have: Vec<&str> = app.bin_menu.items().iter().filter_map(|i| i.label()).collect();
                panic!("no menu row labelled {label:?}; menu has {have:?}")
            });
        let menu = app.bin_menu.get_rect();
        let cx = menu.center().x;
        let y = (0..(menu.height() as i64))
            .map(|d| menu.y0 + d as f64 + 0.5)
            .find(|&y| app.bin_menu.item_at([cx, y]) == Some(idx))
            .unwrap_or_else(|| panic!("row {label:?} has no hit area"));
        move_to(app, doc, cx, y);
        press(app, doc);
    }

    // ---- modifier-aware + wheel drivers ----------------------------------
    // Everything below goes through the same `handle_event` the live window
    // loop calls; nothing reaches into a helper the app itself doesn't use.

    fn dispatch_mods(
        app: &mut VideoEditorApp,
        doc: &mut AutoCommit,
        ev: WindowEvent,
        mods: ModifiersState,
    ) {
        let mut handled = false;
        let mut focus = None;
        let taffy = TaffyTree::<()>::new();
        let node = NodeId::new(0);
        app.handle_event(
            &ev,
            1.0,
            &winit::event::Modifiers::from(mods),
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// Press with Shift held — the multi-select gesture.
    fn shift_press(app: &mut VideoEditorApp, doc: &mut AutoCommit) {
        dispatch_mods(
            app,
            doc,
            WindowEvent::MouseInput {
                device_id: fake_device_id(),
                state: ElementState::Pressed,
                button: MouseButton::Left,
            },
            ModifiersState::SHIFT,
        );
    }

    fn shift_click(app: &mut VideoEditorApp, doc: &mut AutoCommit, x: f64, y: f64) {
        move_to(app, doc, x, y);
        shift_press(app, doc);
        release(app, doc);
    }

    fn wheel(
        app: &mut VideoEditorApp,
        doc: &mut AutoCommit,
        dx: f32,
        dy: f32,
        mods: ModifiersState,
    ) {
        dispatch_mods(
            app,
            doc,
            WindowEvent::MouseWheel {
                device_id: fake_device_id(),
                delta: MouseScrollDelta::LineDelta(dx, dy),
                phase: winit::event::TouchPhase::Moved,
            },
            mods,
        );
    }

    /// Run the app's real `draw` pass. No GPU and no window: the Vello scene is
    /// encoded on the CPU and dropped. Tests call it because `draw` is where
    /// per-frame view reconciliation happens (playhead following, scroll
    /// clamping), so skipping it would test a frame the user never sees.
    fn render(app: &mut VideoEditorApp) {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    /// The lane rect of the nth track of `kind`, plus its id.
    fn lane_of(app: &VideoEditorApp, r: &Regions, kind: TrackKind, n: usize) -> (TrackId, Rect) {
        let (tid, _k, lane) = app
            .lane_rects(r)
            .into_iter()
            .filter(|(_, k, _)| *k == kind)
            .nth(n)
            .expect("a lane of the requested kind");
        (tid, lane)
    }

    /// The on-screen rect of `cid`, resolved through the same geometry the
    /// renderer and the hit-test use.
    fn clip_screen_rect(app: &VideoEditorApp, r: &Regions, cid: ClipId) -> Rect {
        let clip = app.project.clip(cid).cloned().expect("clip exists");
        let (_, _, lane) = app
            .lane_rects(r)
            .into_iter()
            .find(|(tid, _, _)| *tid == clip.track)
            .expect("the clip's lane");
        app.clip_rect(&clip, &lane, r)
    }

    // ---- media bin: drag stays reachable, and drops land where dropped ----

    #[test]
    fn a_drag_from_the_name_band_places_a_clip_instead_of_renaming() {
        // Regression: `clicked_before` was unbounded, so once an item had been
        // clicked, every later press on the name band — which covers most of
        // the row, and is exactly where you grab it — opened a rename and the
        // bin→timeline drag became unreachable.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).map(|mm| mm.kind.is_visual()).unwrap_or(false))
            .expect("a visual bin item");
        let name = app.bin_name_rect(row);
        // First click on the row, as a user browsing the bin would.
        click(&mut app, &mut doc, name.center().x, name.center().y);
        release(&mut app, &mut doc);
        // Wait past the double-click window, into the slow-second-click window
        // where the old code renamed unconditionally.
        std::thread::sleep(std::time::Duration::from_millis(
            BIN_DOUBLE_CLICK_MS as u64 + 60,
        ));
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let before = app.project.clips_on(tid).len();
        // Press on the name band and drag to a lane.
        move_to(&mut app, &mut doc, name.center().x, name.center().y);
        press(&mut app, &mut doc);
        assert!(app.bin_drag.is_some(), "the press armed a drag, not just a rename");
        move_to(&mut app, &mut doc, lane.x0 + 220.0, lane.center().y);
        assert!(
            matches!(app.bin_drag, Some(BinDrag { active: true, .. })),
            "moving past the threshold promoted the drag",
        );
        assert!(app.pending_rename.is_none(), "becoming a drag cancelled the rename intent");
        release(&mut app, &mut doc);
        assert!(app.bin_rename.is_none(), "no rename field opened");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 1,
            "the drag placed a clip on the drop lane in the CRDT",
        );
        assert_eq!(
            app.selected().and_then(|id| reloaded.clip(id)).map(|c| c.media),
            Some(mid),
            "the placed clip references the dragged media",
        );
    }

    #[test]
    fn a_slow_second_click_on_the_name_still_renames() {
        // The other half of the same rule: bounding the rename window must not
        // cost the Finder-style click-pause-click rename.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app.bin_item_rects(&r)[0];
        let name = app.bin_name_rect(row);
        click(&mut app, &mut doc, name.center().x, name.center().y);
        release(&mut app, &mut doc);
        std::thread::sleep(std::time::Duration::from_millis(
            BIN_DOUBLE_CLICK_MS as u64 + 60,
        ));
        // A second press that never moves.
        move_to(&mut app, &mut doc, name.center().x, name.center().y);
        press(&mut app, &mut doc);
        assert_eq!(app.pending_rename, Some(mid), "the press armed a rename intent");
        release(&mut app, &mut doc);
        assert!(
            matches!(app.bin_rename.as_ref().map(|(m, _)| *m), Some(m) if m == mid),
            "releasing without moving began the inline rename",
        );
    }

    #[test]
    fn dropping_in_the_track_header_gutter_places_rather_than_appends() {
        // Regression: `bin_drop_lane` required x >= lanes.x0, so a straight-down
        // drag from the bin landed in the 128px gutter, matched no lane, and
        // fell through to the append path at the end of the timeline.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).map(|mm| mm.kind.is_visual()).unwrap_or(false))
            .expect("a visual bin item");
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let track_end = app.project.clips_on(tid).last().map(|c| c.end()).unwrap_or(0.0);
        assert!(track_end > 1.0, "the fixture track has content to append past");
        let before = app.project.clips_on(tid).len();
        // Straight down from the bin row into the gutter, level with the lane.
        let gutter_x = r.gutter.x0 + r.gutter.width() / 2.0;
        assert!(gutter_x < r.lanes.x0, "the drop really is left of the lane area");
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, gutter_x, lane.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.clips_on(tid).len(), before + 1, "a clip was placed");
        let placed = app.selected().and_then(|id| reloaded.clip(id)).expect("the placed clip");
        assert_eq!(placed.media, mid);
        assert!(
            placed.start < 0.5,
            "a gutter drop places at the start of the visible timeline, not appended at {track_end} (got {})",
            placed.start,
        );
    }

    // ---- horizontal timeline scroll --------------------------------------

    #[test]
    fn shift_wheel_pans_the_timeline_and_clamps_to_the_content() {
        let (mut app, mut doc) = app_and_doc();
        move_to(&mut app, &mut doc, 640.0, 700.0); // over the lanes
        assert_eq!(app.scroll, 0.0, "the timeline opens at the start");
        wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::SHIFT);
        assert!(app.scroll > 0.0, "shift+wheel scrolled right, got {}", app.scroll);
        let r = app.regions();
        let max = app.max_scroll(&r);
        // Far past the end: it stops at the content bound rather than running off.
        for _ in 0..200 {
            wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::SHIFT);
        }
        assert!(
            (app.scroll - max).abs() < 1e-6,
            "scrolling right clamps to max_scroll ({} vs {max})",
            app.scroll,
        );
        // And back: it stops at zero.
        for _ in 0..300 {
            wheel(&mut app, &mut doc, 0.0, 3.0, ModifiersState::SHIFT);
        }
        assert_eq!(app.scroll, 0.0, "scrolling left clamps to the start");
    }

    #[test]
    fn a_horizontal_wheel_pans_without_zooming() {
        let (mut app, mut doc) = app_and_doc();
        move_to(&mut app, &mut doc, 640.0, 700.0);
        let zoom = app.zoom;
        wheel(&mut app, &mut doc, -3.0, 0.0, ModifiersState::empty());
        assert!(app.scroll > 0.0, "a horizontal wheel panned the timeline");
        assert_eq!(app.zoom, zoom, "and left the zoom alone");
    }

    #[test]
    fn zoomed_in_content_past_the_right_edge_becomes_reachable() {
        // The bug this closes: with no way to write `scroll`, zooming in put
        // everything past the viewport permanently out of reach.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let last_end = app.project.clips.iter().map(|c| c.end()).fold(0.0_f64, f64::max);
        move_to(&mut app, &mut doc, r.lanes.x0 + 10.0, r.lanes.center().y);
        // Zoom right in, so the tail is far off-screen.
        for _ in 0..12 {
            wheel(&mut app, &mut doc, 0.0, 3.0, ModifiersState::empty());
        }
        assert!(
            app.time_to_x(last_end, &r.lanes) > r.lanes.x1,
            "the timeline tail really is off the right edge after zooming",
        );
        for _ in 0..400 {
            wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::SHIFT);
        }
        assert!(
            app.time_to_x(last_end, &r.lanes) <= r.lanes.x1 + 1.0,
            "panning brought the end of the timeline into view",
        );
    }

    #[test]
    fn dragging_the_scrollbar_thumb_scrolls_the_timeline() {
        let (mut app, mut doc) = app_and_doc();
        // Zoom in so the content overflows and a bar appears.
        app.zoom = 160.0;
        let r = app.regions();
        let track = app.hscroll_track(&r).expect("an overflowing timeline shows a bar");
        let thumb = app
            .hbar
            .thumb_rect(track, app.h_metrics(&r))
            .expect("a thumb inside the track");
        assert!(thumb.width() < track.width(), "the thumb is shorter than its track");
        // Grab the thumb and drag it right.
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::Scrollbar(ScrollPane::Timeline))),
            "pressing the thumb began a bar drag",
        );
        move_to(&mut app, &mut doc, thumb.center().x + 120.0, thumb.center().y);
        assert!(app.scroll > 0.0, "dragging the thumb scrolled the timeline");
        let dragged = app.scroll;
        release(&mut app, &mut doc);
        assert!(app.drag.is_none(), "release ended the bar drag");
        assert_eq!(app.scroll, dragged, "and left the scroll where the drag put it");
    }

    #[test]
    fn playback_keeps_the_playhead_on_screen() {
        let (mut app, mut doc) = app_and_doc();
        app.zoom = 160.0; // a few seconds visible at a time
        let r = app.regions();
        let visible = app.visible_seconds(&r);
        // Park the playhead well past the right edge, the way playback would.
        app.playhead = (visible * 3.0).min(app.project.duration());
        assert!(
            app.time_to_x(app.playhead, &r.lanes) > r.lanes.x1,
            "the playhead starts off the right edge",
        );
        // Press play through the real transport button, then render a frame.
        let (_a, play) = app.transport_buttons(&r)[2];
        click(&mut app, &mut doc, play.center().x, play.center().y);
        assert!(app.playing, "the transport button started playback");
        render(&mut app);
        let x = app.time_to_x(app.playhead, &r.lanes);
        assert!(
            x >= r.lanes.x0 && x <= r.lanes.x1,
            "the view followed the playhead into view (x={x}, lanes {}..{})",
            r.lanes.x0,
            r.lanes.x1,
        );
    }

    // ---- cross-track clip move -------------------------------------------

    #[test]
    fn dragging_a_clip_onto_another_video_lane_moves_it_there_in_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // Two video lanes exist in the sample project.
        let (t0, lane0) = lane_of(&app, &r, TrackKind::Video, 0);
        let (t1, _lane1) = lane_of(&app, &r, TrackKind::Video, 1);
        assert_ne!(t0, t1, "the fixture has two video tracks");
        // Grab a clip on the lower video lane and drag it up to the other one.
        let cid = app.project.clips_on(t1).first().map(|c| c.id).expect("a clip on V-lower");
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::ClipMove { track, .. }) if track == t1),
            "the move started on the clip's own track",
        );
        move_to(&mut app, &mut doc, cr.center().x + 40.0, lane0.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clip(cid).map(|c| c.track),
            Some(t0),
            "the clip changed track in the CRDT, not just on screen",
        );
    }

    #[test]
    fn a_video_clip_dragged_over_an_audio_lane_keeps_its_own_track() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (vid, _vlane) = lane_of(&app, &r, TrackKind::Video, 0);
        let (_aid, alane) = lane_of(&app, &r, TrackKind::Audio, 0);
        let cid = app.project.clips_on(vid).first().map(|c| c.id).expect("a video clip");
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, cr.center().x + 30.0, alane.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clip(cid).map(|c| c.track),
            Some(vid),
            "a video clip refuses an audio lane and stays put",
        );
    }

    // ---- track lock -------------------------------------------------------

    #[test]
    fn the_header_lock_pill_locks_a_track_and_blocks_its_edits() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
        let pill = app.lock_pill_rect(header);
        assert!(header.contains(pill.center()), "the lock pill sits inside the track header");
        assert!(
            pill.x1 <= app.mute_pill_rect(header).x0,
            "the lock pill sits clear of the mute pill",
        );
        // Click the pill through the real press path.
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        assert!(
            VideoDoc::load_project(&doc).track(tid).map(|t| t.locked).unwrap_or(false),
            "the pill wrote the lock to the CRDT",
        );
        // A press on a clip of that track selects it but arms no edit drag.
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip on the track");
        let start_before = app.project.clip(cid).unwrap().start;
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        press(&mut app, &mut doc);
        assert_eq!(app.selected(), Some(cid), "a locked clip can still be selected");
        assert!(app.drag.is_none(), "but no move/trim drag begins on a locked track");
        move_to(&mut app, &mut doc, cr.center().x + 90.0, cr.center().y);
        release(&mut app, &mut doc);
        assert_eq!(
            VideoDoc::load_project(&doc).clip(cid).map(|c| c.start),
            Some(start_before),
            "dragging a locked clip does not move it",
        );
        // Nor do the keyboard edits.
        dispatch(&mut app, &mut doc, named_ev(NamedKey::Delete));
        assert!(
            VideoDoc::load_project(&doc).clip(cid).is_some(),
            "Delete does not remove a clip on a locked track",
        );
        dispatch(&mut app, &mut doc, char_ev("s"));
        let after_split = VideoDoc::load_project(&doc);
        assert_eq!(
            after_split.clips_on(tid).len(),
            app.project.clips_on(tid).len(),
            "S does not split a clip on a locked track",
        );
        assert_eq!(
            after_split.clip(cid).map(|c| c.start),
            Some(start_before),
            "and an arrow nudge leaves it alone too",
        );
    }

    #[test]
    fn a_locked_lane_refuses_a_bin_drop_and_the_clip_lands_elsewhere() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
        let pill = app.lock_pill_rect(header);
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        let before = app.project.clips_on(tid).len();
        let (_mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).map(|mm| mm.kind.is_visual()).unwrap_or(false))
            .expect("a visual bin item");
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        press(&mut app, &mut doc);
        move_to(&mut app, &mut doc, lane.x0 + 200.0, lane.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before,
            "nothing was added to the locked track",
        );
        let placed = app.selected().and_then(|id| reloaded.clip(id)).expect("it landed somewhere");
        assert_ne!(placed.track, tid, "and it went to a track that accepts it");
    }

    // ---- multi-selection --------------------------------------------------

    #[test]
    fn shift_click_extends_the_selection_and_delete_removes_all_of_it_at_once() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> =
            app.project.clips_on(tid).iter().take(2).map(|c| c.id).collect();
        assert_eq!(ids.len(), 2, "the fixture track has two clips to select");
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        assert_eq!(app.selection.len(), 1, "a plain click selects exactly one");
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        assert_eq!(app.selection.len(), 2, "shift-click extended the selection");
        assert!(app.selection.contains(ids[0]) && app.selection.contains(ids[1]));
        assert_eq!(app.selected(), Some(ids[1]), "the shift-clicked clip is the new anchor");
        // Delete both, and prove it was one undo boundary.
        let _ = app.actions.take_committed();
        dispatch(&mut app, &mut doc, named_ev(NamedKey::Delete));
        let reloaded = VideoDoc::load_project(&doc);
        assert!(reloaded.clip(ids[0]).is_none() && reloaded.clip(ids[1]).is_none(),
            "both selected clips were deleted from the CRDT");
        assert!(
            app.actions.take_committed().is_some(),
            "the multi-clip delete armed an undo boundary",
        );
        assert!(
            app.actions.take_committed().is_none(),
            "exactly one boundary for the whole selection, not one per clip",
        );
    }

    #[test]
    fn shift_click_a_selected_clip_removes_it_from_the_selection() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app.project.clips_on(tid).iter().take(2).map(|c| c.id).collect();
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        assert_eq!(app.selection.len(), 1, "the second shift-click deselected it");
        assert_eq!(app.selected(), Some(ids[0]), "the anchor fell back to what is left");
    }

    #[test]
    fn a_marquee_across_empty_lane_space_selects_the_clips_it_sweeps() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let clips: Vec<ClipId> = app.project.clips_on(tid).iter().map(|c| c.id).collect();
        assert!(clips.len() >= 2, "the fixture track has clips to sweep");
        // Start past the end of the last clip (empty space) and sweep back left
        // across the whole lane.
        let last = clip_screen_rect(&app, &r, *clips.last().unwrap());
        let start_x = (last.x1 + 30.0).min(r.lanes.x1 - 4.0);
        move_to(&mut app, &mut doc, start_x, lane.center().y);
        press(&mut app, &mut doc);
        assert!(matches!(app.drag, Some(Drag::Marquee { .. })), "a press on empty lane space armed a band");
        move_to(&mut app, &mut doc, r.lanes.x0 + 2.0, lane.center().y);
        assert!(app.marquee.is_some(), "moving past the threshold opened the band");
        release(&mut app, &mut doc);
        for id in &clips {
            assert!(app.selection.contains(*id), "the band selected every clip it crossed");
        }
    }

    #[test]
    fn a_click_on_empty_lane_space_still_scrubs_the_playhead() {
        // The marquee must not cost the click-to-scrub the lanes always had.
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (_tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let x = r.lanes.x1 - 20.0;
        let expected = app.x_to_time(x, &r.lanes).min(app.project.duration());
        move_to(&mut app, &mut doc, x, lane.center().y);
        press(&mut app, &mut doc);
        release(&mut app, &mut doc);
        assert!(app.selection.is_empty(), "clicking empty space cleared the selection");
        assert!(
            (app.playhead - expected).abs() < 0.2,
            "and scrubbed the playhead there ({} vs {expected})",
            app.playhead,
        );
    }

    #[test]
    fn dragging_a_multi_selection_carries_every_clip_by_the_same_delta() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app.project.clips_on(tid).iter().take(2).map(|c| c.id).collect();
        let before: Vec<f64> = ids.iter().map(|id| app.project.clip(*id).unwrap().start).collect();
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        // Grab the first one (already selected) and drag right.
        let a = clip_screen_rect(&app, &r, ids[0]);
        move_to(&mut app, &mut doc, a.center().x, a.center().y);
        press(&mut app, &mut doc);
        assert_eq!(app.selection.len(), 2, "pressing inside a multi-selection keeps it");
        move_to(&mut app, &mut doc, a.center().x + 60.0, a.center().y);
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        let after: Vec<f64> = ids.iter().map(|id| reloaded.clip(*id).unwrap().start).collect();
        let d0 = after[0] - before[0];
        let d1 = after[1] - before[1];
        assert!(d0 > 0.1, "the grabbed clip moved right ({d0})");
        assert!(
            (d0 - d1).abs() < 1e-6,
            "the rest of the selection moved by the same delta ({d0} vs {d1})",
        );
    }

    // ---- cut / duplicate / clip context menu ------------------------------

    #[test]
    fn ctrl_x_cuts_the_selection_and_ctrl_v_pastes_it_back() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip");
        let media = app.project.clip(cid).unwrap().media;
        let cr = clip_screen_rect(&app, &r, cid);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        release(&mut app, &mut doc);
        let before = VideoDoc::load_project(&doc).clips.len();
        assert!(key_press(&mut app, &mut doc, "x", true), "Ctrl+X is handled");
        let cut = VideoDoc::load_project(&doc);
        assert!(cut.clip(cid).is_none(), "Ctrl+X removed the clip from the CRDT");
        assert_eq!(cut.clips.len(), before - 1);
        // And it is on the clipboard, so Ctrl+V brings it back.
        app.playhead = 1.0;
        assert!(key_press(&mut app, &mut doc, "v", true), "Ctrl+V is handled");
        let pasted = VideoDoc::load_project(&doc);
        assert_eq!(pasted.clips.len(), before, "the cut clip pasted back");
        let new_id = app.selected().expect("the pasted clip is selected");
        assert_eq!(pasted.clip(new_id).map(|c| c.media), Some(media));
    }

    #[test]
    fn ctrl_d_duplicates_the_whole_selection_on_the_same_tracks() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let ids: Vec<ClipId> = app.project.clips_on(tid).iter().take(2).map(|c| c.id).collect();
        let a = clip_screen_rect(&app, &r, ids[0]);
        click(&mut app, &mut doc, a.center().x, a.center().y);
        release(&mut app, &mut doc);
        let b = clip_screen_rect(&app, &r, ids[1]);
        shift_click(&mut app, &mut doc, b.center().x, b.center().y);
        let before = VideoDoc::load_project(&doc).clips_on(tid).len();
        let _ = app.actions.take_committed();
        assert!(key_press(&mut app, &mut doc, "d", true), "Ctrl+D is handled");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 2,
            "both selected clips were duplicated onto their own track",
        );
        assert_eq!(app.selection.len(), 2, "the copies become the selection");
        for id in app.selection.ids() {
            assert!(!ids.contains(id), "the selection moved to the new clips");
        }
        assert!(app.actions.take_committed().is_some(), "one undo boundary for the duplicate");
        assert!(app.actions.take_committed().is_none(), "and only one");
    }

    #[test]
    fn right_clicking_a_clip_opens_a_menu_whose_duplicate_adds_a_clip() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip");
        let cr = clip_screen_rect(&app, &r, cid);
        move_to(&mut app, &mut doc, cr.center().x, cr.center().y);
        right_press(&mut app, &mut doc);
        assert!(app.bin_menu.is_open(), "right-clicking a clip opens a context menu");
        assert!(matches!(app.menu_target, Some(MenuTarget::Clip(c)) if c == cid));
        assert_eq!(app.selected(), Some(cid), "and re-targets the selection at it");
        let before = VideoDoc::load_project(&doc).clips_on(tid).len();
        click_menu_item(&mut app, &mut doc, "Duplicate");
        assert_eq!(
            VideoDoc::load_project(&doc).clips_on(tid).len(),
            before + 1,
            "Duplicate added a clip through the CRDT",
        );
        assert!(!app.bin_menu.is_open(), "the menu closed after the choice");
    }

    #[test]
    fn the_track_menu_lock_entry_locks_the_track() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
        move_to(&mut app, &mut doc, header.x0 + 20.0, header.center().y);
        right_press(&mut app, &mut doc);
        assert!(matches!(app.menu_target, Some(MenuTarget::Track(t)) if t == tid));
        click_menu_item(&mut app, &mut doc, "Lock track");
        assert!(
            VideoDoc::load_project(&doc).track(tid).map(|t| t.locked).unwrap_or(false),
            "the menu entry locked the track in the CRDT",
        );
    }

    // ---- affordances ------------------------------------------------------

    #[test]
    fn the_zoom_chip_reports_the_zoom_and_fits_the_project_to_the_window() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        app.zoom = 180.0;
        let dur = app.project.duration();
        assert!(
            app.time_to_x(dur, &r.lanes) > r.lanes.x1,
            "zoomed right in, the end is off screen",
        );
        let chip = app.zoom_chip_rect(&r);
        click(&mut app, &mut doc, chip.center().x, chip.center().y);
        release(&mut app, &mut doc);
        assert!(
            app.time_to_x(dur, &r.lanes) <= r.lanes.x1 + 1.0,
            "clicking the chip fitted the whole project into the lane area",
        );
        assert_eq!(app.scroll, 0.0, "and parked the view at the start");
        // The keyboard route agrees.
        app.zoom = 180.0;
        assert!(key_press(&mut app, &mut doc, "z", false), "Z is handled");
        assert!(app.time_to_x(dur, &r.lanes) <= r.lanes.x1 + 1.0);
    }

    #[test]
    fn the_pointer_says_what_each_region_does() {
        use winit::window::CursorIcon;
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // A bin row can be dragged out.
        let (_mid, row) = app.bin_item_rects(&r)[0];
        assert_eq!(app.cursor_shape(row.center()), Some(CursorIcon::Grab));
        // A clip body grabs; its edges resize.
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip");
        let cr = clip_screen_rect(&app, &r, cid);
        assert_eq!(app.cursor_shape(cr.center()), Some(CursorIcon::Grab));
        assert_eq!(app.cursor_shape(Point::new(cr.x0, cr.center().y)), Some(CursorIcon::EwResize));
        // Empty lane space is where a marquee starts.
        assert_eq!(
            app.cursor_shape(Point::new(r.lanes.x1 - 4.0, cr.center().y)),
            Some(CursorIcon::Crosshair),
        );
        // The ruler scrubs.
        assert_eq!(app.cursor_shape(r.ruler.center()), Some(CursorIcon::ColResize));
        // Buttons are buttons.
        assert_eq!(app.cursor_shape(r.export_btn.center()), Some(CursorIcon::Pointer));
        // A locked track refuses, and says so.
        let lane = app.lane_rects(&r).into_iter().find(|(t, _, _)| *t == tid).unwrap().2;
        let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
        let pill = app.lock_pill_rect(header);
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        assert_eq!(app.cursor_shape(cr.center()), Some(CursorIcon::NotAllowed));
    }

    // ---- responsive geometry ---------------------------------------------

    /// The app laid out at an arbitrary window size, synced to the sample.
    fn app_sized(w: f64, h: f64) -> (VideoEditorApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }

    /// Window sizes worth proving, from the narrow capture width up.
    const SIZES: [(f64, f64); 8] = [
        (420.0, 560.0),
        (500.0, 640.0),
        (620.0, 700.0),
        (720.0, 820.0),
        (900.0, 700.0),
        (1280.0, 800.0),
        (1920.0, 1080.0),
        (3440.0, 1440.0),
    ];

    #[test]
    fn no_region_is_inverted_at_any_window_size() {
        for (w, h) in SIZES {
            let (app, _doc) = app_sized(w, h);
            let r = app.regions();
            for (name, rect) in [
                ("topbar", r.topbar),
                ("preview", r.preview),
                ("canvas", r.canvas),
                ("transport", r.transport),
                ("inspector", r.inspector),
                ("timeline", r.timeline),
                ("ruler", r.ruler),
                ("gutter", r.gutter),
                ("lanes", r.lanes),
            ] {
                assert!(
                    rect.x1 >= rect.x0 && rect.y1 >= rect.y0,
                    "{name} is inverted at {w}x{h}: {rect:?}",
                );
            }
            let scrub = app.scrubber_rect(&r);
            assert!(
                scrub.x1 >= scrub.x0 && scrub.y1 >= scrub.y0,
                "the scrubber is inverted at {w}x{h}: {scrub:?}",
            );
        }
    }

    #[test]
    fn the_scrubber_knob_stays_inside_its_track_at_every_size() {
        // Regression: below roughly 500px the track went inverted and the knob
        // drew outside it; above about 2000px it stretched edge to edge.
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            let r = app.regions();
            let scrub = app.scrubber_rect(&r);
            if !app.has_scrubber(&r) {
                assert_eq!(scrub, Rect::ZERO, "a transport with no room draws no scrubber");
                continue;
            }
            assert!(
                scrub.width() <= MAX_SCRUBBER_W + 1.0,
                "the scrubber is capped at {w}x{h} (got {})",
                scrub.width(),
            );
            assert!(scrub.x1 <= r.transport.x1, "and stays inside the transport");
            for frac in [0.0, 0.5, 1.0] {
                app.playhead = frac * app.project.duration();
                let knob_x = scrub.x0 + scrub.width() * frac;
                assert!(
                    knob_x >= scrub.x0 - 0.001 && knob_x <= scrub.x1 + 0.001,
                    "the knob is inside the track at {w}x{h}, frac {frac}",
                );
            }
        }
    }

    /// The card's header and its first section were both positioned from
    /// fixed offsets off the card top, so the filename subtitle and the
    /// RESOLUTION label were drawn through each other.
    #[test]
    fn the_export_dialog_header_never_collides_with_the_first_section() {
        let mut tr = TextRenderer::new();
        let subtitle_h = tr.measure("Hg", 12.0, None).1 as f64;
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            app.show_export = true;
            let r = app.regions();
            let l = app.export_layout(&r);
            let subtitle_bottom = l.card.y0 + 44.0 + subtitle_h;
            let first_label_top = l
                .resolution
                .first()
                .expect("the dialog always offers a resolution")
                .1
                .y0
                - 18.0;
            assert!(
                first_label_top >= subtitle_bottom,
                "{w}x{h}: RESOLUTION starts at {first_label_top} but the filename \
                 runs to {subtitle_bottom} — they overlap",
            );
            // …and the groups still stop before the footer they share the card with.
            let last = l.fps.last().expect("frame rates are offered").1;
            assert!(
                last.y1 <= l.card.y1 - 96.0,
                "{w}x{h}: the frame-rate row runs into the size estimate",
            );
        }
    }

    /// The dialog must not offer an action it will refuse. It used to say
    /// "Export needs the GStreamer build" and paint an enabled primary Export
    /// button directly beneath that sentence.
    #[test]
    fn the_export_button_is_only_offered_when_export_can_actually_run() {
        let (mut app, mut doc) = app_sized(1280.0, 800.0);
        app.show_export = true;
        let r = app.regions();
        let l = app.export_layout(&r);
        assert_eq!(
            app.export_is_offered(),
            crate::export::unavailable_reason().is_none(),
            "the offer must track what the export module says it can do",
        );
        if !app.export_is_offered() {
            // Pressing it must do nothing at all — no job, no status change.
            app.press_export(&mut doc, &r, l.confirm.center());
            assert!(
                matches!(app.export_status, ExportStatus::Idle),
                "a refused Export must not move the status",
            );
            assert!(!app.export_running(), "a refused Export must not start a job");
        }
    }

    /// Same defect as the media bin's scrim, one token over: the modal painted
    /// `surfaces.overlay` — the opaque deep-menu surface — as its backdrop, so
    /// the editor vanished behind the dialog instead of dimming.
    #[test]
    fn the_export_modal_dims_the_editor_rather_than_deleting_it() {
        for theme in [Theme::dark(), Theme::light()] {
            let (mut app, _doc) = app_sized(1280.0, 800.0);
            app.show_export = true;
            let r = app.regions();
            let px = render_pixels(&mut app, 1280, 800, &theme);
            // The timeline lanes stretch well past the card on both sides.
            let card = app.export_layout(&r).card;
            let left = Rect::new(r.lanes.x0, r.lanes.y0, card.x0 - 8.0, r.lanes.y1);
            let colours = region_colours(&px, 1280, 800, left);
            assert!(
                colours > 2,
                "the timeline behind the export dialog is a flat slab \
                 ({colours} distinct colours) — the backdrop erased it",
            );
        }
    }

    #[test]
    fn the_export_dialog_stays_inside_the_window() {
        // Regression: a fixed 460x360 card hung off both edges of a small
        // window, putting Cancel and Export outside the surface.
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            app.show_export = true;
            let r = app.regions();
            let l = app.export_layout(&r);
            assert!(
                l.card.x0 >= app.rect.x0 && l.card.x1 <= app.rect.x1,
                "the export card fits horizontally at {w}x{h}: {:?}",
                l.card,
            );
            assert!(
                l.card.y0 >= app.rect.y0 && l.card.y1 <= app.rect.y1,
                "the export card fits vertically at {w}x{h}: {:?}",
                l.card,
            );
            for (name, btn) in [("cancel", l.cancel), ("confirm", l.confirm)] {
                assert!(
                    btn.x0 >= app.rect.x0 && btn.x1 <= app.rect.x1
                        && btn.y0 >= app.rect.y0 && btn.y1 <= app.rect.y1,
                    "the {name} button is on screen at {w}x{h}: {btn:?}",
                );
            }
        }
    }

    #[test]
    fn the_topbar_buttons_never_collide_with_other_topbar_content() {
        // The centred project-meta string used to run under these buttons at
        // 720px — the narrow capture width itself. It now lives in the
        // transport, so the topbar holds only the hamburger and the buttons.
        for (w, h) in SIZES {
            let (app, _doc) = app_sized(w, h);
            let r = app.regions();
            let btns = [r.import_btn, r.split_btn, r.export_btn];
            for (i, a) in btns.iter().enumerate() {
                for b in btns.iter().skip(i + 1) {
                    assert!(
                        a.x1 <= b.x0 || b.x1 <= a.x0,
                        "topbar buttons overlap at {w}x{h}: {a:?} vs {b:?}",
                    );
                }
                if r.menu_btn.width() > 0.0 {
                    assert!(
                        r.menu_btn.x1 <= a.x0,
                        "the hamburger clears the buttons at {w}x{h}",
                    );
                }
            }
        }
    }

    #[test]
    fn every_window_size_renders_without_panicking() {
        // The draw pass is where clip layers must balance and where a crushed
        // canvas would otherwise put a HUD outside its pane. This proves only
        // the absence of a panic — which is NOT proof the frame is usable; see
        // `the_open_overlay_bin_leaves_the_editor_behind_it_visible` for the
        // test that looks at the pixels.
        for (w, h) in SIZES {
            let (mut app, _doc) = app_sized(w, h);
            render(&mut app);
            app.show_export = true;
            render(&mut app);
        }
    }

    /// Rasterise one frame of `app` at `w`×`h` in `theme` and hand back RGBA.
    /// The real GPU path the app ships on — a machine that cannot rasterise
    /// cannot observe this property, so `new` panicking there is the correct
    /// outcome rather than a silent skip.
    fn render_pixels(app: &mut VideoEditorApp, w: u32, h: u32, theme: &Theme) -> Vec<u8> {
        let mut scene = Scene::new();
        let mut tr = TextRenderer::new();
        app.rect = Rect::new(0.0, 0.0, w as f64, h as f64);
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &PresenceCtx::empty());
        ui::offscreen::OffscreenRenderer::new(w, h).render_rgba(&scene)
    }

    /// How many visually distinct colours a rect of a rendered frame contains,
    /// quantised to 4 bits per channel so antialiasing noise does not count as
    /// variety. A pane covered by an opaque slab returns 1.
    fn region_colours(rgba: &[u8], fw: u32, fh: u32, rect: Rect) -> usize {
        let x0 = rect.x0.max(0.0) as u32;
        let y0 = rect.y0.max(0.0) as u32;
        let x1 = (rect.x1 as u32).min(fw);
        let y1 = (rect.y1 as u32).min(fh);
        let mut seen = HashSet::new();
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let i = ((y * fw + x) * 4) as usize;
                if i + 2 < rgba.len() {
                    seen.insert((rgba[i] >> 4, rgba[i + 1] >> 4, rgba[i + 2] >> 4));
                }
                x += 2;
            }
            y += 2;
        }
        seen.len()
    }

    // ---- narrow-window panes ---------------------------------------------

    /// A number cut in half by the pane edge is not a clipped number, it is a
    /// wrong one.
    ///
    /// Regression: the transport drew its total duration at a fixed offset
    /// with no width check, so at 900x700 "00:42:00" landed as `00:4` — a
    /// plausible-looking project length that was four seconds instead of
    /// forty-two. Every part now goes through [`readout_fit`], which drops
    /// what will not fit whole.
    #[test]
    fn the_transport_readout_drops_parts_it_cannot_draw_whole() {
        // Widths in the neighbourhood of the real measurements: an 18px bold
        // "00:23:00" is ~74px, the "/  00:42:00" tail ~62px, the meta ~96px.
        let (big, total, meta) = (74.0, 62.0, 96.0);
        // Roomy: all three, in order, none overlapping.
        let wide = readout_fit(Rect::new(0.0, 0.0, 700.0, 58.0), big, total, meta);
        assert_eq!(wide.big_x, Some(220.0));
        assert_eq!(wide.total_x, Some(298.0));
        assert!(wide.meta_x.unwrap() > wide.total_x.unwrap() + total);
        // The 900x700 case: the pane holds the playhead time but not the
        // total, so the total goes rather than arriving as "00:4".
        let mid = readout_fit(Rect::new(268.0, 0.0, 600.0, 58.0), big, total, meta);
        assert_eq!(mid.big_x, Some(488.0), "the playhead time still fits");
        assert_eq!(mid.total_x, None, "and the total is dropped, not clipped");
        assert_eq!(mid.meta_x, None);
        // Narrower still: nothing is drawn rather than a fragment of a time.
        let tight = readout_fit(Rect::new(0.0, 0.0, 260.0, 58.0), big, total, meta);
        assert_eq!(tight, ReadoutFit::default());
        // Whatever is drawn always ends inside the pane.
        for w in [200.0_f64, 260.0, 300.0, 420.0, 500.0, 700.0, 1200.0] {
            let t = Rect::new(0.0, 0.0, w, 58.0);
            let f = readout_fit(t, big, total, meta);
            for (x, width) in [(f.big_x, big), (f.total_x, total), (f.meta_x, meta)] {
                if let Some(x) = x {
                    assert!(x + width <= t.x1, "{w}px: a part overruns the transport");
                }
            }
        }
    }

    /// Below its breakpoint the inspector gets out of the way, exactly as the
    /// bin does — so the preview and the transport get the width back.
    ///
    /// Regression: a fixed 300px inspector was 60% of a 500px window, which
    /// left the transport ~200px and clipped its fifth button.
    #[test]
    fn a_narrow_window_collapses_the_inspector_and_gives_the_transport_its_width_back() {
        for (w, h) in [(420.0_f64, 560.0_f64), (500.0, 640.0), (620.0, 700.0), (720.0, 820.0)] {
            let (mut app, _doc) = app_sized(w, h);
            app.flap_settled = true;
            app.inspector_flap.snap(false);
            let r = app.regions();
            assert!(r.inspector_overlay, "{w}x{h} must collapse the inspector");
            assert!(!app.inspector_is_visible(&r), "and start it closed");
            assert!(
                r.inspector.x0 >= w - 0.5,
                "{w}x{h}: a closed inspector is off the right edge",
            );
            // The five transport buttons are the point: they must all land
            // inside the transport now that it has the window's width.
            for (_, rect) in app.transport_buttons(&r) {
                assert!(
                    rect.x1 <= r.transport.x1,
                    "{w}x{h}: a transport button is cut off at {} (pane ends {})",
                    rect.x1, r.transport.x1,
                );
            }
        }
        // Wide windows are unchanged: the inspector keeps its column.
        for (w, h) in [(900.0_f64, 700.0_f64), (1280.0, 800.0), (1920.0, 1080.0)] {
            let (mut app, _doc) = app_sized(w, h);
            app.flap_settled = true;
            let r = app.regions();
            assert!(!r.inspector_overlay, "{w}x{h} keeps the inspector docked");
            assert!(app.inspector_is_visible(&r));
            assert_eq!(r.inspector.x1, w, "docked against the right edge");
            assert_eq!(r.preview.x1, r.inspector.x0, "the preview ends at it");
        }
    }

    #[test]
    fn the_inspector_toggle_shows_and_hides_the_collapsed_panel() {
        let (mut app, mut doc) = app_sized(500.0, 640.0);
        app.flap_settled = true;
        app.inspector_flap.snap(false);
        let r = app.regions();
        assert!(r.inspector_btn.width() > 0.0, "a collapsed inspector offers a toggle");
        assert!(!app.inspector_is_visible(&r));
        // Press the toggle: it opens.
        app.last_mouse = r.inspector_btn.center();
        app.handle_press(&mut doc, &r, false);
        app.inspector_flap.snap(true);
        let r = app.regions();
        assert!(app.inspector_is_visible(&r));
        assert!(r.inspector.x1 <= 500.0 + 0.5, "an open flap is on screen");
        // Esc puts it away again.
        app.handle_key(
            &Key::Named(NamedKey::Escape),
            &winit::event::Modifiers::default(),
            &mut doc,
            &r,
        );
        app.inspector_flap.snap(false);
        assert!(!app.inspector_is_visible(&app.regions()));
        // The docked layout has no toggle to offer.
        let (mut wide, _d) = app_sized(1280.0, 800.0);
        wide.flap_settled = true;
        assert_eq!(wide.regions().inspector_btn.width(), 0.0);
    }

    /// The canvas HUD must not paint over the composition.
    ///
    /// Regression: the frame size was drawn in the canvas's top-right, which
    /// is exactly where `Transform::pip()` places a corner inset — so every
    /// top-right picture-in-picture had "1920x1080" burned across it.
    #[test]
    fn the_canvas_hud_leaves_a_corner_inset_alone() {
        let mut app = VideoEditorApp::new_for_shot(ShotScenario::Composed);
        let (w, h) = (900.0_f64, 700.0_f64);
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        app.rect = Rect::new(0.0, 0.0, w, h);
        app.sync(&mut doc, &automerge::ROOT);
        let px = render_pixels(&mut app, w as u32, h as u32, &Theme::dark());
        let r = app.regions();
        // The top-right quarter of the canvas: where the scenario's inset is.
        let corner = Rect::new(
            r.canvas.x0 + r.canvas.width() * 0.6,
            r.canvas.y0,
            r.canvas.x1,
            r.canvas.y0 + r.canvas.height() * 0.25,
        );
        // The inset is a mid-tone amber. HUD text was near-white at 70% over
        // it, which is far brighter than anything the picture itself holds.
        let mut whites = 0;
        let (x0, y0) = (corner.x0 as u32, corner.y0 as u32);
        let (x1, y1) = ((corner.x1 as u32).min(w as u32), (corner.y1 as u32).min(h as u32));
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w as u32 + x) * 4) as usize;
                if i + 2 < px.len()
                    && px[i] > 200 && px[i + 1] > 200 && px[i + 2] > 200
                {
                    whites += 1;
                }
            }
        }
        assert_eq!(whites, 0, "HUD text is painted over the corner inset");
        // The one HUD readout that stays is per-frame information, and it is
        // pinned to the left so it cannot reach a right-hand inset.
        assert!(r.canvas.width() > 0.0);
    }

    /// The overlay media bin dims the editor behind it; it must never erase it.
    ///
    /// Regression: the scrim scaled its alpha by `surfaces.scrim.components[3]`,
    /// meaning to honour a token-carried translucency — but every theme builds
    /// that token opaque, so the multiply was a no-op and a fully-revealed flap
    /// painted a 100%-opaque slab from the topbar to the bottom of the window.
    /// The preview, transport, inspector and timeline all still drew, underneath
    /// it, and every window size "rendered without panicking".
    #[test]
    fn the_open_overlay_bin_leaves_the_editor_behind_it_visible() {
        for theme in [Theme::dark(), Theme::light()] {
            for (w, h) in [(420.0_f64, 560.0_f64), (620.0, 700.0), (720.0, 820.0)] {
                let (mut app, _doc) = app_sized(w, h);
                app.bin_flap.snap(true);
                app.flap_settled = true;
                let r = app.regions();
                assert!(r.bin_overlay, "{w}x{h} must be the overlay layout");
                assert!(r.scrim.width() > 0.0, "an open overlay bin paints a scrim");
                let px = render_pixels(&mut app, w as u32, h as u32, &theme);
                // Sample the editor strictly to the right of the bin flap, so
                // what we measure is the region the scrim covers and the bin
                // does not.
                let right = |rect: Rect| {
                    Rect::new(rect.x0.max(r.bin.x1 + 4.0), rect.y0, rect.x1, rect.y1)
                };
                for (name, rect) in [
                    ("preview", right(r.preview)),
                    ("inspector", right(r.inspector)),
                    ("timeline", right(r.timeline)),
                ] {
                    if rect.width() < 8.0 || rect.height() < 8.0 {
                        continue; // genuinely no room at this size
                    }
                    let colours = region_colours(&px, w as u32, h as u32, rect);
                    assert!(
                        colours > 2,
                        "{w}x{h}: the {name} is a flat slab behind the open bin                          ({colours} distinct colours) — the scrim erased it",
                    );
                }
            }
        }
    }

    // ---- pane scrolling ---------------------------------------------------

    #[test]
    fn the_inspector_scrollbar_is_grabbable_and_reaches_the_effects_section() {
        // Regression: the inspector painted a scroll indicator with no
        // hit-test, so the EFFECTS block clipped off the bottom of the pane
        // could be seen to exist and never be reached.
        let (mut app, mut doc) = app_and_doc();
        // Select a video clip and give it enough effects to overflow the pane.
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip");
        app.selection.select_only(cid);
        for k in EffectKind::ALL {
            app.add_effect_to_selected(&mut doc, k);
        }
        render(&mut app);
        let r = app.regions();
        assert!(app.inspector_max_scroll(&r) > 0.5, "the inspector content overflows");
        let track = app.inspector_scroll_track(&r).expect("an overflowing pane shows a bar");
        let thumb = app
            .inspector_bar
            .thumb_rect(track, app.inspector_metrics(&r))
            .expect("a thumb");
        assert!(thumb.height() < track.height(), "the thumb is shorter than its track");
        // Grab it and drag down.
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y);
        press(&mut app, &mut doc);
        assert!(
            matches!(app.drag, Some(Drag::Scrollbar(ScrollPane::Inspector))),
            "pressing the inspector thumb began a bar drag",
        );
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y + 400.0);
        assert!(app.inspector_scroll > 0.5, "dragging scrolled the inspector");
        release(&mut app, &mut doc);
        // Scrolled to the end, the Add-effect button is inside the pane.
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let l = app.inspector_layout(&r);
        assert!(l.valid);
        assert!(
            l.add_effect.y1 <= r.inspector.y1 + 0.5,
            "the Add effect button is reachable once scrolled ({} vs pane bottom {})",
            l.add_effect.y1,
            r.inspector.y1,
        );
    }

    #[test]
    fn a_press_on_the_inspector_scrollbar_does_not_fall_through_to_a_control() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, _lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip");
        app.selection.select_only(cid);
        for k in EffectKind::ALL {
            app.add_effect_to_selected(&mut doc, k);
        }
        render(&mut app);
        let r = app.regions();
        let track = app.inspector_scroll_track(&r).expect("a bar");
        // Page-click above the thumb: the bar takes it, no slider moves.
        let opacity_before = app.project.clip(cid).unwrap().opacity;
        move_to(&mut app, &mut doc, track.center().x, track.y1 - 2.0);
        press(&mut app, &mut doc);
        assert!(app.inspector_scroll > 0.0, "the track click paged the pane");
        assert_eq!(
            app.project.clip(cid).unwrap().opacity,
            opacity_before,
            "and did not also drag whatever slider was underneath",
        );
    }

    #[test]
    fn the_media_bin_scrolls_so_every_row_can_be_reached() {
        // Regression: rows past the pane were drawn outside it and reachable
        // by nothing at all.
        let (mut app, mut doc) = app_and_doc();
        // Import enough stills to overflow the bin.
        for i in 0..14 {
            let png = write_real_png(16 + i, 16);
            app.import_one(&mut doc, &png);
            std::fs::remove_file(&png).ok();
        }
        render(&mut app);
        let r = app.regions();
        assert!(app.bin_max_scroll(&r) > 0.5, "the bin list overflows its pane");
        let last = *app.bin_order().last().expect("a last row");
        let row_of = |app: &VideoEditorApp, r: &Regions, mid: MediaId| {
            app.bin_item_rects(r).into_iter().find(|(m, _)| *m == mid).map(|(_, rect)| rect)
        };
        let before = row_of(&app, &r, last).expect("the last row has a rect");
        assert!(
            before.y1 > app.bin_viewport(&r).y1,
            "the last row starts out below the fold",
        );
        // Wheel over the bin scrolls the list (it used to zoom the timeline).
        let zoom = app.zoom;
        move_to(&mut app, &mut doc, r.bin.center().x, r.bin.center().y);
        for _ in 0..40 {
            wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::empty());
        }
        assert_eq!(app.zoom, zoom, "a wheel over the bin does not zoom the timeline");
        assert!(app.bin_scroll > 0.0, "it scrolls the bin instead");
        let after = row_of(&app, &r, last).expect("the last row still has a rect");
        assert!(
            after.y1 <= app.bin_viewport(&r).y1 + 1.0,
            "the last row is now inside the pane ({} vs {})",
            after.y1,
            app.bin_viewport(&r).y1,
        );
        // And it is genuinely clickable there.
        move_to(&mut app, &mut doc, after.center().x, after.center().y);
        press(&mut app, &mut doc);
        release(&mut app, &mut doc);
        assert_eq!(app.selected_media, Some(last), "the scrolled-to row selects on click");
    }

    #[test]
    fn the_bin_scrollbar_thumb_drags_the_list() {
        let (mut app, mut doc) = app_and_doc();
        for i in 0..14 {
            let png = write_real_png(16 + i, 16);
            app.import_one(&mut doc, &png);
            std::fs::remove_file(&png).ok();
        }
        render(&mut app);
        let r = app.regions();
        let track = app.bin_scroll_track(&r).expect("an overflowing bin shows a bar");
        let thumb = app.bin_bar.thumb_rect(track, app.bin_metrics(&r)).expect("a thumb");
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y);
        press(&mut app, &mut doc);
        assert!(matches!(app.drag, Some(Drag::Scrollbar(ScrollPane::Bin))));
        move_to(&mut app, &mut doc, thumb.center().x, thumb.center().y + 300.0);
        assert!(app.bin_scroll > 0.0, "dragging the bin thumb scrolled the list");
        release(&mut app, &mut doc);
    }

    #[test]
    fn a_wheel_over_the_preview_does_not_move_the_timeline() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (zoom, scroll) = (app.zoom, app.scroll);
        move_to(&mut app, &mut doc, r.canvas.center().x, r.canvas.center().y);
        wheel(&mut app, &mut doc, 0.0, 3.0, ModifiersState::empty());
        wheel(&mut app, &mut doc, 0.0, -3.0, ModifiersState::empty());
        assert_eq!(app.zoom, zoom, "the preview is not a timeline zoom surface");
        assert_eq!(app.scroll, scroll);
    }

    // ---- filmstrip / import honesty --------------------------------------

    #[test]
    fn every_filmstrip_cell_of_a_visible_clip_has_a_frame_to_draw() {
        // Regression: the clip body fetched ONE thumbnail and tiled it, and
        // when that lookup missed, every cell drew an empty box — a row of
        // pale rectangles that reads as "this clip has no picture". The draw
        // pass now requests a frame per cell, and every cell resolves to
        // something real to blit.
        let (mut app, _doc) = app_and_doc();
        render(&mut app); // the pass that requests the strips
        let r = app.regions();
        let mut checked = 0;
        for (tid, kind, lane) in app.lane_rects(&r) {
            if kind != TrackKind::Video {
                continue;
            }
            for clip in app.project.clips_on(tid) {
                let cr = app.clip_rect(clip, &lane, &r);
                if cr.x1 < r.lanes.x0 || cr.x0 > r.lanes.x1 {
                    continue;
                }
                let inner = clip_inner_rect(cr);
                if inner.height() <= 4.0 || inner.width() <= 4.0 {
                    continue;
                }
                let (_, n) = filmstrip_cells(inner);
                assert!(n >= 1);
                let span = clip.out_point - clip.in_point;
                for i in 0..n {
                    let t = clip.in_point + ((i as f64 + 0.5) / n as f64) * span;
                    assert!(
                        app.thumbnails.frame_at(clip.media, t).is_some(),
                        "cell {i} of the clip at {} has no frame to draw",
                        clip.start,
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "the fixture actually has visible video cells to check");
    }

    #[test]
    fn the_strip_request_and_the_strip_draw_agree_on_cell_count() {
        // The two passes derive the cell layout from the same helpers; if they
        // ever diverge, frames get decoded at times no cell asks for and every
        // cell falls back to the representative.
        for (w, h) in [(60.0, 20.0), (300.0, 44.0), (1200.0, 60.0)] {
            let inner = Rect::new(0.0, 0.0, w, h);
            let (cell_w, n) = filmstrip_cells(inner);
            assert!(cell_w >= 24.0 && cell_w <= 56.0, "cell width stays in range");
            assert!(n >= 1, "there is always at least one cell");
            assert!(
                (n as f64) * (cell_w + 2.0) <= w + cell_w + 2.0,
                "the cells fit the body they were computed for",
            );
        }
    }

    #[test]
    fn a_refused_import_tells_the_user_what_the_file_actually_is() {
        let (mut app, mut doc) = app_and_doc();
        let dir = std::env::temp_dir().join(format!("qos-ve-import-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not-a-video.mp4");
        std::fs::write(&path, b"<!DOCTYPE html><html><body>nope</body></html>").unwrap();
        let before = app.project.media.len();
        assert!(!app.import_one(&mut doc, &path), "an HTML page is refused");
        let notice = app.import_notice.clone().expect("a refusal explains itself");
        assert!(
            notice.contains("not-a-video.mp4"),
            "the notice names the file: {notice}",
        );
        assert!(
            notice.contains("not a media file"),
            "and carries the probe's own reason: {notice}",
        );
        assert_eq!(app.project.media.len(), before, "nothing was added to the bin");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_import_carries_an_unknown_frame_rate_as_unknown() {
        // A still image has no frame rate. Substituting 30 would put a number
        // in the bin that the file never stated.
        let (mut app, mut doc) = app_and_doc();
        let png = write_real_png(24, 16);
        let want = png.file_name().unwrap().to_string_lossy().into_owned();
        assert!(app.import_one(&mut doc, &png), "a real PNG imports");
        let added = app
            .project
            .media
            .iter()
            .find(|m| m.name == want)
            .expect("the imported still is in the bin");
        assert_eq!(added.fps, 0.0, "an unknown frame rate stays 0, not an invented 30");
        assert_eq!(added.width, 24, "and the real dimensions came from the file");
        std::fs::remove_file(&png).ok();
    }

    #[test]
    fn pasting_after_its_track_is_locked_lands_on_a_track_that_accepts_it() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (tid, lane) = lane_of(&app, &r, TrackKind::Video, 0);
        let cid = app.project.clips_on(tid).first().map(|c| c.id).expect("a clip");
        let cr = clip_screen_rect(&app, &r, cid);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        release(&mut app, &mut doc);
        assert!(key_press(&mut app, &mut doc, "c", true), "Ctrl+C is handled");
        // Lock the source track, then paste.
        let header = Rect::new(r.gutter.x0, lane.y0, r.gutter.x1, lane.y1);
        let pill = app.lock_pill_rect(header);
        click(&mut app, &mut doc, pill.center().x, pill.center().y);
        app.playhead = 2.0;
        assert!(key_press(&mut app, &mut doc, "v", true), "Ctrl+V is handled");
        let reloaded = VideoDoc::load_project(&doc);
        let new_id = app.selected().expect("something was pasted");
        let pasted = reloaded.clip(new_id).expect("the pasted clip is in the CRDT");
        assert_ne!(pasted.track, tid, "the paste avoided the locked track");
        assert_eq!(
            reloaded.track(pasted.track).map(|t| t.kind),
            Some(TrackKind::Video),
            "and still landed on a video track",
        );
    }

    #[test]
    fn right_click_bin_item_opens_menu_and_delete_removes_media_from_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, row) = app.bin_item_rects(&r)[0];
        // Right-click the row → the context menu opens targeting that media.
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        right_press(&mut app, &mut doc);
        assert!(app.bin_menu.is_open(), "right-click opens the bin context menu");
        assert!(matches!(app.menu_target, Some(MenuTarget::Media(m)) if m == mid));

        let before = VideoDoc::load_project(&doc).media.len();
        click_menu_item(&mut app, &mut doc, "Delete");
        let after = VideoDoc::load_project(&doc);
        assert_eq!(after.media.len(), before - 1, "Delete removed the media from the CRDT");
        assert!(after.media(mid).is_none(), "the deleted source is gone");
        assert!(!app.bin_menu.is_open(), "the menu closed after choosing Delete");
    }

    #[test]
    fn inline_rename_writes_the_new_name_to_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let (mid, _row) = app.bin_item_rects(&r)[0];
        app.begin_rename(mid);
        assert!(app.bin_rename.is_some(), "rename began");
        // begin_edit select-all: the first keystroke replaces the whole name.
        for ch in ["R", "e", "e", "l"] {
            dispatch(&mut app, &mut doc, char_ev(ch));
        }
        // Enter commits → dispatches an UpsertMedia carrying the new name.
        dispatch(&mut app, &mut doc, named_ev(NamedKey::Enter));
        assert!(app.bin_rename.is_none(), "Enter ends the rename");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.media(mid).unwrap().name, "Reel", "rename wrote through to the CRDT");
        assert!(
            app.actions.take_committed().is_some(),
            "the rename dispatched an action, arming one undo boundary",
        );
    }

    #[test]
    fn drag_bin_item_to_a_lane_places_a_clip_at_the_drop_time() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // A visual bin item, and a video lane to drop it on.
        let (mid, row) = app
            .bin_item_rects(&r)
            .into_iter()
            .find(|(m, _)| app.project.media(*m).map(|mm| mm.kind.is_visual()).unwrap_or(false))
            .expect("a visual bin item");
        let (tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(_, k, _)| *k == TrackKind::Video)
            .expect("a video lane");
        let before = VideoDoc::load_project(&doc).clips_on(tid).len();
        // Press on the bin item, drag past the threshold onto the lane, release.
        move_to(&mut app, &mut doc, row.center().x, row.center().y);
        press(&mut app, &mut doc);
        let drop_x = lane.x0 + 300.0;
        let drop_y = lane.center().y;
        move_to(&mut app, &mut doc, drop_x, drop_y);
        assert!(
            matches!(app.bin_drag, Some(BinDrag { active: true, .. })),
            "moving past the threshold promotes the bin drag",
        );
        release(&mut app, &mut doc);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(
            reloaded.clips_on(tid).len(),
            before + 1,
            "a clip was placed on the drop track in the CRDT",
        );
        let placed = app.selected().and_then(|id| reloaded.clip(id)).expect("placed clip selected");
        assert_eq!(placed.media, mid, "the placed clip references the dragged media");
        let expected_t = app.x_to_time(drop_x, &r.lanes);
        assert!(
            (placed.start - expected_t).abs() < 0.2,
            "clip placed near the drop time ({} vs {expected_t})",
            placed.start,
        );
        assert!(app.bin_drag.is_none(), "the drag ended on release");
    }

    /// Write a real 16-bit PCM mono WAV to a temp file for waveform tests.
    fn write_real_wav() -> std::path::PathBuf {
        let mut data = Vec::new();
        for i in 0..8000u32 {
            let s = ((i as f32 * 0.2).sin() * 0.7 * 32767.0) as i16;
            data.extend_from_slice(&s.to_le_bytes());
        }
        let sr = 8000u32;
        let mut v = Vec::new();
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&1u16.to_le_bytes());
        v.extend_from_slice(&sr.to_le_bytes());
        v.extend_from_slice(&(sr * 2).to_le_bytes());
        v.extend_from_slice(&2u16.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes());
        v.extend_from_slice(b"data");
        v.extend_from_slice(&(data.len() as u32).to_le_bytes());
        v.extend_from_slice(&data);
        let f = std::env::temp_dir().join(format!("qos-ve-appwave-{}.wav", std::process::id()));
        std::fs::write(&f, v).unwrap();
        f
    }

    #[test]
    fn app_warms_a_real_waveform_for_an_audio_source() {
        // An audio source pointing at a real WAV file gets real peaks cached by
        // the sync wiring (synchronous warm, as a --shots capture uses).
        let f = write_real_wav();
        let mut app = VideoEditorApp::new();
        app.warm_waveforms_sync = true;
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        let m = MediaItem {
            id: Uuid::new_v4(),
            name: "vo.wav".into(),
            kind: MediaKind::Audio,
            duration: 1.0,
            width: 0,
            height: 0,
            fps: 0.0,
            palette: 5,
            path: f.to_string_lossy().into_owned(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        };
        let mid = m.id;
        VideoDoc::upsert_media(&mut doc, &m);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.waveforms.peaks(mid).is_some(),
            "the app warmed real peaks for the audio source",
        );
        let peaks = app.waveforms.peaks(mid).unwrap();
        assert_eq!(peaks.len(), crate::waveform::WAVE_BUCKETS);
        assert!(peaks.iter().cloned().fold(0.0, f32::max) > 0.9, "a real signal is present");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn clicking_add_audio_track_button_adds_a_track_to_the_crdt() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        let before = VideoDoc::load_project(&doc)
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .count();
        let (_, abtn) = app
            .add_track_buttons(&r)
            .into_iter()
            .find(|(k, _)| *k == TrackKind::Audio)
            .expect("an add-audio-track button");
        click(&mut app, &mut doc, abtn.center().x, abtn.center().y);
        let after = VideoDoc::load_project(&doc);
        assert_eq!(
            after.tracks.iter().filter(|t| t.kind == TrackKind::Audio).count(),
            before + 1,
            "the add-audio-track button added a track to the CRDT",
        );
    }

    #[test]
    fn right_click_track_header_delete_removes_the_track_and_its_clips() {
        let (mut app, mut doc) = app_and_doc();
        let r = app.regions();
        // A track that actually has clips, so we can prove they go too.
        let (tid, _k, lane) = app
            .lane_rects(&r)
            .into_iter()
            .find(|(tid, _, _)| !app.project.clips_on(*tid).is_empty())
            .expect("a track with clips");
        let clip_ids: Vec<Uuid> = app.project.clips_on(tid).iter().map(|c| c.id).collect();
        // Right-click the header (gutter) → the track menu opens for that track.
        move_to(&mut app, &mut doc, r.gutter.x0 + 20.0, lane.center().y);
        right_press(&mut app, &mut doc);
        assert!(app.bin_menu.is_open(), "right-click opens the track menu");
        assert!(matches!(app.menu_target, Some(MenuTarget::Track(t)) if t == tid));
        let before = VideoDoc::load_project(&doc).tracks.len();
        click_menu_item(&mut app, &mut doc, "Delete track");
        let after = VideoDoc::load_project(&doc);
        assert_eq!(after.tracks.len(), before - 1, "the track was removed from the CRDT");
        assert!(after.track(tid).is_none());
        for cid in clip_ids {
            assert!(after.clip(cid).is_none(), "the track's clips were removed too");
        }
    }

    // ---- geometry, rate, titles, keys and markers -------------------------

    /// Scroll the inspector until `find` returns a rect that is actually on
    /// screen, then hand back that rect. The new sections live below the fold
    /// on a 1280x800 window, and a test that clicked their un-scrolled
    /// coordinates would be pressing the timeline.
    fn reveal<F>(app: &mut VideoEditorApp, find: F) -> Rect
    where
        F: Fn(&InspectorLayout) -> Option<Rect>,
    {
        let r = app.regions();
        let max = app.inspector_max_scroll(&r);
        let mut best = None;
        let mut offset = 0.0;
        while offset <= max + 1.0 {
            app.inspector_scroll = offset.min(max);
            if let Some(rect) = find(&app.inspector_layout(&r)) {
                if rect.y0 > r.inspector.y0 + 8.0 && rect.y1 < r.inspector.y1 - 8.0 {
                    best = Some(rect);
                    break;
                }
            }
            offset += 12.0;
        }
        best.expect("control never became visible at any scroll offset")
    }

    /// Every new control writes through an action, so the value has to be in
    /// the CRDT — not merely in the in-memory preview — once the interaction
    /// ends. This drags each ranged control and reads the document back.
    #[test]
    fn dragging_a_geometry_control_writes_the_transform_to_the_document() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        assert!(
            VideoDoc::load_project(&doc).clip(id).unwrap().transform.is_identity(),
            "starts as the plain aspect-fit",
        );
        // Drag Scale X to a quarter along its 0.05..4 span.
        let track = reveal(&mut app, |l| l.num(NumField::ScaleX));
        let x = track.x0 + track.width() * 0.25;
        click(&mut app, &mut doc, x, track.center().y);
        release(&mut app, &mut doc);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().transform;
        let want = NumField::ScaleX.from_frac(0.25);
        assert!(
            (after.scale_x - want).abs() < 0.05,
            "scale_x is {} not ~{want}", after.scale_x,
        );
        assert!(!after.is_identity(), "the clip is no longer plainly fitted");
        // Crop writes its own edge and leaves the others alone.
        let track = reveal(&mut app, |l| l.num(NumField::CropTop));
        click(&mut app, &mut doc, track.x0 + track.width() * 0.5, track.center().y);
        release(&mut app, &mut doc);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().transform;
        assert!(after.crop_top > 0.3 && after.crop_top < 0.6, "crop_top is {}", after.crop_top);
        assert_eq!(after.crop_bottom, 0.0, "only the dragged edge moved");
        assert!(after.is_cropped());
    }

    /// A rate slider on a linear track would put 1x at 5% of the way along.
    /// It is logarithmic, so normal speed is the centre and half / double are
    /// symmetric about it — and the value reaches the document.
    #[test]
    fn the_rate_control_is_logarithmic_and_re_times_the_clip() {
        assert!(
            (NumField::Speed.to_frac(1.0) - 0.5).abs() < 1e-9,
            "normal speed sits at the centre of the track",
        );
        let half = NumField::Speed.to_frac(0.5);
        let double = NumField::Speed.to_frac(2.0);
        assert!(
            ((0.5 - half) - (double - 0.5)).abs() < 1e-9,
            "half and double speed are the same distance from normal",
        );
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let before = VideoDoc::load_project(&doc).clip(id).unwrap().duration();
        // Click the "2x" preset.
        let rect = reveal(&mut app, |l| {
            l.speed_presets.iter().find(|(rate, _)| *rate == 2.0).map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        assert_eq!(after.speed.rate(), 2.0);
        assert!(
            (after.duration() - before / 2.0).abs() < 1e-6,
            "2x halves the clip's length on the timeline",
        );
        assert_eq!(after.source_span(), before, "and leaves its source window alone");
        // Reverse is a separate decision from the rate.
        let rect = reveal(&mut app, |l| l.reverse);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        assert!(after.speed.reverse, "the toggle reversed it");
        assert_eq!(after.speed.rate(), 2.0, "without disturbing the rate");
    }

    /// A corner preset has to produce a real inset — scaled down and pushed
    /// into the named corner — not merely a non-default transform.
    #[test]
    fn a_picture_in_picture_preset_insets_the_clip_into_that_corner() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        // The layout lists the corners in the order top-left, top-right,
        // bottom-left, bottom-right.
        let rect = reveal(&mut app, |l| l.pip.get(3).map(|(_, _, r)| *r));
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let t = VideoDoc::load_project(&doc).clip(id).unwrap().transform;
        assert!(t.scale_x < 0.5 && t.scale_y < 0.5, "the picture is inset, not full frame");
        assert!(t.pos_x > 0.0, "bottom-right sits right of centre");
        assert!(t.pos_y > 0.0, "and below it");
        // And the way back.
        let rect = reveal(&mut app, |l| l.reset_transform);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert!(
            VideoDoc::load_project(&doc).clip(id).unwrap().transform.is_identity(),
            "Fill frame restores the plain aspect-fit",
        );
    }

    /// The three states a key control shows are what the whole keyframe UI
    /// rests on: if "animated" and "a key is here" looked the same, clicking
    /// would be a coin toss between adding and editing.
    #[test]
    fn the_key_control_reports_off_between_and_on_a_key() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        assert_eq!(app.key_state(Param::Opacity), KeyState::Off);
        // One click on an unkeyed control starts the animation, at whatever
        // the parameter already reads — so keying never moves the picture.
        let before = app.project.clip(id).unwrap().opacity;
        let rect = reveal(&mut app, |l| {
            l.keys.iter().find(|(p, _)| *p == Param::Opacity).map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let clip = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        assert!(clip.anim.is_animated(Param::Opacity), "the first click keyed it");
        assert!(
            (clip.param_at(Param::Opacity, 23.0) - before).abs() < 1e-9,
            "and planted the value it already had",
        );
        assert_eq!(app.key_state(Param::Opacity), KeyState::OnKey);
        // Step off the key: still animated, no key here.
        app.playhead = 24.0;
        assert_eq!(app.key_state(Param::Opacity), KeyState::Between);
        // A second click at a keyed control opens the menu rather than
        // silently re-keying, because there is more than one thing to do.
        let rect = reveal(&mut app, |l| {
            l.keys.iter().find(|(p, _)| *p == Param::Opacity).map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        assert!(app.bin_menu.is_open());
        assert!(matches!(app.menu_target, Some(MenuTarget::Keyframe(Param::Opacity))));
    }

    /// Once a parameter is animated its constant is dead — every reader goes
    /// through the curve. Dragging its slider therefore has to write a *key*,
    /// or the control would appear to do nothing.
    #[test]
    fn dragging_an_animated_slider_writes_a_key_not_a_dead_constant() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        // Key it once at the head.
        app.key_here(&mut doc, Param::Opacity);
        let keys_before = VideoDoc::load_project(&doc)
            .clip(id).unwrap().anim.track(Param::Opacity).unwrap().keys.len();
        assert_eq!(keys_before, 1);
        // Move on and drag the opacity slider to a new value.
        app.playhead = 25.0;
        let r = app.regions();
        app.inspector_scroll = 0.0;
        let track = app.inspector_layout(&r).opacity.expect("opacity row");
        let x = track.x0 + track.width() * 0.3;
        click(&mut app, &mut doc, x, track.center().y);
        release(&mut app, &mut doc);
        let clip = VideoDoc::load_project(&doc).clip(id).unwrap().clone();
        let keys = &clip.anim.track(Param::Opacity).unwrap().keys;
        assert_eq!(keys.len(), 2, "the drag added a second key rather than writing a constant");
        assert!(
            (clip.param_at(Param::Opacity, 25.0) - 0.3).abs() < 0.06,
            "the curve reads the dragged value here, got {}",
            clip.param_at(Param::Opacity, 25.0),
        );
        // The head key is untouched, so the animation actually goes somewhere.
        assert!(
            (clip.param_at(Param::Opacity, 23.0) - clip.param_at(Param::Opacity, 25.0)).abs() > 0.1,
            "the two keys hold different values",
        );
    }

    /// The keyframe menu's rows are indexed past its separators, and a wrong
    /// offset would silently perform the neighbouring action.
    #[test]
    fn the_keyframe_menu_shapes_and_removes_the_key_under_the_playhead() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        app.playhead = 23.0;
        app.key_here(&mut doc, Param::Opacity);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.project.clip(id).unwrap().key_interp_at(Param::Opacity, 23.0),
            Some(Interp::Linear),
        );
        // Row 3 is Hold: 0 remove, 1 separator, 2 Linear, 3 Hold, 4 Ease.
        app.open_key_menu(Param::Opacity, Point::new(400.0, 400.0));
        app.key_menu_choose(&mut doc, Param::Opacity, 3);
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.project.clip(id).unwrap().key_interp_at(Param::Opacity, 23.0),
            Some(Interp::Hold),
            "the menu re-shaped the key it was opened on",
        );
        // Row 0 removes it, and the last key going takes the curve with it.
        app.open_key_menu(Param::Opacity, Point::new(400.0, 400.0));
        app.key_menu_choose(&mut doc, Param::Opacity, 0);
        app.sync(&mut doc, &automerge::ROOT);
        assert!(!app.project.clip(id).unwrap().anim.is_animated(Param::Opacity));
    }

    /// "+ Title" has to go all the way to an editable clip: a title with no
    /// bytes to import is the one source where leaving an unnamed item in the
    /// bin would be pure friction.
    #[test]
    fn adding_a_title_places_a_clip_and_opens_its_words() {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 12.0;
        let media_before = app.project.media.len();
        let clips_before = app.project.clips.len();
        let r = app.regions();
        let btn = app.add_title_rect(&r);
        click(&mut app, &mut doc, btn.center().x, btn.center().y);
        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.media.len(), media_before + 1, "a source was made");
        assert_eq!(p.clips.len(), clips_before + 1, "and placed on the timeline");
        let item = p.media.iter().find(|m| m.kind == MediaKind::Text).expect("a title source");
        assert!(item.text.is_some(), "with a text body to edit");
        // A title has no bytes: nothing to fetch, so it is never "not on this
        // device" on any peer.
        assert!(item.path.is_empty());
        assert!(item.kind.is_generated());
        let clip = p.clips.iter().find(|c| c.media == item.id).expect("its clip");
        assert!(
            (clip.start - 12.0).abs() < 0.1,
            "placed at the playhead, got {}", clip.start,
        );
        assert_eq!(app.selection.primary(), Some(clip.id), "and selected");
        assert!(app.title_edit.is_some(), "with the caret in its words");
        // Typing and committing writes the words and renames the source.
        app.commit_title_text(&mut doc, item.id, "SUNSET");
        let p = VideoDoc::load_project(&doc);
        let item = p.media.iter().find(|m| m.id == item.id).unwrap();
        assert_eq!(item.text.as_ref().unwrap().content, "SUNSET");
        assert_eq!(item.name, "SUNSET", "the bin row follows the words");
    }

    /// The title controls edit the *source*, so they commit a media write —
    /// a clip write would drop them on the floor.
    #[test]
    fn the_title_controls_write_the_source() {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 12.0;
        app.add_title(&mut doc);
        app.title_edit = None;
        let mid = app.selected_media_item().unwrap().id;
        // Alignment.
        let rect = reveal(&mut app, |l| {
            l.title_align.iter().find(|(a, _)| *a == TextAlign::Right).map(|(_, r)| *r)
        });
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let t = VideoDoc::load_project(&doc)
            .media(mid).unwrap().text.clone().unwrap();
        assert_eq!(t.align, TextAlign::Right);
        // Colour: an authored value, not a palette slot, so it survives a
        // theme change.
        let rect = reveal(&mut app, |l| l.title_colors.get(2).map(|(_, r)| *r));
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let t = VideoDoc::load_project(&doc).media(mid).unwrap().text.clone().unwrap();
        assert_eq!(t.color, TITLE_COLORS[2]);
        // Size is a fraction of the frame height, dragged.
        let track = reveal(&mut app, |l| l.num(NumField::TitleSize));
        let before = t.size;
        click(&mut app, &mut doc, track.x0 + track.width() * 0.8, track.center().y);
        release(&mut app, &mut doc);
        let t = VideoDoc::load_project(&doc).media(mid).unwrap().text.clone().unwrap();
        assert!(t.size > before, "the size slider raised it: {} -> {}", before, t.size);
        assert!(t.size <= 0.4, "and stayed inside the offered range");
    }

    /// Markers are listed, renamed, navigated and deleted from one place, and
    /// each kind commits through the action that owns it.
    #[test]
    fn the_marker_list_names_navigates_and_deletes() {
        let (mut app, mut doc) = app_and_doc();
        app.playhead = 8.0;
        // M marks the playhead and opens the name for typing.
        assert!(key_press(&mut app, &mut doc, "m", false));
        assert!(app.marker_rename.is_some(), "an unnamed tick is a marker you cannot find");
        let target = app.marker_rename.as_ref().unwrap().0;
        app.commit_marker_rename(&mut doc, target, "act two");
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.markers.len(), 1);
        assert_eq!(p.markers[0].name, "act two");
        assert!((p.markers[0].t - 8.0).abs() < 1e-6);
        // A second one, so navigation has somewhere to go.
        app.playhead = 20.0;
        key_press(&mut app, &mut doc, "m", false);
        let second = app.marker_rename.as_ref().unwrap().0;
        app.commit_marker_rename(&mut doc, second, "sunset");
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        // , and . step between them and stop at the ends.
        app.playhead = 14.0;
        key_press(&mut app, &mut doc, ",", false);
        assert!((app.playhead - 8.0).abs() < 1e-6, "stepped back to the first");
        key_press(&mut app, &mut doc, ".", false);
        assert!((app.playhead - 20.0).abs() < 1e-6, "and forward past it to the second");
        key_press(&mut app, &mut doc, ".", false);
        assert!((app.playhead - 20.0).abs() < 1e-6, "navigation stops rather than wrapping");
        // Clicking a row takes the playhead there; the cross removes it.
        let row = reveal(&mut app, |l| l.marker_rows.first().map(|(_, r)| *r));
        click(&mut app, &mut doc, row.center().x, row.center().y);
        assert!((app.playhead - 8.0).abs() < 1e-6, "the row navigated");
        let row = reveal(&mut app, |l| l.marker_rows.first().map(|(_, r)| *r));
        let (_, _, del) = marker_row_parts(row);
        click(&mut app, &mut doc, del.center().x, del.center().y);
        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.markers.len(), 1, "the cross deleted one");
        assert_eq!(p.markers[0].name, "sunset", "and left the other");
    }

    /// A clip marker is stored clip-local and edited through a clip write,
    /// which is a different action from a timeline marker's.
    #[test]
    fn a_clip_marker_is_listed_beside_the_timeline_ones_and_deletes_as_a_clip_write() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        let start = app.project.clip(id).unwrap().start;
        app.selection.select_only(id);
        app.project.clip_mut(id).unwrap().add_marker(start + 1.0, "beat", 2);
        app.dispatch_clip(&mut doc, id);
        app.sync(&mut doc, &automerge::ROOT);
        app.playhead = 4.0;
        key_press(&mut app, &mut doc, "m", false);
        app.marker_rename = None;
        app.sync(&mut doc, &automerge::ROOT);
        // One list, both kinds, sorted by where they actually land.
        let r = app.regions();
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let rows = app.inspector_layout(&r).marker_rows.clone();
        assert_eq!(rows.len(), 2, "the clip's marker is listed with the timeline's");
        assert!(rows.iter().any(|(m, _)| m.clip == Some(id)));
        assert!(rows.iter().any(|(m, _)| m.clip.is_none()));
        // The clip one reports an absolute time even though it is stored local.
        let clip_row = rows.iter().find(|(m, _)| m.clip == Some(id)).unwrap().0;
        assert!((clip_row.at - (start + 1.0)).abs() < 1e-6);
        // Deleting it writes the clip, not the project.
        app.delete_marker(&mut doc, clip_row);
        let p = VideoDoc::load_project(&doc);
        assert!(p.clip(id).unwrap().markers.is_empty(), "gone from the clip");
        assert_eq!(p.markers.len(), 1, "and the timeline marker is untouched");
    }

    /// A folded section may hide controls; it must never hide state.
    #[test]
    fn a_folded_section_still_reports_what_is_inside_it() {
        let (mut app, mut doc) = app_and_doc();
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let clip = app.project.clip(id).unwrap().clone();
        // Nothing to report while everything is at its default.
        assert_eq!(app.section_summary(Section::Speed, &clip, None), None);
        assert_eq!(app.section_summary(Section::Transform, &clip, None), None);
        // Re-time and inset it, and the folded headers say so.
        app.edit_selected(&mut doc, |c| {
            c.set_speed(2.0, true);
            c.transform = crate::model::Transform::pip(0.3, (1.0, -1.0), 0.04);
            c.transform.crop_left = 0.1;
        });
        let clip = app.project.clip(id).unwrap().clone();
        let speed = app.section_summary(Section::Speed, &clip, None).expect("a rate to report");
        assert!(speed.contains('2') && speed.contains("rev"), "speed summary was {speed:?}");
        let geom = app.section_summary(Section::Transform, &clip, None).expect("a placement");
        assert!(geom.contains("cropped"), "transform summary was {geom:?}");
        // Folding is a property of the panel, not of the clip: it survives
        // selecting something else.
        app.collapsed.insert(Section::Transform);
        let other = app.project.top_video_clip_at(6.0).map(|c| c.id);
        if let Some(other) = other {
            app.selection.select_only(other);
        }
        assert!(app.collapsed.contains(&Section::Transform));
        let r = app.regions();
        let l = app.inspector_layout(&r);
        assert!(l.num(NumField::ScaleX).is_none(), "a folded section lays out no controls");
        assert!(
            l.sections.iter().any(|(s, _)| *s == Section::Transform),
            "but keeps its header, so it can be unfolded again",
        );
    }

    #[test]
    fn inspector_add_effect_button_adds_via_the_picker() {
        let (mut app, mut doc) = app_and_doc();
        // A video clip with no effects (the market cutaway around t=23).
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
        let r = app.regions();
        // The effects controls sit below the fold; scroll to reveal them.
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let l = app.inspector_layout(&r);
        assert!(
            l.add_effect.width() > 0.0 && r.inspector.contains(l.add_effect.center()),
            "the add-effect button is visible when scrolled",
        );
        let before = VideoDoc::load_project(&doc).clip(id).unwrap().effects.len();
        click(&mut app, &mut doc, l.add_effect.center().x, l.add_effect.center().y);
        assert!(app.bin_menu.is_open(), "add-effect opens the picker");
        assert!(matches!(app.menu_target, Some(MenuTarget::AddEffect)));
        // Choose the first effect kind (Grayscale, idx 0).
        let menu = app.bin_menu.get_rect();
        let cx = menu.center().x;
        let ry = (0..(menu.height() as i64))
            .map(|d| menu.y0 + d as f64 + 0.5)
            .find(|&y| app.bin_menu.item_at([cx, y]) == Some(0))
            .unwrap();
        move_to(&mut app, &mut doc, cx, ry);
        press(&mut app, &mut doc);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().effects.clone();
        assert_eq!(after.len(), before + 1, "an effect was added through the inspector");
        assert_eq!(after.last().unwrap().kind, EffectKind::Grayscale);
    }

    #[test]
    fn inspector_remove_and_reorder_effects() {
        let (mut app, mut doc) = app_and_doc();
        // The drone clip carries two seeded effects.
        let id = app.project.top_video_clip_at(6.0).unwrap().id;
        app.selection.select_only(id);
        let kinds0: Vec<_> = VideoDoc::load_project(&doc)
            .clip(id)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.kind)
            .collect();
        assert!(kinds0.len() >= 2, "the drone has seeded effects");
        let r = app.regions();
        app.inspector_scroll = app.inspector_max_scroll(&r);
        let l = app.inspector_layout(&r);
        // Move the second effect up via its ▲.
        let (up, _d, _s, _x) = VideoEditorApp::effect_row_parts(l.effect_rows[1]);
        assert!(r.inspector.contains(up.center()), "the reorder control is visible");
        click(&mut app, &mut doc, up.center().x, up.center().y);
        let kinds1: Vec<_> = VideoDoc::load_project(&doc)
            .clip(id)
            .unwrap()
            .effects
            .iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds1[0], kinds0[1], "the second effect moved to the front");
        assert_eq!(kinds1[1], kinds0[0]);
        // Remove the head via its ✕.
        let l = app.inspector_layout(&r);
        let (_u, _dn, _sl, remove) = VideoEditorApp::effect_row_parts(l.effect_rows[0]);
        click(&mut app, &mut doc, remove.center().x, remove.center().y);
        let after = VideoDoc::load_project(&doc).clip(id).unwrap().effects.len();
        assert_eq!(after, kinds0.len() - 1, "the ✕ removed one effect from the CRDT");
    }

    #[test]
    fn dropped_file_imports_into_the_bin_through_the_doc() {
        // An OS file drop onto the window imports, exactly like the portal pick.
        let f = write_real_png(200, 120);
        let mut app = VideoEditorApp::new();
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(app.project.media.is_empty(), "blank project starts empty");
        dispatch(&mut app, &mut doc, WindowEvent::DroppedFile(f.clone()));
        assert_eq!(app.project.media.len(), 1, "the dropped file landed in the bin");
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.media.len(), 1, "and replicated through the document");
        assert_eq!(reloaded.media[0].width, 200, "with real probed metadata");
        std::fs::remove_file(&f).ok();
    }

    #[test]
    fn double_click_bin_item_on_empty_project_creates_track_and_clip() {
        // A blank live project with one imported media and no tracks: double-
        // clicking a bin item must create a matching track and place the clip.
        let f = write_real_png(320, 240);
        let mut app = VideoEditorApp::new_import(vec![f.clone()]);
        app.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(app.project.media.len(), 1);
        assert!(app.project.tracks.is_empty(), "blank project starts with no tracks");
        let r = app.regions();
        let (_mid, rect) = app.bin_item_rects(&r)[0];
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        click(&mut app, &mut doc, rect.center().x, rect.center().y);
        let reloaded = VideoDoc::load_project(&doc);
        assert_eq!(reloaded.tracks.len(), 1, "a matching track was created");
        assert_eq!(reloaded.clips.len(), 1, "the clip was placed on the timeline");
        std::fs::remove_file(&f).ok();
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
        app.selection.clear();
        let r = app.regions();
        // The market cutaway on V1 sits around t=22.
        let clip = app.project.top_video_clip_at(22.0).unwrap().clone();
        let (_tid, _k, lane) = app.lane_rects(&r).into_iter()
            .find(|(tid, _, _)| *tid == clip.track).unwrap();
        let cr = app.clip_rect(&clip, &lane, &r);
        click(&mut app, &mut doc, cr.center().x, cr.center().y);
        assert_eq!(app.selected(), Some(clip.id), "clicking a clip selects it");
    }

    #[test]
    fn editing_brightness_brightens_the_preview_composite() {
        // Drive the real inspector edit path, then assert the preview module's
        // composited frame for that clip is brighter — proving inspector →
        // CRDT → preview is one connected pipeline.
        let (mut app, mut doc) = app_and_doc();
        // Pick the market cutaway on V1 (neutral grade, fully steady at t=23).
        let id = app.project.top_video_clip_at(23.0).unwrap().id;
        app.selection.select_only(id);
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
