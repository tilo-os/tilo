//! A hosted app's calls of another app's action go through the dispatch as child invocations of
//! the invocation being served: the run waits on the answer, and the invocation the broker served
//! is answered when the run ends.

use super::*;

use tilo_actions::remote::{
    ActionCallError, InvocationReceipt, ReceiptStatus, RemoteActionMessage,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};

/// An app another one calls: one action, on its own package, answering what it was given.
const CALLEE: &str = r#"app test.callee "Callee"

action add(n: Int) -> Int {
    n
}
"#;

/// An app whose action writes its counter, calls another app's action, and writes again: what
/// it wrote before the call is the run's, and taken back when the run traps after it.
const CALLER: &str = r#"app test.caller "Caller"

use test.callee as callee

type Counter {
    value: Int merge counter
}

root Counter

default view counter for Counter

view counter(c: Counter) {
    text("{c.value}")
}

action twice(c: Counter) -> Int {
    c.value += 1
    let n = callee.add(2)
    c.value += n
    c.value
}
"#;

const CALLER_APP: &str = "test.caller";
const CALLEE_APP: &str = "test.callee";

/// The caller and the package it uses, compiled from their sources.
fn bundles() -> Bundle {
    let callee = tilolang::compile_package(CALLEE, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the callee compiles: {diagnostics:#?}"));
    let caller = tilolang::compile_package(
        CALLER,
        &tilolang::IdsLock::new(),
        std::slice::from_ref(&callee.meta),
    )
    .unwrap_or_else(|diagnostics| panic!("the caller compiles: {diagnostics:#?}"));
    Bundle::new(&caller, std::slice::from_ref(&callee))
}

/// What the caller's counter holds.
fn counter(hosted: &Hosted) -> i64 {
    let host = hosted.instance().host().expect("the runtime");
    let root = host.root_of(CALLER_APP).expect("the caller's root");
    match host
        .runtime()
        .read(&root, "value")
        .expect("the counter reads")
    {
        Value::Int(value) => value,
        other => panic!("the counter's value is {other:?}"),
    }
}

/// The address the caller publishes `twice` at.
fn twice(hosted: &Hosted) -> (String, String) {
    let action = hosted
        .published()
        .iter()
        .find(|action| action.address.action == "twice")
        .expect("the caller publishes `twice`");
    (
        action.address.resource.clone(),
        action.address.action.clone(),
    )
}

/// Deliver the broker's invocation `id` of the action `address` to the instance.
fn serve(inbox: &Inbox, id: u64, invocation: InvocationId, address: &(String, String)) {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation,
        resource: address.0.clone(),
        action: address.1.clone(),
        argument: None,
        actor: tilo_actions::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        context: None,
    });
}

/// Answer the child call `invocation` as the dispatch would.
fn receipt(inbox: &Inbox, invocation: InvocationId, status: ReceiptStatus) {
    inbox.borrow_mut().push_back(CollabEvent::RemoteAction {
        message: RemoteActionMessage::Receipt(InvocationReceipt { invocation, status }),
    });
}

/// The child call the instance sent, as the dispatch reads it from what crossed its connection.
fn child_sent(sent: &Sent) -> Option<tilo_actions::remote::InvokeRequest> {
    sent.borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
            } => Some(request.clone()),
            _ => None,
        })
}

/// The answer the instance gave the broker for the invocation delivered as `id`.
fn answered(sent: &Sent, id: u64) -> Option<InvocationUpdate> {
    sent.borrow()
        .iter()
        .rev()
        .find_map(|request| match request {
            CollabRequest::UpdateInvocation {
                id: answered,
                update,
            } if *answered == id => Some(update.clone()),
            _ => None,
        })
}

/// The completed answer of a call answering `value`.
fn completed(value: serde_json::Value) -> ReceiptStatus {
    ReceiptStatus::Completed {
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

tilo_test_env::scenario! {
    fn a_child_call_waits_on_the_dispatch_and_its_answer_ends_the_served_invocation;

    phase a_call_that_needs_a_yes_waits_until_the_person_answers_and_then_answers_the_served_invocation;
    {
        let route = RouteSpec::app(CALLER_APP, uuid::Uuid::new_v4());
        let mut host = TilolangProcess::new(wake());
        let mut attributes = spec(1, "ada", &route).showing(Shows::Nothing);
        attributes.installed.insert(CALLEE_APP.to_owned(), String::new());
        let (mut window, sent, inbox) = instance_on(
            &mut host,
            attributes,
            bundles(),
            &route,
            Rc::default(),
            &Rc::default(),
        );
        settle(&mut host, &mut window);
        let address = twice(host.instances().next().expect("the instance runs"));
        let served = InvocationId::new();

        // The broker delivers the caller's action: the app's code writes, calls the other app,
        // and waits for its answer.
        serve(&inbox, 7, served, &address);
        settle(&mut host, &mut window);
        let child = child_sent(&sent).expect("the call went through the dispatch");
        assert!(
            child.on_behalf_of == Some(served),
            "the call continues the served invocation: {child:?}"
        );
        assert_eq!(
            counter(host.instances().next().unwrap()),
            1,
            "what it wrote before the call is held, not published"
        );
        assert!(
            answered(&sent, 7).is_none(),
            "the served invocation waits on the call"
        );

        // The dispatch asks the person: the call stays open and the run keeps waiting.
        receipt(
            &inbox,
            child.invocation,
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired { departure: Vec::new() },
            },
        );
        settle(&mut host, &mut window);
        assert!(
            answered(&sent, 7).is_none(),
            "an asked call is still open: nothing answers the served invocation"
        );

        // They allow it: the run goes on, writes again, and the served invocation is answered.
        receipt(&inbox, child.invocation, completed(serde_json::json!(2)));
        settle(&mut host, &mut window);
        assert_eq!(
            counter(host.instances().next().unwrap()),
            3,
            "the run went on with the answer and added it"
        );
        match answered(&sent, 7) {
            Some(InvocationUpdate::Succeeded { invocation, .. }) => {
                assert_eq!(invocation, served, "the served invocation is answered");
            }
            other => panic!("the served invocation is answered succeeded: {other:?}"),
        }
        assert!(
            host.instances()
                .next()
                .unwrap()
                .store()
                .open_runs()
                .is_empty(),
            "nothing waits once the run ended"
        );
    }

    phase a_declined_call_ends_the_run_and_takes_back_what_it_wrote;
    {
        let route = RouteSpec::app(CALLER_APP, uuid::Uuid::new_v4());
        let mut host = TilolangProcess::new(wake());
        let mut attributes = spec(1, "ada", &route).showing(Shows::Nothing);
        attributes.installed.insert(CALLEE_APP.to_owned(), String::new());
        let (mut window, sent, inbox) = instance_on(
            &mut host,
            attributes,
            bundles(),
            &route,
            Rc::default(),
            &Rc::default(),
        );
        settle(&mut host, &mut window);
        let address = twice(host.instances().next().expect("the instance runs"));
        let served = InvocationId::new();
        serve(&inbox, 8, served, &address);
        settle(&mut host, &mut window);
        let child = child_sent(&sent).expect("the call went through the dispatch");
        assert_eq!(counter(host.instances().next().unwrap()), 1);

        // The dispatch refuses the call: the app's code traps after the call, and what the run
        // wrote before it is taken back.
        receipt(
            &inbox,
            child.invocation,
            ReceiptStatus::Rejected {
                error: ActionCallError::NotPermitted,
            },
        );
        settle(&mut host, &mut window);
        assert_eq!(
            counter(host.instances().next().unwrap()),
            0,
            "what the run wrote before the call is taken back"
        );
        assert!(
            matches!(answered(&sent, 8), Some(InvocationUpdate::Failed { .. })),
            "the served invocation is answered failed: {:?}",
            answered(&sent, 8)
        );
    }

    phase a_host_starting_over_the_stores_runs_resumes_them_on_the_answer;
    {
        let route = RouteSpec::app(CALLER_APP, uuid::Uuid::new_v4());
        let bus = Rc::default();
        let mut host = TilolangProcess::new(wake());
        let mut attributes = spec(1, "ada", &route).showing(Shows::Nothing);
        attributes.installed.insert(CALLEE_APP.to_owned(), String::new());
        let (mut window, sent, inbox) = instance_on(
            &mut host,
            attributes.clone(),
            bundles(),
            &route,
            Rc::default(),
            &bus,
        );
        settle(&mut host, &mut window);
        let address = twice(host.instances().next().expect("the instance runs"));
        serve(&inbox, 9, InvocationId::new(), &address);
        settle(&mut host, &mut window);
        let child = child_sent(&sent).expect("the call went through the dispatch");
        assert_eq!(counter(host.instances().next().unwrap()), 1);
        // The store keeps the waiting run: it is what a host starting over it waits on.
        let store = host.instances().next().unwrap().store().clone();
        assert_eq!(store.open_runs().len(), 1, "the run waits on its call");
        drop(host);

        // A host starts over the store: its run map is the journals' waits, and the answer
        // resumes the run there.
        let mut restarted = TilolangProcess::new(wake());
        let (mut window, _sent, inbox) =
            instance_with_store(&mut restarted, attributes, bundles(), &route, &bus, store);
        settle(&mut restarted, &mut window);
        receipt(&inbox, child.invocation, completed(serde_json::json!(2)));
        settle(&mut restarted, &mut window);
        // The invocation the run was a step of is not kept with the journal, so its answer has
        // nobody to end: the run stays waiting rather than committing its half, unanswerable.
        let restarted = restarted.instances().next().unwrap();
        assert_eq!(
            restarted.store().open_runs().len(),
            1,
            "the resumed run is still waiting"
        );
        assert_eq!(
            counter(restarted),
            1,
            "what it wrote before the call is not committed, nobody was answered"
        );
    }
}

/// [`instance_on`] over a store the test already holds: a host starting over what an earlier
/// one left waiting.
fn instance_with_store(
    host: &mut TilolangProcess,
    attributes: Spec,
    bundle: Bundle,
    route: &RouteSpec,
    bus: &Rc<Bus>,
    store: ResourceStore,
) -> (Window, Sent, Inbox) {
    let broker = Broker::on(bus, route.clone(), &attributes.person);
    let (sent, inbox) = (broker.sent(), broker.inbox());
    let mut manager = CollaborationManager::with_peer_transport(broker);
    assert!(manager.send(CollabRequest::Hello {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id,
        record: route.document.clone(),
        resource_type: None,
        handle: Some(attributes.person.clone()),
    }));
    let _ = manager.poll();
    let connection = Connection::new(manager).expect("the connection joined its route");
    let (ours, theirs) = Channel::pair().unwrap();
    let watched = ours.as_fd().try_clone_to_owned().unwrap();
    let hosted = Hosted::new(
        attributes,
        connection,
        (store, tilolang_process::hosted::memory_content()),
        Arc::new(bundle),
        &tilolang_process::hosted::Kept::default(),
        Box::new(ChannelTransport::new(ours).unwrap()),
        (&Rc::new(Phrases::new("en-US")), host.formats()),
        wake(),
    );
    host.add(hosted, Some(watched));
    let window = Window::over(ChannelTransport::new(theirs).unwrap());
    (window, sent, inbox)
}
