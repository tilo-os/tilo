//! Settings' notifications pane: which kinds of notification one app arrives as, decided where
//! the compositor reads them.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::Actor;
use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::value::same_value;
use tilolang_tilo::runtime::Value;

use crate::fixtures::Sent;
use crate::fixtures::{control_under, labelled, set_reading};
use crate::invoker::Answered as _;
use crate::settings_panes::{invoke, open_window, settings};

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// One app's own rules: the pane shows which kinds of notification it arrives as, and a press
/// sets that one kind through the section's verb while the kinds it left unset stay unset.
#[test]
fn the_notifications_pane_sets_which_kinds_one_app_arrives_as() {
    let (host, _) = settings();
    set_reading(
        &host,
        "settings/notifications",
        &serde_json::json!({
            "per_app": {
                "so.tilo.mail": {
                    "banners": false,
                    "sounds": true,
                    "priority": false,
                    "priority_contacts": false,
                },
            },
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("notifications")]);
    let mut view = open_window(&host);

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    // The app's own rules show once its row is the one being edited, in place of every app's at
    // once.
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Its rules"),
    )
    .expect("the app's rules show");
    view.poll_patches().expect("the rules draw");

    view.press_answering(
        &host,
        &me(),
        &control_under(view.tree(), "Show banners", "ui.Button"),
    )
    .expect("the app's banners are turned on");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the notifications section: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.settings.sections", "set-app-kinds"),
        "the section's own verb, as its surface publishes it"
    );
    let arg = |wanted: &str| -> Option<Value> {
        args.iter()
            .find(|(named, _)| named == wanted)
            .and_then(|(_, value)| match value {
                Value::Opt(Some(inner)) => Some((**inner).clone()),
                Value::Opt(None) => None,
                other => Some(other.clone()),
            })
    };
    assert_eq!(arg("app"), Some(Value::text("so.tilo.mail")));
    // A drawn toggle carries its true as `1` and the host reads it back as a `bool`, so the
    // language's own equality judges the flag, not the view's encoding of it.
    let is_true = |value: Option<Value>| value.is_some_and(|v| same_value(&v, &Value::Bool(true)));
    assert!(
        is_true(arg("banners")),
        "the button turns the app's off banners on; it sent {args:?}"
    );
    assert_eq!(
        arg("previews"),
        None,
        "a kind the app left unset stays unset, following the person's rule"
    );
    assert!(
        is_true(arg("sounds")),
        "the rule's own sounds stay on: {args:?}"
    );
    assert_eq!(arg("badges"), None);
}
