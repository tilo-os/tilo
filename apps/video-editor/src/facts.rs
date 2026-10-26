//! The project's facts: the cut as a brief states it, to check it against
//! one ([`facts`]). Shots in timeline order with their source window,
//! titles with their words, colour and timing, the sound lanes, the frame
//! and the total length, in seconds to two decimals. No ids: two versions'
//! facts compare field by field.

use serde_json::{Map, Value, json};

use crate::model::{Clip, MediaKind, Project, Track, TrackKind, Transition};

/// Gaps shorter than this are rounding, not a gap a person would see.
const GAP: f64 = 0.005;

/// Seconds to two decimals.
fn secs(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// What the cut holds, as a brief would state it.
///
/// # Errors
/// Never; the signature is the one `#[resource(facts = path)]` takes.
pub fn facts(project: &Project) -> Result<Value, String> {
    Ok(of(project))
}

/// [`facts`] of `project`.
#[must_use]
pub fn of(project: &Project) -> Value {
    let mut out = Map::new();
    out.insert("project".into(), json!(project.name));
    out.insert(
        "frame".into(),
        json!({ "width": project.width, "height": project.height, "fps": project.fps }),
    );
    let placed = !project.clips.is_empty();
    out.insert(
        "duration".into(),
        json!(if placed {
            secs(project.duration())
        } else {
            0.0
        }),
    );
    if project.mark_in.is_some() || project.mark_out.is_some() {
        out.insert(
            "render_range".into(),
            json!({
                "start": secs(project.mark_in.unwrap_or(0.0)),
                "end": secs(project.mark_out.unwrap_or_else(|| project.duration())),
            }),
        );
    }
    let pictures = picture_clips(project, false);
    out.insert(
        "shots".into(),
        Value::Array(
            pictures
                .iter()
                .map(|(clip, track)| shot(project, clip, track))
                .collect(),
        ),
    );
    let gaps = gaps(project, &pictures);
    if !gaps.is_empty() {
        out.insert("gaps".into(), Value::Array(gaps));
    }
    out.insert(
        "titles".into(),
        Value::Array(
            picture_clips(project, true)
                .iter()
                .map(|(clip, track)| title(project, clip, track))
                .collect(),
        ),
    );
    out.insert("audio".into(), Value::Array(audio(project)));
    Value::Object(out)
}

/// The clips on picture lanes, in timeline order (lower lane first at one
/// time): titles when `titles`, else every other picture.
fn picture_clips(project: &Project, titles: bool) -> Vec<(&Clip, &Track)> {
    let mut clips: Vec<(&Clip, &Track)> = project
        .clips
        .iter()
        .filter_map(|clip| {
            let track = project.track(clip.track)?;
            let kind = project.media(clip.media)?.kind;
            let wanted = track.kind == TrackKind::Video && (kind == MediaKind::Text) == titles;
            wanted.then_some((clip, track))
        })
        .collect();
    clips.sort_by(|(a, at), (b, bt)| a.start.total_cmp(&b.start).then(at.index.cmp(&bt.index)));
    clips
}

/// Where `clip` sits and what it takes from its source.
fn placement(project: &Project, clip: &Clip, track: &Track) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert(
        "source".into(),
        json!(project.media(clip.media).map(|media| media.name.as_str())),
    );
    out.insert("track".into(), json!(track.name));
    out.insert("start".into(), json!(secs(clip.start)));
    out.insert("end".into(), json!(secs(clip.end())));
    out.insert("in".into(), json!(secs(clip.in_point)));
    out.insert("out".into(), json!(secs(clip.out_point)));
    out.insert("duration".into(), json!(secs(clip.duration())));
    out.insert("fade_in".into(), json!(secs(clip.fade_in)));
    out.insert("fade_out".into(), json!(secs(clip.fade_out)));
    if !clip.speed.is_normal() {
        out.insert("speed".into(), json!(clip.speed.signed()));
    }
    out
}

/// One shot: [`placement`], and what changes its picture from a plain cut.
fn shot(project: &Project, clip: &Clip, track: &Track) -> Value {
    let mut out = placement(project, clip, track);
    if clip.transition != Transition::None {
        out.insert(
            "transition".into(),
            json!({ "kind": clip.transition.as_str(), "length": secs(clip.transition_len()) }),
        );
    }
    if (clip.opacity - 1.0).abs() > f64::EPSILON {
        out.insert("opacity".into(), json!(secs(clip.opacity)));
    }
    if !clip.effects.is_empty() {
        let effects: Vec<&str> = clip
            .effects
            .iter()
            .map(|effect| effect.kind.as_str())
            .collect();
        out.insert("effects".into(), json!(effects));
    }
    Value::Object(out)
}

/// One title: its words and look, where it shows, and its lane's place in
/// the stack (`layer` 1 is the bottom picture lane).
fn title(project: &Project, clip: &Clip, track: &Track) -> Value {
    let mut out = Map::new();
    if let Some(text) = project
        .media(clip.media)
        .and_then(|media| media.text.as_ref())
    {
        out.insert("text".into(), json!(text.content));
        out.insert(
            "colour".into(),
            json!(crate::agent_timeline::colour_hex(text.color)),
        );
        out.insert("size".into(), json!(text.size));
    }
    out.insert("track".into(), json!(track.name));
    out.insert("layer".into(), json!(track.index));
    out.insert("start".into(), json!(secs(clip.start)));
    out.insert("end".into(), json!(secs(clip.end())));
    out.insert("fade_in".into(), json!(secs(clip.fade_in)));
    out.insert("fade_out".into(), json!(secs(clip.fade_out)));
    Value::Object(out)
}

/// The stretches of the timeline before its end where a lane holding shots
/// shows none, from 0 s.
fn gaps(project: &Project, pictures: &[(&Clip, &Track)]) -> Vec<Value> {
    let mut gaps = Vec::new();
    for track in project.display_tracks() {
        let mut at = 0.0_f64;
        for (clip, _) in pictures.iter().filter(|(_, lane)| lane.id == track.id) {
            if clip.start - at > GAP {
                gaps.push(json!({ "track": track.name, "from": secs(at), "to": secs(clip.start) }));
            }
            at = at.max(clip.end());
        }
    }
    gaps
}

/// The sound lanes, top first, each with its clips left to right.
fn audio(project: &Project) -> Vec<Value> {
    project
        .display_tracks()
        .into_iter()
        .filter(|track| track.kind == TrackKind::Audio)
        .map(|track| {
            let mut out = Map::new();
            out.insert("track".into(), json!(track.name));
            if track.muted {
                out.insert("muted".into(), json!(true));
            }
            if track.gain_db.abs() > f64::EPSILON {
                out.insert("gain_db".into(), json!(secs(track.gain_db)));
            }
            let clips: Vec<Value> = project
                .clips_on(track.id)
                .into_iter()
                .map(|clip| {
                    let mut placed = placement(project, clip, track);
                    if (clip.volume - 1.0).abs() > f64::EPSILON {
                        placed.insert("volume".into(), json!(secs(clip.volume)));
                    }
                    Value::Object(placed)
                })
                .collect();
            out.insert("clips".into(), Value::Array(clips));
            Value::Object(out)
        })
        .collect()
}

#[cfg(test)]
mod tests;
