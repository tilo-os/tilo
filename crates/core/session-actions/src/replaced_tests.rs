use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, PendingPoll, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_lock::MutexRecover as _;

use super::{CurrentSession, Reached};

/// A session's server: what it was asked, and what it says of each invocation.
#[derive(Default)]
struct Server {
    asked: Mutex<Vec<InvocationId>>,
    says: Mutex<HashMap<InvocationId, InvocationState>>,
    /// Ends every invocation it is asked at once.
    at_once: bool,
}

impl Server {
    fn say(&self, id: InvocationId, update: InvocationUpdate) {
        self.says
            .lock_recover()
            .insert(id, InvocationState::new(id, update));
    }

    fn asked(&self) -> Vec<InvocationId> {
        self.asked.lock_recover().clone()
    }
}

impl ActionProvider for Server {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.asked.lock_recover().push(request.invocation);
        let status = if self.at_once {
            ReceiptStatus::Completed {
                output: ActionOutput::default(),
            }
        } else {
            ReceiptStatus::Accepted
        };
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status,
        }))
    }

    fn observe(&self, _caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.says.lock_recover().get(id).cloned()
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// The session a page holds now, which the test replaces.
#[derive(Clone, Default)]
struct Holding(Arc<Mutex<Option<Reached>>>);

impl Holding {
    fn hold(&self, generation: u64, server: &Arc<Server>) {
        *self.0.lock_recover() = Some((generation, Arc::clone(server) as _));
    }

    fn let_go(&self) {
        *self.0.lock_recover() = None;
    }

    fn current(&self) -> CurrentSession {
        let held = self.clone();
        CurrentSession::new(
            move || held.0.lock_recover().clone(),
            || ActionCallError::Unavailable {
                reason: "not connected".into(),
            },
        )
    }
}

fn caller() -> Caller {
    Caller::local_person(tilo_member_types::PersonId::from_root([7; 32]))
}

fn watch(id: InvocationId) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation: id,
        target: RemoteActionTarget {
            provider: "person-accounts".into(),
            address: ActionAddress::new("accounts", "watch"),
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

fn item(id: InvocationId, sequence: u64, value: &str) -> InvocationUpdate {
    InvocationUpdate::Item {
        invocation: id,
        sequence,
        value: serde_json::json!(value),
    }
}

fn received<T>(mut answer: Pending<T>) -> T {
    match answer.poll_ready() {
        PendingPoll::Ready(value) => value,
        PendingPoll::Waiting | PendingPoll::Abandoned => panic!("no answer at once"),
    }
}

#[test]
fn what_a_window_follows_is_asked_again_of_the_session_that_replaced_its_own() {
    let holding = Holding::default();
    let first = Arc::new(Server::default());
    holding.hold(1, &first);
    let current = holding.current();
    let id = InvocationId::new();

    // A window follows the person's accounts on the first session, which answers twice.
    let receipt = received(current.invoke(&caller(), watch(id))).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    first.say(id, item(id, 1, "one account"));
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 1, "one account")
    );
    first.say(id, item(id, 2, "two accounts"));
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 2, "two accounts")
    );

    // The page lets that session go: the window is told what it was told last, and is not
    // told the invocation is gone. It looks again soon, not on a timer of its own.
    holding.let_go();
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 2, "two accounts")
    );
    assert!(current.next_change(&caller(), &id).is_some());
    // A pane opened while there is no session waits for the next one, rather than keeping a
    // refusal it would show after the page connected again.
    let opened = InvocationId::new();
    let receipt = received(current.invoke(&caller(), watch(opened))).unwrap();
    assert_eq!(receipt.status, ReceiptStatus::Accepted);
    assert_eq!(
        current.observe(&caller(), &opened).unwrap().update,
        InvocationUpdate::Accepted { invocation: opened }
    );
    assert!(current.next_change(&caller(), &opened).is_some());

    // The session that replaced it never heard of the invocation: it is asked again there,
    // once, under the same id, and until it answers the window keeps what it had.
    let second = Arc::new(Server::default());
    holding.hold(2, &second);
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 2, "two accounts")
    );
    assert_eq!(second.asked(), vec![id]);
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 2, "two accounts")
    );
    assert_eq!(
        second.asked(),
        vec![id],
        "asked again once, not on every look"
    );
    assert_eq!(first.asked(), vec![id]);
    // What the pane opened in between is asked of it too, and answered from there.
    current.observe(&caller(), &opened);
    assert_eq!(second.asked(), vec![id, opened]);
    second.say(opened, item(opened, 1, "one account"));
    assert_eq!(
        current.observe(&caller(), &opened).unwrap().update,
        item(opened, 1, "one account")
    );

    // Its first answer numbers its results from the start: the window hears it as the next one,
    // so a reading that moves past what it saw is a new reading.
    second.say(id, item(id, 1, "three accounts"));
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 3, "three accounts")
    );
    second.say(id, item(id, 2, "four accounts"));
    assert_eq!(
        current.observe(&caller(), &id).unwrap().update,
        item(id, 4, "four accounts")
    );
}

#[test]
fn what_ended_is_never_asked_again_and_what_ends_after_a_replacement_is_told_once() {
    let holding = Holding::default();
    let first = Arc::new(Server {
        at_once: true,
        ..Server::default()
    });
    holding.hold(1, &first);
    let current = holding.current();

    // An action that ended at its receipt is done: a new session does not start it again.
    let done = InvocationId::new();
    let receipt = received(current.invoke(&caller(), watch(done))).unwrap();
    assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
    let second = Arc::new(Server::default());
    holding.hold(2, &second);
    assert_eq!(current.observe(&caller(), &done), None);
    assert!(second.asked().is_empty());

    // One still running when its session was replaced, which the new session ends at once:
    // its caller is told how it ended, and then nothing more is kept of it.
    let running = InvocationId::new();
    received(current.invoke(&caller(), watch(running))).unwrap();
    let third = Arc::new(Server {
        at_once: true,
        ..Server::default()
    });
    holding.hold(3, &third);
    assert_eq!(
        current.observe(&caller(), &running).unwrap().update,
        InvocationUpdate::Succeeded {
            invocation: running,
            output: ActionOutput::default(),
        }
    );
    assert_eq!(third.asked(), vec![running]);
    assert_eq!(current.observe(&caller(), &running), None);
    assert!(current.next_change(&caller(), &running).is_none());
}
