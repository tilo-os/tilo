//! Every component of the toolkit an app can place is reachable from tilolang as `ui.<Name>`,
//! with the props, slots and events the toolkit describes; and a public app-facing widget the
//! toolkit does not describe yet says why, in a list that only shrinks
//! (`crates/sdk/ui-core/src/component/not_yet_described.txt`).
//!
//! The widget list is read from the toolkit's own sources (`component_scan.rs`, the same reading
//! that generates the component registry), never hand-kept: a widget the toolkit gains, or a
//! component it describes that tilolang does not reach, fails here. Plumbing is out by rule, not
//! by name: a widget counts only when it is public through the toolkit's widget tree (`widget`,
//! `search`, what an app draws with), and the toolkit says where each host, root and part sits
//! outside a view (`#[not_a_component(kind, "why")]` at the widget).
//!
//! That every *described* component lays out from a view is held by the gallery
//! (`crates/lang/tilolang-cli/tests/layout_check.rs`); this holds the components the toolkit has
//! not described yet to a reason.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[path = "../../../../sdk/ui-core/component_scan.rs"]
#[allow(
    dead_code,
    reason = "the build script reads the parts of the scan this check does not"
)]
mod component_scan;

fn toolkit_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/ui-core/src")
}

/// The components tilolang reaches as `ui.<Name>`: the toolkit's catalogue, which is what the
/// checker reads a `ui.<Name>(...)` call against.
fn bound() -> BTreeSet<String> {
    tilolang::toolkit::catalogue()
        .names()
        .into_iter()
        .map(str::to_owned)
        .collect()
}

/// What the toolkit's sources declare: its public widgets and the ones it describes.
fn toolkit() -> component_scan::Scan {
    component_scan::scan(&toolkit_src()).expect("the toolkit's sources read")
}

/// The ledger: each public app-facing widget the toolkit has not described yet, with the reason
/// its line gives, and the line it was written on for messages.
fn ledger() -> Vec<(String, String)> {
    let path = toolkit_src().join("component/not_yet_described.txt");
    let text = std::fs::read_to_string(&path).expect("the toolkit's ledger reads");
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (name, why) = line.split_once('—').unwrap_or_else(|| {
                panic!("`{line}` in {} names no reason after `—`", path.display());
            });
            (name.trim().to_owned(), why.trim().to_owned())
        })
        .collect()
}

#[test]
fn every_app_facing_widget_is_bound_or_says_why_not() {
    let scan = toolkit();
    let bound = bound();
    let owed: BTreeSet<String> = ledger().into_iter().map(|(name, _)| name).collect();
    let unaccounted: Vec<String> = scan
        .widgets
        .iter()
        .filter(|w| w.exempt.is_none())
        // A widget reaches a view by the name its component is described by: `IconWidget` is
        // `ui.Icon`, `ReactionStripWidget` is `ui.Reactions`.
        .map(|w| {
            scan.component_of_widget(&w.name)
                .unwrap_or_else(|| w.name.clone())
        })
        .filter(|name| !bound.contains(name) && !owed.contains(name))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "{} public app-facing widget(s) are neither reachable from tilolang as `ui.<Name>` nor \
         listed with a reason in crates/sdk/ui-core/src/component/not_yet_described.txt: {}. \
         Describe each widget (`#[derive(Component)]` props and `impl Described`, see \
         `tilo_ui::component`) so every app can place it from a view, or say where it is \
         declared why a view never places it (`#[not_a_component(kind, \"why\")]`).",
        unaccounted.len(),
        unaccounted.join(", ")
    );
}

#[test]
fn the_ledger_only_shrinks_and_names_widgets_that_are_really_out() {
    let scan = toolkit();
    let bound = bound();
    let mut stale = Vec::new();
    for (name, why) in ledger() {
        assert!(
            !why.is_empty(),
            "`{name}` says why it is not reachable from tilolang yet"
        );
        let widget = scan.widgets.iter().find(|w| w.name == name);
        let component = scan
            .component_of_widget(&name)
            .unwrap_or_else(|| name.clone());
        if bound.contains(&name) || bound.contains(&component) || scan.is_described(&name) {
            stale.push(format!(
                "{name}: the toolkit describes it, so `ui.{name}` is bound"
            ));
        } else {
            match widget {
                None => stale.push(format!("{name}: the toolkit declares no such widget")),
                Some(w) if w.exempt.is_some() => stale.push(format!(
                    "{name}: the toolkit says where it is declared that it is not a component"
                )),
                Some(_) => {}
            }
        }
    }
    assert!(
        stale.is_empty(),
        "remove these from crates/sdk/ui-core/src/component/not_yet_described.txt, which only \
         shrinks:\n{}",
        stale.join("\n")
    );
}

/// The ledger holds no component the toolkit has not written yet: the catalogue and the toolkit's
/// declarations are one reading, so a component bound without a described widget cannot hide.
#[test]
fn every_bound_component_is_one_the_toolkit_declares() {
    let declared: BTreeSet<String> = toolkit().component_names().into_iter().collect();
    let undeclared: Vec<String> = bound()
        .into_iter()
        .filter(|name| !declared.contains(name))
        .collect();
    assert!(
        undeclared.is_empty(),
        "the catalogue names components the toolkit's sources do not declare: {}",
        undeclared.join(", ")
    );
}
