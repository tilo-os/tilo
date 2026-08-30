//! Pure data model for the editor — media bin, tracks, clips, project.
//!
//! This layer is rendering- and CRDT-free: it is the in-memory mirror of
//! the Automerge document (see [`crate::doc`]). The app reloads it from
//! the document every `sync`, mutates the document on every edit, then
//! reloads — so this struct is always a faithful projection of shared
//! state. All the non-trivial editing math (split, trim, move, ripple
//! duration) lives here behind unit tests so the widget layer stays thin.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::{self, Hydration};

pub type MediaId = Uuid;
pub type TrackId = Uuid;
pub type ClipId = Uuid;

/// Smallest clip we allow a trim/split to produce, in seconds. Stops a
/// drag from collapsing a clip to a zero-width sliver that can't be
/// grabbed again.
pub const MIN_CLIP: f64 = 0.10;

/// Per-clip colour + blend filters. All are linear, normalised so the
/// inspector sliders run 0..1 and a fresh clip is the identity:
///   * `brightness`/`contrast`/`saturation`: 0.5 = unchanged, 0 = none,
///     1 = doubled. (Stored as 0..1 so one slider widget fits them all.)
///
/// These are render parameters only — the model stores them, the preview
/// applies them, and they replicate field-by-field through the CRDT so two
/// peers grading the same clip converge.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Filters {
    pub brightness: f64,
    pub contrast: f64,
    pub saturation: f64,
}

impl Default for Filters {
    fn default() -> Self {
        // 0.5 is the neutral midpoint of every slider.
        Self {
            brightness: 0.5,
            contrast: 0.5,
            saturation: 0.5,
        }
    }
}

impl Filters {
    /// Identity filters leave the frame untouched.
    pub fn is_identity(&self) -> bool {
        (self.brightness - 0.5).abs() < 1e-6
            && (self.contrast - 0.5).abs() < 1e-6
            && (self.saturation - 0.5).abs() < 1e-6
    }
    /// Map a 0..1 slider value to a multiplier in roughly 0..2 (0.5→1.0).
    pub fn gain(v: f64) -> f64 {
        (v.clamp(0.0, 1.0) * 2.0).max(0.0)
    }
}

/// A stackable per-clip image effect, applied to the frame *after* the base
/// colour grade ([`Filters`]). Unlike the grade (three fixed axes), effects are
/// an ordered list the user builds: add, remove and reorder. `amount` is the
/// effect's intensity in 0..1 (1 = full).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipEffect {
    pub kind: EffectKind,
    pub amount: f64,
}

impl ClipEffect {
    pub fn new(kind: EffectKind) -> Self {
        Self {
            kind,
            amount: kind.default_amount(),
        }
    }
}

/// The kinds of stackable effect a clip can carry. Each transforms the graded
/// colour; the preview and the (gated) export apply them in list order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    Grayscale,
    Sepia,
    Invert,
    Warm,
    Cool,
    Vignette,
}

impl EffectKind {
    pub const ALL: [EffectKind; 6] = [
        EffectKind::Grayscale,
        EffectKind::Sepia,
        EffectKind::Invert,
        EffectKind::Warm,
        EffectKind::Cool,
        EffectKind::Vignette,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            EffectKind::Grayscale => "grayscale",
            EffectKind::Sepia => "sepia",
            EffectKind::Invert => "invert",
            EffectKind::Warm => "warm",
            EffectKind::Cool => "cool",
            EffectKind::Vignette => "vignette",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            EffectKind::Grayscale => "Grayscale",
            EffectKind::Sepia => "Sepia",
            EffectKind::Invert => "Invert",
            EffectKind::Warm => "Warm",
            EffectKind::Cool => "Cool",
            EffectKind::Vignette => "Vignette",
        }
    }
    pub fn from_key(s: &str) -> Option<Self> {
        Some(match s.trim().to_lowercase().as_str() {
            "grayscale" | "greyscale" | "grey" | "gray" | "b&w" => EffectKind::Grayscale,
            "sepia" => EffectKind::Sepia,
            "invert" | "negative" => EffectKind::Invert,
            "warm" => EffectKind::Warm,
            "cool" => EffectKind::Cool,
            "vignette" => EffectKind::Vignette,
            _ => return None,
        })
    }
    /// A sensible default intensity when the effect is first added.
    pub fn default_amount(self) -> f64 {
        match self {
            EffectKind::Vignette => 0.5,
            _ => 0.8,
        }
    }
}

/// What a piece of source media is. Drives the bin icon, whether it
/// lands on a video or audio track by default, and which inspector
/// controls (opacity vs. volume) apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Video => "video",
            MediaKind::Audio => "audio",
            MediaKind::Image => "image",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "audio" => MediaKind::Audio,
            "image" => MediaKind::Image,
            _ => MediaKind::Video,
        }
    }
    /// Does this media carry a picture? Images and video do; audio does not.
    pub fn is_visual(self) -> bool {
        matches!(self, MediaKind::Video | MediaKind::Image)
    }
}

/// A track is a horizontal lane. Video lanes stack above audio lanes;
/// `index` orders within a kind (V1 below V2, A1 above A2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

impl TrackKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "audio" => TrackKind::Audio,
            _ => TrackKind::Video,
        }
    }
}

/// Edit-point transition applied at a clip's head. `None` is a hard cut.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transition {
    None,
    CrossDissolve,
    FadeIn,
    DipToBlack,
    Wipe,
}

impl Transition {
    pub const ALL: [Transition; 5] = [
        Transition::None,
        Transition::CrossDissolve,
        Transition::FadeIn,
        Transition::DipToBlack,
        Transition::Wipe,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Transition::None => "none",
            Transition::CrossDissolve => "dissolve",
            Transition::FadeIn => "fade",
            Transition::DipToBlack => "dip",
            Transition::Wipe => "wipe",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Transition::None => "Cut",
            Transition::CrossDissolve => "Dissolve",
            Transition::FadeIn => "Fade",
            Transition::DipToBlack => "Dip",
            Transition::Wipe => "Wipe",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "dissolve" => Transition::CrossDissolve,
            "fade" => Transition::FadeIn,
            "dip" => Transition::DipToBlack,
            "wipe" => Transition::Wipe,
            _ => Transition::None,
        }
    }
}

/// An imported source clip sitting in the media bin. `duration` is the
/// full source length; clips reference a sub-range of it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: MediaId,
    pub name: String,
    pub kind: MediaKind,
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// Palette slot (0..N) used to tint the bin thumbnail and timeline
    /// clip body. Resolved to a real `Color` from the active theme — the
    /// model never stores raw RGB so theming stays OS-controlled.
    pub palette: u8,
    /// Workspace-relative path to the source bytes on the P2P FUSE
    /// (`/var/workspace/<path>`). This — not the bytes — is what the CRDT
    /// replicates; the decode path hydrates the bytes on demand. See
    /// [`crate::media`] for the seam.
    pub path: String,
    /// Whether the source bytes are hydrated locally yet. Defaults to
    /// `Projected`; the decode path flips it to `Local` once the FUSE
    /// resolves real bytes. View-derived, but persisted so a re-opened
    /// project remembers what was already fetched.
    #[serde(default = "default_hydration", with = "hydration_serde")]
    pub hydration: Hydration,
}

fn default_hydration() -> Hydration {
    Hydration::Projected
}

mod hydration_serde {
    use super::Hydration;
    use serde::{Deserialize, Deserializer, Serializer};
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "signature is fixed by serde's `#[serde(with = ...)]` contract, which always \
                  calls this with `&Hydration`"
    )]
    pub fn serialize<S: Serializer>(h: &Hydration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(h.as_str())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Hydration, D::Error> {
        Ok(Hydration::from_key(&String::deserialize(d)?))
    }
}

impl MediaItem {
    /// Resolve this item's bytes through the FUSE-media seam, reporting
    /// where they live and whether they're hydrated. The one place a
    /// reference becomes a (possibly-local) file.
    pub fn source(&self) -> media::MediaSource {
        media::resolve(&self.path)
    }
}

/// A track instance in the timeline.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub name: String,
    pub kind: TrackKind,
    pub index: u32,
    pub muted: bool,
    pub locked: bool,
}

/// A placed clip: a reference to media, positioned on a track, sourcing a
/// `[in_point, out_point]` window of that media.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub track: TrackId,
    pub media: MediaId,
    /// Timeline position of the clip's left edge, seconds.
    pub start: f64,
    /// Source in-point, seconds into the media.
    pub in_point: f64,
    /// Source out-point, seconds into the media.
    pub out_point: f64,
    pub opacity: f64,
    pub volume: f64,
    pub transition: Transition,
    pub palette: u8,
    /// Per-clip colour grade. Identity by default.
    #[serde(default)]
    pub filters: Filters,
    /// Fade-in length in seconds (audio ramp / video dip from black at the
    /// head). 0 = no fade.
    #[serde(default)]
    pub fade_in: f64,
    /// Fade-out length in seconds at the tail. 0 = no fade.
    #[serde(default)]
    pub fade_out: f64,
    /// Ordered stack of image effects applied after the base grade. Empty by
    /// default; the user adds / removes / reorders them.
    #[serde(default)]
    pub effects: Vec<ClipEffect>,
}

impl Clip {
    /// On-timeline length, seconds.
    pub fn duration(&self) -> f64 {
        (self.out_point - self.in_point).max(0.0)
    }
    /// Timeline position of the clip's right edge, seconds.
    pub fn end(&self) -> f64 {
        self.start + self.duration()
    }
    /// True if timeline time `t` falls within this clip's span.
    pub fn covers(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }

    /// Append an effect of `kind` (at its default intensity) to the stack.
    pub fn add_effect(&mut self, kind: EffectKind) {
        self.effects.push(ClipEffect::new(kind));
    }
    /// Remove the effect at `idx` (no-op if out of range).
    pub fn remove_effect(&mut self, idx: usize) {
        if idx < self.effects.len() {
            self.effects.remove(idx);
        }
    }
    /// Move the effect at `idx` one slot earlier (toward the start of the
    /// stack), changing the order it's applied. No-op at the top / out of range.
    pub fn move_effect_up(&mut self, idx: usize) {
        if idx > 0 && idx < self.effects.len() {
            self.effects.swap(idx, idx - 1);
        }
    }
    /// Move the effect at `idx` one slot later. No-op at the bottom / out of range.
    pub fn move_effect_down(&mut self, idx: usize) {
        if idx + 1 < self.effects.len() {
            self.effects.swap(idx, idx + 1);
        }
    }

    /// Envelope gain in 0..1 from the fade-in / fade-out ramps at timeline
    /// time `t`. 1.0 in the clip's steady middle; ramps linearly across the
    /// `fade_in`/`fade_out` regions. Used by both the preview (video dip)
    /// and the (gated) audio mix.
    pub fn envelope(&self, t: f64) -> f64 {
        let into = t - self.start;
        let dur = self.duration();
        if dur <= 0.0 {
            return 1.0;
        }
        let mut g: f64 = 1.0;
        if self.fade_in > 1e-6 && into < self.fade_in {
            g = g.min((into / self.fade_in).clamp(0.0, 1.0));
        }
        if self.fade_out > 1e-6 {
            let from_end = dur - into;
            if from_end < self.fade_out {
                g = g.min((from_end / self.fade_out).clamp(0.0, 1.0));
            }
        }
        g.clamp(0.0, 1.0)
    }
}

/// The whole editable project.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub media: Vec<MediaItem>,
    pub tracks: Vec<Track>,
    pub clips: Vec<Clip>,
}

impl Project {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            fps: 30.0,
            width: 1920,
            height: 1080,
            media: Vec::new(),
            tracks: Vec::new(),
            clips: Vec::new(),
        }
    }

    pub fn media(&self, id: MediaId) -> Option<&MediaItem> {
        self.media.iter().find(|m| m.id == id)
    }
    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }
    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Tracks in vertical display order, top → bottom: video lanes
    /// (highest index first), then audio lanes (lowest index first).
    /// This matches every NLE's "video above, audio below" convention.
    pub fn display_tracks(&self) -> Vec<&Track> {
        let mut video: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .collect();
        video.sort_by_key(|t| std::cmp::Reverse(t.index));
        let mut audio: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .collect();
        audio.sort_by_key(|a| a.index);
        video.into_iter().chain(audio).collect()
    }

    /// Clips on `track`, sorted left-to-right by start.
    pub fn clips_on(&self, track: TrackId) -> Vec<&Clip> {
        let mut v: Vec<&Clip> = self.clips.iter().filter(|c| c.track == track).collect();
        v.sort_by(|a, b| {
            a.start
                .partial_cmp(&b.start)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    /// Total timeline length = the furthest clip end (min 1s so an empty
    /// project still has a ruler to look at).
    pub fn duration(&self) -> f64 {
        self.clips
            .iter()
            .map(Clip::end)
            .fold(0.0_f64, f64::max)
            .max(1.0)
    }

    /// The video clip visible at timeline time `t`: the one on the
    /// highest video track (largest index) whose span covers `t`. This
    /// is what the preview canvas renders.
    pub fn top_video_clip_at(&self, t: f64) -> Option<&Clip> {
        let mut best: Option<(&Clip, u32)> = None;
        for c in &self.clips {
            let Some(track) = self.track(c.track) else {
                continue;
            };
            if track.kind != TrackKind::Video {
                continue;
            }
            if !c.covers(t) {
                continue;
            }
            if best.is_none_or(|(_, idx)| track.index > idx) {
                best = Some((c, track.index));
            }
        }
        best.map(|(c, _)| c)
    }

    /// Append a clip referencing `media`'s full length at timeline `start`
    /// on `track`. Returns the new clip id. Inherits the media's palette.
    pub fn add_clip(&mut self, track: TrackId, media: MediaId, start: f64) -> Option<ClipId> {
        let m = self.media(media)?.clone();
        let id = Uuid::new_v4();
        self.clips.push(Clip {
            id,
            track,
            media,
            start: start.max(0.0),
            in_point: 0.0,
            out_point: m.duration,
            opacity: 1.0,
            volume: 1.0,
            transition: Transition::None,
            palette: m.palette,
            filters: Filters::default(),
            fade_in: 0.0,
            fade_out: 0.0,
            effects: Vec::new(),
        });
        Some(id)
    }

    /// Split the clip at timeline time `at`. The left part keeps the id
    /// and is shortened; a fresh right part is appended. Returns the new
    /// right-hand clip id, or `None` if `at` isn't strictly inside the
    /// clip (nothing to split).
    pub fn split_clip(&mut self, id: ClipId, at: f64) -> Option<ClipId> {
        let clip = self.clip(id)?.clone();
        if at <= clip.start + MIN_CLIP || at >= clip.end() - MIN_CLIP {
            return None;
        }
        let offset = at - clip.start; // seconds into the clip == seconds into source
        let split_src = clip.in_point + offset;
        let mut right = clip;
        right.id = Uuid::new_v4();
        right.start = at;
        right.in_point = split_src;
        // A head transition belongs to the new cut, not the right half.
        right.transition = Transition::None;
        // The fade-in stays with the head (left); the fade-out belongs to
        // the tail (right). Each half keeps the relevant ramp only.
        right.fade_in = 0.0;
        // Shorten the left half to end at the split (and drop its tail fade,
        // which now lands inside the right clip).
        if let Some(left) = self.clip_mut(id) {
            left.out_point = split_src;
            left.fade_out = 0.0;
        }
        let new_id = right.id;
        self.clips.push(right);
        Some(new_id)
    }

    /// Move a clip's left edge to `new_start` (clamped to >= 0). Trimming
    /// is unaffected — the whole source window slides.
    pub fn move_clip(&mut self, id: ClipId, new_start: f64) {
        if let Some(c) = self.clip_mut(id) {
            c.start = new_start.max(0.0);
        }
    }

    /// Drag the left (in) handle by `delta` seconds. Positive `delta`
    /// trims later into the source (clip shrinks from the left and its
    /// timeline start moves right); negative reveals earlier source.
    /// Clamped so `in_point >= 0` and the clip keeps at least `MIN_CLIP`.
    pub fn trim_in(&mut self, id: ClipId, delta: f64) {
        if let Some(c) = self.clip_mut(id) {
            let max_delta = (c.out_point - c.in_point) - MIN_CLIP; // can't cross out
            let min_delta = -c.in_point; // can't go before source 0
            let d = delta.clamp(min_delta, max_delta);
            c.in_point += d;
            c.start += d;
        }
    }

    /// Drag the right (out) handle by `delta` seconds. Clamped so the
    /// clip stays within the source length and keeps at least `MIN_CLIP`.
    pub fn trim_out(&mut self, id: ClipId, delta: f64) {
        let media_dur = self
            .clip(id)
            .and_then(|c| self.media(c.media))
            .map(|m| m.duration);
        if let (Some(c), Some(media_dur)) = (self.clip_mut(id), media_dur) {
            let max_out = media_dur;
            let min_out = c.in_point + MIN_CLIP;
            c.out_point = (c.out_point + delta).clamp(min_out, max_out);
        }
    }

    pub fn delete_clip(&mut self, id: ClipId) {
        self.clips.retain(|c| c.id != id);
    }

    /// Delete a clip and close the gap: every later clip *on the same
    /// track* shifts left by the deleted clip's duration, so there's no
    /// hole where it was. Returns the ids of the clips that moved (so the
    /// caller can replicate exactly those through the CRDT) — empty if the
    /// clip didn't exist.
    pub fn ripple_delete(&mut self, id: ClipId) -> Vec<ClipId> {
        let Some(clip) = self.clip(id).cloned() else {
            return Vec::new();
        };
        let gap = clip.duration();
        let track = clip.track;
        let after = clip.start;
        self.clips.retain(|c| c.id != id);
        let mut moved = Vec::new();
        for c in &mut self.clips {
            if c.track == track && c.start >= after - 1e-9 {
                c.start = (c.start - gap).max(0.0);
                moved.push(c.id);
            }
        }
        moved
    }

    /// Candidate snap targets (seconds) a dragged clip edge should magnetise
    /// to: the playhead, plus the start/end of every *other* clip. The
    /// caller picks the nearest within a pixel threshold. `exclude` skips
    /// the clip being dragged so it never snaps to itself.
    pub fn snap_candidates(&self, exclude: ClipId, playhead: f64) -> Vec<f64> {
        let mut out = vec![0.0, playhead];
        for c in &self.clips {
            if c.id == exclude {
                continue;
            }
            out.push(c.start);
            out.push(c.end());
        }
        out
    }

    /// Snap `t` to the nearest candidate within `tol` seconds, else return
    /// `t` unchanged. Pure helper so the snapping is unit-testable.
    pub fn snap_to_candidates(t: f64, candidates: &[f64], tol: f64) -> f64 {
        let mut best: Option<(f64, f64)> = None; // (candidate, distance)
        for &c in candidates {
            let d = (c - t).abs();
            if d <= tol && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((c, d));
            }
        }
        best.map_or(t, |(c, _)| c)
    }

    /// Paste `clip` as a new independent clip starting at `start` on
    /// `track` (the clip keeps its trim window, filters and fades). Returns
    /// the new id. Used by copy/paste — the source clip is unchanged.
    pub fn paste_clip(&mut self, mut clip: Clip, track: TrackId, start: f64) -> ClipId {
        clip.id = Uuid::new_v4();
        clip.track = track;
        clip.start = start.max(0.0);
        let id = clip.id;
        self.clips.push(clip);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_with_clip() -> (Project, MediaId, TrackId, ClipId) {
        let mut p = Project::new("t");
        let m = MediaItem {
            id: Uuid::new_v4(),
            name: "src".into(),
            kind: MediaKind::Video,
            duration: 20.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            palette: 0,
            path: media::workspace_path("src.mov"),
            hydration: Hydration::Projected,
        };
        let mid = m.id;
        p.media.push(m);
        let t = Track {
            id: Uuid::new_v4(),
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 1,
            muted: false,
            locked: false,
        };
        let tid = t.id;
        p.tracks.push(t);
        let cid = p.add_clip(tid, mid, 0.0).unwrap();
        (p, mid, tid, cid)
    }

    #[test]
    fn add_clip_uses_full_source_length() {
        let (p, _m, _t, cid) = project_with_clip();
        let c = p.clip(cid).unwrap();
        assert_eq!(c.in_point, 0.0);
        assert_eq!(c.out_point, 20.0);
        assert_eq!(c.duration(), 20.0);
        assert_eq!(c.end(), 20.0);
    }

    #[test]
    fn split_produces_two_contiguous_clips() {
        let (mut p, _m, _t, cid) = project_with_clip();
        let right = p.split_clip(cid, 8.0).unwrap();
        let left = p.clip(cid).unwrap();
        let right = p.clip(right).unwrap();
        // Left ends exactly where right begins on the timeline.
        assert!((left.end() - 8.0).abs() < 1e-9);
        assert!((right.start - 8.0).abs() < 1e-9);
        // Source is contiguous across the cut.
        assert!((left.out_point - right.in_point).abs() < 1e-9);
        // Total duration is preserved.
        assert!((left.duration() + right.duration() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn split_outside_span_is_rejected() {
        let (mut p, _m, _t, cid) = project_with_clip();
        assert!(p.split_clip(cid, 0.0).is_none());
        assert!(p.split_clip(cid, 25.0).is_none());
        assert_eq!(p.clips.len(), 1);
    }

    #[test]
    fn trim_in_moves_start_and_in_together() {
        let (mut p, _m, _t, cid) = project_with_clip();
        p.trim_in(cid, 3.0);
        let c = p.clip(cid).unwrap();
        assert!((c.in_point - 3.0).abs() < 1e-9);
        assert!((c.start - 3.0).abs() < 1e-9);
        // right edge unchanged
        assert!((c.end() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn trim_out_clamps_to_source_length() {
        let (mut p, _m, _t, cid) = project_with_clip();
        p.trim_out(cid, 50.0); // way past the 20s source
        let c = p.clip(cid).unwrap();
        assert!((c.out_point - 20.0).abs() < 1e-9);
    }

    #[test]
    fn trim_in_cannot_cross_out_point() {
        let (mut p, _m, _t, cid) = project_with_clip();
        p.trim_in(cid, 1000.0);
        let c = p.clip(cid).unwrap();
        assert!(c.duration() >= MIN_CLIP - 1e-9);
    }

    #[test]
    fn top_video_clip_prefers_higher_track() {
        let (mut p, mid, _t, _cid) = project_with_clip();
        // Add a higher video track with an overlapping clip.
        let t2 = Track {
            id: Uuid::new_v4(),
            name: "V2".into(),
            kind: TrackKind::Video,
            index: 2,
            muted: false,
            locked: false,
        };
        let t2id = t2.id;
        p.tracks.push(t2);
        let over = p.add_clip(t2id, mid, 0.0).unwrap();
        let top = p.top_video_clip_at(5.0).unwrap();
        assert_eq!(top.id, over, "V2 clip must win over V1 at the same time");
    }

    #[test]
    fn display_tracks_order_video_above_audio() {
        let mut p = Project::new("t");
        for (name, kind, index) in [
            ("V1", TrackKind::Video, 1),
            ("V2", TrackKind::Video, 2),
            ("A1", TrackKind::Audio, 1),
            ("A2", TrackKind::Audio, 2),
        ] {
            p.tracks.push(Track {
                id: Uuid::new_v4(),
                name: name.into(),
                kind,
                index,
                muted: false,
                locked: false,
            });
        }
        let order: Vec<&str> = p.display_tracks().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(order, vec!["V2", "V1", "A1", "A2"]);
    }

    #[test]
    fn ripple_delete_closes_the_gap() {
        let (mut p, mid, tid, first) = project_with_clip();
        // first occupies [0,20]. Place two more after it.
        let second = p.add_clip(tid, mid, 20.0).unwrap();
        let third = p.add_clip(tid, mid, 40.0).unwrap();
        let first_dur = p.clip(first).unwrap().duration();
        let moved = p.ripple_delete(first);
        // Both later clips shifted left by the deleted duration.
        assert!(moved.contains(&second) && moved.contains(&third));
        assert!((p.clip(second).unwrap().start - (20.0 - first_dur)).abs() < 1e-9);
        assert!((p.clip(third).unwrap().start - (40.0 - first_dur)).abs() < 1e-9);
        assert_eq!(p.clips.len(), 2);
    }

    #[test]
    fn snap_magnetises_within_tolerance_only() {
        let (p, _m, _t, cid) = project_with_clip();
        // Candidates include 0.0 and the clip end (20.0) and playhead 7.0.
        let cands = p.snap_candidates(Uuid::new_v4(), 7.0);
        // 19.9 is within 0.2 of the 20.0 clip end → snaps.
        assert!((Project::snap_to_candidates(19.9, &cands, 0.2) - 20.0).abs() < 1e-9);
        // 7.1 snaps to the playhead at 7.0.
        assert!((Project::snap_to_candidates(7.1, &cands, 0.2) - 7.0).abs() < 1e-9);
        // 12.0 is far from every candidate → unchanged.
        assert!((Project::snap_to_candidates(12.0, &cands, 0.2) - 12.0).abs() < 1e-9);
        // Excluding the clip removes its edges from the candidate set.
        let excl = p.snap_candidates(cid, 7.0);
        assert!((Project::snap_to_candidates(19.9, &excl, 0.2) - 19.9).abs() < 1e-9);
    }

    #[test]
    fn paste_clip_is_independent_of_source() {
        let (mut p, _m, tid, cid) = project_with_clip();
        let src = p.clip(cid).unwrap().clone();
        let pasted = p.paste_clip(src, tid, 30.0);
        assert_ne!(pasted, cid, "pasted clip gets a fresh id");
        assert!((p.clip(pasted).unwrap().start - 30.0).abs() < 1e-9);
        // Mutating the paste leaves the source clip untouched.
        p.clip_mut(pasted).unwrap().opacity = 0.1;
        assert!((p.clip(cid).unwrap().opacity - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fade_envelope_ramps_at_head_and_tail() {
        let (mut p, _m, _t, cid) = project_with_clip(); // [0,20]
        {
            let c = p.clip_mut(cid).unwrap();
            c.fade_in = 2.0;
            c.fade_out = 4.0;
        }
        let c = p.clip(cid).unwrap();
        assert!(c.envelope(0.0) < 0.01, "silent at the very head");
        assert!(
            (c.envelope(1.0) - 0.5).abs() < 1e-6,
            "half through a 2s fade-in"
        );
        assert!((c.envelope(10.0) - 1.0).abs() < 1e-6, "full in the middle");
        assert!(
            (c.envelope(18.0) - 0.5).abs() < 1e-6,
            "half through a 4s fade-out"
        );
        assert!(c.envelope(20.0) < 0.01, "silent at the tail");
    }

    #[test]
    fn split_keeps_fade_in_on_head_and_fade_out_on_tail() {
        let (mut p, _m, _t, cid) = project_with_clip(); // [0,20]
        {
            let c = p.clip_mut(cid).unwrap();
            c.fade_in = 2.0;
            c.fade_out = 3.0;
        }
        let right = p.split_clip(cid, 8.0).unwrap();
        let left = p.clip(cid).unwrap();
        let right = p.clip(right).unwrap();
        assert!((left.fade_in - 2.0).abs() < 1e-9, "head keeps fade-in");
        assert_eq!(left.fade_out, 0.0, "head loses tail fade");
        assert_eq!(right.fade_in, 0.0, "tail loses head fade");
        assert!((right.fade_out - 3.0).abs() < 1e-9, "tail keeps fade-out");
    }

    #[test]
    fn effects_stack_add_remove_and_reorder() {
        let (mut p, _m, _t, cid) = project_with_clip();
        let c = p.clip_mut(cid).unwrap();
        assert!(c.effects.is_empty(), "a fresh clip has no effects");
        c.add_effect(EffectKind::Grayscale);
        c.add_effect(EffectKind::Sepia);
        c.add_effect(EffectKind::Invert);
        let kinds = |c: &Clip| c.effects.iter().map(|e| e.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds(c),
            vec![EffectKind::Grayscale, EffectKind::Sepia, EffectKind::Invert]
        );
        // A fresh effect carries its kind's default intensity.
        assert!((c.effects[0].amount - EffectKind::Grayscale.default_amount()).abs() < 1e-9);
        // Reorder: move Invert (idx 2) up.
        c.move_effect_up(2);
        assert_eq!(
            kinds(c),
            vec![EffectKind::Grayscale, EffectKind::Invert, EffectKind::Sepia]
        );
        // Remove the head.
        c.remove_effect(0);
        assert_eq!(kinds(c), vec![EffectKind::Invert, EffectKind::Sepia]);
        // Out-of-range moves / removes are no-ops.
        c.move_effect_up(0);
        c.move_effect_down(1);
        c.remove_effect(9);
        assert_eq!(kinds(c), vec![EffectKind::Invert, EffectKind::Sepia]);
    }

    #[test]
    fn filters_default_is_identity() {
        assert!(Filters::default().is_identity());
        assert!(
            (Filters::gain(0.5) - 1.0).abs() < 1e-9,
            "0.5 slider = unity gain"
        );
        assert!(Filters::gain(0.0) < 1e-9);
        assert!((Filters::gain(1.0) - 2.0).abs() < 1e-9);
    }
}
