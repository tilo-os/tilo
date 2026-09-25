//! An agent opens an application it was not given at spawn, the way a person
//! does from the launcher, and then works in the record it opened: the real
//! Cloud broker desktop surface, the Store desktop owner, worker reconcile
//! mounting the new Notes host worker, and the task's effective scopes.
use super::*;

const NOTES: &str = "so.tilo.notes";

async fn run_step(
    workers: &mut AgentWorkers,
    documents: &mut mpsc::Receiver<PendingHostDocument>,
    network: &mut mpsc::Receiver<NeighbourMsg>,
    dispatch: &VerifiedEnrolledDispatch,
    step: u32,
    peer: u64,
    resource: &str,
    action: &str,
    argument: Option<String>,
) -> qos_actions::InvocationUpdate {
    let mut answer = workers
        .step_service
        .submit(qos_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch: dispatch.clone(),
            step,
            peer,
            resource: resource.into(),
            action: action.into(),
            argument,
        })
        .unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let reply = loop {
        workers.pump().unwrap();
        drain(workers, documents, network);
        if let Ok(reply) = answer.try_recv() {
            break reply;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "step {step} never answered"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };
    match reply {
        Ok(reply) => serde_json::from_str(&reply.update_json).unwrap(),
        Err(error) => qos_actions::InvocationUpdate::Failed {
            invocation: qos_actions::InvocationId::default(),
            error: qos_actions::ActionFailure {
                code: "step_refused".into(),
                message: format!("{error:?}"),
                details: None,
                retryable: false,
            },
        },
    }
}

/// Every string value in a committed record document.
fn document_strings(owner: &RuntimeOwner, record: &str) -> Vec<String> {
    use ui::automerge::{ObjType, ReadDoc, Value};
    fn walk(doc: &ui::automerge::AutoCommit, obj: &ui::automerge::ObjId, out: &mut Vec<String>) {
        let values: Vec<_> = match doc.object_type(obj) {
            Ok(ObjType::Map | ObjType::Table) => doc
                .keys(obj)
                .filter_map(|key| doc.get(obj, key.as_str()).ok().flatten())
                .collect(),
            Ok(ObjType::List) => (0..doc.length(obj))
                .filter_map(|index| doc.get(obj, index).ok().flatten())
                .collect(),
            Ok(ObjType::Text) => {
                out.push(doc.text(obj).unwrap_or_default());
                return;
            }
            Err(_) => return,
        };
        for (value, id) in values {
            match value {
                Value::Object(_) => walk(doc, &id, out),
                Value::Scalar(scalar) => {
                    if let Some(text) = scalar.to_str() {
                        out.push(text.to_owned());
                    }
                }
            }
        }
    }
    let bytes = owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .document(NOTES, record)
        .unwrap();
    let doc = ui::automerge::AutoCommit::load(&bytes).unwrap();
    let mut out = vec![];
    walk(&doc, &ui::automerge::ROOT, &mut out);
    out
}

/// The agent-visible catalogue entry shape of published surfaces.
fn catalogue_json(surfaces: &[PublishedSurface]) -> String {
    serde_json::json!({"version":1,"surfaces": surfaces.iter().map(|s| serde_json::json!({
        "app_id": s.app_id, "record_id": s.route.record_id, "live": s.live,
        "actions": s.actions.iter().map(|a| serde_json::json!({
            "resource": a.resource, "action": a.action, "resource_kind": a.resource_kind,
            "operation_kind": a.operation_kind, "contract": a.contract,
        })).collect::<Vec<_>>(),
    })).collect::<Vec<_>>()})
    .to_string()
}

#[tokio::test(flavor = "current_thread")]
async fn agent_opens_notes_then_creates_a_note_via_its_ops() {
    let (directory, owner) = super::super::tests::selected();
    let (mut workers, mut documents, mut network) = started(directory.path(), owner).await;
    let grants = workers.publications().unwrap().grants;
    let scopes = qos_agent_control_protocol::scopes_from_grants(&grants).unwrap();
    assert!(
        scopes
            .iter()
            .any(|s| s.app_id == qos_agent_control_protocol::DESKTOP_APP_ID
                && s.classes == [qos_agent_control_protocol::OperationClass::AppOpen]),
        "the spawn offer includes the desktop's open-application action"
    );
    assert!(
        !scopes.iter().any(|s| s.app_id == NOTES),
        "Notes is not open at spawn"
    );
    let task =
        enrolled_task_granted(&mut workers, &mut documents, &mut network, true, grants).await;
    let broker = workers.app_broker().unwrap();
    let dispatch = broker
        .enrolled_task_dispatch(&task.channel, task.invocation)
        .unwrap();

    // Discover "Open Notes" in the same catalogue the agent reads.
    let open = format!(
        "{}{NOTES}",
        qos_agent_control_protocol::OPEN_APP_RESOURCE_PREFIX
    );
    let desktop = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .find(|s| {
            s.live
                && s.app_id == qos_agent_control_protocol::DESKTOP_APP_ID
                && s.actions
                    .iter()
                    .any(|a| a.resource == open && a.action == "open")
        })
        .expect("desktop publishes Open Notes");
    let catalogue = qos_action_broker::agent_control::scoped::filter_scoped_catalogue(
        &catalogue_json(&[desktop.clone()]),
        &workers.publications().unwrap().grants,
    )
    .unwrap();
    assert!(
        catalogue.contains(&open),
        "Open Notes is in the agent's catalogue"
    );

    let before: Vec<String> = workers
        .owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .rows()
        .into_iter()
        .filter(|row| row.app_id == NOTES)
        .map(|row| row.record_id)
        .collect();
    let opened = run_step(
        &mut workers,
        &mut documents,
        &mut network,
        &dispatch,
        0,
        desktop.peer_id,
        &open,
        "open",
        None,
    )
    .await;
    assert!(
        matches!(opened, qos_actions::InvocationUpdate::Succeeded { .. }),
        "open failed: {opened:?}"
    );
    let record = workers
        .owner
        .store()
        .unwrap()
        .peer()
        .unwrap()
        .rows()
        .into_iter()
        .find(|row| row.app_id == NOTES && !before.contains(&row.record_id))
        .expect("a new Notes record")
        .record_id;
    assert_eq!(
        workers
            .owner
            .store()
            .unwrap()
            .task_opened_records(&dispatch.envelope().task_id),
        vec![qos_agent_control_protocol::OpenedRecord {
            app_id: NOTES.into(),
            record_id: record.clone(),
        }]
    );

    // The host mounts a Notes worker for the new record; its operations are
    // published like any app's.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    let (notes_peer, notebook) = loop {
        workers.pump().unwrap();
        drain(&mut workers, &mut documents, &mut network);
        if let Some(found) = workers
            .broker
            .published_action_surfaces()
            .into_iter()
            .find_map(|s| {
                (s.live && s.app_id == NOTES && s.route.record_id == record)
                    .then(|| {
                        s.actions
                            .iter()
                            .find(|a| a.action == "new-page")
                            .map(|a| (s.peer_id, a.resource.clone()))
                    })
                    .flatten()
            })
        {
            break found;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "Notes worker never published"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    };

    // The record the task opened is inside its effective scopes; a Notes
    // record it did not open is not.
    let store = workers.owner.store().unwrap();
    store
        .authorize_agent_task_action(&dispatch, NOTES, &record, &notebook, "new-page")
        .unwrap();
    assert!(
        store
            .authorize_agent_task_action(
                &dispatch,
                NOTES,
                "tab:00000000000000ff",
                &notebook,
                "new-page"
            )
            .is_err()
    );
    drop(store);

    let created = run_step(
        &mut workers,
        &mut documents,
        &mut network,
        &dispatch,
        1,
        notes_peer,
        &notebook,
        "new-page",
        Some("Groceries".into()),
    )
    .await;
    assert!(
        matches!(created, qos_actions::InvocationUpdate::Succeeded { .. }),
        "new-page failed: {created:?}"
    );
    let strings = document_strings(&workers.owner, &record);
    assert!(
        strings.iter().any(|text| text == "Groceries"),
        "the note is in the record the agent opened: {strings:?}"
    );
}
