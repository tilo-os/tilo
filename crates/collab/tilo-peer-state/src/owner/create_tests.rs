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

#[tokio::test]
async fn a_founded_workspace_is_held_here_and_restores_from_its_record() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[12; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[11; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, "Founders", &device, &key, now)
        .await
        .unwrap();
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
        crate::join::WorkspaceId::parse(&workspace.to_string())
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

#[tokio::test]
async fn a_workspace_names_its_own_host_after_it_was_founded_without_one() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[21; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[22; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, "Founders", &device, &key, now)
        .await
        .unwrap();
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert!(
        !owner.name_host(workspace, hint).unwrap(),
        "a workspace that says where its host is reached is not written again"
    );
    // A record founded before it said where its host is reached is written
    // once, and only with this workspace's own host: a hint of another
    // workspace is refused, so a record never names its host where another
    // workspace's host is.
    let mut view = owner
        .with_current(workspace, |view| Ok(view.clone()))
        .unwrap();
    view.record.host_hint = None;
    owner.publish_workspace(workspace, view, None).unwrap();
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
    assert!(
        owner
            .name_host(
                workspace,
                testing::own_host_hint_here(workspace, &device, &key)
            )
            .unwrap(),
        "the workspace's own host is named once"
    );
}

#[tokio::test]
async fn a_workspace_founded_without_an_address_is_held_and_named_later() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[41; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[42; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    // A session with nowhere other devices reach it founds the workspace all
    // the same: its governance is held here, and its host is named later.
    let workspace = WorkspaceId::new();
    owner
        .create_workspace(workspace, "Founders", &device, &key, None, now)
        .await
        .unwrap();
    assert!(
        owner
            .with_current(workspace, |view| Ok(view.transport_hint().is_err()))
            .unwrap(),
        "there is nowhere to reach its host at yet"
    );

    // The next session says where the workspace's own host is reached.
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert!(owner.name_host(workspace, hint.clone()).unwrap());
    drop(owner);
    assert_eq!(
        open_file(&path)
            .unwrap()
            .with_current(workspace, |view| view.transport_hint().cloned())
            .unwrap(),
        hint
    );
}

#[tokio::test]
async fn a_workspace_founded_before_its_record_named_its_host_is_named_once() {
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
            Some(&testing::own_host_hint_here(workspace, &device, &key)),
            now,
        )
        .await
        .unwrap();
    let mut view = owner
        .with_current(workspace, |view| Ok(view.clone()))
        .unwrap();
    assert!(view.transport_hint().is_ok());
    view.record.host_hint = None;
    owner.publish_workspace(workspace, view, None).unwrap();
    assert!(
        owner
            .with_current(workspace, |view| Ok(view.transport_hint().is_err()))
            .unwrap(),
        "a record that does not say where its own host is reached names none"
    );

    // The next session says where the workspace's own host is reached.
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert!(owner.name_host(workspace, hint.clone()).unwrap());
    drop(owner);
    assert_eq!(
        open_file(&path)
            .unwrap()
            .with_current(workspace, |view| view.transport_hint().cloned())
            .unwrap(),
        hint
    );
}

#[tokio::test]
async fn a_workspace_names_its_own_host_again_once_the_relay_it_is_reached_through_changes() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("workspaces.json");
    let key = SigningKey::from_bytes(&[51; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[52; 32]), &key, "A's desktop");
    let now = SystemTime::now();
    let owner = open_file(&path).unwrap();
    let workspace = testing::founded(&owner, "Founders", &device, &key, now)
        .await
        .unwrap();
    let id = iroh_base::EndpointId::from_bytes(&device.device_public_key).unwrap();
    let relay: iroh_base::RelayUrl = "https://relay.example.com/".parse().unwrap();
    // Founded with the address it had before its endpoint reached a relay: what its members read
    // names only the network the device stands in.
    let reached_through = testing::own_host_hint(
        workspace,
        iroh_base::EndpointAddr::new(id).with_relay_url(relay.clone()),
        &device,
        &key,
    );
    assert!(
        owner.name_host(workspace, reached_through.clone()).unwrap(),
        "the record names the relay the device is reached through now"
    );
    // A net report moves the address beside the relay: who reaches the host did not change, so
    // the record is not published for it.
    let moved = testing::own_host_hint(
        workspace,
        iroh_base::EndpointAddr::new(id)
            .with_relay_url(relay)
            .with_ip_addr((std::net::Ipv4Addr::LOCALHOST, 5000).into()),
        &device,
        &key,
    );
    assert!(
        !owner.name_host(workspace, moved).unwrap(),
        "an address that moved beside the same relay writes nothing"
    );
    drop(owner);
    assert_eq!(
        open_file(&path)
            .unwrap()
            .with_current(workspace, |view| view.transport_hint().cloned())
            .unwrap(),
        reached_through
    );
}

#[tokio::test]
async fn a_workspace_is_founded_by_its_own_device_named_or_not() {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let owner = open_file(&directory.path().join("workspaces.json")).unwrap();
    let key = SigningKey::from_bytes(&[13; 32]);
    let device = testing::certificate(&SigningKey::from_bytes(&[14; 32]), &key, "A's desktop");
    let other = SigningKey::from_bytes(&[15; 32]);
    let now = SystemTime::now();
    // A workspace nobody named is founded with no name, which every reader
    // shows in their own language; a name too long is still refused.
    let unnamed = WorkspaceId::new();
    let hint = testing::own_host_hint_here(unnamed, &device, &key);
    assert_eq!(
        owner
            .create_workspace(unnamed, "  ", &device, &key, Some(&hint), now)
            .await
            .unwrap(),
        unnamed
    );
    let long = WorkspaceId::new();
    let hint = testing::own_host_hint_here(long, &device, &key);
    assert_eq!(
        owner
            .create_workspace(long, &"n".repeat(81), &device, &key, Some(&hint), now)
            .await
            .unwrap_err(),
        "workspace name must contain 1–80 characters"
    );
    let workspace = WorkspaceId::new();
    let hint = testing::own_host_hint_here(workspace, &device, &key);
    assert_eq!(
        owner
            .create_workspace(workspace, "Founders", &device, &other, Some(&hint), now)
            .await
            .unwrap_err(),
        "a workspace is founded with its own device's key"
    );
}
