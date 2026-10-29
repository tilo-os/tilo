//! The inspector as a reader meets it: a folded section still says what is
//! in it, the controls drawn above the first section are stops too, and the
//! mix controls name the lane they act on.

use crate::hosted_editor as hosted;

use hosted::{Editor, go_to, named, open, playhead, press, select_on, value};
use ui::a11y::Action;
use ui::accesskit::Role;
use video_editor::Project;
use video_editor::param::{Interp, Param};
use video_editor::seed::sample_project;

const SUNSET: &str = "Sunset_Timelapse.mp4";

/// The demo project with the sunset clip (V2, 21–27 s) changed by `edit`,
/// that clip selected.
fn with_sunset(edit: impl FnOnce(&mut video_editor::model::Clip)) -> Editor {
    let mut project: Project = sample_project();
    let sunset = project.media.iter().find(|m| m.name == SUNSET).unwrap().id;
    edit(
        project
            .clips
            .iter_mut()
            .find(|c| c.media == sunset)
            .unwrap(),
    );
    let mut app = open(&project);
    select_on(&mut app, SUNSET, "V2");
    app
}

/// What the folded section `name` reports.
fn folded(app: &mut Editor, name: &str) -> String {
    let section = named(app, name).unwrap_or_else(|| panic!("no {name} section"));
    if section.is_expanded() == Some(true) {
        press(app, name);
    }
    assert_eq!(named(app, name).unwrap().is_expanded(), Some(false));
    value(app, name)
}

#[test]
fn a_folded_curves_section_says_how_the_curve_is_shaped() {
    let mut app = with_sunset(|_| {});
    assert_eq!(folded(&mut app, "Curves"), "unchanged");

    let mut app = with_sunset(|c| {
        c.curves.master.set_point(0.5, 0.8);
        c.curves.blue.set_point(0.4, 0.2);
        c.curves.mix = 0.5;
    });
    let heard = folded(&mut app, "Curves");
    assert!(
        heard.contains("RGB") && heard.contains('B'),
        "names the channels: {heard}"
    );
    assert!(heard.contains("50%"), "and says it is turned down: {heard}");

    let mut app = with_sunset(|c| {
        c.curves.master.set_point(0.5, 0.8);
        c.curves.mix = 0.0;
    });
    let off = folded(&mut app, "Curves");
    assert!(off.contains("bypassed"), "a bypassed curve says so: {off}");
}

#[test]
fn a_folded_geometry_section_says_it_is_animated_where_the_curve_crosses_rest() {
    // Keyed from the default, the constants stay the plain fit; at 23 s the
    // curve reads rest, so only the animation itself can be reported.
    let mut app = with_sunset(|c| {
        c.set_key_at(Param::PosX, 22.0, -0.4, Interp::Linear);
        c.set_key_at(Param::PosX, 24.0, 0.4, Interp::Linear);
    });
    go_to(&mut app, "0:23");
    let heard = folded(&mut app, "Transform");
    assert!(heard.contains("position +0.00"), "{heard}");
    assert!(heard.contains("animated"), "{heard}");
}

#[test]
fn a_sound_clip_is_offered_no_curves() {
    let mut app = open(&sample_project());
    select_on(&mut app, "VO_Take3.wav", "A1");
    assert!(
        named(&mut app, "Curves").is_none(),
        "there is no picture to grade"
    );
}

#[test]
fn the_controls_drawn_above_the_first_section_are_stops_before_it() {
    let mut app = with_sunset(|_| {});
    let inspector = named(&mut app, "Inspector").unwrap();
    let nodes = app.harness().a11y_nodes();
    let children: Vec<_> = inspector
        .children()
        .iter()
        .map(|id| &nodes.iter().find(|(n, _)| n == id).unwrap().1)
        .collect();
    let first_section = children
        .iter()
        .position(|n| n.role() == Role::Group)
        .expect("the panel has sections");
    let at = |name: &str| {
        children
            .iter()
            .position(|n| n.label() == Some(name))
            .unwrap_or_else(|| panic!("{name} is a stop of the panel itself"))
    };
    let mut top = vec![
        "Opacity".to_string(),
        "Brightness".into(),
        "Contrast".into(),
        "Saturation".into(),
        "Fade in".into(),
        "Fade out".into(),
    ];
    top.extend(["cut", "dissolve", "fade", "dip", "wipe"].map(|t| format!("Transition {t}")));
    top.extend((1..=7).map(|n| format!("Label colour {n}")));
    for name in top {
        assert!(
            at(&name) < first_section,
            "{name} is drawn above the sections"
        );
    }
}

#[test]
fn a_lane_reset_appears_once_the_mix_is_moved_and_puts_it_back() {
    let mut app = open(&sample_project());
    select_on(&mut app, "VO_Take3.wav", "A1");
    let reset = "Reset A1 to 0.0 dB, centre";
    assert!(
        named(&mut app, reset).is_none(),
        "nothing to reset at unity"
    );

    hosted::act(&mut app, "Gain", &Action::Increment).unwrap();
    hosted::act(&mut app, "Pan", &Action::Decrement).unwrap();
    let lane = |app: &mut Editor| {
        let project = app.state();
        project
            .tracks
            .iter()
            .find(|t| t.name == "A1")
            .unwrap()
            .clone()
    };
    assert!(
        lane(&mut app).mix_is_set(),
        "the lane's mix reached the document"
    );
    assert!(value(&mut app, "A1").contains(&lane(&mut app).gain_words()));

    let node = named(&mut app, reset).expect("the reset names its lane");
    assert_eq!(node.role(), Role::Button);
    press(&mut app, reset);
    assert!(!lane(&mut app).mix_is_set(), "unity and centre again");
    assert!(named(&mut app, reset).is_none());
}

#[test]
fn an_unmeasured_mix_says_so_and_c_has_nowhere_to_go() {
    let mut app = open(&sample_project());
    let words = value(&mut app, "Mix level");
    assert!(!words.contains("clipped"), "{words}");
    assert!(words.to_lowercase().contains("not measured"), "{words}");

    go_to(&mut app, "0:07");
    app.harness().key("c");
    assert_eq!(
        playhead(&mut app),
        "00:07:00",
        "nothing clipped, so nothing moved"
    );
}
