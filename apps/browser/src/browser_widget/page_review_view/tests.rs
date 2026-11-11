//! The page's review as its window's host drives it: the real app, the
//! deterministic test engine as the page, and the page's own actions invoked
//! for the person at the window and for agents the action door admitted.

use std::sync::Arc;

use serde_json::{Value, json};
use ui::framework::testing::TestApp;
use ui::tilo_actions::{ActionInvoker, ActionSpec, ActorKind, InvocationUpdate, ResourceSpec};
use ui::tilo_resource::Effect as _;
use ui::webview::test_engine::TestEngine;
use ui::webview::{PageRequest, WebViewCommand, WebViewEngine as _};

use crate::effects::BrowserEffect;
use crate::resource::BrowserShell;

type App = TestApp<BrowserShell>;

/// The page every test reviews.
const FORM: &str = "https://visas.example.org/apply";

/// A Browser window on [`FORM`], its page the test engine.
fn window() -> (Arc<TestEngine>, App) {
    let engine = TestEngine::new();
    super::super::test_window_engine::install(engine.clone());
    let mut app = App::launched(&[FORM]).expect("Browser opens");
    // The engine shows the page, as it does once the address loaded.
    engine.send(WebViewCommand::Navigate(FORM.into()));
    eventually(&mut app, "the window shows the form's page", |app| {
        page_action(app, "review-page")
            .is_some_and(|action| action.contract.availability.is_available())
    });
    (engine, app)
}

/// Run the window's frames until `holds`, as an idle window runs them when
/// it is woken; fail with what the window says if it never does.
fn eventually(app: &mut App, what: &str, mut holds: impl FnMut(&mut App) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        app.settle();
        if holds(app) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "{what}; the window says {:?}",
            app.texts()
        );
        app.harness()
            .wait_until(ui::time::Duration::from_millis(20), |_| false);
    }
}

/// The action `id` the window publishes on its page.
fn page_action(app: &mut App, id: &str) -> Option<ActionSpec> {
    fn find(resources: &[ResourceSpec], wanted: &str) -> Option<ResourceSpec> {
        resources.iter().find_map(|resource| {
            (resource.id.0 == wanted)
                .then(|| resource.clone())
                .or_else(|| find(&resource.children, wanted))
        })
    }
    find(&app.harness().resources(), "browser.page")?
        .actions
        .into_iter()
        .find(|action| action.id == id)
}

/// Invoke `effect` on the page for `invoker`, as the host does for an agent
/// it admitted; for nobody, the person at the window. Its answer.
fn invoke(app: &mut App, invoker: Option<&ActionInvoker>, effect: &BrowserEffect) -> Value {
    let instance = app
        .harness()
        .component_instance_id()
        .expect("the Browser component");
    let address = ui::bound_dispatch::BoundActionAddress {
        instance_id: instance,
        address: effect.address_on("browser.page"),
    };
    let (invocation, started) = app
        .harness()
        .start_bound_job_for(
            invoker.cloned(),
            &address,
            &effect.arguments(),
            invoker.is_some(),
        )
        .unwrap_or_else(|error| panic!("{} is admitted: {error}", effect.id()));
    if let Some(output) = started.completed() {
        return output.value.unwrap_or_default();
    }
    app.harness()
        .wait_until(ui::time::Duration::from_secs(30), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(InvocationUpdate::is_terminal)
        });
    for update in app.harness().job_updates(invocation) {
        match update {
            InvocationUpdate::Succeeded { output, .. } => {
                return output.value.clone().unwrap_or_default();
            }
            InvocationUpdate::Failed { error, .. } => {
                panic!("{}: {}: {}", effect.id(), error.code, error.message)
            }
            _ => {}
        }
    }
    panic!("{} did not answer", effect.id());
}

fn agent(name: &str) -> ActionInvoker {
    ActionInvoker::actor(name).with_kind(ActorKind::Agent)
}

/// One question of the page's form: its name, its kind, what its field
/// holds, and who the engine says set that (`Value::Null` for no one).
type Answer<'a> = (&'a str, &'a str, &'a str, Value);

/// Set by `actor`'s action at place `seq` of the tab's record.
fn by(actor: &str, seq: u64) -> Value {
    json!({"how": "action", "by": actor, "seq": seq})
}

/// The page holds `answers` and the tab's record `record` from now on: what
/// the engine's forms, fields and record reads answer, in their own shapes,
/// until the page changes again.
fn page_holds(engine: &TestEngine, answers: &[Answer<'_>], record: &[Value]) {
    let mut asked = Vec::new();
    let mut fields = Vec::new();
    for (index, (question, kind, value, set)) in answers.iter().enumerate() {
        let element = format!("d1e{}", index + 2);
        let mut ask = json!({"name": question, "kind": kind, "element": element});
        if !value.is_empty() {
            // The forms read never carries a secret either.
            ask["value"] = if *kind == "secret" {
                json!("<14 characters hidden>")
            } else {
                json!(value)
            };
        }
        asked.push(ask);
        let mut field = json!({"element": element, "role": "textbox", "name": question,
            "value": value, "form": "form1", "question": question});
        if !set.is_null() {
            field["set"] = set.clone();
        }
        fields.push(field);
    }
    engine.set_page_data(
        "forms",
        json!({"forms": [{"form": "form1", "label": "Apply for a visitor visa",
            "method": "post", "action": "/apply", "signature": "5d0c7a1e",
            "submit": ["Send application"], "fields": asked}]}),
    );
    engine.set_page_data("fields", json!({"fields": fields}));
    engine.set_page_data(
        "ledger",
        json!({"entries": record, "next": record.len(), "more": false, "dropped": 0,
            "waiting": 0}),
    );
}

/// What the row named `name` says under its name: its marks, what it holds,
/// who set it and its status, as assistive technology reads it.
fn row(app: &mut App, name: &str) -> Option<String> {
    let texts = app.texts();
    let at = texts.iter().position(|text| text == name)?;
    texts.get(at + 1).cloned()
}

/// Whether the row named `name` says each of `words`.
fn row_says(app: &mut App, name: &str, words: &[&str]) -> bool {
    row(app, name).is_some_and(|said| words.iter().all(|word| said.contains(word)))
}

/// The page requests the engine was sent, with who asked each.
fn page_asks(engine: &TestEngine) -> Vec<(Option<String>, PageRequest)> {
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

fn fill_form(question: &str, answer: &str) -> BrowserEffect {
    BrowserEffect::FillPageForm {
        form: "form1".into(),
        answers: [(question.to_owned(), json!(answer))].into_iter().collect(),
        submit: None,
        submit_button: None,
        cross_origin: None,
        frame: None,
    }
}

/// The review opens through the page's own action for the person at the
/// window and for an agent alike, starts empty on a page that asks nothing,
/// and closing it shows the page again.
#[test]
fn the_review_opens_for_a_person_and_for_an_agent_and_closes_to_the_page() {
    let (engine, mut app) = window();
    for id in ["review-page", "show-page-field"] {
        assert!(
            page_action(&mut app, id).is_some(),
            "{id} is one of the page's actions"
        );
    }
    // The window reads its page's forms itself once the page has loaded,
    // for the actions the page publishes. The review reads nothing yet.
    let before = page_asks(&engine);
    assert!(
        before
            .iter()
            .all(|ask| *ask == (None, PageRequest::Forms { frame: None })),
        "no field and no record is read of a page nobody reviews: {before:?}"
    );

    let said = invoke(&mut app, None, &BrowserEffect::ReviewPage);
    assert_eq!(
        said,
        json!("The review of this page is open in the window.")
    );
    eventually(
        &mut app,
        "the review of a page with no form is empty",
        |app| {
            app.texts()
                .iter()
                .any(|text| text == "No forms on this page")
        },
    );
    assert!(
        !app.harness().targets().contains_key("browser.surface"),
        "the review stands in the page's place"
    );
    // The review reads the page as the person at the window, the tab's one
    // unnamed actor: no agent is told anything by it.
    let asks = page_asks(&engine);
    assert!(
        asks.iter()
            .any(|(_, request)| matches!(request, PageRequest::Fields { .. }))
            && asks
                .iter()
                .any(|(_, request)| matches!(request, PageRequest::Ledger { .. }))
    );
    assert!(asks.iter().all(|(actor, _)| actor.is_none()), "{asks:?}");

    app.effect(&BrowserEffect::ShowPage).expect("show-page");
    eventually(&mut app, "closing the review shows the page again", |app| {
        app.harness().targets().contains_key("browser.surface")
    });
    assert!(
        !app.texts()
            .iter()
            .any(|text| text == "No forms on this page")
    );

    let ada = agent("agent:ada");
    invoke(&mut app, Some(&ada), &BrowserEffect::ReviewPage);
    eventually(
        &mut app,
        "an agent opens the review for the person",
        |app| {
            app.texts()
                .iter()
                .any(|text| text == "No forms on this page")
        },
    );
}

/// Two agents answer parts of one form. The review, opened once before
/// either acted, shows each answer under the name of the agent that set it
/// and what was done in the tab, in order, without being asked again.
#[test]
fn each_answer_shows_under_the_agent_that_set_it_as_the_agents_work() {
    let (engine, mut app) = window();
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    page_holds(
        &engine,
        &[
            ("Given names", "text", "", Value::Null),
            ("Passport number", "text", "", Value::Null),
        ],
        &[],
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review lists the form's questions", |app| {
        row_says(app, "Given names", &["No answer", "No one set this"])
    });
    assert!(row_says(&mut app, "Passport number", &["No answer"]));
    assert!(
        app.texts()
            .iter()
            .any(|text| text == "Apply for a visitor visa"),
        "the questions stand under their form"
    );

    // Ada answers one question. The page holds her answer from then on.
    let first = json!({"seq": 1, "actor": "agent:ada", "action": "fill_form", "ok": true,
        "answers": [{"name": "Given names", "value": "Maria Elena"}]});
    page_holds(
        &engine,
        &[
            ("Given names", "text", "Maria Elena", by("agent:ada", 1)),
            ("Passport number", "text", "", Value::Null),
        ],
        std::slice::from_ref(&first),
    );
    invoke(
        &mut app,
        Some(&ada),
        &fill_form("Given names", "Maria Elena"),
    );
    eventually(&mut app, "the review shows Ada's answer", |app| {
        row_says(app, "Given names", &["Maria Elena", "Set by ada"])
    });
    assert!(row_says(&mut app, "Passport number", &["No answer"]));

    // Ben answers another.
    let second = json!({"seq": 2, "actor": "agent:ben", "action": "fill_form", "ok": true,
        "answers": [{"name": "Passport number", "value": "P7K290XB4"}]});
    page_holds(
        &engine,
        &[
            ("Given names", "text", "Maria Elena", by("agent:ada", 1)),
            ("Passport number", "text", "P7K290XB4", by("agent:ben", 2)),
        ],
        &[first, second],
    );
    invoke(
        &mut app,
        Some(&ben),
        &fill_form("Passport number", "P7K290XB4"),
    );
    eventually(&mut app, "the review shows Ben's answer", |app| {
        row_says(app, "Passport number", &["P7K290XB4", "Set by ben"])
    });
    let given = row(&mut app, "Given names").expect("Ada's row");
    assert!(
        given.contains("Maria Elena") && given.contains("Set by ada") && !given.contains("ben"),
        "{given}"
    );
    let passport = row(&mut app, "Passport number").expect("Ben's row");
    assert!(!passport.contains("ada"), "{passport}");

    // What was done in the tab, in order, with who did it.
    let texts = app.texts();
    let done: Vec<&str> = texts
        .iter()
        .zip(texts.iter().skip(1))
        .filter(|(name, _)| *name == "Answered 1 question")
        .map(|(_, said)| said.as_str())
        .collect();
    assert_eq!(done, ["Given names, By ada", "Passport number, By ben"]);

    // Each agent acted under its own name; the review's reads under none.
    let asks = page_asks(&engine);
    let acted: Vec<Option<&str>> = asks
        .iter()
        .filter(|(_, request)| matches!(request, PageRequest::FillForm { .. }))
        .map(|(actor, _)| actor.as_deref())
        .collect();
    assert_eq!(acted, [Some("agent:ada"), Some("agent:ben")]);
    assert!(
        asks.iter()
            .filter(|(_, request)| matches!(
                request,
                PageRequest::Fields { .. } | PageRequest::Forms { .. } | PageRequest::Ledger { .. }
            ))
            .all(|(actor, _)| actor.is_none())
    );
}

/// A field that no longer holds what was set is marked, though nothing
/// acted: the review reads the page again by itself. A further action then
/// shows without the review being opened again.
#[test]
fn a_field_changed_after_it_was_set_is_marked_and_the_review_follows_the_page() {
    let (engine, mut app) = window();
    let (ada, ben) = (agent("agent:ada"), agent("agent:ben"));
    let filled = json!({"seq": 1, "actor": "agent:ada", "action": "fill", "ok": true,
        "target": {"role": "textbox", "name": "Arrival"}, "text": "2027-02-03"});
    page_holds(
        &engine,
        &[("Arrival", "date", "2027-02-03", by("agent:ada", 1))],
        std::slice::from_ref(&filled),
    );
    invoke(
        &mut app,
        Some(&ada),
        &BrowserEffect::FillPage {
            element: "d1e2".into(),
            text: "2027-02-03".into(),
            frame: None,
        },
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review shows what Ada set", |app| {
        row_says(app, "Arrival", &["2027-02-03", "Set by ada"])
    });
    let arrival = row(&mut app, "Arrival").expect("the row");
    assert!(!arrival.contains("Changed"), "{arrival}");

    // The page's own script rewrites the answer. No action ran, so nothing
    // tells the review: it finds out by reading the page.
    let mut changed = by("agent:ada", 1);
    changed["changed"] = json!(true);
    page_holds(
        &engine,
        &[("Arrival", "date", "2027-02-10", changed)],
        std::slice::from_ref(&filled),
    );
    eventually(&mut app, "the changed field is marked", |app| {
        row_says(app, "Arrival", &["Changed since it was set"])
    });
    assert!(row_says(
        &mut app,
        "Arrival",
        &["2027-02-10", "Set by ada", "Needs attention"]
    ));

    // Ben sets it again: the review follows, still open.
    let again = json!({"seq": 2, "actor": "agent:ben", "action": "fill", "ok": true,
        "target": {"role": "textbox", "name": "Arrival"}, "text": "2027-02-12"});
    page_holds(
        &engine,
        &[("Arrival", "date", "2027-02-12", by("agent:ben", 2))],
        &[filled, again],
    );
    invoke(
        &mut app,
        Some(&ben),
        &BrowserEffect::FillPage {
            element: "d1e2".into(),
            text: "2027-02-12".into(),
            frame: None,
        },
    );
    eventually(&mut app, "the review shows Ben's answer", |app| {
        row_says(app, "Arrival", &["2027-02-12", "Set by ben"])
    });
    let arrival = row(&mut app, "Arrival").expect("the row");
    assert!(!arrival.contains("Changed"), "{arrival}");
    assert_eq!(
        row(&mut app, "Filled Arrival").as_deref(),
        Some("2027-02-03, By ada"),
        "the record keeps what each wrote, oldest first"
    );
}

/// What a person typed straight into the page stands in the tab's record
/// among the actions, said as typing and under no agent's name. What was
/// written into a secret field, typed or by an action, shows as set and
/// never as text, whatever the record's entry carries.
#[test]
fn what_a_person_typed_is_listed_as_typing_and_a_secret_only_as_set() {
    let (engine, mut app) = window();
    let record = [
        json!({"seq": 1, "actor": "", "how": "input", "ok": true,
            "target": {"role": "textbox", "name": "Email"}, "text": "ada@example.org"}),
        json!({"seq": 2, "actor": "", "how": "input", "ok": true, "secret": true,
            "target": {"role": "textbox", "name": "Portal password"}, "text": "hunter2-typed"}),
        json!({"seq": 3, "actor": "agent:ada", "action": "press", "ok": true, "secret": true,
            "message": "Pressed a key in a secret field", "text": "7",
            "target": {"role": "textbox", "name": "Card PIN"}}),
    ];
    page_holds(
        &engine,
        &[("Email", "email", "ada@example.org", json!({"how": "input"}))],
        &record,
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review lists what was typed", |app| {
        row(app, "Typed into Email").is_some()
    });
    assert_eq!(
        row(&mut app, "Typed into Email").as_deref(),
        Some("ada@example.org, Direct input")
    );
    assert_eq!(
        row(&mut app, "Typed into Portal password").as_deref(),
        Some("Set, not shown, Direct input"),
        "the record says the field was set"
    );
    assert_eq!(
        row(&mut app, "Pressed a key in Card PIN").as_deref(),
        Some("Set, not shown, By ada"),
        "no key is named"
    );
    let tree = format!("{:?}", app.harness().a11y_tree_update());
    assert!(!tree.contains("hunter2"), "a secret reached the tree");
    assert!(
        !app.texts().iter().any(|text| text.contains("hunter2")),
        "a secret is read out"
    );
}

/// What the window held from leaving the page stands in the review among the
/// actions: what waited for the person and whose action reached it, who let
/// it leave, who kept the page as it is, and what was dropped with nobody
/// answering.
#[test]
fn what_was_held_is_listed_with_who_let_it_leave() {
    let (engine, mut app) = window();
    let visa = "POST https://visas.example.org";
    let tracker = "POST https://tracker.example";
    let receipt = "receipt.pdf, https://visas.example.org";
    let hold = |seq: u64, actor: &str, action: &str, message: &str| {
        json!({"seq": seq, "actor": actor, "how": "hold", "action": action, "ok": true,
            "message": message, "url": FORM})
    };
    let record = [
        json!({"seq": 1, "actor": "agent:intake", "action": "click", "ok": true,
            "target": {"role": "button", "name": "Send application"}}),
        hold(2, "agent:intake", "submit", visa),
        hold(3, "", "allow", visa),
        hold(4, "", "request", tracker),
        hold(5, "agent:rev", "keep", tracker),
        hold(6, "", "download", receipt),
        hold(7, "", "drop", receipt),
    ];
    page_holds(
        &engine,
        &[("Email", "email", "ada@example.org", json!({"how": "input"}))],
        &record,
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    let waits = format!("Submission held for you: {visa}");
    eventually(&mut app, "the review lists what was held", |app| {
        row(app, &waits).is_some()
    });
    assert_eq!(
        row(&mut app, &waits).as_deref(),
        Some("By intake"),
        "under the agent whose action reached it"
    );
    assert_eq!(
        row(&mut app, "Let leave by you").as_deref(),
        Some(visa),
        "the person let it leave, and the row says what"
    );
    let texts = app.texts();
    for said in [
        format!("Request held for you: {tracker}"),
        format!("Download held for you: {receipt}"),
    ] {
        assert!(texts.contains(&said), "{said:?} is not in {texts:?}");
    }
    assert_eq!(
        row(&mut app, "Kept as it is").as_deref(),
        Some("POST https://tracker.example, By rev"),
        "the agent that kept the page is named"
    );
    assert_eq!(
        row(&mut app, "Dropped, nobody answered").as_deref(),
        Some(receipt)
    );
    assert!(
        !texts.iter().any(|text| text == "Sent the form"),
        "a held submission is not listed as one that was sent: {texts:?}"
    );
}

/// A secret is reviewed as set or not set. Its text is in no part of the
/// window's accessibility tree, whatever a read of the page carried.
#[test]
fn a_secrets_text_is_never_in_the_reviews_accessibility_tree() {
    let (engine, mut app) = window();
    // A read that carried a secret's text, and a record that kept it: the
    // engine sends neither, and the review would not show them if it did.
    let leaked = json!({"seq": 1, "actor": "agent:ada", "action": "fill", "ok": true,
        "target": {"role": "textbox", "name": "Portal password"}, "text": "hunter2-secret"});
    page_holds(
        &engine,
        &[
            (
                "Portal password",
                "secret",
                "hunter2-secret",
                by("agent:ada", 1),
            ),
            (
                "Card PIN",
                "text",
                "<4 characters hidden>",
                json!({"how": "input"}),
            ),
            ("Recovery code", "secret", "", Value::Null),
        ],
        &[leaked],
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review lists the secrets", |app| {
        row(app, "Portal password").is_some()
    });
    assert_eq!(
        row(&mut app, "Portal password").as_deref(),
        Some("Set, Set by ada")
    );
    assert_eq!(
        row(&mut app, "Card PIN").as_deref(),
        Some("Set, Typed into the page"),
        "a field the page lists by its length is a secret too"
    );
    assert_eq!(
        row(&mut app, "Recovery code").as_deref(),
        Some("Not set, No one set this")
    );
    assert_eq!(
        row(&mut app, "Filled Portal password").as_deref(),
        Some("By ada"),
        "what was written into a secret is not listed"
    );
    let tree = format!("{:?}", app.harness().a11y_tree_update());
    for hidden in ["hunter2", "characters hidden"] {
        assert!(!tree.contains(hidden), "{hidden} reached the tree");
        assert!(
            !app.texts().iter().any(|text| text.contains(hidden)),
            "{hidden} is read out"
        );
    }
}

/// How often the review read the page's fields.
fn field_reads(engine: &TestEngine) -> usize {
    page_asks(engine)
        .iter()
        .filter(|(_, request)| matches!(request, PageRequest::Fields { .. }))
        .count()
}

/// An engine that tells page events says when the page changed itself. The
/// review of its page reads on no timer: a field the page's own script
/// rewrote is not read until the engine tells of it, and then once.
#[test]
fn the_review_follows_what_the_engine_tells_and_reads_on_no_timer() {
    use ui::webview::{PageChange, PageFieldChange};
    let (engine, mut app) = window();
    engine.set_page_events(true);
    let ada = agent("agent:ada");
    let filled = json!({"seq": 1, "actor": "agent:ada", "action": "fill", "ok": true,
        "target": {"role": "textbox", "name": "Arrival"}, "text": "2027-02-03"});
    page_holds(
        &engine,
        &[("Arrival", "date", "2027-02-03", by("agent:ada", 1))],
        std::slice::from_ref(&filled),
    );
    invoke(
        &mut app,
        Some(&ada),
        &BrowserEffect::FillPage {
            element: "d1e2".into(),
            text: "2027-02-03".into(),
            frame: None,
        },
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review shows what Ada set", |app| {
        row_says(app, "Arrival", &["2027-02-03", "Set by ada"])
    });
    app.harness()
        .wait_until(ui::time::Duration::from_millis(300), |_| false);
    app.settle();
    let before = field_reads(&engine);

    // The page's own script rewrites the answer. Longer than a timer would
    // take passes, and nothing is read: nothing said the page changed.
    let mut changed = by("agent:ada", 1);
    changed["changed"] = json!(true);
    page_holds(
        &engine,
        &[("Arrival", "date", "2027-02-10", changed)],
        std::slice::from_ref(&filled),
    );
    app.harness()
        .wait_until(ui::time::Duration::from_millis(2500), |_| false);
    app.settle();
    assert_eq!(field_reads(&engine), before, "nothing is read on a timer");
    assert!(row_says(&mut app, "Arrival", &["2027-02-03"]));

    // The engine tells of the field. It says which, never what it holds:
    // the review reads the page.
    engine.emit_page_changed(PageChange {
        document: 1,
        fields: vec![PageFieldChange {
            element: "d1e2".into(),
            role: "textbox".into(),
            name: "Arrival".into(),
            typed: false,
            form: Some("form1".into()),
            question: Some("Arrival".into()),
        }],
        ..PageChange::default()
    });
    eventually(&mut app, "the changed field is marked", |app| {
        row_says(app, "Arrival", &["2027-02-10", "Changed since it was set"])
    });
    app.harness()
        .wait_until(ui::time::Duration::from_millis(300), |_| false);
    app.settle();
    assert_eq!(field_reads(&engine), before + 1, "read once, when told");
}

/// Choosing a row is the page's own action for it: the page shows again,
/// and the engine is asked to bring that field into view.
#[test]
fn choosing_a_row_brings_its_field_into_view_on_the_page() {
    let (engine, mut app) = window();
    page_holds(
        &engine,
        &[
            ("Given names", "text", "Maria Elena", by("agent:ada", 1)),
            ("Passport number", "text", "P7K290XB4", by("agent:ben", 2)),
        ],
        &[],
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review lists the form's questions", |app| {
        app.harness()
            .targets()
            .contains_key("browser.review.form1.1")
    });
    app.harness()
        .click_named("browser.review.form1.1")
        .expect("the passport number's row");
    eventually(&mut app, "the page shows again", |app| {
        app.harness().targets().contains_key("browser.surface")
    });
    let scrolls: Vec<_> = page_asks(&engine)
        .into_iter()
        .filter_map(|(actor, request)| match request {
            PageRequest::Scroll { into_view, .. } => Some((actor, into_view)),
            _ => None,
        })
        .collect();
    assert_eq!(scrolls, [(None, Some("d1e3".to_owned()))]);
}

/// The fields of a frame the tab's actors read or acted in are reviewed
/// with the page's own, under their frame: what each holds and who set it.
/// A frame's form is its own document's, whatever id it shares with a form
/// of the page, and choosing its row brings the field into view in that
/// frame.
#[test]
fn the_fields_of_a_frame_are_reviewed_under_their_frame() {
    let (engine, mut app) = window();
    page_holds(
        &engine,
        &[("Given names", "text", "Maria Elena", by("agent:ada", 1))],
        &[],
    );
    // The fields read lists the frame's fields after the page's own, each
    // with the frame it is in.
    engine.set_page_data(
        "fields",
        json!({"fields": [
            {"element": "d1e2", "role": "textbox", "name": "Given names",
                "value": "Maria Elena", "form": "form1", "question": "Given names",
                "set": by("agent:ada", 1)},
            {"element": "d3e2", "role": "textbox", "name": "Card number",
                "value": "<16 characters hidden>", "form": "form1", "question": "Card number",
                "frame": "3", "set": {"how": "input"}},
            {"element": "d3e3", "role": "textbox", "name": "Name on card",
                "value": "Ada Lovelace", "form": "form1", "question": "Name on card",
                "frame": "3", "set": by("agent:ben", 2)},
        ]}),
    );
    invoke(&mut app, None, &BrowserEffect::ReviewPage);
    eventually(&mut app, "the review lists the frame's fields", |app| {
        app.harness()
            .targets()
            .contains_key("browser.review.frame3.form1.1")
    });
    assert!(
        app.texts().iter().any(|text| text.contains("In frame 3")),
        "{:?}",
        app.texts()
    );
    assert!(row_says(
        &mut app,
        "Given names",
        &["Maria Elena", "Set by ada"]
    ));
    assert_eq!(
        row(&mut app, "Card number").as_deref(),
        Some("Set, Typed into the page"),
        "a secret in a frame is said as set"
    );
    assert!(row_says(
        &mut app,
        "Name on card",
        &["Ada Lovelace", "Set by ben"]
    ));
    assert!(
        app.harness()
            .targets()
            .contains_key("browser.review.form1.0"),
        "the page's own form keeps its rows"
    );

    app.harness()
        .click_named("browser.review.frame3.form1.1")
        .expect("the row of the name on the card");
    eventually(&mut app, "the page shows again", |app| {
        app.harness().targets().contains_key("browser.surface")
    });
    let scrolls: Vec<_> = page_asks(&engine)
        .into_iter()
        .filter_map(|(actor, request)| match request {
            PageRequest::Scroll {
                into_view, frame, ..
            } => Some((actor, into_view, frame)),
            _ => None,
        })
        .collect();
    assert_eq!(
        scrolls,
        [(
            None,
            Some("d3e3".to_owned()),
            ui::webview::FrameId::parse("3")
        )]
    );
}

/// The screenshot scenario reviews an application two agents and a person
/// filled: every mark the review has, from the engine's own answer shapes.
#[test]
fn the_screenshot_scenario_reviews_a_filled_application() {
    let mut app = App::capture(Some("review")).expect("the scenario stages");
    app.settle();
    assert!(row_says(
        &mut app,
        "Given names",
        &["Maria Elena", "Set by intake"]
    ));
    assert!(row_says(
        &mut app,
        "Passport expiry",
        &[
            "Not valid",
            "Set by documents",
            "The passport must be valid for six months after arrival."
        ]
    ));
    assert!(row_says(
        &mut app,
        "Refused a visa before?",
        &["No", "Set by documents"]
    ));
    assert!(row_says(
        &mut app,
        "Arrival",
        &["Changed since it was set", "2027-02-10", "Set by intake"]
    ));
    assert!(row_says(&mut app, "Email", &["Typed into the page"]));
    assert!(row_says(&mut app, "Phone", &["Required", "No answer"]));
    assert_eq!(
        row(&mut app, "Portal password").as_deref(),
        Some("Set, Set by you")
    );
    assert!(row_says(
        &mut app,
        "I declare these answers are true",
        &["Yes", "Set by the page"]
    ));
    assert!(row_says(
        &mut app,
        "Clicked Send application",
        &["Failed", "By documents"]
    ));
}

/// The More menu's row, the bar's Page menu and a key all run the page's own
/// action. While the review shows, the bar names it after the page's site,
/// and Done, that site and Escape run the action that shows the page again;
/// each row is bound to the page's action for its field.
#[test]
fn the_menu_the_bar_and_a_key_reach_the_review_through_the_pages_actions() {
    use super::super::BrowserWidget;
    use super::super::architecture_tests::{
        activate_overflow_command, assert_action_binding, compose,
    };
    use super::super::tests::layout_keeping_tree;
    use crate::effects::BrowserOperation;
    use ui::tilo_resource::Resource as _;
    use ui::window_toolbar::ToolbarEvent;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    let root = crate::resource::Browser::KIND;
    let queued = |page: &BrowserWidget, resource: &str, action: &str| {
        page.pending_controls.iter().any(|invocation| {
            invocation.address.resource == resource && invocation.address.action == action
        })
    };
    let press = |page: &mut BrowserWidget, key: Key, held: ModifiersState| {
        let (tree, node) = layout_keeping_tree(page, 1000.0, 700.0, &ui::theme::Theme::dark());
        ui::testing::Dispatch::new()
            .layout(&tree, node)
            .modifiers(held)
            .send(page, &ui::synthetic_input::key_press(key, None))
            .handled
    };

    // The More menu's row.
    let mut page = BrowserWidget::new_headless();
    compose(&mut page);
    activate_overflow_command(&mut page, "Review page");
    assert!(page.run_pending_commands());
    assert!(queued(&page, "browser.page", "review-page"));

    // The bar's Page menu.
    let mut page = BrowserWidget::new_headless();
    compose(&mut page);
    let entry = page
        .window_toolbar()
        .actions
        .into_iter()
        .flat_map(|action| action.items)
        .find(|item| item.name == "Review page")
        .expect("the bar's Page menu offers the review");
    assert!(!entry.on, "it is not showing yet");
    page.run_window_toolbar_event(ToolbarEvent::Action { id: entry.id });
    assert!(queued(&page, "browser.page", "review-page"));

    // Its key, with the command key of either platform held.
    let mut page = BrowserWidget::new_headless();
    compose(&mut page);
    let held = ModifiersState::CONTROL | ModifiersState::SUPER | ModifiersState::ALT;
    assert!(press(&mut page, Key::Character("e".into()), held));
    assert!(queued(&page, "browser.page", "review-page"));

    // While it shows: Done and each row are bound to the canonical actions,
    // and the bar names the review after the page's site.
    let mut page = BrowserWidget::new_headless();
    page.stage(Some("review"));
    compose(&mut page);
    assert!(page.page_review_shown());
    assert_action_binding(&page, "browser.review.close", root, "show-page");
    assert_action_binding(
        &page,
        "browser.review.form1.0",
        "browser.page",
        "show-page-field",
    );
    let bar = page.window_toolbar();
    let location = bar.location.expect("a location");
    assert!(!location.editable, "the review is no address to type");
    assert_eq!(
        location.segments.last().map(|place| place.label.as_str()),
        Some("Review")
    );
    let entry = bar
        .actions
        .iter()
        .flat_map(|action| &action.items)
        .find(|item| item.name == "Review page")
        .expect("the Page menu still names the review");
    assert!(entry.on, "it is lit while the review shows");
    page.run_window_toolbar_event(ToolbarEvent::Commit {
        text: location.segments[0].value.clone(),
    });
    assert!(
        queued(&page, root, "show-page"),
        "the site's place is the page"
    );

    // Escape leaves it, through the same action, which shows the page again.
    let mut page = BrowserWidget::new_headless();
    page.stage(Some("review"));
    compose(&mut page);
    assert!(press(
        &mut page,
        Key::Named(NamedKey::Escape),
        ModifiersState::empty()
    ));
    assert!(queued(&page, root, "show-page"));
    page.perform_browser_operation(BrowserOperation::ShowPage)
        .unwrap();
    assert!(!page.page_review_shown());
}
