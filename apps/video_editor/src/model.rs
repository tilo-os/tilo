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

pub type MediaId = Uuid;
pub type TrackId = Uuid;
pub type ClipId = Uuid;

/// Smallest clip we allow a trim/split to produce, in seconds. Stops a
/// drag from collapsing a clip to a zero-width sliver that can't be
/// grabbed again.
pub const MIN_CLIP: f64 = 0.10;

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
    pub fn from_str(s: &str) -> Self {
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
    pub fn from_str(s: &str) -> Self {
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
    pub fn from_str(s: &str) -> Self {
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
        let mut video: Vec<&Track> =
            self.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
        video.sort_by(|a, b| b.index.cmp(&a.index));
        let mut audio: Vec<&Track> =
            self.tracks.iter().filter(|t| t.kind == TrackKind::Audio).collect();
        audio.sort_by(|a, b| a.index.cmp(&b.index));
        video.into_iter().chain(audio).collect()
    }

    /// Clips on `track`, sorted left-to-right by start.
    pub fn clips_on(&self, track: TrackId) -> Vec<&Clip> {
        let mut v: Vec<&Clip> =
            self.clips.iter().filter(|c| c.track == track).collect();
        v.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
        v
    }

    /// Total timeline length = the furthest clip end (min 1s so an empty
    /// project still has a ruler to look at).
    pub fn duration(&self) -> f64 {
        self.clips
            .iter()
            .map(|c| c.end())
            .fold(0.0_f64, f64::max)
            .max(1.0)
    }

    /// The video clip visible at timeline time `t`: the one on the
    /// highest video track (largest index) whose span covers `t`. This
    /// is what the preview canvas renders.
    pub fn top_video_clip_at(&self, t: f64) -> Option<&Clip> {
        let mut best: Option<(&Clip, u32)> = None;
        for c in &self.clips {
            let Some(track) = self.track(c.track) else { continue };
            if track.kind != TrackKind::Video {
                continue;
            }
            if !c.covers(t) {
                continue;
            }
            if best.map(|(_, idx)| track.index > idx).unwrap_or(true) {
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
        let mut right = clip.clone();
        right.id = Uuid::new_v4();
        right.start = at;
        right.in_point = split_src;
        // A head transition belongs to the new cut, not the right half.
        right.transition = Transition::None;
        // Shorten the left half to end at the split.
        if let Some(left) = self.clip_mut(id) {
            left.out_point = split_src;
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
        let order: Vec<&str> =
            p.display_tracks().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(order, vec!["V2", "V1", "A1", "A2"]);
    }
}
