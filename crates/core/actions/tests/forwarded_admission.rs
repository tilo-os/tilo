//! A provider handed on as a shared handle is the provider: a call made through the handle
//! carries what the dispatch admitted of it ([`Admitted`]) — the cancellation its caller sets,
//! the provider a call's own further calls go through — and reaches the provider's
//! [`asks_first`](ActionProvider::asks_first) and [`watch`](ActionProvider::watch) too.
//!
//! A handle that forwards only `invoke` starts the call with a default [`Admitted`]: the call runs
//! on after its caller cancelled it, and no longer goes through its session.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admits, Admitted, Caller, InvokeArgument, InvokeRequest,
    Outcome, Pending, PendingPoll, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput};

const PROVIDER: &str = "test.forwarded";
const APP: &str = "so.tilo.test";
const WATCHED: &str = "the test provider watches this";

/// What a provider was told of the call it was handed.
struct Seen {
    stop: Arc<AtomicBool>,
    through: Option<Arc<dyn ActionProvider + Send + Sync>>,
}

/// A provider that records what the dispatch told it of each call.
#[derive(Default)]
struct Recorder {
    ran: Mutex<Option<Seen>>,
    watched: AtomicUsize,
}

impl Admits for Recorder {
    const PROVIDER: &'static str = PROVIDER;
    const APP: &'static str = APP;

    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        Vec::new()
    }

    fn start(
        &self,
        _caller: &Caller,
        _request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        *self.ran.lock().unwrap() = Some(Seen {
            stop: Arc::clone(&admitted.stop),
            through: admitted.through.clone(),
        });
        Ok(Pending::ready(Ok(ActionOutput::default())))
    }

    fn asks_first(
        &self,
        _caller: &Caller,
        _request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Option<Pending<Result<(), ActionCallError>>> {
        // The provider's own way of asking: a call nobody approved yet waits for its person.
        *self.ran.lock().unwrap() = Some(Seen {
            stop: Arc::clone(&admitted.stop),
            through: admitted.through.clone(),
        });
        Some(Pending::ready(Ok(())))
    }

    fn live(
        &self,
        _caller: &Caller,
        _request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::LiveView, String> {
        self.watched.fetch_add(1, Ordering::SeqCst);
        Err(WATCHED.to_owned())
    }
}

fn caller() -> Caller {
    Caller::local_person("person").in_workspace("workspace")
}

fn request() -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: PROVIDER.into(),
            address: ActionAddress::new("resource", "action"),
        },
        argument: InvokeArgument::Json(serde_json::Map::new()),
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// What a handle was handed, as the provider it wraps saw it.
fn admitted_once(recorder: &Recorder) -> Seen {
    let seen = recorder.ran.lock().unwrap().take();
    seen.expect("the provider was handed the call")
}

/// A shared handle passes the call's admission to the provider it shares.
#[test]
fn a_call_through_a_shared_handle_reaches_the_provider_admitted() {
    let recorder = Arc::new(Recorder::default());
    let through: Arc<dyn ActionProvider + Send + Sync> = Arc::new(Recorder::default());
    let stop = Arc::new(AtomicBool::new(false));
    let admitted = Admitted {
        through: Some(Arc::clone(&through)),
        stop: Arc::clone(&stop),
        ..Admitted::default()
    };

    let mut receipt =
        <Arc<Recorder> as ActionProvider>::run(&recorder, &caller(), request(), admitted);
    assert!(
        matches!(receipt.poll_ready(), PendingPoll::Ready(Ok(_))),
        "the call was carried out"
    );

    let seen = admitted_once(&recorder);
    assert!(
        Arc::ptr_eq(&seen.stop, &stop),
        "the provider stops on the flag its caller's cancel sets"
    );
    assert!(
        seen.through
            .as_ref()
            .is_some_and(|answered| Arc::ptr_eq(answered, &through)),
        "the provider makes its own calls through the session the dispatch named"
    );
}

/// A handle erased to the trait still passes the call's admission on.
#[test]
fn a_call_through_an_erased_handle_reaches_the_provider_admitted() {
    let recorder = Arc::new(Recorder::default());
    let erased: Arc<dyn ActionProvider + Send + Sync> = Arc::clone(&recorder) as _;
    let stop = Arc::new(AtomicBool::new(false));
    let admitted = Admitted {
        through: None,
        stop: Arc::clone(&stop),
        ..Admitted::default()
    };

    let mut receipt = <Arc<dyn ActionProvider + Send + Sync> as ActionProvider>::run(
        &erased,
        &caller(),
        request(),
        admitted,
    );
    assert!(
        matches!(receipt.poll_ready(), PendingPoll::Ready(Ok(_))),
        "the call was carried out"
    );
    let seen = admitted_once(&recorder);
    assert!(
        Arc::ptr_eq(&seen.stop, &stop),
        "the provider stops on the flag its caller's cancel sets"
    );
}

/// A handle answers its person's question, and follows a live result, as the provider does.
#[test]
fn a_handle_reaches_the_providers_question_and_live_result() {
    let recorder = Arc::new(Recorder::default());
    let stop = Arc::new(AtomicBool::new(false));
    let admitted = Admitted {
        through: None,
        stop: Arc::clone(&stop),
        ..Admitted::default()
    };

    let asking =
        <Arc<Recorder> as ActionProvider>::asks_first(&recorder, &caller(), &request(), &admitted);
    let Some(mut asked) = asking else {
        panic!("the handle answers its person's question");
    };
    assert!(
        matches!(asked.poll_ready(), PendingPoll::Ready(Ok(()))),
        "the provider decides before the call whether it waits for its person"
    );
    let seen = admitted_once(&recorder);
    assert!(
        Arc::ptr_eq(&seen.stop, &stop),
        "the question is asked with what the dispatch admitted of the call"
    );

    let watching = <Arc<Recorder> as ActionProvider>::watch(&recorder, &caller(), &request());
    assert_eq!(
        watching.err().as_deref(),
        Some(WATCHED),
        "the handle follows the provider's live result"
    );
    assert_eq!(recorder.watched.load(Ordering::SeqCst), 1);
}
