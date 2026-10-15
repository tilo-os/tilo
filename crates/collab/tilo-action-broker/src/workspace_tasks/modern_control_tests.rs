use super::*;

use std::collections::BTreeMap;
use tilo_desktop_model::{
    WorkspaceTaskActiveRun, WorkspaceTaskControl, WorkspaceTaskDirection,
    WorkspaceTaskDirectionApplied, WorkspaceTaskExecution, WorkspaceTaskExecutionPhase,
    WorkspaceTaskMutationReceipt, WorkspaceTaskStatus,
};

fn task(id: &str, agent: &str, route: WorkspaceTaskRoute) -> WorkspaceTask {
    WorkspaceTask {
        id: format!("task:{id}"),
        title: format!("Task {id}"),
        created_by: "person".into(),
        assigned_agent: Some(agent.into()),
        route,
        status: WorkspaceTaskStatus::InProgress,
        resource: None,
        created_at: 1,
        updated_at: 2,
        audit: Vec::new(),
    }
}

fn control(agent: &str, epoch: u64, after: u64, applied: u64) -> WorkspaceTaskControl {
    WorkspaceTaskControl {
        run_epoch: epoch,
        instruction: Some("Private full task instruction".into()),
        active_run: Some(WorkspaceTaskActiveRun {
            epoch,
            agent_id: agent.into(),
            started_at: 2,
            applied_direction_revision: applied,
            approval: None,
        }),
        execution: Some(WorkspaceTaskExecution {
            run_epoch: epoch,
            sequence: 2,
            phase: WorkspaceTaskExecutionPhase::Executing,
            updated_at: 3,
        }),
        directions: (1..=after)
            .map(|revision| WorkspaceTaskDirection {
                revision,
                invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(u128::from(revision))),
                author: "person".into(),
                text: format!("Direction {revision}"),
                created_at: 2,
                applied: (revision <= applied).then(|| WorkspaceTaskDirectionApplied {
                    agent_id: agent.into(),
                    run_epoch: epoch,
                    applied_at: 3,
                }),
            })
            .collect(),
        ..WorkspaceTaskControl::default()
    }
}

fn snapshot() -> WorkspaceTaskSnapshot {
    let tasks = vec![
        task("model", "scribe", WorkspaceTaskRoute::Model),
        task("rules", "reed", WorkspaceTaskRoute::RulesOnly),
    ];
    let controls = BTreeMap::from([
        (tasks[0].id.clone(), control("scribe", 7, 2, 1)),
        (tasks[1].id.clone(), control("reed", 4, 0, 0)),
    ]);
    WorkspaceTaskSnapshot {
        tasks,
        controls,
        grants: Vec::new(),
    }
}

fn envelope(
    resource: &str,
    action: &str,
    arguments: Option<serde_json::Value>,
) -> InvocationEnvelope {
    InvocationEnvelope {
        id: 1,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(91)),
        resource: resource.into(),
        action: action.into(),
        argument: arguments
            .map(|value| format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, value)),
        actor: "not-authority".into(),
        context: None,
    }
}

fn action_ids(snapshot: &WorkspaceTaskSnapshot) -> Vec<(String, String)> {
    actions(
        snapshot,
        "Workspace",
        &std::collections::BTreeSet::from(["scribe".into(), "reed".into()]),
    )
    .into_iter()
    .map(|action| (action.resource, action.action))
    .collect()
}

#[test]
fn removed_agent_withdraws_its_task_controls_from_the_current_surface() {
    let snapshot = snapshot();
    let ids = actions(
        &snapshot,
        "Workspace",
        &std::collections::BTreeSet::from(["reed".into()]),
    )
    .into_iter()
    .map(|action| (action.resource, action.action))
    .collect::<Vec<_>>();
    assert!(
        !ids.iter()
            .any(|(resource, _)| resource == "desktop:task:model")
    );
    assert!(ids.contains(&("desktop:task:rules".into(), "stop".into())));
}

#[test]
fn queue_keeps_the_full_instruction_distinct_from_its_public_title() {
    let instruction = "Read the complete proposal and identify every dependency. ".repeat(12);
    assert!(instruction.chars().count() > 240);
    let request = envelope(
        COLLECTION,
        "queue",
        Some(serde_json::json!({
            "title": "Review proposal",
            "instruction": instruction,
            "assigned_agent": "scribe",
            "planning": "model"
        })),
    );
    assert!(matches!(
        command(&request).unwrap(),
        Some(WorkspaceTaskCommand::QueueInstruction {
            title,
            instruction: retained,
            assigned_agent,
            route: WorkspaceTaskRoute::Model,
        }) if title == "Review proposal" && retained == instruction && assigned_agent == "scribe"
    ));
    assert!(action_ids(&snapshot()).contains(&(COLLECTION.into(), "queue".into())));
}

#[test]
fn queue_about_a_selection_carries_its_resource_and_anchor() {
    let resource = serde_json::json!({
        "app_id": "so.tilo.word-processor",
        "app_label": "Writer",
        "id": "doc:proposal",
        "kind": "word-processor.document",
        "label": "Proposal",
        "anchor": {
            "place": {"item": "block-3", "start": 4, "end": 19},
            "excerpt": "the second claim"
        }
    });
    let request = envelope(
        COLLECTION,
        "queue",
        Some(serde_json::json!({
            "title": "Tighten this",
            "instruction": "Tighten the selected sentence",
            "assigned_agent": "scribe",
            "planning": "model",
            "resource_json": resource.to_string(),
        })),
    );
    let Some(WorkspaceTaskCommand::QueueInstructionOn {
        instruction,
        resource,
        route: WorkspaceTaskRoute::Model,
        ..
    }) = command(&request).unwrap()
    else {
        panic!("a queue about a resource keeps the resource")
    };
    assert_eq!(instruction, "Tighten the selected sentence");
    assert_eq!(resource.id, "doc:proposal");
    let anchor = resource
        .anchor
        .expect("the selection travels with the task");
    assert_eq!(
        anchor.place,
        Some(tilo_actions::SelectionPlace::range("block-3", 4, 19))
    );
    assert_eq!(anchor.excerpt.as_deref(), Some("the second claim"));

    for forged in [
        serde_json::json!({"app_id":"so.tilo.word-processor","app_label":"Writer","id":"doc","kind":"k","label":"l","anchor":{"place":{"item":"b","start":9,"end":2}}}),
        serde_json::json!({"app_id":"so.tilo.word-processor","app_label":"Writer","id":"doc","kind":"k","label":"l","anchor":{"lease":"x"}}),
        serde_json::json!({"app_id":"so.tilo.word-processor","app_label":"Writer","id":"doc","kind":"k","label":"l","agent_request":{}}),
    ] {
        let request = envelope(
            COLLECTION,
            "queue",
            Some(serde_json::json!({
                "title": "Tighten this",
                "instruction": "Tighten it",
                "assigned_agent": "scribe",
                "planning": "model",
                "resource_json": forged.to_string(),
            })),
        );
        assert!(command(&request).is_err(), "{forged} must be refused");
    }
}

#[test]
fn queued_instruction_receipt_completes_the_modern_queue_action() {
    let mut snapshot = snapshot();
    let task = snapshot
        .tasks
        .iter_mut()
        .find(|task| task.id == "task:model")
        .unwrap();
    task.status = WorkspaceTaskStatus::Queued;
    snapshot
        .controls
        .get_mut("task:model")
        .unwrap()
        .receipts
        .push(WorkspaceTaskMutationReceipt {
            device: [9; 32],
            invocation: uuid::Uuid::from_u128(91),
            request_digest: [3; 32],
            result: WorkspaceTaskMutationResult::Queued {
                task_id: "task:model".into(),
            },
        });
    let queued = envelope(
        COLLECTION,
        "queue",
        Some(serde_json::json!({
            "title":"Task model",
            "instruction":"Private full task instruction",
            "assigned_agent":"scribe",
            "planning":"model"
        })),
    );

    let completed = result(&snapshot, &queued, [9; 32], 12).unwrap();
    assert_eq!(completed["task"]["id"], "task:model");
    assert_eq!(completed["managed_route"], true);
    assert!(
        !completed
            .to_string()
            .contains("Private full task instruction")
    );
}

#[test]
fn current_owner_surface_offers_only_controls_the_exact_run_supports() {
    let snapshot = snapshot();
    let ids = action_ids(&snapshot);
    assert!(ids.contains(&(COLLECTION.into(), "read-current-tasks".into())));
    assert!(ids.contains(&("desktop:task:model".into(), "read-task".into())));
    assert!(ids.contains(&("desktop:task:model".into(), "direct".into())));
    assert!(ids.contains(&("desktop:task:model".into(), "stop".into())));
    assert!(ids.contains(&("desktop:task:rules".into(), "read-task".into())));
    assert!(!ids.contains(&("desktop:task:rules".into(), "direct".into())));
    assert!(ids.contains(&("desktop:task:rules".into(), "stop".into())));

    let mut withdrawn = snapshot;
    withdrawn.controls.get_mut("task:model").unwrap().active_run = None;
    let withdrawn = action_ids(&withdrawn);
    assert!(!withdrawn.contains(&("desktop:task:model".into(), "direct".into())));
    assert!(!withdrawn.contains(&("desktop:task:model".into(), "stop".into())));
}

#[test]
fn current_reads_filter_the_exact_agent_and_keep_private_directions_out() {
    let snapshot = snapshot();
    let current = envelope(
        COLLECTION,
        "read-current-tasks",
        Some(serde_json::json!({"agent":"scribe"})),
    );
    assert!(is_inspect(&current));
    let read = inspect(&snapshot, &current).unwrap();
    assert_eq!(read["complete"], true);
    assert_eq!(read["total"], 1);
    assert_eq!(read["tasks"][0]["task_id"], "task:model");
    assert_eq!(read["tasks"][0]["run_epoch"], 7);
    assert_eq!(read["tasks"][0]["after_revision"], 2);
    assert_eq!(read["tasks"][0]["applied_revision"], 1);
    assert!(!read.to_string().contains("Direction 2"));
    assert!(!read.to_string().contains("Private full task instruction"));

    let other = envelope(
        COLLECTION,
        "read-current-tasks",
        Some(serde_json::json!({"agent":"other"})),
    );
    assert_eq!(inspect(&snapshot, &other).unwrap()["total"], 0);

    let exact = envelope("desktop:task:model", "read-task", None);
    assert_eq!(
        inspect(&snapshot, &exact).unwrap()["active_agent"],
        "scribe"
    );
    assert!(
        inspect(
            &snapshot,
            &envelope("desktop:task:missing", "read-task", None)
        )
        .is_err()
    );
}

#[test]
fn private_conversation_is_exact_run_scoped_and_absent_from_public_reads() {
    let snapshot = snapshot();
    let private = envelope(
        "desktop:task:model",
        "read-conversation",
        Some(serde_json::json!({"run_epoch":7})),
    );
    // The broker routes this through owner-person certificate custody and the
    // current assigned-agent membership check, just like task control reads.
    assert!(is_inspect(&private));
    assert_eq!(
        inspection_agent(&snapshot, &private).unwrap().as_deref(),
        Some("scribe")
    );
    let conversation = inspect(&snapshot, &private).unwrap();
    assert_eq!(conversation["turns"].as_array().unwrap().len(), 3);
    assert_eq!(
        conversation["turns"][0]["text"],
        "Private full task instruction"
    );
    assert_eq!(conversation["turns"][1]["applied"]["run_epoch"], 7);
    assert!(conversation["turns"][2]["applied"].is_null());
    for action in ["inspect", "read-task"] {
        let public = inspect(&snapshot, &envelope("desktop:task:model", action, None)).unwrap();
        assert!(!public.to_string().contains("Private full task instruction"));
        assert!(!public.to_string().contains("Direction 2"));
    }
    for query in [
        serde_json::json!({"run_epoch":8}),
        serde_json::json!({"run_epoch":7,"caller":"person"}),
    ] {
        assert!(
            inspect(
                &snapshot,
                &envelope("desktop:task:model", "read-conversation", Some(query))
            )
            .is_err()
        );
    }
}

#[test]
fn direct_and_stop_decode_only_the_observed_task_run_frontier() {
    let direct = envelope(
        "desktop:task:model",
        "direct",
        Some(serde_json::json!({
            "run_epoch": 7,
            "after_revision": 2,
            "text": "Use the smaller proof"
        })),
    );
    assert!(matches!(
        command(&direct).unwrap(),
        Some(WorkspaceTaskCommand::DirectionForRun {
            task_id,
            run_epoch: 7,
            after_revision: 2,
            text,
        }) if task_id == "task:model" && text == "Use the smaller proof"
    ));
    let stop = envelope(
        "desktop:task:model",
        "stop",
        Some(serde_json::json!({"run_epoch":7,"after_revision":2})),
    );
    assert!(matches!(
        command(&stop).unwrap(),
        Some(WorkspaceTaskCommand::CancelForRun {
            task_id,
            run_epoch: 7,
            after_revision: 2,
        }) if task_id == "task:model"
    ));
    for malformed in [
        envelope("desktop:task:", "direct", Some(serde_json::json!({}))),
        envelope(
            "desktop:task:model",
            "direct",
            Some(serde_json::json!({"run_epoch":7,"after_revision":1,"text":"x","agent":"reed"})),
        ),
        envelope(
            "desktop:task:model",
            "stop",
            Some(serde_json::json!({"run_epoch":7})),
        ),
    ] {
        assert!(command(&malformed).is_err() || command(&malformed).unwrap().is_none());
    }
}

#[test]
fn direction_receipt_is_saved_but_only_owner_ack_makes_it_applied() {
    let mut snapshot = snapshot();
    snapshot
        .controls
        .get_mut("task:model")
        .unwrap()
        .receipts
        .push(WorkspaceTaskMutationReceipt {
            device: [9; 32],
            invocation: uuid::Uuid::from_u128(91),
            request_digest: [3; 32],
            result: WorkspaceTaskMutationResult::DirectionAdded {
                task_id: "task:model".into(),
                revision: 2,
            },
        });
    let direct = envelope(
        "desktop:task:model",
        "direct",
        Some(serde_json::json!({"run_epoch":7,"after_revision":1,"text":"continue"})),
    );
    let saved = result(&snapshot, &direct, [9; 32], 12).unwrap();
    assert_eq!(saved["saved"], true);
    assert_eq!(saved["applied"], false);
    snapshot
        .controls
        .get_mut("task:model")
        .unwrap()
        .active_run
        .as_mut()
        .unwrap()
        .applied_direction_revision = 2;
    assert_eq!(
        result(&snapshot, &direct, [9; 32], 13).unwrap()["applied"],
        true
    );
    let other = envelope(
        "desktop:task:rules",
        "direct",
        Some(serde_json::json!({"run_epoch":7,"after_revision":1,"text":"continue"})),
    );
    assert!(result(&snapshot, &other, [9; 32], 13).is_err());
}

#[test]
fn inspection_redacts_private_instruction_in_legacy_chat_attachment() {
    let mut state = snapshot();
    state.tasks[0].resource = Some(tilo_desktop_model::WorkspaceTaskResource {
        app_id: Some("so.tilo.chat".into()),
        app_label: Some("Chat".into()),
        id: "message:source".into(),
        kind: "message".into(),
        label: "Source".into(),
        agent_request: Some(tilo_desktop_model::WorkspaceAgentRequest {
            chat_record: "record".into(),
            channel_id: "channel".into(),
            message_id: "message".into(),
            instruction: "Private attachment instruction".into(),
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(92)),
        }),
        anchor: None,
    });
    let read = inspect(&state, &envelope("desktop:task:model", "inspect", None)).unwrap();
    assert_eq!(read["task"]["resource"]["id"], "message:source");
    assert!(read["task"]["resource"].get("agent_request").is_none());
    assert!(!read.to_string().contains("Private"));
    assert!(
        state.tasks[0]
            .resource
            .as_ref()
            .unwrap()
            .agent_request
            .is_some(),
        "inspection does not erase the executable source"
    );
}
