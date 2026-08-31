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
    Clip, EffectKind, Filters, MediaItem, MediaKind, Project, TextSource, Track, TrackKind,
    Transition,
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
        }
    }

    fn to_media(&self) -> Option<MediaItem> {
        Some(MediaItem {
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
            markers: p.markers.clone(),
        }
    }

    fn to_project(&self) -> Project {
        let mut p = Project::new(&self.name);
        p.fps = self.fps;
        p.width = self.width;
        p.height = self.height;
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

/// Parse a slider value (`0..1`) from a palette argument, else the neutral
/// midpoint — a malformed number never wedges the build.
fn parse_unit(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.5).clamp(0.0, 1.0)
}

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
        root = root.with_action(ActionSpec::prompted(
            "rename-project",
            "Rename project",
            "Name",
            "project name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut next = snap.clone();
                next.name = name.trim().to_string();
                encode(&set_project_meta_action(&next))
            },
        ));
    }

    // Project-level: a timeline marker at the playhead, named.
    {
        let snap = p.clone();
        let at = ctx.playhead;
        root = root.with_action(ActionSpec::prompted(
            "add-marker",
            "Add marker at playhead",
            "Marker name",
            "Marker",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut next = snap.clone();
                let name = if name.trim().is_empty() {
                    "Marker"
                } else {
                    name.trim()
                };
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
        root = root.with_action(ActionSpec::prompted(
            "add-title",
            "Add title",
            "The words",
            "Title",
            ExecutionLocus::ClientLocal,
            move |text: &str| {
                let content = if text.trim().is_empty() {
                    "Title"
                } else {
                    text.trim()
                };
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
        tr = tr.with_action(ActionSpec::prompted(
            "rename-track",
            "Rename track",
            "Name",
            "track name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut next = rename_snap.clone();
                next.name = name.trim().to_string();
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

            // Effects stack — add one by name, or clear them all.
            let add_base = clip.clone();
            cr = cr.with_action(ActionSpec::prompted(
                "add-effect",
                "Add effect",
                "grayscale / sepia / invert / warm / cool / vignette",
                "grayscale",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = add_base.clone();
                    if let Some(k) = EffectKind::from_str(arg) {
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
            cr = cr.with_action(ActionSpec::prompted(
                "set-speed",
                "Set speed",
                "Rate, e.g. 0.5 / 1 / 2",
                "2",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = speed_base.clone();
                    let rate = arg
                        .trim()
                        .trim_end_matches(['x', 'X', '\u{d7}'])
                        .parse()
                        .unwrap_or(1.0);
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
                cr = cr.with_action(ActionSpec::prompted(
                    id,
                    label,
                    match axis {
                        GeomAxis::Rotation => "Degrees, e.g. -15",
                        GeomAxis::Scale => "Multiplier, e.g. 0.5",
                        _ => "Frame widths, e.g. -0.25",
                    },
                    match axis {
                        GeomAxis::Scale => "0.5",
                        GeomAxis::Rotation => "15",
                        _ => "0.25",
                    },
                    ExecutionLocus::ClientLocal,
                    move |arg: &str| {
                        let v: f64 = arg.trim().parse().unwrap_or_default();
                        let mut next = base.clone();
                        match axis {
                            GeomAxis::Scale => {
                                next.transform.scale_x = v;
                                next.transform.scale_y = v;
                            }
                            GeomAxis::PosX => next.transform.pos_x = v,
                            GeomAxis::PosY => next.transform.pos_y = v,
                            GeomAxis::Rotation => next.transform.rotation = v,
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
                cr = cr.with_action(ActionSpec::prompted(
                    id,
                    label,
                    "Fraction cut off, e.g. 0.1",
                    "0.1",
                    ExecutionLocus::ClientLocal,
                    move |arg: &str| {
                        let v = parse_unit(arg);
                        let mut next = base.clone();
                        match edge {
                            CropEdge::Left => next.transform.crop_left = v,
                            CropEdge::Right => next.transform.crop_right = v,
                            CropEdge::Top => next.transform.crop_top = v,
                            CropEdge::Bottom => next.transform.crop_bottom = v,
                        }
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
                cr = cr.with_action(ActionSpec::prompted(
                    "add-clip-marker",
                    "Add marker on this clip",
                    "Marker name",
                    "Marker",
                    ExecutionLocus::ClientLocal,
                    move |arg: &str| {
                        let mut next = marker_base.clone();
                        let name = if arg.trim().is_empty() {
                            "Marker"
                        } else {
                            arg.trim()
                        };
                        next.add_marker(at, name, next.palette);
                        encode(&upsert_clip_action(&next))
                    },
                ));
            }

            // Transition at the head cut — prompted by name.
            let base = clip.clone();
            cr = cr.with_action(ActionSpec::prompted(
                "set-transition",
                "Set transition",
                "cut / dissolve / fade / dip / wipe",
                "dissolve",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = base.clone();
                    next.transition = parse_transition(arg);
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
            mr = mr.with_action(ActionSpec::prompted(
                "set-title-text",
                "Set title text",
                "The words",
                &text.content,
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    t.content = arg.to_string();
                    next.name = arg.lines().next().unwrap_or("Title").to_string();
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            let size_base = m.clone();
            mr = mr.with_action(ActionSpec::prompted(
                "set-title-size",
                "Set title size",
                "Fraction of the frame height, e.g. 0.12",
                "0.12",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = size_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    t.size = arg.trim().parse().unwrap_or(t.size);
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            let align_base = m.clone();
            mr = mr.with_action(ActionSpec::prompted(
                "set-title-align",
                "Set title alignment",
                "left / center / right",
                "center",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = align_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    t.align = crate::model::TextAlign::from_str(arg.trim());
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            // The title's colour, as `#rrggbb` or `r,g,b` in 0..1. Stored as
            // a real value rather than a palette slot: a title's colour is
            // authored content and has to survive a theme change.
            let color_base = m.clone();
            mr = mr.with_action(ActionSpec::prompted(
                "set-title-colour",
                "Set title colour",
                "#ffcc00, or r,g,b in 0..1",
                "#ffffff",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = color_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    if let Some(rgb) = parse_colour(arg) {
                        t.color = rgb;
                    }
                    next.text = Some(t);
                    encode(&upsert_media_action(&next))
                },
            ));
            let backdrop_base = m.clone();
            mr = mr.with_action(ActionSpec::prompted(
                "set-title-backdrop",
                "Set title backdrop",
                "Opacity 0-1 of the plate behind the words",
                "0.3",
                ExecutionLocus::ClientLocal,
                move |arg: &str| {
                    let mut next = backdrop_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    t.backdrop = parse_unit(arg);
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

        mr = mr.with_action(ActionSpec::prompted(
            "rename-media",
            "Rename media",
            "Name",
            "clip name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut next = rename_snap.clone();
                next.name = name.trim().to_string();
                encode(&upsert_media_action(&next))
            },
        ));
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

/// Which geometric channel a prompted transform action drives.
#[derive(Clone, Copy)]
enum GeomAxis {
    Scale,
    PosX,
    PosY,
    Rotation,
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
