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
    row(gap: .related) {
        text("{c.value}")
        button(bump(c)) as bump
    }
}

action bump(c: Counter) -> Int {
    c.value += 1
    c.value
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
    let client = tilolang::compile_package(
        CALLER,
        &tilolang::IdsLock::new(),
        std::slice::from_ref(&callee.meta),
    )
    .unwrap_or_else(|diagnostics| panic!("the caller compiles: {diagnostics:#?}"));
    Bundle::new(&client, std::slice::from_ref(&callee))
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

/// The address the caller publishes its action `name` at.
fn published_at(hosted: &Hosted, name: &str) -> tilo_actions::ActionAddress {
    hosted
        .published()
        .iter()
        .find(|action| action.address.action == name)
        .unwrap_or_else(|| panic!("the caller publishes `{name}`"))
        .address
        .clone()
}

/// Every call the instance sent through the dispatch, in order.
fn invokes(sent: &Sent) -> Vec<tilo_actions::remote::InvokeRequest> {
    sent.borrow()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message: RemoteActionMessage::Invoke(request),
                ..
            } => Some(request.clone()),
            _ => None,
        })
        .collect()
}

/// The one press the instance sent through the dispatch: the app's call of its own action, which
/// continues nothing.
fn the_press(sent: &Sent) -> tilo_actions::remote::InvokeRequest {
    let presses: Vec<_> = invokes(sent)
        .into_iter()
        .filter(|request| request.target.provider == tilo_actions::remote::OWN_PROVIDER)
        .collect();
    assert_eq!(presses.len(), 1, "one press, one call: {presses:#?}");
    let press = presses.into_iter().next().unwrap();
    assert!(press.on_behalf_of.is_none(), "a press continues nothing");
    press
}

/// The calls the instance made continuing `parent`.
fn children_of(sent: &Sent, parent: InvocationId) -> Vec<tilo_actions::remote::InvokeRequest> {
    invokes(sent)
        .into_iter()
        .filter(|request| request.on_behalf_of == Some(parent))
        .collect()
}

/// The dispatch hands the press `press` of `address` back to the instance as `id`, admitted as
/// the person who pressed.
fn hand_back(inbox: &Inbox, id: u64, press: InvocationId, address: &tilo_actions::ActionAddress) {
    serve(
        inbox,
        id,
        press,
        &(address.resource.clone(), address.action.clone()),
    );
}

/// The caller's instance with a window, the person at it: what its connection sent, and what the
/// test's dispatch hands it.
fn pressed_caller() -> (TilolangProcess, Window, Sent, Inbox) {
    let route = RouteSpec::app(CALLER_APP, uuid::Uuid::new_v4());
    let mut host = TilolangProcess::new(wake());
    let mut attributes = spec(1, "ada", &route);
    attributes
        .installed
        .insert(CALLEE_APP.to_owned(), String::new());
    let (mut window, sent, inbox) = instance_on(
        &mut host,
        attributes,
        bundles(),
        &route,
        Rc::default(),
        &Rc::default(),
    );
    settle(&mut host, &mut window);
    (host, window, sent, inbox)
}

/// Press the caller's action `name`, as the palette or a menu does.
fn press(host: &mut TilolangProcess, window: &mut Window, name: &str) {
    let address = published_at(host.instances().next().expect("the instance runs"), name);
    window.send(&[ToApp::Invoke {
        address,
        argument: None,
    }]);
    settle(host, window);
}

/// What the caller's counter holds, in the instance `host` runs.
fn count(host: &TilolangProcess) -> i64 {
    counter(host.instances().next().expect("the instance runs"))
}

/// The first node of `tree`, depth first, that `which` picks.
fn find_node<'a>(
    tree: &'a patch::Node,
    which: &dyn Fn(&patch::Node) -> bool,
) -> Option<&'a patch::Node> {
    if which(tree) {
        return Some(tree);
    }
    tree.children
        .iter()
        .find_map(|child| find_node(child, which))
}

tilo_test_env::scenario! {
    fn a_press_is_a_call_of_its_own_run_here_at_once;

    phase a_press_runs_at_once_and_is_answered_once_the_dispatch_hands_it_back;
    {
        let (mut host, mut window, sent, inbox) = pressed_caller();
        press(&mut host, &mut window, "bump");
        assert_eq!(count(&host), 1, "the press ran at once, before the dispatch answered");
        let bump = published_at(host.instances().next().unwrap(), "bump");
        let call = the_press(&sent);
        assert_eq!(call.target.address, bump, "the call is of the pressed action");
        assert!(
            matches!(
                call.confirmation,
                Some(tilo_actions::remote::Confirmation::Confirmed)
            ),
            "the press is the person's own word for it"
        );
        assert!(answered(&sent, 21).is_none(), "nothing to answer before it is handed back");

        // The dispatch admitted it as the person and hands it back: the instance answers it with
        // what it did, and does not run it again.
        hand_back(&inbox, 21, call.invocation, &bump);
        settle(&mut host, &mut window);
        assert_eq!(count(&host), 1, "a press runs once");
        match answered(&sent, 21) {
            Some(InvocationUpdate::Succeeded { invocation, output }) => {
                assert_eq!(invocation, call.invocation);
                assert_eq!(output.value, Some(serde_json::json!(1)), "what the action answered");
            }
            other => panic!("the press is answered succeeded: {other:?}"),
        }

        // One press is one step of the person's: their undo takes it back whole.
        window.send(&[ToApp::Undo]);
        settle(&mut host, &mut window);
        assert_eq!(count(&host), 0, "undo takes the press back");
    }

    phase a_call_the_press_makes_continues_it_once_the_dispatch_has_it;
    {
        let (mut host, mut window, sent, inbox) = pressed_caller();
        press(&mut host, &mut window, "twice");
        assert_eq!(count(&host), 1, "what the press wrote before its call is held");
        let call = the_press(&sent);
        assert!(
            children_of(&sent, call.invocation).is_empty(),
            "its call waits until the dispatch has the press it continues"
        );

        let address = published_at(host.instances().next().unwrap(), "twice");
        hand_back(&inbox, 22, call.invocation, &address);
        settle(&mut host, &mut window);
        let children = children_of(&sent, call.invocation);
        assert_eq!(children.len(), 1, "the call goes, continuing the press: {children:#?}");
        let child = &children[0];
        assert_eq!(child.target.address.action, "add");
        assert!(answered(&sent, 22).is_none(), "the press waits on its call");

        receipt(&inbox, child.invocation, completed(serde_json::json!(2)));
        settle(&mut host, &mut window);
        assert_eq!(count(&host), 3, "the press went on with the answer");
        match answered(&sent, 22) {
            Some(InvocationUpdate::Succeeded { invocation, .. }) => {
                assert_eq!(invocation, call.invocation, "the press ends with its run");
            }
            other => panic!("the press is answered succeeded: {other:?}"),
        }

        window.send(&[ToApp::Undo]);
        settle(&mut host, &mut window);
        assert_eq!(count(&host), 0, "one undo takes the whole press back");
    }

    phase a_press_the_dispatch_refuses_continues_nothing;
    {
        let (mut host, mut window, sent, inbox) = pressed_caller();
        press(&mut host, &mut window, "twice");
        let call = the_press(&sent);
        receipt(
            &inbox,
            call.invocation,
            ReceiptStatus::Rejected {
                error: ActionCallError::NotPermitted,
            },
        );
        settle(&mut host, &mut window);
        assert!(
            children_of(&sent, call.invocation).is_empty(),
            "nothing goes continuing a press the dispatch refused"
        );
        assert_eq!(count(&host), 0, "the run its call refused takes back what it wrote");
    }

    phase a_control_pressed_in_the_window_is_a_call_too;
    {
        let (mut host, mut window, sent, inbox) = pressed_caller();
        let root = window.root.as_ref().expect("the window shows");
        // The window's one control: the button that runs `bump`.
        let button = find_node(root, &|node| !node.events.is_empty())
            .unwrap_or_else(|| panic!("the window shows the bump button: {root:#?}"))
            .clone();
        let event = button.events.keys().next().expect("the button presses").clone();
        window.send(&[ToApp::Fire {
            level: patch::Level::Root,
            key: button.key,
            event,
            value: None,
            gesture: None,
        }]);
        settle(&mut host, &mut window);
        assert_eq!(count(&host), 1, "the button's press ran at once");
        let call = the_press(&sent);
        assert_eq!(call.target.address.action, "bump");
        hand_back(&inbox, 23, call.invocation, &call.target.address);
        settle(&mut host, &mut window);
        assert_eq!(count(&host), 1, "a press runs once");
        assert!(
            matches!(
                answered(&sent, 23),
                Some(InvocationUpdate::Succeeded { invocation, .. }) if invocation == call.invocation
            ),
            "{:?}",
            answered(&sent, 23)
        );
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
                ..
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
                error: ActionCallError::ConfirmationRequired(Box::default()),
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

    phase a_host_starting_over_the_stores_runs_ends_them_a_failure;
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

        // A host starts over the store: the run left waiting cannot be answered (the invocation
        // the call went out as died with the host, and what the run held went with it), so it is
        // ended with the failure that is true of it rather than left waiting for ever.
        let mut restarted = TilolangProcess::new(wake());
        let (mut window, sent, inbox) =
            instance_with_store(&mut restarted, attributes, bundles(), &route, &bus, store);
        settle(&mut restarted, &mut window);
        let restarted_instance = restarted.instances().next().unwrap();
        assert_eq!(
            restarted_instance.store().open_runs().len(),
            0,
            "the run left waiting is ended, not left waiting"
        );
        // The invocation the run served ends with it, where the person sees it, saying why.
        let ended = answered(&sent, 9).expect("the served invocation is answered");
        let InvocationUpdate::Failed { invocation, error } = ended else {
            panic!("the served invocation ends failed: {ended:?}");
        };
        assert_eq!(invocation, child.invocation);
        assert!(
            error.message.contains("stopped when its app restarted"),
            "{}",
            error.message
        );
        // An answer to the call arrives late: nobody waits on it any more, and nothing runs.
        receipt(&inbox, child.invocation, completed(serde_json::json!(2)));
        settle(&mut restarted, &mut window);
        assert_eq!(
            counter(restarted.instances().next().unwrap()),
            1,
            "nothing of the run went on"
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

/// An app whose answer drafts a message and sends it: the send reaches outside, so the person is
/// asked first, and what they read is the draft the answer wrote.
const ANSWERS: &str = r#"app test.answers "Answers"

type Draft {
    to: Text
    subject: Text
    body: Text
    key: Text
    sent: Bool = false
}

type Box {
    drafts: Collection<Draft>
}

root Box

default view box for Box

view box(b: Box) {
    text("{b.drafts.len()}")
}

action send(d: Draft) (shows: [(d.to, .addresses), d.subject, (d.body, .long), (d.key, .secret)]) {
    d.sent = true
    outside("post", (d.to, d.subject, d.body))
}

action answer(b: Box, to: Text, body: Text) -> Draft {
    let d = b.drafts.insert(Draft(to: to, subject: "Re: lunch", body: body, key: "s3cret"))
    send(d)
    d
}
"#;

const ANSWERS_APP: &str = "test.answers";

/// The drafts the answers app holds, each by whether it went.
fn drafts(hosted: &Hosted) -> Vec<bool> {
    let host = hosted.instance().host().expect("the runtime");
    let runtime = host.runtime();
    let root = host.root_of(ANSWERS_APP).expect("the app's root");
    let Value::List(drafts) = runtime.read(&root, "drafts").expect("the drafts read") else {
        panic!("drafts is a list");
    };
    drafts
        .into_iter()
        .map(|draft| {
            let Value::Ref(at) = draft else {
                panic!("a draft is a record: {draft:?}");
            };
            runtime.read(&at.target, "sent").expect("sent reads") == Value::Bool(true)
        })
        .collect()
}

/// Deliver the broker's invocation `id` of `action` on `resource`, with `arguments`, as `ada`.
fn deliver(
    inbox: &Inbox,
    id: u64,
    invocation: InvocationId,
    (resource, action): (String, String),
    arguments: &serde_json::Value,
) {
    inbox.borrow_mut().push_back(CollabEvent::Invocation {
        id,
        invocation,
        resource,
        action,
        argument: Some(format!(
            "{}{arguments}",
            tilo_actions::ARGUMENTS_JSON_PREFIX
        )),
        actor: tilo_actions::Actor::Member(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        context: None,
    });
}

/// The answers app running for `ada`, with the address it publishes `answer` at.
fn answers() -> (TilolangProcess, Window, Sent, Inbox, (String, String)) {
    let compiled = tilolang::compile_package(ANSWERS, &tilolang::IdsLock::new(), &[])
        .unwrap_or_else(|diagnostics| panic!("the app compiles: {diagnostics:#?}"));
    let route = RouteSpec::app(ANSWERS_APP, uuid::Uuid::new_v4());
    let mut host = TilolangProcess::new(wake());
    let attributes = spec(1, "ada", &route).showing(Shows::Nothing);
    let (mut window, sent, inbox) = instance_on(
        &mut host,
        attributes,
        Bundle::new(&compiled, &[]),
        &route,
        Rc::default(),
        &Rc::default(),
    );
    settle(&mut host, &mut window);
    let answer = host
        .instances()
        .next()
        .expect("the instance runs")
        .published()
        .iter()
        .find(|action| action.address.action == "answer")
        .map(|action| {
            (
                action.address.resource.clone(),
                action.address.action.clone(),
            )
        })
        .expect("the app publishes `answer`");
    (host, window, sent, inbox, answer)
}

tilo_test_env::scenario! {
    fn an_asked_call_of_the_apps_own_action_shows_what_its_run_drafted;

    phase allowed_the_send_runs_once_without_asking_again;
    {
        let (mut host, mut window, sent, inbox, answer) = answers();
        let served = InvocationId::new();
        deliver(
            &inbox,
            7,
            served,
            answer,
            &serde_json::json!({ "to": "bo@example.org", "body": "Noon works" }),
        );
        settle(&mut host, &mut window);
        let child = child_sent(&sent).expect("the send went through the dispatch");
        assert!(child.on_behalf_of == Some(served), "{child:?}");
        let facts = child.context.as_ref().map(|context| context.shows.clone());
        assert_eq!(
            facts,
            Some(vec![
                tilo_actions::remote::asked::Fact::new("To", "bo@example.org"),
                tilo_actions::remote::asked::Fact::new("Subject", "Re: lunch"),
                tilo_actions::remote::asked::Fact::new("Body", "Noon works"),
                tilo_actions::remote::asked::Fact::new("Key", ""),
            ]),
            "the card reads the draft the answer wrote, and never the secret"
        );
        assert_eq!(drafts(host.instances().next().unwrap()), [false]);

        // The dispatch asks the person, who allows it: it delivers the send to the app, which
        // runs it as the answer it was given, without asking again.
        receipt(
            &inbox,
            child.invocation,
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired(Box::default()),
            },
        );
        settle(&mut host, &mut window);
        let tilo_actions::remote::InvokeArgument::Json(arguments) = child.argument.clone() else {
            panic!("the send's arguments are named: {:?}", child.argument);
        };
        deliver(
            &inbox,
            8,
            child.invocation,
            (
                child.target.address.resource.clone(),
                child.target.address.action.clone(),
            ),
            &serde_json::Value::Object(arguments),
        );
        settle(&mut host, &mut window);
        assert!(
            matches!(answered(&sent, 8), Some(InvocationUpdate::Succeeded { .. })),
            "the allowed send runs: {:?}",
            answered(&sent, 8)
        );
        assert_eq!(
            child_sent(&sent).map(|again| again.invocation),
            Some(child.invocation),
            "nobody is asked again"
        );
        assert_eq!(drafts(host.instances().next().unwrap()), [true]);
        receipt(&inbox, child.invocation, completed(serde_json::Value::Null));
        settle(&mut host, &mut window);
        assert!(
            matches!(answered(&sent, 7), Some(InvocationUpdate::Succeeded { .. })),
            "the answer is answered once its send went: {:?}",
            answered(&sent, 7)
        );
    }

    phase declined_the_answer_ends_and_its_draft_is_taken_back;
    {
        let (mut host, mut window, sent, inbox, answer) = answers();
        deliver(
            &inbox,
            9,
            InvocationId::new(),
            answer,
            &serde_json::json!({ "to": "bo@example.org", "body": "Noon works" }),
        );
        settle(&mut host, &mut window);
        let child = child_sent(&sent).expect("the send went through the dispatch");
        assert_eq!(drafts(host.instances().next().unwrap()), [false]);
        receipt(
            &inbox,
            child.invocation,
            ReceiptStatus::Rejected {
                error: ActionCallError::ConfirmationRequired(Box::default()),
            },
        );
        settle(&mut host, &mut window);

        // The person declines, and the dispatch ends the call so: the whole run is refused, and
        // the draft it wrote is gone.
        receipt(
            &inbox,
            child.invocation,
            ReceiptStatus::Cancelled {
                message: Some(tilo_actions::remote::asked::DECLINED.to_owned()),
            },
        );
        settle(&mut host, &mut window);
        assert!(
            drafts(host.instances().next().unwrap()).is_empty(),
            "declining takes the draft back"
        );
        assert!(
            matches!(answered(&sent, 9), Some(InvocationUpdate::Failed { .. })),
            "the answer is refused: {:?}",
            answered(&sent, 9)
        );
    }
}
