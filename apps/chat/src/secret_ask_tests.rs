//! A secret asked for in the conversation: what its question keeps, who
//! answers it, and where what is typed goes.

use std::sync::Arc;

use serde_json::json;
use tilo_actions::remote::{Caller, InvokeArgument, TestHost, person_vault};
use ui::RemoteActions;
use ui::framework::testing::TestApp;
use ui::tilo_actions::{Acting, ActionInvoker};
use ui::tilo_resource::MemberId;
use ui::widget::FieldRole;

use super::Asked;
use crate::app::Hit;
use crate::model::{ME, Message};
use crate::question::{DECLINED, FILLED, SECRET};
use crate::resource::ChatShell;
use crate::testing::{MountedChat, chain, message_item, mounted_post, mounted_rows};

const CHANNEL: &str = "c_chat";
const REFERENCE: &str = "vault.item:0123456789abcdef0123456789abcdef";
const TYPED: &str = "correct horse battery staple";

/// The person at the window, as what they post is attributed.
fn window_person(app: &mut TestApp<ChatShell>) -> MemberId {
    mounted_post(app, CHANNEL, "Good morning");
    app.settle();
    mounted_rows(app, CHANNEL)
        .last()
        .expect("the post is shown")
        .author
        .clone()
}

/// An agent acting for `person`, as a host admits its calls.
fn helper_of(person: &MemberId) -> ActionInvoker {
    ActionInvoker::admitted(&Acting::agent("helper").acting_for(Acting::person(person.id())))
}

/// What the agent's question for a site's sign-in carries.
fn sign_in() -> serde_json::Value {
    json!({"kind": "login", "belongsTo": ["https://www.example.com/login"], "username": "ada"})
}

/// The question's arguments, asking for `detail`.
fn asking(detail: &serde_json::Value) -> serde_json::Value {
    json!({"channel": CHANNEL, "text": "I need it to sign in.", "kind": SECRET, "detail": detail})
}

/// The agent asks `person` for a site's sign-in: the message that asks.
fn asked(app: &mut TestApp<ChatShell>, person: &MemberId) -> Message {
    app.call_by(helper_of(person), &chain(), "ask", asking(&sign_in()))
        .expect("the agent asks its person");
    row(app, |m| m.question.is_some())
}

/// The newest message of the conversation that `wanted` picks.
fn row(app: &mut TestApp<ChatShell>, wanted: impl Fn(&Message) -> bool) -> Message {
    app.settle();
    mounted_rows(app, CHANNEL)
        .into_iter()
        .rfind(|m| wanted(m))
        .expect("the message is shown")
}

/// A secret's question says what is asked for and where it belongs, its
/// card says so in the system's words with no button, and one that names no
/// kind or no place is refused before anyone is asked.
#[test]
fn a_secrets_question_says_what_is_asked_for_and_where_it_belongs() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    let message = asked(&mut app, &me);
    let question = message.question.clone().unwrap();
    assert_eq!(question.kind.0, SECRET);
    assert_eq!(question.person, me);
    let wanted = Asked::of(&question).expect("it reads as a request for a secret");
    assert_eq!(wanted.kind, person_vault::ItemKind::Login);
    assert_eq!(wanted.belongs_to, ["https://www.example.com/login"]);
    assert_eq!(wanted.username, "ada");

    let shown = app.chat().question_shown(&message).expect("it shows");
    assert!(shown.card && shown.answers.is_empty(), "{shown:?}");
    assert_eq!(shown.lines[0].0, "Password for www.example.com");
    assert!(
        shown.spoken().contains("It goes to your vault"),
        "{shown:?}"
    );

    let before = mounted_rows(&mut app, CHANNEL).len();
    for unusable in [
        json!(null),
        json!({"kind": "login"}),
        json!({"kind": "login", "belongsTo": []}),
        json!({"kind": "login", "belongsTo": ["my bank"]}),
        json!({"kind": "passphrase", "belongsTo": ["https://example.com"]}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": TYPED}),
    ] {
        assert!(
            app.call_by(helper_of(&me), &chain(), "ask", asking(&unusable))
                .is_err(),
            "{unusable}"
        );
    }
    app.settle();
    assert_eq!(mounted_rows(&mut app, CHANNEL).len(), before);
}

/// Only the person asked answers, themselves; their answer carries the
/// vault item's reference and nothing else, and a question they decline
/// says so.
#[test]
fn only_the_person_asked_answers_and_the_answer_carries_the_reference() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    let message = asked(&mut app, &me);
    let filled = json!({"answer": FILLED, "detail": {"reference": REFERENCE}});
    for other in [ActionInvoker::person("u_bob"), helper_of(&me)] {
        assert!(
            app.call_by(
                other,
                &message_item(&message.id),
                "answer-question",
                filled.clone()
            )
            .is_err(),
            "only the person asked answers, themselves"
        );
    }
    app.call_on(&message_item(&message.id), "answer-question", filled)
        .expect("the person asked answers");
    let answered = row(&mut app, |m| m.id == message.id);
    let given = answered.question.clone().unwrap().answered.unwrap();
    assert_eq!(given.answer.0, FILLED);
    assert_eq!(given.detail.0, json!({"reference": REFERENCE}).to_string());
    let shown = app.chat().question_shown(&answered).expect("it shows");
    assert!(shown.spoken().contains("Kept in the vault"), "{shown:?}");

    let second = asked(&mut app, &me);
    app.call_on(
        &message_item(&second.id),
        "answer-question",
        json!({"answer": DECLINED}),
    )
    .expect("the person declines");
    let declined = row(&mut app, |m| m.id == second.id);
    let shown = app.chat().question_shown(&declined).expect("it shows");
    assert!(shown.spoken().contains("Not given"), "{shown:?}");

    // A question put to someone else shows this reader no field.
    let theirs = asked(&mut app, &MemberId::person("u_bob"));
    app.settle();
    app.frame();
    assert_ne!(
        app.chat().secret_entry.shown_for().as_deref(),
        Some(theirs.id.as_str())
    );
}

/// The person asked sees the protected field under the question. What they
/// type goes to their vault's host with the kind and the places the question
/// names, and to nothing else: the question is answered with the reference
/// the vault gave, their words say so, and the resource holds nothing typed.
#[test]
fn what_is_typed_goes_to_the_vault_and_the_question_is_answered_with_its_reference() {
    let vault = Arc::new(TestHost::new().completing(|_| tilo_actions::ActionOutput {
        value: Some(json!({"reference": REFERENCE})),
        resources: Vec::new(),
    }));
    let _served = RemoteActions::enter(vault.clone(), Caller::local_person(ME));
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    let message = asked(&mut app, &me);
    app.settle();
    app.frame();
    assert_eq!(
        app.chat().secret_entry.shown_for().as_deref(),
        Some(message.id.as_str()),
        "the field is shown for the question that waits"
    );
    assert!(
        app.chat()
            .hits
            .iter()
            .any(|(_, hit)| *hit == Hit::SecretField(message.id.clone())),
        "the field stands under the question"
    );

    app.with_chat(|chat| {
        let answer = chat.secret_entry.type_for_test(FieldRole::Secret, TYPED);
        chat.save_secret(&message.id, &answer);
    });
    app.settle();
    let sent = vault.asked();
    let request = sent.last().expect("the vault's host is asked");
    assert_eq!(request.target, person_vault::target(person_vault::ADD));
    assert_eq!(
        request.argument,
        InvokeArgument::Json(
            json!({
                "kind": "login",
                "belongsTo": ["https://www.example.com/login"],
                "username": "ada",
                "secret": TYPED,
            })
            .as_object()
            .unwrap()
            .clone()
        )
    );

    let answered = row(&mut app, |m| m.id == message.id);
    let given = answered.question.unwrap().answered.expect("it is answered");
    assert_eq!(given.answer.0, FILLED);
    assert_eq!(given.detail.0, json!({"reference": REFERENCE}).to_string());
    let said = row(&mut app, |m| m.author == me);
    assert_eq!(said.text, "It is in my vault now.");
    assert_eq!(
        app.chat().secret_entry.shown_for(),
        None,
        "the field is gone"
    );
    for kept in mounted_rows(&mut app, CHANNEL) {
        let kept = format!("{kept:?}");
        assert!(!kept.contains(TYPED), "the resource holds what was typed");
    }
}

/// The protected field stands under the question in every theme and in a
/// window as narrow as a phone, inside the window.
#[test]
fn the_field_is_drawn_in_every_theme_and_fits_a_narrow_window() {
    let mut app = TestApp::<ChatShell>::new().unwrap();
    let me = window_person(&mut app);
    let message = asked(&mut app, &me);
    for theme in [
        ui::Theme::dark(),
        ui::Theme::light(),
        ui::Theme::high_contrast(),
    ] {
        app.harness().set_theme(theme);
        app.harness().resize((390, 844)).unwrap();
        app.settle();
        // Narrowed, the conversation stays in front; its slide lands.
        app.with_widget::<ui::widget::NavShell, _>(ui::widget::NavShell::snap);
        app.settle();
        app.frame();
        let fields: Vec<_> = app
            .chat()
            .hits
            .iter()
            .filter(|(_, hit)| *hit == Hit::SecretField(message.id.clone()))
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(fields.len(), 1, "one field under the one question");
        assert!(
            fields.iter().all(|rect| rect.width() > 0.0
                && rect.height() > 0.0
                && rect.x0 >= 0.0
                && rect.x1 <= 390.0),
            "{fields:?}"
        );
    }
}
