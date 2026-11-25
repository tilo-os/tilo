//! A governance change names the action it carries out
//! ([`tilo_peer_state::ActionStamp`]): its verb, who decided it, and the
//! time its commit carries. What a watcher of the workspace's members reads a
//! ban, a removal and an unban apart by, and what every reader of the
//! workspace's changes reads an action from the commit that makes it.
//!
//! Each verb is performed through the workspace's own governance methods, the
//! path its actions run; what is read back is the committed governance, the
//! changes a history or a feed of them reads.

use automerge::Change as AmChange;
use tilo_action_policy::roles::governance;
use tilo_action_policy::{Actor, MemberId, PersonId};
use tilo_peer_state::join::Role;
use tilo_user_identity::DeviceCert;

use super::tests::{governor, guest_device, test_host};
use super::*;

/// The heads the workspace's governance stands at.
async fn heads(workspaces: &Workspaces, workspace: &str) -> Vec<automerge::ChangeHash> {
    workspaces
        .state(workspace)
        .await
        .expect("the workspace is held")
        .workspace_heads()
        .unwrap_or_default()
}

/// Every governance change committed past `since`, as the change names it: its
/// stamp and the time its commit carries.
async fn stamped(
    workspaces: &Workspaces,
    workspace: &str,
    since: &[automerge::ChangeHash],
) -> Vec<(collab_types::Stamp, i64)> {
    let state = workspaces
        .state(workspace)
        .await
        .expect("the workspace is held");
    let changes = state.workspace_changes_since(since);
    changes
        .into_iter()
        .filter_map(|bytes| {
            let change = AmChange::from_bytes(bytes).ok()?;
            Some((collab_types::Stamp::of(&change)?, change.timestamp()))
        })
        .collect()
}

/// The person whose root is `root` (a user key), as a stamp names them.
fn person(root: [u8; 32]) -> String {
    hex::encode(root)
}

fn actor(root: [u8; 32]) -> Actor {
    Actor::Member(MemberId::person(
        PersonId::parse(&person(root)).expect("a root key names a person"),
    ))
}

/// One governance change, naming `verb` and decided by `by`, committed no
/// earlier than `started` (Unix seconds).
fn assert_one(stamps: &[(collab_types::Stamp, i64)], verb: &str, by: &Actor, started: u64) {
    assert_eq!(stamps.len(), 1, "one `{verb}` change, got {stamps:?}");
    let (stamp, time) = &stamps[0];
    assert_eq!(stamp.verb, verb, "the change names its action: {stamp:?}");
    assert_eq!(
        stamp.invoker,
        collab_types::Stamp::by(by),
        "the change names who decided it: {stamp:?}"
    );
    assert!(
        *time >= i64::try_from(started).unwrap_or(i64::MAX),
        "the change is stamped with the time it happened: {time} < {started}"
    );
}

/// A workspace of `workspaces`, administered once so its document stops
/// starting over: the first administration of a workspace writes its document
/// as a fresh base, which holds the change's state and none of its history.
async fn made_workspace(workspaces: &Workspaces, owner: &DeviceCert, name: &str) -> String {
    let workspace = workspaces.create(name).await.unwrap().workspace;
    workspaces
        .add_member(
            &workspace,
            owner,
            AddMemberRequest {
                request_id: [250; 32],
                user_public_key: [250; 32],
                place: MemberPlace::Role(Role::Viewer),
                name: None,
            },
        )
        .await
        .unwrap();
    workspace
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one workspace's member actions, read end to end"
)]
async fn each_member_action_stamps_the_members_change_it_makes() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let by = actor(owner.user_public_key);
    let target = [9u8; 32];

    let workspace = made_workspace(&workspaces, &owner, "Added").await;
    let before = heads(&workspaces, &workspace).await;
    let started = tilo_clock::unix_now();
    workspaces
        .add_member(
            &workspace,
            &owner,
            AddMemberRequest {
                request_id: [1; 32],
                user_public_key: target,
                place: MemberPlace::Role(Role::Editor),
                name: None,
            },
        )
        .await
        .unwrap();
    assert_one(
        &stamped(&workspaces, &workspace, &before).await,
        governance::ADD_MEMBER,
        &by,
        started,
    );

    let before = heads(&workspaces, &workspace).await;
    let started = tilo_clock::unix_now();
    workspaces
        .set_member_role(
            &workspace,
            &owner,
            SetMemberRoleRequest {
                request_id: [2; 32],
                user_public_key: target,
                role: Role::Viewer,
            },
        )
        .await
        .unwrap();
    assert_one(
        &stamped(&workspaces, &workspace, &before).await,
        governance::SET_ROLE,
        &by,
        started,
    );

    let before = heads(&workspaces, &workspace).await;
    let started = tilo_clock::unix_now();
    workspaces
        .remove_member(
            &workspace,
            &owner,
            RemoveMemberRequest {
                request_id: [3; 32],
                user_public_key: target,
            },
        )
        .await
        .unwrap();
    assert_one(
        &stamped(&workspaces, &workspace, &before).await,
        governance::REMOVE_MEMBER,
        &by,
        started,
    );

    let workspace = made_workspace(&workspaces, &owner, "Banned").await;
    let banned = [10u8; 32];
    workspaces
        .add_member(
            &workspace,
            &owner,
            AddMemberRequest {
                request_id: [4; 32],
                user_public_key: banned,
                place: MemberPlace::Role(Role::Editor),
                name: None,
            },
        )
        .await
        .unwrap();
    let before = heads(&workspaces, &workspace).await;
    let started = tilo_clock::unix_now();
    workspaces
        .ban_member(
            &workspace,
            &owner,
            RemoveMemberRequest {
                request_id: [5; 32],
                user_public_key: banned,
            },
        )
        .await
        .unwrap();
    // A ban is one action across two governance changes: the removal, then
    // the block that keeps the person out. Both name the ban.
    let stamps = stamped(&workspaces, &workspace, &before).await;
    assert!(
        (1..=2).contains(&stamps.len()),
        "a ban is one removal and its block, got {stamps:?}"
    );
    let started = i64::try_from(started).unwrap_or(i64::MAX);
    for (stamp, time) in &stamps {
        assert_eq!(stamp.verb, governance::BAN, "{stamp:?}");
        assert_eq!(stamp.invoker, collab_types::Stamp::by(&by), "{stamp:?}");
        assert!(*time >= started, "{time} < {started}");
    }

    let before = heads(&workspaces, &workspace).await;
    let started = tilo_clock::unix_now();
    workspaces
        .unban_member(
            &workspace,
            &owner,
            RemoveMemberRequest {
                request_id: [6; 32],
                user_public_key: banned,
            },
        )
        .await
        .unwrap();
    assert_one(
        &stamped(&workspaces, &workspace, &before).await,
        governance::UNBAN,
        &by,
        started,
    );

    server.shutdown().await;
}

#[tokio::test]
async fn a_ban_and_an_unban_reach_the_members_in_that_order() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = made_workspace(&workspaces, &owner, "Moderated").await;
    let person = [11u8; 32];
    workspaces
        .add_member(
            &workspace,
            &owner,
            AddMemberRequest {
                request_id: [1; 32],
                user_public_key: person,
                place: MemberPlace::Role(Role::Editor),
                name: None,
            },
        )
        .await
        .unwrap();
    let before = heads(&workspaces, &workspace).await;
    workspaces
        .ban_member(
            &workspace,
            &owner,
            RemoveMemberRequest {
                request_id: [2; 32],
                user_public_key: person,
            },
        )
        .await
        .unwrap();
    workspaces
        .unban_member(
            &workspace,
            &owner,
            RemoveMemberRequest {
                request_id: [3; 32],
                user_public_key: person,
            },
        )
        .await
        .unwrap();
    let stamps = stamped(&workspaces, &workspace, &before).await;
    // The two changes a ban makes name it; lifting it names the unban after.
    let mut verbs: Vec<&str> = stamps.iter().map(|(stamp, _)| stamp.verb.as_str()).collect();
    verbs.dedup();
    assert_eq!(
        verbs,
        [governance::BAN, governance::UNBAN],
        "the changes reach a reader in the order they were made: {stamps:?}"
    );
    let times: Vec<i64> = stamps.iter().map(|(_, time)| *time).collect();
    assert!(
        times.windows(2).all(|pair| pair[0] <= pair[1]),
        "the changes keep the order they were made in: {times:?}"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn a_refused_member_action_stamps_no_members_change() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let workspace = made_workspace(&workspaces, &owner, "Refused").await;
    let (_, stranger) = guest_device();
    let before = heads(&workspaces, &workspace).await;

    // Someone the workspace has never let in cannot move its people, ban one
    // or remove one: the change is refused before any is written.
    let refused = workspaces
        .set_member_role(
            &workspace,
            &stranger,
            SetMemberRoleRequest {
                request_id: [1; 32],
                user_public_key: owner.user_public_key,
                role: Role::Viewer,
            },
        )
        .await;
    assert!(refused.is_err(), "a stranger moves nobody");
    let refused = workspaces
        .ban_member(
            &workspace,
            &stranger,
            RemoveMemberRequest {
                request_id: [2; 32],
                user_public_key: owner.user_public_key,
            },
        )
        .await;
    assert!(refused.is_err(), "a stranger bans nobody");

    assert!(
        stamped(&workspaces, &workspace, &before).await.is_empty(),
        "a refused change is no change, and appears in no feed"
    );
    assert_eq!(
        heads(&workspaces, &workspace).await,
        before,
        "the workspace's governance stands where it did"
    );
    server.shutdown().await;
}
