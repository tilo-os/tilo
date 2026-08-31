//! The video editor's exported **actions** and the **resource tree** it
//! publishes to Ctrl+K.
//!
//! Every document-mutating operation the editor performs on its shared timeline
//! is a [`VideoEditorAction`]: a small serializable value that deterministically
//! mutates the CRDT (see [`crate::doc`], where the project, media bin, tracks
//! and clips live as keyed CRDT maps). Because the timeline already lives in the
//! shared document, an action replicates and replays for free. The exact same
//! [`EncodedAction`] is what the on-screen affordances (the timeline drag, the
//! inspector sliders, the split / trash buttons, the keyboard shortcuts)
//! dispatch and what the Ctrl+K palette dispatches — one source of truth, one
//! replay path.
//!
//! The resource tree mirrors the data model: the project owns its tracks, and a
//! track owns its clips. Each exports the [`ActionSpec`]s Ctrl+K offers.
//!
//! ## What is (and isn't) an action
//!
//! * **Action** — the clip *metadata*: its source reference, in/out window,
//!   track and timeline position, plus grade / opacity / volume / fades /
//!   transition; track add/rename/mute/delete/reorder; project settings.
//! * **Not an action (presentation)** — the playhead / scrub, zoom, selection,
//!   which panel is open, play/pause transport. Nothing to replicate.
//! * **Not an action (external)** — media decode, thumbnail / waveform
//!   generation, GPU render, export/encode, FUSE byte hydration. The clip
//!   metadata is the action; the *pixels* are produced outside the document.
//!
//! ## Determinism contract
//!
//! Ids are minted at *authoring* time and carried in the payload; a clip's
//! final position and trim are carried on the commit (drag end / key press),
//! not per drag frame. [`Action::apply`] never touches the RNG or the
//! wall-clock, and a malformed payload (an unparseable id) is a **no-op** rather
//! than an error, so a bad replay can't wedge a peer. Multi-clip bulk edits
//! (a ripple delete, a split) are carried as one action so they land as a
//! single undo boundary and replay atomically.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ParamType, Resource, ResourceProvider,
};

use crate::doc::VideoDoc;
use crate::model::{
    Clip, EffectKind, Filters, MaskShape, MediaItem, MediaKind, Project, TextSource, Track,
    TrackKind, Transition,
};
use crate::param::{Interp, Param};

/// Registry `type_id` for every video-editor action.
pub const VIDEO_EDITOR_ACTION_TYPE: &str = "video_editor.action";

/// A serializable snapshot of a clip's scalar fields, in exactly the
/// representation [`crate::doc`] stores (ids as strings, transition as its
/// stable tag). Carrying the document shape keeps [`Action::apply`] a pure
/// write with no re-derivation, and makes [`Self::from_clip`]/[`Self::to_clip`]
/// lossless round-trips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipData {
    pub id: String,
    pub track: String,
    pub media: String,
    pub start: f64,
    pub in_point: f64,
    pub out_point: f64,
    pub opacity: f64,
    pub volume: f64,
    pub transition: String,
    pub palette: u8,
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
    pub fade_in: f64,
    pub fade_out: f64,
    /// Incoming-transition length, seconds. Absent on the wire for a peer that
    /// predates the field; `serde(default)` reads that as 0.0, which
    /// [`Clip::transition_len`] resolves to the default.
    #[serde(default)]
    pub transition_duration: f64,
    /// The ordered effects stack, carried verbatim so a whole-clip write
    /// (add / remove / reorder an effect) replays byte-identically.
    #[serde(default)]
    pub effects: Vec<crate::model::ClipEffect>,
    /// Geometry: crop, scale, rotation, position, anchor. Absent on the
    /// wire for a peer that predates it, which `serde(default)` reads as
    /// the identity aspect-fit — the placement that peer is already
    /// drawing, so the two do not diverge visually.
    #[serde(default)]
    pub transform: crate::model::Transform,
    /// Playback rate and direction. Defaults to normal speed.
    #[serde(default)]
    pub speed: crate::model::Speed,
    /// Tone curves. Absent on the wire for a peer that predates them,
    /// which `serde(default)` reads as the straight line — the ungraded
    /// picture that peer is already drawing.
    #[serde(default)]
    pub curves: crate::curve::Curve,
    /// Colour key. Absent on the wire for a peer that predates keying,
    /// which `serde(default)` reads as "not keyed" — the composite that
    /// peer is already drawing, so the two do not diverge.
    #[serde(default)]
    pub key: crate::model::ChromaKey,
    /// Shape mask, same story: absent means no mask.
    #[serde(default)]
    pub mask: crate::model::Mask,
    /// Keyframe curves, carried whole: adding, moving or deleting a key is
    /// a new clip write, so a curve edit replays as one atomic change.
    #[serde(default)]
    pub anim: crate::param::ClipAnimation,
    /// Clip-local markers.
    #[serde(default)]
    pub markers: Vec<crate::model::Marker>,
}

impl ClipData {
    pub fn from_clip(c: &Clip) -> Self {
        Self {
            id: c.id.to_string(),
            track: c.track.to_string(),
            media: c.media.to_string(),
            start: c.start,
            in_point: c.in_point,
            out_point: c.out_point,
            opacity: c.opacity,
            volume: c.volume,
            transition: c.transition.as_str().to_string(),
            palette: c.palette,
            brightness: c.filters.brightness,
            contrast: c.filters.contrast,
            saturation: c.filters.saturation,
            fade_in: c.fade_in,
            fade_out: c.fade_out,
            transition_duration: c.transition_duration,
            effects: c.effects.clone(),
            transform: c.transform,
            speed: c.speed,
            curves: c.curves.clone(),
            key: c.key,
            mask: c.mask,
            anim: c.anim.clone(),
            markers: c.markers.clone(),
        }
    }

    /// Reconstruct the model clip, or `None` when the payload is malformed (an
    /// unparseable id) so `apply` can no-op instead of fabricating a value,
    /// which would break replay determinism.
    fn to_clip(&self) -> Option<Clip> {
        Some(Clip {
            id: self.id.parse::<Uuid>().ok()?,
            track: self.track.parse::<Uuid>().ok()?,
            media: self.media.parse::<Uuid>().ok()?,
            start: self.start,
            in_point: self.in_point,
            out_point: self.out_point,
            opacity: self.opacity,
            volume: self.volume,
            transition: Transition::from_str(&self.transition),
            palette: self.palette,
            filters: Filters {
                brightness: self.brightness,
                contrast: self.contrast,
                saturation: self.saturation,
            },
            fade_in: self.fade_in,
            fade_out: self.fade_out,
            transition_duration: self.transition_duration,
            effects: self.effects.clone(),
            transform: self.transform,
            speed: self.speed,
            curves: self.curves.clone(),
            key: self.key,
            mask: self.mask,
            anim: self.anim.clone(),
            markers: self.markers.clone(),
        })
    }
}

/// A serializable snapshot of a track's scalar fields, in the document's shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackData {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub index: u32,
    pub muted: bool,
    pub locked: bool,
    /// Absent on the wire for a peer that predates solo; `serde(default)`
    /// makes that read as "not soloed" rather than failing the decode.
    #[serde(default)]
    pub solo: bool,
}

impl TrackData {
    pub fn from_track(t: &Track) -> Self {
        Self {
            id: t.id.to_string(),
            name: t.name.clone(),
            kind: t.kind.as_str().to_string(),
            index: t.index,
            muted: t.muted,
            locked: t.locked,
            solo: t.solo,
        }
    }

    fn to_track(&self) -> Option<Track> {
        Some(Track {
            id: self.id.parse::<Uuid>().ok()?,
            name: self.name.clone(),
            kind: TrackKind::from_str(&self.kind),
            index: self.index,
            muted: self.muted,
            locked: self.locked,
            solo: self.solo,
        })
    }
}

/// A serializable snapshot of a bin media item, in the document's shape. This is
/// the *reference* (name, source path, probed metadata) the CRDT replicates —
/// never the bytes, which hydrate on demand through the FUSE-media seam.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaData {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub palette: u8,
    pub path: String,
    pub hydration: String,
    /// A title's text and styling. `None` for every decoded source, and
    /// absent on the wire for a peer that predates titles — which reads as
    /// `None`, i.e. an ordinary reference.
    #[serde(default)]
    pub text: Option<crate::model::TextSource>,
    /// Whether a picture source also carries sound. Travels with the item so
    /// a collaborator's mixer finds the same audio ours does; absent in a
    /// payload written before the mixer could read it, hence `default`.
    #[serde(default)]
    pub has_audio: bool,
}

impl MediaData {
    pub fn from_media(m: &MediaItem) -> Self {
        Self {
            id: m.id.to_string(),
            name: m.name.clone(),
            kind: m.kind.as_str().to_string(),
            duration: m.duration,
            width: m.width,
            height: m.height,
            fps: m.fps,
            palette: m.palette,
            path: m.path.clone(),
            hydration: m.hydration.as_str().to_string(),
            text: m.text.clone(),
            has_audio: m.has_audio,
        }
    }

    fn to_media(&self) -> Option<MediaItem> {
        Some(MediaItem {
            has_audio: self.has_audio,
            id: self.id.parse::<Uuid>().ok()?,
            name: self.name.clone(),
            kind: MediaKind::from_str(&self.kind),
            duration: self.duration,
            width: self.width,
            height: self.height,
            fps: self.fps,
            palette: self.palette,
            path: self.path.clone(),
            hydration: crate::media::Hydration::from_str(&self.hydration),
            text: self.text.clone(),
        })
    }
}

/// A serializable snapshot of the project-level settings the document stores.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    /// Whether the size and rate above were chosen rather than defaulted.
    /// Absent on the wire for a peer that predates project settings, which
    /// `serde(default)` reads as "not chosen" — the state that peer is
    /// already in, so the two do not diverge.
    #[serde(default)]
    pub format_explicit: bool,
    /// Timeline in / out marks, in absolute seconds. Absent means unmarked,
    /// which is the whole timeline.
    #[serde(default)]
    pub mark_in: Option<f64>,
    #[serde(default)]
    pub mark_out: Option<f64>,
    /// Timeline markers, in absolute seconds. They live with the project
    /// settings because that is the document map they are stored in.
    #[serde(default)]
    pub markers: Vec<crate::model::Marker>,
}

impl ProjectMeta {
    pub fn from_project(p: &Project) -> Self {
        Self {
            name: p.name.clone(),
            fps: p.fps,
            width: p.width,
            height: p.height,
            format_explicit: p.format_explicit,
            mark_in: p.mark_in,
            mark_out: p.mark_out,
            markers: p.markers.clone(),
        }
    }

    fn to_project(&self) -> Project {
        let mut p = Project::new(&self.name);
        p.fps = self.fps;
        p.width = self.width;
        p.height = self.height;
        p.format_explicit = self.format_explicit;
        p.mark_in = self.mark_in;
        p.mark_out = self.mark_out;
        p.markers = self.markers.clone();
        p
    }
}

/// Every collaborative operation the editor exports on its timeline.
///
/// One `UpsertClip` covers create, move, trim and every per-clip field edit —
/// a whole-clip write carrying the final shape. The multi-clip operations
/// (`RippleDeleteClip`, `SplitClip`) carry every affected clip so they replay
/// atomically as a single change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum VideoEditorAction {
    /// Create or replace a clip's fields: covers add, move (a new `start`),
    /// trim (new `in`/`out`), grade, opacity/volume, fades, transition and
    /// palette — all are just a new whole-clip write.
    UpsertClip(ClipData),
    /// Delete a single clip, leaving any gap where it was.
    DeleteClip { id: String },
    /// Delete a clip and close the gap: `moved` carries the post-shift position
    /// of every later clip on the same track, resolved at authoring time so
    /// `apply` needs no recompute. One action, one undo boundary.
    RippleDeleteClip { id: String, moved: Vec<ClipData> },
    /// Split a clip in two: `left` is the shortened original, `right` the fresh
    /// tail (its id minted at authoring). Both halves carried so the split is a
    /// pure two-clip write with no RNG in `apply`.
    SplitClip { left: ClipData, right: ClipData },
    /// Create or replace a track's fields: covers add, rename, mute/lock and a
    /// reorder (a new `index`).
    UpsertTrack(TrackData),
    /// Delete a track and every clip on it (`clips` are the clip ids, resolved
    /// at authoring). One action, one undo boundary.
    DeleteTrack { id: String, clips: Vec<String> },
    /// Bring a bin media reference into the document (its source path + probed
    /// metadata). The bytes are external — decoded / hydrated outside the doc.
    /// Also covers a bin rename (a whole-media write with a new `name`).
    UpsertMedia(MediaData),
    /// Remove a bin media reference and every clip that placed it on the
    /// timeline (`clips` are the affected clip ids, resolved at authoring), so
    /// deleting a source can't leave dangling clips. One action, one undo
    /// boundary.
    RemoveMedia { id: String, clips: Vec<String> },
    /// Replace the project settings (name / fps / resolution).
    SetProjectMeta(ProjectMeta),
    /// Replace the **whole** project: settings, media bin, tracks and clips.
    ///
    /// This is what opening a project file does. It is an action rather than a
    /// side-door document swap for three reasons that all matter:
    ///
    ///   * **Undo.** Opening a file lands on one boundary, so Ctrl+Z after an
    ///     accidental Open puts the previous project back rather than leaving
    ///     the user staring at someone else's timeline.
    ///   * **Replication.** A collaborator's session follows into the project
    ///     that was opened, instead of the two peers silently diverging.
    ///   * **One write path.** The document is only ever mutated by
    ///     [`VideoDoc`], so a project that arrives from a file is stored
    ///     exactly like one that was built by hand.
    ///
    /// Everything absent from the payload is *removed*, which is the
    /// difference between opening a project and merging one — see
    /// [`ProjectData::to_project`].
    LoadProject(ProjectData),
}

/// A whole project, in the shape [`Action::apply`] can write without
/// re-deriving anything. Composed from the existing per-entity payloads so
/// there is exactly one definition of "a clip on the wire".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectData {
    pub meta: ProjectMeta,
    #[serde(default)]
    pub media: Vec<MediaData>,
    #[serde(default)]
    pub tracks: Vec<TrackData>,
    #[serde(default)]
    pub clips: Vec<ClipData>,
}

impl ProjectData {
    pub fn from_project(p: &Project) -> Self {
        Self {
            meta: ProjectMeta::from_project(p),
            media: p.media.iter().map(MediaData::from_media).collect(),
            tracks: p.tracks.iter().map(TrackData::from_track).collect(),
            clips: p.clips.iter().map(ClipData::from_clip).collect(),
        }
    }

    /// Rebuild the model. A payload entry that will not parse is skipped
    /// rather than failing the whole load, matching the determinism contract:
    /// a malformed field must not wedge a peer replaying the change.
    pub fn to_project(&self) -> Project {
        let mut p = self.meta.to_project();
        p.media = self.media.iter().filter_map(MediaData::to_media).collect();
        p.tracks = self.tracks.iter().filter_map(TrackData::to_track).collect();
        p.clips = self.clips.iter().filter_map(ClipData::to_clip).collect();
        p
    }
}

impl Action for VideoEditorAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            VideoEditorAction::UpsertClip(data) => {
                if let Some(c) = data.to_clip() {
                    VideoDoc::upsert_clip(doc, &c);
                }
            }
            VideoEditorAction::DeleteClip { id } => {
                if let Ok(uuid) = id.parse::<Uuid>() {
                    VideoDoc::remove_clip(doc, uuid);
                }
            }
            VideoEditorAction::RippleDeleteClip { id, moved } => {
                if let Ok(uuid) = id.parse::<Uuid>() {
                    VideoDoc::remove_clip(doc, uuid);
                }
                for m in moved {
                    if let Some(c) = m.to_clip() {
                        VideoDoc::upsert_clip(doc, &c);
                    }
                }
            }
            VideoEditorAction::SplitClip { left, right } => {
                if let Some(c) = left.to_clip() {
                    VideoDoc::upsert_clip(doc, &c);
                }
                if let Some(c) = right.to_clip() {
                    VideoDoc::upsert_clip(doc, &c);
                }
            }
            VideoEditorAction::UpsertTrack(data) => {
                if let Some(t) = data.to_track() {
                    VideoDoc::upsert_track(doc, &t);
                }
            }
            VideoEditorAction::DeleteTrack { id, clips } => {
                if let Ok(uuid) = id.parse::<Uuid>() {
                    VideoDoc::remove_track(doc, uuid);
                }
                for c in clips {
                    if let Ok(uuid) = c.parse::<Uuid>() {
                        VideoDoc::remove_clip(doc, uuid);
                    }
                }
            }
            VideoEditorAction::UpsertMedia(data) => {
                if let Some(m) = data.to_media() {
                    VideoDoc::upsert_media(doc, &m);
                }
            }
            VideoEditorAction::RemoveMedia { id, clips } => {
                if let Ok(uuid) = id.parse::<Uuid>() {
                    VideoDoc::remove_media(doc, uuid);
                }
                for c in clips {
                    if let Ok(uuid) = c.parse::<Uuid>() {
                        VideoDoc::remove_clip(doc, uuid);
                    }
                }
            }
            VideoEditorAction::SetProjectMeta(meta) => {
                VideoDoc::write_project_meta(doc, &meta.to_project());
            }
            VideoEditorAction::LoadProject(data) => {
                VideoDoc::replace_project(doc, &data.to_project());
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register the editor's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<VideoEditorAction>(VIDEO_EDITOR_ACTION_TYPE);
}

/// Encode any [`VideoEditorAction`] into the wire form the palette and the
/// on-screen affordances both dispatch — the single point that turns the
/// editor's actions into replayable envelopes.
pub fn encode(action: &VideoEditorAction) -> EncodedAction {
    EncodedAction::encode(VIDEO_EDITOR_ACTION_TYPE, action)
        .expect("VideoEditorAction is always serializable")
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-screen
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

pub fn upsert_clip_action(c: &Clip) -> VideoEditorAction {
    VideoEditorAction::UpsertClip(ClipData::from_clip(c))
}

pub fn delete_clip_action(id: &Uuid) -> VideoEditorAction {
    VideoEditorAction::DeleteClip { id: id.to_string() }
}

pub fn ripple_delete_action(id: &Uuid, moved: &[Clip]) -> VideoEditorAction {
    VideoEditorAction::RippleDeleteClip {
        id: id.to_string(),
        moved: moved.iter().map(ClipData::from_clip).collect(),
    }
}

pub fn split_clip_action(left: &Clip, right: &Clip) -> VideoEditorAction {
    VideoEditorAction::SplitClip {
        left: ClipData::from_clip(left),
        right: ClipData::from_clip(right),
    }
}

pub fn upsert_track_action(t: &Track) -> VideoEditorAction {
    VideoEditorAction::UpsertTrack(TrackData::from_track(t))
}

pub fn delete_track_action(id: &Uuid, clips: &[Uuid]) -> VideoEditorAction {
    VideoEditorAction::DeleteTrack {
        id: id.to_string(),
        clips: clips.iter().map(std::string::ToString::to_string).collect(),
    }
}

pub fn upsert_media_action(m: &MediaItem) -> VideoEditorAction {
    VideoEditorAction::UpsertMedia(MediaData::from_media(m))
}

pub fn remove_media_action(id: &Uuid, clips: &[Uuid]) -> VideoEditorAction {
    VideoEditorAction::RemoveMedia {
        id: id.to_string(),
        clips: clips.iter().map(std::string::ToString::to_string).collect(),
    }
}

pub fn set_project_meta_action(p: &Project) -> VideoEditorAction {
    VideoEditorAction::SetProjectMeta(ProjectMeta::from_project(p))
}

/// Replace the whole project — what opening a project file dispatches.
pub fn load_project_action(p: &Project) -> VideoEditorAction {
    VideoEditorAction::LoadProject(ProjectData::from_project(p))
}

// ===========================================================================
// Resource tree
// ===========================================================================

/// The presentation context the editor hands [`provide_resources`]: a snapshot
/// of the current project (the doc projection) plus the live playhead — state
/// that lives in the app, not the document. The playhead makes the split /
/// trim-to-playhead actions buildable from where the user actually is.
#[derive(Debug, Clone)]
pub struct VideoResourceCtx {
    pub project: Project,
    pub playhead: f64,
}

fn immediate(id: &str, label: &str, action: &VideoEditorAction) -> ActionSpec {
    ActionSpec::immediate(id, label, VIDEO_EDITOR_ACTION_TYPE, action)
        .expect("encode video-editor action")
}

/// Every spelling `EffectKind::from_str` accepts, in the exact form a caller
/// has to send it.
///
/// The declared set is the *parser's* vocabulary, not the shorter list the old
/// placeholder narrated: "greyscale" and "negative" have always been accepted,
/// and declaring a type must not quietly take away a name that worked. What it
/// does take away is the guess — an unknown name is now refused by name
/// instead of adding nothing and reporting success. Case folding goes with it:
/// binding matches exactly, so these are the spellings, and a caller reads
/// them rather than guessing at capitalisation.
const EFFECT_CHOICES: [&str; 11] = [
    "grayscale",
    "greyscale",
    "gray",
    "grey",
    "b&w",
    "sepia",
    "invert",
    "negative",
    "warm",
    "cool",
    "vignette",
];

/// Every spelling `parse_transition` accepts, on the same rule as
/// [`EFFECT_CHOICES`]: the placeholder listed five names, the parser takes
/// twelve, so the declaration has to be the parser's set.
const TRANSITION_CHOICES: [&str; 12] = [
    "cut",
    "none",
    "dissolve",
    "cross dissolve",
    "crossdissolve",
    "fade",
    "fade in",
    "fadein",
    "dip",
    "dip to black",
    "diptoblack",
    "wipe",
];

/// Parse a title colour: `#rrggbb` / `rrggbb`, or three comma-separated
/// `0..1` components. `None` for anything else, so a typo leaves the colour
/// alone rather than blanking a title to black.
fn parse_colour(s: &str) -> Option<[f64; 4]> {
    let s = s.trim();
    let hex = s.strip_prefix('#').unwrap_or(s);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some([
            ((v >> 16) & 0xff) as f64 / 255.0,
            ((v >> 8) & 0xff) as f64 / 255.0,
            (v & 0xff) as f64 / 255.0,
            1.0,
        ]);
    }
    let parts: Vec<f64> = s
        .split(',')
        .filter_map(|p| p.trim().parse::<f64>().ok())
        .collect();
    match parts.len() {
        3 => Some([parts[0], parts[1], parts[2], 1.0]),
        4 => Some([parts[0], parts[1], parts[2], parts[3]]),
        _ => None,
    }
}

/// Build the resource tree the editor currently has open: the project, its
/// tracks (top-to-bottom display order), and the clips on each track — every
/// document-mutating operation exported as an action.
pub fn provide_resources(ctx: &VideoResourceCtx) -> Vec<Resource> {
    let p = &ctx.project;
    let mut root = Resource::new(
        "video.project".to_string(),
        "video.project",
        if p.name.is_empty() {
            "Project".to_string()
        } else {
            p.name.clone()
        },
    );

    // Project-level: rename (settings live in the doc).
    {
        let snap = p.clone();
        root = root.with_action(ActionSpec::typed(
            "rename-project",
            "Rename project",
            "project name…",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "what the project is called",
            )],
            move |args: &ActionArgs| {
                let mut next = snap.clone();
                // Text is the one type the palette can always produce, so the
                // fallback is unreachable from either door; the name it
                // already has is the harmless answer if that ever changes.
                let name = args.text("name").unwrap_or(&next.name).trim().to_string();
                next.name = name;
                encode(&set_project_meta_action(&next))
            },
        ));
    }

    // Project-level: the format. A project shot vertically, square or in 4K
    // had no way to say so — the frame was fixed at 1920×1080 at 30 from the
    // moment it was created, and the export dialog's own pickers scale a
    // preset height by the *project's* aspect, so vertical footage came out
    // letterboxed into 16:9 with no exit.
    {
        let snap = p.clone();
        root = root.with_action(ActionSpec::typed(
            "set-frame-size",
            "Set frame size",
            "1080 1920",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required(
                    "width",
                    ParamType::Number,
                    "frame width in pixels; clamped to 16..8192 and rounded even",
                ),
                ActionParam::required(
                    "height",
                    ParamType::Number,
                    "frame height in pixels; clamped to 16..8192 and rounded even",
                ),
            ],
            move |args: &ActionArgs| {
                let mut next = snap.clone();
                // An unbound argument leaves that dimension as it is rather
                // than collapsing the frame: half a declared pair is still a
                // legible intent ("make it 1080 wide"), and a project with no
                // width is not one anyone can render.
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "clamped into u32 range by set_frame_size, and a \
                              negative or NaN dimension falls back to the current one"
                )]
                let px = |v: Option<f64>, current: u32| -> u32 {
                    match v {
                        Some(v) if v.is_finite() && v >= 1.0 => v.round() as u32,
                        _ => current,
                    }
                };
                let w = px(args.number("width"), next.width);
                let h = px(args.number("height"), next.height);
                next.set_frame_size(w, h);
                encode(&set_project_meta_action(&next))
            },
        ));
        let snap = p.clone();
        root = root.with_action(ActionSpec::typed(
            "set-frame-rate",
            "Set frame rate",
            "24",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "fps",
                ParamType::Number,
                "project frames per second; clamped to 1..240",
            )],
            move |args: &ActionArgs| {
                let mut next = snap.clone();
                // Clamped rather than refused, like every other ranged number
                // here; an unbound one keeps the rate the project has.
                next.set_fps(args.number("fps").unwrap_or(next.fps));
                encode(&set_project_meta_action(&next))
            },
        ));
    }

    // Project-level: the render range. The export loop used to run the whole
    // timeline unconditionally, so checking a ten-second fix meant
    // re-rendering everything.
    {
        let snap = p.clone();
        let at = ctx.playhead;
        let mut marked = snap.clone();
        marked.set_mark_in(at);
        root = root.with_action(immediate(
            "mark-in",
            "Mark in at playhead",
            &set_project_meta_action(&marked),
        ));
        let mut marked = snap.clone();
        marked.set_mark_out(at);
        root = root.with_action(immediate(
            "mark-out",
            "Mark out at playhead",
            &set_project_meta_action(&marked),
        ));
        if p.has_marks() {
            let mut cleared = snap.clone();
            cleared.clear_marks();
            root = root.with_action(immediate(
                "clear-marks",
                "Clear in / out marks",
                &set_project_meta_action(&cleared),
            ));
        }
        let snap = p.clone();
        root = root.with_action(ActionSpec::typed(
            "set-render-range",
            "Set render range",
            "0 10",
            ExecutionLocus::ClientLocal,
            vec![
                ActionParam::required(
                    "start",
                    ParamType::Number,
                    "seconds on the timeline where a render begins",
                ),
                ActionParam::required(
                    "end",
                    ParamType::Number,
                    "seconds where it ends; must be later than the start",
                ),
            ],
            move |args: &ActionArgs| {
                let mut next = snap.clone();
                // Set out first: `set_mark_in` drops an out mark that would
                // sit at or before it, and doing it the other way round would
                // throw away the value that was just asked for.
                if let Some(end) = args.number("end") {
                    next.set_mark_out(end);
                }
                if let Some(start) = args.number("start") {
                    next.set_mark_in(start);
                }
                encode(&set_project_meta_action(&next))
            },
        ));
    }

    // Project-level: a timeline marker at the playhead, named.
    {
        let snap = p.clone();
        let at = ctx.playhead;
        root = root.with_action(ActionSpec::typed(
            "add-marker",
            "Add marker at playhead",
            "Marker",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "what the marker is called; an empty name reads as “Marker”",
            )],
            move |args: &ActionArgs| {
                let mut next = snap.clone();
                let name = args.text("name").unwrap_or("").trim();
                let name = if name.is_empty() { "Marker" } else { name };
                next.add_marker(at, name, 1);
                encode(&set_project_meta_action(&next))
            },
        ));
        if let Some(m) = p.markers.iter().find(|m| (m.t - ctx.playhead).abs() < 0.25) {
            let mut cleared = p.clone();
            cleared.remove_marker(m.id);
            root = root.with_action(immediate(
                "remove-marker",
                &format!("Remove marker \u{201c}{}\u{201d}", m.name),
                &set_project_meta_action(&cleared),
            ));
        }
    }

    // Project-level: a title. It lands in the bin like any other source —
    // the id is minted here, at authoring time, so the action replays
    // byte-identically on a peer.
    {
        let palette = (p.media.len() as u8) % 7;
        root = root.with_action(ActionSpec::typed(
            "add-title",
            "Add title",
            "Title",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "text",
                ParamType::Text,
                "the words the title shows; empty reads as “Title”",
            )],
            move |args: &ActionArgs| {
                let words = args.text("text").unwrap_or("").trim();
                let content = if words.is_empty() { "Title" } else { words };
                let item = MediaItem {
                    id: Uuid::new_v4(),
                    name: content.lines().next().unwrap_or("Title").to_string(),
                    kind: MediaKind::Text,
                    // Long enough to be trimmed down rather than stretched.
                    duration: 5.0,
                    palette,
                    // A title has no bytes: nothing to hydrate, nothing to
                    // fetch, and it renders on every peer immediately.
                    path: String::new(),
                    hydration: crate::media::Hydration::Local,
                    text: Some(TextSource::new(content)),
                    ..Default::default()
                };
                encode(&upsert_media_action(&item))
            },
        ));
    }

    // Project-level: add a fresh empty track of either kind (index = current
    // max of that kind + 1) — the same whole-track write the timeline's
    // add-track buttons dispatch.
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let index = p
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.index)
            .max()
            .unwrap_or(0)
            + 1;
        let name = format!(
            "{}{index}",
            if kind == TrackKind::Video { "V" } else { "A" }
        );
        let track = Track {
            id: Uuid::new_v4(),
            name,
            kind,
            index,
            muted: false,
            locked: false,
            solo: false,
        };
        let (id, label) = if kind == TrackKind::Video {
            ("add-video-track", "Add video track")
        } else {
            ("add-audio-track", "Add audio track")
        };
        root = root.with_action(immediate(id, label, &upsert_track_action(&track)));
    }

    for track in p.display_tracks() {
        let tid = track.id;
        let mut tr = Resource::new(
            format!("video.track:{tid}"),
            "video.track",
            track.name.clone(),
        );

        // Track-level: rename, toggle mute (deterministic flip against the
        // snapshot), delete (with its clips).
        let rename_snap = track.clone();
        tr = tr.with_action(ActionSpec::typed(
            "rename-track",
            "Rename track",
            "track name…",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "what the track is called in the timeline",
            )],
            move |args: &ActionArgs| {
                let mut next = rename_snap.clone();
                let name = args.text("name").unwrap_or(&next.name).trim().to_string();
                next.name = name;
                encode(&upsert_track_action(&next))
            },
        ));
        let mut muted = track.clone();
        muted.muted = !muted.muted;
        tr = tr.with_action(immediate(
            "toggle-mute",
            if track.muted {
                "Unmute track"
            } else {
                "Mute track"
            },
            &upsert_track_action(&muted),
        ));
        let track_clip_ids: Vec<Uuid> = p.clips_on(tid).iter().map(|c| c.id).collect();
        tr = tr.with_action(immediate(
            "delete-track",
            "Delete track",
            &delete_track_action(&tid, &track_clip_ids),
        ));

        for clip in p.clips_on(tid) {
            let cid = clip.id;
            let label = p
                .media(clip.media)
                .map_or_else(|| format!("Clip {cid}"), |m| m.name.clone());
            let mut cr = Resource::new(format!("video.clip:{cid}"), "video.clip", label);

            // Delete + ripple-delete.
            cr = cr.with_action(immediate(
                "delete",
                "Delete clip",
                &delete_clip_action(&cid),
            ));
            let mut ripple_project = p.clone();
            let moved_ids = ripple_project.ripple_delete(cid);
            let moved: Vec<Clip> = moved_ids
                .iter()
                .filter_map(|id| ripple_project.clip(*id).cloned())
                .collect();
            cr = cr.with_action(immediate(
                "ripple-delete",
                "Ripple delete (close gap)",
                &ripple_delete_action(&cid, &moved),
            ));

            // Split at the playhead — only when the playhead is strictly inside.
            let mut split_project = p.clone();
            if let Some(new_id) = split_project.split_clip(cid, ctx.playhead)
                && let (Some(left), Some(right)) =
                    (split_project.clip(cid), split_project.clip(new_id))
            {
                cr = cr.with_action(immediate(
                    "split",
                    "Split at playhead",
                    &split_clip_action(left, right),
                ));
            }

            // Trim in / out to the playhead — only when it lies within the span.
            if clip.covers(ctx.playhead) {
                let mut ti = p.clone();
                let din = ctx.playhead - clip.start;
                ti.trim_in(cid, din);
                if let Some(c) = ti.clip(cid) {
                    cr = cr.with_action(immediate(
                        "trim-in",
                        "Trim in to playhead",
                        &upsert_clip_action(c),
                    ));
                }
                let mut to = p.clone();
                let dout = ctx.playhead - clip.end();
                to.trim_out(cid, dout);
                if let Some(c) = to.clip(cid) {
                    cr = cr.with_action(immediate(
                        "trim-out",
                        "Trim out to playhead",
                        &upsert_clip_action(c),
                    ));
                }
            }

            // Colour grade — prompted `0..1` sliders folded into a whole-clip
            // write.
            for (id, label, field) in [
                ("set-brightness", "Set brightness", FilterAxis::Brightness),
                ("set-contrast", "Set contrast", FilterAxis::Contrast),
                ("set-saturation", "Set saturation", FilterAxis::Saturation),
            ] {
                let base = clip.clone();
                // Typed rather than prompted: `parse_unit` answered 0.5 for
                // anything it could not read, so `set-brightness banana`
                // silently made a real, plausible-looking edit instead of
                // being refused. A `Number` parameter is checked before the
                // document is touched, and a caller can *read* the type
                // rather than parse "Value 0–1" out of a placeholder.
                cr = cr.with_action(ActionSpec::typed(
                    id,
                    label,
                    "0.5",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "value",
                        ParamType::Number,
                        "0 to 1; 0.5 leaves the picture as shot",
                    )],
                    move |args: &ActionArgs| {
                        // Out of range is clamped rather than refused: 1.4 is
                        // a legible intent ("as bright as it goes"), unlike a
                        // word, which now never reaches here.
                        let v = args.number("value").unwrap_or(0.5).clamp(0.0, 1.0);
                        let mut next = base.clone();
                        match field {
                            FilterAxis::Brightness => next.filters.brightness = v,
                            FilterAxis::Contrast => next.filters.contrast = v,
                            FilterAxis::Saturation => next.filters.saturation = v,
                        }
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }

            // Level: opacity and volume. Both had a slider in the inspector
            // and no action at all, so the one thing an automation or an
            // agent most often wants of a clip — turn it down — was
            // reachable only by dragging.
            for (id, label, axis) in [
                ("set-opacity", "Set opacity", LevelAxis::Opacity),
                ("set-volume", "Set volume", LevelAxis::Volume),
            ] {
                let base = clip.clone();
                cr = cr.with_action(ActionSpec::typed(
                    id,
                    label,
                    "0.5",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "value",
                        ParamType::Number,
                        "0 to 1; 1 is full, 0 is silent or invisible",
                    )],
                    move |args: &ActionArgs| {
                        let mut next = base.clone();
                        let current = match axis {
                            LevelAxis::Opacity => next.opacity,
                            LevelAxis::Volume => next.volume,
                        };
                        let v = args.number("value").unwrap_or(current).clamp(0.0, 1.0);
                        match axis {
                            LevelAxis::Opacity => next.opacity = v,
                            LevelAxis::Volume => next.volume = v,
                        }
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }

            // Fades, in seconds and clamped to the clip: a fade longer than
            // the clip it is on is not a fade.
            for (id, label, head) in [
                ("set-fade-in", "Set fade in", true),
                ("set-fade-out", "Set fade out", false),
            ] {
                let base = clip.clone();
                cr = cr.with_action(ActionSpec::typed(
                    id,
                    label,
                    "1",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "seconds",
                        ParamType::Number,
                        "how long the ramp runs, in seconds; 0 removes it",
                    )],
                    move |args: &ActionArgs| {
                        let mut next = base.clone();
                        let current = if head { next.fade_in } else { next.fade_out };
                        let max = next.duration();
                        let v = args.number("seconds").unwrap_or(current).clamp(0.0, max);
                        if head {
                            next.fade_in = v;
                        } else {
                            next.fade_out = v;
                        }
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }

            // The chroma key's switch, and the mask's shape. Both are the
            // *gate* on a set of sliders that only exist once they are on, so
            // an action surface without them could describe the sliders and
            // never reach them.
            let mut keyed = clip.clone();
            keyed.key.enabled = !keyed.key.enabled;
            cr = cr.with_action(immediate(
                "toggle-key",
                if clip.key.enabled {
                    "Switch chroma key off"
                } else {
                    "Switch chroma key on"
                },
                &upsert_clip_action(&keyed),
            ));
            let mask_base = clip.clone();
            cr = cr.with_action(ActionSpec::typed(
                "set-mask-shape",
                "Set mask shape",
                "ellipse",
                ExecutionLocus::ClientLocal,
                vec![
                    ActionParam::required(
                        "shape",
                        ParamType::Text,
                        "the shape the mask cuts; \u{201c}none\u{201d} removes it",
                    )
                    .with_choices(crate::model::MaskShape::ALL.map(MaskShape::as_str)),
                ],
                move |args: &ActionArgs| {
                    let mut next = mask_base.clone();
                    // `MaskShape::from_str` reads everything it does not know
                    // as `None` — which would silently *remove* a mask — so
                    // the vocabulary is declared and an unbound value leaves
                    // the shape alone.
                    if let Some(name) = args.text("shape") {
                        next.mask.shape = MaskShape::from_str(name);
                    }
                    encode(&upsert_clip_action(&next))
                },
            ));

            // The nine knobs behind that switch and that shape: the key's
            // tolerance, softness and spill, and the mask's own geometry. Each
            // had an inspector slider and an `Increment`/`Decrement` for a
            // reader, and no action — so "switch the key on" was addressable by
            // name and "key 30% of the plate" was not, which leaves the whole
            // section reachable and unusable to a script.
            //
            // The parameter names follow the ones already here rather than a
            // generic "value" each: a `0..1` amount is `value` like the grade,
            // an offset from centre is `offset` like `set-pos-x`, an extent is
            // `fraction` like the crops, and a tilt is `degrees` like
            // `set-rotation`.
            for (id, label, axis, param, description) in [
                (
                    "set-key-tolerance",
                    "Set key tolerance",
                    KeyMaskAxis::Tolerance,
                    "value",
                    "how wide a band of chroma is removed, 0 to 1; 0 keys nothing",
                ),
                (
                    "set-key-softness",
                    "Set key softness",
                    KeyMaskAxis::Softness,
                    "value",
                    "width of the soft edge past the tolerance, 0 to 1",
                ),
                (
                    "set-key-spill",
                    "Set key spill removal",
                    KeyMaskAxis::Spill,
                    "value",
                    "how much of the key colour is pulled back out of what survives, 0 to 1",
                ),
                (
                    "set-mask-x",
                    "Set mask centre X",
                    KeyMaskAxis::MaskX,
                    "offset",
                    "mask centre right of the frame centre, in frame widths",
                ),
                (
                    "set-mask-y",
                    "Set mask centre Y",
                    KeyMaskAxis::MaskY,
                    "offset",
                    "mask centre below the frame centre, in frame heights",
                ),
                (
                    "set-mask-width",
                    "Set mask width",
                    KeyMaskAxis::MaskW,
                    "fraction",
                    "full width of the shape as a fraction of the frame; 1 spans it",
                ),
                (
                    "set-mask-height",
                    "Set mask height",
                    KeyMaskAxis::MaskH,
                    "fraction",
                    "full height of the shape as a fraction of the frame; 1 spans it",
                ),
                (
                    "set-mask-rotation",
                    "Set mask rotation (degrees)",
                    KeyMaskAxis::MaskRotation,
                    "degrees",
                    "clockwise rotation of the shape",
                ),
                (
                    "set-mask-feather",
                    "Set mask feather",
                    KeyMaskAxis::MaskFeather,
                    "value",
                    "softness of the edge, 0 to 1 of the shape's own half-extent",
                ),
            ] {
                let base = clip.clone();
                cr = cr.with_action(ActionSpec::typed(
                    id,
                    label,
                    match axis {
                        KeyMaskAxis::MaskRotation => "15",
                        KeyMaskAxis::MaskX | KeyMaskAxis::MaskY => "0.25",
                        _ => "0.5",
                    },
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(param, ParamType::Number, description)],
                    move |args: &ActionArgs| {
                        let mut next = base.clone();
                        // Declared as a number, so a word is refused before the
                        // document is touched rather than read as some
                        // plausible middle value. An argument that did not bind
                        // at all leaves the knob where it was.
                        let v = args.number(param).unwrap_or_else(|| axis.get(&next));
                        axis.set(&mut next, v);
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }

            // Effects stack — add one by name, or clear them all.
            let add_base = clip.clone();
            // The vocabulary is declared, not narrated. `EffectKind::from_str`
            // answers `None` for a name it does not know, which the old
            // prompt turned into "the action ran and added nothing" — a
            // typo that reported success. Binding refuses it by name now.
            cr = cr.with_action(ActionSpec::typed(
                "add-effect",
                "Add effect",
                "grayscale",
                ExecutionLocus::ClientLocal,
                vec![
                    ActionParam::required(
                        "effect",
                        ParamType::Text,
                        "which effect to append to the clip's stack",
                    )
                    .with_choices(EFFECT_CHOICES),
                ],
                move |args: &ActionArgs| {
                    let mut next = add_base.clone();
                    // A bound value is one of `EFFECT_CHOICES`, so `from_str`
                    // cannot fail here; an unbound one adds nothing.
                    if let Some(k) = args.text("effect").and_then(EffectKind::from_str) {
                        next.add_effect(k);
                    }
                    encode(&upsert_clip_action(&next))
                },
            ));
            if !clip.effects.is_empty() {
                let mut cleared = clip.clone();
                cleared.effects.clear();
                cr = cr.with_action(immediate(
                    "clear-effects",
                    "Clear effects",
                    &upsert_clip_action(&cleared),
                ));
            }

            // Speed. The rate is prompted; reverse is a toggle, because
            // "play it backwards" is a decision, not a number.
            let speed_base = clip.clone();
            cr = cr.with_action(ActionSpec::typed(
                "set-speed",
                "Set speed",
                "2",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "rate",
                    ParamType::Number,
                    "playback rate: 1 is normal, 0.5 half speed, 2 double",
                )],
                move |args: &ActionArgs| {
                    let mut next = speed_base.clone();
                    // Out of range is still clamped, by `Speed::new` — asking
                    // for 100× is a legible "as fast as it goes". Unreadable
                    // is no longer 1×: a word cannot bind to a number, so a
                    // slowed clip stays slowed instead of silently snapping
                    // back to normal speed. The `2x` spelling goes with it —
                    // the suffix was prose wrapped around the number, and the
                    // number is what is declared.
                    let rate = args.number("rate").unwrap_or(next.speed.rate());
                    next.set_speed(rate, next.speed.reverse);
                    encode(&upsert_clip_action(&next))
                },
            ));
            let mut reversed = clip.clone();
            reversed.set_speed(clip.speed.rate(), !clip.speed.reverse);
            cr = cr.with_action(immediate(
                "toggle-reverse",
                if clip.speed.reverse {
                    "Play forwards"
                } else {
                    "Play backwards"
                },
                &upsert_clip_action(&reversed),
            ));
            if !clip.speed.is_normal() {
                let mut normal = clip.clone();
                normal.set_speed(1.0, false);
                cr = cr.with_action(immediate(
                    "reset-speed",
                    "Reset speed to normal",
                    &upsert_clip_action(&normal),
                ));
            }

            // Geometry. The corner insets are the shape people actually
            // want; anything finer is a prompted scale or position.
            for (id, label, corner) in [
                ("pip-tr", "Inset top-right", (1.0, -1.0)),
                ("pip-tl", "Inset top-left", (-1.0, -1.0)),
                ("pip-br", "Inset bottom-right", (1.0, 1.0)),
                ("pip-bl", "Inset bottom-left", (-1.0, 1.0)),
            ] {
                let mut inset = clip.clone();
                inset.transform = crate::model::Transform::pip(0.3, corner, 0.04);
                cr = cr.with_action(immediate(id, label, &upsert_clip_action(&inset)));
            }
            for (id, label, axis) in [
                ("set-scale", "Set scale", GeomAxis::Scale),
                ("set-pos-x", "Set position X", GeomAxis::PosX),
                ("set-pos-y", "Set position Y", GeomAxis::PosY),
                ("set-rotation", "Set rotation (degrees)", GeomAxis::Rotation),
            ] {
                let base = clip.clone();
                // Every geometry channel is a real number on `Transform` —
                // degrees included, because a 12.5° tilt is an ordinary
                // value — so each declares `Number` under the name of the
                // quantity it carries rather than a generic "value". The old
                // prompt read an unparseable word as `0.0`, which for scale
                // meant the picture silently disappeared.
                let (param, description) = match axis {
                    GeomAxis::Scale => (
                        "factor",
                        "size relative to the fitted picture; 1 leaves it as placed",
                    ),
                    GeomAxis::Rotation => ("degrees", "clockwise rotation about the anchor"),
                    GeomAxis::PosX => ("offset", "offset right of centre, in frame widths"),
                    GeomAxis::PosY => ("offset", "offset below centre, in frame heights"),
                };
                cr = cr.with_action(ActionSpec::typed(
                    id,
                    label,
                    match axis {
                        GeomAxis::Scale => "0.5",
                        GeomAxis::Rotation => "15",
                        _ => "0.25",
                    },
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(param, ParamType::Number, description)],
                    move |args: &ActionArgs| {
                        let mut next = base.clone();
                        match axis {
                            GeomAxis::Scale => {
                                let v = args.number("factor").unwrap_or(next.transform.scale_x);
                                next.transform.scale_x = v;
                                next.transform.scale_y = v;
                            }
                            GeomAxis::PosX => {
                                next.transform.pos_x =
                                    args.number("offset").unwrap_or(next.transform.pos_x);
                            }
                            GeomAxis::PosY => {
                                next.transform.pos_y =
                                    args.number("offset").unwrap_or(next.transform.pos_y);
                            }
                            GeomAxis::Rotation => {
                                next.transform.rotation =
                                    args.number("degrees").unwrap_or(next.transform.rotation);
                            }
                        }
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }
            // Crop, one edge at a time. The inspector offers these as four
            // sliders; the palette takes the same fraction as a number.
            for (id, label, edge) in [
                ("crop-left", "Crop left edge", CropEdge::Left),
                ("crop-right", "Crop right edge", CropEdge::Right),
                ("crop-top", "Crop top edge", CropEdge::Top),
                ("crop-bottom", "Crop bottom edge", CropEdge::Bottom),
            ] {
                let base = clip.clone();
                cr = cr.with_action(ActionSpec::typed(
                    id,
                    label,
                    "0.1",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "fraction",
                        ParamType::Number,
                        "how much of this edge to cut away, 0 to 1",
                    )],
                    move |args: &ActionArgs| {
                        let mut next = base.clone();
                        let cut = match edge {
                            CropEdge::Left => &mut next.transform.crop_left,
                            CropEdge::Right => &mut next.transform.crop_right,
                            CropEdge::Top => &mut next.transform.crop_top,
                            CropEdge::Bottom => &mut next.transform.crop_bottom,
                        };
                        // Half of `parse_unit` survives: 1.4 still clamps to
                        // "all of it", which is a legible intent. The other
                        // half — answering 0.5 for a word, a real crop nobody
                        // asked for — is unreachable now that the parameter
                        // is a declared number, so the edge keeps its value.
                        *cut = args.number("fraction").unwrap_or(*cut).clamp(0.0, 1.0);
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }
            if !clip.transform.is_identity() {
                let mut flat = clip.clone();
                flat.transform = crate::model::Transform::default();
                cr = cr.with_action(immediate(
                    "reset-transform",
                    "Reset position and scale",
                    &upsert_clip_action(&flat),
                ));
            }

            // Keyframes, at the playhead. Keying a parameter plants it at
            // whatever it reads right now, so the first key never moves the
            // picture — the second one is what makes it animate.
            if clip.covers(ctx.playhead) {
                let at = ctx.playhead;
                for param in Param::SIMPLE {
                    let base = clip.clone();
                    let Some(now) = base.param_const(param) else {
                        continue;
                    };
                    let mut keyed = base.clone();
                    let current = keyed.param_at(param, at);
                    keyed.set_key_at(param, at, current, Interp::Linear);
                    cr = cr.with_action(immediate(
                        &format!("key-{}", param.as_str()),
                        &format!("Key {} here ({now:.2})", param.label().to_lowercase()),
                        &upsert_clip_action(&keyed),
                    ));
                }
                for param in clip.anim.animated_params() {
                    let mut cleared = clip.clone();
                    cleared.anim.clear(param);
                    cr = cr.with_action(immediate(
                        &format!("unkey-{}", param.as_str()),
                        &format!("Remove {} animation", param.label().to_lowercase()),
                        &upsert_clip_action(&cleared),
                    ));
                }
                // A marker on the clip, named.
                let marker_base = clip.clone();
                cr = cr.with_action(ActionSpec::typed(
                    "add-clip-marker",
                    "Add marker on this clip",
                    "Marker",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "name",
                        ParamType::Text,
                        "what the marker is called; an empty name reads as “Marker”",
                    )],
                    move |args: &ActionArgs| {
                        let mut next = marker_base.clone();
                        let name = args.text("name").unwrap_or("").trim();
                        let name = if name.is_empty() { "Marker" } else { name };
                        next.add_marker(at, name, next.palette);
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }

            // Transition at the head cut — prompted by name.
            let base = clip.clone();
            cr = cr.with_action(ActionSpec::typed(
                "set-transition",
                "Set transition",
                "dissolve",
                ExecutionLocus::ClientLocal,
                vec![
                    ActionParam::required(
                        "transition",
                        ParamType::Text,
                        "the edit-point transition at this clip's head",
                    )
                    .with_choices(TRANSITION_CHOICES),
                ],
                move |args: &ActionArgs| {
                    let mut next = base.clone();
                    // `parse_transition` falls through to a hard cut for a
                    // name it does not know, so "swipe" used to silently
                    // remove the dissolve that was there. Declaring the
                    // vocabulary puts that branch out of reach of a bound
                    // value, and an unbound one leaves the cut alone.
                    if let Some(name) = args.text("transition") {
                        next.transition = parse_transition(name);
                    }
                    encode(&upsert_clip_action(&next))
                },
            ));

            tr = tr.with_child(cr);
        }
        root = root.with_child(tr);
    }

    // Media bin: each source exports rename + delete-from-bin (delete carries
    // the clips that placed it, so the palette path matches the on-screen
    // context menu). Appended after the tracks so track ordering is stable.
    let mut bin_media: Vec<&MediaItem> = p.media.iter().collect();
    bin_media.sort_by(|a, b| a.name.cmp(&b.name));
    for m in bin_media {
        let mid = m.id;
        let mut mr = Resource::new(format!("video.media:{mid}"), "video.media", m.name.clone());
        let rename_snap = m.clone();
        if let Some(text) = m.text.clone() {
            let base = m.clone();
            mr = mr.with_action(ActionSpec::typed(
                "set-title-text",
                "Set title text",
                &text.content,
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "text",
                    ParamType::Text,
                    "the words the title shows; its first line names it in the bin",
                )],
                move |args: &ActionArgs| {
                    let mut next = base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    let words = args.text("text").unwrap_or(&t.content).to_string();
                    next.name = words.lines().next().unwrap_or("Title").to_string();
                    t.content = words;
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            let size_base = m.clone();
            mr = mr.with_action(ActionSpec::typed(
                "set-title-size",
                "Set title size",
                "0.12",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "size",
                    ParamType::Number,
                    "cap height as a fraction of the frame height",
                )],
                move |args: &ActionArgs| {
                    let mut next = size_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    // This one never invented a size — an unreadable one left
                    // it alone — and that is exactly what an unbound argument
                    // still means.
                    t.size = args.number("size").unwrap_or(t.size);
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            let align_base = m.clone();
            mr = mr.with_action(ActionSpec::typed(
                "set-title-align",
                "Set title alignment",
                "center",
                ExecutionLocus::ClientLocal,
                vec![
                    ActionParam::required(
                        "align",
                        ParamType::Text,
                        "where the title's lines sit in the frame",
                    )
                    .with_choices(crate::model::TextAlign::ALL.map(|a| a.as_str())),
                ],
                move |args: &ActionArgs| {
                    let mut next = align_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    // `TextAlign::from_str` reads *everything* it does not
                    // recognise as `Center`, so "centre" and "banana" were
                    // both a silent re-centring. The three declared names are
                    // the only ones that can reach it now.
                    if let Some(name) = args.text("align") {
                        t.align = crate::model::TextAlign::from_str(name);
                    }
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            // The title's colour, as `#rrggbb` or `r,g,b` in 0..1. Stored as
            // a real value rather than a palette slot: a title's colour is
            // authored content and has to survive a theme change.
            let color_base = m.clone();
            mr = mr.with_action(ActionSpec::typed(
                "set-title-colour",
                "Set title colour",
                "#ffffff",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "colour",
                    ParamType::Text,
                    "#rrggbb, or r,g,b (optionally a) components in 0..1",
                )],
                move |args: &ActionArgs| {
                    let mut next = color_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    // Text with no choices, because a colour is not an
                    // enumeration — and `parse_colour`'s leniency stays: it
                    // answers `None` rather than a colour it guessed, so a
                    // typo leaves the title as it was instead of blanking it
                    // to black.
                    if let Some(rgb) = args.text("colour").and_then(parse_colour) {
                        t.color = rgb;
                    }
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            let backdrop_base = m.clone();
            mr = mr.with_action(ActionSpec::typed(
                "set-title-backdrop",
                "Set title backdrop",
                "0.3",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "opacity",
                    ParamType::Number,
                    "opacity of the plate behind the words, 0 to 1",
                )],
                move |args: &ActionArgs| {
                    let mut next = backdrop_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    // Clamped, not guessed — the other half of `parse_unit`.
                    t.backdrop = args.number("opacity").unwrap_or(t.backdrop).clamp(0.0, 1.0);
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            for (id, bold) in [("toggle-title-bold", true), ("toggle-title-italic", false)] {
                let base = m.clone();
                let on = if bold { text.bold } else { text.italic };
                let label = match (bold, on) {
                    (true, true) => "Unset bold",
                    (true, false) => "Set bold",
                    (false, true) => "Unset italic",
                    (false, false) => "Set italic",
                };
                let mut next = base.clone();
                let mut t = next.text.clone().unwrap_or_default();
                if bold {
                    t.bold = !t.bold;
                } else {
                    t.italic = !t.italic;
                }
                next.text = Some(t);
                mr = mr.with_action(immediate(id, label, &upsert_media_action(&next)));
            }
        }

        mr = mr.with_action(ActionSpec::typed(
            "rename-media",
            "Rename media",
            "clip name…",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "name",
                ParamType::Text,
                "what this source is called in the bin",
            )],
            move |args: &ActionArgs| {
                let mut next = rename_snap.clone();
                let name = args.text("name").unwrap_or(&next.name).trim().to_string();
                next.name = name;
                encode(&upsert_media_action(&next))
            },
        ));
        // Take this source's shape as the project's. Offered only for a
        // source that has one — audio and titles carry no frame — so the
        // action is absent rather than present and inert.
        if m.width > 0 && m.height > 0 {
            let mut next = p.clone();
            next.set_frame_size(m.width, m.height);
            if m.fps > 0.0 {
                next.set_fps(m.fps);
            }
            mr = mr.with_action(immediate(
                "use-as-project-format",
                "Use this shape for the project",
                &set_project_meta_action(&next),
            ));
        }
        let clip_ids: Vec<Uuid> = p
            .clips
            .iter()
            .filter(|c| c.media == mid)
            .map(|c| c.id)
            .collect();
        mr = mr.with_action(immediate(
            "delete-media",
            "Delete from bin",
            &remove_media_action(&mid, &clip_ids),
        ));
        root = root.with_child(mr);
    }

    vec![root]
}

#[derive(Clone, Copy)]
enum FilterAxis {
    Brightness,
    Contrast,
    Saturation,
}

/// Which `0..1` level a prompted action drives. Not a `FilterAxis`: neither
/// is a grade — one is how much of the picture is composited, the other how
/// loud it is.
#[derive(Clone, Copy)]
enum LevelAxis {
    Opacity,
    Volume,
}

/// Which geometric channel a prompted transform action drives.
#[derive(Clone, Copy)]
enum GeomAxis {
    Scale,
    PosX,
    PosY,
    Rotation,
}

/// One of the nine knobs the chroma key and the mask carry.
///
/// These reached an assistive technology through `Increment`/`Decrement` on
/// their inspector rows and had no action at all, so a script could switch a
/// key on by name and then had no way to say how much of the plate to take
/// out. The two blocks share an enum because they share a shape: a named
/// scalar on the selected clip, folded into a whole-clip write.
#[derive(Clone, Copy)]
enum KeyMaskAxis {
    Tolerance,
    Softness,
    Spill,
    MaskX,
    MaskY,
    MaskW,
    MaskH,
    MaskRotation,
    MaskFeather,
}

impl KeyMaskAxis {
    /// Read the knob off a clip, so an unbound argument leaves it where it is
    /// rather than answering with a plausible-looking default.
    fn get(self, c: &Clip) -> f64 {
        match self {
            KeyMaskAxis::Tolerance => c.key.tolerance,
            KeyMaskAxis::Softness => c.key.softness,
            KeyMaskAxis::Spill => c.key.spill,
            KeyMaskAxis::MaskX => c.mask.x,
            KeyMaskAxis::MaskY => c.mask.y,
            KeyMaskAxis::MaskW => c.mask.w,
            KeyMaskAxis::MaskH => c.mask.h,
            KeyMaskAxis::MaskRotation => c.mask.rotation,
            KeyMaskAxis::MaskFeather => c.mask.feather,
        }
    }

    /// Write it, clamped to what the value can legibly mean.
    ///
    /// Out of range is clamped rather than refused wherever the far stop is a
    /// real intent — "take out everything on this side of the wheel" is 1, and
    /// asking for 1.4 says that. It is *not* clamped where the model has a
    /// reason to allow more: a mask may sit past the frame edge (that is how an
    /// edge is put outside the picture instead of across it) and degrees wrap.
    fn set(self, c: &mut Clip, v: f64) {
        match self {
            KeyMaskAxis::Tolerance => c.key.tolerance = v.clamp(0.0, 1.0),
            KeyMaskAxis::Softness => c.key.softness = v.clamp(0.0, 1.0),
            KeyMaskAxis::Spill => c.key.spill = v.clamp(0.0, 1.0),
            KeyMaskAxis::MaskX => c.mask.x = v,
            KeyMaskAxis::MaskY => c.mask.y = v,
            // A negative extent is not a shape, so zero is the floor; there is
            // no ceiling, because a mask wider than the frame is how a soft
            // edge is kept out of the picture.
            KeyMaskAxis::MaskW => c.mask.w = v.max(0.0),
            KeyMaskAxis::MaskH => c.mask.h = v.max(0.0),
            KeyMaskAxis::MaskRotation => c.mask.rotation = v,
            KeyMaskAxis::MaskFeather => c.mask.feather = v.clamp(0.0, 1.0),
        }
    }
}

/// Which edge a prompted crop action cuts.
#[derive(Clone, Copy)]
enum CropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Map a palette transition name to the stored [`Transition`], tolerant of the
/// on-screen labels ("Cut", "Dissolve", …) as well as the stored tags.
fn parse_transition(s: &str) -> Transition {
    match s.trim().to_lowercase().as_str() {
        "cut" | "none" => Transition::None,
        "dissolve" | "crossdissolve" | "cross dissolve" => Transition::CrossDissolve,
        "fade" | "fadein" | "fade in" => Transition::FadeIn,
        "dip" | "diptoblack" | "dip to black" => Transition::DipToBlack,
        "wipe" => Transition::Wipe,
        other => Transition::from_str(other),
    }
}

impl ResourceProvider for crate::app::VideoEditorApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<Resource> {
        match self.video_resource_ctx() {
            Some(ctx) => provide_resources(&ctx),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {

    /// Every action the palette offers for `ctx`, flattened with its
    /// resource, so a test can look one up by id.
    fn all_specs(ctx: &VideoResourceCtx) -> Vec<(String, ActionSpec)> {
        fn walk(r: &Resource, out: &mut Vec<(String, ActionSpec)>) {
            for a in &r.actions {
                out.push((a.id.clone(), a.clone()));
            }
            for c in &r.children {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        for r in provide_resources(ctx) {
            walk(&r, &mut out);
        }
        out
    }

    fn spec(ctx: &VideoResourceCtx, id: &str) -> ActionSpec {
        all_specs(ctx)
            .into_iter()
            .find(|(i, _)| i == id)
            .unwrap_or_else(|| {
                panic!(
                    "the palette offers {id}: {:?}",
                    all_specs(ctx)
                        .iter()
                        .map(|(i, _)| i.clone())
                        .collect::<Vec<_>>()
                )
            })
            .1
    }

    /// Apply `spec` (immediate or prompted with `arg`) to a document seeded
    /// from `p`, and read the project back — the whole replay path.
    fn run(p: &Project, spec: &ActionSpec, arg: Option<&str>) -> Project {
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, p);
        let encoded = match arg {
            Some(a) => prompted_encode(spec, a),
            None => immediate_encode(spec),
        };
        let action: VideoEditorAction =
            serde_json::from_slice(&encoded.payload).expect("payload decodes");
        action.apply(&mut doc).expect("applies");
        VideoDoc::load_project(&doc)
    }

    /// A project with one clip under the playhead.
    fn ctx_fixture() -> (VideoResourceCtx, Uuid) {
        let mut p = Project::new("act");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "v.mp4".into(),
            kind: MediaKind::Video,
            duration: 30.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            path: "media/v.mp4".into(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
        (
            VideoResourceCtx {
                project: p,
                playhead: 4.0,
            },
            cid,
        )
    }

    #[test]
    fn opening_a_project_replays_as_one_action_that_replaces_everything() {
        // Opening a file is an action, so it undoes and replicates. That is
        // only true if the *encoded* form carries the whole project — a
        // payload that dropped the media bin would replay on a peer as a
        // timeline of clips with no sources.
        let (ctx, _cid) = ctx_fixture();
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, &ctx.project);

        let mut opened = Project::new("Opened");
        opened.fps = 24.0;
        let mid = Uuid::new_v4();
        opened.media.push(MediaItem {
            id: mid,
            name: "New.mp4".into(),
            kind: MediaKind::Video,
            duration: 8.0,
            width: 1280,
            height: 720,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        opened.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            solo: true,
            ..Default::default()
        });
        opened.clips.push(Clip {
            id: Uuid::new_v4(),
            track: tid,
            media: mid,
            start: 2.0,
            out_point: 8.0,
            ..Default::default()
        });

        // Through the wire, not through the struct: this is the exact path a
        // peer replaying the change takes.
        let encoded = encode(&load_project_action(&opened));
        let decoded: VideoEditorAction =
            serde_json::from_slice(&encoded.payload).expect("payload decodes");
        decoded.apply(&mut doc).expect("applies");

        let back = VideoDoc::load_project(&doc);
        assert_eq!(back.name, "Opened");
        assert_eq!(back.fps, 24.0);
        assert_eq!(
            back.media.len(),
            1,
            "the previous bin is replaced, not merged"
        );
        assert_eq!(back.media[0].name, "New.mp4");
        assert_eq!(back.tracks.len(), 1);
        assert!(back.tracks[0].solo);
        assert_eq!(back.clips.len(), 1);
        assert_eq!(back.clips[0].media, mid);
    }

    #[test]
    fn a_whole_project_survives_the_wire_with_every_field_intact() {
        // `ProjectData` is composed from the per-entity payloads so there is
        // one definition of "a clip on the wire" — this proves the
        // composition did not quietly drop a layer.
        let (ctx, cid) = ctx_fixture();
        let mut p = ctx.project;
        p.add_marker(3.5, "Beat", 2);
        {
            let c = p.clip_mut(cid).unwrap();
            c.set_speed(0.5, false);
            c.transition_duration = 0.75;
            c.set_key_at(Param::Opacity, 1.0, 0.4, Interp::Linear);
        }

        let json = serde_json::to_vec(&load_project_action(&p)).unwrap();
        let back: VideoEditorAction = serde_json::from_slice(&json).unwrap();
        let VideoEditorAction::LoadProject(data) = back else {
            panic!("the action round-trips as itself");
        };
        let rebuilt = data.to_project();

        assert_eq!(rebuilt.markers.len(), 1);
        assert_eq!(rebuilt.markers[0].name, "Beat");
        assert_eq!(rebuilt.media.len(), p.media.len());
        assert_eq!(rebuilt.tracks.len(), p.tracks.len());
        let c = rebuilt.clip(cid).expect("the clip kept its id");
        assert_eq!(c.speed, p.clip(cid).unwrap().speed);
        assert!((c.transition_duration - 0.75).abs() < 1e-9);
        assert_eq!(c.anim, p.clip(cid).unwrap().anim);
    }

    #[test]
    fn the_whole_clip_write_carries_geometry_speed_curves_and_markers() {
        // `UpsertClip` is the one door every per-clip edit goes through, so
        // anything it drops is an edit that silently does not replicate.
        let (mut p, cid) = {
            let (ctx, cid) = ctx_fixture();
            (ctx.project, cid)
        };
        {
            let c = p.clip_mut(cid).unwrap();
            c.transform = crate::model::Transform::pip(0.3, (1.0, -1.0), 0.05);
            c.set_speed(2.5, true);
            c.set_key_at(Param::Opacity, 0.0, 0.2, Interp::Ease);
            c.set_key_at(Param::Opacity, 3.0, 0.9, Interp::Hold);
            c.transition_duration = 1.25;
            c.add_marker(1.0, "beat", 2);
            c.key = crate::model::ChromaKey {
                enabled: true,
                color: [0.09, 0.71, 0.18],
                tolerance: 0.26,
                softness: 0.08,
                spill: 0.55,
            };
            c.mask = crate::model::Mask {
                shape: crate::model::MaskShape::Rect,
                x: 0.12,
                y: -0.34,
                w: 0.55,
                h: 0.42,
                rotation: 12.0,
                feather: 0.19,
                invert: true,
            };
            c.set_key_at(Param::MaskX, 2.0, 0.4, Interp::Linear);
        }
        let want = p.clip(cid).unwrap().clone();
        let data = ClipData::from_clip(&want);
        let back = data.to_clip().expect("round-trips");
        assert_eq!(back, want, "the wire payload is lossless");
        // Through serde too, which is how it actually travels — a field
        // missing its `serde` attribute survives the struct copy above and
        // is dropped the moment it is encoded.
        let wire = serde_json::to_vec(&data).expect("encodes");
        let decoded: ClipData = serde_json::from_slice(&wire).expect("decodes");
        assert_eq!(
            decoded.to_clip().expect("rebuilds"),
            want,
            "and over the wire"
        );

        // And through a real document.
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, &p);
        VideoEditorAction::UpsertClip(data)
            .apply(&mut doc)
            .expect("applies");
        assert_eq!(VideoDoc::load_project(&doc).clip(cid).unwrap(), &want);
    }

    /// A peer that predates keying sends a payload with no `key`/`mask`
    /// field at all. That has to read as "not keyed, not masked" — the
    /// composite that peer is already drawing — rather than failing the
    /// decode and wedging the replay.
    #[test]
    fn a_payload_from_before_keying_reads_as_unkeyed_and_unmasked() {
        let (ctx, cid) = ctx_fixture();
        let want = ctx.project.clip(cid).unwrap().clone();
        let mut json: serde_json::Value =
            serde_json::to_value(ClipData::from_clip(&want)).expect("encodes");
        let obj = json.as_object_mut().expect("a map on the wire");
        assert!(obj.remove("key").is_some(), "the field is there to remove");
        assert!(obj.remove("mask").is_some());
        let older: ClipData = serde_json::from_value(json).expect("an older payload still decodes");
        let c = older.to_clip().expect("and still rebuilds");
        assert!(!c.key.is_active(), "absent means nothing is keyed");
        assert!(!c.mask.is_active(), "absent means the layer is not cut");
        assert_eq!(c, want, "and nothing else about the clip moved");
    }

    #[test]
    fn the_palette_can_set_a_speed_and_reverse_it() {
        let (ctx, cid) = ctx_fixture();
        let sped = run(&ctx.project, &spec(&ctx, "set-speed"), Some("2"));
        let c = sped.clip(cid).unwrap();
        assert_eq!(c.speed.rate(), 2.0);
        assert_eq!(
            c.duration(),
            15.0,
            "and the clip is half as long on the timeline"
        );

        let back = run(&ctx.project, &spec(&ctx, "toggle-reverse"), None);
        assert!(back.clip(cid).unwrap().speed.reverse);
    }

    #[test]
    fn the_palette_can_inset_a_clip_and_put_it_back() {
        let (ctx, cid) = ctx_fixture();
        let inset = run(&ctx.project, &spec(&ctx, "pip-tr"), None);
        let t = inset.clip(cid).unwrap().transform;
        assert!(!t.is_identity());
        assert!(t.pos_x > 0.0 && t.pos_y < 0.0, "top-right: {t:?}");

        // With the clip already inset, the palette offers the way back.
        let ctx2 = VideoResourceCtx {
            project: inset,
            ..ctx
        };
        let flat = run(&ctx2.project, &spec(&ctx2, "reset-transform"), None);
        assert!(flat.clip(cid).unwrap().transform.is_identity());
    }

    #[test]
    fn the_palette_keys_a_parameter_at_the_playhead_without_moving_the_picture() {
        let (ctx, cid) = ctx_fixture();
        let before = ctx.project.clip(cid).unwrap().param_at(Param::Opacity, 4.0);
        let keyed = run(&ctx.project, &spec(&ctx, "key-opacity"), None);
        let c = keyed.clip(cid).unwrap();
        assert!(c.anim.is_animated(Param::Opacity), "a curve exists now");
        assert_eq!(
            c.param_at(Param::Opacity, 4.0),
            before,
            "the first key is planted where the value already was",
        );

        // And with a curve present, the palette offers to take it away.
        let ctx2 = VideoResourceCtx {
            project: keyed,
            ..ctx
        };
        let cleared = run(&ctx2.project, &spec(&ctx2, "unkey-opacity"), None);
        assert!(!cleared.clip(cid).unwrap().anim.is_animated(Param::Opacity));
    }

    #[test]
    fn the_palette_adds_a_timeline_marker_and_removes_it() {
        let (ctx, _) = ctx_fixture();
        let marked = run(&ctx.project, &spec(&ctx, "add-marker"), Some("Act two"));
        assert_eq!(marked.markers.len(), 1);
        assert_eq!(marked.markers[0].name, "Act two");
        assert_eq!(marked.markers[0].t, 4.0, "at the playhead");

        let ctx2 = VideoResourceCtx {
            project: marked,
            ..ctx
        };
        let gone = run(&ctx2.project, &spec(&ctx2, "remove-marker"), None);
        assert!(gone.markers.is_empty());
    }

    #[test]
    fn the_palette_adds_a_title_to_the_bin() {
        let (ctx, _) = ctx_fixture();
        let with_title = run(&ctx.project, &spec(&ctx, "add-title"), Some("Chapter One"));
        let t = with_title
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .expect("a title landed in the bin");
        assert_eq!(
            t.text.as_ref().map(|t| t.content.as_str()),
            Some("Chapter One")
        );
        assert_eq!(t.name, "Chapter One", "and it is named after its words");
        assert!(t.path.is_empty(), "a title has no bytes to fetch");

        // Its words can then be edited from the bin.
        let ctx2 = VideoResourceCtx {
            project: with_title,
            ..ctx
        };
        let edited = run(
            &ctx2.project,
            &spec(&ctx2, "set-title-text"),
            Some("Chapter Two"),
        );
        let t = edited
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .unwrap();
        assert_eq!(
            t.text.as_ref().map(|t| t.content.as_str()),
            Some("Chapter Two")
        );
    }

    #[test]
    fn a_clip_marker_from_the_palette_lands_on_the_clip_not_the_project() {
        let (ctx, cid) = ctx_fixture();
        let marked = run(&ctx.project, &spec(&ctx, "add-clip-marker"), Some("hit"));
        assert!(marked.markers.is_empty(), "not a project marker");
        let m = &marked.clip(cid).unwrap().markers;
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "hit");
        assert_eq!(m[0].t, 4.0, "clip-local, and this clip starts at zero");
    }

    #[test]
    fn every_new_action_replays_to_the_same_document_twice() {
        // The determinism contract: no RNG, no clock, so applying the same
        // encoded action to the same base twice lands identically.
        let (ctx, _) = ctx_fixture();
        for (id, arg) in [
            ("set-speed", Some("1.75")),
            ("toggle-reverse", None),
            ("pip-bl", None),
            ("key-scale_x", None),
            ("add-marker", Some("m")),
            ("add-clip-marker", Some("m")),
            ("set-rotation", Some("-12")),
        ] {
            // One authoring, applied twice. Ids are minted when the action
            // is *built* and travel in the payload, so what has to be
            // deterministic is `apply` — two separate authorings of
            // "add a marker" are two different markers, and should be.
            let sp = spec(&ctx, id);
            let encoded = match arg {
                Some(a) => prompted_encode(&sp, a),
                None => immediate_encode(&sp),
            };
            let apply_once = || {
                let mut doc = AutoCommit::new();
                VideoDoc::write_project(&mut doc, &ctx.project);
                let action: VideoEditorAction =
                    serde_json::from_slice(&encoded.payload).expect("decodes");
                action.apply(&mut doc).expect("applies");
                serde_json::to_value(VideoDoc::load_project(&doc)).unwrap()
            };
            assert_eq!(apply_once(), apply_once(), "{id} replays deterministically");
        }
    }

    use super::*;
    use crate::app::VideoEditorApp;
    use qos_actions::{ActionInput, ActionRegistry, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// An editor with the sample project seeded into its document, its in-memory
    /// project synced to match (mirrors the live wiring).
    fn seeded() -> (VideoEditorApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut app = VideoEditorApp::new();
        app.sync(&mut doc, &automerge::ROOT);
        (app, doc)
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            _ => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// Ctrl+K on the editor yields the nested tree: the project → its tracks →
    /// the clips on each, each exporting the actions its data model implies.
    /// Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_project_track_clip_tree() {
        let (app, _doc) = seeded();
        let resources = Widget::resources(&app, &_doc);
        assert_eq!(resources.len(), 1, "one open project");
        let project = &resources[0];
        assert_eq!(project.kind, "video.project");
        let proj_actions: Vec<&str> = project.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(proj_actions.contains(&"rename-project"), "{proj_actions:?}");

        let tracks: Vec<_> = project
            .children
            .iter()
            .filter(|c| c.kind == "video.track")
            .collect();
        assert_eq!(tracks.len(), 4, "four sample tracks");
        // The bin's media are also published, each with rename + delete.
        let media: Vec<_> = project
            .children
            .iter()
            .filter(|c| c.kind == "video.media")
            .collect();
        assert_eq!(media.len(), 7, "seven sample media items");
        let media_actions: Vec<&str> = media[0].actions.iter().map(|a| a.id.as_str()).collect();
        assert!(media_actions.contains(&"rename-media"), "{media_actions:?}");
        assert!(media_actions.contains(&"delete-media"), "{media_actions:?}");
        let track = tracks[0];
        assert_eq!(track.kind, "video.track");
        let track_actions: Vec<&str> = track.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(track_actions.contains(&"rename-track"), "{track_actions:?}");
        assert!(track_actions.contains(&"delete-track"), "{track_actions:?}");
        assert!(track_actions.contains(&"toggle-mute"), "{track_actions:?}");

        // Some clip must expose the trim / delete / filter surface.
        let clip = project
            .children
            .iter()
            .flat_map(|t| t.children.iter())
            .find(|c| c.kind == "video.clip")
            .expect("a clip resource");
        let clip_actions: Vec<&str> = clip.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(clip_actions.contains(&"delete"), "{clip_actions:?}");
        assert!(clip_actions.contains(&"ripple-delete"), "{clip_actions:?}");
        assert!(clip_actions.contains(&"set-brightness"), "{clip_actions:?}");

        // The one palette surfaces the tree's actions as rows. The result
        // list is capped, so each is asked for the way a user asks — by
        // typing — rather than by assuming an unfiltered palette shows the
        // whole tree at once.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        for want in [
            "Delete clip",
            "Rename project",
            "Add title",
            "Add marker at playhead",
        ] {
            palette.set_query(want);
            let labels = palette.result_labels();
            assert!(
                labels.iter().any(|l| l.contains(want)),
                "the palette finds {want:?}: {labels:?}",
            );
        }
    }

    /// Dispatching `DeleteClip` through the executor removes exactly that clip
    /// from the document.
    #[test]
    fn delete_clip_action_removes_it_from_the_doc() {
        let (_app, mut doc) = seeded();
        let project = VideoDoc::load_project(&doc);
        let id = project.clips[0].id;
        let before = project.clips.len();

        let action = delete_clip_action(&id);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let after = VideoDoc::load_project(&doc);
        assert_eq!(after.clips.len(), before - 1);
        assert!(after.clip(id).is_none(), "the clip is gone from the CRDT");
    }

    /// A ripple delete lands as ONE action that both removes the clip and shifts
    /// every later clip on the same track left — a single change, not N.
    #[test]
    fn ripple_delete_is_one_action_that_closes_the_gap() {
        let (_app, mut doc) = seeded();
        let project = VideoDoc::load_project(&doc);
        // Find a track whose first clip has a later neighbour.
        let (id, track, next_id, next_start) = project
            .tracks
            .iter()
            .find_map(|t| {
                let clips = project.clips_on(t.id);
                if clips.len() >= 2 {
                    Some((clips[0].id, t.id, clips[1].id, clips[1].start))
                } else {
                    None
                }
            })
            .expect("a track with two clips");
        let _ = track;

        let mut plan = project;
        let moved_ids = plan.ripple_delete(id);
        let moved: Vec<Clip> = moved_ids
            .iter()
            .filter_map(|m| plan.clip(*m).cloned())
            .collect();
        let action = ripple_delete_action(&id, &moved);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let after = VideoDoc::load_project(&doc);
        assert!(after.clip(id).is_none(), "the rippled clip is gone");
        assert!(
            after.clip(next_id).unwrap().start < next_start,
            "the later clip shifted left to close the gap",
        );
    }

    /// Deleting a bin media removes the source *and* every clip that placed it,
    /// in one action, so no dangling clip is left referencing a gone source.
    #[test]
    fn remove_media_action_also_deletes_its_clips() {
        let (_app, mut doc) = seeded();
        let project = VideoDoc::load_project(&doc);
        // Pick a media that is placed on the timeline at least once.
        let mid = project
            .media
            .iter()
            .find(|m| project.clips.iter().any(|c| c.media == m.id))
            .map(|m| m.id)
            .expect("a placed media");
        let clip_ids: Vec<Uuid> = project
            .clips
            .iter()
            .filter(|c| c.media == mid)
            .map(|c| c.id)
            .collect();
        assert!(!clip_ids.is_empty());

        let action = remove_media_action(&mid, &clip_ids);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let after = VideoDoc::load_project(&doc);
        assert!(
            after.media(mid).is_none(),
            "the source is gone from the bin"
        );
        for cid in &clip_ids {
            assert!(
                after.clip(*cid).is_none(),
                "a clip of the removed media is gone"
            );
        }
    }

    /// The collaboration guarantee: replaying the same encoded action on a
    /// second independent replica (from the same base state) lands on the
    /// byte-identical document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, seed) = seeded();
        let mut a = seed;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        // Move a clip: a whole-clip upsert with a new start.
        let project = VideoDoc::load_project(&a);
        let mut clip = project.clips[0].clone();
        clip.start += 5.0;
        let action = upsert_clip_action(&clip);
        let encoded = encode(&action);

        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert!(
            (VideoDoc::load_project(&b).clip(clip.id).unwrap().start - clip.start).abs() < 1e-9
        );
    }

    /// Single source of truth: the clip resource's `delete` spec encodes the
    /// byte-identical action the on-screen trash button dispatches
    /// (`delete_clip_action(id)`).
    #[test]
    fn palette_and_button_encode_delete_identically() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let clip = resources[0]
            .children
            .iter()
            .flat_map(|t| t.children.iter())
            .find(|c| c.kind == "video.clip")
            .unwrap();
        let cid: Uuid = clip.id.0.trim_start_matches("video.clip:").parse().unwrap();
        let spec = clip.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&delete_clip_action(&cid));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth"
        );
    }

    /// The prompted brightness spec builds the same whole-clip upsert an
    /// inspector edit to 0.9 would — a slider value folded into one action.
    #[test]
    fn prompted_brightness_builds_the_expected_clip_write() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let clip_res = resources[0]
            .children
            .iter()
            .flat_map(|t| t.children.iter())
            .find(|c| c.kind == "video.clip")
            .unwrap();
        let cid: Uuid = clip_res
            .id
            .0
            .trim_start_matches("video.clip:")
            .parse()
            .unwrap();
        let spec = clip_res
            .actions
            .iter()
            .find(|a| a.id == "set-brightness")
            .unwrap();
        let from_spec = prompted_encode(spec, "0.9");

        let mut expected = VideoDoc::load_project(&doc).clip(cid).unwrap().clone();
        expected.filters.brightness = 0.9;
        let from_builder = encode(&upsert_clip_action(&expected));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "slider folds into one clip write"
        );
    }

    /// A grade action declares a number, and a word is refused before the
    /// document is touched.
    ///
    /// This used to be a free-text prompt parsed by `parse_unit`, which
    /// answered `0.5` for anything it could not read. So `set-brightness
    /// banana` did not fail — it made a real edit, to a plausible value, and
    /// reported success. A caller had no way to learn the type either: "Value
    /// 0–1" was prose in a placeholder. Declaring the parameter makes the type
    /// readable and makes a bad value an error rather than an edit.
    #[test]
    fn a_word_is_refused_where_the_grade_declares_a_number() {
        use qos_actions::params::ArgValue;

        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let clip_res = resources[0]
            .children
            .iter()
            .flat_map(|t| t.children.iter())
            .find(|c| c.kind == "video.clip")
            .unwrap();
        let spec = clip_res
            .actions
            .iter()
            .find(|a| a.id == "set-brightness")
            .unwrap();

        // The type is published, not described in prose.
        assert_eq!(spec.params().len(), 1, "one declared parameter");
        assert_eq!(spec.params()[0].name.as_ref(), "value");
        assert_eq!(spec.params()[0].ty, ParamType::Number);

        let refused = ActionArgs::bind(
            &spec.params(),
            [("value".to_string(), ArgValue::Text("banana".into()))],
        );
        assert!(
            refused.is_err(),
            "a word bound to a number-typed parameter must be refused, not read as 0.5",
        );

        let bound = ActionArgs::bind(
            &spec.params(),
            [("value".to_string(), ArgValue::Number(0.8))],
        )
        .expect("a number binds");
        assert_eq!(bound.number("value"), Some(0.8));
    }

    /// The fixture with a title in the bin, so the media resource exports the
    /// title actions as well as the rename.
    fn ctx_with_title() -> VideoResourceCtx {
        let (ctx, _) = ctx_fixture();
        let with_title = run(&ctx.project, &spec(&ctx, "add-title"), Some("Chapter One"));
        VideoResourceCtx {
            project: with_title,
            ..ctx
        }
    }

    /// The title source in the bin, whichever media item it turned out to be.
    fn title_of(p: &Project) -> TextSource {
        p.media
            .iter()
            .find(|m| m.kind == MediaKind::Text)
            .and_then(|m| m.text.clone())
            .expect("a title in the bin")
    }

    /// Every action that takes an argument, with the names and types it
    /// declares **in order**. One table, so a new parameterised action has to
    /// be added here — and the test below proves nothing parameterised is
    /// missing.
    ///
    /// A parameter *list* rather than a single pair: the project's frame size
    /// is one decision carried by two numbers, and an action that took a
    /// width without a height would be a shape nobody asked for. Every
    /// assertion the single-pair table made is still made, once per declared
    /// parameter.
    const DECLARED: [(&str, &[(&str, ParamType)]); 28] = [
        ("rename-project", &[("name", ParamType::Text)]),
        (
            "set-frame-size",
            &[("width", ParamType::Number), ("height", ParamType::Number)],
        ),
        ("set-frame-rate", &[("fps", ParamType::Number)]),
        (
            "set-render-range",
            &[("start", ParamType::Number), ("end", ParamType::Number)],
        ),
        ("add-marker", &[("name", ParamType::Text)]),
        ("add-title", &[("text", ParamType::Text)]),
        ("rename-track", &[("name", ParamType::Text)]),
        ("set-brightness", &[("value", ParamType::Number)]),
        ("set-contrast", &[("value", ParamType::Number)]),
        ("set-saturation", &[("value", ParamType::Number)]),
        ("set-opacity", &[("value", ParamType::Number)]),
        ("set-volume", &[("value", ParamType::Number)]),
        ("set-fade-in", &[("seconds", ParamType::Number)]),
        ("set-fade-out", &[("seconds", ParamType::Number)]),
        ("add-effect", &[("effect", ParamType::Text)]),
        ("set-speed", &[("rate", ParamType::Number)]),
        ("set-scale", &[("factor", ParamType::Number)]),
        ("set-pos-x", &[("offset", ParamType::Number)]),
        ("set-pos-y", &[("offset", ParamType::Number)]),
        ("set-rotation", &[("degrees", ParamType::Number)]),
        ("crop-left", &[("fraction", ParamType::Number)]),
        ("crop-right", &[("fraction", ParamType::Number)]),
        ("crop-top", &[("fraction", ParamType::Number)]),
        ("crop-bottom", &[("fraction", ParamType::Number)]),
        ("add-clip-marker", &[("name", ParamType::Text)]),
        ("set-transition", &[("transition", ParamType::Text)]),
        ("set-mask-shape", &[("shape", ParamType::Text)]),
        ("set-title-text", &[("text", ParamType::Text)]),
    ];

    /// The rest of the table. Split only because a single array literal past
    /// thirty entries stopped being readable; the test walks both.
    const DECLARED_MORE: [(&str, &[(&str, ParamType)]); 14] = [
        ("set-title-size", &[("size", ParamType::Number)]),
        ("set-title-align", &[("align", ParamType::Text)]),
        ("set-title-colour", &[("colour", ParamType::Text)]),
        ("set-title-backdrop", &[("opacity", ParamType::Number)]),
        ("rename-media", &[("name", ParamType::Text)]),
        // The chroma key's three sliders and the mask's six geometry knobs.
        ("set-key-tolerance", &[("value", ParamType::Number)]),
        ("set-key-softness", &[("value", ParamType::Number)]),
        ("set-key-spill", &[("value", ParamType::Number)]),
        ("set-mask-x", &[("offset", ParamType::Number)]),
        ("set-mask-y", &[("offset", ParamType::Number)]),
        ("set-mask-width", &[("fraction", ParamType::Number)]),
        ("set-mask-height", &[("fraction", ParamType::Number)]),
        ("set-mask-rotation", &[("degrees", ParamType::Number)]),
        ("set-mask-feather", &[("value", ParamType::Number)]),
    ];

    /// Both halves of the table, as one list.
    fn declared() -> Vec<(&'static str, &'static [(&'static str, ParamType)])> {
        DECLARED.into_iter().chain(DECLARED_MORE).collect()
    }

    /// Every action that takes an argument names it and declares its type.
    ///
    /// That declaration is the whole surface for a caller with no screen: a
    /// screen reader, a script and an agent read the enumeration and nothing
    /// else, so a type stated only in placeholder prose ("Value 0-1", "Rate,
    /// e.g. 0.5 / 1 / 2") is a type none of them can see. The second half of
    /// the test is the one that keeps it honest — an action that takes an
    /// argument and is not in the table fails here rather than shipping as
    /// another untyped prompt.
    #[test]
    fn every_parameterised_action_declares_its_argument() {
        let ctx = ctx_with_title();
        for (id, params) in declared() {
            let sp = spec(&ctx, id);
            assert_eq!(
                sp.params().len(),
                params.len(),
                "{id} declares exactly the parameters this test has read",
            );
            for (i, (name, ty)) in params.iter().enumerate() {
                let p = &sp.params()[i];
                assert_eq!(p.name.as_ref(), *name, "{id} names its argument {i}");
                assert_eq!(p.ty, *ty, "{id} declares the type of {name}");
                assert!(p.required, "{id} needs {name}");
                assert!(!p.description.is_empty(), "{id} says what {name} means",);
            }
        }

        let known = declared();
        for (id, sp) in all_specs(&ctx) {
            if sp.params().is_empty() {
                continue;
            }
            assert!(
                known.iter().any(|(k, _)| *k == id),
                "{id} takes an argument but declares nothing this test has read",
            );
        }
    }

    /// The chroma key's three sliders and the mask's six geometry knobs write
    /// the value they are named for.
    ///
    /// Declaring the parameter is half of it; the other half is that the number
    /// lands on the knob the id names. Nine near-identical actions is exactly
    /// the shape a copy-paste slip hides in — `set-mask-height` writing
    /// `mask.w` would satisfy every type assertion in this file and stay
    /// invisible until someone actually masked something — so each is checked
    /// against every one of its neighbours as well as against itself.
    #[test]
    fn the_key_and_mask_knobs_write_the_value_they_are_named_for() {
        let (ctx, cid) = ctx_fixture();
        type Read = fn(&Clip) -> f64;
        let cases: [(&str, &str, f64, Read); 9] = [
            ("set-key-tolerance", "0.31", 0.31, |c| c.key.tolerance),
            ("set-key-softness", "0.17", 0.17, |c| c.key.softness),
            ("set-key-spill", "0.62", 0.62, |c| c.key.spill),
            ("set-mask-x", "-0.24", -0.24, |c| c.mask.x),
            ("set-mask-y", "0.44", 0.44, |c| c.mask.y),
            ("set-mask-width", "0.73", 0.73, |c| c.mask.w),
            ("set-mask-height", "0.29", 0.29, |c| c.mask.h),
            ("set-mask-rotation", "-37", -37.0, |c| c.mask.rotation),
            ("set-mask-feather", "0.08", 0.08, |c| c.mask.feather),
        ];
        let before = ctx.project.clip(cid).expect("the fixture clip").clone();
        for (id, arg, want, read) in cases {
            let after = run(&ctx.project, &spec(&ctx, id), Some(arg));
            let clip = after.clip(cid).expect("the clip survives its own edit");
            assert!(
                (read(clip) - want).abs() < 1e-9,
                "{id} left {} where it was asked for {want}",
                read(clip),
            );
            for (other, other_read) in cases.iter().map(|(i, _, _, r)| (*i, *r)) {
                if other == id {
                    continue;
                }
                assert!(
                    (other_read(clip) - other_read(&before)).abs() < 1e-9,
                    "{id} also moved the knob {other} is for",
                );
            }
        }
        // Out of range clamps where the far stop is a legible intent, and does
        // not where the model has a reason to allow more.
        let hot = run(&ctx.project, &spec(&ctx, "set-key-tolerance"), Some("1.8"));
        let tol = hot.clip(cid).expect("clip").key.tolerance;
        assert!(
            (tol - 1.0).abs() < 1e-9,
            "\u{201c}as much as it takes\u{201d} is the far stop, not {tol}",
        );
        let wide = run(&ctx.project, &spec(&ctx, "set-mask-width"), Some("1.6"));
        assert!(
            (wide.clip(cid).expect("clip").mask.w - 1.6).abs() < 1e-9,
            "a mask may reach past the frame — that is how a soft edge is kept \
             out of the picture rather than drawn across it",
        );
        let inverted = run(&ctx.project, &spec(&ctx, "set-mask-width"), Some("-0.5"));
        let w = inverted.clip(cid).expect("clip").mask.w;
        assert!(
            (w - 0.0).abs() < 1e-9,
            "a negative extent is not a shape: {w}"
        );
    }

    /// A key or mask knob asked for nothing it can read leaves the value where
    /// it was, rather than answering with a plausible middle.
    ///
    /// The failure this rules out is the one the grade actions were built to
    /// rule out: a lenient parser that read a word as 0.5 made a real,
    /// convincing edit and reported success.
    #[test]
    fn a_word_never_reaches_a_key_or_mask_knob() {
        use qos_actions::params::ArgValue;

        let (ctx, _cid) = ctx_fixture();
        for (id, param) in [
            ("set-key-tolerance", "value"),
            ("set-key-softness", "value"),
            ("set-key-spill", "value"),
            ("set-mask-x", "offset"),
            ("set-mask-y", "offset"),
            ("set-mask-width", "fraction"),
            ("set-mask-height", "fraction"),
            ("set-mask-rotation", "degrees"),
            ("set-mask-feather", "value"),
        ] {
            let sp = spec(&ctx, id);
            assert!(
                ActionArgs::bind(
                    sp.params(),
                    vec![(param.to_string(), ArgValue::Text("a lot".into()))]
                )
                .is_err(),
                "{id} must refuse a word where {param} declares a number",
            );
            let bound = ActionArgs::bind(
                sp.params(),
                vec![(param.to_string(), ArgValue::Number(0.4))],
            )
            .unwrap_or_else(|e| panic!("{id} takes a number for {param}: {e}"));
            assert_eq!(bound.number(param), Some(0.4));
        }
    }

    /// A word is refused wherever a number is declared, everywhere — not only
    /// on the grade.
    ///
    /// Each of these used to be a free-text prompt with a lenient parser
    /// behind it: `parse_unit` answered 0.5, the geometry parse answered 0.0
    /// (for scale, an invisible clip), the speed parse answered 1x. All three
    /// were a real edit reported as success. Declaring `Number` makes the bad
    /// call a refusal, before the document is touched.
    #[test]
    fn a_word_is_refused_wherever_a_number_is_declared() {
        use qos_actions::params::ArgValue;

        let ctx = ctx_with_title();
        for (id, params) in declared() {
            for (target, ty) in params {
                if *ty != ParamType::Number {
                    continue;
                }
                let sp = spec(&ctx, id);
                // Every declared argument supplied, so what is under test is
                // the *type* of one of them and not a missing sibling. An
                // action carrying two numbers has to refuse a word in either
                // position, not only the first.
                let with = |bad: Option<&str>| -> Vec<(String, ArgValue)> {
                    params
                        .iter()
                        .map(|(name, _)| {
                            let v = match bad {
                                Some(word) if name == target => ArgValue::Text(word.into()),
                                _ => ArgValue::Number(0.25),
                            };
                            ((*name).to_string(), v)
                        })
                        .collect()
                };
                assert!(
                    ActionArgs::bind(sp.params(), with(Some("banana"))).is_err(),
                    "{id} must refuse a word where {target} declares a number",
                );
                let bound = ActionArgs::bind(sp.params(), with(None))
                    .unwrap_or_else(|e| panic!("{id} takes a number for {target}: {e}"));
                assert_eq!(bound.number(target), Some(0.25));
                // An integer widens — JSON spells 1.0 as 1, and a rate of 1
                // has to stay passable.
                let ints: Vec<(String, ArgValue)> = params
                    .iter()
                    .map(|(name, _)| ((*name).to_string(), ArgValue::Integer(1)))
                    .collect();
                assert!(
                    ActionArgs::bind(sp.params(), ints).is_ok(),
                    "{id} takes an integer literal as a number",
                );
                // And an argument by a name it never declared is refused
                // rather than dropped, so a caller cannot believe it asked
                // for something it did not get.
                let mut stray = with(None);
                stray.push(("v".to_string(), ArgValue::Number(0.25)));
                assert!(
                    ActionArgs::bind(sp.params(), stray).is_err(),
                    "{id} refuses an argument it never declared",
                );
            }
        }
    }

    /// A name outside the declared vocabulary is refused, and every name the
    /// vocabulary lists binds.
    ///
    /// The prose list used to be the entire specification — "grayscale /
    /// sepia / invert / warm / cool / vignette" in a placeholder — with the
    /// parser answering "nothing happened" for a name it did not know, and
    /// `parse_transition` answering "hard cut". Declared choices are the same
    /// list a caller can read *and* be held to.
    #[test]
    fn a_name_outside_the_declared_choices_is_refused() {
        use qos_actions::params::ArgValue;

        let ctx = ctx_with_title();
        for (id, name, never_offered) in [
            ("add-effect", "effect", "chrome"),
            ("set-transition", "transition", "swipe"),
            ("set-title-align", "align", "centre"),
        ] {
            let sp = spec(&ctx, id);
            let choices = &sp.params()[0].choices;
            assert!(!choices.is_empty(), "{id} enumerates its vocabulary");
            for c in choices {
                ActionArgs::bind(
                    sp.params(),
                    [(name.to_string(), ArgValue::Text(c.to_string()))],
                )
                .unwrap_or_else(|e| panic!("{id} accepts its own choice {c:?}: {e}"));
            }
            assert!(
                ActionArgs::bind(
                    sp.params(),
                    [(name.to_string(), ArgValue::Text(never_offered.into()))],
                )
                .is_err(),
                "{id} refuses {never_offered:?}, a name it never offered",
            );
            assert!(
                ActionArgs::bind(sp.params(), [(name.to_string(), ArgValue::Number(1.0))]).is_err(),
                "{id} refuses a number where it declares a name",
            );
        }

        // The declared set is the parser's set, in both directions: nothing is
        // offered that the parser cannot read, and nothing the model can be is
        // unreachable through the names offered. That is what stops the
        // vocabulary and the parser drifting apart once they are two lists.
        let effect = spec(&ctx, "add-effect");
        let effects = &effect.params()[0].choices;
        for c in effects {
            assert!(
                EffectKind::from_str(c).is_some(),
                "{c:?} is offered, so the parser has to know it",
            );
        }
        for k in EffectKind::ALL {
            assert!(
                effects.iter().any(|c| EffectKind::from_str(c) == Some(k)),
                "{} is reachable by name",
                k.as_str(),
            );
        }
        let transition = spec(&ctx, "set-transition");
        let transitions = &transition.params()[0].choices;
        for t in Transition::ALL {
            assert!(
                transitions.iter().any(|c| parse_transition(c) == t),
                "{} is reachable by name",
                t.as_str(),
            );
        }
    }

    /// Valid input still lands, and unreadable input now changes nothing
    /// instead of inventing a plausible edit.
    #[test]
    fn an_unreadable_value_no_longer_invents_an_edit() {
        let (ctx, cid) = ctx_fixture();

        // `parse_unit("half")` answered 0.5 — half the frame cropped away, on
        // a typo. The word cannot bind now, so the edge keeps its value.
        let cropped = run(&ctx.project, &spec(&ctx, "crop-left"), Some("half"));
        assert_eq!(cropped.clip(cid).unwrap().transform.crop_left, 0.0);
        // A number still lands, and out of range still clamps: asking to cut
        // 1.4 of the frame is a legible "all of it".
        let cropped = run(&ctx.project, &spec(&ctx, "crop-left"), Some("1.4"));
        assert_eq!(cropped.clip(cid).unwrap().transform.crop_left, 1.0);
        let cropped = run(&ctx.project, &spec(&ctx, "crop-bottom"), Some("0.25"));
        assert_eq!(cropped.clip(cid).unwrap().transform.crop_bottom, 0.25);

        // The geometry parse answered 0.0, which for scale is a clip that is
        // no longer on screen.
        let scaled = run(&ctx.project, &spec(&ctx, "set-scale"), Some("banana"));
        assert_eq!(scaled.clip(cid).unwrap().transform.scale_x, 1.0);
        let scaled = run(&ctx.project, &spec(&ctx, "set-scale"), Some("0.5"));
        assert_eq!(scaled.clip(cid).unwrap().transform.scale_y, 0.5);
        // Rotation is a real number and stays one — degrees are not integers.
        let turned = run(&ctx.project, &spec(&ctx, "set-rotation"), Some("-12.5"));
        assert_eq!(turned.clip(cid).unwrap().transform.rotation, -12.5);

        // The speed parse answered 1.0, so a bad rate silently returned a
        // slowed clip to normal. Now a slowed clip stays slowed.
        let fast = run(&ctx.project, &spec(&ctx, "set-speed"), Some("2"));
        let ctx2 = VideoResourceCtx {
            project: fast,
            ..ctx
        };
        let still_fast = run(&ctx2.project, &spec(&ctx2, "set-speed"), Some("quickly"));
        assert_eq!(still_fast.clip(cid).unwrap().speed.rate(), 2.0);
    }

    /// A named transition and effect still land; a name the action never
    /// offered leaves the clip alone instead of quietly cutting.
    #[test]
    fn a_named_transition_and_effect_land_and_an_unknown_name_does_not() {
        let (ctx, cid) = ctx_fixture();

        let dipped = run(
            &ctx.project,
            &spec(&ctx, "set-transition"),
            Some("dip to black"),
        );
        assert_eq!(dipped.clip(cid).unwrap().transition, Transition::DipToBlack);
        let ctx2 = VideoResourceCtx {
            project: dipped,
            ..ctx
        };
        let kept = run(&ctx2.project, &spec(&ctx2, "set-transition"), Some("swipe"));
        assert_eq!(
            kept.clip(cid).unwrap().transition,
            Transition::DipToBlack,
            "an unoffered name no longer falls through to a hard cut",
        );

        // An alias the placeholder never listed but the parser has always
        // taken still works — declaring the type must not take a working name
        // away.
        let inverted = run(&ctx.project, &spec(&ctx, "add-effect"), Some("negative"));
        assert_eq!(
            inverted.clip(cid).unwrap().effects.last().map(|e| e.kind),
            Some(EffectKind::Invert),
        );
        let none = run(&ctx.project, &spec(&ctx, "add-effect"), Some("chrome"));
        assert!(none.clip(cid).unwrap().effects.is_empty());
    }

    /// The title's own arguments: three declared names for alignment, numbers
    /// for size and backdrop, and free text for the colour — which keeps its
    /// leniency deliberately.
    #[test]
    fn the_title_actions_hold_their_declared_shapes() {
        let ctx = ctx_with_title();

        let left = run(&ctx.project, &spec(&ctx, "set-title-align"), Some("left"));
        assert_eq!(title_of(&left).align, crate::model::TextAlign::Left);
        let ctx2 = VideoResourceCtx {
            project: left,
            ..ctx
        };
        // "centre" used to be read as `Center` — an edit, from a spelling the
        // action never offered, indistinguishable from a real one.
        let same = run(
            &ctx2.project,
            &spec(&ctx2, "set-title-align"),
            Some("centre"),
        );
        assert_eq!(title_of(&same).align, crate::model::TextAlign::Left);

        let big = run(&ctx2.project, &spec(&ctx2, "set-title-size"), Some("0.3"));
        assert_eq!(title_of(&big).size, 0.3);
        let plate = run(
            &ctx2.project,
            &spec(&ctx2, "set-title-backdrop"),
            Some("1.4"),
        );
        assert_eq!(title_of(&plate).backdrop, 1.0, "clamped, not invented");

        // A colour is free text with no set to enumerate, and `parse_colour`
        // still answers `None` for a word rather than guessing: the title
        // keeps the colour it had instead of going black.
        let before = title_of(&ctx2.project).color;
        let unchanged = run(
            &ctx2.project,
            &spec(&ctx2, "set-title-colour"),
            Some("puce"),
        );
        assert_eq!(title_of(&unchanged).color, before);
        let red = run(
            &ctx2.project,
            &spec(&ctx2, "set-title-colour"),
            Some("#ff0000"),
        );
        assert_eq!(title_of(&red).color, [1.0, 0.0, 0.0, 1.0]);
    }

    /// The three renames still rename, through a parameter that now has a
    /// name of its own rather than the generic prompted one.
    #[test]
    fn the_renames_still_rename_through_their_named_parameter() {
        let (ctx, _) = ctx_fixture();
        let renamed = run(
            &ctx.project,
            &spec(&ctx, "rename-project"),
            Some("  Cut 3  "),
        );
        assert_eq!(renamed.name, "Cut 3");
        let tracks = run(&ctx.project, &spec(&ctx, "rename-track"), Some("B-roll"));
        assert_eq!(tracks.tracks[0].name, "B-roll");
        let media = run(&ctx.project, &spec(&ctx, "rename-media"), Some("hero"));
        assert_eq!(media.media[0].name, "hero");
    }

    /// The Ctrl+K "Add effect" prompt appends the named effect to the clip's
    /// stack through the same whole-clip write the inspector uses.
    #[test]
    fn add_effect_action_appends_to_the_stack() {
        let (app, mut doc) = seeded();
        let resources = Widget::resources(&app, &doc);
        let clip_res = resources[0]
            .children
            .iter()
            .flat_map(|t| t.children.iter())
            .find(|c| c.kind == "video.clip")
            .unwrap();
        let cid: Uuid = clip_res
            .id
            .0
            .trim_start_matches("video.clip:")
            .parse()
            .unwrap();
        let before = VideoDoc::load_project(&doc)
            .clip(cid)
            .unwrap()
            .effects
            .len();
        let spec = clip_res
            .actions
            .iter()
            .find(|a| a.id == "add-effect")
            .expect("add-effect action");
        let encoded = prompted_encode(spec, "sepia");
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        let after = VideoDoc::load_project(&doc)
            .clip(cid)
            .unwrap()
            .effects
            .clone();
        assert_eq!(
            after.len(),
            before + 1,
            "the effect was appended to the stack"
        );
        assert_eq!(after.last().unwrap().kind, EffectKind::Sepia);
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the doc and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        // Seed the sample into a real host State so undo is the production path.
        let mut state = ui::State::new();
        VideoDoc::seed_sample(&mut state.doc);
        let before_heads = state.doc.get_heads();
        let project = VideoDoc::load_project(&state.doc);
        let id = project.clips[0].id;
        let before_start = project.clip(id).unwrap().start;

        // Move the clip via the action + host undo recording.
        let mut moved = project.clip(id).unwrap().clone();
        moved.start = before_start + 7.0;
        let action = upsert_clip_action(&moved);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert!(
            (VideoDoc::load_project(&state.doc).clip(id).unwrap().start - (before_start + 7.0))
                .abs()
                < 1e-9,
            "the action moved the clip",
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert!(
            (VideoDoc::load_project(&state.doc).clip(id).unwrap().start - before_start).abs()
                < 1e-9,
            "undo reverted the move and nothing else",
        );
    }
}
