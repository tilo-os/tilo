//! Every public widget of the toolkit is a described component, which tilolang reaches as
//! `ui.<Name>` the day it is written, or says where it is declared why it is not
//! (`#[not_a_component(kind, "why")]`). A public widget that does neither fails here.

#[path = "../../component_scan.rs"]
#[allow(
    dead_code,
    reason = "the build script reads the parts of the scan this check does not"
)]
mod component_scan;

use std::path::Path;

fn scan() -> component_scan::Scan {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    component_scan::scan(&src).expect("the toolkit's sources read")
}

/// Widgets written before the registry was generated that are not described yet. The list only
/// shrinks: a widget described since must leave it, and no widget may join it.
const NOT_YET_DESCRIBED: &str = include_str!("not_yet_described.txt");

#[test]
fn every_public_widget_is_a_component_or_says_why_not() {
    let scan = scan();
    let owed: Vec<&str> = NOT_YET_DESCRIBED.lines().map(str::trim).collect();
    let paid: Vec<&str> = owed
        .iter()
        .copied()
        .filter(|name| {
            !scan
                .widgets
                .iter()
                .any(|w| w.name == *name && w.exempt.is_none() && !scan.is_described(&w.name))
        })
        .collect();
    assert!(
        paid.is_empty(),
        "described now (or gone, or marked not a component), so remove from \
         component/not_yet_described.txt:\n{}",
        paid.join("\n")
    );
    let missing: Vec<String> = scan
        .widgets
        .iter()
        .filter(|w| w.exempt.is_none() && !scan.is_described(&w.name))
        .filter(|w| !owed.contains(&w.name.as_str()))
        .map(|w| format!("  {} ({})", w.name, w.file.display()))
        .collect();
    assert!(
        missing.is_empty(),
        "{} public widgets are neither described nor say why not. Describe each \
         (`#[derive(Component)]` props and `impl Described`, see `tilo_ui::component`), and \
         tilolang reaches it as `ui.<Name>`; or mark it `#[not_a_component(kind, \"why\")]`:\n{}",
        missing.len(),
        missing.join("\n")
    );
}

#[test]
fn a_described_widget_does_not_also_say_it_is_not_one() {
    let scan = scan();
    for w in &scan.widgets {
        if let Some((kind, reason)) = &w.exempt {
            assert!(
                !scan.is_described(&w.name),
                "{} is described and also marked not a component ({kind}: {reason})",
                w.name
            );
            assert!(
                !reason.trim().is_empty(),
                "{} says why it is not a component",
                w.name
            );
        }
    }
}

/// The scan reads what the sources declare: the registry it generates is the catalogue, and
/// the widgets it finds include those every app uses, so the checks above are not vacuous.
#[test]
fn the_scan_finds_the_toolkits_declarations() {
    let scan = scan();
    for name in ["Button", "Card", "NavShell", "Toggle", "Slider"] {
        assert!(
            scan.widgets.iter().any(|w| w.name == name),
            "{name} is found as a public widget"
        );
    }
    assert!(scan.is_described("Button"));
    let catalogue = super::catalogue();
    let mut generated: Vec<&str> = catalogue.names();
    generated.sort_unstable();
    let mut declared: Vec<String> = scan
        .described
        .iter()
        .map(|d| d.props.clone())
        .chain(scan.custom.iter().flat_map(|c| c.props.clone()))
        .chain(scan.parts().into_iter().map(|p| p.name.clone()))
        .collect();
    declared.sort_unstable();
    assert_eq!(
        declared.len(),
        generated.len(),
        "the catalogue holds one component per declared props: {declared:?} against {generated:?}"
    );
}
