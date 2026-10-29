//! `tilolang layout-check` through its library: a fixture app whose views are each laid out
//! wrong in one way is caught for every kind of problem, at the view that is wrong; and the
//! shipped apps show no problem their allow-list (`tests/layout_check/allowed`) does not
//! already name, so a new one fails here.

use std::path::{Path, PathBuf};

use tilolang_cli::layout_check::{self, Kind, Options};

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn app(name: &str) -> PathBuf {
    manifest().join("../../../apps/tl").join(name)
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
        (Kind::ClippedText, "clipped"),
        (Kind::Overlap, "overlapping"),
        (Kind::SmallHitTarget, "small_target"),
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
            Kind::ClippedText => "clipped",
            Kind::Overlap => "overlapping",
            Kind::SmallHitTarget => "small_target",
            Kind::ScrollsSideways => "sideways",
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
fn the_apps_show_no_layout_problem_their_allow_list_does_not_name() {
    let report = layout_check::check(&Options {
        dirs: ["notes", "tasks", "mail", "chat"].map(app).to_vec(),
        allow: Some(manifest().join("tests/layout_check/allowed")),
        ..Options::default()
    })
    .expect("the apps lay out");
    assert!(
        report.new_findings().next().is_none() && report.stale.is_empty(),
        "new layout problems, or allowed ones gone (take them out of the allow-list):\n{}",
        layout_check::render(&report)
    );
}
