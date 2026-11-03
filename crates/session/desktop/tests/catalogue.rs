//! What the desktop publishes: one verb vocabulary per resource, every verb
//! declared once, and every row offering a verb of its resource's.

#![allow(clippy::unwrap_used, clippy::panic, reason = "a test's assertions")]

use std::collections::BTreeSet;
use std::fmt::Write as _;

use tilo_actions::ResourceSpec;
use tilo_desktop::desk_verbs::launchpad_resource;
use tilo_desktop::desk_verbs::{
    CHROME_RESOURCE, DESK_RESOURCE, OpenSearch, chrome_resource, desk_resource,
};
use tilo_desktop_surface::SearchRow;
use tilo_desktop_surface::navigation::{ChromeEffect, DeskEffect};
use tilo_resource::Effect;

/// Every resource the desktop publishes: the chrome over the windows, the
/// desk under them with its windows, a display and the open search, and the
/// launchpad.
fn published() -> Vec<ResourceSpec> {
    let tabs = [(7, "Notes".to_owned()), (8, "Mail".to_owned())];
    let rows = [SearchRow {
        title: "Scratch".to_owned(),
        subtitle: None,
        kind: "Apps".to_owned(),
        facts: Vec::new(),
    }];
    vec![
        chrome_resource(),
        desk_resource(
            &tabs,
            &[1],
            Some(OpenSearch {
                query: "scr",
                rows: &rows,
                searching: false,
            }),
        ),
        launchpad_resource(),
    ]
}

/// Every action the resource tree offers, with the resource offering it.
fn offered(resource: &ResourceSpec, into: &mut Vec<(String, String)>) {
    for action in &resource.actions {
        into.push((resource.id.as_str().to_owned(), action.id.clone()));
    }
    for child in &resource.children {
        offered(child, into);
    }
}

/// The resources the desktop publishes, each with the verbs it offers, as
/// one text a change to the catalogue shows in review.
fn catalogue() -> String {
    let mut catalogue = String::new();
    for resource in published() {
        write_resource(&mut catalogue, &resource, 0);
    }
    catalogue
}

/// One resource of the catalogue: its address, its kind and its label, then
/// every verb it offers as `id label`, then its children, indented.
fn write_resource(catalogue: &mut String, resource: &ResourceSpec, depth: usize) {
    let indent = "  ".repeat(depth);
    let _ = writeln!(
        catalogue,
        "{indent}{} {} ({})",
        resource.id.as_str(),
        resource.kind,
        resource.label
    );
    let mut verbs: Vec<String> = resource
        .actions
        .iter()
        .map(|spec| format!("{indent}  {} {}\n", spec.id, spec.label))
        .collect();
    verbs.sort_unstable();
    for verb in verbs {
        catalogue.push_str(&verb);
    }
    for child in &resource.children {
        write_resource(catalogue, child, depth.saturating_add(1));
    }
}

#[test]
fn the_catalogue_is_what_the_desktop_publishes() {
    let expected = include_str!("catalogue.txt");
    assert_eq!(catalogue(), expected);
}

#[test]
fn no_verb_is_declared_twice() {
    let mut seen = BTreeSet::new();
    for decl in DeskEffect::declarations() {
        assert!(seen.insert(decl.id), "{} is declared twice", decl.id);
    }
    for decl in ChromeEffect::declarations() {
        assert!(seen.insert(decl.id), "{} is declared twice", decl.id);
    }
}

#[test]
fn a_resource_offers_each_verb_once() {
    // A resource names a verb by its id: two rows of one resource offering
    // the same id leaves the second one unaddressable.
    for resource in published() {
        let mut seen = BTreeSet::new();
        for action in &resource.actions {
            assert!(
                seen.insert(action.id.as_str()),
                "{} offers {} twice",
                resource.id.as_str(),
                action.id
            );
        }
    }
}

#[test]
fn the_desks_and_the_chromes_verbs_are_told_apart() {
    assert_eq!(DESK_RESOURCE, "desktop:desk");
    assert_eq!(CHROME_RESOURCE, "desktop:chrome");
    let desk: BTreeSet<&str> = DeskEffect::declarations()
        .iter()
        .map(|decl| decl.id)
        .collect();
    let chrome: BTreeSet<&str> = ChromeEffect::declarations()
        .iter()
        .map(|decl| decl.id)
        .collect();
    assert!(desk.is_disjoint(&chrome));
}

#[test]
fn every_row_offers_a_verb_of_the_desktop() {
    for resource in published() {
        let mut actions = Vec::new();
        offered(&resource, &mut actions);
        for (at, action) in actions {
            assert!(
                DeskEffect::declaration(&action).is_some()
                    || ChromeEffect::declaration(&action).is_some(),
                "{at} offers {action}, which neither vocabulary declares"
            );
        }
    }
}
