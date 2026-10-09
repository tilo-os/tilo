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

/// Two catalogue pages; `done` completes at once, `slow` is still running
/// the first time it is observed and done the next, `forbidden` is refused.
/// It cannot signal changes, so it answers a waiting observe at once, as a
/// host from before waits does.
#[derive(Default)]
struct FakeHost {
    seen: Mutex<Vec<Caller>>,
    observed: std::sync::atomic::AtomicUsize,
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
        let first = self
            .observed
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 0;
        Some(InvocationState::new(
            *id,
            if first {
                InvocationUpdate::Accepted { invocation: *id }
            } else {
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: ActionOutput::default(),
                }
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
    assert!(
        page.exhausted,
        "the aggregate retains authoritative completion"
    );
    assert!(page.next_cursor.is_empty());
    assert!(!page.stale_cursor);
    assert!(!page.budget_exhausted);
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
fn incomplete_or_contradictory_catalogue_pages_never_publish_partial_offers() {
    for (exhausted, next_cursor) in [(false, Vec::new()), (true, vec![1])] {
        let mut actions = RemoteActions::session();
        let answer = actions.catalogue(CatalogueQuery::default());
        let requests = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest { correlation, .. },
            },
        ] = requests.as_slice()
        else {
            panic!("first catalogue request");
        };
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CatalogueResponse {
                correlation: *correlation,
                result: Ok(CataloguePage {
                    revision: "r1".into(),
                    rows: vec![row("partial-offer")],
                    exhausted,
                    next_cursor,
                    ..CataloguePage::default()
                }),
            },
        });
        assert!(matches!(
            answer.get(),
            RemoteCall::Refused(ActionHostError::Unavailable { reason })
                if reason.contains("inconsistent completion metadata")
        ));
        assert!(!actions.is_busy());
        assert!(actions.take_collab_requests().is_empty());
    }
}

#[test]
fn a_single_page_catalogue_read_leaves_hierarchy_budget_with_its_caller() {
    let (host, actions) = client();
    let answer = actions.catalogue_page(CatalogueQuery::default());
    let RemoteCall::Done(page) = answer.get() else {
        panic!("the first page is answered: {:?}", answer.get());
    };
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.resource.id.as_str())
            .collect::<Vec<_>>(),
        ["a"]
    );
    assert_eq!(page.next_cursor, [1]);
    assert_eq!(host.seen.lock().unwrap().len(), 1);
    assert!(!actions.is_busy());
}

#[test]
fn a_provider_only_catalogue_request_keeps_the_wire_zero() {
    let mut actions = RemoteActions::session();
    let _answer = actions.catalogue(CatalogueQuery {
        max_results: 0,
        ..CatalogueQuery::default()
    });
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { query, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("one provider-only catalogue request")
    };
    assert_eq!(query.max_results, 0);
    assert!(query.provider.is_none());
}

#[test]
fn a_repeated_catalogue_cursor_is_refused_without_another_request() {
    let mut actions = RemoteActions::session();
    let answer = actions.catalogue(CatalogueQuery::default());
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("first catalogue request")
    };
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: *correlation,
            result: Ok(CataloguePage {
                revision: "r1".into(),
                rows: vec![row("a")],
                next_cursor: vec![1],
                ..CataloguePage::default()
            }),
        },
    });
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("second catalogue request")
    };
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: *correlation,
            result: Ok(CataloguePage {
                revision: "r1".into(),
                rows: vec![row("b")],
                next_cursor: vec![1],
                ..CataloguePage::default()
            }),
        },
    });
    assert!(matches!(
        answer.get(),
        RemoteCall::Refused(ActionHostError::Unavailable { reason })
            if reason.contains("repeated a cursor")
    ));
    assert!(actions.take_collab_requests().is_empty());
}

#[test]
fn a_catalogue_walk_has_a_total_page_bound() {
    let mut actions = RemoteActions::session();
    let answer = actions.catalogue(CatalogueQuery::default());
    for page in 0..MAX_CATALOGUE_PAGES {
        let requests = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest { correlation, .. },
            },
        ] = requests.as_slice()
        else {
            panic!("catalogue request for page {page}")
        };
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CatalogueResponse {
                correlation: *correlation,
                result: Ok(CataloguePage {
                    revision: "r1".into(),
                    rows: vec![row(&format!("row-{page}"))],
                    next_cursor: vec![u8::try_from(page + 1).unwrap()],
                    ..CataloguePage::default()
                }),
            },
        });
    }
    assert!(matches!(
        answer.get(),
        RemoteCall::Refused(ActionHostError::Unavailable { reason })
            if reason.contains("page budget")
    ));
    assert!(actions.take_collab_requests().is_empty());
}

#[test]
fn stale_catalogue_restarts_share_the_global_page_bound() {
    let mut actions = RemoteActions::session();
    let answer = actions.catalogue(CatalogueQuery::default());
    for page in 0..MAX_CATALOGUE_PAGES {
        let requests = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::CatalogueRequest { correlation, .. },
            },
        ] = requests.as_slice()
        else {
            panic!("catalogue request for stale page {page}")
        };
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CatalogueResponse {
                correlation: *correlation,
                result: Ok(CataloguePage {
                    revision: format!("r{page}"),
                    stale_cursor: true,
                    ..CataloguePage::default()
                }),
            },
        });
    }
    assert!(matches!(
        answer.get(),
        RemoteCall::Refused(ActionHostError::Unavailable { reason })
            if reason.contains("page budget")
    ));
    assert!(actions.take_collab_requests().is_empty());
}

#[test]
fn an_unannounced_catalogue_revision_change_is_refused() {
    let mut actions = RemoteActions::session();
    let answer = actions.catalogue(CatalogueQuery::default());
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("first catalogue request")
    };
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: *correlation,
            result: Ok(CataloguePage {
                revision: "r1".into(),
                next_cursor: vec![1],
                ..CataloguePage::default()
            }),
        },
    });
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("second catalogue request")
    };
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: *correlation,
            result: Ok(CataloguePage {
                revision: "r2".into(),
                exhausted: true,
                ..CataloguePage::default()
            }),
        },
    });
    assert!(matches!(
        answer.get(),
        RemoteCall::Refused(ActionHostError::Unavailable { reason })
            if reason.contains("changed revision")
    ));
}

#[test]
fn an_incomplete_catalogue_budget_response_is_never_reported_as_complete() {
    let mut actions = RemoteActions::session();
    let answer = actions.catalogue(CatalogueQuery::default());
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
        },
    ] = requests.as_slice()
    else {
        panic!("catalogue request")
    };
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: RemoteActionMessage::CatalogueResponse {
            correlation: *correlation,
            result: Ok(CataloguePage {
                revision: "r1".into(),
                rows: vec![row("partial")],
                exhausted: true,
                budget_exhausted: true,
                ..CataloguePage::default()
            }),
        },
    });
    assert!(matches!(
        answer.get(),
        RemoteCall::Refused(ActionHostError::Unavailable { reason })
            if reason.contains("work budget")
    ));
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

/// Against a host that answers a waiting observe at once with nothing new
/// (one from before waits), the client falls back to observing on its
/// interval.
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

/// The one remote-action request in `requests`.
fn only_message(requests: Vec<CollabRequest>) -> RemoteActionMessage {
    let [CollabRequest::RemoteAction { message }] = <[CollabRequest; 1]>::try_from(requests)
        .unwrap_or_else(|requests| panic!("one request: {requests:?}"))
    else {
        panic!("a remote-action request");
    };
    message
}

fn accepted(invocation: InvocationId) -> RemoteActionMessage {
    RemoteActionMessage::Receipt(InvocationReceipt {
        invocation,
        status: ReceiptStatus::Accepted,
    })
}

fn update(invocation: InvocationId, update: InvocationUpdate) -> CollabEvent {
    CollabEvent::RemoteAction {
        message: RemoteActionMessage::Update {
            invocation,
            state: Some(InvocationState::new(invocation, update)),
        },
    }
}

fn progress(invocation: InvocationId, sequence: u64) -> InvocationUpdate {
    InvocationUpdate::Progress {
        invocation,
        sequence,
        phase: "renaming".into(),
        completed: None,
        total: None,
        message: None,
    }
}

/// A running invocation is followed by one waiting observe at a time, sent
/// the moment the last answer lands — never by a poll on a timer.
#[test]
fn a_running_invocation_sends_one_waiting_observe_not_a_poll() {
    let clock = crate::background::Pumped::new();
    let _entered = clock.background().enter();
    let invocation = InvocationId(uuid::Uuid::from_bytes([0x60; 16]));
    let mut actions = RemoteActions::session();
    let running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
    let requests = actions.take_collab_requests();
    assert!(
        matches!(
            requests.as_slice(),
            [CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(InvokeRequest {
                    wait_ms: Some(INVOKE_WAIT_MS),
                    ..
                }),
            }]
        ),
        "the invocation asks for its outcome in the same round trip: {requests:?}"
    );

    // Still running when the receipt was due: follow it at once.
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: accepted(invocation),
    });
    assert_eq!(
        only_message(actions.take_collab_requests()),
        RemoteActionMessage::Observe {
            invocation,
            wait_ms: Some(OBSERVE_WAIT_MS),
            seen: Some(UpdateMark::Accepted),
        }
    );
    for _ in 0..5 {
        clock.advance(OBSERVE_INTERVAL);
        assert!(
            actions.take_collab_requests().is_empty(),
            "no poll while a waiting observe is out"
        );
    }

    actions.handle_collab_event(&update(invocation, progress(invocation, 1)));
    assert!(matches!(
        running.get(),
        RemoteCall::Done(InvocationUpdate::Progress { sequence: 1, .. })
    ));
    assert_eq!(
        only_message(actions.take_collab_requests()),
        RemoteActionMessage::Observe {
            invocation,
            wait_ms: Some(OBSERVE_WAIT_MS),
            seen: Some(UpdateMark::Progress { sequence: 1 }),
        }
    );

    actions.handle_collab_event(&update(
        invocation,
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput::default(),
        },
    ));
    assert!(running.get().is_settled());
    assert!(actions.take_collab_requests().is_empty());
    assert!(!actions.is_busy());
}

/// A wait that ran out with nothing new is waited again, not mistaken for
/// a host that does not wait.
#[test]
fn a_wait_that_ran_out_is_waited_again() {
    let clock = crate::background::Pumped::new();
    let _entered = clock.background().enter();
    let invocation = InvocationId(uuid::Uuid::from_bytes([0x61; 16]));
    let mut actions = RemoteActions::session();
    let _running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
    let _invoke = actions.take_collab_requests();
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: accepted(invocation),
    });
    let _observe = actions.take_collab_requests();
    clock.advance(Duration::from_millis(u64::from(OBSERVE_WAIT_MS)));
    actions.handle_collab_event(&update(
        invocation,
        InvocationUpdate::Accepted { invocation },
    ));
    assert!(matches!(
        actions.take_collab_requests().as_slice(),
        [CollabRequest::RemoteAction {
            message: RemoteActionMessage::Observe {
                wait_ms: Some(OBSERVE_WAIT_MS),
                seen: Some(UpdateMark::Accepted),
                ..
            },
        }]
    ));
}

/// A host from before waits answers the waiting observe at once with
/// nothing new; the client then observes it on its interval, as before.
#[test]
fn an_old_host_is_observed_on_the_interval() {
    let clock = crate::background::Pumped::new();
    let _entered = clock.background().enter();
    let invocation = InvocationId(uuid::Uuid::from_bytes([0x62; 16]));
    let mut actions = RemoteActions::session();
    let running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
    let _invoke = actions.take_collab_requests();
    actions.handle_collab_event(&CollabEvent::RemoteAction {
        message: accepted(invocation),
    });
    let _observe = actions.take_collab_requests();
    actions.handle_collab_event(&update(
        invocation,
        InvocationUpdate::Accepted { invocation },
    ));
    assert!(
        actions.take_collab_requests().is_empty(),
        "not asked again before the interval"
    );
    clock.advance(OBSERVE_INTERVAL);
    assert_eq!(
        only_message(actions.take_collab_requests()),
        RemoteActionMessage::observe(invocation)
    );
    actions.handle_collab_event(&update(
        invocation,
        InvocationUpdate::Succeeded {
            invocation,
            output: ActionOutput::default(),
        },
    ));
    assert!(running.get().is_settled());
}
