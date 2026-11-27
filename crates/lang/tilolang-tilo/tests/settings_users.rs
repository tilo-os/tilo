//! Settings' Users pane: the person's own picture, shown from the people reading and changed
//! through the session that keeps it with their identity.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::Actor;
use tilolang_runtime::delegate::Invocation;
use tilolang_tilo::runtime::Value;

use crate::fixtures::Sent;
use crate::fixtures::invoker::Answered as _;
use crate::fixtures::{drawn, set_reading};
use crate::settings_panes::{invoke, open_window, settings};

/// The person at the settings window.
fn me() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The picture of a PNG pixel: what the person chose, as the reading carries it.
const PICTURE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNgAAAAAgABSK+kcQAAAABJRU5ErkJggg==";

/// The Users pane shows the person's own picture from the people reading, and removing it is the
/// session's own verb, which takes it out of their identity record.
#[test]
fn the_users_pane_shows_the_persons_picture_and_takes_it_away() {
    let (host, _) = settings();
    // The pane reads this computer's login accounts first, as every pane wrapped in `read_pane`
    // reads its own resource before it draws.
    set_reading(
        &host,
        "users",
        &serde_json::json!({ "serving": true, "accounts": [] }),
    );
    set_reading(
        &host,
        "session:people",
        &serde_json::json!({
            "rows": [],
            "me": {
                "key": "self",
                "name": "Grace",
                "colour": "3389d1",
                "photo": PICTURE,
                "agent": false,
                "bot": false,
                "automatic": false,
                "me": true,
                "availability": "available",
                "doing": "",
                "activity": "",
                "in_app": "",
                "call": "",
                "follows": "",
                "joined": "",
                "following": false,
                "follower": false,
                "can_message": false,
                "given": "",
                "colour_id": "",
                "colour_slot": null,
                "devices": [],
                "linked": [],
                "agent_client": "",
                "user_key": "",
            },
            "showing": "",
            "following": "",
            "waved": "",
            "arrived": [],
            "arrived_seen": "",
        }),
    );
    invoke(&host, "open_pane", vec![Value::text("users")]);
    let view = open_window(&host);

    let face = view
        .tree()
        .of_kind("ui.Avatar")
        .into_iter()
        .find(|node| node.props.get("name") == Some(&Value::text("Grace")))
        .unwrap_or_else(|| panic!("the person's face; the pane drew {}", drawn(view.tree())));
    assert_eq!(
        face.props.get("picture"),
        Some(&Value::text(PICTURE)),
        "their face draws the picture their record keeps"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    let remove = view
        .tree()
        .of_kind("ui.Button")
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text("Remove the picture")))
        .expect("a button to remove the picture")
        .key
        .clone();
    view.press_answering(&host, &me(), &remove)
        .expect("the picture is taken away");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.device", "set-photo")
    );
    assert!(
        args.iter()
            .any(|(named, value)| named == "picture" && *value == Value::text("")),
        "an empty picture goes back to their initials: {args:?}"
    );
}
