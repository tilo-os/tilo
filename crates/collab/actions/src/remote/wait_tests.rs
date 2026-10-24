use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use super::super::{
    ActionHost, ActionHostError, Admission, Caller, CataloguePage, CatalogueQuery, Completer,
    InvocationLedger, InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending,
    PendingPoll, REMOTE_ACTIONS_VERSION, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget, pending, serve,
};
use super::{MAX_IMMEDIATE_WAKES, MAX_REMOTE_WAIT, UpdateMark};
use crate::ActionAddress;
use crate::outcome::{ActionOutput, InvocationId, InvocationUpdate};

/// How a [`Stepped`] host signals changes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Signals {
    /// Not at all: `next_change` is `None`, as for a host that predates it.
    None,
    /// When the test says so.
    OnDemand,
    /// Always already fired: a host whose every wake is spurious.
    AlwaysFired,
}

/// One invocation whose state the test steps, and the change signals
/// waiters asked for.
struct Stepped {
    signals: Signals,
    update: Mutex<Option<InvocationUpdate>>,
    revoked: AtomicBool,
    waiting: Mutex<Vec<Completer<()>>>,
    invoked: Mutex<Vec<InvokeRequest>>,
}

impl Stepped {
    fn new(signals: Signals) -> Arc<Self> {
        Arc::new(Self {
            signals,
            update: Mutex::new(None),
            revoked: AtomicBool::new(false),
            waiting: Mutex::new(Vec::new()),
            invoked: Mutex::new(Vec::new()),
        })
    }

    /// A host already running `id`, still at `Accepted`.
    fn running(signals: Signals, id: InvocationId) -> Arc<Self> {
        let host = Self::new(signals);
        host.step(InvocationUpdate::Accepted { invocation: id });
        host
    }

    fn step(&self, update: InvocationUpdate) {
        *self.update.lock().unwrap() = Some(update);
    }

    /// Fire every change signal handed out so far.
    fn signal(&self) {
        let waiting = std::mem::take(&mut *self.waiting.lock().unwrap());
        for waiter in waiting {
            waiter.complete(());
        }
    }

    /// Signals a wait still listens on; one an answered wait let go of is
    /// abandoned, which ends the host's own watch for it.
    fn parked(&self) -> usize {
        self.waiting
            .lock()
            .unwrap()
            .iter()
            .filter(|waiter| !waiter.is_abandoned())
            .count()
    }
}

impl ActionHost for Stepped {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let invocation = request.invocation;
        self.invoked.lock().unwrap().push(request);
        self.step(InvocationUpdate::Accepted { invocation });
        Pending::ready(Ok(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        if self.revoked.load(Ordering::SeqCst) {
            return None;
        }
        let update = self.update.lock().unwrap().clone()?;
        Some(InvocationState::new(*id, update))
    }

    fn next_change(&self, _caller: &Caller, _id: &InvocationId) -> Option<Pending<()>> {
        match self.signals {
            Signals::None => None,
            Signals::AlwaysFired => Some(Pending::ready(())),
            Signals::OnDemand => {
                let (changed, change) = pending();
                self.waiting.lock().unwrap().push(changed);
                Some(change)
            }
        }
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}

fn alice() -> Caller {
    Caller::authenticated([1; 32], [2; 32])
}

fn id() -> InvocationId {
    InvocationId(uuid::Uuid::from_u128(0x606))
}

fn succeeded(invocation: InvocationId) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(serde_json::json!({"renamed": true})),
            resources: Vec::new(),
        },
    }
}

fn waiting_observe(invocation: InvocationId, wait_ms: u32) -> RemoteActionMessage {
    RemoteActionMessage::Observe {
        invocation,
        wait_ms: Some(wait_ms),
        seen: Some(UpdateMark::Accepted),
    }
}

fn rename(invocation: InvocationId, wait_ms: Option<u32>) -> InvokeRequest {
    InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: "7".into(),
            address: ActionAddress::new("doc:1", "rename"),
        },
        argument: InvokeArgument::Text("Plans".into()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms,
        appearance: None,
    }
}

fn answered<T>(answer: &mut Pending<T>) -> Option<T> {
    match answer.poll_ready() {
        PendingPoll::Ready(value) => Some(value),
        PendingPoll::Waiting => None,
        PendingPoll::Abandoned => panic!("a waiting answer is never abandoned"),
    }
}

fn state_of(message: RemoteActionMessage) -> Option<InvocationState> {
    let RemoteActionMessage::Update { state, .. } = message else {
        panic!("an observe is answered with an update: {message:?}");
    };
    state
}

#[test]
fn a_waiting_observe_answers_when_the_invocation_finishes() {
    let host = Stepped::running(Signals::OnDemand, id());
    let mut answer = serve(&host, &alice(), waiting_observe(id(), 1_000)).unwrap();
    assert!(answered(&mut answer).is_none(), "held while nothing moved");
    assert_eq!(host.parked(), 1, "one parked signal, no thread, no timer");

    host.step(succeeded(id()));
    host.signal();
    let state = state_of(answered(&mut answer).expect("answered on the change"));
    assert_eq!(state.unwrap().update, succeeded(id()));
}

#[test]
fn a_spurious_wake_keeps_one_wait_parked() {
    let host = Stepped::running(Signals::OnDemand, id());
    let mut answer = serve(&host, &alice(), waiting_observe(id(), 1_000)).unwrap();
    for _ in 0..3 {
        host.signal();
        assert!(
            answered(&mut answer).is_none(),
            "nothing moved past Accepted"
        );
        assert_eq!(host.parked(), 1, "at most one signal per waiting request");
    }
    host.step(InvocationUpdate::Progress {
        invocation: id(),
        sequence: 1,
        phase: "renaming".into(),
        completed: None,
        total: None,
        message: None,
    });
    host.signal();
    let state = state_of(answered(&mut answer).expect("progress moves past what was seen"));
    assert_eq!(
        UpdateMark::of(&state.unwrap().update),
        UpdateMark::Progress { sequence: 1 }
    );
}

#[test]
fn a_waiting_observe_times_out_with_the_latest_state() {
    let host = Stepped::running(Signals::OnDemand, id());
    let mut at_once = serve(&host, &alice(), waiting_observe(id(), 0)).unwrap();
    let state = state_of(answered(&mut at_once).expect("a zero wait answers at once"));
    assert_eq!(UpdateMark::of(&state.unwrap().update), UpdateMark::Accepted);

    let mut briefly = serve(&host, &alice(), waiting_observe(id(), 1)).unwrap();
    assert!(answered(&mut briefly).is_none());
    let past = tilo_clock::now()
        .checked_add(std::time::Duration::from_millis(2))
        .unwrap();
    while tilo_clock::now() < past {
        std::hint::spin_loop();
    }
    // The host's bounded change interval: nothing changed, the wait is over.
    host.signal();
    let state = state_of(answered(&mut briefly).expect("answered once the wait ran out"));
    assert_eq!(UpdateMark::of(&state.unwrap().update), UpdateMark::Accepted);
    assert_eq!(host.parked(), 0, "nothing stays parked after the answer");
}

#[test]
fn a_wait_is_capped_whatever_it_asks() {
    let before = tilo_clock::now();
    let until = super::deadline(u32::MAX);
    assert!(until <= tilo_clock::now().checked_add(MAX_REMOTE_WAIT).unwrap());
    assert!(until >= before.checked_add(MAX_REMOTE_WAIT).unwrap());
}

#[test]
fn an_old_style_observe_answers_immediately() {
    let host = Stepped::running(Signals::OnDemand, id());
    let old = format!(
        r#"{{"version":{REMOTE_ACTIONS_VERSION},"message":{{"Observe":{{"invocation":"{}"}}}}}}"#,
        id().0
    );
    let frame = RemoteActionFrame::decode(old.as_bytes()).expect("an old frame still decodes");
    assert_eq!(frame.message, RemoteActionMessage::observe(id()));
    let mut answer = serve(&host, &alice(), frame.message).unwrap();
    let state = state_of(answered(&mut answer).expect("answered at once, as before"));
    assert_eq!(UpdateMark::of(&state.unwrap().update), UpdateMark::Accepted);
    assert_eq!(host.parked(), 0);
}

#[test]
fn a_host_without_change_signals_answers_a_waiting_observe_at_once() {
    let host = Stepped::running(Signals::None, id());
    let mut answer = serve(&host, &alice(), waiting_observe(id(), 1_000)).unwrap();
    let state = state_of(answered(&mut answer).expect("nothing to wait on"));
    assert_eq!(UpdateMark::of(&state.unwrap().update), UpdateMark::Accepted);
}

#[test]
fn a_host_whose_signal_always_fired_cannot_spin_a_wait() {
    let host = Stepped::running(Signals::AlwaysFired, id());
    let mut answer = serve(&host, &alice(), waiting_observe(id(), 1_000)).unwrap();
    let state =
        state_of(answered(&mut answer).unwrap_or_else(|| {
            panic!("answered after at most {MAX_IMMEDIATE_WAKES} immediate wakes")
        }));
    assert_eq!(UpdateMark::of(&state.unwrap().update), UpdateMark::Accepted);
}

#[test]
fn a_waiting_invoke_returns_a_terminal_receipt() {
    let host = Stepped::new(Signals::OnDemand);
    let mut answer = serve(
        &host,
        &alice(),
        RemoteActionMessage::Invoke(rename(id(), Some(2_000))),
    )
    .unwrap();
    assert!(answered(&mut answer).is_none(), "held past admission");
    assert_eq!(
        host.invoked.lock().unwrap().as_slice(),
        [rename(id(), None)],
        "the host runs the request without the transport's wait"
    );
    host.step(succeeded(id()));
    host.signal();
    assert_eq!(
        answered(&mut answer),
        Some(RemoteActionMessage::Receipt(InvocationReceipt {
            invocation: id(),
            status: ReceiptStatus::Completed {
                output: ActionOutput {
                    value: Some(serde_json::json!({"renamed": true})),
                    resources: Vec::new(),
                },
            },
        })),
        "one round trip carries the outcome"
    );

    // Without a wait the receipt is the admission, as before.
    let host = Stepped::new(Signals::OnDemand);
    let mut answer = serve(
        &host,
        &alice(),
        RemoteActionMessage::Invoke(rename(id(), None)),
    )
    .unwrap();
    assert!(matches!(
        answered(&mut answer),
        Some(RemoteActionMessage::Receipt(InvocationReceipt {
            status: ReceiptStatus::Accepted,
            ..
        }))
    ));
}

#[test]
fn a_waiting_answer_rechecks_the_caller() {
    let host = Stepped::running(Signals::OnDemand, id());
    let mut answer = serve(&host, &alice(), waiting_observe(id(), 1_000)).unwrap();
    // Access goes between the request and the change.
    host.revoked.store(true, Ordering::SeqCst);
    host.step(succeeded(id()));
    host.signal();
    assert_eq!(
        state_of(answered(&mut answer).expect("answered")),
        None,
        "the answer is read as the caller is now"
    );
}

#[test]
fn a_waiting_observe_ends_with_its_execution() {
    let host = Stepped::running(Signals::OnDemand, id());
    let caller = alice().in_new_execution();
    let mut answer = serve(&host, &caller, waiting_observe(id(), 1_000)).unwrap();
    assert!(answered(&mut answer).is_none());
    caller.retire_execution();
    host.signal();
    let state = state_of(answered(&mut answer).expect("a retired run stops waiting"));
    assert_eq!(UpdateMark::of(&state.unwrap().update), UpdateMark::Accepted);
}

#[test]
fn a_retry_that_waits_differently_is_the_same_invocation() {
    let mut ledger = InvocationLedger::<u32>::default();
    ledger
        .admit(&alice(), &rename(id(), None), || Ok(1))
        .unwrap();
    let (handle, admission) = ledger
        .admit(&alice(), &rename(id(), Some(5_000)), || Ok(2))
        .unwrap();
    assert_eq!((*handle, admission), (1, Admission::Retried));
    let mut other = rename(id(), Some(5_000));
    other.argument = InvokeArgument::Text("Other".into());
    assert_eq!(
        ledger
            .admit(&alice(), &other, || Ok(3))
            .map(|(handle, _)| *handle),
        Err(ActionHostError::InvocationConflict)
    );
}

/// The v1 vocabulary as a peer built before waits decodes it.
mod old {
    use super::{Deserialize, InvocationId, InvokeArgument, RemoteActionTarget, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    pub struct InvokeRequest {
        pub invocation: InvocationId,
        pub target: RemoteActionTarget,
        #[serde(default)]
        pub argument: InvokeArgument,
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    pub enum Message {
        Invoke(InvokeRequest),
        Observe { invocation: InvocationId },
    }

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    pub struct Frame {
        pub version: u16,
        pub message: Message,
    }
}

#[test]
fn old_and_new_peers_read_each_others_frames() {
    let encode = |message| RemoteActionFrame::new(message).encode().unwrap();

    // A new caller's waiting requests reach an old host as the plain
    // requests it always answered.
    let observe: old::Frame =
        serde_json::from_slice(&encode(waiting_observe(id(), 10_000))).unwrap();
    assert_eq!(observe.message, old::Message::Observe { invocation: id() });
    let invoke: old::Frame = serde_json::from_slice(&encode(RemoteActionMessage::Invoke(rename(
        id(),
        Some(2_000),
    ))))
    .unwrap();
    let old::Message::Invoke(request) = invoke.message else {
        panic!("an invoke");
    };
    assert_eq!(request.invocation, id());
    assert_eq!(request.argument, InvokeArgument::Text("Plans".into()));

    // An old caller's requests reach a new host without a wait.
    for (sent, received) in [
        (
            old::Message::Observe { invocation: id() },
            RemoteActionMessage::observe(id()),
        ),
        (
            old::Message::Invoke(old::InvokeRequest {
                invocation: id(),
                target: rename(id(), None).target,
                argument: InvokeArgument::Text("Plans".into()),
            }),
            RemoteActionMessage::Invoke(rename(id(), None)),
        ),
    ] {
        let bytes = serde_json::to_vec(&old::Frame {
            version: REMOTE_ACTIONS_VERSION,
            message: sent,
        })
        .unwrap();
        assert_eq!(RemoteActionFrame::decode(&bytes).unwrap().message, received);
    }

    // And a request that does not wait is the very bytes an old peer sent.
    assert_eq!(
        String::from_utf8(encode(RemoteActionMessage::observe(InvocationId(
            uuid::Uuid::nil()
        ))))
        .unwrap(),
        r#"{"version":1,"message":{"Observe":{"invocation":"00000000-0000-0000-0000-000000000000"}}}"#
    );
}
