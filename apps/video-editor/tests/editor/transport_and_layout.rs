//! The transport's play button, the divider between preview and timeline,
//! and a fresh project's first moments.

use crate::hosted_editor as hosted;

use hosted::{Editor, find, go_to, named, open, playhead, press};
use ui::accesskit::Role;
use video_editor::seed::sample_project;

const DIVIDER: &str = "Resize the preview and the timeline";

/// An editor on a new, empty project; `focus` puts the keyboard on it.
fn blank(focus: bool) -> Editor {
    let mut app = Editor::new().unwrap();
    if focus {
        app.harness().click_named("video_editor_root").unwrap();
    }
    app
}

#[test]
fn the_play_button_names_and_stops_a_running_shuttle() {
    let mut app = open(&sample_project());
    for keys in [&["l"][..], &["l", "l", "l", "j"], &["l", "k"]] {
        // Mid-timeline, so a reverse shuttle has somewhere to run.
        go_to(&mut app, "0:10");
        for key in keys {
            app.harness().key(key);
        }
        let moving = keys.last() != Some(&"k");
        assert_eq!(
            named(&mut app, "Pause").is_some(),
            moving,
            "after {keys:?} the button says what pressing it does now"
        );
        if moving {
            press(&mut app, "Pause");
            assert!(
                named(&mut app, "Play").is_some(),
                "pressing it stopped the shuttle rather than playing on top of it"
            );
        }
    }
}

#[test]
fn dragging_the_divider_up_gives_the_timeline_more_room() {
    let mut app = open(&sample_project());
    app.harness().resize((800, 600)).unwrap();
    let divider = named(&mut app, DIVIDER).expect("a separator a reader can find");
    assert_eq!(divider.role(), Role::Splitter);
    let share = |app: &mut Editor| -> f64 {
        let v = named(app, DIVIDER).unwrap().value().unwrap().to_string();
        v.trim_end_matches('%').parse().unwrap()
    };
    let before = share(&mut app);
    let at = divider.bounds().unwrap();
    let (x, y) = (f64::midpoint(at.x0, at.x1), f64::midpoint(at.y0, at.y1));
    app.harness().drag_at((x, y), (x, y - 120.0));
    let after = named(&mut app, DIVIDER).unwrap().bounds().unwrap();
    assert!(
        after.y0 < at.y0 - 100.0,
        "the divider moved up: {at:?} -> {after:?}"
    );
    assert!(share(&mut app) < before, "the preview gave up its share");
}

#[test]
fn a_fresh_project_starts_at_zero_and_titles_land_there() {
    let mut app = blank(false);
    assert_eq!(playhead(&mut app), "00:00:00");
    app.harness().click_named("video_editor_root").unwrap();
    go_to(&mut app, "0:10");
    app.harness().key("t");
    let project = app.state();
    assert_eq!(project.clips.len(), 1, "the title is placed");
    assert!(
        project.clips[0].start.abs() < 1e-9,
        "never after a gap of nothing"
    );
    // A title has no file to be missing.
    let (_, row) = find(&mut app, |n| {
        n.role() == Role::ListItem && n.value().is_some_and(|v| !v.contains(" on "))
    })
    .expect("the title is in the bin");
    let words = row.value().unwrap().to_lowercase();
    assert!(
        !words.contains("not downloaded") && !words.contains("not on this device"),
        "{words}"
    );
}

#[test]
fn the_import_shortcut_asks_the_host_chooser_at_every_size() {
    // The empty bin names Ctrl+Shift+O as its keyboard route to Import.
    for size in [(960, 820), (1280, 800), (1440, 900)] {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = blank(true);
        app.harness().resize(size).unwrap();
        assert!(
            find(&mut app, |n| n
                .label()
                .is_some_and(|l| l.contains("Ctrl+Shift+O")))
            .is_some(),
            "{size:?}: the empty bin says how to import"
        );
        app.harness()
            .set_modifiers(&["ctrl".into(), "shift".into()]);
        app.harness().key("o");
        app.harness().set_modifiers(&[]);
        let asked = recorder.pick_requests();
        assert_eq!(asked.len(), 1, "{size:?}: one chooser");
        assert_eq!(asked[0].title(), "Import media");
    }
}
