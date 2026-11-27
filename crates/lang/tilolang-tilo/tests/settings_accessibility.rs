//! Settings' accessibility pane over the desktop's own services: the hints that teach its
//! controls, and what this device keeps of what the person copies.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::Actor;
use tilolang_runtime::delegate::Invocation;
use tilolang_tilo::runtime::Value;

use crate::fixtures::Sent;
use crate::fixtures::{control_under, labelled, set_reading};
use crate::invoker::Answered as _;
use crate::settings_panes::{invoke, open_window, settings};

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The Accessibility pane's hints and copies: "Teach me again" forgets the hints through the
/// section's own verb, the hints switch edits the person's setting where it is kept, and the
/// clipboard's keeping is the device's own action.
#[test]
fn the_accessibility_pane_teaches_hints_again_and_stops_keeping_copies() {
    let (host, _) = settings();
    // The pane draws over the person's accessibility settings, which it reads before it draws,
    // and this device's share of them: the sign-in switch.
    set_reading(&host, "settings/accessibility", &serde_json::json!({}));
    set_reading(
        &host,
        "devices/{device}/settings/accessibility",
        &serde_json::json!({ "apply_to_sign_in": true }),
    );
    set_reading(
        &host,
        "settings/control-hints",
        &serde_json::json!({ "learned": { "enabled": true, "known": ["open-search"] } }),
    );
    set_reading(
        &host,
        "session:clipboard",
        &serde_json::json!({ "copies": [], "keeping": true }),
    );
    invoke(&host, "open_pane", vec![Value::text("accessibility")]);
    let view = open_window(&host);

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    // The hints switch is the person's setting: the edit goes to the section where it is kept.
    let switch = control_under(view.tree(), "Show hints", "ui.Toggle");
    assert_eq!(
        view.bound_value(&switch),
        Some(Value::Bool(true)),
        "the switch shows the section's own field"
    );
    view.edit(&switch, &Value::Bool(false))
        .expect("the hints turn off");
    let sent_edits = sent.borrow().clone();
    let [
        Invocation::Edit {
            app,
            target,
            path,
            field,
            value,
            ..
        },
    ] = sent_edits.as_slice()
    else {
        panic!("one field edit: {sent_edits:?}");
    };
    assert_eq!(app.as_str(), "so.tilo.settings.sections");
    assert_eq!(target.id_string(), "settings/control-hints");
    assert_eq!(path.as_slice(), ["learned".to_owned()]);
    assert_eq!(field.as_str(), "enabled");
    assert_eq!(*value, Value::Bool(false));

    // Teaching again is the section's verb, over the whole set of hints.
    sent.borrow_mut().clear();
    view.press_answering(
        &host,
        &me(),
        &labelled(view.tree(), "ui.Button", "Teach me again"),
    )
    .expect("the hints are taught again");
    let sent_verbs = sent.borrow().clone();
    let [Invocation::Action { app, name, .. }] = sent_verbs.as_slice() else {
        panic!("one verb: {sent_verbs:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.settings.sections", "show-control-hints-again")
    );

    // What the device keeps of the person's copies is the clipboard's own action.
    sent.borrow_mut().clear();
    view.press_answering(
        &host,
        &me(),
        &control_under(view.tree(), "Keep what I copy", "ui.Button"),
    )
    .expect("keeping is turned off");
    let sent_clip = sent.borrow().clone();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent_clip.as_slice()
    else {
        panic!("one verb of the clipboard: {sent_clip:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "keep-copies")
    );
    assert!(
        args.iter()
            .any(|(named, value)| named == "on" && *value == Value::Bool(false)),
        "the switch that showed it on turns it off: {args:?}"
    );
}
