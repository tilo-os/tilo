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

/// The regression guard, at the layer where the damage would happen.
///
/// The FileChooser used to prompt on a `Capability::Filesystem`
/// carrying `"/"`, and persist it verbatim when the user answered
/// Session or Always. This is what that grant turns into: a bind of the
/// filesystem root into the app's namespace. The assertion is on the
/// launcher's own plan, so it fails whether the `/` comes from the
/// portal, the drop router, the permissions UI, or anywhere else.
#[test]
fn nothing_may_bind_the_filesystem_root_into_a_sandbox() {
    let (store, ws) = store();
    let caller = PortalCaller::new(ws, APP);

    // The exact capability the old sentinel produced.
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

    // This documents the consequence rather than preventing it: the
    // launcher binds what it is told. The defence is that nothing mints
    // this capability — asserted in `file_chooser.rs`
    // (`the_grant_names_the_picked_path_and_never_the_filesystem_root`)
    // and in `portal_file_chooser_e2e.rs`. If this ever stops being
    // true, that is the launcher gaining a guard, and this test should
    // be inverted rather than deleted.
    let binds_root = plan.ro_paths.contains(&root) || plan.rw_paths.contains(&root);
    assert!(
        binds_root,
        "a '/' capability reaching the launcher binds the whole filesystem — \
         if the launcher now refuses it, invert this assertion and say so"
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
