//! Building and reading a cut by name, for a caller with no screen and no
//! files: an agent in a room.
//!
//! A person imports a file through a chooser, drags a source onto a lane and
//! reads the timeline off the screen. An agent has none of those, so the
//! project offers the same edits by what they name instead: room content
//! brought into the bin ([`effect`] answering `import-content`), a bin
//! source placed by its id or name (`add-clip`), a placed clip moved, set or
//! removed, a title added and restyled, and the timeline read back as JSON
//! (`read`, and `export-timeline-content` keeping it as content). The verbs
//! live on [`Project`] in [`crate::resource`] and run the editing math here,
//! so they replicate, undo and replay like a person's edits; [`answer`] tells
//! the caller what each did.
//!
//! A source imported from content stores its content address
//! (`tilo.content:<hash>`) as its path: what a render on the room's host
//! reads its bytes by, where a file path would mean nothing.

use std::io::Read as _;

use automerge::{Patch, PatchAction, Prop};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ui::framework::content::ContentStore;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::tilo_resource::{self, Doc, Id, JsonSchema, Refusal, Resource as _};
use uuid::Uuid;

use crate::actions::VideoEditorEffect;
use crate::model::{
    Clip, ClipId, MIN_CLIP, MediaItem, MediaKind, Project, TextAlign, Track, TrackId, TrackKind,
    Transition, key,
};
use crate::param::Param;

/// How much of a source the probe reads: its container header and index,
/// which a faststart file keeps at the front.
const PROBE_BYTES: u64 = 96 * 1024 * 1024;

/// Slack when comparing times, below anything the timeline can show.
const EPSILON: f64 = 1e-6;

/// The most clips a read answers with, so one answer stays readable; a
/// longer timeline is read a track at a time.
pub const MAX_READ_CLIPS: usize = 400;

/// What a refusal naming an unknown source tells the caller to do.
const IMPORT_HINT: &str = "bring room content into the bin with import-content, or make a title \
                           with add-title";

/// The verbs whose answers [`answer`] writes.
pub const ADD_CLIP: &str = "add-clip";
pub const MOVE_CLIP: &str = "move-clip";
pub const SET_CLIP: &str = "set-clip";
pub const REMOVE_CLIP: &str = "remove-clip";
pub const ADD_TITLE: &str = "add-title";
pub const SET_TITLE: &str = "set-title";

/// The id a source imported from the content `hash` gets: the same for the
/// same content, so importing it twice finds the first import rather than
/// filling the bin with copies.
#[must_use]
pub fn content_media_id(hash: &str) -> Uuid {
    let mut bytes = [0_u8; 16];
    for (slot, pair) in bytes.iter_mut().zip(hash.as_bytes().chunks(2)) {
        *slot = std::str::from_utf8(pair)
            .ok()
            .and_then(|digits| u8::from_str_radix(digits, 16).ok())
            .unwrap_or(0);
    }
    uuid::Builder::from_random_bytes(bytes).into_uuid()
}

/// The hash a content address names: `tilo.content:<hash>`, or the bare hash.
///
/// # Errors
/// Returns why `address` names no content.
pub fn content_hash_of(address: &str) -> Result<String, String> {
    let address = address.trim();
    let hash = tilo_actions::content_hash(address)
        .unwrap_or(address)
        .to_ascii_lowercase();
    if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(hash)
    } else {
        Err(format!(
            "`{address}` names no content: give `tilo.content:<hash>`"
        ))
    }
}

/// The content address of a source imported from content, `None` for a file.
fn content_of(media: &MediaItem) -> Option<&str> {
    tilo_actions::content_hash(&media.path).map(|_| media.path.as_str())
}

/// The kind and number a lane's name reads as: `V2` a second video track,
/// `a1` the first audio track; `None` for any other name.
fn lane_named(named: &str) -> Option<(TrackKind, u32)> {
    let mut chars = named.chars();
    let kind = match chars.next()?.to_ascii_uppercase() {
        'V' => TrackKind::Video,
        'A' => TrackKind::Audio,
        _ => return None,
    };
    let index = chars
        .as_str()
        .parse::<u32>()
        .ok()
        .filter(|n| (1..=99).contains(n))?;
    Some((kind, index))
}

/// `kind` as a refusal names a track of it.
fn a_track(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "a video track",
        TrackKind::Audio => "an audio track",
    }
}

/// The most candidates a refusal lists, so it stays one readable line.
const MOST_LISTED: usize = 12;

/// `text` as a name is compared: case aside, any run of spaces as one.
fn folded(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// A source's name with a start time written after it, as
/// `shot-b.mp4@3` or `shot-b.mp4 at 3 s`: the name and the time.
fn with_start(named: &str) -> Option<(&str, f64)> {
    let (name, at) = if let Some(split) = named.rsplit_once('@') {
        split
    } else {
        // ASCII lowercasing keeps every byte where it was.
        let split = named.to_ascii_lowercase().rfind(" at ")?;
        (named.get(..split)?, named.get(split.saturating_add(4)..)?)
    };
    let at = at.trim();
    let at: f64 = at.strip_suffix('s').unwrap_or(at).trim().parse().ok()?;
    let name = name.trim();
    (at.is_finite() && at >= 0.0 && !name.is_empty()).then_some((name, at))
}

/// `entries` joined for a refusal, at most [`MOST_LISTED`] of them.
fn listed(entries: Vec<String>) -> String {
    let more = entries.len().saturating_sub(MOST_LISTED);
    let mut shown: Vec<String> = entries.into_iter().take(MOST_LISTED).collect();
    if more > 0 {
        shown.push(format!("and {more} more"));
    }
    shown.join(", ")
}

impl Project {
    /// The title `named` names: its id, the id of a clip showing it, or its
    /// words (or their first line, its bin name), case and spacing aside,
    /// when exactly one title has them. An agent names a title by what it
    /// reads on it far more often than by the id `add-title` answered.
    pub(crate) fn title_named(&self, named: &str) -> Result<Uuid, Refusal> {
        let named = named.trim();
        if let Ok(id) = named.parse::<Uuid>() {
            if self.media(id).is_some() {
                return Ok(id);
            }
            if let Some(clip) = self.clip(id)
                && self.media(clip.media).is_some_and(|m| m.text.is_some())
            {
                return Ok(clip.media);
            }
        }
        let wanted = folded(named);
        let titles: Vec<&MediaItem> = self.media.iter().filter(|m| m.text.is_some()).collect();
        let reads = |media: &MediaItem| {
            folded(&media.name) == wanted
                || media
                    .text
                    .as_ref()
                    .is_some_and(|text| folded(&text.content) == wanted)
        };
        let matching: Vec<&MediaItem> = titles.iter().copied().filter(|m| reads(m)).collect();
        let entry = |media: &&MediaItem| {
            let words = media
                .text
                .as_ref()
                .map_or(&media.name, |text| &text.content);
            let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
            format!("“{words}” ({})", media.id)
        };
        match matching.as_slice() {
            [one] => Ok(one.id),
            [] if titles.is_empty() => Err(Refusal::new(format!(
                "no title reads `{named}`: the project has no titles; make one with add-title"
            ))),
            [] => Err(Refusal::new(format!(
                "no title reads `{named}`; the titles are {}",
                listed(titles.iter().map(entry).collect())
            ))),
            several => Err(Refusal::new(format!(
                "{} titles read `{named}`: {}; name one by its id",
                several.len(),
                listed(several.iter().map(entry).collect())
            ))),
        }
    }

    /// The clip `named` names: its id, or its source (by id, by name, or a
    /// title by its words; case and spacing aside) when that source is on
    /// the timeline once, or once starting at the time written after it
    /// (`shot-b.mp4@3`). A picture and the sound linked to it are one
    /// placement, named by the picture: every linked edit takes both.
    pub(crate) fn clip_named(&self, named: &str) -> Result<ClipId, Refusal> {
        let named = named.trim();
        if let Ok(id) = named.parse::<Uuid>()
            && self.clip(id).is_some()
        {
            return Ok(id);
        }
        let (source, at, mut placed) = match self.clips_of(named) {
            placed if placed.is_empty() => match with_start(named) {
                Some((source, at)) => (source, Some(at), self.clips_of(source)),
                None => (named, None, placed),
            },
            placed => (named, None, placed),
        };
        if placed.is_empty() {
            let all: Vec<&Clip> = self.clips.iter().collect();
            return Err(Refusal::new(if all.is_empty() {
                format!("`{named}` names no clip: the timeline is empty")
            } else {
                format!(
                    "`{named}` names no clip; the clips are {}",
                    self.clips_listed(all)
                )
            }));
        }
        let every = placed.clone();
        if let Some(at) = at {
            let slack = 0.5 / self.fps.max(1.0);
            placed.retain(|clip| (clip.start - at).abs() <= slack);
        }
        let leads: Vec<&Clip> = placed
            .iter()
            .copied()
            .filter(|clip| !self.follows_a_picture(clip, &placed))
            .collect();
        match (leads.as_slice(), at) {
            ([one], _) => Ok(one.id),
            ([], Some(at)) => Err(Refusal::new(format!(
                "no clip of `{source}` starts at {at} s; its clips are {}",
                self.clips_listed(every)
            ))),
            (several, _) => Err(Refusal::new(format!(
                "`{source}` is on the timeline {} times: {}; name one by its id, or as \
                 `{source}@<start>`",
                several.len(),
                self.clips_listed(several.to_vec())
            ))),
        }
    }

    /// The clips of the sources `named` names, by id, name or a title's
    /// words, in timeline order.
    fn clips_of(&self, named: &str) -> Vec<&Clip> {
        let wanted = folded(named);
        let sources: Vec<Uuid> = self
            .media
            .iter()
            .filter(|media| {
                media.id.to_string() == named
                    || folded(&media.name) == wanted
                    || media
                        .text
                        .as_ref()
                        .is_some_and(|text| folded(&text.content) == wanted)
            })
            .map(|media| media.id)
            .collect();
        let mut placed: Vec<&Clip> = self
            .clips
            .iter()
            .filter(|clip| sources.contains(&clip.media))
            .collect();
        placed.sort_by(|a, b| a.start.total_cmp(&b.start));
        placed
    }

    /// Whether `clip` is sound linked to a picture among `placed`.
    fn follows_a_picture(&self, clip: &Clip, placed: &[&Clip]) -> bool {
        self.clip_kind(clip.id) == TrackKind::Audio
            && self.link_group(clip.id).iter().any(|member| {
                *member != clip.id
                    && self.clip_kind(*member) == TrackKind::Video
                    && placed.iter().any(|other| other.id == *member)
            })
    }

    /// `clips` as a refusal lists them: what each shows, where, and its id.
    fn clips_listed(&self, clips: Vec<&Clip>) -> String {
        listed(
            clips
                .into_iter()
                .map(|clip| {
                    format!(
                        "{} at {} s on {} ({})",
                        self.media(clip.media).map_or("a clip", |m| m.name.as_str()),
                        secs(clip.start),
                        self.track(clip.track)
                            .map_or("no track", |t| t.name.as_str()),
                        clip.id
                    )
                })
                .collect(),
        )
    }

    /// The bin source `named`, by id or by name (case aside).
    pub(crate) fn source_named(&self, named: &str) -> Result<&MediaItem, Refusal> {
        let named = named.trim();
        if let Ok(id) = named.parse::<Uuid>()
            && let Some(media) = self.media(id)
        {
            return Ok(media);
        }
        let mut matching = self
            .media
            .iter()
            .filter(|media| media.name.eq_ignore_ascii_case(named));
        match (matching.next(), matching.next()) {
            (Some(media), None) => Ok(media),
            (Some(_), Some(_)) => Err(Refusal::new(format!(
                "more than one source is named `{named}`: name it by its id"
            ))),
            (None, _) => Err(Refusal::new(format!(
                "no source `{named}` is in the bin: {IMPORT_HINT}"
            ))),
        }
    }

    /// The track `named`, by id or name, which must carry `kind`; with no
    /// name, the lowest unlocked track of `kind` (V1: the cut, under the
    /// titles and overlays on the tracks above it). A name no track has
    /// that reads as a lane, V1 or A2, makes that track, as no name makes
    /// the first: a new project has none, and its first clip names V1.
    fn lane_for(&mut self, named: Option<&str>, kind: TrackKind) -> Result<TrackId, Refusal> {
        let Some(named) = named.map(str::trim).filter(|named| !named.is_empty()) else {
            let lowest = self
                .tracks
                .iter()
                .filter(|track| track.kind == kind && !track.locked)
                .min_by_key(|track| track.index)
                .map(|track| track.id);
            return Ok(lowest.unwrap_or_else(|| self.new_lane(kind)));
        };
        let found = named
            .parse::<Uuid>()
            .ok()
            .and_then(|id| self.track(id))
            .or_else(|| {
                self.tracks
                    .iter()
                    .find(|track| track.name.eq_ignore_ascii_case(named))
            });
        let track = match (found, lane_named(named)) {
            (Some(track), _) => track,
            (None, Some((lane, index))) if lane == kind => return Ok(self.lane_at(kind, index)),
            (None, Some((lane, _))) => {
                return Err(Refusal::new(format!(
                    "{named} would be {}; this goes on {}",
                    a_track(lane),
                    a_track(kind)
                )));
            }
            (None, None) => {
                let lanes: Vec<&str> = self.tracks.iter().map(|t| t.name.as_str()).collect();
                return Err(Refusal::new(format!(
                    "no track `{named}`; the tracks are {}. Name one like {} to make it",
                    if lanes.is_empty() {
                        "none yet".to_owned()
                    } else {
                        lanes.join(", ")
                    },
                    if kind == TrackKind::Video { "V1" } else { "A1" }
                )));
            }
        };
        if track.kind != kind {
            return Err(Refusal::new(format!(
                "{} is {}; this goes on {}",
                track.name,
                a_track(track.kind),
                a_track(kind)
            )));
        }
        if track.locked {
            return Err(Refusal::new(format!("{} is locked", track.name)));
        }
        Ok(track.id)
    }

    /// A new empty track of `kind` above (or below) the others: V2 over V1.
    pub(crate) fn new_lane(&mut self, kind: TrackKind) -> TrackId {
        let index = self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .map(|t| t.index)
            .max()
            .unwrap_or(0)
            + 1;
        self.lane_at(kind, index)
    }

    /// A new empty track of `kind` named for `index`: V3, A1.
    fn lane_at(&mut self, kind: TrackKind, index: u32) -> TrackId {
        let prefix = if kind == TrackKind::Video { "V" } else { "A" };
        let id = tilo_resource::fresh_uuid();
        self.tracks.push(Track {
            id,
            name: format!("{prefix}{index}"),
            kind,
            index,
            ..Track::default()
        });
        id
    }

    /// Refuse a span on `track` that a clip outside `moving` already covers:
    /// an agent does not see the lane, so a clip hidden under another is
    /// never what it meant.
    fn lane_free(
        &self,
        track: TrackId,
        start: f64,
        end: f64,
        moving: &[ClipId],
    ) -> Result<(), Refusal> {
        let taken = self.clips_on(track).into_iter().find(|clip| {
            !moving.contains(&clip.id) && clip.start < end - EPSILON && clip.end() > start + EPSILON
        });
        match taken {
            None => Ok(()),
            Some(clip) => {
                let lane = self.track(track).map_or("the track", |t| t.name.as_str());
                let free = self
                    .clips_on(track)
                    .iter()
                    .filter(|c| !moving.contains(&c.id))
                    .map(|c| c.end())
                    .fold(0.0, f64::max);
                Err(Refusal::new(format!(
                    "{lane} is taken from {:.2} s to {:.2} s by {}: place it at {free:.2} s or \
                     later, or on another track",
                    clip.start,
                    clip.end(),
                    self.media(clip.media).map_or("a clip", |m| m.name.as_str()),
                )))
            }
        }
    }

    /// Place a bin source on a track: `add-clip`.
    ///
    /// A still or a title has no length of its own, so asking for more of
    /// one than it lasts lengthens the source rather than refusing.
    pub(crate) fn agent_add_clip(&mut self, placing: &ClipPlacement) -> Result<ClipId, Refusal> {
        let ClipPlacement {
            media,
            track,
            at,
            source_in,
            source_out,
            fade_in,
            fade_out,
        } = placing.clone();
        let media = media.as_str();
        let track = track.as_deref();
        let source = self.source_named(media)?.clone();
        let kind = if source.kind == MediaKind::Audio {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        let lane = self.lane_for(track, kind)?;
        let source_in = finite_at_least_zero(source_in.unwrap_or(0.0), "in")?;
        let source_out = source_out.unwrap_or(source.duration);
        if !source_out.is_finite() || source_out < source_in + MIN_CLIP {
            return Err(Refusal::new(format!(
                "out is at least {MIN_CLIP} s after in ({source_in} s)"
            )));
        }
        let stretches = matches!(source.kind, MediaKind::Image | MediaKind::Text);
        if source_out > source.duration + EPSILON && !stretches {
            return Err(Refusal::new(format!(
                "{} lasts {:.2} s; out is at most that",
                source.name, source.duration
            )));
        }
        let start = match at {
            Some(at) => finite_at_least_zero(at, "at")?,
            None => self
                .clips_on(lane)
                .iter()
                .map(|c| c.end())
                .fold(0.0, f64::max),
        };
        let length = source_out - source_in;
        self.lane_free(lane, start, start + length, &[])?;
        check_fades(fade_in, fade_out, length)?;
        if source_out > source.duration
            && let Some(item) = self.media.find_mut(&key(source.id))
        {
            item.duration = source_out;
        }
        let id = self
            .add_clip(lane, source.id, start)
            .ok_or_else(|| Refusal::new(format!("no source `{media}`: {IMPORT_HINT}")))?;
        let clip = self.clip_mut(id).ok_or("the clip was not placed")?;
        clip.in_point = source_in;
        clip.out_point = source_out;
        clip.fade_in = fade_in.unwrap_or(0.0);
        clip.fade_out = fade_out.unwrap_or(0.0);
        Ok(id)
    }

    /// Move a clip, and the clips linked to it by the same amount, to a time
    /// and optionally another track of its kind: `move-clip`.
    pub(crate) fn agent_move_clip(
        &mut self,
        id: ClipId,
        at: f64,
        track: Option<&str>,
    ) -> Result<(), Refusal> {
        let at = finite_at_least_zero(at, "at")?;
        let clip = self
            .clip(id)
            .cloned()
            .ok_or("no such clip in this project")?;
        let lane = match track {
            Some(named) => self.lane_for(Some(named), self.clip_kind(id))?,
            None => clip.track,
        };
        let delta = at - clip.start;
        let group = self.link_group(id);
        for member in &group {
            let Some(other) = self.clip(*member) else {
                continue;
            };
            let (lane, start) = if *member == id {
                (lane, at)
            } else {
                (other.track, other.start + delta)
            };
            if start < -EPSILON {
                return Err(Refusal::new(
                    "a linked clip would start before the timeline does",
                ));
            }
            self.lane_free(
                lane,
                start.max(0.0),
                start.max(0.0) + other.duration(),
                &group,
            )?;
        }
        for member in group {
            if member == id {
                if !self.move_clip_to(id, lane, at) {
                    return Err(Refusal::new("the clip cannot go on that track"));
                }
            } else if let Some(other) = self.clip(member).map(|c| c.start) {
                self.move_clip(member, other + delta);
            }
        }
        Ok(())
    }

    /// Set a clip's fades, opacity, volume and incoming transition:
    /// `set-clip`. Each value left out stays.
    pub(crate) fn agent_set_clip(
        &mut self,
        id: ClipId,
        settings: &ClipSettings,
    ) -> Result<(), Refusal> {
        let ClipSettings {
            fade_in,
            fade_out,
            opacity,
            volume,
            transition,
            transition_duration,
        } = settings.clone();
        let transition = transition.as_deref();
        if [fade_in, fade_out, opacity, volume, transition_duration]
            .iter()
            .all(Option::is_none)
            && transition.is_none()
        {
            return Err(Refusal::new(
                "name something to set: fade_in, fade_out, opacity, volume or transition",
            ));
        }
        let clip = self
            .clip(id)
            .cloned()
            .ok_or("no such clip in this project")?;
        let fade_in = fade_in.or(Some(clip.fade_in));
        let fade_out = fade_out.or(Some(clip.fade_out));
        check_fades(fade_in, fade_out, clip.duration())?;
        let unit = |value: Option<f64>, most: f64, what: &str| match value {
            Some(v) if !(v.is_finite() && (0.0..=most).contains(&v)) => {
                Err(Refusal::new(format!("{what} is between 0 and {most}")))
            }
            _ => Ok(()),
        };
        unit(opacity, 1.0, "opacity")?;
        unit(volume, 2.0, "volume")?;
        for (value, param) in [(opacity, Param::Opacity), (volume, Param::Volume)] {
            if value.is_some() && clip.anim.is_animated(param) {
                return Err(Refusal::new(format!(
                    "the clip's {} is keyframed: set it at a time with set-param",
                    param.as_str()
                )));
            }
        }
        let transition = transition.map(transition_named).transpose()?;
        if let Some(length) = transition_duration
            && !(length.is_finite() && length > 0.0 && length <= clip.duration())
        {
            return Err(Refusal::new(
                "a transition lasts more than 0 s and at most the clip",
            ));
        }
        let clip = self.clip_mut(id).ok_or("no such clip in this project")?;
        clip.fade_in = fade_in.unwrap_or(0.0);
        clip.fade_out = fade_out.unwrap_or(0.0);
        if let Some(v) = opacity {
            clip.set_param_const(Param::Opacity, v);
        }
        if let Some(v) = volume {
            clip.set_param_const(Param::Volume, v);
        }
        if let Some(transition) = transition {
            clip.transition = transition;
        }
        if let Some(length) = transition_duration {
            clip.transition_duration = length;
        }
        Ok(())
    }

    /// Restyle a title, keeping what is left out: `set-title`. Its bin name
    /// follows the first line of new words.
    pub(crate) fn agent_set_title(
        &mut self,
        media: Uuid,
        style: &TitleStyle,
    ) -> Result<(), Refusal> {
        if style.is_empty() {
            return Err(Refusal::new(
                "name something to set: text, colour, size or align",
            ));
        }
        let item = self
            .media
            .find_mut(&key(media))
            .ok_or_else(|| Refusal::new(format!("no such source: {IMPORT_HINT}")))?;
        let Some(text) = item.text.as_mut() else {
            return Err(Refusal::new(format!(
                "{} is not a title; only a title's words are set",
                item.name
            )));
        };
        style.apply(text)?;
        if let Some(first) = text.content.lines().next() {
            item.name = first.to_owned();
        }
        Ok(())
    }
}

/// Where `add-clip` puts a bin source: its parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClipPlacement {
    /// The source's id or name in the bin
    pub media: String,
    /// The track's id or name, e.g. V1 or A2, made when the project has none of
    /// that name; the first of the source's kind when omitted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    /// Seconds on the timeline where it starts; after the track's last clip when omitted
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f64>,
    /// Seconds into the source where it begins; 0 when omitted
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub source_in: Option<f64>,
    /// Seconds into the source where it ends; the source's end when omitted
    #[serde(rename = "out", default, skip_serializing_if = "Option::is_none")]
    pub source_out: Option<f64>,
    /// Seconds of fade from black (and silence) at its head
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_in: Option<f64>,
    /// Seconds of fade to black (and silence) at its tail
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<f64>,
}

/// What `set-clip` changes on a clip: its parameters, each left out stays.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipSettings {
    /// Seconds of fade from black (and silence) at its head
    pub fade_in: Option<f64>,
    /// Seconds of fade to black (and silence) at its tail
    pub fade_out: Option<f64>,
    /// Opacity from 0 to 1
    pub opacity: Option<f64>,
    /// Volume from 0 to 2; 1 is as recorded
    pub volume: Option<f64>,
    /// Transition into it: none, dissolve, fade, dip or wipe
    pub transition: Option<String>,
    /// Seconds the transition lasts
    pub transition_duration: Option<f64>,
}

/// What a title's words look like, as an agent names it: each part left out
/// stays as it is (or as a new title's default).
#[derive(Default)]
pub(crate) struct TitleStyle {
    pub text: Option<String>,
    pub colour: Option<String>,
    pub size: Option<f64>,
    pub align: Option<String>,
}

impl TitleStyle {
    fn is_empty(&self) -> bool {
        self.text.is_none() && self.colour.is_none() && self.size.is_none() && self.align.is_none()
    }

    /// Set `text` to this style.
    ///
    /// # Errors
    /// Returns why a colour, size or alignment is not one a title takes.
    pub(crate) fn apply(&self, text: &mut crate::model::TextSource) -> Result<(), Refusal> {
        if let Some(words) = &self.text {
            let words = words.trim();
            if words.is_empty() {
                return Err(Refusal::new("a title has words"));
            }
            words.clone_into(&mut text.content);
        }
        if let Some(colour) = &self.colour {
            text.color = colour_named(colour)?;
        }
        if let Some(size) = self.size {
            if !(size.is_finite() && (0.02..=0.5).contains(&size)) {
                return Err(Refusal::new(
                    "a title's size is a fraction of the frame height from 0.02 to 0.5 \
                     (0.12 is the default)",
                ));
            }
            text.size = size;
        }
        if let Some(align) = &self.align {
            text.align = align_named(align)?;
        }
        Ok(())
    }
}

/// A time an edit can start at.
fn finite_at_least_zero(value: f64, what: &str) -> Result<f64, Refusal> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(Refusal::new(format!(
            "{what} is a time in seconds, 0 or later"
        )))
    }
}

/// Fades that fit a clip of `length` seconds.
fn check_fades(fade_in: Option<f64>, fade_out: Option<f64>, length: f64) -> Result<(), Refusal> {
    let (fade_in, fade_out) = (fade_in.unwrap_or(0.0), fade_out.unwrap_or(0.0));
    if !(fade_in.is_finite() && fade_out.is_finite() && fade_in >= 0.0 && fade_out >= 0.0) {
        return Err(Refusal::new("a fade lasts 0 s or more"));
    }
    if fade_in + fade_out > length + EPSILON {
        return Err(Refusal::new(format!(
            "the fades ({fade_in} s in, {fade_out} s out) are longer than the clip ({length:.2} s)"
        )));
    }
    Ok(())
}

/// The transition a caller names.
fn transition_named(name: &str) -> Result<Transition, Refusal> {
    let name = name.trim().to_ascii_lowercase();
    let found = match name.as_str() {
        "none" | "cut" => Some(Transition::None),
        "dissolve" | "cross-dissolve" | "crossfade" => Some(Transition::CrossDissolve),
        "fade" | "fade-in" => Some(Transition::FadeIn),
        "dip" | "dip-to-black" => Some(Transition::DipToBlack),
        "wipe" => Some(Transition::Wipe),
        _ => None,
    };
    found.ok_or_else(|| {
        Refusal::new(format!(
            "no transition `{name}`: none, dissolve, fade, dip or wipe"
        ))
    })
}

/// The alignment a caller names.
fn align_named(name: &str) -> Result<TextAlign, Refusal> {
    match name.trim().to_ascii_lowercase().as_str() {
        "left" => Ok(TextAlign::Left),
        "center" | "centre" | "middle" => Ok(TextAlign::Center),
        "right" => Ok(TextAlign::Right),
        other => Err(Refusal::new(format!(
            "no alignment `{other}`: left, center or right"
        ))),
    }
}

/// The colour a caller names: `#rgb`, `#rrggbb`, `#rrggbbaa` or a plain
/// colour word, as straight sRGB in 0..1 with alpha.
fn colour_named(name: &str) -> Result<[f64; 4], Refusal> {
    let named = name.trim().to_ascii_lowercase();
    let hex = match named.as_str() {
        "white" => "ffffff",
        "black" => "000000",
        "red" => "e53935",
        "green" => "43a047",
        "blue" => "1e88e5",
        "yellow" => "fdd835",
        "orange" => "fb8c00",
        "purple" => "8e24aa",
        "pink" => "d81b60",
        "grey" | "gray" => "9e9e9e",
        other => other.strip_prefix('#').unwrap_or(other),
    };
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect::<Option<_>>()
        .ok_or_else(|| bad_colour(name))?;
    let channel = |pair: &[u8]| match pair {
        [high, low] => f64::from(high * 16 + low) / 255.0,
        [single] => f64::from(single * 17) / 255.0,
        _ => 1.0,
    };
    let channels: Vec<f64> = match digits.len() {
        3 | 4 => digits.chunks(1).map(channel).collect(),
        6 | 8 => digits.chunks(2).map(channel).collect(),
        _ => return Err(bad_colour(name)),
    };
    match channels.as_slice() {
        [r, g, b] => Ok([*r, *g, *b, 1.0]),
        [r, g, b, a] => Ok([*r, *g, *b, *a]),
        _ => Err(bad_colour(name)),
    }
}

fn bad_colour(name: &str) -> Refusal {
    Refusal::new(format!(
        "no colour `{name}`: give #rrggbb or a colour word such as white or yellow"
    ))
}

/// The colour as `#rrggbb` (with alpha when not opaque).
pub(crate) fn colour_hex(colour: [f64; 4]) -> String {
    let byte = |v: f64| {
        let scaled = (v.clamp(0.0, 1.0) * 255.0).round();
        // In 0..=255 after the clamp.
        format!("{:02x}", scaled as u8)
    };
    let [r, g, b, a] = colour;
    if a >= 1.0 {
        format!("#{}{}{}", byte(r), byte(g), byte(b))
    } else {
        format!("#{}{}{}{}", byte(r), byte(g), byte(b), byte(a))
    }
}

/// A rounded time, so answers read as the editor shows them.
fn secs(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// One bin source as a caller reads it.
#[must_use]
pub fn media_json(media: &MediaItem) -> Value {
    let mut out = Map::new();
    out.insert("id".into(), json!(media.id.to_string()));
    out.insert("name".into(), json!(media.name));
    out.insert("kind".into(), json!(media.kind.as_str()));
    out.insert("duration".into(), json!(secs(media.duration)));
    if media.kind.is_visual() && media.kind != MediaKind::Text {
        out.insert("width".into(), json!(media.width));
        out.insert("height".into(), json!(media.height));
    }
    if media.kind == MediaKind::Video {
        out.insert("fps".into(), json!(media.fps));
        out.insert("has_audio".into(), json!(media.has_audio));
    }
    if let Some(content) = content_of(media) {
        out.insert("content".into(), json!(content));
    }
    if let Some(text) = &media.text {
        out.insert("text".into(), json!(text.content));
        out.insert("colour".into(), json!(colour_hex(text.color)));
        out.insert("size".into(), json!(text.size));
        out.insert("align".into(), json!(text.align.as_str()));
    }
    Value::Object(out)
}

/// One placed clip as a caller reads it: what it shows, where, and what
/// differs from a plain cut.
#[must_use]
pub fn clip_json(project: &Project, clip: &Clip) -> Value {
    let media = project.media(clip.media);
    let mut out = Map::new();
    out.insert("id".into(), json!(clip.id.to_string()));
    out.insert("media".into(), json!(clip.media.to_string()));
    out.insert("media_name".into(), json!(media.map(|m| m.name.as_str())));
    out.insert(
        "track".into(),
        json!(project.track(clip.track).map(|t| t.name.as_str())),
    );
    out.insert("start".into(), json!(secs(clip.start)));
    out.insert("end".into(), json!(secs(clip.end())));
    out.insert("in".into(), json!(secs(clip.in_point)));
    out.insert("out".into(), json!(secs(clip.out_point)));
    if let Some(text) = media.and_then(|m| m.text.as_ref()) {
        out.insert("title".into(), json!(text.content));
    }
    let extras = [
        ("fade_in", clip.fade_in, 0.0),
        ("fade_out", clip.fade_out, 0.0),
        ("opacity", clip.opacity, 1.0),
        ("volume", clip.volume, 1.0),
    ];
    for (name, value, plain) in extras {
        if (value - plain).abs() > EPSILON {
            out.insert(name.into(), json!(secs(value)));
        }
    }
    if clip.transition != Transition::None {
        out.insert("transition".into(), json!(clip.transition.as_str()));
        out.insert(
            "transition_duration".into(),
            json!(secs(clip.transition_len())),
        );
    }
    if !clip.speed.is_normal() {
        out.insert("speed".into(), json!(clip.speed.signed()));
    }
    if !clip.effects.is_empty() {
        let effects: Vec<Value> = clip
            .effects
            .iter()
            .map(|e| json!({ "id": e.id.to_string(), "kind": e.kind.as_str(), "amount": e.amount }))
            .collect();
        out.insert("effects".into(), Value::Array(effects));
    }
    if project.is_linked(clip.id) {
        let partners: Vec<Value> = project
            .link_partners(clip.id)
            .iter()
            .map(|id| Value::String(id.to_string()))
            .collect();
        out.insert("linked".into(), Value::Array(partners));
    }
    Value::Object(out)
}

/// One track and its clips, left to right, at most `budget` of them (what
/// is left of the budget after them is written back).
fn track_json(project: &Project, track: &Track, budget: &mut usize) -> Value {
    let all = project.clips_on(track.id);
    let clips: Vec<Value> = all
        .iter()
        .take(*budget)
        .map(|clip| clip_json(project, clip))
        .collect();
    *budget = budget.saturating_sub(clips.len());
    let mut out = Map::new();
    out.insert("id".into(), json!(track.id.to_string()));
    out.insert("name".into(), json!(track.name));
    out.insert("kind".into(), json!(track.kind.as_str()));
    if all.len() > clips.len() {
        out.insert("more_clips".into(), json!(all.len() - clips.len()));
    }
    out.insert("clips".into(), Value::Array(clips));
    for (name, on) in [
        ("muted", track.muted),
        ("locked", track.locked),
        ("solo", track.solo),
    ] {
        if on {
            out.insert(name.into(), json!(true));
        }
    }
    Value::Object(out)
}

/// The whole timeline as a caller reads it: the project's format, each
/// track (top picture lane first) with its clips, the bin and the markers.
#[must_use]
pub fn timeline_json(project: &Project) -> Value {
    let mut budget = MAX_READ_CLIPS;
    let tracks: Vec<Value> = project
        .display_tracks()
        .into_iter()
        .map(|track| track_json(project, track, &mut budget))
        .collect();
    json!({
        "project": {
            "name": project.name,
            "width": project.width,
            "height": project.height,
            "fps": project.fps,
            "duration": secs(project.duration()),
        },
        "tracks": tracks,
        "bin": project.media.iter().map(media_json).collect::<Vec<_>>(),
        "markers": project
            .markers
            .iter()
            .map(|m| json!({ "id": m.id.to_string(), "at": secs(m.t), "name": m.name }))
            .collect::<Vec<_>>(),
    })
}

/// `read`: the timeline, or one clip, source or track (by id, or a track or
/// source by name) the caller names.
///
/// # Errors
/// Returns that `item` names nothing in the project.
pub fn read_timeline(project: &Project, item: Option<&Id>) -> Result<Value, String> {
    let Some(item) = item else {
        return Ok(timeline_json(project));
    };
    let named = item.as_str().trim();
    if let Ok(id) = named.parse::<Uuid>() {
        if let Some(clip) = project.clip(id) {
            return Ok(clip_json(project, clip));
        }
        if let Some(media) = project.media(id) {
            return Ok(media_json(media));
        }
    }
    if let Some(track) = project
        .tracks
        .iter()
        .find(|track| track.id.to_string() == named || track.name.eq_ignore_ascii_case(named))
    {
        let mut every = usize::MAX;
        return Ok(track_json(project, track, &mut every));
    }
    project
        .source_named(named)
        .map(media_json)
        .map_err(|_| format!("`{named}` names no clip, track or source of this project"))
}

/// The ids under the root map `map` (`clips`, `media`) that `changes`
/// wrote or removed, in the order first touched.
fn touched(changes: &[Patch], map: &str) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for patch in changes {
        let id = match (patch.path.as_slice(), &patch.action) {
            (
                [(_, Prop::Map(root))],
                PatchAction::PutMap { key, .. } | PatchAction::DeleteMap { key },
            ) if root == map => key.parse::<Uuid>().ok(),
            ([(_, Prop::Map(root)), (_, Prop::Map(key)), ..], _) if root == map => {
                key.parse::<Uuid>().ok()
            }
            _ => None,
        };
        if let Some(id) = id
            && !ids.contains(&id)
        {
            ids.push(id);
        }
    }
    ids
}

/// What a timeline verb did, once it applied to `doc` as `changes`: the
/// clip or source it made or changed, and what a removal took and moved.
#[must_use]
pub fn answer(doc: &Doc, action: &str, changes: &[Patch]) -> Option<Value> {
    if ![
        ADD_CLIP,
        MOVE_CLIP,
        SET_CLIP,
        REMOVE_CLIP,
        ADD_TITLE,
        SET_TITLE,
    ]
    .contains(&action)
    {
        return None;
    }
    let project = Project::read(doc).ok()?;
    let clips = touched(changes, "clips");
    match action {
        ADD_CLIP => {
            let clip = clips.iter().find_map(|id| project.clip(*id))?;
            Some(json!({ "clip": clip.id.to_string(), "placed": clip_json(&project, clip) }))
        }
        MOVE_CLIP | SET_CLIP => {
            let edited: Vec<Value> = clips
                .iter()
                .filter_map(|id| project.clip(*id))
                .map(|clip| clip_json(&project, clip))
                .collect();
            Some(json!({ "clips": edited }))
        }
        REMOVE_CLIP => {
            let (gone, moved): (Vec<Uuid>, Vec<Uuid>) =
                clips.iter().partition(|id| project.clip(**id).is_none());
            Some(json!({
                "removed": gone.iter().map(ToString::to_string).collect::<Vec<_>>(),
                "moved": moved
                    .iter()
                    .filter_map(|id| project.clip(*id))
                    .map(|clip| clip_json(&project, clip))
                    .collect::<Vec<_>>(),
            }))
        }
        _ => {
            let media = touched(changes, "media")
                .into_iter()
                .find_map(|id| project.media(id))?;
            Some(json!({ "media": media.id.to_string(), "source": media_json(media) }))
        }
    }
}

/// Answer the effects of this module ([`VideoEditorEffect::ImportContent`],
/// [`VideoEditorEffect::ExportTimelineContent`]); `None` for any other,
/// which the editor answers itself. `content` is the store the window was
/// lent, else the one the running action has.
pub fn effect<H: 'static>(
    effect: &VideoEditorEffect,
    content: Option<&ContentStore>,
    cx: &mut EffectCx<Project, H>,
) -> Option<EffectAnswer> {
    let store = || {
        content
            .cloned()
            .or_else(tilo_resource::content::current)
            .ok_or_else(|| "this window keeps no content".to_owned())
    };
    match effect {
        VideoEditorEffect::ImportContent {
            content,
            name,
            duration,
        } => Some(store().and_then(|store| {
            let project = cx.live().get();
            let item = imported(&project, &store, content, name.as_deref(), *duration)?;
            if project.media(item.id).is_none() {
                cx.submit(Project::paths().media.insert(&item));
            }
            Ok(Effected::value(json!({
                "media": item.id.to_string(),
                "source": media_json(&item),
                "how": "place it on the timeline with add-clip {media}",
            })))
        })),
        VideoEditorEffect::ExportTimelineContent => Some(store().and_then(|store| {
            let timeline = timeline_json(&cx.live().get());
            let bytes = serde_json::to_vec_pretty(&timeline).map_err(|e| e.to_string())?;
            let hash = store.put(&bytes).map_err(|e| e.to_string())?;
            let address = tilo_actions::content_resource(&hash)
                .ok_or_else(|| format!("{hash:?} is not a content hash"))?;
            Ok(Effected::Done(tilo_actions::ActionOutput {
                value: Some(json!({
                    "content": address,
                    "media_type": "application/json",
                    "bytes": bytes.len(),
                })),
                resources: vec![address],
            }))
        })),
        _ => None,
    }
}

/// The bin source the content at `address` makes in `project`: the one
/// already imported from it, else a new one from its container's metadata.
///
/// # Errors
/// Returns why the content is not here or is not media the editor reads.
pub fn imported(
    project: &Project,
    store: &ContentStore,
    address: &str,
    name: Option<&str>,
    duration: Option<f64>,
) -> Result<MediaItem, String> {
    let hash = content_hash_of(address)?;
    let id = content_media_id(&hash);
    if let Some(existing) = project.media(id) {
        return Ok(existing.clone());
    }
    let mut head = Vec::new();
    store
        .open(&hash)
        .ok_or_else(|| format!("content `{hash}` is not held in this room"))?
        .take(PROBE_BYTES)
        .read_to_end(&mut head)
        .map_err(|error| format!("content `{hash}` could not be read: {error}"))?;
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    let hint = std::path::Path::new(name.unwrap_or("content"));
    let probed = crate::probe::probe_bytes(&head, hint)
        .map_err(|error| format!("content `{hash}` {error}"))?;
    let duration = match (probed.duration, duration) {
        (known, _) if known > 0.0 => known,
        (_, Some(given)) if given.is_finite() && given > 0.0 => given,
        _ => {
            return Err(format!(
                "content `{hash}`'s container does not say how long it lasts: give duration"
            ));
        }
    };
    let short: String = hash.chars().take(8).collect();
    let name = name.map_or_else(
        || format!("{}-{short}", probed.kind.label().to_ascii_lowercase()),
        str::to_owned,
    );
    Ok(MediaItem {
        id,
        name,
        kind: probed.kind,
        duration,
        width: probed.width,
        height: probed.height,
        // 0.0 is what the container did not say, carried as the editor's own
        // import carries it.
        fps: probed.fps,
        palette: u8::try_from(project.media.len() % 7).unwrap_or(0),
        path: tilo_actions::content_resource(&hash).unwrap_or_default(),
        text: None,
        has_audio: probed.has_audio,
    })
}

#[cfg(test)]
mod tests;
