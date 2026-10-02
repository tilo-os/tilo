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
//! * **View operation (presentation)** — reveal a track, clip, marker or media
//!   resource. These are brokered and agent-drivable but never touch the CRDT,
//!   replicate, or enter undo history. Scrub, zoom and transport remain local
//!   implementation details rather than document actions.
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

use tilo_actions::{
    Action, ActionAddress, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry,
    ActionSpec, Doc, EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType,
    Resource, ResultSpec, RetryPolicy,
};

use crate::curve::Curve;
use crate::doc::VideoDoc;
use crate::model::{
    ChromaKey, Clip, EffectKind, Filters, MAX_GAIN_DB, MIN_GAIN_DB, Mask, MaskShape, MediaItem,
    MediaKind, Project, TextSource, Track, TrackKind, Transform, Transition,
};
use crate::param::{ClipAnimation, Interp, Param};

/// Registry `type_id` for every video-editor action.
pub const VIDEO_EDITOR_ACTION_TYPE: &str = "video_editor.action";
pub const VIDEO_EDITOR_OPERATION_TYPE: &str = "video_editor.operation";

pub fn track_resource_id(id: Uuid) -> String {
    format!("video.track:{id}")
}

pub fn clip_resource_id(id: Uuid) -> String {
    format!("video.clip:{id}")
}

/// A stack entry is an authored value, not its current ordinal. Pairing the
/// immutable effect id with its owner keeps duplicate effects distinct and
/// makes a stale row fail closed after a reorder or delete.
pub fn effect_resource_id(clip: Uuid, effect: Uuid) -> String {
    format!("video.effect:{clip}:{effect}")
}

pub fn marker_resource_id(id: Uuid) -> String {
    format!("video.marker:{id}")
}

pub fn media_resource_id(id: Uuid) -> String {
    format!("video.media:{id}")
}

pub fn reveal_track_address(id: Uuid) -> ActionAddress {
    ActionAddress::new(track_resource_id(id), "open")
}

pub fn reveal_clip_address(id: Uuid) -> ActionAddress {
    ActionAddress::new(clip_resource_id(id), "open")
}

pub fn reveal_effect_address(clip: Uuid, effect: Uuid) -> ActionAddress {
    ActionAddress::new(effect_resource_id(clip, effect), "open")
}

pub fn reveal_marker_address(id: Uuid) -> ActionAddress {
    ActionAddress::new(marker_resource_id(id), "open")
}

pub fn reveal_media_address(id: Uuid) -> ActionAddress {
    ActionAddress::new(media_resource_id(id), "open")
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoEditorOperation {
    RevealTrack {
        track: String,
    },
    RevealClip {
        clip: String,
    },
    RevealEffect {
        clip: String,
        effect: String,
    },
    RevealMarker {
        marker: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        clip: Option<String>,
    },
    RevealMedia {
        media: String,
    },
    ImportMedia {
        path: Option<String>,
    },
    OpenProject {
        path: Option<String>,
    },
    SaveProject {
        path: Option<String>,
    },
    SaveProjectAs {
        path: Option<String>,
    },
    Export {
        path: Option<String>,
    },
    CancelExport,
}

pub fn operation_kind(operation: &VideoEditorOperation) -> OperationKind {
    match operation {
        VideoEditorOperation::RevealTrack { .. }
        | VideoEditorOperation::RevealClip { .. }
        | VideoEditorOperation::RevealEffect { .. }
        | VideoEditorOperation::RevealMarker { .. }
        | VideoEditorOperation::RevealMedia { .. } => OperationKind::View,
        VideoEditorOperation::ImportMedia { .. }
        | VideoEditorOperation::OpenProject { .. }
        | VideoEditorOperation::SaveProject { .. }
        | VideoEditorOperation::SaveProjectAs { .. }
        | VideoEditorOperation::Export { .. }
        | VideoEditorOperation::CancelExport => OperationKind::Effect,
    }
}

#[allow(
    clippy::expect_used,
    reason = "the passed enum is a serde-derived local value, so ActionSpec encoding is infallible by construction"
)]
fn operation(
    id: &str,
    label: &str,
    kind: OperationKind,
    value: &VideoEditorOperation,
) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, VIDEO_EDITOR_OPERATION_TYPE, value)
        .expect("VideoEditorOperation is always serializable");
    match value {
        VideoEditorOperation::RevealTrack { .. }
        | VideoEditorOperation::RevealClip { .. }
        | VideoEditorOperation::RevealEffect { .. }
        | VideoEditorOperation::RevealMarker { .. }
        | VideoEditorOperation::RevealMedia { .. } => spec
            .with_contract(ActionContract::view())
            .retry_policy(RetryPolicy::Idempotent),
        _ => spec,
    }
}

/// Decode a palette operation payload, or `None` when `type_id` is not the
/// editor's operation type.
///
/// # Errors
///
/// The deserializer's message when the payload is not a
/// [`VideoEditorOperation`].
pub fn decode_operation(
    type_id: &str,
    payload: &[u8],
) -> Result<Option<VideoEditorOperation>, String> {
    if type_id != VIDEO_EDITOR_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|e| e.to_string())
}

/// A serializable snapshot of a clip's scalar fields, in exactly the
/// representation [`crate::doc`] stores (ids as strings, transition as its
/// stable tag). Carrying the document shape keeps [`Action::apply`] a pure
/// write with no re-derivation, and makes [`Self::from_clip`]/`Self::to_clip`
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
    /// The ordered effects stack, carried verbatim by explicit full
    /// replacements; ordinary effect edits patch only this stored key.
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
    /// The link group this clip belongs to, as its plain id. Absent on the
    /// wire for a peer that predates linked audio and video, which
    /// `serde(default)` reads as "not linked" -- the independent clip that
    /// peer is already editing, so the two do not diverge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Whether this picture clip's sound has been split onto an audio clip.
    /// Absent on the wire for a peer that predates the split, which
    /// `serde(default)` reads as "still carries its own sound" -- what that
    /// peer is already playing.
    #[serde(default)]
    pub sound_split: bool,
    /// Whether edits follow this clip's group. Travels so unlinking
    /// replicates; absent in an older payload, where a group meant linked.
    #[serde(default = "crate::model::yes_default")]
    pub link_follows: bool,
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
            link: c.link.map(|l| l.to_string()),
            sound_split: c.sound_split,
            link_follows: c.link_follows,
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
            transition: Transition::from_name(&self.transition),
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
            // An unparseable group id is dropped rather than fabricated: the
            // clip lands unlinked, which is a state the editor already knows
            // how to draw, instead of joining a group that does not exist.
            link: self.link.as_ref().and_then(|l| l.parse::<Uuid>().ok()),
            sound_split: self.sound_split,
            link_follows: self.link_follows,
        })
    }
}

/// A serializable snapshot of a track's scalar fields, in the document's shape.
///
/// `PartialEq` but not `Eq`: the fader and the pan are real numbers, and
/// there is no total order on those worth pretending to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The lane's fader in decibels, and where it sits across the stereo
    /// field. Absent on the wire for a peer that predates the mixer, which
    /// `serde(default)` reads as unity and centre — the level that peer is
    /// already hearing, so the two do not diverge.
    #[serde(default)]
    pub gain_db: f64,
    #[serde(default)]
    pub pan: f64,
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
            gain_db: t.gain_db,
            pan: t.pan,
        }
    }

    fn to_track(&self) -> Option<Track> {
        Some(Track {
            id: self.id.parse::<Uuid>().ok()?,
            name: self.name.clone(),
            kind: TrackKind::from_name(&self.kind),
            index: self.index,
            muted: self.muted,
            locked: self.locked,
            solo: self.solo,
            // Clamped on the way in: a payload from a peer built with a wider
            // ceiling must not put this lane at a level no control here can
            // reach — the fader would then be unable to undo what arrived.
            gain_db: self.gain_db.clamp(MIN_GAIN_DB, MAX_GAIN_DB),
            pan: self.pan.clamp(-1.0, 1.0),
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
            kind: MediaKind::from_name(&self.kind),
            duration: self.duration,
            width: self.width,
            height: self.height,
            fps: self.fps,
            palette: self.palette,
            path: self.path.clone(),
            hydration: crate::media::Hydration::from_name(&self.hydration),
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
        p.markers.clone_from(&self.markers);
        p
    }
}

/// Every collaborative operation the editor exports on its timeline.
///
/// `UpsertClip` is the durable first write for a newly-created clip. Existing
/// clips use optimistic field patches, so an ordinary UI edit cannot restore
/// unrelated state from an old snapshot. The structural multi-clip operations
/// (`RippleDeleteClip`, `SplitClip`) carry every affected clip so they replay
/// atomically as a single change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "an action is built, encoded and applied one at a time; none are stored in bulk"
)]
pub enum VideoEditorAction {
    /// Create a newly-authored clip, or explicitly replace one with a complete
    /// externally supplied snapshot. Normal editor interactions use a patch.
    UpsertClip(ClipData),
    /// Update only the independently stored clip properties named by the
    /// action, after checking the values the caller observed. Resource
    /// commands use this instead of `UpsertClip` so stale palette specs have
    /// no unrelated fields they could restore.
    PatchClipFields {
        clip: String,
        changes: Vec<ClipFieldPatch>,
    },
    /// Several existing clips patched as one optimistic action. Preconditions
    /// for every member are checked before any member is written.
    PatchClipsFields { patches: Vec<ClipPatchData> },
    /// Append one freshly-authored effect at an authored rank. Concurrent
    /// appends may share a rank; their UUIDs provide the deterministic
    /// tie-break, so replay never derives an ordinal from receiver state.
    AddEffect {
        clip: String,
        ordinal: usize,
        effect: crate::model::ClipEffect,
    },
    /// Change one named effect only if its amount still matches the state the
    /// caller saw. This is intentionally not an `UpsertClip`: a stale slider
    /// must refuse instead of restoring old trims, markers, or a reordered
    /// stack over a collaborator's current document.
    SetEffectAmount {
        clip: String,
        effect: String,
        expected_amount_bits: u64,
        amount: f64,
    },
    /// Remove the exact effect instance the caller saw. Kind and amount are
    /// an optimistic precondition so an intervening edit refuses cleanly.
    RemoveEffect {
        clip: String,
        effect: String,
        expected_kind: EffectKind,
        expected_amount_bits: u64,
    },
    /// Reorder a named instance only while it has the same index and neighbour
    /// that made this movement meaningful. `neighbour` is the entry it swaps
    /// with, so duplicate effect kinds never make an old arrow ambiguous.
    MoveEffect {
        clip: String,
        effect: String,
        expected_index: usize,
        neighbour: String,
        up: bool,
    },
    /// Clear a stack only while every current effect matches the snapshot.
    /// This is deliberately a narrow effects write, never an old whole clip.
    ClearEffects {
        clip: String,
        expected: Vec<crate::model::ClipEffect>,
    },
    /// Create or explicitly replace **several** clips in one change, together
    /// with any tracks the change needs that do not exist yet. Ordinary edits
    /// of existing linked clips use `PatchClipsFields`, so a stale snapshot
    /// cannot restore unrelated fields. `tracks` carries a lane only when the
    /// change makes one — for example, giving a shot its own audio clip in a
    /// project that has no audio lane yet.
    UpsertClips {
        #[serde(default)]
        tracks: Vec<TrackData>,
        clips: Vec<ClipData>,
    },
    /// Delete **several** clips in one change, then write `moved` — the
    /// post-shift position of every clip a ripple pushed left.
    ///
    /// `moved` is empty for a plain delete, which leaves the gaps where they
    /// were. Both halves of a linked pair go in one action for the same reason
    /// they are written in one: a peer that replayed half of it would be left
    /// with an orphan.
    DeleteClips {
        ids: Vec<String>,
        #[serde(default)]
        moved: Vec<ClipData>,
    },
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
    /// Append one named timeline marker without replacing the project's
    /// marker map. Markers are resources in their own right, so this action
    /// carries the authored marker identity rather than a positional list.
    AddProjectMarker { marker: crate::model::Marker },
    /// Tombstone one timeline marker by stable id.
    RemoveProjectMarker { marker: String },
    /// Edit one text field on a project marker without replacing the marker
    /// list or project metadata.
    SetProjectMarkerText {
        marker: String,
        field: MarkerTextField,
        expected: String,
        value: String,
    },
    /// Append one marker owned by a clip. The clip is explicit so identical
    /// marker ids cannot be confused across local timelines.
    AddClipMarker {
        clip: String,
        marker: crate::model::Marker,
    },
    /// Tombstone one marker from a clip without touching its other metadata.
    RemoveClipMarker { clip: String, marker: String },
    /// Edit one text field on a clip-local marker without replacing sibling
    /// markers or an unrelated clip property.
    SetClipMarkerText {
        clip: String,
        marker: String,
        field: MarkerTextField,
        expected: String,
        value: String,
    },
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipPatchData {
    pub clip: String,
    pub changes: Vec<ClipFieldPatch>,
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

/// Equality for optimistic effect mutations. `f64::to_bits` deliberately
/// distinguishes a malformed NaN payload from an authored amount instead of
/// letting two incomparable values look like a safe concurrent match.
fn same_effect_stack(
    current: &[crate::model::ClipEffect],
    expected: &[crate::model::ClipEffect],
) -> bool {
    current.len() == expected.len()
        && current.iter().zip(expected).all(|(current, expected)| {
            current.id == expected.id
                && current.kind == expected.kind
                && current.amount.to_bits() == expected.amount.to_bits()
        })
}

/// A single durable clip property written by an externally invokable resource
/// action.  It intentionally carries only the observed property, never a
/// `ClipData` snapshot: a saved palette command must not put back an old trim,
/// lane, marker list, or effect stack after another editor changed it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipFieldPatch {
    Number {
        field: ClipNumberField,
        expected_bits: u64,
        value: f64,
    },
    Text {
        field: ClipTextField,
        expected: String,
        value: String,
    },
    /// A new, typed compound value. It is atomic at the action boundary (a
    /// curve edit or transform preset is one authored gesture), then expands
    /// into the document's keyed maps.
    Compound {
        field: ClipCompoundField,
        expected: ClipCompound,
        value: ClipCompound,
    },
    Bool {
        field: ClipBoolField,
        expected: bool,
        value: bool,
    },
    Byte {
        field: ClipByteField,
        expected: u8,
        value: u8,
    },
    /// Link membership is intentionally typed instead of being folded into a
    /// whole clip snapshot. A group action carries one such patch per member
    /// and validates all observed memberships before it writes any of them.
    Link {
        expected: Option<String>,
        value: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipBoolField {
    SpeedReverse,
    SoundSplit,
    LinkFollows,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipByteField {
    Palette,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipNumberField {
    Start,
    InPoint,
    OutPoint,
    Opacity,
    Volume,
    Brightness,
    Contrast,
    Saturation,
    FadeIn,
    FadeOut,
    TransitionDuration,
    SpeedRate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipTextField {
    Track,
    Transition,
}

/// One independently editable text cell on a stable marker record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarkerTextField {
    Name,
    Note,
}

/// Named compound clip records. Each is represented by leaf CRDT fields or
/// keyed child records in the document; this enum keeps the action protocol
/// typed without making a UI gesture depend on an opaque JSON blob.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipCompoundField {
    Transform,
    Curves,
    Key,
    Mask,
    Animation,
    Markers,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipCompound {
    Transform(Transform),
    Curves(Curve),
    Key(ChromaKey),
    Mask(Mask),
    Animation(ClipAnimation),
    Markers(Vec<crate::model::Marker>),
}

#[expect(
    clippy::too_many_lines,
    reason = "one comparison per patchable clip field, read top to bottom"
)]
pub fn clip_field_patch_action(before: &Clip, after: &Clip) -> VideoEditorAction {
    let mut changes = Vec::new();
    for (field, expected, value) in [
        (ClipNumberField::Start, before.start, after.start),
        (ClipNumberField::InPoint, before.in_point, after.in_point),
        (ClipNumberField::OutPoint, before.out_point, after.out_point),
        (ClipNumberField::Opacity, before.opacity, after.opacity),
        (ClipNumberField::Volume, before.volume, after.volume),
        (
            ClipNumberField::Brightness,
            before.filters.brightness,
            after.filters.brightness,
        ),
        (
            ClipNumberField::Contrast,
            before.filters.contrast,
            after.filters.contrast,
        ),
        (
            ClipNumberField::Saturation,
            before.filters.saturation,
            after.filters.saturation,
        ),
        (ClipNumberField::FadeIn, before.fade_in, after.fade_in),
        (ClipNumberField::FadeOut, before.fade_out, after.fade_out),
        (
            ClipNumberField::TransitionDuration,
            before.transition_duration,
            after.transition_duration,
        ),
        (
            ClipNumberField::SpeedRate,
            before.speed.rate,
            after.speed.rate,
        ),
    ] {
        if expected.to_bits() != value.to_bits() {
            changes.push(ClipFieldPatch::Number {
                field,
                expected_bits: expected.to_bits(),
                value,
            });
        }
    }
    if before.palette != after.palette {
        changes.push(ClipFieldPatch::Byte {
            field: ClipByteField::Palette,
            expected: before.palette,
            value: after.palette,
        });
    }
    if before.link != after.link {
        changes.push(ClipFieldPatch::Link {
            expected: before.link.map(|link| link.to_string()),
            value: after.link.map(|link| link.to_string()),
        });
    }
    if before.track != after.track {
        changes.push(ClipFieldPatch::Text {
            field: ClipTextField::Track,
            expected: before.track.to_string(),
            value: after.track.to_string(),
        });
    }
    for (field, expected, value) in [
        (
            ClipBoolField::SpeedReverse,
            before.speed.reverse,
            after.speed.reverse,
        ),
        (
            ClipBoolField::SoundSplit,
            before.sound_split,
            after.sound_split,
        ),
        (
            ClipBoolField::LinkFollows,
            before.link_follows,
            after.link_follows,
        ),
    ] {
        if expected != value {
            changes.push(ClipFieldPatch::Bool {
                field,
                expected,
                value,
            });
        }
    }
    if before.transition != after.transition {
        changes.push(ClipFieldPatch::Text {
            field: ClipTextField::Transition,
            expected: before.transition.as_str().to_string(),
            value: after.transition.as_str().to_string(),
        });
    }
    for (field, expected, value) in [
        (
            ClipCompoundField::Transform,
            ClipCompound::Transform(before.transform),
            ClipCompound::Transform(after.transform),
        ),
        (
            ClipCompoundField::Curves,
            ClipCompound::Curves(before.curves.clone()),
            ClipCompound::Curves(after.curves.clone()),
        ),
        (
            ClipCompoundField::Key,
            ClipCompound::Key(before.key),
            ClipCompound::Key(after.key),
        ),
        (
            ClipCompoundField::Mask,
            ClipCompound::Mask(before.mask),
            ClipCompound::Mask(after.mask),
        ),
        (
            ClipCompoundField::Animation,
            ClipCompound::Animation(before.anim.clone()),
            ClipCompound::Animation(after.anim.clone()),
        ),
        (
            ClipCompoundField::Markers,
            ClipCompound::Markers(before.markers.clone()),
            ClipCompound::Markers(after.markers.clone()),
        ),
    ] {
        if expected != value {
            changes.push(ClipFieldPatch::Compound {
                field,
                expected,
                value,
            });
        }
    }
    VideoEditorAction::PatchClipFields {
        clip: before.id.to_string(),
        changes,
    }
}

fn patch_matches(clip: &Clip, patch: &ClipFieldPatch) -> bool {
    match patch {
        ClipFieldPatch::Number {
            field,
            expected_bits,
            ..
        } => {
            let current = match field {
                ClipNumberField::Start => clip.start,
                ClipNumberField::InPoint => clip.in_point,
                ClipNumberField::OutPoint => clip.out_point,
                ClipNumberField::Opacity => clip.opacity,
                ClipNumberField::Volume => clip.volume,
                ClipNumberField::Brightness => clip.filters.brightness,
                ClipNumberField::Contrast => clip.filters.contrast,
                ClipNumberField::Saturation => clip.filters.saturation,
                ClipNumberField::FadeIn => clip.fade_in,
                ClipNumberField::FadeOut => clip.fade_out,
                ClipNumberField::TransitionDuration => clip.transition_duration,
                ClipNumberField::SpeedRate => clip.speed.rate,
            };
            current.to_bits() == *expected_bits
        }
        ClipFieldPatch::Text {
            field, expected, ..
        } => match field {
            ClipTextField::Track => clip.track.to_string() == *expected,
            ClipTextField::Transition => clip.transition.as_str() == expected,
        },
        ClipFieldPatch::Compound {
            field, expected, ..
        } => {
            let current = match field {
                ClipCompoundField::Transform => ClipCompound::Transform(clip.transform),
                ClipCompoundField::Curves => ClipCompound::Curves(clip.curves.clone()),
                ClipCompoundField::Key => ClipCompound::Key(clip.key),
                ClipCompoundField::Mask => ClipCompound::Mask(clip.mask),
                ClipCompoundField::Animation => ClipCompound::Animation(clip.anim.clone()),
                ClipCompoundField::Markers => ClipCompound::Markers(clip.markers.clone()),
            };
            current == *expected
        }
        ClipFieldPatch::Bool {
            field, expected, ..
        } => match field {
            ClipBoolField::SpeedReverse => clip.speed.reverse == *expected,
            ClipBoolField::SoundSplit => clip.sound_split == *expected,
            ClipBoolField::LinkFollows => clip.link_follows == *expected,
        },
        ClipFieldPatch::Byte {
            field: ClipByteField::Palette,
            expected,
            ..
        } => clip.palette == *expected,
        ClipFieldPatch::Link { expected, .. } => {
            clip.link.map(|link| link.to_string()) == *expected
        }
    }
}

fn write_patch(doc: &mut Doc, id: Uuid, patch: &ClipFieldPatch) -> bool {
    match patch {
        ClipFieldPatch::Number { field, value, .. } => VideoDoc::set_existing_clip_number(
            doc,
            id,
            match field {
                ClipNumberField::Start => "start",
                ClipNumberField::InPoint => "in",
                ClipNumberField::OutPoint => "out",
                ClipNumberField::Opacity => "opacity",
                ClipNumberField::Volume => "volume",
                ClipNumberField::Brightness => "brightness",
                ClipNumberField::Contrast => "contrast",
                ClipNumberField::Saturation => "saturation",
                ClipNumberField::FadeIn => "fade_in",
                ClipNumberField::FadeOut => "fade_out",
                ClipNumberField::TransitionDuration => "transition_duration",
                ClipNumberField::SpeedRate => "speed_rate",
            },
            *value,
        ),
        ClipFieldPatch::Text { field, value, .. } => VideoDoc::set_existing_clip_text(
            doc,
            id,
            match field {
                ClipTextField::Track => "track",
                ClipTextField::Transition => "transition",
            },
            value,
        ),
        ClipFieldPatch::Compound { value, .. } => match value {
            ClipCompound::Transform(value) => {
                VideoDoc::set_existing_clip_transform(doc, id, *value)
            }
            ClipCompound::Curves(value) => VideoDoc::set_existing_clip_curves(doc, id, value),
            ClipCompound::Key(value) => VideoDoc::set_existing_clip_key(doc, id, *value),
            ClipCompound::Mask(value) => VideoDoc::set_existing_clip_mask(doc, id, *value),
            ClipCompound::Animation(value) => VideoDoc::set_existing_clip_animation(doc, id, value),
            ClipCompound::Markers(value) => VideoDoc::set_existing_clip_markers(doc, id, value),
        },
        ClipFieldPatch::Bool { field, value, .. } => VideoDoc::set_existing_clip_bool(
            doc,
            id,
            match field {
                ClipBoolField::SpeedReverse => "speed_reverse",
                ClipBoolField::SoundSplit => "sound_split",
                ClipBoolField::LinkFollows => "link_follows",
            },
            *value,
        ),
        ClipFieldPatch::Byte { field, value, .. } => VideoDoc::set_existing_clip_u8(
            doc,
            id,
            match field {
                ClipByteField::Palette => "palette",
            },
            *value,
        ),
        ClipFieldPatch::Link { value, .. } => {
            VideoDoc::set_existing_clip_link(doc, id, value.as_deref())
        }
    }
}

/// Reject ambiguous or malformed patch payloads before looking at or changing
/// the document. In particular, two writes to one key would make a bulk link
/// action depend on payload order instead of the state each collaborator saw.
fn validate_patch_set(changes: &[ClipFieldPatch]) -> Result<(), ActionError> {
    let compound_matches_field = |field: ClipCompoundField, value: &ClipCompound| {
        matches!(
            (field, value),
            (ClipCompoundField::Transform, ClipCompound::Transform(_))
                | (ClipCompoundField::Curves, ClipCompound::Curves(_))
                | (ClipCompoundField::Key, ClipCompound::Key(_))
                | (ClipCompoundField::Mask, ClipCompound::Mask(_))
                | (ClipCompoundField::Animation, ClipCompound::Animation(_))
                | (ClipCompoundField::Markers, ClipCompound::Markers(_))
        )
    };
    let key = |patch: &ClipFieldPatch| match patch {
        ClipFieldPatch::Number { field, .. } => match field {
            ClipNumberField::Start => "start",
            ClipNumberField::InPoint => "in",
            ClipNumberField::OutPoint => "out",
            ClipNumberField::Opacity => "opacity",
            ClipNumberField::Volume => "volume",
            ClipNumberField::Brightness => "brightness",
            ClipNumberField::Contrast => "contrast",
            ClipNumberField::Saturation => "saturation",
            ClipNumberField::FadeIn => "fade_in",
            ClipNumberField::FadeOut => "fade_out",
            ClipNumberField::TransitionDuration => "transition_duration",
            ClipNumberField::SpeedRate => "speed_rate",
        },
        ClipFieldPatch::Text { field, .. } => match field {
            ClipTextField::Track => "track",
            ClipTextField::Transition => "transition",
        },
        ClipFieldPatch::Compound { field, .. } => match field {
            ClipCompoundField::Transform => "transform",
            ClipCompoundField::Curves => "curves",
            ClipCompoundField::Key => "key",
            ClipCompoundField::Mask => "mask",
            ClipCompoundField::Animation => "anim",
            ClipCompoundField::Markers => "markers",
        },
        ClipFieldPatch::Bool { field, .. } => match field {
            ClipBoolField::SpeedReverse => "speed_reverse",
            ClipBoolField::SoundSplit => "sound_split",
            ClipBoolField::LinkFollows => "link_follows",
        },
        ClipFieldPatch::Byte { field, .. } => match field {
            ClipByteField::Palette => "palette",
        },
        ClipFieldPatch::Link { .. } => "link",
    };
    if changes.iter().enumerate().any(|(index, patch)| {
        changes
            .iter()
            .take(index)
            .any(|previous| key(previous) == key(patch))
    }) {
        return Err(ActionError::Refused(
            "patch writes one clip property more than once".into(),
        ));
    }
    if changes.iter().any(|patch| {
        matches!(patch, ClipFieldPatch::Number { value, .. } if !value.is_finite())
            || matches!(patch, ClipFieldPatch::Link { value: Some(value), .. } if value.parse::<Uuid>().is_err())
            || matches!(patch, ClipFieldPatch::Compound { field, expected, value }
                if !compound_matches_field(*field, expected)
                    || !compound_matches_field(*field, value))
    }) {
        return Err(ActionError::Refused("invalid clip patch value".into()));
    }
    Ok(())
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
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per action variant, each the whole document write for that action"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            VideoEditorAction::UpsertClip(data) => {
                if let Some(c) = data.to_clip() {
                    VideoDoc::upsert_clip_preserving_nested(doc, &c);
                }
            }
            VideoEditorAction::PatchClipFields { clip, changes } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid patch clip id".into()))?;
                // A typed builder with no bound value intentionally encodes a
                // no-op rather than inventing a default edit. It carries no
                // stale state and therefore needs no target lookup.
                if changes.is_empty() {
                    return Ok(());
                }
                validate_patch_set(changes)?;
                let project = VideoDoc::load_project(doc);
                let current = project
                    .clip(clip_id)
                    .ok_or_else(|| ActionError::Refused("clip is no longer available".into()))?;
                if !changes.iter().all(|patch| patch_matches(current, patch)) {
                    return Err(ActionError::Refused(
                        "clip property changed before this action could apply".into(),
                    ));
                }
                if changes
                    .iter()
                    .any(|patch| !write_patch(doc, clip_id, patch))
                {
                    return Err(ActionError::Refused("clip is no longer available".into()));
                }
            }
            VideoEditorAction::PatchClipsFields { patches } => {
                // Validate the *complete* grouped payload before reading the
                // document. The following collect then checks every member
                // against one projection before the first CRDT write.
                for patch in patches {
                    validate_patch_set(&patch.changes)?;
                }
                if patches.iter().enumerate().any(|(index, patch)| {
                    patches
                        .iter()
                        .take(index)
                        .any(|previous| previous.clip == patch.clip)
                }) {
                    return Err(ActionError::Refused(
                        "bulk patch names one clip more than once".into(),
                    ));
                }
                let project = VideoDoc::load_project(doc);
                let parsed: Result<Vec<_>, _> = patches
                    .iter()
                    .map(|patch| {
                        let id = patch
                            .clip
                            .parse::<Uuid>()
                            .map_err(|_| ActionError::Refused("invalid patch clip id".into()))?;
                        let current = project.clip(id).ok_or_else(|| {
                            ActionError::Refused("clip is no longer available".into())
                        })?;
                        if !patch
                            .changes
                            .iter()
                            .all(|change| patch_matches(current, change))
                        {
                            return Err(ActionError::Refused(
                                "clip property changed before this action could apply".into(),
                            ));
                        }
                        Ok((id, &patch.changes))
                    })
                    .collect();
                for (id, changes) in parsed? {
                    if changes.iter().any(|change| !write_patch(doc, id, change)) {
                        return Err(ActionError::Refused("clip is no longer available".into()));
                    }
                }
            }
            VideoEditorAction::AddEffect {
                clip,
                ordinal,
                effect,
            } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect clip id".into()))?;
                let project = VideoDoc::load_project(doc);
                {
                    project.clip(clip_id).ok_or_else(|| {
                        ActionError::Refused("effect clip is no longer available".into())
                    })?;
                    if effect.id.is_nil() {
                        return Err(ActionError::Refused("invalid authored effect id".into()));
                    }
                }
                if !VideoDoc::add_clip_effect(doc, clip_id, *effect, *ordinal) {
                    return Err(ActionError::Refused(
                        "effect clip is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::SetEffectAmount {
                clip,
                effect,
                expected_amount_bits,
                amount,
            } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect clip id".into()))?;
                let effect_id = effect
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect id".into()))?;
                if !amount.is_finite() || !(0.0..=1.0).contains(amount) {
                    return Err(ActionError::Refused(
                        "effect amount is outside 0 to 1".into(),
                    ));
                }
                let project = VideoDoc::load_project(doc);
                {
                    let clip = project.clip(clip_id).ok_or_else(|| {
                        ActionError::Refused("effect clip is no longer available".into())
                    })?;
                    let instance = clip
                        .effects
                        .iter()
                        .find(|instance| instance.id == effect_id)
                        .ok_or_else(|| {
                            ActionError::Refused("effect is no longer available".into())
                        })?;
                    if instance.amount.to_bits() != *expected_amount_bits {
                        return Err(ActionError::Refused(
                            "effect amount changed before this edit could apply".into(),
                        ));
                    }
                }
                if !VideoDoc::set_clip_effect_amount(doc, clip_id, effect_id, *amount) {
                    return Err(ActionError::Refused(
                        "effect clip is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::RemoveEffect {
                clip,
                effect,
                expected_kind,
                expected_amount_bits,
            } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect clip id".into()))?;
                let effect_id = effect
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect id".into()))?;
                let mut project = VideoDoc::load_project(doc);
                let (effects, animation) = {
                    let clip = project.clip_mut(clip_id).ok_or_else(|| {
                        ActionError::Refused("effect clip is no longer available".into())
                    })?;
                    let index = clip.effect_index(effect_id).ok_or_else(|| {
                        ActionError::Refused("effect is no longer available".into())
                    })?;
                    let instance = clip.effects.get(index).copied().ok_or_else(|| {
                        ActionError::Refused("effect is no longer available".into())
                    })?;
                    if instance.kind != *expected_kind
                        || instance.amount.to_bits() != *expected_amount_bits
                    {
                        return Err(ActionError::Refused(
                            "effect changed before it could be removed".into(),
                        ));
                    }
                    clip.remove_effect(index);
                    (clip.effects.clone(), clip.anim.clone())
                };
                if !VideoDoc::set_clip_effects_and_animation(doc, clip_id, &effects, &animation) {
                    return Err(ActionError::Refused(
                        "effect clip is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::MoveEffect {
                clip,
                effect,
                expected_index,
                neighbour,
                up,
            } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect clip id".into()))?;
                let effect_id = effect
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect id".into()))?;
                let neighbour_id = neighbour
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect neighbour id".into()))?;
                let mut project = VideoDoc::load_project(doc);
                let (effects, animation) = {
                    let clip = project.clip_mut(clip_id).ok_or_else(|| {
                        ActionError::Refused("effect clip is no longer available".into())
                    })?;
                    let index = clip.effect_index(effect_id).ok_or_else(|| {
                        ActionError::Refused("effect is no longer available".into())
                    })?;
                    let Some(target) = (if *up {
                        index.checked_sub(1)
                    } else {
                        (index + 1 < clip.effects.len()).then_some(index + 1)
                    }) else {
                        return Err(ActionError::Refused(
                            "effect can no longer move in that direction".into(),
                        ));
                    };
                    if index != *expected_index
                        || clip.effects.get(target).map(|e| e.id) != Some(neighbour_id)
                    {
                        return Err(ActionError::Refused(
                            "effect order changed before this move could apply".into(),
                        ));
                    }
                    if *up {
                        clip.move_effect_up(index);
                    } else {
                        clip.move_effect_down(index);
                    }
                    (clip.effects.clone(), clip.anim.clone())
                };
                if !VideoDoc::set_clip_effects_and_animation(doc, clip_id, &effects, &animation) {
                    return Err(ActionError::Refused(
                        "effect clip is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::ClearEffects { clip, expected } => {
                let clip_id = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid effect clip id".into()))?;
                let mut project = VideoDoc::load_project(doc);
                let (effects, animation) = {
                    let current = project.clip_mut(clip_id).ok_or_else(|| {
                        ActionError::Refused("effect clip is no longer available".into())
                    })?;
                    if !same_effect_stack(&current.effects, expected) {
                        return Err(ActionError::Refused(
                            "effect stack changed before it could be cleared".into(),
                        ));
                    }
                    current.clear_effects();
                    (current.effects.clone(), current.anim.clone())
                };
                if !VideoDoc::set_clip_effects_and_animation(doc, clip_id, &effects, &animation) {
                    return Err(ActionError::Refused(
                        "effect clip is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::AddProjectMarker { marker } => {
                if marker.id.is_nil() || !marker.t.is_finite() {
                    return Err(ActionError::Refused("invalid project marker".into()));
                }
                if !VideoDoc::add_project_marker(doc, marker) {
                    return Err(ActionError::Refused(
                        "project marker is already present or project vanished".into(),
                    ));
                }
            }
            VideoEditorAction::RemoveProjectMarker { marker } => {
                let marker = marker
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid project marker id".into()))?;
                if !VideoDoc::remove_project_marker(doc, marker) {
                    return Err(ActionError::Refused(
                        "project marker is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::SetProjectMarkerText {
                marker,
                field,
                expected,
                value,
            } => {
                let marker = marker
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid project marker id".into()))?;
                if *field == MarkerTextField::Name && value.trim().is_empty() {
                    return Err(ActionError::Refused("marker name cannot be empty".into()));
                }
                let project = VideoDoc::load_project(doc);
                let current = project.marker(marker).ok_or_else(|| {
                    ActionError::Refused("project marker is no longer available".into())
                })?;
                let observed = match field {
                    MarkerTextField::Name => &current.name,
                    MarkerTextField::Note => &current.note,
                };
                if observed != expected
                    || !VideoDoc::set_project_marker_text(
                        doc,
                        marker,
                        match field {
                            MarkerTextField::Name => "name",
                            MarkerTextField::Note => "note",
                        },
                        value,
                    )
                {
                    return Err(ActionError::Refused(
                        "project marker changed before this edit could apply".into(),
                    ));
                }
            }
            VideoEditorAction::AddClipMarker { clip, marker } => {
                let clip = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid clip marker clip id".into()))?;
                if marker.id.is_nil() || !marker.t.is_finite() {
                    return Err(ActionError::Refused("invalid clip marker".into()));
                }
                if !VideoDoc::add_clip_marker(doc, clip, marker) {
                    return Err(ActionError::Refused(
                        "clip marker is already present or clip vanished".into(),
                    ));
                }
            }
            VideoEditorAction::RemoveClipMarker { clip, marker } => {
                let clip = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid clip marker clip id".into()))?;
                let marker = marker
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid clip marker id".into()))?;
                if !VideoDoc::remove_clip_marker(doc, clip, marker) {
                    return Err(ActionError::Refused(
                        "clip marker is no longer available".into(),
                    ));
                }
            }
            VideoEditorAction::SetClipMarkerText {
                clip,
                marker,
                field,
                expected,
                value,
            } => {
                let clip = clip
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid clip marker clip id".into()))?;
                let marker = marker
                    .parse::<Uuid>()
                    .map_err(|_| ActionError::Refused("invalid clip marker id".into()))?;
                if *field == MarkerTextField::Name && value.trim().is_empty() {
                    return Err(ActionError::Refused("marker name cannot be empty".into()));
                }
                let project = VideoDoc::load_project(doc);
                let current = project
                    .clip(clip)
                    .and_then(|clip| clip.markers.iter().find(|entry| entry.id == marker))
                    .ok_or_else(|| {
                        ActionError::Refused("clip marker is no longer available".into())
                    })?;
                let observed = match field {
                    MarkerTextField::Name => &current.name,
                    MarkerTextField::Note => &current.note,
                };
                if observed != expected
                    || !VideoDoc::set_clip_marker_text(
                        doc,
                        clip,
                        marker,
                        match field {
                            MarkerTextField::Name => "name",
                            MarkerTextField::Note => "note",
                        },
                        value,
                    )
                {
                    return Err(ActionError::Refused(
                        "clip marker changed before this edit could apply".into(),
                    ));
                }
            }
            VideoEditorAction::UpsertClips { tracks, clips } => {
                for data in tracks {
                    if let Some(t) = data.to_track() {
                        VideoDoc::upsert_track(doc, &t);
                    }
                }
                for data in clips {
                    if let Some(c) = data.to_clip() {
                        VideoDoc::upsert_clip_preserving_nested(doc, &c);
                    }
                }
            }
            VideoEditorAction::DeleteClips { ids, moved } => {
                for id in ids {
                    if let Ok(uuid) = id.parse::<Uuid>() {
                        VideoDoc::remove_clip(doc, uuid);
                    }
                }
                for m in moved {
                    if let Some(c) = m.to_clip() {
                        VideoDoc::upsert_clip_preserving_nested(doc, &c);
                    }
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
                        VideoDoc::upsert_clip_preserving_nested(doc, &c);
                    }
                }
            }
            VideoEditorAction::SplitClip { left, right } => {
                if let Some(c) = left.to_clip() {
                    VideoDoc::upsert_clip_preserving_nested(doc, &c);
                }
                if let Some(c) = right.to_clip() {
                    VideoDoc::upsert_clip_preserving_nested(doc, &c);
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

    /// Removing work — clips, a track with its clips, a source with the clips
    /// that place it — or replacing the whole project is destructive, so it
    /// is confirmed exactly as a declared destructive action is. Every other
    /// edit (a trim, a move, a grade, a marker) applies directly.
    fn risk(&self) -> tilo_actions::RiskLevel {
        match self {
            Self::DeleteClips { .. }
            | Self::DeleteClip { .. }
            | Self::RippleDeleteClip { .. }
            | Self::DeleteTrack { .. }
            | Self::RemoveMedia { .. }
            | Self::LoadProject(_) => tilo_actions::RiskLevel::Destructive,
            _ => tilo_actions::RiskLevel::Low,
        }
    }
}

/// Register the editor's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<VideoEditorAction>(VIDEO_EDITOR_ACTION_TYPE);
}

/// Encode any [`VideoEditorAction`] into the wire form the palette and the
/// on-screen affordances both dispatch — the single point that turns the
/// editor's actions into replayable envelopes.
///
/// # Panics
///
/// Never in practice: every variant is a plain serde-derived value.
#[allow(
    clippy::expect_used,
    reason = "the serde-derived action is encoded immediately after construction and has no fallible fields"
)]
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

/// Write several newly-authored clips as one change. Existing linked edits
/// use [`patch_clips_fields_action`] instead.
pub fn upsert_clips_action(clips: &[Clip]) -> VideoEditorAction {
    upsert_tracks_and_clips_action(&[], clips)
}

/// Build one optimistic, all-or-nothing patch for existing clips that changed
/// between two local project snapshots. The caller supplies the original
/// snapshot, never a live merged CRDT projection, so remote changes to other
/// fields cannot become accidental reverse patches.
pub fn patch_clips_fields_action(
    before: &Project,
    after: &Project,
    ids: &[Uuid],
) -> VideoEditorAction {
    let patches = ids
        .iter()
        .filter_map(|id| {
            let (before, after) = (before.clip(*id)?, after.clip(*id)?);
            match clip_field_patch_action(before, after) {
                VideoEditorAction::PatchClipFields { clip, changes } => {
                    (!changes.is_empty()).then_some(ClipPatchData { clip, changes })
                }
                _ => None,
            }
        })
        .collect();
    VideoEditorAction::PatchClipsFields { patches }
}

/// Write several clips *and* the tracks they need, as one change.
pub fn upsert_tracks_and_clips_action(tracks: &[Track], clips: &[Clip]) -> VideoEditorAction {
    VideoEditorAction::UpsertClips {
        tracks: tracks.iter().map(TrackData::from_track).collect(),
        clips: clips.iter().map(ClipData::from_clip).collect(),
    }
}

/// Delete several clips (and write the clips a ripple shifted) as one change.
/// A single clip with nothing shifted stays the single-clip delete it was.
pub fn delete_clips_action(ids: &[Uuid], moved: &[Clip]) -> VideoEditorAction {
    match (ids, moved) {
        ([one], []) => delete_clip_action(one),
        _ => VideoEditorAction::DeleteClips {
            ids: ids.iter().map(ToString::to_string).collect(),
            moved: moved.iter().map(ClipData::from_clip).collect(),
        },
    }
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
        clips: clips.iter().map(ToString::to_string).collect(),
    }
}

pub fn upsert_media_action(m: &MediaItem) -> VideoEditorAction {
    VideoEditorAction::UpsertMedia(MediaData::from_media(m))
}

pub fn remove_media_action(id: &Uuid, clips: &[Uuid]) -> VideoEditorAction {
    VideoEditorAction::RemoveMedia {
        id: id.to_string(),
        clips: clips.iter().map(ToString::to_string).collect(),
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
#[derive(Debug, Clone, Default)]
pub struct VideoResourceCtx {
    pub project: Project,
    pub playhead: f64,
    /// What the mixer last measured — see [`crate::audio::MixMeters`].
    ///
    /// Also app state rather than document state, and here for the same
    /// reason the playhead is: without it the only thing that can act on a
    /// mix that went over is a pointer. A caller with no screen would be left
    /// to guess how far over it went and which lanes were in it, which is the
    /// silent clamp again with an extra step.
    pub mix: crate::audio::MixSnapshot,
}

#[allow(
    clippy::expect_used,
    reason = "the serde-derived document action is the value just supplied to the immediate action builder"
)]
fn immediate(id: &str, label: &str, action: &VideoEditorAction) -> ActionSpec {
    ActionSpec::immediate(id, label, VIDEO_EDITOR_ACTION_TYPE, action)
        .expect("encode video-editor action")
        .reversible()
}

/// Publish the undo and consequence semantics the editor already implements.
/// Every exported operation is one atomic document transaction. Removing a
/// track, clip, effect, marker, or bin source is additionally destructive and
/// therefore requires confirmation when invoked headlessly.
fn annotate_contracts(resource: &mut Resource) {
    for action in &mut resource.actions {
        if action.kind == OperationKind::Document {
            action.contract.reversible = true;
        }
        if action.kind == OperationKind::Document
            && (action.id == "delete"
                || action.id.starts_with("delete-")
                || action.id.starts_with("remove-"))
        {
            let old = action.clone();
            *action = old.destructive();
        }
    }
    for child in &mut resource.children {
        annotate_contracts(child);
    }
}

/// Every spelling `EffectKind::from_name` accepts, in the exact form a caller
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
            f64::from((v >> 16) & 0xff) / 255.0,
            f64::from((v >> 8) & 0xff) / 255.0,
            f64::from(v & 0xff) / 255.0,
            1.0,
        ]);
    }
    let parts: Vec<f64> = s
        .split(',')
        .filter_map(|p| p.trim().parse::<f64>().ok())
        .collect();
    match parts.as_slice() {
        [red, green, blue] => Some([*red, *green, *blue, 1.0]),
        [red, green, blue, alpha] => Some([*red, *green, *blue, *alpha]),
        _ => None,
    }
}

/// Playhead/mix-accurate conveniences — presentation state, not CRDT.
///
/// Standalone [`provide_resources`] nests these under `video.project`. Hosted
/// [`PublishesResources`](ui::resources::PublishesResources) returns the same
/// slices alone so agents keep mark/split/trim-at-playhead (and mix trim)
/// without a second project root beside [`crate::resource::VideoProject`].
#[allow(
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "playhead-bound payloads serialize only local serde enums; the clip loop mirrors the timeline tree"
)]
pub fn live_playhead_resources(ctx: &VideoResourceCtx) -> Vec<Resource> {
    let p = &ctx.project;
    let at = ctx.playhead;
    let mut playhead = Resource::new(
        "video.playhead",
        "video.playhead",
        format!("Playhead · {at:.2}s"),
    );

    let mut marked = p.clone();
    marked.set_mark_in(at);
    playhead = playhead.with_action(immediate(
        "mark-in",
        "Mark in at playhead",
        &set_project_meta_action(&marked),
    ));
    let mut marked = p.clone();
    marked.set_mark_out(at);
    playhead = playhead.with_action(immediate(
        "mark-out",
        "Mark out at playhead",
        &set_project_meta_action(&marked),
    ));

    if ctx.mix.clipped() {
        let snap = p.clone();
        let lanes = ctx.mix.measured_tracks();
        let over = ctx.mix.worst_over_db();
        let lane_words = match ctx.mix.clipped_tracks().len() {
            0 => String::new(),
            1 => ", 1 lane over on its own".to_string(),
            n => format!(", {n} lanes over on their own"),
        };
        playhead = playhead.with_action(
            ActionSpec::typed(
                "trim-mix",
                "Trim the mix to fit",
                "0",
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "headroom",
                    ParamType::Number,
                    "decibels to leave below full scale; 0 lands the measured \
                     peak exactly at full scale",
                )],
                move |args: &ActionArgs| {
                    let headroom = args.number("headroom").unwrap_or(0.0).max(0.0);
                    let next = snap.lanes_trimmed_by(&lanes, over + headroom);
                    encode(&upsert_tracks_and_clips_action(&next, &[]))
                },
            )
            .with_description(format!(
                "the mix was measured {over:+.1} dB over full scale in {} place{}{lane_words}; \
                 every lane comes down by the same amount, so the balance is unchanged",
                ctx.mix.overs.len(),
                if ctx.mix.overs.len() == 1 { "" } else { "s" },
            )),
        );
    }

    playhead = playhead.with_action(ActionSpec::typed(
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
            let raw_name = args.text("name").unwrap_or("").trim();
            let label = if raw_name.is_empty() {
                "Marker"
            } else {
                raw_name
            };
            let project_marker = crate::model::Marker::new(at, label, 1);
            encode(&VideoEditorAction::AddProjectMarker {
                marker: project_marker,
            })
        },
    ));
    if let Some(m) = p.markers.iter().find(|m| (m.t - at).abs() < 0.25) {
        playhead = playhead.with_action(immediate(
            "remove-marker",
            &format!("Remove marker \u{201c}{}\u{201d}", m.name),
            &VideoEditorAction::RemoveProjectMarker {
                marker: m.id.to_string(),
            },
        ));
    }

    for clip in &p.clips {
        let cid = clip.id;
        let group = p.link_group(cid);
        // Distinct ids from timeline `video.clip:{id}` so the playhead slice
        // can sit beside the document tree without a duplicate resource id.
        let mut cr = Resource::new(
            format!("video.playhead-clip:{cid}"),
            "video.playhead-clip",
            p.media(clip.media)
                .map_or_else(|| format!("Clip {cid}"), |m| m.name.clone()),
        );

        let mut split_project = p.clone();
        let halves = split_project.split_linked(cid, at);
        if !halves.is_empty() {
            let written: Vec<Clip> = halves
                .iter()
                .flat_map(|(l, r)| [*l, *r])
                .filter_map(|id| split_project.clip(id).cloned())
                .collect();
            let action = match written.as_slice() {
                [left, right] => split_clip_action(left, right),
                many => upsert_clips_action(many),
            };
            cr = cr.with_action(immediate("split", "Split at playhead", &action));
        }

        if clip.covers(at) {
            let mut ti = p.clone();
            let din = at - clip.start;
            ti.trim_in_linked(cid, din);
            if group.iter().any(|id| ti.clip(*id).is_some()) {
                cr = cr.with_action(immediate(
                    "trim-in",
                    "Trim in to playhead",
                    &patch_clips_fields_action(p, &ti, &group),
                ));
            }
            let mut to = p.clone();
            let dout = at - clip.end();
            to.trim_out_linked(cid, dout);
            if group.iter().any(|id| to.clip(*id).is_some()) {
                cr = cr.with_action(immediate(
                    "trim-out",
                    "Trim out to playhead",
                    &patch_clips_fields_action(p, &to, &group),
                ));
            }
        }

        if !cr.actions.is_empty() {
            playhead = playhead.with_child(cr);
        }
    }

    annotate_contracts(&mut playhead);
    vec![playhead]
}

/// Build the resource tree the editor currently has open: the project, its
/// tracks (top-to-bottom display order), and the clips on each track — every
/// document-mutating operation exported as an action. Playhead/mix conveniences
/// ride [`live_playhead_resources`] under the project root.
///
/// # Panics
///
/// Never in practice: only local serde-derived operations are encoded, and
/// the one index is taken after proving the candidate list non-empty.
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "the resource builder serializes only its local serde enums and indexes the first partner only after proving the candidate list non-empty"
)]
/// Timeline tree used by [`Resource::children`] and the root action split.
/// Playhead rows are included here and stripped by the resource's children.
pub(crate) fn project_tree(ctx: &VideoResourceCtx) -> Vec<Resource> {
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
    for (id, label, variant) in [
        ("import", "Import media", 0_u8),
        ("open", "Open project", 1),
        ("save", "Save project", 2),
        ("save-as", "Save project as", 3),
    ] {
        root = root.with_action(
            ActionSpec::typed_operation(
                id,
                label,
                "path (omit to use the system picker/current project)",
                OperationKind::Effect,
                VIDEO_EDITOR_OPERATION_TYPE,
                vec![ActionParam::optional_owned(
                    "path",
                    ParamType::Text,
                    "Direct filesystem path",
                )],
                move |args: &ActionArgs| {
                    let path = args
                        .text("path")
                        .filter(|p| !p.trim().is_empty())
                        .map(str::to_string);
                    let op = match variant {
                        0 => VideoEditorOperation::ImportMedia { path },
                        1 => VideoEditorOperation::OpenProject { path },
                        2 => VideoEditorOperation::SaveProject { path },
                        _ => VideoEditorOperation::SaveProjectAs { path },
                    };
                    EncodedAction::encode_operation(VIDEO_EDITOR_OPERATION_TYPE, &op)
                        .expect("video operation serializes")
                },
            )
            .execution(ExecutionMode::Background { cancellable: false })
            .returns(ResultSpec::Job),
        );
    }
    // The action's identity, parameters, and job contract do not disappear
    // when this build lacks an encoder. Consumers can still discover the
    // canonical operation and explain its availability consistently; only
    // invocation is refused until an encoder-capable build is present.
    let export = ActionSpec::typed_operation(
        "export",
        "Export video",
        "path (omit to use the system picker)",
        OperationKind::Effect,
        VIDEO_EDITOR_OPERATION_TYPE,
        vec![ActionParam::optional_owned(
            "path",
            ParamType::Text,
            "Direct export path",
        )],
        |args: &ActionArgs| {
            EncodedAction::encode_operation(
                VIDEO_EDITOR_OPERATION_TYPE,
                &VideoEditorOperation::Export {
                    path: args
                        .text("path")
                        .filter(|p| !p.trim().is_empty())
                        .map(str::to_string),
                },
            )
            .expect("export operation serializes")
        },
    )
    .execution(ExecutionMode::Background { cancellable: true })
    .returns(ResultSpec::Job);
    root = root.with_action(match crate::export::unavailable_reason() {
        Some(_) => export.unavailable(
            "export_unavailable",
            "Video export is unavailable on this system",
        ),
        None => export,
    });
    if let Ok(spec) = ActionSpec::operation(
        "cancel-export",
        "Cancel export",
        OperationKind::Effect,
        VIDEO_EDITOR_OPERATION_TYPE,
        &VideoEditorOperation::CancelExport,
    ) {
        root = root.with_action(spec.destructive().unavailable(
            "runtime_state_required",
            "Cancel is offered in the Export panel while a render is running",
        ));
    }

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
    // re-rendering everything. Playhead mark-in/out and mix trim ride
    // [`live_playhead_resources`].
    {
        if p.has_marks() {
            let mut cleared = p.clone();
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

    // Project markers are first-class navigable resources. Their marker id,
    // rather than a captured timestamp, keeps a search row attached to the
    // marker when a collaborator changes its time before activation.
    for marker in &p.markers {
        root = root.with_child(
            Resource::new(
                marker_resource_id(marker.id),
                "video.marker",
                marker.name.clone(),
            )
            .with_action(operation(
                "open",
                "Reveal marker",
                OperationKind::View,
                &VideoEditorOperation::RevealMarker {
                    marker: marker.id.to_string(),
                    clip: None,
                },
            ))
            .with_action(ActionSpec::typed(
                "rename",
                "Rename marker",
                marker.name.clone(),
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "name",
                    ParamType::Text,
                    "marker name",
                )],
                {
                    let marker = marker.clone();
                    move |args: &ActionArgs| {
                        let value = args.text("name").unwrap_or("").trim().to_string();
                        encode(&VideoEditorAction::SetProjectMarkerText {
                            marker: marker.id.to_string(),
                            field: MarkerTextField::Name,
                            expected: marker.name.clone(),
                            value,
                        })
                    }
                },
            ))
            .with_action(ActionSpec::typed(
                "set-note",
                "Set marker note",
                marker.note.clone(),
                ExecutionLocus::ClientLocal,
                vec![ActionParam::required(
                    "note",
                    ParamType::Text,
                    "marker note",
                )],
                {
                    let marker = marker.clone();
                    move |args: &ActionArgs| {
                        encode(&VideoEditorAction::SetProjectMarkerText {
                            marker: marker.id.to_string(),
                            field: MarkerTextField::Note,
                            expected: marker.note.clone(),
                            value: args.text("note").unwrap_or("").to_string(),
                        })
                    }
                },
            ))
            .with_action(immediate(
                "remove",
                &format!("Remove marker \u{201c}{}\u{201d}", marker.name),
                &VideoEditorAction::RemoveProjectMarker {
                    marker: marker.id.to_string(),
                },
            )),
        );
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
            gain_db: 0.0,
            pan: 0.0,
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
        let mut tr = Resource::new(track_resource_id(tid), "video.track", track.name.clone())
            .with_action(operation(
                "open",
                "Reveal track",
                OperationKind::View,
                &VideoEditorOperation::RevealTrack {
                    track: tid.to_string(),
                },
            ));

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
        // The lane's fader, in decibels. Typed rather than prompted so a
        // caller can *read* that it takes a number and what the stops are,
        // instead of parsing a range out of a placeholder — and so
        // `set-gain loud` is refused before the document is touched.
        //
        // Decibels reach the action surface for the same reason they reach
        // the panel: an automation that wants "6 dB under" should say so, and
        // a linear multiplier would make every caller do the log itself.
        let gain_snap = track.clone();
        tr = tr.with_action(ActionSpec::typed(
            "set-gain",
            "Set track gain",
            "0",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "db",
                ParamType::Number,
                "decibels; 0 is unity, +12 is the loudest, -60 is off",
            )],
            move |args: &ActionArgs| {
                let mut next = gain_snap.clone();
                // Out of range is clamped rather than refused: +40 is a
                // legible intent ("as loud as it goes") the way a word is
                // not, and a word never reaches here.
                next.gain_db = args
                    .number("db")
                    .unwrap_or(next.gain_db)
                    .clamp(MIN_GAIN_DB, MAX_GAIN_DB);
                encode(&upsert_track_action(&next))
            },
        ));
        let pan_snap = track.clone();
        tr = tr.with_action(ActionSpec::typed(
            "set-pan",
            "Set track pan",
            "0",
            ExecutionLocus::ClientLocal,
            vec![ActionParam::required(
                "pan",
                ParamType::Number,
                "-1 hard left, 0 centre, 1 hard right",
            )],
            move |args: &ActionArgs| {
                let mut next = pan_snap.clone();
                next.pan = args.number("pan").unwrap_or(next.pan).clamp(-1.0, 1.0);
                encode(&upsert_track_action(&next))
            },
        ));
        // Back to unity and centre in one step. Not reachable by `set-gain 0`
        // alone — that leaves the pan where it was — and "put this lane back"
        // is one intent, so it is one action and one undo boundary.
        if track.mix_is_set() {
            let mut flat = track.clone();
            flat.gain_db = 0.0;
            flat.pan = 0.0;
            tr = tr.with_action(immediate(
                "reset-mix",
                "Reset gain and pan",
                &upsert_track_action(&flat),
            ));
        }
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
            let mut cr =
                Resource::new(clip_resource_id(cid), "video.clip", label).with_action(operation(
                    "open",
                    "Reveal clip",
                    OperationKind::View,
                    &VideoEditorOperation::RevealClip {
                        clip: cid.to_string(),
                    },
                ));

            // Every clip linked to this one. A clip that is not linked is a
            // group of one, so every operation below is written against the
            // group and an unlinked clip takes exactly the path it always did.
            let group = p.link_group(cid);
            let linked = group.len() > 1;

            // Delete + ripple-delete. Both take the whole group: half a shot
            // left on the timeline after deleting the other half is the
            // surprise a link exists to avoid.
            cr = cr.with_action(immediate(
                "delete",
                if linked {
                    "Delete clip and its linked half"
                } else {
                    "Delete clip"
                },
                &delete_clips_action(&group, &[]),
            ));
            let mut ripple_project = p.clone();
            let (removed, moved_ids) = ripple_project.ripple_delete_linked(cid);
            let moved: Vec<Clip> = moved_ids
                .iter()
                .filter_map(|id| ripple_project.clip(*id).cloned())
                .collect();
            cr = cr.with_action(immediate(
                "ripple-delete",
                "Ripple delete (close gap)",
                &delete_clips_action(&removed, &moved),
            ));

            // Split / trim-at-playhead ride [`live_playhead_resources`].

            // Colour grade — prompted `0..1` sliders folded into optimistic
            // clip-field patches.
            for (id, label, field) in [
                ("set-brightness", "Set brightness", FilterAxis::Brightness),
                ("set-contrast", "Set contrast", FilterAxis::Contrast),
                ("set-saturation", "Set saturation", FilterAxis::Saturation),
            ] {
                let base = clip.clone();
                let at = ctx.playhead;
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
                        // Through `set_param_at`, as the inspector's own
                        // slider does. Writing the constant is invisible on a
                        // clip whose grade is keyframed, because the curve
                        // wins at every instant — so the action reported
                        // success and changed nothing, while the identical
                        // drag worked. An action and a click must not
                        // disagree about what they did.
                        next.set_param_at(field.param(), at, v);
                        // If a curve still owns the playhead value, the
                        // constant write was a no-op for playback — key the
                        // authored instant so `param_at` and the picture agree.
                        if (next.param_at(field.param(), at) - v).abs() >= 1e-9 {
                            next.set_key_at(
                                field.param(),
                                at,
                                v,
                                next.key_interp_at(field.param(), at).unwrap_or_default(),
                            );
                        }
                        encode(&clip_field_patch_action(&base, &next))
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
                let at = ctx.playhead;
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
                        // The level *now*, which is the curve's value on a
                        // keyframed clip. Reading the constant would make an
                        // omitted argument snap an animated level back to
                        // whatever rest happened to be.
                        let current = next.param_at(axis.param(), at);
                        let v = args.number("value").unwrap_or(current).clamp(0.0, 1.0);
                        // As the inspector's slider does — see the grade
                        // actions above for why the constant is not enough.
                        next.set_param_at(axis.param(), at, v);
                        if (next.param_at(axis.param(), at) - v).abs() >= 1e-9 {
                            next.set_key_at(
                                axis.param(),
                                at,
                                v,
                                next.key_interp_at(axis.param(), at).unwrap_or_default(),
                            );
                        }
                        encode(&clip_field_patch_action(&base, &next))
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
                        encode(&clip_field_patch_action(&base, &next))
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
                &clip_field_patch_action(clip, &keyed),
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
                    // `MaskShape::from_name` reads everything it does not know
                    // as `None` — which would silently *remove* a mask — so
                    // the vocabulary is declared and an unbound value leaves
                    // the shape alone.
                    if let Some(name) = args.text("shape") {
                        next.mask.shape = MaskShape::from_name(name);
                    }
                    encode(&clip_field_patch_action(&mask_base, &next))
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
                        encode(&clip_field_patch_action(&base, &next))
                    },
                ));
            }

            // Effects stack — add one by name, or clear them all.
            let add_clip = cid;
            let add_ordinal = clip.effects.len();
            // The vocabulary is declared, not narrated. `EffectKind::from_name`
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
                    // A bound value is one of `EFFECT_CHOICES`, so `from_name`
                    // cannot fail here. An unbound builder produces a
                    // deliberately invalid id, which `apply` refuses rather
                    // than quietly inventing a default effect.
                    let effect = args
                        .text("effect")
                        .and_then(EffectKind::from_name)
                        .map_or_else(
                            || crate::model::ClipEffect {
                                id: Uuid::nil(),
                                kind: EffectKind::Grayscale,
                                amount: EffectKind::Grayscale.default_amount(),
                            },
                            crate::model::ClipEffect::new,
                        );

                    encode(&VideoEditorAction::AddEffect {
                        clip: add_clip.to_string(),
                        ordinal: add_ordinal,
                        effect,
                    })
                },
            ));
            if !clip.effects.is_empty() {
                cr = cr.with_action(immediate(
                    "clear-effects",
                    "Clear effects",
                    &VideoEditorAction::ClearEffects {
                        clip: cid.to_string(),
                        expected: clip.effects.clone(),
                    },
                ));
            }

            // Each effect is a durable stack instance, not a label or an
            // index. A duplicate Sepia can therefore be found, changed,
            // moved, or removed without a concurrent reorder turning a saved
            // target into its neighbour.
            for effect in &clip.effects {
                let effect_id = effect.id;
                let effect_amount = effect.amount;
                let mut er = Resource::new(
                    effect_resource_id(cid, effect_id),
                    "video.effect",
                    effect.kind.label(),
                )
                .with_action(operation(
                    "open",
                    "Reveal effect",
                    OperationKind::View,
                    &VideoEditorOperation::RevealEffect {
                        clip: cid.to_string(),
                        effect: effect_id.to_string(),
                    },
                ));

                er = er.with_action(ActionSpec::typed(
                    "set-amount",
                    "Set effect amount",
                    "0.8",
                    ExecutionLocus::ClientLocal,
                    vec![ActionParam::required(
                        "amount",
                        ParamType::Number,
                        "effect intensity, from 0 to 1",
                    )],
                    move |args: &ActionArgs| {
                        encode(&VideoEditorAction::SetEffectAmount {
                            clip: cid.to_string(),
                            effect: effect_id.to_string(),
                            expected_amount_bits: effect_amount.to_bits(),
                            amount: args
                                .number("amount")
                                .unwrap_or(effect_amount)
                                .clamp(0.0, 1.0),
                        })
                    },
                ));

                er = er.with_action(immediate(
                    "remove",
                    "Remove effect",
                    &VideoEditorAction::RemoveEffect {
                        clip: cid.to_string(),
                        effect: effect_id.to_string(),
                        expected_kind: effect.kind,
                        expected_amount_bits: effect.amount.to_bits(),
                    },
                ));

                let index = clip.effect_index(effect_id).unwrap_or(0);
                if index > 0 {
                    er = er.with_action(immediate(
                        "move-up",
                        "Move effect up",
                        &VideoEditorAction::MoveEffect {
                            clip: cid.to_string(),
                            effect: effect_id.to_string(),
                            expected_index: index,
                            neighbour: clip.effects[index - 1].id.to_string(),
                            up: true,
                        },
                    ));
                }
                if index + 1 < clip.effects.len() {
                    er = er.with_action(immediate(
                        "move-down",
                        "Move effect down",
                        &VideoEditorAction::MoveEffect {
                            clip: cid.to_string(),
                            effect: effect_id.to_string(),
                            expected_index: index,
                            neighbour: clip.effects[index + 1].id.to_string(),
                            up: false,
                        },
                    ));
                }
                cr = cr.with_child(er);
            }

            // Speed. The rate is prompted; reverse is a toggle, because
            // "play it backwards" is a decision, not a number.
            //
            // Every one of the three re-times the whole link group. Speed is
            // the field where letting the two halves disagree is not a
            // cosmetic drift: the sound slides further from the picture with
            // every second of the shot, and nothing on screen says why.
            let speed_project = p.clone();
            let speed_group = group.clone();
            let speed_cid = cid;
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
                    let mut next = speed_project.clone();
                    // Out of range is still clamped, by `Speed::new` — asking
                    // for 100× is a legible "as fast as it goes". Unreadable
                    // is no longer 1×: a word cannot bind to a number, so a
                    // slowed clip stays slowed instead of silently snapping
                    // back to normal speed. The `2x` spelling goes with it —
                    // the suffix was prose wrapped around the number, and the
                    // number is what is declared.
                    let (was, reverse) = next
                        .clip(speed_cid)
                        .map_or((1.0, false), |c| (c.speed.rate(), c.speed.reverse));
                    let rate = args.number("rate").unwrap_or(was);
                    next.set_speed_linked(speed_cid, rate, reverse);
                    encode(&patch_clips_fields_action(
                        &speed_project,
                        &next,
                        &speed_group,
                    ))
                },
            ));
            let mut reversed = p.clone();
            reversed.set_speed_linked(cid, clip.speed.rate(), !clip.speed.reverse);
            cr = cr.with_action(immediate(
                "toggle-reverse",
                if clip.speed.reverse {
                    "Play forwards"
                } else {
                    "Play backwards"
                },
                &patch_clips_fields_action(p, &reversed, &group),
            ));
            if !clip.speed.is_normal() {
                let mut normal = p.clone();
                normal.set_speed_linked(cid, 1.0, false);
                cr = cr.with_action(immediate(
                    "reset-speed",
                    "Reset speed to normal",
                    &patch_clips_fields_action(p, &normal, &group),
                ));
            }

            // ---- linked audio and video ---------------------------------
            //
            // A link is a shared group id on the clips, so linking and
            // unlinking are ordinary clip writes. They are actions rather than
            // only a menu item because a capability reachable only by clicking
            // is invisible to automation, to a script and to a reader.
            if linked {
                let mut cut = p.clone();
                cut.unlink_clips(&group);
                cr = cr.with_action(immediate(
                    "unlink",
                    "Unlink audio and video",
                    &patch_clips_fields_action(p, &cut, &group),
                ));
            }
            {
                // Give a picture that carries sound its own audio clip, so the
                // sound can be slid, levelled, faded and cut on its own.
                //
                // Offered exactly while `can_split_audio` holds: a picture lane,
                // a source the container said has a sound track, and no audio
                // half already carrying it. That is also what a project made
                // before linked audio existed is offered, and what a shot whose
                // sound half was deleted is offered again.
                if p.can_split_audio(cid) {
                    let mut next = p.clone();
                    // The lane it lands on, and the lane the change has to
                    // *make* when the project has no audio track yet.
                    let (audio_track, fresh) =
                        if let Some(t) = next.placement_track(TrackKind::Audio, None) {
                            (t, Vec::new())
                        } else {
                            let t = Track {
                                id: Uuid::new_v4(),
                                name: "A1".to_string(),
                                kind: TrackKind::Audio,
                                index: 1,
                                muted: false,
                                locked: false,
                                solo: false,
                                gain_db: 0.0,
                                pan: 0.0,
                            };
                            let id = t.id;
                            next.tracks.push(t.clone());
                            (id, vec![t])
                        };
                    if let Some(audio_id) = next.add_linked_audio(cid, audio_track) {
                        let written: Vec<Clip> = [cid, audio_id]
                            .iter()
                            .filter_map(|id| next.clip(*id).cloned())
                            .collect();
                        cr = cr.with_action(immediate(
                            "link-audio",
                            "Split off linked audio clip",
                            &upsert_tracks_and_clips_action(&fresh, &written),
                        ));
                    }
                }
            }
            // Link this clip to another one that is already on the timeline —
            // the reverse of `unlink`, and the only way back once a pair has
            // been separated. The partner is named by id rather than described
            // in prose, and the ids that exist are declared as the choices, so
            // a clip that is not there is refused instead of silently linking
            // nothing.
            let others: Vec<String> = p
                .clips
                .iter()
                .filter(|c| c.id != cid && !group.contains(&c.id))
                .map(|c| c.id.to_string())
                .collect();
            if !others.is_empty() {
                let link_project = p.clone();
                let link_cid = cid;
                let first = others[0].clone();
                cr = cr.with_action(ActionSpec::typed(
                    "link-with",
                    "Link with another clip",
                    &first,
                    ExecutionLocus::ClientLocal,
                    vec![
                        ActionParam::required(
                            "clip",
                            ParamType::Text,
                            "the id of the clip to move, trim and re-time with this one",
                        )
                        .with_choices(others.clone()),
                    ],
                    move |args: &ActionArgs| {
                        let mut next = link_project.clone();
                        let other = args.text("clip").and_then(|s| s.parse::<Uuid>().ok());
                        // An unbound or unknown partner links nothing rather
                        // than linking this clip to itself — and writes
                        // nothing, rather than a clip made of defaults.
                        let Some(other) = other.filter(|o| next.clip(*o).is_some()) else {
                            return encode(&VideoEditorAction::PatchClipsFields {
                                patches: vec![],
                            });
                        };
                        // Both sides join one fresh group, and anything either
                        // of them was already linked to comes along -- a link
                        // is a group, so joining is a merge, not a swap.
                        let mut members = next.link_group(link_cid);
                        for m in next.link_group(other) {
                            if !members.contains(&m) {
                                members.push(m);
                            }
                        }
                        next.link_clips(&members);
                        encode(&patch_clips_fields_action(&link_project, &next, &members))
                    },
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
                cr = cr.with_action(immediate(id, label, &clip_field_patch_action(clip, &inset)));
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
                        encode(&clip_field_patch_action(&base, &next))
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
                        encode(&clip_field_patch_action(&base, &next))
                    },
                ));
            }
            if !clip.transform.is_identity() {
                let mut flat = clip.clone();
                flat.transform = crate::model::Transform::default();
                cr = cr.with_action(immediate(
                    "reset-transform",
                    "Reset position and scale",
                    &clip_field_patch_action(clip, &flat),
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
                        &clip_field_patch_action(&base, &keyed),
                    ));
                }
                for param in clip.anim.animated_params() {
                    let mut cleared = clip.clone();
                    cleared.anim.clear(param);
                    cr = cr.with_action(immediate(
                        &format!("unkey-{}", param.as_str()),
                        &format!("Remove {} animation", param.label().to_lowercase()),
                        &clip_field_patch_action(clip, &cleared),
                    ));
                }
                // A marker on the clip, named.
                let marker_clip = cid;
                let marker_palette = clip.palette;
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
                        let name = args.text("name").unwrap_or("").trim();
                        let name = if name.is_empty() { "Marker" } else { name };
                        let marker = crate::model::Marker::new(at, name, marker_palette);
                        encode(&VideoEditorAction::AddClipMarker {
                            clip: marker_clip.to_string(),
                            marker,
                        })
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
                    encode(&clip_field_patch_action(&base, &next))
                },
            ));

            for marker in &clip.markers {
                cr = cr.with_child(
                    Resource::new(
                        marker_resource_id(marker.id),
                        "video.marker",
                        marker.name.clone(),
                    )
                    .with_action(operation(
                        "open",
                        "Reveal marker",
                        OperationKind::View,
                        &VideoEditorOperation::RevealMarker {
                            marker: marker.id.to_string(),
                            clip: Some(cid.to_string()),
                        },
                    ))
                    .with_action(ActionSpec::typed(
                        "rename",
                        "Rename marker",
                        marker.name.clone(),
                        ExecutionLocus::ClientLocal,
                        vec![ActionParam::required(
                            "name",
                            ParamType::Text,
                            "marker name",
                        )],
                        {
                            let marker = marker.clone();
                            move |args: &ActionArgs| {
                                let value = args.text("name").unwrap_or("").trim().to_string();
                                encode(&VideoEditorAction::SetClipMarkerText {
                                    clip: cid.to_string(),
                                    marker: marker.id.to_string(),
                                    field: MarkerTextField::Name,
                                    expected: marker.name.clone(),
                                    value,
                                })
                            }
                        },
                    ))
                    .with_action(ActionSpec::typed(
                        "set-note",
                        "Set marker note",
                        marker.note.clone(),
                        ExecutionLocus::ClientLocal,
                        vec![ActionParam::required(
                            "note",
                            ParamType::Text,
                            "marker note",
                        )],
                        {
                            let marker = marker.clone();
                            move |args: &ActionArgs| {
                                encode(&VideoEditorAction::SetClipMarkerText {
                                    clip: cid.to_string(),
                                    marker: marker.id.to_string(),
                                    field: MarkerTextField::Note,
                                    expected: marker.note.clone(),
                                    value: args.text("note").unwrap_or("").to_string(),
                                })
                            }
                        },
                    ))
                    .with_action(immediate(
                        "remove",
                        &format!("Remove marker \u{201c}{}\u{201d}", marker.name),
                        &VideoEditorAction::RemoveClipMarker {
                            clip: cid.to_string(),
                            marker: marker.id.to_string(),
                        },
                    )),
                );
            }

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
        let mut mr = Resource::new(media_resource_id(mid), "video.media", m.name.clone())
            .with_action(operation(
                "open",
                "Reveal media",
                OperationKind::View,
                &VideoEditorOperation::RevealMedia {
                    media: mid.to_string(),
                },
            ));
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
                    .with_choices(
                        crate::model::TextAlign::ALL.map(crate::model::TextAlign::as_str),
                    ),
                ],
                move |args: &ActionArgs| {
                    let mut next = align_base.clone();
                    let mut t = next.text.clone().unwrap_or_default();
                    // `TextAlign::from_name` reads *everything* it does not
                    // recognise as `Center`, so "centre" and "banana" were
                    // both a silent re-centring. The three declared names are
                    // the only ones that can reach it now.
                    if let Some(name) = args.text("align") {
                        t.align = crate::model::TextAlign::from_name(name);
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

    for extra in live_playhead_resources(ctx) {
        root = root.with_child(extra);
    }

    annotate_contracts(&mut root);
    vec![root]
}

/// The published project. Playhead and mix rows stay live extras on this tree
/// so standalone callers still see them; the framework root adds them itself.
pub fn provide_resources(ctx: &VideoResourceCtx) -> Vec<Resource> {
    // Standalone / palette / tests need the timeline at the *live* playhead
    // (clip markers, keys, trim). `tilo_resource::publish` builds document
    // children at playhead 0 and keeps playhead doors on the surface — that
    // path stays for the framework root; this tree is the live editor door.
    let mut roots = project_tree(ctx);
    for root in &mut roots {
        annotate_contracts(root);
    }
    roots
}

#[derive(Clone, Copy)]
enum FilterAxis {
    Brightness,
    Contrast,
    Saturation,
}

impl FilterAxis {
    /// The animatable parameter this axis drives, so an action writes through
    /// the same door the inspector's slider does.
    fn param(self) -> crate::param::Param {
        match self {
            FilterAxis::Brightness => crate::param::Param::Brightness,
            FilterAxis::Contrast => crate::param::Param::Contrast,
            FilterAxis::Saturation => crate::param::Param::Saturation,
        }
    }
}

/// Which `0..1` level a prompted action drives. Not a `FilterAxis`: neither
/// is a grade — one is how much of the picture is composited, the other how
/// loud it is.
#[derive(Clone, Copy)]
enum LevelAxis {
    Opacity,
    Volume,
}

impl LevelAxis {
    /// The animatable parameter this level drives, so an action writes
    /// through the same door the inspector's slider does.
    fn param(self) -> crate::param::Param {
        match self {
            LevelAxis::Opacity => crate::param::Param::Opacity,
            LevelAxis::Volume => crate::param::Param::Volume,
        }
    }
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
/// scalar on the selected clip, folded into an optimistic field patch.
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
        other => Transition::from_name(other),
    }
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::VideoEditorApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let ctx = self.video_resource_ctx();
        // The document is the project once it holds one. An unsynced editor
        // still has the project only on the widget. Playhead and mix stay live.
        let project = if VideoDoc::is_empty(doc) {
            ctx.project.clone()
        } else {
            VideoDoc::load_project(doc)
        };
        let label = if project.name.is_empty() {
            "Project".to_string()
        } else {
            project.name.clone()
        };
        let mut root = ui::tilo_resource::publish::<crate::resource::VideoProject>(
            "video.project",
            &label,
            &project,
        );
        root.children.extend(live_playhead_resources(&ctx));
        vec![root]
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

    /// The spec with `id` published **on a named resource**.
    ///
    /// [`spec`] takes the first match anywhere in the tree, which is fine for
    /// an action only one resource carries. Every audio lane publishes its
    /// own `set-gain`, so a test that used `spec` there would drive one lane
    /// and assert about another.
    fn spec_on(ctx: &VideoResourceCtx, resource: &str, id: &str) -> ActionSpec {
        fn walk(r: &Resource, resource: &str, id: &str, out: &mut Option<ActionSpec>) {
            if r.id.as_str() == resource
                && let Some(a) = r.actions.iter().find(|a| a.id == id)
            {
                *out = Some(a.clone());
            }
            for c in &r.children {
                walk(c, resource, id, out);
            }
        }
        let mut found = None;
        for r in provide_resources(ctx) {
            walk(&r, resource, id, &mut found);
        }
        found.unwrap_or_else(|| panic!("`{resource}` publishes no `{id}`"))
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
            None => spec_encoding(spec, None),
        };
        let action: VideoEditorAction =
            serde_json::from_slice(&encoded.payload).expect("payload decodes");
        action.apply(&mut doc).expect("applies");
        VideoDoc::load_project(&doc)
    }

    fn rejects(spec: &ActionSpec, arg: &str) -> bool {
        serde_json::from_slice::<VideoEditorAction>(&prompted_encode(spec, arg).payload).is_err()
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
                ..Default::default()
            },
            cid,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the values are written through an action and read back unchanged, or clamped to a literal bound"
    )]
    fn whole_project_actions_replace_and_survive_the_wire() {
        // Case `opening_a_project_replays_as_one_action_that_replaces_everything`.
        {
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
        // Case `a_whole_project_survives_the_wire_with_every_field_intact`.
        {
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
        // Case `explicit_full_clip_replacement_carries_geometry_speed_curves_and_markers`.
        {
            // Import and explicit replacement still carry all persisted fields;
            // ordinary editor interactions cover them with narrow patches below.
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
        // Case `a_payload_from_before_keying_reads_as_unkeyed_and_unmasked`:
        // A peer that predates keying sends a payload with no `key`/`mask`
        // field at all. That has to read as "not keyed, not masked" — the
        // composite that peer is already drawing — rather than failing the
        // decode and wedging the replay.
        {
            let (ctx, cid) = ctx_fixture();
            let want = ctx.project.clip(cid).unwrap().clone();
            let mut json: serde_json::Value =
                serde_json::to_value(ClipData::from_clip(&want)).expect("encodes");
            let obj = json.as_object_mut().expect("a map on the wire");
            assert!(obj.remove("key").is_some(), "the field is there to remove");
            assert!(obj.remove("mask").is_some());
            let older: ClipData =
                serde_json::from_value(json).expect("an older payload still decodes");
            let c = older.to_clip().expect("and still rebuilds");
            assert!(!c.key.is_active(), "absent means nothing is keyed");
            assert!(!c.mask.is_active(), "absent means the layer is not cut");
            assert_eq!(c, want, "and nothing else about the clip moved");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the values are written through an action and read back unchanged, or clamped to a literal bound"
    )]
    fn palette_edits_speed_inset_keys_markers_and_titles() {
        // Case `the_palette_can_set_a_speed_and_reverse_it`.
        {
            let (ctx, cid) = ctx_fixture();
            let sped = run(&ctx.project, &spec(&ctx, "set-speed"), Some("2"));
            let c = sped.clip(cid).unwrap();
            assert_eq!(c.speed.rate(), 2.0);
            assert!(
                ((c.duration()) - (15.0)).abs() < 1e-9,
                "and the clip is half as long on the timeline"
            );

            let back = run(&ctx.project, &spec(&ctx, "toggle-reverse"), None);
            assert!(back.clip(cid).unwrap().speed.reverse);
        }
        // Case `the_palette_can_inset_a_clip_and_put_it_back`.
        {
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
        // Case `the_palette_keys_a_parameter_at_the_playhead_without_moving_the_picture`.
        {
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
        // Case `the_palette_adds_a_timeline_marker_and_removes_it`.
        {
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
        // Case `the_palette_adds_a_title_to_the_bin`.
        {
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
        // Case `a_clip_marker_from_the_palette_lands_on_the_clip_not_the_project`.
        {
            let (ctx, cid) = ctx_fixture();
            let marked = run(&ctx.project, &spec(&ctx, "add-clip-marker"), Some("hit"));
            assert!(marked.markers.is_empty(), "not a project marker");
            let m = &marked.clip(cid).unwrap().markers;
            assert_eq!(m.len(), 1);
            assert_eq!(m[0].name, "hit");
            assert_eq!(m[0].t, 4.0, "clip-local, and this clip starts at zero");
        }
        // Case `every_new_action_replays_to_the_same_document_twice`.
        {
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
                    None => spec_encoding(&sp, None),
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
    }

    use super::*;
    use crate::app::VideoEditorApp;
    use tilo_actions::{ActionInput, ActionRegistry, LocalTransport};
    use ui::testing::action_contract::{find_spec, spec_encoding};
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

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn palette_tree_and_removals_edit_the_document() {
        // Case `ctrl_k_publishes_the_project_track_clip_tree`:
        // Ctrl+K on the editor yields the nested tree: the project → its tracks →
        // the clips on each, each exporting the actions its data model implies.
        // Drives the real resource hook.
        {
            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            assert_eq!(resources.len(), 1, "one open project");
            let project = &resources[0];
            assert_eq!(project.kind, "video_editor.project");
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
        }
        // Case `delete_clip_action_removes_it_from_the_doc`:
        // Dispatching `DeleteClip` through the executor removes exactly that clip
        // from the document.
        {
            let (_app, mut doc) = seeded();
            let project = VideoDoc::load_project(&doc);
            let id = project.clips[0].id;
            let before = project.clips.len();

            let action = delete_clip_action(&id);
            let encoded = encode(&action);
            let decoded = registry().decode(&encoded).unwrap();
            let mut tx = LocalTransport::new();
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let after = VideoDoc::load_project(&doc);
            assert_eq!(after.clips.len(), before - 1);
            assert!(after.clip(id).is_none(), "the clip is gone from the CRDT");
        }
        // Case `ripple_delete_is_one_action_that_closes_the_gap`:
        // A ripple delete lands as ONE action that both removes the clip and shifts
        // every later clip on the same track left — a single change, not N.
        {
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
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

            let after = VideoDoc::load_project(&doc);
            assert!(after.clip(id).is_none(), "the rippled clip is gone");
            assert!(
                after.clip(next_id).unwrap().start < next_start,
                "the later clip shifted left to close the gap",
            );
        }
        // Case `remove_media_action_also_deletes_its_clips`:
        // Deleting a bin media removes the source *and* every clip that placed it,
        // in one action, so no dangling clip is left referencing a gone source.
        {
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
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

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
        // Case `add_effect_action_appends_to_the_stack`:
        // The Ctrl+K "Add effect" prompt appends the named effect to the clip's
        // stack through the same narrow effects action the inspector uses.
        {
            let (app, mut doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
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
            tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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
    }

    /// The editor's published tree for `doc`: a fresh editor synced to it,
    /// exactly as the host wires one to its document.
    fn resources_for(doc: &AutoCommit) -> Vec<Resource> {
        let mut app = VideoEditorApp::new();
        let mut doc = doc.clone();
        app.sync(&mut doc, &automerge::ROOT);
        ui::resources::resources(&app, &doc)
    }

    fn clip_id(res: &Resource) -> Uuid {
        res.id
            .0
            .trim_start_matches("video.clip:")
            .parse()
            .expect("a clip uuid")
    }

    /// The first sample clip moved `by` seconds, as one narrow field patch.
    fn move_first_clip(doc: &AutoCommit, by: f64) -> EncodedAction {
        let project = VideoDoc::load_project(doc);
        let mut clip = project.clips[0].clone();
        clip.start += by;
        encode(&clip_field_patch_action(&project.clips[0], &clip))
    }

    /// Ripple-delete the first clip of a track that has a later neighbour.
    fn ripple_delete_first(doc: &AutoCommit) -> EncodedAction {
        let mut project = VideoDoc::load_project(doc);
        let id = project
            .tracks
            .iter()
            .find_map(|t| {
                let clips = project.clips_on(t.id);
                (clips.len() >= 2).then(|| clips[0].id)
            })
            .expect("a track with two clips");
        let moved_ids = project.ripple_delete(id);
        let moved: Vec<Clip> = moved_ids
            .iter()
            .filter_map(|m| project.clip(*m).cloned())
            .collect();
        encode(&ripple_delete_action(&id, &moved))
    }

    /// Remove a placed bin media together with every clip that places it.
    fn remove_placed_media(doc: &AutoCommit) -> EncodedAction {
        let project = VideoDoc::load_project(doc);
        let mid = project
            .media
            .iter()
            .find(|m| project.clips.iter().any(|c| c.media == m.id))
            .map(|m| m.id)
            .expect("a placed media");
        let clips: Vec<Uuid> = project
            .clips
            .iter()
            .filter(|c| c.media == mid)
            .map(|c| c.id)
            .collect();
        encode(&remove_media_action(&mid, &clips))
    }

    ui::action_contract_tests! {
        seed: seeded().1,
        registry: registry(),
        resources: |doc| resources_for(doc),
        palette: [
            "Delete clip",
            "Rename project",
            "Add title",
            "Add marker at playhead",
        ],
        replay: [
            move_clip => |doc| move_first_clip(doc, 5.0),
            delete_clip => |doc| encode(&delete_clip_action(&VideoDoc::load_project(doc).clips[0].id)),
            ripple_delete => |doc| ripple_delete_first(doc),
            remove_media => |doc| remove_placed_media(doc),
        ],
        undo: [
            move_clip => |doc| move_first_clip(doc, 7.0),
            ripple_delete => |doc| ripple_delete_first(doc),
            remove_media => |doc| remove_placed_media(doc),
        ],
        encodes: [
            delete_clip => ("video.clip", "delete", |_, res| {
                encode(&delete_clip_action(&clip_id(res)))
            }),
        ],
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the values are written through an action and read back unchanged, or clamped to a literal bound"
    )]
    fn declared_arguments_are_parsed_and_refused_by_type() {
        // Case `prompted_brightness_builds_the_expected_clip_write`:
        // The prompted brightness spec builds the same narrow field patch an
        // inspector edit to 0.9 would — a slider value folded into one action.
        {
            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
            let (clip_res, spec) = find_spec(&resources, "video.clip", "set-brightness");
            let from_spec = prompted_encode(spec, "0.9");

            let before = VideoDoc::load_project(&doc)
                .clip(clip_id(clip_res))
                .unwrap()
                .clone();
            let mut expected = before.clone();
            expected.filters.brightness = 0.9;
            let from_builder = encode(&clip_field_patch_action(&before, &expected));
            assert_eq!(
                from_spec.payload, from_builder.payload,
                "slider folds into one narrow clip-field action"
            );
        }
        // Case `an_action_on_a_keyframed_clip_moves_what_it_plays`:
        // A grade action declares a number, and a word is refused before the
        // document is touched.
        //
        // This used to be a free-text prompt parsed by `parse_unit`, which
        // answered `0.5` for anything it could not read. So `set-brightness
        // banana` did not fail — it made a real edit, to a plausible value, and
        // reported success. A caller had no way to learn the type either: "Value
        // 0–1" was prose in a placeholder. Declaring the parameter makes the type
        // readable and makes a bad value an error rather than an edit.
        // An action on a keyframed clip changes what the clip is playing.
        //
        // The grade and level actions wrote the constant field. `param_at`
        // prefers the curve and only falls back to the constant, so on a clip
        // whose brightness is keyframed the write was invisible — the action
        // reported success and the picture did not move. The identical drag in
        // the inspector worked, because it goes through `set_param_at`. An
        // action and a click disagreeing about what they did is the one thing
        // this surface exists to prevent.
        {
            use crate::param::{Interp, Param};
            use tilo_actions::params::ArgValue;

            // Built straight from a project, so this needs no widget.
            let mut project = crate::doc::sample_project();
            let cid = project
                .clips
                .iter()
                .find(|c| {
                    project
                        .track(c.track)
                        .is_some_and(|t| t.kind == crate::model::TrackKind::Video)
                })
                .expect("a video clip")
                .id;
            let at = {
                let c = project.clip(cid).expect("clip");
                c.start + (c.end() - c.start) * 0.5
            };
            project.clip_mut(cid).expect("clip").set_key_at(
                Param::Brightness,
                at,
                0.2,
                Interp::Linear,
            );

            let before = project
                .clip(cid)
                .expect("clip")
                .param_at(Param::Brightness, at);
            assert!(
                (before - 0.2).abs() < 1e-6,
                "the fixture is not actually keyframed: {before}",
            );

            let resources = provide_resources(&VideoResourceCtx {
                project: project.clone(),
                playhead: at,
                ..Default::default()
            });
            let clip_res = resources
                .iter()
                .flat_map(|r| r.children.iter())
                .flat_map(|t| t.children.iter())
                .find(|c| c.kind == "video.clip" && c.id.0.ends_with(&cid.to_string()))
                .expect("the keyed clip is published");
            let spec = clip_res
                .actions
                .iter()
                .find(|a| a.id == "set-brightness")
                .expect("set-brightness is offered");

            let encoded = spec
                .build_args([("value".to_string(), ArgValue::Number(0.9))])
                .expect("a number binds");
            let action: VideoEditorAction =
                serde_json::from_slice(&encoded.payload).expect("payload decodes");

            let mut doc = automerge::AutoCommit::new();
            VideoDoc::write_project(&mut doc, &project);
            action.apply(&mut doc).expect("the action applies");

            let after = VideoDoc::load_project(&doc)
                .clip(cid)
                .expect("clip survives")
                .param_at(Param::Brightness, at);
            assert!(
                (after - 0.9).abs() < 0.05,
                "set-brightness 0.9 on a keyframed clip left it playing {after} \
             (was {before}) — the action wrote the constant the curve hides",
            );
        }
        // Case `a_word_is_refused_where_the_grade_declares_a_number`.
        {
            use tilo_actions::params::ArgValue;

            let (app, doc) = seeded();
            let resources = ui::resources::resources(&app, &doc);
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
                spec.params(),
                [("value".to_string(), ArgValue::Text("banana".into()))],
            );
            assert!(
                refused.is_err(),
                "a word bound to a number-typed parameter must be refused, not read as 0.5",
            );

            let bound = ActionArgs::bind(
                spec.params(),
                [("value".to_string(), ArgValue::Number(0.8))],
            )
            .expect("a number binds");
            assert_eq!(bound.number("value"), Some(0.8));
        }
        // Case `every_parameterised_action_declares_its_argument`:
        // Every action that takes an argument names it and declares its type.
        //
        // That declaration is the whole surface for a caller with no screen: a
        // screen reader, a script and an agent read the enumeration and nothing
        // else, so a type stated only in placeholder prose ("Value 0-1", "Rate,
        // e.g. 0.5 / 1 / 2") is a type none of them can see. The second half of
        // the test is the one that keeps it honest — an action that takes an
        // argument and is not in the table fails here rather than shipping as
        // another untyped prompt.
        {
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
                    assert!(!p.description.is_empty(), "{id} says what {name} means");
                }
            }

            let known = declared();
            for (id, sp) in all_specs(&ctx) {
                if sp.params().is_empty() || sp.kind == OperationKind::Effect {
                    continue;
                }
                assert!(
                    known.iter().any(|(k, _)| *k == id),
                    "{id} takes an argument but declares nothing this test has read",
                );
            }
        }
        // Case `the_key_and_mask_knobs_write_the_value_they_are_named_for`:
        // The chroma key's three sliders and the mask's six geometry knobs write
        // the value they are named for.
        //
        // Declaring the parameter is half of it; the other half is that the number
        // lands on the knob the id names. Nine near-identical actions is exactly
        // the shape a copy-paste slip hides in — `set-mask-height` writing
        // `mask.w` would satisfy every type assertion in this file and stay
        // invisible until someone actually masked something — so each is checked
        // against every one of its neighbours as well as against itself.
        {
            type Read = fn(&Clip) -> f64;
            let (ctx, cid) = ctx_fixture();
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
        // Case `a_word_never_reaches_a_key_or_mask_knob`:
        // A key or mask knob asked for nothing it can read leaves the value where
        // it was, rather than answering with a plausible middle.
        //
        // The failure this rules out is the one the grade actions were built to
        // rule out: a lenient parser that read a word as 0.5 made a real,
        // convincing edit and reported success.
        {
            use tilo_actions::params::ArgValue;

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
        // Case `a_word_is_refused_wherever_a_number_is_declared`:
        // A word is refused wherever a number is declared, everywhere — not only
        // on the grade.
        //
        // Each of these used to be a free-text prompt with a lenient parser
        // behind it: `parse_unit` answered 0.5, the geometry parse answered 0.0
        // (for scale, an invisible clip), the speed parse answered 1x. All three
        // were a real edit reported as success. Declaring `Number` makes the bad
        // call a refusal, before the document is touched.
        {
            use tilo_actions::params::ArgValue;

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
        // Case `a_name_outside_the_declared_choices_is_refused`:
        // A name outside the declared vocabulary is refused, and every name the
        // vocabulary lists binds.
        //
        // The prose list used to be the entire specification — "grayscale /
        // sepia / invert / warm / cool / vignette" in a placeholder — with the
        // parser answering "nothing happened" for a name it did not know, and
        // `parse_transition` answering "hard cut". Declared choices are the same
        // list a caller can read *and* be held to.
        {
            use tilo_actions::params::ArgValue;

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
                    ActionArgs::bind(sp.params(), [(name.to_string(), ArgValue::Number(1.0))])
                        .is_err(),
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
                    EffectKind::from_name(c).is_some(),
                    "{c:?} is offered, so the parser has to know it",
                );
            }
            for k in EffectKind::ALL {
                assert!(
                    effects.iter().any(|c| EffectKind::from_name(c) == Some(k)),
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
        // Case `an_unreadable_value_no_longer_invents_an_edit`:
        // Valid input still lands, and unreadable input now changes nothing
        // instead of inventing a plausible edit.
        {
            let (ctx, cid) = ctx_fixture();

            // `parse_unit("half")` answered 0.5 — half the frame cropped away, on
            // a typo. The word cannot bind now, so the edge keeps its value.
            assert!(rejects(&spec(&ctx, "crop-left"), "half"));
            // A number still lands, and out of range still clamps: asking to cut
            // 1.4 of the frame is a legible "all of it".
            let cropped = run(&ctx.project, &spec(&ctx, "crop-left"), Some("1.4"));
            assert_eq!(cropped.clip(cid).unwrap().transform.crop_left, 1.0);
            let cropped = run(&ctx.project, &spec(&ctx, "crop-bottom"), Some("0.25"));
            assert_eq!(cropped.clip(cid).unwrap().transform.crop_bottom, 0.25);

            // The geometry parse answered 0.0, which for scale is a clip that is
            // no longer on screen.
            assert!(rejects(&spec(&ctx, "set-scale"), "banana"));
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
            assert!(rejects(&spec(&ctx2, "set-speed"), "quickly"));
        }
        // Case `a_named_transition_and_effect_land_and_an_unknown_name_does_not`:
        // A named transition and effect still land; a name the action never
        // offered leaves the clip alone instead of quietly cutting.
        {
            let (ctx, cid) = ctx_fixture();

            let dipped = run(
                &ctx.project,
                &spec(&ctx, "set-transition"),
                Some("dip to black"),
            );
            assert_eq!(dipped.clip(cid).unwrap().transition, Transition::DipToBlack);
            let ctx2 = VideoResourceCtx {
                project: dipped,
                // Named rather than spread: the rest of this test still reads
                // `ctx`, and the meter is not `Copy`, so a spread would move it out
                // from under the assertions below.
                mix: ctx.mix.clone(),
                ..ctx
            };
            assert!(rejects(&spec(&ctx2, "set-transition"), "swipe"));

            // An alias the placeholder never listed but the parser has always
            // taken still works — declaring the type must not take a working name
            // away.
            let inverted = run(&ctx.project, &spec(&ctx, "add-effect"), Some("negative"));
            assert_eq!(
                inverted.clip(cid).unwrap().effects.last().map(|e| e.kind),
                Some(EffectKind::Invert),
            );
            assert!(rejects(&spec(&ctx, "add-effect"), "chrome"));
        }
        // Case `the_title_actions_hold_their_declared_shapes`:
        // The title's own arguments: three declared names for alignment, numbers
        // for size and backdrop, and free text for the colour — which keeps its
        // leniency deliberately.
        {
            let ctx = ctx_with_title();

            let left = run(&ctx.project, &spec(&ctx, "set-title-align"), Some("left"));
            assert_eq!(title_of(&left).align, crate::model::TextAlign::Left);
            let ctx2 = VideoResourceCtx {
                project: left,
                ..ctx
            };
            // "centre" used to be read as `Center` — an edit, from a spelling the
            // action never offered, indistinguishable from a real one.
            assert!(rejects(&spec(&ctx2, "set-title-align"), "centre"));

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
        // Case `the_renames_still_rename_through_their_named_parameter`:
        // The three renames still rename, through a parameter that now has a
        // name of its own rather than the generic prompted one.
        {
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
    }

    /// The fixture with a title in the bin, so the media resource exports the
    /// title actions as well as the rename.
    fn ctx_with_title() -> VideoResourceCtx {
        let (ctx, _) = ctx_fixture();
        let mut with_title = run(&ctx.project, &spec(&ctx, "add-title"), Some("Chapter One"));
        // A second clip on the timeline, so the actions that name *another*
        // clip -- linking one to another -- are offered here too. With a
        // single clip there is nothing to name, the action is (rightly)
        // absent, and the declaration table below could not reach it.
        let (track, media) = {
            let c = with_title.clips.first().expect("the fixture clip");
            (c.track, c.media)
        };
        with_title.add_clip(track, media, 20.0);
        // …and a mix that has been measured over, so the action that trims one
        // back is offered here too. Same reason: an action nobody can reach
        // from this fixture is an action the declaration table cannot check.
        let lane = with_title.clips.first().map(|c| c.track);
        let meters = crate::audio::MixMeters::new();
        meters.record(
            3.0,
            &crate::audio::MixReport {
                master: crate::audio::BlockMeter { peak: [1.4, 1.2] },
                tracks: lane
                    .map(|t| vec![(t, crate::audio::BlockMeter { peak: [1.4, 1.2] })])
                    .unwrap_or_default(),
            },
        );
        VideoResourceCtx {
            project: with_title,
            mix: meters.snapshot(),
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
    const DECLARED_MORE: [(&str, &[(&str, ParamType)]); 18] = [
        // Linking names the partner by id, and the ids that exist are the
        // declared choices — so a clip that is not there is refused rather
        // than silently linking nothing.
        ("link-with", &[("clip", ParamType::Text)]),
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
        // The lane's fader and its pan. Decibels reach the action surface as
        // decibels: an automation that wants "6 under" should be able to say
        // so, and a linear multiplier would make every caller do the log.
        ("set-gain", &[("db", ParamType::Number)]),
        ("set-pan", &[("pan", ParamType::Number)]),
        // Bringing a mix that clipped back under full scale. Headroom is
        // declared rather than assumed: "make it fit" and "leave me a dB" are
        // different intents and a caller has to be able to say which.
        ("trim-mix", &[("headroom", ParamType::Number)]),
    ];

    /// Both halves of the table, as one list.
    fn declared() -> Vec<(&'static str, &'static [(&'static str, ParamType)])> {
        DECLARED.into_iter().chain(DECLARED_MORE).collect()
    }

    // ---- linked audio and video -----------------------------------------

    /// A fixture whose picture source carries sound, so the palette offers to
    /// split that sound onto a clip of its own.
    fn ctx_with_sound() -> (VideoResourceCtx, Uuid) {
        let (mut ctx, cid) = ctx_fixture();
        for m in &mut ctx.project.media {
            m.has_audio = true;
        }
        (ctx, cid)
    }

    fn ctx_of(p: Project, playhead: f64) -> VideoResourceCtx {
        VideoResourceCtx {
            project: p,
            playhead,
            ..Default::default()
        }
    }

    /// The picture clip and the sound clip of `p`'s only shot.
    fn halves(p: &Project) -> (Uuid, Uuid) {
        let picture = p
            .clips
            .iter()
            .find(|c| p.track(c.track).is_some_and(|t| t.kind == TrackKind::Video))
            .expect("a picture clip")
            .id;
        let sound = p
            .clips
            .iter()
            .find(|c| p.track(c.track).is_some_and(|t| t.kind == TrackKind::Audio))
            .expect("a sound clip")
            .id;
        (picture, sound)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn linked_palette_edits_carry_both_halves() {
        // Case `the_palette_splits_a_shots_sound_onto_its_own_clip`:
        // Splitting the sound off is an action, so a script, an agent and a
        // reader can do it — and it makes the audio lane it needs when the
        // project has none, in the same change, so it never half-lands.
        {
            let (ctx, cid) = ctx_with_sound();
            assert_eq!(
                ctx.project.tracks.len(),
                1,
                "the fixture starts with only a picture lane",
            );
            let after = run(&ctx.project, &spec(&ctx, "link-audio"), None);
            assert_eq!(after.clips.len(), 2, "the sound is a clip of its own");
            assert_eq!(
                after.tracks.len(),
                2,
                "and the lane it needed came with it, in the same change",
            );
            let (picture, sound) = halves(&after);
            assert_eq!(picture, cid);
            assert_eq!(
                after.clip(picture).expect("picture").link,
                after.clip(sound).expect("sound").link,
            );
            assert!(after.is_linked(picture));
            assert!(
                after.clip(picture).expect("picture").sound_split,
                "or the shot would be heard twice",
            );
            assert!(
                !crate::audio::plan_audio(&after, 4.0)
                    .iter()
                    .any(|l| l.clip == picture),
                "the picture stops carrying the sound the audio clip now has",
            );
        }
        // Case `the_palette_unlinks_a_pair_and_links_it_back`:
        // Unlinking is explicit and reversible, both ways round, from the palette.
        {
            let (ctx, _cid) = ctx_with_sound();
            let split = run(&ctx.project, &spec(&ctx, "link-audio"), None);
            let (picture, sound) = halves(&split);
            let ctx = ctx_of(split, 4.0);

            let cut = run(&ctx.project, &spec(&ctx, "unlink"), None);
            // Behaviour, not representation: the group id survives a break so the
            // editor can still find the sound this shot was split from, and it is
            // `link_follows` that stops the two moving together.
            assert!(!cut.is_linked(picture) && !cut.is_linked(sound));
            assert!(cut.link_partners(picture).is_empty());
            assert_eq!(
                cut.detached_sound(picture),
                Some(sound),
                "the sound is still findable, which is what stops a second copy \
             of it being offered",
            );
            assert_eq!(cut.clips.len(), 2, "unlinking removes nothing");

            // …and back, by naming the partner. The ids that exist are declared
            // as the choices, so a clip that is not there is refused.
            let ctx = ctx_of(cut, 4.0);
            let sp = spec(&ctx, "link-with");
            let choices = &sp.params()[0].choices;
            assert!(
                choices.iter().any(|c| c.as_ref() == sound.to_string()),
                "the other clip's id is offered: {choices:?}",
            );
            let relinked = run(&ctx.project, &sp, Some(&sound.to_string()));
            assert!(
                relinked.is_linked(picture),
                "a pair that was separated has to be able to come back together",
            );
            assert_eq!(
                relinked.clip(picture).expect("picture").link,
                relinked.clip(sound).expect("sound").link,
            );
        }
        // Case `every_palette_edit_that_follows_the_link_carries_both_halves`:
        // Delete, ripple-delete, split, trim and re-time all follow the link
        // through the palette, exactly as they do on screen.
        {
            let (ctx, _cid) = ctx_with_sound();
            let split = run(&ctx.project, &spec(&ctx, "link-audio"), None);
            let (picture, sound) = halves(&split);
            let ctx = ctx_of(split, 4.0);

            let gone = run(&ctx.project, &spec(&ctx, "delete"), None);
            assert!(
                gone.clip(picture).is_none() && gone.clip(sound).is_none(),
                "half a shot left on the timeline is the surprise a link avoids",
            );

            let rippled = run(&ctx.project, &spec(&ctx, "ripple-delete"), None);
            assert!(rippled.clip(picture).is_none() && rippled.clip(sound).is_none());

            let cut = run(&ctx.project, &spec(&ctx, "split"), None);
            assert_eq!(cut.clips.len(), 4, "both lanes were cut");
            let tails: Vec<&Clip> = cut.clips.iter().filter(|c| c.start > 3.9).collect();
            assert_eq!(tails.len(), 2);
            assert_eq!(
                tails[0].link, tails[1].link,
                "the two tails share a group of their own",
            );
            assert_ne!(
                tails[0].link,
                cut.clip(picture).expect("head").link,
                "and it is not the heads'",
            );

            let fast = run(&ctx.project, &spec(&ctx, "set-speed"), Some("2"));
            assert!((fast.clip(picture).expect("picture").speed.rate() - 2.0).abs() < 1e-9);
            assert!(
                (fast.clip(sound).expect("sound").speed.rate() - 2.0).abs() < 1e-9,
                "re-timing one half and not the other desyncs them permanently",
            );

            let trimmed = run(&ctx.project, &spec(&ctx, "trim-in"), None);
            assert!(
                (trimmed.clip(picture).expect("picture").in_point
                    - trimmed.clip(sound).expect("sound").in_point)
                    .abs()
                    < 1e-9,
                "the sound keeps covering the picture",
            );
            assert!(trimmed.clip(sound).expect("sound").in_point > 0.0);
        }
        // Case `unlinked_clip_edits_use_one_optimistic_member_patch`:
        // An unlinked clip uses the same narrow action shape as a linked group:
        // one optimistic member patch rather than an old full snapshot.
        {
            let (ctx, cid) = ctx_fixture();
            let sp = spec(&ctx, "set-speed");
            let from_spec = prompted_encode(&sp, "2");
            let mut expected = ctx.project.clip(cid).expect("the clip").clone();
            expected.set_speed(2.0, false);
            let mut after = ctx.project.clone();
            *after.clip_mut(cid).unwrap() = expected.clone();
            assert_eq!(
                from_spec.payload,
                encode(&patch_clips_fields_action(&ctx.project, &after, &[cid])).payload,
                "one clip is one narrow member patch",
            );
            let del = spec_encoding(&spec(&ctx, "delete"), None);
            assert_eq!(
                del.payload,
                encode(&delete_clip_action(&cid)).payload,
                "and one delete is still one delete",
            );
        }
        // Case `linked_membership_patch_is_atomic_and_refuses_a_stale_group_member`.
        {
            let (ctx, first) = ctx_fixture();
            let mut project = ctx.project;
            let second = Uuid::new_v4();
            let mut sibling = project.clip(first).expect("fixture clip").clone();
            sibling.id = second;
            let old_group = Uuid::new_v4();
            let primary = project.clip_mut(first).expect("first");
            primary.link = Some(old_group);
            primary.link_follows = true;
            sibling.link = Some(old_group);
            sibling.link_follows = true;
            project.clips.push(sibling);

            let new_group = Uuid::new_v4();
            let action = VideoEditorAction::PatchClipsFields {
                patches: vec![
                    ClipPatchData {
                        clip: first.to_string(),
                        changes: vec![ClipFieldPatch::Link {
                            expected: Some(old_group.to_string()),
                            value: Some(new_group.to_string()),
                        }],
                    },
                    ClipPatchData {
                        clip: second.to_string(),
                        changes: vec![ClipFieldPatch::Link {
                            expected: Some(old_group.to_string()),
                            value: Some(new_group.to_string()),
                        }],
                    },
                ],
            };

            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &project);
            action.apply(&mut doc).expect("both members still match");
            let linked = VideoDoc::load_project(&doc);
            assert_eq!(linked.clip(first).expect("first").link, Some(new_group));
            assert_eq!(linked.clip(second).expect("second").link, Some(new_group));

            // Rewind the fixture and let a peer alter only one member. The action
            // must validate both before it writes either, so the first cannot be
            // moved into a group whose second half rejected the replay.
            VideoDoc::write_project(&mut doc, &project);
            let changed_group = Uuid::new_v4();
            let mut peer = project.clone();
            peer.clip_mut(second).expect("second").link = Some(changed_group);
            VideoDoc::upsert_clip(&mut doc, peer.clip(second).expect("second"));
            assert!(
                action.apply(&mut doc).is_err(),
                "stale member refuses group"
            );
            let refused = VideoDoc::load_project(&doc);
            assert_eq!(refused.clip(first).expect("first").link, Some(old_group));
            assert_eq!(
                refused.clip(second).expect("second").link,
                Some(changed_group)
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    #[expect(
        clippy::float_cmp,
        reason = "the values are written through an action and read back unchanged, or clamped to a literal bound"
    )]
    fn captured_field_actions_refuse_stale_values() {
        // Case `an_effect_resource_edits_the_named_instance_and_carries_it_through_replay`.
        {
            let (mut ctx, clip_id) = ctx_fixture();
            let clip = ctx.project.clip_mut(clip_id).expect("fixture clip");
            clip.add_effect(EffectKind::Sepia);
            clip.add_effect(EffectKind::Sepia);
            clip.set_key_at(Param::EffectAmount(0), 0.0, 0.2, Interp::Linear);
            clip.set_key_at(Param::EffectAmount(1), 0.0, 0.8, Interp::Linear);
            let first = clip.effects[0].id;
            let second = clip.effects[1].id;
            let resource = effect_resource_id(clip_id, second);

            let amount = run(
                &ctx.project,
                &spec_on(&ctx, &resource, "set-amount"),
                Some("0.25"),
            );
            let changed = amount.clip(clip_id).expect("written clip");
            assert_eq!(
                changed
                    .effects
                    .iter()
                    .find(|effect| effect.id == second)
                    .map(|e| e.amount),
                Some(0.25),
                "the duplicate named by id changed",
            );
            assert_eq!(
                changed
                    .effects
                    .iter()
                    .find(|effect| effect.id == first)
                    .map(|e| e.amount),
                Some(EffectKind::Sepia.default_amount()),
                "its neighbour was not selected by index",
            );

            ctx.project = amount;
            let moved = run(&ctx.project, &spec_on(&ctx, &resource, "move-up"), None);
            assert_eq!(
                moved.clip(clip_id).expect("moved clip").effects[0].id,
                second
            );
            let moved_clip = moved.clip(clip_id).expect("moved clip");
            assert_eq!(
                moved_clip.anim.sample(Param::EffectAmount(0), 0.0),
                Some(0.8),
                "the moved effect carried its keyed amount"
            );
            assert_eq!(
                moved_clip.anim.sample(Param::EffectAmount(1), 0.0),
                Some(0.2),
                "its neighbour carried its own curve in the other direction"
            );

            ctx.project = moved;
            let removed = run(&ctx.project, &spec_on(&ctx, &resource, "remove"), None);
            assert!(
                removed
                    .clip(clip_id)
                    .expect("removed clip")
                    .effect_index(second)
                    .is_none(),
                "the exact addressed instance was removed",
            );
            assert_eq!(
                removed
                    .clip(clip_id)
                    .expect("removed clip")
                    .anim
                    .sample(Param::EffectAmount(0), 0.0),
                Some(0.2),
                "removing the head closes the animation slot with the remaining effect"
            );

            let mut before_clear = removed;
            before_clear.clip_mut(clip_id).unwrap().set_key_at(
                Param::Opacity,
                0.0,
                0.6,
                Interp::Linear,
            );
            ctx.project = before_clear;
            let cleared = run(
                &ctx.project,
                &spec_on(&ctx, &clip_resource_id(clip_id), "clear-effects"),
                None,
            );
            let cleared_clip = cleared.clip(clip_id).expect("cleared clip");
            assert!(cleared_clip.effects.is_empty());
            assert_eq!(
                cleared_clip.anim.sample(Param::EffectAmount(0), 0.0),
                None,
                "clearing effects clears their now-orphaned curves"
            );
            assert_eq!(
                cleared_clip.anim.sample(Param::Opacity, 0.0),
                Some(0.6),
                "clearing effects preserves unrelated animation"
            );
        }
        // Case `captured_effect_actions_refuse_stale_stack_changes_without_overwriting_other_fields`.
        {
            let (mut ctx, clip_id) = ctx_fixture();
            let clip = ctx.project.clip_mut(clip_id).expect("fixture clip");
            clip.add_effect(EffectKind::Sepia);
            clip.add_effect(EffectKind::Invert);
            let first = clip.effects[0].id;
            let second = clip.effects[1].id;
            let second_resource = effect_resource_id(clip_id, second);

            // A captured amount action applies after an unrelated opacity edit,
            // and only writes `effects` — it must not restore old opacity.
            let amount: VideoEditorAction = serde_json::from_slice(
                &prompted_encode(&spec_on(&ctx, &second_resource, "set-amount"), "0.25").payload,
            )
            .expect("effect action decodes");
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &ctx.project);
            let mut other_field = ctx.project.clone();
            other_field.clip_mut(clip_id).expect("clip").opacity = 0.37;
            VideoDoc::upsert_clip(&mut doc, other_field.clip(clip_id).expect("changed clip"));
            amount
                .apply(&mut doc)
                .expect("unrelated write does not stale effect");
            let after = VideoDoc::load_project(&doc);
            let after_clip = after.clip(clip_id).expect("clip remains");
            assert_eq!(
                after_clip.opacity, 0.37,
                "old snapshot did not overwrite opacity"
            );
            assert_eq!(
                after_clip
                    .effects
                    .iter()
                    .find(|effect| effect.id == second)
                    .map(|effect| effect.amount),
                Some(0.25),
            );

            // A move captured against [first, second] refuses after a peer has
            // already reordered it; it cannot swap whatever happens to be there.
            let move_up: VideoEditorAction = serde_json::from_slice(
                &spec_encoding(&spec_on(&ctx, &second_resource, "move-up"), None).payload,
            )
            .expect("move action decodes");
            let mut reordered = ctx.project.clone();
            reordered
                .clip_mut(clip_id)
                .expect("clip")
                .move_effect_down(0);
            VideoDoc::upsert_clip(&mut doc, reordered.clip(clip_id).expect("reordered clip"));
            assert!(
                move_up.apply(&mut doc).is_err(),
                "changed neighbour is stale"
            );
            assert_eq!(
                VideoDoc::load_project(&doc)
                    .clip(clip_id)
                    .expect("clip")
                    .effects[0]
                    .id,
                second,
                "the peer order survived the refused action",
            );

            // Removal also carries the value it saw. A peer changing that value
            // makes delete refuse, and a peer deleting it makes set refuse.
            let first_resource = effect_resource_id(clip_id, first);
            let remove: VideoEditorAction = serde_json::from_slice(
                &spec_encoding(&spec_on(&ctx, &first_resource, "remove"), None).payload,
            )
            .expect("remove action decodes");
            let mut changed = ctx.project.clone();
            changed.clip_mut(clip_id).expect("clip").effects[0].amount = 0.1;
            VideoDoc::upsert_clip(&mut doc, changed.clip(clip_id).expect("changed clip"));
            assert!(remove.apply(&mut doc).is_err(), "changed instance is stale");
            assert!(
                VideoDoc::load_project(&doc)
                    .clip(clip_id)
                    .expect("clip")
                    .effect_index(first)
                    .is_some(),
                "refusal did not delete a changed effect",
            );
        }
        // Case `captured_clip_field_action_updates_only_its_field_and_refuses_a_stale_value`.
        {
            let (ctx, clip_id) = ctx_fixture();
            let action: VideoEditorAction = serde_json::from_slice(
                &prompted_encode(&spec(&ctx, "set-brightness"), "0.9").payload,
            )
            .expect("field action decodes");
            assert!(
                matches!(&action, VideoEditorAction::PatchClipFields { .. }),
                "resource edits are narrow patches, never whole clip snapshots",
            );

            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &ctx.project);
            let mut peer = ctx.project.clone();
            let peer_clip = peer.clip_mut(clip_id).expect("fixture clip");
            peer_clip.start += 3.0;
            peer_clip.opacity = 0.42;
            VideoDoc::upsert_clip(&mut doc, peer.clip(clip_id).expect("peer clip"));
            action
                .apply(&mut doc)
                .expect("unrelated peer fields do not stale the patch");
            let written = VideoDoc::load_project(&doc)
                .clip(clip_id)
                .expect("clip")
                .clone();
            assert_eq!(written.start, peer.clip(clip_id).expect("peer clip").start);
            assert_eq!(written.opacity, 0.42);
            assert_eq!(written.filters.brightness, 0.9);

            let mut stale = ctx.project.clone();
            stale
                .clip_mut(clip_id)
                .expect("fixture clip")
                .filters
                .brightness = 0.1;
            VideoDoc::upsert_clip(&mut doc, stale.clip(clip_id).expect("stale clip"));
            assert!(
                action.apply(&mut doc).is_err(),
                "a changed property refuses"
            );
            let after_project = VideoDoc::load_project(&doc);
            let after_refusal = after_project.clip(clip_id).expect("clip");
            assert_eq!(after_refusal.filters.brightness, 0.1);
        }
        // Case `palette_and_curves_are_narrow_existing_clip_fields`.
        {
            let (ctx, clip_id) = ctx_fixture();
            let before = ctx.project.clip(clip_id).expect("fixture clip").clone();
            let mut after = before.clone();
            after.palette = 6;
            after.curves.master.set_point(0.5, 0.8);
            let VideoEditorAction::PatchClipFields { changes, .. } =
                clip_field_patch_action(&before, &after)
            else {
                panic!("existing clip diff is narrow");
            };
            assert!(changes.iter().any(|patch| matches!(
                patch,
                ClipFieldPatch::Byte {
                    field: ClipByteField::Palette,
                    ..
                }
            )));
            assert!(changes.iter().any(|patch| matches!(
                patch,
                ClipFieldPatch::Compound {
                    field: ClipCompoundField::Curves,
                    ..
                }
            )));

            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &ctx.project);
            VideoEditorAction::PatchClipFields {
                clip: clip_id.to_string(),
                changes,
            }
            .apply(&mut doc)
            .expect("narrow palette and curve patch applies");
            let written = VideoDoc::load_project(&doc)
                .clip(clip_id)
                .expect("clip")
                .clone();
            assert_eq!(written.palette, 6);
            assert!((written.curves.master.eval(0.5) - 0.8).abs() < 1e-9);
        }
    }

    // ---- trimming a mix that clipped -------------------------------------

    /// Two audio lanes with clips on them, and a meter that has measured the
    /// pair `over` decibels past full scale on both.
    fn ctx_with_clipped_mix(over_db: f64) -> (VideoResourceCtx, Vec<Uuid>) {
        let (ctx, _) = ctx_fixture();
        let mut p = ctx.project.clone();
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "a.wav".into(),
            kind: MediaKind::Audio,
            duration: 30.0,
            path: "media/a.wav".into(),
            hydration: crate::media::Hydration::Local,
            ..Default::default()
        });
        let mut lanes = Vec::new();
        for index in 1..=2u32 {
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: format!("A{index}"),
                kind: TrackKind::Audio,
                index,
                ..Default::default()
            });
            p.add_clip(tid, mid, 0.0).expect("media exists");
            lanes.push(tid);
        }
        let peak = crate::model::db_to_linear(over_db) as f32;
        let meters = crate::audio::MixMeters::new();
        meters.record(
            2.0,
            &crate::audio::MixReport {
                master: crate::audio::BlockMeter { peak: [peak, peak] },
                tracks: lanes
                    .iter()
                    .map(|t| {
                        (
                            *t,
                            crate::audio::BlockMeter {
                                peak: [peak * 0.6, peak * 0.6],
                            },
                        )
                    })
                    .collect(),
            },
        );
        (
            VideoResourceCtx {
                project: p,
                mix: meters.snapshot(),
                ..ctx
            },
            lanes,
        )
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn mix_trim_and_lane_gain_actions_follow_the_measurement() {
        // Case `trimming_is_offered_only_once_a_mix_has_actually_been_measured_over`:
        // The action is a report about a measurement, so it exists only once
        // there is one. Offering to "fix" the levels of a mix nobody has listened
        // to would be offering to quieten a mix that is fine.
        {
            let (quiet, _) = ctx_fixture();
            assert!(
                !all_specs(&quiet).iter().any(|(id, _)| id == "trim-mix"),
                "nothing has clipped, so there is nothing to trim",
            );
            let (clipped, _) = ctx_with_clipped_mix(3.0);
            assert!(
                all_specs(&clipped).iter().any(|(id, _)| id == "trim-mix"),
                "a measured overload is what makes the trim meaningful",
            );
        }
        // Case `setting_a_lanes_gain_and_pan_through_the_action_moves_the_lane`:
        // Running it takes every lane in the mix down by exactly what the meter
        // measured, and by the *same* amount, so the balance survives.
        // Invoking `set-gain` and `set-pan` actually moves the lane, and out of
        // range is clamped rather than stored.
        //
        // The declared-shape table only proves these actions *say* they take a
        // number called `db`. It would pass just as happily if the closure read
        // a different parameter name, dropped the value, or clamped the wrong
        // way — which is the whole of what a caller with no screen depends on.
        {
            let (ctx, lanes) = ctx_with_clipped_mix(0.0);
            let a1 = lanes[0];

            let lane = format!("video.track:{a1}");
            let after = run(&ctx.project, &spec_on(&ctx, &lane, "set-gain"), Some("-6"));
            assert!(
                (after.track(a1).expect("A1").gain_db + 6.0).abs() < 1e-6,
                "the lane came down 6 dB: {}",
                after.track(a1).expect("A1").gain_db,
            );

            let after = run(&ctx.project, &spec_on(&ctx, &lane, "set-pan"), Some("-1"));
            assert!(
                (after.track(a1).expect("A1").pan + 1.0).abs() < 1e-6,
                "the lane went hard left: {}",
                after.track(a1).expect("A1").pan,
            );

            // Beyond the ends, the intent is legible even though the number is
            // not, so it is clamped to the end rather than refused or stored raw.
            let after = run(&ctx.project, &spec_on(&ctx, &lane, "set-gain"), Some("999"));
            assert!(
                (after.track(a1).expect("A1").gain_db - MAX_GAIN_DB).abs() < 1e-6,
                "asking for more than the fader has gives the top of the fader, \
             not 999: {}",
                after.track(a1).expect("A1").gain_db,
            );
            let after = run(&ctx.project, &spec_on(&ctx, &lane, "set-pan"), Some("-9"));
            assert!(
                (after.track(a1).expect("A1").pan + 1.0).abs() < 1e-6,
                "pan clamps to hard left: {}",
                after.track(a1).expect("A1").pan,
            );

            // And `reset-mix` puts both back in one step — the reason it exists
            // rather than leaving a caller to send `set-gain 0` and forget the pan.
            let mut moved = ctx.project.clone();
            {
                let t = moved.tracks.iter_mut().find(|t| t.id == a1).expect("A1");
                t.gain_db = 4.5;
                t.pan = -0.35;
            }
            let moved_ctx = VideoResourceCtx {
                project: moved,
                mix: ctx.mix.clone(),
                ..ctx
            };
            let after = run(
                &moved_ctx.project,
                &spec_on(&moved_ctx, &lane, "reset-mix"),
                None,
            );
            let t = after.track(a1).expect("A1");
            assert!(
                t.gain_db.abs() < 1e-6 && t.pan.abs() < 1e-6,
                "reset puts the lane back to unity and centre in one step: \
             {} dB, pan {}",
                t.gain_db,
                t.pan,
            );
        }
        // Case `the_trim_action_lowers_every_lane_by_the_measured_overshoot`.
        {
            let (ctx, lanes) = ctx_with_clipped_mix(4.0);
            // Two lanes deliberately at different levels, so "the balance is
            // unchanged" is a claim with something to be wrong about.
            let mut p = ctx.project.clone();
            p.tracks
                .iter_mut()
                .find(|t| t.id == lanes[0])
                .expect("A1")
                .gain_db = 2.0;
            p.tracks
                .iter_mut()
                .find(|t| t.id == lanes[1])
                .expect("A2")
                .gain_db = -5.0;
            let ctx = VideoResourceCtx {
                project: p,
                mix: ctx.mix.clone(),
                ..ctx
            };

            let after = run(&ctx.project, &spec(&ctx, "trim-mix"), Some("0"));
            let want = ctx.mix.worst_over_db();
            assert!(
                (want - 4.0).abs() < 1e-6,
                "the fixture measured 4 dB over: {want}",
            );
            assert!(
                (after.track(lanes[0]).expect("A1").gain_db - (2.0 - want)).abs() < 1e-6,
                "A1 came down by the overshoot: {}",
                after.track(lanes[0]).expect("A1").gain_db,
            );
            assert!(
                (after.track(lanes[1]).expect("A2").gain_db - (-5.0 - want)).abs() < 1e-6,
                "and A2 by the same, so they are still 7 dB apart: {}",
                after.track(lanes[1]).expect("A2").gain_db,
            );
        }
        // Case `the_trims_headroom_argument_is_read`:
        // The declared argument is honoured rather than decorative.
        {
            let (ctx, lanes) = ctx_with_clipped_mix(4.0);
            let plain = run(&ctx.project, &spec(&ctx, "trim-mix"), Some("0"));
            let spare = run(&ctx.project, &spec(&ctx, "trim-mix"), Some("3"));
            let gain = |p: &Project| p.track(lanes[0]).expect("A1").gain_db;
            assert!(
                (gain(&plain) - gain(&spare) - 3.0).abs() < 1e-6,
                "three decibels of headroom is three decibels lower: {} vs {}",
                gain(&plain),
                gain(&spare),
            );
            // Negative headroom would be an instruction to leave it clipping,
            // which is the state this exists to end.
            let back = run(&ctx.project, &spec(&ctx, "trim-mix"), Some("-6"));
            assert!(
                (gain(&back) - gain(&plain)).abs() < 1e-6,
                "asking for headroom below zero is asking for none: {}",
                gain(&back),
            );
        }
        // Case `the_trim_states_the_measurement_it_is_a_report_about`:
        // The measurement reaches a caller that has no screen. This is the whole
        // point of putting it on the action surface: an agent or a script reads
        // the report from the enumeration, exactly as a person reads it off the
        // red chip.
        {
            let (ctx, _) = ctx_with_clipped_mix(4.0);
            let sp = spec(&ctx, "trim-mix");
            let words = sp.description.unwrap_or_default();
            assert!(words.contains("+4.0 dB over"), "how far over: {words}");
            assert!(words.contains("1 place"), "and in how many places: {words}");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn exported_edit_contracts_and_concurrent_replays_are_honest() {
        // Case `exported_edits_declare_undo_and_destructive_removals`.
        {
            let (ctx, _) = ctx_fixture();
            let specs = all_specs(&ctx);
            assert!(
                specs
                    .iter()
                    .filter(|(_, action)| action.kind == OperationKind::Document)
                    .all(|(_, action)| action.contract.reversible)
            );
            assert!(
                specs
                    .iter()
                    .filter(|(_, action)| action.kind != OperationKind::Document)
                    .all(|(_, action)| !action.contract.reversible)
            );
            let (_, delete) = specs
                .iter()
                .find(|(_, action)| action.id == "delete")
                .expect("fixture clip exports deletion");
            assert_eq!(delete.contract.risk, tilo_actions::RiskLevel::Destructive);
            assert_eq!(
                delete.contract.confirmation,
                tilo_actions::ConfirmationPolicy::Confirm
            );
        }
        // Case `file_and_render_effects_are_canonical_and_not_falsely_reversible`.
        {
            let resources = provide_resources(&VideoResourceCtx::default());
            let root = &resources[0];
            for id in [
                "import",
                "open",
                "save",
                "save-as",
                "export",
                "cancel-export",
            ] {
                let action = root.actions.iter().find(|action| action.id == id).unwrap();
                assert_eq!(action.kind, OperationKind::Effect, "{id}");
                assert!(!action.contract.reversible, "{id}");
                if id != "cancel-export" {
                    assert!(
                        matches!(action.contract.execution, ExecutionMode::Background { .. }),
                        "{id}"
                    );
                    assert_eq!(action.contract.result, ResultSpec::Job, "{id}");
                    assert_eq!(action.params()[0].name.as_ref(), "path", "{id}");
                }
            }
            let cancel = root
                .actions
                .iter()
                .find(|a| a.id == "cancel-export")
                .unwrap();
            assert!(!cancel.contract.availability.is_available());
        }
        // Case `concurrent_effect_actions_replay_in_either_order_with_one_stable_stack`.
        {
            let (ctx, clip) = ctx_fixture();
            let mut seed = AutoCommit::new();
            VideoDoc::write_project(&mut seed, &ctx.project);
            let initial = ctx.project.clip(clip).expect("fixture clip").effects.len();
            let first = VideoEditorAction::AddEffect {
                clip: clip.to_string(),
                ordinal: initial,
                effect: crate::model::ClipEffect::new(EffectKind::Sepia),
            };
            let second = VideoEditorAction::AddEffect {
                clip: clip.to_string(),
                ordinal: initial,
                effect: crate::model::ClipEffect::new(EffectKind::Vignette),
            };
            let mut forward = AutoCommit::load(&seed.save()).expect("fork");
            first.apply(&mut forward).expect("first applies");
            second.apply(&mut forward).expect("second applies");
            let mut reverse = AutoCommit::load(&seed.save()).expect("fork");
            second.apply(&mut reverse).expect("second applies");
            first.apply(&mut reverse).expect("first applies");
            let ids = |doc: &AutoCommit| {
                VideoDoc::load_project(doc)
                    .clip(clip)
                    .expect("clip")
                    .effects
                    .iter()
                    .map(|effect| effect.id)
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                ids(&forward),
                ids(&reverse),
                "rank + UUID tie-break is independent of delivery order"
            );
        }
        // Case `delete_wins_over_a_delayed_effect_update`.
        {
            let (ctx, clip) = ctx_fixture();
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &ctx.project);
            let effect = crate::model::ClipEffect::new(EffectKind::Sepia);
            let add = VideoEditorAction::AddEffect {
                clip: clip.to_string(),
                ordinal: 0,
                effect,
            };
            add.apply(&mut doc).expect("initial add applies");
            let remove = VideoEditorAction::RemoveEffect {
                clip: clip.to_string(),
                effect: effect.id.to_string(),
                expected_kind: effect.kind,
                expected_amount_bits: effect.amount.to_bits(),
            };
            let delayed = VideoEditorAction::SetEffectAmount {
                clip: clip.to_string(),
                effect: effect.id.to_string(),
                expected_amount_bits: effect.amount.to_bits(),
                amount: 0.2,
            };
            remove.apply(&mut doc).expect("delete applies");
            assert!(
                delayed.apply(&mut doc).is_err(),
                "a delayed update cannot recreate a deleted effect"
            );
            assert!(
                add.apply(&mut doc).is_err(),
                "a delayed original action is stopped by the durable tombstone"
            );
            assert!(
                VideoDoc::load_project(&doc)
                    .clip(clip)
                    .expect("clip")
                    .effects
                    .iter()
                    .all(|entry| entry.id != effect.id)
            );
        }
        // Case `delayed_structural_clip_upsert_keeps_new_nested_effects_and_markers`.
        {
            let (ctx, clip) = ctx_fixture();
            let stale = VideoEditorAction::UpsertClip(ClipData::from_clip(
                ctx.project.clip(clip).expect("fixture clip"),
            ));
            let effect = crate::model::ClipEffect::new(EffectKind::Sepia);
            let marker = crate::model::Marker::new(2.0, "beat", 3);
            let add_effect = VideoEditorAction::AddEffect {
                clip: clip.to_string(),
                ordinal: 0,
                effect,
            };
            let add_marker = VideoEditorAction::AddClipMarker {
                clip: clip.to_string(),
                marker: marker.clone(),
            };
            let mut doc = AutoCommit::new();
            VideoDoc::write_project(&mut doc, &ctx.project);
            add_effect.apply(&mut doc).expect("new effect applies");
            add_marker.apply(&mut doc).expect("new marker applies");
            stale
                .apply(&mut doc)
                .expect("delayed structural upsert remains valid");
            let written = VideoDoc::load_project(&doc);
            let written = written.clip(clip).expect("clip remains");
            assert!(written.effects.iter().any(|entry| entry.id == effect.id));
            assert!(written.markers.iter().any(|entry| entry.id == marker.id));
        }
    }
}
