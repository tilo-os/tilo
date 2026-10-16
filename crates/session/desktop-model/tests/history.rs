//! A workspace's append-only history stays out of what reads the desktop as
//! it is now: [`history::without_history`] serialisation and
//! [`Workspace::policy_view`].

use collab_types::caps::{Access, Actor, CapOverride, ResourceRef, Scope};
use tilo_action_policy::{ActionKind, Answer, Grant, KindSet, Subject, TreePath};
use tilo_actions::{ActionContext, EffectReceiptStatus, InvocationId};
use tilo_desktop_model::*;

/// The clock these fixtures run at, Unix seconds.
const NOW: u64 = 1_800_000_000;

fn receipt(n: u128) -> WorkspaceEffectReceipt {
    WorkspaceEffectReceipt {
        invocation: InvocationId(uuid::Uuid::from_u128(n)),
        actor: "owner".into(),
        context: ActionContext::default(),
        status: EffectReceiptStatus::Succeeded,
        completed_at: 42,
    }
}

fn task(audit: usize) -> WorkspaceTask {
    WorkspaceTask {
        id: "task-1".into(),
        title: "Tidy up".into(),
        created_by: "owner".into(),
        assigned_agent: None,
        route: WorkspaceTaskRoute::Manual,
        status: WorkspaceTaskStatus::Queued,
        resource: None,
        subject: None,
        created_at: 1,
        updated_at: 2,
        audit: (0..audit)
            .map(|at| {
                WorkspaceTaskAuditEvent::new(
                    i64::try_from(at).unwrap_or(i64::MAX),
                    "owner",
                    "retry",
                    [],
                )
            })
            .collect(),
    }
}

/// A desktop whose one workspace has `receipts` effect receipts and a task
/// with as many audit events.
fn desktop(receipts: u128) -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceId(1), "Home");
    workspace.set_owner("owner");
    workspace.strips.push(Strip::new(StripId(7), "Main"));
    workspace.effect_receipts = (0..receipts).map(receipt).collect();
    workspace.tasks = vec![task(usize::try_from(receipts).unwrap_or(usize::MAX))];
    DesktopTree {
        workspaces: vec![workspace],
        ..DesktopTree::default()
    }
}

#[test]
fn serialising_without_history_leaves_out_every_history_field() {
    let tree = desktop(3);
    let full = serde_json::to_value(&tree).unwrap();
    let workspace = &full["workspaces"][0];
    assert_eq!(workspace["effect_receipts"].as_array().unwrap().len(), 3);
    assert_eq!(workspace["tasks"][0]["audit"].as_array().unwrap().len(), 3);
    assert!(workspace.get("ownership_transfer_audit").is_some());
    assert!(workspace.get("ownership_transfer_cancellations").is_some());

    let current: serde_json::Value =
        history::without_history(|| serde_json::to_value(&tree)).unwrap();
    let workspace = &current["workspaces"][0];
    for field in [
        "effect_receipts",
        "ownership_transfer_audit",
        "ownership_transfer_cancellations",
    ] {
        assert!(workspace.get(field).is_none(), "{field} is history");
    }
    assert!(workspace["tasks"][0].get("audit").is_none());
    // Everything that is not history is still there.
    assert_eq!(workspace["tasks"][0]["title"], "Tidy up");
    assert_eq!(workspace["strips"][0]["name"], "Main");
    assert_eq!(workspace["policy"]["owner"], "owner");

    // The switch is scoped: serialising after it writes history again, so
    // persistence and replication are never affected.
    let again = serde_json::to_value(&tree).unwrap();
    assert_eq!(again, full);
}

#[test]
fn a_dump_without_history_is_the_same_size_whatever_the_history() {
    let size = |receipts| {
        let tree = desktop(receipts);
        history::without_history(|| serde_json::to_vec(&tree))
            .unwrap()
            .len()
    };
    assert_eq!(size(0), size(10_000));
}

#[test]
fn nested_switches_restore_the_outer_one() {
    let tree = desktop(1);
    let inner_then_outer = history::without_history(|| {
        history::without_history(|| ());
        serde_json::to_value(&tree)
    })
    .unwrap();
    assert!(
        inner_then_outer["workspaces"][0]
            .get("effect_receipts")
            .is_none()
    );
}

#[test]
fn a_policy_view_decides_like_the_workspace_and_carries_no_layout_or_history() {
    let mut workspace = desktop(50).workspaces.remove(0);
    workspace.admit("editor", tilo_action_policy::Role::Editor);
    let notes = workspace.node().app("so.tilo.notes");
    workspace.grant(Grant::new(
        "notes-reads",
        Subject::app("so.tilo.notes"),
        notes,
        KindSet::READ,
        Answer::Allow,
        Subject::person("owner"),
    ));
    workspace.grant_sandbox(CapOverride::grant(
        collab_types::caps::Subject::app("so.tilo.notes"),
        Scope::FsPath("/var/workspace/notes".into()),
        Access::Read,
    ));
    let view = workspace.policy_view();

    assert!(view.strips.is_empty());
    assert!(view.tasks.is_empty());
    assert!(view.effect_receipts.is_empty());
    assert!(view.shortcuts.is_empty());
    assert!(view.schedule_claims.is_empty());
    assert!(view.ownership_transfer_audit.is_empty());
    assert!(view.ownership_transfer_cancellations.is_empty());
    assert_eq!(
        (view.id, &view.room, &view.name),
        (workspace.id, &workspace.room, &workspace.name)
    );

    let people = ["owner", "editor", "stranger"];
    let decided_at = [workspace.node(), workspace.node().app("so.tilo.notes")];
    for person in people {
        assert_eq!(
            view.may_govern_at(person, tilo_action_policy::roles::governance::GRANT, NOW),
            workspace.may_govern_at(person, tilo_action_policy::roles::governance::GRANT, NOW),
            "{person}"
        );
        assert_eq!(view.role_of(person), workspace.role_of(person));
        for kind in ActionKind::ALL {
            for node in &decided_at {
                assert_eq!(
                    view.answer_at(person, node, kind, NOW),
                    workspace.answer_at(person, node, kind, NOW),
                    "{person} {kind:?} {node}"
                );
            }
        }
    }
    let app = |ws: &Workspace| {
        ws.subject_authority(Subject::app("so.tilo.notes"))
            .decide(
                &ws.node().join(&TreePath::root().app("so.tilo.notes")),
                ActionKind::Read,
                &context_at(NOW),
            )
            .answer
    };
    assert_eq!(app(&view), app(&workspace));
    let code = Actor::app("so.tilo.notes").in_room(workspace.room.to_string());
    let notes_files = ResourceRef::FsPath("/var/workspace/notes/a.md".into());
    assert_eq!(
        view.sandbox_decide_at(&code, &notes_files, Access::Read, NOW),
        workspace.sandbox_decide_at(&code, &notes_files, Access::Read, NOW),
    );
}
