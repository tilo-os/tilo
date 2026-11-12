//! A site's sign-in in the window ([`crate::page_sign_in`],
//! [`super::sign_in`]): the person answers in the prompt over the page, an
//! agent through the window's row, and what either gave is in nothing the
//! window publishes or prints.

use std::sync::Arc;

use ui::webview::{
    HttpAuthChallenge, HttpAuthCredentials, PageDialog, PageDialogKind, WebViewCommand,
    WebViewEngine as _,
};
use ui::widget::FieldRole;
use winit::keyboard::NamedKey;

use super::tests::{effect_operation, layout_keeping_tree, synced};
use super::*;

const ASKER: &str = "https://staging.example.org";
const USER: &str = "ada.lovelace-77";
const PASSWORD: &str = "correct-horse-9";

/// A window in `mode` on a page of the site, its engine the tests' own.
fn window(mode: BrowsingMode) -> (Arc<TestEngine>, BrowserWidget) {
    let engine = TestEngine::new();
    engine.send(WebViewCommand::Navigate(format!("{ASKER}/")));
    let mut widget = BrowserWidget::with_engine_mode(engine.clone(), true, mode);
    let _ = synced(&mut widget);
    (engine, widget)
}

fn asks(id: u32, asker: &str, retry: bool) -> WebViewEvent {
    WebViewEvent::HttpAuth(HttpAuthChallenge {
        id,
        asker: asker.into(),
        proxy: false,
        realm: "Staff wiki".into(),
        scheme: "basic".into(),
        retry,
    })
}

/// Every answer the window gave to a sign-in, in order.
fn answers(engine: &TestEngine) -> Vec<(u32, String, Option<HttpAuthCredentials>)> {
    engine
        .commands()
        .into_iter()
        .filter_map(|command| match command {
            WebViewCommand::AnswerHttpAuth {
                id,
                asker,
                credentials,
            } => Some((id, asker, credentials)),
            _ => None,
        })
        .collect()
}

fn given() -> HttpAuthCredentials {
    HttpAuthCredentials::new(USER, PASSWORD)
}

/// The row the window publishes for the sign-in a request waits on.
fn row(widget: &BrowserWidget) -> Option<ui::tilo_actions::ResourceSpec> {
    widget
        .published()
        .children
        .into_iter()
        .find(|row| row.id.as_str() == crate::page_sign_in::ROW)
}

/// What the row's action `id` with `arguments` comes to, as a caller sends
/// it.
fn through_row(
    widget: &BrowserWidget,
    id: &str,
    arguments: &serde_json::Value,
) -> BrowserOperation {
    let row = row(widget).expect("a request waits on a sign-in");
    let spec = row
        .actions
        .iter()
        .find(|spec| spec.id == id)
        .unwrap_or_else(|| panic!("{id} is offered on the row"));
    let encoded = spec
        .build_json(arguments.as_object().expect("arguments are an object"))
        .expect("the action takes these arguments");
    effect_operation(&encoded.payload)
}

/// Everything the window publishes that could carry text: each row's id
/// and name, what is shown of it, and each action's name and the arguments
/// bound to it.
fn published_text(widget: &BrowserWidget) -> String {
    fn collect(resource: &ui::tilo_actions::ResourceSpec, into: &mut String) {
        let shown = resource.presentation.as_ref();
        let texts = [
            Some(&resource.id.0),
            Some(&resource.label),
            shown.and_then(|shown| shown.detail.as_ref()),
            shown.and_then(|shown| shown.badge.as_ref()),
            shown.and_then(|shown| shown.preview.as_ref()),
        ];
        for text in texts.into_iter().flatten() {
            into.push_str(text);
            into.push('\n');
        }
        for action in &resource.actions {
            into.push_str(&action.label);
            into.push('\n');
            if let Ok(bound) = action.build_json(&serde_json::Map::new()) {
                into.push_str(&String::from_utf8_lossy(&bound.payload));
                into.push('\n');
            }
        }
        for child in &resource.children {
            collect(child, into);
        }
    }
    let mut text = String::new();
    collect(&widget.published(), &mut text);
    text
}

/// Whether the prompt, or the page's own question, stands over the window.
fn shows(widget: &BrowserWidget, id: &str) -> bool {
    ui::widget::find_test_id(widget, id).is_some()
}

/// Press `key` in the window, as a person does.
fn press(widget: &mut BrowserWidget, tree: &TaffyTree<()>, node: NodeId, key: NamedKey) {
    ui::testing::Dispatch::new()
        .layout(tree, node)
        .send(widget, &ui::synthetic_input::named_key(key));
}

/// Type `text` into the window key by key, as a person does.
fn type_text(widget: &mut BrowserWidget, tree: &TaffyTree<()>, node: NodeId, text: &str) {
    for character in text.chars() {
        ui::testing::Dispatch::new().layout(tree, node).send(
            widget,
            &ui::synthetic_input::char_key(&character.to_string()),
        );
    }
}

/// A person signs in from the prompt over the page: the user name, Tab, the
/// password, Enter. What they type is the prompt's alone until it is sent,
/// and then goes to the asker the prompt named. Asked again because it was
/// not accepted, the prompt says so and keeps the user name; Escape cancels.
#[test]
fn a_person_signs_in_from_the_prompt_over_the_page() {
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let theme = Theme::dark();
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(engine.accepts_http_auth(), "the page's sign-ins come here");
    assert!(row(&widget).is_none() && !shows(&widget, crate::page_sign_in::ROW));

    engine.push_event(asks(7, ASKER, false));
    let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    let shown = row(&widget).expect("the sign-in is the window's row");
    assert_eq!(shown.label, "Sign in to staging.example.org");
    assert!(shows(&widget, crate::page_sign_in::ROW));
    assert!(
        widget.modal_capture().is_some(),
        "the prompt owns the window until it is answered"
    );

    type_text(&mut widget, &tree, node, USER);
    press(&mut widget, &tree, node, NamedKey::Tab);
    type_text(&mut widget, &tree, node, PASSWORD);
    let prompt = widget.page_sign_in.prompt().expect("the prompt is open");
    assert_eq!(
        prompt
            .field(FieldRole::Username)
            .map(ui::widget::TextInput::value),
        Some(USER)
    );
    assert!(
        prompt
            .field(FieldRole::Secret)
            .is_some_and(|field| field.is_secret() && field.value() == PASSWORD),
        "the password is typed into a secret field"
    );
    // What is typed is the prompt's alone: nothing published holds it, and
    // nothing is sent before the person says so.
    let published = published_text(&widget);
    assert!(
        published.contains("Sign in to staging.example.org") && published.contains("Staff wiki"),
        "what the row says is in what is searched: {published}"
    );
    assert!(
        !published.contains(PASSWORD) && !published.contains(USER),
        "{published}"
    );
    assert!(answers(&engine).is_empty());

    press(&mut widget, &tree, node, NamedKey::Enter);
    assert_eq!(
        answers(&engine),
        [(7, ASKER.to_owned(), Some(given()))],
        "what was typed goes to the asker the prompt named"
    );
    assert!(
        !format!("{:?}", engine.commands()).contains(PASSWORD),
        "the command prints without the password"
    );
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(row(&widget).is_none() && !shows(&widget, crate::page_sign_in::ROW));

    // The site did not accept them and asks again.
    engine.push_event(asks(8, ASKER, true));
    let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    let prompt = widget.page_sign_in.prompt().expect("the prompt is open");
    assert_eq!(
        prompt.error(),
        Some("That user name or password was not accepted.")
    );
    assert_eq!(
        prompt
            .field(FieldRole::Username)
            .map(ui::widget::TextInput::value),
        Some(USER),
        "the user name the person gave stays"
    );
    assert!(
        prompt
            .field(FieldRole::Secret)
            .is_some_and(|field| field.value().is_empty()),
        "the password never does"
    );
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
    let published = published_text(&widget);
    assert!(!published.contains(USER), "{published}");

    press(&mut widget, &tree, node, NamedKey::Escape);
    assert_eq!(
        answers(&engine)[1],
        (8, ASKER.to_owned(), None),
        "Escape cancels"
    );
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(row(&widget).is_none());
}

/// An agent answers the same question through the window's row, under the
/// rules of any secret it gives a page: it asks the person first, the
/// password is a secret parameter, the answer says who was answered and
/// never with what, and the window holds a sign-in from then on. What a
/// person typed into the prompt is no part of what the agent can read or
/// send.
#[test]
fn an_agent_signs_in_through_the_row_under_the_rules_of_any_secret() {
    use ui::permissions::policy::Answer;
    use ui::permissions::policy::agent_answers::default_answer;
    const TYPED: &str = "typed-by-the-person-3";
    let (engine, mut widget) = window(BrowsingMode::Visitor);
    let theme = Theme::dark();
    engine.push_event(asks(3, ASKER, false));
    let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(
        !widget.page_holds_sign_in(),
        "a window opened for an agent holds no sign-in yet"
    );
    let shown = row(&widget).expect("the sign-in is the window's row");
    let sign_in = shown
        .actions
        .iter()
        .find(|action| action.id == "answer-page-sign-in")
        .expect("the row offers the answer");
    assert!(sign_in.contract.validate_declaration().is_empty());
    assert!(
        sign_in.params().iter().any(
            |param| param.name == "password" && param.ty == ui::tilo_actions::ParamType::Secret
        ),
        "the password is a secret parameter, kept out of logs and history"
    );
    assert_eq!(
        default_answer(
            sign_in.contract.need().kind,
            sign_in.contract.requires_confirmation()
        ),
        Answer::Ask,
        "an agent asks the person before it gives a site credentials"
    );

    // The person started typing; the agent's window reads none of it.
    type_text(&mut widget, &tree, node, "someone.else-5");
    press(&mut widget, &tree, node, NamedKey::Tab);
    type_text(&mut widget, &tree, node, TYPED);
    let published = published_text(&widget);
    assert!(
        !published.contains(TYPED) && !published.contains("someone.else-5"),
        "{published}"
    );

    let ada = ui::tilo_actions::ActionInvoker::actor("agent:ada")
        .with_kind(ui::tilo_actions::ActorKind::Agent);
    let answer = through_row(
        &widget,
        "answer-page-sign-in",
        &serde_json::json!({"username": USER, "password": PASSWORD}),
    );
    let Ok(ui::framework::Effected::Done(output)) =
        widget.admit_for(ui::tilo_actions::InvocationId::new(), Some(&ada), answer)
    else {
        panic!("the sign-in answers at once");
    };
    let said = output
        .value
        .map(|value| value.to_string())
        .unwrap_or_default();
    assert!(said.contains(ASKER), "it names who was answered: {said}");
    assert!(
        !said.contains(PASSWORD) && !said.contains(USER) && !said.contains(TYPED),
        "and never with what: {said}"
    );
    assert_eq!(
        answers(&engine),
        [(3, ASKER.to_owned(), Some(given()))],
        "the agent's own credentials, not what stood in the prompt"
    );
    assert!(
        widget.page_holds_sign_in(),
        "the page holds a sign-in from the moment credentials are sent"
    );
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(row(&widget).is_none() && !shows(&widget, crate::page_sign_in::ROW));

    // An answer to a sign-in no request waits on any more sends nothing.
    let stale = BrowserOperation::AnswerPageSignIn {
        sign_in: 3,
        credentials: Some(given()),
    };
    assert!(
        widget
            .admit_for(ui::tilo_actions::InvocationId::new(), Some(&ada), stale)
            .is_err()
    );
    assert_eq!(answers(&engine).len(), 1);

    // Asked again, the agent cancels through the row.
    engine.push_event(asks(4, ASKER, true));
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    let cancel = through_row(&widget, "cancel-page-sign-in", &serde_json::json!({}));
    let Ok(ui::framework::Effected::Done(output)) =
        widget.admit_for(ui::tilo_actions::InvocationId::new(), Some(&ada), cancel)
    else {
        panic!("the cancel answers at once");
    };
    let said = output
        .value
        .map(|value| value.to_string())
        .unwrap_or_default();
    assert!(said.contains("Cancelled") && said.contains(ASKER), "{said}");
    assert_eq!(answers(&engine)[1], (4, ASKER.to_owned(), None));
}

/// Nothing waits for good, and one sign-in is open at a time: the engine
/// letting a sign-in go takes the prompt down unanswered, and a newer
/// sign-in takes the place of one still open, so what is typed then goes to
/// the one the prompt names and an answer for the old one reaches nobody.
#[test]
fn a_sign_in_ends_with_its_request_and_a_newer_one_takes_its_place() {
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let theme = Theme::dark();
    engine.push_event(asks(9, ASKER, false));
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(row(&widget).is_some() && shows(&widget, crate::page_sign_in::ROW));

    // The tab went elsewhere, or nobody answered in the engine's time.
    engine.push_event(WebViewEvent::HttpAuthClosed { id: 9 });
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(row(&widget).is_none() && !shows(&widget, crate::page_sign_in::ROW));
    assert!(answers(&engine).is_empty(), "nobody answered it");

    engine.push_event(asks(10, ASKER, false));
    engine.push_event(asks(11, "https://other.example", false));
    let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert_eq!(
        row(&widget).map(|row| row.label).as_deref(),
        Some("Sign in to other.example")
    );
    let late = BrowserOperation::AnswerPageSignIn {
        sign_in: 10,
        credentials: Some(given()),
    };
    assert!(
        widget
            .admit(ui::tilo_actions::InvocationId::new(), late)
            .is_err(),
        "an answer for the one it replaced reaches nobody"
    );
    assert!(answers(&engine).is_empty());

    type_text(&mut widget, &tree, node, USER);
    press(&mut widget, &tree, node, NamedKey::Tab);
    type_text(&mut widget, &tree, node, PASSWORD);
    press(&mut widget, &tree, node, NamedKey::Enter);
    assert_eq!(
        answers(&engine),
        [(11, "https://other.example".to_owned(), Some(given()))]
    );
}

/// One question stands over the page at a time. A sign-in comes before the
/// page's own question: answering it leaves the page's question as it was,
/// to show again.
#[test]
fn a_sign_in_stands_over_the_pages_own_question_until_it_is_answered() {
    let (engine, mut widget) = window(BrowsingMode::Normal);
    let theme = Theme::dark();
    engine.push_event(WebViewEvent::PageDialog(PageDialog {
        id: 2,
        kind: PageDialogKind::Confirm,
        origin: ASKER.into(),
        message: "Go on?".into(),
        default_text: String::new(),
    }));
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(shows(&widget, crate::page_dialogs::ROW));

    engine.push_event(asks(5, ASKER, false));
    let (tree, node) = layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(shows(&widget, crate::page_sign_in::ROW));
    assert!(
        !shows(&widget, crate::page_dialogs::ROW),
        "one question at a time"
    );
    let rows: Vec<String> = widget
        .published()
        .children
        .into_iter()
        .map(|row| row.id.0)
        .collect();
    assert!(
        rows.iter().any(|id| id == crate::page_sign_in::ROW)
            && rows.iter().any(|id| id == crate::page_dialogs::ROW),
        "both wait as rows: {rows:?}"
    );

    press(&mut widget, &tree, node, NamedKey::Escape);
    assert_eq!(answers(&engine), [(5, ASKER.to_owned(), None)]);
    assert!(
        !engine
            .commands()
            .iter()
            .any(|command| matches!(command, WebViewCommand::AnswerPageDialog { .. })),
        "the page's question is not answered by it"
    );
    layout_keeping_tree(&mut widget, 1000.0, 700.0, &theme);
    assert!(shows(&widget, crate::page_dialogs::ROW));
    assert!(!shows(&widget, crate::page_sign_in::ROW));
}
