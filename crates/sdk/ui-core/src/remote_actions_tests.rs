use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

use super::*;

fn row(id: &str) -> CatalogueRow {
    CatalogueRow {
        provider: "7".into(),
        app_id: "so.tilo.example".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: id.into(),
            kind: "example.item".into(),
            label: id.into(),
            presentation: None,
        },
        action: None,
        has_children: false,
        visibility: None,
    }
}

fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: "7".into(),
        address: ActionAddress::new("example.item:1", action),
    }
}

/// Two catalogue pages; `done` completes at once, `slow` runs until
/// observed, `forbidden` is refused.
#[derive(Default)]
struct FakeHost {
    seen: Mutex<Vec<Caller>>,
}

impl ActionHost for FakeHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        self.seen.lock().unwrap().push(caller.clone());
        let second = !query.cursor.is_empty();
        Pending::ready(Ok(CataloguePage {
            revision: "r1".into(),
            rows: vec![row(if second { "b" } else { "a" })],
            next_cursor: if second { Vec::new() } else { vec![1] },
            exhausted: second,
            ..CataloguePage::default()
        }))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let status = match request.target.address.action.as_str() {
            "done" => ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::json!({"ok": true})),
                    resources: Vec::new(),
                },
            },
            "slow" => ReceiptStatus::Accepted,
            _ => return Pending::ready(Err(ActionHostError::NotPermitted)),
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        Some(InvocationState::new(
            *id,
            InvocationUpdate::Succeeded {
                invocation: *id,
                output: ActionOutput::default(),
            },
        ))
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
        Ok(())
    }
}

fn client() -> (Arc<FakeHost>, RemoteActions) {
    let host = Arc::new(FakeHost::default());
    let actions = RemoteActions::with_host(
        Arc::clone(&host) as Arc<dyn ActionHost + Send + Sync>,
        Caller::local_app("so.tilo.example").in_room("room"),
    );
    (host, actions)
}

#[test]
fn a_catalogue_read_gathers_every_page_as_the_app() {
    let (host, actions) = client();
    let answer = actions.catalogue(CatalogueQuery::default());
    let RemoteCall::Done(page) = answer.get() else {
        panic!("the catalogue is answered: {:?}", answer.get());
    };
    let ids: Vec<_> = page
        .rows
        .iter()
        .map(|row| row.resource.id.clone())
        .collect();
    assert_eq!(ids, ["a", "b"]);
    let seen = host.seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "one request per page");
    assert!(
        seen.iter().all(|caller| *caller.principal()
            == tilo_actions::remote::CallerPrincipal::App("so.tilo.example".into())),
        "the app asks as itself"
    );
    assert!(!actions.is_busy());
}

#[test]
fn an_invocation_reports_its_outcome_or_its_refusal() {
    let (_host, actions) = client();
    let done = actions.invoke(target("done"), InvokeArgument::None, None);
    assert!(matches!(
        done.get(),
        RemoteCall::Done(InvocationUpdate::Succeeded { .. })
    ));
    let refused = actions.invoke(target("forbidden"), InvokeArgument::None, None);
    assert_eq!(
        refused.get(),
        RemoteCall::Refused(ActionHostError::NotPermitted)
    );
    assert!(refused.get().is_settled());
}

#[test]
fn a_retry_safe_invocation_keeps_the_callers_durable_identity() {
    let invocation = InvocationId(uuid::Uuid::from_bytes([0x4a; 16]));
    let mut actions = RemoteActions::session();
    let _answer = actions.invoke_with_id(invocation, target("done"), InvokeArgument::None, None);
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(request),
        },
    ] = requests.as_slice()
    else {
        panic!("one invocation request");
    };
    assert_eq!(request.invocation, invocation);
}

#[test]
fn a_running_invocation_is_followed_until_it_ends() {
    let clock = crate::background::Pumped::new();
    let _entered = clock.background().enter();
    let (_host, actions) = client();
    let slow = actions.invoke(target("slow"), InvokeArgument::None, None);
    assert!(matches!(
        slow.get(),
        RemoteCall::Done(InvocationUpdate::Accepted { .. })
    ));
    assert!(!slow.get().is_settled());
    actions.poll();
    assert!(
        !slow.get().is_settled(),
        "not observed again before the interval"
    );
    clock.advance(OBSERVE_INTERVAL);
    actions.poll();
    assert!(matches!(
        slow.get(),
        RemoteCall::Done(InvocationUpdate::Succeeded { .. })
    ));
    assert!(!actions.is_busy());
}

#[test]
fn the_session_transport_carries_requests_out_and_answers_back() {
    let mut actions = RemoteActions::session();
    let answer = actions.catalogue(CatalogueQuery::default());
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("one catalogue request: {requests:?}");
    };
    assert!(actions.take_collab_requests().is_empty(), "sent once");
    // An answer for someone else's request is not ours.
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: correlation + 100,
            result: Err(ActionHostError::NotPermitted),
        },
    });
    assert!(answer.get().is_pending());
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: *correlation,
            result: Err(ActionHostError::NotPermitted),
        },
    });
    assert_eq!(
        answer.get(),
        RemoteCall::Refused(ActionHostError::NotPermitted)
    );
}

#[test]
fn cancellation_acknowledgement_never_pretends_the_invocation_stopped() {
    let invocation = InvocationId(uuid::Uuid::from_bytes([0x6c; 16]));
    let mut actions = RemoteActions::session();
    let running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
    let _invoke = actions.take_collab_requests();

    let cancellation = actions.cancel(invocation).expect("tracked invocation");
    assert_eq!(cancellation.get(), CancellationCall::Pending);
    assert_eq!(
        actions.cancel(invocation).unwrap().get(),
        CancellationCall::Pending
    );
    let requests = actions.take_collab_requests();
    assert!(matches!(
        requests.as_slice(),
        [CollabRequest::RemoteAction {
            message: RemoteActionMessage::Cancel { invocation: id },
        }] if *id == invocation
    ));
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CancelResponse {
            invocation,
            result: Ok(()),
        },
    });
    assert_eq!(cancellation.get(), CancellationCall::Acknowledged);
    assert_eq!(
        actions.cancel(invocation).unwrap().get(),
        CancellationCall::Acknowledged
    );
    assert!(actions.take_collab_requests().is_empty());
    assert!(
        !running.get().is_settled(),
        "a cancellation receipt is not a stopped invocation"
    );

    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::Update {
            invocation,
            state: Some(InvocationState::new(
                invocation,
                InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("Stopped by the person".into()),
                },
            )),
        },
    });
    assert!(matches!(
        running.get(),
        RemoteCall::Done(InvocationUpdate::Cancelled { .. })
    ));
    assert!(actions.cancel(invocation).is_none());
    assert!(
        actions
            .cancel(InvocationId(uuid::Uuid::from_bytes([0xee; 16])))
            .is_none()
    );
}

#[test]
fn refused_cancellation_leaves_the_invocation_running_and_actionable() {
    let invocation = InvocationId(uuid::Uuid::from_bytes([0x7c; 16]));
    let mut actions = RemoteActions::session();
    let running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
    let _invoke = actions.take_collab_requests();
    let cancellation = actions.cancel(invocation).expect("tracked invocation");
    let _cancel = actions.take_collab_requests();
    let error = ActionHostError::NotCancellable {
        reason: "the provider is still attaching".into(),
    };
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CancelResponse {
            invocation,
            result: Err(error.clone()),
        },
    });
    assert_eq!(cancellation.get(), CancellationCall::Refused(error));
    assert!(!running.get().is_settled());
    assert!(actions.is_busy());

    let retried = actions.cancel(invocation).expect("running retry");
    assert_eq!(retried.get(), CancellationCall::Pending);
    let requests = actions.take_collab_requests();
    assert!(matches!(
        requests.as_slice(),
        [CollabRequest::RemoteAction {
            message: RemoteActionMessage::Cancel { invocation: id },
        }] if *id == invocation
    ));
    assert_eq!(
        actions.cancel(invocation).unwrap().get(),
        CancellationCall::Pending
    );
    assert!(actions.take_collab_requests().is_empty());
}

#[test]
fn an_entered_host_serves_the_current_client_until_the_guard_drops() {
    let host = Arc::new(FakeHost::default());
    {
        let _served = RemoteActions::enter(
            Arc::clone(&host) as Arc<dyn ActionHost + Send + Sync>,
            Caller::local_app("so.tilo.example").in_room("room"),
        );
        let answer = RemoteActions::current().catalogue(CatalogueQuery::default());
        assert!(matches!(answer.get(), RemoteCall::Done(_)));
    }
    let mut after = RemoteActions::current();
    let _pending = after.catalogue(CatalogueQuery::default());
    assert_eq!(
        after.take_collab_requests().len(),
        1,
        "without a guard the client talks through the session"
    );
}
