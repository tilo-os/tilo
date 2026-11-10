//! A page's forms as the page's own actions, and site memory, as an agent
//! and the person meet them: Browser in its real app host over the
//! deterministic test engine, its published actions invoked the way the host
//! invokes them for an agent it admitted, and its document read back as the
//! workspace keeps it.
//!
//! The engine's page holds what a test says it holds ([`page_holds`]): every
//! read of its forms answers that, the window's own as an agent's.

use std::sync::Arc;

use ui::framework::testing::TestApp;
use ui::tilo_actions::{
    ActionFailure, ActionInvoker, ActionSpec, ActorKind, DoorError, InvocationUpdate, ParamType,
};
use ui::tilo_resource::Replica;
use ui::webview::{FormSubmit, PageNavigation, PageRecordNote, WebViewEngine as _};

use super::*;
use crate::effects::BrowserEffect;
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

/// The engine's page holds `forms` from now on: what every read of its
/// forms answers, whoever reads, until the page holds others.
fn page_holds(engine: &TestEngine, forms: &[serde_json::Value]) {
    engine.set_page_data("forms", serde_json::json!({ "forms": forms }));
}

/// Run the window's frames until what one frame started has reached the
/// next: an answer of the engine heard, a change of the document shown.
fn settle(app: &mut TestApp<BrowserShell>) {
    for _ in 0..3 {
        app.settle();
    }
}

/// A Browser window opened with `arguments`, over an engine of its own whose
/// page holds `forms`.
fn launch_on(
    arguments: &[&str],
    forms: &[serde_json::Value],
) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    let engine = TestEngine::new();
    page_holds(&engine, forms);
    crate::browser_widget::test_window_engine::install(engine.clone());
    let mut app = TestApp::<BrowserShell>::launched(arguments).expect("Browser opens");
    engine.push_event(WebViewEvent::LoadFinished);
    settle(&mut app);
    (engine, app)
}

/// A Browser window opened with `arguments` on a page that asks nothing.
fn launch(arguments: &[&str]) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    launch_on(arguments, &[])
}

/// A window an agent opened on `url`, whose page asks nothing yet.
fn open(url: &str) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    open_on(url, &[])
}

/// A window an agent opened on `url`, whose page holds `forms`.
fn open_on(url: &str, forms: &[serde_json::Value]) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    launch_on(&[ui::webview::VISITOR_WINDOW_FLAG, url], forms)
}

/// Close the window and open another on `url` over a new engine whose page
/// holds `forms`, on the same profile and document: a later visit.
fn visit_again(
    app: TestApp<BrowserShell>,
    url: &str,
    forms: &[serde_json::Value],
) -> (Arc<TestEngine>, TestApp<BrowserShell>) {
    let engine = TestEngine::new();
    page_holds(&engine, forms);
    crate::browser_widget::test_window_engine::install(engine.clone());
    let mut app = app
        .relaunch(&[ui::webview::VISITOR_WINDOW_FLAG, url])
        .expect("Browser opens again");
    engine.push_event(WebViewEvent::LoadFinished);
    settle(&mut app);
    // A window opened again may show the page it showed last: the visit is
    // to `url`, so it goes there as following a link does.
    if shown(&mut app) != url {
        engine.send(WebViewCommand::Navigate(url.into()));
        settle(&mut app);
    }
    assert_eq!(shown(&mut app), url);
    (engine, app)
}

/// The window goes to `url`, whose page takes its time: the engine shows the
/// address and has not finished loading it. Answers what the engine says
/// once it has ([`finish_visit`]).
fn begin_visit(
    engine: &TestEngine,
    app: &mut TestApp<BrowserShell>,
    url: &str,
) -> Vec<WebViewEvent> {
    engine.send(WebViewCommand::Navigate(url.into()));
    let (finished, begun): (Vec<_>, Vec<_>) =
        engine.drain_events().into_iter().partition(|event| {
            matches!(
                event,
                WebViewEvent::LoadFinished | WebViewEvent::LoadProgress(_)
            )
        });
    for event in begun {
        engine.push_event(event);
    }
    engine.push_event(WebViewEvent::LoadProgress(0.4));
    settle(app);
    assert_eq!(shown(app), url);
    finished
}

/// The page [`begin_visit`] went to has finished loading.
fn finish_visit(engine: &TestEngine, app: &mut TestApp<BrowserShell>, finished: Vec<WebViewEvent>) {
    for event in finished {
        engine.push_event(event);
    }
    settle(app);
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
    settle(app);
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

/// The page comes to hold `forms`, as a page's own script draws a form
/// after it loaded, and the agent reads the page's forms.
fn read_forms(engine: &TestEngine, app: &mut TestApp<BrowserShell>, forms: &[serde_json::Value]) {
    page_holds(engine, forms);
    run(app, "read-page-forms", &serde_json::json!({})).answer("the page's forms are read");
}

/// Every request the engine was sent for the page, with who asked it.
fn requests(engine: &TestEngine) -> Vec<(Option<String>, PageRequest)> {
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

/// The window's own read of its page's forms: the read `read-page-forms`
/// makes, asked by nobody the tab names.
fn own_read() -> (Option<String>, PageRequest) {
    (None, PageRequest::Forms { frame: None })
}

/// How often the window read its page's forms itself.
fn own_reads(engine: &TestEngine) -> usize {
    requests(engine)
        .into_iter()
        .filter(|request| *request == own_read())
        .count()
}

/// What the tab's actors asked the engine for on the page, in order: every
/// request but the window's own reads.
fn asked(engine: &TestEngine) -> Vec<(Option<String>, PageRequest)> {
    requests(engine)
        .into_iter()
        .filter(|request| *request != own_read())
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

/// A page that asks nothing publishes no form. Once it holds one and an
/// agent reads its forms, the form is an action of the page, named after
/// it, with one typed parameter per question; running it answers the form
/// through the engine under the agent's name and reports each answer as the
/// engine did.
#[test]
fn a_read_form_is_a_typed_action_of_the_page_and_running_it_answers_the_form() {
    let (engine, mut app) = open(APPLY);
    assert!(
        form_actions(&mut app).is_empty(),
        "a page of an unknown site that asks nothing publishes no form"
    );
    read_forms(&engine, &mut app, &[visa_form()]);
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
    read_forms(&engine, &mut app, &[visa_form()]);
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
/// before anything reads the page, with the parameters the first visit gave
/// it. Once the page has loaded the window reads it and the action is the
/// read page's own, and it runs against the page by its signature. Another
/// page of the site publishes none of it.
#[test]
fn a_later_visit_publishes_the_forms_action_before_any_read() {
    let (_, mut app) = open_on(APPLY, &[visa_form()]);
    let first = form_actions(&mut app);
    assert_eq!(ids(&first), ["form-5f1c2a9b", "send-form-5f1c2a9b"]);

    let (engine, mut app) = visit_again(app, APPLY, &[visa_form()]);
    // The actions follow the page: another page of the site has none.
    page_holds(&engine, &[]);
    engine.send(WebViewCommand::Navigate("https://visa.test/help".into()));
    assert!(form_actions(&mut app).is_empty());

    // The form's page again, still loading: nothing has read it.
    let reads = own_reads(&engine);
    let finished = begin_visit(&engine, &mut app, "https://visa.test/apply?returning=1");
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
    assert_eq!(own_reads(&engine), reads, "publishing it read nothing");
    assert!(asked(&engine).is_empty(), "and no agent read the page");

    // The page has loaded and still holds the form: the window reads it,
    // and the action is the read page's own.
    page_holds(&engine, &[visa_form()]);
    finish_visit(&engine, &mut app, finished);
    assert_eq!(own_reads(&engine), reads + 1);
    let read = form_actions(&mut app);
    assert_eq!(ids(&read), ids(&first));
    let description = read[0].description.clone().expect("a description");
    assert!(
        !description.contains("remembered of this site"),
        "{description}"
    );

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
    .answer("the form is still the page's");
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
}

/// A site that changed its form since the last visit has the form as it is
/// now published by the window's own read, in place of the remembered one,
/// on the page and in memory: no agent meets the old action or reads the
/// page.
#[test]
fn a_form_the_site_changed_is_replaced_by_the_windows_own_read() {
    let (_, app) = open_on(APPLY, &[visa_form()]);
    assert!(remembered(&app, VISA).is_some());

    let mut changed = visa_form();
    changed["signature"] = "0badc0de".into();
    changed["fields"].as_array_mut().unwrap().push(
        serde_json::json!({"name": "Purpose of visit", "kind": "text", "required": true,
            "value": "", "element": "d1e12"}),
    );
    let (engine, mut app) = visit_again(app, APPLY, &[changed]);
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-0badc0de", "send-form-0badc0de"]);
    assert!(
        typed(&actions[0]).contains(&("purpose_of_visit".to_owned(), ParamType::Text, true)),
        "{:?}",
        typed(&actions[0])
    );
    assert!(asked(&engine).is_empty(), "no agent read the page");
    assert!(
        remembered(&app, VISA).is_none(),
        "the old shape is forgotten"
    );
    assert!(remembered(&app, "0badc0de").is_some());
}

/// A form the page's own script replaced after the page was read is not the
/// one its action was made for. Running that action is refused with a code
/// that says the form changed: nothing but the engine's own check of the
/// signature reached the page. The window then reads the page again by
/// itself and publishes the form as it is.
#[test]
fn a_form_that_changed_under_its_action_is_refused_and_the_page_read_again() {
    let (engine, mut app) = open_on(APPLY, &[visa_form()]);
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-5f1c2a9b", "send-form-5f1c2a9b"]
    );
    // The page draws another form in its place. Nothing tells the window.
    let mut changed = visa_form();
    changed["signature"] = "0badc0de".into();
    page_holds(&engine, &[changed]);
    let reads = own_reads(&engine);
    // The engine finds no form of the old signature and answers nothing.
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
            && refused.message.contains("as it was read")
            && refused.message.contains("Nothing was placed")
            && refused.message.contains("reads its forms again by itself")
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

    // The window read the page again: the form as it is now replaces the
    // one the action was made for, on the page and in memory.
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-0badc0de", "send-form-0badc0de"]);
    assert!(
        actions
            .iter()
            .all(|action| action.contract.availability.is_available())
    );
    assert_eq!(own_reads(&engine), reads + 1, "one read, and no agent's");
    assert_eq!(asked(&engine).len(), 1);
    // The page opened with the first shape and will again: memory keeps it
    // as the shape the other one grew from.
    assert!(remembered(&app, VISA).is_some());
    assert_eq!(
        remembered(&app, "0badc0de").map(|form| form.grew_from),
        Some(VISA.to_owned())
    );
}

/// Two forms of one page are two actions, each with its own questions, and
/// each runs against its own form.
#[test]
fn two_forms_of_a_page_are_two_actions() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, &[visa_form(), search_form()]);
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
/// keeps which form followed which: the form the window reads on the page a
/// sent form led to follows it, and on a later visit the first form says
/// what the site asks next.
#[test]
fn sending_is_its_own_action_and_memory_keeps_which_form_followed_which() {
    let (engine, mut app) = open(APPLY);
    read_forms(&engine, &mut app, &[visa_form()]);
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

    // The site answers with its next page, which asks for the payment. The
    // window reads it once it has loaded: no agent has to.
    let pay = "https://visa.test/pay";
    let payment = serde_json::json!({
        "form": "form1", "label": "Pay the fee", "method": "POST", "action": "/pay",
        "signature": "c0ffee00", "submit": ["Pay"],
        "fields": [{"name": "Card number", "kind": "secret", "required": true, "value": "",
            "element": "d2e2"}],
    });
    let read_by_agents = asked(&engine).len();
    page_holds(&engine, std::slice::from_ref(&payment));
    engine.send(WebViewCommand::Navigate(pay.into()));
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-c0ffee00", "send-form-c0ffee00"]
    );
    assert_eq!(asked(&engine).len(), read_by_agents);
    assert_eq!(
        remembered(&app, "c0ffee00").map(|form| form.follows),
        Some(VISA.to_owned())
    );

    // A later visit: the window opens on the payment page it showed last,
    // which asks for the payment as before, and goes back to the first form.
    let (engine, mut app) = visit_again(app, pay, &[payment]);
    page_holds(&engine, &[visa_form()]);
    engine.send(WebViewCommand::Navigate(APPLY.into()));
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-5f1c2a9b", "send-form-5f1c2a9b"]);
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

/// An answer that makes the form ask more is answered as it always was,
/// and the form gets its new action by itself: the window reads the page
/// again and publishes the form as it is now, without any agent's read. A
/// field the typed path refused for what it is (an editor written by
/// typing) is no parameter from then on, on this visit and the next.
#[test]
fn a_form_that_grows_gets_its_new_action_without_an_agents_read() {
    let (engine, mut app) = open_on(APPLY, &[visa_form()]);
    assert!(asked(&engine).is_empty(), "the window read the page");
    // Answering "Other" makes the page ask which country: from then on it
    // holds the form in the shape it grew into.
    let mut grown = visa_form();
    grown["signature"] = "0badc0de".into();
    grown["fields"].as_array_mut().unwrap().push(
        serde_json::json!({"name": "Which country?", "kind": "text", "required": true,
            "value": "", "element": "d1e12"}),
    );
    page_holds(&engine, &[grown]);
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
    let reads = own_reads(&engine);
    let answered = run(
        &mut app,
        "form-5f1c2a9b",
        &serde_json::json!({"full_name": "Ada Lovelace", "arrival": "2027-02-03",
            "i_agree": true, "nationality": "Other"}),
    )
    .answer("one answer was placed");
    assert_eq!(answered["data"]["results"][0]["code"], "needs_typing");
    assert_eq!(
        answered["data"]["now_asks"],
        serde_json::json!(["Which country?"])
    );
    let message = answered["message"].as_str().expect("a message");
    assert!(
        message.contains("the page reads its forms again by itself"),
        "{message}"
    );

    // The window read the page again by itself: the form as it is now is an
    // action of the page, with the question the answer opened.
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-0badc0de", "send-form-0badc0de"]);
    assert!(
        actions
            .iter()
            .all(|action| action.contract.availability.is_available()),
        "the grown form's actions can be run"
    );
    assert!(
        typed(&actions[0]).contains(&("which_country".to_owned(), ParamType::Text, true)),
        "{:?}",
        typed(&actions[0])
    );
    assert_eq!(
        own_reads(&engine),
        reads + 1,
        "one read of the window's own"
    );
    assert_eq!(
        asked(&engine).len(),
        1,
        "the agent asked the engine for its one answer and never read the page"
    );
    assert_eq!(
        remembered(&app, "0badc0de").map(|form| form.grew_from),
        Some(VISA.to_owned())
    );
    // Nothing reads on: the page is not read again until it has a reason.
    settle(&mut app);
    assert_eq!(own_reads(&engine), reads + 1);

    // A later visit opens with the shape the page opens with, without the
    // field that needs typing.
    let (_, mut app) = visit_again(app, APPLY, &[visa_form()]);
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
    assert!(
        remembered(&app, "0badc0de").is_some(),
        "the shape the form grows into is still remembered"
    );
}

/// In the person's own window a page holds their sign-ins, and the answers
/// a question offers may be their own: the page's action offers them, and
/// memory, which the workspace shares, does not keep them.
#[test]
fn the_answers_a_signed_in_page_offers_are_not_remembered() {
    let (engine, mut app) = launch(&[APPLY]);
    read_forms(&engine, &mut app, &[visa_form()]);
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

/// A first visit has its forms as actions without any agent's read: the
/// window reads the page itself once it has loaded, publishes each form as
/// a typed action and remembers its shape. The read is the tab's unnamed
/// actor's, and the action it publishes runs for an agent like any other.
#[test]
fn a_first_visit_publishes_a_forms_action_with_no_agent_read() {
    let (engine, mut app) = open_on(APPLY, &[visa_form(), search_form()]);
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
    assert_eq!(actions[0].label, "Apply for a visitor visa");
    assert_eq!(
        typed(&actions[0]),
        [
            ("full_name".to_owned(), ParamType::Text, true),
            ("date_of_birth_day".to_owned(), ParamType::Number, false),
            ("arrival".to_owned(), ParamType::Text, true),
            ("nationality".to_owned(), ParamType::Text, false),
            ("languages".to_owned(), ParamType::Array, false),
            ("i_agree".to_owned(), ParamType::Boolean, true),
        ]
    );
    assert_eq!(actions[0].params()[3].choices, ["Kenya", "Other"]);
    assert!(
        actions
            .iter()
            .all(|action| action.contract.availability.is_available())
    );
    let all = requests(&engine);
    assert!(
        !all.is_empty() && all.iter().all(|request| *request == own_read()),
        "the engine was asked for the page's forms by the window alone: {all:?}"
    );
    assert!(asked(&engine).is_empty(), "no agent read the page");
    assert!(remembered(&app, VISA).is_some() && remembered(&app, "0a11ce00").is_some());

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
    .answer("the action the window's read published answers the form");
    assert_eq!(
        asked(&engine),
        [fill(
            "0a11ce00",
            &serde_json::json!({"Words": "consulate hours"}),
            FormSubmit::No,
        )]
    );
}

/// A page without forms costs one read: the window reads it once when it
/// has loaded and not again, however long the page stays in front.
#[test]
fn a_page_without_forms_is_read_once() {
    let (engine, mut app) = open(APPLY);
    let before = own_reads(&engine);
    engine.send(WebViewCommand::Navigate("https://visa.test/contact".into()));
    settle(&mut app);
    assert_eq!(own_reads(&engine), before + 1, "read once it has loaded");
    assert!(form_actions(&mut app).is_empty());
    // A minute on, past every wait the window keeps for its page.
    let later = ui::time::now() + ui::time::Duration::from_secs(60);
    ui::time::testing::with_monotonic_time(later, || settle(&mut app));
    settle(&mut app);
    assert_eq!(own_reads(&engine), before + 1, "and never again");
    assert!(app.state().site_forms.is_empty(), "nothing to remember");
}

/// A reload at the same address reads the page again: what it holds after
/// loading anew is what it publishes.
#[test]
fn a_reload_at_the_same_address_reads_the_page_again() {
    let (engine, mut app) = open_on(APPLY, &[visa_form()]);
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-5f1c2a9b", "send-form-5f1c2a9b"]
    );
    let reads = own_reads(&engine);
    page_holds(&engine, &[visa_form(), search_form()]);
    settle(&mut app);
    assert_eq!(
        own_reads(&engine),
        reads,
        "a page nothing says changed is not read again"
    );
    engine.send(WebViewCommand::Reload);
    assert_eq!(
        ids(&form_actions(&mut app)),
        [
            "form-5f1c2a9b",
            "send-form-5f1c2a9b",
            "form-0a11ce00",
            "send-form-0a11ce00"
        ]
    );
    assert_eq!(shown(&mut app), APPLY);
    assert_eq!(own_reads(&engine), reads + 1);
    assert!(asked(&engine).is_empty());
}

/// The window's reads take no place in the tab's record and name no actor:
/// each is the read `read-page-forms` makes, which only observes the page,
/// sent as the tab's unnamed actor, and the record holds nothing after
/// them.
#[test]
fn the_windows_reads_take_no_place_in_the_record() {
    let (engine, mut app) = open_on(APPLY, &[visa_form()]);
    engine.send(WebViewCommand::Reload);
    settle(&mut app);
    let reads = requests(&engine);
    assert!(reads.len() >= 2, "{reads:?}");
    for (actor, request) in &reads {
        assert_eq!(*actor, None, "the window reads as nobody the tab names");
        assert_eq!(*request, PageRequest::Forms { frame: None });
        assert!(
            request.only_observes(),
            "a read operates nothing, so it is never set down"
        );
    }
    let record = run(&mut app, "read-page-record", &serde_json::json!({}))
        .answer("the tab's record is read");
    assert_eq!(record["data"]["entries"], serde_json::json!([]));
    assert_eq!(record["data"]["next"], 0);
}

/// The test engine as a window's engine, answering page requests late while
/// a test says so: what it answers meanwhile reaches the window only once
/// the test lets it, after whatever else the engine said in between.
struct SlowEngine {
    engine: Arc<TestEngine>,
    /// Whether answers are held back, and the ones that are.
    held: std::sync::Mutex<(bool, Vec<WebViewEvent>)>,
}

impl SlowEngine {
    fn over(engine: &Arc<TestEngine>) -> Arc<Self> {
        Arc::new(Self {
            engine: Arc::clone(engine),
            held: std::sync::Mutex::new((false, Vec::new())),
        })
    }

    /// Hold the engine's answers back from now on, or let them through.
    fn hold_answers(&self, hold: bool) {
        self.held.lock().unwrap().0 = hold;
    }
}

impl ui::webview::WebViewEngine for SlowEngine {
    fn create_tab(&self) -> Result<SharedEngine, String> {
        self.engine.create_tab()
    }
    fn set_visible(&self, visible: bool) {
        self.engine.set_visible(visible);
    }
    fn take_new_tab_requests(&self) -> Vec<(String, bool)> {
        self.engine.take_new_tab_requests()
    }
    fn accept_popups(&self) {
        self.engine.accept_popups();
    }
    fn accept_page_dialogs(&self) {
        self.engine.accept_page_dialogs();
    }
    fn take_popups(&self) -> Vec<ui::webview::Popup> {
        self.engine.take_popups()
    }
    fn adopt_popup(&self, popup: u32) -> Result<SharedEngine, String> {
        self.engine.adopt_popup(popup)
    }
    fn measures_document(&self) -> bool {
        self.engine.measures_document()
    }
    fn send(&self, command: WebViewCommand) {
        self.engine.send(command);
    }
    fn navigate_owned(
        &self,
        request_id: u32,
        command: ui::webview::NavigationCommand,
    ) -> Result<(), String> {
        self.engine.navigate_owned(request_id, command)
    }
    fn frames(&self) -> ui::webview::FrameSlot {
        self.engine.frames()
    }
    fn drain_events(&self) -> Vec<WebViewEvent> {
        let mut events = self.engine.drain_events();
        let mut held = self.held.lock().unwrap();
        if held.0 {
            let (answers, others): (Vec<_>, Vec<_>) = events
                .into_iter()
                .partition(|event| matches!(event, WebViewEvent::PageResult { .. }));
            held.1.extend(answers);
            return others;
        }
        events.append(&mut held.1);
        events
    }
    fn supports_composition(&self) -> bool {
        self.engine.supports_composition()
    }
    fn text_field(&self) -> Option<ui::webview::TextField> {
        self.engine.text_field()
    }
}

/// The window's read is of the page it asked. An answer that arrives once
/// the window shows another page says nothing of that page: it is dropped,
/// published nowhere and not remembered.
#[test]
fn a_read_answered_after_its_page_has_gone_is_dropped() {
    let engine = TestEngine::new();
    let slow = SlowEngine::over(&engine);
    crate::browser_widget::test_window_engine::install(slow.clone());
    let mut app = TestApp::<BrowserShell>::launched(&[ui::webview::VISITOR_WINDOW_FLAG, APPLY])
        .expect("Browser opens");
    engine.push_event(WebViewEvent::LoadFinished);
    settle(&mut app);
    assert_eq!(shown(&mut app), APPLY);
    assert!(form_actions(&mut app).is_empty());

    // The page loads anew with a form. The window asks the engine for its
    // forms, and the engine takes its time to answer.
    page_holds(&engine, &[visa_form()]);
    let reads = own_reads(&engine);
    slow.hold_answers(true);
    engine.send(WebViewCommand::Reload);
    settle(&mut app);
    assert_eq!(own_reads(&engine), reads + 1);
    assert!(
        form_actions(&mut app).is_empty(),
        "the engine has not answered the read yet"
    );

    // The window goes on to a page that asks nothing, and only then does
    // the answer about the page it left arrive.
    page_holds(&engine, &[]);
    engine.send(WebViewCommand::Navigate("https://visa.test/contact".into()));
    slow.hold_answers(false);
    assert!(
        form_actions(&mut app).is_empty(),
        "the form of the page it left is no action of this one"
    );
    assert_eq!(shown(&mut app), "https://visa.test/contact");
    assert!(
        app.state().site_forms.is_empty(),
        "and nothing is remembered of a read that was dropped"
    );
    assert_eq!(
        own_reads(&engine),
        reads + 2,
        "the page it went to is read once"
    );

    // An engine that answers in time is heard: the same page, loaded anew
    // with the form, publishes it.
    page_holds(&engine, &[visa_form()]);
    engine.send(WebViewCommand::Navigate(APPLY.into()));
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-5f1c2a9b", "send-form-5f1c2a9b"]
    );
}

/// A private window reads its page like any other, so the page's forms are
/// its actions, but it writes nothing to memory and publishes nothing that
/// was remembered.
#[test]
fn a_private_window_neither_publishes_from_memory_nor_writes_to_it() {
    let help = "https://visa.test/help";
    let engine = TestEngine::new();
    page_holds(&engine, &[search_form()]);
    crate::browser_widget::test_window_engine::install(engine.clone());
    let mut app = TestApp::<BrowserShell>::launched(&["--private", help]).expect("Browser opens");
    engine.push_event(WebViewEvent::LoadFinished);
    settle(&mut app);
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-0a11ce00", "send-form-0a11ce00"],
        "what the page asks is the page's action in a private window too"
    );
    assert!(own_reads(&engine) > 0);
    assert!(
        app.state().site_forms.is_empty(),
        "a private window remembers nothing it read"
    );

    // Its document remembers a form of the site. A window that is not
    // private publishes it while the form's page loads
    // (`a_later_visit_publishes_the_forms_action_before_any_read`).
    let shape = RememberedForm {
        signature: VISA.into(),
        label: "Apply for a visitor visa".into(),
        questions: vec![RememberedQuestion {
            name: "Full name".into(),
            kind: "text".into(),
            ..RememberedQuestion::default()
        }],
        submit: vec!["Send application".into()],
        ..RememberedForm::default()
    };
    app.invoke(Browser::REMEMBER_FORMS.with(browser::RememberForms {
        origin: SITE.into(),
        page: "/apply".into(),
        forms: vec![shape],
        whole: true,
        at: 1,
    }))
    .expect("the document remembers a form");
    page_holds(&engine, &[]);
    let finished = begin_visit(&engine, &mut app, APPLY);
    assert!(
        form_actions(&mut app).is_empty(),
        "a private window publishes nothing that was remembered"
    );
    // The page has loaded and asks nothing. A window that is not private
    // would forget the form for this page now.
    finish_visit(&engine, &mut app, finished);
    assert!(form_actions(&mut app).is_empty());
    let kept = remembered(&app, VISA).expect("memory is as it was");
    assert_eq!((kept.pages, kept.seen), (vec!["/apply".to_owned()], 1));
    assert_eq!(app.state().site_forms.len(), 1);
}

/// A window whose history is paused publishes what its page asks and
/// remembers none of it.
#[test]
fn a_window_whose_history_is_paused_remembers_no_form() {
    let (engine, mut app) = open(APPLY);
    app.effect_on("browser.session", &BrowserEffect::TogglePause)
        .expect("history pauses in this window");
    page_holds(&engine, &[visa_form()]);
    engine.send(WebViewCommand::Reload);
    assert_eq!(
        ids(&form_actions(&mut app)),
        ["form-5f1c2a9b", "send-form-5f1c2a9b"]
    );
    assert!(app.state().site_forms.is_empty());
}

/// In the person's own window the page may hold their sign-ins. The
/// window's own read is nobody's the person admitted, so it publishes each
/// question without the answers it offers, which may be the person's own;
/// an agent the person let read the page gets them.
#[test]
fn the_windows_read_of_a_signed_in_page_publishes_no_offered_answers() {
    let (engine, mut app) = launch_on(&[APPLY], &[visa_form()]);
    let actions = form_actions(&mut app);
    assert_eq!(ids(&actions), ["form-5f1c2a9b", "send-form-5f1c2a9b"]);
    assert_eq!(
        typed(&actions[0])[3],
        ("nationality".to_owned(), ParamType::Text, false)
    );
    assert!(actions[0].params()[3].choices.is_empty());
    let whole = format!("{:?}", actions[0].params());
    for offered in ["Kenya", "Dutch"] {
        assert!(!whole.contains(offered), "{offered} is published: {whole}");
    }
    read_forms(&engine, &mut app, &[visa_form()]);
    let actions = form_actions(&mut app);
    assert_eq!(actions[0].params()[3].choices, ["Kenya", "Other"]);
}
