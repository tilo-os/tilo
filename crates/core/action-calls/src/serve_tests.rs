//! Serving requests against a provider: a host answering each invocation once as its caller,
//! the invocation log behind it, and the wire's messages read back through a frame.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use tilo_actions::outcome::{ActionFailure, ActionOutput, InvocationId, InvocationUpdate};
use tilo_actions::params::ARGUMENTS_JSON_PREFIX;
use tilo_actions::remote::*;
use tilo_actions::{
    ActionAddress, ActionContext, ActionContract, ActionDescriptor, ActionState, Appearance,
    ExecutionLocus, InputSpec, OperationKind,
};

use crate::{Admission, InvocationLog, Reservation, serve};

fn alice() -> Caller {
    Caller::authenticated([1; 32], [2; 32])
}

fn mallory() -> Caller {
    Caller::authenticated([9; 32], [8; 32])
}

fn descriptor(resource: &str, action: &str) -> ActionDescriptor {
    ActionDescriptor {
        did: None,
        state: ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(resource, action),
        resource_kind: "notes.note".into(),
        resource_label: "Groceries".into(),
        action_label: "Archive".into(),
        keywords: Vec::new(),
        icon: None,
        description: Some("Move the note to the archive".into()),
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Document,
        contract: ActionContract::default(),
        input: InputSpec::None,
        params: Vec::new(),
        view: None,
    }
}

/// A provider with one action whose runs are counted, so a test can prove
/// how many times the action actually started.
struct CountingHost {
    runs: AtomicU32,
    log: Mutex<InvocationLog<Mutex<InvocationUpdate>>>,
}

impl CountingHost {
    fn new() -> Self {
        Self {
            runs: AtomicU32::new(0),
            log: Mutex::new(InvocationLog::default()),
        }
    }

    fn runs(&self) -> u32 {
        self.runs.load(Ordering::SeqCst)
    }

    fn finish(&self, caller: &Caller, id: &InvocationId) {
        if let Some(update) = self.log.lock().unwrap().get(caller, id) {
            *update.lock().unwrap() = InvocationUpdate::Succeeded {
                invocation: *id,
                output: ActionOutput::default(),
            };
        }
    }
}

impl CountingHost {
    fn cancel_now(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionCallError> {
        let log = self.log.lock().unwrap();
        let mut update = log
            .get(caller, id)
            .ok_or(ActionCallError::UnknownInvocation)?
            .lock()
            .unwrap();
        if update.is_terminal() {
            return Err(ActionCallError::NotCancellable {
                reason: "finished".into(),
            });
        }
        *update = InvocationUpdate::Cancelled {
            invocation: *id,
            message: None,
        };
        drop(update);
        drop(log);
        Ok(())
    }
}

impl ActionProvider for CountingHost {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::from_rows(
            "1".into(),
            vec![catalogue_row("note:1", "archive")],
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready((|| {
            if request.target.address != ActionAddress::new("note:1", "archive") {
                return Err(ActionCallError::UnknownAction);
            }
            let update = {
                let mut log = self.log.lock().unwrap();
                let (update, _) = log.admit(caller, &request, || {
                    self.runs.fetch_add(1, Ordering::SeqCst);
                    Ok(Mutex::new(InvocationUpdate::Accepted {
                        invocation: request.invocation,
                    }))
                })?;
                let update = update.lock().unwrap().clone();
                drop(log);
                update
            };
            Ok(InvocationState::new(request.invocation, update).receipt())
        })())
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.log
            .lock()
            .unwrap()
            .get(caller, id)
            .map(|update| InvocationState::new(*id, update.lock().unwrap().clone()))
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(self.cancel_now(caller, id))
    }
}

fn catalogue_row(resource: &str, action: &str) -> CatalogueRow {
    CatalogueRow {
        offered: None,
        provider: "7".into(),
        app_id: "notes".into(),
        parent_resource: String::new(),
        resource: CatalogueResource {
            id: resource.into(),
            kind: "notes.note".into(),
            label: "Groceries".into(),
            presentation: None,
        },
        action: Some(descriptor(resource, action)),
        has_children: false,
        visibility: None,
        surface: None,
    }
}

/// The answer of a provider that answers immediately.
fn now<T>(mut answer: Pending<T>) -> T {
    match answer.poll_ready() {
        PendingPoll::Ready(value) => value,
        PendingPoll::Waiting | PendingPoll::Abandoned => panic!("the provider answers at once"),
    }
}

/// A provider offering more actions than one frame holds: twelve thousand,
/// each described at length, as a large app's surface publishes them.
#[derive(Clone)]
struct HugeCatalogue;

const HUGE_CATALOGUE: usize = 12_000;

impl ActionProvider for HugeCatalogue {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        let rows = (0..HUGE_CATALOGUE)
            .map(|n| {
                let mut row = catalogue_row(&format!("note:{n}"), &format!("archive-{n}"));
                if let Some(action) = row.action.as_mut() {
                    action.description = Some("Moves the note out of the list. ".repeat(8));
                }
                row
            })
            .collect();
        Pending::ready(Ok(CataloguePage::from_rows("1".into(), rows, &query)))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        _request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

/// A caller paging a catalogue larger than a frame, through the provider's
/// answer and the frame each page travels in, reads every action once, and
/// no frame is over the limit: a page ends at its size as well as its count,
/// whatever count was asked for, plain or a resource at a time.
#[test]
fn a_catalogue_larger_than_a_frame_travels_in_pages_that_each_fit_one() {
    for by_resource in [false, true] {
        let mut query = CatalogueQuery {
            max_results: u32::MAX,
            by_resource,
            ..CatalogueQuery::default()
        };
        let mut listed = Vec::new();
        let mut pages = 0usize;
        loop {
            let asked = RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: query.clone(),
            };
            let answer =
                now(serve(&HugeCatalogue, &alice(), asked).expect("a request is answered"));
            let bytes = RemoteActionFrame::new(answer)
                .encode()
                .expect("every page fits one frame");
            assert!(bytes.len() <= MAX_REMOTE_ACTION_FRAME_BYTES);
            let RemoteActionMessage::CatalogueResponse {
                result: Ok(page), ..
            } = RemoteActionFrame::decode(&bytes).unwrap().message
            else {
                panic!("a catalogue request is answered with its page");
            };
            pages += 1;
            listed.extend(
                expand(page.rows)
                    .into_iter()
                    .filter_map(|row| row.action.map(|action| action.address.action)),
            );
            if page.exhausted {
                break;
            }
            query.cursor = page.next_cursor;
            query.revision = Some(page.revision);
        }
        assert!(pages > 1, "the catalogue is larger than one frame");
        let expected: Vec<String> = (0..HUGE_CATALOGUE)
            .map(|n| format!("archive-{n}"))
            .collect();
        assert_eq!(listed, expected, "every action is listed once, in order");
    }
}

fn archive(invocation: InvocationId) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: "7".into(),
            address: ActionAddress::new("note:1", "archive"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn round_trip(message: RemoteActionMessage) -> RemoteActionMessage {
    let bytes = RemoteActionFrame::new(message).encode().unwrap();
    RemoteActionFrame::decode(&bytes).unwrap().message
}

/// One of each catalogue and invoke message shape.
fn request_messages(invocation: InvocationId) -> Vec<RemoteActionMessage> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("title".into(), serde_json::json!("Weekly"));
    vec![
        RemoteActionMessage::CatalogueRequest {
            correlation: 3,
            query: CatalogueQuery::default(),
        },
        RemoteActionMessage::CatalogueRequest {
            correlation: 5,
            query: CatalogueQuery {
                provider: Some("7".into()),
                parent_resource: Some("folder:a".into()),
                needle: "gro".into(),
                cursor: vec![1, 2],
                revision: Some("r1".into()),
                max_results: 8,
                limits: Some(CatalogueLimits {
                    max_bytes: Some(4096),
                    max_work: None,
                    max_depth: Some(1),
                }),
                ..CatalogueQuery::default()
            },
        },
        RemoteActionMessage::CatalogueResponse {
            correlation: 3,
            result: now(CountingHost::new().query(&alice(), CatalogueQuery::default())),
        },
        RemoteActionMessage::CatalogueResponse {
            correlation: 4,
            result: Err(ActionCallError::Unavailable {
                reason: "no apps".into(),
            }),
        },
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            argument: InvokeArgument::Json(arguments),
            context: Some(ActionContext::for_resource(
                "note:1",
                "notes.note",
                "Groceries",
            )),
            ..archive(invocation)
        }),
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            argument: InvokeArgument::Text("hello".into()),
            ..archive(invocation)
        }),
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            confirmation: Some(Confirmation::Phrase("delete it".into())),
            ..archive(invocation)
        }),
        RemoteActionMessage::Invoke(InvokeRequest {
            wait_ms: Some(2_000),
            appearance: None,
            ..archive(invocation)
        }),
    ]
}

/// One of each receipt, update and cancel message shape.
fn outcome_messages(invocation: InvocationId) -> Vec<RemoteActionMessage> {
    vec![
        RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::json!({"archived": true})),
                    resources: vec!["notes.note:1".into()],
                },
            },
        }),
        RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Failed {
                error: ActionFailure {
                    code: "disk_full".into(),
                    message: "no space".into(),
                    details: None,
                    retryable: true,
                },
            },
        }),
        RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Rejected {
                error: ActionCallError::InvocationConflict,
            },
        }),
        RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Cancelled {
                message: Some("stopped".into()),
            },
        }),
        RemoteActionMessage::observe(invocation),
        RemoteActionMessage::Observe {
            correlation: 7,
            invocation,
            wait_ms: Some(10_000),
            seen: Some(UpdateMark::Progress { sequence: 2 }),
        },
        RemoteActionMessage::Update {
            invocation,
            state: Some(InvocationState::new(
                invocation,
                InvocationUpdate::Progress {
                    invocation,
                    sequence: 2,
                    phase: "copying".into(),
                    completed: Some(1),
                    total: Some(4),
                    message: None,
                },
            )),
            correlation: Some(7),
        },
        RemoteActionMessage::Update {
            invocation,
            state: None,
            correlation: Some(7),
        },
        RemoteActionMessage::Cancel { invocation },
        RemoteActionMessage::CancelResponse {
            invocation,
            result: Err(ActionCallError::NotCancellable {
                reason: "finished".into(),
            }),
        },
    ]
}

tilo_test_env::scenario! {
    fn remote_messages_and_failures_read_back_typed;

    phase a_stale_revision_is_typed_and_travels_as_older_peers_read_it;
    // A stale revision is typed in this build and travels in the shape
    // every peer decodes: an older peer reads the invalid request it always
    // sent, and the reasons older providers gave it read back as the variant.
    {
        let encoded = serde_json::to_value(ActionCallError::StaleRevision).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({"InvalidRequest": {"reason": STALE_REVISION_REASON}})
        );
        assert_eq!(
            serde_json::from_value::<ActionCallError>(encoded).unwrap(),
            ActionCallError::StaleRevision
        );
        assert_eq!(ActionCallError::StaleRevision.code(), "stale_revision");
        // Only that exact reason: a refusal that merely mentions a revision
        // is the refusal it is.
        let other = ActionCallError::Refused {
            reason: "the call changed since that revision, honestly".into(),
        };
        let round_trip: ActionCallError =
            serde_json::from_value(serde_json::to_value(&other).unwrap()).unwrap();
        assert_eq!(round_trip, other);
        let needs = ActionCallError::InvalidRequest {
            reason: CONFIRMED_ACTION_NEEDS_REVISION.into(),
        };
        assert!(needs.needs_revision());
        assert!(!ActionCallError::StaleRevision.needs_revision());
    }

    phase a_failure_with_details_round_trips;
    // A failure's structured details travel beside its message.
    {
        let update = InvocationUpdate::Failed {
            invocation: InvocationId::new(),
            error: ActionFailure {
                code: "invalid_arguments".into(),
                message: "argument `pageSize` must be at most 64".into(),
                details: Some(serde_json::json!({
                    "param": "pageSize",
                    "violation": {"kind": "maximum", "bound": 64}
                })),
                retryable: false,
            },
        };
        let encoded = serde_json::to_string(&update).unwrap();
        assert_eq!(
            serde_json::from_str::<InvocationUpdate>(&encoded).unwrap(),
            update
        );
    }

    phase a_refusal_for_want_of_the_person_carries_what_they_are_shown;
    // A run that reached something only the person may allow is refused for
    // want of their answer, and that refusal carries what the run stated
    // about it: the page departure it holds, as it travels to whoever asks them.
    {
        let facts = asked::AskedFacts::default()
            .with_fact("kind", "submit")
            .with_fact("destination", "https://visa.example")
            .with_fact("picture", "tilo.content:5f2b");
        let invocation = InvocationId::new();
        let state = InvocationState::new(
            invocation,
            InvocationUpdate::Failed {
                invocation,
                error: ActionFailure {
                    code: CONFIRMATION_REQUIRED_CODE.into(),
                    message: "the person has to allow this".into(),
                    details: Some(facts.to_details()),
                    retryable: false,
                },
            },
        );
        let receipt = state.receipt();
        let ReceiptStatus::Rejected {
            error: ActionCallError::ConfirmationRequired(stated),
        } = &receipt.status
        else {
            panic!("a run waiting on the person is a refusal: {receipt:?}");
        };
        assert_eq!(**stated, facts);
        // The refusal travels with it, so whoever asks the person in another process
        // shows what leaves and not a name for it.
        let error = ActionCallError::asks(facts);
        let wire = serde_json::to_value(&error).unwrap();
        assert_eq!(
            serde_json::from_value::<ActionCallError>(wire).unwrap(),
            error
        );
        let wire = postcard::to_allocvec(&error).unwrap();
        assert_eq!(postcard::from_bytes::<ActionCallError>(&wire).unwrap(), error);
    }

    phase the_remote_wire_is_versioned_and_explicit;
    // Every message round-trips through a frame, frames from another version are refused not
    // misread, and the wire names versions and messages explicitly.
    // Invocations each phase below in order; every phase builds its own fixture.
    {
        every_message_round_trips_through_a_frame();
        frames_from_another_version_are_refused_not_misread();
        the_wire_names_versions_and_messages_explicitly();
    }
}

fn every_message_round_trips_through_a_frame() {
    let invocation = InvocationId(uuid::Uuid::from_u128(7));
    let messages = request_messages(invocation)
        .into_iter()
        .chain(outcome_messages(invocation));
    for message in messages {
        assert_eq!(round_trip(message.clone()), message);
    }
}

fn frames_from_another_version_are_refused_not_misread() {
    let mut frame = serde_json::to_value(RemoteActionFrame::new(
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery::default(),
        },
    ))
    .unwrap();
    frame["version"] = serde_json::json!(REMOTE_ACTIONS_VERSION + 1);
    // A newer peer may also use a message this build has never seen.
    frame["message"] = serde_json::json!({"Subscribe": {"topic": 1}});
    let bytes = serde_json::to_vec(&frame).unwrap();
    assert_eq!(
        RemoteActionFrame::decode(&bytes),
        Err(RemoteFrameError::UnsupportedVersion {
            version: REMOTE_ACTIONS_VERSION + 1
        })
    );
    assert!(matches!(
        RemoteActionFrame::decode(
            format!(r#"{{"version":{REMOTE_ACTIONS_VERSION},"message":{{"Subscribe":{{}}}}}}"#)
                .as_bytes()
        ),
        Err(RemoteFrameError::Malformed(_))
    ));
    assert!(matches!(
        RemoteActionFrame::decode(&vec![b' '; MAX_REMOTE_ACTION_FRAME_BYTES + 1]),
        Err(RemoteFrameError::TooLarge { .. })
    ));
}

fn the_wire_names_versions_and_messages_explicitly() {
    let bytes = RemoteActionFrame::new(RemoteActionMessage::Cancel {
        invocation: InvocationId(uuid::Uuid::nil()),
    })
    .encode()
    .unwrap();
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        format!(
            r#"{{"version":{REMOTE_ACTIONS_VERSION},"message":{{"Cancel":{{"invocation":"00000000-0000-0000-0000-000000000000"}}}}}}"#
        )
    );
}

fn a_retried_invocation_reports_the_original_and_runs_once() {
    let provider = CountingHost::new();
    let id = InvocationId::new();
    let first = now(provider.invoke(&alice(), archive(id))).unwrap();
    assert_eq!(first.status, ReceiptStatus::Accepted);
    let retry = now(provider.invoke(&alice(), archive(id))).unwrap();
    assert_eq!(retry, first);
    assert_eq!(provider.runs(), 1, "a retry never starts a second run");

    provider.finish(&alice(), &id);
    let after = now(provider.invoke(&alice(), archive(id))).unwrap();
    assert!(matches!(after.status, ReceiptStatus::Completed { .. }));
    assert_eq!(provider.runs(), 1, "a retry after completion reports it");
}

fn an_invocation_id_cannot_be_reused_or_borrowed() {
    let provider = CountingHost::new();
    let id = InvocationId::new();
    now(provider.invoke(&alice(), archive(id))).unwrap();

    let mut different = archive(id);
    different.argument = InvokeArgument::Text("other".into());
    assert_eq!(
        now(provider.invoke(&alice(), different)),
        Err(ActionCallError::InvocationConflict)
    );
    assert_eq!(
        now(provider.invoke(&mallory(), archive(id))),
        Err(ActionCallError::InvocationConflict),
        "another actor cannot attach to someone else's invocation"
    );
    assert_eq!(provider.observe(&mallory(), &id), None);
    assert_eq!(
        now(provider.cancel(&mallory(), &id)),
        Err(ActionCallError::UnknownInvocation)
    );
    assert_eq!(provider.runs(), 1);
}

fn a_refused_invocation_is_not_remembered() {
    let mut log = InvocationLog::<u32>::default();
    let request = archive(InvocationId::new());
    assert_eq!(
        log.admit(&alice(), &request, || Err(ActionCallError::NotPermitted)),
        Err(ActionCallError::NotPermitted)
    );
    let (handle, admission) = log.admit(&alice(), &request, || Ok(5)).unwrap();
    assert_eq!((*handle, admission), (5, Admission::Started));
}

fn the_log_forgets_oldest_invocations_past_capacity() {
    let mut log = InvocationLog::<u32>::with_capacity(2);
    let requests: Vec<_> = (0..3).map(|_| archive(InvocationId::new())).collect();
    for (n, request) in (0u32..).zip(&requests) {
        log.admit(&alice(), request, || Ok(n)).unwrap();
    }
    assert_eq!(log.get(&alice(), &requests[0].invocation), None);
    assert_eq!(log.get(&alice(), &requests[2].invocation), Some(&2));
}

fn serve_answers_requests_and_ignores_responses() {
    let provider = std::sync::Arc::new(CountingHost::new());
    let id = InvocationId::new();
    let Some(RemoteActionMessage::Receipt(receipt)) = serve(
        &provider,
        &alice(),
        RemoteActionMessage::Invoke(archive(id)),
    )
    .map(now) else {
        panic!("an invoke is answered with a receipt");
    };
    assert_eq!(receipt.status, ReceiptStatus::Accepted);

    let mut unknown = archive(InvocationId::new());
    unknown.target.address = ActionAddress::new("note:2", "delete");
    assert_eq!(
        serve(
            &provider,
            &alice(),
            RemoteActionMessage::Invoke(unknown.clone())
        )
        .map(now),
        Some(RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: unknown.invocation,
            status: ReceiptStatus::Rejected {
                error: ActionCallError::UnknownAction
            },
        }))
    );
    assert_eq!(
        serve(
            &provider,
            &alice(),
            RemoteActionMessage::Cancel { invocation: id }
        )
        .map(now),
        Some(RemoteActionMessage::CancelResponse {
            invocation: id,
            result: Ok(()),
        })
    );
    let Some(RemoteActionMessage::Update {
        state: Some(state), ..
    }) = serve(&provider, &alice(), RemoteActionMessage::observe(id)).map(now)
    else {
        panic!("an observe is answered with the invocation state");
    };
    assert!(matches!(
        state.receipt().status,
        ReceiptStatus::Cancelled { .. }
    ));
    assert!(serve(&provider, &alice(), RemoteActionMessage::Receipt(receipt)).is_none());
}

fn typed_arguments_use_the_local_invocation_slot() {
    let mut object = serde_json::Map::new();
    object.insert("n".into(), serde_json::json!(2));
    assert_eq!(InvokeArgument::None.to_slot(), None);
    assert_eq!(
        InvokeArgument::Text("hi".into()).to_slot().as_deref(),
        Some("hi")
    );
    assert_eq!(
        InvokeArgument::Json(object.clone()).to_slot(),
        Some(format!("{ARGUMENTS_JSON_PREFIX}{{\"n\":2}}"))
    );
    // A slot read back names the argument it was written from, so a
    // device that sends an invocation on runs what it admitted.
    for argument in [
        InvokeArgument::None,
        InvokeArgument::Text("hi".into()),
        InvokeArgument::Json(object),
    ] {
        assert_eq!(
            InvokeArgument::from_slot(argument.to_slot().as_deref()),
            argument
        );
    }
}

fn a_remote_device_is_the_person_its_root_names() {
    let caller = Caller::authenticated([0xAB; 32], [1; 32]);
    assert_eq!(
        caller.acting().member_id(),
        Some(&tilo_member_types::MemberId::person("ab".repeat(32))),
        "every device of one person is the same member"
    );
    assert_eq!(caller.workspace(), None);
    let agent = Caller::local_agent("helper", "ada").in_workspace("workspace-1");
    assert_eq!(
        agent.acting().member_id(),
        Some(&tilo_member_types::MemberId::agent("helper", "ada"))
    );
    assert_eq!(agent.workspace(), Some("workspace-1"));
    assert_eq!(agent.device_public_key(), None);
}

fn a_refusal_answers_every_request_and_nothing_else() {
    let error = ActionCallError::Unavailable {
        reason: "down".into(),
    };
    let id = InvocationId::new();
    assert_eq!(
        refuse(&invoke_message(id), error.clone()),
        Some(RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: id,
            status: ReceiptStatus::Rejected {
                error: error.clone()
            },
        }))
    );
    assert_eq!(
        refuse(&RemoteActionMessage::observe(id), error.clone()),
        Some(RemoteActionMessage::Update {
            invocation: id,
            state: None,
            correlation: Some(0),
        })
    );
    assert_eq!(
        refuse(
            &RemoteActionMessage::Update {
                invocation: id,
                state: None,
                correlation: None,
            },
            error
        ),
        None
    );
}

fn invoke_message(id: InvocationId) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(archive(id))
}

/// A change feed, asked across the API.
mod change_wire;

tilo_test_env::scenario! {
    fn a_host_serves_each_invocation_once_as_its_caller;

    phase a_retry_with_another_appearance_is_another_invocation;
    {
        let request = archive(InvocationId(uuid::Uuid::from_u128(9)));
        let appearing = InvokeRequest {
            appearance: Some(Appearance::new("alice", None, None).unwrap()),
            ..request.clone()
        };
        assert!(!request.same_invocation(&appearing));
        assert!(appearing.same_invocation(&InvokeRequest {
            wait_ms: Some(10),
            ..appearing.clone()
        }));
        let json = serde_json::to_string(&request).unwrap();
        assert!(!json.contains("appearance"), "{json}");
    }

    phase remote_invocations_run_at_most_once_per_owned_id;
    // A retried invocation reports the original and runs once; ids cannot be reused or borrowed;
    // refusals are not remembered; the log forgets the oldest past capacity; a reservation holds
    // the id until the start completes.
    // Invocations each phase below in order; every phase builds its own fixture.
    {
        a_retried_invocation_reports_the_original_and_runs_once();
        an_invocation_id_cannot_be_reused_or_borrowed();
        a_refused_invocation_is_not_remembered();
        the_log_forgets_oldest_invocations_past_capacity();
        a_reservation_holds_the_id_until_the_start_completes();
    }

    phase admitted_executions_share_authority_but_cannot_borrow_invocations;
    {
        let first = alice().in_new_execution();
        let second = alice().in_new_execution();
        assert_eq!(first.acting(), second.acting());
        assert_eq!(first.workspace(), second.workspace());
        assert_ne!(first.execution_id(), second.execution_id());
        let provider = CountingHost::new();
        let id = InvocationId::new();
        let admitted = now(provider.invoke(&first, archive(id))).unwrap();
        assert_eq!(now(provider.invoke(&first, archive(id))).unwrap(), admitted);
        assert_eq!(
            now(provider.invoke(&second, archive(id))),
            Err(ActionCallError::InvocationConflict)
        );
        assert_eq!(provider.observe(&second, &id), None);
        assert_eq!(
            now(provider.cancel(&second, &id)),
            Err(ActionCallError::UnknownInvocation)
        );
        let retained = first.clone();
        let hash = |caller: &Caller| {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            std::hash::Hash::hash(caller, &mut hasher);
            std::hash::Hasher::finish(&hasher)
        };
        let before = hash(&first);
        provider.retire_execution(&first);
        assert!(!retained.execution_is_live());
        assert!(second.execution_is_live());
        assert_eq!(first, retained);
        assert_eq!(hash(&first), before);
        let reconnected = retained.in_new_execution();
        assert!(reconnected.execution_is_live());
        assert_ne!(reconnected.execution_id(), first.execution_id());
        provider.finish(&first, &id);
        assert!(matches!(
            now(provider.invoke(&first, archive(id))).unwrap().status,
            ReceiptStatus::Completed { .. }
        ));
        assert!(provider.observe(&first, &id).is_some());
        assert!(provider.observe(&second, &id).is_none());
        // The connection that replaced the ended one is the same caller going on: it follows
        // and asks again what it started there, and the invocation is carried out once. The
        // one open beside it borrows nothing, even now.
        assert!(reconnected.continues(&first));
        assert!(!second.continues(&first));
        assert!(provider.observe(&reconnected, &id).is_some());
        assert!(matches!(
            now(provider.invoke(&reconnected, archive(id))).unwrap().status,
            ReceiptStatus::Completed { .. }
        ));
        assert_eq!(provider.runs(), 1);
        let someone_else = Caller::authenticated([3; 32], [4; 32]).in_new_execution();
        assert!(!someone_else.continues(&first), "another device continues nothing");
    }

    phase serving_answers_each_request_as_the_person_the_device_belongs_to;
    // Serve answers requests and ignores responses, typed arguments use the local invocation slot,
    // a remote device is the person its root names, and a refusal answers every request and nothing
    // else.
    // Invocations each phase below in order; every phase builds its own fixture.
    {
        serve_answers_requests_and_ignores_responses();
        typed_arguments_use_the_local_invocation_slot();
        a_remote_device_is_the_person_its_root_names();
        a_refusal_answers_every_request_and_nothing_else();
    }

    phase capacity_churn_preserves_in_flight_deduplication;
    {
        let caller = alice();
        let mut log = InvocationLog::<u32>::with_capacity(3);
        let pending = archive(InvocationId::new());
        assert_eq!(log.reserve(&caller, &pending), Ok(Reservation::Reserved));
        let mut completed = VecDeque::new();
        for n in 0..8 {
            let request = archive(InvocationId::new());
            assert_eq!(
                log.admit(&caller, &request, || Ok(n)),
                Ok((&n, Admission::Started))
            );
            completed.push_back(request.invocation);
            if completed.len() > 2 {
                let evicted = completed.pop_front().unwrap();
                assert_eq!(log.get(&caller, &evicted), None);
            }
            for id in &completed {
                assert!(log.get(&caller, id).is_some());
            }
            assert!(matches!(
                log.admit(&caller, &pending, || panic!("duplicate dispatch")),
                Err(ActionCallError::Unanswered)
            ));
            assert_eq!(
                log.reserve(&mallory(), &pending),
                Err(ActionCallError::InvocationConflict)
            );
        }
        log.complete(&pending.invocation, 99);
        assert_eq!(
            log.admit(&caller, &pending, || panic!("duplicate dispatch")),
            Ok((&99, Admission::Retried))
        );
        let next = archive(InvocationId::new());
        assert_eq!(log.reserve(&caller, &next), Ok(Reservation::Reserved));
        assert_eq!(log.get(&caller, &pending.invocation), None);
    }

    phase capacity_churn_refuses_new_ids_when_every_slot_is_reserved;
    {
        let caller = alice();
        // Zero capacity is clamped to one, which must still protect its reservation.
        for capacity in 0..=2 {
            let mut log = InvocationLog::<u32>::with_capacity(capacity);
            let pending: Vec<_> = (0..capacity.max(1))
                .map(|_| archive(InvocationId::new()))
                .collect();
            for request in &pending {
                assert_eq!(log.reserve(&caller, request), Ok(Reservation::Reserved));
            }
            let refused = archive(InvocationId::new());
            for _ in 0..4 {
                assert!(matches!(
                    log.admit(&caller, &refused, || panic!("dispatch at capacity")),
                    Err(ActionCallError::Unavailable { .. })
                ));
                for request in &pending {
                    assert!(matches!(
                        log.admit(&caller, request, || panic!("duplicate dispatch")),
                        Err(ActionCallError::Unanswered)
                    ));
                }
            }
            for (n, request) in pending.iter().enumerate() {
                let n = u32::try_from(n).unwrap();
                log.complete(&request.invocation, n);
                assert_eq!(
                    log.admit(&caller, request, || panic!("duplicate dispatch")),
                    Ok((&n, Admission::Retried))
                );
            }
            // Capacity refusals are not remembered and can be retried later.
            assert_eq!(log.reserve(&caller, &refused), Ok(Reservation::Reserved));
            log.abandon(&refused.invocation);
            assert_eq!(log.reserve(&caller, &refused), Ok(Reservation::Reserved));
        }
    }
}

fn a_reservation_holds_the_id_until_the_start_completes() {
    let mut log = InvocationLog::<u32>::default();
    let request = archive(InvocationId::new());
    assert_eq!(log.reserve(&alice(), &request), Ok(Reservation::Reserved));
    assert!(
        matches!(
            log.reserve(&alice(), &request),
            Err(ActionCallError::Unanswered)
        ),
        "a retry during the start neither runs twice nor pretends to be admitted: it is \
         asked again, as an answer lost on its way is"
    );
    assert_eq!(log.get(&alice(), &request.invocation), None);
    log.complete(&request.invocation, 9);
    assert_eq!(log.reserve(&alice(), &request), Ok(Reservation::Admitted));
    assert_eq!(log.get(&alice(), &request.invocation), Some(&9));

    let failed = archive(InvocationId::new());
    assert_eq!(log.reserve(&alice(), &failed), Ok(Reservation::Reserved));
    log.abandon(&failed.invocation);
    assert_eq!(
        log.reserve(&alice(), &failed),
        Ok(Reservation::Reserved),
        "an abandoned start ran nothing and may be admitted again"
    );
}
