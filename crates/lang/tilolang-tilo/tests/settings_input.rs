//! The Mouse and keyboard pane's keyboard half, as a person uses it: the layouts each attached
//! keyboard types and the list they share, the drawing of the layout being inspected with the key
//! a press inspects, and what Caps Lock and the compose key do.
//!
//! Every step is the pane's own: a control it shows is found and pressed, and what it sends is
//! read as the settings service would receive it.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use serde_json::{Value as Json, json};
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{InvokeOutcome, UiNode};
use tilolang_tilo::host::Host;
use tilolang_tilo::platform_apps::platform_roots;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ViewInstance};

const APP: &str = "so.tilo.settings";

/// The app as it ships: its source, its ids and the platform's packages it reads through.
fn settings() -> Host {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    host
}

/// What the session publishes of the reading `ty` (`so.tilo.settings.sections#Input`), as the
/// view of the app names it: every field by its name.
fn publish(host: &Host, ty: &str, fields: &Json) {
    let (published, ty) = platform_roots()
        .into_iter()
        .find(|(_, root)| root == ty)
        .unwrap_or_else(|| panic!("{ty} is one of the platform's resources"));
    let reference = ResourceRef::parse_id(&published, ty).expect("a resource id");
    host.runtime()
        .set_reading(&reference, &fields.to_string())
        .expect("the reading is handed in");
}

/// The pane the window shows, opened on its own.
fn open_pane(host: &Host) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == "input_pane")
        .expect("the input pane is declared")
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .expect("the pane opens");
    view.poll_patches().expect("the pane draws");
    view
}

/// Every text the view shows, as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        for value in node.props.values() {
            match value {
                Value::Text(text) => out.push(text.clone()),
                Value::Rich(rich) => out.push(rich.written_text()),
                _ => {}
            }
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

/// The first control of `kind` whose property `prop` reads `label`.
fn control(tree: &UiNode, kind: &str, prop: &str, label: &str) -> String {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get(prop) == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a {kind} whose {prop} reads {label}"))
        .key
        .clone()
}

/// The key of the button named `name` for assistive technology.
fn button(tree: &UiNode, name: &str) -> String {
    control(tree, "ui.Button", "accessible_name", name)
}

/// One invocation as it was sent, with its arguments by name.
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// What the pressed control sent, as the settings service receives it: its verb and its arguments.
fn sent_invocation(sent: &Rc<RefCell<Vec<Invocation>>>) -> (String, Vec<(String, Value)>) {
    let sent = sent.borrow();
    let [Invocation::Action { name, args, .. }] = sent.as_slice() else {
        panic!("one verb of the settings: {sent:?}");
    };
    (name.clone(), args.clone())
}

/// The argument named `name`.
fn argument<'a>(args: &'a [(String, Value)], name: &str) -> &'a Value {
    &args
        .iter()
        .find(|(named, _)| named == name)
        .unwrap_or_else(|| panic!("an argument {name}: {args:?}"))
        .1
}

/// An argument as the settings service receives it: a value a press carried arrives wrapped in
/// the option an event's value may be, and the service reads it unwrapped.
fn received(value: &Value) -> Json {
    match value {
        Value::Opt(Some(inner)) => received(inner),
        Value::Opt(None) => Json::Null,
        other => tilolang_runtime::value::to_json(other).expect("a value writes as JSON"),
    }
}

/// The person's input on a device whose two keyboards type: one the shared layouts, one its own.
fn person_and_device(host: &Host) {
    publish(
        host,
        "so.tilo.settings.sections#Input",
        &json!({
            "touchpad_natural_scroll": true,
            "mouse_natural_scroll": false,
            "tap_to_click": true,
            "pointer_speed": 0.5,
            "touchpad_pointer_speed": null,
            "mouse_pointer_speed": null,
            "key_repeat": 0.5,
            "key_repeat_delay_ms": null,
            "key_repeat_rate_hz": null,
            "keyboard_layout": "de",
            "tap_drag": true,
            "tap_drag_lock": false,
            "disable_while_typing": true,
            "middle_emulation": true,
            "left_handed": false,
            "accel_profile": "adaptive",
            "touchpad_gestures": true,
            "touchpad_click_method": "default",
            "touchpad_scroll_method": "two_finger",
            "scroll_speed": 1.0,
            "caps_lock": null,
            "compose_key": null,
            "keyboard_layouts": ["de", "us"],
            "paste_shortcuts": {"org.example.Term": "ctrl_v"},
        }),
    );
    publish(
        host,
        "so.tilo.settings.sections#InputDevice",
        &json!({
            "touchpad_enabled": true,
            "touchpad_disabled_on_external_mouse": false,
            "keyboard_model": "pc105",
            "keyboard_assignments": {
                "kbd-b": {"sources": ["fr", "us"], "active": "fr", "options": {}},
            },
            "keyboard_preview": null,
            "keyboard_preview_extra_levels": false,
            "keyboard_preview_level": 0,
            "keyboard_inspected_shape": 2,
            "tablets": {},
        }),
    );
    publish(
        host,
        "so.tilo.device#InputDevices",
        &json!({
            "devices": [
                {
                    "key": "kbd-a",
                    "name": "The laptop's keyboard",
                    "kind": "keyboard",
                    "pad_buttons": 0,
                    "pad_rings": [],
                    "pad_strips": [],
                    "pad_mode_buttons": [],
                    "physical_device": "",
                },
                {
                    "key": "kbd-b",
                    "name": "The desk's keyboard",
                    "kind": "keyboard",
                    "pad_buttons": 0,
                    "pad_rings": [],
                    "pad_strips": [],
                    "pad_mode_buttons": [],
                    "physical_device": "",
                },
            ],
            "active_keyboard": "kbd-b",
        }),
    );
    publish(
        host,
        "so.tilo.device#Displays",
        &json!({
            "displays": [],
            "mode": "mirror",
            "pending": false,
            "pending_until_unix_ms": null,
            "focused": "",
            "level": 0,
            "bar_reached": false,
            "bar_covered": false,
            "dock_reached": false,
            "dock_covered": false,
        }),
    );
    publish(
        host,
        "so.tilo.device#Apps",
        &json!({
            "installed": [
                {"key": "org.example.Term", "name": "Terminal", "icon": "terminal", "category": ""},
                {"key": "org.example.Writer", "name": "Writer", "icon": "file", "category": ""},
            ],
        }),
    );
    publish(
        host,
        "so.tilo.device#Keyboard",
        &json!({
            "layouts": [
                {"id": "de", "label": "German"},
                {"id": "fr", "label": "French"},
                {"id": "us", "label": "English (US)"},
            ],
            "layouts_error": null,
            "drawing": {
                "layout": "de",
                "model": "pc105",
                "width": 40.0,
                "height": 20.0,
                "level_count": 2,
                "keys": [
                    {
                        "name": "AD01",
                        "outline": [
                            {"x": 0.0, "y": 0.0},
                            {"x": 10.0, "y": 0.0},
                            {"x": 10.0, "y": 10.0},
                            {"x": 0.0, "y": 10.0},
                        ],
                        "levels": ["q", "Q"],
                    },
                    {
                        "name": "AD02",
                        "outline": [
                            {"x": 10.0, "y": 0.0},
                            {"x": 20.0, "y": 0.0},
                            {"x": 20.0, "y": 10.0},
                            {"x": 10.0, "y": 10.0},
                        ],
                        "levels": ["w", "W"],
                    },
                ],
            },
            "drawing_error": null,
        }),
    );
}

/// The pane lists the keyboards attached, what each types, and the shared list; the drawing shows
/// the layout the settings inspect, and the inspected key's levels.
#[test]
fn the_pane_shows_the_keyboards_attached_their_layouts_and_the_keys() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let shown = texts(&view);

    for part in [
        "The laptop's keyboard",
        "The desk's keyboard",
        "German",
        "French",
        "English (US)",
        "The shared layouts",
    ] {
        assert!(
            shown.iter().any(|text| text.contains(part)),
            "{part} stands on the pane: {shown:?}"
        );
    }
    assert!(
        shown
            .iter()
            .any(|text| text.contains("The shared layouts, typing with German")),
        "the laptop's keyboard types the shared layouts: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|text| text.contains("Its own layouts, typing with French")),
        "the desk's keyboard types its own: {shown:?}"
    );

    let drawing = view
        .tree()
        .of_kind("ui.Drawing")
        .into_iter()
        .find(|node| {
            node.props
                .get("label")
                .and_then(|label| match label {
                    Value::Text(text) => Some(text.contains("German")),
                    _ => None,
                })
                .unwrap_or(false)
        })
        .expect("the keyboard is drawn");
    assert!(
        drawing
            .props
            .get("label")
            .is_some_and(|label| matches!(label, Value::Text(text) if text.contains("keys"))),
        "the drawing says what it shows: {:?}",
        drawing.props.get("label")
    );
    // The inspected key's levels are read below the drawing.
    for part in ["Base", "Shift", "w", "W"] {
        assert!(
            shown.iter().any(|text| text == part),
            "{part} of the inspected key is shown: {shown:?}"
        );
    }
}

/// Giving one keyboard a layout sends the layouts it types with it, over the keyboard's own
/// identity: the shared list and the other keyboard stay as they were.
#[test]
fn a_keyboard_is_given_a_layout_of_its_own() {
    let host = settings();
    person_and_device(&host);
    let mut view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    let dropdown = control(
        &view.tree(),
        "ui.Dropdown",
        "caption",
        "The laptop's keyboard types",
    );
    view.edit(&dropdown, &Value::text("English (US)"))
        .expect("the laptop's keyboard is given a layout");
    view.poll_patches().expect("the choice draws");
    view.press(&button(
        view.tree(),
        "Give The laptop's keyboard the chosen layout",
    ))
    .expect("the laptop's keyboard is given the chosen layout");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-keyboard-sources");
    assert_eq!(
        argument(&args, "keyboard"),
        &Value::text("kbd-a"),
        "the keyboard it was chosen for"
    );
    assert_eq!(
        argument(&args, "sources"),
        &Value::List(vec![Value::text("de"), Value::text("us")]),
        "the shared layouts, with the chosen one"
    );
    assert_eq!(
        argument(&args, "active"),
        &Value::text("us"),
        "typing with the chosen one"
    );
}

/// A keyboard with layouts of its own types the shared ones again when asked to.
#[test]
fn a_keyboard_goes_back_to_the_shared_layouts() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press(&button(
        view.tree(),
        "Type the shared layouts again for The desk's keyboard",
    ))
    .expect("the desk's keyboard goes back to the shared layouts");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "use-shared-layouts");
    assert_eq!(argument(&args, "keyboard"), &Value::text("kbd-b"));
}

/// Taking a layout out of a keyboard's own list sends the list without it, typing with one that
/// is left.
#[test]
fn a_layout_is_taken_out_of_a_keyboards_own_list() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press(&button(
        view.tree(),
        "Remove French from The desk's keyboard",
    ))
    .expect("French is taken out of the desk's keyboard");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-keyboard-sources");
    assert_eq!(argument(&args, "keyboard"), &Value::text("kbd-b"));
    assert_eq!(
        argument(&args, "sources"),
        &Value::List(vec![Value::text("us")])
    );
    assert_eq!(
        argument(&args, "active"),
        &Value::text("us"),
        "the layout it typed with is gone, so it types with the one left"
    );
}

/// Adding a layout to the shared list sends the list with it, typing with the layout it already
/// typed with.
#[test]
fn a_layout_joins_the_shared_list_without_changing_what_is_typed() {
    let host = settings();
    person_and_device(&host);
    let mut view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    let dropdown = control(&view.tree(), "ui.Dropdown", "caption", "Another layout");
    view.edit(&dropdown, &Value::text("French"))
        .expect("French is chosen");
    view.poll_patches().expect("the choice draws");
    view.press(&button(
        view.tree(),
        "Add the chosen layout to the shared layouts",
    ))
    .expect("French joins the shared list");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-layouts");
    assert_eq!(
        argument(&args, "layouts"),
        &Value::List(vec![
            Value::text("de"),
            Value::text("us"),
            Value::text("fr"),
        ])
    );
    assert_eq!(
        argument(&args, "active"),
        &Value::text("de"),
        "the layout typed with is unchanged"
    );
}

/// Taking a layout out of the shared list sends the list without it, still typing with the
/// layout it typed with.
#[test]
fn a_layout_leaves_the_shared_list() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press(&button(
        view.tree(),
        "Remove English (US) from the shared layouts",
    ))
    .expect("English (US) leaves the shared list");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-layouts");
    assert_eq!(
        argument(&args, "layouts"),
        &Value::List(vec![Value::text("de")])
    );
    assert_eq!(
        argument(&args, "active"),
        &Value::text("de"),
        "the layout typed with stays"
    );
}

/// Pressing a key inspects it at the level shown, and choosing another level shows what every key
/// types on it.
#[test]
fn a_pressed_key_is_inspected_and_the_level_is_chosen() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    let drawing = view
        .tree()
        .of_kind("ui.Drawing")
        .into_iter()
        .find(|node| {
            node.props
                .get("label")
                .is_some_and(|label| matches!(label, Value::Text(text) if text.contains("German")))
        })
        .expect("the keyboard is drawn")
        .key
        .clone();
    // Each key is its body and its legend: the second key's body is shape 2.
    let outcome = view
        .fire_with(&drawing, "press", Some(&Value::Int(2)))
        .expect("the second key is pressed");
    assert!(matches!(outcome, InvokeOutcome::Done(_)), "{outcome:?}");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "inspect-keyboard");
    assert_eq!(
        received(argument(&args, "shape")),
        json!(2),
        "the shape the press landed on"
    );
    assert_eq!(received(argument(&args, "level")), json!(0));

    sent.borrow_mut().clear();
    view.press(&control(&view.tree(), "ui.Segment", "label", "Shift"))
        .expect("the Shift level is chosen");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "inspect-keyboard");
    assert_eq!(received(argument(&args, "level")), json!(1));
    assert_eq!(
        received(argument(&args, "shape")),
        json!(2),
        "the shape inspected stays the one pressed"
    );
}

/// Another layout is drawn without typing with it, and the keyboard's own layout comes back.
#[test]
fn another_layout_is_drawn_and_the_keyboards_own_comes_back() {
    let host = settings();
    person_and_device(&host);
    let mut view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    let dropdown = control(
        &view.tree(),
        "ui.Dropdown",
        "caption",
        "Draw another layout",
    );
    view.edit(&dropdown, &Value::text("English (US)"))
        .expect("another layout is chosen");
    view.poll_patches().expect("the choice draws");
    view.press(&control(&view.tree(), "ui.Button", "label", "Draw it"))
        .expect("another layout is drawn");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "show-keyboard-layout");
    assert_eq!(
        argument(&args, "layout"),
        &Value::Opt(Some(Box::new(Value::text("us"))))
    );
}

/// What Caps Lock and the compose key do are sent as the choices they name.
#[test]
fn caps_lock_and_the_compose_key_are_set() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press(&control(&view.tree(), "ui.Segment", "label", "Escape"))
        .expect("Caps Lock is made Escape");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-caps-lock");
    assert_eq!(argument(&args, "behaviour"), &Value::text("escape"));

    sent.borrow_mut().clear();
    view.press(&control(&view.tree(), "ui.Segment", "label", "Right Alt"))
        .expect("Right Alt is made the compose key");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-compose-key");
    assert_eq!(argument(&args, "key"), &Value::text("ralt"));
}

/// An app that pastes differently is given its own shortcut, and put back on its own again.
#[test]
fn an_apps_paste_shortcut_is_chosen_and_given_back() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    assert!(
        texts(&view).iter().any(|text| text == "Pastes with Ctrl+V"),
        "the Terminal pastes with Ctrl+V: {:?}",
        texts(&view)
    );

    view.press(&control(
        &view.tree(),
        "ui.Segment",
        "label",
        "Ctrl+Shift+V",
    ))
    .expect("the Terminal is given Ctrl+Shift+V");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-paste-shortcut");
    assert_eq!(argument(&args, "app"), &Value::text("org.example.Term"));
    assert_eq!(argument(&args, "shortcut"), &Value::text("ctrl_shift_v"));

    sent.borrow_mut().clear();
    view.press(&button(view.tree(), "Use Terminal's own shortcut"))
        .expect("the Terminal goes back to its own shortcut");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-paste-shortcut");
    assert_eq!(argument(&args, "app"), &Value::text("org.example.Term"));
    assert_eq!(
        argument(&args, "shortcut"),
        &Value::text("default"),
        "the shortcut its app id suggests"
    );
}

/// An app with no shortcut of its own is chosen from the apps this device has, not typed.
#[test]
fn another_app_joins_with_a_chosen_shortcut() {
    let host = settings();
    person_and_device(&host);
    let mut view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    let apps = control(&view.tree(), "ui.Dropdown", "caption", "The app");
    view.edit(&apps, &Value::text("Writer"))
        .expect("Writer is chosen");
    view.poll_patches().expect("the choice draws");
    view.press(&control(&view.tree(), "ui.Button", "label", "Set it"))
        .expect("Writer is given the chosen shortcut");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-paste-shortcut");
    assert_eq!(
        argument(&args, "app"),
        &Value::text("org.example.Writer"),
        "the app the device knows by that name"
    );
    assert_eq!(argument(&args, "shortcut"), &Value::text("ctrl_v"));
}

/// A layout is moved along the order the shared layouts are switched in.
#[test]
fn a_layout_moves_along_the_shared_order() {
    let host = settings();
    person_and_device(&host);
    let view = open_pane(&host);
    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press(&button(
        view.tree(),
        "Move English (US) earlier in the shared layouts",
    ))
    .expect("English (US) moves before German");
    let (name, args) = sent_invocation(&sent);
    assert_eq!(name, "set-layouts");
    assert_eq!(
        argument(&args, "layouts"),
        &Value::List(vec![Value::text("us"), Value::text("de")])
    );
    assert_eq!(
        argument(&args, "active"),
        &Value::text("de"),
        "the layout typed with is unchanged"
    );
}
