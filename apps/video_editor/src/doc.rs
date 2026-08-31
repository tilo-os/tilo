//! Automerge document model for the editor project.
//!
//! Structure (the shared, collaborative half of the editor — playhead,
//! zoom and selection are deliberately per-peer view-state and live in
//! the widget, not here):
//!
//! ```text
//! root
//! ├── "project" (Map)  name / fps / width / height
//! ├── "media"   (Map keyed by MediaId)  → { name,kind,duration,w,h,fps,palette }
//! ├── "tracks"  (Map keyed by TrackId)  → { name,kind,index,muted,locked,solo }
//! └── "clips"   (Map keyed by ClipId)   → { track,media,start,in,out,opacity,
//!                                            volume,transition,transition_duration,
//!                                            palette,filters,fades,effects }
//! ```
//!
//! Every helper takes the borrowed `&mut AutoCommit` (the app's shared
//! `State::doc`). Two peers that adopt a common base converge: a moved
//! clip, a trimmed in-point, a new track all merge field-by-field.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use uuid::Uuid;

use crate::media::{self, Hydration};
use crate::model::{
    Clip, EffectKind, Filters, MediaItem, MediaKind, Project, Track, TrackKind, Transition,
};

pub struct VideoDoc;

impl VideoDoc {
    fn map(doc: &mut AutoCommit, key: &str) -> ObjId {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, key, ObjType::Map)
                .expect("put_object map"),
        }
    }

    fn ensure_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object child"),
        }
    }

    fn get_f64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<f64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::F64(f) => Some(*f),
                ScalarValue::Int(i) => Some(*i as f64),
                ScalarValue::Uint(u) => Some(*u as f64),
                _ => None,
            },
            _ => None,
        }
    }
    fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Int(i) => Some(*i),
                ScalarValue::Uint(u) => Some(*u as i64),
                ScalarValue::F64(f) => Some(*f as i64),
                _ => None,
            },
            _ => None,
        }
    }
    /// A boolean whose absence means `default` rather than `false` — for a
    /// field added after documents were already being written, where the old
    /// shape's silence has a meaning of its own.
    fn get_bool_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: bool) -> bool {
        match doc.get(obj, key) {
            Ok(Some((automerge::Value::Scalar(v), _))) => match v.as_ref() {
                automerge::ScalarValue::Boolean(b) => *b,
                _ => default,
            },
            _ => default,
        }
    }

    fn get_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> bool {
        matches!(
            doc.get(obj, key),
            Ok(Some((Value::Scalar(v), _))) if matches!(v.as_ref(), ScalarValue::Boolean(true))
        )
    }
    fn get_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Whether the map at `key` exists **and** holds at least one entry.
    /// A map that was created but never written to is not content.
    fn has_entries(doc: &AutoCommit, key: &str) -> bool {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.length(&id) > 0,
            _ => false,
        }
    }

    /// True when this document carries no editor content at all.
    ///
    /// Emptiness spans **every** map an editing session writes, not just
    /// `project`: importing media writes only the `media` map, and a document
    /// holding imported clips is plainly not empty even before a project map
    /// exists. Keying on `project` alone reported such a document empty, which
    /// let a demo seed overwrite real imported media.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !["project", "media", "tracks", "clips"]
            .iter()
            .any(|k| Self::has_entries(doc, k))
    }

    // --- writers ---------------------------------------------------------

    pub fn write_project_meta(doc: &mut AutoCommit, p: &Project) {
        let proj = Self::map(doc, "project");
        let _ = doc.put(&proj, "name", p.name.as_str());
        let _ = doc.put(&proj, "fps", p.fps);
        let _ = doc.put(&proj, "width", i64::from(p.width));
        let _ = doc.put(&proj, "height", i64::from(p.height));
        let _ = doc.put(&proj, "format_explicit", p.format_explicit);
        // An absent mark is an absent key, not a sentinel: a peer that reads
        // 0.0 for "no in mark" cannot tell it from a mark on the first frame.
        for (key, mark) in [("mark_in", p.mark_in), ("mark_out", p.mark_out)] {
            match mark {
                Some(t) => {
                    let _ = doc.put(&proj, key, t);
                }
                None => {
                    let _ = doc.delete(&proj, key);
                }
            }
        }
        let _ = doc.put(
            &proj,
            "markers",
            serde_json::to_string(&p.markers).unwrap_or_else(|_| "[]".to_string()),
        );
    }

    pub fn upsert_media(doc: &mut AutoCommit, m: &MediaItem) {
        let media = Self::map(doc, "media");
        let obj = Self::ensure_child(doc, &media, &m.id.to_string());
        let _ = doc.put(&obj, "name", m.name.as_str());
        let _ = doc.put(&obj, "kind", m.kind.as_str());
        let _ = doc.put(&obj, "duration", m.duration);
        let _ = doc.put(&obj, "width", i64::from(m.width));
        let _ = doc.put(&obj, "height", i64::from(m.height));
        let _ = doc.put(&obj, "fps", m.fps);
        let _ = doc.put(&obj, "palette", i64::from(m.palette));
        // FUSE-media seam: the CRDT carries the workspace path + hydration
        // state, never the bytes (see crate::media).
        let _ = doc.put(&obj, "path", m.path.as_str());
        let _ = doc.put(&obj, "hydration", m.hydration.as_str());
        let _ = doc.put(&obj, "has_audio", m.has_audio);
        // A title's text and styling. Written as a compact JSON scalar for
        // the same reason the effects stack is: the action model rewrites a
        // media item whole, so a structured sub-map would buy no finer merge
        // granularity and would cost a schema.
        match &m.text {
            Some(t) => {
                let _ = doc.put(&obj, "text", serde_json::to_string(t).unwrap_or_default());
            }
            // Explicitly cleared, so turning a title back into a plain
            // reference replicates as a change rather than as silence.
            None => {
                let _ = doc.delete(&obj, "text");
            }
        }
    }

    pub fn upsert_track(doc: &mut AutoCommit, t: &Track) {
        let tracks = Self::map(doc, "tracks");
        let obj = Self::ensure_child(doc, &tracks, &t.id.to_string());
        let _ = doc.put(&obj, "name", t.name.as_str());
        let _ = doc.put(&obj, "kind", t.kind.as_str());
        let _ = doc.put(&obj, "index", i64::from(t.index));
        let _ = doc.put(&obj, "muted", t.muted);
        let _ = doc.put(&obj, "locked", t.locked);
        let _ = doc.put(&obj, "solo", t.solo);
    }

    pub fn upsert_clip(doc: &mut AutoCommit, c: &Clip) {
        let clips = Self::map(doc, "clips");
        let obj = Self::ensure_child(doc, &clips, &c.id.to_string());
        let _ = doc.put(&obj, "track", c.track.to_string());
        let _ = doc.put(&obj, "media", c.media.to_string());
        let _ = doc.put(&obj, "start", c.start);
        let _ = doc.put(&obj, "in", c.in_point);
        let _ = doc.put(&obj, "out", c.out_point);
        let _ = doc.put(&obj, "opacity", c.opacity);
        let _ = doc.put(&obj, "volume", c.volume);
        let _ = doc.put(&obj, "transition", c.transition.as_str());
        let _ = doc.put(&obj, "palette", i64::from(c.palette));
        let _ = doc.put(&obj, "brightness", c.filters.brightness);
        let _ = doc.put(&obj, "contrast", c.filters.contrast);
        let _ = doc.put(&obj, "saturation", c.filters.saturation);
        let _ = doc.put(&obj, "fade_in", c.fade_in);
        let _ = doc.put(&obj, "fade_out", c.fade_out);
        let _ = doc.put(&obj, "transition_duration", c.transition_duration);
        // The effects stack is an ordered list; store it as a compact JSON
        // string (the whole clip is written atomically by the action model, so
        // a scalar string is the simplest faithful representation).
        let _ = doc.put(
            &obj,
            "effects",
            serde_json::to_string(&c.effects).unwrap_or_else(|_| "[]".to_string()),
        );
        // Geometry, time-remap, curves and markers. Each is written as its
        // own key rather than one blob, so a peer reading a document that
        // predates any one of them still gets the others.
        let _ = doc.put(
            &obj,
            "transform",
            serde_json::to_string(&c.transform).unwrap_or_default(),
        );
        let _ = doc.put(&obj, "speed_rate", c.speed.rate);
        let _ = doc.put(&obj, "speed_reverse", c.speed.reverse);
        let _ = doc.put(
            &obj,
            "curves",
            serde_json::to_string(&c.curves).unwrap_or_default(),
        );
        let _ = doc.put(
            &obj,
            "key",
            serde_json::to_string(&c.key).unwrap_or_default(),
        );
        let _ = doc.put(
            &obj,
            "mask",
            serde_json::to_string(&c.mask).unwrap_or_default(),
        );
        let _ = doc.put(
            &obj,
            "anim",
            serde_json::to_string(&c.anim).unwrap_or_default(),
        );
        let _ = doc.put(
            &obj,
            "markers",
            serde_json::to_string(&c.markers).unwrap_or_else(|_| "[]".to_string()),
        );
        // The link group. Written as the plain id when there is one and
        // *deleted* when there is not, so unlinking replicates as a change
        // rather than as silence -- a peer that already held the link would
        // otherwise keep drawing the pair and keep moving both halves.
        match c.link {
            Some(link) => {
                let _ = doc.put(&obj, "link", link.to_string());
            }
            None => {
                let _ = doc.delete(&obj, "link");
            }
        }
        let _ = doc.put(&obj, "sound_split", c.sound_split);
        let _ = doc.put(&obj, "link_follows", c.link_follows);
    }

    pub fn remove_clip(doc: &mut AutoCommit, id: Uuid) {
        let clips = Self::map(doc, "clips");
        let _ = doc.delete(&clips, id.to_string().as_str());
    }

    pub fn remove_media(doc: &mut AutoCommit, id: Uuid) {
        let media = Self::map(doc, "media");
        let _ = doc.delete(&media, id.to_string().as_str());
    }

    pub fn remove_track(doc: &mut AutoCommit, id: Uuid) {
        let tracks = Self::map(doc, "tracks");
        let _ = doc.delete(&tracks, id.to_string().as_str());
    }

    /// Mirror an entire in-memory project into the document. Used to seed
    /// the sample project and to push a freshly-loaded project back so
    /// new clips (e.g. a split) replicate.
    pub fn write_project(doc: &mut AutoCommit, p: &Project) {
        Self::write_project_meta(doc, p);
        for m in &p.media {
            Self::upsert_media(doc, m);
        }
        for t in &p.tracks {
            Self::upsert_track(doc, t);
        }
        for c in &p.clips {
            Self::upsert_clip(doc, c);
        }
    }

    /// Every key currently present in the root map `key`.
    fn keys_of(doc: &AutoCommit, key: &str) -> Vec<String> {
        match doc.get(&ROOT, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).collect(),
            _ => Vec::new(),
        }
    }

    /// Make the document hold **exactly** `p` — the write behind opening a
    /// project file.
    ///
    /// The difference from [`write_project`](Self::write_project) is the
    /// removals: anything in the document that `p` does not have goes. Without
    /// that, opening a second project would *merge* it into the first and the
    /// user would get both timelines at once.
    ///
    /// Entries `p` also has are upserted rather than deleted and recreated.
    /// That keeps re-opening the project you already have open close to a
    /// no-op, and — because the keys are stable ids — it is what lets two
    /// peers who open the same file converge on one project instead of two
    /// copies of it.
    pub fn replace_project(doc: &mut AutoCommit, p: &Project) {
        let keep_media: Vec<String> = p.media.iter().map(|m| m.id.to_string()).collect();
        let keep_tracks: Vec<String> = p.tracks.iter().map(|t| t.id.to_string()).collect();
        let keep_clips: Vec<String> = p.clips.iter().map(|c| c.id.to_string()).collect();

        for (map_key, keep) in [
            ("media", &keep_media),
            ("tracks", &keep_tracks),
            ("clips", &keep_clips),
        ] {
            let obj = Self::map(doc, map_key);
            for existing in Self::keys_of(doc, map_key) {
                if !keep.contains(&existing) {
                    let _ = doc.delete(&obj, existing.as_str());
                }
            }
        }

        Self::write_project(doc, p);
    }

    // --- reader ----------------------------------------------------------

    /// Project the whole document back into the in-memory model. Returns a
    /// default empty project if nothing has been seeded.
    pub fn load_project(doc: &AutoCommit) -> Project {
        let mut p = Project::new("Untitled");
        if let Ok(Some((Value::Object(ObjType::Map), proj))) = doc.get(&ROOT, "project") {
            if let Some(n) = Self::get_str(doc, &proj, "name") {
                p.name = n;
            }
            p.fps = Self::get_f64(doc, &proj, "fps").unwrap_or(30.0);
            p.width = Self::get_i64(doc, &proj, "width").unwrap_or(1920) as u32;
            p.height = Self::get_i64(doc, &proj, "height").unwrap_or(1080) as u32;
            p.format_explicit = Self::get_bool(doc, &proj, "format_explicit");
            p.mark_in = Self::get_f64(doc, &proj, "mark_in");
            p.mark_out = Self::get_f64(doc, &proj, "mark_out");
            p.markers = Self::get_str(doc, &proj, "markers")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
        }

        if let Ok(Some((Value::Object(ObjType::Map), media))) = doc.get(&ROOT, "media") {
            for key in doc.keys(&media) {
                let Ok(id) = key.parse::<Uuid>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&media, key.as_str())
                else {
                    continue;
                };
                let name = Self::get_str(doc, &obj, "name").unwrap_or_default();
                let path = Self::get_str(doc, &obj, "path")
                    .unwrap_or_else(|| crate::media::workspace_path(&name));
                p.media.push(MediaItem {
                    id,
                    name,
                    kind: MediaKind::from_str(
                        &Self::get_str(doc, &obj, "kind").unwrap_or_default(),
                    ),
                    duration: Self::get_f64(doc, &obj, "duration").unwrap_or(0.0),
                    width: Self::get_i64(doc, &obj, "width").unwrap_or(0) as u32,
                    height: Self::get_i64(doc, &obj, "height").unwrap_or(0) as u32,
                    fps: Self::get_f64(doc, &obj, "fps").unwrap_or(30.0),
                    palette: Self::get_i64(doc, &obj, "palette").unwrap_or(0) as u8,
                    path,
                    hydration: crate::media::Hydration::from_str(
                        &Self::get_str(doc, &obj, "hydration").unwrap_or_default(),
                    ),
                    // Absent for every decoded source, and for any document
                    // written before titles existed.
                    text: Self::get_str(doc, &obj, "text")
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    // Absent in any document written before the mixer could
                    // find a video file's sound. False is the honest default:
                    // it reads as "no sound track was found".
                    has_audio: Self::get_bool(doc, &obj, "has_audio"),
                });
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), tracks))) = doc.get(&ROOT, "tracks") {
            for key in doc.keys(&tracks) {
                let Ok(id) = key.parse::<Uuid>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&tracks, key.as_str())
                else {
                    continue;
                };
                p.tracks.push(Track {
                    id,
                    name: Self::get_str(doc, &obj, "name").unwrap_or_default(),
                    kind: TrackKind::from_str(
                        &Self::get_str(doc, &obj, "kind").unwrap_or_default(),
                    ),
                    index: Self::get_i64(doc, &obj, "index").unwrap_or(0) as u32,
                    muted: Self::get_bool(doc, &obj, "muted"),
                    locked: Self::get_bool(doc, &obj, "locked"),
                    // Absent in documents written before solo existed, which
                    // `get_bool` reads as false — the right default.
                    solo: Self::get_bool(doc, &obj, "solo"),
                });
            }
        }

        if let Ok(Some((Value::Object(ObjType::Map), clips))) = doc.get(&ROOT, "clips") {
            for key in doc.keys(&clips) {
                let Ok(id) = key.parse::<Uuid>() else {
                    continue;
                };
                let Ok(Some((Value::Object(ObjType::Map), obj))) = doc.get(&clips, key.as_str())
                else {
                    continue;
                };
                let track = Self::get_str(doc, &obj, "track").and_then(|s| s.parse::<Uuid>().ok());
                let media = Self::get_str(doc, &obj, "media").and_then(|s| s.parse::<Uuid>().ok());
                let (Some(track), Some(media)) = (track, media) else {
                    continue;
                };
                p.clips.push(Clip {
                    id,
                    track,
                    media,
                    start: Self::get_f64(doc, &obj, "start").unwrap_or(0.0),
                    in_point: Self::get_f64(doc, &obj, "in").unwrap_or(0.0),
                    out_point: Self::get_f64(doc, &obj, "out").unwrap_or(0.0),
                    opacity: Self::get_f64(doc, &obj, "opacity").unwrap_or(1.0),
                    volume: Self::get_f64(doc, &obj, "volume").unwrap_or(1.0),
                    transition: Transition::from_str(
                        &Self::get_str(doc, &obj, "transition").unwrap_or_default(),
                    ),
                    palette: Self::get_i64(doc, &obj, "palette").unwrap_or(0) as u8,
                    filters: Filters {
                        brightness: Self::get_f64(doc, &obj, "brightness").unwrap_or(0.5),
                        contrast: Self::get_f64(doc, &obj, "contrast").unwrap_or(0.5),
                        saturation: Self::get_f64(doc, &obj, "saturation").unwrap_or(0.5),
                    },
                    fade_in: Self::get_f64(doc, &obj, "fade_in").unwrap_or(0.0),
                    fade_out: Self::get_f64(doc, &obj, "fade_out").unwrap_or(0.0),
                    // Absent before the field existed; 0.0 means "default"
                    // (see Clip::transition_len), not "instant cut".
                    transition_duration: Self::get_f64(doc, &obj, "transition_duration")
                        .unwrap_or(0.0),
                    effects: Self::get_str(doc, &obj, "effects")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    // Each of these is absent in a document written before it
                    // existed, and each default restores the old behaviour:
                    // identity geometry, normal speed, no key, no mask, no
                    // curves, no markers.
                    transform: Self::get_str(doc, &obj, "transform")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    speed: crate::model::Speed {
                        rate: Self::get_f64(doc, &obj, "speed_rate").unwrap_or(1.0),
                        reverse: Self::get_bool(doc, &obj, "speed_reverse"),
                    },
                    curves: Self::get_str(doc, &obj, "curves")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    key: Self::get_str(doc, &obj, "key")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    mask: Self::get_str(doc, &obj, "mask")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    anim: Self::get_str(doc, &obj, "anim")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    markers: Self::get_str(doc, &obj, "markers")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                    // Absent in every document written before links existed,
                    // which reads as "not linked" -- the independent clip that
                    // peer is already editing.
                    link: Self::get_str(doc, &obj, "link").and_then(|s| s.parse::<Uuid>().ok()),
                    // Absent before the sound could be split off, which reads
                    // as "this clip still carries its own" -- what that peer
                    // is already playing.
                    sound_split: Self::get_bool(doc, &obj, "sound_split"),
                    // Absent in any document written before unlinking kept
                    // the group. True there, because a clip that has a group
                    // at all was linked when it was written.
                    link_follows: Self::get_bool_or(doc, &obj, "link_follows", true),
                });
            }
        }

        Self::in_a_settled_order(&mut p);
        p
    }

    /// Put a freshly-loaded project's collections into an order that does not
    /// depend on how the document happened to be keyed.
    ///
    /// Automerge hands a map's entries back in key order, and every id here is
    /// a UUID minted when the project was made, so for a project seeded in
    /// this process that order is re-rolled on every run. Left as it arrived,
    /// one project loaded twice put its tracks, clips and media in two
    /// different orders -- and a timeline drawn in a different order is a
    /// different picture. That is what made two `--shots` captures of one
    /// scenario impossible to diff: same shapes, same count, shuffled.
    ///
    /// Ordering is by what the thing *is* rather than by its id: a track by
    /// the lane it occupies, a clip by where it sits on that lane, a source by
    /// its name. Two peers that agree about the edit therefore agree about the
    /// order, which an id-based sort could not promise.
    fn in_a_settled_order(p: &mut Project) {
        p.tracks
            .sort_by(|a, b| a.index.cmp(&b.index).then_with(|| a.name.cmp(&b.name)));
        let lane: std::collections::HashMap<Uuid, usize> = p
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id, i))
            .collect();
        p.clips.sort_by(|a, b| {
            lane.get(&a.track)
                .unwrap_or(&usize::MAX)
                .cmp(lane.get(&b.track).unwrap_or(&usize::MAX))
                .then_with(|| a.start.total_cmp(&b.start))
                .then_with(|| a.in_point.total_cmp(&b.in_point))
        });
        p.media.sort_by(|a, b| a.name.cmp(&b.name));
    }

    /// Build the demo project so the editor opens populated (and `--shots`
    /// renders a real edit, not an empty canvas), then seed it into `doc`.
    pub fn seed_sample(doc: &mut AutoCommit) {
        let p = sample_project();
        Self::write_project(doc, &p);
    }
}

/// A short documentary-style cut: drone B-roll under an interview, an
/// overlaid logo and a sunset insert with a dissolve, narration on A1 and
/// an ambient bed on A2. Built deterministically so screenshots are
/// stable across runs.
pub fn sample_project() -> Project {
    let mut p = Project::new("Coastline_Doc_v3");
    // A cut this far along has a format: it is 1080p at 30, and the next clip
    // to land on it must not re-shape the timeline it is already cut against.
    p.set_frame_size(1920, 1080);
    p.set_fps(30.0);

    let mk = |name: &str, kind: MediaKind, dur: f64, w: u32, h: u32, fps: f64, pal: u8| {
        MediaItem {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            // Sample media stands in for footage that was never probed.
            has_audio: false,
            duration: dur,
            width: w,
            height: h,
            fps,
            palette: pal,
            // Sample media lives under the workspace media prefix; the
            // bundled sample-media/ dir hydrates these for --shots.
            path: media::workspace_path(name),
            hydration: Hydration::Projected,
            text: None,
        }
    };

    let drone = mk(
        "Drone_Coastline.mov",
        MediaKind::Video,
        24.0,
        3840,
        2160,
        24.0,
        0,
    );
    let interview = mk(
        "Interview_A.mp4",
        MediaKind::Video,
        38.5,
        1920,
        1080,
        30.0,
        1,
    );
    let market = mk(
        "Cutaway_Market.mp4",
        MediaKind::Video,
        12.0,
        1920,
        1080,
        30.0,
        2,
    );
    let sunset = mk(
        "Sunset_Timelapse.mp4",
        MediaKind::Video,
        9.0,
        3840,
        2160,
        30.0,
        3,
    );
    let logo = mk("Logo_Bug.png", MediaKind::Image, 6.0, 800, 800, 30.0, 4);
    let vo = mk("VO_Take3.wav", MediaKind::Audio, 32.0, 0, 0, 0.0, 5);
    let pad = mk("Ambient_Pad.wav", MediaKind::Audio, 60.0, 0, 0, 0.0, 6);

    let (drone_id, interview_id, market_id, sunset_id, logo_id, vo_id, pad_id) = (
        drone.id,
        interview.id,
        market.id,
        sunset.id,
        logo.id,
        vo.id,
        pad.id,
    );
    p.media = vec![drone, interview, market, sunset, logo, vo, pad];

    let mk_track = |name: &str, kind: TrackKind, index: u32| Track {
        id: Uuid::new_v4(),
        name: name.into(),
        kind,
        index,
        muted: false,
        locked: false,
        solo: false,
    };
    let v1 = mk_track("V1", TrackKind::Video, 1);
    let v2 = mk_track("V2", TrackKind::Video, 2);
    let a1 = mk_track("A1", TrackKind::Audio, 1);
    let a2 = mk_track("A2", TrackKind::Audio, 2);
    let (v1id, v2id, a1id, a2id) = (v1.id, v2.id, a1.id, a2.id);
    p.tracks = vec![v1, v2, a1, a2];

    // Helper to push a placed, pre-trimmed clip with grade + fades.
    #[allow(clippy::too_many_arguments)]
    let mut place = |track: Uuid,
                     media: Uuid,
                     start: f64,
                     in_p: f64,
                     out_p: f64,
                     pal: u8,
                     opacity: f64,
                     volume: f64,
                     transition: Transition,
                     filters: Filters,
                     fade_in: f64,
                     fade_out: f64| {
        p.clips.push(Clip {
            id: Uuid::new_v4(),
            track,
            media,
            start,
            in_point: in_p,
            out_point: out_p,
            opacity,
            volume,
            transition,
            palette: pal,
            filters,
            fade_in,
            fade_out,
            transition_duration: 0.0,
            effects: Vec::new(),
            ..Default::default()
        });
    };

    let neutral = Filters::default();
    // A graded look on the drone open: brighter, punchier contrast.
    let graded = Filters {
        brightness: 0.62,
        contrast: 0.64,
        saturation: 0.58,
    };

    // V1 — the spine: drone open (graded + fade-in), interview body, market cutaway.
    place(
        v1id,
        drone_id,
        0.0,
        2.0,
        10.0,
        0,
        1.0,
        1.0,
        Transition::FadeIn,
        graded,
        1.5,
        0.0,
    );
    place(
        v1id,
        interview_id,
        8.0,
        5.0,
        17.0,
        1,
        1.0,
        1.0,
        Transition::CrossDissolve,
        neutral,
        0.0,
        0.0,
    );
    place(
        v1id,
        market_id,
        20.0,
        0.0,
        6.0,
        2,
        1.0,
        1.0,
        Transition::None,
        neutral,
        0.0,
        1.0,
    );
    // V2 — overlays: logo bug, then a sunset insert that dissolves in.
    place(
        v2id,
        logo_id,
        1.0,
        0.0,
        4.0,
        4,
        0.9,
        1.0,
        Transition::FadeIn,
        neutral,
        0.0,
        0.0,
    );
    place(
        v2id,
        sunset_id,
        21.0,
        0.0,
        6.0,
        3,
        0.85,
        1.0,
        Transition::CrossDissolve,
        neutral,
        0.0,
        1.2,
    );
    // A1 — narration aligned under the interview.
    place(
        a1id,
        vo_id,
        8.0,
        1.5,
        13.5,
        5,
        1.0,
        1.0,
        Transition::None,
        neutral,
        0.3,
        0.5,
    );
    // A2 — ambient bed under the whole piece, ducked low, gentle fades.
    place(
        a2id,
        pad_id,
        0.0,
        0.0,
        28.0,
        6,
        1.0,
        0.4,
        Transition::FadeIn,
        neutral,
        2.0,
        3.0,
    );

    // A couple of stacked effects on the drone open so the demo shows the
    // effects stack (warm grade, then a gentle vignette).
    if let Some(drone) = p.clips.first_mut() {
        drone.add_effect(EffectKind::Warm);
        drone.add_effect(EffectKind::Vignette);
    }

    p
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::model::{Speed, TextAlign, TextSource, Transform};
    use crate::param::{Interp, Param};

    /// Write `p` and read it straight back — the round-trip every peer
    /// makes, and the one an autosaved project makes on reopen.
    fn round_trip(p: &Project) -> Project {
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, p);
        VideoDoc::load_project(&doc)
    }

    /// A project with one clip, so a field can be set on it and chased
    /// through the document.
    fn one_clip() -> (Project, Uuid) {
        let mut p = Project::new("rt");
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
            hydration: Hydration::Local,
            ..Default::default()
        });
        let tid = Uuid::new_v4();
        p.tracks.push(crate::model::Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            ..Default::default()
        });
        let cid = p.add_clip(tid, mid, 0.0).expect("media exists");
        (p, cid)
    }

    #[test]
    fn a_key_and_a_mask_survive_the_document() {
        use crate::model::{ChromaKey, Mask, MaskShape};
        let (mut p, cid) = one_clip();
        let key = ChromaKey {
            enabled: true,
            color: [0.11, 0.68, 0.24],
            tolerance: 0.27,
            softness: 0.09,
            spill: 0.63,
        };
        let mask = Mask {
            shape: MaskShape::Ellipse,
            x: -0.18,
            y: 0.24,
            w: 0.41,
            h: 0.77,
            rotation: -22.0,
            feather: 0.31,
            invert: true,
        };
        {
            let c = p.clip_mut(cid).unwrap();
            c.key = key;
            c.mask = mask;
        }
        let back = round_trip(&p);
        let c = back.clip(cid).unwrap();
        assert_eq!(c.key, key, "every field of the key, not just the switch");
        assert_eq!(c.mask, mask, "and every field of the mask");
    }

    #[test]
    fn a_tone_curve_survives_the_document_with_every_channel() {
        use crate::curve::Curve;
        let (mut p, cid) = one_clip();
        let mut want = Curve::default();
        // One point on each channel, all at different heights, so a bake
        // that dropped or transposed a channel cannot come back equal.
        want.master.set_point(0.5, 0.72);
        want.red.set_point(0.3, 0.44);
        want.green.set_point(0.6, 0.21);
        want.blue.set_point(0.8, 0.93);
        want.mix = 0.65;
        p.clip_mut(cid).unwrap().curves = want.clone();

        let back = round_trip(&p);
        let c = back.clip(cid).unwrap();
        assert_eq!(
            c.curves, want,
            "every channel and the amount, not just the master"
        );
    }

    #[test]
    fn a_clip_written_before_curves_existed_reads_as_ungraded() {
        // The `curves` key is simply absent in such a document, and the
        // default has to be the straight line — a curve that read back as
        // anything else would re-grade footage nobody touched.
        let (p, cid) = one_clip();
        let back = round_trip(&p);
        let c = back.clip(cid).unwrap();
        assert!(
            c.curves.is_identity(),
            "an untouched clip comes back ungraded"
        );
    }

    #[test]
    fn a_transform_survives_the_document() {
        let (mut p, cid) = one_clip();
        let want = Transform {
            scale_x: 0.4,
            scale_y: 0.55,
            pos_x: -0.2,
            pos_y: 0.31,
            rotation: -14.5,
            anchor_x: 0.1,
            anchor_y: 0.9,
            crop_left: 0.05,
            crop_right: 0.06,
            crop_top: 0.07,
            crop_bottom: 0.08,
        };
        p.clip_mut(cid).unwrap().transform = want;
        let back = round_trip(&p);
        assert_eq!(back.clip(cid).unwrap().transform, want);
    }

    #[test]
    fn speed_survives_the_document_in_both_directions() {
        let (mut p, cid) = one_clip();
        p.clip_mut(cid).unwrap().speed = Speed::new(2.5, true);
        let back = round_trip(&p);
        let got = back.clip(cid).unwrap();
        assert_eq!(got.speed, Speed::new(2.5, true));
        assert_eq!(got.duration(), p.clip(cid).unwrap().duration());
    }

    #[test]
    fn keyframe_curves_survive_the_document_with_their_interpolation() {
        let (mut p, cid) = one_clip();
        {
            let c = p.clip_mut(cid).unwrap();
            c.set_key_at(Param::Opacity, 0.0, 0.0, Interp::Ease);
            c.set_key_at(Param::Opacity, 3.0, 1.0, Interp::Hold);
            c.set_key_at(Param::PosX, 1.0, -0.5, Interp::Linear);
        }
        let back = round_trip(&p);
        let got = back.clip(cid).unwrap();
        assert!(got.anim.is_animated(Param::Opacity));
        assert!(got.anim.is_animated(Param::PosX));
        let keys = &got.anim.track(Param::Opacity).unwrap().keys;
        assert_eq!(keys.len(), 2);
        assert_eq!(
            keys[0].interp,
            Interp::Ease,
            "the shape of the curve comes back too"
        );
        assert_eq!(keys[1].interp, Interp::Hold);
        // And the sampled value is identical, which is what actually matters.
        for t in [0.0, 0.7, 1.5, 2.9, 5.0] {
            assert_eq!(
                got.param_at(Param::Opacity, t),
                p.clip(cid).unwrap().param_at(Param::Opacity, t),
                "the curve reads the same at {t}s",
            );
        }
    }

    #[test]
    fn markers_survive_on_both_the_project_and_the_clip() {
        let (mut p, cid) = one_clip();
        p.add_marker(4.0, "act two", 3);
        p.clip_mut(cid)
            .unwrap()
            .add_marker(2.0, "beat", 5)
            .expect("inside");
        let back = round_trip(&p);
        assert_eq!(back.markers.len(), 1);
        assert_eq!(back.markers[0].name, "act two");
        assert_eq!(back.markers[0].palette, 3);
        let cm = &back.clip(cid).unwrap().markers;
        assert_eq!(cm.len(), 1);
        assert_eq!(cm[0].name, "beat");
        assert_eq!(cm[0].t, 2.0, "still clip-local");
    }

    #[test]
    fn a_title_survives_the_document_with_its_styling() {
        let mut p = Project::new("t");
        let mid = Uuid::new_v4();
        let text = TextSource {
            content: "Two\nlines".into(),
            size: 0.22,
            color: [0.9, 0.1, 0.4, 0.8],
            align: TextAlign::Right,
            font: "Some Family".into(),
            bold: false,
            italic: true,
            backdrop: 0.35,
        };
        p.media.push(MediaItem {
            id: mid,
            name: "Title".into(),
            kind: MediaKind::Text,
            duration: 5.0,
            text: Some(text.clone()),
            ..Default::default()
        });
        let back = round_trip(&p);
        let m = back.media(mid).expect("the title is in the bin");
        assert_eq!(m.kind, MediaKind::Text);
        assert_eq!(m.text.as_ref(), Some(&text));
    }

    #[test]
    fn clearing_a_title_replicates_as_a_change_rather_than_as_silence() {
        // Turning a title back into a plain reference has to *remove* the
        // text, or a peer that already has it would keep drawing it.
        let mut p = Project::new("t");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "Title".into(),
            kind: MediaKind::Text,
            text: Some(TextSource::new("gone soon")),
            ..Default::default()
        });
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, &p);
        assert!(
            VideoDoc::load_project(&doc)
                .media(mid)
                .unwrap()
                .text
                .is_some()
        );

        p.media[0].text = None;
        p.media[0].kind = MediaKind::Video;
        VideoDoc::upsert_media(&mut doc, &p.media[0]);
        assert_eq!(
            VideoDoc::load_project(&doc).media(mid).unwrap().text,
            None,
            "the title's text is gone from the document, not merely ignored",
        );
    }

    #[test]
    fn a_document_written_before_any_of_this_still_loads_unchanged() {
        // The compatibility contract: every new key is absent in an older
        // document, and every default is the behaviour that document had.
        let (p, cid) = one_clip();
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, &p);
        // Strip the new keys, leaving exactly what an older peer wrote.
        let clips = VideoDoc::map(&mut doc, "clips");
        let obj = VideoDoc::ensure_child(&mut doc, &clips, &cid.to_string());
        for key in [
            "transform",
            "speed_rate",
            "speed_reverse",
            "anim",
            "markers",
            "key",
            "mask",
        ] {
            let _ = doc.delete(&obj, key);
        }
        let media = VideoDoc::map(&mut doc, "media");
        let mobj = VideoDoc::ensure_child(&mut doc, &media, &p.media[0].id.to_string());
        let _ = doc.delete(&mobj, "text");
        let proj = VideoDoc::map(&mut doc, "project");
        let _ = doc.delete(&proj, "markers");

        let back = VideoDoc::load_project(&doc);
        let c = back.clip(cid).expect("the clip still loads");
        assert!(
            c.transform.is_identity(),
            "no geometry means the aspect-fit placement"
        );
        assert!(c.speed.is_normal(), "no speed means normal speed");
        assert!(c.anim.is_empty(), "no curves");
        assert!(!c.key.is_active(), "no key means nothing is keyed out");
        assert!(
            !c.mask.is_active(),
            "and no mask means the layer is not cut"
        );
        assert!(c.markers.is_empty());
        assert!(back.markers.is_empty());
        assert_eq!(back.media(p.media[0].id).unwrap().text, None);
        assert_eq!(
            c.duration(),
            p.clip(cid).unwrap().duration(),
            "and the same length"
        );
    }

    #[test]
    fn a_document_holding_only_imported_media_is_not_empty() {
        // Import writes the `media` map and nothing else. A document in that
        // state holds real user content, so it must never report empty — that
        // is what let a demo seed run over an imported clip.
        let mut doc = AutoCommit::new();
        assert!(VideoDoc::is_empty(&doc), "a fresh document is empty");
        VideoDoc::upsert_media(
            &mut doc,
            &MediaItem {
                id: Uuid::new_v4(),
                name: "Imported.mp4".into(),
                kind: MediaKind::Video,
                duration: 12.0,
                width: 1920,
                height: 1080,
                fps: 25.0,
                palette: 0,
                path: "media/Imported.mp4".into(),
                hydration: Hydration::Local,
                ..Default::default()
            },
        );
        assert!(
            !VideoDoc::is_empty(&doc),
            "a document with imported media is not empty"
        );
        // The project map is still absent — emptiness no longer keys on it alone.
        assert!(
            !matches!(
                doc.get(&ROOT, "project"),
                Ok(Some((Value::Object(ObjType::Map), _)))
            ),
            "import wrote no project map"
        );
    }

    #[test]
    fn an_untouched_map_is_not_content() {
        // `map()` materialises an empty map as a side effect of any read-modify
        // helper; an empty map is structure, not content, so the document is
        // still empty.
        let mut doc = AutoCommit::new();
        let _ = VideoDoc::map(&mut doc, "media");
        let _ = VideoDoc::map(&mut doc, "project");
        assert!(VideoDoc::is_empty(&doc), "empty maps carry no content");
    }

    #[test]
    fn sample_round_trips_through_doc() {
        let mut doc = AutoCommit::new();
        assert!(VideoDoc::is_empty(&doc));
        VideoDoc::seed_sample(&mut doc);
        assert!(!VideoDoc::is_empty(&doc));

        let p = VideoDoc::load_project(&doc);
        assert_eq!(p.name, "Coastline_Doc_v3");
        assert_eq!(p.media.len(), 7);
        assert_eq!(p.tracks.len(), 4);
        assert_eq!(p.clips.len(), 7);
        // A clip's source window survived the round-trip.
        let interview = p
            .media
            .iter()
            .find(|m| m.name == "Interview_A.mp4")
            .unwrap();
        let clip = p.clips.iter().find(|c| c.media == interview.id).unwrap();
        assert!((clip.duration() - 12.0).abs() < 1e-9);
        assert_eq!(clip.transition, Transition::CrossDissolve);
    }

    #[test]
    fn effects_stack_round_trips_through_the_doc() {
        use crate::model::{ClipEffect, EffectKind};
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut p = VideoDoc::load_project(&doc);
        let cid = p.clips[0].id;
        {
            let c = p.clip_mut(cid).unwrap();
            c.effects = vec![
                ClipEffect {
                    kind: EffectKind::Sepia,
                    amount: 0.6,
                },
                ClipEffect {
                    kind: EffectKind::Invert,
                    amount: 1.0,
                },
            ];
        }
        VideoDoc::upsert_clip(&mut doc, p.clip(cid).unwrap());
        let reload = VideoDoc::load_project(&doc);
        let effects = &reload.clip(cid).unwrap().effects;
        assert_eq!(
            effects.len(),
            2,
            "the effects stack survived the round-trip in order"
        );
        assert_eq!(effects[0].kind, EffectKind::Sepia);
        assert!((effects[0].amount - 0.6).abs() < 1e-9);
        assert_eq!(effects[1].kind, EffectKind::Invert);
    }

    #[test]
    fn upsert_clip_mutates_in_place() {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let mut p = VideoDoc::load_project(&doc);
        let cid = p.clips[0].id;
        if let Some(c) = p.clip_mut(cid) {
            c.start = 99.0;
        }
        VideoDoc::upsert_clip(&mut doc, p.clip(cid).unwrap());
        let reload = VideoDoc::load_project(&doc);
        assert_eq!(
            reload.clips.len(),
            p.clips.len(),
            "no duplicate clip created"
        );
        assert!((reload.clip(cid).unwrap().start - 99.0).abs() < 1e-9);
    }

    #[test]
    fn remove_clip_deletes_from_doc() {
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let p = VideoDoc::load_project(&doc);
        let cid = p.clips[0].id;
        VideoDoc::remove_clip(&mut doc, cid);
        let reload = VideoDoc::load_project(&doc);
        assert!(reload.clip(cid).is_none());
        assert_eq!(reload.clips.len(), 6);
    }

    #[test]
    fn replacing_a_project_removes_what_the_new_one_does_not_have() {
        // The difference between opening a project and merging one. Without
        // the removals a user who opens a second project gets both timelines
        // stacked on top of each other and no way to tell which is which.
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let before = VideoDoc::load_project(&doc);
        assert!(before.clips.len() > 1 && !before.media.is_empty());

        let mut other = Project::new("Other");
        let mid = uuid::Uuid::new_v4();
        other.media.push(MediaItem {
            id: mid,
            name: "Only.mp4".into(),
            kind: MediaKind::Video,
            duration: 5.0,
            ..Default::default()
        });
        let tid = uuid::Uuid::new_v4();
        other.tracks.push(Track {
            id: tid,
            name: "V1".into(),
            index: 1,
            ..Default::default()
        });
        let cid = uuid::Uuid::new_v4();
        other.clips.push(Clip {
            id: cid,
            track: tid,
            media: mid,
            out_point: 5.0,
            ..Default::default()
        });

        VideoDoc::replace_project(&mut doc, &other);
        let after = VideoDoc::load_project(&doc);

        assert_eq!(after.name, "Other");
        assert_eq!(after.media.len(), 1, "the old bin is gone, not merged");
        assert_eq!(after.tracks.len(), 1, "the old tracks are gone");
        assert_eq!(after.clips.len(), 1, "the old timeline is gone");
        assert_eq!(after.clips[0].id, cid);
        for old in &before.clips {
            assert!(
                after.clip(old.id).is_none(),
                "no clip of the old project survives"
            );
        }
    }

    #[test]
    fn reopening_the_same_project_keeps_its_ids() {
        // Replace is upsert-plus-remove, not delete-everything-and-recreate.
        // Ids are the CRDT keys, so re-minting them would turn "two peers
        // opened the same file" into two disjoint projects that merge into
        // duplicates rather than converging.
        let mut doc = AutoCommit::new();
        VideoDoc::seed_sample(&mut doc);
        let before = VideoDoc::load_project(&doc);

        VideoDoc::replace_project(&mut doc, &before);
        let after = VideoDoc::load_project(&doc);

        assert_eq!(after.clips.len(), before.clips.len());
        for c in &before.clips {
            let same = after.clip(c.id).expect("the same clip is still here");
            assert!((same.start - c.start).abs() < 1e-9);
            assert_eq!(same.media, c.media);
        }
    }

    #[test]
    fn two_peers_converge_on_a_moved_clip() {
        let mut a = AutoCommit::new();
        VideoDoc::seed_sample(&mut a);
        let mut b = AutoCommit::load(&a.save()).unwrap();

        // B moves a clip.
        let pb = VideoDoc::load_project(&b);
        let cid = pb.clips[0].id;
        let mut moved = pb.clip(cid).unwrap().clone();
        moved.start = 42.0;
        VideoDoc::upsert_clip(&mut b, &moved);

        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let pa = VideoDoc::load_project(&a);
        assert!((pa.clip(cid).unwrap().start - 42.0).abs() < 1e-9);
    }

    #[test]
    fn a_link_survives_the_document_and_breaking_it_replicates() {
        let (mut p, cid) = one_clip();
        let group = Uuid::new_v4();
        {
            let c = p.clip_mut(cid).unwrap();
            c.link = Some(group);
            c.sound_split = true;
        }
        let back = round_trip(&p);
        let c = back.clip(cid).expect("the clip");
        assert_eq!(c.link, Some(group), "the group id replicates");
        assert!(c.sound_split, "and so does where the sound comes from");

        // Unlinking has to *remove* the key, or a peer that already holds the
        // link would keep drawing the pair and keep moving both halves.
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, &p);
        let mut cut = p.clone();
        cut.unlink_clips(&[cid]);
        VideoDoc::upsert_clip(&mut doc, cut.clip(cid).expect("the clip"));
        // The claim is about the *peer*, not about which key carries it: a
        // reader of this document must stop following the link. It used to be
        // spelled "the link key is gone"; the group id now survives a break
        // so the editor can still find the sound a shot was split from, and
        // `link_follows` is what a peer reads instead.
        let seen = VideoDoc::load_project(&doc);
        assert!(
            !seen.is_linked(cid),
            "an unlink that writes nothing is an unlink a peer never sees",
        );
        assert!(
            seen.link_partners(cid).is_empty(),
            "a peer that still moves both halves has not seen the break",
        );
    }

    #[test]
    fn a_clip_written_before_links_existed_reads_as_an_ordinary_clip() {
        // The compatibility half: no `link` key and no `sound_split` key must
        // mean "not linked, and still carrying its own sound" -- which is what
        // that document's author is already playing.
        let (p, cid) = one_clip();
        let mut doc = AutoCommit::new();
        VideoDoc::write_project(&mut doc, &p);
        let clips = VideoDoc::map(&mut doc, "clips");
        let obj = VideoDoc::ensure_child(&mut doc, &clips, &cid.to_string());
        let _ = doc.delete(&obj, "link");
        let _ = doc.delete(&obj, "sound_split");
        let c = VideoDoc::load_project(&doc)
            .clip(cid)
            .expect("the clip")
            .clone();
        assert!(c.link.is_none());
        assert!(
            !c.sound_split,
            "an old project's picture clip has to keep carrying its sound",
        );
    }
}
