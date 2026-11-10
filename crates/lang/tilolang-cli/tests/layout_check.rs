//! `tilolang layout-check` through its library: a fixture app whose views are each laid out
//! wrong in one way is caught for every kind of problem, at the view that is wrong; and the
//! shipped apps show no problem their allow-list (`tests/layout_check/allowed`) does not
//! already name, so a new one fails here.

use std::path::{Path, PathBuf};
use std::process::Command;

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
        // Two words drawn in one place are unreadable however their boxes stand: the view that
        // lays one over the other breaks in both ways.
        (Kind::TextOnText, "overlapping"),
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
            // Two words drawn in one place are unreadable however their boxes stand: the view
            // that lays one over the other breaks in both ways.
            Kind::Overlap | Kind::TextOnText => "overlapping",
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
            // No page in this fixture stands inside another.
            | Kind::PageInPage
            | Kind::RaggedColumns
            | Kind::DoubleSelection
            // The fixture's lists paint no selection of their own.
            | Kind::SelectionBleed
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
            | Kind::TitleClippedWithRoom
            // The fixture's windows ship one language: nothing reads from another catalogue.
            | Kind::MixedLanguage
            // No window in this fixture opens an item under its row, and no page names itself
            // in a bar beside a heading of its own. Its bars stand wide enough to keep the
            // sidebar's toggle in its corner, clear of the way back.
            | Kind::OpenOutsideList
            | Kind::RepeatedPageTitle
            | Kind::DoubleNavigation
            // The fixture's sections hold no tree, so no first level stands off its title.
            | Kind::FirstLevelIndent
            // The fixture's windows declare no actions of their own, so their bars hold the
            // window's controls alone, each once, and no action stands as an icon.
            | Kind::DuplicateBarControl
            | Kind::IconOnlyBarAction
            // The fixture's windows compose no shell with a header row of its own under a bar.
            // None declares the one main thing to do either.
            | Kind::StackedHeaderRows
            | Kind::UnmarkedBarPrimary
            // No window in this fixture lists items beside a pane that says nothing is chosen.
            | Kind::PlaceholderPane
            // The fixture's windows compose no sidebar over a header naming a place.
            | Kind::UnchosenPlace
            // The fixture's windows compose no page around a list, and open no item into its
            // row.
            | Kind::NestedScroller
            | Kind::RepeatedRowTitle
            // No window in this fixture opens an item into its row, so no document stands
            // beside the rows of the list it opened from.
            | Kind::ItemShownTwice
            // The fixture's cards each hold what they are given, so none stands empty.
            | Kind::EmptySurface
            // No window in this fixture docks a bar at the foot of a page column, and none
            // lines its edges with a card or a notice, nor stands one against another.
            | Kind::DockedColumn
            | Kind::RoundedOnEdge
            | Kind::CrampedCard
            // No window in this fixture docks a sidebar beside content it squeezes.
            | Kind::ContentBesideSidebar => "(no view)",
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

/// The `languages` fixture copied to a scratch directory, with the German word for
/// `w-a-word-only-the-source-language` taken out: an app whose German is short one message. The
/// fixture in the tree is translated whole; the drift is made here, where the check is tested.
fn short_of_one_german_word() -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let source = manifest().join("tests/layout_check/languages");
    let scratch = tempfile::tempdir()?;
    copy_tree(&source, &scratch.path().join("languages"))?;
    let german = scratch.path().join("languages/locales/de-DE.ftl");
    let whole = std::fs::read_to_string(&german)?;
    let mut short = String::with_capacity(whole.len());
    for line in whole.lines() {
        if !line
            .trim_start()
            .starts_with(".w-a-word-only-the-source-language")
        {
            short.push_str(line);
            short.push('\n');
        }
    }
    std::fs::write(&german, short)?;
    let languages = scratch.path().join("languages");
    Ok((scratch, languages))
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// A window reads in one language throughout: a word it shows from a catalogue in another
/// language — here the app's source standing in for a translation it lacks — is a finding, and
/// the same window read in the language the person speaks has none. The person's language is
/// staged whatever locale this machine runs in, so the two runs differ by the person alone.
#[test]
fn a_word_a_window_shows_from_another_languages_catalogue_is_a_finding() {
    let (_scratch, dir) = short_of_one_german_word()
        .unwrap_or_else(|error| panic!("the languages fixture copies: {error}"));
    let report = |locale: &str| {
        layout_check::check(&Options {
            dirs: vec![dir.clone()],
            locale: locale.to_owned(),
            ..Options::default()
        })
        .unwrap_or_else(|error| panic!("the fixture lays out: {error}"))
    };
    let english = report(layout_check::DEFAULT_LOCALE);
    assert!(
        english.findings.is_empty(),
        "every word reads in English:\n{}",
        layout_check::render(&english)
    );
    let german = report("de-DE");
    let mixed: Vec<&str> = german
        .findings
        .iter()
        .filter(|f| f.kind == Kind::MixedLanguage)
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(
        mixed.len(),
        1,
        "the one word German lacks:\n{}",
        layout_check::render(&german)
    );
    assert!(
        mixed[0].contains("w-a-word-only-the-source-language"),
        "the word the app's German leaves out: {}",
        mixed[0]
    );
    assert_eq!(
        german.findings.len(),
        1,
        "and nothing else:\n{}",
        layout_check::render(&german)
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

/// A view another app's page draws is laid out there, so it is reached for the package that
/// defines it though none of that package's own windows shows it; a row view of a type inside a
/// collection of a union is opened on an item of that type; and a view nothing calls is named as
/// one to delete, apart from a view a view calls that no scenario reaches.
#[test]
fn a_view_another_app_draws_and_a_union_items_row_view_are_reached_while_a_dead_view_is_named() {
    let dir = manifest().join("tests/layout_check/reach");
    let report = layout_check::check(&Options {
        dirs: vec![dir.join("drawer"), dir.join("drawn")],
        search: vec![dir],
        ..Options::default()
    })
    .expect("the fixtures lay out");
    let unreached: Vec<(&str, bool)> = report
        .unreached
        .iter()
        .map(|view| (view.view.as_str(), view.called))
        .collect();
    assert_eq!(
        unreached,
        [
            ("layoutdrawn › behind", true),
            ("layoutdrawn › orphan", false)
        ],
        "the row another app draws and the note's row are reached, and the two no layout \
         shows say which they are:\n{}",
        layout_check::render(&report)
    );
    let rendered = layout_check::render(&report);
    assert!(
        rendered.contains(
            "note: no view calls layoutdrawn › orphan: nothing can draw it, so delete it"
        ),
        "{rendered}"
    );
    assert!(
        rendered.contains(
            "note: no scenario reaches layoutdrawn › behind: a view calls it, so a scenario must \
             reach the view that draws it"
        ),
        "{rendered}"
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

/// A window held in the hand shows its list; the row a person taps opens what it holds in
/// front of the list. The run stages every such screen, named after what it shows — for an app
/// whose rows open a page over the list (a board's cards) as for one whose window pushes it,
/// and for one whose list stands over what it shows as a drawer its sidebar toggle brings
/// forward.
#[test]
fn a_phone_layout_shows_what_a_tapped_row_opens() {
    use tilo_test_env::SpawnUnderEnvLock as _;

    for (app, shows) in [
        // Notes holds its tree over the note's page as a drawer at this width: the window's
        // sidebar toggle brings the tree forward, and a row of it opens its note with the
        // drawer behind.
        ("notes", ", then the sidebar at 390×844"),
        (
            "notes",
            ", then the sidebar, then the row tapped at 390×844",
        ),
        // A board keeps no detail of its own: the card a finger taps is the page that shows.
        ("board", "then a card_page in front at 390×844"),
    ] {
        let dir = manifest().join("../../../apps/tl").join(app);
        let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("layout-check")
            .arg(&dir)
            .args(["--dump", "390x844"])
            .output_locked()
            .expect("the check runs");
        let doc = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "{app} is laid out: {}",
            doc.lines().last().unwrap_or_default()
        );
        let headers: Vec<&str> = doc.lines().filter(|line| line.starts_with("== ")).collect();
        assert!(
            headers.iter().any(|header| header.contains(shows)),
            "a phone window shows what `{shows}` names in {app}:\n{}",
            headers.join("\n")
        );
    }
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
