//! What an agent's window holds before the page sends it: every form
//! submission, navigation or request with a method of its own, download and
//! payment waits for the person ([`super::holds`]).

use std::sync::Arc;

use ui::webview::{
    HeldKind, HeldRequest, PageKey, PageRequest, WebViewCommand, WebViewEngine as _,
};

use super::tests::{effect_operation, synced};
use super::*;

/// A window in `mode` at `url`, its engine the tests' own.
fn page_in(mode: BrowsingMode, url: &str) -> (Arc<TestEngine>, BrowserWidget, AutoCommit, ObjId) {
    let engine = TestEngine::new();
    engine.send(WebViewCommand::Navigate(url.into()));
    let mut widget = BrowserWidget::with_engine_mode(engine.clone(), true, mode);
    let (doc, root) = synced(&mut widget);
    (engine, widget, doc, root)
}

/// The page resource this window publishes.
fn page_resource(widget: &BrowserWidget) -> ui::tilo_actions::ResourceSpec {
    widget
        .published()
        .children
        .into_iter()
        .find(|resource| resource.id.0 == "browser.page")
        .expect("the page is published")
}

/// Start page action `id` with `args`, as a caller does.
fn start(
    widget: &mut BrowserWidget,
    id: &str,
    args: &serde_json::Value,
) -> ui::tilo_actions::InvocationId {
    let action = page_resource(widget)
        .actions
        .into_iter()
        .find(|action| action.id == id)
        .unwrap_or_else(|| panic!("{id} is published on the page"));
    let encoded = action
        .build_json(args.as_object().expect("arguments are an object"))
        .expect("the action takes these arguments");
    let invocation = ui::tilo_actions::InvocationId::new();
    assert!(
        matches!(
            widget.admit(invocation, effect_operation(&encoded.payload)),
            Ok(ui::framework::Effected::Pending)
        ),
        "{id} is accepted"
    );
    invocation
}

/// What `invocation` answered with, once the window has synced.
fn answer(
    widget: &mut BrowserWidget,
    doc: &AutoCommit,
    root: &ObjId,
    invocation: ui::tilo_actions::InvocationId,
) -> ui::tilo_actions::InvocationUpdate {
    widget.sync(doc, root);
    widget
        .poll_invocation(invocation)
        .expect("the invocation answered")
}

/// Every page request the engine was sent, in order.
fn page_requests(engine: &TestEngine) -> Vec<PageRequest> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::Page { request, .. } => Some(request),
            _ => None,
        })
        .collect()
}

/// Every answer the window gave to a held request, in order.
fn held_answers(engine: &TestEngine) -> Vec<(u32, bool)> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::AnswerHeldRequest { id, allow } => Some((id, allow)),
            _ => None,
        })
        .collect()
}

/// A submission the engine reports holding, as the visa form's is.
fn held_submit(id: u32) -> HeldRequest {
    let mut request = HeldRequest::new(id, HeldKind::Submit);
    request.method = "POST".into();
    request.origin = "https://visa.example".into();
    request
}

/// The code an action answers when what it reached waits for the person.
fn needs_the_person(update: &ui::tilo_actions::InvocationUpdate) -> bool {
    matches!(
        update,
        ui::tilo_actions::InvocationUpdate::Failed { error, .. }
            if error.code == holds::NEEDS_PERSON
    )
}

#[test]
fn only_a_window_an_agent_operates_holds_what_its_page_sends() {
    let (engine, ..) = page_in(BrowsingMode::Visitor, "https://visa.example/apply");
    assert!(
        engine.holds_departures(),
        "an agent's window holds every departure"
    );
    let (own, ..) = page_in(BrowsingMode::Normal, "https://visa.example/apply");
    assert!(
        !own.holds_departures(),
        "the person's own window sends what they click"
    );
}

#[test]
fn a_form_fills_without_asking_and_asks_once_when_it_is_submitted() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://visa.example/apply");
    let steps = [
        (
            "fill-page",
            serde_json::json!({"element": "d1e1", "text": "Ada"}),
        ),
        (
            "select-page",
            serde_json::json!({"element": "d1e2", "values": ["NL"]}),
        ),
        (
            "fill-page-secret",
            serde_json::json!({"element": "d1e3", "text": "hunter2"}),
        ),
    ];
    for (id, args) in steps {
        let invocation = start(&mut widget, id, &args);
        widget.sync(&doc, &root);
        let update = answer(&mut widget, &doc, &root, invocation);
        assert!(
            matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
            "{id} fills without asking anyone: {update:?}"
        );
    }
    assert!(
        held_answers(&engine).is_empty(),
        "filling a form asks nobody"
    );
    // The engine holds the submission the action starts, before the page
    // answers the action.
    engine.hold(held_submit(1));
    let submit = start(
        &mut widget,
        "submit-page",
        &serde_json::json!({"element": "d1e9"}),
    );
    widget.sync(&doc, &root);
    assert_eq!(
        page_requests(&engine).last(),
        Some(&PageRequest::Submit {
            element: "d1e9".into(),
            cross_origin: false,
            frame: None,
        }),
        "the submission is asked of the page"
    );
    let update = answer(&mut widget, &doc, &root, submit);
    assert!(
        needs_the_person(&update),
        "the submission waits for the person: {update:?}"
    );
    let Some(ui::tilo_actions::InvocationUpdate::Failed { error, .. }) = Some(&update) else {
        unreachable!()
    };
    assert!(
        error.message.contains("https://visa.example"),
        "the person is told where it goes: {}",
        error.message
    );
    assert!(
        error.message.contains("POST"),
        "and how it goes: {}",
        error.message
    );
}

#[test]
fn the_persons_allow_releases_the_held_submission_instead_of_submitting_again() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://visa.example/apply");
    engine.hold(held_submit(1));
    let submit = start(
        &mut widget,
        "submit-page",
        &serde_json::json!({"element": "d1e9"}),
    );
    widget.sync(&doc, &root);
    let asked = answer(&mut widget, &doc, &root, submit);
    assert!(needs_the_person(&asked));
    let sent = page_requests(&engine).len();
    // The person allowed it: the same invocation comes back, as the relay
    // hands a witnessed invocation to the window again.
    assert!(
        matches!(
            widget.admit(submit, effect_operation(&submit_payload(&widget))),
            Ok(ui::framework::Effected::Pending)
        ),
        "the invocation runs again"
    );
    assert_eq!(
        held_answers(&engine),
        vec![(1, true)],
        "the held submission leaves, exactly once"
    );
    assert_eq!(
        page_requests(&engine).len(),
        sent,
        "the page action is not run a second time"
    );
    engine.push_event(WebViewEvent::HeldRequestClosed { id: 1 });
    let released = answer(&mut widget, &doc, &root, submit);
    let ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } = &released else {
        panic!("the released submission answers what left: {released:?}");
    };
    let value = output.value.clone().unwrap();
    assert_eq!(value["released"], true);
    assert_eq!(value["origin"], "https://visa.example");
    assert_eq!(value["method"], "POST");
    assert!(
        widget.holds.waiting().next().is_none(),
        "nothing waits any more"
    );
}

/// The payload of the page's own `submit-page`, as a caller sends it again.
fn submit_payload(widget: &BrowserWidget) -> Vec<u8> {
    page_resource(widget)
        .actions
        .into_iter()
        .find(|action| action.id == "submit-page")
        .expect("submit-page is published")
        .build_json(
            serde_json::json!({"element": "d1e9"})
                .as_object()
                .expect("arguments"),
        )
        .expect("the action takes an element")
        .payload
}

#[test]
fn a_click_on_the_submit_button_is_held_and_the_page_stays() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://visa.example/apply");
    engine.hold(held_submit(2));
    let click = start(
        &mut widget,
        "click-page",
        &serde_json::json!({"element": "d1e9"}),
    );
    widget.sync(&doc, &root);
    let update = answer(&mut widget, &doc, &root, click);
    assert!(needs_the_person(&update), "{update:?}");
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::Click {
            element: "d1e9".into(),
            frame: None,
        }],
        "the click reached the page and the submission it started is held"
    );
    // The hold waits on the person through the invocation that reached it:
    // its answer runs that same invocation again, so the window publishes no
    // second way to answer it, and nobody is asked about it twice.
    let resource = page_resource(&widget);
    assert!(
        !resource
            .actions
            .iter()
            .any(|action| action.id == "keep-held-request" || action.id == "send-held-request"),
        "a hold an action reached is answered by that action, not beside it"
    );
    assert_eq!(
        held_answers(&engine),
        Vec::new(),
        "nothing was sent and nothing was dropped while the person decides"
    );
}

#[test]
fn a_hold_one_departure_names_does_not_answer_another() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://shop.example/order");
    for (id, origin) in [(4, "https://orders.example"), (5, "https://pay.example")] {
        let mut request = HeldRequest::new(id, HeldKind::Request);
        request.method = "POST".into();
        request.origin = origin.into();
        engine.hold(request);
    }
    widget.sync(&doc, &root);
    // Each departure is answered by the id it is published with: the page
    // names both, and neither answers the other.
    let named: Vec<String> = page_resource(&widget)
        .actions
        .into_iter()
        .filter(|action| action.id == "send-held-request")
        .filter_map(|action| {
            let id = action
                .params()
                .iter()
                .find(|param| param.name == "id")?
                .choices
                .first()?
                .to_string();
            Some(id)
        })
        .collect();
    assert_eq!(
        named,
        ["4", "5"],
        "each wait names the departure it answers"
    );
}

#[test]
fn enter_in_a_field_is_held() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://visa.example/apply");
    engine.hold(held_submit(3));
    let press = start(
        &mut widget,
        "press-page",
        &serde_json::json!({"key": "Enter", "element": "d1e1"}),
    );
    widget.sync(&doc, &root);
    let update = answer(&mut widget, &doc, &root, press);
    assert!(needs_the_person(&update), "{update:?}");
    assert_eq!(
        page_requests(&engine),
        vec![PageRequest::Press {
            element: Some("d1e1".into()),
            key: PageKey::parse("Enter").unwrap(),
            frame: None,
        }]
    );
}

#[test]
fn a_request_the_page_sends_on_its_own_is_published_and_answered() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://shop.example/order");
    let mut request = HeldRequest::new(4, HeldKind::Request);
    request.method = "POST".into();
    request.origin = "https://orders.example".into();
    engine.hold(request);
    widget.sync(&doc, &root);
    assert!(
        widget.page_operation.is_none(),
        "no action was running: the page posted on its own"
    );
    // The window publishes the wait, so whoever may ask the page answers it.
    let resource = page_resource(&widget);
    let ids: Vec<&str> = resource
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    assert!(ids.contains(&"send-held-request"), "{ids:?}");
    assert!(ids.contains(&"keep-held-request"), "{ids:?}");
    let mut send = resource
        .actions
        .into_iter()
        .find(|action| action.id == "send-held-request")
        .expect("a held request is sent from the page it waits on");
    assert!(
        send.type_mut().label.contains("orders.example"),
        "the person is told where it goes: {}",
        send.type_mut().label
    );
    let encoded = send
        .build_json(serde_json::json!({"id": "4"}).as_object().expect("an id"))
        .expect("the answer names the held request");
    let answer = ui::tilo_actions::InvocationId::new();
    widget
        .admit(answer, effect_operation(&encoded.payload))
        .expect("the answer is accepted");
    assert_eq!(held_answers(&engine), vec![(4, true)]);
}

#[test]
fn a_download_is_held_like_every_other_departure_the_page_starts() {
    let (engine, mut widget, doc, root) = page_in(BrowsingMode::Visitor, "https://files.example/");
    engine.push_event(WebViewEvent::NativeDownloadRequested {
        id: 9,
        url: "https://files.example/return.pdf".into(),
        name: "return.pdf".into(),
    });
    widget.sync(&doc, &root);
    let resource = page_resource(&widget);
    let mut send = resource
        .actions
        .into_iter()
        .find(|action| action.id == "send-held-request")
        .expect("the download waits on the person");
    let label = send.type_mut().label.clone();
    assert!(
        label.contains("return.pdf"),
        "the question names the file the person would keep: {label}"
    );
    let encoded = send
        .build_json(serde_json::json!({"id": "9"}).as_object().expect("an id"))
        .expect("the answer names the download");
    let answer = ui::tilo_actions::InvocationId::new();
    widget
        .admit(answer, effect_operation(&encoded.payload))
        .expect("the answer is accepted");
    assert!(
        engine.commands().into_iter().any(|command| matches!(
            command,
            WebViewCommand::DownloadDecision { id: 9, allow: true }
        )),
        "the person's allow keeps the file"
    );
    assert!(
        widget.capabilities.prompt(&widget.engine).is_none(),
        "the window puts no prompt of its own beside the person's question"
    );
}

#[test]
fn a_hold_nobody_answers_is_dropped_and_the_page_stays() {
    let (engine, mut widget, doc, root) =
        page_in(BrowsingMode::Visitor, "https://visa.example/apply");
    let mut request = held_submit(5);
    request.origin = "https://visa.example".into();
    engine.hold(request);
    widget.sync(&doc, &root);
    assert_eq!(widget.holds.waiting().count(), 1);
    // The window's own deadline passes with nobody answering.
    let dropped = widget
        .holds
        .expire(std::time::Instant::now() + std::time::Duration::from_secs(60 * 60));
    for held in dropped {
        let (id, kind) = (held.request.id, held.request.kind);
        widget.release_held(id, kind, false);
    }
    assert!(widget.holds.waiting().next().is_none());
    assert_eq!(
        held_answers(&engine),
        vec![(5, false)],
        "the window drops what nobody answered"
    );
    assert_eq!(page_requests(&engine), Vec::new(), "nothing left the page");
}
