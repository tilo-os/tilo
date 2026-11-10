//! A page's forms as the page's own actions, and site memory, as an agent
//! meets them: Browser in its real app host over the deterministic test
//! engine, its published actions invoked the way the host invokes them for
//! an agent it admitted, and its document read back as the workspace keeps
//! it.

use std::sync::Arc;

use ui::framework::testing::TestApp;
use ui::tilo_actions::{
    ActionFailure, ActionInvoker, ActionSpec, ActorKind, DoorError, InvocationUpdate, ParamType,
};
use ui::tilo_resource::Replica;
use ui::webview::{FormSubmit, PageNavigation, PageRecordNote, WebViewEngine as _};

use super::*;
use crate::resource::BrowserShell;
use crate::site_memory::{RememberedForm, RememberedQuestion};

const SITE: &str = "https://visa.test";
const APPLY: &str = "https://visa.test/apply";
const VISA: &str = "5f1c2a9b";

/// The visa form as the engine reads it, with what its fields hold at the
/// time: a name a person typed, a secret already entered.
fn visa_form() -> serde_json::Value {
    serde_json::json!({
        "form": "form1", "label": "Apply for a visitor visa", "method": "POST",
        "action": "/apply", "signature": VISA, "submit": ["Send application"],
        "fields": [
            {"name": "Full name", "kind": "text", "required": true, "value": "Ada Lovelace",
                "element": "d1e2"},
            {"name": "Date of birth: Day", "kind": "number", "value": "", "element": "d1e3"},
            {"name": "Arrival", "kind": "date", "required": true, "value": "",
                "element": "d1e4"},
            {"name": "Nationality", "kind": "choice", "choices": ["Kenya", "Other"],
                "value": "", "element": "d1e5"},
            {"name": "Languages", "kind": "choices", "choices": ["English", "Dutch", "German"],
                "value": [], "elements": ["d1e6", "d1e7", "d1e8"]},
            {"name": "I agree", "kind": "flag", "required": true, "value": false,
                "element": "d1e9"},
            {"name": "Passcode", "kind": "secret", "required": true,
                "value": "<4 characters hidden>", "element": "d1e10"},
        ],
    })
}

/// A second form of the same page: the site's search.
fn search_form() -> serde_json::Value {
    serde_json::json!({
        "form": "form2", "label": "Search this site", "method": "GET", "action": "/search",
        "signature": "0a11ce00", "submit": ["Search"],
        "fields": [{"name": "Words", "kind": "text", "value": "", "element": "d1e20"}],
    })
}

fn agent() -> ActionInvoker {
    ActionInvoker::actor("agent:ada").with_kind(ActorKind::Agent)
}

/// An engine answer from the page at `url`.
fn reply(url: &str, message: &str, data: serde_json::Value) -> PageReply {
    PageReply {
        url: url.into(),
        message: message.into(),
        outline: None,
        data: Some(data),
        image: None,
        navigation: PageNavigation::None,
        loading: false,
        record: PageRecordNote::default(),
    }
}

/// A Browser window an agent opened on `url`, over an engine of its own.
fn launch(arguments: &[&str]) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    let engine = TestEngine::new();
    crate::browser_widget::test_window_engine::install(engine.clone());
    let mut app = TestApp::<BrowserShell>::launched(arguments).expect("Browser opens");
    engine.push_event(WebViewEvent::LoadFinished);
    app.settle();
    (engine, app)
}

fn open(url: &str) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    launch(&[ui::webview::VISITOR_WINDOW_FLAG, url])
}

/// Close the window and open another on `url` over a new engine, on the
/// same profile and document: a later visit.
fn visit_again(app: TestApp<BrowserShell>, url: &str) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    let engine = TestEngine::new();
    crate::browser_widget::test_window_engine::install(engine.clone());
    let mut app = app
        .relaunch(&[ui::webview::VISITOR_WINDOW_FLAG, url])
        .expect("Browser opens again");
    engine.push_event(WebViewEvent::LoadFinished);
    app.settle();
    // A window opened again may show the page it showed last: the visit is
    // to `url`, so it goes there as following a link does.
    if shown(&mut app) != url {
        engine.send(WebViewCommand::Navigate(url.into()));
        app.settle();
    }
    assert_eq!(shown(&mut app), url);
    (engine, app)
}

/// The address of the page the window shows.
fn shown(app: &mut TestApp<BrowserShell>) -> String {
    app.with_widget::<crate::BrowserWindow, _>(|window| window.page_mut().current_uri.clone())
        .expect("the window shows a page")
}

fn find<'a>(resources: &'a [ResourceSpec], id: &str) -> Option<&'a ResourceSpec> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource)
        } else {
            find(&resource.children, id)
        }
    })
}

/// The actions the page publishes for its forms, as the host lists them.
fn form_actions(app: &mut TestApp<BrowserShell>) -> Vec<ActionSpec> {
    app.settle();
    let published = app.harness().resources();
    find(&published, "browser.page")
        .expect("the page is published")
        .actions
        .iter()
        .filter(|action| action.id.starts_with("form-") || action.id.starts_with("send-form-"))
        .cloned()
        .collect()
}

fn ids(actions: &[ActionSpec]) -> Vec<&str> {
    actions.iter().map(|action| action.id.as_str()).collect()
}

/// An action's parameters: name, type, whether required.
fn typed(action: &ActionSpec) -> Vec<(String, ParamType, bool)> {
    action
        .params()
        .iter()
        .map(|param| (param.name.to_string(), param.ty, param.required))
        .collect()
}

/// The address of the page's published action `action`, as the host takes
/// it from what the window publishes.
fn address(
    app: &mut TestApp<BrowserShell>,
    action: &str,
) -> ui::bound_dispatch::BoundActionAddress {
    let instance_id = app
        .harness()
        .component_instance_id()
        .expect("the Browser component");
    let published = app.harness().resources();
    let address = find(&published, "browser.page")
        .and_then(|page| page.action_address(action))
        .unwrap_or_else(|| panic!("the page publishes no {action}"));
    ui::bound_dispatch::BoundActionAddress {
        instance_id,
        address,
    }
}

/// How a page action ended, read as the answer or the refusal a step expects.
trait Ended {
    /// What it answered; `why` it must have.
    fn answer(self, why: &str) -> serde_json::Value;
    /// What it was refused with; `why` it must have been.
    fn refusal(self, why: &str) -> ActionFailure;
}

impl Ended for InvocationUpdate {
    fn answer(self, why: &str) -> serde_json::Value {
        match self {
            Self::Succeeded { output, .. } => output.value.expect("a page action answers a value"),
            other => panic!("{why}: {other:?}"),
        }
    }

    fn refusal(self, why: &str) -> ActionFailure {
        match self {
            Self::Failed { error, .. } => error,
            other => panic!("{why}: {other:?}"),
        }
    }
}

/// Run the page's published action `action` for the agent, as the host runs
/// an invocation it admitted, and wait for how it ends.
fn run(
    app: &mut TestApp<BrowserShell>,
    action: &str,
    arguments: &serde_json::Value,
) -> InvocationUpdate {
    let address = address(app, action);
    let (invocation, started) = app
        .harness()
        .start_bound_job_for(
            Some(agent()),
            &address,
            arguments.as_object().expect("arguments are an object"),
            true,
        )
        .unwrap_or_else(|error| panic!("{action} was not admitted: {error:?}"));
    assert!(
        started.completed().is_none(),
        "{action} answers once the page has"
    );
    app.harness()
        .wait_until(ui::time::Duration::from_secs(30), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        });
    let updates = app.harness().job_updates(invocation).to_vec();
    // What the answer taught reaches the document before the next step.
    app.settle();
    updates
        .into_iter()
        .find(InvocationUpdate::is_terminal)
        .unwrap_or_else(|| panic!("{action} never answered"))
}

/// What the door refuses `arguments` for `action` with before anything
/// runs, if it does.
fn refused_at_the_door(
    app: &mut TestApp<BrowserShell>,
    action: &str,
    arguments: &serde_json::Value,
) -> Option<DoorError> {
    let address = address(app, action);
    app.harness()
        .start_bound_job_for(
            Some(agent()),
            &address,
            arguments.as_object().expect("arguments are an object"),
            true,
        )
        .err()
}

/// Read the page's forms as the agent does: the engine answers `forms`.
fn read_forms(
    engine: &TestEngine,
    app: &mut TestApp<BrowserShell>,
    url: &str,
    forms: &[serde_json::Value],
) {
    let count = forms.len();
    engine.push_page_reply(Ok(reply(
        url,
        &format!("{count} forms"),
        serde_json::json!({ "forms": forms }),
    )));
    run(app, "read-page-forms", &serde_json::json!({})).answer("the page's forms are read");
}

/// Who asked the engine for what on the page, in order.
fn asked(engine: &TestEngine) -> Vec<(Option<String>, PageRequest)> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::Page { actor, request, .. } => {
                Some((actor.map(|actor| actor.as_str().to_owned()), request))
            }
            _ => None,
        })
        .collect()
}

/// The request a form action makes of the engine for the agent.
fn fill(
    signature: &str,
    answers: &serde_json::Value,
    submit: FormSubmit,
) -> (Option<String>, PageRequest) {
    (
        Some("agent:ada".to_owned()),
        PageRequest::FillForm {
            form: signature.into(),
            answers: answers.as_object().expect("answers are an object").clone(),
            submit,
            cross_origin: false,
        },
    )
}

/// What the engine reports of a form answered whole.
fn placed(signature: &str, names: &[&str], submitted: bool) -> serde_json::Value {
    serde_json::json!({
        "form": "form1", "signature": signature,
        "results": names.iter().map(|name| serde_json::json!({"name": name, "ok": true}))
            .collect::<Vec<_>>(),
        "set": names.iter().map(|name| serde_json::json!({"name": name, "value": "…"}))
            .collect::<Vec<_>>(),
        "submitted": submitted, "now_asks": [], "unanswered": [],
    })
}

fn remembered(app: &TestApp<BrowserShell>, signature: &str) -> Option<RememberedForm> {
    crate::site_memory::get(&app.state().site_forms, SITE, signature).cloned()
}

/// On a first visit nothing is known of the page. Once an agent reads its
/// forms the form is an action of the page, named after it, with one typed
/// parameter per question; running it answers the form through the engine
/// under the agent's name and reports each answer as the engine did.
#[test]
fn a_read_form_is_a_typed_action_of_the_page_and_running_it_answers_the_form() {
    let (engine, mut app) = open(APPLY);
    assert!(
        form_actions(&mut app).is_empty(),
        "an unread page of an unknown site publishes no form"
    );
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-5f1c2a9b", "send-form-5f1c2a9b"]);
    let answer = &actions[0];
    assert_eq!(answer.label, "Apply for a visitor visa");
    assert_eq!(
        typed(answer),
        [
            ("full_name".to_owned(), ParamType::Text, true),
            ("date_of_birth_day".to_owned(), ParamType::Number, false),
            ("arrival".to_owned(), ParamType::Text, true),
            ("nationality".to_owned(), ParamType::Text, false),
            ("languages".to_owned(), ParamType::Array, false),
            ("i_agree".to_owned(), ParamType::Boolean, true),
        ]
    );
    assert_eq!(answer.params()[3].choices, ["Kenya", "Other"]);
    assert!(answer.contract.availability.is_available());
    assert!(
        answer.reaches_people() && answer.reaches_agents(),
        "the palette, scripts and agents all list it"
    );
    assert_eq!(actions[1].label, "Send: Apply for a visitor visa");

    // Typed: what a question does not take never reaches the page.
    for wrong in [
        serde_json::json!({"full_name": "Ada", "arrival": "next week", "i_agree": true}),
        serde_json::json!({"full_name": "Ada", "arrival": "2027-02-03", "i_agree": "yes"}),
        serde_json::json!({"full_name": "Ada", "arrival": "2027-02-03", "i_agree": true,
            "nationality": "Mars"}),
        serde_json::json!({"full_name": "Ada"}),
    ] {
        assert!(
            refused_at_the_door(&mut app, "form-5f1c2a9b", &wrong).is_some(),
            "{wrong}"
        );
    }
    let before = asked(&engine).len();

    // The engine's own report of each answer is the action's answer: what
    // was placed, what was not and why, with the ref its own action takes.
    let report = serde_json::json!({
        "form": "form1", "signature": VISA,
        "results": [
            {"name": "Full name", "ok": true},
            {"name": "Arrival", "ok": false, "code": "rejected",
                "message": "The page did not take the answer to \"Arrival\"", "element": "d1e4"},
            {"name": "Nationality", "ok": true},
            {"name": "I agree", "ok": true},
        ],
        "set": [
            {"name": "Full name", "value": "Ada Lovelace"},
            {"name": "Nationality", "value": "Other"},
            {"name": "I agree", "value": "yes"},
        ],
        "submitted": false, "now_asks": [], "unanswered": ["Arrival", "Passcode"],
    });
    engine.push_page_reply(Ok(PageReply {
        navigation: PageNavigation::Possible,
        ..reply(
            APPLY,
            "Answered 3 of 4 questions of form1 \"Apply for a visitor visa\"",
            report.clone(),
        )
    }));
    let answered = run(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({
            "full_name": "Ada Lovelace", "arrival": "2027-02-03", "i_agree": true,
            "nationality": "Other",
        }),
    )
    .answer("the form is answered");
    assert_eq!(answered["data"], report);
    assert_eq!(answered["data"]["results"][1]["code"], "rejected");
    assert_eq!(answered["data"]["results"][1]["element"], "d1e4");
    assert_eq!(answered["provenance"], "web-content");
    assert_eq!(
        asked(&engine)[before..],
        [fill(
            VISA,
            &serde_json::json!({
                "Full name": "Ada Lovelace", "Arrival": "2027-02-03", "I agree": true,
                "Nationality": "Other",
            }),
            FormSubmit::No,
        )],
        "the form is named by its signature and answered by the names it asks under, \
         under the agent's own name, and not sent"
    );
}

/// A secret is never a parameter of the form's action, which says where it
/// goes instead; and what memory keeps of the form is its shape: no value a
/// field held, typed by a person or placed by an agent, and of the secret
/// only that the question is one.
#[test]
fn a_secret_is_no_parameter_and_nothing_a_field_held_is_remembered() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);
    let actions = form_actions(&mut app);
    let answer = &actions[0];
    assert!(
        !answer
            .params()
            .iter()
            .any(|param| param.name.contains("pass") || param.ty == ParamType::Secret),
        "{:?}",
        typed(answer)
    );
    let description = answer
        .description
        .clone()
        .expect("the action says what it does");
    assert!(
        description.contains("\"Passcode\" is a secret and takes fill-page-secret"),
        "{description}"
    );
    let all = serde_json::json!({
        "full_name": "Ada Lovelace", "arrival": "2027-02-03", "i_agree": true,
        "passcode": "open-sesame",
    });
    assert!(
        refused_at_the_door(&mut app, "form-5f1c2a9b", &all).is_some(),
        "a secret given to the form's action never reaches the page"
    );
    assert_eq!(asked(&engine).len(), 1, "only the read reached the page");

    engine.push_page_reply(Ok(reply(
        APPLY,
        "Answered 2 of 2 questions",
        placed(VISA, &["Full name", "Arrival"], false),
    )));
    run(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({"full_name": "Grace Hopper", "arrival": "2027-02-03",
            "i_agree": true}),
    )
    .answer("the form is answered");

    let kept = remembered(&app, VISA).expect("the form is remembered");
    assert_eq!(kept.label, "Apply for a visitor visa");
    assert_eq!(kept.pages, ["/apply"]);
    assert_eq!(kept.submit, ["Send application"]);
    let secret = kept
        .questions
        .iter()
        .find(|question| question.name == "Passcode")
        .expect("that the form asks for a secret is its shape");
    assert_eq!(
        *secret,
        RememberedQuestion {
            name: "Passcode".into(),
            kind: "secret".into(),
            choices: Vec::new(),
            required: true,
            fallback: "secret_field".into(),
        }
    );
    let whole = serde_json::to_string(&app.state().site_forms).expect("memory is plain data");
    for held in [
        "Ada Lovelace",
        "Grace Hopper",
        "2027-02-03",
        "open-sesame",
        "characters hidden",
        "d1e",
    ] {
        assert!(!whole.contains(held), "memory holds {held}: {whole}");
    }
}

/// On a later visit the form's action is published at once from memory,
/// before anything reads the page, with the parameters the read gave it, and
/// runs against the page by the remembered signature. Another page of the
/// site publishes none of it, and coming back to the page does.
#[test]
fn a_later_visit_publishes_the_forms_action_before_any_read() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);
    let first = form_actions(&mut app);

    let (engine, mut app) = visit_again(app, "https://visa.test/apply?returning=1");
    assert!(asked(&engine).is_empty(), "nothing has read this page");
    let again = form_actions(&mut app);
    assert_eq!(ids(&again), ids(&first));
    assert_eq!(typed(&again[0]), typed(&first[0]));
    assert_eq!(again[0].params()[3].choices, ["Kenya", "Other"]);
    assert_eq!(again[0].label, "Apply for a visitor visa");
    let description = again[0].description.clone().expect("a description");
    assert!(
        description.contains("remembered of this site") && description.contains("form_changed"),
        "{description}"
    );
    assert!(asked(&engine).is_empty(), "publishing it read nothing");

    engine.push_page_reply(Ok(reply(
        APPLY,
        "Answered 3 of 3 questions",
        placed(VISA, &["Full name", "Arrival", "I agree"], false),
    )));
    let answered = run(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({"full_name": "Ada Lovelace", "arrival": "2027-02-03",
            "i_agree": true}),
    )
    .answer("the remembered form is still the page's");
    assert_eq!(answered["data"]["results"][2]["ok"], true);
    assert_eq!(
        asked(&engine),
        [fill(
            VISA,
            &serde_json::json!({"Full name": "Ada Lovelace", "Arrival": "2027-02-03",
                "I agree": true}),
            FormSubmit::No,
        )]
    );

    // The actions follow the page: another page of the site has none, and
    // the page they belong to has them again, unread.
    engine.send(WebViewCommand::Navigate("https://visa.test/contact".into()));
    assert!(form_actions(&mut app).is_empty());
    engine.send(WebViewCommand::Navigate(APPLY.into()));
    assert_eq!(ids(&form_actions(&mut app)), ids(&first));
    assert_eq!(asked(&engine).len(), 1, "and still nothing read the page");
}

/// A remembered form the page no longer asks is refused when its action is
/// run, with a code that says the form changed and to read the page again:
/// nothing but the engine's own check of the signature reached the page.
/// Reading the page then publishes the form as it is and updates memory.
#[test]
fn a_changed_form_is_refused_from_memory_and_replaced_once_the_page_is_read() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);

    let (engine, mut app) = visit_again(app, APPLY);
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-5f1c2a9b", "send-form-5f1c2a9b"]
    );
    // The site has changed its form since: the engine finds none of that
    // signature and answers nothing.
    engine.push_page_reply(Err(PageFailure::new(
        "no_form",
        "This page has no form \"5f1c2a9b\": it has form1 \"Apply for a visitor visa\". A \
         form named by a signature it no longer has has changed; read the page's forms again",
    )));
    let refused = run(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({"full_name": "Ada Lovelace", "arrival": "2027-02-03",
            "i_agree": true}),
    )
    .refusal("a form that changed is not answered");
    assert_eq!(refused.code, "browser.page.form_changed");
    assert!(
        refused.message.contains("\"Apply for a visitor visa\"")
            && refused.message.contains("as it was remembered")
            && refused.message.contains("Nothing was placed")
            && refused.message.contains("read-page-forms"),
        "{}",
        refused.message
    );
    assert!(!refused.retryable, "it runs once, and did not run");
    assert_eq!(
        asked(&engine).len(),
        1,
        "the one request named the form by the signature it had: no other form was touched"
    );
    assert!(
        form_actions(&mut app).is_empty(),
        "the action of a form the page does not hold is withdrawn"
    );
    assert!(
        remembered(&app, VISA).is_some(),
        "memory is corrected by a read, not by a refusal"
    );

    // The page is read: the form as it is now replaces the remembered one,
    // on the page and in memory.
    let mut changed = visa_form();
    changed["signature"] = "0badc0de".into();
    changed["fields"].as_array_mut().unwrap().push(
        serde_json::json!({"name": "Purpose of visit", "kind": "text", "required": true,
            "value": "", "element": "d1e12"}),
    );
    read_forms(&engine, &mut app, APPLY, &[changed]);
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-0badc0de", "send-form-0badc0de"]);
    assert!(
        typed(&actions[0]).contains(&("purpose_of_visit".to_owned(), ParamType::Text, true)),
        "{:?}",
        typed(&actions[0])
    );
    assert!(
        remembered(&app, VISA).is_none(),
        "the old shape is forgotten"
    );
    assert!(remembered(&app, "0badc0de").is_some());

    // And the next visit opens with the form as it is now.
    let (_, mut app) = visit_again(app, APPLY);
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-0badc0de", "send-form-0badc0de"]
    );
}

/// Two forms of one page are two actions, each with its own questions, and
/// each runs against its own form.
#[test]
fn two_forms_of_a_page_are_two_actions() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, APPLY, &[visa_form(), search_form()]);
    let actions = form_actions(&mut app);
    assert_eq!(
        ids(&actions),
        [
            "form-5f1c2a9b",
            "send-form-5f1c2a9b",
            "form-0a11ce00",
            "send-form-0a11ce00"
        ]
    );
    assert_eq!(actions[2].label, "Search this site");
    assert_eq!(
        typed(&actions[2]),
        [("words".to_owned(), ParamType::Text, false)]
    );
    let before = asked(&engine).len();
    engine.push_page_reply(Ok(reply(
        APPLY,
        "Answered 1 of 1 questions",
        placed("0a11ce00", &["Words"], false),
    )));
    run(
        &mut app,
        "form-0a11ce00",
        &serde_json::json!({"words": "consulate hours"}),
    )
    .answer("the search form is answered");
    assert_eq!(
        asked(&engine)[before..],
        [fill(
            "0a11ce00",
            &serde_json::json!({"Words": "consulate hours"}),
            FormSubmit::No,
        )]
    );
    assert!(remembered(&app, VISA).is_some() && remembered(&app, "0a11ce00").is_some());
}

/// Sending is the form's other action. In a flow of several pages memory
/// keeps which form followed which: the form read on the page a sent form
/// led to follows it, and on a later visit the first form says what the
/// site asks next.
#[test]
fn sending_is_its_own_action_and_memory_keeps_which_form_followed_which() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);
    let before = asked(&engine).len();
    engine.push_page_reply(Ok(PageReply {
        navigation: PageNavigation::Possible,
        ..reply(
            APPLY,
            "Answered 0 of 0 questions of form1; sent",
            placed(VISA, &[], true),
        )
    }));
    let sent =
        run(&mut app, "send-form-5f1c2a9b", &serde_json::json!({})).answer("the form is sent");
    assert_eq!(sent["data"]["submitted"], true);
    assert_eq!(
        asked(&engine)[before..],
        [fill(VISA, &serde_json::json!({}), FormSubmit::First)],
        "sending answers nothing: it sends the form as it stands, with its first button"
    );

    // The site answers with its next page, which asks for the payment.
    let pay = "https://visa.test/pay";
    engine.send(WebViewCommand::Navigate(pay.into()));
    app.settle();
    let payment = serde_json::json!({
        "form": "form1", "label": "Pay the fee", "method": "POST", "action": "/pay",
        "signature": "c0ffee00", "submit": ["Pay"],
        "fields": [{"name": "Card number", "kind": "secret", "required": true, "value": "",
            "element": "d2e2"}],
    });
    read_forms(&engine, &mut app, pay, &[payment]);
    assert_eq!(
        remembered(&app, "c0ffee00").map(|form| form.follows),
        Some(VISA.to_owned())
    );

    let (engine, mut app) = visit_again(app, APPLY);
    let actions = form_actions(&mut app);
    let description = actions[0].description.clone().expect("a description");
    assert!(
        description.contains("After this form was sent, the site asked: \"Pay the fee\""),
        "{description}"
    );
    engine.push_page_reply(Ok(reply(APPLY, "sent", placed(VISA, &[], true))));
    let sent = run(&mut app, "send-form-5f1c2a9b", &serde_json::json!({}))
        .answer("the remembered form is sent");
    assert_eq!(
        sent["data"]["then"],
        serde_json::json!([{"label": "Pay the fee", "signature": "c0ffee00",
            "action": "form-c0ffee00"}])
    );
}

/// An answer that makes the form ask more withdraws its action, and says
/// so, until the page is read; and a field the typed path refused for what
/// it is (an editor written by typing) is no parameter from then on, on
/// this visit and the next.
#[test]
fn what_running_a_form_teaches_changes_its_action() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);
    engine.push_page_reply(Ok(reply(
        APPLY,
        "Answered 1 of 2 questions of form1; the form now also asks \"Which country?\"",
        serde_json::json!({
            "form": "form1", "signature": VISA,
            "results": [
                {"name": "Full name", "ok": false, "code": "needs_typing",
                    "message": "\"Full name\" is written by typing; fill d1e2 on its own",
                    "element": "d1e2"},
                {"name": "Nationality", "ok": true},
            ],
            "set": [{"name": "Nationality", "value": "Other"}],
            "submitted": false, "now_asks": ["Which country?"], "unanswered": [],
        }),
    )));
    let answered = run(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({"full_name": "Ada Lovelace", "arrival": "2027-02-03",
            "i_agree": true, "nationality": "Other"}),
    )
    .answer("one answer was placed");
    assert_eq!(answered["data"]["results"][0]["code"], "needs_typing");
    let message = answered["message"].as_str().expect("a message");
    assert!(
        message.contains("its action is withdrawn until the page's forms are read again"),
        "{message}"
    );
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-5f1c2a9b", "send-form-5f1c2a9b"]);
    assert!(
        actions
            .iter()
            .all(|action| !action.contract.availability.is_available()),
        "the form asks something else now: its actions say so until it is read"
    );
    let withdrawn = refused_at_the_door(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({"arrival": "2027-02-03", "i_agree": true}),
    );
    assert!(
        matches!(&withdrawn, Some(DoorError::Unavailable { code, .. }) if code == "form_changed"),
        "{withdrawn:?}"
    );

    // Read again, the form has the shape it grew into; a later visit opens
    // with the shape the page opens with, without the field that needs typing.
    let mut grown = visa_form();
    grown["signature"] = "0badc0de".into();
    read_forms(&engine, &mut app, APPLY, &[grown]);
    assert_eq!(
        remembered(&app, "0badc0de").map(|form| form.grew_from),
        Some(VISA.to_owned())
    );
    let (_, mut app) = visit_again(app, APPLY);
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-5f1c2a9b", "send-form-5f1c2a9b"]);
    assert!(
        !typed(&actions[0])
            .iter()
            .any(|(name, _, _)| name == "full_name"),
        "{:?}",
        typed(&actions[0])
    );
    let description = actions[0].description.clone().expect("a description");
    assert!(
        description.contains("\"Full name\" is written by typing and takes fill-page"),
        "{description}"
    );
}

/// In the person's own window a page holds their sign-ins, and the answers
/// a question offers may be their own: the page's action offers them, and
/// memory, which the workspace shares, does not keep them.
#[test]
fn the_answers_a_signed_in_page_offers_are_not_remembered() {
    let (engine, mut app) = launch(&[APPLY]);
    read_forms(&engine, &mut app, APPLY, &[visa_form()]);
    let actions = form_actions(&mut app);
    assert_eq!(actions[0].params()[3].choices, ["Kenya", "Other"]);
    let kept = remembered(&app, VISA).expect("the form's shape is remembered");
    assert!(
        kept.questions
            .iter()
            .all(|question| question.choices.is_empty()),
        "{kept:?}"
    );
    assert_eq!(kept.questions.len(), 7);
}

/// Memory is the document's: it changes through the document's own action,
/// which takes a form's shape and nothing else, and two devices that each
/// remembered a form hold both once they have synced.
#[test]
fn memory_is_the_documents_and_syncs_like_the_rest_of_it() {
    let form = |signature: &str, label: &str| RememberedForm {
        signature: signature.into(),
        label: label.into(),
        questions: vec![RememberedQuestion {
            name: "Name".into(),
            kind: "text".into(),
            ..RememberedQuestion::default()
        }],
        ..RememberedForm::default()
    };
    let remember = |origin: &str, page: &str, form: RememberedForm| {
        Browser::REMEMBER_FORMS.with(browser::RememberForms {
            origin: origin.into(),
            page: page.into(),
            forms: vec![form],
            whole: true,
            at: 1,
        })
    };
    let mut ours = Replica::<Browser>::new();
    let mut theirs = ours.fork();
    ours.apply(remember(SITE, "/apply", form("aaaa1111", "Apply")))
        .expect("a form's shape is remembered");
    theirs
        .apply(remember(SITE, "/contact", form("bbbb2222", "Contact")))
        .expect("a form's shape is remembered");
    assert!(
        ours.apply(remember("mailto:a@b", "/apply", form("cccc3333", "")))
            .is_err(),
        "a site is an http(s) origin"
    );
    assert!(
        ours.apply(remember(SITE, "/apply", form("not a signature", "")))
            .is_err()
    );
    ours.sync_with(&mut theirs).expect("the two devices sync");
    let (ours, theirs) = (ours.state().unwrap(), theirs.state().unwrap());
    assert_eq!(ours.site_forms, theirs.site_forms);
    let known: Vec<&str> = ours.site_forms.keys().map(String::as_str).collect();
    assert_eq!(
        known,
        ["https://visa.test aaaa1111", "https://visa.test bbbb2222"]
    );
}
