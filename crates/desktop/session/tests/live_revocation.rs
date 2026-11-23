//! The running shell's file access list, as the Files daemon mounting a workspace is told it: the
//! session tells it on the daemon's connection as the daemon connects and again when it changes,
//! so taking a folder or a member's role away reaches the mount at its next open instead of
//! waiting for the next login. The daemon signs what it is told into the workspace authority every
//! mount enforces.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session lives until the \
              mount it serves is done"
)]

use crate::common::files::{Mount, mount_on};
use crate::common::people::{Device, Site, waited};
use tilo_server_protocol::workspaces::Role;

/// A runtime the mount's sync runs on, entered for as long as the test holds it.
fn mounts_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime for the mount")
}

/// The list the mount was last told, once it was told one.
fn access_list(mount: &Mount) -> Option<collab_types::FsAuthority> {
    let told = mount.service().access().borrow().clone()?;
    Some(serde_json::from_str(&told).expect("the list the mount is told reads"))
}

/// Wait until the list `mount` was told holds `arrived`, and answer it.
fn told_until(
    what: &str,
    (site, device): (&Site, &Device),
    mount: &Mount,
    arrived: impl Fn(&collab_types::FsAuthority) -> bool,
) -> collab_types::FsAuthority {
    waited(what, site, &[device], || {
        access_list(mount).is_some_and(|list| arrived(&list))
    });
    access_list(mount).expect("the list the mount was told")
}

/// A file of `workspace`, under the tree the device's files keep that workspace's own in: the
/// paths its roles' access ranges over.
fn note(workspace: &str) -> String {
    format!(
        "{}/{}/{workspace}/brief/plan.md",
        tilo_paths::WORKSPACE_MOUNT,
        tilo_paths::WORKSPACE_TREES_DIR
    )
}

/// Can `person` reach `path` with `access` in the list the mount was told? This is
/// [`collab_types::decide_capability`], the decision every Files open asks, over the list the
/// daemon holds rather than a session-side copy.
fn may(
    list: &collab_types::FsAuthority,
    person: [u8; 32],
    path: &str,
    access: collab_types::caps::Access,
) -> bool {
    let actor = tilo_action_policy::Acting::person(hex::encode(person));
    collab_types::decide_capability(
        &actor,
        &collab_types::ResourceRef::FsPath(path.into()),
        access,
        &list.capabilities,
        Some(0),
    ) == collab_types::CapDecision::Allow
}

#[test]
fn the_session_tells_a_mounting_daemon_its_file_access_list() {
    let (site, ada, _bob, ada_laptop) = crate::common::people::a_site_with_files_for_one_laptop();
    let runtime = mounts_runtime();
    let _in_runtime = runtime.enter();
    let mount = mount_on(&ada_laptop, &site.workspace);
    let list = told_until("the file access list", (&site, &ada_laptop), &mount, |_| {
        true
    });
    let note = note(&site.workspace);
    assert!(
        may(&list, ada.public, &note, collab_types::caps::Access::Write),
        "the person the device acts for writes their workspace's files"
    );
}

#[test]
fn a_role_change_reaches_the_daemon_and_a_removal_takes_the_files_away() {
    let (site, _ada, bob, ada_laptop) = crate::common::people::a_site_with_files_for_one_laptop();
    let runtime = mounts_runtime();
    let _in_runtime = runtime.enter();
    let mount = mount_on(&ada_laptop, &site.workspace);
    let at = (&site, &ada_laptop);
    let note = note(&site.workspace);
    let outside = told_until("the file access list", at, &mount, |_| true);
    assert!(
        !may(
            &outside,
            bob.public,
            &note,
            collab_types::caps::Access::Read
        ),
        "someone outside the workspace is granted nothing"
    );

    // Admitted read-only, they read the workspace's files and cannot write them.
    let mut bob_laptop = bob.device("Bob's laptop");
    bob_laptop.install_files();
    bob_laptop.join(&site.invite(bob.public, Role::Viewer));
    bob_laptop.wait_holding(&site.workspace);
    site.wait_for_member(bob.public);
    let read_only = told_until("the list after the viewer's role", at, &mount, |list| {
        may(list, bob.public, &note, collab_types::caps::Access::Read)
    });
    assert!(
        !may(
            &read_only,
            bob.public,
            &note,
            collab_types::caps::Access::Write
        ),
        "a member with a read-only role cannot write a workspace file"
    );

    // Their role changes to editor: the daemon is told writing too.
    site.set_role(bob.public, Role::Editor);
    told_until("the list after the editor's role", at, &mount, |list| {
        may(list, bob.public, &note, collab_types::caps::Access::Write)
    });

    // Removed from the workspace, their device reaches nothing at its next open.
    site.remove(bob.public);
    let removed = told_until("the list after the removal", at, &mount, |list| {
        !may(list, bob.public, &note, collab_types::caps::Access::Read)
    });
    assert!(
        !may(
            &removed,
            bob.public,
            &note,
            collab_types::caps::Access::Write
        ),
        "a removed member writes nothing"
    );
}
