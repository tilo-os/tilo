//! `tilolang layout-check` through its library: a fixture app whose views are each laid out
//! wrong in one way is caught for every kind of problem, at the view that is wrong. The shipped
//! apps are laid out by the `layout-check` t checks, which read the allow-list
//! (`tests/layout_check/allowed`) and keep every size's dump and shots.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use tilolang_cli::layout_check::{self, Kind, Options};

use crate::manifest;

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
        // Two words drawn in one place are unreadable however their boxes stand: the view that lays
        // one over the other breaks in both ways.
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
    for finding in &report.findings {
        let expected = match finding.kind {
            Kind::Overflow => "overflowing",
            // Two words drawn in one place are unreadable however their boxes stand: the view that
            // lays one over the other breaks in both ways.
            Kind::Overlap | Kind::TextOnText => "overlapping",
            // A word wider than its column breaks (`clipped` is laid out right), and the chip takes
            // a finger's room on touch (`small_target` is laid out right).
            Kind::ClippedText | Kind::SmallHitTarget | Kind::LostContent => "",
            Kind::ScrollsSideways => "sideways",
            // Layout snaps every edge, broken or not; and a window gives a view that composes no
            // shell its page, so no page anywhere is one bare row.
            Kind::OffPixel
            | Kind::BareRow
            | Kind::PageInPage
            | Kind::RaggedColumns
            | Kind::DoubleSelection
            | Kind::SelectionBleed
            | Kind::CrampedGutter
            | Kind::SeveralAdds
            | Kind::CodedName
            | Kind::UnsharedVerb
            | Kind::NestedCard
            | Kind::LoneDoor
            | Kind::EmptyHeading
            | Kind::ZeroCount
            | Kind::RepeatedCount
            // No window in this fixture floats a popover or a sheet, and none composes a shell
            // whose title the row's actions squeeze.
            | Kind::PastWindow
            | Kind::TitleClippedWithRoom
            | Kind::MixedLanguage
            // No window in this fixture opens an item under its row, and no page names itself in a
            // bar beside a heading of its own.
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
            | Kind::StackedHeaderRows
            | Kind::UnmarkedBarPrimary
            // No window in this fixture lists items beside a pane that says nothing is chosen.
            | Kind::PlaceholderPane
            | Kind::UnchosenPlace
            // The fixture's windows compose no page around a list, and open no item into its row.
            | Kind::NestedScroller
            | Kind::RepeatedRowTitle
            // No window in this fixture opens an item into its row, so no document stands beside
            // the rows of the list it opened from.
            | Kind::ItemShownTwice
            // The fixture's cards each hold what they are given, so none stands empty, and no
            // panel of it gives its title a body the title alone fills.
            | Kind::EmptySurface
            | Kind::HeaderOnly
            // No window in this fixture docks a bar at the foot of a page column, and none lines
            // its edges with a card or a notice, nor stands one against another.
            | Kind::DockedColumn
            | Kind::RoundedOnEdge
            | Kind::CrampedCard
            | Kind::ContentBesideSidebar
            | Kind::HeadingRoom
            // A list of this fixture groups nothing, so no group of it opens on a row.
            | Kind::OrphanedRow
            // The fixture's pages lay no section under a block of their own, and no list stands
            // beside the page it opens.
            | Kind::CrowdedHeading
            | Kind::DetailRowNotChosen
            // No window in this fixture declares a column header over a column that draws none or
            // offers an action in its bar and its own row both, no field of it stands one band tall
            // with its line off that band's middle (a field centres its own line), and no agent or
            // peer is drawn in it, so no presence tag stands anywhere.
            | Kind::HeaderOnReveal
            | Kind::OfferedTwice
            | Kind::LineOffCentre
            | Kind::PresenceTag
            | Kind::UnruledBody
            // No page of this fixture leads its heading with a control, and none stands on a
            // phone's screen.
            | Kind::ChevronOffLine
            | Kind::IndentedBody
            // Its lists group no rows under headings of their own, so none heads its rows.
            | Kind::RepeatedGroupTitle
            | Kind::HuggingRow
            | Kind::RepeatedDayFact => "(no view)",
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

/// A view whose body is one `layers` is the window's Layers: its row at the foot covers the content
/// under it on purpose, which is no overlap.
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

/// The `languages` fixture copied to a scratch directory, with the German word for `w-a-word-only-
/// the-source-language` taken out: an app whose German is short one message.
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

/// A window reads in one language throughout: a word it shows from a catalogue in another language
/// — here the app's source standing in for a translation it lacks — is a finding, and the same
/// window read in the language the person speaks has none.
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

/// A desk bounded by its window: a middle longer than the window scrolls between the bar and the
/// dock (a filled share takes no more than its share, in a stack and in layers), and a dock holding
/// more than the window is wide is as wide as the window, its items scrolling along it (a chrome
/// body narrows to the room it has).
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

/// The frames one layout may take: the layout itself, and one more for each change the app
/// answers it with.
const FRAMES_PER_LAYOUT: u64 = 30;

/// The layout nodes one layout may build: every node of the tree the frame rebuilds, 142 of them
/// for a board of lanes and cards.
const NODES_PER_LAYOUT: u64 = 180;

/// The text shaped outside the renderer's cache, per layout: these strings are the same on every
/// layout, so the cache holds all of them after the first, and only a stage's own words are shaped
/// again: 10 a layout for the board.
const SHAPED_PER_LAYOUT: u64 = 14;

/// One check of the board fixture, read for what it found and then for what it cost.
///
/// A board's lanes are as tall as the page they are in: a lane holding as many cards as the pane
/// shows draws every one of them under its heading, and a lane holding more keeps them all inside
/// the view that scrolls them.
///
/// What a layout costs is held in the toolkit's own units rather than in time: a check lays every
/// view of an app out from every scenario, at every size, display scale and text scale, and what it
/// spends per layout is what makes a run fast or slow. Wall time says nothing on a machine other
/// work shares, so this holds a layout to the frames the toolkit lays out, the layout nodes it
/// builds and the text it shapes outside the cache — counters that the same sources give the
/// same numbers on any machine, in any build profile.
#[test]
fn a_board_lane_draws_every_card_it_has_room_for_and_a_layout_costs_no_more_work_than_it_did() {
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

    // What that same check cost, a layout.
    let layouts = u64::try_from(report.layouts).expect("layouts fit");
    assert!(
        layouts >= 40,
        "every layout of the app runs, not {layouts} of them"
    );
    let per_layout = |count: u64| count / layouts;
    let work = report.work;
    assert!(
        per_layout(work.frames) <= FRAMES_PER_LAYOUT,
        "a layout costs at most {FRAMES_PER_LAYOUT} frames, not {} ({} frames over {layouts} layouts)",
        per_layout(work.frames),
        work.frames
    );
    assert!(
        per_layout(work.nodes) <= NODES_PER_LAYOUT,
        "a layout builds at most {NODES_PER_LAYOUT} layout nodes, not {} ({} nodes over {layouts} layouts)",
        per_layout(work.nodes),
        work.nodes
    );
    assert!(
        per_layout(work.shaped) <= SHAPED_PER_LAYOUT,
        "a layout shapes at most {SHAPED_PER_LAYOUT} runs outside the cache, not {} ({} over {layouts} layouts)",
        per_layout(work.shaped),
        work.shaped
    );
}

/// A view another app's page draws is laid out there, so it is reached for the package that defines
/// it though none of that package's own windows shows it; a row view of a type inside a collection
/// of a union is opened on an item of that type; and a view nothing calls is named as one to
/// delete, apart from a view a view calls that no scenario reaches.
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

/// A view whose body is a bare `if let` leaves no widget of its own: absent, it draws nothing, and
/// present, what it draws is the view it calls.
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

/// Held strictly, an allow-list line whose finding no longer shows fails the run: a fixed problem
/// whose line stays behind reads to the next person as a problem still there.
#[test]
fn a_strict_run_fails_on_an_allow_list_line_that_no_longer_shows() {
    use tilo_test_env::SpawnUnderEnvLock as _;

    let scratch = tempfile::tempdir().expect("a scratch directory");
    let allow = scratch.path().join("allowed");
    std::fs::write(&allow, "overlap nowhere › view [Row]\n").expect("the allow-list writes");
    let run = |strict: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tilolang"));
        command
            .arg("layout-check")
            .arg(manifest().join("tests/layout_check/branches"))
            .arg("--allow")
            .arg(&allow);
        if strict {
            command.arg("--strict");
        }
        command.output_locked().expect("the check runs")
    };
    let lenient = run(false);
    assert!(
        lenient.status.success(),
        "{}",
        String::from_utf8_lossy(&lenient.stderr)
    );
    let strict = run(true);
    assert!(!strict.status.success(), "a stale line fails a strict run");
    assert!(
        String::from_utf8_lossy(&strict.stdout).contains("take it out"),
        "the run says which line to take out: {}",
        String::from_utf8_lossy(&strict.stdout)
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
fn a_state_the_run_stages_lays_the_window_out_there() {
    // Which place a window stands on is the run's, not the app's: `--state place=Task.today` lays
    // out the place a person has not picked, and a value the app declares no filter for is refused
    // before anything is laid out.
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
    // The units run at the same time, so a failure stops the sweep wherever it stands: the run
    // names the unit that failed rather than the turn the stop left empty.
    assert!(
        refused.starts_with("tasks: the scenario `"),
        "the run names the unit that failed: {refused}"
    );
}

/// A window held in the hand shows its list; the row a person taps opens what it holds in front of
/// the list.
#[test]
fn a_phone_layout_shows_what_a_tapped_row_opens() {
    use tilo_test_env::SpawnUnderEnvLock as _;

    for (app, shown) in [
        // Notes is a tree and the note it opens: at this width the tree is the window's first
        // screen and a row of it pushes the note in front, with the way back to the tree. One run
        // of the check dumps both pages.
        (
            "notes",
            &[
                ", then a note_editor in front at 390×844",
                ", then Back pressed, then a note_editor in front at 390×844",
            ][..],
        ),
        // A board keeps no detail of its own: the card a finger taps is the page that shows.
        ("board", &["then a card_page in front at 390×844"][..]),
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
        for &shows in shown {
            assert!(
                headers.iter().any(|header| header.contains(shows)),
                "a phone window shows what `{shows}` names in {app}:\n{}",
                headers.join("\n")
            );
        }
    }
}

/// A row's box is a control of its own, named for what it does to the item (`Mark Buy oat milk and
/// eggs done`): a run presses it by those words, as a person or an agent does, and the row it
/// belongs to stands where it stood, drawn checked, for the shot.
#[test]
fn a_run_presses_a_rows_box_by_its_own_words() {
    use tilo_test_env::SpawnUnderEnvLock as _;

    let shots = manifest().join("../../../target/layout-press-shots");
    let _ = std::fs::remove_dir_all(&shots);
    std::fs::create_dir_all(&shots).expect("the shots' directory");
    let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
        .arg("layout-check")
        .arg(manifest().join("../../../apps/tl/tasks"))
        .arg("--allow")
        .arg(manifest().join("tests/layout_check/allowed"))
        .args(["--dump", "390x844"])
        .arg("--shots")
        .arg(&shots)
        .args(["--press", "Mark Buy oat milk and eggs done"])
        .output_locked()
        .expect("the check runs");
    let doc = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "the press reaches the box: {}",
        doc.lines().last().unwrap_or_default()
    );
    assert!(
        doc.lines().any(|line| line.starts_with("== ")
            && line.contains("then Mark Buy oat milk and eggs done pressed")),
        "the window as the press leaves it is read:\n{doc}"
    );
    let pressed = std::fs::read_dir(&shots)
        .expect("the shots' directory")
        .filter_map(std::result::Result::ok)
        .any(|entry| {
            let path = entry.path();
            path.extension().is_some_and(|extension| extension == "png")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains("pressed"))
        });
    assert!(
        pressed,
        "the window as the press leaves it is shot, in {shots:?}"
    );
}

/// A run over every app asks for the frames a finding came from (`--dump-findings`), and a run
/// narrowed to a view can still ask for every frame (`--dump`): a page holding the layout of
/// every frame of every view is too big to keep.
#[test]
fn a_dump_of_findings_holds_the_frames_a_finding_came_from() {
    use tilo_test_env::SpawnUnderEnvLock as _;
    use tilolang_cli::layout_check::Dump;

    assert!(Dump::Findings((390, 844)).prints((390, 844), true));
    assert!(!Dump::Findings((390, 844)).prints((390, 844), false));
    assert!(Dump::Every((390, 844)).prints((390, 844), false));
    assert!(!Dump::Every((390, 844)).prints((320, 568), true));

    let frames = |scope: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("layout-check")
            .arg(manifest().join("tests/layout_check/broken"))
            .arg(manifest().join("tests/layout_check/layered"))
            .args([scope, "390x844"])
            .output_locked()
            .expect("the check runs");
        assert!(
            !output.status.success(),
            "the fixture whose views are broken still fails the run"
        );
        String::from_utf8_lossy(&output.stderr)
            .lines()
            .filter(|line| line.starts_with("== ") && line.ends_with("— the frame"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let every = frames("--dump");
    let found = frames("--dump-findings");
    assert!(!every.is_empty(), "the run reads frames of both apps");
    assert!(
        found.len() < every.len(),
        "the frames a finding came from are kept, not every frame:\n{found:#?}\nof {}",
        every.len()
    );
    assert!(
        found.iter().all(|header| every.contains(header)),
        "the frames a finding came from are frames the run read"
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
    // Which components the gallery shows, and whether a component's declaration exempts it, is the
    // toolkit's own check (`tilo_ui::component::gallery`), which a widget's author runs in the
    // toolkit's tests; this app is laid out below as well.
    let problems = ui::component::gallery::coverage(&source).problems();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    let report = layout_check::check(&Options {
        dirs: vec![dir],
        ..Options::default()
    })
    .expect("the gallery lays out, every component of it built");
    assert!(report.layouts > 0, "the gallery is laid out");
}

/// An allow-list names one finding a line: blank lines and what follows a `#` are no finding, an
/// id named twice is allowed once, and a list that is not there is an error naming it, never an
/// empty list.
#[test]
fn an_allow_list_is_one_finding_a_line_past_its_comments_and_blank_lines() {
    let scratch = tempfile::tempdir().expect("a scratch directory");
    let path = scratch.path().join("allowed");
    let rows: [(&str, &str, &[&str]); 8] = [
        ("an empty file", "", &[]),
        (
            "blank lines and comments alone",
            "\n   \n# known\n  # and this\n",
            &[],
        ),
        (
            "one finding",
            "overflow:tasks:list\n",
            &["overflow:tasks:list"],
        ),
        (
            "no line end after the last finding",
            "overflow:tasks:list",
            &["overflow:tasks:list"],
        ),
        (
            "room around a finding and a comment after it",
            "  overflow:tasks:list \t# since the board\n",
            &["overflow:tasks:list"],
        ),
        ("a comment that holds a second #", "a # b # c\n", &["a"]),
        ("a finding named twice", "b\na\nb\n", &["a", "b"]),
        ("line ends of another system", "a\r\nb\r\n", &["a", "b"]),
    ];
    for (row, text, ids) in rows {
        std::fs::write(&path, text).expect("the list writes");
        let read = layout_check::read_allow(&path)
            .unwrap_or_else(|error| panic!("{row}: the list reads: {error}"));
        let read: Vec<&str> = read.iter().map(String::as_str).collect();
        assert_eq!(read, ids, "{row}");
    }
    let missing = scratch.path().join("no such list");
    let refused =
        layout_check::read_allow(&missing).expect_err("a list that is not there is no empty list");
    assert!(
        refused.starts_with(&missing.display().to_string()),
        "the error names the list: {refused}"
    );
}

/// A run the command cannot read is refused before anything is laid out, saying what was wrong
/// with it: a `--dump` size that is no size, a flag left without its value, a flag it does not
/// have.
#[test]
fn a_run_the_command_cannot_read_is_refused_with_what_is_wrong_with_it() {
    use tilo_test_env::SpawnUnderEnvLock as _;

    let rows: [(&[&str], &str); 9] = [
        (
            &["--dump", "390"],
            "--dump 390: a window size such as 390x844",
        ),
        (&["--dump", ""], "--dump : a window size such as 390x844"),
        (
            &["--dump", "x844"],
            "--dump x844: a window size such as 390x844",
        ),
        (
            &["--dump", "390x"],
            "--dump 390x: a window size such as 390x844",
        ),
        (
            &["--dump", "-390x844"],
            "--dump -390x844: a window size such as 390x844",
        ),
        (
            &["--dump", "390x844x2"],
            "--dump 390x844x2: a window size such as 390x844",
        ),
        (&["--dump"], "--dump needs a value"),
        (&["--dump", "390x844", "--press"], "--press needs a value"),
        (&["--no-such-flag"], "unknown option --no-such-flag"),
    ];
    for (args, said) in rows {
        let output = Command::new(env!("CARGO_BIN_EXE_tilolang"))
            .arg("layout-check")
            .args(args)
            .output_locked()
            .expect("the command runs");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?} is refused as a run that cannot be read: {stderr}"
        );
        assert!(
            stderr.contains(said),
            "{args:?} says what is wrong with it ({said}): {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "{args:?} lays nothing out: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// The allow-list holds in both directions. A finding it names is allowed and no longer fails the
/// run, one it does not name stays new, and an id it names that no finding has is called stale, so
/// the list never keeps a problem that is gone.
#[test]
fn an_allow_list_allows_the_findings_it_names_and_says_which_of_them_are_gone() {
    let broken = manifest().join("tests/layout_check/broken");
    let unlisted = layout_check::check(&Options {
        dirs: vec![broken.clone()],
        ..Options::default()
    })
    .expect("the fixture lays out");
    assert!(
        unlisted.stale.is_empty() && unlisted.findings.iter().all(|finding| !finding.allowed),
        "a run with no allow-list allows nothing and calls nothing stale:\n{}",
        layout_check::render(&unlisted)
    );
    let ids: BTreeSet<&str> = unlisted
        .findings
        .iter()
        .map(|finding| finding.id.as_str())
        .collect();
    assert!(
        ids.len() >= 2,
        "the fixture breaks in more than one way: {ids:?}"
    );
    let named = *ids.first().expect("a finding");
    let gone = "overflow layoutbroken › no_such_view [Text]";
    assert!(!ids.contains(gone), "no finding is `{gone}`: {ids:?}");

    let scratch = tempfile::tempdir().expect("a scratch directory");
    let list = scratch.path().join("allowed");
    std::fs::write(
        &list,
        format!("# one that shows, one that is gone\n{named}\n{gone}\n"),
    )
    .expect("the list writes");
    let report = layout_check::check(&Options {
        dirs: vec![broken],
        allow: Some(list),
        ..Options::default()
    })
    .expect("the fixture lays out");
    let said = layout_check::render(&report);
    for finding in &report.findings {
        assert_eq!(
            finding.allowed,
            finding.id == named,
            "the list allows `{named}` and nothing else, and `{}` is judged otherwise:\n{said}",
            finding.id
        );
    }
    assert!(
        report.findings.iter().any(|finding| finding.allowed),
        "the finding the list names still shows, allowed:\n{said}"
    );
    let new: BTreeSet<&str> = report
        .new_findings()
        .map(|finding| finding.id.as_str())
        .collect();
    let mut unnamed = ids.clone();
    unnamed.remove(named);
    assert_eq!(
        new, unnamed,
        "every finding the list does not name is still new:\n{said}"
    );
    assert_eq!(
        report.stale,
        [gone],
        "the id no finding has is the one stale id:\n{said}"
    );
    assert!(
        said.contains(&format!("allowed {named}: ")),
        "the report marks the finding it allows:\n{said}"
    );
    assert!(
        said.contains(&format!(
            "note: the allow-list names `{gone}`, which no longer shows: take it out"
        )),
        "the report says which id to take out of the list:\n{said}"
    );
}

/// A state the run cannot stage stops it at the first window, saying what is wrong with it and
/// which unit it stopped: one written without a value, and one no view of the app declares.
#[test]
fn a_state_with_no_value_or_that_no_view_declares_is_refused_at_the_unit_it_stops() {
    let rows = [
        (
            "place",
            "--state place names no value: write `--state place=Task.today`",
        ),
        (
            "place=Sample.first",
            "--state place=Sample.first: no view of `so.tilo.lang.layoutbroken` declares the state \
             `place`",
        ),
    ];
    for (state, said) in rows {
        let refused = layout_check::check(&Options {
            dirs: vec![manifest().join("tests/layout_check/broken")],
            states: vec![state.to_owned()],
            ..Options::default()
        })
        .expect_err("a run that stages a state no window can take is refused");
        assert!(
            refused.contains(said),
            "`--state {state}` says what is wrong with it: {refused}"
        );
        assert!(
            refused.starts_with("layoutbroken: the scenario `a sample` "),
            "`--state {state}` names the unit it stopped: {refused}"
        );
    }
}
