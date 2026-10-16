//! Building a cut by name, for a caller with no screen and no files: a
//! source the room holds brought into the bin (`import-content`), a bin
//! source placed on a track (`add-clip`) and a placed clip moved
//! (`move-clip`). Each builds the editor's own action — the media write a
//! file import makes, the clip write a drop from the bin makes, the
//! optimistic patch a drag makes — so it replicates, undoes and replays like
//! the edit a person makes with the pointer, and each answers what it did
//! with the timeline it left ([`answer`]).

use std::path::Path;

use automerge::{Patch, PatchAction, Prop};
use tilo_actions::{ActionArgs, ActionParam, ParamType};
use ui::tilo_resource::{ActionDecl, Doc};
use uuid::Uuid;

use crate::actions::{
    VideoEditorAction, patch_clips_fields_action, upsert_media_action,
    upsert_tracks_and_clips_action,
};
use crate::doc::VideoDoc;
use crate::model::{Clip, MIN_CLIP, MediaItem, MediaKind, Project, Track, TrackId, TrackKind};
use crate::resource::VideoProject;

/// The id of the declaration that brings room content into the bin.
pub const IMPORT_CONTENT: &str = "import-content";
/// The id of the declaration that places a bin source on a track.
pub const ADD_CLIP: &str = "add-clip";
/// The id of the declaration that moves a placed clip.
pub const MOVE_CLIP: &str = "move-clip";

/// How much of a source the probe reads: its container header and index,
/// which a faststart file keeps at the front. A longer file whose index sits
/// at its end is imported with the duration its caller gives.
const PROBE_BYTES: u64 = 96 * 1024 * 1024;

/// Slack when comparing times, below anything the timeline can show.
const EPSILON: f64 = 1e-6;

/// The declarations bound to `project`: what it holds names the sources,
/// tracks and clips their arguments refer to. With them, [`crate::revision`]'s
/// titles and clip edits, so a caller reaches every edit of a cut on the
/// project itself rather than on a child it has to list first.
pub(crate) fn declarations(project: &Project) -> Vec<ActionDecl<VideoProject>> {
    let mut declared = vec![
        import_content(project),
        add_clip(project),
        move_clip(project),
    ];
    declared.extend(crate::revision::declarations(project));
    declared
}

/// What `action`, one of these declarations or [`crate::revision`]'s, did
/// once it applied to `doc` as `changes`: the source or clips it wrote and
/// the timeline it left.
pub(crate) fn answer(doc: &Doc, action: &str, changes: &[Patch]) -> Option<serde_json::Value> {
    let project = || VideoDoc::load_project(doc);
    match action {
        IMPORT_CONTENT => {
            let project = project();
            let media = touched(changes, "media")
                .into_iter()
                .find_map(|id| project.media(id))?;
            Some(serde_json::json!({
                "media": crate::timeline::media_json(&project, media),
            }))
        }
        ADD_CLIP => {
            let project = project();
            let clip = touched(changes, "clips")
                .into_iter()
                .find_map(|id| project.clip(id))?;
            let track = project.track(clip.track).map(|t| t.name.clone());
            Some(serde_json::json!({
                "clip": clip.id.to_string(),
                "track": track,
                "start": clip.start,
                "end": clip.end(),
                "timeline": crate::timeline::timeline_json(&project),
            }))
        }
        MOVE_CLIP => {
            let project = project();
            let moved: Vec<serde_json::Value> = touched(changes, "clips")
                .into_iter()
                .filter_map(|id| project.clip(id))
                .map(|clip| crate::timeline::clip_json(&project, clip))
                .collect();
            Some(serde_json::json!({
                "moved": moved,
                "timeline": crate::timeline::timeline_json(&project),
            }))
        }
        _ => crate::revision::answer(doc, action, changes),
    }
}

/// The ids of the entries of the root map `map` (`clips`, `media`) that
/// `changes` wrote, in the order first written.
pub(crate) fn touched(changes: &[Patch], map: &str) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for patch in changes {
        let id = match (patch.path.as_slice(), &patch.action) {
            ([(_, Prop::Map(root))], PatchAction::PutMap { key, .. }) if root == map => {
                key.parse::<Uuid>().ok()
            }
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

/// The hash a `content` argument names: `tilo.content:<hash>`, or the bare
/// hash.
fn content_hash_of(address: &str) -> Result<String, String> {
    let address = address.trim();
    let hash = crate::media::content_hash(address)
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

/// The front of the content at `hash` the probe reads, from the store the
/// running window was lent.
fn content_prefix(hash: &str) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let store = ui::tilo_resource::content::current()
        .ok_or("this window keeps no content to import media from")?;
    let reader = store
        .open(hash)
        .ok_or_else(|| format!("content `{hash}` is not available on this machine"))?;
    let mut bytes = Vec::new();
    reader
        .take(PROBE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("content `{hash}` could not be read: {error}"))?;
    Ok(bytes)
}

/// A duration argument: positive seconds, or absent.
fn duration_arg(args: &ActionArgs) -> Result<Option<f64>, String> {
    match args.number("duration") {
        None => Ok(None),
        Some(seconds) if seconds.is_finite() && seconds > 0.0 => Ok(Some(seconds)),
        Some(_) => Err("`duration` is the source's length in seconds, above 0".into()),
    }
}

/// The bin item for room content `hash` named `name`, described from the
/// container at the front of its `bytes`: its kind, size and frame rate, and
/// its duration, else `duration` (always for a still, which has none of its
/// own).
fn media_from_content(
    hash: &str,
    name: &str,
    bytes: &[u8],
    duration: Option<f64>,
    palette: u8,
) -> Result<MediaItem, String> {
    let probed = crate::probe::probe_bytes(bytes, Path::new(name))
        .map_err(|error| format!("content `{hash}` {error}"))?;
    let length = match (probed.kind, duration) {
        (MediaKind::Image, Some(seconds)) => seconds,
        (_, Some(seconds)) if probed.duration <= 0.0 => seconds,
        _ => probed.duration,
    };
    if length <= 0.0 {
        return Err(format!(
            "content `{hash}` is {} whose container gives no duration: give `duration` in \
             seconds",
            probed.kind.label().to_lowercase()
        ));
    }
    Ok(MediaItem {
        id: Uuid::new_v4(),
        name: name.to_owned(),
        kind: probed.kind,
        duration: length,
        width: probed.width,
        height: probed.height,
        fps: probed.fps,
        palette,
        path: crate::media::content_address(hash),
        hydration: crate::media::Hydration::Projected,
        text: None,
        has_audio: probed.has_audio,
    })
}

/// `import-content`: a video, sound or picture the room holds, into the bin.
fn import_content(project: &Project) -> ActionDecl<VideoProject> {
    let palette = (project.media.len() % 7) as u8;
    ActionDecl::new(
        IMPORT_CONTENT,
        "Import media from content",
        vec![
            ActionParam::required(
                "content",
                ParamType::Text,
                "The media's content address, `tilo.content:<hash>` (an attachment, a shared \
                 file, an export): an MP4/MOV/M4A, WebM/MKV, MP3, WAV, PNG or JPEG",
            ),
            ActionParam::optional(
                "name",
                ParamType::Text,
                "What the source is called in the bin, e.g. the file's name; its extension \
                 helps recognise a container the bytes alone do not name",
            ),
            ActionParam::optional(
                "duration",
                ParamType::Number,
                "Seconds the source lasts, used only when its container gives no duration; \
                 for a picture, how long it shows when placed (default 5)",
            ),
        ],
        move |args, _now| {
            let hash = content_hash_of(args.text("content").unwrap_or_default())?;
            let name = args
                .text("name")
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map_or_else(
                    || format!("Media {}", hash.chars().take(8).collect::<String>()),
                    str::to_owned,
                );
            let duration = duration_arg(args)?;
            let bytes = content_prefix(&hash)?;
            let item = media_from_content(&hash, &name, &bytes, duration, palette)?;
            Ok(upsert_media_action(&item))
        },
    )
    .describe(
        "Add a video, sound or picture the room holds to the project's media bin, by its \
         content address, with its duration, frame size and frame rate read from its \
         container. Nothing is placed on the timeline: place it with add-clip. Answers the \
         new source's id.",
    )
    .answering()
}

/// What to do about a source the bin lacks: a model naming a file the room
/// holds (an attachment, a shared file) has to bring it into the bin first,
/// and only the content address does that.
fn import_hint() -> String {
    format!(
        "A file the room holds goes into the bin with {IMPORT_CONTENT} first: pass its \
         content address (`tilo.content:<hash>`) as `content` and the name to place it by \
         as `name`"
    )
}

/// The bin source an argument names: its id (`video.media:<id>` too), else
/// its name exactly, else its name ignoring case.
pub(crate) fn media_named<'a>(project: &'a Project, named: &str) -> Result<&'a MediaItem, String> {
    let named = named.trim();
    if named.is_empty() {
        return Err("name the source to place: its id or its name in the bin".into());
    }
    if named.starts_with("tilo.content:") {
        return Err(format!(
            "`{named}` is a content address, not a bin source: {IMPORT_CONTENT} it first \
             (`content`: `{named}`), then place it by the name or id that answers"
        ));
    }
    if let Ok(id) = named.trim_start_matches("video.media:").parse::<Uuid>() {
        return project
            .media(id)
            .ok_or_else(|| format!("the bin holds no source {id}; {}", import_hint()));
    }
    let exact: Vec<&MediaItem> = project.media.iter().filter(|m| m.name == named).collect();
    let found = if exact.is_empty() {
        project
            .media
            .iter()
            .filter(|m| m.name.eq_ignore_ascii_case(named))
            .collect()
    } else {
        exact
    };
    match found.as_slice() {
        [one] => Ok(one),
        [] => {
            let held: Vec<&str> = project.media.iter().map(|m| m.name.as_str()).collect();
            Err(format!(
                "the bin holds no source named `{named}`; it holds {held:?}. {}",
                import_hint()
            ))
        }
        many => Err(format!(
            "{} sources are named `{named}`: name one by its id ({})",
            many.len(),
            many.iter()
                .map(|m| m.id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// A track an argument names: its id (`video.track:<id>` too), else its name
/// ignoring case. A name like `V2` or `A1` that no track has yet is the next
/// track of that kind, made with the clip.
enum TrackChoice {
    Existing(TrackId),
    New(Track),
}

fn track_named(project: &Project, named: &str) -> Result<TrackChoice, String> {
    let named = named.trim();
    if let Ok(id) = named.trim_start_matches("video.track:").parse::<Uuid>() {
        return project
            .track(id)
            .map(|t| TrackChoice::Existing(t.id))
            .ok_or_else(|| format!("the project has no track {id}"));
    }
    if let Some(track) = project
        .tracks
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(named))
    {
        return Ok(TrackChoice::Existing(track.id));
    }
    let mut chars = named.chars();
    let kind = match chars.next().map(|c| c.to_ascii_uppercase()) {
        Some('V') => Some(TrackKind::Video),
        Some('A') => Some(TrackKind::Audio),
        _ => None,
    };
    let index = chars.as_str().parse::<u32>().ok().filter(|n| *n > 0);
    if let (Some(kind), Some(index)) = (kind, index) {
        return Ok(TrackChoice::New(new_track(kind, index)));
    }
    let lanes: Vec<&str> = project
        .display_tracks()
        .into_iter()
        .map(|t| t.name.as_str())
        .collect();
    Err(format!(
        "the project has no track `{named}`; it has {lanes:?} (a name like V2 or A1 that no \
         track has yet makes that track)"
    ))
}

/// A new track of `kind`, `V<index>` or `A<index>`.
fn new_track(kind: TrackKind, index: u32) -> Track {
    Track {
        id: Uuid::new_v4(),
        name: format!(
            "{}{index}",
            if kind == TrackKind::Video { "V" } else { "A" }
        ),
        kind,
        index,
        ..Default::default()
    }
}

/// The lane a source of `kind` goes on.
fn lane_for(kind: MediaKind) -> TrackKind {
    if kind.is_visual() {
        TrackKind::Video
    } else {
        TrackKind::Audio
    }
}

/// A time argument `name`: seconds at or after the start, or absent.
fn time_arg(args: &ActionArgs, name: &str) -> Result<Option<f64>, String> {
    match args.number(name) {
        None => Ok(None),
        Some(seconds) if seconds.is_finite() && seconds >= 0.0 => Ok(Some(seconds)),
        Some(_) => Err(format!("`{name}` is seconds at or after 0")),
    }
}

/// The two fade parameters `add-clip`, `move-clip` and `set-clip` share.
pub(crate) fn fade_params() -> [ActionParam; 2] {
    [
        ActionParam::optional(
            "fade_in",
            ParamType::Number,
            "Seconds the clip fades up from black (and its sound from silence) at its head; \
             0 removes it; at most the clip's length",
        ),
        ActionParam::optional(
            "fade_out",
            ParamType::Number,
            "Seconds the clip fades to black (and its sound to silence) at its tail; 0 removes \
             it; at most the clip's length",
        ),
    ]
}

/// A fade argument `name` (`fade_in`, `fade_out`) for a clip lasting
/// `length` seconds: absent, or seconds from 0 to the clip's length. Refused
/// rather than clamped past the end, so a caller learns the clip is shorter
/// than it thought instead of getting a fade it did not ask for.
fn fade_arg(args: &ActionArgs, name: &str, length: f64) -> Result<Option<f64>, String> {
    match time_arg(args, name)? {
        Some(seconds) if seconds > length + EPSILON => Err(format!(
            "`{name}` is at most the clip's length, {length:.3}s"
        )),
        Some(seconds) => Ok(Some(seconds.min(length))),
        None => Ok(None),
    }
}

/// Set the fades `args` give on `clip`, checked against its length.
pub(crate) fn apply_fades(clip: &mut Clip, args: &ActionArgs) -> Result<(), String> {
    let length = clip.duration();
    if let Some(seconds) = fade_arg(args, "fade_in", length)? {
        clip.fade_in = seconds;
    }
    if let Some(seconds) = fade_arg(args, "fade_out", length)? {
        clip.fade_out = seconds;
    }
    Ok(())
}

/// Refuse a clip at `start..end` on `track` that would cover part of another
/// clip there (other than those in `moving`): which one shows would be
/// left to chance.
fn refuse_overlap(
    project: &Project,
    track: TrackId,
    (start, end): (f64, f64),
    moving: &[Uuid],
) -> Result<(), String> {
    let lane = project
        .track(track)
        .map_or("the track", |t| t.name.as_str());
    match project
        .clips_on(track)
        .into_iter()
        .filter(|c| !moving.contains(&c.id))
        .find(|c| start < c.end() - EPSILON && c.start < end - EPSILON)
    {
        Some(other) => {
            let name = project
                .media(other.media)
                .map_or("a clip", |m| m.name.as_str());
            Err(format!(
                "{lane} already has {name} ({}) from {:.3}s to {:.3}s there; place it at {:.3}s \
                 or later, or on another track",
                other.id,
                other.start,
                other.end(),
                other.end()
            ))
        }
        None => Ok(()),
    }
}

/// The track `add-clip` places a source of lane `lane` on, and the tracks
/// the placement makes: the named one, else the first unlocked one of the
/// lane, else a new one.
fn placement_track(
    project: &Project,
    args: &ActionArgs,
    lane: TrackKind,
) -> Result<(TrackId, Vec<Track>), String> {
    let named = args.text("track").map(str::trim).filter(|t| !t.is_empty());
    let choice = match named {
        Some(named) => track_named(project, named)?,
        None => project.placement_track(lane, None).map_or_else(
            || {
                let index = project
                    .tracks
                    .iter()
                    .filter(|t| t.kind == lane)
                    .map(|t| t.index)
                    .max()
                    .unwrap_or(0)
                    + 1;
                TrackChoice::New(new_track(lane, index))
            },
            TrackChoice::Existing,
        ),
    };
    let (track, made) = match choice {
        TrackChoice::Existing(id) => (project.track(id).cloned(), Vec::new()),
        TrackChoice::New(track) => (Some(track.clone()), vec![track]),
    };
    let track = track.ok_or("the track is gone")?;
    let kind_words = |kind: TrackKind| {
        if kind == TrackKind::Video {
            "a video"
        } else {
            "an audio"
        }
    };
    if track.kind != lane {
        return Err(format!(
            "that source is {} and {} is {} track: place it on {} track",
            if lane == TrackKind::Video {
                "a picture"
            } else {
                "sound"
            },
            track.name,
            kind_words(track.kind),
            kind_words(lane),
        ));
    }
    if track.locked {
        return Err(format!("{} is locked", track.name));
    }
    Ok((track.id, made))
}

/// The source window `add-clip` reads from `media`: `in` and `out`, within
/// the source unless it is a still.
fn source_window(media: &MediaItem, args: &ActionArgs) -> Result<(f64, f64), String> {
    let in_point = time_arg(args, "in")?.unwrap_or(0.0);
    let still = matches!(media.kind, MediaKind::Image | MediaKind::Text);
    let out_point = match time_arg(args, "out")? {
        Some(out) => out,
        None if media.duration > 0.0 => media.duration,
        None => {
            return Err(format!(
                "{}'s length is unknown: give `out`, seconds into it",
                media.name
            ));
        }
    };
    if !still && media.duration > 0.0 && out_point > media.duration + EPSILON {
        return Err(format!(
            "{} lasts {:.3}s: `out` is at most that",
            media.name, media.duration
        ));
    }
    if out_point - in_point < MIN_CLIP - EPSILON {
        return Err(format!(
            "`out` must be at least {MIN_CLIP}s after `in` ({in_point}s)"
        ));
    }
    Ok((in_point, out_point))
}

/// Build `add-clip` from its arguments against `project`.
fn place(project: &Project, args: &ActionArgs) -> Result<VideoEditorAction, String> {
    let media = media_named(project, args.text("media").unwrap_or_default())?;
    let (track, made) = placement_track(project, args, lane_for(media.kind))?;
    let (in_point, out_point) = source_window(media, args)?;
    let start = match time_arg(args, "at")? {
        Some(at) => at,
        None => project
            .clips_on(track)
            .iter()
            .map(|c| c.end())
            .fold(0.0, f64::max),
    };
    let mut clip = Clip {
        id: Uuid::new_v4(),
        track,
        media: media.id,
        start,
        in_point,
        out_point,
        palette: media.palette,
        ..Default::default()
    };
    apply_fades(&mut clip, args)?;
    refuse_overlap(project, track, (clip.start, clip.end()), &[])?;
    Ok(upsert_tracks_and_clips_action(&made, &[clip]))
}

/// `add-clip`: a bin source placed on a track at a time, reading a window of
/// the source.
fn add_clip(project: &Project) -> ActionDecl<VideoProject> {
    let project = project.clone();
    ActionDecl::new(
        ADD_CLIP,
        "Place a source on the timeline",
        vec![
            ActionParam::required(
                "media",
                ParamType::Text,
                "The bin source to place, a video, sound, picture or title: its id (as \
                 import-content answered or read-timeline lists it) or its name in the bin",
            ),
            ActionParam::optional(
                "track",
                ParamType::Text,
                "The track: its id or name (V1, A1, …). A name like V2 that no track has yet \
                 makes that track. Default: the first unlocked track of the source's kind \
                 (video for pictures and titles, audio for sound), made if there is none",
            ),
            ActionParam::optional(
                "at",
                ParamType::Number,
                "Seconds on the timeline where the clip starts; default: the end of the \
                 track's last clip",
            ),
            ActionParam::optional(
                "in",
                ParamType::Number,
                "Seconds into the source where the clip begins reading (default 0)",
            ),
            ActionParam::optional(
                "out",
                ParamType::Number,
                "Seconds into the source where the clip stops reading, after `in`; default: \
                 the source's end. The clip lasts out - in seconds",
            ),
        ]
        .into_iter()
        .chain(fade_params())
        .collect(),
        move |args, _now| place(&project, args),
    )
    .describe(
        "Place a bin source (video, sound, picture or title) on a track as a new clip: at \
         `at` seconds on the timeline, reading the source from `in` to `out` seconds, with \
         optional fades. A clip may not cover another on the same track. Answers the new \
         clip's id and the timeline.",
    )
    .answering()
}

/// The clip an argument names: its id, `video.clip:<id>` too.
pub(crate) fn clip_named<'a>(project: &'a Project, named: &str) -> Result<&'a Clip, String> {
    let named = named.trim();
    let id = named
        .trim_start_matches("video.clip:")
        .parse::<Uuid>()
        .map_err(|_| format!("`{named}` is no clip id; read-timeline lists them"))?;
    project
        .clip(id)
        .ok_or_else(|| format!("the timeline has no clip {id}"))
}

/// `move-clip`: a placed clip to a new start, and to another track of its
/// kind when named. The clips linked to it keep their place beside it.
fn move_clip(project: &Project) -> ActionDecl<VideoProject> {
    let project = project.clone();
    ActionDecl::new(
        MOVE_CLIP,
        "Move a clip",
        vec![
            ActionParam::required(
                "clip",
                ParamType::Text,
                "The clip's id, as add-clip answered or read-timeline lists it",
            ),
            ActionParam::required(
                "start",
                ParamType::Number,
                "Seconds on the timeline where the clip starts after the move",
            ),
            ActionParam::optional(
                "track",
                ParamType::Text,
                "Another track of the same kind to move it to: its id or name (V2, A1, …); \
                 default: the track it is on",
            ),
        ]
        .into_iter()
        .chain(fade_params())
        .collect(),
        move |args, _now| {
            let clip = clip_named(&project, args.text("clip").unwrap_or_default())?;
            let start = time_arg(args, "start")?.ok_or("give `start`, seconds on the timeline")?;
            let target = match args.text("track").map(str::trim).filter(|t| !t.is_empty()) {
                None => clip.track,
                Some(named) => match track_named(&project, named)? {
                    TrackChoice::Existing(id) => id,
                    TrackChoice::New(track) => {
                        return Err(format!(
                            "the project has no track {}: add it first (add-video-track, \
                             add-audio-track) or place a clip on it with add-clip",
                            track.name
                        ));
                    }
                },
            };
            if !project.clip_editable(clip.id) {
                return Err("the clip's track is locked".into());
            }
            if target != clip.track && !project.can_place(clip.id, target) {
                return Err(
                    "the clip cannot go on that track: it is locked or of the other kind".into(),
                );
            }
            let group = project.link_group(clip.id);
            let delta = start - clip.start;
            let mut moved = project.clone();
            for id in &group {
                if let Some(member) = moved.clip_mut(*id) {
                    member.start = (member.start + delta).max(0.0);
                    if *id == clip.id {
                        member.track = target;
                        apply_fades(member, args)?;
                    }
                }
            }
            for id in &group {
                if let Some(member) = moved.clip(*id) {
                    refuse_overlap(&project, member.track, (member.start, member.end()), &group)?;
                }
            }
            match patch_clips_fields_action(&project, &moved, &group) {
                VideoEditorAction::PatchClipsFields { patches } if patches.is_empty() => {
                    Err("the clip is there already".into())
                }
                action => Ok(action),
            }
        },
    )
    .describe(
        "Move a placed clip to start at `start` seconds, keeping its length and the part of \
         the source it reads; to another track of its kind when `track` is given, and with \
         the fades given. Clips linked to it move by the same amount. Answers the moved clips \
         and the timeline.",
    )
    .answering()
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::similar_names,
    reason = "times built from exact literals compare exactly; the bindings are named after \
              the MP4 boxes they hold"
)]
mod tests {
    use super::*;
    use tilo_actions::Action as _;

    /// A blank project document, edited through its declarations.
    struct Fixture {
        doc: Doc,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                doc: automerge::AutoCommit::new(),
            }
        }

        fn project(&self) -> Project {
            VideoDoc::load_project(&self.doc)
        }

        /// Run declaration `id` with `arguments`, applying what it builds.
        fn run(&mut self, id: &str, arguments: serde_json::Value) -> Result<(), String> {
            let project = self.project();
            let decl = declarations(&project)
                .into_iter()
                .find(|decl| decl.id == id)
                .ok_or("not declared")?;
            let serde_json::Value::Object(arguments) = arguments else {
                return Err("arguments are an object".into());
            };
            let args = ActionArgs::bind_json(&decl.params, &arguments)
                .map_err(|error| format!("{error:?}"))?;
            let action = (decl.build)(&args, 0)?;
            action.apply(&mut self.doc).map_err(|e| e.to_string())
        }

        /// Run `id` and answer as the resource answers it.
        fn answered(&mut self, id: &str, arguments: serde_json::Value) -> serde_json::Value {
            let heads = self.doc.get_heads();
            self.run(id, arguments).expect("the declaration applies");
            let after = self.doc.get_heads();
            let changes = self.doc.diff(&heads, &after);
            answer(&self.doc, id, &changes).expect("it answers")
        }

        fn put_media(&mut self, item: &MediaItem) {
            upsert_media_action(item)
                .apply(&mut self.doc)
                .expect("the media is written");
        }
    }

    /// An ISOBMFF box.
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(&kind);
        out.extend_from_slice(payload);
        out
    }

    /// The header of a `width`×`height` MP4 lasting `millis` milliseconds at
    /// 25 frames a second: what the probe reads, no frames.
    fn tiny_mp4(millis: u32, width: u32, height: u32) -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&millis.to_be_bytes());
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&(width << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(height << 16).to_be_bytes());
        let mut mdhd = vec![0u8; 24];
        mdhd[12..16].copy_from_slice(&25_000u32.to_be_bytes());
        mdhd[16..20].copy_from_slice(&(millis * 25).to_be_bytes());
        let mut hdlr = vec![0u8; 24];
        hdlr[8..12].copy_from_slice(b"vide");
        let mut stsd = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 16];
        stsd.extend_from_slice(b"avc1");
        let mut stts = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stts.extend_from_slice(&(millis / 40).to_be_bytes());
        stts.extend_from_slice(&1000u32.to_be_bytes());
        let stbl = boxed(
            *b"stbl",
            &[boxed(*b"stsd", &stsd), boxed(*b"stts", &stts)].concat(),
        );
        let mdia = boxed(
            *b"mdia",
            &[
                boxed(*b"mdhd", &mdhd),
                boxed(*b"hdlr", &hdlr),
                boxed(*b"minf", &stbl),
            ]
            .concat(),
        );
        let trak = boxed(*b"trak", &[boxed(*b"tkhd", &tkhd), mdia].concat());
        let moov = boxed(*b"moov", &[boxed(*b"mvhd", &mvhd), trak].concat());
        [
            boxed(*b"ftyp", b"isom\0\0\0\0isomavc1"),
            moov,
            boxed(*b"mdat", &[]),
        ]
        .concat()
    }

    /// The header of a `width`×`height` PNG.
    fn tiny_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        png
    }

    fn footage(name: &str, kind: MediaKind, duration: f64) -> MediaItem {
        MediaItem {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            duration,
            ..Default::default()
        }
    }

    #[test]
    fn a_source_is_placed_on_the_track_of_its_kind_made_when_there_is_none() {
        let mut fixture = Fixture::new();
        let harbour = footage("Harbour", MediaKind::Video, 8.0);
        let gulls = footage("Gulls", MediaKind::Audio, 20.0);
        fixture.put_media(&harbour);
        fixture.put_media(&gulls);

        let first = fixture.answered(
            ADD_CLIP,
            serde_json::json!({"media": "Harbour", "in": 1.0, "out": 3.0}),
        );
        let project = fixture.project();
        let clip_id: Uuid = first["clip"].as_str().unwrap().parse().unwrap();
        let clip = project.clip(clip_id).unwrap();
        assert_eq!(first["track"], "V1");
        assert_eq!(clip.start, 0.0);
        assert_eq!((clip.in_point, clip.out_point), (1.0, 3.0));
        assert_eq!(
            first["timeline"]["tracks"][0]["clips"][0]["id"],
            clip_id.to_string()
        );

        // Without `at`, the next clip follows the last on its track.
        let second = fixture.answered(
            ADD_CLIP,
            serde_json::json!({"media": harbour.id.to_string()}),
        );
        assert_eq!(second["start"], 2.0);
        assert_eq!(second["end"], 10.0);

        // Sound goes to an audio lane of its own.
        let sound = fixture.answered(ADD_CLIP, serde_json::json!({"media": "gulls", "at": 4.0}));
        assert_eq!(sound["track"], "A1");
        assert_eq!(fixture.project().clips.len(), 3);
    }

    #[test]
    fn a_placement_is_refused_with_a_reason_it_can_be_corrected_by() {
        let mut fixture = Fixture::new();
        let harbour = footage("Harbour", MediaKind::Video, 8.0);
        fixture.put_media(&harbour);
        fixture
            .run(ADD_CLIP, serde_json::json!({"media": "Harbour", "at": 0.0}))
            .unwrap();

        let overlap = fixture
            .run(ADD_CLIP, serde_json::json!({"media": "Harbour", "at": 4.0}))
            .unwrap_err();
        assert!(overlap.contains("8.000s or later"), "{overlap}");
        let long = fixture
            .run(
                ADD_CLIP,
                serde_json::json!({"media": "Harbour", "at": 9.0, "out": 12.0}),
            )
            .unwrap_err();
        assert!(long.contains("lasts 8.000s"), "{long}");
        let unknown = fixture
            .run(ADD_CLIP, serde_json::json!({"media": "Lighthouse"}))
            .unwrap_err();
        assert!(
            unknown.contains("Harbour"),
            "names what the bin holds: {unknown}"
        );
        assert!(
            unknown.contains("import-content first") && unknown.contains("tilo.content:<hash>"),
            "says how to bring the source in: {unknown}"
        );
        let address = fixture
            .run(
                ADD_CLIP,
                serde_json::json!({"media": "tilo.content:abc123"}),
            )
            .unwrap_err();
        assert!(
            address.contains("is a content address") && address.contains("import-content it first"),
            "{address}"
        );
        let sound_lane = fixture
            .run(
                ADD_CLIP,
                serde_json::json!({"media": "Harbour", "track": "A1"}),
            )
            .unwrap_err();
        assert!(sound_lane.contains("video track"), "{sound_lane}");
        assert_eq!(fixture.project().clips.len(), 1, "nothing else was placed");
    }

    #[test]
    fn a_title_is_placed_on_a_new_upper_track_for_as_long_as_asked() {
        let mut fixture = Fixture::new();
        let harbour = footage("Harbour", MediaKind::Video, 8.0);
        fixture.put_media(&harbour);
        let mut title = footage("Opening", MediaKind::Text, 5.0);
        title.text = Some(crate::model::TextSource::new("Opening"));
        fixture.put_media(&title);
        fixture
            .run(ADD_CLIP, serde_json::json!({"media": "Harbour"}))
            .unwrap();

        let placed = fixture.answered(
            ADD_CLIP,
            serde_json::json!({"media": "Opening", "track": "V2", "at": 1.0, "out": 7.0}),
        );
        assert_eq!(placed["track"], "V2");
        let timeline = &placed["timeline"];
        assert_eq!(timeline["tracks"][0]["name"], "V2", "the upper track first");
        assert_eq!(timeline["tracks"][0]["clips"][0]["title"], "Opening");
        assert_eq!(timeline["tracks"][0]["clips"][0]["duration"], 7.0);
    }

    #[test]
    fn a_clip_moves_in_time_and_across_tracks_of_its_kind() {
        let mut fixture = Fixture::new();
        let harbour = footage("Harbour", MediaKind::Video, 8.0);
        fixture.put_media(&harbour);
        let placed = fixture.answered(
            ADD_CLIP,
            serde_json::json!({"media": "Harbour", "out": 2.0}),
        );
        let clip = placed["clip"].as_str().unwrap().to_owned();
        fixture
            .run(
                ADD_CLIP,
                serde_json::json!({"media": "Harbour", "track": "V2", "at": 0.0, "out": 1.0}),
            )
            .unwrap();

        let moved = fixture.answered(
            MOVE_CLIP,
            serde_json::json!({"clip": clip, "start": 5.0, "track": "V2"}),
        );
        assert_eq!(moved["moved"][0]["id"], clip.as_str());
        assert_eq!(moved["moved"][0]["start"], 5.0);
        let project = fixture.project();
        let clip = project.clip(clip.parse().unwrap()).unwrap();
        assert_eq!(clip.start, 5.0);
        assert_eq!(project.track(clip.track).unwrap().name, "V2");
        assert_eq!(
            (clip.in_point, clip.out_point),
            (0.0, 2.0),
            "the window is kept"
        );

        let onto = fixture
            .run(
                MOVE_CLIP,
                serde_json::json!({"clip": clip.id.to_string(), "start": 0.5}),
            )
            .unwrap_err();
        assert!(onto.contains("1.000s or later"), "{onto}");
        let missing = fixture
            .run(
                MOVE_CLIP,
                serde_json::json!({"clip": clip.id.to_string(), "start": 0.0, "track": "V9"}),
            )
            .unwrap_err();
        assert!(missing.contains("no track V9"), "{missing}");
    }

    #[test]
    fn content_becomes_a_bin_source_with_the_duration_its_container_gives() {
        let hash = "cd".repeat(32);
        let mp4 = tiny_mp4(2_500, 64, 48);
        let item = media_from_content(&hash, "harbour.mp4", &mp4, None, 3).unwrap();
        assert_eq!(item.kind, MediaKind::Video);
        assert!((item.duration - 2.5).abs() < 1e-9, "{}", item.duration);
        assert_eq!((item.width, item.height), (64, 48));
        assert!((item.fps - 25.0).abs() < 1e-9, "{}", item.fps);
        assert_eq!(item.path, format!("tilo.content:{hash}"));
        assert_eq!(crate::media::content_hash(&item.path), Some(hash.as_str()));

        // A caller's duration fills only what the container leaves out.
        let given = media_from_content(&hash, "harbour.mp4", &mp4, Some(9.0), 3).unwrap();
        assert!((given.duration - 2.5).abs() < 1e-9);
        let png = tiny_png(16, 9);
        let still = media_from_content(&hash, "card.png", &png, Some(3.0), 0).unwrap();
        assert_eq!((still.kind, still.duration), (MediaKind::Image, 3.0));

        let text = media_from_content(&hash, "notes.txt", b"just words\n", None, 0).unwrap_err();
        assert!(text.contains("not a media file"), "{text}");
        assert!(content_hash_of("tilo.content:xyz").is_err());
        assert_eq!(
            content_hash_of(&format!("tilo.content:{}", hash.to_uppercase())).unwrap(),
            hash
        );
    }
}
