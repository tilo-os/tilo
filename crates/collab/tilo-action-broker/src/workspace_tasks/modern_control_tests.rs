use super::*;

use std::collections::BTreeMap;
use tilo_desktop_model::{
    WorkspaceTaskActiveRun, WorkspaceTaskControl, WorkspaceTaskDirection,
    WorkspaceTaskDirectionApplied, WorkspaceTaskExecution, WorkspaceTaskExecutionPhase,
    WorkspaceTaskMutationReceipt, WorkspaceTaskStatus,
};

fn task(id: &str, agent: &str, route: WorkspaceTaskRoute) -> WorkspaceTask {
    WorkspaceTask {
        parent_task_id: None,
        child_order: None,
        labels: Vec::new(),
        responsible_assignees: Vec::new(),
        id: format!("task:{id}"),
        title: format!("Task {id}"),
        created_by: "person".into(),
        assigned_agent: Some(agent.into()),
        route,
        status: WorkspaceTaskStatus::InProgress,
        resource: None,
        subject: None,
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

#[test]
fn registered_metadata_actions_parse_and_filter_real_task_frontiers() {
    let mut state = snapshot();
    state.tasks[1].parent_task_id = Some(state.tasks[0].id.clone());
    state.tasks[1].labels = vec!["review".into()];
    state.tasks[1].responsible_assignees = vec!["person".into()];
    let catalogue = actions(
        &state,
        "Workspace",
        &std::collections::BTreeSet::from(["scribe".into(), "reed".into()]),
    );
    for verb in ["create-subtask", "move-task", "set-labels", "set-assignees"] {
        assert!(
            catalogue
                .iter()
                .any(|action| action.resource == "desktop:task:model" && action.action == verb)
        );
    }
    assert!(
        catalogue
            .iter()
            .any(|action| action.resource == COLLECTION && action.action == "list-tasks")
    );
    let request = envelope(
        "desktop:task:model",
        "create-subtask",
        Some(serde_json::json!({"title":"Child", "assigned_agent":"reed", "route":"rules_only"})),
    );
    assert!(
        matches!(command(&request).unwrap(), Some(WorkspaceTaskCommand::CreateSubtask { assigned_agent: Some(agent), route: Some(WorkspaceTaskRoute::RulesOnly), .. }) if agent == "reed")
    );
    let request = envelope(
        COLLECTION,
        "list-tasks",
        Some(
            serde_json::json!({"parent_task_id":"task:model", "label":"review", "assignee":"person"}),
        ),
    );
    assert!(is_inspect(&request));
    let value = inspect(&state, &request).unwrap();
    assert_eq!(value["total"], 1);
    assert_eq!(value["tasks"][0]["task_id"], "task:rules");
    assert_eq!(
        value["tasks"][0]["responsible_assignees"],
        serde_json::json!(["person"])
    );
    let parent = inspect(&state, &envelope("desktop:task:model", "read-task", None)).unwrap();
    assert_eq!(parent["child_progress"]["in_progress"], 1);
    assert!(!value.to_string().contains("Private"));
}

#[test]
fn unassigned_model_task_publishes_approval_with_optional_executor() {
    let mut state = snapshot();
    state.tasks[0].assigned_agent = None;
    state.tasks[0].status = WorkspaceTaskStatus::Queued;
    state.controls.get_mut("task:model").unwrap().active_run = None;
    let catalogue = actions(&state, "Workspace", &std::collections::BTreeSet::default());
    let approve = catalogue
        .iter()
        .find(|row| row.resource == "desktop:task:model" && row.action == "approve-run")
        .expect("unassigned model tasks must offer conscious Start before an executor exists");
    let agent = approve
        .params
        .iter()
        .find(|param| param.name == "agent_id")
        .unwrap();
    assert!(!agent.required);
    for name in ["expected_run_epoch", "after_revision"] {
        assert!(
            approve
                .params
                .iter()
                .find(|param| param.name == name)
                .unwrap()
                .required
        );
    }
    assert!(
        catalogue
            .iter()
            .any(|row| row.resource == COLLECTION && row.action == "read-task-compute")
    );
}

#[test]
fn omitted_approval_executor_is_stable_per_invocation_not_per_task() {
    let mut request = envelope(
        "desktop:task:model",
        "approve-run",
        Some(serde_json::json!({"expected_run_epoch":0,"after_revision":0})),
    );
    let selected = |request: &InvocationEnvelope| {
        let Some(WorkspaceTaskCommand::ApproveRun {
            task_id,
            agent_id,
            expected_run_epoch,
            after_revision,
        }) = command(request).unwrap()
        else {
            panic!("approval command")
        };
        assert_eq!(task_id, "task:model");
        assert_eq!((expected_run_epoch, after_revision), (0, 0));
        agent_id
    };
    let first = selected(&request);
    assert_eq!(first, format!("native-{}", request.invocation.0));
    request.id += 1;
    assert_eq!(
        selected(&request),
        first,
        "transport retry preserves actor selection"
    );
    request.invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(92));
    assert_ne!(
        selected(&request),
        first,
        "a new conscious Start selects a fresh actor"
    );
    request.argument = Some(format!(
        "{}{}",
        tilo_actions::ARGUMENTS_JSON_PREFIX,
        serde_json::json!({"agent_id":"scribe","expected_run_epoch":0,"after_revision":0})
    ));
    assert_eq!(selected(&request), "scribe");
}

#[test]
fn private_children_are_queued_without_approving_a_run() {
    let request = envelope(
        "desktop:task:model",
        "queue-private-subtask",
        Some(serde_json::json!({
            "title": "Review section",
            "instruction": "Private review goal",
            "route": "model"
        })),
    );
    assert!(matches!(
        command(&request).unwrap(),
        Some(WorkspaceTaskCommand::QueuePrivateSubtask {
            parent_task_id, title, instruction, assigned_agent, route: WorkspaceTaskRoute::Model,
        }) if parent_task_id == "task:model" && title == "Review section"
            && instruction == "Private review goal" && assigned_agent.is_empty()
    ));
    for invalid in [
        serde_json::json!({"title":"Child","instruction":"Goal","route":"manual"}),
        serde_json::json!({"title":"Child","instruction":"Goal","route":"model","approval":true}),
    ] {
        assert!(
            command(&envelope(
                "desktop:task:model",
                "queue-private-subtask",
                Some(invalid)
            ))
            .is_err()
        );
    }
    let catalogue = action_ids(&snapshot());
    assert!(catalogue.contains(&("desktop:task:model".into(), "queue-private-subtask".into())));
    assert!(catalogue.contains(&("desktop:task:model".into(), "send-message".into())));
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
    assert!(ids.contains(&("desktop:task:model".into(), "read-task".into())));
    assert!(
        !ids.iter()
            .any(|(resource, action)| resource == "desktop:task:model" && action != "read-task")
    );
    assert!(ids.contains(&("desktop:task:rules".into(), "stop".into())));
}

#[test]
fn windowless_task_source_is_the_real_published_task_resource() {
    let snapshot = snapshot();
    let source = model_source(&snapshot, "room", "task:model").unwrap();
    let tilo_joined_room::task_admission::model::NativeModelSource::Task { task_id, resource } =
        source
    else {
        panic!("semantic task source")
    };
    assert_eq!(task_id, "task:model");
    assert_eq!(
        resource,
        tilo_joined_room::NativeModelContract::task_resource("room", &task_id)
    );
    let published = actions(
        &snapshot,
        "Workspace",
        &std::collections::BTreeSet::default(),
    );
    assert!(
        published
            .iter()
            .any(|action| action.resource == task_resource_id(&task_id)
                && action.resource_kind == tilo_desktop_model::task::TASK_KIND
                && action.action == "read-task")
    );
    assert!(model_source(&snapshot, "room", "task:absent").is_err());
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
fn queue_about_a_selection_carries_its_subject_and_attaches_nothing() {
    let subject = serde_json::json!({
        "app_id": "so.tilo.files",
        "app_label": "Files",
        "resource": {"id": "files:folder/reports", "kind": "files.folder", "label": "Reports"},
        "anchor": {
            "items": [{"item": "q1.pdf"}, {"item": "q2.pdf"}],
            "excerpt": "- q1.pdf\n- q2.pdf"
        }
    });
    let queue = |subject: &serde_json::Value| {
        envelope(
            COLLECTION,
            "queue",
            Some(serde_json::json!({
                "title": "Summarize these",
                "instruction": "Summarize the selected reports",
                "assigned_agent": "scribe",
                "planning": "model",
                "subject_json": subject.to_string(),
            })),
        )
    };
    let Some(WorkspaceTaskCommand::QueueInstructionAbout {
        instruction,
        subject: about,
        route: WorkspaceTaskRoute::Model,
        ..
    }) = command(&queue(&subject)).unwrap()
    else {
        panic!("a queue about a selection keeps the selection")
    };
    assert_eq!(instruction, "Summarize the selected reports");
    assert_eq!(about.resource.id, "files:folder/reports");
    assert_eq!(
        about.anchor.items,
        [
            tilo_actions::SelectionPlace::item("q1.pdf"),
            tilo_actions::SelectionPlace::item("q2.pdf")
        ]
    );

    for forged in [
        serde_json::json!({"app_id":"so.tilo.files","app_label":"Files","resource":{"id":"r","kind":"k","label":"l"},"anchor":{"place":{"item":"b","start":9,"end":2}}}),
        serde_json::json!({"app_id":"so.tilo.files","app_label":"Files","resource":{"id":"r","kind":"k","label":"l"},"anchor":{"lease":"x"}}),
        serde_json::json!({"app_id":"so.tilo.files","app_label":"Files","resource":{"id":"r","kind":"k","label":"l"},"anchor":{}}),
        serde_json::json!({"app_id":"so.tilo.files","app_label":"Files","resource":{"id":"r","kind":"k","label":"l"},"anchor":{"excerpt":"x"},"scope":"room"}),
    ] {
        assert!(
            command(&queue(&forged)).is_err(),
            "{forged} must be refused"
        );
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
    assert!(ids.contains(&("desktop:task:model".into(), "steer".into())));
    assert!(ids.contains(&("desktop:task:model".into(), "direct".into())));
    assert!(ids.contains(&("desktop:task:model".into(), "wait-task-message".into())));
    assert!(ids.contains(&("desktop:task:model".into(), "stop".into())));
    assert!(ids.contains(&("desktop:task:rules".into(), "read-task".into())));
    assert!(!ids.contains(&("desktop:task:rules".into(), "direct".into())));
    assert!(!ids.contains(&("desktop:task:rules".into(), "steer".into())));
    assert!(ids.contains(&("desktop:task:rules".into(), "stop".into())));

    let mut withdrawn = snapshot;
    withdrawn.controls.get_mut("task:model").unwrap().active_run = None;
    let withdrawn = action_ids(&withdrawn);
    assert!(!withdrawn.contains(&("desktop:task:model".into(), "direct".into())));
    assert!(!withdrawn.contains(&("desktop:task:model".into(), "steer".into())));
    assert!(!withdrawn.contains(&("desktop:task:model".into(), "wait-task-message".into())));
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
    let mut snapshot = snapshot();
    snapshot
        .controls
        .get_mut("task:model")
        .unwrap()
        .presentations
        .insert(
            "7:0".into(),
            collab_types::task_presentation::TaskPresentation {
                run: "7:0".into(),
                received_at: Some(1),
                sequence: 1,
                text: "Private reply".into(),
                reasoning: "Provider-exposed summary".into(),
                finish: None,
                detail: String::new(),
            },
        );
    let private = envelope(
        "desktop:task:model",
        "read-conversation",
        Some(serde_json::json!({"run_epoch":7})),
    );
    // The broker obtains private text only through exact owner-person custody;
    // public task projections cannot recover this private initial instruction.
    assert!(is_inspect(&private));
    assert_eq!(
        inspection_agent(&snapshot, &private).unwrap().as_deref(),
        Some("scribe")
    );
    let instructions = crate::services::NativeTaskPrivateInstructions {
        instruction: Some("Private full task instruction".into()),
        ..Default::default()
    };
    let conversation = private_conversation(&snapshot, &private, Some(&instructions)).unwrap();
    assert_eq!(conversation["turns"].as_array().unwrap().len(), 4);
    assert_eq!(
        conversation["turns"][3]["presentation"]["reasoning"],
        "Provider-exposed summary"
    );
    let stable_id = conversation["turns"][3]["id"].clone();
    snapshot
        .controls
        .get_mut("task:model")
        .unwrap()
        .presentations
        .get_mut("7:0")
        .unwrap()
        .sequence = 2;
    assert_eq!(
        private_conversation(&snapshot, &private, Some(&instructions)).unwrap()["turns"][3]["id"],
        stable_id
    );
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
        assert!(!public.to_string().contains("Private model reasoning"));
        assert!(!public.to_string().contains("Private reply"));
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
fn canonical_steer_retains_direct_arguments_and_contract() {
    let snapshot = snapshot();
    let actions = actions(
        &snapshot,
        "Workspace",
        &std::collections::BTreeSet::from(["scribe".into()]),
    );
    let find = |name: &str| {
        actions
            .iter()
            .find(|action| action.resource == "desktop:task:model" && action.action == name)
            .unwrap()
    };
    assert_eq!(find("steer").params, find("direct").params);
    assert_eq!(find("steer").contract, find("direct").contract);
    let args = serde_json::json!({"run_epoch":7,"after_revision":2,"text":"Use the smaller proof"});
    let steer = envelope("desktop:task:model", "steer", Some(args.clone()));
    let direct = envelope("desktop:task:model", "direct", Some(args));
    assert_eq!(
        serde_json::to_value(command(&steer).unwrap()).unwrap(),
        serde_json::to_value(command(&direct).unwrap()).unwrap()
    );
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
