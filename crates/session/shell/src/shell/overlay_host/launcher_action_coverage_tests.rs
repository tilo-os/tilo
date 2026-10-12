//! Everything the launcher shows is a typed action reachable by its address.
//!
//! The palette and the control sockets read one provider registry, so a row a
//! person can activate is a `(provider, id)` an agent can invoke. These tests
//! hold that over the production registry: every row a query lists resolves
//! on its own, with no search before it, and the launcher's own operations —
//! opening apps, settings, switching workspaces, locking and logging out —
//! are among them.

use super::*;
use crate::shell::chrome_state::ChromeState;
use crate::shell::search::action_api::{AllVisible, ResourceRef};
use crate::shell::state::WorkspaceTree;
use ui::widget::Container;

/// Queries a person types into the launcher, from nothing to a word.
const QUERIES: &[&str] = &[
    "",
    "open",
    "settings",
    "lock",
    "log out",
    "workspace",
    "new",
    "a",
    "e",
];

#[test]
#[serial_test::serial]
fn every_listed_launcher_row_resolves_by_its_address_alone() {
    let scratch = tempfile::tempdir().expect("scratch home");
    let home = scratch.path().join("home");
    std::fs::create_dir_all(home.join("Documents")).expect("documents");
    std::fs::write(home.join("Documents/coverage-report.md"), "# coverage").expect("document");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", home.join(".config"));
    env.set("XDG_DATA_HOME", home.join(".local/share"));
    env.set("XDG_STATE_HOME", home.join(".local/state"));
    env.set("XDG_CACHE_HOME", home.join(".cache"));

    let host = ShellOverlayHost::new(Box::new(Container::new()));
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    let state = chrome.borrow();
    let doc = AutoCommit::new();

    let mut listed: Vec<ResourceRef> = Vec::new();
    for query in QUERIES {
        for action in host
            .action_catalog(query, &state, &doc, &automerge::ROOT, &AllVisible)
            .enumerate()
        {
            if !listed.contains(&action.resource) {
                listed.push(action.resource);
            }
        }
    }
    assert!(!listed.is_empty(), "the launcher lists nothing");

    let unresolved: Vec<String> = listed
        .iter()
        .filter(|resource| {
            host.action_catalog_for_resource(resource, &state, &doc, &automerge::ROOT, &AllVisible)
                .select(resource)
                .is_none()
        })
        .map(|resource| format!("{}:{}", resource.provider, resource.id))
        .collect();
    assert!(
        unresolved.is_empty(),
        "listed in the launcher but not invocable by address: {unresolved:?}"
    );

    // The launcher's own operations are among them.
    let has = |provider: &str, id: &str| listed.contains(&ResourceRef::new(provider, id));
    for (provider, id) in [
        ("session", "lock"),
        ("session", "logout"),
        ("actions", "settings.open"),
        ("actions", "workspace.next"),
        ("actions", "workspace.previous"),
    ] {
        assert!(has(provider, id), "the launcher offers no {provider}:{id}");
    }
    assert!(
        listed.iter().any(|resource| resource.provider == "actions"
            && resource.id.starts_with("workspace.select.")),
        "switching to a named workspace is an action"
    );
}

/// An address a person never searched for is still an address: the palette's
/// last scope pill and the last listing do not decide what resolves.
#[test]
#[serial_test::serial]
fn resolution_and_listing_ignore_the_palettes_scope_pill() {
    let host = ShellOverlayHost::new(Box::new(Container::new()));
    let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
    chrome.borrow_mut().search_scope = crate::shell::chrome_state::SearchScope::ActiveApp;
    let state = chrome.borrow();
    let doc = AutoCommit::new();
    let lock = ResourceRef::new("session", "lock");
    assert!(
        host.action_catalog_for_resource(&lock, &state, &doc, &automerge::ROOT, &AllVisible)
            .select(&lock)
            .is_some()
    );
    assert!(
        host.action_catalog("lock", &state, &doc, &automerge::ROOT, &AllVisible)
            .contains(&lock),
        "an agent's listing is not narrowed by the pill a person left selected"
    );
}
