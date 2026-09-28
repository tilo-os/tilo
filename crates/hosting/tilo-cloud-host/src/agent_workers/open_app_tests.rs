//! An agent opens an application it was not given at spawn, the way a person
//! does from the launcher, and then works in the record it opened: the real
//! Cloud broker desktop surface, the Store desktop owner, worker reconcile
//! mounting the new Notes host worker, and the task's effective scopes.
use super::*;

const NOTES: &str = "so.tilo.notes";

async fn run_step(
    mut pump: Pump<'_>,
    request: tilo_action_broker::enrolled_dispatch::EnrolledStepRequest,
) -> tilo_actions::InvocationUpdate {
    let step = request.step;
    let mut answer = pump.workers.step_service.submit(request).unwrap();
    let deadline = tokio::time::Instant::now() + crate::agent_workers::tests::HARNESS_WAIT;
    let reply = loop {
        pump.turn();
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
        Err(error) => tilo_actions::InvocationUpdate::Failed {
            invocation: tilo_actions::InvocationId::default(),
            error: tilo_actions::ActionFailure {
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
                    if let Some(text) = scalar.as_str() {
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
    let grants = offered_grants(&mut workers, &mut documents, &mut network).await;
    let scopes = tilo_agent_control_protocol::scopes_from_grants(&grants).unwrap();
    assert!(
        scopes
            .iter()
            .any(|s| s.app_id == tilo_agent_control_protocol::DESKTOP_APP_ID
                && s.classes == [tilo_agent_control_protocol::OperationClass::AppOpen]),
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
        tilo_agent_control_protocol::OPEN_APP_RESOURCE_PREFIX
    );
    let desktop = workers
        .broker
        .published_action_surfaces()
        .into_iter()
        .find(|s| {
            s.live
                && s.app_id == tilo_agent_control_protocol::DESKTOP_APP_ID
                && s.actions
                    .iter()
                    .any(|a| a.resource == open && a.action == "open")
        })
        .expect("desktop publishes Open Notes");
    let offered = offered_grants(&mut workers, &mut documents, &mut network).await;
    let catalogue = tilo_action_broker::agent_control::scoped::filter_scoped_catalogue(
        &catalogue_json(std::slice::from_ref(&desktop)),
        &offered,
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
        Pump {
            workers: &mut workers,
            documents: &mut documents,
            network: &mut network,
        },
        tilo_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch: dispatch.clone(),
            step: 0,
            peer: desktop.peer_id,
            resource: open.clone(),
            action: "open".into(),
            argument: None,
        },
    )
    .await;
    assert!(
        matches!(opened, tilo_actions::InvocationUpdate::Succeeded { .. }),
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
        vec![tilo_agent_control_protocol::OpenedRecord {
            app_id: NOTES.into(),
            record_id: record.clone(),
        }]
    );

    // The host mounts a Notes worker for the new record; its operations are
    // published like any app's.
    let deadline = tokio::time::Instant::now() + crate::agent_workers::tests::HARNESS_WAIT;
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
        Pump {
            workers: &mut workers,
            documents: &mut documents,
            network: &mut network,
        },
        tilo_action_broker::enrolled_dispatch::EnrolledStepRequest {
            dispatch: dispatch.clone(),
            step: 1,
            peer: notes_peer,
            resource: notebook.clone(),
            action: "new-page".into(),
            argument: Some("Groceries".into()),
        },
    )
    .await;
    assert!(
        matches!(created, tilo_actions::InvocationUpdate::Succeeded { .. }),
        "new-page failed: {created:?}"
    );
    let strings = document_strings(&workers.owner, &record);
    assert!(
        strings.iter().any(|text| text == "Groceries"),
        "the note is in the record the agent opened: {strings:?}"
    );
}
