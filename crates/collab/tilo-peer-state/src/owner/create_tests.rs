//! A workspace this device founds is one its own host keeps: it exists
//! from the moment it is made, and it comes back from its record, saying
//! where its own host is reached.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]
use crate::owner::testing;
use crate::owner::{WorkspaceId, open_file};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;

#[test]
fn a_founded_workspace_is_held_here_and_restores_from_its_record() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[12; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[11; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, "Founders", &device, &key, now).unwrap();
    let (hosted, workspace_id, name) = owner
        .with_current(workspace, |view| {
            Ok((
                view.hosted_here(),
                view.workspace_id(),
                view.peer().governance().workspace_name(),
            ))
        })
        .unwrap();
    assert!(
        hosted,
        "a founded workspace is the workspace its own host keeps"
    );
    assert_eq!(
        Some(workspace_id),
        crate::join::room_workspace_id(&workspace.to_string()).ok()
    );
    assert_eq!(name.as_deref(), Some("Founders"));
    let stored = owner.storage();
    assert!(stored.holds(workspace));
    drop(owner);

    // It comes back from the record it published: founded here, the workspace
    // holds neither an enrollment nor a vouch, and needs neither.
    let reopened = open_file(&path).unwrap();
    assert_eq!(
        reopened
            .with_current(workspace, |view| Ok((
                view.hosted_here(),
                view.peer().governance().workspace_name()
            )))
            .unwrap(),
        (true, Some("Founders".to_owned()))
    );
    // Where its own host is reached is the hint it recorded as it was
    // founded: what an action's owner commit and the workspace's route reach
    // the host by, on this device and after a restart.
    let hint = reopened
        .with_current(workspace, |view| view.transport_hint().cloned())
        .expect("a founded workspace says where its own host is reached");
    assert_eq!(hint, testing::own_host_hint_here(workspace, &device, &key));
}

#[test]
fn a_workspace_names_its_own_host_after_it_was_founded_without_one() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[21; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[22; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, "Founders", &device, &key, now).unwrap();
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert!(
        !owner.name_host(workspace, hint).unwrap(),
        "a workspace that says where its host is reached is not written again"
    );
    // A hint of another workspace is refused: a record never names its host
    // where another workspace's host is.
    let elsewhere = WorkspaceId::new();
    assert!(
        owner
            .name_host(
                workspace,
                testing::own_host_hint_here(elsewhere, &device, &key)
            )
            .is_err(),
        "a hint of another workspace is not this workspace's host"
    );
}

#[test]
fn a_workspace_founded_before_its_record_named_its_host_is_named_once() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[31; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[32; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    // As a build that founded a workspace without naming its host left it.
    let workspace = WorkspaceId::new();
    owner
        .create_workspace(
            workspace,
            "Founders",
            &device,
            &key,
            testing::own_host_hint_here(workspace, &device, &key),
            now,
        )
        .unwrap();
    let mut view = owner
        .with_current(workspace, |view| Ok(view.clone()))
        .unwrap();
    assert!(view.transport_hint().is_ok());
    view.record.host_hint = None;
    owner.publish_workspace(workspace, view, None).unwrap();
    assert!(
        !owner.names_host(workspace),
        "a record that does not say where its own host is reached names none"
    );

    // The next session says where the workspace's own host is reached.
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert!(owner.name_host(workspace, hint.clone()).unwrap());
    assert!(owner.names_host(workspace));
    drop(owner);
    assert_eq!(
        open_file(&path)
            .unwrap()
            .with_current(workspace, |view| view.transport_hint().cloned())
            .unwrap(),
        hint
    );
}

#[test]
fn a_workspace_is_founded_by_its_own_device_with_a_name() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let owner = open_file(&directory.path().join("workspaces.json")).unwrap();
    let key = SigningKey::from_bytes(&[13; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[14; 32]), &key, "A's desktop");
    let other = SigningKey::from_bytes(&[15; 32]);
    let now = SystemTime::now();
    let unnamed = WorkspaceId::new();
    let hint = testing::own_host_hint_here(unnamed, &device, &key);
    assert_eq!(
        owner
            .create_workspace(unnamed, "  ", &device, &key, hint, now)
            .unwrap_err(),
        "workspace name must contain 1–80 characters"
    );
    let workspace = WorkspaceId::new();
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert_eq!(
        owner
            .create_workspace(workspace, "Founders", &device, &other, hint, now)
            .unwrap_err(),
        "a workspace is founded with its own device's key"
    );
}
