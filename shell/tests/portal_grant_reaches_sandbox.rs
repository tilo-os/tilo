//! The other half of "the app can open the file": what happens to the
//! grant the dialog minted.
//!
//! `portal_file_chooser_e2e.rs` proves the app is handed a path it can
//! read *now*. This proves the grant that came with it is the thing the
//! sandbox is actually built from — that the chain
//!
//! ```text
//!   dialog pick
//!     → Workspace::capabilities          (the portal's grant store)
//!     → launcher::capabilities_for_app   (what the compositor publishes)
//!     → CapabilityPlan                   (what becomes bwrap argv + Landlock)
//! ```
//!
//! carries the picked path end to end, and carries *only* it.
//!
//! Why this matters as a separate test: the mount namespace of a running
//! sandboxed app is fixed at launch, so a grant made mid-session cannot
//! change it. What the grant does instead is decide the *next* launch.
//! If the grant were dropped, mis-scoped, or widened anywhere along this
//! chain, an app would open a file once through the portal and then find
//! it gone on restart — or, in the failure this branch removes, find the
//! entire filesystem bound in.

use std::path::PathBuf;

use launcher::capabilities::CapabilityPlan;
use shell::portal::{GrantStoreHandle, PortalCaller};
use shell::shell::launcher as shell_launcher;
use shell::shell::state::{
    Capability, FsMode, GRANT_SESSION, Workspace, WorkspaceId, WorkspaceTree,
};

const APP: &str = "dev.qos.editor";

/// A workspace owned by the local user, plus a grant store over it.
fn store() -> (GrantStoreHandle, WorkspaceId) {
    let mut tree = WorkspaceTree::default();
    let id = WorkspaceId::from(1);
    tree.workspaces.push(Workspace::new(id, "demo"));
    tree.set_active(id);
    let handle = tree.shared();
    {
        let mut t = handle.write();
        let owner = shell::shell::state::local_user_id();
        t.adopt_ownerless_workspaces(&owner);
    }
    (GrantStoreHandle::new(handle), id)
}

/// The plan the launcher would build for `app`, from the capabilities
/// the compositor publishes for the workspace. This is the production
/// pair: `set_workspace_capabilities` is what `launch_in_workspace`
/// calls, `capabilities_for_app` is what `launch_with_document` reads,
/// and `CapabilityPlan::for_app` is what turns the result into binds.
fn plan_for(store: &GrantStoreHandle, ws: WorkspaceId, app: &str) -> CapabilityPlan {
    let caps = {
        let tree = store.tree_handle();
        let guard = tree.read();
        guard
            .workspaces
            .iter()
            .find(|w| w.id == ws)
            .expect("workspace")
            .capabilities
            .clone()
    };
    shell_launcher::set_workspace_capabilities(caps);
    let published = shell_launcher::capabilities_for_app(app);
    CapabilityPlan::for_app(app, &published, None)
}

#[test]
fn an_opened_file_is_bound_read_only_at_the_next_launch() {
    let (store, ws) = store();
    let caller = PortalCaller::new(ws, APP);
    let picked = PathBuf::from("/home/someone/Documents/report.odt");

    // Exactly what `FileChooserBackend::pick_and_grant` records for an
    // OpenFile whose dialog the user confirmed.
    store.insert(
        &caller,
        Capability::Filesystem {
            path: picked.clone(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );

    let plan = plan_for(&store, ws, APP);

    assert!(
        plan.ro_paths.contains(&picked),
        "the picked file must be bound read-only into the sandbox; \
         ro={:?} rw={:?}",
        plan.ro_paths,
        plan.rw_paths
    );
    assert!(
        !plan.rw_paths.contains(&picked),
        "an Open must not produce a writable bind; rw={:?}",
        plan.rw_paths
    );
}

#[test]
fn a_saved_file_is_bound_writable_at_the_next_launch() {
    let (store, ws) = store();
    let caller = PortalCaller::new(ws, APP);
    let picked = PathBuf::from("/home/someone/Documents/new-notes.md");

    store.insert(
        &caller,
        Capability::Filesystem {
            path: picked.clone(),
            mode: FsMode::ReadWrite,
            scope: GRANT_SESSION,
        },
    );

    let plan = plan_for(&store, ws, APP);

    assert!(
        plan.rw_paths.contains(&picked),
        "a Save must be bound writable, or the app's next save fails on a \
         read-only mount; ro={:?} rw={:?}",
        plan.ro_paths,
        plan.rw_paths
    );
}

/// The launcher refuses to bind the filesystem root, and must keep
/// refusing.
///
/// `launcher::capabilities::is_bindable_path` rejects `"/"` outright
/// (`path.is_absolute() && raw != "/"`), so the old `"/"` sentinel never
/// reached bwrap as a root bind. That is defence in depth and it is
/// load-bearing — this test is what stops the guard being relaxed on the
/// grounds that "nothing mints a `/` grant anyway".
#[test]
fn the_launcher_refuses_to_bind_the_filesystem_root() {
    let (store, ws) = store();
    let caller = PortalCaller::new(ws, APP);

    // The exact capability the removed sentinel produced.
    store.insert(
        &caller,
        Capability::Filesystem {
            path: PathBuf::from("/"),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );

    let plan = plan_for(&store, ws, APP);
    let root = PathBuf::from("/");
    assert!(
        !plan.ro_paths.contains(&root) && !plan.rw_paths.contains(&root),
        "the launcher must never bind '/' into a sandbox; ro={:?} rw={:?}",
        plan.ro_paths,
        plan.rw_paths
    );
}

/// Why a `"/"` grant is dangerous even though the launcher refuses to
/// bind it: `Scope::FsPath("/")` is a prefix match over every path, so
/// every compositor-side check that asks "has this app already been
/// granted this?" answers yes for anything — the same predicate
/// `fs/src/acl.rs` answers `open`/`read`/`write` with, and the one
/// `flow::resolve` consults before deciding whether to prompt.
#[test]
fn a_root_grant_makes_every_path_read_as_already_granted() {
    let (store, ws) = store();
    let caller = PortalCaller::new(ws, APP);

    let secret = std::path::Path::new("/home/someone/.ssh/id_ed25519");
    assert!(
        !store.filesystem_path_granted(&caller, secret),
        "precondition: the app starts with nothing granted"
    );

    store.insert(
        &caller,
        Capability::Filesystem {
            path: PathBuf::from("/"),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );

    assert!(
        store.filesystem_path_granted(&caller, secret),
        "a '/' grant covers its whole subtree — this is the blast radius \
         the FileChooser sentinel used to record on an 'Always' click, and \
         the reason the dialog now grants only the path the user picked"
    );
}

/// A grant for one file must not drag its neighbours in.
#[test]
fn a_grant_covers_the_named_path_and_nothing_beside_it() {
    let (store, ws) = store();
    let caller = PortalCaller::new(ws, APP);
    let picked = PathBuf::from("/home/someone/Documents/report.odt");
    store.insert(
        &caller,
        Capability::Filesystem {
            path: picked.clone(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );

    let plan = plan_for(&store, ws, APP);

    assert_eq!(
        plan.ro_paths,
        vec![picked],
        "one pick is one bind — no parent directory, no siblings"
    );
    assert!(plan.rw_paths.is_empty());
    assert!(
        !plan.allow_network,
        "a file grant must not carry network access"
    );
    assert!(plan.devices.is_empty(), "a file grant must not carry devices");
}

/// Two different apps in one workspace do not inherit each other's picks.
#[test]
fn a_grant_is_scoped_to_the_app_that_asked() {
    let (store, ws) = store();
    let picked = PathBuf::from("/home/someone/Documents/private.odt");
    store.insert(
        &PortalCaller::new(ws, APP),
        Capability::Filesystem {
            path: picked.clone(),
            mode: FsMode::Read,
            scope: GRANT_SESSION,
        },
    );

    let other = plan_for(&store, ws, "dev.qos.other");
    assert!(
        !other.ro_paths.contains(&picked),
        "an app that did not open the dialog must not receive the grant; \
         got {:?}",
        other.ro_paths
    );
}
