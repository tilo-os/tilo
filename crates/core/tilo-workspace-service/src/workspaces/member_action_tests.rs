//! A governance change names the action it carries out
//! ([`tilo_peer_state::ActionStamp`]): its verb, who decided it, and the
//! time its commit carries. What a watcher of the workspace's members reads a
//! ban, a removal and an unban apart by, and what every reader of the
//! workspace's changes reads an action from the commit that makes it.
//!
//! Each verb is performed through the workspace's own governance methods, the
//! path its actions run; what is read back is the committed governance, the
//! changes a history or a feed of them reads.

use tilo_action_policy::roles::governance;
use tilo_action_policy::{Actor, MemberId, PersonId};
use tilo_peer_state::join::Role;

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

/// Every named governance change the workspace lists
/// ([`tilo_peer_state::workspace::base::named_changes`]): those its bases
/// kept, then those of its history since.
async fn named(workspaces: &Workspaces, workspace: &str) -> Vec<(collab_types::Stamp, i64)> {
    let state = workspaces
        .state(workspace)
        .await
        .expect("the workspace is held");
    state
        .admitted_workspace()
        .map(tilo_peer_state::workspace::base::named_changes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|kept| Some((collab_types::Stamp::of_message(&kept.message)?, kept.time)))
        .collect()
}

/// Every named governance change made after the first `since` the workspace
/// listed, as the change names it: its stamp and the time its commit carries.
async fn stamped(
    workspaces: &Workspaces,
    workspace: &str,
    since: usize,
) -> Vec<(collab_types::Stamp, i64)> {
    named(workspaces, workspace).await.split_off(since)
}

/// How many named governance changes the workspace lists now.
async fn listed(workspaces: &Workspaces, workspace: &str) -> usize {
    named(workspaces, workspace).await.len()
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

/// A new workspace of `workspaces`, never administered: its first
/// administration starts its document over from a base, which keeps the
/// action it names.
async fn made_workspace(workspaces: &Workspaces, name: &str) -> String {
    workspaces.create(name).await.unwrap().workspace
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

    let workspace = made_workspace(&workspaces, "Added").await;
    let before = listed(&workspaces, &workspace).await;
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
        &stamped(&workspaces, &workspace, before).await,
        governance::ADD_MEMBER,
        &by,
        started,
    );

    let before = listed(&workspaces, &workspace).await;
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
        &stamped(&workspaces, &workspace, before).await,
        governance::SET_ROLE,
        &by,
        started,
    );

    let before = listed(&workspaces, &workspace).await;
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
        &stamped(&workspaces, &workspace, before).await,
        governance::REMOVE_MEMBER,
        &by,
        started,
    );

    let workspace = made_workspace(&workspaces, "Banned").await;
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
    let before = listed(&workspaces, &workspace).await;
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
    let stamps = stamped(&workspaces, &workspace, before).await;
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

    let before = listed(&workspaces, &workspace).await;
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
        &stamped(&workspaces, &workspace, before).await,
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
    let workspace = made_workspace(&workspaces, "Moderated").await;
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
    let before = listed(&workspaces, &workspace).await;
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
    let stamps = stamped(&workspaces, &workspace, before).await;
    // The two changes a ban makes name it; lifting it names the unban after.
    let mut verbs: Vec<&str> = stamps
        .iter()
        .map(|(stamp, _)| stamp.verb.as_str())
        .collect();
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
    let workspace = made_workspace(&workspaces, "Refused").await;
    let (_, stranger) = guest_device();
    let before = heads(&workspaces, &workspace).await;
    let listed_before = listed(&workspaces, &workspace).await;

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
        stamped(&workspaces, &workspace, listed_before)
            .await
            .is_empty(),
        "a refused change is no change, and appears in no feed"
    );
    assert_eq!(
        heads(&workspaces, &workspace).await,
        before,
        "the workspace's governance stands where it did"
    );
    server.shutdown().await;
}

/// The named changes `peer`'s governance lists, as each names its action.
fn listed_by(peer: &tilo_peer_state::StatePeer) -> Vec<(collab_types::Stamp, i64)> {
    peer.admitted_workspace()
        .map(tilo_peer_state::workspace::base::named_changes)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|kept| Some((collab_types::Stamp::of_message(&kept.message)?, kept.time)))
        .collect()
}

#[tokio::test]
async fn a_base_keeps_every_action_the_history_before_it_took() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let by = actor(owner.user_public_key);
    let workspace = made_workspace(&workspaces, "Kept").await;
    let founded = listed(&workspaces, &workspace).await;
    let (first, second) = ([21u8; 32], [22u8; 32]);
    let add = |request: u8, person: [u8; 32]| AddMemberRequest {
        request_id: [request; 32],
        user_public_key: person,
        place: MemberPlace::Role(Role::Editor),
        name: None,
    };
    workspaces
        .add_member(&workspace, &owner, add(1, first))
        .await
        .unwrap();
    workspaces
        .remove_member(
            &workspace,
            &owner,
            RemoveMemberRequest {
                request_id: [2; 32],
                user_public_key: first,
            },
        )
        .await
        .unwrap();
    workspaces
        .add_member(&workspace, &owner, add(3, second))
        .await
        .unwrap();
    let held = workspaces
        .state(&workspace)
        .await
        .expect("the workspace is held");
    let before = listed_by(&held);

    // Someone new after a withdrawal: the document starts over, so the one
    // just added never receives what was withdrawn.
    let mut rebased = held.clone();
    let generation = |peer: &tilo_peer_state::StatePeer| {
        peer.admitted_workspace()
            .map_or(0, tilo_peer_state::workspace::base::generation)
    };
    assert!(
        rebased.rebase_workspace().unwrap(),
        "the document starts over"
    );
    assert!(
        generation(&rebased) > generation(&held),
        "from a newer base"
    );
    let base = rebased
        .admitted_workspace()
        .unwrap()
        .clone()
        .get_changes(&[]);
    assert_eq!(base.len(), 1, "a base holds none of the history before it");
    assert!(
        base[0].message().is_none(),
        "the base itself names no action"
    );
    assert_eq!(
        listed_by(&rebased),
        before,
        "every action before the base is still listed, once and in order"
    );

    let kept: Vec<&(collab_types::Stamp, i64)> = before.iter().skip(founded).collect();
    let verbs: Vec<&str> = kept.iter().map(|(stamp, _)| stamp.verb.as_str()).collect();
    assert_eq!(
        verbs,
        [
            governance::ADD_MEMBER,
            governance::REMOVE_MEMBER,
            governance::ADD_MEMBER
        ],
        "{kept:?}"
    );
    for (stamp, _) in &kept {
        assert_eq!(stamp.invoker, collab_types::Stamp::by(&by), "{stamp:?}");
    }
    let times: Vec<i64> = kept.iter().map(|(_, time)| *time).collect();
    assert!(times.windows(2).all(|pair| pair[0] <= pair[1]), "{times:?}");
    server.shutdown().await;
}

/// What a watcher of the whole workspace reads, the way the desktop's feed
/// reads it: the members' changes this device's service names, taken into a
/// log of the workspace's changes, and the page after `cursor`.
fn feed_after(
    documents: &crate::device_documents::ServiceDocuments,
    log: &mut tilo_resource_store::history::feed::ChangeLog,
    workspace: &str,
    cursor: Option<&str>,
) -> tilo_actions::remote::ChangePage {
    use crate::workspace_documents::DeviceDocuments as _;
    let named: Vec<([u8; 32], String, i64)> = documents
        .member_changes(workspace)
        .expect("the workspace's members read")
        .into_iter()
        .map(|change| (change.change, change.message, change.time))
        .collect();
    log.take_in_named(
        &(
            tilo_action_policy::agent_answers::WORKSPACE_APP.to_owned(),
            "workspace/members".to_owned(),
        ),
        &named,
    );
    log.page(cursor, None)
}

#[tokio::test]
async fn a_watcher_of_the_workspace_reads_each_member_action_once_with_who_and_when() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let by = collab_types::Stamp::by(&actor(owner.user_public_key));
    let workspace = made_workspace(&workspaces, "Watched").await;
    let documents = crate::device_documents::ServiceDocuments::of(workspaces.clone());
    let mut log = tilo_resource_store::history::feed::ChangeLog::new();
    let cursor = feed_after(&documents, &mut log, &workspace, None).cursor;
    let started = i64::try_from(tilo_clock::unix_now()).unwrap_or(i64::MAX) * 1000;
    let (banned, removed, stranger) = ([31u8; 32], [32u8; 32], guest_device().1);
    for (request, person) in [(1u8, banned), (2, removed)] {
        workspaces
            .add_member(
                &workspace,
                &owner,
                AddMemberRequest {
                    request_id: [request; 32],
                    user_public_key: person,
                    place: MemberPlace::Role(Role::Editor),
                    name: None,
                },
            )
            .await
            .unwrap();
    }
    let gone = |request: u8, person: [u8; 32]| RemoveMemberRequest {
        request_id: [request; 32],
        user_public_key: person,
    };
    workspaces
        .ban_member(&workspace, &owner, gone(3, banned))
        .await
        .unwrap();
    // Someone the workspace never let in removes nobody, and nothing of it
    // reaches a watcher.
    assert!(
        workspaces
            .remove_member(&workspace, &stranger, gone(4, removed))
            .await
            .is_err()
    );
    workspaces
        .remove_member(&workspace, &owner, gone(5, removed))
        .await
        .unwrap();
    workspaces
        .unban_member(&workspace, &owner, gone(6, banned))
        .await
        .unwrap();

    let page = feed_after(&documents, &mut log, &workspace, Some(&cursor));
    let verbs: Vec<&str> = page
        .changes
        .iter()
        .map(|change| change.entry["verb"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        verbs,
        [
            governance::ADD_MEMBER,
            governance::ADD_MEMBER,
            governance::BAN,
            governance::REMOVE_MEMBER,
            governance::UNBAN,
        ],
        "each action once, in the order it was made, the first administration's \
         among them: {page:?}"
    );
    for change in &page.changes {
        assert_eq!(change.app, tilo_action_policy::agent_answers::WORKSPACE_APP);
        assert_eq!(change.record, "workspace/members");
        assert_eq!(change.entry["by"], by.as_str(), "{change:?}");
        assert_eq!(change.entry["happened"], "edited", "{change:?}");
        let time = change.entry["time_ms"].as_i64().unwrap_or_default();
        assert!(time >= started, "{time} < {started}: {change:?}");
    }
    // Read again, a watcher is told nothing twice.
    let again = feed_after(&documents, &mut log, &workspace, Some(&page.cursor));
    assert!(again.changes.is_empty(), "{again:?}");
    server.shutdown().await;
}
