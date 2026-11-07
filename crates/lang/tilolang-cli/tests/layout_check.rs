//! `tilolang layout-check` through its library: a fixture app whose views are each laid out
//! wrong in one way is caught for every kind of problem, at the view that is wrong; and the
//! shipped apps show no problem their allow-list (`tests/layout_check/allowed`) does not
//! already name, so a new one fails here.

use std::path::{Path, PathBuf};

use tilolang_cli::layout_check::{self, Kind, Options};

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Every app in `apps/tl`: a package directory with an `app.tl`, so an app added there is
/// checked without being named here.
fn apps() -> std::io::Result<Vec<PathBuf>> {
    let root = manifest().join("../../../apps/tl");
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let dir = entry?.path();
        if dir.join("app.tl").is_file() {
            dirs.push(dir);
        }
    }
    dirs.sort();
    Ok(dirs)
}

#[test]
fn every_kind_of_broken_layout_is_caught_at_the_view_that_breaks_it() {
    let report = layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/broken")],
        ..Options::default()
    })
    .expect("the fixture lays out");
    let found = |kind: Kind, view: &str| {
        report
            .findings
            .iter()
            .any(|f| f.kind == kind && f.view == view && f.line.is_some())
    };
    for (kind, view) in [
        (Kind::Overflow, "overflowing"),
        (Kind::Overlap, "overlapping"),
        (Kind::ScrollsSideways, "sideways"),
    ] {
        assert!(
            found(kind, view),
            "{} in `{view}` is caught:\n{}",
            kind.name(),
            layout_check::render(&report)
        );
    }
    // Each broken view breaks in its own way only.
    for finding in &report.findings {
        let expected = match finding.kind {
            Kind::Overflow => "overflowing",
            Kind::Overlap => "overlapping",
            // A word wider than its column breaks (`clipped` is laid out right), and the chip
            // takes a finger's room on touch (`small_target` is laid out right). A view in
            // this fixture has no scroll view given no room: `a_board_lane_draws_every_card_it_
            // has_room_for` holds that one.
            Kind::ClippedText | Kind::SmallHitTarget | Kind::LostContent => "",
            Kind::ScrollsSideways => "sideways",
            // Layout snaps every edge, broken or not; and a window gives a view that composes
            // no shell its page, so no page anywhere is one bare row. The fixtures write no
            // rows, no cards and no field names: the rules read off a window's own composition
            // hold nothing to find in them.
            Kind::OffPixel
            | Kind::BareRow
            | Kind::RaggedColumns
            | Kind::DoubleSelection
            | Kind::CrampedGutter
            | Kind::SeveralAdds
            | Kind::CodedName
            | Kind::UnsharedVerb
            | Kind::NestedCard
            | Kind::LoneDoor
            | Kind::EmptyHeading
            // No window in this fixture draws a count.
            | Kind::ZeroCount
            | Kind::RepeatedCount
            // No window in this fixture floats a popover or a sheet, and none composes a
            // shell whose title the row's actions squeeze.
            | Kind::PastWindow
            | Kind::TitleClippedWithRoom => "(no view)",
        };
        assert_eq!(
            finding.view,
            expected,
            "an unexpected finding:\n{}",
            layout_check::render(&report)
        );
    }
    assert!(report.new_findings().next().is_some(), "the check fails");
}

/// A view whose body is one `layers` is the window's Layers: its row at the foot covers the
/// content under it on purpose, which is no overlap. The column's pane the window reconciles
/// the view in reads as what it holds, so the check sees the layers and not a box of its own;
/// and two layers overlap only where the text of one lies over the other's, not wherever a
/// box holding text covers another.
#[test]
fn a_view_whose_body_is_layers_lays_its_children_over_each_other() {
    let report = layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/layered")],
        ..Options::default()
    })
    .expect("the fixture lays out");
    assert!(
        report.findings.is_empty(),
        "the row over the content is where the view put it:\n{}",
        layout_check::render(&report)
    );
}

/// A desk bounded by its window: a middle longer than the window scrolls between the bar and
/// the dock (a filled share takes no more than its share, in a stack and in layers), and a dock
/// holding more than the window is wide is as wide as the window, its items scrolling along it
/// (a chrome body narrows to the room it has).
#[test]
fn a_desk_longer_and_wider_than_its_window_scrolls_inside_it() {
    let report = layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/bounded")],
        ..Options::default()
    })
    .expect("the fixture lays out");
    assert!(
        report.findings.is_empty(),
        "the middle and the dock scroll inside the window:\n{}",
        layout_check::render(&report)
    );
}

/// A board's lanes are as tall as the page they are in: a lane holding as many cards as the
/// pane shows draws every one of them under its heading, and a lane holding more keeps them
/// all inside the view that scrolls them. A lane given no height loses its cards instead, which
/// the check catches (a scroll view with no room for what it holds).
#[test]
fn a_board_lane_draws_every_card_it_has_room_for() {
    let report = layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/board")],
        ..Options::default()
    })
    .expect("the fixture lays out");
    assert!(report.layouts > 0, "the board is laid out");
    assert!(
        report.unreached.is_empty(),
        "the board's views are laid out:\n{}",
        layout_check::render(&report)
    );
    assert!(
        report.findings.is_empty(),
        "no lane loses the cards it has room for:\n{}",
        layout_check::render(&report)
    );
}

/// A view whose body is a bare `if let` leaves no widget of its own: absent, it draws nothing,
/// and present, what it draws is the view it calls. Each still ran, so each is reached.
#[test]
fn a_view_that_is_one_branch_is_reached_whether_or_not_it_draws() {
    let report = layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/branches")],
        ..Options::default()
    })
    .expect("the fixture lays out");
    assert!(
        report.unreached.is_empty(),
        "every view the window ran is reached:\n{}",
        layout_check::render(&report)
    );
    assert!(
        report.findings.is_empty(),
        "the fixture is laid out right:\n{}",
        layout_check::render(&report)
    );
}

#[test]
fn edges_sized_in_fractions_of_a_rem_land_on_device_pixels_at_every_display_scale() {
    let scales = layout_check::layouts()
        .iter()
        .map(|layout| layout.display_scale)
        .collect::<Vec<_>>();
    for scale in [1.0, 1.5, 2.0] {
        assert!(scales.contains(&scale), "the check lays out at {scale}x");
    }
    let report = layout_check::check(&Options {
        dirs: vec![manifest().join("tests/layout_check/pixels")],
        ..Options::default()
    })
    .expect("the fixture lays out");
    assert!(
        report.findings.is_empty(),
        "every edge stands on a device pixel:\n{}",
        layout_check::render(&report)
    );
}

#[test]
fn the_apps_show_no_layout_problem_their_allow_list_does_not_name() {
    let report = layout_check::check(&Options {
        dirs: apps().expect("apps/tl reads"),
        allow: Some(manifest().join("tests/layout_check/allowed")),
        ..Options::default()
    })
    .expect("the apps lay out");
    assert!(
        report.layouts > 0 && apps().expect("apps/tl reads").len() >= 8,
        "every app in apps/tl is checked"
    );
    assert!(
        report.unreached.is_empty(),
        "every view is laid out: a scenario reaches each, or the system shows it:\n{}",
        layout_check::render(&report)
    );
    assert!(
        report.new_findings().next().is_none() && report.stale.is_empty(),
        "new layout problems, or allowed ones gone (take them out of the allow-list):\n{}",
        layout_check::render(&report)
    );
}

#[test]
fn a_state_the_run_stages_lays_the_window_out_there() {
    // Which place a window stands on is the run's, not the app's: `--state place=Task.today`
    // lays out the place a person has not picked, and a value the app declares no filter for
    // is refused before anything is laid out.
    let tasks = manifest().join("../../../apps/tl/tasks");
    let report = layout_check::check(&Options {
        dirs: vec![tasks.clone()],
        states: vec!["place=Task.today".to_owned()],
        ..Options::default()
    })
    .expect("the app lays out at the place the run stages");
    assert!(report.layouts > 0, "the app is laid out");
    let refused = layout_check::check(&Options {
        dirs: vec![tasks],
        states: vec!["place=Task.nonsense".to_owned()],
        ..Options::default()
    })
    .expect_err("a state takes a value the app's code names");
    assert!(
        refused.contains("declares no filter `nonsense`"),
        "the run says what it could not stage: {refused}"
    );
}

#[test]
fn the_gallery_shows_every_component_of_the_toolkit_and_each_builds() {
    let dir = manifest().join("../../../apps/tl/gallery");
    let mut source = String::new();
    for entry in std::fs::read_dir(&dir).expect("the gallery reads") {
        let path = entry.expect("the gallery reads").path();
        if path.extension().is_some_and(|e| e == "tl") {
            source.push_str(&std::fs::read_to_string(&path).expect("the gallery reads"));
        }
    }
    // Which components the gallery shows, and whether a component's declaration exempts it, is
    // the toolkit's own check (`tilo_ui::component::gallery`), which a widget's author runs in
    // the toolkit's tests; this app is laid out below as well.
    let problems = ui::component::gallery::coverage(&source).problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    let report = layout_check::check(&Options {
        dirs: vec![dir],
        ..Options::default()
    })
    .expect("the gallery lays out, every component of it built");
    assert!(report.layouts > 0, "the gallery is laid out");
}
