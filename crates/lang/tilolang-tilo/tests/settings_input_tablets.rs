//! Settings' Mouse and keyboard pane sets a drawing tablet's pad: a button runs any of the
//! desktop's shortcuts, and each ring and strip is set in every mode the pad reports, as the
//! section's own verbs take them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, ResourceRef, Value, ViewInstance};

const APP: &str = "so.tilo.settings";

/// The pad a test attaches, by the identity its settings are kept under.
const PAD: &str = "056a:0374:Wacom Intuos Pro M Pad";

/// The app as it ships: its source, its ids and the platform's packages it reads through.
fn settings() -> (Host, tilolang::Compiled) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas = crate::fixtures::settings_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    (host, compiled)
}

/// The reading a resource of the platform is served as, by what its id names: the pad with three
/// buttons and one ring of three modes, the desktop's shortcut table, and the choices the settings
/// service reads back for the pad.
fn reading(id: &str) -> String {
    if id.contains("input-devices") {
        serde_json::json!({
            "devices": [{
                "key": PAD,
                "name": "Wacom Intuos Pro M Pad",
                "kind": "tablet_pad",
                "pad_buttons": 3,
                "pad_rings": [3],
                "pad_strips": [],
                "pad_mode_buttons": [2],
                "physical_device": "",
            }],
            "active_keyboard": null,
        })
        .to_string()
    } else if id.contains("displays") {
        serde_json::json!({ "displays": [] }).to_string()
    } else if id.contains("shortcuts-table") {
        serde_json::json!({
            "bindings": [
                { "id": "search", "chord": "Super+Space" },
                { "id": "focus-previous-window", "chord": "Super+Left" },
            ],
            "actions": [],
        })
        .to_string()
    } else if id.contains("tablet-choices") {
        serde_json::json!({
            "tablets": {
                PAD: {
                    "lower_button": "default",
                    "upper_button": "default",
                    "third_button": "default",
                    "eraser": "native",
                    "pad_buttons": { "0": "show-map" },
                    "pad_rings": { "0": ["scroll", "zoom", "nothing"] },
                    "pad_strips": {},
                },
            },
        })
        .to_string()
    } else if id.starts_with("devices/") && id.ends_with("settings/input") {
        // This device's input section: no tablet has anything stored of its own yet.
        serde_json::json!({ "tablets": {} }).to_string()
    } else if id.ends_with("settings/input") {
        // The person's input section: the pane's other rows, at their defaults.
        serde_json::json!({ "paste_shortcuts": {}, "keyboard_layouts": ["us"] }).to_string()
    } else {
        serde_json::json!({}).to_string()
    }
}

/// Hand the pane every reading it asks for, as the session publishes them: each resource of the
/// platform at the address a view names it by, so nothing it reads is missing.
fn serve_readings(host: &Host) {
    for (published, ty) in tilolang_tilo::platform_apps::platform_roots() {
        let reference = ResourceRef::parse_id(&published, &ty).expect("a resource id");
        host.runtime()
            .set_reading(&reference, &reading(&published))
            .expect("the reading is handed in");
    }
}

/// The pane opened as its own view, with what it reads served.
fn input_pane(host: &Host, compiled: &tilolang::Compiled) -> ViewInstance {
    let id = compiled
        .meta
        .views
        .iter()
        .find(|view| view.name == "input_pane")
        .expect("the pane is declared")
        .id
        .clone();
    serve_readings(host);
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

/// The key of the dropdown the pane captions `caption`.
fn dropdown(tree: &tilolang_runtime::UiNode, caption: &str) -> String {
    tree.of_kind("ui.Dropdown")
        .into_iter()
        .find(|node| node.props.get("caption") == Some(&Value::text(caption)))
        .unwrap_or_else(|| {
            let shown: Vec<String> = tree
                .of_kind("ui.Dropdown")
                .into_iter()
                .filter_map(|node| match node.props.get("caption") {
                    Some(Value::Text(caption)) => Some(caption.clone()),
                    _ => None,
                })
                .collect();
            panic!("a dropdown captioned {caption}; the pane shows {shown:?}")
        })
        .key
        .clone()
}

/// One invocation as it was sent, with its arguments by name.
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// The arguments of the one invocation `sent` holds, by name, with the action it names.
fn sent_once(sent: &Rc<RefCell<Vec<Invocation>>>) -> (String, Vec<(String, Value)>) {
    let sent = sent.borrow();
    let [Invocation::Action { name, args, .. }] = sent.as_slice() else {
        panic!("one verb of the section: {sent:?}");
    };
    (name.clone(), args.clone())
}

fn argument(args: &[(String, Value)], wanted: &str) -> Value {
    args.iter()
        .find(|(named, _)| named == wanted)
        .map(|(_, value)| value.clone())
        .unwrap_or_else(|| panic!("an argument `{wanted}`: {args:?}"))
}

#[test]
fn a_pad_button_is_given_any_of_the_desktops_shortcuts() {
    let (host, compiled) = settings();
    let mut view = input_pane(&host, &compiled);

    // The button offers the desktop's shortcuts by what they do and the keys they are on, and
    // carries the id the section's verb takes.
    let key = dropdown(view.tree(), "Button 1");
    let node = view.find(&key).expect("the button's dropdown").clone();
    let options = match node.props.get("options") {
        Some(Value::List(values)) => values.clone(),
        other => panic!("the dropdown's options: {other:?}"),
    };
    let values = match node.props.get("values") {
        Some(Value::List(values)) => values.clone(),
        other => panic!("the dropdown's values: {other:?}"),
    };
    assert!(
        options.contains(&Value::text("Search (Super+Space)")),
        "the shortcut reads as what it does and its keys: {options:?}"
    );
    assert!(
        values.contains(&Value::text("shortcut:focus-previous-window")),
        "a choice carries the catalogue's own id: {values:?}"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.fire_with(
        &key,
        "change",
        Some(&Value::text("shortcut:focus-previous-window")),
    )
    .expect("the choice is taken");
    let (name, args) = sent_once(&sent);
    assert_eq!(name, "set-pad-button");
    assert_eq!(argument(&args, "device"), Value::text(PAD));
    assert_eq!(argument(&args, "button"), Value::Int(0));
    assert_eq!(
        argument(&args, "action"),
        Value::text("shortcut:focus-previous-window"),
        "the shortcut is the choice the verb already takes"
    );
}

#[test]
fn a_ring_is_set_in_every_mode_the_pad_reports() {
    let (host, compiled) = settings();
    let mut view = input_pane(&host, &compiled);

    // One row per mode the ring reports, each showing what the settings service reads for it.
    let ring = view.tree().of_kind("ui.Dropdown");
    for mode in 1..=3 {
        assert!(
            ring.iter().any(|node| node.props.get("caption")
                == Some(&Value::text(format!("Ring 1, mode {mode}")))),
            "mode {mode} of the ring is set on its own"
        );
    }
    assert!(
        !ring
            .iter()
            .any(|node| node.props.get("caption") == Some(&Value::text("Ring 1, mode 4"))),
        "the ring reports three modes"
    );
    let second = dropdown(view.tree(), "Ring 1, mode 2");
    let node = view
        .find(&second)
        .expect("the second mode's dropdown")
        .clone();
    assert_eq!(
        node.props.get("value"),
        Some(&Value::text("zoom")),
        "the row shows what the second mode does now"
    );

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));
    view.fire_with(&second, "change", Some(&Value::text("volume")))
        .expect("the choice is taken");
    let (name, args) = sent_once(&sent);
    assert_eq!(name, "set-pad-dial");
    assert_eq!(argument(&args, "device"), Value::text(PAD));
    assert_eq!(argument(&args, "dial"), Value::text("ring"));
    assert_eq!(argument(&args, "index"), Value::Int(0));
    assert_eq!(
        argument(&args, "mode"),
        Value::Int(1),
        "the second mode of the ring, counted from zero as the mapping is"
    );
    assert_eq!(argument(&args, "action"), Value::text("volume"));
}

#[test]
fn a_pads_mode_button_switches_modes_and_is_given_nothing() {
    let (host, compiled) = settings();
    let view = input_pane(&host, &compiled);

    assert!(
        view.tree()
            .of_kind("ui.Dropdown")
            .iter()
            .all(|node| node.props.get("caption") != Some(&Value::text("Button 3"))),
        "the pad's mode button is no choice to set"
    );
    assert!(
        crate::fixtures::nodes(view.tree())
            .iter()
            .filter_map(|node| node.props.get("secondary"))
            .any(|text| *text == Value::text("Switches the pad between its modes")),
        "it says what the button does instead: {:?}",
        crate::fixtures::nodes(view.tree())
            .iter()
            .filter_map(|node| node.props.get("secondary"))
            .collect::<Vec<_>>()
    );
}
