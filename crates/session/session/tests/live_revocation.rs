//! The running desk's file access list, as the Files daemon enforces it: the
//! session publishes it as it starts and again when it changes, so taking a
//! folder or a member's role away reaches the app at its next open instead of
//! waiting for the next login.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use tilo_session::Session;

/// How many times the stand-in Files daemon was asked to publish.
fn publications(seen: &Path) -> u64 {
    std::fs::read_to_string(seen.join("count"))
        .ok()
        .and_then(|count| count.trim().parse().ok())
        .unwrap_or(0)
}

/// A stand-in for `fs publish-authority` in `root`, writing what it is handed
/// to `root/seen`: it keeps exactly the list the daemon would sign. `seen` is
/// returned.
fn stand_in(root: &Path) -> std::path::PathBuf {
    let seen = root.join("seen");
    std::fs::create_dir_all(&seen).unwrap();
    let script = root.join("fs");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             set -e\n\
             test \"$1\" = publish-authority\n\
             test \"$2\" = --acl\n\
             n=$(cat {seen}/count 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             cp \"$3\" {seen}/$n.json\n\
             echo \"$n\" > {seen}/count\n",
            seen = seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(root.join("runtime")).unwrap();
    seen
}

/// The private home and daemon stand-in a test's session runs in.
fn session_env(root: &Path) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", root)
        .set("XDG_CONFIG_HOME", root.join("config"))
        .set("XDG_RUNTIME_DIR", root.join("runtime"))
        .set("TILO_IDENTITY_ROOT", root.join("identity"))
        .set("TILO_AUTHORITY_SOCKET_DIR", root.join("sockets"))
        .set("TILO_FS_AUTHORITY_PUBLISHER", root.join("fs"));
    env
}

/// Where the session writes the list the daemon mounts.
fn acl_path(root: &Path) -> std::path::PathBuf {
    root.join("config/tilo/fs-capabilities.json")
}

/// The list the daemon was handed last, as it signed it, if it was handed one
/// whose signature step has finished.
fn published(seen: &Path) -> Option<collab_types::FsAuthority> {
    let last = publications(seen);
    if last == 0 {
        return None;
    }
    let bytes = std::fs::read(seen.join(format!("{last}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The list the daemon was handed last, as it signed it.
fn last_published(seen: &Path) -> collab_types::FsAuthority {
    published(seen).expect("the daemon was handed a list")
}

#[test]
fn the_session_publishes_its_file_access_list_to_the_files_daemon() {
    let root = common::private_home();
    let seen = stand_in(root.path());
    let _env = session_env(root.path());
    let acl = acl_path(root.path());
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the file access list", |_| {
        acl.is_file() && publications(&seen) == 1
    });
    let published: collab_types::FsAuthority =
        serde_json::from_slice(&std::fs::read(&acl).unwrap()).unwrap();
    assert!(
        !published.workspace_members.is_empty(),
        "every workspace says who may read it: {published:?}"
    );
    // A desk change that grants nothing new is not published again: the
    // daemon signs a list, not every frame.
    common::show_preset(&mut session, "Frames");
    for _ in 0..5 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        publications(&seen),
        1,
        "an unchanged list is not republished"
    );
    let kept: collab_types::FsAuthority =
        serde_json::from_slice(&std::fs::read(&acl).unwrap()).unwrap();
    assert_eq!(kept.capabilities, published.capabilities);
}

/// Someone the person admits to the workspace in front, as their person id
/// (the hex of the key their devices act under).
const MEMBER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const NOTE: &str = "/var/workspace/brief/plan.md";

fn member_key() -> [u8; 32] {
    hex::decode(MEMBER).unwrap().try_into().unwrap()
}

/// Can `MEMBER` reach `path` with `access` in the list the daemon was handed?
/// This is [`collab_types::decide_capability`], the decision every Files open
/// asks, over the published bytes rather than a session-side copy.
fn member_may(
    list: &collab_types::FsAuthority,
    path: &str,
    access: collab_types::caps::Access,
) -> bool {
    let actor = collab_types::caps::Actor::signed(
        collab_types::crypto::Ed25519PublicKey::from_bytes(member_key()),
    );
    collab_types::decide_capability(
        &actor,
        &collab_types::ResourceRef::FsPath(path.into()),
        access,
        &list.capabilities,
        Some(0),
    ) == collab_types::CapDecision::Allow
}

/// Change the workspace in front, as the person's own governance does.
fn change_workspace(session: &Session, change: impl FnOnce(&mut tilo_desktop_model::Workspace)) {
    let mut tree = session.handle().read(Clone::clone);
    let workspace = tree.active_workspace_mut().expect("a workspace in front");
    change(workspace);
    session.handle().replace_tree(tree);
}

/// Give `MEMBER` `role` in the workspace in front and return the list the
/// daemon was handed in answer: the session notices the replica's change on
/// its own, with no timer and no request for it. `arrived` is what that list
/// says, so the wait is for the access the role gives and not for a number of
/// publications: a list published for a change still in flight may carry this
/// one too, and how many publications a change takes is not what a role
/// change promises.
fn published_after_role(
    session: &mut Session,
    seen: &Path,
    publications_before: u64,
    role: tilo_action_policy::Role,
    arrived: impl Fn(&collab_types::FsAuthority) -> bool,
) -> collab_types::FsAuthority {
    let owner = common::PERSON.to_owned();
    change_workspace(session, |workspace| {
        tilo_desktop::workspace_settings::set_member_role(
            workspace,
            &owner,
            &MEMBER.to_owned(),
            role,
        )
        .expect("the person changes the member's role");
    });
    common::until(session, "the list after the role", |_| {
        publications(seen) > publications_before
            && published(seen).is_some_and(|list| arrived(&list))
    });
    last_published(seen)
}

#[test]
fn a_role_change_reaches_the_daemon_and_a_removal_takes_the_files_away() {
    let root = common::private_home();
    let seen = stand_in(root.path());
    let _env = session_env(root.path());
    let acl = acl_path(root.path());
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the file access list", |_| {
        acl.is_file() && publications(&seen) >= 1
    });
    assert!(
        !member_may(
            &last_published(&seen),
            NOTE,
            collab_types::caps::Access::Read
        ),
        "someone outside the workspace is granted nothing"
    );

    // Admitted read-only, they read the workspace's files and cannot write
    // them.
    let read_only = published_after_role(
        &mut session,
        &seen,
        publications(&seen),
        tilo_action_policy::Role::Viewer,
        |list| member_may(list, NOTE, collab_types::caps::Access::Read),
    );
    assert!(member_may(
        &read_only,
        NOTE,
        collab_types::caps::Access::Read
    ));
    assert!(
        !member_may(&read_only, NOTE, collab_types::caps::Access::Write),
        "a member with a read-only role cannot write a workspace file"
    );

    // Their role changes to editor: the daemon is handed writing too.
    let editing = published_after_role(
        &mut session,
        &seen,
        publications(&seen),
        tilo_action_policy::Role::Editor,
        |list| member_may(list, NOTE, collab_types::caps::Access::Write),
    );
    assert!(
        member_may(&editing, NOTE, collab_types::caps::Access::Write),
        "the daemon is handed the editor's access"
    );

    // Removed from the workspace, their device reaches nothing at its next
    // open, and the relay membership drops them in the same revision.
    let before_removal = publications(&seen);
    let owner = common::PERSON.to_owned();
    change_workspace(&session, |workspace| {
        tilo_desktop::workspace_settings::remove_member(workspace, &owner, &MEMBER.to_owned())
            .expect("the person removes the member");
    });
    common::until(&mut session, "the list after the removal", |_| {
        publications(&seen) > before_removal
            && published(&seen).is_some_and(|list| {
                !member_may(&list, NOTE, collab_types::caps::Access::Read)
                    && !member_may(&list, NOTE, collab_types::caps::Access::Write)
                    && list
                        .workspace_members
                        .values()
                        .flatten()
                        .all(|member| member.user.0 != member_key())
            })
    });
    let removed = last_published(&seen);
    assert!(!member_may(
        &removed,
        NOTE,
        collab_types::caps::Access::Read
    ));
    assert!(!member_may(
        &removed,
        NOTE,
        collab_types::caps::Access::Write
    ));
    assert!(
        removed
            .workspace_members
            .values()
            .flatten()
            .all(|member| member.user.0 != member_key()),
        "the relay no longer admits them either: {removed:?}"
    );
}
