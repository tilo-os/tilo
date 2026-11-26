use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, CatalogueResource,
    CatalogueRow, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};

use super::*;
use std::sync::atomic::Ordering;

fn row(id: &str) -> CatalogueRow {
    CatalogueRow {
        offered: None,
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
        surface: None,
    }
}

fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: "7".into(),
        address: ActionAddress::new("example.item:1", action),
    }
}

/// Two catalogue pages; `done` completes at once, `slow` is still running for its first
/// [`RUNNING_LOOKS`] observes and done after, `forbidden` is refused. It cannot signal changes, so it
/// answers a waiting observe at once, as a provider from before waits does.
#[derive(Default)]
struct FakeHost {
    seen: Mutex<Vec<Caller>>,
    observed: std::sync::atomic::AtomicUsize,
}

impl ActionProvider for FakeHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
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
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        let status = match request.target.address.action.as_str() {
            "done" => ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::json!({"ok": true})),
                    resources: Vec::new(),
                },
            },
            "slow" => ReceiptStatus::Accepted,
            _ => return Pending::ready(Err(ActionCallError::NotPermitted)),
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        let running = self.observed.fetch_add(1, Ordering::SeqCst) < RUNNING_LOOKS;
        Some(InvocationState::new(
            *id,
            if running {
                InvocationUpdate::Accepted { invocation: *id }
            } else {
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: ActionOutput::default(),
                }
            },
        ))
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Ok(()))
    }
}

/// How often `slow` is seen still running: once by the provider's own look when the invoke asks to wait,
/// once by the client's first waiting observe.
const RUNNING_LOOKS: usize = 2;

fn catalogue_query(actions: &mut RemoteActions, what: &str) -> CatalogueQuery {
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { query, .. },
            ..
        },
    ] = requests.as_slice()
    else {
        panic!("{what}: {requests:?}")
    };
    query.clone()
}

fn catalogue_correlation(actions: &mut RemoteActions, what: &str) -> u64 {
    let requests = actions.take_collab_requests();
    let [
        CollabRequest::RemoteAction {
            message: RemoteActionMessage::CatalogueRequest { correlation, .. },
            ..
        },
    ] = requests.as_slice()
    else {
        panic!("{what}: {requests:?}")
    };
    *correlation
}

tilo_test_env::scenario! {
    fn a_catalogue_page_is_bounded_and_refused_when_it_cannot_be;

    phase a_provider_only_catalogue_request_keeps_the_wire_zero;
    {
        let mut actions = RemoteActions::session();
        let _answer = actions.catalogue_page(CatalogueQuery {
            max_results: 0,
            ..CatalogueQuery::default()
        });
        let query = catalogue_query(&mut actions, "one provider-only catalogue request");
        assert_eq!(query.max_results, 0);
        assert!(query.provider.is_none());
    }

    phase an_unanswered_catalogue_read_is_refused_after_its_wait;
    // A catalogue request nothing answers (no provider in reach of the connection) is refused as unavailable
    // once its wait is up, so the app can say so instead of waiting forever.
    {
        let clock = tilo_ui_base::background::Pumped::new();
        let _entered = clock.background().enter();
        let mut actions = RemoteActions::session();
        let answer = actions.catalogue_page(CatalogueQuery::default());
        assert_eq!(
            actions.take_collab_requests().len(),
            1,
            "the request is sent"
        );
        clock.advance(CATALOGUE_WAIT - Duration::from_millis(1));
        assert!(actions.take_collab_requests().is_empty());
        assert!(answer.get().is_pending(), "still within its wait");
        clock.advance(Duration::from_millis(1));
        actions.poll();
        assert_eq!(
            answer.get(),
            Arrival::Failed(ActionCallError::Unavailable {
                reason: CATALOGUE_UNANSWERED.into()
            })
        );
        assert!(!actions.is_busy(), "nothing is left waiting");
    }
}

tilo_test_env::scenario! {
    fn an_invocation_is_followed_to_its_end_and_cancelled_honestly;

    phase a_retry_safe_invocation_keeps_the_callers_durable_identity;
    {
        let invocation = InvocationId(uuid::Uuid::from_bytes([0x4a; 16]));
        let mut actions = RemoteActions::session();
        let _answer = actions.invoke_with_id(invocation, target("done"), InvokeArgument::None, None);
        let requests = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                ..
            },
        ] = requests.as_slice()
        else {
            panic!("one invocation request");
        };
        assert_eq!(request.invocation, invocation);
    }

    phase cancellation_acknowledgement_never_pretends_the_invocation_stopped;
    {
        let invocation = InvocationId(uuid::Uuid::from_bytes([0x6c; 16]));
        let mut actions = RemoteActions::session();
        let running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
        let _invoke = actions.take_collab_requests();

        let cancellation = actions.cancel(invocation).expect("tracked invocation");
        assert!(cancellation.get().is_pending());
        assert!(actions.cancel(invocation).unwrap().get().is_pending());
        let requests = actions.take_collab_requests();
        assert!(matches!(
            requests.as_slice(),
            [CollabRequest::RemoteAction {
                message: RemoteActionMessage::Cancel { invocation: id },
                ..
            }] if *id == invocation
        ));
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CancelResponse {
                invocation,
                result: Ok(()),
            },
        });
        assert_eq!(cancellation.get(), Arrival::Ready(()));
        assert_eq!(
            actions.cancel(invocation).unwrap().get(),
            Arrival::Ready(())
        );
        assert!(actions.take_collab_requests().is_empty());
        assert!(
            !running.get().is_finished(),
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
                correlation: None,
            },
        });
        assert!(matches!(
            running.get(),
            Arrival::Ready(InvocationUpdate::Cancelled { .. })
        ));
        assert!(actions.cancel(invocation).is_none());
        assert!(
            actions
                .cancel(InvocationId(uuid::Uuid::from_bytes([0xee; 16])))
                .is_none()
        );
    }

    phase refused_cancellation_leaves_the_invocation_running_and_actionable;
    {
        let invocation = InvocationId(uuid::Uuid::from_bytes([0x7c; 16]));
        let mut actions = RemoteActions::session();
        let running = actions.invoke_with_id(invocation, target("slow"), InvokeArgument::None, None);
        let _invoke = actions.take_collab_requests();
        let cancellation = actions.cancel(invocation).expect("tracked invocation");
        let _cancel = actions.take_collab_requests();
        let error = ActionCallError::NotCancellable {
            reason: "the provider is still attaching".into(),
        };
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CancelResponse {
                invocation,
                result: Err(error.clone()),
            },
        });
        assert_eq!(cancellation.get(), Arrival::Failed(error));
        assert!(!running.get().is_finished());
        assert!(actions.is_busy());

        let retried = actions.cancel(invocation).expect("running retry");
        assert!(retried.get().is_pending());
        let requests = actions.take_collab_requests();
        assert!(matches!(
            requests.as_slice(),
            [CollabRequest::RemoteAction {
                message: RemoteActionMessage::Cancel { invocation: id },
                ..
            }] if *id == invocation
        ));
        assert!(actions.cancel(invocation).unwrap().get().is_pending());
        assert!(actions.take_collab_requests().is_empty());
    }

    phase a_running_invocation_sends_one_waiting_observe_not_a_poll;
    // A running invocation is followed by one waiting observe at a time, sent the moment the last answer
    // lands — never by a poll on a timer.
    {
        let clock = tilo_ui_base::background::Pumped::new();
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
                        appearance: None,
                        ..
                    }),
                    ..
                }]
            ),
            "the invocation asks for its outcome in the same round trip: {requests:?}"
        );

        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: accepted(invocation),
        });
        assert_eq!(
            only_message(actions.take_collab_requests()),
            RemoteActionMessage::Observe {
                correlation: 0,
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
            Arrival::Ready(InvocationUpdate::Progress { sequence: 1, .. })
        ));
        assert_eq!(
            only_message(actions.take_collab_requests()),
            RemoteActionMessage::Observe {
                correlation: 0,
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
        assert!(running.get().is_finished());
        assert!(actions.take_collab_requests().is_empty());
        assert!(!actions.is_busy());
    }

    phase a_wait_that_ran_out_is_waited_again;
    {
        let clock = tilo_ui_base::background::Pumped::new();
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
                ..
            }]
        ));
    }

    phase an_old_host_is_observed_on_the_interval;
    // A provider from before waits answers the waiting observe at once with nothing new; the client then
    // observes it on its interval, as before.
    {
        let clock = tilo_ui_base::background::Pumped::new();
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
        assert!(running.get().is_finished());
    }

    phase a_followed_result_shows_each_item_and_stops_when_dropped;
    // A followed live result shows each new item the provider sends, keeps a waiting observe out for the next,
    // and is cancelled once dropped.
    {
        let mut actions = RemoteActions::session();
        let followed = actions.follow(target("live"), InvokeArgument::None);
        let invocation = followed.invocation();
        let requests = actions.take_collab_requests();
        let [
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                ..
            },
        ] = requests.as_slice()
        else {
            panic!("one invocation: {requests:?}");
        };
        assert_eq!(request.wait_ms, None, "the receipt is the admission");
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::Receipt(InvocationReceipt {
                invocation,
                status: ReceiptStatus::Accepted,
            }),
        });
        for (sequence, count) in [(1, 0), (2, 3)] {
            let requests = actions.take_collab_requests();
            assert!(
                matches!(
                    requests.as_slice(),
                    [CollabRequest::RemoteAction {
                        message: RemoteActionMessage::Observe {
                            wait_ms: Some(_),
                            seen: Some(_),
                            ..
                        },
                        ..
                    }]
                ),
                "a waiting observe follows each update: {requests:?}"
            );
            let item = InvocationUpdate::Item {
                invocation,
                sequence,
                value: serde_json::json!({ "count": count }),
            };
            actions.handle_collab_event(&CollabEvent::RemoteAction {
                message: RemoteActionMessage::Update {
                    invocation,
                    state: Some(InvocationState::new(invocation, item.clone())),
                    correlation: None,
                },
            });
            assert_eq!(followed.call().get(), Arrival::Ready(item));
        }
        let _observe = actions.take_collab_requests();
        drop(followed);
        let requests = actions.take_collab_requests();
        assert!(
            matches!(
                requests.as_slice(),
                [CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Cancel { invocation: id },
                    ..
                }] if *id == invocation
            ),
            "dropping it cancels the invocation: {requests:?}"
        );
    }
}

tilo_test_env::scenario! {
    fn a_client_talks_through_the_session_or_the_host_entered_for_it;

    phase the_session_transport_carries_requests_out_and_answers_back;
    {
        let mut actions = RemoteActions::session();
        let answer = actions.catalogue_page(CatalogueQuery::default());
        let correlation = catalogue_correlation(&mut actions, "one catalogue request");
        assert!(actions.take_collab_requests().is_empty(), "sent once");
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CatalogueResponse {
                correlation: correlation + 100,
                result: Err(ActionCallError::NotPermitted),
            },
        });
        assert!(answer.get().is_pending());
        actions.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Err(ActionCallError::NotPermitted),
            },
        });
        assert_eq!(answer.get(), Arrival::Failed(ActionCallError::NotPermitted));
    }
}

fn only_message(requests: Vec<CollabRequest>) -> RemoteActionMessage {
    let [CollabRequest::RemoteAction { message, .. }] = <[CollabRequest; 1]>::try_from(requests)
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
            correlation: None,
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

tilo_test_env::scenario! {
    fn a_client_made_before_its_host_is_entered_is_that_hosts;

    phase every_part_of_the_window_asks_over_the_client_the_app_carries;
    {
        // The app carries one client on its collaboration connection. A part of the window that
        // makes its client later — History following what it lists — asks over that one, so the app
        // carries the request, and the answer reaches the part that asked.
        let mut carried = RemoteActions::session();
        let _sharing = RemoteActions::carrying(carried.clone());
        let answer = RemoteActions::current().catalogue_page(CatalogueQuery::default());
        assert!(answer.get().is_pending(), "nothing in the app answers it");
        let correlation = catalogue_correlation(&mut carried, "the request a later part made");
        carried.handle_collab_event(&CollabEvent::RemoteAction {
            message: RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(CataloguePage {
                    revision: "r1".into(),
                    rows: vec![row("a")],
                    exhausted: true,
                    ..CataloguePage::default()
                }),
            },
        });
        assert!(
            matches!(answer.get(), Arrival::Ready(page) if page.rows.len() == 1),
            "the answer reaches the part that asked: {:?}",
            answer.get()
        );
    }

    phase a_client_that_names_the_session_keeps_to_it;
    {
        let provider = Arc::new(FakeHost::default());
        let mut session = RemoteActions::session();
        let _served = RemoteActions::enter(
            Arc::clone(&provider) as Arc<dyn ActionProvider + Send + Sync>,
            Caller::local_person("ada").in_workspace("workspace"),
        );
        let _pending = session.catalogue_page(CatalogueQuery::default());
        assert_eq!(session.take_collab_requests().len(), 1);
    }

    phase an_invocation_names_the_press_the_window_handled;
    // What a call made while the window handles the person's input travels with is that press's
    // serial, which only the compositor can confirm: the host refuses one it did not issue.
    {
        tilo_ui_base::press::note(7);
        let mut actions = RemoteActions::session();
        let _running = actions.invoke_with_id(
            InvocationId(uuid::Uuid::from_bytes([0x61; 16])),
            target("slow"),
            InvokeArgument::None,
            None,
        );
        let requests = actions.take_collab_requests();
        assert!(
            matches!(
                requests.as_slice(),
                [CollabRequest::RemoteAction {
                    message: RemoteActionMessage::Invoke(_),
                    press: Some(7),
                }]
            ),
            "the invocation names the press it followed: {requests:?}"
        );
        tilo_ui_base::press::forget();
    }
}
