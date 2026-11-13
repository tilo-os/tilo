//! What the desktop publishes: one verb vocabulary per resource, every verb declared once, and
//! every row offering a verb of its resource's.

#![allow(clippy::unwrap_used, clippy::panic, reason = "a test's assertions")]

use std::collections::BTreeSet;
use std::fmt::Write as _;

use tilo_actions::ResourceSpec;
use tilo_desktop::desk_verbs::launchpad_resource;
use tilo_desktop::desk_verbs::{
    CHROME_RESOURCE, DESK_RESOURCE, OpenSearch, chrome_resource, desk_resource, tab_resource_id,
};
use tilo_desktop_surface::SearchRow;
use tilo_desktop_surface::navigation::{ChromeEffect, DeskEffect};
use tilo_desktop_surface::window;
use tilo_resource::Effect;

/// Every resource the desktop publishes: the chrome over the windows, the desk under them with its
/// windows, a display and the open search, and the launchpad.
fn published() -> Vec<ResourceSpec> {
    let windows = [
        tilo_desktop::desk_verbs::Shown {
            tab: 7,
            title: "Notes".to_owned(),
            foreign: None,
        },
        tilo_desktop::desk_verbs::Shown {
            tab: 8,
            title: "Mail".to_owned(),
            foreign: Some("so.tilo.mail".to_owned()),
        },
    ];
    let rows = [SearchRow {
        title: "Scratch".to_owned(),
        subtitle: None,
        kind: "Apps".to_owned(),
        facts: Vec::new(),
    }];
    vec![
        chrome_resource(&ui::shortcut_table::ShortcutConfig::default()),
        desk_resource(
            &windows,
            &[8],
            &[1],
            Some(OpenSearch {
                query: "scr",
                rows: &rows,
                searching: false,
            }),
        ),
        launchpad_resource(&tilo_desktop::launchpad::LaunchpadAction::ALL),
    ]
}

/// The resources the desktop publishes, each with the verbs it offers, as one text a change to the
/// catalogue shows in review.
fn catalogue() -> String {
    let mut catalogue = String::new();
    for resource in published() {
        write_resource(&mut catalogue, &resource, 0);
    }
    catalogue
}

/// One resource of the catalogue: its address, its kind and its label, then every verb it offers as
/// `id label` , then its children, indented.
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
    // A resource names a verb by its id: two rows of one resource offering the same id leaves the
    // second one unaddressable.
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
        for spec in actions(&resource) {
            let action = spec.id.as_str();
            let read = matches!(
                spec.action_type().operation_type.as_deref(),
                Some(window::LOOK | window::READ)
            );
            assert!(
                read || DeskEffect::declaration(action).is_some()
                    || ChromeEffect::declaration(action).is_some(),
                "{} offers {action}, which neither vocabulary declares",
                resource.id.as_str()
            );
        }
    }
}

#[test]
fn the_launchpad_cards_are_its_controls_for_scripts() {
    // A card submits the launchpad verb for what the empty desk shows, and a script or an agent
    // invokes the same verb by the row this resource publishes. People reach the verbs through
    // their host actions, which is what the palette lists, so this resource is never the palette's
    // door.
    let launchpad = launchpad_resource(&tilo_desktop::launchpad::LaunchpadAction::ALL);
    assert!(
        !launchpad.children.is_empty(),
        "the launchpad has its cards"
    );
    for card in &launchpad.children {
        for action in &card.actions {
            assert!(
                !action.reaches_people(),
                "{} ({}) is listed for people",
                card.id.as_str(),
                action.id
            );
            assert!(
                action.reaches_agents(),
                "{} ({}) stays a control scripts can run",
                card.id.as_str(),
                action.id
            );
        }
    }
}

/// A window this session does not run — another desktop's — is read and pictured through the desk,
/// and nothing acts on it: the two verbs it adds only read, and reach agents, who have no other way
/// to see it.
#[test]
fn a_foreign_window_offers_the_two_reads_and_nothing_that_acts() {
    let desk = published()
        .into_iter()
        .find(|resource| resource.id.as_str() == DESK_RESOURCE)
        .expect("the desk");
    let window = desk
        .children
        .iter()
        .find(|resource| resource.id.as_str() == tab_resource_id(8))
        .expect("the foreign window's row");
    let reads: Vec<String> = window
        .actions
        .iter()
        .filter(|spec| {
            matches!(
                spec.action_type().operation_type.as_deref(),
                Some(window::LOOK | window::READ)
            )
        })
        .map(|spec| spec.id.clone())
        .collect();
    assert_eq!(
        reads,
        ["look", "read"],
        "the foreign window is read and pictured, and its own verbs are these"
    );
    for spec in &window.actions {
        let ty = spec.action_type();
        if ty.operation_type.as_deref() != Some(window::LOOK)
            && ty.operation_type.as_deref() != Some(window::READ)
        {
            continue;
        }
        assert_eq!(
            ty.contract.kind,
            tilo_actions::ActionKind::Read,
            "{} reads and nothing else",
            spec.id
        );
        assert_eq!(
            spec.audience(),
            tilo_actions::Audience::Agents,
            "{} is offered to whoever cannot see the window otherwise",
            spec.id
        );
    }
    // The desk's own verbs are the only ones it adds to a window it runs.
    let window = published()
        .into_iter()
        .find(|resource| resource.id.as_str() == DESK_RESOURCE)
        .and_then(|desk| {
            desk.children
                .into_iter()
                .find(|resource| resource.id.as_str() == tab_resource_id(7))
        })
        .expect("the session's own window");
    assert!(
        window.actions.iter().all(|spec| {
            spec.action_type().operation_type.as_deref() != Some(window::LOOK)
                && spec.action_type().operation_type.as_deref() != Some(window::READ)
        }),
        "a window the session runs says what it shows itself"
    );
}

/// Every action `resource` and its children offer.
fn actions(resource: &ResourceSpec) -> Vec<tilo_actions::ActionSpec> {
    let mut found: Vec<tilo_actions::ActionSpec> = resource.actions.clone();
    for child in &resource.children {
        found.extend(actions(child));
    }
    found
}
