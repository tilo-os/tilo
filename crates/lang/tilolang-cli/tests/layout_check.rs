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
            // takes a finger's room on touch (`small_target` is laid out right).
            Kind::ClippedText | Kind::SmallHitTarget => "",
            Kind::ScrollsSideways => "sideways",
            // Layout snaps every edge, broken or not.
            Kind::OffPixel => "(no view)",
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

/// Whether `source` names the component `name` as `ui.<name>`, not as the start of a longer
/// name (`ui.Card` is not `ui.CardGrid`).
fn names_component(source: &str, name: &str) -> bool {
    let wanted = format!("ui.{name}");
    source.match_indices(&wanted).any(|(at, _)| {
        !source
            .get(at.saturating_add(wanted.len())..)
            .and_then(|rest| rest.chars().next())
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// The gallery app shows every component of the toolkit, as tilolang reaches it (`ui.<Name>`,
/// from the toolkit's generated catalogue), and lays out headlessly with each of them built:
/// so every public component compiles and renders from tilolang, and a component added to the
/// toolkit fails here until the gallery shows it. (The check fails on a component that did
/// not build; the apps' check above holds the gallery's layout to the others' bar.)
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
    let missing: Vec<&str> = tilolang::toolkit::catalogue()
        .names()
        .into_iter()
        .filter(|name| !names_component(&source, name))
        .collect();
    assert!(
        missing.is_empty(),
        "the gallery (apps/tl/gallery) shows no `ui.{{Name}}` for: {}",
        missing.join(", ")
    );
    let report = layout_check::check(&Options {
        dirs: vec![dir],
        ..Options::default()
    })
    .expect("the gallery lays out, every component of it built");
    assert!(report.layouts > 0, "the gallery is laid out");
}
