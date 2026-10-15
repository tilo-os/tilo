//! The timeline as data a caller with no screen can read and check: every
//! track top to bottom with its clips in time order, each clip with the
//! source it plays, where it sits, the window of the source it reads and
//! what is applied to it. `read-timeline` answers it and
//! `export-timeline-content` keeps it as content, so an agent sees the edit
//! it made and a reviewer can compare two cuts without decoding a frame.

use serde_json::{Map, Value, json};

use crate::model::{Clip, MediaItem, Project, Transition};

/// Seconds rounded to a tenth of a millisecond: what the timeline can tell
/// apart, without the float noise a trim leaves (`4.999999999`).
fn secs(value: f64) -> f64 {
    if value.is_finite() {
        (value * 10_000.0).round() / 10_000.0
    } else {
        0.0
    }
}

/// A title colour as `#rrggbb`.
fn hex(color: [f64; 4]) -> String {
    let byte = |c: f64| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let [red, green, blue, _] = color;
    format!("#{:02x}{:02x}{:02x}", byte(red), byte(green), byte(blue))
}

/// Where a source's bytes are: its content address when the room holds it,
/// else the path it was imported from. A title has neither.
fn source_fields(media: &MediaItem, fields: &mut Map<String, Value>) {
    if let Some(hash) = crate::media::content_hash(&media.path) {
        fields.insert("content".into(), crate::media::content_address(hash).into());
    } else if !media.path.is_empty() {
        fields.insert("path".into(), media.path.clone().into());
    }
}

/// A title's words and how they are set.
fn title_fields(media: &MediaItem, fields: &mut Map<String, Value>) {
    if let Some(text) = &media.text {
        fields.insert(
            "title".into(),
            json!({
                "text": text.content,
                "color": hex(text.color),
                "size": secs(text.size),
                "align": text.align.as_str(),
            }),
        );
    }
}

/// One bin source: what it is, how long, how large, where its bytes are and
/// how many clips place it.
#[must_use]
pub fn media_json(project: &Project, media: &MediaItem) -> Value {
    let mut fields = Map::new();
    fields.insert("id".into(), media.id.to_string().into());
    fields.insert("name".into(), media.name.clone().into());
    fields.insert("kind".into(), media.kind.as_str().into());
    fields.insert("duration".into(), secs(media.duration).into());
    if media.width > 0 && media.height > 0 {
        fields.insert("width".into(), media.width.into());
        fields.insert("height".into(), media.height.into());
    }
    if media.kind.is_visual() && !media.kind.is_generated() && media.fps > 0.0 {
        fields.insert("fps".into(), secs(media.fps).into());
    }
    if media.has_audio {
        fields.insert("has_audio".into(), true.into());
    }
    source_fields(media, &mut fields);
    title_fields(media, &mut fields);
    let placed = project.clips.iter().filter(|c| c.media == media.id).count();
    fields.insert("placed".into(), placed.into());
    Value::Object(fields)
}

/// One placed clip, as [`timeline_json`] lists it under its track. What is
/// at its default (full opacity and volume, no fades, a cut, no effects, the
/// ungraded picture, normal speed) is left out.
#[must_use]
pub fn clip_json(project: &Project, clip: &Clip) -> Value {
    let mut fields = Map::new();
    fields.insert("id".into(), clip.id.to_string().into());
    fields.insert("media".into(), clip.media.to_string().into());
    if let Some(media) = project.media(clip.media) {
        fields.insert("name".into(), media.name.clone().into());
        fields.insert("kind".into(), media.kind.as_str().into());
        if let Some(text) = &media.text {
            fields.insert("title".into(), text.content.clone().into());
        }
    }
    fields.insert("start".into(), secs(clip.start).into());
    fields.insert("end".into(), secs(clip.end()).into());
    fields.insert("duration".into(), secs(clip.duration()).into());
    fields.insert("in".into(), secs(clip.in_point).into());
    fields.insert("out".into(), secs(clip.out_point).into());
    if !clip.speed.is_normal() {
        fields.insert(
            "speed".into(),
            json!({"rate": secs(clip.speed.rate()), "reverse": clip.speed.reverse}),
        );
    }
    for (key, value, default) in [
        ("opacity", clip.opacity, 1.0),
        ("volume", clip.volume, 1.0),
        ("fade_in", clip.fade_in, 0.0),
        ("fade_out", clip.fade_out, 0.0),
    ] {
        if (value - default).abs() > 1e-9 {
            fields.insert(key.into(), secs(value).into());
        }
    }
    if clip.transition != Transition::None {
        fields.insert(
            "transition".into(),
            json!({
                "kind": clip.transition.as_str(),
                "duration": secs(clip.transition_len()),
            }),
        );
    }
    if !clip.effects.is_empty() {
        fields.insert(
            "effects".into(),
            clip.effects
                .iter()
                .map(|effect| {
                    json!({
                        "id": effect.id.to_string(),
                        "kind": effect.kind.as_str(),
                        "amount": secs(effect.amount),
                    })
                })
                .collect::<Vec<_>>()
                .into(),
        );
    }
    if !clip.filters.is_identity() {
        fields.insert(
            "grade".into(),
            json!({
                "brightness": secs(clip.filters.brightness),
                "contrast": secs(clip.filters.contrast),
                "saturation": secs(clip.filters.saturation),
            }),
        );
    }
    if !clip.markers.is_empty() {
        fields.insert(
            "markers".into(),
            clip.markers
                .iter()
                .map(|marker| json!({"at": secs(marker.t), "name": marker.name}))
                .collect::<Vec<_>>()
                .into(),
        );
    }
    let partners = project.link_partners(clip.id);
    if !partners.is_empty() {
        fields.insert(
            "linked".into(),
            partners
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .into(),
        );
    }
    Value::Object(fields)
}

/// The whole timeline: every track top to bottom with its clips in time
/// order, then the project's format and length, the bin's sources and the
/// markers. The tracks lead, so an answer cut short keeps the cut.
#[must_use]
pub fn timeline_json(project: &Project) -> Value {
    let length = project.clips.iter().map(Clip::end).fold(0.0_f64, f64::max);
    let tracks: Vec<Value> = project
        .display_tracks()
        .into_iter()
        .map(|track| {
            let mut fields = Map::new();
            fields.insert("id".into(), track.id.to_string().into());
            fields.insert("name".into(), track.name.clone().into());
            fields.insert("kind".into(), track.kind.as_str().into());
            if track.muted {
                fields.insert("muted".into(), true.into());
            }
            if track.locked {
                fields.insert("locked".into(), true.into());
            }
            fields.insert(
                "clips".into(),
                project
                    .clips_on(track.id)
                    .into_iter()
                    .map(|clip| clip_json(project, clip))
                    .collect::<Vec<_>>()
                    .into(),
            );
            Value::Object(fields)
        })
        .collect();
    let mut media: Vec<&MediaItem> = project.media.iter().collect();
    media.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    let mut markers: Vec<_> = project.markers.iter().collect();
    markers.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut about = Map::new();
    about.insert("name".into(), project.name.clone().into());
    about.insert("duration".into(), secs(length).into());
    about.insert("width".into(), project.width.into());
    about.insert("height".into(), project.height.into());
    about.insert("fps".into(), secs(project.fps).into());
    if let Some(mark) = project.mark_in {
        about.insert("mark_in".into(), secs(mark).into());
    }
    if let Some(mark) = project.mark_out {
        about.insert("mark_out".into(), secs(mark).into());
    }
    json!({
        "tracks": tracks,
        "project": about,
        "media": media
            .into_iter()
            .map(|item| media_json(project, item))
            .collect::<Vec<_>>(),
        "markers": markers
            .into_iter()
            .map(|marker| json!({"id": marker.id.to_string(), "at": secs(marker.t), "name": marker.name}))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipEffect, EffectKind, MediaKind, TextSource, Track, TrackKind};
    use uuid::Uuid;

    #[test]
    fn the_timeline_lists_tracks_with_their_clips_in_time_order_and_what_each_plays() {
        let mut project = Project::new("Cut");
        project.tracks.push(Track {
            id: Uuid::new_v4(),
            name: "V1".into(),
            kind: TrackKind::Video,
            index: 0,
            ..Default::default()
        });
        let track = project.tracks[0].id;
        let hash = "ab".repeat(32);
        let footage = MediaItem {
            id: Uuid::new_v4(),
            name: "Harbour".into(),
            kind: MediaKind::Video,
            duration: 8.0,
            width: 64,
            height: 48,
            fps: 25.0,
            path: crate::media::content_address(&hash),
            ..Default::default()
        };
        let title = MediaItem {
            id: Uuid::new_v4(),
            name: "Opening".into(),
            kind: MediaKind::Text,
            duration: 5.0,
            text: Some(TextSource::new("Opening")),
            ..Default::default()
        };
        project.media = vec![footage.clone(), title.clone()];
        let late = project.add_clip(track, footage.id, 6.0).unwrap();
        let early = project.add_clip(track, title.id, 0.0).unwrap();
        let clip = project.clip_mut(late).unwrap();
        clip.in_point = 1.0;
        clip.out_point = 3.5;
        clip.fade_in = 0.5;
        clip.transition = Transition::CrossDissolve;
        clip.effects.push(ClipEffect::new(EffectKind::Sepia));

        let timeline = timeline_json(&project);
        let clips = timeline["tracks"][0]["clips"].as_array().unwrap();
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0]["id"], early.to_string(), "in time order");
        assert_eq!(clips[0]["title"], "Opening");
        assert!(
            clips[0].get("opacity").is_none() && clips[0].get("transition").is_none(),
            "defaults are left out: {}",
            clips[0]
        );
        assert_eq!(clips[0]["kind"], "text");
        assert_eq!(clips[1]["start"], 6.0);
        assert_eq!(clips[1]["duration"], 2.5);
        assert_eq!(clips[1]["end"], 8.5);
        assert_eq!(clips[1]["in"], 1.0);
        assert_eq!(clips[1]["out"], 3.5);
        assert_eq!(clips[1]["fade_in"], 0.5);
        assert_eq!(clips[1]["transition"]["kind"], "dissolve");
        assert_eq!(clips[1]["effects"][0]["kind"], "sepia");
        assert_eq!(timeline["project"]["duration"], 8.5);
        let bin = timeline["media"].as_array().unwrap();
        assert_eq!(bin[0]["name"], "Harbour", "the bin by name");
        assert_eq!(bin[0]["placed"], 1);
        assert_eq!(bin[0]["width"], 64);
        assert_eq!(bin[0]["content"], format!("tilo.content:{hash}"));
        assert_eq!(bin[1]["title"]["text"], "Opening");
        assert_eq!(
            timeline
                .as_object()
                .unwrap()
                .keys()
                .next()
                .map(String::as_str),
            Some("tracks"),
            "the tracks lead"
        );
    }
}
