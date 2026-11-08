//! The running desk's file access list, as the Files daemon reads it: the session writes it as it
//! starts and again when it changes, so taking a folder or a member's role away reaches the app at
//! its next open instead of waiting for the next login. The daemon re-reads the file on change and
//! signs it into the workspace authority every mount enforces.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test fixtures: a failed step is the assertion"
)]

mod common;

use std::path::Path;
use std::time::Duration;

use tilo_session::Session;

/// The private home the session runs in.
fn session_env(root: &Path) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", root)
        .set("XDG_CONFIG_HOME", root.join("config"))
        .set("XDG_RUNTIME_DIR", root.join("runtime"))
        .set("TILO_IDENTITY_ROOT", root.join("identity"));
    env
}

/// Where the session writes the list the daemon reads.
fn acl_path(root: &Path) -> std::path::PathBuf {
    root.join("config/tilo/fs-capabilities.json")
}

/// The list the daemon reads, once the session has written one.
fn access_list(acl: &Path) -> Option<collab_types::FsAuthority> {
    serde_json::from_slice(&std::fs::read(acl).ok()?).ok()
}

#[test]
fn the_session_writes_its_file_access_list_for_the_files_daemon() {
    let root = common::private_home();
    let _env = session_env(root.path());
    let acl = acl_path(root.path());
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the file access list", |_| acl.is_file());
    let written = access_list(&acl).expect("the list is readable");
    assert!(
        !written.workspace_members.is_empty(),
        "every workspace says who may read it: {written:?}"
    );
    // A desk change that grants nothing new does not rewrite the list: the daemon signs every list
    // it reads into a new revision.
    let before = std::fs::read(&acl).unwrap();
    common::show_preset(&mut session, "Frames");
    for _ in 0..5 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    assert_eq!(
        std::fs::read(&acl).unwrap(),
        before,
        "an unchanged list is not written again"
    );
}

/// Someone the person admits to the workspace in front, as their person id (the hex of the key
/// their devices act under).
const MEMBER: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const NOTE: &str = "/var/workspace/brief/plan.md";

fn member_key() -> [u8; 32] {
    hex::decode(MEMBER).unwrap().try_into().unwrap()
}

/// Can `MEMBER` reach `path` with `access` in the list the daemon reads? This is
/// [`collab_types::decide_capability`], the decision every Files open asks, over the written bytes
/// rather than a session-side copy.
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

/// Give `MEMBER` `role` in the workspace in front and wait for the list the daemon reads to say so:
/// the session notices the replica's change on its own, with no timer and no request for it.
fn written_after_role(
    session: &mut Session,
    acl: &Path,
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
        access_list(acl).is_some_and(|list| arrived(&list))
    });
    access_list(acl).expect("the list the daemon reads")
}

#[test]
fn a_role_change_reaches_the_daemon_and_a_removal_takes_the_files_away() {
    let root = common::private_home();
    let _env = session_env(root.path());
    let acl = acl_path(root.path());
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the file access list", |_| acl.is_file());
    assert!(
        !member_may(
            &access_list(&acl).unwrap(),
            NOTE,
            collab_types::caps::Access::Read
        ),
        "someone outside the workspace is granted nothing"
    );

    // Admitted read-only, they read the workspace's files and cannot write them.
    let read_only = written_after_role(
        &mut session,
        &acl,
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

    // Their role changes to editor: the daemon reads writing too.
    let editing = written_after_role(
        &mut session,
        &acl,
        tilo_action_policy::Role::Editor,
        |list| member_may(list, NOTE, collab_types::caps::Access::Write),
    );
    assert!(
        member_may(&editing, NOTE, collab_types::caps::Access::Write),
        "the daemon reads the editor's access"
    );

    // Removed from the workspace, their device reaches nothing at its next open, and the relay
    // membership drops them in the same revision.
    let owner = common::PERSON.to_owned();
    change_workspace(&session, |workspace| {
        tilo_desktop::workspace_settings::remove_member(workspace, &owner, &MEMBER.to_owned())
            .expect("the person removes the member");
    });
    common::until(&mut session, "the list after the removal", |_| {
        access_list(&acl).is_some_and(|list| {
            !member_may(&list, NOTE, collab_types::caps::Access::Read)
                && !member_may(&list, NOTE, collab_types::caps::Access::Write)
                && list
                    .workspace_members
                    .values()
                    .flatten()
                    .all(|member| member.user.0 != member_key())
        })
    });
    let removed = access_list(&acl).unwrap();
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
