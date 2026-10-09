#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "test assertions over exact values written and read back"
)]

//! The project's verbs through the door, the edits bound to the playhead,
//! and the project as it is stored and replicated.

use ui::framework::ResourceSurface;
use ui::tilo_actions::OperationKind;
use ui::tilo_resource::{Replica, Resource};

use super::*;
use crate::actions::{VideoEditorEffect, VideoResourceCtx, surface_declarations};
use crate::app::VideoEditorApp;
use crate::media::Hydration;
use crate::model::{
    ChromaKey, Clip, ClipEffect, MAX_GAIN_DB, Mask, MaskShape, Speed, TextAlign, Transform,
    Transition,
};
use crate::param::Interp;

fn id(uuid: Uuid) -> Id {
    Id::from_raw(uuid.to_string())
}

/// A replica holding `project`, opened the way a project file is.
fn holding(project: &Project) -> Replica<Project> {
    let mut replica = Replica::<Project>::new();
    replica
        .apply(Invoke::edit(&load_project(project)).unwrap())
        .unwrap();
    replica
}

/// A project with one 30 s video clip at 0 on V1.
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
        has_audio: true,
        path: "media/v.mp4".into(),
        hydration: Hydration::Local,
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
    p.clip_mut(cid).unwrap().out_point = 30.0;
    (p, cid)
}

/// Write `p` and read it straight back — the round trip every peer makes.
fn round_trip(p: &Project) -> Project {
    Project::read(&crate::seed::document_of(p)).unwrap()
}

// ---- What the project offers -------------------------------------------

#[test]
fn the_project_publishes_its_verbs_and_the_editor_binds_the_playhead_edits() {
    let published = Replica::<Project>::new().published().unwrap();
    assert_eq!(published.kind, "video-editor.project");
    let ids: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
    for expected in [
        "add-marker",
        "set-frame-size",
        "set-frame-rate",
        "add-track",
        "add-title",
        "field-edit",
        "group",
    ] {
        assert!(
            ids.contains(&expected),
            "`{expected}` is published: {ids:?}"
        );
    }
    let add_marker = published
        .actions
        .iter()
        .find(|action| action.id == "add-marker")
        .unwrap();
    assert_eq!(add_marker.kind, OperationKind::Document);

    // The editor binds the time verbs to its playhead.
    let mut app = VideoEditorApp::new();
    let sample = crate::seed::sample_project();
    let clip = sample.clips.first().unwrap().clone();
    let at = clip.start + clip.duration() * 0.5;
    app.seed_unpublished_project_for_test(sample.clone());
    app.set_playhead_for_test(at);
    let bound = ResourceSurface::<Project, VideoEditorEffect>::bound_declarations(&app);
    let ids: Vec<&str> = bound.iter().map(|decl| decl.id).collect();
    for expected in [
        "mark-in-here",
        "mark-out-here",
        "add-marker-here",
        "split-here",
        "trim-in-here",
        "trim-out-here",
    ] {
        assert!(ids.contains(&expected), "`{expected}` is bound: {ids:?}");
    }
    let split = bound.iter().find(|decl| decl.id == "split-here").unwrap();
    assert_eq!(
        split
            .params
            .iter()
            .map(|p| p.name.to_string())
            .collect::<Vec<_>>(),
        ["clip"],
        "the time is the playhead's; only the clip is asked for"
    );
    let build = |clip_id: Uuid| {
        let args = tilo_actions::ActionArgs::bind_json(
            &split.params,
            &serde_json::Map::from_iter([(
                "clip".to_owned(),
                serde_json::json!(clip_id.to_string()),
            )]),
        )
        .unwrap();
        (split.build)(&args, 0).unwrap()
    };
    let mut replica = holding(&sample);
    let clips = sample.clips.len();
    replica.apply_action(&build(clip.id)).unwrap();
    assert!(
        replica.state().unwrap().clips.len() > clips,
        "splitting the clip under the playhead cuts it"
    );
    let elsewhere = sample
        .clips
        .iter()
        .find(|c| !c.covers(at))
        .expect("a clip the playhead is not over");
    assert!(
        replica.apply_action(&build(elsewhere.id)).is_err(),
        "a clip the playhead does not cover is refused"
    );
}

#[test]
fn edits_bound_to_the_playhead_follow_it_and_the_markers_near_it() {
    let (mut p, _) = one_clip();
    let marker = p.add_marker(4.0, "Act two", 2);
    let ctx = |project: &Project, playhead: f64| VideoResourceCtx {
        project: project.clone(),
        playhead,
        mix: crate::audio::MixSnapshot::default(),
    };
    let near = surface_declarations(&ctx(&p, 4.1));
    let remove = near
        .iter()
        .find(|decl| decl.id == "remove-marker-here")
        .expect("a marker under the playhead can be removed there");
    assert!(remove.label.contains("Act two"), "{}", remove.label);
    let action = (remove.build)(&tilo_actions::ActionArgs::default(), 0).unwrap();
    let mut replica = holding(&p);
    replica.apply_action(&action).unwrap();
    assert!(replica.state().unwrap().marker(marker).is_none());

    let far = surface_declarations(&ctx(&p, 9.0));
    assert!(!far.iter().any(|decl| decl.id == "remove-marker-here"));
    assert!(
        !far.iter().any(|decl| decl.id == "trim-mix"),
        "nothing was measured over, so there is nothing to trim"
    );

    let here = far
        .iter()
        .find(|decl| decl.id == "add-marker-here")
        .unwrap();
    let args = tilo_actions::ActionArgs::bind_json(
        &here.params,
        &serde_json::Map::from_iter([("name".to_owned(), serde_json::json!("Chorus"))]),
    )
    .unwrap();
    let mut replica = holding(&p);
    replica
        .apply_action(&(here.build)(&args, 0).unwrap())
        .unwrap();
    let state = replica.state().unwrap();
    let chorus = state.markers.iter().find(|m| m.name == "Chorus").unwrap();
    assert_eq!(chorus.t, 9.0, "at the playhead");
    let mark_in = far.iter().find(|decl| decl.id == "mark-in-here").unwrap();
    replica
        .apply_action(&(mark_in.build)(&tilo_actions::ActionArgs::default(), 0).unwrap())
        .unwrap();
    assert_eq!(replica.state().unwrap().mark_in, Some(9.0));
}

// ---- Verbs ---------------------------------------------------------------

#[test]
fn speed_markers_titles_and_the_frame_change_through_their_verbs() {
    let (p, cid) = one_clip();
    let mut replica = holding(&p);
    replica.apply(Project::RETIME.with((id(cid), 2.0))).unwrap();
    replica.apply(Project::REVERSE.with(id(cid))).unwrap();
    let clip = replica.state().unwrap().clip(cid).unwrap().clone();
    assert_eq!(clip.speed, Speed::new(2.0, true));
    assert!(
        (clip.duration() - 15.0).abs() < 1e-9,
        "twice as fast is half as long"
    );
    assert!(replica.apply(Project::RETIME.with((id(cid), 0.0))).is_err());
    replica.apply(Project::NORMAL_SPEED.with(id(cid))).unwrap();
    assert!(
        replica
            .state()
            .unwrap()
            .clip(cid)
            .unwrap()
            .speed
            .is_normal()
    );

    // A marker, and its removal (which is confirmed).
    replica
        .apply(Project::PLACE_MARKER.with((12.5, "  ".to_owned())))
        .unwrap();
    let marker = replica.state().unwrap().markers[0].clone();
    assert_eq!((marker.t, marker.name.as_str()), (12.5, "Marker"));
    assert!(
        replica
            .apply(Project::PLACE_MARKER.with((-1.0, "Before time".to_owned())))
            .is_err()
    );
    assert!(
        replica
            .apply(Project::DELETE_MARKER.with(id(marker.id)))
            .is_err(),
        "removing waits for confirmation"
    );
    replica
        .apply_confirmed(Project::DELETE_MARKER.with(id(marker.id)))
        .unwrap();
    assert!(replica.state().unwrap().markers.is_empty());

    // A title lands in the bin as a text source.
    replica
        .apply(Project::ADD_TITLE.with("Opening\nsecond line".to_owned()))
        .unwrap();
    let state = replica.state().unwrap();
    let title = state
        .media
        .iter()
        .find(|m| m.kind == MediaKind::Text)
        .unwrap();
    assert_eq!(title.name, "Opening");
    assert_eq!(title.text.as_ref().unwrap().content, "Opening\nsecond line");

    // The frame is clamped and rounded rather than stored raw.
    replica
        .apply(Project::RESIZE_FRAME.with((99_999, 721)))
        .unwrap();
    replica.apply(Project::RETIME_FRAMES.with(1000.0)).unwrap();
    let state = replica.state().unwrap();
    assert_eq!(
        (state.width, state.height),
        (8192, 720),
        "clamped, and rounded to even"
    );
    assert_eq!(state.fps, 240.0);
    replica
        .apply(Project::ADOPT_MEDIA_FORMAT.with(id(p.media[0].id)))
        .unwrap();
    let state = replica.state().unwrap();
    assert_eq!((state.width, state.height, state.fps), (1920, 1080, 30.0));

    // The render range.
    replica
        .apply(Project::RENDER_RANGE.with((2.0, 8.0)))
        .unwrap();
    let state = replica.state().unwrap();
    assert_eq!((state.mark_in, state.mark_out), (Some(2.0), Some(8.0)));
    assert!(
        replica
            .apply(Project::RENDER_RANGE.with((8.0, 2.0)))
            .is_err()
    );
    replica.apply(Project::CLEAR_RENDER_RANGE.with(())).unwrap();
    let state = replica.state().unwrap();
    assert_eq!((state.mark_in, state.mark_out), (None, None));
}

#[test]
fn a_value_is_keyed_at_the_time_named_without_moving_the_rest_of_the_curve() {
    let (mut p, cid) = one_clip();
    let mut replica = holding(&p);
    // Not animated: the constant changes.
    replica
        .apply(Project::SET_PARAM.with((id(cid), Param::Opacity, 3.0, 0.4)))
        .unwrap();
    let clip = replica.state().unwrap().clip(cid).unwrap().clone();
    assert_eq!(clip.param_at(Param::Opacity, 0.0), 0.4);
    assert!(!clip.anim.is_animated(Param::Opacity));

    // Animated: a key at that time, the others where they were.
    {
        let c = p.clip_mut(cid).unwrap();
        c.set_key_at(Param::PosX, 0.0, 0.0, Interp::Linear);
        c.set_key_at(Param::PosX, 10.0, 1.0, Interp::Linear);
    }
    let mut replica = holding(&p);
    replica
        .apply(Project::SET_PARAM.with((id(cid), Param::PosX, 5.0, -0.5)))
        .unwrap();
    let clip = replica.state().unwrap().clip(cid).unwrap().clone();
    assert_eq!(clip.param_at(Param::PosX, 5.0), -0.5);
    assert_eq!(clip.param_at(Param::PosX, 0.0), 0.0);
    assert_eq!(clip.param_at(Param::PosX, 10.0), 1.0);
    assert!(
        replica
            .apply(Project::SET_PARAM.with((id(cid), Param::PosX, 5.0, f64::NAN)))
            .is_err()
    );
}

#[test]
fn deleting_clips_sources_and_tracks_takes_what_hangs_off_them() {
    let sample = crate::seed::sample_project();
    let lone = sample
        .clips
        .iter()
        .find(|c| sample.link_partners(c.id).is_empty())
        .unwrap()
        .clone();

    // A delete waits for confirmation, then leaves a gap.
    let mut replica = holding(&sample);
    assert!(
        replica
            .apply(Project::REMOVE_CLIP.with(id(lone.id)))
            .is_err()
    );
    replica
        .apply_confirmed(Project::REMOVE_CLIP.with(id(lone.id)))
        .unwrap();
    let after = replica.state().unwrap();
    assert!(after.clip(lone.id).is_none());
    assert_eq!(after.clips.len(), sample.clips.len() - 1);

    // A ripple delete closes it: later clips on the lane move left by the
    // clip's length, clips on other lanes stay.
    let mut replica = holding(&sample);
    replica
        .apply_confirmed(Project::CLOSE_GAP.with(id(lone.id)))
        .unwrap();
    let after = replica.state().unwrap();
    for clip in &sample.clips {
        let Some(now) = after.clip(clip.id) else {
            assert_eq!(clip.id, lone.id);
            continue;
        };
        if clip.track == lone.track && clip.start > lone.start {
            assert!((now.start - (clip.start - lone.duration()).max(0.0)).abs() < 1e-9);
        } else {
            assert_eq!(now.start, clip.start, "{} stayed", clip.id);
        }
    }

    // A source takes every clip of it.
    let placed = lone.media;
    let mut replica = holding(&sample);
    replica
        .apply_confirmed(Project::REMOVE_MEDIA.with(id(placed)))
        .unwrap();
    let after = replica.state().unwrap();
    assert!(after.media(placed).is_none());
    assert!(after.clips.iter().all(|c| c.media != placed));

    // A track takes its clips.
    let mut replica = holding(&sample);
    replica
        .apply_confirmed(Project::DELETE_TRACK.with(id(lone.track)))
        .unwrap();
    let after = replica.state().unwrap();
    assert!(after.track(lone.track).is_none());
    assert!(after.clips.iter().all(|c| c.track != lone.track));

    // A locked track's clips are not edited.
    let mut locked = sample;
    locked
        .tracks
        .iter_mut()
        .find(|t| t.id == lone.track)
        .unwrap()
        .locked = true;
    let mut replica = holding(&locked);
    assert!(
        replica
            .apply_confirmed(Project::REMOVE_CLIP.with(id(lone.id)))
            .is_err()
    );
    // And a name that is no clip is refused, not guessed at.
    assert!(
        replica
            .apply_confirmed(Project::REMOVE_CLIP.with(Id::from_raw("nope")))
            .is_err()
    );
}

#[test]
fn an_effect_is_added_moved_set_and_removed_by_its_own_id() {
    let (p, cid) = one_clip();
    let mut replica = holding(&p);
    replica
        .apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Sepia)))
        .unwrap();
    replica
        .apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Sepia)))
        .unwrap();
    replica
        .apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Invert)))
        .unwrap();
    let effects = replica.state().unwrap().clip(cid).unwrap().effects.clone();
    let kinds: Vec<EffectKind> = effects.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        [EffectKind::Sepia, EffectKind::Sepia, EffectKind::Invert],
        "appended in order"
    );
    assert_ne!(effects[0].id, effects[1].id, "two Sepias are two instances");

    // The second Sepia moves to the bottom and dims; the first is untouched.
    let second = effects[1].id;
    replica
        .apply(Project::MOVE_EFFECT.with((id(cid), id(second), false)))
        .unwrap();
    replica
        .apply(Project::SET_EFFECT_AMOUNT.with((id(cid), id(second), 0.25)))
        .unwrap();
    let now = replica.state().unwrap().clip(cid).unwrap().effects.clone();
    assert_eq!(
        now.iter().map(|e| e.id).collect::<Vec<_>>(),
        [effects[0].id, effects[2].id, second]
    );
    assert_eq!(now[2].amount, 0.25);
    assert_eq!(now[0].amount, effects[0].amount);
    assert!(
        replica
            .apply(Project::SET_EFFECT_AMOUNT.with((id(cid), id(second), 1.5)))
            .is_err()
    );
    assert!(
        replica
            .apply(Project::MOVE_EFFECT.with((id(cid), id(second), false)))
            .is_err(),
        "the bottom one cannot move further down"
    );

    replica
        .apply_confirmed(Project::REMOVE_EFFECT.with((id(cid), id(second))))
        .unwrap();
    assert!(
        replica
            .apply_confirmed(Project::REMOVE_EFFECT.with((id(cid), id(second))))
            .is_err(),
        "a stale id removes nothing, not its neighbour"
    );
    let now = replica.state().unwrap().clip(cid).unwrap().effects.clone();
    assert_eq!(
        now.iter().map(|e| e.id).collect::<Vec<_>>(),
        [effects[0].id, effects[2].id]
    );
    replica
        .apply_confirmed(Project::CLEAR_EFFECTS.with(id(cid)))
        .unwrap();
    assert!(
        replica
            .state()
            .unwrap()
            .clip(cid)
            .unwrap()
            .effects
            .is_empty()
    );
}

#[test]
fn linked_edits_carry_both_halves_of_a_shot() {
    let (p, cid) = one_clip();
    let mut replica = holding(&p);
    replica.apply(Project::SPLIT_AUDIO.with(id(cid))).unwrap();
    let state = replica.state().unwrap();
    let partners = state.link_partners(cid);
    assert_eq!(partners.len(), 1, "the sound is a clip of its own, linked");
    let sound = partners[0];
    assert!(state.clip(cid).unwrap().sound_split);
    assert!(
        state
            .track(state.clip(sound).unwrap().track)
            .is_some_and(|t| t.kind == TrackKind::Audio)
    );
    assert!(
        replica.apply(Project::SPLIT_AUDIO.with(id(cid))).is_err(),
        "a shot whose sound is split has none left to split"
    );

    // Trimming, cutting and re-timing one half takes the other along.
    replica
        .apply(Project::TRIM_TAIL.with((id(cid), 20.0)))
        .unwrap();
    replica
        .apply(Project::TRIM_HEAD.with((id(sound), 2.0)))
        .unwrap();
    let state = replica.state().unwrap();
    for half in [cid, sound] {
        let clip = state.clip(half).unwrap();
        assert!((clip.start - 2.0).abs() < 1e-9, "{half} starts at 2");
        assert!((clip.end() - 20.0).abs() < 1e-9, "{half} ends at 20");
    }
    replica
        .apply(Project::RETIME.with((id(sound), 2.0)))
        .unwrap();
    let state = replica.state().unwrap();
    assert_eq!(state.clip(cid).unwrap().speed.rate(), 2.0);
    replica.apply(Project::CUT.with((id(cid), 5.0))).unwrap();
    let state = replica.state().unwrap();
    assert_eq!(state.clips.len(), 4, "both halves were cut");
    let tails: Vec<&Clip> = state.clips.iter().filter(|c| c.start > 4.0).collect();
    assert_eq!(tails.len(), 2);
    assert!(
        state.link_partners(tails[0].id).contains(&tails[1].id),
        "the tails are a shot of their own"
    );
    assert!(
        !state.link_partners(cid).contains(&tails[0].id),
        "and not linked to the heads"
    );

    // Unlinked, and linked back.
    replica.apply(Project::UNLINK.with(id(cid))).unwrap();
    assert!(!replica.state().unwrap().is_linked(cid));
    assert!(replica.apply(Project::UNLINK.with(id(cid))).is_err());
    replica
        .apply(Project::LINK_WITH.with((id(cid), id(sound))))
        .unwrap();
    assert!(replica.state().unwrap().link_partners(cid).contains(&sound));
    assert!(
        replica
            .apply(Project::LINK_WITH.with((id(cid), id(cid))))
            .is_err()
    );
}

/// A project with two audio lanes, and the mix measured `over_db` over.
fn clipped_mix(over_db: f64) -> (Project, crate::audio::MixSnapshot, Vec<Uuid>) {
    let (mut p, _) = one_clip();
    let mid = Uuid::new_v4();
    p.media.push(MediaItem {
        id: mid,
        name: "a.wav".into(),
        kind: MediaKind::Audio,
        duration: 30.0,
        path: "media/a.wav".into(),
        hydration: Hydration::Local,
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
        p.add_clip(tid, mid, 0.0).unwrap();
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
    (p, meters.snapshot(), lanes)
}

#[test]
fn the_mix_is_trimmed_by_what_was_measured_and_lanes_stay_in_range() {
    let (mut p, mix, lanes) = clipped_mix(4.0);
    p.tracks
        .iter_mut()
        .find(|t| t.id == lanes[0])
        .unwrap()
        .gain_db = 2.0;
    p.tracks
        .iter_mut()
        .find(|t| t.id == lanes[1])
        .unwrap()
        .gain_db = -5.0;
    let ctx = VideoResourceCtx {
        project: p.clone(),
        playhead: 0.0,
        mix,
    };
    let decls = surface_declarations(&ctx);
    let trim = decls
        .iter()
        .find(|decl| decl.id == "trim-mix")
        .expect("a measured overload is what makes the trim meaningful");
    let words = trim.description.clone().unwrap_or_default();
    assert!(words.contains("+4.0 dB over"), "how far over: {words}");
    assert!(words.contains("1 place"), "and in how many places: {words}");
    let run = |headroom: f64| {
        let args = tilo_actions::ActionArgs::bind_json(
            &trim.params,
            &serde_json::Map::from_iter([("headroom".to_owned(), serde_json::json!(headroom))]),
        )
        .unwrap();
        let mut replica = holding(&p);
        replica
            .apply_action(&(trim.build)(&args, 0).unwrap())
            .unwrap();
        let state = replica.state().unwrap();
        (
            state.track(lanes[0]).unwrap().gain_db,
            state.track(lanes[1]).unwrap().gain_db,
        )
    };
    let over = ctx.mix.worst_over_db();
    assert!((over - 4.0).abs() < 1e-6);
    let (a1, a2) = run(0.0);
    assert!(
        (a1 - (2.0 - over)).abs() < 1e-6,
        "A1 came down by the overshoot: {a1}"
    );
    assert!(
        (a2 - (-5.0 - over)).abs() < 1e-6,
        "and A2 by the same: {a2}"
    );
    let (spare, _) = run(3.0);
    assert!((a1 - spare - 3.0).abs() < 1e-6, "headroom is read");
    let (none, _) = run(-6.0);
    assert!((none - a1).abs() < 1e-6, "headroom below zero is none");

    // A lane's level and pan are fields; out of range is refused, and
    // `reset-mix` puts both back in one step.
    let a1 = lanes[0];
    let mut replica = holding(&p);
    let lane = Project::paths().tracks.item(&id(a1));
    replica
        .apply(Invoke::edit(&lane.gain_db.set(-6.0)).unwrap())
        .unwrap();
    replica
        .apply(Invoke::edit(&lane.pan.set(-1.0)).unwrap())
        .unwrap();
    let t = replica.state().unwrap().track(a1).unwrap().clone();
    assert_eq!((t.gain_db, t.pan), (-6.0, -1.0));
    assert!(
        replica
            .apply(Invoke::edit(&lane.gain_db.set(MAX_GAIN_DB + 1.0)).unwrap())
            .is_err()
    );
    assert!(
        replica
            .apply(Invoke::edit(&lane.pan.set(-9.0)).unwrap())
            .is_err()
    );
    replica.apply(Project::RESET_MIX.with(id(a1))).unwrap();
    let t = replica.state().unwrap().track(a1).unwrap().clone();
    assert_eq!((t.gain_db, t.pan), (0.0, 0.0));
}

// ---- Replay and convergence ---------------------------------------------

#[test]
fn verbs_replay_to_the_same_document_on_a_peer() {
    let sample = crate::seed::sample_project();
    let mut origin = Replica::<Project>::new();
    let mut peer = origin.fork();
    origin
        .apply(Invoke::edit(&load_project(&sample)).unwrap())
        .unwrap();
    let clip = sample.clips[0].id;
    let track = sample.tracks[0].id;
    let media = sample.media[0].id;
    origin
        .apply(Project::ADD_EFFECT.with((id(clip), EffectKind::Vignette)))
        .unwrap();
    origin
        .apply(Project::ADD_TRACK.with(TrackKind::Audio))
        .unwrap();
    origin
        .apply(Project::PLACE_CLIP.with((id(media), id(track), 200.0)))
        .unwrap();
    origin
        .apply(Project::ADD_TITLE.with("Credits".to_owned()))
        .unwrap();
    origin
        .apply(Project::PLACE_MARKER.with((3.0, "Beat".to_owned())))
        .unwrap();
    for envelope in origin.log().to_vec() {
        peer.replay(&envelope).unwrap();
    }
    assert_eq!(origin.heads(), peer.heads());
    let (a, b) = (origin.state().unwrap(), peer.state().unwrap());
    assert_eq!(a, b, "ids minted inside the verbs replay identically");
    assert_eq!(a.tracks.len(), sample.tracks.len() + 1);
    assert_eq!(a.clips.len(), sample.clips.len() + 1);
}

#[test]
fn concurrent_effect_and_marker_edits_keep_every_authored_item() {
    let (p, cid) = one_clip();
    let mut a = holding(&p);
    a.apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Grayscale)))
        .unwrap();
    a.apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Warm)))
        .unwrap();
    let mut b = a.fork();
    let base = a.state().unwrap().clip(cid).unwrap().effects.clone();

    a.apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Sepia)))
        .unwrap();
    a.apply(Project::PLACE_MARKER.with((2.0, "Ada cut".to_owned())))
        .unwrap();
    // Ada moves the first effect down while Bo dims it and adds his own.
    a.apply(Project::MOVE_EFFECT.with((id(cid), id(base[0].id), false)))
        .unwrap();
    b.apply(Project::SET_EFFECT_AMOUNT.with((id(cid), id(base[0].id), 0.3)))
        .unwrap();
    b.apply(Project::ADD_EFFECT.with((id(cid), EffectKind::Vignette)))
        .unwrap();
    b.apply(Project::PLACE_MARKER.with((5.0, "Bo beat".to_owned())))
        .unwrap();

    a.sync_with(&mut b).unwrap();
    let (left, right) = (a.state().unwrap(), b.state().unwrap());
    assert_eq!(left, right, "both peers hold one stable stack");
    let effects = &left.clip(cid).unwrap().effects;
    let kinds: Vec<EffectKind> = effects.iter().map(|e| e.kind).collect();
    for kind in [
        EffectKind::Grayscale,
        EffectKind::Warm,
        EffectKind::Sepia,
        EffectKind::Vignette,
    ] {
        assert_eq!(
            kinds.iter().filter(|k| **k == kind).count(),
            1,
            "{kind:?} once: {kinds:?}"
        );
    }
    let moved = effects.iter().find(|e| e.id == base[0].id).unwrap();
    assert_eq!(moved.amount, 0.3, "the moved effect kept Bo's change");
    assert!(
        effects.iter().position(|e| e.id == base[0].id)
            > effects.iter().position(|e| e.id == base[1].id),
        "and Ada's move"
    );
    let names: Vec<&str> = left.markers.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["Ada cut", "Bo beat"], "markers sort by time");

    // A delete wins over a change made to what it deleted.
    let mut c = a.fork();
    a.apply_confirmed(Project::REMOVE_EFFECT.with((id(cid), id(base[1].id))))
        .unwrap();
    c.apply(Project::SET_EFFECT_AMOUNT.with((id(cid), id(base[1].id), 0.9)))
        .unwrap();
    a.sync_with(&mut c).unwrap();
    assert!(
        a.state()
            .unwrap()
            .clip(cid)
            .unwrap()
            .effects
            .iter()
            .all(|e| e.id != base[1].id),
        "the removed effect is not revived by a late change to it"
    );
    assert_eq!(a.state().unwrap(), c.state().unwrap());
}

#[test]
fn two_peers_converge_on_a_moved_clip_and_its_neighbours() {
    let sample = crate::seed::sample_project();
    let mut a = holding(&sample);
    let mut b = a.fork();
    let first = sample.clips[0].id;
    let second = sample.clips[1].id;
    let lane = Project::paths().clips;
    b.apply(Invoke::edit(&lane.item(&id(first)).start.set(42.0)).unwrap())
        .unwrap();
    a.apply(Invoke::edit(&lane.item(&id(second)).opacity.set(0.5)).unwrap())
        .unwrap();
    a.sync_with(&mut b).unwrap();
    let merged = a.state().unwrap();
    assert_eq!(merged, b.state().unwrap());
    assert_eq!(merged.clip(first).unwrap().start, 42.0);
    assert_eq!(merged.clip(second).unwrap().opacity, 0.5);
    let starts: Vec<f64> = merged.clips.iter().map(|c| c.start).collect();
    assert!(
        starts.windows(2).all(|w| w[0] <= w[1]),
        "clips sort by where they sit: {starts:?}"
    );
}

// ---- The project as stored ----------------------------------------------

#[test]
fn replacing_the_project_removes_what_the_new_one_lacks_and_keeps_shared_ids() {
    let sample = crate::seed::sample_project();
    let mut replica = holding(&sample);
    let mut other = Project::new("Other");
    let mid = Uuid::new_v4();
    other.media.push(MediaItem {
        id: mid,
        name: "Only.mp4".into(),
        kind: MediaKind::Video,
        duration: 5.0,
        ..Default::default()
    });
    let tid = Uuid::new_v4();
    other.tracks.push(Track {
        id: tid,
        name: "V1".into(),
        index: 1,
        ..Default::default()
    });
    let cid = Uuid::new_v4();
    other.clips.push(Clip {
        id: cid,
        track: tid,
        media: mid,
        out_point: 5.0,
        ..Default::default()
    });
    replica
        .apply(Invoke::edit(&load_project(&other)).unwrap())
        .unwrap();
    let after = replica.state().unwrap();
    assert_eq!(after.name, "Other");
    assert_eq!(after.media.len(), 1, "the old bin is gone, not merged");
    assert_eq!(after.tracks.len(), 1);
    assert_eq!(after.clips.len(), 1);
    assert_eq!(after.clips[0].id, cid);

    // Reopening the same project changes nothing: the ids are the keys, so
    // two peers that opened the same file converge rather than duplicate.
    let mut replica = holding(&sample);
    let heads = replica.heads();
    replica
        .apply(Invoke::edit(&load_project(&sample)).unwrap())
        .unwrap();
    let after = replica.state().unwrap();
    assert_eq!(after, replica.state_at(&heads).unwrap());
    let text = crate::seed::document_of(&sample);
    assert_eq!(Project::read(&text).unwrap(), after);
}

#[test]
fn clip_grading_geometry_speed_and_curves_survive_the_document() {
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
    let transform = Transform {
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
    let mut curves = crate::curve::Curve::default();
    curves.master.set_point(0.5, 0.72);
    curves.red.set_point(0.3, 0.44);
    curves.green.set_point(0.6, 0.21);
    curves.blue.set_point(0.8, 0.93);
    curves.mix = 0.65;
    let untouched = round_trip(&p);
    assert!(
        untouched.clip(cid).unwrap().curves.is_identity(),
        "an untouched clip comes back ungraded"
    );
    {
        let c = p.clip_mut(cid).unwrap();
        c.key = key;
        c.mask = mask;
        c.transform = transform;
        c.curves = curves.clone();
        c.speed = Speed::new(2.5, true);
        c.set_key_at(Param::Opacity, 0.0, 0.0, Interp::Ease);
        c.set_key_at(Param::Opacity, 3.0, 1.0, Interp::Hold);
        c.set_key_at(Param::PosX, 1.0, -0.5, Interp::Linear);
        c.transition = Transition::CrossDissolve;
    }
    let back = round_trip(&p);
    let c = back.clip(cid).unwrap();
    assert_eq!(c.key, key, "every field of the key, not just the switch");
    assert_eq!(c.mask, mask);
    assert_eq!(c.transform, transform);
    assert_eq!(c.curves, curves, "every channel and the amount");
    assert_eq!(c.speed, Speed::new(2.5, true));
    assert_eq!(c.duration(), p.clip(cid).unwrap().duration());
    let keys = &c.anim.track(Param::Opacity).unwrap().keys;
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys[0].interp,
        Interp::Ease,
        "the curve's shape comes back too"
    );
    assert_eq!(keys[1].interp, Interp::Hold);
    for t in [0.0, 0.7, 1.5, 2.9, 5.0] {
        assert_eq!(
            c.param_at(Param::Opacity, t),
            p.clip(cid).unwrap().param_at(Param::Opacity, t),
            "the curve reads the same at {t}s",
        );
    }
    assert_eq!(back, p, "nothing else changed on the way");
}

#[test]
fn markers_titles_and_links_survive_and_their_removal_replicates() {
    let (mut p, cid) = one_clip();
    p.add_marker(4.0, "act two", 3);
    p.clip_mut(cid)
        .unwrap()
        .add_marker(2.0, "beat", 5)
        .expect("inside");
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
    let group = Uuid::new_v4();
    {
        let c = p.clip_mut(cid).unwrap();
        c.link = Some(group);
        c.sound_split = true;
    }
    let back = round_trip(&p);
    assert_eq!(back.markers[0].name, "act two");
    assert_eq!(back.markers[0].palette, 3);
    let cm = &back.clip(cid).unwrap().markers;
    assert_eq!(
        (cm[0].name.as_str(), cm[0].t),
        ("beat", 2.0),
        "still clip-local"
    );
    assert_eq!(back.media(mid).unwrap().text.as_ref(), Some(&text));
    assert_eq!(back.clip(cid).unwrap().link, Some(group));
    assert!(back.clip(cid).unwrap().sound_split);

    // Clearing a title or a link is a change a peer sees, not silence.
    let mut replica = holding(&p);
    let title = Project::paths().media.item(&id(mid));
    replica
        .apply(Invoke::group([
            Invoke::edit(&title.text.set(None)).unwrap(),
            Invoke::edit(&title.kind.set(MediaKind::Video)).unwrap(),
        ]))
        .unwrap();
    assert_eq!(replica.state().unwrap().media(mid).unwrap().text, None);
    replica
        .apply(Invoke::edit(&Project::paths().clips.item(&id(cid)).link.set(None)).unwrap())
        .unwrap();
    let seen = replica.state().unwrap();
    assert!(!seen.is_linked(cid));
    assert!(seen.link_partners(cid).is_empty());
}

#[test]
fn the_sample_project_round_trips_through_its_document() {
    let p = Project::read(&crate::seed::sample_document()).unwrap();
    assert_eq!(p.name, "Coastline_Doc_v3");
    assert_eq!(p.media.len(), 7);
    assert_eq!(p.tracks.len(), 4);
    assert_eq!(p.clips.len(), 7);
    let interview = p
        .media
        .iter()
        .find(|m| m.name == "Interview_A.mp4")
        .unwrap();
    let clip = p.clips.iter().find(|c| c.media == interview.id).unwrap();
    assert!((clip.duration() - 12.0).abs() < 1e-9);
    assert_eq!(clip.transition, Transition::CrossDissolve);
    assert_eq!(p, round_trip(&p), "reading it back changes nothing");

    // An effect stack keeps its order through the document.
    let mut stacked = p.clone();
    let cid = stacked.clips[0].id;
    stacked.clip_mut(cid).unwrap().effects = vec![
        ClipEffect {
            id: Uuid::new_v4(),
            kind: EffectKind::Sepia,
            amount: 0.6,
        },
        ClipEffect {
            id: Uuid::new_v4(),
            kind: EffectKind::Invert,
            amount: 1.0,
        },
    ]
    .into();
    let back = round_trip(&stacked);
    let kinds: Vec<EffectKind> = back
        .clip(cid)
        .unwrap()
        .effects
        .iter()
        .map(|e| e.kind)
        .collect();
    assert_eq!(kinds, [EffectKind::Sepia, EffectKind::Invert]);
    let doc = ui::tilo_resource::Doc::new();
    assert!(
        Project::read(&doc).unwrap().clips.is_empty(),
        "a fresh document is an empty project"
    );
}
