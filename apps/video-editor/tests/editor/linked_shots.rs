//! A shot whose source carries sound is two clips, picture and sound, linked:
//! the edits that keep them in sync take both halves, the ones that make the
//! sound worth being a clip of its own (levels, fades) take one, and a locked
//! lane refuses the whole shot.

use crate::hosted_editor as hosted;

use hosted::{Editor, clip_of, go_to, open, press, select_on, track_name, with_modifier};
use video_editor::Project;
use video_editor::model::{ClipId, TrackKind};
use video_editor::seed::sample_project;

const MARKET: &str = "Cutaway_Market.mp4";

/// The demo project with `names` recorded as carrying sound.
fn with_sound(names: &[&str]) -> Project {
    let mut project = sample_project();
    for media in &mut project.media {
        if names.contains(&media.name.as_str()) {
            media.has_audio = true;
        }
    }
    project
}

/// The market cutaway (V1, 20–26 s) with its sound split off through the
/// inspector, the picture selected. Returns the picture and the sound.
fn linked_shot() -> (Editor, ClipId, ClipId) {
    let mut app = open(&with_sound(&[MARKET]));
    select_on(&mut app, MARKET, "V1");
    press(&mut app, "Split off audio");
    let project = app.state();
    let picture = clip_of(&project, MARKET, TrackKind::Video);
    let partners = project.link_partners(picture);
    assert_eq!(partners.len(), 1, "the sound came off onto its own clip");
    (app, picture, partners[0])
}

#[test]
fn splitting_off_the_sound_makes_a_linked_pair_the_document_holds() {
    let (mut app, picture, sound) = linked_shot();
    let project = app.state();
    assert_eq!(project.clip_kind(sound), TrackKind::Audio);
    let (v, a) = (project.clip(picture).unwrap(), project.clip(sound).unwrap());
    assert!(v.link.is_some() && v.link == a.link);
    assert!(v.sound_split, "or the shot would be heard twice");
    assert!(
        hosted::named(&mut app, "Unlink audio and video").is_some(),
        "the control now offers the way back"
    );
}

#[test]
fn placing_a_source_with_sound_lands_a_linked_pair() {
    let mut app = open(&with_sound(&["Interview_A.mp4"]));
    let before = app.state();
    // The bin row (not a placed clip) adds the source at the playhead.
    let bin_row = |name: &'static str| {
        move |n: &ui::accesskit::Node| {
            n.role() == ui::accesskit::Role::ListItem
                && n.label() == Some(name)
                && n.value().is_some_and(|v| !v.contains(" on "))
        }
    };
    let (row, _) = hosted::find(&mut app, bin_row("Interview_A.mp4")).unwrap();
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .unwrap();
    let after = app.state();
    assert_eq!(after.clips.len(), before.clips.len() + 2, "two clips");
    let added: Vec<_> = after
        .clips
        .iter()
        .filter(|c| before.clip(c.id).is_none())
        .collect();
    assert_eq!(added[0].link, added[1].link);
    assert!(added[0].link.is_some());
    let kinds: Vec<_> = added.iter().map(|c| after.clip_kind(c.id)).collect();
    assert!(kinds.contains(&TrackKind::Audio) && kinds.contains(&TrackKind::Video));

    // A source with no sound still lands as one clip.
    let (row, _) = hosted::find(&mut app, bin_row("Sunset_Timelapse.mp4")).unwrap();
    app.harness()
        .perform_a11y_action(row, &ui::a11y::Action::Activate)
        .unwrap();
    let last = app.state();
    assert_eq!(last.clips.len(), after.clips.len() + 1);
    let only = last
        .clips
        .iter()
        .find(|c| after.clip(c.id).is_none())
        .unwrap();
    assert!(!last.is_linked(only.id));
}

#[test]
fn a_locked_sound_lane_refuses_every_edit_of_the_shot() {
    for gesture in ["i", "o", "s", "Delete", "nudge"] {
        let (mut app, picture, sound) = linked_shot();
        let lane = track_name(&app.state(), sound);
        press(&mut app, &format!("Lock {lane}"));
        assert!(
            app.state()
                .track_locked(app.state().clip(sound).unwrap().track)
        );
        select_on(&mut app, MARKET, "V1");
        go_to(&mut app, "0:21");
        let before = app.state();
        match gesture {
            "nudge" => with_modifier(&mut app, "shift", "ArrowRight"),
            key => app.harness().key(key),
        }
        assert_eq!(
            app.state(),
            before,
            "{gesture} edited a shot on a locked lane"
        );
        assert!(app.state().clip(picture).is_some());
    }
}

#[test]
fn trims_take_both_halves_once_unlocked_and_one_once_unlinked() {
    for unlink in [false, true] {
        for key in ["i", "o"] {
            let (mut app, picture, sound) = linked_shot();
            let lane = format!("Lock {}", track_name(&app.state(), sound));
            press(&mut app, &lane);
            press(&mut app, &lane);
            if unlink {
                press(&mut app, "Unlink audio and video");
                press(&mut app, &lane);
            }
            select_on(&mut app, MARKET, "V1");
            go_to(&mut app, "0:21");
            let before = app.state();
            app.harness().key(key);
            let after = app.state();
            assert_ne!(
                after.clip(picture),
                before.clip(picture),
                "{key} trims picture"
            );
            if unlink {
                assert_eq!(
                    after.clip(sound),
                    before.clip(sound),
                    "locked, unlinked sound stays"
                );
            } else {
                let (v, a) = (after.clip(picture).unwrap(), after.clip(sound).unwrap());
                assert!(
                    (v.start - a.start).abs() < 1e-9
                        && (v.in_point - a.in_point).abs() < 1e-9
                        && (v.end() - a.end()).abs() < 1e-9,
                    "{key}: the sound keeps covering the picture: {v:?} {a:?}"
                );
            }
        }
    }
}

#[test]
fn deleting_either_way_takes_the_whole_shot() {
    for ripple in [false, true] {
        let (mut app, picture, sound) = linked_shot();
        if ripple {
            with_modifier(&mut app, "shift", "Delete");
        } else {
            app.harness().key("Delete");
        }
        let project = app.state();
        assert!(project.clip(picture).is_none() && project.clip(sound).is_none());
    }
}

#[test]
fn splitting_a_shot_cuts_both_lanes_into_two_linked_pairs() {
    let (mut app, picture, sound) = linked_shot();
    let clips = app.state().clips.len();
    go_to(&mut app, "0:22");
    app.harness().key("s");
    let project = app.state();
    assert_eq!(project.clips.len(), clips + 2, "both lanes were cut");
    let heads = project.clip(picture).unwrap().link;
    assert_eq!(project.clip(sound).unwrap().link, heads);
    let tails: Vec<_> = project
        .clips
        .iter()
        .filter(|c| c.link.is_some() && c.link != heads)
        .collect();
    assert_eq!(tails.len(), 2, "the tails are a pair of their own");
    assert_eq!(tails[0].link, tails[1].link);
}

#[test]
fn nudging_one_half_carries_the_other_on_its_own_lane() {
    let (mut app, picture, sound) = linked_shot();
    let before = app.state();
    with_modifier(&mut app, "shift", "ArrowRight");
    let after = app.state();
    let moved = |id| after.clip(id).unwrap().start - before.clip(id).unwrap().start;
    assert!((moved(picture) - 1.0).abs() < 1e-6, "{}", moved(picture));
    assert!((moved(sound) - moved(picture)).abs() < 1e-6);
    assert_eq!(
        after.clip(sound).unwrap().track,
        before.clip(sound).unwrap().track
    );
}

#[test]
fn re_timing_the_picture_re_times_its_sound() {
    let (mut app, picture, sound) = linked_shot();
    hosted::act(&mut app, "Rate", &ui::a11y::Action::Increment).unwrap();
    let project = app.state();
    let rate = project.clip(picture).unwrap().speed.rate();
    assert!((rate - 1.0).abs() > 1e-6, "the picture was re-timed");
    assert!(
        (project.clip(sound).unwrap().speed.rate() - rate).abs() < 1e-9,
        "re-timing one half and not the other desyncs them permanently"
    );
}

#[test]
fn levels_and_fades_stay_with_the_clip_they_were_set_on() {
    let (mut app, picture, sound) = linked_shot();
    let lane = track_name(&app.state(), sound);
    select_on(&mut app, MARKET, &lane);
    let before = app.state();
    for field in ["Fade in", "Gain"] {
        hosted::act(&mut app, field, &ui::a11y::Action::Increment).unwrap();
    }
    let after = app.state();
    assert_ne!(after.clip(sound), before.clip(sound), "the sound was faded");
    assert_eq!(
        after.clip(picture),
        before.clip(picture),
        "the picture was not"
    );
}

#[test]
fn copies_of_a_shot_are_a_new_pair_tied_to_nothing_else() {
    for duplicate in [false, true] {
        let (mut app, picture, sound) = linked_shot();
        let before = app.state();
        let original = before.clip(picture).unwrap().link;
        if duplicate {
            with_modifier(&mut app, "ctrl", "d");
        } else {
            with_modifier(&mut app, "ctrl", "c");
            go_to(&mut app, "0:40");
            with_modifier(&mut app, "ctrl", "v");
        }
        let after = app.state();
        let copies: Vec<_> = after
            .clips
            .iter()
            .filter(|c| before.clip(c.id).is_none())
            .collect();
        assert_eq!(copies.len(), 2, "the sound came with the picture");
        assert!(copies[0].link.is_some() && copies[0].link == copies[1].link);
        assert_ne!(copies[0].link, original, "the copy moves on its own");
        assert_eq!(after.clip(picture).unwrap().link, original);
        assert_eq!(after.clip(sound).unwrap().link, original);
    }
}

#[test]
fn the_link_control_unlinks_and_links_again() {
    // Only the market shot, so selecting everything selects its two halves.
    let mut project = with_sound(&[MARKET]);
    let market = project.media.iter().find(|m| m.name == MARKET).unwrap().id;
    project.clips.retain(|c| c.media == market);
    let mut app = open(&project);
    select_on(&mut app, MARKET, "V1");
    press(&mut app, "Split off audio");
    let picture = clip_of(&app.state(), MARKET, TrackKind::Video);

    let versions = app.versions().len();
    press(&mut app, "Unlink audio and video");
    assert!(!app.state().is_linked(picture), "the pair came apart");
    assert_eq!(app.versions().len(), versions + 1, "as one step");

    with_modifier(&mut app, "ctrl", "a");
    press(&mut app, "Link audio and video");
    assert!(app.state().is_linked(picture), "and is tied back together");
}
