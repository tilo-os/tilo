//! A change made at this device stays in force over the room's commits
//! until the room's document carries it, survives a restart, and a refusal
//! is said: a sandbox capability taken back, and an effect receipt for the
//! room's history.
use automerge::transaction::Transactable as _;
use collab_types::caps::{Access, Scope, Subject};

use crate::shell::state::{
    CapOverride, RoomChange, RoomId, Standing, Workspace, WorkspaceEffectReceipt, WorkspaceTree,
    WorkspaceTreeHandle,
};

/// The room document `workspace` stands for, as its host would commit it,
/// carrying `sandbox` and `receipts`.
fn document_of(
    workspace: &Workspace,
    sandbox: &[CapOverride],
    receipts: &[WorkspaceEffectReceipt],
) -> automerge::AutoCommit {
    let mut document = automerge::AutoCommit::new();
    document
        .put(automerge::ROOT, "room", workspace.room().to_string())
        .expect("room");
    document
        .put(automerge::ROOT, "name", workspace.name.clone())
        .expect("name");
    tilo_desktop_model::projection::project_room_policy(&mut document, &workspace.policy)
        .expect("policy");
    tilo_desktop_model::projection::project_room_sandbox(&mut document, sandbox).expect("sandbox");
    for receipt in receipts {
        tilo_desktop_model::projection::project_effect_receipts(
            &mut document,
            &receipt.actor,
            std::slice::from_ref(receipt),
        )
        .expect("receipts");
    }
    document
}

/// The same room after a commit of its own (a rename), which changes none of
/// what the outbox holds.
fn after_another_commit(document: &automerge::AutoCommit, name: &str) -> automerge::AutoCommit {
    let mut next = document.clone();
    next.put(automerge::ROOT, "name", name).expect("name");
    next
}

fn capability() -> CapOverride {
    CapOverride::grant(
        Subject::app("so.tilo.reader"),
        Scope::NetHost("example.org".into()),
        Access::Read,
    )
    .with_id("reader-net")
}

fn receipt() -> WorkspaceEffectReceipt {
    WorkspaceEffectReceipt::recorded(
        tilo_actions::InvocationId::new(),
        &"ab".repeat(32),
        tilo_actions::ActionContext::default(),
        tilo_actions::EffectReceiptStatus::Succeeded,
        1,
    )
}

fn workspace_of(handle: &WorkspaceTreeHandle, room: RoomId) -> Workspace {
    handle
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .cloned()
        .expect("the room's workspace")
}

#[test]
fn a_capability_taken_back_stays_taken_back_until_the_room_carries_it() {
    let (chrome, _guard) = crate::shell::native_workspace_owner::test_chrome_governing_its_room();
    let governor = chrome.borrow().active_local_user();
    let handle = chrome.borrow().tree_handle();
    let (local, room) = handle
        .write()
        .desktop
        .active_workspace_mut()
        .map(|workspace| {
            workspace.sandbox.push(capability());
            (workspace.id, workspace.room())
        })
        .expect("a workspace");
    let granted = document_of(&workspace_of(&handle, room), &[capability()], &[]);
    handle.write().admit_room_document_for_test(room, &granted);
    assert!(workspace_of(&handle, room).sandbox.contains(&capability()));

    assert!(
        handle
            .write()
            .narrow_sandbox(local, &governor, "reader-net"),
        "the capability is taken back here at once"
    );
    for name in ["Plans", "Plans again"] {
        handle
            .write()
            .admit_room_document_for_test(room, &after_another_commit(&granted, name));
        assert!(
            !workspace_of(&handle, room).sandbox.contains(&capability()),
            "a commit of the room made before the revocation reached it does not grant it again"
        );
    }

    // The revocation is written; this device's replica still holds a
    // commit from before it.
    let written = handle.read().room_outbox[0].id;
    handle
        .write()
        .outbox_answered(written, crate::shell::state::HostAnswer::Accepted);
    assert!(
        matches!(
            handle.read().room_outbox.as_slice(),
            [entry] if entry.standing == Standing::Accepted
        ),
        "the accepted revocation waits for the room's document"
    );
    handle
        .write()
        .admit_room_document_for_test(room, &after_another_commit(&granted, "Late"));
    assert!(!workspace_of(&handle, room).sandbox.contains(&capability()));

    // The revocation lands.
    let mut revoked = after_another_commit(&granted, "Late");
    tilo_desktop_model::projection::project_room_sandbox(&mut revoked, &[]).expect("sandbox");
    handle.write().admit_room_document_for_test(room, &revoked);
    assert!(!workspace_of(&handle, room).sandbox.contains(&capability()));
    assert!(
        handle.read().room_outbox.is_empty(),
        "nothing waits once the room carries the revocation"
    );

    // A grant made after it is the room's to give.
    let mut regranted = revoked.clone();
    let again = CapOverride {
        access: Access::Write,
        ..capability()
    };
    tilo_desktop_model::projection::project_room_sandbox(
        &mut regranted,
        std::slice::from_ref(&again),
    )
    .expect("sandbox");
    handle
        .write()
        .admit_room_document_for_test(room, &regranted);
    assert!(workspace_of(&handle, room).sandbox.contains(&again));
}

#[test]
fn a_capability_taken_back_stays_taken_back_after_a_restart_before_it_landed() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.json");
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(path.clone());
    let room = tree.desktop.active_workspace().expect("a workspace").room();
    let workspace = tree
        .desktop
        .active_workspace()
        .expect("a workspace")
        .clone();
    let granted = document_of(&workspace, &[capability()], &[]);
    tree.admit_room_document_for_test(room, &granted);
    tree.request_governance(
        room,
        crate::shell::state::GovernanceRequest::RevokeSandbox("reader-net".into()),
    );
    tree.admit_room_document_for_test(room, &after_another_commit(&granted, "Plans"));
    crate::shell::persist::save_tree_to(&tree, &path).expect("save");
    drop(tree);

    let mut loaded = crate::shell::persist::load_tree_from(&path).expect("load");
    loaded.admit_room_document_for_test(room, &after_another_commit(&granted, "Plans"));
    let shown = loaded
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room() == room)
        .expect("the room's workspace");
    assert!(
        !shown.sandbox.contains(&capability()),
        "the room's commit after the restart does not grant it again"
    );
    assert!(
        matches!(
            loaded.room_outbox.as_slice(),
            [entry] if entry.standing == Standing::Asking
        ),
        "and the revocation is still to be asked"
    );
}

#[test]
fn a_receipt_made_before_a_crash_is_still_shown_and_sent_after_the_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.json");
    let mut tree = WorkspaceTree::bootstrap_default();
    tree.persist_path = Some(path.clone());
    let room = tree.desktop.active_workspace().expect("a workspace").room();
    let workspace = tree
        .desktop
        .active_workspace()
        .expect("a workspace")
        .clone();
    let before = document_of(&workspace, &[], &[]);
    tree.admit_room_document_for_test(room, &before);
    let receipt = receipt();
    assert!(tree.record_in_room(room, receipt.clone()));
    assert!(
        !tree.record_in_room(room, receipt.clone()),
        "a receipt is recorded once"
    );
    tree.admit_room_document_for_test(room, &after_another_commit(&before, "Plans"));
    let shown = |tree: &WorkspaceTree| {
        tree.desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room() == room)
            .expect("the room's workspace")
            .effect_receipts
            .iter()
            .filter(|held| held.invocation == receipt.invocation)
            .count()
    };
    assert_eq!(shown(&tree), 1, "the room's next commit keeps it shown");
    crate::shell::persist::save_tree_to(&tree, &path).expect("save");
    drop(tree);

    // The process ends before the host committed it.
    let mut loaded = crate::shell::persist::load_tree_from(&path).expect("load");
    loaded.persist_path = Some(path);
    loaded.admit_room_document_for_test(room, &after_another_commit(&before, "Plans"));
    assert_eq!(shown(&loaded), 1, "still shown after the restart");
    let handle = loaded.shared();
    let (network, _commands) = super::shell_network_for_test(handle.clone(), 1);
    network.send_outbox_off_loop();
    assert!(
        matches!(
            handle.read().room_outbox.as_slice(),
            [entry] if entry.standing == Standing::Asking
                && entry.change == RoomChange::Receipt(Box::new(receipt.clone()))
        ),
        "it waits to be written until this device holds the room to write it to"
    );
    handle.write().admit_room_document_for_test(
        room,
        &document_of(&workspace, &[], std::slice::from_ref(&receipt)),
    );
    assert!(handle.read().room_outbox.is_empty());
    assert_eq!(shown(&handle.read()), 1, "the room's history holds it once");
}

#[test]
fn a_receipt_the_room_refuses_is_said_and_no_longer_shown() {
    let (chrome, _guard) = crate::shell::native_workspace_owner::test_chrome_governing_its_room();
    let handle = chrome.borrow().tree_handle();
    let room = handle
        .read()
        .desktop
        .active_workspace()
        .expect("a workspace")
        .room();
    let before = document_of(&workspace_of(&handle, room), &[], &[]);
    handle.write().admit_room_document_for_test(room, &before);
    let receipt = receipt();
    assert!(handle.write().record_in_room(room, receipt.clone()));

    // This device cannot write it: here it holds no custody of the room to
    // sign it with.
    let (network, _commands) = super::shell_network_for_test(handle.clone(), 1);
    network.send_outbox_off_loop();
    assert!(handle.read().room_outbox.is_empty());
    assert!(
        !workspace_of(&handle, room)
            .effect_receipts
            .iter()
            .any(|held| held.invocation == receipt.invocation),
        "the room's history does not show what the room refused"
    );
    assert!(
        crate::shell::workspace_governance::report_governance_refusals(&mut chrome.borrow_mut())
    );
    let title = crate::shell::i18n::t_args(
        "host-action-cannot-run",
        &[(
            "title",
            crate::shell::i18n::t("governance-change-history").into(),
        )],
    );
    let state = chrome.borrow();
    let said = state
        .notifications()
        .iter()
        .find(|notice| notice.title == title)
        .expect("the refusal is said");
    assert!(said.body.contains("custody"), "why: {}", said.body);
}

/// The grant `agent-notes`: the agent `claude-code` may read and write Notes in
/// `workspace`, given by `by`.
fn notes_grant(workspace: &Workspace, by: &str) -> tilo_action_policy::Grant {
    tilo_action_policy::Grant::new(
        "agent-notes",
        tilo_action_policy::Subject::agent("claude-code"),
        workspace.node().app("so.tilo.notes"),
        tilo_action_policy::KindSet::NONE
            .with(tilo_action_policy::ActionKind::Read)
            .with(tilo_action_policy::ActionKind::Undoable),
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(by.to_owned()),
    )
}

/// What the room's policy here answers the agent for a write to Notes: what
/// every door's admission, and every admitted job's check before its
/// effect, decides on.
fn agent_writes_notes(handle: &WorkspaceTreeHandle, room: RoomId) -> tilo_action_policy::Answer {
    let workspace = workspace_of(handle, room);
    workspace
        .subject_authority(tilo_action_policy::Subject::agent("claude-code"))
        .decide(
            &workspace.node().app("so.tilo.notes"),
            tilo_action_policy::ActionKind::Undoable,
            &tilo_desktop_model::context_at(tilo_clock::unix_now()),
        )
        .answer
}

fn revoke(handle: &WorkspaceTreeHandle, room: RoomId) {
    handle.write().request_governance(
        room,
        crate::shell::state::GovernanceRequest::Policy(tilo_action_policy::Change::Revoke(
            tilo_action_policy::GrantId::new("agent-notes"),
        )),
    );
}

#[test]
fn a_revoked_grant_ends_here_at_once_and_stays_ended_until_the_room_carries_it() {
    use tilo_action_policy::Answer;
    let (chrome, _guard) = crate::shell::native_workspace_owner::test_chrome_governing_its_room();
    let governor = chrome.borrow().active_local_user();
    let handle = chrome.borrow().tree_handle();
    let room = handle
        .read()
        .desktop
        .active_workspace()
        .expect("a workspace")
        .room();
    let mut granting = workspace_of(&handle, room);
    let grant = notes_grant(&granting, &governor);
    granting.policy.grants.push(grant.clone());
    let granted = document_of(&granting, &[], &[]);
    handle.write().admit_room_document_for_test(room, &granted);
    assert_eq!(agent_writes_notes(&handle, room), Answer::Allow);

    revoke(&handle, room);
    assert_eq!(
        agent_writes_notes(&handle, room),
        Answer::Deny,
        "the revocation ends the grant here before the room's governor commits it"
    );
    // Not written yet, and the room commits on.
    for name in ["Plans", "Plans again"] {
        handle
            .write()
            .admit_room_document_for_test(room, &after_another_commit(&granted, name));
        assert_eq!(
            agent_writes_notes(&handle, room),
            Answer::Deny,
            "a commit of the room made before the revocation reached it does not grant it again"
        );
    }

    // The revocation lands.
    let revoked = document_of(&workspace_of(&handle, room), &[], &[]);
    handle.write().admit_room_document_for_test(room, &revoked);
    assert_eq!(agent_writes_notes(&handle, room), Answer::Deny);
    assert!(
        handle.read().room_outbox.is_empty(),
        "nothing waits once the room carries the revocation"
    );

    // The same grant given again is the room's to carry.
    let mut regranting = workspace_of(&handle, room);
    regranting.policy.grants.push(grant);
    handle
        .write()
        .admit_room_document_for_test(room, &document_of(&regranting, &[], &[]));
    assert_eq!(agent_writes_notes(&handle, room), Answer::Allow);
}

/// A revocation this device cannot write (here: it holds no custody of the
/// room to sign it with) is refused, and the grant is the room's again.
#[test]
fn a_grant_whose_revocation_is_refused_here_is_the_rooms_again() {
    use tilo_action_policy::Answer;
    let (chrome, _guard) = crate::shell::native_workspace_owner::test_chrome_governing_its_room();
    let governor = chrome.borrow().active_local_user();
    let handle = chrome.borrow().tree_handle();
    let room = handle
        .read()
        .desktop
        .active_workspace()
        .expect("a workspace")
        .room();
    let mut granting = workspace_of(&handle, room);
    granting
        .policy
        .grants
        .push(notes_grant(&granting, &governor));
    handle
        .write()
        .admit_room_document_for_test(room, &document_of(&granting, &[], &[]));

    revoke(&handle, room);
    assert_eq!(agent_writes_notes(&handle, room), Answer::Deny);
    let (network, _commands) = super::shell_network_for_test(handle.clone(), 1);
    network.send_outbox_off_loop();
    assert!(handle.read().room_outbox.is_empty());
    assert_eq!(
        agent_writes_notes(&handle, room),
        Answer::Allow,
        "the room's grants stand until a revocation is written"
    );
}
