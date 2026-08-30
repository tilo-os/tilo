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
//! ├── "tracks"  (Map keyed by TrackId)  → { name,kind,index,muted,locked }
//! └── "clips"   (Map keyed by ClipId)   → { track,media,start,in,out,opacity,
//!                                            volume,transition,palette }
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
            _ =>
            {
                #[allow(
                    clippy::expect_used,
                    reason = "ROOT is always a map by automerge's own invariant, so put_object \
                              on it cannot fail"
                )]
                doc.put_object(&ROOT, key, ObjType::Map)
                    .expect("put_object map")
            }
        }
    }

    fn ensure_child(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ =>
            {
                #[allow(
                    clippy::expect_used,
                    reason = "parent was just confirmed to be a map by the match arm above, so \
                              put_object on it cannot fail"
                )]
                doc.put_object(parent, key, ObjType::Map)
                    .expect("put_object child")
            }
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

    /// True if no project has been seeded into this document yet.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        !matches!(
            doc.get(&ROOT, "project"),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    // --- writers ---------------------------------------------------------

    pub fn write_project_meta(doc: &mut AutoCommit, p: &Project) {
        let proj = Self::map(doc, "project");
        let _ = doc.put(&proj, "name", p.name.as_str());
        let _ = doc.put(&proj, "fps", p.fps);
        let _ = doc.put(&proj, "width", i64::from(p.width));
        let _ = doc.put(&proj, "height", i64::from(p.height));
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
    }

    pub fn upsert_track(doc: &mut AutoCommit, t: &Track) {
        let tracks = Self::map(doc, "tracks");
        let obj = Self::ensure_child(doc, &tracks, &t.id.to_string());
        let _ = doc.put(&obj, "name", t.name.as_str());
        let _ = doc.put(&obj, "kind", t.kind.as_str());
        let _ = doc.put(&obj, "index", i64::from(t.index));
        let _ = doc.put(&obj, "muted", t.muted);
        let _ = doc.put(&obj, "locked", t.locked);
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
        // The effects stack is an ordered list; store it as a compact JSON
        // string (the whole clip is written atomically by the action model, so
        // a scalar string is the simplest faithful representation).
        let _ = doc.put(
            &obj,
            "effects",
            serde_json::to_string(&c.effects).unwrap_or_else(|_| "[]".to_string()),
        );
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

    // --- reader ----------------------------------------------------------

    /// Project the whole document back into the in-memory model. Returns a
    /// default empty project if nothing has been seeded.
    #[allow(
        clippy::too_many_lines,
        reason = "one straight-line section per doc map (project, media, tracks, clips); \
                  splitting it would just move each section into a same-shaped helper"
    )]
    pub fn load_project(doc: &AutoCommit) -> Project {
        let mut p = Project::new("Untitled");
        if let Ok(Some((Value::Object(ObjType::Map), proj))) = doc.get(&ROOT, "project") {
            if let Some(n) = Self::get_str(doc, &proj, "name") {
                p.name = n;
            }
            p.fps = Self::get_f64(doc, &proj, "fps").unwrap_or(30.0);
            p.width = Self::get_i64(doc, &proj, "width").unwrap_or(1920) as u32;
            p.height = Self::get_i64(doc, &proj, "height").unwrap_or(1080) as u32;
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
                    kind: MediaKind::from_key(
                        &Self::get_str(doc, &obj, "kind").unwrap_or_default(),
                    ),
                    duration: Self::get_f64(doc, &obj, "duration").unwrap_or(0.0),
                    width: Self::get_i64(doc, &obj, "width").unwrap_or(0) as u32,
                    height: Self::get_i64(doc, &obj, "height").unwrap_or(0) as u32,
                    fps: Self::get_f64(doc, &obj, "fps").unwrap_or(30.0),
                    palette: Self::get_i64(doc, &obj, "palette").unwrap_or(0) as u8,
                    path,
                    hydration: crate::media::Hydration::from_key(
                        &Self::get_str(doc, &obj, "hydration").unwrap_or_default(),
                    ),
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
                    kind: TrackKind::from_key(
                        &Self::get_str(doc, &obj, "kind").unwrap_or_default(),
                    ),
                    index: Self::get_i64(doc, &obj, "index").unwrap_or(0) as u32,
                    muted: Self::get_bool(doc, &obj, "muted"),
                    locked: Self::get_bool(doc, &obj, "locked"),
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
                    transition: Transition::from_key(
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
                    effects: Self::get_str(doc, &obj, "effects")
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                });
            }
        }

        p
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
#[allow(
    clippy::too_many_lines,
    reason = "flat, deterministic literal data for the demo cut (media items, tracks, clips) \
              with no branching; splitting it would only chop one literal list into several"
)]
pub fn sample_project() -> Project {
    let mut p = Project::new("Coastline_Doc_v3");
    p.fps = 30.0;
    p.width = 1920;
    p.height = 1080;

    let mk = |name: &str, kind: MediaKind, dur: f64, w: u32, h: u32, fps: f64, pal: u8| {
        MediaItem {
            id: Uuid::new_v4(),
            name: name.into(),
            kind,
            duration: dur,
            width: w,
            height: h,
            fps,
            palette: pal,
            // Sample media lives under the workspace media prefix; the
            // bundled sample-media/ dir hydrates these for --shots.
            path: media::workspace_path(name),
            hydration: Hydration::Projected,
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
            effects: Vec::new(),
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
}
