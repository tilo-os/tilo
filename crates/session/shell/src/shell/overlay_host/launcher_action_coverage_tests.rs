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
        ("actions", "workspace.create"),
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

fn workspace_creation_chrome() -> crate::shell::chrome_state::Chrome {
    use crate::shell::state::{FreshId as _, Workspace, WorkspaceId};
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::new();
    let mut workspace = Workspace::new(id, "Existing");
    workspace.set_owner("shell-fixture");
    tree.desktop.workspaces.push(workspace);
    tree.desktop.active = id;
    ChromeState::new_with_fixture_handle(
        crate::shell::state::WorkspaceTreeHandle::new(tree),
        crate::shell::hosted_inputs::HostedTileInputs::default(),
    )
}

#[test]
#[serial_test::serial]
fn workspace_create_address_creates_and_selects_one_owned_empty_room() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let chrome = workspace_creation_chrome();
    let host = ShellOverlayHost::new(Box::new(Container::new()));
    let mut doc = AutoCommit::new();
    let address = ResourceRef::new("actions", "workspace.create");
    let catalog = host.action_catalog_for_resource(
        &address,
        &chrome.borrow(),
        &doc,
        &automerge::ROOT,
        &AllVisible,
    );
    let action = catalog
        .select(&address)
        .expect("creation resolves without a search");
    assert_eq!(action.params().len(), 1);
    assert_eq!(action.params()[0].name, "name");
    let contract = action.contract().unwrap();
    assert_eq!(contract.kind, tilo_actions::ActionKind::Undoable);
    assert_eq!(contract.retry, tilo_actions::RetryPolicy::AtMostOnce);
    let previous = chrome.borrow().tree().desktop.active;
    action
        .invoke_json(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
            serde_json::json!({"name": "Browser acceptance"})
                .as_object()
                .unwrap(),
        )
        .expect("owner creation succeeds");
    let state = chrome.borrow();
    let tree = state.tree();
    assert_eq!(tree.desktop.workspaces.len(), 2);
    assert_ne!(tree.desktop.active, previous);
    let created = tree.desktop.active_workspace().unwrap();
    assert_eq!(created.name, "Browser acceptance");
    assert_eq!(created.owner().map(String::as_str), Some("shell-fixture"));
    assert_ne!(created.room, tree.desktop.workspaces[0].room);
    assert!(
        created
            .strips
            .iter()
            .all(|strip| strip.window_groups.is_empty())
    );
    assert!(created.collaborators.is_empty());
    assert!(
        created.policy.members.is_empty(),
        "only the owner is present"
    );
    assert_eq!(
        created.policy.grants,
        tilo_action_policy::roles::room_role_grants(
            &created.room.to_string(),
            &tilo_action_policy::Subject::Role(tilo_action_policy::Role::Owner),
        ),
        "only the default role grants are present"
    );
    assert_eq!(tree.desktop.workspaces[0].name, "Existing");
}

#[test]
#[serial_test::serial]
fn workspace_create_refuses_denied_actors_revocation_and_stale_context() {
    use crate::shell::search::visibility::PolicyVisibility;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let chrome = workspace_creation_chrome();
    let host = ShellOverlayHost::new(Box::new(Container::new()));
    let mut doc = AutoCommit::new();
    let address = ResourceRef::new("actions", "workspace.create");
    {
        let state = chrome.borrow();
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace().unwrap();
        for actor in [
            workspace.actor_for_person(&"unrelated".to_string()),
            workspace.actor_for_agent("uninvited"),
        ] {
            let visibility = PolicyVisibility::new(workspace, actor);
            assert!(
                host.action_catalog_for_resource(
                    &address,
                    &state,
                    &doc,
                    &automerge::ROOT,
                    &visibility,
                )
                .select(&address)
                .is_none(),
                "denied actor has no creation door"
            );
        }
    }
    for stale_context in [false, true] {
        let catalog = host.action_catalog_for_resource(
            &address,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        {
            let state = chrome.borrow();
            let mut tree = state.tree_mut();
            if stale_context {
                let replacement = tree.append_empty_workspace("Replacement context");
                tree.desktop
                    .active_workspace_mut()
                    .unwrap()
                    .set_owner("shell-fixture");
                tree.set_active(replacement);
                tree.desktop
                    .active_workspace_mut()
                    .unwrap()
                    .set_owner("shell-fixture");
            } else {
                let workspace = tree.desktop.active_workspace_mut().unwrap();
                workspace.policy.owner = None;
                workspace.policy.members.clear();
            }
        }
        let count = chrome.borrow().tree().desktop.workspaces.len();
        assert!(
            catalog
                .select(&address)
                .unwrap()
                .invoke_nullary(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                )
                .is_err()
        );
        assert_eq!(chrome.borrow().tree().desktop.workspaces.len(), count);
    }
}

#[test]
#[serial_test::serial]
fn workspace_create_default_and_invalid_names_use_the_declared_action() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let chrome = workspace_creation_chrome();
    let host = ShellOverlayHost::new(Box::new(Container::new()));
    let mut doc = AutoCommit::new();
    let address = ResourceRef::new("actions", "workspace.create");
    for name in [String::new(), "\nprivate".to_string(), "a".repeat(121)] {
        let catalog = host.action_catalog_for_resource(
            &address,
            &chrome.borrow(),
            &doc,
            &automerge::ROOT,
            &AllVisible,
        );
        assert!(
            catalog
                .select(&address)
                .unwrap()
                .invoke_json(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &automerge::ROOT,
                    &chrome,
                    serde_json::json!({"name": name}).as_object().unwrap(),
                )
                .is_err()
        );
        assert_eq!(chrome.borrow().tree().desktop.workspaces.len(), 1);
    }
    let catalog = host.action_catalog_for_resource(
        &address,
        &chrome.borrow(),
        &doc,
        &automerge::ROOT,
        &AllVisible,
    );
    catalog
        .select(&address)
        .unwrap()
        .invoke_nullary(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
            &chrome,
        )
        .unwrap();
    assert_eq!(
        chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .name,
        crate::shell::i18n::t("search-action-workspace-create-default-name")
    );
}
