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
    Action, ActionError, ActionInput, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, Resource, ResourceProvider,
};

use crate::doc::VideoDoc;
use crate::model::{
    Clip, EffectKind, Filters, MediaItem, MediaKind, Project, Track, TrackKind, Transition,
};

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
    /// The ordered effects stack, carried verbatim so a whole-clip write
    /// (add / remove / reorder an effect) replays byte-identically.
    #[serde(default)]
    pub effects: Vec<crate::model::ClipEffect>,
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
            effects: c.effects.clone(),
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
            transition: Transition::from_key(&self.transition),
            palette: self.palette,
            filters: Filters {
                brightness: self.brightness,
                contrast: self.contrast,
                saturation: self.saturation,
            },
            fade_in: self.fade_in,
            fade_out: self.fade_out,
            effects: self.effects.clone(),
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
        }
    }

    fn to_track(&self) -> Option<Track> {
        Some(Track {
            id: self.id.parse::<Uuid>().ok()?,
            name: self.name.clone(),
            kind: TrackKind::from_key(&self.kind),
            index: self.index,
            muted: self.muted,
            locked: self.locked,
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
        }
    }

    fn to_media(&self) -> Option<MediaItem> {
        Some(MediaItem {
            id: self.id.parse::<Uuid>().ok()?,
            name: self.name.clone(),
            kind: MediaKind::from_key(&self.kind),
            duration: self.duration,
            width: self.width,
            height: self.height,
            fps: self.fps,
            palette: self.palette,
            path: self.path.clone(),
            hydration: crate::media::Hydration::from_key(&self.hydration),
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
}

impl ProjectMeta {
    pub fn from_project(p: &Project) -> Self {
        Self {
            name: p.name.clone(),
            fps: p.fps,
            width: p.width,
            height: p.height,
        }
    }

    fn to_project(&self) -> Project {
        let mut p = Project::new(&self.name);
        p.fps = self.fps;
        p.width = self.width;
        p.height = self.height;
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
    SplitClip {
        left: Box<ClipData>,
        right: Box<ClipData>,
    },
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
///
/// # Panics
///
/// Never panics: a `VideoEditorAction` that fails to serialize (it never
/// should — every field is a plain string, number or nested plain struct)
/// falls back to an empty-payload envelope rather than crashing the caller.
pub fn encode(action: &VideoEditorAction) -> EncodedAction {
    EncodedAction::encode(VIDEO_EDITOR_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: VIDEO_EDITOR_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
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
        left: Box::new(ClipData::from_clip(left)),
        right: Box::new(ClipData::from_clip(right)),
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
    ActionSpec::immediate(id, label, VIDEO_EDITOR_ACTION_TYPE, action).unwrap_or_else(|_| {
        ActionSpec {
            id: id.to_string(),
            label: label.to_string(),
            description: None,
            locus: action.locus(),
            kind: qos_actions::OperationKind::Document,
            input: ActionInput::Immediate(encode(action)),
        }
    })
}

/// Parse a slider value (`0..1`) from a palette argument, else the neutral
/// midpoint — a malformed number never wedges the build.
fn parse_unit(s: &str) -> f64 {
    s.trim().parse::<f64>().unwrap_or(0.5).clamp(0.0, 1.0)
}

/// Build the resource tree the editor currently has open: the project, its
/// tracks (top-to-bottom display order), and the clips on each track — every
/// document-mutating operation exported as an action.
#[allow(
    clippy::too_many_lines,
    reason = "one nested loop per resource level (project -> tracks -> clips, plus the bin); \
              the length is the depth of the tree published, not accidental complexity"
)]
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
                cr = cr.with_action(ActionSpec::prompted(
                    id,
                    label,
                    "Value 0–1",
                    "0.5",
                    ExecutionLocus::ClientLocal,
                    move |arg: &str| {
                        let v = parse_unit(arg);
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
                    if let Some(k) = EffectKind::from_key(arg) {
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

/// Map a palette transition name to the stored [`Transition`], tolerant of the
/// on-screen labels ("Cut", "Dissolve", …) as well as the stored tags.
fn parse_transition(s: &str) -> Transition {
    match s.trim().to_lowercase().as_str() {
        "cut" | "none" => Transition::None,
        "dissolve" | "crossdissolve" | "cross dissolve" => Transition::CrossDissolve,
        "fade" | "fadein" | "fade in" => Transition::FadeIn,
        "dip" | "diptoblack" | "dip to black" => Transition::DipToBlack,
        "wipe" => Transition::Wipe,
        other => Transition::from_key(other),
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
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    /// Ctrl+K on the editor yields the nested tree: the project → its tracks →
    /// the clips on each, each exporting the actions its data model implies.
    /// Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_project_track_clip_tree() {
        let (app, doc) = seeded();
        let resources = Widget::resources(&app, &doc);
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

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete clip")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Rename project")),
            "{labels:?}"
        );
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
